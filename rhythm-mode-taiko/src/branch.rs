use std::collections::BTreeMap;

use rhythm_chart::{
    accuracy_threshold_from_percent, BranchDecisionHint, BranchDecisionPoint, Tick,
    ACCURACY_THRESHOLD_SCALE,
};
use rhythm_core::{BranchControl, TimedControl};
use thiserror::Error;

use crate::TaikoScoreState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaikoBranchPolicy {
    Automatic,
    Accuracy,
    Roll,
    Score,
    FixedRoute(u8),
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TaikoBranchError {
    #[error("branch segment {segment_id} has no decision hint for {policy:?}")]
    MissingHint {
        segment_id: u32,
        policy: TaikoBranchPolicy,
    },
    #[error("branch segment {segment_id} hint {actual} cannot be used by policy {policy:?}")]
    HintMismatch {
        segment_id: u32,
        policy: TaikoBranchPolicy,
        actual: &'static str,
    },
    #[error("branch segment {segment_id} uses an unsupported raw decision hint")]
    UnsupportedHint { segment_id: u32 },
    #[error(
        "fixed route {route_id} is invalid for branch segment {segment_id} with {route_count} routes"
    )]
    InvalidFixedRoute {
        segment_id: u32,
        route_id: u8,
        route_count: u8,
    },
    #[error("branch segment {segment_id} has no routes")]
    NoRoutes { segment_id: u32 },
    #[error(
        "default route {route_id} is invalid for branch segment {segment_id} with {route_count} routes"
    )]
    InvalidDefaultRoute {
        segment_id: u32,
        route_id: u8,
        route_count: u8,
    },
}

#[derive(Debug, Clone, Copy, Default)]
struct ScoreWindow {
    great: u32,
    ok: u32,
    miss: u32,
    roll_hits: u32,
    score: u32,
}

impl ScoreWindow {
    fn from_score(score: &TaikoScoreState) -> Self {
        Self {
            great: score.great,
            ok: score.ok,
            miss: score.miss,
            roll_hits: score.roll_hits,
            score: score.score,
        }
    }

    fn delta_from(self, baseline: Self) -> Self {
        Self {
            great: self.great.saturating_sub(baseline.great),
            ok: self.ok.saturating_sub(baseline.ok),
            miss: self.miss.saturating_sub(baseline.miss),
            roll_hits: self.roll_hits.saturating_sub(baseline.roll_hits),
            score: self.score.saturating_sub(baseline.score),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RouteAtTick {
    tick: Tick,
    route_id: u8,
}

#[derive(Debug, Clone)]
pub struct TaikoBranchController {
    policy: TaikoBranchPolicy,
    decisions: Vec<BranchDecisionPoint>,
    cursor: usize,
    baseline: ScoreWindow,
    routes: BTreeMap<u32, u8>,
    route_history: BTreeMap<u32, Vec<RouteAtTick>>,
    emitted_controls: usize,
}

impl TaikoBranchController {
    pub fn new(
        policy: TaikoBranchPolicy,
        mut decisions: Vec<BranchDecisionPoint>,
    ) -> Result<Self, TaikoBranchError> {
        decisions.sort_by_key(|point| (point.decision_tick, point.segment_id));
        for decision in &decisions {
            validate_decision(policy, decision)?;
        }

        let mut routes = BTreeMap::new();
        let mut route_history = BTreeMap::new();
        for decision in &decisions {
            routes
                .entry(decision.segment_id)
                .or_insert(decision.default_route_id);
            route_history.entry(decision.segment_id).or_insert_with(|| {
                vec![RouteAtTick {
                    tick: 0,
                    route_id: decision.default_route_id,
                }]
            });
        }

        Ok(Self {
            policy,
            decisions,
            cursor: 0,
            baseline: ScoreWindow::default(),
            routes,
            route_history,
            emitted_controls: 0,
        })
    }

    pub fn emitted_controls(&self) -> usize {
        self.emitted_controls
    }

    pub fn next_decision(&self) -> Option<&BranchDecisionPoint> {
        self.decisions.get(self.cursor)
    }

    pub fn current_routes(&self) -> &BTreeMap<u32, u8> {
        &self.routes
    }

    pub fn route_for_tick(&self, segment_id: u32, tick: Tick) -> u8 {
        let Some(history) = self.route_history.get(&segment_id) else {
            return 0;
        };
        let index = history.partition_point(|state| state.tick <= tick);
        index
            .checked_sub(1)
            .map_or(0, |index| history[index].route_id)
    }

    pub fn controls_for_tick(
        &mut self,
        now: Tick,
        score: &TaikoScoreState,
    ) -> Vec<TimedControl<BranchControl>> {
        let mut controls = Vec::new();
        while self.cursor < self.decisions.len() && self.decisions[self.cursor].decision_tick <= now
        {
            let decision_tick = self.decisions[self.cursor].decision_tick;
            let snapshot = ScoreWindow::from_score(score);
            let delta = snapshot.delta_from(self.baseline);

            while self.cursor < self.decisions.len()
                && self.decisions[self.cursor].decision_tick == decision_tick
            {
                let decision = &self.decisions[self.cursor];
                if let Some(route_id) = select_route(self.policy, decision, delta) {
                    controls.push(TimedControl {
                        tick: decision.decision_tick,
                        control: BranchControl::SetBranchRoute {
                            segment_id: decision.segment_id,
                            route_id,
                        },
                    });
                    self.routes.insert(decision.segment_id, route_id);
                    self.route_history
                        .entry(decision.segment_id)
                        .or_default()
                        .push(RouteAtTick {
                            tick: decision.decision_tick,
                            route_id,
                        });
                    self.emitted_controls = self.emitted_controls.saturating_add(1);
                }
                self.cursor += 1;
            }
            self.baseline = snapshot;
        }
        controls
    }
}

fn validate_decision(
    policy: TaikoBranchPolicy,
    decision: &BranchDecisionPoint,
) -> Result<(), TaikoBranchError> {
    if decision.route_count == 0 {
        return Err(TaikoBranchError::NoRoutes {
            segment_id: decision.segment_id,
        });
    }
    if decision.default_route_id >= decision.route_count {
        return Err(TaikoBranchError::InvalidDefaultRoute {
            segment_id: decision.segment_id,
            route_id: decision.default_route_id,
            route_count: decision.route_count,
        });
    }
    match policy {
        TaikoBranchPolicy::Disabled => Ok(()),
        TaikoBranchPolicy::FixedRoute(route_id) => {
            if route_id >= decision.route_count {
                Err(TaikoBranchError::InvalidFixedRoute {
                    segment_id: decision.segment_id,
                    route_id,
                    route_count: decision.route_count,
                })
            } else {
                Ok(())
            }
        }
        TaikoBranchPolicy::Automatic => match decision.hint.as_ref() {
            None => Err(TaikoBranchError::MissingHint {
                segment_id: decision.segment_id,
                policy,
            }),
            Some(BranchDecisionHint::Raw(_)) => Err(TaikoBranchError::UnsupportedHint {
                segment_id: decision.segment_id,
            }),
            Some(_) => Ok(()),
        },
        TaikoBranchPolicy::Accuracy | TaikoBranchPolicy::Roll | TaikoBranchPolicy::Score => {
            let hint = decision
                .hint
                .as_ref()
                .ok_or(TaikoBranchError::MissingHint {
                    segment_id: decision.segment_id,
                    policy,
                })?;
            let compatible = matches!(
                (policy, hint),
                (
                    TaikoBranchPolicy::Accuracy,
                    BranchDecisionHint::Accuracy { .. }
                ) | (TaikoBranchPolicy::Roll, BranchDecisionHint::Roll { .. })
                    | (TaikoBranchPolicy::Score, BranchDecisionHint::Score { .. })
            );
            if compatible {
                Ok(())
            } else {
                Err(TaikoBranchError::HintMismatch {
                    segment_id: decision.segment_id,
                    policy,
                    actual: hint_name(hint),
                })
            }
        }
    }
}

fn select_route(
    policy: TaikoBranchPolicy,
    decision: &BranchDecisionPoint,
    delta: ScoreWindow,
) -> Option<u8> {
    match policy {
        TaikoBranchPolicy::Disabled => None,
        TaikoBranchPolicy::FixedRoute(route_id) => Some(route_id),
        TaikoBranchPolicy::Accuracy => {
            let BranchDecisionHint::Accuracy { low, high } =
                decision.hint.as_ref().expect("policy validated")
            else {
                unreachable!("policy validated");
            };
            Some(threshold_route(
                accuracy_percent_scaled(delta),
                i64::from(*low),
                i64::from(*high),
                decision.route_count,
            ))
        }
        TaikoBranchPolicy::Roll => {
            let BranchDecisionHint::Roll { low, high } =
                decision.hint.as_ref().expect("policy validated")
            else {
                unreachable!("policy validated");
            };
            Some(threshold_route(
                i64::from(delta.roll_hits),
                i64::from(*low),
                i64::from(*high),
                decision.route_count,
            ))
        }
        TaikoBranchPolicy::Score => {
            let BranchDecisionHint::Score { low, high } =
                decision.hint.as_ref().expect("policy validated")
            else {
                unreachable!("policy validated");
            };
            Some(threshold_route(
                i64::from(delta.score),
                i64::from(*low),
                i64::from(*high),
                decision.route_count,
            ))
        }
        TaikoBranchPolicy::Automatic => {
            let hint = decision.hint.as_ref().expect("policy validated");
            Some(match hint {
                BranchDecisionHint::Accuracy { low, high } => threshold_route(
                    accuracy_percent_scaled(delta),
                    i64::from(*low),
                    i64::from(*high),
                    decision.route_count,
                ),
                BranchDecisionHint::Roll { low, high } => threshold_route(
                    i64::from(delta.roll_hits),
                    i64::from(*low),
                    i64::from(*high),
                    decision.route_count,
                ),
                BranchDecisionHint::Score { low, high } => threshold_route(
                    i64::from(delta.score),
                    i64::from(*low),
                    i64::from(*high),
                    decision.route_count,
                ),
                BranchDecisionHint::Raw(_) => unreachable!("policy validated"),
            })
        }
    }
}

fn hint_name(hint: &BranchDecisionHint) -> &'static str {
    match hint {
        BranchDecisionHint::Accuracy { .. } => "accuracy",
        BranchDecisionHint::Roll { .. } => "roll",
        BranchDecisionHint::Score { .. } => "score",
        BranchDecisionHint::Raw(_) => "raw",
    }
}

fn accuracy_percent_scaled(delta: ScoreWindow) -> i64 {
    let total = u64::from(delta.great) + u64::from(delta.ok) + u64::from(delta.miss);
    if total == 0 {
        return i64::from(accuracy_threshold_from_percent(100));
    }
    let weighted_numerator = i128::from(u64::from(delta.great) * 2 + u64::from(delta.ok));
    let weighted_denominator = i128::from(total) * 2;
    let value = weighted_numerator
        .saturating_mul(100)
        .saturating_mul(i128::from(ACCURACY_THRESHOLD_SCALE))
        / weighted_denominator;
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn threshold_route(value: i64, low: i64, high: i64, route_count: u8) -> u8 {
    let route = if value < low {
        0
    } else if value < high {
        1
    } else {
        2
    };
    route.min(route_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(hint: Option<BranchDecisionHint>) -> BranchDecisionPoint {
        BranchDecisionPoint {
            segment_id: 7,
            decision_tick: 100,
            default_route_id: 0,
            route_count: 3,
            hint,
        }
    }

    #[test]
    fn automatic_policy_uses_hint_and_score_window() {
        let mut controller = TaikoBranchController::new(
            TaikoBranchPolicy::Automatic,
            vec![point(Some(BranchDecisionHint::Accuracy {
                low: accuracy_threshold_from_percent(70),
                high: accuracy_threshold_from_percent(90),
            }))],
        )
        .expect("controller");
        let score = TaikoScoreState {
            great: 9,
            ok: 1,
            ..TaikoScoreState::default()
        };
        let controls = controller.controls_for_tick(100, &score);
        assert_eq!(controls.len(), 1);
        assert_eq!(controller.route_for_tick(7, 99), 0);
        assert_eq!(controller.route_for_tick(7, 100), 2);
    }

    #[test]
    fn invalid_policy_is_rejected_before_match_start() {
        assert!(matches!(
            TaikoBranchController::new(
                TaikoBranchPolicy::FixedRoute(3),
                vec![point(Some(BranchDecisionHint::Roll { low: 1, high: 2 }))]
            ),
            Err(TaikoBranchError::InvalidFixedRoute { .. })
        ));
        assert!(matches!(
            TaikoBranchController::new(
                TaikoBranchPolicy::Accuracy,
                vec![point(Some(BranchDecisionHint::Roll { low: 1, high: 2 }))]
            ),
            Err(TaikoBranchError::HintMismatch { .. })
        ));
        assert!(matches!(
            TaikoBranchController::new(
                TaikoBranchPolicy::Automatic,
                vec![point(Some(BranchDecisionHint::Raw("x".to_owned())))]
            ),
            Err(TaikoBranchError::UnsupportedHint { .. })
        ));
    }

    #[test]
    fn default_route_is_active_before_the_decision_tick() {
        let mut decision = point(None);
        decision.default_route_id = 1;
        let controller = TaikoBranchController::new(TaikoBranchPolicy::Disabled, vec![decision])
            .expect("controller");

        assert_eq!(controller.current_routes().get(&7), Some(&1));
        assert_eq!(controller.route_for_tick(7, 99), 1);
    }
}
