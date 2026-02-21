use rhythm_chart::{CanonicalChart, LaneOrRegion, ObjectKind};
use rhythm_core::{CompileError, Mode, Tick, TimedInput};
use serde::{Deserialize, Serialize};

pub const GREAT_WINDOW_TICKS: Tick = 20_000;
pub const OK_WINDOW_TICKS: Tick = 60_000;
pub const MISS_WINDOW_TICKS: Tick = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RadialAction {
    Tap { region: u16 },
    Touch { region: u16 },
    Slide { from: u16, to: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RadialJudge {
    Great,
    Ok,
    Miss,
    Chain,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RadialScoreState {
    pub score: u32,
    pub combo: u32,
    pub max_combo: u32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub chain: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RadialDisplayKind {
    Tap,
    Touch,
    Slide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RadialFrameNote {
    pub id: u32,
    pub kind: RadialDisplayKind,
    pub start_tick: Tick,
    pub end_tick: Tick,
    pub region: u16,
    pub to_region: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialFrameView {
    pub now: Tick,
    pub notes: Vec<RadialFrameNote>,
    pub score: u32,
    pub combo: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialFinalResult {
    pub score: u32,
    pub max_combo: u32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub chain: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RadialNoteKind {
    Tap,
    Touch,
    Slide,
}

#[derive(Debug, Clone)]
struct RadialNoteState {
    id: u32,
    kind: RadialNoteKind,
    start_tick: Tick,
    end_tick: Tick,
    region: u16,
    to_region: Option<u16>,
    resolved: bool,
}

#[derive(Debug, Clone)]
pub struct RadialCompiled {
    notes: Vec<RadialNoteState>,
    active: Vec<usize>,
    cursor: usize,
}

pub struct RadialMode;

impl Mode for RadialMode {
    type Action = RadialAction;
    type Compiled = RadialCompiled;
    type Judge = RadialJudge;
    type ScoreState = RadialScoreState;
    type FrameView = RadialFrameView;
    type FinalResult = RadialFinalResult;

    fn compile(chart: &CanonicalChart) -> Result<Self::Compiled, CompileError> {
        let mut notes = Vec::with_capacity(chart.objects.len());

        for object in &chart.objects {
            let region = match object.lane_or_region {
                LaneOrRegion::Region(r) | LaneOrRegion::Lane(r) => r,
                LaneOrRegion::None => {
                    return Err(CompileError::Unsupported(format!(
                        "radial object {} missing region",
                        object.id
                    )));
                }
            };

            let kind = match object.kind {
                ObjectKind::Tap => RadialNoteKind::Tap,
                ObjectKind::Touch => RadialNoteKind::Touch,
                ObjectKind::Slide => RadialNoteKind::Slide,
                ObjectKind::Hold | ObjectKind::Roll => {
                    return Err(CompileError::Unsupported(format!(
                        "radial mode does not support {:?}",
                        object.kind
                    )));
                }
            };

            if kind == RadialNoteKind::Slide && object.slide_to.is_none() {
                return Err(CompileError::Unsupported(format!(
                    "slide object {} missing slide_to",
                    object.id
                )));
            }

            notes.push(RadialNoteState {
                id: object.id,
                kind,
                start_tick: object.start_tick,
                end_tick: object.end_tick,
                region,
                to_region: object.slide_to,
                resolved: false,
            });
        }

        Ok(RadialCompiled {
            notes,
            active: Vec::with_capacity(128),
            cursor: 0,
        })
    }

    fn consume_input(
        compiled: &mut Self::Compiled,
        now: Tick,
        input: TimedInput<Self::Action>,
    ) -> Option<Self::Judge> {
        activate_notes(compiled, now + MISS_WINDOW_TICKS);

        let mut best: Option<(usize, Tick, Tick, u32)> = None;

        for note_idx in compiled.active.iter().copied() {
            let note = &compiled.notes[note_idx];
            if note.resolved {
                continue;
            }

            let match_ok = match (note.kind, input.action) {
                (RadialNoteKind::Tap, RadialAction::Tap { region }) => note.region == region,
                (RadialNoteKind::Touch, RadialAction::Touch { region }) => note.region == region,
                (RadialNoteKind::Slide, RadialAction::Slide { from, to }) => {
                    note.region == from && note.to_region == Some(to)
                }
                _ => false,
            };

            if !match_ok {
                continue;
            }

            let delta = (now - note.start_tick).abs();
            if delta > MISS_WINDOW_TICKS {
                continue;
            }

            let key = (delta, note.start_tick, note.id);
            if let Some(current) = best {
                if key < (current.1, current.2, current.3) {
                    best = Some((note_idx, key.0, key.1, key.2));
                }
            } else {
                best = Some((note_idx, key.0, key.1, key.2));
            }
        }

        if let Some((note_idx, delta, _, _)) = best {
            let note = &mut compiled.notes[note_idx];
            note.resolved = true;

            if note.kind == RadialNoteKind::Slide {
                return Some(RadialJudge::Chain);
            }

            return if delta <= GREAT_WINDOW_TICKS {
                Some(RadialJudge::Great)
            } else if delta <= OK_WINDOW_TICKS {
                Some(RadialJudge::Ok)
            } else {
                Some(RadialJudge::Miss)
            };
        }

        Some(RadialJudge::Ignored)
    }

    fn consume_expired(compiled: &mut Self::Compiled, now: Tick, out: &mut Vec<Self::Judge>) {
        activate_notes(compiled, now + MISS_WINDOW_TICKS);

        let mut i = 0;
        while i < compiled.active.len() {
            let note_idx = compiled.active[i];
            let note = &mut compiled.notes[note_idx];

            let remove = if note.resolved {
                true
            } else if now > note.start_tick + MISS_WINDOW_TICKS {
                note.resolved = true;
                out.push(RadialJudge::Miss);
                true
            } else {
                false
            };

            if remove {
                compiled.active.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn apply_judge(score: &mut Self::ScoreState, judge: Self::Judge) {
        match judge {
            RadialJudge::Great => {
                score.score = score.score.saturating_add(1_200);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.great = score.great.saturating_add(1);
            }
            RadialJudge::Ok => {
                score.score = score.score.saturating_add(800);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.ok = score.ok.saturating_add(1);
            }
            RadialJudge::Miss => {
                score.combo = 0;
                score.miss = score.miss.saturating_add(1);
            }
            RadialJudge::Chain => {
                score.score = score.score.saturating_add(500);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.chain = score.chain.saturating_add(1);
            }
            RadialJudge::Ignored => {}
        }
    }

    fn frame_view(
        compiled: &Self::Compiled,
        now: Tick,
        score: &Self::ScoreState,
    ) -> Self::FrameView {
        let mut notes = Vec::with_capacity(compiled.active.len() + 16);

        for note_idx in compiled.active.iter().copied() {
            let note = &compiled.notes[note_idx];
            if note.resolved {
                continue;
            }
            notes.push(to_frame_note(note));
        }

        let mut lookahead = compiled.cursor;
        while lookahead < compiled.notes.len() && notes.len() < 48 {
            let note = &compiled.notes[lookahead];
            if !note.resolved && note.start_tick >= now {
                notes.push(to_frame_note(note));
            }
            lookahead += 1;
        }

        notes.sort_by_key(|note| (note.start_tick, note.id));

        RadialFrameView {
            now,
            notes,
            score: score.score,
            combo: score.combo,
        }
    }

    fn finalize(_compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult {
        RadialFinalResult {
            score: score.score,
            max_combo: score.max_combo,
            great: score.great,
            ok: score.ok,
            miss: score.miss,
            chain: score.chain,
        }
    }

    fn is_finished(compiled: &Self::Compiled, _now: Tick) -> bool {
        compiled.cursor >= compiled.notes.len() && compiled.active.is_empty()
    }
}

fn activate_notes(compiled: &mut RadialCompiled, until_tick: Tick) {
    while compiled.cursor < compiled.notes.len() {
        if compiled.notes[compiled.cursor].start_tick > until_tick {
            break;
        }
        compiled.active.push(compiled.cursor);
        compiled.cursor += 1;
    }
}

fn to_frame_note(note: &RadialNoteState) -> RadialFrameNote {
    RadialFrameNote {
        id: note.id,
        kind: match note.kind {
            RadialNoteKind::Tap => RadialDisplayKind::Tap,
            RadialNoteKind::Touch => RadialDisplayKind::Touch,
            RadialNoteKind::Slide => RadialDisplayKind::Slide,
        },
        start_tick: note.start_tick,
        end_tick: note.end_tick,
        region: note.region,
        to_region: note.to_region,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhythm_chart::{
        CanonicalChart, ChartMetadata, Lane, LaneRole, Object, TempoChange, TimeSignatureChange,
    };
    use rhythm_core::BasicEngine;

    fn chart() -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata::default(),
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: vec![TimeSignatureChange {
                tick: 0,
                numerator: 4,
                denominator: 4,
            }],
            lanes: vec![Lane {
                id: 0,
                name: "regions".to_owned(),
                role: LaneRole::Radial,
            }],
            branch_segments: Vec::new(),
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 500_000,
                    end_tick: 500_000,
                    lane_or_region: LaneOrRegion::Region(0),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Slide,
                    start_tick: 1_000_000,
                    end_tick: 1_100_000,
                    lane_or_region: LaneOrRegion::Region(1),
                    flags: 0,
                    required_hits: 0,
                    slide_to: Some(3),
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
            ],
            events: Vec::new(),
        }
    }

    #[test]
    fn radial_mode_replay_is_stable() {
        let chart = chart();
        let mut a = BasicEngine::<RadialMode>::new_basic(&chart).expect("engine a");
        let mut b = BasicEngine::<RadialMode>::new_basic(&chart).expect("engine b");

        for tick in (0..=1_200_000).step_by(50_000_usize) {
            let mut inputs = Vec::new();
            if tick == 500_000 {
                inputs.push(TimedInput {
                    tick,
                    action: RadialAction::Tap { region: 0 },
                });
            }
            if tick == 1_000_000 {
                inputs.push(TimedInput {
                    tick,
                    action: RadialAction::Slide { from: 1, to: 3 },
                });
            }

            a.step_to(tick, &inputs).expect("step a");
            b.step_to(tick, &inputs).expect("step b");
        }

        assert_eq!(a.replay_hash(), b.replay_hash());
        let result = a.finalize();
        assert_eq!(result.great, 1);
        assert_eq!(result.chain, 1);
        assert_eq!(result.miss, 0);
    }
}
