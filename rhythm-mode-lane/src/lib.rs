use rhythm_chart::{CanonicalChart, LaneOrRegion, ObjectKind};
use rhythm_core::{CompileError, Mode, Tick, TimedInput};
use serde::{Deserialize, Serialize};

pub const GREAT_WINDOW_TICKS: Tick = 25_000;
pub const OK_WINDOW_TICKS: Tick = 70_000;
pub const MISS_WINDOW_TICKS: Tick = 110_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LaneActionKind {
    Press,
    Release,
    Flick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LaneAction {
    pub lane: u16,
    pub kind: LaneActionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LaneJudge {
    Great,
    Ok,
    Miss,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct LaneScoreState {
    pub score: u32,
    pub combo: u32,
    pub max_combo: u32,
    pub accuracy: f32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneDisplayKind {
    Tap,
    Hold,
    Flick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneFrameNote {
    pub id: u32,
    pub lane: u16,
    pub kind: LaneDisplayKind,
    pub start_tick: Tick,
    pub end_tick: Tick,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneFrameView {
    pub now: Tick,
    pub notes: Vec<LaneFrameNote>,
    pub score: u32,
    pub combo: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaneFinalResult {
    pub score: u32,
    pub max_combo: u32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub accuracy: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneNoteKind {
    Tap,
    Hold,
    Flick,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldStage {
    WaitingPress,
    WaitingRelease,
}

#[derive(Debug, Clone)]
struct LaneNoteState {
    id: u32,
    lane: u16,
    kind: LaneNoteKind,
    start_tick: Tick,
    end_tick: Tick,
    hold_stage: Option<HoldStage>,
    resolved: bool,
}

#[derive(Debug, Clone)]
pub struct LaneCompiled {
    notes: Vec<LaneNoteState>,
    active: Vec<usize>,
    cursor: usize,
}

pub struct LaneMode;

impl Mode for LaneMode {
    type Action = LaneAction;
    type Compiled = LaneCompiled;
    type Judge = LaneJudge;
    type ScoreState = LaneScoreState;
    type FrameView = LaneFrameView;
    type FinalResult = LaneFinalResult;

    fn compile(chart: &CanonicalChart) -> Result<Self::Compiled, CompileError> {
        let mut notes = Vec::with_capacity(chart.objects.len());

        for object in &chart.objects {
            let lane = match object.lane_or_region {
                LaneOrRegion::Lane(lane) => lane,
                LaneOrRegion::Region(_) | LaneOrRegion::None => {
                    return Err(CompileError::Unsupported(format!(
                        "lane mode object {} missing lane",
                        object.id
                    )));
                }
            };

            let kind = match object.kind {
                ObjectKind::Tap => LaneNoteKind::Tap,
                ObjectKind::Hold => LaneNoteKind::Hold,
                ObjectKind::Slide => LaneNoteKind::Flick,
                ObjectKind::Roll | ObjectKind::Touch => {
                    return Err(CompileError::Unsupported(format!(
                        "lane mode does not support {:?}",
                        object.kind
                    )));
                }
            };

            notes.push(LaneNoteState {
                id: object.id,
                lane,
                kind,
                start_tick: object.start_tick,
                end_tick: object.end_tick,
                hold_stage: match kind {
                    LaneNoteKind::Hold => Some(HoldStage::WaitingPress),
                    LaneNoteKind::Tap | LaneNoteKind::Flick => None,
                },
                resolved: false,
            });
        }

        Ok(LaneCompiled {
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

        let mut best: Option<(usize, Tick, Tick, u32, bool)> = None;

        for note_idx in compiled.active.iter().copied() {
            let note = &compiled.notes[note_idx];
            if note.resolved || note.lane != input.action.lane {
                continue;
            }

            let (match_ok, center_tick, is_release) = match (note.kind, input.action.kind) {
                (LaneNoteKind::Tap, LaneActionKind::Press) => (true, note.start_tick, false),
                (LaneNoteKind::Flick, LaneActionKind::Flick) => (true, note.start_tick, false),
                (LaneNoteKind::Hold, LaneActionKind::Press)
                    if note.hold_stage == Some(HoldStage::WaitingPress) =>
                {
                    (true, note.start_tick, false)
                }
                (LaneNoteKind::Hold, LaneActionKind::Release)
                    if note.hold_stage == Some(HoldStage::WaitingRelease) =>
                {
                    (true, note.end_tick, true)
                }
                _ => (false, 0, false),
            };

            if !match_ok {
                continue;
            }

            let delta = (now - center_tick).abs();
            if delta > MISS_WINDOW_TICKS {
                continue;
            }

            let key = (delta, center_tick, note.id, is_release);
            if let Some(current) = best {
                if key < (current.1, current.2, current.3, current.4) {
                    best = Some((note_idx, key.0, key.1, key.2, key.3));
                }
            } else {
                best = Some((note_idx, key.0, key.1, key.2, key.3));
            }
        }

        if let Some((note_idx, delta, _, _, is_release)) = best {
            let note = &mut compiled.notes[note_idx];

            if note.kind == LaneNoteKind::Hold && !is_release {
                note.hold_stage = Some(HoldStage::WaitingRelease);
            } else {
                note.resolved = true;
            }

            return if delta <= GREAT_WINDOW_TICKS {
                Some(LaneJudge::Great)
            } else if delta <= OK_WINDOW_TICKS {
                Some(LaneJudge::Ok)
            } else {
                Some(LaneJudge::Miss)
            };
        }

        Some(LaneJudge::Ignored)
    }

    fn consume_expired(compiled: &mut Self::Compiled, now: Tick, out: &mut Vec<Self::Judge>) {
        activate_notes(compiled, now + MISS_WINDOW_TICKS);

        let mut i = 0;
        while i < compiled.active.len() {
            let note_idx = compiled.active[i];
            let note = &mut compiled.notes[note_idx];

            let remove = if note.resolved {
                true
            } else {
                match note.kind {
                    LaneNoteKind::Tap | LaneNoteKind::Flick => {
                        if now > note.start_tick + MISS_WINDOW_TICKS {
                            note.resolved = true;
                            out.push(LaneJudge::Miss);
                            true
                        } else {
                            false
                        }
                    }
                    LaneNoteKind::Hold => match note.hold_stage {
                        Some(HoldStage::WaitingPress) => {
                            if now > note.start_tick + MISS_WINDOW_TICKS {
                                note.resolved = true;
                                out.push(LaneJudge::Miss);
                                true
                            } else {
                                false
                            }
                        }
                        Some(HoldStage::WaitingRelease) => {
                            if now > note.end_tick + MISS_WINDOW_TICKS {
                                note.resolved = true;
                                out.push(LaneJudge::Miss);
                                true
                            } else {
                                false
                            }
                        }
                        None => true,
                    },
                }
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
            LaneJudge::Great => {
                score.great = score.great.saturating_add(1);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.score = score.score.saturating_add(1_000);
                score.accuracy = (score.accuracy + 1.0).min(1.0);
            }
            LaneJudge::Ok => {
                score.ok = score.ok.saturating_add(1);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.score = score.score.saturating_add(700);
                score.accuracy = (score.accuracy + 0.6).min(1.0);
            }
            LaneJudge::Miss => {
                score.miss = score.miss.saturating_add(1);
                score.combo = 0;
                score.accuracy = (score.accuracy - 0.4).max(0.0);
            }
            LaneJudge::Ignored => {}
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

        LaneFrameView {
            now,
            notes,
            score: score.score,
            combo: score.combo,
        }
    }

    fn finalize(_compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult {
        LaneFinalResult {
            score: score.score,
            max_combo: score.max_combo,
            great: score.great,
            ok: score.ok,
            miss: score.miss,
            accuracy: score.accuracy,
        }
    }

    fn is_finished(compiled: &Self::Compiled, _now: Tick) -> bool {
        compiled.cursor >= compiled.notes.len() && compiled.active.is_empty()
    }
}

fn activate_notes(compiled: &mut LaneCompiled, until_tick: Tick) {
    while compiled.cursor < compiled.notes.len() {
        if compiled.notes[compiled.cursor].start_tick > until_tick {
            break;
        }
        compiled.active.push(compiled.cursor);
        compiled.cursor += 1;
    }
}

fn to_frame_note(note: &LaneNoteState) -> LaneFrameNote {
    LaneFrameNote {
        id: note.id,
        lane: note.lane,
        kind: match note.kind {
            LaneNoteKind::Tap => LaneDisplayKind::Tap,
            LaneNoteKind::Hold => LaneDisplayKind::Hold,
            LaneNoteKind::Flick => LaneDisplayKind::Flick,
        },
        start_tick: note.start_tick,
        end_tick: note.end_tick,
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
            lanes: vec![
                Lane {
                    id: 0,
                    name: "L0".to_owned(),
                    role: LaneRole::Generic,
                },
                Lane {
                    id: 1,
                    name: "L1".to_owned(),
                    role: LaneRole::Generic,
                },
            ],
            branch_segments: Vec::new(),
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 500_000,
                    end_tick: 500_000,
                    lane_or_region: LaneOrRegion::Lane(0),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Hold,
                    start_tick: 1_000_000,
                    end_tick: 1_400_000,
                    lane_or_region: LaneOrRegion::Lane(1),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
            ],
            events: Vec::new(),
        }
    }

    #[test]
    fn lane_mode_replay_is_stable() {
        let chart = chart();
        let mut a = BasicEngine::<LaneMode>::new_basic(&chart).expect("engine a");
        let mut b = BasicEngine::<LaneMode>::new_basic(&chart).expect("engine b");

        for tick in (0..=1_500_000).step_by(50_000_usize) {
            let mut inputs = Vec::new();
            if tick == 500_000 {
                inputs.push(TimedInput {
                    tick,
                    action: LaneAction {
                        lane: 0,
                        kind: LaneActionKind::Press,
                    },
                });
            }
            if tick == 1_000_000 {
                inputs.push(TimedInput {
                    tick,
                    action: LaneAction {
                        lane: 1,
                        kind: LaneActionKind::Press,
                    },
                });
            }
            if tick == 1_400_000 {
                inputs.push(TimedInput {
                    tick,
                    action: LaneAction {
                        lane: 1,
                        kind: LaneActionKind::Release,
                    },
                });
            }

            a.step_to(tick, &inputs).expect("step a");
            b.step_to(tick, &inputs).expect("step b");
        }

        assert_eq!(a.replay_hash(), b.replay_hash());
        let final_result = a.finalize();
        assert_eq!(final_result.great, 3);
        assert_eq!(final_result.miss, 0);
    }
}
