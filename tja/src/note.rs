#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use rhythm_core::Note;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub enum TaikoNoteVariant {
    Don = 0b01,
    Kat = 0b10,
    Both = 0b11,
    Invisible = 0b1111111,
    Unknown = 0,
}

impl From<TaikoNoteVariant> for u16 {
    fn from(val: TaikoNoteVariant) -> Self {
        match val {
            TaikoNoteVariant::Don => 0b01,
            TaikoNoteVariant::Kat => 0b10,
            TaikoNoteVariant::Both => 0b11,
            TaikoNoteVariant::Invisible => 0b1111111,
            TaikoNoteVariant::Unknown => 0,
        }
    }
}

impl From<u16> for TaikoNoteVariant {
    fn from(value: u16) -> Self {
        match value {
            0b01 => TaikoNoteVariant::Don,
            0b10 => TaikoNoteVariant::Kat,
            0b11 => TaikoNoteVariant::Both,
            0b1111111 => TaikoNoteVariant::Invisible,
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

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
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

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct TaikoNote {
    // in milliseconds
    pub start: f64,
    // in milliseconds
    pub duration: f64,
    pub volume: u16,
    pub variant: TaikoNoteVariant,
    #[serde(rename = "type")]
    pub note_type: TaikoNoteType,
    /// Speed is calculated as (bpm * scroll)
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

    #[allow(refining_impl_trait)]
    fn variant(&self) -> TaikoNoteVariant {
        self.variant
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

    fn set_variant(&mut self, variant: impl Into<u16>) {
        self.variant = TaikoNoteVariant::from(variant.into());
    }

    fn matches_variant(&self, variant: impl Into<u16>) -> bool {
        let variant: u16 = variant.into();
        let variant: TaikoNoteVariant = variant.into();
        match variant {
            TaikoNoteVariant::Don => {
                self.variant == TaikoNoteVariant::Don || self.variant == TaikoNoteVariant::Both
            }
            TaikoNoteVariant::Kat => {
                self.variant == TaikoNoteVariant::Kat || self.variant == TaikoNoteVariant::Both
            }
            _ => false,
        }
    }
}

impl Eq for TaikoNote {}

impl Ord for TaikoNote {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.start.partial_cmp(&other.start).unwrap()
    }
}
