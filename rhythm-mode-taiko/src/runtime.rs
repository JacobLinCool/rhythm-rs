use std::collections::{BTreeMap, VecDeque};

use rhythm_chart::{BranchDecisionPoint, CanonicalChart, ObjectKind, Tick};
use rhythm_core::{CompileError, ControlledEngine, FrameOutput, StepError, TimedInput};
use thiserror::Error;

use crate::{
    TaikoAction, TaikoBranchController, TaikoBranchError, TaikoBranchPolicy, TaikoFinalResult,
    TaikoMode, TaikoScoreState, MISS_WINDOW_TICKS,
};

/// An optional branch-route gate for a synthetic input such as autoplay.
///
/// Manual and authoritative network inputs are unconditional. A gated input is
/// consumed only when the named route is active at the input's exact tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaikoInputRoute {
    pub segment_id: u32,
    pub route_id: u8,
}

/// One input scheduled for the canonical taiko runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledTaikoInput {
    pub input: TimedInput<TaikoAction>,
    pub required_route: Option<TaikoInputRoute>,
}

impl ScheduledTaikoInput {
    pub const fn unconditional(input: TimedInput<TaikoAction>) -> Self {
        Self {
            input,
            required_route: None,
        }
    }

    pub const fn for_route(input: TimedInput<TaikoAction>, segment_id: u32, route_id: u8) -> Self {
        Self {
            input,
            required_route: Some(TaikoInputRoute {
                segment_id,
                route_id,
            }),
        }
    }
}

impl From<TimedInput<TaikoAction>> for ScheduledTaikoInput {
    fn from(input: TimedInput<TaikoAction>) -> Self {
        Self::unconditional(input)
    }
}

#[derive(Debug, Error)]
pub enum TaikoRuntimeBuildError {
    #[error("invalid taiko branch policy: {0}")]
    Branch(#[from] TaikoBranchError),
    #[error("taiko chart cannot compile: {0}")]
    Compile(#[from] CompileError),
}

/// Canonical exact-boundary orchestration for taiko simulation.
///
/// Every boundary uses one invariant ordering:
///
/// 1. expire notes and apply their judges to score;
/// 2. decide branches from that updated score and apply controls;
/// 3. consume inputs at the boundary.
///
/// The runtime owns the engine, branch controller, and deterministic resolution
/// schedule so clients and authoritative servers cannot choose different frame
/// partitioning rules.
pub struct TaikoRuntime {
    engine: ControlledEngine<TaikoMode>,
    branch: TaikoBranchController,
    resolution_boundaries: VecDeque<Tick>,
    finished: bool,
}

impl TaikoRuntime {
    pub fn new(
        chart: &CanonicalChart,
        policy: TaikoBranchPolicy,
        decisions: Vec<BranchDecisionPoint>,
    ) -> Result<Self, TaikoRuntimeBuildError> {
        let branch = TaikoBranchController::new(policy, decisions)?;
        let engine = ControlledEngine::<TaikoMode>::new_controlled(chart)?;
        Ok(Self {
            engine,
            branch,
            resolution_boundaries: resolution_boundaries(chart),
            finished: false,
        })
    }

    pub fn now(&self) -> Tick {
        self.engine.now()
    }

    pub fn score(&self) -> &TaikoScoreState {
        self.engine.score()
    }

    pub fn finalize(&self) -> TaikoFinalResult {
        self.engine.finalize()
    }

    pub fn replay_hash(&self) -> u64 {
        self.engine.replay_hash()
    }

    pub fn emitted_controls(&self) -> usize {
        self.branch.emitted_controls()
    }

    pub fn next_decision(&self) -> Option<&BranchDecisionPoint> {
        self.branch.next_decision()
    }

    pub fn current_routes(&self) -> &BTreeMap<u32, u8> {
        self.branch.current_routes()
    }

    pub fn route_for_tick(&self, segment_id: u32, tick: Tick) -> u8 {
        self.branch.route_for_tick(segment_id, tick)
    }

    pub fn input_is_enabled(&self, input: ScheduledTaikoInput) -> bool {
        input.required_route.is_none_or(|required| {
            self.route_for_tick(required.segment_id, input.input.tick) == required.route_id
        })
    }

    pub fn advance_to(
        &mut self,
        now: Tick,
        inputs: &[ScheduledTaikoInput],
    ) -> Result<FrameOutput<TaikoMode>, StepError> {
        validate_advance(self.engine.now(), now, inputs)?;
        if self.finished {
            // Terminal score, replay, and branch state are immutable. Advancing
            // after finish exists only to keep the client projection clock moving.
            return self.engine.step_to_with_controls(now, &[], &[]);
        }

        let mut input_cursor = 0_usize;
        let mut judges = Vec::new();

        let mut output = loop {
            while self
                .resolution_boundaries
                .front()
                .is_some_and(|tick| *tick < self.engine.now())
            {
                self.resolution_boundaries.pop_front();
            }

            let boundary = next_boundary(
                self.engine.now(),
                now,
                inputs.get(input_cursor).map(|input| input.input.tick),
                self.branch
                    .next_decision()
                    .map(|decision| decision.decision_tick),
                self.resolution_boundaries.front().copied(),
            )?;

            // This first zero-event step is intentional: the core engine applies
            // expiry judges to score before the branch controller observes it.
            let mut output = self.engine.step_to_with_controls(boundary, &[], &[])?;
            judges.append(&mut output.judges);

            let controls = self.branch.controls_for_tick(boundary, self.engine.score());
            let input_start = input_cursor;
            while input_cursor < inputs.len() && inputs[input_cursor].input.tick == boundary {
                input_cursor += 1;
            }
            let frame_inputs = inputs[input_start..input_cursor]
                .iter()
                .copied()
                .filter(|input| self.input_is_enabled(*input))
                .map(|input| input.input)
                .collect::<Vec<_>>();

            while self
                .resolution_boundaries
                .front()
                .is_some_and(|tick| *tick <= boundary)
            {
                self.resolution_boundaries.pop_front();
            }

            if !controls.is_empty() || !frame_inputs.is_empty() {
                output = self
                    .engine
                    .step_to_with_controls(boundary, &controls, &frame_inputs)?;
                judges.append(&mut output.judges);
            }

            self.finished |= output.finished;
            let should_stop = self.finished || boundary == now;
            if should_stop {
                break output;
            }
        };

        output.judges = judges;
        Ok(output)
    }
}

fn validate_advance(
    current: Tick,
    now: Tick,
    inputs: &[ScheduledTaikoInput],
) -> Result<(), StepError> {
    if now < current {
        return Err(StepError::NonMonotonicStep { now, current });
    }
    let mut previous = current;
    for input in inputs {
        if input.input.tick < previous {
            return Err(StepError::UnsortedInputs);
        }
        if input.input.tick > now {
            return Err(StepError::InputTickOutOfFrame);
        }
        previous = input.input.tick;
    }
    Ok(())
}

fn next_boundary(
    current: Tick,
    now: Tick,
    input: Option<Tick>,
    decision: Option<Tick>,
    resolution: Option<Tick>,
) -> Result<Tick, StepError> {
    let boundary = [input, decision, resolution]
        .into_iter()
        .flatten()
        .filter(|tick| *tick <= now)
        .min()
        .unwrap_or(now);
    if boundary < current {
        return Err(StepError::NonMonotonicStep {
            now: boundary,
            current,
        });
    }
    Ok(boundary)
}

fn resolution_boundaries(chart: &CanonicalChart) -> VecDeque<Tick> {
    let mut boundaries = Vec::with_capacity(chart.objects.len().saturating_mul(2));
    for object in &chart.objects {
        let resolution_tick = match object.kind {
            ObjectKind::Tap => object
                .start_tick
                .saturating_add(MISS_WINDOW_TICKS)
                .saturating_add(1),
            ObjectKind::Hold | ObjectKind::Roll | ObjectKind::Slide | ObjectKind::Touch => {
                object.end_tick.saturating_add(1)
            }
        };
        let branch_start_tick = object.start_tick.saturating_add(1);
        if object.branch_segment_id.is_some() && branch_start_tick != resolution_tick {
            boundaries.push(branch_start_tick);
        }
        boundaries.push(resolution_tick);
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    boundaries.into()
}

#[cfg(test)]
mod tests {
    use rhythm_chart::{
        accuracy_threshold_from_percent, BranchDecisionHint, BranchSegment, ChartMetadata, Lane,
        LaneOrRegion, LaneRole, Object, TempoChange, SCROLL_SCALE,
    };

    use super::*;
    use crate::{FLAG_BALLOON, LANE_DON};

    const DECISION_TICK: Tick = MISS_WINDOW_TICKS + 1;
    const BRANCH_NOTE_TICK: Tick = 500_000;

    fn cadence_chart() -> (CanonicalChart, Vec<BranchDecisionPoint>) {
        let mut objects = vec![Object {
            id: 1,
            kind: ObjectKind::Tap,
            start_tick: 0,
            end_tick: 0,
            lane_or_region: LaneOrRegion::Lane(LANE_DON),
            flags: 0,
            required_hits: 0,
            slide_to: None,
            scroll_scaled: SCROLL_SCALE,
            branch_segment_id: None,
            branch_route_id: 0,
        }];
        objects.extend((0_u8..3).map(|route_id| Object {
            id: u32::from(route_id) + 2,
            kind: ObjectKind::Tap,
            start_tick: BRANCH_NOTE_TICK,
            end_tick: BRANCH_NOTE_TICK,
            lane_or_region: LaneOrRegion::Lane(LANE_DON),
            flags: 0,
            required_hits: 0,
            slide_to: None,
            scroll_scaled: SCROLL_SCALE,
            branch_segment_id: Some(7),
            branch_route_id: route_id,
        }));
        let hint = BranchDecisionHint::Accuracy {
            low: accuracy_threshold_from_percent(70),
            high: accuracy_threshold_from_percent(90),
        };
        (
            CanonicalChart {
                metadata: ChartMetadata {
                    title: "Cadence".to_owned(),
                    difficulty_name: Some("Oni".to_owned()),
                    difficulty_level: Some(1),
                    ..ChartMetadata::default()
                },
                tempo_map: vec![TempoChange {
                    tick: 0,
                    micros_per_quarter: 500_000,
                }],
                signatures: Vec::new(),
                lanes: vec![Lane {
                    id: LANE_DON,
                    name: "Don".to_owned(),
                    role: LaneRole::TaikoDon,
                }],
                branch_segments: vec![BranchSegment {
                    id: 7,
                    default_route_id: 0,
                    route_count: 3,
                    decision_hint: Some(hint.clone()),
                }],
                objects,
                events: Vec::new(),
            },
            vec![BranchDecisionPoint {
                segment_id: 7,
                decision_tick: DECISION_TICK,
                default_route_id: 0,
                route_count: 3,
                hint: Some(hint),
            }],
        )
    }

    fn autoplay_candidates() -> Vec<ScheduledTaikoInput> {
        (0_u8..3)
            .map(|route_id| {
                ScheduledTaikoInput::for_route(
                    TimedInput {
                        tick: BRANCH_NOTE_TICK,
                        action: TaikoAction::LEFT_DON,
                    },
                    7,
                    route_id,
                )
            })
            .collect()
    }

    #[test]
    fn expiry_is_applied_before_same_tick_branch_decision() {
        let (chart, decisions) = cadence_chart();
        let mut runtime =
            TaikoRuntime::new(&chart, TaikoBranchPolicy::Automatic, decisions).expect("runtime");

        let output = runtime.advance_to(DECISION_TICK, &[]).expect("advance");

        assert_eq!(output.score.miss, 1);
        assert_eq!(runtime.current_routes().get(&7), Some(&0));
        assert_eq!(output.judges, vec![crate::TaikoJudge::MissExpired]);
    }

    #[test]
    fn incomplete_balloon_is_a_non_miss_at_the_shared_expiry_boundary() {
        let (mut chart, _) = cadence_chart();
        chart.branch_segments.clear();
        chart.objects = vec![Object {
            id: 1,
            kind: ObjectKind::Roll,
            start_tick: 100_000,
            end_tick: 200_000,
            lane_or_region: LaneOrRegion::Lane(LANE_DON),
            flags: FLAG_BALLOON,
            required_hits: 3,
            slide_to: None,
            scroll_scaled: SCROLL_SCALE,
            branch_segment_id: None,
            branch_route_id: 0,
        }];
        let mut runtime =
            TaikoRuntime::new(&chart, TaikoBranchPolicy::Disabled, Vec::new()).expect("runtime");
        let input = ScheduledTaikoInput::unconditional(TimedInput {
            tick: 150_000,
            action: TaikoAction::LEFT_DON,
        });

        let output = runtime.advance_to(250_000, &[input]).expect("advance");

        assert_eq!(output.judges, vec![crate::TaikoJudge::RollHit]);
        assert!(output.finished);
        assert_eq!(runtime.score().score, 100);
        assert_eq!(runtime.score().combo, 0);
        assert_eq!(runtime.score().miss, 0);
        assert_eq!(runtime.score().gauge, 0.0);
    }

    #[test]
    fn coarse_and_fine_cadence_are_replay_equivalent() {
        let (chart, decisions) = cadence_chart();
        let mut coarse = TaikoRuntime::new(&chart, TaikoBranchPolicy::Automatic, decisions.clone())
            .expect("coarse");
        let mut fine =
            TaikoRuntime::new(&chart, TaikoBranchPolicy::Automatic, decisions).expect("fine");
        let inputs = autoplay_candidates();

        let coarse_output = coarse.advance_to(700_000, &inputs).expect("coarse advance");
        for tick in [50_000, DECISION_TICK, 300_000] {
            fine.advance_to(tick, &[]).expect("fine intermediate");
        }
        let fine_output = fine.advance_to(700_000, &inputs).expect("fine advance");

        assert_eq!(coarse.finalize(), fine.finalize());
        assert_eq!(coarse.replay_hash(), fine.replay_hash());
        assert_eq!(coarse.current_routes(), fine.current_routes());
        assert_eq!(coarse.emitted_controls(), fine.emitted_controls());
        assert_eq!(coarse_output.now, fine_output.now);
        assert!(coarse_output.finished);
        assert!(fine_output.finished);
        assert_eq!(coarse.finalize().great, 1);
        assert_eq!(coarse.finalize().miss, 1);
    }
}
