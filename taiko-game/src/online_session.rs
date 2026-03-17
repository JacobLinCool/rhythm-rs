use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use taiko_multiplayer_protocol::{
    ClientMessage, FinalResultReport, PingPayload, PlayerStateUpdate, RoomPhase, RoomRole,
    RoomSnapshot, ServerMessage,
};

use rhythm_core::Tick;
use rhythm_mode_taiko::{TaikoAction, TaikoJudge};

use crate::online::{
    now_unix_ms, ClockSyncState, LobbySubState, LocalPlayerRuntime, NetworkClient, NetworkEvent,
    PreparedMatch,
};

const PING_INTERVAL: Duration = Duration::from_millis(800);
const HIT_FLASH_TICKS: Tick = 200_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct RemoteFlash {
    pub(crate) judge: Option<TaikoJudge>,
    pub(crate) judge_until: Tick,
    pub(crate) input_action: Option<TaikoAction>,
    pub(crate) input_until: Tick,
}

#[derive(Debug, Clone)]
pub(crate) enum SessionAction {
    SongSelected {
        source_id: String,
        course_index: usize,
    },
    PhaseChanged(RoomPhase),
}

pub(crate) struct OnlineSession {
    pub(crate) network: NetworkClient,
    pub(crate) room_code: Option<String>,
    pub(crate) actor_id: Option<String>,
    pub(crate) role: Option<RoomRole>,
    pub(crate) snapshot: Option<RoomSnapshot>,
    pub(crate) live_states: HashMap<String, PlayerStateUpdate>,
    pub(crate) final_results: HashMap<String, FinalResultReport>,
    pub(crate) ready: bool,
    pub(crate) status_message: String,
    pub(crate) error_message: Option<String>,

    pub(crate) clock_sync: ClockSyncState,
    pub(crate) local_unix_base_ms: u64,
    pub(crate) local_mono_base: Instant,
    pub(crate) last_ping_sent: Instant,

    pub(crate) prepared_match: Option<PreparedMatch>,
    pub(crate) local_player: Option<LocalPlayerRuntime>,

    pub(crate) remote_flashes: HashMap<String, RemoteFlash>,
    pub(crate) lobby_sub_state: LobbySubState,
    pub(crate) local_course_index: usize,
    pub(crate) host_course_index: usize,
    pub(crate) pending_actions: Vec<SessionAction>,
}

impl OnlineSession {
    pub(crate) fn new(network: NetworkClient) -> Self {
        let local_unix_base_ms = now_unix_ms();
        let local_mono_base = Instant::now();

        Self {
            network,
            room_code: None,
            actor_id: None,
            role: None,
            snapshot: None,
            live_states: HashMap::new(),
            final_results: HashMap::new(),
            ready: false,
            status_message: "connecting...".to_owned(),
            error_message: None,
            clock_sync: ClockSyncState::default(),
            local_unix_base_ms,
            local_mono_base,
            last_ping_sent: Instant::now(),
            prepared_match: None,
            local_player: None,
            remote_flashes: HashMap::new(),
            lobby_sub_state: LobbySubState::BrowsingSongs,
            local_course_index: 0,
            host_course_index: 0,
            pending_actions: Vec::new(),
        }
    }

    pub(crate) fn local_now_ms(&self) -> u64 {
        self.local_unix_base_ms
            .saturating_add(self.local_mono_base.elapsed().as_millis() as u64)
    }

    pub(crate) fn estimated_server_now_ms(&self) -> u64 {
        self.clock_sync.estimated_server_now_ms(self.local_now_ms())
    }

    pub(crate) fn current_phase(&self) -> RoomPhase {
        self.snapshot
            .as_ref()
            .map(|s| s.phase)
            .unwrap_or(RoomPhase::Lobby)
    }

    pub(crate) fn estimated_server_tick(&self) -> rhythm_core::Tick {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return 0;
        };

        let Some(start_at_ms) = snapshot.start_at_ms else {
            return snapshot.server_tick;
        };

        let server_now_ms = self.estimated_server_now_ms();
        if server_now_ms <= start_at_ms {
            return 0;
        }
        server_now_ms
            .saturating_sub(start_at_ms)
            .saturating_mul(1000) as rhythm_core::Tick
    }

    pub(crate) fn is_local_host(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                let actor = self.actor_id.as_deref()?;
                snapshot
                    .players
                    .iter()
                    .find(|p| p.player_id == actor)
                    .map(|p| p.is_host)
            })
            .unwrap_or(false)
    }

    /// Poll network events and update clock sync. Call every tick.
    pub(crate) fn tick_network(&mut self) -> Result<()> {
        let local_now_ms = self.local_now_ms();
        self.clock_sync.tick(local_now_ms);

        while let Some(event) = self.network.try_recv()? {
            match event {
                NetworkEvent::Server(message) => self.handle_server_message(*message)?,
                NetworkEvent::Closed(reason) => {
                    self.error_message = Some(reason);
                    return Ok(());
                }
            }
        }

        if self.last_ping_sent.elapsed() >= PING_INTERVAL {
            let nonce = local_now_ms;
            self.network
                .send(ClientMessage::Ping(PingPayload {
                    nonce,
                    client_send_ms: Some(local_now_ms),
                    server_send_ms: None,
                }))
                .ok();
            self.last_ping_sent = Instant::now();
        }

        Ok(())
    }

    fn handle_server_message(&mut self, message: ServerMessage) -> Result<()> {
        match message {
            ServerMessage::Hello(hello) => {
                self.status_message = format!(
                    "connected (protocol={}, session={})",
                    hello.protocol_version, hello.session.session_id
                );
            }
            ServerMessage::Error(error) => {
                let fatal = matches!(
                    error.code.as_str(),
                    "unsupported_protocol"
                        | "protocol_violation"
                        | "invalid_name"
                        | "room_not_found"
                        | "room_full"
                        | "already_joined"
                        | "unknown_session"
                );
                if fatal {
                    self.error_message = Some(format!("{}: {}", error.code, error.message));
                } else {
                    self.status_message = format!("error: {}: {}", error.code, error.message);
                }
            }
            ServerMessage::RoomCreated(created) => {
                self.room_code = Some(created.room_code.clone());
                self.actor_id = Some(created.player_id);
                self.role = Some(RoomRole::Player);
                self.status_message = format!("room created: {}", created.room_code);
            }
            ServerMessage::RoomJoined(joined) => {
                self.room_code = Some(joined.room_code.clone());
                self.actor_id = Some(joined.actor_id);
                self.role = Some(joined.role);
                self.status_message =
                    format!("joined room {} as {:?}", joined.room_code, joined.role);
            }
            ServerMessage::RoomSnapshot(snapshot) => {
                self.ingest_snapshot(snapshot);
            }
            ServerMessage::SongSelected(selection) => {
                self.status_message = format!(
                    "song selected: {} [{}]",
                    selection.title, selection.course_index
                );
                self.pending_actions.push(SessionAction::SongSelected {
                    source_id: selection.source_id.clone(),
                    course_index: selection.course_index,
                });
            }
            ServerMessage::MatchCountdown(countdown) => {
                self.status_message = format!("match countdown: {} ms", countdown.start_at_ms);
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.phase = RoomPhase::Countdown;
                    snapshot.start_at_ms = Some(countdown.start_at_ms);
                    snapshot.song = Some(countdown.song.clone());
                }
                self.pending_actions
                    .push(SessionAction::PhaseChanged(RoomPhase::Countdown));
            }
            ServerMessage::MatchStarted(started) => {
                self.status_message = "match started".to_owned();
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.phase = RoomPhase::Playing;
                    snapshot.start_at_ms = Some(started.start_at_ms);
                }
                self.pending_actions
                    .push(SessionAction::PhaseChanged(RoomPhase::Playing));
            }
            ServerMessage::InputEvent(envelope) => {
                let flash = self
                    .remote_flashes
                    .entry(envelope.player_id.clone())
                    .or_insert(RemoteFlash {
                        judge: None,
                        judge_until: 0,
                        input_action: None,
                        input_until: 0,
                    });
                flash.input_action = Some(envelope.event.action);
                flash.input_until = envelope.event.tick.saturating_add(HIT_FLASH_TICKS);
            }
            ServerMessage::PlayerStateUpdate(update) => {
                if let Some(judge) = latest_flashable_judge(&update.state.recent_judges) {
                    let flash = self
                        .remote_flashes
                        .entry(update.player_id.clone())
                        .or_insert(RemoteFlash {
                            judge: None,
                            judge_until: 0,
                            input_action: None,
                            input_until: 0,
                        });
                    flash.judge = Some(judge);
                    flash.judge_until = update.state.now_tick.saturating_add(HIT_FLASH_TICKS);
                }
                self.live_states
                    .insert(update.player_id.clone(), update.state.clone());
            }
            ServerMessage::FinalResult(final_result) => {
                self.final_results
                    .insert(final_result.player_id.clone(), final_result.report.clone());
            }
            ServerMessage::Pong(pong) => {
                if let (Some(client_send_ms), Some(server_send_ms)) =
                    (pong.client_send_ms, pong.server_send_ms)
                {
                    let client_receive_ms = self.local_now_ms();
                    self.clock_sync.observe_sample(
                        client_send_ms,
                        client_receive_ms,
                        server_send_ms,
                    );
                }
            }
        }
        Ok(())
    }

    fn ingest_snapshot(&mut self, snapshot: RoomSnapshot) {
        self.room_code = Some(snapshot.room_code.clone());
        self.ready = self
            .actor_id
            .as_deref()
            .and_then(|actor| {
                snapshot
                    .players
                    .iter()
                    .find(|p| p.player_id == actor)
                    .map(|p| p.ready)
            })
            .unwrap_or(false);

        for player in &snapshot.players {
            if let Some(state) = &player.last_state {
                self.live_states
                    .insert(player.player_id.clone(), state.clone());
            }
            if let Some(result) = &player.final_result {
                self.final_results
                    .insert(player.player_id.clone(), result.clone());
            }
        }

        self.snapshot = Some(snapshot);
    }

    pub(crate) fn is_disconnected(&self) -> bool {
        self.error_message.is_some()
    }

    /// Get the current flash state for a remote player, expiring stale entries.
    pub(crate) fn remote_flash_for(&self, player_id: &str, now_tick: Tick) -> Option<&RemoteFlash> {
        self.remote_flashes
            .get(player_id)
            .filter(|f| f.judge_until > now_tick || f.input_until > now_tick)
    }
}

fn latest_flashable_judge(judges: &[TaikoJudge]) -> Option<TaikoJudge> {
    judges.iter().rev().copied().find(|judge| {
        matches!(
            judge,
            TaikoJudge::Great { .. }
                | TaikoJudge::Ok { .. }
                | TaikoJudge::Miss { .. }
                | TaikoJudge::MissExpired
        )
    })
}
