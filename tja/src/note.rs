#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use rhythm_core::Note;

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub enum TaikoNoteVariant {
    Don,
    Kat,
    Both,
    Invisible,
    Unknown,
}

impl From<TaikoNoteVariant> for u16 {
    fn from(val: TaikoNoteVariant) -> Self {
        match val {
            TaikoNoteVariant::Don => 0,
            TaikoNoteVariant::Kat => 1,
            TaikoNoteVariant::Both => 2,
            TaikoNoteVariant::Invisible => 100,
            TaikoNoteVariant::Unknown => u16::MAX,
        }
    }
}

impl From<u16> for TaikoNoteVariant {
    fn from(value: u16) -> Self {
        match value {
            0 => TaikoNoteVariant::Don,
            1 => TaikoNoteVariant::Kat,
            2 => TaikoNoteVariant::Both,
            100 => TaikoNoteVariant::Invisible,
            _ => TaikoNoteVariant::Unknown,
        }
    }
}

impl PartialEq<u16> for TaikoNoteVariant {
    fn eq(&self, other: &u16) -> bool {
        *self as u16 == *other
    }
}

impl PartialEq<TaikoNoteVariant> for u16 {
    fn eq(&self, other: &TaikoNoteVariant) -> bool {
        *self == *other as u16
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub enum TaikoNoteType {
    Small,
    Big,
    SmallCombo,
    BigCombo,
    Balloon,
    Yam,
    GogoStart,
    GogoEnd,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct TaikoNote {
    pub start: f64,
    pub duration: f64,
    pub volume: u16,
    pub variant: TaikoNoteVariant,
    #[serde(rename = "type")]
    pub note_type: TaikoNoteType,
    pub speed: f32,
}

impl Note for TaikoNote {
    fn start(&self) -> f64 {
        self.start
    }

    fn duration(&self) -> f64 {
        self.duration
    }

    fn volume(&self) -> u16 {
        self.volume
    }

    fn variant(&self) -> u16 {
        self.variant.into()
    }

    fn set_start(&mut self, start: f64) {
        self.start = start;
    }

    fn set_duration(&mut self, duration: f64) {
        self.duration = duration;
    }

    fn set_volume(&mut self, volume: u16) {
        self.volume = volume;
    }

    fn set_variant(&mut self, variant: u16) {
        self.variant = TaikoNoteVariant::from(variant);
    }
}
