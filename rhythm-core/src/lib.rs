use std::hash::{Hash, Hasher};

use rhythm_chart::CanonicalChart;
use thiserror::Error;

pub type Tick = i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct NoControl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimedInput<A> {
    pub tick: Tick,
    pub action: A,
}

/// A deterministic control event injected by an external branch/controller layer.
///
/// Runtime consumes controls in ascending `tick` order before frame inputs.
///
/// ```no_run
/// use rhythm_core::{BranchControl, TimedControl};
///
/// let control = TimedControl {
///     tick: 1_000_000,
///     control: BranchControl::SetBranchRoute {
///         segment_id: 1,
///         route_id: 2,
///     },
/// };
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimedControl<C> {
    pub tick: Tick,
    pub control: C,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BranchControl {
    SetBranchRoute { segment_id: u32, route_id: u8 },
    EnableSegment { segment_id: u32 },
    DisableSegment { segment_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameOutput<M: Mode> {
    pub now: Tick,
    pub judges: Vec<M::Judge>,
    pub score: M::ScoreState,
    pub frame_view: M::FrameView,
    pub finished: bool,
    pub replay_hash: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayLog<A, C = NoControl> {
    pub inputs: Vec<TimedInput<A>>,
    pub controls: Vec<TimedControl<C>>,
    pub replay_hash: u64,
}

#[derive(Debug, Error)]
pub enum CompileError {
    #[error("invalid chart: {0}")]
    InvalidChart(#[from] rhythm_chart::ValidationError),
    #[error("unsupported chart feature: {0}")]
    Unsupported(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ControlError {
    #[error("unknown segment id {0}")]
    UnknownSegment(u32),
    #[error("invalid route {route_id} for segment {segment_id}")]
    InvalidRoute { segment_id: u32, route_id: u8 },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum StepError {
    #[error("non-monotonic step_to: now={now} < current={current}")]
    NonMonotonicStep { now: Tick, current: Tick },
    #[error("inputs must be sorted ascending and monotonic")]
    UnsortedInputs,
    #[error("input tick exceeds frame tick")]
    InputTickOutOfFrame,
    #[error("controls must be sorted ascending and monotonic")]
    UnsortedControls,
    #[error("control tick exceeds frame tick")]
    ControlTickOutOfFrame,
    #[error(transparent)]
    Control(#[from] ControlError),
}

pub trait Mode {
    type Action: Copy + Eq + Hash + Send + Sync + 'static;
    type Compiled;
    type Judge: Copy + Eq + Hash + Send + Sync + 'static;
    type ScoreState: Default + Clone;
    type FrameView: Clone;
    type FinalResult;

    fn init_score(_compiled: &Self::Compiled) -> Self::ScoreState {
        Self::ScoreState::default()
    }

    fn compile(chart: &CanonicalChart) -> Result<Self::Compiled, CompileError>;

    fn consume_input(
        compiled: &mut Self::Compiled,
        now: Tick,
        input: TimedInput<Self::Action>,
    ) -> Option<Self::Judge>;

    fn consume_expired(compiled: &mut Self::Compiled, now: Tick, out: &mut Vec<Self::Judge>);

    fn apply_judge(score: &mut Self::ScoreState, judge: Self::Judge);

    fn frame_view(
        compiled: &Self::Compiled,
        now: Tick,
        score: &Self::ScoreState,
    ) -> Self::FrameView;

    fn finalize(compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult;

    fn is_finished(compiled: &Self::Compiled, now: Tick) -> bool;
}

pub trait ControlledMode: Mode {
    type Control: Copy + Eq + Hash + Send + Sync + 'static;

    fn consume_control(
        compiled: &mut Self::Compiled,
        control: TimedControl<Self::Control>,
    ) -> Result<(), ControlError>;
}

pub type BasicEngine<M> = Engine<M, NoControl>;
pub type ControlledEngine<M> = Engine<M, <M as ControlledMode>::Control>;

#[derive(Debug)]
pub struct Engine<M: Mode, C: Copy + Eq + Hash + Send + Sync + 'static = NoControl> {
    compiled: M::Compiled,
    score: M::ScoreState,
    now: Tick,
    replay_inputs: Vec<TimedInput<M::Action>>,
    replay_controls: Vec<TimedControl<C>>,
    replay_hasher: Fnv1aHasher,
    scratch: Vec<M::Judge>,
}

impl<M: Mode, C: Copy + Eq + Hash + Send + Sync + 'static> Engine<M, C> {
    fn build(chart: &CanonicalChart) -> Result<Self, CompileError> {
        chart.validate()?;
        let compiled = M::compile(chart)?;
        let score = M::init_score(&compiled);

        Ok(Self {
            compiled,
            score,
            now: 0,
            replay_inputs: Vec::new(),
            replay_controls: Vec::new(),
            replay_hasher: Fnv1aHasher::default(),
            scratch: Vec::with_capacity(64),
        })
    }

    pub fn now(&self) -> Tick {
        self.now
    }

    pub fn score(&self) -> &M::ScoreState {
        &self.score
    }

    pub fn compiled(&self) -> &M::Compiled {
        &self.compiled
    }

    pub fn step_to(
        &mut self,
        now: Tick,
        inputs: &[TimedInput<M::Action>],
    ) -> Result<FrameOutput<M>, StepError> {
        validate_monotonic_step(self.now, now)?;
        validate_input_order(self.now, now, inputs)?;

        self.scratch.clear();

        // Deterministic execution order: expire first, then process current frame inputs.
        M::consume_expired(&mut self.compiled, now, &mut self.scratch);

        for input in inputs {
            self.replay_inputs.push(*input);
            replay_hash_value(&mut self.replay_hasher, input);

            if let Some(judge) = M::consume_input(&mut self.compiled, now, *input) {
                self.scratch.push(judge);
            }
        }

        for judge in self.scratch.iter().copied() {
            M::apply_judge(&mut self.score, judge);
            replay_hash_value(&mut self.replay_hasher, &judge);
        }

        self.now = now;

        Ok(FrameOutput {
            now,
            judges: self.scratch.to_vec(),
            score: self.score.clone(),
            frame_view: M::frame_view(&self.compiled, now, &self.score),
            finished: M::is_finished(&self.compiled, now),
            replay_hash: self.replay_hasher.finish(),
        })
    }

    pub fn replay_log(&self) -> ReplayLog<M::Action, C> {
        ReplayLog {
            inputs: self.replay_inputs.clone(),
            controls: self.replay_controls.clone(),
            replay_hash: self.replay_hasher.finish(),
        }
    }

    pub fn replay_hash(&self) -> u64 {
        self.replay_hasher.finish()
    }

    pub fn finalize(&self) -> M::FinalResult {
        M::finalize(&self.compiled, &self.score)
    }
}

impl<M: Mode> Engine<M, NoControl> {
    pub fn new_basic(chart: &CanonicalChart) -> Result<Self, CompileError> {
        Self::build(chart)
    }
}

impl<M: ControlledMode> Engine<M, M::Control> {
    pub fn new_controlled(chart: &CanonicalChart) -> Result<Self, CompileError> {
        Self::build(chart)
    }

    /// Step simulation with both controls and inputs.
    ///
    /// Deterministic order is fixed:
    /// `consume_expired -> consume_control -> consume_input -> apply_judge`.
    ///
    /// # Errors
    /// Returns [`StepError`] if ticks are invalid or mode rejects a control.
    ///
    /// ```no_run
    /// use rhythm_chart::{CanonicalChart, ChartMetadata, Lane, LaneRole, TempoChange};
    /// use rhythm_core::{
    ///     BranchControl, CompileError, ControlError, ControlledEngine, ControlledMode, Mode, Tick,
    ///     TimedControl, TimedInput,
    /// };
    ///
    /// #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    /// enum DemoAction {
    ///     Hit,
    /// }
    ///
    /// #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    /// enum DemoJudge {
    ///     Ok,
    /// }
    ///
    /// struct DemoMode;
    ///
    /// impl Mode for DemoMode {
    ///     type Action = DemoAction;
    ///     type Compiled = ();
    ///     type Judge = DemoJudge;
    ///     type ScoreState = u32;
    ///     type FrameView = ();
    ///     type FinalResult = u32;
    ///
    ///     fn compile(_chart: &CanonicalChart) -> Result<Self::Compiled, CompileError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn consume_input(
    ///         _compiled: &mut Self::Compiled,
    ///         _now: Tick,
    ///         _input: TimedInput<Self::Action>,
    ///     ) -> Option<Self::Judge> {
    ///         Some(DemoJudge::Ok)
    ///     }
    ///
    ///     fn consume_expired(_compiled: &mut Self::Compiled, _now: Tick, _out: &mut Vec<Self::Judge>) {}
    ///
    ///     fn apply_judge(_score: &mut Self::ScoreState, _judge: Self::Judge) {}
    ///
    ///     fn frame_view(_compiled: &Self::Compiled, _now: Tick, _score: &Self::ScoreState) -> Self::FrameView {}
    ///
    ///     fn finalize(_compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult {
    ///         *score
    ///     }
    ///
    ///     fn is_finished(_compiled: &Self::Compiled, _now: Tick) -> bool {
    ///         false
    ///     }
    /// }
    ///
    /// impl ControlledMode for DemoMode {
    ///     type Control = BranchControl;
    ///
    ///     fn consume_control(
    ///         _compiled: &mut Self::Compiled,
    ///         _control: TimedControl<Self::Control>,
    ///     ) -> Result<(), ControlError> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let chart = CanonicalChart {
    ///     metadata: ChartMetadata::default(),
    ///     tempo_map: vec![TempoChange {
    ///         tick: 0,
    ///         micros_per_quarter: 500_000,
    ///     }],
    ///     signatures: Vec::new(),
    ///     lanes: vec![Lane {
    ///         id: 0,
    ///         name: "lane".to_owned(),
    ///         role: LaneRole::Generic,
    ///     }],
    ///     branch_segments: Vec::new(),
    ///     objects: Vec::new(),
    ///     events: Vec::new(),
    /// };
    ///
    /// let mut engine = ControlledEngine::<DemoMode>::new_controlled(&chart)?;
    /// let controls = [TimedControl {
    ///     tick: 1_000_000,
    ///     control: BranchControl::EnableSegment { segment_id: 1 },
    /// }];
    /// let inputs = [TimedInput {
    ///     tick: 1_000_000,
    ///     action: DemoAction::Hit,
    /// }];
    ///
    /// let _frame = engine.step_to_with_controls(1_000_000, &controls, &inputs)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn step_to_with_controls(
        &mut self,
        now: Tick,
        controls: &[TimedControl<M::Control>],
        inputs: &[TimedInput<M::Action>],
    ) -> Result<FrameOutput<M>, StepError> {
        validate_monotonic_step(self.now, now)?;
        validate_control_order(self.now, now, controls)?;
        validate_input_order(self.now, now, inputs)?;

        self.scratch.clear();

        // Deterministic execution order: expire -> controls -> inputs.
        M::consume_expired(&mut self.compiled, now, &mut self.scratch);

        for control in controls {
            self.replay_controls.push(*control);
            replay_hash_value(&mut self.replay_hasher, control);
            M::consume_control(&mut self.compiled, *control)?;
        }

        for input in inputs {
            self.replay_inputs.push(*input);
            replay_hash_value(&mut self.replay_hasher, input);

            if let Some(judge) = M::consume_input(&mut self.compiled, now, *input) {
                self.scratch.push(judge);
            }
        }

        for judge in self.scratch.iter().copied() {
            M::apply_judge(&mut self.score, judge);
            replay_hash_value(&mut self.replay_hasher, &judge);
        }

        self.now = now;

        Ok(FrameOutput {
            now,
            judges: self.scratch.to_vec(),
            score: self.score.clone(),
            frame_view: M::frame_view(&self.compiled, now, &self.score),
            finished: M::is_finished(&self.compiled, now),
            replay_hash: self.replay_hasher.finish(),
        })
    }
}

fn validate_monotonic_step(current: Tick, now: Tick) -> Result<(), StepError> {
    if now < current {
        return Err(StepError::NonMonotonicStep { now, current });
    }
    Ok(())
}

fn validate_input_order<A>(
    from: Tick,
    to: Tick,
    inputs: &[TimedInput<A>],
) -> Result<(), StepError> {
    let mut last_tick = from;
    for input in inputs {
        if input.tick < last_tick {
            return Err(StepError::UnsortedInputs);
        }
        if input.tick > to {
            return Err(StepError::InputTickOutOfFrame);
        }
        last_tick = input.tick;
    }
    Ok(())
}

fn validate_control_order<C>(
    from: Tick,
    to: Tick,
    controls: &[TimedControl<C>],
) -> Result<(), StepError> {
    let mut last_tick = from;
    for control in controls {
        if control.tick < last_tick {
            return Err(StepError::UnsortedControls);
        }
        if control.tick > to {
            return Err(StepError::ControlTickOutOfFrame);
        }
        last_tick = control.tick;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Fnv1aHasher {
    state: u64,
}

impl Default for Fnv1aHasher {
    fn default() -> Self {
        Self {
            state: 0xcbf29ce484222325,
        }
    }
}

impl Hasher for Fnv1aHasher {
    fn finish(&self) -> u64 {
        self.state
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.state ^= u64::from(*b);
            self.state = self.state.wrapping_mul(0x100000001b3);
        }
    }
}

fn replay_hash_value<T: Hash>(hasher: &mut Fnv1aHasher, value: &T) {
    value.hash(hasher);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhythm_chart::{
        BranchSegment, ChartMetadata, Lane, LaneOrRegion, LaneRole, Object, ObjectKind, TempoChange,
    };

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Action {
        Hit,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    enum Judge {
        Great,
        Miss,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct Score {
        great: u32,
        miss: u32,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct View {
        pending: usize,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Final {
        great: u32,
        miss: u32,
    }

    #[derive(Debug, Clone)]
    struct Note {
        start: Tick,
        hit: bool,
        segment_id: Option<u32>,
        route_id: u8,
    }

    #[derive(Debug, Clone)]
    struct DummyCompiled {
        cursor: usize,
        notes: Vec<Note>,
        selected_route: u8,
        enabled: bool,
    }

    #[derive(Debug)]
    struct DummyMode;

    impl Mode for DummyMode {
        type Action = Action;
        type Compiled = DummyCompiled;
        type Judge = Judge;
        type ScoreState = Score;
        type FrameView = View;
        type FinalResult = Final;

        fn compile(chart: &CanonicalChart) -> Result<Self::Compiled, CompileError> {
            let notes = chart
                .objects
                .iter()
                .map(|o| Note {
                    start: o.start_tick,
                    hit: false,
                    segment_id: o.branch_segment_id,
                    route_id: o.branch_route_id,
                })
                .collect::<Vec<_>>();
            Ok(DummyCompiled {
                cursor: 0,
                notes,
                selected_route: 0,
                enabled: true,
            })
        }

        fn consume_input(
            compiled: &mut Self::Compiled,
            now: Tick,
            _input: TimedInput<Self::Action>,
        ) -> Option<Self::Judge> {
            while compiled.cursor < compiled.notes.len() {
                let note = &mut compiled.notes[compiled.cursor];
                if note.hit {
                    compiled.cursor += 1;
                    continue;
                }

                if !compiled.enabled {
                    break;
                }

                if note.segment_id.is_some() && note.route_id != compiled.selected_route {
                    break;
                }

                if (note.start - now).abs() <= 5 {
                    note.hit = true;
                    compiled.cursor += 1;
                    return Some(Judge::Great);
                }
                break;
            }
            None
        }

        fn consume_expired(compiled: &mut Self::Compiled, now: Tick, out: &mut Vec<Self::Judge>) {
            while compiled.cursor < compiled.notes.len() {
                let note = &mut compiled.notes[compiled.cursor];
                if note.hit {
                    compiled.cursor += 1;
                    continue;
                }

                if note.segment_id.is_some() && note.route_id != compiled.selected_route {
                    if note.start < now {
                        note.hit = true;
                        compiled.cursor += 1;
                    }
                    break;
                }

                if !compiled.enabled {
                    if note.start < now {
                        note.hit = true;
                        compiled.cursor += 1;
                    }
                    break;
                }

                if now > note.start + 5 {
                    note.hit = true;
                    compiled.cursor += 1;
                    out.push(Judge::Miss);
                } else {
                    break;
                }
            }
        }

        fn apply_judge(score: &mut Self::ScoreState, judge: Self::Judge) {
            match judge {
                Judge::Great => score.great += 1,
                Judge::Miss => score.miss += 1,
            }
        }

        fn frame_view(
            compiled: &Self::Compiled,
            _now: Tick,
            _score: &Self::ScoreState,
        ) -> Self::FrameView {
            let pending = compiled.notes.iter().filter(|n| !n.hit).count();
            View { pending }
        }

        fn finalize(_compiled: &Self::Compiled, score: &Self::ScoreState) -> Self::FinalResult {
            Final {
                great: score.great,
                miss: score.miss,
            }
        }

        fn is_finished(compiled: &Self::Compiled, _now: Tick) -> bool {
            compiled.notes.iter().all(|n| n.hit)
        }
    }

    impl ControlledMode for DummyMode {
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
                    if segment_id != 1 {
                        return Err(ControlError::UnknownSegment(segment_id));
                    }
                    if route_id > 2 {
                        return Err(ControlError::InvalidRoute {
                            segment_id,
                            route_id,
                        });
                    }
                    compiled.selected_route = route_id;
                    Ok(())
                }
                BranchControl::EnableSegment { segment_id } => {
                    if segment_id != 1 {
                        return Err(ControlError::UnknownSegment(segment_id));
                    }
                    compiled.enabled = true;
                    Ok(())
                }
                BranchControl::DisableSegment { segment_id } => {
                    if segment_id != 1 {
                        return Err(ControlError::UnknownSegment(segment_id));
                    }
                    compiled.enabled = false;
                    Ok(())
                }
            }
        }
    }

    fn chart() -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata::default(),
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: Vec::new(),
            lanes: vec![Lane {
                id: 0,
                name: "lane".to_owned(),
                role: LaneRole::Generic,
            }],
            branch_segments: vec![BranchSegment {
                id: 1,
                default_route_id: 0,
                route_count: 3,
                decision_hint: None,
            }],
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 100,
                    end_tick: 100,
                    lane_or_region: LaneOrRegion::Lane(0),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: Some(1),
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Tap,
                    start_tick: 200,
                    end_tick: 200,
                    lane_or_region: LaneOrRegion::Lane(0),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: Some(1),
                    branch_route_id: 1,
                },
            ],
            events: Vec::new(),
        }
    }

    #[test]
    fn deterministic_replay_hash() {
        let chart = chart();
        let mut a = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine a");
        let mut b = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine b");

        for tick in (0..=220).step_by(10) {
            let mut inputs = Vec::new();
            if tick == 100 {
                inputs.push(TimedInput {
                    tick,
                    action: Action::Hit,
                });
            }
            a.step_to(tick, &inputs).expect("step a");
            b.step_to(tick, &inputs).expect("step b");
        }

        assert_eq!(a.replay_hash(), b.replay_hash());

        let final_a = a.finalize();
        assert_eq!(final_a.great, 1);
        assert_eq!(final_a.miss, 0);
    }

    #[test]
    fn deterministic_replay_hash_with_controls() {
        let chart = chart();
        let mut a = ControlledEngine::<DummyMode>::new_controlled(&chart).expect("engine a");
        let mut b = ControlledEngine::<DummyMode>::new_controlled(&chart).expect("engine b");

        for tick in (0..=220).step_by(10) {
            let controls = if tick == 150 {
                vec![TimedControl {
                    tick,
                    control: BranchControl::SetBranchRoute {
                        segment_id: 1,
                        route_id: 1,
                    },
                }]
            } else {
                Vec::new()
            };
            let inputs = if tick == 200 {
                vec![TimedInput {
                    tick,
                    action: Action::Hit,
                }]
            } else {
                Vec::new()
            };

            a.step_to_with_controls(tick, &controls, &inputs)
                .expect("step a");
            b.step_to_with_controls(tick, &controls, &inputs)
                .expect("step b");
        }

        assert_eq!(a.replay_hash(), b.replay_hash());

        let replay = a.replay_log();
        assert_eq!(replay.controls.len(), 1);
        assert_eq!(a.finalize().great, 1);
    }

    #[test]
    fn reject_unsorted_controls() {
        let chart = chart();
        let mut engine = ControlledEngine::<DummyMode>::new_controlled(&chart).expect("engine");
        let controls = vec![
            TimedControl {
                tick: 120,
                control: BranchControl::EnableSegment { segment_id: 1 },
            },
            TimedControl {
                tick: 110,
                control: BranchControl::DisableSegment { segment_id: 1 },
            },
        ];

        let err = engine
            .step_to_with_controls(120, &controls, &[])
            .expect_err("must fail");
        assert_eq!(err, StepError::UnsortedControls);
    }

    #[test]
    fn reject_control_tick_outside_frame() {
        let chart = chart();
        let mut engine = ControlledEngine::<DummyMode>::new_controlled(&chart).expect("engine");
        let controls = vec![TimedControl {
            tick: 130,
            control: BranchControl::EnableSegment { segment_id: 1 },
        }];

        let err = engine
            .step_to_with_controls(120, &controls, &[])
            .expect_err("must fail");
        assert_eq!(err, StepError::ControlTickOutOfFrame);
    }

    #[test]
    fn propagate_control_errors() {
        let chart = chart();
        let mut engine = ControlledEngine::<DummyMode>::new_controlled(&chart).expect("engine");
        let controls = vec![TimedControl {
            tick: 100,
            control: BranchControl::EnableSegment { segment_id: 99 },
        }];

        let err = engine
            .step_to_with_controls(100, &controls, &[])
            .expect_err("must fail");
        assert_eq!(err, StepError::Control(ControlError::UnknownSegment(99)));
    }

    #[test]
    fn reject_unsorted_inputs() {
        let chart = chart();
        let mut engine = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine");
        let inputs = [
            TimedInput {
                tick: 120,
                action: Action::Hit,
            },
            TimedInput {
                tick: 110,
                action: Action::Hit,
            },
        ];
        let err = engine.step_to(120, &inputs).expect_err("must fail");
        assert_eq!(err, StepError::UnsortedInputs);
    }

    #[test]
    fn reject_non_monotonic_step() {
        let chart = chart();
        let mut engine = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine");
        engine.step_to(100, &[]).expect("first step");
        let err = engine.step_to(90, &[]).expect_err("must fail");
        assert_eq!(
            err,
            StepError::NonMonotonicStep {
                now: 90,
                current: 100
            }
        );
    }

    #[test]
    fn reject_input_tick_outside_frame() {
        let chart = chart();
        let mut engine = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine");
        let err = engine
            .step_to(
                120,
                &[TimedInput {
                    tick: 130,
                    action: Action::Hit,
                }],
            )
            .expect_err("must fail");
        assert_eq!(err, StepError::InputTickOutOfFrame);
    }

    #[test]
    fn replay_100_times_consistent() {
        let chart = chart();
        let mut hashes = Vec::new();

        for _ in 0..100 {
            let mut engine = BasicEngine::<DummyMode>::new_basic(&chart).expect("engine");
            for tick in (0..=220).step_by(10) {
                let inputs = if tick == 100 {
                    vec![TimedInput {
                        tick,
                        action: Action::Hit,
                    }]
                } else {
                    Vec::new()
                };
                engine.step_to(tick, &inputs).expect("step");
            }
            hashes.push(engine.replay_hash());
        }

        assert!(hashes.windows(2).all(|w| w[0] == w[1]));
    }
}
