use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Tick = i64;
pub const TICKS_PER_SECOND: Tick = 1_000_000;
pub const SCROLL_SCALE: i32 = 1_000_000;
pub const ACCURACY_THRESHOLD_SCALE: i32 = 10_000;

const fn default_scroll_scaled() -> i32 {
    SCROLL_SCALE
}

fn is_default_scroll_scaled(value: &i32) -> bool {
    *value == SCROLL_SCALE
}

#[must_use]
pub const fn accuracy_threshold_from_percent(percent: i32) -> i32 {
    percent * ACCURACY_THRESHOLD_SCALE
}

#[must_use]
pub fn format_accuracy_threshold(threshold: i32) -> String {
    let abs = i64::from(threshold).abs();
    let scale = i64::from(ACCURACY_THRESHOLD_SCALE);
    let whole = abs / scale;
    let fraction = abs % scale;
    if fraction == 0 {
        return if threshold.is_negative() {
            format!("-{whole}")
        } else {
            whole.to_string()
        };
    }

    let mut fraction_str = format!("{fraction:04}");
    while fraction_str.ends_with('0') {
        fraction_str.pop();
    }
    if threshold.is_negative() {
        format!("-{whole}.{fraction_str}")
    } else {
        format!("{whole}.{fraction_str}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ChartMetadata {
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub charter: String,
    pub audio_path: Option<String>,
    pub offset: Tick,
    pub difficulty_name: Option<String>,
    pub difficulty_level: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TempoChange {
    pub tick: Tick,
    /// Microseconds per quarter note.
    pub micros_per_quarter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSignatureChange {
    pub tick: Tick,
    pub numerator: u8,
    pub denominator: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LaneRole {
    Generic,
    TaikoDon,
    TaikoKat,
    Radial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lane {
    pub id: u16,
    pub name: String,
    pub role: LaneRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LaneOrRegion {
    #[default]
    None,
    Lane(u16),
    Region(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Tap,
    Hold,
    Roll,
    Slide,
    Touch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BranchDecisionHint {
    /// Accuracy thresholds in percent, fixed-point with `ACCURACY_THRESHOLD_SCALE`.
    Accuracy {
        low: i32,
        high: i32,
    },
    Roll {
        low: i32,
        high: i32,
    },
    Score {
        low: u32,
        high: u32,
    },
    Raw(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchSegment {
    pub id: u32,
    pub default_route_id: u8,
    pub route_count: u8,
    pub decision_hint: Option<BranchDecisionHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Object {
    pub id: u32,
    pub kind: ObjectKind,
    pub start_tick: Tick,
    pub end_tick: Tick,
    pub lane_or_region: LaneOrRegion,
    pub flags: u32,
    pub required_hits: u16,
    pub slide_to: Option<u16>,
    #[serde(
        default = "default_scroll_scaled",
        skip_serializing_if = "is_default_scroll_scaled"
    )]
    pub scroll_scaled: i32,
    pub branch_segment_id: Option<u32>,
    pub branch_route_id: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChartEventKind {
    GogoStart,
    GogoEnd,
    BarLine,
    Marker(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartEvent {
    pub tick: Tick,
    pub kind: ChartEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CanonicalChart {
    pub metadata: ChartMetadata,
    pub tempo_map: Vec<TempoChange>,
    pub signatures: Vec<TimeSignatureChange>,
    pub lanes: Vec<Lane>,
    pub branch_segments: Vec<BranchSegment>,
    pub objects: Vec<Object>,
    pub events: Vec<ChartEvent>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ValidationError {
    #[error("tempo map is empty")]
    EmptyTempoMap,
    #[error("tempo change at tick {0} has invalid micros_per_quarter {1}")]
    InvalidTempo(Tick, u32),
    #[error("tempo map is not sorted at tick {0}")]
    UnsortedTempo(Tick),
    #[error("time signature denominator must be non-zero at tick {0}")]
    InvalidTimeSignature(Tick),
    #[error("duplicate lane id {0}")]
    DuplicateLane(u16),
    #[error("branch segments are not strictly sorted by id at id {0}")]
    UnsortedBranchSegment(u32),
    #[error("duplicate branch segment id {0}")]
    DuplicateBranchSegment(u32),
    #[error("branch segment {0} has invalid route_count {1}")]
    InvalidBranchRouteCount(u32, u8),
    #[error("branch segment {0} has invalid default route {1} for route_count {2}")]
    InvalidDefaultBranchRoute(u32, u8, u8),
    #[error("object id {object_id} references unknown branch segment {segment_id}")]
    UnknownBranchSegment { object_id: u32, segment_id: u32 },
    #[error(
        "object id {object_id} has branch route {route_id} out of range for segment {segment_id} (route_count {route_count})"
    )]
    InvalidObjectBranchRoute {
        object_id: u32,
        segment_id: u32,
        route_id: u8,
        route_count: u8,
    },
    #[error("object id {0} has branch_route_id != 0 without branch_segment_id")]
    UnexpectedBranchRoute(u32),
    #[error("object id {0} is duplicated")]
    DuplicateObjectId(u32),
    #[error("object id {0} has negative tick")]
    NegativeTick(u32),
    #[error("object id {0} has end < start")]
    InvalidDuration(u32),
    #[error("objects are not sorted by (start_tick, id): object id {0}")]
    UnsortedObject(u32),
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("invalid encoding")]
    InvalidEncoding,
    #[error("invalid format: {0}")]
    InvalidFormat(String),
    #[error("validation failed: {0}")]
    Validation(#[from] ValidationError),
}

pub trait ChartImporter {
    /// Import raw bytes and return a fully validated canonical chart.
    ///
    /// Importers must return deterministic output for deterministic replay.
    ///
    /// ```no_run
    /// use rhythm_chart::{ChartImporter, CanonicalChart, ImportError};
    ///
    /// struct MyImporter;
    ///
    /// impl ChartImporter for MyImporter {
    ///     fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError> {
    ///         let mut chart: CanonicalChart = serde_json::from_slice(raw)
    ///             .map_err(|e| ImportError::InvalidFormat(e.to_string()))?;
    ///         chart.sort_and_validate()?;
    ///         Ok(chart)
    ///     }
    /// }
    /// ```
    fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct JsonImporter;

impl ChartImporter for JsonImporter {
    fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError> {
        let mut chart: CanonicalChart = serde_json::from_slice(raw)
            .map_err(|e| ImportError::InvalidFormat(format!("json parse error: {e}")))?;
        chart.sort_and_validate()?;
        Ok(chart)
    }
}

impl CanonicalChart {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.tempo_map.is_empty() {
            return Err(ValidationError::EmptyTempoMap);
        }

        let mut last_tempo_tick = Tick::MIN;
        for tempo in &self.tempo_map {
            if tempo.micros_per_quarter == 0 {
                return Err(ValidationError::InvalidTempo(
                    tempo.tick,
                    tempo.micros_per_quarter,
                ));
            }
            if tempo.tick < last_tempo_tick {
                return Err(ValidationError::UnsortedTempo(tempo.tick));
            }
            last_tempo_tick = tempo.tick;
        }

        for signature in &self.signatures {
            if signature.denominator == 0 {
                return Err(ValidationError::InvalidTimeSignature(signature.tick));
            }
        }

        let mut lane_ids = HashSet::new();
        for lane in &self.lanes {
            if !lane_ids.insert(lane.id) {
                return Err(ValidationError::DuplicateLane(lane.id));
            }
        }

        let mut branch_segment_routes = HashMap::new();
        let mut last_segment_id: Option<u32> = None;

        for segment in &self.branch_segments {
            if let Some(last_id) = last_segment_id {
                if segment.id < last_id {
                    return Err(ValidationError::UnsortedBranchSegment(segment.id));
                }
            }
            last_segment_id = Some(segment.id);

            if segment.route_count == 0 {
                return Err(ValidationError::InvalidBranchRouteCount(
                    segment.id,
                    segment.route_count,
                ));
            }

            if segment.default_route_id >= segment.route_count {
                return Err(ValidationError::InvalidDefaultBranchRoute(
                    segment.id,
                    segment.default_route_id,
                    segment.route_count,
                ));
            }

            if branch_segment_routes
                .insert(segment.id, segment.route_count)
                .is_some()
            {
                return Err(ValidationError::DuplicateBranchSegment(segment.id));
            }
        }

        let mut object_ids = HashSet::new();
        let mut last_object_key: Option<(Tick, u32)> = None;
        for object in &self.objects {
            if !object_ids.insert(object.id) {
                return Err(ValidationError::DuplicateObjectId(object.id));
            }

            if object.start_tick < 0 || object.end_tick < 0 {
                return Err(ValidationError::NegativeTick(object.id));
            }

            if object.end_tick < object.start_tick {
                return Err(ValidationError::InvalidDuration(object.id));
            }

            match object.branch_segment_id {
                Some(segment_id) => {
                    let route_count = match branch_segment_routes.get(&segment_id) {
                        Some(route_count) => *route_count,
                        None => {
                            return Err(ValidationError::UnknownBranchSegment {
                                object_id: object.id,
                                segment_id,
                            });
                        }
                    };
                    if object.branch_route_id >= route_count {
                        return Err(ValidationError::InvalidObjectBranchRoute {
                            object_id: object.id,
                            segment_id,
                            route_id: object.branch_route_id,
                            route_count,
                        });
                    }
                }
                None => {
                    if object.branch_route_id != 0 {
                        return Err(ValidationError::UnexpectedBranchRoute(object.id));
                    }
                }
            }

            let current_key = (object.start_tick, object.id);
            if let Some(last_key) = last_object_key {
                if current_key < last_key {
                    return Err(ValidationError::UnsortedObject(object.id));
                }
            }
            last_object_key = Some(current_key);
        }

        Ok(())
    }

    pub fn sort_and_validate(&mut self) -> Result<(), ValidationError> {
        self.tempo_map.sort_by_key(|tempo| tempo.tick);
        self.signatures.sort_by_key(|signature| signature.tick);
        self.events.sort_by_key(|event| event.tick);
        self.branch_segments.sort_by_key(|segment| segment.id);
        self.objects
            .sort_by_key(|object| (object.start_tick, object.id));
        self.validate()
    }
}

#[must_use]
pub fn ticks_from_seconds(seconds: f64) -> Tick {
    (seconds * TICKS_PER_SECOND as f64).round() as Tick
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_chart() -> CanonicalChart {
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
                name: "L0".to_owned(),
                role: LaneRole::Generic,
            }],
            branch_segments: vec![BranchSegment {
                id: 1,
                default_route_id: 0,
                route_count: 3,
                decision_hint: Some(BranchDecisionHint::Accuracy {
                    low: accuracy_threshold_from_percent(70),
                    high: accuracy_threshold_from_percent(85),
                }),
            }],
            objects: vec![Object {
                id: 1,
                kind: ObjectKind::Tap,
                start_tick: 1_000,
                end_tick: 1_000,
                lane_or_region: LaneOrRegion::Lane(0),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: SCROLL_SCALE,
                branch_segment_id: Some(1),
                branch_route_id: 0,
            }],
            events: Vec::new(),
        }
    }

    #[test]
    fn validate_chart() {
        let chart = valid_chart();
        assert!(chart.validate().is_ok());
    }

    #[test]
    fn reject_unsorted_objects() {
        let mut chart = valid_chart();
        chart.objects.push(Object {
            id: 0,
            kind: ObjectKind::Tap,
            start_tick: 10,
            end_tick: 10,
            lane_or_region: LaneOrRegion::Lane(0),
            flags: 0,
            required_hits: 0,
            slide_to: None,
            scroll_scaled: SCROLL_SCALE,
            branch_segment_id: None,
            branch_route_id: 0,
        });

        assert_eq!(chart.validate(), Err(ValidationError::UnsortedObject(0)));
    }

    #[test]
    fn reject_unknown_branch_segment() {
        let mut chart = valid_chart();
        chart.objects[0].branch_segment_id = Some(99);
        assert_eq!(
            chart.validate(),
            Err(ValidationError::UnknownBranchSegment {
                object_id: 1,
                segment_id: 99,
            })
        );
    }

    #[test]
    fn reject_branch_route_without_segment() {
        let mut chart = valid_chart();
        chart.objects[0].branch_segment_id = None;
        chart.objects[0].branch_route_id = 2;
        assert_eq!(
            chart.validate(),
            Err(ValidationError::UnexpectedBranchRoute(1))
        );
    }

    #[test]
    fn reject_branch_route_overflow() {
        let mut chart = valid_chart();
        chart.objects[0].branch_route_id = 3;
        assert_eq!(
            chart.validate(),
            Err(ValidationError::InvalidObjectBranchRoute {
                object_id: 1,
                segment_id: 1,
                route_id: 3,
                route_count: 3,
            })
        );
    }

    #[test]
    fn reject_duplicate_branch_segment_id() {
        let mut chart = valid_chart();
        chart.branch_segments.push(BranchSegment {
            id: 1,
            default_route_id: 0,
            route_count: 3,
            decision_hint: None,
        });

        assert_eq!(
            chart.validate(),
            Err(ValidationError::DuplicateBranchSegment(1))
        );
    }

    #[test]
    fn reject_unsorted_branch_segment_id() {
        let mut chart = valid_chart();
        chart.branch_segments.push(BranchSegment {
            id: 0,
            default_route_id: 0,
            route_count: 1,
            decision_hint: None,
        });

        assert_eq!(
            chart.validate(),
            Err(ValidationError::UnsortedBranchSegment(0))
        );
    }

    #[test]
    fn json_importer_roundtrip() {
        let chart = valid_chart();
        let raw = serde_json::to_vec(&chart).expect("serialize");
        let importer = JsonImporter;
        let parsed = importer.import(&raw).expect("import");
        assert_eq!(parsed, chart);
    }
}
