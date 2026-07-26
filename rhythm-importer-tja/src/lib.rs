use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::str;

use encoding_rs::SHIFT_JIS;
pub use rhythm_chart::BranchDecisionPoint;
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

/// Version of the TJA-to-canonical import contract, including default budgets.
pub const TJA_IMPORTER_SEMANTICS_VERSION: u32 = 3;
/// Required hits assigned to every balloon note when a course has no hit-count
/// list (either a missing header or an empty `BALLOON:` header).
///
/// This preserves the only historical default used by this importer while making
/// it an explicit, fingerprinted application rule. It is not claimed to be an
/// official TJA format default.
pub const DEFAULT_BALLOON_HITS: u16 = 5;
/// Complete descriptor for the TJA-to-canonical import contract.
pub const TJA_IMPORTER_SEMANTICS_DESCRIPTOR: &str = concat!(
    "rhythm-importer-tja/v3\n",
    "parser=tja@0.5.0;mode=full-with-blanks;encoding=utf8-then-shift-jis\n",
    "source=complete-uppercase-START-END-courses+known-key-value-lines+known-directives+",
    "note-digits-commas-ascii-whitespace;parser-reconciliation=exact-course-segment-note-",
    "balloon-counts;unsupported=reject;SECTION=reject;post-BRANCHEND-only-END\n",
    "metadata-keys=TITLE,SUBTITLE,WAVE,BPM,OFFSET,DEMOSTART,GENRE,MAKER,SONGVOL,SEVOL,",
    "SCOREMODE,localized-title-subtitle;header-keys=COURSE,LEVEL,BALLOON,SCOREINIT,",
    "SCOREDIFF,STYLE;required=BPM+course-header-before-first-START;",
    "header-values=known-COURSE+LEVEL-1..10+SCOREINIT-optional-empty-one-or-two-u32+",
    "SCOREDIFF-optional-empty-u32;",
    "defaults=OFFSET:0+DEMOSTART:none+text:empty+WAVE:none+SCROLL:1+MEASURE:4/4+GOGO:false;",
    "legacy-score-metadata=SCOREINIT+SCOREDIFF+SCOREMODE-optional-empty-and-ignored;",
    "course-order=known-course-rank+level-or-max+source-index\n",
    "quantization=tick:round-seconds-times-1000000+bpm:round-60000000-div-bpm+",
    "scroll:round-multiplier-times-1000000;numeric=bpm-finite-positive-u32-micros+",
    "scroll-finite-i32-scaled+timestamp-finite-i64-microseconds+",
    "demostart-finite-nonnegative\n",
    "mapping=0:empty,1:don,2:kat,3:big-don,4:big-kat,5:roll,6:big-roll,",
    "7:balloon,8:roll-end,9:balloon-alt;lanes=don:0+kat:1+both:2;",
    "object-ids=dense-source-iteration;canonical-sort=start-tick-then-id\n",
    "tempo=seed-global-BPM-at-zero+default-route-notes+same-tick-latest-source+",
    "collapse-adjacent-equal;signature=seed-4/4-at-zero+explicit-zero-overrides-seed+",
    "same-tick-conflict-reject+collapse-adjacent-equal;",
    "events=default-route-only+barline-same-tick-conflict-reject+",
    "gogo-same-stream-and-global-conflict-reject+state-transitions-only;",
    "scroll-state=per-branch-stream+empty-segment-inherits+note-latest\n",
    "branch=contiguous-condition-runs+strict-N-E-M-cycles+equal-route-duration+",
    "three-routes+default-N+at-least-one-playable-object;",
    "branch-decision=one-active-measure-before-earliest-route-object+clamp-zero;",
    "branch-hints=accuracy-0..100-percent+roll-nonnegative+score-u32+known-kind\n",
    "roll-pairing=strict-no-nesting+matched-end+end>=start+big-flag-preserved;",
    "balloon-hits=explicit-list-exactly-one-positive-u16-entry-per-balloon+",
    "missing-or-explicit-empty-header-app-default-5-per-balloon;",
    "canonical-schema-version=1;canonical-validation=required\n",
    "limits=raw:16777216,line:1048576,key-value:65536,branch-condition:256,",
    "courses:16,balloon-values/course:65536,note-symbols/course:262144,",
    "segments/course:65536,objects/course:131072,events/course:65536,",
    "tempo/course:16384,signatures/course:16384,branch-segments/course:4096,",
    "branch-decisions/course:4096,total-note-symbols:524288,total-segments:131072,",
    "total-objects:262144,total-events:131072,total-tempo:32768,",
    "total-signatures:32768,total-branch-segments:8192,total-branch-decisions:8192\n",
);
/// SHA-256 of [`TJA_IMPORTER_SEMANTICS_DESCRIPTOR`].
pub const TJA_IMPORTER_SEMANTICS_SHA256: &str =
    "2d2624803b616e3d8644db66ed06778e29f197926603bdb639b6937a18951fb6";

pub const FLAG_BIG: u32 = 1 << 0;
pub const FLAG_BALLOON: u32 = 1 << 1;

/// Hard resource budgets for one TJA import.
///
/// Parser-facing limits are checked directly from the decoded source before the
/// upstream `tja` parser allocates a `Note` for every note symbol. Canonical
/// limits are checked again at the exact output insertion point. Per-course and
/// aggregate limits are both mandatory so increasing `max_courses` cannot
/// accidentally multiply the importer's memory ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TjaImportLimits {
    pub max_raw_bytes: usize,
    pub max_line_bytes: usize,
    pub max_key_value_bytes: usize,
    pub max_branch_condition_bytes: usize,
    pub max_courses: usize,
    pub max_balloon_values_per_course: usize,
    pub max_note_symbols_per_course: usize,
    pub max_segments_per_course: usize,
    pub max_objects_per_course: usize,
    pub max_events_per_course: usize,
    pub max_tempo_changes_per_course: usize,
    pub max_time_signatures_per_course: usize,
    pub max_branch_segments_per_course: usize,
    pub max_branch_decisions_per_course: usize,
    pub max_total_note_symbols: usize,
    pub max_total_segments: usize,
    pub max_total_objects: usize,
    pub max_total_events: usize,
    pub max_total_tempo_changes: usize,
    pub max_total_time_signatures: usize,
    pub max_total_branch_segments: usize,
    pub max_total_branch_decisions: usize,
}

impl TjaImportLimits {
    pub const DEFAULT: Self = Self {
        max_raw_bytes: 16 * 1024 * 1024,
        max_line_bytes: 1024 * 1024,
        max_key_value_bytes: 64 * 1024,
        max_branch_condition_bytes: 256,
        max_courses: 16,
        max_balloon_values_per_course: 65_536,
        max_note_symbols_per_course: 262_144,
        max_segments_per_course: 65_536,
        max_objects_per_course: 131_072,
        max_events_per_course: 65_536,
        max_tempo_changes_per_course: 16_384,
        max_time_signatures_per_course: 16_384,
        max_branch_segments_per_course: 4_096,
        max_branch_decisions_per_course: 4_096,
        max_total_note_symbols: 524_288,
        max_total_segments: 131_072,
        max_total_objects: 262_144,
        max_total_events: 131_072,
        max_total_tempo_changes: 32_768,
        max_total_time_signatures: 32_768,
        max_total_branch_segments: 8_192,
        max_total_branch_decisions: 8_192,
    };
}

impl Default for TjaImportLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
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
        self.import_song_with_limits(raw, TjaImportLimits::default())
    }

    /// Import one `.tja` blob under explicit parser and canonical output budgets.
    pub fn import_song_with_limits(
        &self,
        raw: &[u8],
        limits: TjaImportLimits,
    ) -> Result<ImportedSong, ImportError> {
        check_limit("raw bytes", raw.len(), limits.max_raw_bytes)?;
        let text = decode_text(raw)?;
        let preflight = preflight_source(&text, &limits)?;

        let mut parser = TJAParser::with_mode(ParsingMode::FullWithBlanks);
        parser
            .parse_str(&text)
            .map_err(ImportError::InvalidFormat)?;

        let metadata = parser
            .get_metadata()
            .ok_or_else(|| ImportError::InvalidFormat("missing TJA metadata".to_owned()))?;
        let charts = parser.get_charts();
        check_limit("courses", charts.len(), limits.max_courses)?;
        if charts.len() != preflight.courses.len() {
            return Err(ImportError::InvalidFormat(format!(
                "TJA parser course count differs from strict source scan: {} != {}",
                charts.len(),
                preflight.courses.len()
            )));
        }
        let mut parsed_total_segments = 0_usize;
        let mut parsed_total_note_symbols = 0_usize;
        for (course_index, (chart, expected)) in charts.iter().zip(&preflight.courses).enumerate() {
            check_limit(
                "segments per course",
                chart.segments.len(),
                limits.max_segments_per_course,
            )?;
            if chart.segments.len() != expected.segments {
                return Err(ImportError::InvalidFormat(format!(
                    "TJA parser segment count differs from strict source scan for course #{}: {} != {}",
                    course_index + 1,
                    chart.segments.len(),
                    expected.segments
                )));
            }
            if chart.balloons.len() != expected.balloon_declaration.parser_value_count() {
                return Err(ImportError::InvalidFormat(format!(
                    "TJA parser BALLOON count differs from strict source scan for course #{}: {} != {}",
                    course_index + 1,
                    chart.balloons.len(),
                    expected.balloon_declaration.parser_value_count()
                )));
            }
            parsed_total_segments = parsed_total_segments
                .checked_add(chart.segments.len())
                .ok_or_else(|| {
                    ImportError::InvalidFormat(
                        "TJA import counter overflow for total parsed segments".to_owned(),
                    )
                })?;
            let course_note_symbols = chart
                .segments
                .iter()
                .try_fold(0_usize, |count, segment| {
                    count.checked_add(segment.notes.len())
                })
                .ok_or_else(|| {
                    ImportError::InvalidFormat(
                        "TJA import counter overflow for parsed note symbols".to_owned(),
                    )
                })?;
            check_limit(
                "note symbols per course",
                course_note_symbols,
                limits.max_note_symbols_per_course,
            )?;
            if course_note_symbols != expected.note_symbols {
                return Err(ImportError::InvalidFormat(format!(
                    "TJA parser note count differs from strict source scan for course #{}: {} != {}",
                    course_index + 1,
                    course_note_symbols,
                    expected.note_symbols
                )));
            }
            parsed_total_note_symbols = parsed_total_note_symbols
                .checked_add(course_note_symbols)
                .ok_or_else(|| {
                    ImportError::InvalidFormat(
                        "TJA import counter overflow for total parsed note symbols".to_owned(),
                    )
                })?;
        }
        check_limit(
            "total segments",
            parsed_total_segments,
            limits.max_total_segments,
        )?;
        check_limit(
            "total note symbols",
            parsed_total_note_symbols,
            limits.max_total_note_symbols,
        )?;

        let mut indexed_charts = Vec::new();
        try_reserve_exact(&mut indexed_charts, charts.len(), "course index")?;
        indexed_charts.extend(0..charts.len());
        indexed_charts.sort_by_key(|idx| {
            let chart = &charts[*idx];
            (
                chart.course().map_or(u8::MAX, course_rank),
                chart.level().unwrap_or(i32::MAX),
                *idx,
            )
        });

        let mut totals = ImportTotals::default();
        let mut courses = Vec::new();
        try_reserve_exact(&mut courses, indexed_charts.len(), "imported courses")?;
        for chart_idx in indexed_charts {
            let canonical = build_chart(
                metadata,
                &charts[chart_idx],
                preflight.courses[chart_idx],
                &limits,
                &mut totals,
            )?;
            let branch_decisions =
                build_branch_decision_table_bounded(&canonical, &limits, &mut totals)?;
            try_push(
                &mut courses,
                ImportedCourse {
                    chart: canonical,
                    branch_decisions,
                },
                "imported course",
            )?;
        }

        Ok(ImportedSong {
            title: metadata.get("TITLE").cloned().unwrap_or_default(),
            subtitle: metadata.get("SUBTITLE").cloned().unwrap_or_default(),
            artist: metadata.get("ARTIST").cloned().unwrap_or_default(),
            audio_path: metadata
                .get("WAVE")
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            demo_start_seconds: parse_optional_f64(metadata, "DEMOSTART")?,
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

    /// Import all canonical courses under explicit resource budgets.
    pub fn import_all_with_limits(
        &self,
        raw: &[u8],
        limits: TjaImportLimits,
    ) -> Result<Vec<CanonicalChart>, ImportError> {
        let song = self.import_song_with_limits(raw, limits)?;
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
                    decision_tick: tick.saturating_sub(measure_ticks).max(0),
                    default_route_id: segment.default_route_id,
                    route_count: segment.route_count,
                    hint: segment.decision_hint.clone(),
                }
            })
        })
        .collect::<Vec<_>>();

    points.sort_by_key(|point| (point.decision_tick, point.segment_id));
    points
}

fn build_branch_decision_table_bounded(
    chart: &CanonicalChart,
    limits: &TjaImportLimits,
    totals: &mut ImportTotals,
) -> Result<Vec<BranchDecisionPoint>, ImportError> {
    let mut budget = CourseOutputBudget::new(limits, totals);
    let mut earliest_tick = HashMap::<u32, Tick>::new();
    earliest_tick
        .try_reserve(chart.branch_segments.len())
        .map_err(|error| {
            ImportError::InvalidFormat(format!(
                "TJA import allocation failed while reserving branch decision index: {error}"
            ))
        })?;
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

    let mut points = Vec::new();
    for segment in &chart.branch_segments {
        let Some(tick) = earliest_tick.get(&segment.id) else {
            continue;
        };
        budget.branch_decision()?;
        let measure_ticks = measure_duration_ticks_at(chart, *tick);
        try_push(
            &mut points,
            BranchDecisionPoint {
                segment_id: segment.id,
                decision_tick: tick.saturating_sub(measure_ticks).max(0),
                default_route_id: segment.default_route_id,
                route_count: segment.route_count,
                hint: segment.decision_hint.clone(),
            },
            "branch decisions",
        )?;
    }

    points.sort_by_key(|point| (point.decision_tick, point.segment_id));
    Ok(points)
}

fn measure_duration_ticks_at(chart: &CanonicalChart, tick: Tick) -> Tick {
    let micros_per_quarter = tempo_micros_per_quarter_at(&chart.tempo_map, tick);
    let (numerator, denominator) = signature_at(&chart.signatures, tick);

    let measure =
        (u64::from(micros_per_quarter) * u64::from(numerator) * 4) / u64::from(denominator);
    measure as Tick
}

fn tempo_micros_per_quarter_at(tempo_map: &[TempoChange], tick: Tick) -> u32 {
    let idx = tempo_map.partition_point(|tempo| tempo.tick <= tick);
    if idx == 0 {
        tempo_map
            .first()
            .expect("validated canonical charts have a nonempty tempo map")
            .micros_per_quarter
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

#[derive(Debug, Default)]
struct PreflightSummary {
    courses: Vec<PreflightCourseShape>,
}

#[derive(Debug, Clone, Copy)]
struct PreflightCourseShape {
    note_symbols: usize,
    segments: usize,
    balloon_symbols: usize,
    balloon_declaration: BalloonDeclaration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum BalloonDeclaration {
    #[default]
    Missing,
    Default,
    Explicit(usize),
}

impl BalloonDeclaration {
    fn parser_value_count(self) -> usize {
        match self {
            Self::Explicit(count) => count,
            Self::Missing | Self::Default => 0,
        }
    }
}

#[derive(Debug, Default)]
struct PreflightCourse {
    note_symbols: usize,
    segments: usize,
    potential_objects: usize,
    balloon_symbols: usize,
    balloon_declaration: BalloonDeclaration,
    open_segment: bool,
    after_branch_end: bool,
}

#[derive(Debug, Default)]
struct PreflightTotals {
    note_symbols: usize,
    segments: usize,
    potential_objects: usize,
}

#[derive(Debug, Default)]
struct ImportTotals {
    objects: usize,
    events: usize,
    tempo_changes: usize,
    time_signatures: usize,
    branch_segments: usize,
    branch_decisions: usize,
}

struct CourseOutputBudget<'a> {
    limits: &'a TjaImportLimits,
    totals: &'a mut ImportTotals,
    objects: usize,
    events: usize,
    tempo_changes: usize,
    time_signatures: usize,
    branch_segments: usize,
    branch_decisions: usize,
}

impl<'a> CourseOutputBudget<'a> {
    fn new(limits: &'a TjaImportLimits, totals: &'a mut ImportTotals) -> Self {
        Self {
            limits,
            totals,
            objects: 0,
            events: 0,
            tempo_changes: 0,
            time_signatures: 0,
            branch_segments: 0,
            branch_decisions: 0,
        }
    }

    fn object(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.objects,
            &mut self.totals.objects,
            self.limits.max_objects_per_course,
            self.limits.max_total_objects,
            "objects per course",
            "total objects",
        )
    }

    fn event(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.events,
            &mut self.totals.events,
            self.limits.max_events_per_course,
            self.limits.max_total_events,
            "events per course",
            "total events",
        )
    }

    fn tempo_change(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.tempo_changes,
            &mut self.totals.tempo_changes,
            self.limits.max_tempo_changes_per_course,
            self.limits.max_total_tempo_changes,
            "tempo changes per course",
            "total tempo changes",
        )
    }

    fn time_signature(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.time_signatures,
            &mut self.totals.time_signatures,
            self.limits.max_time_signatures_per_course,
            self.limits.max_total_time_signatures,
            "time signatures per course",
            "total time signatures",
        )
    }

    fn branch_segment(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.branch_segments,
            &mut self.totals.branch_segments,
            self.limits.max_branch_segments_per_course,
            self.limits.max_total_branch_segments,
            "branch segments per course",
            "total branch segments",
        )
    }

    fn check_additional_branch_segments(&self, additional: usize) -> Result<(), ImportError> {
        let per_course = self
            .branch_segments
            .checked_add(additional)
            .ok_or_else(|| {
                ImportError::InvalidFormat(
                    "TJA import counter overflow for branch segments per course".to_owned(),
                )
            })?;
        let total = self
            .totals
            .branch_segments
            .checked_add(additional)
            .ok_or_else(|| {
                ImportError::InvalidFormat(
                    "TJA import counter overflow for total branch segments".to_owned(),
                )
            })?;
        check_limit(
            "branch segments per course",
            per_course,
            self.limits.max_branch_segments_per_course,
        )?;
        check_limit(
            "total branch segments",
            total,
            self.limits.max_total_branch_segments,
        )
    }

    fn branch_decision(&mut self) -> Result<(), ImportError> {
        increment_bounded(
            &mut self.branch_decisions,
            &mut self.totals.branch_decisions,
            self.limits.max_branch_decisions_per_course,
            self.limits.max_total_branch_decisions,
            "branch decisions per course",
            "total branch decisions",
        )
    }
}

fn preflight_source(text: &str, limits: &TjaImportLimits) -> Result<PreflightSummary, ImportError> {
    let mut summary = PreflightSummary::default();
    let mut totals = PreflightTotals::default();
    let mut current = None::<PreflightCourse>;
    let mut metadata_phase = true;
    let mut seen_bpm = false;
    let mut pending_balloon_declaration = BalloonDeclaration::Missing;

    for (line_index, raw_line) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let normalized = raw_line
            .split_once("//")
            .map_or(raw_line, |(before_comment, _)| before_comment)
            .trim();
        if normalized.is_empty() {
            continue;
        }

        check_limit("line bytes", normalized.len(), limits.max_line_bytes)?;

        if current
            .as_ref()
            .is_some_and(|course| course.after_branch_end)
            && normalized != "#END"
        {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: only #END may follow #BRANCHEND because tja@0.5.0 drops later chart content"
            )));
        }

        if let Some(command) = normalized.strip_prefix('#') {
            let (name, args) = command
                .split_once(' ')
                .map_or((command, ""), |(name, args)| (name, args.trim()));

            if name.eq_ignore_ascii_case("START") {
                if name != "START" {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: #START must use uppercase spelling"
                    )));
                }
                if current.is_some() {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: nested #START is not allowed"
                    )));
                }
                if metadata_phase {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: at least one course header is required before #START"
                    )));
                }
                if !seen_bpm {
                    return Err(ImportError::InvalidFormat(
                        "missing required finite positive BPM metadata".to_owned(),
                    ));
                }
                if !matches!(args, "" | "P1" | "P2") {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: #START accepts only no player, P1, or P2"
                    )));
                }

                let course_count = checked_next(summary.courses.len(), "courses")?;
                check_limit("courses", course_count, limits.max_courses)?;
                current = Some(PreflightCourse {
                    balloon_declaration: std::mem::take(&mut pending_balloon_declaration),
                    ..PreflightCourse::default()
                });
                continue;
            }

            if name.eq_ignore_ascii_case("END") {
                if name != "END" {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: #END must use uppercase spelling"
                    )));
                }
                require_no_directive_args(name, args, line_number)?;
                let Some(mut course) = current.take() else {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: #END without an open course"
                    )));
                };
                if course.open_segment {
                    preflight_segment(&mut course, &mut totals, limits)?;
                }
                match course.balloon_declaration {
                    BalloonDeclaration::Explicit(count) if course.balloon_symbols != count => {
                        return Err(ImportError::InvalidFormat(format!(
                            "line {line_number}: BALLOON values must match balloon notes exactly: {} != {}",
                            count, course.balloon_symbols
                        )));
                    }
                    BalloonDeclaration::Missing
                    | BalloonDeclaration::Default
                    | BalloonDeclaration::Explicit(_) => {}
                }
                try_push(
                    &mut summary.courses,
                    PreflightCourseShape {
                        note_symbols: course.note_symbols,
                        segments: course.segments,
                        balloon_symbols: course.balloon_symbols,
                        balloon_declaration: course.balloon_declaration,
                    },
                    "preflight course shapes",
                )?;
                continue;
            }

            let Some(course) = current.as_mut() else {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: directive #{name} is only valid inside #START/#END"
                )));
            };
            validate_chart_directive(name, args, line_number, course, limits)?;
            continue;
        }

        if let Some(course) = current.as_mut() {
            for byte in normalized.bytes() {
                match byte {
                    b'0'..=b'9' => {
                        course.note_symbols =
                            checked_next(course.note_symbols, "note symbols per course")?;
                        totals.note_symbols =
                            checked_next(totals.note_symbols, "total note symbols")?;
                        check_limit(
                            "note symbols per course",
                            course.note_symbols,
                            limits.max_note_symbols_per_course,
                        )?;
                        check_limit(
                            "total note symbols",
                            totals.note_symbols,
                            limits.max_total_note_symbols,
                        )?;
                        course.open_segment = true;

                        if matches!(byte, b'7' | b'9') {
                            course.balloon_symbols =
                                checked_next(course.balloon_symbols, "balloon notes per course")?;
                        }

                        if matches!(byte, b'1'..=b'4' | b'8') {
                            course.potential_objects =
                                checked_next(course.potential_objects, "objects per course")?;
                            totals.potential_objects =
                                checked_next(totals.potential_objects, "total objects")?;
                            check_limit(
                                "objects per course",
                                course.potential_objects,
                                limits.max_objects_per_course,
                            )?;
                            check_limit(
                                "total objects",
                                totals.potential_objects,
                                limits.max_total_objects,
                            )?;
                        }
                    }
                    b',' => {
                        preflight_segment(course, &mut totals, limits)?;
                        course.open_segment = false;
                    }
                    byte if byte.is_ascii_whitespace() => {}
                    _ => {
                        return Err(ImportError::InvalidFormat(format!(
                            "line {line_number}: chart data may contain only note digits, commas, and ASCII whitespace"
                        )));
                    }
                }
            }
            continue;
        }

        let (key, value) = normalized.split_once(':').ok_or_else(|| {
            ImportError::InvalidFormat(format!(
                "line {line_number}: expected a metadata/header key:value pair"
            ))
        })?;
        let key = key.trim().to_ascii_uppercase();
        let value = value.trim();
        if key.is_empty() {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: metadata/header key cannot be empty"
            )));
        }
        check_limit(
            "metadata/header value bytes",
            value.len(),
            limits.max_key_value_bytes,
        )?;

        if is_metadata_key(&key) {
            if !metadata_phase {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: global metadata {key} appears after course headers"
                )));
            }
            validate_metadata_value(&key, value, line_number, &mut seen_bpm)?;
        } else if is_header_key(&key) {
            metadata_phase = false;
            validate_header_value(
                &key,
                value,
                line_number,
                limits,
                &mut pending_balloon_declaration,
            )?;
        } else {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: unsupported metadata/header key {key}"
            )));
        }
    }

    if current.is_some() {
        return Err(ImportError::InvalidFormat(
            "unterminated course: missing #END".to_owned(),
        ));
    }
    if summary.courses.is_empty() {
        return Err(ImportError::InvalidFormat(
            "TJA source contains no complete courses".to_owned(),
        ));
    }

    Ok(summary)
}

fn validate_chart_directive(
    name: &str,
    args: &str,
    line_number: usize,
    course: &mut PreflightCourse,
    limits: &TjaImportLimits,
) -> Result<(), ImportError> {
    match name.to_ascii_uppercase().as_str() {
        "BPMCHANGE" => {
            let bpm = parse_required_f64(args, "#BPMCHANGE", line_number)?;
            bpm_to_micros_per_quarter(bpm)?;
        }
        "SCROLL" => {
            let scroll = parse_required_f64(args, "#SCROLL", line_number)?;
            scroll_to_scaled(scroll)?;
        }
        "DELAY" => {
            let delay = parse_required_f64(args, "#DELAY", line_number)?;
            timestamp_to_tick(delay, "#DELAY")?;
        }
        "MEASURE" => {
            let (numerator, denominator) = args.split_once('/').ok_or_else(|| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: #MEASURE requires numerator/denominator"
                ))
            })?;
            let numerator = numerator.trim().parse::<u8>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: #MEASURE numerator must be in 1..={}",
                    u8::MAX
                ))
            })?;
            let denominator = denominator.trim().parse::<u8>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: #MEASURE denominator must be in 1..={}",
                    u8::MAX
                ))
            })?;
            if numerator == 0 || denominator == 0 {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: #MEASURE values must be non-zero"
                )));
            }
        }
        "GOGOSTART" | "GOGOEND" | "BARLINEOFF" | "BARLINEON" | "N" | "E" | "M" => {
            require_no_directive_args(name, args, line_number)?;
        }
        "BRANCHSTART" => {
            check_limit(
                "branch condition bytes",
                args.len(),
                limits.max_branch_condition_bytes,
            )?;
            parse_branch_decision_hint(args)?;
        }
        "BRANCHEND" => {
            require_no_directive_args(name, args, line_number)?;
            course.after_branch_end = true;
        }
        "SECTION" => {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: #SECTION is unsupported because tja@0.5.0 treats it as a no-op"
            )));
        }
        _ => {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: unsupported or malformed directive #{name}"
            )));
        }
    }
    Ok(())
}

fn require_no_directive_args(
    name: &str,
    args: &str,
    line_number: usize,
) -> Result<(), ImportError> {
    if !args.is_empty() {
        return Err(ImportError::InvalidFormat(format!(
            "line {line_number}: #{name} does not accept arguments"
        )));
    }
    Ok(())
}

fn parse_required_f64(raw: &str, context: &str, line_number: usize) -> Result<f64, ImportError> {
    if raw.is_empty() {
        return Err(ImportError::InvalidFormat(format!(
            "line {line_number}: {context} requires a numeric value"
        )));
    }
    let value = raw.parse::<f64>().map_err(|_| {
        ImportError::InvalidFormat(format!(
            "line {line_number}: invalid {context} numeric value {raw:?}"
        ))
    })?;
    if !value.is_finite() {
        return Err(ImportError::InvalidFormat(format!(
            "line {line_number}: {context} must be finite"
        )));
    }
    Ok(value)
}

fn is_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "TITLE"
            | "SUBTITLE"
            | "WAVE"
            | "BPM"
            | "OFFSET"
            | "DEMOSTART"
            | "GENRE"
            | "MAKER"
            | "SONGVOL"
            | "SEVOL"
            | "SCOREMODE"
            | "TITLEJA"
            | "TITLEEN"
            | "TITLECN"
            | "TITLETW"
            | "TITLEZH"
            | "TITLEKO"
            | "SUBTITLEJA"
            | "SUBTITLEEN"
            | "SUBTITLECN"
            | "SUBTITLETW"
            | "SUBTITLEZH"
            | "SUBTITLEKO"
    )
}

fn is_header_key(key: &str) -> bool {
    matches!(
        key,
        "COURSE" | "LEVEL" | "BALLOON" | "SCOREINIT" | "SCOREDIFF" | "STYLE"
    )
}

fn validate_metadata_value(
    key: &str,
    value: &str,
    line_number: usize,
    seen_bpm: &mut bool,
) -> Result<(), ImportError> {
    match key {
        "BPM" => {
            let bpm = parse_required_f64(value, "BPM", line_number)?;
            bpm_to_micros_per_quarter(bpm)?;
            *seen_bpm = true;
        }
        "OFFSET" => {
            let seconds = parse_required_f64(value, key, line_number)?;
            timestamp_to_tick(seconds, key)?;
        }
        "DEMOSTART" => {
            let seconds = parse_required_f64(value, key, line_number)?;
            if seconds < 0.0 {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: DEMOSTART must be non-negative"
                )));
            }
            timestamp_to_tick(seconds, key)?;
        }
        "SONGVOL" | "SEVOL" => {
            let volume = value.parse::<u8>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: {key} must be an integer in 0..=100"
                ))
            })?;
            if volume > 100 {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: {key} must be in 0..=100"
                )));
            }
        }
        "SCOREMODE" if !value.is_empty() => {
            value.parse::<u8>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: SCOREMODE must be an unsigned integer"
                ))
            })?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_header_value(
    key: &str,
    value: &str,
    line_number: usize,
    limits: &TjaImportLimits,
    pending_balloon_declaration: &mut BalloonDeclaration,
) -> Result<(), ImportError> {
    match key {
        "COURSE" => {
            value.parse::<TjaCourse>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: unsupported COURSE value {value:?}"
                ))
            })?;
        }
        "LEVEL" => {
            let level = value.parse::<u8>().map_err(|_| {
                ImportError::InvalidFormat(format!("line {line_number}: LEVEL must be in 1..=10"))
            })?;
            if !(1..=10).contains(&level) {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: LEVEL must be in 1..=10"
                )));
            }
        }
        "BALLOON" => {
            if value.is_empty() {
                *pending_balloon_declaration = BalloonDeclaration::Default;
                return Ok(());
            }
            let mut count = 0_usize;
            for raw_hits in value.split(',') {
                let hits = raw_hits.trim().parse::<u16>().map_err(|_| {
                    ImportError::InvalidFormat(format!(
                        "line {line_number}: BALLOON hit counts must be in 1..={}",
                        u16::MAX
                    ))
                })?;
                if hits == 0 {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: BALLOON hit counts must be positive"
                    )));
                }
                count = checked_next(count, "balloon values per course")?;
                check_limit(
                    "balloon values per course",
                    count,
                    limits.max_balloon_values_per_course,
                )?;
            }
            *pending_balloon_declaration = BalloonDeclaration::Explicit(count);
        }
        "SCOREINIT" if !value.is_empty() => {
            let mut count = 0_usize;
            for value in value.split(',') {
                count += 1;
                if count > 2 || value.trim().parse::<u32>().is_err() {
                    return Err(ImportError::InvalidFormat(format!(
                        "line {line_number}: SCOREINIT must contain one or two unsigned integers"
                    )));
                }
            }
            if count == 0 {
                return Err(ImportError::InvalidFormat(format!(
                    "line {line_number}: SCOREINIT must contain one or two unsigned integers"
                )));
            }
        }
        "SCOREDIFF" if !value.is_empty() => {
            value.parse::<u32>().map_err(|_| {
                ImportError::InvalidFormat(format!(
                    "line {line_number}: {key} must be an unsigned integer"
                ))
            })?;
        }
        "STYLE" if value.is_empty() => {
            return Err(ImportError::InvalidFormat(format!(
                "line {line_number}: STYLE cannot be empty"
            )));
        }
        _ => {}
    }
    Ok(())
}

fn preflight_segment(
    course: &mut PreflightCourse,
    totals: &mut PreflightTotals,
    limits: &TjaImportLimits,
) -> Result<(), ImportError> {
    course.segments = checked_next(course.segments, "segments per course")?;
    totals.segments = checked_next(totals.segments, "total segments")?;
    check_limit(
        "segments per course",
        course.segments,
        limits.max_segments_per_course,
    )?;
    check_limit("total segments", totals.segments, limits.max_total_segments)
}

fn increment_bounded(
    per_course: &mut usize,
    total: &mut usize,
    per_course_limit: usize,
    total_limit: usize,
    per_course_name: &str,
    total_name: &str,
) -> Result<(), ImportError> {
    let next_per_course = checked_next(*per_course, per_course_name)?;
    let next_total = checked_next(*total, total_name)?;
    check_limit(per_course_name, next_per_course, per_course_limit)?;
    check_limit(total_name, next_total, total_limit)?;
    *per_course = next_per_course;
    *total = next_total;
    Ok(())
}

fn checked_next(current: usize, name: &str) -> Result<usize, ImportError> {
    current.checked_add(1).ok_or_else(|| {
        ImportError::InvalidFormat(format!("TJA import counter overflow for {name}"))
    })
}

fn check_limit(name: &str, observed: usize, limit: usize) -> Result<(), ImportError> {
    if observed > limit {
        return Err(ImportError::InvalidFormat(format!(
            "TJA import limit exceeded: {name} {observed} > {limit}"
        )));
    }
    Ok(())
}

fn try_reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    context: &str,
) -> Result<(), ImportError> {
    values.try_reserve_exact(additional).map_err(|error| {
        ImportError::InvalidFormat(format!(
            "TJA import allocation failed while reserving {context}: {error}"
        ))
    })
}

fn try_push<T>(values: &mut Vec<T>, value: T, context: &str) -> Result<(), ImportError> {
    values.try_reserve(1).map_err(|error| {
        ImportError::InvalidFormat(format!(
            "TJA import allocation failed while growing {context}: {error}"
        ))
    })?;
    values.push(value);
    Ok(())
}

fn try_hash_insert<K, V>(
    values: &mut HashMap<K, V>,
    key: K,
    value: V,
    context: &str,
) -> Result<Option<V>, ImportError>
where
    K: Eq + Hash,
{
    if values.len() == values.capacity() && !values.contains_key(&key) {
        values.try_reserve(1).map_err(|error| {
            ImportError::InvalidFormat(format!(
                "TJA import allocation failed while growing {context}: {error}"
            ))
        })?;
    }
    Ok(values.insert(key, value))
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
    if !parsed.is_finite() {
        return Err(ImportError::InvalidFormat(format!(
            "numeric metadata for {key} must be finite: {trimmed}"
        )));
    }
    Ok(Some(parsed))
}

fn build_chart(
    metadata: &TjaMetadata,
    chart: &TjaChart,
    expected: PreflightCourseShape,
    limits: &TjaImportLimits,
    totals: &mut ImportTotals,
) -> Result<CanonicalChart, ImportError> {
    check_limit(
        "segments per course",
        chart.segments.len(),
        limits.max_segments_per_course,
    )?;
    let parsed_note_symbols = chart
        .segments
        .iter()
        .try_fold(0_usize, |count, segment| {
            count.checked_add(segment.notes.len())
        })
        .ok_or_else(|| {
            ImportError::InvalidFormat(
                "TJA import counter overflow for parsed note symbols".to_owned(),
            )
        })?;
    check_limit(
        "note symbols per course",
        parsed_note_symbols,
        limits.max_note_symbols_per_course,
    )?;

    let mut budget = CourseOutputBudget::new(limits, totals);
    let (branch_segments, segment_branch_meta) = derive_branch_data(&chart.segments, &mut budget)?;

    let seed_micros = bpm_to_micros_per_quarter(metadata.bpm)?;

    let mut tempo_by_tick = BTreeMap::<Tick, u32>::new();
    tempo_by_tick.insert(0, seed_micros);

    let mut signature_by_tick = BTreeMap::<Tick, (u8, u8)>::new();
    signature_by_tick.insert(0, (4, 4));

    let mut barline_by_tick = BTreeMap::<Tick, i32>::new();
    let mut gogo_by_tick = BTreeMap::<Tick, bool>::new();
    let mut gogo_by_stream_tick = HashMap::<(StreamKey, Tick), bool>::new();
    let mut current_scroll_by_stream = HashMap::<StreamKey, i32>::new();

    let mut objects = Vec::<Object>::new();
    let mut next_object_id: u32 = 1;
    let mut balloon_cursor = 0usize;
    let mut open_rolls = HashMap::<StreamKey, PendingRoll>::new();

    for (seg_idx, segment) in chart.segments.iter().enumerate() {
        let seg_tick = timestamp_to_tick(segment.timestamp, "segment timestamp")?;

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
        let stream_scroll_scaled = current_scroll_by_stream
            .get(&stream_key)
            .copied()
            .unwrap_or(SCROLL_SCALE);
        let segment_scroll_scaled = segment
            .notes
            .first()
            .map(|note| scroll_to_scaled(note.scroll))
            .transpose()?
            .unwrap_or(stream_scroll_scaled);

        // Canonical chart events are global; derive bar lines from the default route stream.
        if segment.barline && stream_key.route_id == 0 {
            if let Some(existing) = barline_by_tick.get(&seg_tick) {
                if *existing != segment_scroll_scaled {
                    return Err(ImportError::InvalidFormat(format!(
                        "conflicting bar-line scroll at tick {seg_tick}: {existing} vs {segment_scroll_scaled}"
                    )));
                }
            }
            barline_by_tick.insert(seg_tick, segment_scroll_scaled);
        }

        for note in &segment.notes {
            let tick = timestamp_to_tick(note.timestamp, "note timestamp")?;
            let note_scroll_scaled = scroll_to_scaled(note.scroll)?;
            // TJA allows in-measure BPM edits, and timestamp rounding may collapse adjacent
            // notes with different BPM onto the same tick. Keep the latest BPM at that tick.
            if stream_key.route_id == 0 {
                tempo_by_tick.insert(tick, bpm_to_micros_per_quarter(note.bpm)?);
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

                    budget.object()?;
                    try_push(
                        &mut objects,
                        Object {
                            id: next_object_id,
                            kind: ObjectKind::Tap,
                            start_tick: tick,
                            end_tick: tick,
                            lane_or_region: LaneOrRegion::Lane(lane),
                            flags,
                            required_hits: 0,
                            slide_to: None,
                            scroll_scaled: note_scroll_scaled,
                            branch_segment_id: branch_meta.segment_id,
                            branch_route_id: branch_meta.route_id,
                        },
                        "objects",
                    )?;
                    next_object_id = next_object_id.checked_add(1).ok_or_else(|| {
                        ImportError::InvalidFormat("canonical object id space exhausted".to_owned())
                    })?;
                }
                NoteType::Roll | NoteType::RollBig | NoteType::Balloon | NoteType::BalloonAlt => {
                    if open_rolls.contains_key(&stream_key) {
                        return Err(ImportError::InvalidFormat(format!(
                            "nested roll start in stream {:?} at tick {tick}",
                            stream_key
                        )));
                    }

                    let (flags, required_hits) = match note.note_type {
                        NoteType::Balloon | NoteType::BalloonAlt => (
                            FLAG_BALLOON,
                            next_balloon_hits(
                                chart,
                                &mut balloon_cursor,
                                expected.balloon_declaration,
                            )?,
                        ),
                        NoteType::Roll => (0, 0),
                        NoteType::RollBig => (FLAG_BIG, 0),
                        _ => unreachable!(),
                    };

                    try_hash_insert(
                        &mut open_rolls,
                        stream_key,
                        PendingRoll {
                            start_tick: tick,
                            flags,
                            required_hits,
                            scroll_scaled: note_scroll_scaled,
                        },
                        "open rolls",
                    )?;
                }
                NoteType::EndOf => {
                    let pending = open_rolls.remove(&stream_key).ok_or_else(|| {
                        ImportError::InvalidFormat(format!(
                            "unmatched roll end in stream {:?} at tick {tick}",
                            stream_key
                        ))
                    })?;
                    if tick < pending.start_tick {
                        return Err(ImportError::InvalidFormat(format!(
                            "roll end precedes start in stream {:?}: {} < {}",
                            stream_key, tick, pending.start_tick
                        )));
                    }

                    budget.object()?;
                    try_push(
                        &mut objects,
                        Object {
                            id: next_object_id,
                            kind: ObjectKind::Roll,
                            start_tick: pending.start_tick,
                            end_tick: tick,
                            lane_or_region: LaneOrRegion::Lane(LANE_BOTH),
                            flags: pending.flags,
                            required_hits: pending.required_hits,
                            slide_to: None,
                            scroll_scaled: pending.scroll_scaled,
                            branch_segment_id: branch_meta.segment_id,
                            branch_route_id: branch_meta.route_id,
                        },
                        "objects",
                    )?;
                    next_object_id = next_object_id.checked_add(1).ok_or_else(|| {
                        ImportError::InvalidFormat("canonical object id space exhausted".to_owned())
                    })?;
                }
            }

            try_hash_insert(
                &mut current_scroll_by_stream,
                stream_key,
                note_scroll_scaled,
                "scroll streams",
            )?;
        }
    }

    if !open_rolls.is_empty() {
        return Err(ImportError::InvalidFormat(
            "unclosed roll note at end of chart".to_owned(),
        ));
    }
    if balloon_cursor != expected.balloon_symbols {
        return Err(ImportError::InvalidFormat(format!(
            "TJA parser balloon note count differs from strict source scan: {} != {}",
            balloon_cursor, expected.balloon_symbols
        )));
    }
    let mut branch_has_object = Vec::new();
    try_reserve_exact(
        &mut branch_has_object,
        branch_segments.len(),
        "branch object coverage",
    )?;
    branch_has_object.resize(branch_segments.len(), false);
    for object in &objects {
        let Some(segment_id) = object.branch_segment_id else {
            continue;
        };
        let index = usize::try_from(segment_id)
            .ok()
            .and_then(|id| id.checked_sub(1))
            .filter(|index| *index < branch_has_object.len())
            .ok_or_else(|| {
                ImportError::InvalidFormat(format!(
                    "object {} references invalid derived branch segment {segment_id}",
                    object.id
                ))
            })?;
        branch_has_object[index] = true;
    }
    if let Some(index) = branch_has_object.iter().position(|has_object| !has_object) {
        return Err(ImportError::InvalidFormat(format!(
            "branch segment {} contains no playable objects",
            index + 1
        )));
    }

    let tempo_map = build_tempo_map(&tempo_by_tick, &mut budget)?;
    let signatures = build_signature_map(&signature_by_tick, &mut budget)?;

    let mut events = Vec::new();
    for (tick, scroll_scaled) in barline_by_tick {
        budget.event()?;
        try_push(
            &mut events,
            ChartEvent {
                tick,
                kind: ChartEventKind::BarLine { scroll_scaled },
            },
            "events",
        )?;
    }

    let mut gogo_state = false;
    for (tick, is_gogo) in gogo_by_tick {
        if is_gogo != gogo_state {
            budget.event()?;
            try_push(
                &mut events,
                ChartEvent {
                    tick,
                    kind: if is_gogo {
                        ChartEventKind::GogoStart
                    } else {
                        ChartEventKind::GogoEnd
                    },
                },
                "events",
            )?;
            gogo_state = is_gogo;
        }
    }

    let mut canonical = CanonicalChart {
        metadata: ChartMetadata {
            title: metadata.get("TITLE").cloned().unwrap_or_default(),
            subtitle: metadata.get("SUBTITLE").cloned().unwrap_or_default(),
            artist: metadata.get("ARTIST").cloned().unwrap_or_default(),
            charter: metadata.get("MAKER").cloned().unwrap_or_default(),
            audio_path: metadata
                .get("WAVE")
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty()),
            offset: timestamp_to_tick(metadata.offset, "metadata OFFSET")?,
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
    budget: &mut CourseOutputBudget<'_>,
) -> Result<(Vec<BranchSegment>, Vec<SegmentBranchMeta>), ImportError> {
    let mut branch_segments = Vec::new();
    let mut segment_meta = Vec::new();
    try_reserve_exact(&mut segment_meta, segments.len(), "branch segment metadata")?;
    segment_meta.resize(segments.len(), SegmentBranchMeta::default());

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
                    try_push(&mut run_indices, idx, "branch run indices")?;
                    idx += 1;
                }
                _ => break,
            }
        }

        let blocks = split_route_cycles(segments, &run_indices, budget)?;
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
                    route_end[usize::from(route)].max(segment_end_tick(&segments[seg_idx])?);
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

            budget.branch_segment()?;
            try_push(
                &mut branch_segments,
                BranchSegment {
                    id: branch_id,
                    default_route_id: 0,
                    route_count: 3,
                    decision_hint: parse_branch_decision_hint(&condition)?,
                },
                "branch segments",
            )?;

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
    budget: &CourseOutputBudget<'_>,
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
                    let next_block_count =
                        checked_next(blocks.len(), "branch segments per course")?;
                    budget.check_additional_branch_segments(next_block_count)?;
                    try_push(&mut blocks, current, "branch route blocks")?;
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
        try_push(&mut current, seg_idx, "branch route block")?;
        last_route = Some(route);
    }

    if !current.is_empty() {
        let next_block_count = checked_next(blocks.len(), "branch segments per course")?;
        budget.check_additional_branch_segments(next_block_count)?;
        try_push(&mut blocks, current, "branch route blocks")?;
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
            let max = 100_i32
                .checked_mul(ACCURACY_THRESHOLD_SCALE)
                .expect("accuracy scale fits i32");
            if low < 0 || high > max {
                return Err(ImportError::InvalidFormat(
                    "accuracy thresholds must be in 0..=100 percent".to_owned(),
                ));
            }
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
            if low < 0 || high < 0 {
                return Err(ImportError::InvalidFormat(
                    "roll thresholds must be non-negative".to_owned(),
                ));
            }
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
        _ => {
            return Err(ImportError::InvalidFormat(format!(
                "unsupported branch condition kind: {}",
                parts[0]
            )));
        }
    };

    Ok(hint)
}

fn next_balloon_hits(
    chart: &TjaChart,
    cursor: &mut usize,
    declaration: BalloonDeclaration,
) -> Result<u16, ImportError> {
    let balloon_number = (*cursor)
        .checked_add(1)
        .ok_or_else(|| ImportError::InvalidFormat("balloon index overflow".to_owned()))?;
    let value = match declaration {
        BalloonDeclaration::Missing | BalloonDeclaration::Default => {
            *cursor = balloon_number;
            return Ok(DEFAULT_BALLOON_HITS);
        }
        BalloonDeclaration::Explicit(_) => {
            chart.balloons.get(*cursor).copied().ok_or_else(|| {
                ImportError::InvalidFormat(format!(
                    "missing BALLOON hit count for balloon #{balloon_number}"
                ))
            })?
        }
    };
    *cursor = balloon_number;

    if value <= 0 || value > i32::from(u16::MAX) {
        return Err(ImportError::InvalidFormat(format!(
            "BALLOON hit count #{balloon_number} must be in 1..={}: {value}",
            u16::MAX
        )));
    }

    Ok(value as u16)
}

fn segment_end_tick(segment: &Segment) -> Result<Tick, ImportError> {
    let mut end_sec = segment.timestamp;
    for note in &segment.notes {
        if note.timestamp > end_sec {
            end_sec = note.timestamp;
        }
    }
    timestamp_to_tick(end_sec, "branch segment end timestamp")
}

fn build_tempo_map(
    tempo_by_tick: &BTreeMap<Tick, u32>,
    budget: &mut CourseOutputBudget<'_>,
) -> Result<Vec<TempoChange>, ImportError> {
    let mut map = Vec::new();
    let mut last = None::<u32>;

    for (tick, micros_per_quarter) in tempo_by_tick {
        if last != Some(*micros_per_quarter) {
            budget.tempo_change()?;
            try_push(
                &mut map,
                TempoChange {
                    tick: *tick,
                    micros_per_quarter: *micros_per_quarter,
                },
                "tempo changes",
            )?;
            last = Some(*micros_per_quarter);
        }
    }

    Ok(map)
}

fn build_signature_map(
    signature_by_tick: &BTreeMap<Tick, (u8, u8)>,
    budget: &mut CourseOutputBudget<'_>,
) -> Result<Vec<TimeSignatureChange>, ImportError> {
    let mut map = Vec::new();
    let mut last = None::<(u8, u8)>;

    for (tick, (numerator, denominator)) in signature_by_tick {
        if last != Some((*numerator, *denominator)) {
            budget.time_signature()?;
            try_push(
                &mut map,
                TimeSignatureChange {
                    tick: *tick,
                    numerator: *numerator,
                    denominator: *denominator,
                },
                "time signatures",
            )?;
            last = Some((*numerator, *denominator));
        }
    }

    Ok(map)
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
    try_hash_insert(
        gogo_by_stream_tick,
        (stream_key, tick),
        gogo,
        "per-stream gogo states",
    )?;
    Ok(())
}

fn bpm_to_micros_per_quarter(bpm: f64) -> Result<u32, ImportError> {
    if !bpm.is_finite() || bpm <= 0.0 {
        return Err(ImportError::InvalidFormat(format!(
            "BPM must be finite and positive: {bpm}"
        )));
    }

    let micros_per_quarter = (60_000_000.0 / bpm).round();
    if !micros_per_quarter.is_finite()
        || micros_per_quarter < 1.0
        || micros_per_quarter > f64::from(u32::MAX)
    {
        return Err(ImportError::InvalidFormat(format!(
            "BPM is outside the canonical tempo range: {bpm}"
        )));
    }

    Ok(micros_per_quarter as u32)
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

fn scroll_to_scaled(scroll: f64) -> Result<i32, ImportError> {
    if !scroll.is_finite() {
        return Err(ImportError::InvalidFormat(format!(
            "SCROLL must be finite: {scroll}"
        )));
    }
    let scaled = (scroll * f64::from(SCROLL_SCALE)).round();
    if !scaled.is_finite() || scaled < f64::from(i32::MIN) || scaled > f64::from(i32::MAX) {
        return Err(ImportError::InvalidFormat(format!(
            "SCROLL is outside the canonical fixed-point range: {scroll}"
        )));
    }
    Ok(scaled as i32)
}

fn timestamp_to_tick(seconds: f64, context: &str) -> Result<Tick, ImportError> {
    if !seconds.is_finite() {
        return Err(ImportError::InvalidFormat(format!(
            "{context} must be finite: {seconds}"
        )));
    }

    const TICK_MIN_INCLUSIVE: f64 = -9_223_372_036_854_775_808.0;
    const TICK_MAX_EXCLUSIVE: f64 = 9_223_372_036_854_775_808.0;
    let ticks = (seconds * rhythm_chart::TICKS_PER_SECOND as f64).round();
    if !ticks.is_finite() || !(TICK_MIN_INCLUSIVE..TICK_MAX_EXCLUSIVE).contains(&ticks) {
        return Err(ImportError::InvalidFormat(format!(
            "{context} is outside the canonical tick range: {seconds}"
        )));
    }

    Ok(ticks_from_seconds(seconds))
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
