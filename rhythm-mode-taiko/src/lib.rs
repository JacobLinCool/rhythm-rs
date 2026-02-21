use std::collections::HashMap;

use rhythm_chart::{CanonicalChart, LaneOrRegion, ObjectKind, SCROLL_SCALE};
use rhythm_core::{
    BranchControl, CompileError, ControlError, ControlledMode, Mode, Tick, TimedControl, TimedInput,
};
use serde::{Deserialize, Serialize};

pub const LANE_DON: u16 = 0;
pub const LANE_KAT: u16 = 1;
pub const LANE_BOTH: u16 = 2;

pub const FLAG_BIG: u32 = 1 << 0;
pub const FLAG_BALLOON: u32 = 1 << 1;

pub const GREAT_WINDOW_TICKS: Tick = 30_000;
pub const OK_WINDOW_TICKS: Tick = 80_000;
pub const MISS_WINDOW_TICKS: Tick = 110_000;

const MAX_LEVEL: usize = 10;
const BASE_SCORE_POOL: u64 = 1_000_000;
const ROLL_HIT_SCORE: u32 = 100;
const ROLL_HITS_PER_SECOND: u64 = 16;
const SCORE_ROUND_UNIT: u64 = 10;

// Legacy taiko gauge coefficients (difficulty x level).
const GAUGE_MISS_FACTOR: [[f32; 11]; 5] = [
    [
        0.0,
        1.0 / 2.0,
        1.0 / 2.0,
        1.0 / 2.0,
        1.0 / 2.0,
        1.0 / 2.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    ],
    [
        0.0,
        1.0 / 2.0,
        1.0 / 2.0,
        1.0 / 2.0,
        3.0 / 4.0,
        1.0,
        1.0,
        1.0,
        0.0,
        0.0,
        0.0,
    ],
    [
        0.0,
        3.0 / 4.0,
        3.0 / 4.0,
        1.0,
        7.0 / 6.0,
        5.0 / 4.0,
        5.0 / 4.0,
        5.0 / 4.0,
        5.0 / 4.0,
        0.0,
        0.0,
    ],
    [
        0.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        2.0,
        2.0,
        2.0,
    ],
    [
        0.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        8.0 / 5.0,
        2.0,
        2.0,
        2.0,
    ],
];

const GAUGE_PASS_THRESHOLD: [[f32; 11]; 5] = [
    [0.0, 0.36, 0.38, 0.38, 0.44, 0.44, 0.0, 0.0, 0.0, 0.0, 0.0],
    [
        0.0, 0.4595, 0.4595, 0.487, 0.4925, 0.525, 0.525, 0.525, 0.0, 0.0, 0.0,
    ],
    [
        0.0, 0.545, 0.545, 0.508, 0.484, 0.4725, 0.4812, 0.4812, 0.4812, 0.0, 0.0,
    ],
    [
        0.0, 0.566, 0.566, 0.566, 0.566, 0.566, 0.566, 0.566, 0.56, 0.6, 0.6,
    ],
    [
        0.0, 0.566, 0.566, 0.566, 0.566, 0.566, 0.566, 0.566, 0.56, 0.6, 0.6,
    ],
];

const GAUGE_FULL_THRESHOLD: [[f32; 11]; 5] = [
    [
        0.0, 0.6, 0.63333, 0.63333, 0.73333, 0.73333, 0.0, 0.0, 0.0, 0.0, 0.0,
    ],
    [
        0.0, 0.656, 0.656, 0.6955, 0.7035, 0.75, 0.75, 0.75, 0.0, 0.0, 0.0,
    ],
    [
        0.0, 0.775, 0.775, 0.725, 0.692, 0.675, 0.6875, 0.6875, 0.6875, 0.0, 0.0,
    ],
    [
        0.0, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7, 0.75, 0.75,
    ],
    [
        0.0, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7075, 0.7, 0.75, 0.75,
    ],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaikoAction {
    Don,
    Kat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaikoJudge {
    Great { delta_tick: Tick },
    Ok { delta_tick: Tick },
    Miss { delta_tick: Tick },
    MissExpired,
    RollHit,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaikoJudgeKind {
    Great,
    Ok,
    Miss,
    MissExpired,
    RollHit,
    Ignored,
}

impl TaikoJudge {
    pub const fn kind(self) -> TaikoJudgeKind {
        match self {
            Self::Great { .. } => TaikoJudgeKind::Great,
            Self::Ok { .. } => TaikoJudgeKind::Ok,
            Self::Miss { .. } => TaikoJudgeKind::Miss,
            Self::MissExpired => TaikoJudgeKind::MissExpired,
            Self::RollHit => TaikoJudgeKind::RollHit,
            Self::Ignored => TaikoJudgeKind::Ignored,
        }
    }

    pub const fn timing_delta_tick(self) -> Option<Tick> {
        match self {
            Self::Great { delta_tick } | Self::Ok { delta_tick } | Self::Miss { delta_tick } => {
                Some(delta_tick)
            }
            Self::MissExpired | Self::RollHit | Self::Ignored => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaikoScoreState {
    pub score: u32,
    pub combo: u32,
    pub max_combo: u32,
    pub gauge: f32,
    pub pass_threshold: f32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub roll_hits: u32,
    #[serde(skip)]
    great_score_gain: u32,
    #[serde(skip)]
    ok_score_gain: u32,
    #[serde(skip)]
    great_gauge_gain: f32,
    #[serde(skip)]
    ok_gauge_gain: f32,
    #[serde(skip)]
    miss_gauge_loss_hit: f32,
    #[serde(skip)]
    miss_gauge_loss_expired: f32,
}

impl Default for TaikoScoreState {
    fn default() -> Self {
        Self {
            score: 0,
            combo: 0,
            max_combo: 0,
            gauge: 0.0,
            pass_threshold: 0.8,
            great: 0,
            ok: 0,
            miss: 0,
            roll_hits: 0,
            great_score_gain: 0,
            ok_score_gain: 0,
            great_gauge_gain: 0.012,
            ok_gauge_gain: 0.008,
            miss_gauge_loss_hit: 0.02,
            miss_gauge_loss_expired: 0.02,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaikoDisplayKind {
    Tap,
    Roll,
    Balloon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaikoFrameNote {
    pub id: u32,
    pub lane: u16,
    pub is_big: bool,
    pub kind: TaikoDisplayKind,
    pub start_tick: Tick,
    pub end_tick: Tick,
    pub remaining_hits: u16,
    pub scroll_scaled: i32,
}

impl TaikoFrameNote {
    pub fn scroll_multiplier(self) -> f32 {
        self.scroll_scaled as f32 / SCROLL_SCALE as f32
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaikoFrameView {
    pub now: Tick,
    pub notes: Vec<TaikoFrameNote>,
    pub score: u32,
    pub combo: u32,
    pub gauge: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaikoFinalResult {
    pub score: u32,
    pub max_combo: u32,
    pub gauge: f32,
    pub pass_threshold: f32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub roll_hits: u32,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaikoNoteKind {
    Tap,
    Roll,
    Balloon,
}

#[derive(Debug, Clone)]
struct TaikoNoteState {
    id: u32,
    lane: u16,
    is_big: bool,
    start_tick: Tick,
    end_tick: Tick,
    kind: TaikoNoteKind,
    required_hits: u16,
    hits: u16,
    resolved: bool,
    scroll_scaled: i32,
    branch_segment_idx: Option<u16>,
    branch_route_id: u8,
}

#[derive(Debug, Clone, Copy)]
struct SegmentStateAtTick {
    tick: Tick,
    enabled: bool,
    route_id: u8,
}

#[derive(Debug, Clone, Copy)]
struct GaugeProfile {
    full_threshold: f32,
    pass_ratio: f32,
    miss_factor: f32,
    ok_factor: f32,
    total_tap_notes: u32,
    great_score_gain: u32,
    ok_score_gain: u32,
}

#[derive(Debug, Clone)]
pub struct TaikoCompiled {
    notes: Vec<TaikoNoteState>,
    active: Vec<usize>,
    cursor: usize,
    gauge_profile: GaugeProfile,
    segment_index: HashMap<u32, usize>,
    segment_enabled: Vec<bool>,
    segment_route: Vec<u8>,
    segment_route_count: Vec<u8>,
    segment_history: Vec<Vec<SegmentStateAtTick>>,
}

pub struct TaikoMode;

impl Mode for TaikoMode {
    type Action = TaikoAction;
    type Compiled = TaikoCompiled;
    type Judge = TaikoJudge;
    type ScoreState = TaikoScoreState;
    type FrameView = TaikoFrameView;
    type FinalResult = TaikoFinalResult;

    fn init_score(compiled: &Self::Compiled) -> Self::ScoreState {
        let base = 1.0 / compiled.gauge_profile.total_tap_notes as f32;
        TaikoScoreState {
            pass_threshold: compiled.gauge_profile.pass_ratio,
            great_score_gain: compiled.gauge_profile.great_score_gain,
            ok_score_gain: compiled.gauge_profile.ok_score_gain,
            great_gauge_gain: base / compiled.gauge_profile.full_threshold,
            ok_gauge_gain: base * compiled.gauge_profile.ok_factor
                / compiled.gauge_profile.full_threshold,
            miss_gauge_loss_hit: base * compiled.gauge_profile.miss_factor
                / compiled.gauge_profile.full_threshold,
            miss_gauge_loss_expired: base * compiled.gauge_profile.miss_factor,
            ..TaikoScoreState::default()
        }
    }

    fn compile(chart: &CanonicalChart) -> Result<Self::Compiled, CompileError> {
        if chart.branch_segments.len() > usize::from(u16::MAX) {
            return Err(CompileError::Unsupported(
                "too many branch segments for taiko mode".to_owned(),
            ));
        }

        let mut segment_index = HashMap::with_capacity(chart.branch_segments.len());
        let mut segment_enabled = Vec::with_capacity(chart.branch_segments.len());
        let mut segment_route = Vec::with_capacity(chart.branch_segments.len());
        let mut segment_route_count = Vec::with_capacity(chart.branch_segments.len());
        let mut segment_history = Vec::with_capacity(chart.branch_segments.len());

        for (idx, segment) in chart.branch_segments.iter().enumerate() {
            segment_index.insert(segment.id, idx);
            segment_enabled.push(true);
            segment_route.push(segment.default_route_id);
            segment_route_count.push(segment.route_count);
            segment_history.push(vec![SegmentStateAtTick {
                tick: 0,
                enabled: true,
                route_id: segment.default_route_id,
            }]);
        }

        let mut notes = Vec::with_capacity(chart.objects.len());

        for object in &chart.objects {
            let lane = match object.lane_or_region {
                LaneOrRegion::Lane(lane) => lane,
                LaneOrRegion::Region(_) | LaneOrRegion::None => {
                    return Err(CompileError::Unsupported(format!(
                        "taiko object {} missing lane",
                        object.id
                    )));
                }
            };

            let kind = match object.kind {
                ObjectKind::Tap => TaikoNoteKind::Tap,
                ObjectKind::Hold | ObjectKind::Roll => {
                    if object.flags & FLAG_BALLOON != 0 || object.required_hits > 0 {
                        TaikoNoteKind::Balloon
                    } else {
                        TaikoNoteKind::Roll
                    }
                }
                ObjectKind::Slide | ObjectKind::Touch => {
                    return Err(CompileError::Unsupported(format!(
                        "taiko does not support {:?}",
                        object.kind
                    )));
                }
            };

            let branch_segment_idx = if let Some(segment_id) = object.branch_segment_id {
                let segment_idx = match segment_index.get(&segment_id).copied() {
                    Some(idx) => idx,
                    None => {
                        return Err(CompileError::Unsupported(format!(
                            "object {} references unknown segment {}",
                            object.id, segment_id
                        )));
                    }
                };

                let idx_u16 = match u16::try_from(segment_idx) {
                    Ok(v) => v,
                    Err(_) => {
                        return Err(CompileError::Unsupported(
                            "branch segment index exceeds u16".to_owned(),
                        ));
                    }
                };
                Some(idx_u16)
            } else {
                if object.branch_route_id != 0 {
                    return Err(CompileError::Unsupported(format!(
                        "object {} has route {} without segment",
                        object.id, object.branch_route_id
                    )));
                }
                None
            };

            notes.push(TaikoNoteState {
                id: object.id,
                lane,
                is_big: object.flags & FLAG_BIG != 0,
                start_tick: object.start_tick,
                end_tick: object.end_tick,
                kind,
                required_hits: object.required_hits,
                hits: 0,
                resolved: false,
                scroll_scaled: object.scroll_scaled,
                branch_segment_idx,
                branch_route_id: object.branch_route_id,
            });
        }

        let gauge_profile = gauge_profile_for_chart(chart, &notes)?;

        Ok(TaikoCompiled {
            notes,
            active: Vec::with_capacity(128),
            cursor: 0,
            gauge_profile,
            segment_index,
            segment_enabled,
            segment_route,
            segment_route_count,
            segment_history,
        })
    }

    fn consume_input(
        compiled: &mut Self::Compiled,
        _now: Tick,
        input: TimedInput<Self::Action>,
    ) -> Option<Self::Judge> {
        let input_tick = input.tick;
        activate_notes(compiled, input_tick + MISS_WINDOW_TICKS);

        let mut best_tap: Option<(usize, Tick, Tick, u32)> = None;
        let mut best_roll: Option<(usize, Tick, u32)> = None;

        for note_idx in compiled.active.iter().copied() {
            let note = &compiled.notes[note_idx];
            if note.resolved || !note_is_selected(compiled, note) {
                continue;
            }

            if !lane_matches(note.lane, input.action) {
                continue;
            }

            match note.kind {
                TaikoNoteKind::Tap => {
                    let delta = (input_tick - note.start_tick).abs();
                    if delta <= MISS_WINDOW_TICKS {
                        let key = (delta, note.start_tick, note.id);
                        if let Some(current) = best_tap {
                            if key < (current.1, current.2, current.3) {
                                best_tap = Some((note_idx, key.0, key.1, key.2));
                            }
                        } else {
                            best_tap = Some((note_idx, key.0, key.1, key.2));
                        }
                    }
                }
                TaikoNoteKind::Roll | TaikoNoteKind::Balloon => {
                    if input_tick >= note.start_tick && input_tick <= note.end_tick {
                        let key = (note.start_tick, note.id);
                        if let Some(current) = best_roll {
                            if key < (current.1, current.2) {
                                best_roll = Some((note_idx, key.0, key.1));
                            }
                        } else {
                            best_roll = Some((note_idx, key.0, key.1));
                        }
                    }
                }
            }
        }

        if let Some((note_idx, delta, _, _)) = best_tap {
            let note = &mut compiled.notes[note_idx];
            note.resolved = true;
            let signed_delta = input_tick - note.start_tick;
            return if delta < GREAT_WINDOW_TICKS {
                Some(TaikoJudge::Great {
                    delta_tick: signed_delta,
                })
            } else if delta < OK_WINDOW_TICKS {
                Some(TaikoJudge::Ok {
                    delta_tick: signed_delta,
                })
            } else {
                Some(TaikoJudge::Miss {
                    delta_tick: signed_delta,
                })
            };
        }

        if let Some((note_idx, _, _)) = best_roll {
            let note = &mut compiled.notes[note_idx];
            note.hits = note.hits.saturating_add(1);
            if note.kind == TaikoNoteKind::Balloon
                && note.required_hits > 0
                && note.hits >= note.required_hits
            {
                note.resolved = true;
            }
            return Some(TaikoJudge::RollHit);
        }

        Some(TaikoJudge::Ignored)
    }

    fn consume_expired(compiled: &mut Self::Compiled, now: Tick, out: &mut Vec<Self::Judge>) {
        activate_notes(compiled, now + MISS_WINDOW_TICKS);

        let mut i = 0;
        while i < compiled.active.len() {
            let note_idx = compiled.active[i];
            let remove = if compiled.notes[note_idx].resolved {
                true
            } else if !note_is_selected(compiled, &compiled.notes[note_idx]) {
                if compiled.notes[note_idx].start_tick < now {
                    compiled.notes[note_idx].resolved = true;
                    true
                } else {
                    false
                }
            } else {
                let note = &mut compiled.notes[note_idx];
                match note.kind {
                    TaikoNoteKind::Tap => {
                        if now > note.start_tick + MISS_WINDOW_TICKS {
                            note.resolved = true;
                            out.push(TaikoJudge::MissExpired);
                            true
                        } else {
                            false
                        }
                    }
                    TaikoNoteKind::Roll => {
                        if now > note.end_tick {
                            note.resolved = true;
                            true
                        } else {
                            false
                        }
                    }
                    TaikoNoteKind::Balloon => {
                        if now > note.end_tick {
                            if note.required_hits > 0 && note.hits < note.required_hits {
                                out.push(TaikoJudge::MissExpired);
                            }
                            note.resolved = true;
                            true
                        } else {
                            false
                        }
                    }
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
            TaikoJudge::Great { .. } => {
                score.score = score.score.saturating_add(score.great_score_gain);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.great = score.great.saturating_add(1);
                score.gauge = (score.gauge + score.great_gauge_gain).min(1.0);
            }
            TaikoJudge::Ok { .. } => {
                score.score = score.score.saturating_add(score.ok_score_gain);
                score.combo = score.combo.saturating_add(1);
                score.max_combo = score.max_combo.max(score.combo);
                score.ok = score.ok.saturating_add(1);
                score.gauge = (score.gauge + score.ok_gauge_gain).min(1.0);
            }
            TaikoJudge::Miss { .. } => {
                score.combo = 0;
                score.miss = score.miss.saturating_add(1);
                score.gauge = (score.gauge - score.miss_gauge_loss_hit).max(0.0);
            }
            TaikoJudge::MissExpired => {
                score.combo = 0;
                score.miss = score.miss.saturating_add(1);
                score.gauge = (score.gauge - score.miss_gauge_loss_expired).max(0.0);
            }
            TaikoJudge::RollHit => {
                score.score = score.score.saturating_add(ROLL_HIT_SCORE);
                score.roll_hits = score.roll_hits.saturating_add(1);
            }
            TaikoJudge::Ignored => {}
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
            if note.resolved || !note_is_selected(compiled, note) {
                continue;
            }
            notes.push(to_frame_note(note));
        }

        let mut lookahead = compiled.cursor;
        while lookahead < compiled.notes.len() && notes.len() < 48 {
            let note = &compiled.notes[lookahead];
            if !note.resolved && note.start_tick >= now && note_is_selected(compiled, note) {
                notes.push(to_frame_note(note));
            }
            lookahead += 1;
        }

        notes.sort_by_key(|note| (note.start_tick, note.id));

        TaikoFrameView {
            now,
            notes,
            score: score.score,
            combo: score.combo,
            gauge: score.gauge,
        }
    }

    fn finalize(_compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult {
        TaikoFinalResult {
            score: score.score,
            max_combo: score.max_combo,
            gauge: score.gauge,
            pass_threshold: score.pass_threshold,
            great: score.great,
            ok: score.ok,
            miss: score.miss,
            roll_hits: score.roll_hits,
            passed: score.gauge >= score.pass_threshold,
        }
    }

    fn is_finished(compiled: &Self::Compiled, _now: Tick) -> bool {
        compiled.cursor >= compiled.notes.len() && compiled.active.is_empty()
    }
}

impl ControlledMode for TaikoMode {
    type Control = BranchControl;

    fn consume_control(
        compiled: &mut Self::Compiled,
        control: TimedControl<Self::Control>,
    ) -> Result<(), ControlError> {
        match control.control {
            BranchControl::SetBranchRoute {
                segment_id,
                route_id,
            } => {
                let segment_idx = lookup_segment_index(compiled, segment_id)?;
                let route_count = compiled.segment_route_count[segment_idx];
                if route_id >= route_count {
                    return Err(ControlError::InvalidRoute {
                        segment_id,
                        route_id,
                    });
                }

                compiled.segment_route[segment_idx] = route_id;
                let enabled = compiled.segment_enabled[segment_idx];
                compiled.segment_history[segment_idx].push(SegmentStateAtTick {
                    tick: control.tick,
                    enabled,
                    route_id,
                });
                Ok(())
            }
            BranchControl::EnableSegment { segment_id } => {
                let segment_idx = lookup_segment_index(compiled, segment_id)?;
                compiled.segment_enabled[segment_idx] = true;
                let route_id = compiled.segment_route[segment_idx];
                compiled.segment_history[segment_idx].push(SegmentStateAtTick {
                    tick: control.tick,
                    enabled: true,
                    route_id,
                });
                Ok(())
            }
            BranchControl::DisableSegment { segment_id } => {
                let segment_idx = lookup_segment_index(compiled, segment_id)?;
                compiled.segment_enabled[segment_idx] = false;
                let route_id = compiled.segment_route[segment_idx];
                compiled.segment_history[segment_idx].push(SegmentStateAtTick {
                    tick: control.tick,
                    enabled: false,
                    route_id,
                });
                Ok(())
            }
        }
    }
}

fn lookup_segment_index(compiled: &TaikoCompiled, segment_id: u32) -> Result<usize, ControlError> {
    compiled
        .segment_index
        .get(&segment_id)
        .copied()
        .ok_or(ControlError::UnknownSegment(segment_id))
}

fn activate_notes(compiled: &mut TaikoCompiled, until_tick: Tick) {
    while compiled.cursor < compiled.notes.len() {
        if compiled.notes[compiled.cursor].start_tick > until_tick {
            break;
        }
        compiled.active.push(compiled.cursor);
        compiled.cursor += 1;
    }
}

fn lane_matches(lane: u16, action: TaikoAction) -> bool {
    match lane {
        LANE_DON => matches!(action, TaikoAction::Don),
        LANE_KAT => matches!(action, TaikoAction::Kat),
        LANE_BOTH => true,
        _ => false,
    }
}

fn note_is_selected(compiled: &TaikoCompiled, note: &TaikoNoteState) -> bool {
    let Some(segment_idx) = note.branch_segment_idx.map(usize::from) else {
        return true;
    };

    let history = &compiled.segment_history[segment_idx];
    let state_idx = history.partition_point(|state| state.tick <= note.start_tick);
    if state_idx == 0 {
        return false;
    }
    let state = history[state_idx - 1];
    state.enabled && note.branch_route_id == state.route_id
}

fn to_frame_note(note: &TaikoNoteState) -> TaikoFrameNote {
    TaikoFrameNote {
        id: note.id,
        lane: note.lane,
        is_big: note.is_big,
        kind: match note.kind {
            TaikoNoteKind::Tap => TaikoDisplayKind::Tap,
            TaikoNoteKind::Roll => TaikoDisplayKind::Roll,
            TaikoNoteKind::Balloon => TaikoDisplayKind::Balloon,
        },
        start_tick: note.start_tick,
        end_tick: note.end_tick,
        remaining_hits: note.required_hits.saturating_sub(note.hits),
        scroll_scaled: note.scroll_scaled,
    }
}

fn gauge_profile_for_chart(
    chart: &CanonicalChart,
    notes: &[TaikoNoteState],
) -> Result<GaugeProfile, CompileError> {
    let difficulty_name = chart.metadata.difficulty_name.as_deref().ok_or_else(|| {
        CompileError::Unsupported(
            "taiko requires chart.metadata.difficulty_name for gauge profile".to_owned(),
        )
    })?;
    let difficulty_idx = difficulty_index_from_name(difficulty_name)?;

    let level = chart.metadata.difficulty_level.ok_or_else(|| {
        CompileError::Unsupported(
            "taiko requires chart.metadata.difficulty_level (1..=10) for gauge profile".to_owned(),
        )
    })?;
    if level == 0 || usize::from(level) > MAX_LEVEL {
        return Err(CompileError::Unsupported(format!(
            "taiko difficulty_level out of range: {level} (expected 1..=10)"
        )));
    }
    let level_idx = usize::from(level);

    let miss_factor = GAUGE_MISS_FACTOR[difficulty_idx][level_idx];
    let pass_threshold = GAUGE_PASS_THRESHOLD[difficulty_idx][level_idx];
    let full_threshold = GAUGE_FULL_THRESHOLD[difficulty_idx][level_idx];
    if miss_factor <= 0.0 || pass_threshold <= 0.0 || full_threshold <= 0.0 {
        return Err(CompileError::Unsupported(format!(
            "unsupported taiko gauge table entry for difficulty={} level={}",
            difficulty_name, level
        )));
    }

    let total_tap_notes = notes
        .iter()
        .filter(|note| matches!(note.kind, TaikoNoteKind::Tap))
        .count()
        .max(1) as u32;
    let great_score_gain = tap_great_score_gain(notes);
    let ok_score_gain = great_score_gain / 2;

    Ok(GaugeProfile {
        full_threshold,
        pass_ratio: pass_threshold / full_threshold,
        miss_factor,
        ok_factor: if difficulty_idx >= 3 { 0.5 } else { 0.75 },
        total_tap_notes,
        great_score_gain,
        ok_score_gain,
    })
}

fn tap_great_score_gain(notes: &[TaikoNoteState]) -> u32 {
    let tap_count = notes
        .iter()
        .filter(|note| matches!(note.kind, TaikoNoteKind::Tap))
        .count() as u64;
    if tap_count == 0 {
        return 0;
    }

    let mut balloon_reserve = 0_u64;
    let mut roll_reserve = 0_u64;
    for note in notes {
        let duration_ticks = note.end_tick.saturating_sub(note.start_tick).max(0) as u64;
        match note.kind {
            TaikoNoteKind::Balloon => {
                let hits = u64::from(note.required_hits);
                let reserve = if hits > 0 {
                    hits.saturating_mul(u64::from(ROLL_HIT_SCORE))
                } else {
                    duration_ticks
                        .saturating_mul(ROLL_HITS_PER_SECOND)
                        .saturating_mul(u64::from(ROLL_HIT_SCORE))
                        / 1_000_000
                };
                balloon_reserve = balloon_reserve.saturating_add(reserve);
            }
            TaikoNoteKind::Roll => {
                let reserve = duration_ticks
                    .saturating_mul(ROLL_HITS_PER_SECOND)
                    .saturating_mul(u64::from(ROLL_HIT_SCORE))
                    / 1_000_000;
                roll_reserve = roll_reserve.saturating_add(reserve);
            }
            TaikoNoteKind::Tap => {}
        }
    }

    let reserved_total = balloon_reserve.saturating_add(roll_reserve);
    let distributable = BASE_SCORE_POOL.saturating_sub(reserved_total);
    if distributable == 0 {
        return 0;
    }

    let denominator = tap_count.saturating_mul(SCORE_ROUND_UNIT);
    let rounded = ceil_div_u64(distributable, denominator).saturating_mul(SCORE_ROUND_UNIT);
    rounded.min(u64::from(u32::MAX)) as u32
}

fn ceil_div_u64(numer: u64, denom: u64) -> u64 {
    if denom == 0 {
        return 0;
    }
    numer.saturating_add(denom - 1) / denom
}

fn difficulty_index_from_name(name: &str) -> Result<usize, CompileError> {
    let normalized = name.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(CompileError::Unsupported(
            "taiko difficulty_name cannot be empty".to_owned(),
        ));
    }

    if let Ok(num) = normalized.parse::<usize>() {
        if num <= 4 {
            return Ok(num);
        }
    }

    if normalized.contains("easy") {
        return Ok(0);
    }
    if normalized.contains("normal") {
        return Ok(1);
    }
    if normalized.contains("hard") {
        return Ok(2);
    }
    if normalized.contains("oni") {
        return Ok(3);
    }
    if normalized.contains("ura") || normalized.contains("edit") {
        return Ok(4);
    }

    Err(CompileError::Unsupported(format!(
        "unknown taiko difficulty_name: {name}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use rhythm_chart::{
        BranchSegment, CanonicalChart, ChartMetadata, Lane, LaneRole, Object, TempoChange,
        TimeSignatureChange,
    };
    use rhythm_core::{BasicEngine, ControlledEngine};

    fn taiko_metadata(difficulty_name: &str, difficulty_level: u8) -> ChartMetadata {
        ChartMetadata {
            difficulty_name: Some(difficulty_name.to_owned()),
            difficulty_level: Some(difficulty_level),
            ..ChartMetadata::default()
        }
    }

    fn chart() -> CanonicalChart {
        CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                    id: LANE_DON,
                    name: "don".to_owned(),
                    role: rhythm_chart::LaneRole::TaikoDon,
                },
                Lane {
                    id: LANE_KAT,
                    name: "kat".to_owned(),
                    role: LaneRole::TaikoKat,
                },
            ],
            branch_segments: Vec::new(),
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 1_000_000,
                    end_tick: 1_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_DON),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Tap,
                    start_tick: 2_000_000,
                    end_tick: 2_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_KAT),
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

    fn branch_chart() -> CanonicalChart {
        CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                id: LANE_DON,
                name: "don".to_owned(),
                role: LaneRole::TaikoDon,
            }],
            branch_segments: vec![BranchSegment {
                id: 7,
                default_route_id: 0,
                route_count: 3,
                decision_hint: None,
            }],
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 1_000_000,
                    end_tick: 1_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_DON),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: Some(7),
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Tap,
                    start_tick: 1_500_000,
                    end_tick: 1_500_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_DON),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: Some(7),
                    branch_route_id: 1,
                },
                Object {
                    id: 3,
                    kind: ObjectKind::Tap,
                    start_tick: 2_000_000,
                    end_tick: 2_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_DON),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: Some(7),
                    branch_route_id: 2,
                },
            ],
            events: Vec::new(),
        }
    }

    #[test]
    fn taiko_golden_replay_hash() {
        let chart = chart();
        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");

        let replay = [
            TimedInput {
                tick: 1_000_000,
                action: TaikoAction::Don,
            },
            TimedInput {
                tick: 2_000_000,
                action: TaikoAction::Kat,
            },
        ];

        for tick in (0..=2_100_000).step_by(50_000_usize) {
            let inputs = replay
                .iter()
                .copied()
                .filter(|input| input.tick == tick)
                .collect::<Vec<_>>();
            engine.step_to(tick, &inputs).expect("step");
        }

        let result = engine.finalize();
        assert_eq!(result.great, 2);
        assert_eq!(result.miss, 0);
        assert_ne!(engine.replay_hash(), 0);
    }

    #[test]
    fn input_tick_drives_judgement_inside_frame_step() {
        let chart = chart();
        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");

        let output = engine
            .step_to(
                1_050_000,
                &[TimedInput {
                    tick: 1_000_000,
                    action: TaikoAction::Don,
                }],
            )
            .expect("step");

        assert_eq!(
            output.judges.first().copied(),
            Some(TaikoJudge::Great { delta_tick: 0 })
        );
    }

    #[test]
    fn branch_route_selects_expected_notes() {
        let chart = branch_chart();
        let mut engine = ControlledEngine::<TaikoMode>::new_controlled(&chart).expect("engine");

        for tick in (0..=2_200_000).step_by(50_000_usize) {
            let controls = if tick == 1_400_000 {
                vec![TimedControl {
                    tick,
                    control: BranchControl::SetBranchRoute {
                        segment_id: 7,
                        route_id: 1,
                    },
                }]
            } else {
                Vec::new()
            };

            let mut inputs = Vec::new();
            if tick == 1_000_000 || tick == 1_500_000 {
                inputs.push(TimedInput {
                    tick,
                    action: TaikoAction::Don,
                });
            }

            let _ = engine
                .step_to_with_controls(tick, &controls, &inputs)
                .expect("step");
        }

        let result = engine.finalize();
        assert_eq!(result.great, 2);
        assert_eq!(result.miss, 0);
    }

    #[test]
    fn branch_control_on_start_tick_is_effective() {
        let mut chart = branch_chart();
        chart.objects = vec![Object {
            id: 10,
            kind: ObjectKind::Tap,
            start_tick: 1_000_000,
            end_tick: 1_000_000,
            lane_or_region: LaneOrRegion::Lane(LANE_DON),
            flags: 0,
            required_hits: 0,
            slide_to: None,
            scroll_scaled: 1_000_000,
            branch_segment_id: Some(7),
            branch_route_id: 1,
        }];

        let mut engine = ControlledEngine::<TaikoMode>::new_controlled(&chart).expect("engine");

        let controls = vec![TimedControl {
            tick: 1_000_000,
            control: BranchControl::SetBranchRoute {
                segment_id: 7,
                route_id: 1,
            },
        }];
        let inputs = vec![TimedInput {
            tick: 1_000_000,
            action: TaikoAction::Don,
        }];

        let _ = engine
            .step_to_with_controls(1_000_000, &controls, &inputs)
            .expect("step");
        let result = engine.finalize();
        assert_eq!(result.great, 1);
        assert_eq!(result.miss, 0);
    }

    #[test]
    #[ignore = "bench-smoke"]
    fn bench_smoke_large_chart() {
        let object_count: u32 = 100_000;
        let mut objects = Vec::with_capacity(object_count as usize);
        for i in 0..object_count {
            let tick = 100_000 + i as i64 * 50_000;
            objects.push(Object {
                id: i + 1,
                kind: ObjectKind::Tap,
                start_tick: tick,
                end_tick: tick,
                lane_or_region: LaneOrRegion::Lane(if i % 2 == 0 { LANE_DON } else { LANE_KAT }),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: 1_000_000,
                branch_segment_id: None,
                branch_route_id: 0,
            });
        }

        let chart = CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                    id: LANE_DON,
                    name: "don".to_owned(),
                    role: LaneRole::TaikoDon,
                },
                Lane {
                    id: LANE_KAT,
                    name: "kat".to_owned(),
                    role: LaneRole::TaikoKat,
                },
            ],
            branch_segments: Vec::new(),
            objects,
            events: Vec::new(),
        };

        let start = Instant::now();
        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");
        for i in 0..object_count {
            let tick = 100_000 + i as i64 * 50_000;
            let action = if i % 2 == 0 {
                TaikoAction::Don
            } else {
                TaikoAction::Kat
            };
            engine
                .step_to(tick, &[TimedInput { tick, action }])
                .expect("step");
        }
        engine
            .step_to(100_000 + object_count as i64 * 50_000 + 500_000, &[])
            .expect("final step");

        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(5), "elapsed={elapsed:?}");
        assert_eq!(engine.finalize().miss, 0);
    }

    #[test]
    #[ignore = "bench-smoke"]
    fn bench_smoke_branch_heavy() {
        let object_count: u32 = 100_000;
        let mut objects = Vec::with_capacity(object_count as usize);
        for i in 0..object_count {
            let tick = 100_000 + i as i64 * 50_000;
            objects.push(Object {
                id: i + 1,
                kind: ObjectKind::Tap,
                start_tick: tick,
                end_tick: tick,
                lane_or_region: LaneOrRegion::Lane(LANE_DON),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: 1_000_000,
                branch_segment_id: Some(1),
                branch_route_id: (i % 3) as u8,
            });
        }

        let chart = CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                id: LANE_DON,
                name: "don".to_owned(),
                role: LaneRole::TaikoDon,
            }],
            branch_segments: vec![BranchSegment {
                id: 1,
                default_route_id: 0,
                route_count: 3,
                decision_hint: None,
            }],
            objects,
            events: Vec::new(),
        };

        let start = Instant::now();
        let mut engine = ControlledEngine::<TaikoMode>::new_controlled(&chart).expect("engine");
        for i in 0..object_count {
            let tick = 100_000 + i as i64 * 50_000;
            let route = ((i / 100) % 3) as u8;
            let controls = [TimedControl {
                tick,
                control: BranchControl::SetBranchRoute {
                    segment_id: 1,
                    route_id: route,
                },
            }];
            let inputs = [TimedInput {
                tick,
                action: TaikoAction::Don,
            }];

            let _ = engine
                .step_to_with_controls(tick, &controls, &inputs)
                .expect("step");
        }
        let _ = engine
            .step_to_with_controls(100_000 + object_count as i64 * 50_000 + 500_000, &[], &[])
            .expect("final step");

        let elapsed = start.elapsed();
        assert!(elapsed < Duration::from_secs(5), "elapsed={elapsed:?}");
    }

    #[test]
    fn compile_requires_difficulty_metadata() {
        let mut chart = chart();
        chart.metadata.difficulty_name = None;

        let err = BasicEngine::<TaikoMode>::new_basic(&chart).err();
        assert!(matches!(err, Some(CompileError::Unsupported(_))));
    }

    #[test]
    fn great_ok_boundaries_match_legacy_strict_less_than() {
        let chart = chart();
        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");

        let great_edge = TimedInput {
            tick: 1_000_000 + GREAT_WINDOW_TICKS,
            action: TaikoAction::Don,
        };
        let ok_edge = TimedInput {
            tick: 2_000_000 + OK_WINDOW_TICKS,
            action: TaikoAction::Kat,
        };

        engine
            .step_to(great_edge.tick, &[great_edge])
            .expect("step");
        engine.step_to(ok_edge.tick, &[ok_edge]).expect("step");
        let result = engine.finalize();
        assert_eq!(result.great, 0);
        assert_eq!(result.ok, 1);
        assert_eq!(result.miss, 1);
    }

    #[test]
    fn pass_threshold_uses_difficulty_and_level_table() {
        let mut hard = chart();
        hard.metadata = taiko_metadata("Hard", 5);
        let hard_engine = BasicEngine::<TaikoMode>::new_basic(&hard).expect("hard");

        let mut oni = chart();
        oni.metadata = taiko_metadata("Oni", 10);
        let oni_engine = BasicEngine::<TaikoMode>::new_basic(&oni).expect("oni");

        let hard_threshold = hard_engine.score().pass_threshold;
        let oni_threshold = oni_engine.score().pass_threshold;

        assert!((hard_threshold - 0.7).abs() < 1e-6);
        assert!((oni_threshold - 0.8).abs() < 1e-6);
    }

    #[test]
    fn hit_miss_penalty_is_stronger_than_timeout_miss() {
        let chart = chart();
        let engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");

        let mut hit_miss = engine.score().clone();
        hit_miss.gauge = 1.0;
        TaikoMode::apply_judge(&mut hit_miss, TaikoJudge::Miss { delta_tick: 0 });

        let mut timeout_miss = engine.score().clone();
        timeout_miss.gauge = 1.0;
        TaikoMode::apply_judge(&mut timeout_miss, TaikoJudge::MissExpired);

        assert!(hit_miss.gauge < timeout_miss.gauge);
    }

    #[test]
    fn score_formula_uses_tap_count_roll_seconds_and_balloon_hits() {
        let chart = CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                    id: LANE_DON,
                    name: "don".to_owned(),
                    role: LaneRole::TaikoDon,
                },
                Lane {
                    id: LANE_BOTH,
                    name: "both".to_owned(),
                    role: LaneRole::Generic,
                },
            ],
            branch_segments: Vec::new(),
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 1_000_000,
                    end_tick: 1_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_DON),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Roll,
                    start_tick: 2_000_000,
                    end_tick: 3_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_BOTH),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 3,
                    kind: ObjectKind::Roll,
                    start_tick: 4_000_000,
                    end_tick: 5_000_000,
                    lane_or_region: LaneOrRegion::Lane(LANE_BOTH),
                    flags: FLAG_BALLOON,
                    required_hits: 7,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
            ],
            events: Vec::new(),
        };

        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");
        let input = TimedInput {
            tick: 1_000_000,
            action: TaikoAction::Don,
        };
        engine.step_to(input.tick, &[input]).expect("step");

        let result = engine.finalize();
        assert_eq!(result.great, 1);
        assert_eq!(result.ok, 0);
        assert_eq!(result.score, 997_700);
    }

    #[test]
    fn ok_score_is_half_of_great_score() {
        let chart = CanonicalChart {
            metadata: taiko_metadata("Hard", 5),
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
                id: LANE_DON,
                name: "don".to_owned(),
                role: LaneRole::TaikoDon,
            }],
            branch_segments: Vec::new(),
            objects: vec![Object {
                id: 1,
                kind: ObjectKind::Tap,
                start_tick: 1_000_000,
                end_tick: 1_000_000,
                lane_or_region: LaneOrRegion::Lane(LANE_DON),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: 1_000_000,
                branch_segment_id: None,
                branch_route_id: 0,
            }],
            events: Vec::new(),
        };

        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("engine");
        let input = TimedInput {
            tick: 1_000_000 + GREAT_WINDOW_TICKS,
            action: TaikoAction::Don,
        };
        engine.step_to(input.tick, &[input]).expect("step");

        let result = engine.finalize();
        assert_eq!(result.great, 0);
        assert_eq!(result.ok, 1);
        assert_eq!(result.score, 500_000);
    }
}
