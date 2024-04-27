use std::cmp::Ordering;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// The `Note` trait represents a rhythm note. (combo notes can be seen as a single note with volume > 1)
pub trait Note: std::fmt::Debug + Ord + Clone {
    /// Returns the start time of the note.
    fn start(&self) -> f64;

    /// Returns the duration of the note.
    fn duration(&self) -> f64;

    /// Returns the volume (max hit count) of the note.
    fn volume(&self) -> u16;

    /// Returns the user-defined hit type of the note.
    /// For multi-track rhythm games, this can be used to the track number.
    fn variant(&self) -> impl Into<u16>;

    /// Sets the start time of the note.
    fn set_start(&mut self, start: f64);

    /// Sets the duration of the note.
    fn set_duration(&mut self, duration: f64);

    /// Sets the volume (max hit count) of the note. (combo notes can be seen as a single note with volume > 1
    fn set_volume(&mut self, volume: u16);

    /// Sets the user-defined hit type of the note.
    /// For multi-track rhythm games, this can be used to the track number.
    fn set_variant(&mut self, variant: impl Into<u16>);

    fn cmp(&self, other: &Self) -> Ordering {
        match self
            .start()
            .partial_cmp(&other.start())
            .unwrap_or(Ordering::Equal)
        {
            std::cmp::Ordering::Equal => {
                match (self.start() + self.duration())
                    .partial_cmp(&(other.start() + other.duration()))
                    .unwrap_or(Ordering::Equal)
                {
                    std::cmp::Ordering::Equal => self.variant().into().cmp(&other.variant().into()),
                    other => other,
                }
            }
            other => other,
        }
    }

    /// Checks if the note matches a specific variant.
    fn matches_variant(&self, variant: impl Into<u16>) -> bool {
        self.variant().into() == variant.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct SimpleNote {
    pub start: f64,
    pub duration: f64,
    pub volume: u16,
    pub variant: u16,
}

impl Eq for SimpleNote {}

impl SimpleNote {
    pub fn new(
        start: impl Into<f64>,
        duration: impl Into<f64>,
        volume: impl Into<u16>,
        variant: impl Into<u16>,
    ) -> Self {
        Self {
            start: start.into(),
            duration: duration.into(),
            volume: volume.into(),
            variant: variant.into(),
        }
    }
}

impl Note for SimpleNote {
    fn start(&self) -> f64 {
        self.start
    }
    fn duration(&self) -> f64 {
        self.duration
    }
    fn volume(&self) -> u16 {
        self.volume
    }
    fn variant(&self) -> impl Into<u16> {
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
        self.variant = variant.into();
    }
}

impl Ord for SimpleNote {
    fn cmp(&self, other: &Self) -> Ordering {
        Note::cmp(self, other)
    }
}
