use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type Tick = i64;
/// Version of the canonical chart data model used for deterministic exchange.
pub const CANONICAL_SCHEMA_VERSION: u32 = 1;
/// Canonical descriptor for the complete v1 chart exchange shape.
///
/// Any accepted field, enum variant, fixed-point scale, default, or ordering
/// rule change must replace this descriptor and its pinned digest together.
pub const CANONICAL_SCHEMA_DESCRIPTOR: &str = concat!(
    "rhythm-chart-canonical/v1\n",
    "encoding=serde-json-compact+struct-declaration-order+externally-tagged-rust-case-enums",
    "+deny-unknown-fields;",
    "integer-tick=i64-microseconds;",
    "scroll-scale=1000000;accuracy-threshold-scale=10000\n",
    "chart-fields=metadata,tempo_map,signatures,lanes,branch_segments,objects,events\n",
    "metadata-fields=title,subtitle,artist,charter,audio_path,offset,difficulty_name,",
    "difficulty_level\n",
    "lane-fields=id,name,role;lane-role=Generic|TaikoDon|TaikoKat|Radial\n",
    "object-fields=id,kind,start_tick,end_tick,lane_or_region,flags,required_hits,slide_to,",
    "scroll_scaled(default=1000000,omit-if-default),branch_segment_id,branch_route_id\n",
    "object-kind=Tap|Hold|Roll|Slide|Touch;",
    "lane-or-region=None|Lane(u16)|Region(u16)\n",
    "tempo-fields=tick,micros_per_quarter;signature-fields=tick,numerator,denominator\n",
    "event-fields=tick,kind;",
    "event-kind=GogoStart|GogoEnd|BarLine{scroll_scaled:i32(default=1000000,omit-if-default)}",
    "|Marker(string)\n",
    "branch-segment-fields=id,default_route_id,route_count,decision_hint;",
    "branch-hint=Accuracy{low:i32,high:i32}|Roll{low:i32,high:i32}|",
    "Score{low:u32,high:u32}|Raw(string)\n",
    "validation=unique+dense-one-based-object-id;sorted-tempo+signature+event+branch-id;",
    "known-lanes+valid-object-ranges+valid-branch-routes;",
    "branch-hint-low<=high+raw-nonempty-no-control-max-bytes:1024\n",
);
/// Stable fingerprint for [`CANONICAL_SCHEMA_DESCRIPTOR`].
pub const CANONICAL_SCHEMA_SHA256: &str =
    "dd32375bd690fdf23d52a231249859b663158890608271173cdc068018bec245";
pub const TICKS_PER_SECOND: Tick = 1_000_000;
pub const SCROLL_SCALE: i32 = 1_000_000;
pub const ACCURACY_THRESHOLD_SCALE: i32 = 10_000;
pub const MAX_BRANCH_HINT_BYTES: usize = 1_024;

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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct TempoChange {
    pub tick: Tick,
    /// Microseconds per quarter note.
    pub micros_per_quarter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct BranchSegment {
    pub id: u32,
    pub default_route_id: u8,
    pub route_count: u8,
    pub decision_hint: Option<BranchDecisionHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDecisionPoint {
    pub segment_id: u32,
    pub decision_tick: Tick,
    pub default_route_id: u8,
    pub route_count: u8,
    pub hint: Option<BranchDecisionHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub enum ChartEventKind {
    GogoStart,
    GogoEnd,
    BarLine {
        #[serde(
            default = "default_scroll_scaled",
            skip_serializing_if = "is_default_scroll_scaled"
        )]
        scroll_scaled: i32,
    },
    Marker(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChartEvent {
    pub tick: Tick,
    pub kind: ChartEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
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
    #[error("time signatures are not sorted at tick {0}")]
    UnsortedTimeSignature(Tick),
    #[error("events are not sorted at tick {0}")]
    UnsortedEvent(Tick),
    #[error("duplicate lane id {0}")]
    DuplicateLane(u16),
    #[error("object id {object_id} references unknown lane {lane_id}")]
    UnknownLane { object_id: u32, lane_id: u16 },
    #[error("branch segments are not strictly sorted by id at id {0}")]
    UnsortedBranchSegment(u32),
    #[error("duplicate branch segment id {0}")]
    DuplicateBranchSegment(u32),
    #[error("branch segment {0} has invalid route_count {1}")]
    InvalidBranchRouteCount(u32, u8),
    #[error("branch segment {0} has invalid default route {1} for route_count {2}")]
    InvalidDefaultBranchRoute(u32, u8, u8),
    #[error("branch segment {0} hint thresholds must satisfy low <= high")]
    InvalidBranchHintThresholds(u32),
    #[error("branch segment {0} raw hint must be nonempty, control-free, and at most 1024 bytes")]
    InvalidRawBranchHint(u32),
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
    #[error("object ids are not dense from 1; missing id {0}")]
    MissingDenseObjectId(u32),
    #[error("object count exceeds the canonical u32 id space")]
    ObjectIdSpaceOverflow,
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

        let mut last_signature_tick = Tick::MIN;
        for signature in &self.signatures {
            if signature.tick < last_signature_tick {
                return Err(ValidationError::UnsortedTimeSignature(signature.tick));
            }
            if signature.numerator == 0 || signature.denominator == 0 {
                return Err(ValidationError::InvalidTimeSignature(signature.tick));
            }
            last_signature_tick = signature.tick;
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

            if let Some(hint) = &segment.decision_hint {
                validate_branch_hint(segment.id, hint)?;
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

            if let LaneOrRegion::Lane(lane_id) = object.lane_or_region {
                if !lane_ids.contains(&lane_id) {
                    return Err(ValidationError::UnknownLane {
                        object_id: object.id,
                        lane_id,
                    });
                }
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

        let object_count =
            u32::try_from(object_ids.len()).map_err(|_| ValidationError::ObjectIdSpaceOverflow)?;
        for expected_id in 1..=object_count {
            if !object_ids.contains(&expected_id) {
                return Err(ValidationError::MissingDenseObjectId(expected_id));
            }
        }

        let mut last_event_tick = Tick::MIN;
        for event in &self.events {
            if event.tick < last_event_tick {
                return Err(ValidationError::UnsortedEvent(event.tick));
            }
            last_event_tick = event.tick;
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

fn validate_branch_hint(segment_id: u32, hint: &BranchDecisionHint) -> Result<(), ValidationError> {
    match hint {
        BranchDecisionHint::Accuracy { low, high } | BranchDecisionHint::Roll { low, high } => {
            if low > high {
                return Err(ValidationError::InvalidBranchHintThresholds(segment_id));
            }
        }
        BranchDecisionHint::Score { low, high } => {
            if low > high {
                return Err(ValidationError::InvalidBranchHintThresholds(segment_id));
            }
        }
        BranchDecisionHint::Raw(raw) => {
            if raw.trim().is_empty()
                || raw.len() > MAX_BRANCH_HINT_BYTES
                || raw.chars().any(char::is_control)
            {
                return Err(ValidationError::InvalidRawBranchHint(segment_id));
            }
        }
    }
    Ok(())
}

#[must_use]
pub fn ticks_from_seconds(seconds: f64) -> Tick {
    (seconds * TICKS_PER_SECOND as f64).round() as Tick
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn canonical_schema_fingerprint_is_pinned_to_the_descriptor() {
        assert_eq!(CANONICAL_SCHEMA_VERSION, 1);
        assert_eq!(
            hex::encode(Sha256::digest(CANONICAL_SCHEMA_DESCRIPTOR.as_bytes())),
            CANONICAL_SCHEMA_SHA256
        );
    }

    #[test]
    fn canonical_json_field_order_and_enum_tags_are_pinned() {
        let chart = CanonicalChart {
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: vec![TimeSignatureChange {
                tick: 0,
                numerator: 4,
                denominator: 4,
            }],
            ..CanonicalChart::default()
        };
        assert_eq!(
            serde_json::to_string(&chart).expect("serialize canonical chart"),
            r#"{"metadata":{"title":"","subtitle":"","artist":"","charter":"","audio_path":null,"offset":0,"difficulty_name":null,"difficulty_level":null},"tempo_map":[{"tick":0,"micros_per_quarter":500000}],"signatures":[{"tick":0,"numerator":4,"denominator":4}],"lanes":[],"branch_segments":[],"objects":[],"events":[]}"#
        );
        assert_eq!(
            serde_json::to_string(&LaneOrRegion::Lane(2)).expect("lane tag"),
            r#"{"Lane":2}"#
        );
        assert_eq!(
            serde_json::to_string(&ChartEventKind::GogoStart).expect("unit event tag"),
            r#""GogoStart""#
        );
        assert_eq!(
            serde_json::to_string(&ChartEventKind::BarLine {
                scroll_scaled: SCROLL_SCALE,
            })
            .expect("default bar-line tag"),
            r#"{"BarLine":{}}"#
        );
        assert_eq!(
            serde_json::to_string(&ChartEventKind::Marker("section".to_owned()))
                .expect("marker tag"),
            r#"{"Marker":"section"}"#
        );
        assert_eq!(
            serde_json::to_string(&BranchDecisionHint::Accuracy { low: 70, high: 85 })
                .expect("branch hint tag"),
            r#"{"Accuracy":{"low":70,"high":85}}"#
        );
    }

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
    fn reject_non_dense_object_ids() {
        let mut chart = valid_chart();
        chart.objects[0].id = 2;
        assert_eq!(
            chart.validate(),
            Err(ValidationError::MissingDenseObjectId(1))
        );
    }

    #[test]
    fn reject_unsorted_signatures_and_events() {
        let mut signatures = valid_chart();
        signatures.signatures.push(TimeSignatureChange {
            tick: -1,
            numerator: 4,
            denominator: 4,
        });
        assert_eq!(
            signatures.validate(),
            Err(ValidationError::UnsortedTimeSignature(-1))
        );

        let mut events = valid_chart();
        events.events = vec![
            ChartEvent {
                tick: 20,
                kind: ChartEventKind::GogoStart,
            },
            ChartEvent {
                tick: 10,
                kind: ChartEventKind::GogoEnd,
            },
        ];
        assert_eq!(events.validate(), Err(ValidationError::UnsortedEvent(10)));
    }

    #[test]
    fn reject_unknown_object_lane() {
        let mut object_lane = valid_chart();
        object_lane.objects[0].lane_or_region = LaneOrRegion::Lane(9);
        assert_eq!(
            object_lane.validate(),
            Err(ValidationError::UnknownLane {
                object_id: 1,
                lane_id: 9,
            })
        );
    }

    #[test]
    fn reject_noncanonical_branch_hints() {
        let mut thresholds = valid_chart();
        thresholds.branch_segments[0].decision_hint =
            Some(BranchDecisionHint::Score { low: 2, high: 1 });
        assert_eq!(
            thresholds.validate(),
            Err(ValidationError::InvalidBranchHintThresholds(1))
        );

        let mut raw = valid_chart();
        raw.branch_segments[0].decision_hint = Some(BranchDecisionHint::Raw("\n".to_owned()));
        assert_eq!(
            raw.validate(),
            Err(ValidationError::InvalidRawBranchHint(1))
        );

        let mut oversized = valid_chart();
        oversized.branch_segments[0].decision_hint = Some(BranchDecisionHint::Raw(
            "x".repeat(MAX_BRANCH_HINT_BYTES + 1),
        ));
        assert_eq!(
            oversized.validate(),
            Err(ValidationError::InvalidRawBranchHint(1))
        );
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

    #[test]
    fn json_importer_rejects_unknown_root_and_nested_fields() {
        let chart = valid_chart();
        let importer = JsonImporter;

        let mut root = serde_json::to_value(&chart).expect("serialize root fixture");
        root["legacy"] = serde_json::json!(true);
        let error = importer
            .import(&serde_json::to_vec(&root).expect("encode root fixture"))
            .expect_err("unknown root field");
        assert!(error.to_string().contains("unknown field `legacy`"));

        let mut nested = serde_json::to_value(&chart).expect("serialize nested fixture");
        nested["tempo_map"][0]["legacy"] = serde_json::json!(true);
        let error = importer
            .import(&serde_json::to_vec(&nested).expect("encode nested fixture"))
            .expect_err("unknown nested field");
        assert!(error.to_string().contains("unknown field `legacy`"));
    }
}
