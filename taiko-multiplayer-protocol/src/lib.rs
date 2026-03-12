use rhythm_mode_taiko::{
    TaikoAction, TaikoFinalResult, TaikoFrameView, TaikoJudge, TaikoScoreState,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

pub type Tick = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomPhase {
    Lobby,
    Countdown,
    Playing,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSongSelection {
    pub source_id: String,
    pub course_index: usize,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub chart_content_hash: String,
    pub audio_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    pub seq: u64,
    pub tick: Tick,
    pub action: TaikoAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerStateUpdate {
    pub seq: u64,
    pub now_tick: Tick,
    pub score: TaikoScoreState,
    pub frame_view: TaikoFrameView,
    pub recent_judges: Vec<TaikoJudge>,
    pub replay_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinalResultReport {
    pub seq: u64,
    pub finish_tick: Tick,
    pub replay_hash: u64,
    pub result: TaikoFinalResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingPayload {
    pub nonce: u64,
    pub client_send_ms: Option<u64>,
    pub server_send_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub protocol_version: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRoomRequest {
    pub room_code: String,
    pub spectate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyRequest {
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSelectSongRequest {
    pub source_id: String,
    pub course_index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomPlayerSnapshot {
    pub player_id: String,
    pub name: String,
    pub is_host: bool,
    pub online: bool,
    pub dnf: bool,
    pub ready: bool,
    pub last_seq: Option<u64>,
    pub last_state: Option<PlayerStateUpdate>,
    pub final_result: Option<FinalResultReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSpectatorSnapshot {
    pub spectator_id: String,
    pub name: String,
    pub online: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomSnapshot {
    pub room_code: String,
    pub phase: RoomPhase,
    pub start_at_ms: Option<u64>,
    pub server_tick: Tick,
    pub song: Option<MatchSongSelection>,
    pub players: Vec<RoomPlayerSnapshot>,
    pub spectators: Vec<RoomSpectatorSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSessionInfo {
    pub session_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    Player,
    Spectator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomCreated {
    pub session: ClientSessionInfo,
    pub room_code: String,
    pub player_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomJoined {
    pub session: ClientSessionInfo,
    pub room_code: String,
    pub role: RoomRole,
    pub actor_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub protocol_version: u32,
    pub session: ClientSessionInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchCountdown {
    pub room_code: String,
    pub start_at_ms: u64,
    pub song: MatchSongSelection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchStarted {
    pub room_code: String,
    pub start_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerInputEnvelope {
    pub player_id: String,
    pub event: InputEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerStateEnvelope {
    pub player_id: String,
    pub state: PlayerStateUpdate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerFinalResultEnvelope {
    pub player_id: String,
    pub report: FinalResultReport,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ClientMessage {
    Hello(ClientHello),
    CreateRoom,
    JoinRoom(JoinRoomRequest),
    Ready(ReadyRequest),
    HostSelectSong(HostSelectSongRequest),
    StartMatch,
    InputEvent(InputEvent),
    PlayerStateUpdate(PlayerStateUpdate),
    FinalResult(FinalResultReport),
    Ping(PingPayload),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello(ServerHello),
    Error(ServerError),
    RoomCreated(RoomCreated),
    RoomJoined(RoomJoined),
    RoomSnapshot(RoomSnapshot),
    SongSelected(MatchSongSelection),
    MatchCountdown(MatchCountdown),
    MatchStarted(MatchStarted),
    InputEvent(PlayerInputEnvelope),
    PlayerStateUpdate(PlayerStateEnvelope),
    FinalResult(PlayerFinalResultEnvelope),
    Pong(PingPayload),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_song() -> MatchSongSelection {
        MatchSongSelection {
            source_id: "chart-1".to_owned(),
            course_index: 2,
            title: "Song".to_owned(),
            subtitle: "Sub".to_owned(),
            artist: "Artist".to_owned(),
            chart_content_hash: "c".repeat(64),
            audio_content_hash: "a".repeat(64),
        }
    }

    fn sample_state_update(seq: u64) -> PlayerStateUpdate {
        PlayerStateUpdate {
            seq,
            now_tick: 1_234_000,
            score: TaikoScoreState::default(),
            frame_view: TaikoFrameView {
                now: 1_234_000,
                notes: Vec::new(),
                bar_lines: Vec::new(),
                score: 0,
                combo: 0,
                gauge: 0.0,
            },
            recent_judges: Vec::new(),
            replay_hash: 123,
        }
    }

    #[test]
    fn client_message_roundtrip() {
        let messages = vec![
            ClientMessage::Hello(ClientHello {
                protocol_version: PROTOCOL_VERSION,
                name: "alice".to_owned(),
            }),
            ClientMessage::CreateRoom,
            ClientMessage::JoinRoom(JoinRoomRequest {
                room_code: "ABC123".to_owned(),
                spectate: false,
            }),
            ClientMessage::Ready(ReadyRequest { ready: true }),
            ClientMessage::HostSelectSong(HostSelectSongRequest {
                source_id: "chart-1".to_owned(),
                course_index: 1,
            }),
            ClientMessage::StartMatch,
            ClientMessage::InputEvent(InputEvent {
                seq: 1,
                tick: 100,
                action: TaikoAction::Don,
            }),
            ClientMessage::PlayerStateUpdate(sample_state_update(2)),
            ClientMessage::FinalResult(FinalResultReport {
                seq: 3,
                finish_tick: 9_000_000,
                replay_hash: 42,
                result: TaikoFinalResult {
                    score: 1,
                    max_combo: 2,
                    gauge: 0.9,
                    pass_threshold: 0.8,
                    great: 3,
                    ok: 4,
                    miss: 5,
                    roll_hits: 6,
                    passed: true,
                },
            }),
            ClientMessage::Ping(PingPayload {
                nonce: 99,
                client_send_ms: Some(1_000),
                server_send_ms: None,
            }),
        ];

        for message in messages {
            let raw = serde_json::to_vec(&message).expect("serialize");
            let decoded: ClientMessage = serde_json::from_slice(&raw).expect("deserialize");
            let normalized = serde_json::to_vec(&decoded).expect("re-serialize");
            assert_eq!(normalized, raw);
        }
    }

    #[test]
    fn server_message_roundtrip() {
        let messages = vec![
            ServerMessage::Hello(ServerHello {
                protocol_version: PROTOCOL_VERSION,
                session: ClientSessionInfo {
                    session_id: "sess-1".to_owned(),
                    name: "alice".to_owned(),
                },
            }),
            ServerMessage::Error(ServerError {
                code: "bad_request".to_owned(),
                message: "invalid".to_owned(),
            }),
            ServerMessage::RoomCreated(RoomCreated {
                session: ClientSessionInfo {
                    session_id: "sess-1".to_owned(),
                    name: "alice".to_owned(),
                },
                room_code: "ABC123".to_owned(),
                player_id: "p1".to_owned(),
            }),
            ServerMessage::RoomJoined(RoomJoined {
                session: ClientSessionInfo {
                    session_id: "sess-2".to_owned(),
                    name: "bob".to_owned(),
                },
                room_code: "ABC123".to_owned(),
                role: RoomRole::Player,
                actor_id: "p2".to_owned(),
            }),
            ServerMessage::RoomSnapshot(RoomSnapshot {
                room_code: "ABC123".to_owned(),
                phase: RoomPhase::Countdown,
                start_at_ms: Some(1_000),
                server_tick: 0,
                song: Some(sample_song()),
                players: vec![RoomPlayerSnapshot {
                    player_id: "p1".to_owned(),
                    name: "alice".to_owned(),
                    is_host: true,
                    online: true,
                    dnf: false,
                    ready: true,
                    last_seq: Some(1),
                    last_state: Some(sample_state_update(1)),
                    final_result: None,
                }],
                spectators: vec![RoomSpectatorSnapshot {
                    spectator_id: "s1".to_owned(),
                    name: "eve".to_owned(),
                    online: true,
                }],
            }),
            ServerMessage::SongSelected(sample_song()),
            ServerMessage::MatchCountdown(MatchCountdown {
                room_code: "ABC123".to_owned(),
                start_at_ms: 2_000,
                song: sample_song(),
            }),
            ServerMessage::MatchStarted(MatchStarted {
                room_code: "ABC123".to_owned(),
                start_at_ms: 2_000,
            }),
            ServerMessage::InputEvent(PlayerInputEnvelope {
                player_id: "p1".to_owned(),
                event: InputEvent {
                    seq: 10,
                    tick: 200,
                    action: TaikoAction::Kat,
                },
            }),
            ServerMessage::PlayerStateUpdate(PlayerStateEnvelope {
                player_id: "p1".to_owned(),
                state: sample_state_update(2),
            }),
            ServerMessage::FinalResult(PlayerFinalResultEnvelope {
                player_id: "p1".to_owned(),
                report: FinalResultReport {
                    seq: 3,
                    finish_tick: 9_000_000,
                    replay_hash: 42,
                    result: TaikoFinalResult {
                        score: 1,
                        max_combo: 2,
                        gauge: 0.9,
                        pass_threshold: 0.8,
                        great: 3,
                        ok: 4,
                        miss: 5,
                        roll_hits: 6,
                        passed: true,
                    },
                },
            }),
            ServerMessage::Pong(PingPayload {
                nonce: 99,
                client_send_ms: Some(1_000),
                server_send_ms: Some(1_020),
            }),
        ];

        for message in messages {
            let raw = serde_json::to_vec(&message).expect("serialize");
            let decoded: ServerMessage = serde_json::from_slice(&raw).expect("deserialize");
            let normalized = serde_json::to_vec(&decoded).expect("re-serialize");
            assert_eq!(normalized, raw);
        }
    }
}
