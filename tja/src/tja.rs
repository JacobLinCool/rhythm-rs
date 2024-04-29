#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::note::TaikoNote;

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct TJA {
    pub header: TJAHeader,
    pub courses: Vec<TJACourse>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct TJAHeader {
    /// The title of the song.
    pub title: Option<String>,
    /// The subtitle of the song.
    pub subtitle: Option<String>,
    /// The BPM of the song.
    pub bpm: Option<f32>,
    /// The wave file of the song.
    pub wave: Option<String>,
    /// The offset of the song.
    pub offset: Option<f32>,
    /// The demo start of the song.
    pub demostart: Option<f32>,
    /// The song volume in percentage.
    pub songvol: Option<i32>,
    /// The sound effect volume in percentage.
    pub sevol: Option<i32>,
    /// The style of the song. Should be either "Single" or "Couple".
    pub style: Option<String>,

    /// The genre of the song.
    pub genre: Option<String>,
    /// The artist of the song.
    pub artist: Option<String>,
}

impl Default for TJAHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl TJAHeader {
    pub fn new() -> Self {
        Self {
            title: None,
            subtitle: None,
            bpm: None,
            wave: None,
            offset: None,
            demostart: None,
            songvol: None,
            sevol: None,
            style: None,
            genre: None,
            artist: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg(feature = "serde")]
#[derive(Serialize, Deserialize)]
pub struct TJACourse {
    pub course: i32,
    pub level: Option<i32>,
    pub scoreinit: Option<i32>,
    pub scorediff: Option<i32>,
    pub notes: Vec<TaikoNote>,
}

impl TJACourse {
    pub fn new(course: i32) -> Self {
        Self {
            course,
            level: None,
            scoreinit: None,
            scorediff: None,
            notes: Vec::new(),
        }
    }
}
