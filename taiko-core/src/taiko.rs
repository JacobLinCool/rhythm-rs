use rhythm_core::{Note, Rhythm};
use serde::{Deserialize, Serialize};
use tja::{TaikoNote, TaikoNoteVariant};

use crate::constant::{
    GUAGE_FULL_THRESHOLD, GUAGE_MISS_FACTOR, GUAGE_PASS_THRESHOLD, RANGE_GREAT, RANGE_MISS,
    RANGE_OK,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Hash, Debug, Serialize, Deserialize)]
pub enum Hit {
    Don,
    Kat,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Judgement {
    Great,
    Ok,
    Miss,
    ComboHit,
    Nothing,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CalculatedNote {
    pub inner: TaikoNote,
    pub idx: usize,
    pub visible_start: f64,
    pub visible_end: f64,
    pub hit_count: u32,
}

impl Eq for CalculatedNote {}

impl Ord for CalculatedNote {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.inner.start.partial_cmp(&other.inner.start).unwrap()
    }
}

impl PartialOrd for CalculatedNote {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.inner.start.partial_cmp(&other.inner.start)
    }
}

impl CalculatedNote {
    pub fn visible(&self, time: f64) -> bool {
        if self.inner.volume == 0 {
            return false;
        }

        if self.variant() == TaikoNoteVariant::Invisible
            || self.variant() == TaikoNoteVariant::Unknown
        {
            return false;
        }

        time > self.visible_start && time < self.visible_end
    }

    pub fn position(&self, time: f64) -> Option<(f64, f64)> {
        if !self.visible(time) {
            return None;
        }

        if self.inner.variant == TaikoNoteVariant::Don
            || self.inner.variant == TaikoNoteVariant::Kat
        {
            let position =
                1.0 - (time - self.visible_start) / (self.visible_end - self.visible_start);
            Some((position, position))
        } else {
            let head = 1.0 - (time - self.visible_start) / (5.0 / self.inner.speed as f64 * 60.0);
            let tail = 1.0
                - (time - self.visible_start - self.inner.duration)
                    / (5.0 / self.inner.speed as f64 * 60.0);

            Some((head, tail))
        }
    }
}

impl Note for CalculatedNote {
    fn start(&self) -> f64 {
        self.inner.start
    }

    fn duration(&self) -> f64 {
        self.inner.duration
    }

    fn volume(&self) -> u16 {
        self.inner.volume
    }

    #[allow(refining_impl_trait)]
    fn variant(&self) -> u16 {
        self.inner.variant.into()
    }

    fn set_start(&mut self, start: f64) {
        self.inner.start = start;
    }

    fn set_duration(&mut self, duration: f64) {
        self.inner.duration = duration;
    }

    fn set_volume(&mut self, volume: u16) {
        self.inner.volume = volume;
    }

    fn set_variant(&mut self, variant: impl Into<u16>) {
        self.inner.variant = TaikoNoteVariant::from(variant.into());
    }

    fn matches_variant(&self, variant: impl Into<u16>) -> bool {
        self.inner.matches_variant(variant)
    }
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
pub struct GameSource {
    pub difficulty: u8,
    pub level: u8,
    pub scoreinit: Option<i32>,
    pub scorediff: Option<i32>,
    pub notes: Vec<TaikoNote>,
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
pub struct InputState<H> {
    /// The current time played in the music, in seconds.
    pub time: f64,
    /// Hit event that happened since the last frame.
    pub hit: Option<H>,
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
pub struct OutputState {
    /// If the game is finished. (All notes are passed)
    pub finished: bool,
    /// The current score of the player.
    pub score: u32,
    /// The current combo of the player.
    pub current_combo: u32,
    /// The maximum combo of the player.
    pub max_combo: u32,
    /// The current soul gauge of the player.
    pub gauge: f64,

    /// The judgement of the hit in the last frame.
    pub judgement: Option<Judgement>,

    /// Display state
    pub display: Vec<CalculatedNote>,
}

#[derive(Clone, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
pub struct Final {
    pub score: u32,
    pub max_combo: u32,
    pub gauge: f64,
    pub greats: u32,
    pub goods: u32,
    pub misses: u32,
    pub max_hit: u32,
    pub passed: bool,
}

pub trait TaikoEngine<H> {
    fn new(src: GameSource) -> Self;
    fn forward(&mut self, input: InputState<H>) -> OutputState;
    fn finalize(&self) -> Final;
}

pub struct DefaultTaikoEngine {
    rhythm: Rhythm<CalculatedNote>,

    difficulty: u8,
    level: u8,
    scoreinit: i32,

    score: u32,
    current_combo: u32,
    max_combo: u32,
    gauge: f64,

    current_time: f64,

    total_notes: usize,

    passed_display: Vec<CalculatedNote>,

    judgements: Vec<Judgement>,
    max_hit_count: u32,
}

impl TaikoEngine<Hit> for DefaultTaikoEngine {
    fn new(src: GameSource) -> Self {
        let notes = src
            .notes
            .iter()
            .enumerate()
            .map(|(idx, note)| {
                let (visible_start, visible_end) = if note.variant() == TaikoNoteVariant::Don
                    || note.variant() == TaikoNoteVariant::Kat
                    || note.variant() == TaikoNoteVariant::Both
                {
                    let start = note.start - (4.5 * 60.0 / note.speed) as f64;
                    let end = note.start + note.duration + (0.5 * 60.0 / note.speed) as f64;
                    (start, end)
                } else {
                    (0.0, 0.0)
                };

                let inner = match note.variant {
                    TaikoNoteVariant::Don | TaikoNoteVariant::Kat => {
                        let mut note = *note;
                        note.start -= RANGE_MISS;
                        note.duration = RANGE_MISS * 2.0;
                        note
                    }
                    _ => *note,
                };

                CalculatedNote {
                    inner,
                    idx,
                    visible_start,
                    visible_end,
                    hit_count: 0,
                }
            })
            .collect::<Vec<_>>();
        let total_notes = notes
            .iter()
            .filter(|note| {
                note.variant() == TaikoNoteVariant::Don || note.variant() == TaikoNoteVariant::Kat
            })
            .count()
            .max(1);
        let rhythm = Rhythm::new(notes.clone());
        let scoreinit = if let Some(s) = src.scoreinit {
            s
        } else {
            let s = (1_000_000 + total_notes - 1) / total_notes;
            let s = (s + 9) / 10 * 10;
            s as i32
        };

        DefaultTaikoEngine {
            rhythm,
            difficulty: src.difficulty,
            level: src.level,
            scoreinit,
            score: 0,
            current_combo: 0,
            max_combo: 0,
            gauge: 0.0,
            current_time: 0.0,
            total_notes,
            passed_display: vec![],
            judgements: vec![],
            max_hit_count: 0,
        }
    }

    fn forward(&mut self, input: InputState<Hit>) -> OutputState {
        let time_diff = input.time - self.current_time;
        self.current_time = input.time;
        let passed = self.rhythm.forward(time_diff);

        let judgement = if let Some(hit) = input.hit {
            match hit {
                Hit::Don => {
                    if let Some((note, delta_from_start)) = self.rhythm.hit(TaikoNoteVariant::Don) {
                        if note.variant() == TaikoNoteVariant::Both {
                            note.hit_count += 1;
                            self.max_hit_count = self.max_hit_count.max(note.hit_count);
                            Some(Judgement::ComboHit)
                        } else {
                            let delta = (delta_from_start - note.duration() / 2.0).abs();
                            if delta < RANGE_GREAT {
                                Some(Judgement::Great)
                            } else if delta < RANGE_OK {
                                Some(Judgement::Ok)
                            } else {
                                Some(Judgement::Miss)
                            }
                        }
                    } else {
                        Some(Judgement::Nothing)
                    }
                }
                Hit::Kat => {
                    if let Some((note, t)) = self.rhythm.hit(TaikoNoteVariant::Kat) {
                        if note.variant() == TaikoNoteVariant::Both {
                            note.hit_count += 1;
                            self.max_hit_count = self.max_hit_count.max(note.hit_count);
                            Some(Judgement::ComboHit)
                        } else {
                            let delta = (t - note.duration() / 2.0).abs();
                            if delta < RANGE_GREAT {
                                Some(Judgement::Great)
                            } else if delta < RANGE_OK {
                                Some(Judgement::Ok)
                            } else {
                                Some(Judgement::Miss)
                            }
                        }
                    } else {
                        Some(Judgement::Nothing)
                    }
                }
            }
        } else {
            None
        };

        // missed notes
        for note in passed.iter() {
            if note.variant() == TaikoNoteVariant::Don || note.variant() == TaikoNoteVariant::Kat {
                self.current_combo = 0;
                self.gauge -= (1.0 / self.total_notes as f64)
                    * GUAGE_MISS_FACTOR[self.difficulty as usize][self.level as usize];
                self.judgements.push(Judgement::Miss);
            }
        }

        let full = GUAGE_FULL_THRESHOLD[self.difficulty as usize][self.level as usize];
        match judgement {
            Some(Judgement::Great) => {
                self.score += self.scoreinit as u32;

                self.current_combo += 1;
                self.max_combo = self.max_combo.max(self.current_combo);

                self.gauge += 1.0 / self.total_notes as f64 / full;
            }
            Some(Judgement::Ok) => {
                self.score += (self.scoreinit as u32) / 2;

                self.current_combo += 1;
                self.max_combo = self.max_combo.max(self.current_combo);

                self.gauge += (1.0 / self.total_notes as f64)
                    * (if self.difficulty >= 3 { 0.5 } else { 0.75 })
                    / full;
            }
            Some(Judgement::Miss) => {
                self.current_combo = 0;

                self.gauge -= (1.0 / self.total_notes as f64)
                    * GUAGE_MISS_FACTOR[self.difficulty as usize][self.level as usize]
                    / full;
            }
            Some(Judgement::ComboHit) => {
                self.score += 100;
            }
            _ => {}
        };

        if let Some(judgement) = judgement {
            self.judgements.push(judgement);
        }

        self.gauge = self.gauge.max(0.0).min(1.0);

        self.passed_display.extend(passed);
        self.passed_display.retain(|note| note.visible(input.time));

        let available_display = self
            .rhythm
            .availables()
            .iter()
            .filter(|note| note.visible(input.time))
            .cloned()
            .collect::<Vec<_>>();

        let mut display = self.passed_display.clone();
        display.extend(available_display);

        OutputState {
            finished: self.rhythm.finished(),
            score: self.score,
            current_combo: self.current_combo,
            max_combo: self.max_combo,
            gauge: self.gauge,
            judgement,
            display,
        }
    }

    fn finalize(&self) -> Final {
        let mut greats = 0;
        let mut goods = 0;
        let mut misses = 0;

        for judgement in self.judgements.iter() {
            match judgement {
                Judgement::Great => greats += 1,
                Judgement::Ok => goods += 1,
                Judgement::Miss => misses += 1,
                _ => {}
            }
        }

        let passed = self.gauge
            >= (GUAGE_PASS_THRESHOLD[self.difficulty as usize][self.level as usize]
                / GUAGE_FULL_THRESHOLD[self.difficulty as usize][self.level as usize]);

        Final {
            score: self.score,
            max_combo: self.max_combo,
            gauge: self.gauge,
            greats,
            goods,
            misses,
            max_hit: self.max_hit_count,
            passed,
        }
    }
}
