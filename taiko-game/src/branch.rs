use std::collections::BTreeMap;
use std::fmt;

use rhythm_chart::{
    accuracy_threshold_from_percent, BranchDecisionHint, Tick, ACCURACY_THRESHOLD_SCALE,
};
use rhythm_core::{BranchControl, TimedControl};
use rhythm_importer_tja::BranchDecisionPoint;
use rhythm_mode_taiko::TaikoScoreState;

use crate::cli::BranchPolicy;

#[derive(Debug)]
pub enum BranchError {
    MissingHint {
        segment_id: u32,
        policy: BranchPolicy,
    },
    HintMismatch {
        segment_id: u32,
        policy: BranchPolicy,
        hint: &'static str,
    },
    UnsupportedHint {
        segment_id: u32,
    },
    InvalidFixedRoute {
        segment_id: u32,
        route_id: u8,
        route_count: u8,
    },
}

impl fmt::Display for BranchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHint { segment_id, policy } => {
                write!(f, "missing branch hint for segment {segment_id} under policy {policy:?}")
            }
            Self::HintMismatch {
                segment_id,
                policy,
                hint,
            } => write!(
                f,
                "branch hint mismatch for segment {segment_id}: policy {policy:?} cannot use hint {hint}"
            ),
            Self::UnsupportedHint { segment_id } => {
                write!(f, "unsupported branch hint for segment {segment_id}")
            }
            Self::InvalidFixedRoute {
                segment_id,
                route_id,
                route_count,
            } => write!(
                f,
                "invalid fixed route {route_id} for segment {segment_id} with route_count {route_count}"
            ),
        }
    }
}

impl std::error::Error for BranchError {}

#[derive(Debug, Clone, Copy, Default)]
struct ScoreSnapshot {
    great: u32,
    ok: u32,
    miss: u32,
    roll_hits: u32,
    score: u32,
}

impl ScoreSnapshot {
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
struct RouteStateAtTick {
    tick: Tick,
    route_id: u8,
}

#[derive(Debug, Clone)]
pub struct BranchController {
    policy: BranchPolicy,
    fixed_route: u8,
    decisions: Vec<BranchDecisionPoint>,
    cursor: usize,
    baseline: ScoreSnapshot,
    routes: BTreeMap<u32, u8>,
    route_history: BTreeMap<u32, Vec<RouteStateAtTick>>,
    emitted_controls: usize,
}

impl BranchController {
    pub fn new(
        policy: BranchPolicy,
        fixed_route: u8,
        mut decisions: Vec<BranchDecisionPoint>,
    ) -> Self {
        decisions.sort_by_key(|point| (point.decision_tick, point.segment_id));

        let mut routes = BTreeMap::new();
        let mut route_history = BTreeMap::new();
        for decision in &decisions {
            routes.entry(decision.segment_id).or_insert(0_u8);
            route_history.entry(decision.segment_id).or_insert_with(|| {
                vec![RouteStateAtTick {
                    tick: 0,
                    route_id: 0,
                }]
            });
        }

        Self {
            policy,
            fixed_route,
            decisions,
            cursor: 0,
            baseline: ScoreSnapshot::default(),
            routes,
            route_history,
            emitted_controls: 0,
        }
    }

    pub fn emitted_controls(&self) -> usize {
        self.emitted_controls
    }

    #[cfg(test)]
    pub fn next_decision(&self) -> Option<&BranchDecisionPoint> {
        self.decisions.get(self.cursor)
    }

    #[cfg(test)]
    pub fn current_routes(&self) -> &BTreeMap<u32, u8> {
        &self.routes
    }

    pub fn route_for_tick(&self, segment_id: u32, tick: Tick) -> u8 {
        let Some(history) = self.route_history.get(&segment_id) else {
            return 0;
        };

        let idx = history.partition_point(|state| state.tick <= tick);
        if idx == 0 {
            0
        } else {
            history[idx - 1].route_id
        }
    }

    pub fn controls_for_tick(
        &mut self,
        now: Tick,
        score: &TaikoScoreState,
    ) -> Result<Vec<TimedControl<BranchControl>>, BranchError> {
        let mut controls = Vec::new();

        while self.cursor < self.decisions.len() && self.decisions[self.cursor].decision_tick <= now
        {
            let decision_tick = self.decisions[self.cursor].decision_tick;
            let snapshot = ScoreSnapshot::from_score(score);
            let delta = snapshot.delta_from(self.baseline);

            while self.cursor < self.decisions.len()
                && self.decisions[self.cursor].decision_tick == decision_tick
            {
                let decision = &self.decisions[self.cursor];
                if let Some(route_id) = self.select_route(decision, delta)? {
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
                        .push(RouteStateAtTick {
                            tick: decision.decision_tick,
                            route_id,
                        });
                    self.emitted_controls = self.emitted_controls.saturating_add(1);
                }
                self.cursor += 1;
            }

            self.baseline = snapshot;
        }

        Ok(controls)
    }

    fn select_route(
        &self,
        decision: &BranchDecisionPoint,
        delta: ScoreSnapshot,
    ) -> Result<Option<u8>, BranchError> {
        match self.policy {
            BranchPolicy::None => Ok(None),
            BranchPolicy::FixedRoute => {
                if self.fixed_route >= decision.route_count {
                    return Err(BranchError::InvalidFixedRoute {
                        segment_id: decision.segment_id,
                        route_id: self.fixed_route,
                        route_count: decision.route_count,
                    });
                }
                Ok(Some(self.fixed_route))
            }
            BranchPolicy::Accuracy => {
                let hint = decision.hint.as_ref().ok_or(BranchError::MissingHint {
                    segment_id: decision.segment_id,
                    policy: self.policy,
                })?;
                let (low, high) = match hint {
                    BranchDecisionHint::Accuracy { low, high } => {
                        (i64::from(*low), i64::from(*high))
                    }
                    other => {
                        return Err(BranchError::HintMismatch {
                            segment_id: decision.segment_id,
                            policy: self.policy,
                            hint: hint_name(other),
                        });
                    }
                };
                Ok(Some(threshold_route(
                    accuracy_percent_scaled(delta),
                    low,
                    high,
                    decision.route_count,
                )))
            }
            BranchPolicy::Roll => {
                let hint = decision.hint.as_ref().ok_or(BranchError::MissingHint {
                    segment_id: decision.segment_id,
                    policy: self.policy,
                })?;
                let (low, high) = match hint {
                    BranchDecisionHint::Roll { low, high } => (i64::from(*low), i64::from(*high)),
                    other => {
                        return Err(BranchError::HintMismatch {
                            segment_id: decision.segment_id,
                            policy: self.policy,
                            hint: hint_name(other),
                        });
                    }
                };
                Ok(Some(threshold_route(
                    i64::from(delta.roll_hits),
                    low,
                    high,
                    decision.route_count,
                )))
            }
            BranchPolicy::Score => {
                let hint = decision.hint.as_ref().ok_or(BranchError::MissingHint {
                    segment_id: decision.segment_id,
                    policy: self.policy,
                })?;
                let (low, high) = match hint {
                    BranchDecisionHint::Score { low, high } => (i64::from(*low), i64::from(*high)),
                    other => {
                        return Err(BranchError::HintMismatch {
                            segment_id: decision.segment_id,
                            policy: self.policy,
                            hint: hint_name(other),
                        });
                    }
                };
                Ok(Some(threshold_route(
                    i64::from(delta.score),
                    low,
                    high,
                    decision.route_count,
                )))
            }
            BranchPolicy::Auto => {
                let hint = decision.hint.as_ref().ok_or(BranchError::MissingHint {
                    segment_id: decision.segment_id,
                    policy: self.policy,
                })?;
                let route = match hint {
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
                    BranchDecisionHint::Raw(_) => {
                        return Err(BranchError::UnsupportedHint {
                            segment_id: decision.segment_id,
                        })
                    }
                };
                Ok(Some(route))
            }
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

fn accuracy_percent_scaled(delta: ScoreSnapshot) -> i64 {
    let total = u64::from(delta.great) + u64::from(delta.ok) + u64::from(delta.miss);
    if total == 0 {
        return i64::from(accuracy_threshold_from_percent(100));
    }

    // TJA accuracy branching uses Great=1.0 and OK=0.5 weight.
    let weighted_num = i128::from(u64::from(delta.great) * 2 + u64::from(delta.ok));
    let weighted_den = i128::from(total) * 2;
    let value = weighted_num
        .saturating_mul(100)
        .saturating_mul(i128::from(ACCURACY_THRESHOLD_SCALE))
        / weighted_den;
    value.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn threshold_route(value: i64, low: i64, high: i64, route_count: u8) -> u8 {
    if route_count <= 1 {
        return 0;
    }

    let raw_route = if value < low {
        0
    } else if value < high {
        1
    } else {
        2
    };

    raw_route.min(route_count.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(
        segment_id: u32,
        decision_tick: Tick,
        hint: BranchDecisionHint,
    ) -> BranchDecisionPoint {
        BranchDecisionPoint {
            segment_id,
            decision_tick,
            route_count: 3,
            hint: Some(hint),
        }
    }

    #[test]
    fn accuracy_policy_uses_window_delta() {
        let decisions = vec![
            point(
                1,
                100,
                BranchDecisionHint::Accuracy {
                    low: accuracy_threshold_from_percent(70),
                    high: accuracy_threshold_from_percent(90),
                },
            ),
            point(
                2,
                200,
                BranchDecisionHint::Accuracy {
                    low: accuracy_threshold_from_percent(70),
                    high: accuracy_threshold_from_percent(90),
                },
            ),
        ];

        let mut controller = BranchController::new(BranchPolicy::Accuracy, 0, decisions);

        let mut score = TaikoScoreState::default();
        score.great = 9;
        score.ok = 1;
        score.miss = 0;

        let controls = controller.controls_for_tick(100, &score).expect("controls");
        assert_eq!(controls.len(), 1);
        assert_eq!(controller.current_routes().get(&1), Some(&2));

        score.great = 10;
        score.ok = 1;
        score.miss = 4;

        let controls = controller.controls_for_tick(200, &score).expect("controls");
        assert_eq!(controls.len(), 1);
        assert_eq!(controller.current_routes().get(&2), Some(&0));
    }

    #[test]
    fn accuracy_policy_supports_decimal_thresholds() {
        let decisions = vec![point(
            1,
            100,
            BranchDecisionHint::Accuracy {
                low: 605_042,
                high: accuracy_threshold_from_percent(80),
            },
        )];

        let mut controller = BranchController::new(BranchPolicy::Accuracy, 0, decisions);

        let mut score = TaikoScoreState::default();
        score.great = 121;
        score.ok = 0;
        score.miss = 79;

        let _ = controller.controls_for_tick(100, &score).expect("controls");
        assert_eq!(controller.current_routes().get(&1), Some(&0));
    }

    #[test]
    fn accuracy_policy_allows_negative_threshold_force_branch() {
        let decisions = vec![point(
            1,
            100,
            BranchDecisionHint::Accuracy {
                low: -20_000,
                high: -10_000,
            },
        )];

        let mut controller = BranchController::new(BranchPolicy::Accuracy, 0, decisions);
        let _ = controller
            .controls_for_tick(100, &TaikoScoreState::default())
            .expect("controls");
        assert_eq!(controller.current_routes().get(&1), Some(&2));
    }

    #[test]
    fn roll_policy_allows_negative_threshold_force_branch() {
        let decisions = vec![point(
            1,
            100,
            BranchDecisionHint::Roll { low: -2, high: -1 },
        )];

        let mut controller = BranchController::new(BranchPolicy::Roll, 0, decisions);
        let _ = controller
            .controls_for_tick(100, &TaikoScoreState::default())
            .expect("controls");
        assert_eq!(controller.current_routes().get(&1), Some(&2));
    }

    #[test]
    fn auto_policy_rejects_raw_hint() {
        let decisions = vec![point(1, 100, BranchDecisionHint::Raw("x".to_owned()))];
        let mut controller = BranchController::new(BranchPolicy::Auto, 0, decisions);

        let err = controller
            .controls_for_tick(100, &TaikoScoreState::default())
            .expect_err("must fail");

        assert!(matches!(
            err,
            BranchError::UnsupportedHint { segment_id: 1 }
        ));
    }

    #[test]
    fn route_for_tick_uses_history_instead_of_current_route() {
        let decisions = vec![BranchDecisionPoint {
            segment_id: 7,
            decision_tick: 100,
            route_count: 3,
            hint: None,
        }];

        let mut controller = BranchController::new(BranchPolicy::FixedRoute, 2, decisions);
        let _ = controller
            .controls_for_tick(100, &TaikoScoreState::default())
            .expect("controls");

        assert_eq!(controller.route_for_tick(7, 99), 0);
        assert_eq!(controller.route_for_tick(7, 100), 2);
        assert_eq!(controller.route_for_tick(7, 120), 2);
    }
}
