use std::collections::HashSet;
use std::fmt;
use std::marker::PhantomData;

pub use rhythm_chart::MAX_BRANCH_HINT_BYTES;
use rhythm_chart::{BranchDecisionHint, Tick, CANONICAL_SCHEMA_SHA256};
use serde::de::{IgnoredAny, SeqAccess, Visitor};
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const API_VERSION: u32 = 1;
/// Canonical semantic descriptor hashed by [`WIRE_SCHEMA_SHA256`].
///
/// Protocol version `1` is intentionally replaced in place during this
/// pre-release redesign. No compatibility shape or field default is retained.
pub const WIRE_SCHEMA_DESCRIPTOR: &str = concat!(
    "taiko-resource-protocol/v1\n",
    "wire=strict-json+deny-unknown-fields+all-fields-required\n",
    "library-fields=api_version,wire_schema_sha256,semantics,songs,warnings\n",
    "song-fields=song_id,source_path,source_id,audio_path(required-nullable),",
    "audio_id(required-nullable),title,subtitle,artist,",
    "demo_start_seconds,courses\n",
    "course-fields=index,name,level,canonical_chart_hash,object_count,branch_segment_count,",
    "base_bpm,branch_decisions\n",
    "branch-decision-fields=segment_id,decision_tick,default_route_id,route_count,hint\n",
    "content-ids=source_id:lowercase-sha256+audio_id:optional-lowercase-sha256;",
    "audio-path-id=both-present-or-both-null\n",
    "canonical-chart-id=sha256(taiko-canonical-chart/v1-domain+canonical-schema-sha256",
    "+nul+serde-json-compact-canonical-chart)\n",
    "song-id=sha256(taiko-song-manifest/v1-domain+source_id+audio-present-u8+optional-audio_id",
    "+four-semantics-version-and-sha256+dense-course-count",
    "+ordered-canonical-chart-hashes)\n",
    "courses=nonempty+dense-zero-based-order;",
    "branch-decisions=count==branch-segment-count+unique-segment-id",
    "+strict-tick-segment-order+nonnegative-tick+route-count>0",
    "+default-route<route-count+bounded-hint\n",
    "semantics=canonical-schema+importer+taiko-ruleset+audio-decoder;",
    "each=nonzero-version+lowercase-sha256\n",
    "limits=library-bytes:16777216,chart-bytes:16777216,audio-bytes:268435456,",
    "songs:32768,warnings:4096,courses-per-song:16,branch-decisions-per-course:65536,",
    "path-bytes:4096,title-bytes:160,subtitle-bytes:160,artist-bytes:160,",
    "course-name-bytes:64,branch-hint-bytes:1024,warning-bytes:4096;",
    "required-presentation=no-surrounding-whitespace\n",
);
pub const WIRE_SCHEMA_SHA256: &str =
    "f55ecfdf15bb0ccf8fe2a91c5e9925df3f146980a6ec8247279f8fe8cb86798f";
pub const MAX_LIBRARY_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_CHART_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;
pub const MAX_AUDIO_RESPONSE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_SONGS_PER_LIBRARY: usize = 32_768;
pub const MAX_WARNINGS_PER_LIBRARY: usize = 4_096;
pub const MAX_COURSES_PER_SONG: usize = 16;
pub const MAX_BRANCH_DECISIONS_PER_COURSE: usize = 65_536;
pub const MAX_RESOURCE_PATH_BYTES: usize = 4_096;
pub const MAX_TITLE_BYTES: usize = 160;
pub const MAX_SUBTITLE_BYTES: usize = 160;
pub const MAX_ARTIST_BYTES: usize = 160;
pub const MAX_COURSE_NAME_BYTES: usize = 64;
pub const MAX_WARNING_BYTES: usize = 4_096;

const CANONICAL_CHART_HASH_DOMAIN: &[u8] = b"taiko-canonical-chart/v1\0";
const SONG_MANIFEST_HASH_DOMAIN: &[u8] = b"taiko-song-manifest/v1\0";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLibraryDocument {
    pub api_version: u32,
    pub wire_schema_sha256: String,
    pub semantics: ResourceSemantics,
    #[serde(deserialize_with = "deserialize_songs")]
    pub songs: Vec<ResourceSong>,
    #[serde(deserialize_with = "deserialize_warnings")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSemantics {
    pub canonical_schema_version: u32,
    pub canonical_schema_sha256: String,
    pub importer_semantics_version: u32,
    pub importer_semantics_sha256: String,
    pub taiko_ruleset_version: u32,
    pub taiko_ruleset_sha256: String,
    pub audio_decoder_semantics_version: u32,
    pub audio_decoder_semantics_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSong {
    pub song_id: String,
    pub source_path: String,
    pub source_id: String,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub audio_path: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub audio_id: Option<String>,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub demo_start_seconds: f64,
    #[serde(deserialize_with = "deserialize_courses")]
    pub courses: Vec<ResourceCourse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCourse {
    pub index: u32,
    pub name: String,
    pub level: Option<u8>,
    pub canonical_chart_hash: String,
    pub object_count: u32,
    pub branch_segment_count: u32,
    pub base_bpm: Option<f64>,
    #[serde(deserialize_with = "deserialize_branch_decisions")]
    pub branch_decisions: Vec<ResourceBranchDecisionPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceBranchDecisionPoint {
    pub segment_id: u32,
    pub decision_tick: Tick,
    pub default_route_id: u8,
    pub route_count: u8,
    pub hint: Option<BranchDecisionHint>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{message}")]
pub struct ResourceValidationError {
    message: String,
}

struct BoundedVecVisitor<T, const MAX: usize> {
    label: &'static str,
    marker: PhantomData<T>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

impl<'de, T, const MAX: usize> Visitor<'de> for BoundedVecVisitor<T, MAX>
where
    T: Deserialize<'de>,
{
    type Value = Vec<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} containing at most {MAX} elements",
            self.label
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(MAX));
        while values.len() < MAX {
            let Some(value) = sequence.next_element()? else {
                return Ok(values);
            };
            values.push(value);
        }
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(format_args!(
                "{} exceeds maximum element count {MAX}",
                self.label
            )));
        }
        Ok(values)
    }
}

fn deserialize_bounded_vec<'de, D, T, const MAX: usize>(
    deserializer: D,
    label: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_seq(BoundedVecVisitor::<T, MAX> {
        label,
        marker: PhantomData,
    })
}

fn deserialize_songs<'de, D>(deserializer: D) -> Result<Vec<ResourceSong>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, ResourceSong, MAX_SONGS_PER_LIBRARY>(deserializer, "songs")
}

fn deserialize_warnings<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, String, MAX_WARNINGS_PER_LIBRARY>(deserializer, "warnings")
}

fn deserialize_courses<'de, D>(deserializer: D) -> Result<Vec<ResourceCourse>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, ResourceCourse, MAX_COURSES_PER_SONG>(deserializer, "courses")
}

fn deserialize_branch_decisions<'de, D>(
    deserializer: D,
) -> Result<Vec<ResourceBranchDecisionPoint>, D::Error>
where
    D: Deserializer<'de>,
{
    deserialize_bounded_vec::<D, ResourceBranchDecisionPoint, MAX_BRANCH_DECISIONS_PER_COURSE>(
        deserializer,
        "branch_decisions",
    )
}

impl ResourceValidationError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl ResourceLibraryDocument {
    /// Validates the complete v1 resource contract after deserialization.
    ///
    /// The transport byte limit must still be enforced before deserialization.
    pub fn validate(&self) -> Result<(), ResourceValidationError> {
        if self.api_version != API_VERSION {
            return Err(ResourceValidationError::new(format!(
                "unsupported resource API version {} (expected {API_VERSION})",
                self.api_version
            )));
        }
        if self.wire_schema_sha256 != WIRE_SCHEMA_SHA256 {
            return Err(ResourceValidationError::new(format!(
                "unsupported resource wire schema {} (expected {WIRE_SCHEMA_SHA256})",
                self.wire_schema_sha256
            )));
        }
        self.semantics.validate()?;
        if self.songs.len() > MAX_SONGS_PER_LIBRARY {
            return Err(ResourceValidationError::new(format!(
                "library has {} songs; maximum is {MAX_SONGS_PER_LIBRARY}",
                self.songs.len()
            )));
        }
        if self.warnings.len() > MAX_WARNINGS_PER_LIBRARY {
            return Err(ResourceValidationError::new(format!(
                "library has {} warnings; maximum is {MAX_WARNINGS_PER_LIBRARY}",
                self.warnings.len()
            )));
        }

        for (index, warning) in self.warnings.iter().enumerate() {
            validate_text(warning, MAX_WARNING_BYTES, true)
                .map_err(|error| error.with_context(format!("library warning {index}")))?;
        }
        let mut song_ids = HashSet::with_capacity(self.songs.len());
        let mut source_paths = HashSet::with_capacity(self.songs.len());
        for (index, song) in self.songs.iter().enumerate() {
            song.validate(&self.semantics)
                .map_err(|error| error.with_context(format!("song {index}")))?;
            if !song_ids.insert(song.song_id.as_str()) {
                return Err(ResourceValidationError::new(format!(
                    "song {index}: duplicate song_id {}",
                    song.song_id
                )));
            }
            if !source_paths.insert(song.source_path.as_str()) {
                return Err(ResourceValidationError::new(format!(
                    "song {index}: duplicate source_path {}",
                    song.source_path
                )));
            }
        }
        Ok(())
    }
}

impl ResourceSemantics {
    pub fn validate(&self) -> Result<(), ResourceValidationError> {
        validate_sha256(&self.canonical_schema_sha256)
            .map_err(|error| error.with_context("canonical schema fingerprint"))?;
        validate_sha256(&self.importer_semantics_sha256)
            .map_err(|error| error.with_context("importer semantics fingerprint"))?;
        validate_sha256(&self.taiko_ruleset_sha256)
            .map_err(|error| error.with_context("taiko ruleset fingerprint"))?;
        validate_sha256(&self.audio_decoder_semantics_sha256)
            .map_err(|error| error.with_context("audio decoder semantics fingerprint"))?;
        if self.canonical_schema_version == 0
            || self.importer_semantics_version == 0
            || self.taiko_ruleset_version == 0
            || self.audio_decoder_semantics_version == 0
        {
            return Err(ResourceValidationError::new(
                "resource semantic versions must be non-zero",
            ));
        }
        Ok(())
    }
}

impl ResourceSong {
    pub fn validate(&self, semantics: &ResourceSemantics) -> Result<(), ResourceValidationError> {
        validate_sha256(&self.song_id).map_err(|error| error.with_context("song_id"))?;
        validate_resource_path(&self.source_path)
            .map_err(|error| error.with_context("source_path"))?;
        validate_sha256(&self.source_id).map_err(|error| error.with_context("source_id"))?;
        match (&self.audio_path, &self.audio_id) {
            (Some(audio_path), Some(audio_id)) => {
                validate_resource_path(audio_path)
                    .map_err(|error| error.with_context("audio_path"))?;
                validate_sha256(audio_id).map_err(|error| error.with_context("audio_id"))?;
            }
            (None, None) => {}
            _ => {
                return Err(ResourceValidationError::new(
                    "audio_path and audio_id must either both be present or both be null",
                ));
            }
        }
        validate_required_presentation_text(&self.title, MAX_TITLE_BYTES)
            .map_err(|error| error.with_context("title"))?;
        validate_text(&self.subtitle, MAX_SUBTITLE_BYTES, true)
            .map_err(|error| error.with_context("subtitle"))?;
        validate_text(&self.artist, MAX_ARTIST_BYTES, true)
            .map_err(|error| error.with_context("artist"))?;
        if !self.demo_start_seconds.is_finite() || self.demo_start_seconds < 0.0 {
            return Err(ResourceValidationError::new(
                "demo_start_seconds must be finite and non-negative",
            ));
        }
        if self.courses.is_empty() {
            return Err(ResourceValidationError::new(
                "song must contain at least one playable course",
            ));
        }
        if self.courses.len() > MAX_COURSES_PER_SONG {
            return Err(ResourceValidationError::new(format!(
                "song has {} courses; maximum is {MAX_COURSES_PER_SONG}",
                self.courses.len()
            )));
        }
        for (position, course) in self.courses.iter().enumerate() {
            let expected_index = u32::try_from(position)
                .map_err(|_| ResourceValidationError::new("course position exceeds u32"))?;
            if course.index != expected_index {
                return Err(ResourceValidationError::new(format!(
                    "course indices must be dense and ordered; expected {expected_index}, got {}",
                    course.index
                )));
            }
            course
                .validate()
                .map_err(|error| error.with_context(format!("course {position}")))?;
        }
        let expected_song_id = song_manifest_sha256(
            &self.source_id,
            self.audio_id.as_deref(),
            semantics,
            &self.courses,
        )?;
        if self.song_id != expected_song_id {
            return Err(ResourceValidationError::new(format!(
                "song_id does not match immutable manifest: expected {expected_song_id}, got {}",
                self.song_id
            )));
        }
        Ok(())
    }
}

impl ResourceCourse {
    fn validate(&self) -> Result<(), ResourceValidationError> {
        validate_required_presentation_text(&self.name, MAX_COURSE_NAME_BYTES)
            .map_err(|error| error.with_context("name"))?;
        validate_sha256(&self.canonical_chart_hash)
            .map_err(|error| error.with_context("canonical_chart_hash"))?;
        if self
            .base_bpm
            .is_some_and(|bpm| !bpm.is_finite() || bpm <= 0.0)
        {
            return Err(ResourceValidationError::new(
                "base_bpm must be finite and positive when present",
            ));
        }
        if self.branch_decisions.len() > MAX_BRANCH_DECISIONS_PER_COURSE {
            return Err(ResourceValidationError::new(format!(
                "course has {} branch decisions; maximum is {MAX_BRANCH_DECISIONS_PER_COURSE}",
                self.branch_decisions.len()
            )));
        }
        if u64::try_from(self.branch_decisions.len()).unwrap_or(u64::MAX)
            != u64::from(self.branch_segment_count)
        {
            return Err(ResourceValidationError::new(
                "branch decision count must equal branch_segment_count",
            ));
        }
        let mut previous_key = None;
        let mut segment_ids = HashSet::with_capacity(self.branch_decisions.len());
        for decision in &self.branch_decisions {
            if !segment_ids.insert(decision.segment_id) {
                return Err(ResourceValidationError::new(
                    "branch decision segment_id values must be unique",
                ));
            }
            if decision.route_count == 0 {
                return Err(ResourceValidationError::new(
                    "branch decision route_count must be non-zero",
                ));
            }
            if decision.default_route_id >= decision.route_count {
                return Err(ResourceValidationError::new(
                    "branch decision default_route_id must be smaller than route_count",
                ));
            }
            if decision.decision_tick < 0 {
                return Err(ResourceValidationError::new(
                    "branch decision decision_tick must be non-negative",
                ));
            }
            let key = (decision.decision_tick, decision.segment_id);
            if previous_key.is_some_and(|previous| previous >= key) {
                return Err(ResourceValidationError::new(
                    "branch decisions must be strictly ordered by tick and segment_id",
                ));
            }
            previous_key = Some(key);
            if let Some(hint) = &decision.hint {
                validate_branch_hint(hint)?;
            }
        }
        Ok(())
    }
}

impl ResourceValidationError {
    fn with_context(self, context: impl AsRef<str>) -> Self {
        Self::new(format!("{}: {}", context.as_ref(), self.message))
    }
}

pub fn validate_sha256(value: &str) -> Result<(), ResourceValidationError> {
    if value.len() != 64 {
        return Err(ResourceValidationError::new(format!(
            "SHA-256 digest must contain 64 lowercase hex characters, got {}",
            value.len()
        )));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ResourceValidationError::new(
            "SHA-256 digest must contain only lowercase hex characters",
        ));
    }
    Ok(())
}

fn validate_text(
    value: &str,
    max_bytes: usize,
    allow_empty: bool,
) -> Result<(), ResourceValidationError> {
    if !allow_empty && value.trim().is_empty() {
        return Err(ResourceValidationError::new("value cannot be empty"));
    }
    if value.len() > max_bytes {
        return Err(ResourceValidationError::new(format!(
            "value exceeds {max_bytes} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(ResourceValidationError::new(
            "value cannot contain control characters",
        ));
    }
    Ok(())
}

fn validate_required_presentation_text(
    value: &str,
    max_bytes: usize,
) -> Result<(), ResourceValidationError> {
    validate_text(value, max_bytes, false)?;
    if value.trim() != value {
        return Err(ResourceValidationError::new(
            "value cannot start or end with whitespace",
        ));
    }
    Ok(())
}

fn validate_branch_hint(hint: &BranchDecisionHint) -> Result<(), ResourceValidationError> {
    match hint {
        BranchDecisionHint::Accuracy { low, high } | BranchDecisionHint::Roll { low, high } => {
            if low > high {
                return Err(ResourceValidationError::new(
                    "branch hint thresholds must satisfy low <= high",
                ));
            }
        }
        BranchDecisionHint::Score { low, high } => {
            if low > high {
                return Err(ResourceValidationError::new(
                    "branch hint thresholds must satisfy low <= high",
                ));
            }
        }
        BranchDecisionHint::Raw(raw) => {
            validate_text(raw, MAX_BRANCH_HINT_BYTES, false)
                .map_err(|error| error.with_context("raw branch hint"))?;
        }
    }
    Ok(())
}

fn validate_resource_path(value: &str) -> Result<(), ResourceValidationError> {
    validate_text(value, MAX_RESOURCE_PATH_BYTES, false)?;
    if value.starts_with('/') || value.starts_with('\\') {
        return Err(ResourceValidationError::new(
            "resource path must be relative",
        ));
    }
    if value.contains('\\') {
        return Err(ResourceValidationError::new(
            "resource path must use forward slashes",
        ));
    }
    let first_component = value.split('/').next().unwrap_or_default().as_bytes();
    if first_component.len() >= 2
        && first_component[0].is_ascii_alphabetic()
        && first_component[1] == b':'
    {
        return Err(ResourceValidationError::new(
            "resource path cannot contain a Windows drive prefix",
        ));
    }
    if value
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(ResourceValidationError::new(
            "resource path must be normalized and cannot contain traversal components",
        ));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum CanonicalChartHashError {
    #[error("canonical chart validation failed: {0}")]
    Validation(#[from] rhythm_chart::ValidationError),
    #[error("canonical chart serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Computes the v1 canonical-chart identity shared by the resource server and client.
///
/// Validation first proves that every collection has canonical ordering and
/// identity constraints. The compact JSON representation is then deterministic
/// for this schema version.
pub fn canonical_chart_sha256(
    chart: &rhythm_chart::CanonicalChart,
) -> Result<String, CanonicalChartHashError> {
    chart.validate()?;
    let encoded = serde_json::to_vec(chart)?;
    let mut hasher = Sha256::new();
    hasher.update(CANONICAL_CHART_HASH_DOMAIN);
    hasher.update(CANONICAL_SCHEMA_SHA256.as_bytes());
    hasher.update([0]);
    hasher.update(encoded);
    Ok(hex::encode(hasher.finalize()))
}

/// Computes the identity of immutable inputs and semantics for one playable song.
///
/// Display metadata and storage paths are deliberately excluded. Course order is
/// significant and course indices must be dense.
pub fn song_manifest_sha256(
    source_id: &str,
    audio_id: Option<&str>,
    semantics: &ResourceSemantics,
    courses: &[ResourceCourse],
) -> Result<String, ResourceValidationError> {
    if courses.is_empty() {
        return Err(ResourceValidationError::new(
            "song manifest must contain at least one course",
        ));
    }
    if courses.len() > MAX_COURSES_PER_SONG {
        return Err(ResourceValidationError::new(format!(
            "song manifest has {} courses; maximum is {MAX_COURSES_PER_SONG}",
            courses.len()
        )));
    }
    let course_count = u32::try_from(courses.len())
        .map_err(|_| ResourceValidationError::new("course count exceeds u32"))?;

    for (position, course) in courses.iter().enumerate() {
        let expected_index = u32::try_from(position)
            .map_err(|_| ResourceValidationError::new("course position exceeds u32"))?;
        if course.index != expected_index {
            return Err(ResourceValidationError::new(format!(
                "course indices must be dense and ordered; expected {expected_index}, got {}",
                course.index
            )));
        }
        course
            .validate()
            .map_err(|error| error.with_context(format!("course {position}")))?;
        validate_sha256(&course.canonical_chart_hash)
            .map_err(|error| error.with_context(format!("course {position} canonical hash")))?;
    }

    song_content_identity_sha256(
        source_id,
        audio_id,
        semantics,
        courses
            .iter()
            .map(|course| course.canonical_chart_hash.as_str()),
        course_count,
    )
}

/// Computes the shared resource/multiplayer identity from immutable content.
///
/// Callers must provide the dense course count they validated for their own
/// representation. Keeping this primitive in the resource protocol prevents a
/// multiplayer manifest from drifting to a second hashing algorithm.
pub fn song_content_identity_sha256<'a>(
    source_id: &str,
    audio_id: Option<&str>,
    semantics: &ResourceSemantics,
    canonical_course_hashes: impl IntoIterator<Item = &'a str>,
    course_count: u32,
) -> Result<String, ResourceValidationError> {
    validate_sha256(source_id).map_err(|error| error.with_context("source_id"))?;
    if let Some(audio_id) = audio_id {
        validate_sha256(audio_id).map_err(|error| error.with_context("audio_id"))?;
    }
    semantics.validate()?;
    if course_count == 0
        || usize::try_from(course_count).unwrap_or(usize::MAX) > MAX_COURSES_PER_SONG
    {
        return Err(ResourceValidationError::new(format!(
            "song manifest course count {course_count} is outside 1..={MAX_COURSES_PER_SONG}"
        )));
    }

    let mut hasher = Sha256::new();
    hasher.update(SONG_MANIFEST_HASH_DOMAIN);
    hasher.update(source_id.as_bytes());
    hasher.update([u8::from(audio_id.is_some())]);
    if let Some(audio_id) = audio_id {
        hasher.update(audio_id.as_bytes());
    }
    hasher.update(semantics.canonical_schema_version.to_be_bytes());
    hasher.update(semantics.canonical_schema_sha256.as_bytes());
    hasher.update(semantics.importer_semantics_version.to_be_bytes());
    hasher.update(semantics.importer_semantics_sha256.as_bytes());
    hasher.update(semantics.taiko_ruleset_version.to_be_bytes());
    hasher.update(semantics.taiko_ruleset_sha256.as_bytes());
    hasher.update(semantics.audio_decoder_semantics_version.to_be_bytes());
    hasher.update(semantics.audio_decoder_semantics_sha256.as_bytes());
    hasher.update(course_count.to_be_bytes());

    let mut actual_count = 0_u32;
    for (position, canonical_hash) in canonical_course_hashes.into_iter().enumerate() {
        validate_sha256(canonical_hash).map_err(|error| {
            error.with_context(format!("course {position} canonical chart hash"))
        })?;
        actual_count = actual_count
            .checked_add(1)
            .ok_or_else(|| ResourceValidationError::new("course count exceeds u32"))?;
        if actual_count > course_count {
            return Err(ResourceValidationError::new(
                "canonical course hash count exceeds declared course count",
            ));
        }
        hasher.update(canonical_hash.as_bytes());
    }
    if actual_count != course_count {
        return Err(ResourceValidationError::new(format!(
            "canonical course hash count {actual_count} does not match declared count {course_count}"
        )));
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    type DocumentMutation = fn(&mut ResourceLibraryDocument);

    fn deserialize_two<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserialize_bounded_vec::<D, u8, 2>(deserializer, "items")
    }

    #[derive(Debug, Deserialize)]
    struct TwoItems {
        #[serde(deserialize_with = "deserialize_two")]
        items: Vec<u8>,
    }

    fn semantics() -> ResourceSemantics {
        ResourceSemantics {
            canonical_schema_version: 1,
            canonical_schema_sha256: "1".repeat(64),
            importer_semantics_version: 1,
            importer_semantics_sha256: "2".repeat(64),
            taiko_ruleset_version: 1,
            taiko_ruleset_sha256: "3".repeat(64),
            audio_decoder_semantics_version: 1,
            audio_decoder_semantics_sha256: "4".repeat(64),
        }
    }

    fn valid_document() -> ResourceLibraryDocument {
        let semantics = semantics();
        let courses = vec![ResourceCourse {
            index: 0,
            name: "Oni".to_owned(),
            level: Some(10),
            canonical_chart_hash: "c".repeat(64),
            object_count: 1,
            branch_segment_count: 0,
            base_bpm: Some(120.0),
            branch_decisions: vec![],
        }];
        let source_id = "a".repeat(64);
        let audio_id = Some("b".repeat(64));
        let song_id = song_manifest_sha256(&source_id, audio_id.as_deref(), &semantics, &courses)
            .expect("song id");
        ResourceLibraryDocument {
            api_version: API_VERSION,
            wire_schema_sha256: WIRE_SCHEMA_SHA256.to_owned(),
            semantics,
            songs: vec![ResourceSong {
                song_id,
                source_path: "pack/song.tja".to_owned(),
                source_id,
                audio_path: Some("pack/song.ogg".to_owned()),
                audio_id,
                title: "Song".to_owned(),
                subtitle: String::new(),
                artist: String::new(),
                demo_start_seconds: 0.0,
                courses,
            }],
            warnings: vec![],
        }
    }

    #[test]
    fn previous_v1_library_shape_is_rejected() {
        let old = r#"{"api_version":1,"songs":[],"warnings":[]}"#;
        assert!(serde_json::from_str::<ResourceLibraryDocument>(old).is_err());

        let mut without_audio_semantics =
            serde_json::to_value(valid_document()).expect("encode fixture");
        without_audio_semantics["semantics"]
            .as_object_mut()
            .expect("semantics object")
            .remove("audio_decoder_semantics_version");
        without_audio_semantics["semantics"]
            .as_object_mut()
            .expect("semantics object")
            .remove("audio_decoder_semantics_sha256");
        assert!(
            serde_json::from_value::<ResourceLibraryDocument>(without_audio_semantics).is_err()
        );

        let mut without_song_id = serde_json::to_value(valid_document()).expect("encode fixture");
        without_song_id["songs"][0]
            .as_object_mut()
            .expect("song object")
            .remove("song_id");
        assert!(serde_json::from_value::<ResourceLibraryDocument>(without_song_id).is_err());

        for field in ["audio_path", "audio_id"] {
            let mut missing_audio_field =
                serde_json::to_value(valid_document()).expect("encode fixture");
            missing_audio_field["songs"][0]
                .as_object_mut()
                .expect("song object")
                .remove(field);
            assert!(
                serde_json::from_value::<ResourceLibraryDocument>(missing_audio_field).is_err(),
                "{field} must remain required even though it is nullable"
            );
        }

        let mut duplicate_content_identity =
            serde_json::to_value(valid_document()).expect("encode fixture");
        duplicate_content_identity["songs"][0]["chart_content_hash"] =
            duplicate_content_identity["songs"][0]["source_id"].clone();
        assert!(
            serde_json::from_value::<ResourceLibraryDocument>(duplicate_content_identity).is_err(),
            "strict v1 must reject the removed duplicate hash field"
        );
    }

    #[test]
    fn collection_limits_fail_during_deserialization_before_unbounded_growth() {
        let valid = serde_json::from_str::<TwoItems>(r#"{"items":[1,2]}"#)
            .expect("two elements fit the decoding bound");
        assert_eq!(valid.items, [1, 2]);

        let error = serde_json::from_str::<TwoItems>(r#"{"items":[1,2,3]}"#)
            .expect_err("third element must be rejected by the sequence visitor");
        assert!(error.to_string().contains("items exceeds maximum"));

        let mut oversized_warnings =
            serde_json::to_value(valid_document()).expect("encode fixture");
        oversized_warnings["warnings"] = serde_json::Value::Array(
            (0..=MAX_WARNINGS_PER_LIBRARY)
                .map(|_| serde_json::Value::String(String::new()))
                .collect(),
        );
        let error = serde_json::from_value::<ResourceLibraryDocument>(oversized_warnings)
            .expect_err("warning bound must apply while decoding");
        assert!(error.to_string().contains("warnings exceeds maximum"));

        let mut oversized_courses = serde_json::to_value(valid_document()).expect("encode fixture");
        let course = oversized_courses["songs"][0]["courses"][0].clone();
        oversized_courses["songs"][0]["courses"] =
            serde_json::Value::Array(vec![course; MAX_COURSES_PER_SONG + 1]);
        let error = serde_json::from_value::<ResourceLibraryDocument>(oversized_courses)
            .expect_err("course bound must apply while decoding");
        assert!(error.to_string().contains("courses exceeds maximum"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = format!(
            r#"{{"api_version":1,"wire_schema_sha256":"{WIRE_SCHEMA_SHA256}","semantics":{{"canonical_schema_version":1,"canonical_schema_sha256":"a","importer_semantics_version":1,"importer_semantics_sha256":"b","taiko_ruleset_version":1,"taiko_ruleset_sha256":"c","audio_decoder_semantics_version":1,"audio_decoder_semantics_sha256":"d"}},"songs":[],"warnings":[],"legacy":true}}"#
        );
        assert!(serde_json::from_str::<ResourceLibraryDocument>(&raw).is_err());

        let mut unknown_semantics = serde_json::to_value(valid_document()).expect("encode fixture");
        unknown_semantics["semantics"]["legacy"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<ResourceLibraryDocument>(unknown_semantics).is_err());
    }

    #[test]
    fn valid_document_satisfies_all_v1_invariants() {
        valid_document().validate().expect("valid document");
    }

    #[test]
    fn audio_absence_is_explicit_and_part_of_song_identity() {
        let mut silent = valid_document();
        silent.songs[0].audio_path = None;
        silent.songs[0].audio_id = None;
        silent.songs[0].song_id = song_manifest_sha256(
            &silent.songs[0].source_id,
            None,
            &silent.semantics,
            &silent.songs[0].courses,
        )
        .expect("silent song id");
        silent.validate().expect("explicitly silent song");
        assert_ne!(silent.songs[0].song_id, valid_document().songs[0].song_id);

        let mut path_only = silent.clone();
        path_only.songs[0].audio_path = Some("pack/song.ogg".to_owned());
        assert!(path_only
            .validate()
            .expect_err("partial audio identity must fail")
            .to_string()
            .contains("both be present or both be null"));

        let mut id_only = silent;
        id_only.songs[0].audio_id = Some("b".repeat(64));
        assert!(id_only
            .validate()
            .expect_err("partial audio identity must fail")
            .to_string()
            .contains("both be present or both be null"));
    }

    #[test]
    fn content_ids_are_canonical_sha256_values() {
        let mut document = valid_document();
        document.songs[0].source_id = "not-a-digest".to_owned();
        let error = document.validate().expect_err("invalid content identity");
        assert!(error.to_string().contains("source_id"));
    }

    #[test]
    fn paths_must_be_normalized_relative_paths() {
        for path in [
            "../song.tja",
            "/song.tja",
            "pack//song.tja",
            r"pack\song.tja",
            "C:/song.tja",
        ] {
            let mut document = valid_document();
            document.songs[0].source_path = path.to_owned();
            assert!(
                document.validate().is_err(),
                "unexpectedly accepted path {path:?}"
            );
        }
    }

    #[test]
    fn branch_summary_invariants_are_enforced() {
        let mut incomplete = valid_document();
        incomplete.songs[0].courses[0].branch_segment_count = 1;
        let error = incomplete
            .validate()
            .expect_err("every branch segment requires one decision");
        assert!(error
            .to_string()
            .contains("must equal branch_segment_count"));

        let mut document = valid_document();
        document.songs[0].courses[0].branch_segment_count = 1;
        document.songs[0].courses[0]
            .branch_decisions
            .push(ResourceBranchDecisionPoint {
                segment_id: 1,
                decision_tick: 0,
                default_route_id: 0,
                route_count: 0,
                hint: None,
            });
        let error = document.validate().expect_err("zero route count");
        assert!(error.to_string().contains("route_count must be non-zero"));

        let mut invalid_default = valid_document();
        invalid_default.songs[0].courses[0].branch_segment_count = 1;
        invalid_default.songs[0].courses[0]
            .branch_decisions
            .push(ResourceBranchDecisionPoint {
                segment_id: 1,
                decision_tick: 0,
                default_route_id: 2,
                route_count: 2,
                hint: None,
            });
        let error = invalid_default
            .validate()
            .expect_err("default route outside route count");
        assert!(error
            .to_string()
            .contains("default_route_id must be smaller"));

        let mut duplicate_segment = valid_document();
        duplicate_segment.songs[0].courses[0].branch_segment_count = 2;
        duplicate_segment.songs[0].courses[0].branch_decisions = vec![
            ResourceBranchDecisionPoint {
                segment_id: 7,
                decision_tick: 0,
                default_route_id: 0,
                route_count: 3,
                hint: None,
            },
            ResourceBranchDecisionPoint {
                segment_id: 7,
                decision_tick: 1,
                default_route_id: 0,
                route_count: 3,
                hint: None,
            },
        ];
        let error = duplicate_segment
            .validate()
            .expect_err("duplicate segment id");
        assert!(error
            .to_string()
            .contains("segment_id values must be unique"));

        let mut negative_tick = valid_document();
        negative_tick.songs[0].courses[0].branch_segment_count = 1;
        negative_tick.songs[0].courses[0]
            .branch_decisions
            .push(ResourceBranchDecisionPoint {
                segment_id: 1,
                decision_tick: -1,
                default_route_id: 0,
                route_count: 3,
                hint: None,
            });
        let error = negative_tick
            .validate()
            .expect_err("negative branch decision tick");
        assert!(error
            .to_string()
            .contains("decision_tick must be non-negative"));
    }

    #[test]
    fn library_song_id_and_source_path_must_be_unique() {
        let mut duplicate_id = valid_document();
        duplicate_id.songs.push(duplicate_id.songs[0].clone());
        let error = duplicate_id.validate().expect_err("duplicate song id");
        assert!(error.to_string().contains("duplicate song_id"));

        let mut duplicate_path = valid_document();
        let mut second = duplicate_path.songs[0].clone();
        second.source_id = "d".repeat(64);
        second.song_id = song_manifest_sha256(
            &second.source_id,
            second.audio_id.as_deref(),
            &duplicate_path.semantics,
            &second.courses,
        )
        .expect("second song identity");
        duplicate_path.songs.push(second);
        let error = duplicate_path
            .validate()
            .expect_err("duplicate source path");
        assert!(error.to_string().contains("duplicate source_path"));
    }

    #[test]
    fn sha256_digests_are_canonical_lowercase() {
        assert!(validate_sha256(&"a".repeat(64)).is_ok());
        assert!(validate_sha256(&"A".repeat(64)).is_err());
        assert!(validate_sha256(&"g".repeat(64)).is_err());
        assert!(validate_sha256(&"a".repeat(63)).is_err());
    }

    #[test]
    fn audio_decoder_semantics_must_be_explicit_and_canonical() {
        let mut zero_version = semantics();
        zero_version.audio_decoder_semantics_version = 0;
        let error = zero_version.validate().expect_err("zero semantic version");
        assert!(error.to_string().contains("versions must be non-zero"));

        let mut uppercase_digest = semantics();
        uppercase_digest.audio_decoder_semantics_sha256 = "A".repeat(64);
        let error = uppercase_digest
            .validate()
            .expect_err("uppercase semantic digest");
        assert!(error
            .to_string()
            .contains("audio decoder semantics fingerprint"));
        assert!(error.to_string().contains("lowercase hex"));
    }

    #[test]
    fn presentation_text_uses_the_multiplayer_accepted_set() {
        let mut exact = valid_document();
        exact.songs[0].title = "t".repeat(MAX_TITLE_BYTES);
        exact.songs[0].subtitle = "s".repeat(MAX_SUBTITLE_BYTES);
        exact.songs[0].artist = "a".repeat(MAX_ARTIST_BYTES);
        exact.songs[0].courses[0].name = "c".repeat(MAX_COURSE_NAME_BYTES);
        exact
            .validate()
            .expect("exact shared presentation limits are valid");

        let oversized_cases: [(&str, DocumentMutation); 4] = [
            ("title", |document: &mut ResourceLibraryDocument| {
                document.songs[0].title = "t".repeat(MAX_TITLE_BYTES + 1);
            }),
            ("subtitle", |document: &mut ResourceLibraryDocument| {
                document.songs[0].subtitle = "s".repeat(MAX_SUBTITLE_BYTES + 1);
            }),
            ("artist", |document: &mut ResourceLibraryDocument| {
                document.songs[0].artist = "a".repeat(MAX_ARTIST_BYTES + 1);
            }),
            ("course name", |document: &mut ResourceLibraryDocument| {
                document.songs[0].courses[0].name = "c".repeat(MAX_COURSE_NAME_BYTES + 1);
            }),
        ];
        for (field, mutate) in oversized_cases {
            let mut oversized = valid_document();
            mutate(&mut oversized);
            assert!(
                oversized.validate().is_err(),
                "{field} above the shared limit must be rejected"
            );
        }

        let whitespace_cases: [(&str, DocumentMutation); 2] = [
            ("title", |document: &mut ResourceLibraryDocument| {
                document.songs[0].title = " padded".to_owned();
            }),
            ("course name", |document: &mut ResourceLibraryDocument| {
                document.songs[0].courses[0].name = "padded ".to_owned();
            }),
        ];
        for (field, mutate) in whitespace_cases {
            let mut padded = valid_document();
            mutate(&mut padded);
            assert!(
                padded.validate().is_err(),
                "{field} with surrounding whitespace must be rejected"
            );
        }

        let mut optional_whitespace = valid_document();
        optional_whitespace.songs[0].subtitle = " optional ".to_owned();
        optional_whitespace.songs[0].artist = " artist ".to_owned();
        optional_whitespace
            .validate()
            .expect("optional presentation text has the same whitespace policy as BoundedText");
    }

    #[test]
    fn wire_schema_fingerprint_is_pinned_to_v1_contract() {
        const PINNED_SCHEMA_SHA256: &str =
            "f55ecfdf15bb0ccf8fe2a91c5e9925df3f146980a6ec8247279f8fe8cb86798f";

        assert_eq!(API_VERSION, 1);
        assert_eq!(
            hex::encode(Sha256::digest(WIRE_SCHEMA_DESCRIPTOR.as_bytes())),
            PINNED_SCHEMA_SHA256
        );
        assert_eq!(WIRE_SCHEMA_SHA256, PINNED_SCHEMA_SHA256);
        assert!(validate_sha256(WIRE_SCHEMA_SHA256).is_ok());
    }

    #[test]
    fn song_id_excludes_display_metadata_but_commits_to_canonical_courses() {
        let mut document = valid_document();
        assert_eq!(
            document.songs[0].song_id,
            "dbe60a121777d40fbddc35d32d3b41959407363a43f0220c2cbe41f600137c18"
        );
        document.songs[0].title = "Localized title".to_owned();
        document
            .validate()
            .expect("display metadata does not change identity");

        document.songs[0].courses[0].canonical_chart_hash = "d".repeat(64);
        let error = document.validate().expect_err("stale song identity");
        assert!(error.to_string().contains("song_id does not match"));

        let mut semantic_change = valid_document();
        semantic_change.semantics.taiko_ruleset_version += 1;
        let error = semantic_change
            .validate()
            .expect_err("song identity commits to ruleset");
        assert!(error.to_string().contains("song_id does not match"));

        let mut audio_version_change = valid_document();
        audio_version_change
            .semantics
            .audio_decoder_semantics_version += 1;
        let error = audio_version_change
            .validate()
            .expect_err("song identity commits to audio decoder semantics version");
        assert!(error.to_string().contains("song_id does not match"));

        let mut audio_digest_change = valid_document();
        audio_digest_change.semantics.audio_decoder_semantics_sha256 = "5".repeat(64);
        let error = audio_digest_change
            .validate()
            .expect_err("song identity commits to audio decoder semantics digest");
        assert!(error.to_string().contains("song_id does not match"));
    }

    #[test]
    fn canonical_chart_hash_is_schema_bound_and_stable() {
        let chart = rhythm_chart::CanonicalChart {
            tempo_map: vec![rhythm_chart::TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            ..rhythm_chart::CanonicalChart::default()
        };
        assert_eq!(
            canonical_chart_sha256(&chart).expect("hash"),
            "fb5b9cd675031029fa857575ee5f0fa8cc6f49684eb74b407657aea79599ebd2"
        );

        let mut changed = chart.clone();
        changed.metadata.title = "changed".to_owned();
        assert_ne!(
            canonical_chart_sha256(&changed).expect("changed hash"),
            canonical_chart_sha256(&chart).expect("base hash")
        );

        assert!(
            matches!(
                canonical_chart_sha256(&rhythm_chart::CanonicalChart::default()),
                Err(CanonicalChartHashError::Validation(
                    rhythm_chart::ValidationError::EmptyTempoMap
                ))
            ),
            "invalid charts must never receive canonical content identities"
        );
    }
}
