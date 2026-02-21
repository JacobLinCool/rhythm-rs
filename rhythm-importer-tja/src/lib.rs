use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str;

use encoding_rs::SHIFT_JIS;
use rhythm_chart::{
    ticks_from_seconds, BranchDecisionHint, BranchSegment, CanonicalChart, ChartEvent,
    ChartEventKind, ChartImporter, ChartMetadata, ImportError, Lane, LaneOrRegion, LaneRole,
    Object, ObjectKind, TempoChange, Tick, TimeSignatureChange, ACCURACY_THRESHOLD_SCALE,
    SCROLL_SCALE,
};
use tja::{
    Chart as TjaChart, Course as TjaCourse, Metadata as TjaMetadata, NoteType, ParsingMode,
    Segment, TJAParser,
};

pub const LANE_DON: u16 = 0;
pub const LANE_KAT: u16 = 1;
pub const LANE_BOTH: u16 = 2;

pub const FLAG_BIG: u32 = 1 << 0;
pub const FLAG_BALLOON: u32 = 1 << 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDecisionPoint {
    pub segment_id: u32,
    pub decision_tick: Tick,
    pub route_count: u8,
    pub hint: Option<BranchDecisionHint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedCourse {
    pub chart: CanonicalChart,
    pub branch_decisions: Vec<BranchDecisionPoint>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSong {
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub audio_path: Option<String>,
    pub demo_start_seconds: Option<f64>,
    pub courses: Vec<ImportedCourse>,
}

#[derive(Debug, Default, Clone)]
pub struct TjaImporter;

impl TjaImporter {
    /// Import one `.tja` blob into song metadata + all canonical courses.
    ///
    /// This keeps `CanonicalChart` unchanged while providing UI-friendly metadata
    /// and branch decision ticks for adapter/controller layers.
    pub fn import_song(&self, raw: &[u8]) -> Result<ImportedSong, ImportError> {
        let text = decode_text(raw)?;

        let mut parser = TJAParser::with_mode(ParsingMode::FullWithBlanks);
        parser
            .parse_str(&text)
            .map_err(ImportError::InvalidFormat)?;

        let parsed = parser.get_parsed_tja();
        let metadata = parsed.metadata.clone();

        let mut indexed_charts = parsed.charts.into_iter().enumerate().collect::<Vec<_>>();
        indexed_charts.sort_by_key(|(idx, chart)| {
            (
                chart.course().map_or(u8::MAX, course_rank),
                chart.level().unwrap_or(i32::MAX),
                *idx,
            )
        });

        let mut courses = Vec::with_capacity(indexed_charts.len());
        for (_, chart) in indexed_charts {
            let canonical = build_chart(&metadata, &chart)?;
            let branch_decisions = build_branch_decision_table(&canonical);
            courses.push(ImportedCourse {
                chart: canonical,
                branch_decisions,
            });
        }

        Ok(ImportedSong {
            title: metadata.get("TITLE").cloned().unwrap_or_default(),
            subtitle: metadata.get("SUBTITLE").cloned().unwrap_or_default(),
            artist: metadata.get("ARTIST").cloned().unwrap_or_default(),
            audio_path: metadata
                .get("WAVE")
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            demo_start_seconds: parse_optional_f64(&metadata, "DEMOSTART")?,
            courses,
        })
    }

    /// Import all courses in a `.tja` blob and convert them into canonical charts.
    ///
    /// The adapter is strict: malformed branch/roll structures return `ImportError::InvalidFormat`.
    ///
    /// ```no_run
    /// use rhythm_importer_tja::TjaImporter;
    ///
    /// let raw = std::fs::read("song.tja")?;
    /// let importer = TjaImporter;
    /// let charts = importer.import_all(&raw)?;
    /// println!("courses={}", charts.len());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn import_all(&self, raw: &[u8]) -> Result<Vec<CanonicalChart>, ImportError> {
        let song = self.import_song(raw)?;
        Ok(song
            .courses
            .into_iter()
            .map(|course| course.chart)
            .collect())
    }
}

impl ChartImporter for TjaImporter {
    fn import(&self, raw: &[u8]) -> Result<CanonicalChart, ImportError> {
        self.import_all(raw)?
            .into_iter()
            .next()
            .ok_or_else(|| ImportError::InvalidFormat("no chart found".to_owned()))
    }
}

pub fn build_branch_decision_table(chart: &CanonicalChart) -> Vec<BranchDecisionPoint> {
    let mut earliest_tick = HashMap::<u32, Tick>::new();
    for object in &chart.objects {
        if let Some(segment_id) = object.branch_segment_id {
            earliest_tick
                .entry(segment_id)
                .and_modify(|tick| {
                    if object.start_tick < *tick {
                        *tick = object.start_tick;
                    }
                })
                .or_insert(object.start_tick);
        }
    }

    let mut points = chart
        .branch_segments
        .iter()
        .filter_map(|segment| {
            earliest_tick.get(&segment.id).map(|tick| {
                let measure_ticks = measure_duration_ticks_at(chart, *tick);
                BranchDecisionPoint {
                    segment_id: segment.id,
                    decision_tick: tick.saturating_sub(measure_ticks),
                    route_count: segment.route_count,
                    hint: segment.decision_hint.clone(),
                }
            })
        })
        .collect::<Vec<_>>();

    points.sort_by_key(|point| (point.decision_tick, point.segment_id));
    points
}

fn measure_duration_ticks_at(chart: &CanonicalChart, tick: Tick) -> Tick {
    let micros_per_quarter = tempo_micros_per_quarter_at(&chart.tempo_map, tick);
    let (numerator, denominator) = signature_at(&chart.signatures, tick);

    let measure =
        (i128::from(micros_per_quarter) * i128::from(numerator) * 4) / i128::from(denominator);
    measure.clamp(1, i128::from(i64::MAX)) as Tick
}

fn tempo_micros_per_quarter_at(tempo_map: &[TempoChange], tick: Tick) -> u32 {
    if tempo_map.is_empty() {
        return 500_000;
    }
    let idx = tempo_map.partition_point(|tempo| tempo.tick <= tick);
    if idx == 0 {
        tempo_map[0].micros_per_quarter
    } else {
        tempo_map[idx - 1].micros_per_quarter
    }
}

fn signature_at(signatures: &[TimeSignatureChange], tick: Tick) -> (u8, u8) {
    if signatures.is_empty() {
        return (4, 4);
    }
    let idx = signatures.partition_point(|sig| sig.tick <= tick);
    if idx == 0 {
        (signatures[0].numerator, signatures[0].denominator)
    } else {
        let sig = signatures[idx - 1];
        (sig.numerator, sig.denominator)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SegmentBranchMeta {
    segment_id: Option<u32>,
    route_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StreamKey {
    segment_id: Option<u32>,
    route_id: u8,
}

#[derive(Debug, Clone, Copy)]
struct PendingRoll {
    start_tick: Tick,
    flags: u32,
    required_hits: u16,
    scroll_scaled: i32,
}

fn decode_text(raw: &[u8]) -> Result<String, ImportError> {
    if let Ok(s) = str::from_utf8(raw) {
        return Ok(s.to_owned());
    }

    let (cow, _, had_errors) = SHIFT_JIS.decode(raw);
    if had_errors {
        return Err(ImportError::InvalidEncoding);
    }

    Ok(cow.into_owned())
}

fn parse_optional_f64(metadata: &TjaMetadata, key: &str) -> Result<Option<f64>, ImportError> {
    let Some(raw) = metadata.get(key) else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed = trimmed.parse::<f64>().map_err(|_| {
        ImportError::InvalidFormat(format!("invalid numeric metadata for {key}: {trimmed}"))
    })?;
    Ok(Some(parsed))
}

fn build_chart(metadata: &TjaMetadata, chart: &TjaChart) -> Result<CanonicalChart, ImportError> {
    let (branch_segments, segment_branch_meta) = derive_branch_data(&chart.segments)?;

    let seed_bpm = metadata.bpm.max(1.0);
    let seed_micros = bpm_to_micros_per_quarter(seed_bpm);

    let mut tempo_by_tick = BTreeMap::<Tick, u32>::new();
    tempo_by_tick.insert(0, seed_micros);

    let mut signature_by_tick = BTreeMap::<Tick, (u8, u8)>::new();
    signature_by_tick.insert(0, (4, 4));

    let mut barline_ticks = BTreeSet::<Tick>::new();
    let mut gogo_by_tick = BTreeMap::<Tick, bool>::new();
    let mut gogo_by_stream_tick = HashMap::<(StreamKey, Tick), bool>::new();

    let mut objects = Vec::<Object>::new();
    let mut next_object_id: u32 = 1;
    let mut balloon_cursor = 0usize;
    let mut open_rolls = HashMap::<StreamKey, PendingRoll>::new();

    for (seg_idx, segment) in chart.segments.iter().enumerate() {
        let seg_tick = ticks_from_seconds(segment.timestamp);

        if segment.barline {
            barline_ticks.insert(seg_tick);
        }

        let numerator = u8::try_from(segment.measure_num).map_err(|_| {
            ImportError::InvalidFormat(format!(
                "invalid time signature numerator {} at {}",
                segment.measure_num, segment.timestamp
            ))
        })?;

        let denominator = u8::try_from(segment.measure_den).map_err(|_| {
            ImportError::InvalidFormat(format!(
                "invalid time signature denominator {} at {}",
                segment.measure_den, segment.timestamp
            ))
        })?;

        if denominator == 0 {
            return Err(ImportError::InvalidFormat(format!(
                "time signature denominator must be non-zero at {}",
                segment.timestamp
            )));
        }

        insert_unique_signature(&mut signature_by_tick, seg_tick, numerator, denominator)?;

        let branch_meta = segment_branch_meta[seg_idx];
        let stream_key = StreamKey {
            segment_id: branch_meta.segment_id,
            route_id: branch_meta.route_id,
        };

        for note in &segment.notes {
            let tick = ticks_from_seconds(note.timestamp);
            // TJA allows in-measure BPM edits, and timestamp rounding may collapse adjacent
            // notes with different BPM onto the same tick. Keep the latest BPM at that tick.
            if stream_key.route_id == 0 {
                tempo_by_tick.insert(tick, bpm_to_micros_per_quarter(note.bpm));
            }
            insert_unique_gogo_per_stream(&mut gogo_by_stream_tick, stream_key, tick, note.gogo)?;
            // Canonical chart events are global; derive them from the default route stream.
            if stream_key.route_id == 0 {
                insert_unique_gogo(&mut gogo_by_tick, tick, note.gogo)?;
            }

            match note.note_type {
                NoteType::Empty => {}
                NoteType::Don | NoteType::Ka | NoteType::DonBig | NoteType::KaBig => {
                    let (lane, flags) = match note.note_type {
                        NoteType::Don => (LANE_DON, 0),
                        NoteType::Ka => (LANE_KAT, 0),
                        NoteType::DonBig => (LANE_DON, FLAG_BIG),
                        NoteType::KaBig => (LANE_KAT, FLAG_BIG),
                        _ => unreachable!(),
                    };

                    objects.push(Object {
                        id: next_object_id,
                        kind: ObjectKind::Tap,
                        start_tick: tick,
                        end_tick: tick,
                        lane_or_region: LaneOrRegion::Lane(lane),
                        flags,
                        required_hits: 0,
                        slide_to: None,
                        scroll_scaled: scroll_to_scaled(note.scroll),
                        branch_segment_id: branch_meta.segment_id,
                        branch_route_id: branch_meta.route_id,
                    });
                    next_object_id = next_object_id.saturating_add(1);
                }
                NoteType::Roll | NoteType::RollBig | NoteType::Balloon | NoteType::BalloonAlt => {
                    if open_rolls.contains_key(&stream_key) {
                        // Some charts emit another roll-start token before the current roll ends.
                        // Treat nested starts as empty (same as digit 0) and keep the first open roll.
                        continue;
                    }

                    let (flags, required_hits) = match note.note_type {
                        NoteType::Balloon | NoteType::BalloonAlt => {
                            (FLAG_BALLOON, next_balloon_hits(chart, &mut balloon_cursor))
                        }
                        NoteType::Roll | NoteType::RollBig => (0, 0),
                        _ => unreachable!(),
                    };

                    open_rolls.insert(
                        stream_key,
                        PendingRoll {
                            start_tick: tick,
                            flags,
                            required_hits,
                            scroll_scaled: scroll_to_scaled(note.scroll),
                        },
                    );
                }
                NoteType::EndOf => {
                    let pending = open_rolls.remove(&stream_key).ok_or_else(|| {
                        ImportError::InvalidFormat(format!(
                            "unmatched roll end in stream {:?} at tick {tick}",
                            stream_key
                        ))
                    })?;

                    objects.push(Object {
                        id: next_object_id,
                        kind: ObjectKind::Roll,
                        start_tick: pending.start_tick,
                        end_tick: tick.max(pending.start_tick),
                        lane_or_region: LaneOrRegion::Lane(LANE_BOTH),
                        flags: pending.flags,
                        required_hits: pending.required_hits,
                        slide_to: None,
                        scroll_scaled: pending.scroll_scaled,
                        branch_segment_id: branch_meta.segment_id,
                        branch_route_id: branch_meta.route_id,
                    });
                    next_object_id = next_object_id.saturating_add(1);
                }
            }
        }
    }

    if !open_rolls.is_empty() {
        return Err(ImportError::InvalidFormat(
            "unclosed roll note at end of chart".to_owned(),
        ));
    }

    let tempo_map = build_tempo_map(&tempo_by_tick);
    let signatures = build_signature_map(&signature_by_tick);

    let mut events = Vec::new();
    for tick in barline_ticks {
        events.push(ChartEvent {
            tick,
            kind: ChartEventKind::BarLine,
        });
    }

    let mut gogo_state = false;
    for (tick, is_gogo) in gogo_by_tick {
        if is_gogo != gogo_state {
            events.push(ChartEvent {
                tick,
                kind: if is_gogo {
                    ChartEventKind::GogoStart
                } else {
                    ChartEventKind::GogoEnd
                },
            });
            gogo_state = is_gogo;
        }
    }

    let mut canonical = CanonicalChart {
        metadata: ChartMetadata {
            title: metadata.get("TITLE").cloned().unwrap_or_default(),
            subtitle: metadata.get("SUBTITLE").cloned().unwrap_or_default(),
            artist: metadata.get("ARTIST").cloned().unwrap_or_default(),
            charter: metadata.get("MAKER").cloned().unwrap_or_default(),
            audio_path: metadata.get("WAVE").cloned(),
            offset: ticks_from_seconds(metadata.offset),
            difficulty_name: chart
                .headers
                .get("COURSE")
                .cloned()
                .or_else(|| chart.course().map(course_name)),
            difficulty_level: chart.level().and_then(|v| u8::try_from(v).ok()),
        },
        tempo_map,
        signatures,
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
            Lane {
                id: LANE_BOTH,
                name: "both".to_owned(),
                role: LaneRole::Generic,
            },
        ],
        branch_segments,
        objects,
        events,
    };

    canonical.sort_and_validate()?;
    compact_chart_storage(&mut canonical);
    Ok(canonical)
}

fn derive_branch_data(
    segments: &[Segment],
) -> Result<(Vec<BranchSegment>, Vec<SegmentBranchMeta>), ImportError> {
    let mut branch_segments = Vec::new();
    let mut segment_meta = vec![SegmentBranchMeta::default(); segments.len()];

    let mut next_segment_id: u32 = 1;
    let mut idx = 0usize;

    while idx < segments.len() {
        let Some((condition, _)) = read_branch_fields(&segments[idx])? else {
            idx += 1;
            continue;
        };

        let mut run_indices = Vec::<usize>::new();
        while idx < segments.len() {
            match read_branch_fields(&segments[idx])? {
                Some((other_condition, _)) if other_condition == condition => {
                    run_indices.push(idx);
                    idx += 1;
                }
                _ => break,
            }
        }

        let blocks = split_route_cycles(segments, &run_indices)?;
        for block in blocks {
            let mut route_seen = [false; 3];
            let mut route_end = [Tick::MIN; 3];

            for &seg_idx in &block {
                let route = parse_route_id(
                    segments[seg_idx]
                        .branch
                        .as_deref()
                        .ok_or_else(|| {
                            ImportError::InvalidFormat("missing branch route token".to_owned())
                        })?
                        .trim(),
                )?;
                route_seen[usize::from(route)] = true;
                route_end[usize::from(route)] =
                    route_end[usize::from(route)].max(segment_end_tick(&segments[seg_idx]));
            }

            if !route_seen.iter().all(|v| *v) {
                return Err(ImportError::InvalidFormat(
                    "branch section must contain N/E/M routes".to_owned(),
                ));
            }

            if route_end[0] != route_end[1] || route_end[1] != route_end[2] {
                return Err(ImportError::InvalidFormat(
                    "branch routes have different durations".to_owned(),
                ));
            }

            let branch_id = next_segment_id;
            next_segment_id = next_segment_id
                .checked_add(1)
                .ok_or_else(|| ImportError::InvalidFormat("too many branch segments".to_owned()))?;

            branch_segments.push(BranchSegment {
                id: branch_id,
                default_route_id: 0,
                route_count: 3,
                decision_hint: parse_branch_decision_hint(&condition)?,
            });

            for &seg_idx in &block {
                let route = parse_route_id(
                    segments[seg_idx]
                        .branch
                        .as_deref()
                        .ok_or_else(|| {
                            ImportError::InvalidFormat("missing branch route token".to_owned())
                        })?
                        .trim(),
                )?;
                segment_meta[seg_idx] = SegmentBranchMeta {
                    segment_id: Some(branch_id),
                    route_id: route,
                };
            }
        }
    }

    Ok((branch_segments, segment_meta))
}

fn split_route_cycles(
    segments: &[Segment],
    run_indices: &[usize],
) -> Result<Vec<Vec<usize>>, ImportError> {
    let mut blocks = Vec::<Vec<usize>>::new();
    let mut current = Vec::<usize>::new();
    let mut seen = [false; 3];
    let mut last_route: Option<u8> = None;

    for &seg_idx in run_indices {
        let route = parse_route_id(
            segments[seg_idx]
                .branch
                .as_deref()
                .ok_or_else(|| ImportError::InvalidFormat("missing branch route token".to_owned()))?
                .trim(),
        )?;

        if current.is_empty() {
            if route != 0 {
                return Err(ImportError::InvalidFormat(
                    "branch section must start at #N route".to_owned(),
                ));
            }
        } else if let Some(last) = last_route {
            if route < last {
                if last == 2 && route == 0 && seen.iter().all(|s| *s) {
                    blocks.push(current);
                    current = Vec::new();
                    seen = [false; 3];

                    if route != 0 {
                        return Err(ImportError::InvalidFormat(
                            "branch section must restart at #N route".to_owned(),
                        ));
                    }
                } else {
                    return Err(ImportError::InvalidFormat(
                        "invalid branch route order".to_owned(),
                    ));
                }
            } else if route > last + 1 {
                return Err(ImportError::InvalidFormat(
                    "branch route order must be N -> E -> M".to_owned(),
                ));
            }
        }

        seen[usize::from(route)] = true;
        current.push(seg_idx);
        last_route = Some(route);
    }

    if !current.is_empty() {
        blocks.push(current);
    }

    Ok(blocks)
}

fn read_branch_fields(segment: &Segment) -> Result<Option<(String, u8)>, ImportError> {
    match (
        segment.branch_condition.as_deref(),
        segment.branch.as_deref().map(str::trim),
    ) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(ImportError::InvalidFormat(
            "inconsistent branch metadata in parsed segment".to_owned(),
        )),
        (Some(condition), Some(route)) => {
            let route_id = parse_route_id(route)?;
            Ok(Some((condition.trim().to_owned(), route_id)))
        }
    }
}

fn parse_route_id(route: &str) -> Result<u8, ImportError> {
    match route.trim().to_ascii_uppercase().as_str() {
        "N" => Ok(0),
        "E" => Ok(1),
        "M" => Ok(2),
        other => Err(ImportError::InvalidFormat(format!(
            "unknown branch route token: {other}"
        ))),
    }
}

fn parse_branch_decision_hint(raw: &str) -> Result<Option<BranchDecisionHint>, ImportError> {
    let parts = raw.split(',').map(str::trim).collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(ImportError::InvalidFormat(format!(
            "invalid BRANCHSTART args: {raw}"
        )));
    }

    let kind = parts[0].to_ascii_lowercase();
    let low_raw = parts[1];
    let high_raw = parts[2];

    let hint = match kind.as_str() {
        "p" | "accuracy" => {
            let low = parse_accuracy_threshold(low_raw, "low")?;
            let high = parse_accuracy_threshold(high_raw, "high")?;
            if low > high {
                return Err(ImportError::InvalidFormat(
                    "accuracy thresholds must satisfy low <= high".to_owned(),
                ));
            }
            Some(BranchDecisionHint::Accuracy { low, high })
        }
        "r" | "roll" => {
            let low = low_raw.parse::<i32>().map_err(|_| {
                ImportError::InvalidFormat(format!("invalid roll low threshold: {low_raw}"))
            })?;
            let high = high_raw.parse::<i32>().map_err(|_| {
                ImportError::InvalidFormat(format!("invalid roll high threshold: {high_raw}"))
            })?;
            if low > high {
                return Err(ImportError::InvalidFormat(
                    "roll thresholds must satisfy low <= high".to_owned(),
                ));
            }
            Some(BranchDecisionHint::Roll { low, high })
        }
        "s" | "score" => {
            let low = low_raw.parse::<u32>().map_err(|_| {
                ImportError::InvalidFormat(format!("invalid score low threshold: {low_raw}"))
            })?;
            let high = high_raw.parse::<u32>().map_err(|_| {
                ImportError::InvalidFormat(format!("invalid score high threshold: {high_raw}"))
            })?;
            if low > high {
                return Err(ImportError::InvalidFormat(
                    "score thresholds must satisfy low <= high".to_owned(),
                ));
            }
            Some(BranchDecisionHint::Score { low, high })
        }
        _ => Some(BranchDecisionHint::Raw(raw.to_owned())),
    };

    Ok(hint)
}

fn next_balloon_hits(chart: &TjaChart, cursor: &mut usize) -> u16 {
    let Some(value) = chart.balloons.get(*cursor).copied() else {
        return 5;
    };
    *cursor += 1;

    if value <= 0 || value > i32::from(u16::MAX) {
        5
    } else {
        value as u16
    }
}

fn segment_end_tick(segment: &Segment) -> Tick {
    let mut end_sec = segment.timestamp;
    for note in &segment.notes {
        if note.timestamp > end_sec {
            end_sec = note.timestamp;
        }
    }
    ticks_from_seconds(end_sec)
}

fn build_tempo_map(tempo_by_tick: &BTreeMap<Tick, u32>) -> Vec<TempoChange> {
    let mut dedup_len = 0_usize;
    let mut last = None::<u32>;
    for micros_per_quarter in tempo_by_tick.values() {
        if last != Some(*micros_per_quarter) {
            dedup_len += 1;
            last = Some(*micros_per_quarter);
        }
    }

    let mut map = Vec::with_capacity(dedup_len);
    let mut last = None::<u32>;

    for (tick, micros_per_quarter) in tempo_by_tick {
        if last != Some(*micros_per_quarter) {
            map.push(TempoChange {
                tick: *tick,
                micros_per_quarter: *micros_per_quarter,
            });
            last = Some(*micros_per_quarter);
        }
    }

    map
}

fn build_signature_map(signature_by_tick: &BTreeMap<Tick, (u8, u8)>) -> Vec<TimeSignatureChange> {
    let mut dedup_len = 0_usize;
    let mut last = None::<(u8, u8)>;
    for signature in signature_by_tick.values() {
        if last != Some(*signature) {
            dedup_len += 1;
            last = Some(*signature);
        }
    }

    let mut map = Vec::with_capacity(dedup_len);
    let mut last = None::<(u8, u8)>;

    for (tick, (numerator, denominator)) in signature_by_tick {
        if last != Some((*numerator, *denominator)) {
            map.push(TimeSignatureChange {
                tick: *tick,
                numerator: *numerator,
                denominator: *denominator,
            });
            last = Some((*numerator, *denominator));
        }
    }

    map
}

fn compact_chart_storage(chart: &mut CanonicalChart) {
    chart.tempo_map.shrink_to_fit();
    chart.signatures.shrink_to_fit();
    chart.lanes.shrink_to_fit();
    chart.branch_segments.shrink_to_fit();
    chart.objects.shrink_to_fit();
    chart.events.shrink_to_fit();
}

fn insert_unique_signature(
    signature_by_tick: &mut BTreeMap<Tick, (u8, u8)>,
    tick: Tick,
    numerator: u8,
    denominator: u8,
) -> Result<(), ImportError> {
    if let Some(existing) = signature_by_tick.get(&tick) {
        // The canonical adapter seeds 4/4 at tick 0. If the chart explicitly sets
        // #MEASURE at tick 0, prefer the explicit chart value over the seed.
        if tick == 0 && *existing == (4, 4) {
            signature_by_tick.insert(tick, (numerator, denominator));
            return Ok(());
        }
        if *existing != (numerator, denominator) {
            return Err(ImportError::InvalidFormat(format!(
                "conflicting time signature at tick {tick}: {}/{} vs {}/{}",
                existing.0, existing.1, numerator, denominator
            )));
        }
    }

    signature_by_tick.insert(tick, (numerator, denominator));
    Ok(())
}

fn insert_unique_gogo(
    gogo_by_tick: &mut BTreeMap<Tick, bool>,
    tick: Tick,
    gogo: bool,
) -> Result<(), ImportError> {
    if let Some(existing) = gogo_by_tick.get(&tick) {
        if *existing != gogo {
            return Err(ImportError::InvalidFormat(format!(
                "conflicting gogo state at tick {tick}"
            )));
        }
    }

    gogo_by_tick.insert(tick, gogo);
    Ok(())
}

fn insert_unique_gogo_per_stream(
    gogo_by_stream_tick: &mut HashMap<(StreamKey, Tick), bool>,
    stream_key: StreamKey,
    tick: Tick,
    gogo: bool,
) -> Result<(), ImportError> {
    if let Some(existing) = gogo_by_stream_tick.get(&(stream_key, tick)) {
        if *existing != gogo {
            return Err(ImportError::InvalidFormat(format!(
                "conflicting gogo state in stream {:?} at tick {tick}",
                stream_key
            )));
        }
    }
    gogo_by_stream_tick.insert((stream_key, tick), gogo);
    Ok(())
}

fn bpm_to_micros_per_quarter(bpm: f64) -> u32 {
    ((60_000_000.0 / bpm.max(1.0)).round() as u64).clamp(1, u32::MAX as u64) as u32
}

fn parse_accuracy_threshold(raw: &str, bound: &str) -> Result<i32, ImportError> {
    const ACCURACY_DIGITS: usize = 4;
    let invalid =
        || ImportError::InvalidFormat(format!("invalid accuracy {bound} threshold: {raw}"));

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid());
    }

    let (sign, value_raw) = if let Some(rest) = trimmed.strip_prefix('-') {
        (-1_i64, rest)
    } else if let Some(rest) = trimmed.strip_prefix('+') {
        (1_i64, rest)
    } else {
        (1_i64, trimmed)
    };
    if value_raw.is_empty() {
        return Err(invalid());
    }

    let (whole_raw, fraction_raw) = match value_raw.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value_raw, None),
    };

    if whole_raw.is_empty() || !whole_raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }

    let whole = whole_raw.parse::<i64>().map_err(|_| invalid())?;
    let scale = i64::from(ACCURACY_THRESHOLD_SCALE);
    let mut scaled_abs = whole.checked_mul(scale).ok_or_else(invalid)?;

    let Some(fraction_raw) = fraction_raw else {
        let signed = scaled_abs.checked_mul(sign).ok_or_else(invalid)?;
        return i32::try_from(signed).map_err(|_| invalid());
    };
    if fraction_raw.is_empty() || !fraction_raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }

    let fraction_bytes = fraction_raw.as_bytes();
    let mut fraction = 0_i64;
    let keep_digits = fraction_bytes.len().min(ACCURACY_DIGITS);
    for &digit in &fraction_bytes[..keep_digits] {
        fraction = fraction
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(digit - b'0')))
            .ok_or_else(invalid)?;
    }
    for _ in keep_digits..ACCURACY_DIGITS {
        fraction = fraction.checked_mul(10).ok_or_else(invalid)?;
    }

    if fraction_bytes.len() > ACCURACY_DIGITS && fraction_bytes[ACCURACY_DIGITS] >= b'5' {
        fraction = fraction.checked_add(1).ok_or_else(invalid)?;
    }
    if fraction >= scale {
        scaled_abs = scaled_abs.checked_add(scale).ok_or_else(invalid)?;
        fraction -= scale;
    }

    scaled_abs = scaled_abs.checked_add(fraction).ok_or_else(invalid)?;
    let signed = scaled_abs.checked_mul(sign).ok_or_else(invalid)?;
    i32::try_from(signed).map_err(|_| invalid())
}

fn scroll_to_scaled(scroll: f64) -> i32 {
    if !scroll.is_finite() {
        return SCROLL_SCALE;
    }
    let scaled = (scroll * f64::from(SCROLL_SCALE)).round();
    scaled.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

fn course_name(course: &TjaCourse) -> String {
    match course {
        TjaCourse::Easy => "Easy".to_owned(),
        TjaCourse::Normal => "Normal".to_owned(),
        TjaCourse::Hard => "Hard".to_owned(),
        TjaCourse::Oni => "Oni".to_owned(),
        TjaCourse::Ura => "Ura".to_owned(),
    }
}

fn course_rank(course: &TjaCourse) -> u8 {
    match course {
        TjaCourse::Easy => 0,
        TjaCourse::Normal => 1,
        TjaCourse::Hard => 2,
        TjaCourse::Oni => 3,
        TjaCourse::Ura => 4,
    }
}
