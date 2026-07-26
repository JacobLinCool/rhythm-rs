use rhythm_mode_taiko::TaikoAction;
use serde::{Deserialize, Serialize};

use crate::controller::ControllerSlot;

pub(super) const PROTOCOL_VERSION: u16 = 1;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ClientMessage {
    Pair {
        protocol: u16,
        token: String,
    },
    Resume {
        protocol: u16,
        session_token: String,
    },
    ReadyAck {
        connection_id: u64,
    },
    Hit {
        seq: u64,
        action: WireAction,
    },
}

impl std::fmt::Debug for ClientMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pair { protocol, .. } => formatter
                .debug_struct("Pair")
                .field("protocol", protocol)
                .field("token", &"[REDACTED]")
                .finish(),
            Self::Resume { protocol, .. } => formatter
                .debug_struct("Resume")
                .field("protocol", protocol)
                .field("session_token", &"[REDACTED]")
                .finish(),
            Self::ReadyAck { connection_id } => formatter
                .debug_struct("ReadyAck")
                .field("connection_id", connection_id)
                .finish(),
            Self::Hit { seq, action } => formatter
                .debug_struct("Hit")
                .field("seq", seq)
                .field("action", action)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WireAction {
    LeftKat,
    LeftDon,
    RightDon,
    RightKat,
}

impl WireAction {
    pub(super) const fn into_action(self) -> TaikoAction {
        match self {
            Self::LeftKat => TaikoAction::LEFT_KAT,
            Self::LeftDon => TaikoAction::LEFT_DON,
            Self::RightDon => TaikoAction::RIGHT_DON,
            Self::RightKat => TaikoAction::RIGHT_KAT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WireSlot {
    P1,
    P2,
}

impl From<ControllerSlot> for WireSlot {
    fn from(value: ControllerSlot) -> Self {
        match value {
            ControllerSlot::One => Self::P1,
            ControllerSlot::Two => Self::P2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ErrorCode {
    InvalidHandshake,
    Unauthorized,
    InvalidMessage,
    OutOfSequence,
    RateLimited,
    QueueFull,
    ServerStopping,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum ServerMessage {
    Ready {
        protocol: u16,
        slot: WireSlot,
        connection_id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_token: Option<String>,
        commit_required: bool,
    },
    Ack {
        next_seq: u64,
        accepted: bool,
    },
    Error {
        code: ErrorCode,
    },
}

impl std::fmt::Debug for ServerMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready {
                protocol,
                slot,
                connection_id,
                session_token,
                commit_required,
            } => formatter
                .debug_struct("Ready")
                .field("protocol", protocol)
                .field("slot", slot)
                .field("connection_id", connection_id)
                .field(
                    "session_token",
                    &session_token.as_ref().map(|_| "[REDACTED]"),
                )
                .field("commit_required", commit_required)
                .finish(),
            Self::Ack { next_seq, accepted } => formatter
                .debug_struct("Ack")
                .field("next_seq", next_seq)
                .field("accepted", accepted)
                .finish(),
            Self::Error { code } => formatter.debug_struct("Error").field("code", code).finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_actions_preserve_all_four_physical_strikes() {
        for (wire, expected) in [
            (WireAction::LeftKat, TaikoAction::LEFT_KAT),
            (WireAction::LeftDon, TaikoAction::LEFT_DON),
            (WireAction::RightDon, TaikoAction::RIGHT_DON),
            (WireAction::RightKat, TaikoAction::RIGHT_KAT),
        ] {
            assert_eq!(wire.into_action(), expected);
        }
    }

    #[test]
    fn client_messages_reject_unknown_fields_timestamps_and_slots() {
        let valid = r#"{"type":"hit","seq":1,"action":"left_don"}"#;
        assert!(matches!(
            serde_json::from_str::<ClientMessage>(valid),
            Ok(ClientMessage::Hit {
                seq: 1,
                action: WireAction::LeftDon
            })
        ));

        for invalid in [
            r#"{"type":"hit","seq":1,"action":"left_don","timestamp":123}"#,
            r#"{"type":"hit","seq":1,"action":"left_don","slot":"p2"}"#,
            r#"{"type":"hit","seq":1,"action":"don"}"#,
            r#"{"type":"pair","protocol":1,"token":"x","extra":true}"#,
            r#"{"type":"resume","protocol":1}"#,
            r#"{"type":"ready_ack","connection_id":1,"extra":true}"#,
        ] {
            assert!(
                serde_json::from_str::<ClientMessage>(invalid).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn secret_bearing_messages_have_redacted_debug_output() {
        let pair = ClientMessage::Pair {
            protocol: 1,
            token: "pair-secret".to_owned(),
        };
        let resume = ClientMessage::Resume {
            protocol: 1,
            session_token: "session-secret".to_owned(),
        };

        let pair_debug = format!("{pair:?}");
        let resume_debug = format!("{resume:?}");
        assert!(!pair_debug.contains("pair-secret"));
        assert!(!resume_debug.contains("session-secret"));
        assert!(pair_debug.contains("[REDACTED]"));
        assert!(resume_debug.contains("[REDACTED]"));
    }

    #[test]
    fn server_ready_shape_omits_absent_session_token() {
        let resumed = ServerMessage::Ready {
            protocol: PROTOCOL_VERSION,
            slot: WireSlot::P2,
            connection_id: 7,
            session_token: None,
            commit_required: false,
        };
        assert_eq!(
            serde_json::to_string(&resumed).expect("serialize"),
            r#"{"type":"ready","protocol":1,"slot":"p2","connection_id":7,"commit_required":false}"#
        );
    }

    #[test]
    fn server_ready_debug_redacts_session_token() {
        let ready = ServerMessage::Ready {
            protocol: PROTOCOL_VERSION,
            slot: WireSlot::P1,
            connection_id: 8,
            session_token: Some("session-secret".to_owned()),
            commit_required: true,
        };
        let debug = format!("{ready:?}");
        assert!(!debug.contains("session-secret"));
        assert!(debug.contains("[REDACTED]"));
    }
}
