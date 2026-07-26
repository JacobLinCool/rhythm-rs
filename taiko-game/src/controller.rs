use std::time::Instant;

use rhythm_mode_taiko::TaikoAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ControllerSlot {
    One,
    Two,
}

impl ControllerSlot {
    pub(crate) const ALL: [Self; 2] = [Self::One, Self::Two];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::One => 0,
            Self::Two => 1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerSource {
    Keyboard,
    MacTrackpadContact,
    TerminalPointer,
    Lan { connection_id: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControllerStrike {
    pub(crate) slot: ControllerSlot,
    pub(crate) source: ControllerSource,
    pub(crate) action: TaikoAction,
    pub(crate) observed_at: Instant,
    pub(crate) sequence: Option<u64>,
    pub(crate) generation: Option<u64>,
}

impl ControllerStrike {
    pub(crate) const fn local(
        slot: ControllerSlot,
        source: ControllerSource,
        action: TaikoAction,
        observed_at: Instant,
    ) -> Self {
        Self {
            slot,
            source,
            action,
            observed_at,
            sequence: None,
            generation: None,
        }
    }

    pub(crate) const fn lan(
        slot: ControllerSlot,
        connection_id: u64,
        action: TaikoAction,
        observed_at: Instant,
        sequence: u64,
        generation: u64,
    ) -> Self {
        Self {
            slot,
            source: ControllerSource::Lan { connection_id },
            action,
            observed_at,
            sequence: Some(sequence),
            generation: Some(generation),
        }
    }
}
