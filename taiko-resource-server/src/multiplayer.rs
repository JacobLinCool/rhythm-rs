use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use taiko_multiplayer_protocol::{
    ClientHello, ClientMessage, ClientSessionInfo, FinalResultReport, HostSelectSongRequest,
    InputEvent, JoinRoomRequest, MatchCountdown, MatchSongSelection, MatchStarted,
    PlayerFinalResultEnvelope, PlayerInputEnvelope, PlayerStateEnvelope, PlayerStateUpdate,
    ReadyRequest, RoomCreated, RoomJoined, RoomPhase, RoomPlayerSnapshot, RoomRole, RoomSnapshot,
    RoomSpectatorSnapshot, ServerError, ServerHello, ServerMessage, PROTOCOL_VERSION,
};
use taiko_resource_protocol::ResourceLibraryDocument;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::Mutex;

const ROOM_MAX_PLAYERS: usize = 4;
const ROOM_MAX_SPECTATORS: usize = 64;
const ROOM_CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const ROOM_CODE_LEN: usize = 4;
const ROOM_CODE_BITS: u32 = (ROOM_CODE_LEN * 5) as u32;
const ROOM_CODE_SPACE: usize = 1usize << ROOM_CODE_BITS;
const ROOM_CODE_MASK: u32 = (1u32 << ROOM_CODE_BITS) - 1;
#[cfg(any(test, feature = "test-fast-countdown"))]
const MATCH_COUNTDOWN_MS: u64 = 1;
#[cfg(not(any(test, feature = "test-fast-countdown")))]
const MATCH_COUNTDOWN_MS: u64 = 3_000;

#[derive(Clone)]
pub struct MultiplayerRegistry {
    inner: Arc<Mutex<RegistryInner>>,
    library_index: Arc<LibraryIndex>,
    server_started_at: Instant,
}

impl MultiplayerRegistry {
    pub fn new(library: &ResourceLibraryDocument) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryInner::default())),
            library_index: Arc::new(LibraryIndex::from_library(library)),
            server_started_at: Instant::now(),
        }
    }

    pub async fn register_session(&self, tx: UnboundedSender<ServerMessage>) -> u64 {
        let mut inner = self.inner.lock().await;
        inner.next_session_id = inner.next_session_id.saturating_add(1);
        let session_id = inner.next_session_id;
        inner.sessions.insert(
            session_id,
            SessionState {
                tx,
                name: None,
                membership: None,
            },
        );
        session_id
    }

    pub async fn remove_session(&self, session_id: u64) {
        let mut inner = self.inner.lock().await;
        let Some(session) = inner.sessions.remove(&session_id) else {
            return;
        };

        let Some(membership) = session.membership else {
            return;
        };

        let now_ms = now_unix_ms();
        match membership {
            SessionMembership::Player {
                room_code,
                player_id,
            } => {
                let mut should_remove_room = false;
                let mut recipients = Vec::new();
                if let Some(room) = inner.rooms.get_mut(&room_code) {
                    if let Some(player) = room.players.iter_mut().find(|p| p.player_id == player_id)
                    {
                        player.online = false;
                        player.ready = false;
                        player.dnf = true;
                    }
                    room.reassign_host_if_needed();
                    room.advance_phase(now_ms);
                    recipients = room.recipient_session_ids();
                    should_remove_room = room.online_participant_count() == 0;
                }

                if should_remove_room {
                    inner.rooms.remove(&room_code);
                } else if let Some(room) = inner.rooms.get(&room_code) {
                    let snapshot = room.snapshot(now_ms);
                    for target in recipients {
                        inner.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
                    }
                }
            }
            SessionMembership::Spectator {
                room_code,
                spectator_id,
            } => {
                let mut should_remove_room = false;
                let mut recipients = Vec::new();
                if let Some(room) = inner.rooms.get_mut(&room_code) {
                    room.spectators.retain(|s| s.spectator_id != spectator_id);
                    room.advance_phase(now_ms);
                    recipients = room.recipient_session_ids();
                    should_remove_room = room.online_participant_count() == 0;
                }

                if should_remove_room {
                    inner.rooms.remove(&room_code);
                } else if let Some(room) = inner.rooms.get(&room_code) {
                    let snapshot = room.snapshot(now_ms);
                    for target in recipients {
                        inner.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
                    }
                }
            }
        }
    }

    pub async fn handle_client_message(&self, session_id: u64, message: ClientMessage) {
        let mut inner = self.inner.lock().await;
        let now_ms = now_unix_ms();

        if !inner.sessions.contains_key(&session_id) {
            return;
        }

        inner.advance_all_rooms(now_ms);

        let result = match message {
            ClientMessage::Hello(payload) => inner.handle_hello(session_id, payload),
            other => {
                if !inner.session_has_hello(session_id) {
                    Err(ServerError {
                        code: "protocol_violation".to_owned(),
                        message: "hello is required before other messages".to_owned(),
                    })
                } else {
                    inner.handle_authenticated_message(
                        session_id,
                        other,
                        now_ms,
                        &self.library_index,
                    )
                }
            }
        };

        if let Err(error) = result {
            inner.send_to(session_id, ServerMessage::Error(error));
        }
    }

    pub fn uptime(&self) -> Duration {
        self.server_started_at.elapsed()
    }
}

#[derive(Default)]
struct RegistryInner {
    next_session_id: u64,
    sessions: HashMap<u64, SessionState>,
    rooms: HashMap<String, RoomState>,
}

impl RegistryInner {
    fn send_to(&self, session_id: u64, message: ServerMessage) {
        if let Some(session) = self.sessions.get(&session_id) {
            let _ = session.tx.send(message);
        }
    }

    fn broadcast_to_room(&self, room: &RoomState, message: ServerMessage) {
        for session_id in room.recipient_session_ids() {
            self.send_to(session_id, message.clone());
        }
    }

    fn session_has_hello(&self, session_id: u64) -> bool {
        self.sessions
            .get(&session_id)
            .and_then(|s| s.name.as_ref())
            .is_some()
    }

    fn session_name(&self, session_id: u64) -> Result<&str, ServerError> {
        self.sessions
            .get(&session_id)
            .and_then(|s| s.name.as_deref())
            .ok_or_else(|| ServerError {
                code: "protocol_violation".to_owned(),
                message: "session has no hello".to_owned(),
            })
    }

    fn session_membership(&self, session_id: u64) -> Option<SessionMembership> {
        self.sessions
            .get(&session_id)
            .and_then(|session| session.membership.clone())
    }

    fn set_session_membership(&mut self, session_id: u64, membership: SessionMembership) {
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.membership = Some(membership);
        }
    }

    fn handle_hello(&mut self, session_id: u64, payload: ClientHello) -> Result<(), ServerError> {
        if payload.protocol_version != PROTOCOL_VERSION {
            return Err(ServerError {
                code: "unsupported_protocol".to_owned(),
                message: format!(
                    "unsupported multiplayer protocol version {} (expected {})",
                    payload.protocol_version, PROTOCOL_VERSION
                ),
            });
        }

        let name = payload.name.trim();
        if name.is_empty() {
            return Err(ServerError {
                code: "invalid_name".to_owned(),
                message: "player name cannot be empty".to_owned(),
            });
        }

        let session = self
            .sessions
            .get_mut(&session_id)
            .ok_or_else(|| ServerError {
                code: "unknown_session".to_owned(),
                message: "session not found".to_owned(),
            })?;

        if session.name.is_some() {
            return Err(ServerError {
                code: "protocol_violation".to_owned(),
                message: "hello can only be sent once".to_owned(),
            });
        }

        session.name = Some(name.to_owned());
        self.send_to(
            session_id,
            ServerMessage::Hello(ServerHello {
                protocol_version: PROTOCOL_VERSION,
                session: ClientSessionInfo {
                    session_id: format!("sess-{session_id}"),
                    name: name.to_owned(),
                },
            }),
        );
        Ok(())
    }

    fn handle_authenticated_message(
        &mut self,
        session_id: u64,
        message: ClientMessage,
        now_ms: u64,
        library_index: &LibraryIndex,
    ) -> Result<(), ServerError> {
        match message {
            ClientMessage::CreateRoom => self.handle_create_room(session_id, now_ms),
            ClientMessage::JoinRoom(payload) => self.handle_join_room(session_id, payload, now_ms),
            ClientMessage::Ready(payload) => self.handle_ready(session_id, payload, now_ms),
            ClientMessage::HostSelectSong(payload) => {
                self.handle_host_select_song(session_id, payload, now_ms, library_index)
            }
            ClientMessage::StartMatch => self.handle_start_match(session_id, now_ms),
            ClientMessage::InputEvent(payload) => self.handle_input_event(session_id, payload),
            ClientMessage::PlayerStateUpdate(payload) => {
                self.handle_player_state_update(session_id, payload, now_ms)
            }
            ClientMessage::FinalResult(payload) => {
                self.handle_final_result(session_id, payload, now_ms)
            }
            ClientMessage::Ping(payload) => {
                let mut payload = payload;
                payload.server_send_ms = Some(now_unix_ms());
                self.send_to(session_id, ServerMessage::Pong(payload));
                Ok(())
            }
            ClientMessage::Hello(_) => Err(ServerError {
                code: "protocol_violation".to_owned(),
                message: "hello is already handled".to_owned(),
            }),
        }
    }

    fn handle_create_room(&mut self, session_id: u64, now_ms: u64) -> Result<(), ServerError> {
        if self.session_membership(session_id).is_some() {
            return Err(ServerError {
                code: "already_joined".to_owned(),
                message: "session already joined a room".to_owned(),
            });
        }

        let name = self.session_name(session_id)?.to_owned();
        let room_code = self.generate_room_code()?;
        let player_id = "p1".to_owned();

        let mut room = RoomState {
            room_code: room_code.clone(),
            phase: RoomPhase::Lobby,
            host_player_id: player_id.clone(),
            song: None,
            start_at_ms: None,
            players: vec![RoomPlayerState {
                session_id,
                player_id: player_id.clone(),
                name: name.clone(),
                ready: false,
                online: true,
                dnf: false,
                last_input_seq: 0,
                last_state_seq: 0,
                last_state: None,
                final_result: None,
            }],
            spectators: Vec::new(),
        };

        room.advance_phase(now_ms);
        let snapshot = room.snapshot(now_ms);
        self.rooms.insert(room_code.clone(), room);
        self.set_session_membership(
            session_id,
            SessionMembership::Player {
                room_code: room_code.clone(),
                player_id: player_id.clone(),
            },
        );

        self.send_to(
            session_id,
            ServerMessage::RoomCreated(RoomCreated {
                session: ClientSessionInfo {
                    session_id: format!("sess-{session_id}"),
                    name,
                },
                room_code: room_code.clone(),
                player_id,
            }),
        );
        self.send_to(session_id, ServerMessage::RoomSnapshot(snapshot));
        Ok(())
    }

    fn handle_join_room(
        &mut self,
        session_id: u64,
        payload: JoinRoomRequest,
        now_ms: u64,
    ) -> Result<(), ServerError> {
        if self.session_membership(session_id).is_some() {
            return Err(ServerError {
                code: "already_joined".to_owned(),
                message: "session already joined a room".to_owned(),
            });
        }

        let room_code = payload.room_code.trim().to_ascii_uppercase();
        if room_code.is_empty() {
            return Err(ServerError {
                code: "invalid_room_code".to_owned(),
                message: "room code cannot be empty".to_owned(),
            });
        }

        let name = self.session_name(session_id)?.to_owned();
        if payload.spectate {
            let (spectator_id, recipients, snapshot) = {
                let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                    code: "room_not_found".to_owned(),
                    message: format!("room `{room_code}` not found"),
                })?;

                room.advance_phase(now_ms);
                let online_spectators = room.spectators.iter().filter(|s| s.online).count();
                if online_spectators >= ROOM_MAX_SPECTATORS {
                    return Err(ServerError {
                        code: "room_full".to_owned(),
                        message: format!("room `{room_code}` spectator slots are full"),
                    });
                }

                let spectator_id = format!("s{}", room.spectators.len() + 1);
                room.spectators.push(RoomSpectatorState {
                    session_id,
                    spectator_id: spectator_id.clone(),
                    name: name.clone(),
                    online: true,
                });
                (
                    spectator_id,
                    room.recipient_session_ids(),
                    room.snapshot(now_ms),
                )
            };

            self.set_session_membership(
                session_id,
                SessionMembership::Spectator {
                    room_code: room_code.clone(),
                    spectator_id: spectator_id.clone(),
                },
            );
            self.send_to(
                session_id,
                ServerMessage::RoomJoined(RoomJoined {
                    session: ClientSessionInfo {
                        session_id: format!("sess-{session_id}"),
                        name,
                    },
                    room_code: room_code.clone(),
                    role: RoomRole::Spectator,
                    actor_id: spectator_id,
                }),
            );
            for target in recipients {
                self.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
            }
            return Ok(());
        }

        let (player_id, recipients, snapshot) = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            room.advance_phase(now_ms);
            if room.phase != RoomPhase::Lobby {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "players can only join while room is in lobby".to_owned(),
                });
            }

            if room.players.len() >= ROOM_MAX_PLAYERS {
                return Err(ServerError {
                    code: "room_full".to_owned(),
                    message: format!("room `{room_code}` player slots are full"),
                });
            }

            let player_id = format!("p{}", room.players.len() + 1);
            room.players.push(RoomPlayerState {
                session_id,
                player_id: player_id.clone(),
                name: name.clone(),
                ready: false,
                online: true,
                dnf: false,
                last_input_seq: 0,
                last_state_seq: 0,
                last_state: None,
                final_result: None,
            });
            (
                player_id,
                room.recipient_session_ids(),
                room.snapshot(now_ms),
            )
        };

        self.set_session_membership(
            session_id,
            SessionMembership::Player {
                room_code: room_code.clone(),
                player_id: player_id.clone(),
            },
        );
        self.send_to(
            session_id,
            ServerMessage::RoomJoined(RoomJoined {
                session: ClientSessionInfo {
                    session_id: format!("sess-{session_id}"),
                    name,
                },
                room_code,
                role: RoomRole::Player,
                actor_id: player_id,
            }),
        );
        for target in recipients {
            self.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
        }
        Ok(())
    }

    fn handle_ready(
        &mut self,
        session_id: u64,
        payload: ReadyRequest,
        now_ms: u64,
    ) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "ready is only available for players".to_owned(),
                })
            }
        };

        let (recipients, snapshot, countdown, countdown_snapshot) = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            if room.phase != RoomPhase::Lobby {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "ready can only be changed in lobby".to_owned(),
                });
            }

            let player = room
                .players
                .iter_mut()
                .find(|player| player.player_id == player_id)
                .ok_or_else(|| ServerError {
                    code: "player_not_found".to_owned(),
                    message: "player not found in room".to_owned(),
                })?;

            if player.dnf {
                return Err(ServerError {
                    code: "player_inactive".to_owned(),
                    message: "cannot ready after disconnect".to_owned(),
                });
            }

            player.ready = payload.ready;
            room.advance_phase(now_ms);
            let recipients = room.recipient_session_ids();
            let snapshot = room.snapshot(now_ms);

            if room.phase == RoomPhase::Lobby && room.can_start_countdown() {
                let start_at_ms = now_ms.saturating_add(MATCH_COUNTDOWN_MS);
                room.phase = RoomPhase::Countdown;
                room.start_at_ms = Some(start_at_ms);
                let song = room.song.clone().ok_or_else(|| ServerError {
                    code: "invalid_state".to_owned(),
                    message: "song missing while starting countdown".to_owned(),
                })?;
                let countdown = MatchCountdown {
                    room_code: room.room_code.clone(),
                    start_at_ms,
                    song,
                };
                let countdown_snapshot = room.snapshot(now_ms);
                (
                    recipients,
                    snapshot,
                    Some(countdown),
                    Some(countdown_snapshot),
                )
            } else {
                (recipients, snapshot, None, None)
            }
        };

        for target in &recipients {
            self.send_to(*target, ServerMessage::RoomSnapshot(snapshot.clone()));
        }
        if let Some(countdown) = countdown {
            for target in &recipients {
                self.send_to(*target, ServerMessage::MatchCountdown(countdown.clone()));
            }
        }
        if let Some(countdown_snapshot) = countdown_snapshot {
            for target in recipients {
                self.send_to(
                    target,
                    ServerMessage::RoomSnapshot(countdown_snapshot.clone()),
                );
            }
        }

        Ok(())
    }

    fn handle_host_select_song(
        &mut self,
        session_id: u64,
        payload: HostSelectSongRequest,
        now_ms: u64,
        library_index: &LibraryIndex,
    ) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "song selection is only available for players".to_owned(),
                })
            }
        };

        let selection = library_index
            .lookup(&payload.source_id, payload.course_index)
            .ok_or_else(|| ServerError {
                code: "song_not_found".to_owned(),
                message: format!(
                    "song not found for source_id={} course_index={}",
                    payload.source_id, payload.course_index
                ),
            })?;

        let (recipients, snapshot) = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            if room.phase != RoomPhase::Lobby {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "song can only be selected in lobby".to_owned(),
                });
            }

            if room.host_player_id != player_id {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "only host can select song".to_owned(),
                });
            }

            room.song = Some(selection.clone());
            for player in &mut room.players {
                if !player.dnf {
                    player.ready = false;
                }
            }

            room.advance_phase(now_ms);
            (room.recipient_session_ids(), room.snapshot(now_ms))
        };

        for target in &recipients {
            self.send_to(*target, ServerMessage::SongSelected(selection.clone()));
        }
        for target in recipients {
            self.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
        }
        Ok(())
    }

    fn handle_start_match(&mut self, session_id: u64, now_ms: u64) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "start_match is only available for players".to_owned(),
                })
            }
        };

        let (recipients, countdown, snapshot) = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            if room.phase != RoomPhase::Lobby {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "start_match is only available in lobby".to_owned(),
                });
            }

            if room.host_player_id != player_id {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "only host can start match".to_owned(),
                });
            }

            if !room.can_start_countdown() {
                return Err(ServerError {
                    code: "not_ready".to_owned(),
                    message:
                        "all active players must be ready and at least two players are required"
                            .to_owned(),
                });
            }

            let start_at_ms = now_ms.saturating_add(MATCH_COUNTDOWN_MS);
            room.phase = RoomPhase::Countdown;
            room.start_at_ms = Some(start_at_ms);
            let song = room.song.clone().ok_or_else(|| ServerError {
                code: "invalid_state".to_owned(),
                message: "song missing while starting countdown".to_owned(),
            })?;
            let countdown = MatchCountdown {
                room_code: room.room_code.clone(),
                start_at_ms,
                song,
            };
            (
                room.recipient_session_ids(),
                countdown,
                room.snapshot(now_ms),
            )
        };

        for target in &recipients {
            self.send_to(*target, ServerMessage::MatchCountdown(countdown.clone()));
        }
        for target in recipients {
            self.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
        }
        Ok(())
    }

    fn handle_input_event(
        &mut self,
        session_id: u64,
        payload: InputEvent,
    ) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "input_event is only available for players".to_owned(),
                })
            }
        };

        let recipients = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            if !matches!(room.phase, RoomPhase::Playing | RoomPhase::Finished) {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "input_event is only available while playing".to_owned(),
                });
            }

            let player = room
                .players
                .iter_mut()
                .find(|player| player.player_id == player_id)
                .ok_or_else(|| ServerError {
                    code: "player_not_found".to_owned(),
                    message: "player not found in room".to_owned(),
                })?;

            if payload.seq <= player.last_input_seq {
                return Ok(());
            }
            player.last_input_seq = payload.seq;
            room.recipient_session_ids()
        };

        for target in recipients {
            self.send_to(
                target,
                ServerMessage::InputEvent(PlayerInputEnvelope {
                    player_id: player_id.clone(),
                    event: payload.clone(),
                }),
            );
        }
        Ok(())
    }

    fn handle_player_state_update(
        &mut self,
        session_id: u64,
        payload: PlayerStateUpdate,
        now_ms: u64,
    ) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "player_state_update is only available for players".to_owned(),
                })
            }
        };

        let recipients = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            room.advance_phase(now_ms);
            if !matches!(room.phase, RoomPhase::Playing | RoomPhase::Finished) {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "player_state_update is only available while playing".to_owned(),
                });
            }

            let player = room
                .players
                .iter_mut()
                .find(|player| player.player_id == player_id)
                .ok_or_else(|| ServerError {
                    code: "player_not_found".to_owned(),
                    message: "player not found in room".to_owned(),
                })?;

            if payload.seq <= player.last_state_seq {
                return Ok(());
            }

            player.last_state_seq = payload.seq;
            player.last_state = Some(payload.clone());
            room.recipient_session_ids()
        };

        for target in recipients {
            self.send_to(
                target,
                ServerMessage::PlayerStateUpdate(PlayerStateEnvelope {
                    player_id: player_id.clone(),
                    state: payload.clone(),
                }),
            );
        }
        Ok(())
    }

    fn handle_final_result(
        &mut self,
        session_id: u64,
        payload: FinalResultReport,
        now_ms: u64,
    ) -> Result<(), ServerError> {
        let (room_code, player_id) = match self.session_membership(session_id) {
            Some(SessionMembership::Player {
                room_code,
                player_id,
            }) => (room_code, player_id),
            _ => {
                return Err(ServerError {
                    code: "permission_denied".to_owned(),
                    message: "final_result is only available for players".to_owned(),
                })
            }
        };

        let (recipients, snapshot) = {
            let room = self.rooms.get_mut(&room_code).ok_or_else(|| ServerError {
                code: "room_not_found".to_owned(),
                message: format!("room `{room_code}` not found"),
            })?;

            room.advance_phase(now_ms);
            if !matches!(room.phase, RoomPhase::Playing | RoomPhase::Finished) {
                return Err(ServerError {
                    code: "invalid_phase".to_owned(),
                    message: "final_result is only available while playing".to_owned(),
                });
            }

            let player = room
                .players
                .iter_mut()
                .find(|player| player.player_id == player_id)
                .ok_or_else(|| ServerError {
                    code: "player_not_found".to_owned(),
                    message: "player not found in room".to_owned(),
                })?;

            if player
                .final_result
                .as_ref()
                .is_some_and(|current| payload.seq <= current.seq)
            {
                return Ok(());
            }

            player.final_result = Some(payload.clone());
            room.advance_phase(now_ms);
            (room.recipient_session_ids(), room.snapshot(now_ms))
        };

        for target in &recipients {
            self.send_to(
                *target,
                ServerMessage::FinalResult(PlayerFinalResultEnvelope {
                    player_id: player_id.clone(),
                    report: payload.clone(),
                }),
            );
        }
        for target in recipients {
            self.send_to(target, ServerMessage::RoomSnapshot(snapshot.clone()));
        }
        Ok(())
    }

    fn generate_room_code(&self) -> Result<String, ServerError> {
        if self.rooms.len() >= ROOM_CODE_SPACE {
            return Err(ServerError {
                code: "room_code_exhausted".to_owned(),
                message: "all room codes are currently allocated".to_owned(),
            });
        }

        let start = getrandom::u32().map_err(|error| ServerError {
            code: "random_unavailable".to_owned(),
            message: format!("failed to generate room code: {error}"),
        })? & ROOM_CODE_MASK;

        for offset in 0..ROOM_CODE_SPACE as u32 {
            let candidate = encode_room_code(start.wrapping_add(offset) & ROOM_CODE_MASK);
            if !self.rooms.contains_key(&candidate) {
                return Ok(candidate);
            }
        }

        Err(ServerError {
            code: "room_code_exhausted".to_owned(),
            message: "all room codes are currently allocated".to_owned(),
        })
    }

    fn advance_all_rooms(&mut self, now_ms: u64) {
        let room_codes = self.rooms.keys().cloned().collect::<Vec<_>>();
        for room_code in room_codes {
            let mut started = None;
            let mut snapshot = None;
            if let Some(room) = self.rooms.get_mut(&room_code) {
                let prev_phase = room.phase;
                room.advance_phase(now_ms);
                if prev_phase != room.phase {
                    if room.phase == RoomPhase::Playing {
                        if let Some(start_at_ms) = room.start_at_ms {
                            started = Some(start_at_ms);
                        }
                    }
                    snapshot = Some(room.snapshot(now_ms));
                }
            }

            if let Some(start_at_ms) = started {
                if let Some(room) = self.rooms.get(&room_code) {
                    self.broadcast_to_room(
                        room,
                        ServerMessage::MatchStarted(MatchStarted {
                            room_code: room_code.clone(),
                            start_at_ms,
                        }),
                    );
                }
            }
            if let Some(snapshot) = snapshot {
                if let Some(room) = self.rooms.get(&room_code) {
                    self.broadcast_to_room(room, ServerMessage::RoomSnapshot(snapshot));
                }
            }
        }
    }
}

fn encode_room_code(mut value: u32) -> String {
    let mut chars = [ROOM_CODE_ALPHABET[0]; ROOM_CODE_LEN];
    for slot in &mut chars {
        let idx = (value & 0b1_1111) as usize;
        *slot = ROOM_CODE_ALPHABET[idx];
        value >>= 5;
    }
    String::from_utf8(chars.to_vec()).expect("room code alphabet must be valid ASCII")
}

#[derive(Clone)]
struct SessionState {
    tx: UnboundedSender<ServerMessage>,
    name: Option<String>,
    membership: Option<SessionMembership>,
}

#[derive(Clone)]
enum SessionMembership {
    Player {
        room_code: String,
        player_id: String,
    },
    Spectator {
        room_code: String,
        spectator_id: String,
    },
}

#[derive(Clone)]
struct RoomPlayerState {
    session_id: u64,
    player_id: String,
    name: String,
    ready: bool,
    online: bool,
    dnf: bool,
    last_input_seq: u64,
    last_state_seq: u64,
    last_state: Option<PlayerStateUpdate>,
    final_result: Option<FinalResultReport>,
}

#[derive(Clone)]
struct RoomSpectatorState {
    session_id: u64,
    spectator_id: String,
    name: String,
    online: bool,
}

#[derive(Clone)]
struct RoomState {
    room_code: String,
    phase: RoomPhase,
    host_player_id: String,
    song: Option<MatchSongSelection>,
    start_at_ms: Option<u64>,
    players: Vec<RoomPlayerState>,
    spectators: Vec<RoomSpectatorState>,
}

impl RoomState {
    fn can_start_countdown(&self) -> bool {
        if self.song.is_none() {
            return false;
        }

        let online_players = self
            .players
            .iter()
            .filter(|player| !player.dnf && player.online)
            .collect::<Vec<_>>();

        if online_players.len() < 2 {
            return false;
        }

        online_players.into_iter().all(|player| player.ready)
    }

    fn reassign_host_if_needed(&mut self) {
        let host_is_online = self
            .players
            .iter()
            .any(|player| player.player_id == self.host_player_id && player.online && !player.dnf);

        if host_is_online {
            return;
        }

        if let Some(next_host) = self
            .players
            .iter()
            .find(|player| player.online && !player.dnf)
            .or_else(|| self.players.iter().find(|player| !player.dnf))
        {
            self.host_player_id = next_host.player_id.clone();
        }
    }

    fn advance_phase(&mut self, now_ms: u64) {
        if self.phase == RoomPhase::Countdown {
            if let Some(start_at_ms) = self.start_at_ms {
                if now_ms >= start_at_ms {
                    self.phase = RoomPhase::Playing;
                }
            }
        }

        if self.phase == RoomPhase::Playing {
            let all_finished = self
                .players
                .iter()
                .filter(|player| !player.dnf)
                .all(|player| player.final_result.is_some());
            if all_finished {
                self.phase = RoomPhase::Finished;
            }
        }
    }

    fn server_tick(&self, now_ms: u64) -> i64 {
        let Some(start_at_ms) = self.start_at_ms else {
            return 0;
        };
        if now_ms <= start_at_ms {
            return 0;
        }
        now_ms.saturating_sub(start_at_ms).saturating_mul(1000) as i64
    }

    fn snapshot(&self, now_ms: u64) -> RoomSnapshot {
        RoomSnapshot {
            room_code: self.room_code.clone(),
            phase: self.phase,
            start_at_ms: self.start_at_ms,
            server_tick: self.server_tick(now_ms),
            song: self.song.clone(),
            players: self
                .players
                .iter()
                .map(|player| RoomPlayerSnapshot {
                    player_id: player.player_id.clone(),
                    name: player.name.clone(),
                    is_host: player.player_id == self.host_player_id,
                    online: player.online,
                    dnf: player.dnf,
                    ready: player.ready,
                    last_seq: (player.last_state_seq > 0).then_some(player.last_state_seq),
                    last_state: player.last_state.clone(),
                    final_result: player.final_result.clone(),
                })
                .collect(),
            spectators: self
                .spectators
                .iter()
                .map(|spectator| RoomSpectatorSnapshot {
                    spectator_id: spectator.spectator_id.clone(),
                    name: spectator.name.clone(),
                    online: spectator.online,
                })
                .collect(),
        }
    }

    fn recipient_session_ids(&self) -> Vec<u64> {
        let mut sessions = Vec::with_capacity(self.players.len() + self.spectators.len());
        sessions.extend(
            self.players
                .iter()
                .filter(|player| player.online)
                .map(|player| player.session_id),
        );
        sessions.extend(
            self.spectators
                .iter()
                .filter(|spectator| spectator.online)
                .map(|spectator| spectator.session_id),
        );
        sessions
    }

    fn online_participant_count(&self) -> usize {
        self.players.iter().filter(|player| player.online).count()
            + self
                .spectators
                .iter()
                .filter(|spectator| spectator.online)
                .count()
    }
}

struct LibraryIndex {
    songs: HashMap<(String, usize), MatchSongSelection>,
}

impl LibraryIndex {
    fn from_library(library: &ResourceLibraryDocument) -> Self {
        let mut songs = HashMap::new();
        for song in &library.songs {
            for course in &song.courses {
                songs.insert(
                    (song.source_id.clone(), course.index),
                    MatchSongSelection {
                        source_id: song.source_id.clone(),
                        course_index: course.index,
                        title: song.title.clone(),
                        subtitle: song.subtitle.clone(),
                        artist: song.artist.clone(),
                        chart_content_hash: song.chart_content_hash.clone(),
                        audio_content_hash: song.audio_content_hash.clone(),
                    },
                );
            }
        }
        Self { songs }
    }

    fn lookup(&self, source_id: &str, course_index: usize) -> Option<MatchSongSelection> {
        self.songs
            .get(&(source_id.to_owned(), course_index))
            .cloned()
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use taiko_multiplayer_protocol::{
        ClientMessage, HostSelectSongRequest, JoinRoomRequest, ReadyRequest, RoomPhase,
        ServerMessage,
    };
    use taiko_resource_protocol::{
        ResourceCourse, ResourceLibraryDocument, ResourceSong, API_VERSION,
    };
    use tokio::sync::mpsc::unbounded_channel;

    fn sample_library() -> ResourceLibraryDocument {
        ResourceLibraryDocument {
            api_version: API_VERSION,
            warnings: Vec::new(),
            songs: vec![ResourceSong {
                source_path: "song/a.tja".to_owned(),
                source_id: "chart-1".to_owned(),
                chart_content_hash: "c".repeat(64),
                audio_path: "song/a.ogg".to_owned(),
                audio_id: "audio-1".to_owned(),
                audio_content_hash: "a".repeat(64),
                title: "A".to_owned(),
                subtitle: String::new(),
                artist: "artist".to_owned(),
                demo_start_seconds: 0.0,
                courses: vec![ResourceCourse {
                    index: 0,
                    name: "Oni".to_owned(),
                    level: Some(10),
                    object_count: 1,
                    branch_segment_count: 0,
                    base_bpm: Some(180.0),
                    branch_decisions: Vec::new(),
                }],
            }],
        }
    }

    fn drain_messages(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMessage>,
    ) -> Vec<ServerMessage> {
        let mut out = Vec::new();
        while let Ok(message) = rx.try_recv() {
            out.push(message);
        }
        out
    }

    #[tokio::test]
    async fn create_join_ready_and_countdown_flow() {
        let registry = MultiplayerRegistry::new(&sample_library());

        let (tx1, mut rx1) = unbounded_channel();
        let (tx2, mut rx2) = unbounded_channel();
        let s1 = registry.register_session(tx1).await;
        let s2 = registry.register_session(tx2).await;

        registry
            .handle_client_message(
                s1,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "host".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(s1, ClientMessage::CreateRoom)
            .await;

        let room_code = drain_messages(&mut rx1)
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::RoomCreated(created) => Some(created.room_code),
                _ => None,
            })
            .expect("room code");

        registry
            .handle_client_message(
                s2,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "guest".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(
                s2,
                ClientMessage::JoinRoom(JoinRoomRequest {
                    room_code: room_code.clone(),
                    spectate: false,
                }),
            )
            .await;

        registry
            .handle_client_message(
                s1,
                ClientMessage::HostSelectSong(HostSelectSongRequest {
                    source_id: "chart-1".to_owned(),
                    course_index: 0,
                }),
            )
            .await;

        registry
            .handle_client_message(s1, ClientMessage::Ready(ReadyRequest { ready: true }))
            .await;
        registry
            .handle_client_message(s2, ClientMessage::Ready(ReadyRequest { ready: true }))
            .await;

        let host_messages = drain_messages(&mut rx1);
        assert!(host_messages
            .iter()
            .any(|message| matches!(message, ServerMessage::MatchCountdown(_))));

        tokio::time::sleep(Duration::from_millis(2)).await;
        registry
            .handle_client_message(
                s1,
                ClientMessage::Ping(taiko_multiplayer_protocol::PingPayload {
                    nonce: 1,
                    client_send_ms: None,
                    server_send_ms: None,
                }),
            )
            .await;

        let guest_messages = drain_messages(&mut rx2);
        assert!(guest_messages
            .iter()
            .any(|message| matches!(message, ServerMessage::MatchStarted(_))));
        assert!(guest_messages.iter().any(|message| {
            matches!(
                message,
                ServerMessage::RoomSnapshot(snapshot) if snapshot.phase == RoomPhase::Playing
            )
        }));
    }

    #[tokio::test]
    async fn created_room_code_is_four_chars_from_room_alphabet() {
        let registry = MultiplayerRegistry::new(&sample_library());
        let (tx, mut rx) = unbounded_channel();
        let session_id = registry.register_session(tx).await;

        registry
            .handle_client_message(
                session_id,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "host".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(session_id, ClientMessage::CreateRoom)
            .await;

        let room_code = drain_messages(&mut rx)
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::RoomCreated(created) => Some(created.room_code),
                _ => None,
            })
            .expect("room code");

        assert_eq!(room_code.len(), ROOM_CODE_LEN);
        assert!(room_code
            .bytes()
            .all(|byte| ROOM_CODE_ALPHABET.contains(&byte)));
    }

    #[tokio::test]
    async fn lobby_host_disconnect_transfers_host() {
        let registry = MultiplayerRegistry::new(&sample_library());

        let (tx1, mut rx1) = unbounded_channel();
        let (tx2, mut rx2) = unbounded_channel();
        let s1 = registry.register_session(tx1).await;
        let s2 = registry.register_session(tx2).await;

        registry
            .handle_client_message(
                s1,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "host".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(s1, ClientMessage::CreateRoom)
            .await;
        let room_code = drain_messages(&mut rx1)
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::RoomCreated(created) => Some(created.room_code),
                _ => None,
            })
            .expect("room code");

        registry
            .handle_client_message(
                s2,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "guest".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(
                s2,
                ClientMessage::JoinRoom(JoinRoomRequest {
                    room_code,
                    spectate: false,
                }),
            )
            .await;

        registry.remove_session(s1).await;

        let messages = drain_messages(&mut rx2);
        let latest_snapshot = messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ServerMessage::RoomSnapshot(snapshot) => Some(snapshot),
                _ => None,
            })
            .expect("snapshot");

        assert!(latest_snapshot
            .players
            .iter()
            .any(|player| player.name == "guest" && player.is_host));
    }

    #[tokio::test]
    async fn stale_player_state_seq_is_ignored() {
        let registry = MultiplayerRegistry::new(&sample_library());

        let (tx1, mut rx1) = unbounded_channel();
        let (tx2, mut rx2) = unbounded_channel();
        let s1 = registry.register_session(tx1).await;
        let s2 = registry.register_session(tx2).await;

        registry
            .handle_client_message(
                s1,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "host".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(s1, ClientMessage::CreateRoom)
            .await;
        let room_code = drain_messages(&mut rx1)
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::RoomCreated(created) => Some(created.room_code),
                _ => None,
            })
            .expect("room code");

        registry
            .handle_client_message(
                s2,
                ClientMessage::Hello(ClientHello {
                    protocol_version: PROTOCOL_VERSION,
                    name: "guest".to_owned(),
                }),
            )
            .await;
        registry
            .handle_client_message(
                s2,
                ClientMessage::JoinRoom(JoinRoomRequest {
                    room_code,
                    spectate: false,
                }),
            )
            .await;
        registry
            .handle_client_message(
                s1,
                ClientMessage::HostSelectSong(HostSelectSongRequest {
                    source_id: "chart-1".to_owned(),
                    course_index: 0,
                }),
            )
            .await;
        registry
            .handle_client_message(s1, ClientMessage::Ready(ReadyRequest { ready: true }))
            .await;
        registry
            .handle_client_message(s2, ClientMessage::Ready(ReadyRequest { ready: true }))
            .await;
        tokio::time::sleep(Duration::from_millis(2)).await;

        let mut score = rhythm_mode_taiko::TaikoScoreState::default();
        score.score = 123;
        let frame = rhythm_mode_taiko::TaikoFrameView {
            now: 1_000,
            notes: Vec::new(),
            bar_lines: Vec::new(),
            score: 123,
            combo: 1,
            gauge: 0.1,
        };

        registry
            .handle_client_message(
                s1,
                ClientMessage::PlayerStateUpdate(PlayerStateUpdate {
                    seq: 2,
                    now_tick: 1_000,
                    score: score.clone(),
                    frame_view: frame.clone(),
                    recent_judges: Vec::new(),
                    replay_hash: 1,
                }),
            )
            .await;
        registry
            .handle_client_message(
                s1,
                ClientMessage::PlayerStateUpdate(PlayerStateUpdate {
                    seq: 1,
                    now_tick: 900,
                    score,
                    frame_view: frame,
                    recent_judges: Vec::new(),
                    replay_hash: 1,
                }),
            )
            .await;

        let state_messages = drain_messages(&mut rx2)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::PlayerStateUpdate(update) => Some(update),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(state_messages.len(), 1);
        assert_eq!(state_messages[0].state.seq, 2);
    }
}
