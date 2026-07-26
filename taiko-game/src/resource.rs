use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use directories::ProjectDirs;
use reqwest::{Client, Response, StatusCode, Url};
use rhythm_chart::{CanonicalChart, CANONICAL_SCHEMA_SHA256, CANONICAL_SCHEMA_VERSION};
use rhythm_importer_tja::{
    build_branch_decision_table, BranchDecisionPoint, ImportedSong, TjaImporter,
    TJA_IMPORTER_SEMANTICS_SHA256, TJA_IMPORTER_SEMANTICS_VERSION,
};
use rhythm_mode_taiko::{TAIKO_RULESET_SHA256, TAIKO_RULESET_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use taiko_resource_protocol::{
    validate_sha256, ResourceLibraryDocument, ResourceSemantics, MAX_AUDIO_RESPONSE_BYTES,
    MAX_CHART_RESPONSE_BYTES, MAX_LIBRARY_RESPONSE_BYTES,
};
use tokio::runtime::Runtime;
use walkdir::WalkDir;

use crate::cli::CliArgs;
use crate::loader::{
    canonical_chart_hash, load_song_library as load_local_song_library, read_chart_file_bounded,
    CourseEntry, RemoteSongIdentity, SongEntry, SongLibrary, SongOrigin,
};

const CACHE_INDEX_VERSION: u32 = 3;
const CACHE_LOCK_FILE_NAME: &str = ".index-v3.lock";
const MAX_CACHE_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEMORY_CACHE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_DISK_CACHE_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_DISK_CACHE_MAX_ENTRIES: usize = 8_192;
const MAX_DISK_CACHE_INDEX_ENTRIES: usize = 32_768;
const DISK_CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const DISK_CACHE_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);
const RESOURCE_TRANSPORT_MAX_ATTEMPTS: usize = 3;

// The server enforces 10s + ceil(content_length / 1 MiB/s), capped at five minutes.
// The client mirrors that transfer model and allows five seconds for transport jitter.
const RESOURCE_SERVER_STREAM_BASE_DEADLINE: Duration = Duration::from_secs(10);
const RESOURCE_SERVER_STREAM_MIN_BYTES_PER_SECOND: u64 = 1024 * 1024;
const RESOURCE_SERVER_STREAM_MAX_DEADLINE: Duration = Duration::from_secs(5 * 60);
const RESOURCE_STREAM_DEADLINE_TOLERANCE: Duration = Duration::from_secs(5);
const RESOURCE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
// The server's size-aware verification deadline is capped at five minutes.
const RESOURCE_RESPONSE_HEADER_TIMEOUT: Duration = Duration::from_secs(5 * 60 + 5);
const RESOURCE_CHUNK_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const RESOURCE_CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const RESOURCE_BODY_INITIAL_CAPACITY_MAX: usize = 1024 * 1024;
const RESOURCE_TRANSPORT_RETRY_DELAYS: [Duration; RESOURCE_TRANSPORT_MAX_ATTEMPTS - 1] =
    [Duration::from_secs(1), Duration::from_secs(2)];
const RESOURCE_BUSY_RETRY_BUDGET: Duration = Duration::from_secs(5 * 60 + 15);
const RESOURCE_BUSY_MAX_ATTEMPTS: usize = 16;
const RESOURCE_BUSY_RETRY_DELAYS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];
const RESOURCE_BUSY_MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
const RESOURCE_BUSY_JITTER_MAX_BASIS_POINTS: u16 = 2_500;

#[derive(Debug, Clone, Copy)]
struct ResourceTransportPolicy {
    connect_timeout: Duration,
    response_header_timeout: Duration,
    chunk_idle_timeout: Duration,
    server_stream_base_deadline: Duration,
    server_stream_min_bytes_per_second: u64,
    server_stream_max_deadline: Duration,
    stream_deadline_tolerance: Duration,
    cancel_poll_interval: Duration,
    transport_retry_delays: [Duration; RESOURCE_TRANSPORT_MAX_ATTEMPTS - 1],
    busy_retry_budget: Duration,
    busy_max_attempts: usize,
    busy_retry_delays: [Duration; 6],
    busy_max_retry_after: Duration,
    busy_jitter_seed: u64,
    busy_jitter_max_basis_points: u16,
}

#[derive(Debug, Clone, Copy)]
struct DiskCacheLockPolicy {
    timeout: Duration,
    poll_interval: Duration,
}

impl DiskCacheLockPolicy {
    const PRODUCTION: Self = Self {
        timeout: DISK_CACHE_LOCK_TIMEOUT,
        poll_interval: DISK_CACHE_LOCK_POLL_INTERVAL,
    };

    fn start(self) -> Result<DiskCacheLockWait> {
        if self.timeout.is_zero() || self.poll_interval.is_zero() {
            bail!("disk cache lock timeout and poll interval must be positive");
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(|| anyhow!("disk cache lock deadline overflow"))?;
        Ok(DiskCacheLockWait {
            deadline,
            poll_interval: self.poll_interval,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct DiskCacheLockWait {
    deadline: Instant,
    poll_interval: Duration,
}

impl DiskCacheLockWait {
    fn pause_before_retry(self) -> bool {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(self.poll_interval.min(remaining));
        true
    }
}

impl Default for ResourceTransportPolicy {
    fn default() -> Self {
        Self {
            connect_timeout: RESOURCE_CONNECT_TIMEOUT,
            response_header_timeout: RESOURCE_RESPONSE_HEADER_TIMEOUT,
            chunk_idle_timeout: RESOURCE_CHUNK_IDLE_TIMEOUT,
            server_stream_base_deadline: RESOURCE_SERVER_STREAM_BASE_DEADLINE,
            server_stream_min_bytes_per_second: RESOURCE_SERVER_STREAM_MIN_BYTES_PER_SECOND,
            server_stream_max_deadline: RESOURCE_SERVER_STREAM_MAX_DEADLINE,
            stream_deadline_tolerance: RESOURCE_STREAM_DEADLINE_TOLERANCE,
            cancel_poll_interval: RESOURCE_CANCEL_POLL_INTERVAL,
            transport_retry_delays: RESOURCE_TRANSPORT_RETRY_DELAYS,
            busy_retry_budget: RESOURCE_BUSY_RETRY_BUDGET,
            busy_max_attempts: RESOURCE_BUSY_MAX_ATTEMPTS,
            busy_retry_delays: RESOURCE_BUSY_RETRY_DELAYS,
            busy_max_retry_after: RESOURCE_BUSY_MAX_RETRY_AFTER,
            busy_jitter_seed: next_busy_jitter_seed(),
            busy_jitter_max_basis_points: RESOURCE_BUSY_JITTER_MAX_BASIS_POINTS,
        }
    }
}

fn next_busy_jitter_seed() -> u64 {
    static NEXT_SEED: AtomicU64 = AtomicU64::new(1);
    let sequence = NEXT_SEED.fetch_add(1, Ordering::Relaxed);
    splitmix64((u64::from(std::process::id()) << 32) ^ sequence)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn busy_retry_delay(
    policy: ResourceTransportPolicy,
    busy_response_number: usize,
    retry_after: Option<Duration>,
) -> Duration {
    let delay_index = busy_response_number
        .saturating_sub(1)
        .min(policy.busy_retry_delays.len() - 1);
    let minimum = policy.busy_retry_delays[delay_index].max(
        retry_after
            .unwrap_or(Duration::ZERO)
            .min(policy.busy_max_retry_after),
    );
    let response_ordinal = u64::try_from(busy_response_number)
        .expect("busy response count is bounded by the admission attempt limit");
    let mixed =
        splitmix64(policy.busy_jitter_seed ^ response_ordinal.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let jitter_basis_points =
        mixed % (u64::from(policy.busy_jitter_max_basis_points).saturating_add(1));
    let jitter_nanos = minimum
        .as_nanos()
        .saturating_mul(u128::from(jitter_basis_points))
        / 10_000;
    let jitter_nanos =
        u64::try_from(jitter_nanos).expect("bounded 30-second admission jitter fits in u64 nanos");
    minimum.saturating_add(Duration::from_nanos(jitter_nanos))
}

#[derive(Debug)]
pub struct ResourceLoadCancelled;

impl std::fmt::Display for ResourceLoadCancelled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("resource load cancelled")
    }
}

impl std::error::Error for ResourceLoadCancelled {}

pub fn is_resource_load_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ResourceLoadCancelled>().is_some()
}

fn never_cancelled() -> bool {
    false
}

fn cancellation_checkpoint(is_cancelled: &dyn Fn() -> bool) -> Result<()> {
    if is_cancelled() {
        return Err(ResourceLoadCancelled.into());
    }
    Ok(())
}

enum ResourceDownloadAttempt<T> {
    Success(T),
    PermanentFailure(anyhow::Error),
    TransientFailure(anyhow::Error),
    HttpFailure {
        status: reqwest::StatusCode,
        error: anyhow::Error,
        retry_after: Option<Duration>,
    },
}

fn retry_after_delay(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn is_retryable_reqwest_error(error: &reqwest::Error) -> bool {
    // With compression disabled and `Accept-Encoding: identity`, reqwest reports HTTP framing
    // failures such as a premature Content-Length EOF as decode errors. Those are transport
    // failures; there is no content codec here whose deterministic rejection should be retried.
    error.is_connect() || error.is_timeout() || error.is_body() || error.is_decode()
}

fn resource_stream_deadline(
    content_length: Option<u64>,
    max_bytes: u64,
    policy: ResourceTransportPolicy,
) -> Duration {
    assert!(
        policy.server_stream_min_bytes_per_second > 0,
        "resource transport rate invariant must be positive"
    );
    let deadline_bytes = content_length.unwrap_or(max_bytes);
    let transfer_seconds = deadline_bytes
        .saturating_add(policy.server_stream_min_bytes_per_second - 1)
        / policy.server_stream_min_bytes_per_second;
    policy
        .server_stream_base_deadline
        .saturating_add(Duration::from_secs(transfer_seconds))
        .min(policy.server_stream_max_deadline)
        .saturating_add(policy.stream_deadline_tolerance)
}

async fn sleep_with_cancellation(
    duration: Duration,
    is_cancelled: &dyn Fn() -> bool,
    poll_interval: Duration,
) -> Result<()> {
    cancellation_checkpoint(is_cancelled)?;
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                cancellation_checkpoint(is_cancelled)?;
                return Ok(());
            }
            _ = tokio::time::sleep(poll_interval) => {
                cancellation_checkpoint(is_cancelled)?;
            }
        }
    }
}

fn server_busy_error(attempts: usize, reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("resource server remained busy: {reason} after {attempts} admission attempts")
}

async fn sleep_with_busy_budget(
    delay: Duration,
    busy_started: tokio::time::Instant,
    attempts: usize,
    is_cancelled: &dyn Fn() -> bool,
    policy: ResourceTransportPolicy,
) -> Result<()> {
    let busy_deadline = busy_started + policy.busy_retry_budget;
    let remaining = busy_deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(server_busy_error(
            attempts,
            format_args!(
                "exhausted the {:?} admission retry budget",
                policy.busy_retry_budget
            ),
        ));
    }
    if delay >= remaining {
        sleep_with_cancellation(remaining, is_cancelled, policy.cancel_poll_interval).await?;
        return Err(server_busy_error(
            attempts,
            format_args!(
                "exhausted the {:?} admission retry budget",
                policy.busy_retry_budget
            ),
        ));
    }
    sleep_with_cancellation(delay, is_cancelled, policy.cancel_poll_interval).await
}

async fn read_response_body_attempt(
    mut response: Response,
    max_bytes: u64,
    url: &Url,
    is_cancelled: &dyn Fn() -> bool,
    policy: ResourceTransportPolicy,
) -> ResourceDownloadAttempt<Arc<[u8]>> {
    if let Err(error) = cancellation_checkpoint(is_cancelled) {
        return ResourceDownloadAttempt::PermanentFailure(error);
    }
    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > max_bytes) {
        return ResourceDownloadAttempt::PermanentFailure(anyhow!(
            "response from {url} exceeds {max_bytes} bytes"
        ));
    }

    let capacity = content_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(RESOURCE_BODY_INITIAL_CAPACITY_MAX);
    let mut body = Vec::with_capacity(capacity);
    let absolute_deadline =
        tokio::time::sleep(resource_stream_deadline(content_length, max_bytes, policy));
    tokio::pin!(absolute_deadline);

    loop {
        let next_chunk = response.chunk();
        tokio::pin!(next_chunk);
        let idle_deadline = tokio::time::sleep(policy.chunk_idle_timeout);
        tokio::pin!(idle_deadline);

        let chunk = loop {
            tokio::select! {
                result = &mut next_chunk => break result,
                _ = &mut idle_deadline => {
                    return ResourceDownloadAttempt::TransientFailure(anyhow!(
                            "response body from {url} was idle for {:?}",
                            policy.chunk_idle_timeout
                        ));
                }
                _ = &mut absolute_deadline => {
                    return ResourceDownloadAttempt::TransientFailure(anyhow!(
                            "response body from {url} exceeded its size-aware transfer deadline"
                        ));
                }
                _ = tokio::time::sleep(policy.cancel_poll_interval) => {
                    if let Err(error) = cancellation_checkpoint(is_cancelled) {
                        return ResourceDownloadAttempt::PermanentFailure(error);
                    }
                }
            }
        };

        match chunk {
            Ok(Some(chunk)) => {
                let next_len = body.len().saturating_add(chunk.len());
                if u64::try_from(next_len).unwrap_or(u64::MAX) > max_bytes {
                    return ResourceDownloadAttempt::PermanentFailure(anyhow!(
                        "response from {url} exceeds {max_bytes} bytes"
                    ));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) => {
                let retryable = is_retryable_reqwest_error(&error);
                let error = anyhow::Error::new(error)
                    .context(format!("failed to read response body from {url}"));
                return if retryable {
                    ResourceDownloadAttempt::TransientFailure(error)
                } else {
                    ResourceDownloadAttempt::PermanentFailure(error)
                };
            }
        }
    }

    if content_length
        .is_some_and(|expected| u64::try_from(body.len()).unwrap_or(u64::MAX) != expected)
    {
        return ResourceDownloadAttempt::TransientFailure(anyhow!(
            "response body from {url} ended before its Content-Length"
        ));
    }
    ResourceDownloadAttempt::Success(body.into())
}

fn expected_resource_semantics() -> ResourceSemantics {
    ResourceSemantics {
        canonical_schema_version: CANONICAL_SCHEMA_VERSION,
        canonical_schema_sha256: CANONICAL_SCHEMA_SHA256.to_owned(),
        importer_semantics_version: TJA_IMPORTER_SEMANTICS_VERSION,
        importer_semantics_sha256: TJA_IMPORTER_SEMANTICS_SHA256.to_owned(),
        taiko_ruleset_version: TAIKO_RULESET_VERSION,
        taiko_ruleset_sha256: TAIKO_RULESET_SHA256.to_owned(),
        audio_decoder_semantics_version: taiko_audio::AUDIO_DECODER_SEMANTICS_VERSION,
        audio_decoder_semantics_sha256: taiko_audio::AUDIO_DECODER_SEMANTICS_SHA256.to_owned(),
    }
}

fn validate_resource_semantics(actual: &ResourceSemantics) -> Result<()> {
    let expected = expected_resource_semantics();
    if actual != &expected {
        bail!("resource semantics mismatch: server={actual:?}, client={expected:?}");
    }
    Ok(())
}

fn read_bounded_reader(reader: impl Read, max_bytes: u64) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    reader
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut body)
        .context("failed to read bounded input")?;
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > max_bytes {
        bail!("input exceeds {max_bytes} bytes");
    }
    Ok(body)
}

#[derive(Debug, Clone)]
pub enum SongAudioSource {
    FilePath(PathBuf),
    Bytes(Arc<[u8]>),
}

pub enum ResourceBackend {
    Local(LocalResourceBackend),
    Remote(Box<RemoteResourceBackend>),
}

pub struct LocalResourceBackend {
    songdir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteCacheMode {
    AppData,
    MemoryOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteResourceKind {
    Chart,
    Audio,
}

#[derive(Debug, Clone)]
struct DiskCacheLayout {
    cache_root: PathBuf,
    endpoint_hash: String,
    chart_dir: PathBuf,
    audio_dir: PathBuf,
    index_file: PathBuf,
}

#[derive(Debug, Clone, Copy)]
struct DiskCachePolicy {
    max_bytes: u64,
    max_entries: usize,
}

impl Default for DiskCachePolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_DISK_CACHE_MAX_BYTES,
            max_entries: DEFAULT_DISK_CACHE_MAX_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheIndexDocument {
    version: u32,
    access_clock: u64,
    entries: BTreeMap<String, DiskCacheEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskCacheEntry {
    size_bytes: u64,
    last_access: u64,
}

#[derive(Debug)]
struct DiskCache {
    layout: DiskCacheLayout,
    policy: DiskCachePolicy,
    index: CacheIndexDocument,
}

#[derive(Debug)]
struct MemoryCache {
    entries: HashMap<String, Arc<[u8]>>,
    insertion_order: VecDeque<String>,
    total_bytes: usize,
    max_bytes: usize,
}

pub struct RemoteResourceBackend {
    endpoint: Url,
    client: Client,
    runtime: Runtime,
    transport_policy: ResourceTransportPolicy,
    disk_cache_lock_policy: DiskCacheLockPolicy,
    memory_cache: Mutex<MemoryCache>,
    disk_cache: Option<Mutex<DiskCache>>,
}

#[derive(Debug, Clone)]
pub struct RemoteCacheSummary {
    pub endpoint_hash: String,
    pub path: PathBuf,
    pub chart_files: usize,
    pub chart_bytes: u64,
    pub audio_files: usize,
    pub audio_bytes: u64,
    pub index_entries: usize,
}

#[derive(Debug, Clone)]
pub struct RemoteCacheOverview {
    pub root: PathBuf,
    pub entries: Vec<RemoteCacheSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RemoteCacheClearResult {
    pub removed_paths: Vec<PathBuf>,
    pub missing_paths: Vec<PathBuf>,
}

impl ResourceBackend {
    pub(crate) fn local(songdir: PathBuf) -> Self {
        Self::Local(LocalResourceBackend { songdir })
    }

    pub fn from_cli(args: &CliArgs) -> Result<Self> {
        match args.resource_endpoint.as_deref() {
            None => Ok(Self::local(args.songdir.clone())),
            Some(endpoint) => {
                let cache_mode = remote_cache_mode(args.resource_cache_memory_only);
                Ok(Self::Remote(Box::new(RemoteResourceBackend::new(
                    endpoint, cache_mode,
                )?)))
            }
        }
    }

    pub fn remote(endpoint: &str, memory_only_cache: bool) -> Result<Self> {
        let cache_mode = remote_cache_mode(memory_only_cache);
        Ok(Self::Remote(Box::new(RemoteResourceBackend::new(
            endpoint, cache_mode,
        )?)))
    }

    #[cfg(test)]
    pub fn load_song_library(&self) -> Result<SongLibrary> {
        self.load_song_library_cancellable(&never_cancelled)
    }

    pub fn load_song_library_cancellable(
        &self,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<SongLibrary> {
        cancellation_checkpoint(is_cancelled)?;
        match self {
            Self::Local(local) => {
                let library = load_local_song_library(&local.songdir)?;
                cancellation_checkpoint(is_cancelled)?;
                Ok(library)
            }
            Self::Remote(remote) => remote.load_song_library_cancellable(is_cancelled),
        }
    }

    #[cfg(test)]
    pub fn load_course_chart(
        &self,
        song: &SongEntry,
        course_index: usize,
        importer: &TjaImporter,
    ) -> Result<CanonicalChart> {
        self.load_course_chart_cancellable(song, course_index, importer, &never_cancelled)
    }

    pub fn load_course_chart_cancellable(
        &self,
        song: &SongEntry,
        course_index: usize,
        importer: &TjaImporter,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<CanonicalChart> {
        cancellation_checkpoint(is_cancelled)?;
        match self {
            Self::Local(_) => {
                let SongOrigin::Local { source_path, .. } = &song.origin else {
                    bail!("local backend received non-local source locator");
                };
                let raw = read_chart_file_bounded(source_path)?;
                cancellation_checkpoint(is_cancelled)?;
                let imported = importer
                    .import_song(&raw)
                    .with_context(|| format!("failed to parse chart {}", source_path.display()))?;
                cancellation_checkpoint(is_cancelled)?;
                let chart = imported
                    .courses
                    .into_iter()
                    .nth(course_index)
                    .map(|course| course.chart)
                    .ok_or_else(|| {
                        anyhow!(
                            "course index {course_index} out of range for {}",
                            source_path.display()
                        )
                    })?;
                cancellation_checkpoint(is_cancelled)?;
                Ok(chart)
            }
            Self::Remote(remote) => {
                remote.load_course_chart_cancellable(song, course_index, importer, is_cancelled)
            }
        }
    }

    pub fn load_song_audio(&self, song: &SongEntry) -> Result<Option<SongAudioSource>> {
        self.load_song_audio_cancellable(song, &never_cancelled)
    }

    pub fn load_song_audio_cancellable(
        &self,
        song: &SongEntry,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<SongAudioSource>> {
        cancellation_checkpoint(is_cancelled)?;
        match self {
            Self::Local(_) => {
                let SongOrigin::Local { audio_path, .. } = &song.origin else {
                    bail!("local backend received non-local audio locator");
                };
                let source = audio_path.clone().map(SongAudioSource::FilePath);
                cancellation_checkpoint(is_cancelled)?;
                Ok(source)
            }
            Self::Remote(remote) => remote.load_song_audio_cancellable(song, is_cancelled),
        }
    }
}

pub fn cache_root_dir() -> Result<PathBuf> {
    cache_root_dir_internal()
}

pub fn inspect_remote_cache() -> Result<RemoteCacheOverview> {
    let root = cache_root_dir_internal()?;
    if !root.exists() {
        return Ok(RemoteCacheOverview {
            root,
            entries: Vec::new(),
            warnings: Vec::new(),
        });
    }

    let _global_guard = lock_global_disk_cache_required(&root)?;
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("failed to read cache root {}", root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warnings.push(format!("skip entry in {}: {error}", root.display()));
                continue;
            }
        };

        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let endpoint_hash = entry.file_name().to_string_lossy().to_string();
        match summarize_cache_dir(&path, &endpoint_hash) {
            Ok(summary) => entries.push(summary),
            Err(error) => warnings.push(format!("skip {}: {error}", path.display())),
        }
    }

    entries.sort_by(|a, b| a.endpoint_hash.cmp(&b.endpoint_hash));
    Ok(RemoteCacheOverview {
        root,
        entries,
        warnings,
    })
}

pub fn clear_all_remote_cache() -> Result<RemoteCacheClearResult> {
    clear_all_remote_cache_at(cache_root_dir_internal()?)
}

fn clear_all_remote_cache_at(root: PathBuf) -> Result<RemoteCacheClearResult> {
    if !root.exists() {
        return Ok(RemoteCacheClearResult {
            removed_paths: Vec::new(),
            missing_paths: vec![root],
        });
    }

    let _global_guard = lock_global_disk_cache_required(&root)?;
    let mut removed_paths = Vec::new();
    for entry in std::fs::read_dir(&root)
        .with_context(|| format!("failed to read cache root {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", root.display()))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("failed to remove cache dir {}", path.display()))?;
        removed_paths.push(path);
    }
    let layout = DiskCacheLayout::for_cache_root(root, sha256_hex(b"cache-clear-all"));
    write_cache_index(&layout, &empty_cache_index())?;

    Ok(RemoteCacheClearResult {
        removed_paths,
        missing_paths: Vec::new(),
    })
}

pub fn clear_remote_cache_for_endpoint(endpoint: &str) -> Result<RemoteCacheClearResult> {
    let root = cache_root_dir_internal()?;
    let endpoint_hash = endpoint_cache_hash(endpoint)?;
    clear_remote_cache_for_endpoint_at(root, endpoint_hash)
}

fn clear_remote_cache_for_endpoint_at(
    root: PathBuf,
    endpoint_hash: String,
) -> Result<RemoteCacheClearResult> {
    let cache_dir = root.join(&endpoint_hash);
    if !root.exists() {
        return Ok(RemoteCacheClearResult {
            removed_paths: Vec::new(),
            missing_paths: vec![cache_dir],
        });
    }
    let _global_guard = lock_global_disk_cache_required(&root)?;
    let layout = DiskCacheLayout::for_cache_root(root, endpoint_hash.clone());
    let mut index = load_cache_index(&layout)?;
    let endpoint_prefix = format!("{endpoint_hash}/");
    let previous_entries = index.entries.len();
    index
        .entries
        .retain(|key, _| !key.starts_with(&endpoint_prefix));
    let index_changed = index.entries.len() != previous_entries;

    if !cache_dir.exists() {
        if index_changed {
            write_cache_index(&layout, &index)?;
        }
        return Ok(RemoteCacheClearResult {
            removed_paths: Vec::new(),
            missing_paths: vec![cache_dir],
        });
    }

    std::fs::remove_dir_all(&cache_dir)
        .with_context(|| format!("failed to remove cache dir {}", cache_dir.display()))?;
    write_cache_index(&layout, &index)?;
    Ok(RemoteCacheClearResult {
        removed_paths: vec![cache_dir],
        missing_paths: Vec::new(),
    })
}

fn remote_cache_mode(memory_only: bool) -> RemoteCacheMode {
    if memory_only {
        RemoteCacheMode::MemoryOnly
    } else {
        RemoteCacheMode::AppData
    }
}

impl RemoteResourceBackend {
    fn new(endpoint: &str, cache_mode: RemoteCacheMode) -> Result<Self> {
        Self::new_with_transport_policy(endpoint, cache_mode, ResourceTransportPolicy::default())
    }

    fn new_with_transport_policy(
        endpoint: &str,
        cache_mode: RemoteCacheMode,
        transport_policy: ResourceTransportPolicy,
    ) -> Result<Self> {
        if transport_policy.server_stream_min_bytes_per_second == 0
            || transport_policy.cancel_poll_interval.is_zero()
            || transport_policy.connect_timeout.is_zero()
            || transport_policy.response_header_timeout.is_zero()
            || transport_policy.chunk_idle_timeout.is_zero()
            || transport_policy.busy_retry_budget.is_zero()
            || transport_policy.busy_max_attempts == 0
            || transport_policy.busy_max_retry_after.is_zero()
            || transport_policy
                .transport_retry_delays
                .iter()
                .any(|delay| delay.is_zero())
            || transport_policy
                .busy_retry_delays
                .iter()
                .any(|delay| delay.is_zero())
            || transport_policy.busy_jitter_max_basis_points > RESOURCE_BUSY_JITTER_MAX_BASIS_POINTS
        {
            bail!("resource transport policy violates its bounded retry or timeout invariants");
        }
        let mut endpoint = Url::parse(endpoint)
            .with_context(|| format!("invalid --resource-endpoint URL: {endpoint}"))?;
        ensure_directory_url(&mut endpoint);

        let client = Client::builder()
            .connect_timeout(transport_policy.connect_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to initialize HTTP client")?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("taiko-resource-io")
            .enable_all()
            .build()
            .context("failed to initialize resource I/O runtime")?;

        let disk_cache = match cache_mode {
            RemoteCacheMode::MemoryOnly => None,
            RemoteCacheMode::AppData => {
                let layout = DiskCacheLayout::from_endpoint(&endpoint)?;
                Some(Mutex::new(DiskCache::open(
                    layout,
                    DiskCachePolicy::default(),
                )?))
            }
        };

        Ok(Self {
            endpoint,
            client,
            runtime,
            transport_policy,
            disk_cache_lock_policy: DiskCacheLockPolicy::PRODUCTION,
            memory_cache: Mutex::new(MemoryCache::new(MAX_MEMORY_CACHE_BYTES)),
            disk_cache,
        })
    }

    fn load_song_library_cancellable(
        &self,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<SongLibrary> {
        cancellation_checkpoint(is_cancelled)?;
        let url = self.api_url("v1/library")?;
        let raw =
            self.download_payload_cancellable(&url, MAX_LIBRARY_RESPONSE_BYTES, is_cancelled)?;
        cancellation_checkpoint(is_cancelled)?;
        let document = serde_json::from_slice::<ResourceLibraryDocument>(&raw)
            .with_context(|| format!("failed to parse response payload from {url}"))?;
        cancellation_checkpoint(is_cancelled)?;

        document
            .validate()
            .context("resource library violates the v1 wire contract")?;
        validate_resource_semantics(&document.semantics)?;

        let mut songs = Vec::with_capacity(document.songs.len());
        for song in document.songs {
            cancellation_checkpoint(is_cancelled)?;
            let chart_hash = validate_content_hash(&song.source_id)
                .with_context(|| {
                    format!(
                        "library entry `{}` has invalid chart hash",
                        song.source_path
                    )
                })?
                .to_owned();
            let audio_hash = song
                .audio_id
                .as_deref()
                .map(validate_content_hash)
                .transpose()
                .with_context(|| {
                    format!(
                        "library entry `{}` has invalid audio hash",
                        song.source_path
                    )
                })?
                .map(ToOwned::to_owned);

            songs.push(SongEntry {
                origin: SongOrigin::Remote {
                    identity: RemoteSongIdentity {
                        song_id: song.song_id,
                        source_id: chart_hash,
                        audio_id: audio_hash,
                    },
                    source_path: PathBuf::from(song.source_path),
                    audio_path: song.audio_path.map(PathBuf::from),
                },
                title: song.title,
                subtitle: song.subtitle,
                artist: song.artist,
                demo_start_seconds: song.demo_start_seconds,
                courses: song
                    .courses
                    .into_iter()
                    .enumerate()
                    .map(|(position, course)| {
                        cancellation_checkpoint(is_cancelled)?;
                        let index = usize::try_from(course.index)
                            .context("course index does not fit this platform")?;
                        if index != position {
                            bail!(
                                "course indices must be dense and ordered; expected {position}, got {index}"
                            );
                        }
                        let canonical_chart_hash =
                            validate_content_hash(&course.canonical_chart_hash)
                                .context("invalid canonical chart hash")?
                                .to_owned();
                        Ok(CourseEntry {
                            index,
                            name: course.name,
                            level: course.level,
                            canonical_chart_hash,
                            object_count: usize::try_from(course.object_count)
                                .context("object count does not fit this platform")?,
                            branch_segment_count: usize::try_from(course.branch_segment_count)
                                .context("branch count does not fit this platform")?,
                            base_bpm: course.base_bpm,
                            branch_decisions: course
                                .branch_decisions
                                .into_iter()
                                .map(|decision| BranchDecisionPoint {
                                    segment_id: decision.segment_id,
                                    decision_tick: decision.decision_tick,
                                    default_route_id: decision.default_route_id,
                                    route_count: decision.route_count,
                                    hint: decision.hint,
                                })
                                .collect(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            });
        }

        songs.sort_by_cached_key(|song| {
            (song.title.to_lowercase(), song.source_path().to_path_buf())
        });
        cancellation_checkpoint(is_cancelled)?;

        Ok(SongLibrary {
            songs,
            warnings: document.warnings,
        })
    }

    fn load_course_chart_cancellable(
        &self,
        song: &SongEntry,
        course_index: usize,
        importer: &TjaImporter,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<CanonicalChart> {
        cancellation_checkpoint(is_cancelled)?;
        let Some(identity) = song.remote_identity() else {
            bail!("remote backend received non-remote source locator");
        };

        let raw = self.fetch_cached_bytes_cancellable(
            RemoteResourceKind::Chart,
            &identity.source_id,
            &identity.source_id,
            is_cancelled,
        )?;
        cancellation_checkpoint(is_cancelled)?;
        let imported = importer.import_song(raw.as_ref()).with_context(|| {
            format!(
                "failed to parse remote chart {}",
                song.source_path().display()
            )
        })?;
        cancellation_checkpoint(is_cancelled)?;
        verify_song_summary(&imported, song)?;

        let chart = imported
            .courses
            .into_iter()
            .nth(course_index)
            .map(|course| course.chart)
            .ok_or_else(|| {
                anyhow!(
                    "course index {course_index} out of range for {}",
                    song.source_path().display()
                )
            })?;
        let expected = song.courses.get(course_index).ok_or_else(|| {
            anyhow!(
                "course summary {course_index} missing for {}",
                song.source_path().display()
            )
        })?;
        verify_canonical_chart_hash(
            &chart,
            &expected.canonical_chart_hash,
            song.source_path(),
            course_index,
        )?;
        verify_course_summary(&chart, expected, song.source_path(), course_index)?;
        cancellation_checkpoint(is_cancelled)?;
        Ok(chart)
    }

    fn load_song_audio_cancellable(
        &self,
        song: &SongEntry,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<SongAudioSource>> {
        cancellation_checkpoint(is_cancelled)?;
        let Some(identity) = song.remote_identity() else {
            bail!("remote backend received non-remote audio locator");
        };

        let audio_id = match (identity.audio_id.as_deref(), song.audio_path()) {
            (None, None) => return Ok(None),
            (Some(audio_id), Some(_)) => audio_id,
            _ => bail!("remote song has a partial audio locator"),
        };
        let bytes = self.fetch_cached_bytes_cancellable(
            RemoteResourceKind::Audio,
            audio_id,
            audio_id,
            is_cancelled,
        )?;
        cancellation_checkpoint(is_cancelled)?;
        Ok(Some(SongAudioSource::Bytes(bytes)))
    }

    #[cfg(test)]
    fn fetch_cached_bytes(
        &self,
        kind: RemoteResourceKind,
        resource_id: &str,
        expected_hash: &str,
    ) -> Result<Arc<[u8]>> {
        self.fetch_cached_bytes_cancellable(kind, resource_id, expected_hash, &never_cancelled)
    }

    fn fetch_cached_bytes_cancellable(
        &self,
        kind: RemoteResourceKind,
        resource_id: &str,
        expected_hash: &str,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<[u8]>> {
        self.fetch_cached_bytes_with_cancellation(
            kind,
            resource_id,
            expected_hash,
            is_cancelled,
            || self.download_resource_cancellable(kind, resource_id, is_cancelled),
        )
    }

    #[cfg(test)]
    fn fetch_cached_bytes_with(
        &self,
        kind: RemoteResourceKind,
        resource_id: &str,
        expected_hash: &str,
        download: impl FnOnce() -> Result<Arc<[u8]>>,
    ) -> Result<Arc<[u8]>> {
        self.fetch_cached_bytes_with_cancellation(
            kind,
            resource_id,
            expected_hash,
            &never_cancelled,
            download,
        )
    }

    fn fetch_cached_bytes_with_cancellation(
        &self,
        kind: RemoteResourceKind,
        resource_id: &str,
        expected_hash: &str,
        is_cancelled: &dyn Fn() -> bool,
        download: impl FnOnce() -> Result<Arc<[u8]>>,
    ) -> Result<Arc<[u8]>> {
        cancellation_checkpoint(is_cancelled)?;
        validate_content_hash(resource_id).context("invalid remote resource id")?;
        validate_content_hash(expected_hash).context("invalid expected resource hash")?;
        if resource_id != expected_hash {
            bail!(
                "remote {} id must equal its expected content hash",
                kind.route_segment()
            );
        }

        if let Some(bytes) = self.get_memory_cached(expected_hash)? {
            cancellation_checkpoint(is_cancelled)?;
            ensure_resource_size(bytes.len(), kind.max_bytes(), "memory cache")?;
            verify_content_hash(bytes.as_ref(), expected_hash, "memory cache")?;
            cancellation_checkpoint(is_cancelled)?;
            self.touch_disk_cached_cancellable(kind, expected_hash, is_cancelled)?;
            return Ok(bytes);
        }

        cancellation_checkpoint(is_cancelled)?;
        if let Some(bytes) = self.load_disk_cached_cancellable(kind, expected_hash, is_cancelled)? {
            cancellation_checkpoint(is_cancelled)?;
            self.put_memory_cached(expected_hash, bytes.clone())?;
            return Ok(bytes);
        }

        cancellation_checkpoint(is_cancelled)?;
        let bytes = verify_downloaded_resource(kind, resource_id, expected_hash, download()?)?;
        cancellation_checkpoint(is_cancelled)?;

        self.store_disk_cached_cancellable(kind, expected_hash, bytes.as_ref(), is_cancelled)?;
        cancellation_checkpoint(is_cancelled)?;
        self.put_memory_cached(expected_hash, bytes.clone())?;

        Ok(bytes)
    }

    fn get_memory_cached(&self, content_hash: &str) -> Result<Option<Arc<[u8]>>> {
        let guard = self
            .memory_cache
            .lock()
            .map_err(|_| anyhow!("memory cache lock poisoned"))?;
        Ok(guard.get(content_hash))
    }

    fn put_memory_cached(&self, content_hash: &str, bytes: Arc<[u8]>) -> Result<()> {
        let mut guard = self
            .memory_cache
            .lock()
            .map_err(|_| anyhow!("memory cache lock poisoned"))?;
        guard.insert(content_hash.to_owned(), bytes);
        Ok(())
    }

    #[cfg(test)]
    fn load_disk_cached(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
    ) -> Result<Option<Arc<[u8]>>> {
        self.load_disk_cached_cancellable(kind, content_hash, &never_cancelled)
    }

    fn load_disk_cached_cancellable(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<[u8]>>> {
        let Some(cache) = self.disk_cache.as_ref() else {
            return Ok(None);
        };
        let wait = self.disk_cache_lock_policy.start()?;
        let Some(mut cache) = try_lock_mutex_until(cache, "disk cache mutex", wait, is_cancelled)?
        else {
            return Ok(None);
        };
        cache.load_with_wait(kind, content_hash, wait, is_cancelled)
    }

    fn touch_disk_cached_cancellable(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(cache) = self.disk_cache.as_ref() else {
            return Ok(());
        };
        let wait = self.disk_cache_lock_policy.start()?;
        let Some(mut cache) = try_lock_mutex_until(cache, "disk cache mutex", wait, is_cancelled)?
        else {
            return Ok(());
        };
        cache.touch_with_wait(kind, content_hash, wait, is_cancelled)
    }

    #[cfg(test)]
    fn store_disk_cached(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
        bytes: &[u8],
    ) -> Result<()> {
        self.store_disk_cached_cancellable(kind, content_hash, bytes, &never_cancelled)
    }

    fn store_disk_cached_cancellable(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
        bytes: &[u8],
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(cache) = self.disk_cache.as_ref() else {
            return Ok(());
        };
        let wait = self.disk_cache_lock_policy.start()?;
        let Some(mut cache) = try_lock_mutex_until(cache, "disk cache mutex", wait, is_cancelled)?
        else {
            return Ok(());
        };
        cache.store_with_wait(kind, content_hash, bytes, wait, is_cancelled)
    }

    fn download_resource_cancellable(
        &self,
        kind: RemoteResourceKind,
        resource_id: &str,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<[u8]>> {
        cancellation_checkpoint(is_cancelled)?;
        let url = self.api_url(&format!("v1/{}/{resource_id}", kind.route_segment()))?;
        self.download_payload_cancellable(&url, kind.max_bytes(), is_cancelled)
    }

    fn download_payload_cancellable(
        &self,
        url: &Url,
        max_bytes: u64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<[u8]>> {
        cancellation_checkpoint(is_cancelled)?;
        self.runtime
            .block_on(self.download_payload_with_retry(url, max_bytes, is_cancelled))
    }

    async fn download_payload_with_retry(
        &self,
        url: &Url,
        max_bytes: u64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Arc<[u8]>> {
        let mut consecutive_transport_failures = 0_usize;
        let mut busy_started: Option<tokio::time::Instant> = None;
        let mut busy_attempts = 0_usize;
        let mut busy_responses = 0_usize;

        loop {
            cancellation_checkpoint(is_cancelled)?;
            if let Some(started) = busy_started {
                if busy_attempts >= self.transport_policy.busy_max_attempts {
                    return Err(server_busy_error(
                        busy_attempts,
                        format_args!(
                            "reached the {}-attempt admission retry limit",
                            self.transport_policy.busy_max_attempts
                        ),
                    ));
                }
                if started.elapsed() >= self.transport_policy.busy_retry_budget {
                    return Err(server_busy_error(
                        busy_attempts,
                        format_args!(
                            "exhausted the {:?} admission retry budget",
                            self.transport_policy.busy_retry_budget
                        ),
                    ));
                }
                busy_attempts += 1;
            }

            let attempt = if let Some(started) = busy_started {
                let busy_deadline = started + self.transport_policy.busy_retry_budget;
                tokio::select! {
                    attempt = self.download_payload_attempt(url, max_bytes, is_cancelled) => attempt,
                    _ = tokio::time::sleep_until(busy_deadline) => {
                        return Err(server_busy_error(
                            busy_attempts,
                            format_args!(
                                "exhausted the {:?} admission retry budget",
                                self.transport_policy.busy_retry_budget
                            ),
                        ));
                    }
                }
            } else {
                self.download_payload_attempt(url, max_bytes, is_cancelled)
                    .await
            };

            match attempt {
                ResourceDownloadAttempt::Success(bytes) => return Ok(bytes),
                ResourceDownloadAttempt::PermanentFailure(error) => return Err(error),
                ResourceDownloadAttempt::TransientFailure(error) => {
                    consecutive_transport_failures += 1;
                    if busy_started.is_some()
                        && busy_attempts >= self.transport_policy.busy_max_attempts
                    {
                        return Err(server_busy_error(
                            busy_attempts,
                            format_args!(
                                "reached the {}-attempt admission retry limit",
                                self.transport_policy.busy_max_attempts
                            ),
                        ));
                    }
                    if consecutive_transport_failures >= RESOURCE_TRANSPORT_MAX_ATTEMPTS {
                        return Err(error.context(format!(
                            "resource transport exhausted {RESOURCE_TRANSPORT_MAX_ATTEMPTS} consecutive attempts"
                        )));
                    }
                    let delay = self.transport_policy.transport_retry_delays
                        [consecutive_transport_failures - 1];
                    if let Some(started) = busy_started {
                        sleep_with_busy_budget(
                            delay,
                            started,
                            busy_attempts,
                            is_cancelled,
                            self.transport_policy,
                        )
                        .await?;
                    } else {
                        sleep_with_cancellation(
                            delay,
                            is_cancelled,
                            self.transport_policy.cancel_poll_interval,
                        )
                        .await?;
                    }
                }
                ResourceDownloadAttempt::HttpFailure {
                    status,
                    error,
                    retry_after,
                } => {
                    if status != StatusCode::SERVICE_UNAVAILABLE {
                        return Err(error);
                    }
                    consecutive_transport_failures = 0;
                    let started = *busy_started.get_or_insert_with(tokio::time::Instant::now);
                    if busy_attempts == 0 {
                        busy_attempts = 1;
                    }
                    busy_responses += 1;
                    if busy_attempts >= self.transport_policy.busy_max_attempts {
                        return Err(server_busy_error(
                            busy_attempts,
                            format_args!(
                                "reached the {}-attempt admission retry limit",
                                self.transport_policy.busy_max_attempts
                            ),
                        ));
                    }
                    let delay =
                        busy_retry_delay(self.transport_policy, busy_responses, retry_after);
                    sleep_with_busy_budget(
                        delay,
                        started,
                        busy_attempts,
                        is_cancelled,
                        self.transport_policy,
                    )
                    .await?;
                }
            }
        }
    }

    async fn download_payload_attempt(
        &self,
        url: &Url,
        max_bytes: u64,
        is_cancelled: &dyn Fn() -> bool,
    ) -> ResourceDownloadAttempt<Arc<[u8]>> {
        if let Err(error) = cancellation_checkpoint(is_cancelled) {
            return ResourceDownloadAttempt::PermanentFailure(error);
        }

        let request = self
            .client
            .get(url.clone())
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .send();
        tokio::pin!(request);
        let header_deadline = tokio::time::sleep(self.transport_policy.response_header_timeout);
        tokio::pin!(header_deadline);
        let response = loop {
            tokio::select! {
                result = &mut request => {
                    break match result {
                        Ok(response) => response,
                        Err(error) => {
                            let retryable = is_retryable_reqwest_error(&error);
                            let error = anyhow::Error::new(error)
                                .context(format!("failed to request {url}"));
                            return if retryable {
                                ResourceDownloadAttempt::TransientFailure(error)
                            } else {
                                ResourceDownloadAttempt::PermanentFailure(error)
                            };
                        }
                    };
                }
                _ = &mut header_deadline => {
                    return ResourceDownloadAttempt::TransientFailure(anyhow!(
                            "response headers from {url} exceeded the verification-aware {:?} deadline",
                            self.transport_policy.response_header_timeout
                        ));
                }
                _ = tokio::time::sleep(self.transport_policy.cancel_poll_interval) => {
                    if let Err(error) = cancellation_checkpoint(is_cancelled) {
                        return ResourceDownloadAttempt::PermanentFailure(error);
                    }
                }
            }
        };

        if let Err(error) = cancellation_checkpoint(is_cancelled) {
            return ResourceDownloadAttempt::PermanentFailure(error);
        }
        let status = response.status();
        let retry_after = retry_after_delay(&response);
        if let Err(error) = response.error_for_status_ref() {
            return ResourceDownloadAttempt::HttpFailure {
                status,
                error: anyhow::Error::new(error).context(format!("resource server rejected {url}")),
                retry_after,
            };
        }

        read_response_body_attempt(
            response,
            max_bytes,
            url,
            is_cancelled,
            self.transport_policy,
        )
        .await
    }

    fn api_url(&self, path: &str) -> Result<Url> {
        self.endpoint
            .join(path)
            .with_context(|| format!("invalid API path `{path}` for endpoint {}", self.endpoint))
    }
}

impl RemoteResourceKind {
    fn route_segment(self) -> &'static str {
        match self {
            Self::Chart => "charts",
            Self::Audio => "audio",
        }
    }

    fn max_bytes(self) -> u64 {
        match self {
            Self::Chart => MAX_CHART_RESPONSE_BYTES,
            Self::Audio => MAX_AUDIO_RESPONSE_BYTES,
        }
    }

    fn from_route_segment(segment: &str) -> Option<Self> {
        match segment {
            "charts" => Some(Self::Chart),
            "audio" => Some(Self::Audio),
            _ => None,
        }
    }
}

impl MemoryCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            insertion_order: VecDeque::new(),
            total_bytes: 0,
            max_bytes,
        }
    }

    fn get(&self, content_hash: &str) -> Option<Arc<[u8]>> {
        self.entries.get(content_hash).cloned()
    }

    fn insert(&mut self, content_hash: String, bytes: Arc<[u8]>) {
        if self.entries.contains_key(&content_hash) || bytes.len() > self.max_bytes {
            return;
        }

        while self.total_bytes.saturating_add(bytes.len()) > self.max_bytes {
            let Some(oldest_hash) = self.insertion_order.pop_front() else {
                break;
            };
            if let Some(oldest) = self.entries.remove(&oldest_hash) {
                self.total_bytes = self.total_bytes.saturating_sub(oldest.len());
            }
        }

        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        self.insertion_order.push_back(content_hash.clone());
        self.entries.insert(content_hash, bytes);
    }
}

impl DiskCachePolicy {
    fn validate(self) -> Result<Self> {
        if self.max_bytes == 0 {
            bail!("disk cache byte quota must be greater than zero");
        }
        if self.max_entries == 0 || self.max_entries > MAX_DISK_CACHE_INDEX_ENTRIES {
            bail!("disk cache entry quota must be in 1..={MAX_DISK_CACHE_INDEX_ENTRIES}");
        }
        Ok(self)
    }
}

fn global_disk_cache_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn try_lock_mutex_until<'a, T>(
    mutex: &'a Mutex<T>,
    label: &str,
    wait: DiskCacheLockWait,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<MutexGuard<'a, T>>> {
    loop {
        cancellation_checkpoint(is_cancelled)?;
        match mutex.try_lock() {
            Ok(guard) => {
                cancellation_checkpoint(is_cancelled)?;
                return Ok(Some(guard));
            }
            Err(std::sync::TryLockError::Poisoned(_)) => {
                bail!("{label} poisoned");
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if !wait.pause_before_retry() {
                    return Ok(None);
                }
            }
        }
    }
}

struct GlobalDiskCacheGuard<'a> {
    _process_guard: MutexGuard<'a, ()>,
    _file_guard: std::fs::File,
}

fn try_lock_global_disk_cache(
    cache_root: &Path,
    wait: DiskCacheLockWait,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<GlobalDiskCacheGuard<'static>>> {
    let Some(process_guard) = try_lock_mutex_until(
        global_disk_cache_lock(),
        "global disk cache mutex",
        wait,
        is_cancelled,
    )?
    else {
        return Ok(None);
    };
    std::fs::create_dir_all(cache_root)
        .with_context(|| format!("failed to create cache root {}", cache_root.display()))?;
    let lock_path = cache_root.join(CACHE_LOCK_FILE_NAME);
    let file_guard = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open cache lock {}", lock_path.display()))?;
    loop {
        cancellation_checkpoint(is_cancelled)?;
        match file_guard.try_lock() {
            Ok(()) => {
                cancellation_checkpoint(is_cancelled)?;
                return Ok(Some(GlobalDiskCacheGuard {
                    _process_guard: process_guard,
                    _file_guard: file_guard,
                }));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if !wait.pause_before_retry() {
                    return Ok(None);
                }
            }
            Err(std::fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("failed to lock cache root {}", cache_root.display())
                });
            }
        }
    }
}

fn lock_global_disk_cache_required(cache_root: &Path) -> Result<GlobalDiskCacheGuard<'static>> {
    let policy = DiskCacheLockPolicy::PRODUCTION;
    let wait = policy.start()?;
    try_lock_global_disk_cache(cache_root, wait, &never_cancelled)?.ok_or_else(|| {
        anyhow!(
            "timed out after {:?} waiting for disk cache coordination at {}",
            policy.timeout,
            cache_root.display()
        )
    })
}

impl DiskCache {
    fn open(layout: DiskCacheLayout, policy: DiskCachePolicy) -> Result<Self> {
        Self::open_with_lock_policy(layout, policy, DiskCacheLockPolicy::PRODUCTION)
    }

    fn open_with_lock_policy(
        layout: DiskCacheLayout,
        policy: DiskCachePolicy,
        lock_policy: DiskCacheLockPolicy,
    ) -> Result<Self> {
        let policy = policy.validate()?;
        validate_content_hash(&layout.endpoint_hash).context("invalid cache endpoint identity")?;
        let mut cache = Self {
            layout,
            policy,
            index: empty_cache_index(),
        };
        let wait = lock_policy.start()?;
        let Some(_global_guard) =
            try_lock_global_disk_cache(&cache.layout.cache_root, wait, &never_cancelled)?
        else {
            eprintln!(
                "disk cache is busy; defer initialization for {}",
                cache.layout.cache_root.display()
            );
            return Ok(cache);
        };
        cache.refresh_locked()?;
        Ok(cache)
    }

    #[cfg(test)]
    fn load(&mut self, kind: RemoteResourceKind, content_hash: &str) -> Result<Option<Arc<[u8]>>> {
        let wait = DiskCacheLockPolicy::PRODUCTION.start()?;
        self.load_with_wait(kind, content_hash, wait, &never_cancelled)
    }

    fn load_with_wait(
        &mut self,
        kind: RemoteResourceKind,
        content_hash: &str,
        wait: DiskCacheLockWait,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Arc<[u8]>>> {
        let Some(_global_guard) =
            try_lock_global_disk_cache(&self.layout.cache_root, wait, is_cancelled)?
        else {
            return Ok(None);
        };
        self.refresh_locked()?;
        self.load_locked(kind, content_hash)
    }

    fn load_locked(
        &mut self,
        kind: RemoteResourceKind,
        content_hash: &str,
    ) -> Result<Option<Arc<[u8]>>> {
        validate_content_hash(content_hash).context("invalid disk cache content hash")?;
        let key = self.layout.entry_key(kind, content_hash);
        let path = self.layout.blob_path(kind, content_hash);
        let Some(entry) = self.index.entries.get(&key).cloned() else {
            if path.exists() {
                remove_file_if_exists(&path).with_context(|| {
                    format!("failed to remove unindexed cache blob {}", path.display())
                })?;
            }
            return Ok(None);
        };

        let verified = (|| -> Result<Vec<u8>> {
            let file = std::fs::File::open(&path)
                .with_context(|| format!("failed to open cache blob {}", path.display()))?;
            let metadata = file
                .metadata()
                .with_context(|| format!("failed to stat cache blob {}", path.display()))?;
            if !metadata.is_file()
                || metadata.len() != entry.size_bytes
                || metadata.len() > kind.max_bytes()
            {
                bail!("cache blob metadata does not match its bounded index entry");
            }
            let bytes = read_bounded_reader(file, kind.max_bytes())
                .with_context(|| format!("failed to read cache blob {}", path.display()))?;
            verify_content_hash(&bytes, content_hash, "disk cache")?;
            Ok(bytes)
        })();

        match verified {
            Ok(bytes) => {
                self.touch_locked(kind, content_hash)?;
                Ok(Some(bytes.into()))
            }
            Err(error) => {
                eprintln!("discard corrupt cache blob {}: {error:#}", path.display());
                self.invalidate_locked(kind, content_hash)?;
                Ok(None)
            }
        }
    }

    fn touch_with_wait(
        &mut self,
        kind: RemoteResourceKind,
        content_hash: &str,
        wait: DiskCacheLockWait,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(_global_guard) =
            try_lock_global_disk_cache(&self.layout.cache_root, wait, is_cancelled)?
        else {
            return Ok(());
        };
        self.refresh_locked()?;
        self.touch_locked(kind, content_hash)
    }

    fn touch_locked(&mut self, kind: RemoteResourceKind, content_hash: &str) -> Result<()> {
        let key = self.layout.entry_key(kind, content_hash);
        let Some(previous_access) = self.index.entries.get(&key).map(|entry| entry.last_access)
        else {
            return Ok(());
        };
        let previous_clock = self.index.access_clock;
        let next_access = previous_clock
            .checked_add(1)
            .ok_or_else(|| anyhow!("disk cache access clock exhausted"))?;
        self.index.access_clock = next_access;
        self.index
            .entries
            .get_mut(&key)
            .expect("entry presence was checked")
            .last_access = next_access;
        if let Err(error) = self.flush_index() {
            self.index.access_clock = previous_clock;
            self.index
                .entries
                .get_mut(&key)
                .expect("entry remains present while rolling back touch")
                .last_access = previous_access;
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn store(&mut self, kind: RemoteResourceKind, content_hash: &str, bytes: &[u8]) -> Result<()> {
        let wait = DiskCacheLockPolicy::PRODUCTION.start()?;
        self.store_with_wait(kind, content_hash, bytes, wait, &never_cancelled)
    }

    fn store_with_wait(
        &mut self,
        kind: RemoteResourceKind,
        content_hash: &str,
        bytes: &[u8],
        wait: DiskCacheLockWait,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let Some(_global_guard) =
            try_lock_global_disk_cache(&self.layout.cache_root, wait, is_cancelled)?
        else {
            return Ok(());
        };
        self.refresh_locked()?;
        self.store_locked(kind, content_hash, bytes)
    }

    fn store_locked(
        &mut self,
        kind: RemoteResourceKind,
        content_hash: &str,
        bytes: &[u8],
    ) -> Result<()> {
        validate_content_hash(content_hash).context("invalid disk cache content hash")?;
        ensure_resource_size(bytes.len(), kind.max_bytes(), "downloaded")?;
        verify_content_hash(bytes, content_hash, "downloaded")?;

        let key = self.layout.entry_key(kind, content_hash);
        if self.index.entries.contains_key(&key) && self.load_locked(kind, content_hash)?.is_some()
        {
            return Ok(());
        }

        let size_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if size_bytes > self.policy.max_bytes {
            bail!(
                "resource requires {size_bytes} cache bytes but the global hard quota is {} bytes",
                self.policy.max_bytes
            );
        }
        let previous_clock = self.index.access_clock;
        let next_access = previous_clock
            .checked_add(1)
            .ok_or_else(|| anyhow!("disk cache access clock exhausted"))?;

        while self.index.entries.len().saturating_add(1) > self.policy.max_entries
            || self
                .total_bytes()?
                .checked_add(size_bytes)
                .is_none_or(|total| total > self.policy.max_bytes)
        {
            let victim = self
                .lru_key()
                .ok_or_else(|| anyhow!("global disk cache quota cannot admit resource"))?;
            self.evict_locked(&victim)?;
        }

        let path = self.layout.blob_path(kind, content_hash);
        write_atomic(&path, bytes)
            .with_context(|| format!("failed to write cache blob {}", path.display()))?;

        self.index.access_clock = next_access;
        self.index.entries.insert(
            key.clone(),
            DiskCacheEntry {
                size_bytes,
                last_access: next_access,
            },
        );
        if let Err(error) = self.flush_index() {
            self.index.entries.remove(&key);
            self.index.access_clock = previous_clock;
            let _ = remove_file_if_exists(&path);
            return Err(error);
        }
        Ok(())
    }

    fn invalidate_locked(&mut self, kind: RemoteResourceKind, content_hash: &str) -> Result<()> {
        let key = self.layout.entry_key(kind, content_hash);
        if self.index.entries.remove(&key).is_some() {
            self.flush_index()?;
        }
        let path = self.layout.blob_path(kind, content_hash);
        if let Err(error) = remove_file_if_exists(&path) {
            eprintln!(
                "failed to remove invalidated cache blob {}: {error:#}",
                path.display()
            );
        }
        Ok(())
    }

    fn evict_locked(&mut self, key: &str) -> Result<()> {
        let path = self.layout.path_for_entry(key)?;
        remove_file_if_exists(&path)
            .with_context(|| format!("failed to evict cache blob {}", path.display()))?;
        self.index
            .entries
            .remove(key)
            .ok_or_else(|| anyhow!("cache eviction target disappeared"))?;
        self.flush_index()
    }

    fn refresh_locked(&mut self) -> Result<()> {
        self.layout.create_dirs()?;
        self.index = load_cache_index(&self.layout)?;
        self.reconcile_locked()
    }

    fn reconcile_locked(&mut self) -> Result<()> {
        let mut changed = false;
        for key in self.index.entries.keys().cloned().collect::<Vec<_>>() {
            let (_, kind, _) = parse_resource_key(&key)?;
            let path = self.layout.path_for_entry(&key)?;
            let entry = self
                .index
                .entries
                .get(&key)
                .expect("key came from the entry map");
            let valid_metadata = std::fs::metadata(&path).is_ok_and(|metadata| {
                metadata.is_file()
                    && metadata.len() == entry.size_bytes
                    && metadata.len() <= kind.max_bytes()
            });
            if !valid_metadata {
                let _ = remove_file_if_exists(&path);
                self.index.entries.remove(&key);
                changed = true;
            }
        }

        for entry in WalkDir::new(&self.layout.cache_root)
            .min_depth(1)
            .into_iter()
        {
            let entry = entry.with_context(|| {
                format!(
                    "failed to inspect global cache root {}",
                    self.layout.cache_root.display()
                )
            })?;
            if !entry.file_type().is_file()
                || entry.path() == self.layout.index_file
                || entry.path() == self.layout.cache_root.join(CACHE_LOCK_FILE_NAME)
            {
                continue;
            }
            let indexed = global_key_for_blob_path(&self.layout.cache_root, entry.path())
                .is_some_and(|key| self.index.entries.contains_key(&key));
            if !indexed {
                remove_file_if_exists(entry.path()).with_context(|| {
                    format!(
                        "failed to remove orphan cache file {}",
                        entry.path().display()
                    )
                })?;
                changed = true;
            }
        }

        while self.index.entries.len() > self.policy.max_entries
            || self.total_bytes()? > self.policy.max_bytes
        {
            let victim = self
                .lru_key()
                .ok_or_else(|| anyhow!("global cache policy reconciliation made no progress"))?;
            remove_file_if_exists(&self.layout.path_for_entry(&victim)?)?;
            self.index.entries.remove(&victim);
            changed = true;
        }

        if changed {
            self.flush_index()?;
        }
        Ok(())
    }

    fn lru_key(&self) -> Option<String> {
        self.index
            .entries
            .iter()
            .min_by(|(left_key, left), (right_key, right)| {
                (left.last_access, left_key).cmp(&(right.last_access, right_key))
            })
            .map(|(key, _)| key.clone())
    }

    fn total_bytes(&self) -> Result<u64> {
        self.index.entries.values().try_fold(0_u64, |total, entry| {
            total
                .checked_add(entry.size_bytes)
                .ok_or_else(|| anyhow!("disk cache byte accounting overflow"))
        })
    }

    fn flush_index(&self) -> Result<()> {
        write_cache_index(&self.layout, &self.index)
    }
}

impl DiskCacheLayout {
    fn from_endpoint(endpoint: &Url) -> Result<Self> {
        Ok(Self::for_cache_root(
            cache_root_dir_internal()?,
            endpoint_cache_hash_url(endpoint),
        ))
    }

    #[cfg(test)]
    fn from_root(cache_root: PathBuf) -> Self {
        Self::for_cache_root(cache_root, sha256_hex(b"test-cache-endpoint"))
    }

    fn for_cache_root(cache_root: PathBuf, endpoint_hash: String) -> Self {
        let endpoint_root = cache_root.join(&endpoint_hash);
        Self {
            index_file: cache_root.join("index-v3.json"),
            cache_root,
            endpoint_hash,
            chart_dir: endpoint_root.join("charts"),
            audio_dir: endpoint_root.join("audio"),
        }
    }

    fn create_dirs(&self) -> Result<()> {
        validate_content_hash(&self.endpoint_hash).context("invalid cache endpoint identity")?;
        for directory in [&self.cache_root, &self.chart_dir, &self.audio_dir] {
            std::fs::create_dir_all(directory)
                .with_context(|| format!("failed to create cache dir {}", directory.display()))?;
        }
        Ok(())
    }

    fn entry_key(&self, kind: RemoteResourceKind, content_hash: &str) -> String {
        format!(
            "{}/{}/{}",
            self.endpoint_hash,
            kind.route_segment(),
            content_hash
        )
    }

    fn blob_path(&self, kind: RemoteResourceKind, content_hash: &str) -> PathBuf {
        match kind {
            RemoteResourceKind::Chart => self.chart_dir.join(content_hash),
            RemoteResourceKind::Audio => self.audio_dir.join(content_hash),
        }
    }

    fn path_for_entry(&self, resource_key: &str) -> Result<PathBuf> {
        let (endpoint_hash, kind, content_hash) = parse_resource_key(resource_key)?;
        Ok(self
            .cache_root
            .join(endpoint_hash)
            .join(kind.route_segment())
            .join(content_hash))
    }
}

fn parse_resource_key(resource_key: &str) -> Result<(&str, RemoteResourceKind, &str)> {
    let mut components = resource_key.split('/');
    let endpoint_hash = components
        .next()
        .ok_or_else(|| anyhow!("cache key is missing endpoint identity"))?;
    let kind = components
        .next()
        .and_then(RemoteResourceKind::from_route_segment)
        .ok_or_else(|| anyhow!("cache key has an invalid resource kind"))?;
    let content_hash = components
        .next()
        .ok_or_else(|| anyhow!("cache key is missing content identity"))?;
    if components.next().is_some() {
        bail!("cache key has unexpected path components");
    }
    validate_content_hash(endpoint_hash).context("invalid endpoint identity in cache key")?;
    validate_content_hash(content_hash).context("invalid content identity in cache key")?;
    Ok((endpoint_hash, kind, content_hash))
}

fn global_key_for_blob_path(cache_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(cache_root).ok()?;
    let mut components = relative.components();
    let endpoint_hash = components.next()?.as_os_str().to_str()?;
    let kind = components.next()?.as_os_str().to_str()?;
    let content_hash = components.next()?.as_os_str().to_str()?;
    if components.next().is_some()
        || validate_content_hash(endpoint_hash).is_err()
        || RemoteResourceKind::from_route_segment(kind).is_none()
        || validate_content_hash(content_hash).is_err()
    {
        return None;
    }
    Some(format!("{endpoint_hash}/{kind}/{content_hash}"))
}

fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove cache file {}", path.display()))
        }
    }
}

fn summarize_cache_dir(path: &Path, endpoint_hash: &str) -> Result<RemoteCacheSummary> {
    let cache_root = path
        .parent()
        .ok_or_else(|| anyhow!("cache endpoint path has no global root"))?;
    let layout =
        DiskCacheLayout::for_cache_root(cache_root.to_path_buf(), endpoint_hash.to_owned());

    let (chart_files, chart_bytes) = directory_blob_stats(&layout.chart_dir)?;
    let (audio_files, audio_bytes) = directory_blob_stats(&layout.audio_dir)?;
    let endpoint_prefix = format!("{endpoint_hash}/");
    let index_entries = load_cache_index(&layout)?
        .entries
        .keys()
        .filter(|key| key.starts_with(&endpoint_prefix))
        .count();

    Ok(RemoteCacheSummary {
        endpoint_hash: endpoint_hash.to_owned(),
        path: path.to_path_buf(),
        chart_files,
        chart_bytes,
        audio_files,
        audio_bytes,
        index_entries,
    })
}

fn directory_blob_stats(dir: &Path) -> Result<(usize, u64)> {
    if !dir.exists() {
        return Ok((0, 0));
    }
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }

    let mut file_count = 0_usize;
    let mut total_bytes = 0_u64;
    for entry in WalkDir::new(dir).into_iter() {
        let entry =
            entry.with_context(|| format!("failed to read entry under {}", dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to stat {}", entry.path().display()))?;
        file_count += 1;
        total_bytes = total_bytes.saturating_add(metadata.len());
    }

    Ok((file_count, total_bytes))
}

fn load_cache_index(layout: &DiskCacheLayout) -> Result<CacheIndexDocument> {
    if !layout.index_file.exists() {
        return Ok(empty_cache_index());
    }

    let file = std::fs::File::open(&layout.index_file)
        .with_context(|| format!("failed to open cache index {}", layout.index_file.display()))?;
    let raw = read_bounded_reader(file, MAX_CACHE_INDEX_BYTES)
        .with_context(|| format!("failed to read cache index {}", layout.index_file.display()))?;
    let parsed = serde_json::from_slice::<CacheIndexDocument>(&raw).with_context(|| {
        format!(
            "failed to parse cache index {}",
            layout.index_file.display()
        )
    })?;

    if parsed.version != CACHE_INDEX_VERSION {
        bail!(
            "unsupported cache index version {} in {}",
            parsed.version,
            layout.index_file.display()
        );
    }
    if parsed.entries.len() > MAX_DISK_CACHE_INDEX_ENTRIES {
        bail!(
            "cache index {} has too many entries",
            layout.index_file.display()
        );
    }
    let mut max_access = 0_u64;
    let mut total_bytes = 0_u64;
    for (resource_key, entry) in &parsed.entries {
        let (_, kind, _) = parse_resource_key(resource_key)?;
        if entry.size_bytes > kind.max_bytes() {
            bail!("cache index entry `{resource_key}` exceeds its resource size bound");
        }
        max_access = max_access.max(entry.last_access);
        total_bytes = total_bytes
            .checked_add(entry.size_bytes)
            .ok_or_else(|| anyhow!("cache index byte accounting overflow"))?;
    }
    if parsed.access_clock < max_access {
        bail!(
            "cache index {} has a regressing access clock",
            layout.index_file.display()
        );
    }

    Ok(parsed)
}

fn empty_cache_index() -> CacheIndexDocument {
    CacheIndexDocument {
        version: CACHE_INDEX_VERSION,
        access_clock: 0,
        entries: BTreeMap::new(),
    }
}

fn write_cache_index(layout: &DiskCacheLayout, index: &CacheIndexDocument) -> Result<()> {
    let raw = serde_json::to_vec(index).context("failed to serialize cache index")?;
    ensure_resource_size(raw.len(), MAX_CACHE_INDEX_BYTES, "cache index")?;
    write_atomic(&layout.index_file, &raw).with_context(|| {
        format!(
            "failed to atomically write global cache index {}",
            layout.index_file.display()
        )
    })
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    static NEXT_ATOMIC_WRITE: AtomicU64 = AtomicU64::new(1);

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create dir {}", parent.display()))?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let sequence = NEXT_ATOMIC_WRITE.fetch_add(1, Ordering::Relaxed);
    let temp_path = path.with_extension(format!("tmp-{}-{nonce}-{sequence}", std::process::id()));

    let mut temp = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
    temp.write_all(content)
        .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
    temp.sync_all()
        .with_context(|| format!("failed to sync temp file {}", temp_path.display()))?;
    drop(temp);
    if let Err(error) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| {
            format!(
                "failed to move temp file {} to {}",
                temp_path.display(),
                path.display()
            )
        });
    }

    #[cfg(unix)]
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync cache dir {}", parent.display()))?;

    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
}

fn validate_content_hash(hash: &str) -> Result<&str> {
    validate_sha256(hash).map_err(anyhow::Error::new)?;
    Ok(hash)
}

fn verify_content_hash(bytes: &[u8], expected_hash: &str, source: &str) -> Result<()> {
    let actual_hash = sha256_hex(bytes);
    if actual_hash != expected_hash {
        bail!("{source} content hash mismatch: expected {expected_hash}, got {actual_hash}");
    }
    Ok(())
}

fn verify_downloaded_resource(
    kind: RemoteResourceKind,
    resource_id: &str,
    expected_hash: &str,
    bytes: Arc<[u8]>,
) -> Result<Arc<[u8]>> {
    let downloaded_hash = sha256_hex(bytes.as_ref());
    if downloaded_hash != expected_hash {
        bail!(
            "remote {} {} hash mismatch: expected {}, got {}",
            kind.route_segment(),
            resource_id,
            expected_hash,
            downloaded_hash
        );
    }
    Ok(bytes)
}

fn ensure_resource_size(length: usize, max_bytes: u64, source: &str) -> Result<()> {
    if u64::try_from(length).unwrap_or(u64::MAX) > max_bytes {
        bail!("{source} resource exceeds {max_bytes} bytes");
    }
    Ok(())
}

fn verify_canonical_chart_hash(
    chart: &CanonicalChart,
    expected_hash: &str,
    source_path: &Path,
    course_index: usize,
) -> Result<()> {
    validate_content_hash(expected_hash).context("invalid expected canonical chart hash")?;
    let actual_hash = canonical_chart_hash(chart)?;
    if actual_hash != expected_hash {
        bail!(
            "canonical chart hash mismatch for {} course {}: expected {}, got {}",
            source_path.display(),
            course_index,
            expected_hash,
            actual_hash
        );
    }
    Ok(())
}

fn verify_course_summary(
    chart: &CanonicalChart,
    expected: &CourseEntry,
    source_path: &Path,
    course_index: usize,
) -> Result<()> {
    let actual_name = chart
        .metadata
        .difficulty_name
        .clone()
        .unwrap_or_else(|| format!("Course {}", course_index + 1));
    let actual_base_bpm = chart
        .tempo_map
        .first()
        .map(|tempo| 60_000_000.0 / tempo.micros_per_quarter as f64);
    let actual_branch_decisions = build_branch_decision_table(chart);
    if expected.name != actual_name
        || expected.level != chart.metadata.difficulty_level
        || expected.object_count != chart.objects.len()
        || expected.branch_segment_count != chart.branch_segments.len()
        || expected.base_bpm != actual_base_bpm
        || expected.branch_decisions != actual_branch_decisions
    {
        bail!(
            "course summary mismatch for {} course {}",
            source_path.display(),
            course_index
        );
    }
    Ok(())
}

fn verify_song_summary(imported: &ImportedSong, expected: &SongEntry) -> Result<()> {
    let actual_title = if imported.title.trim().is_empty() {
        expected
            .source_path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map_or_else(|| "Untitled".to_owned(), ToOwned::to_owned)
    } else {
        imported.title.clone()
    };
    let actual_demo_start = imported.demo_start_seconds.unwrap_or(0.0);
    if expected.title != actual_title
        || expected.subtitle != imported.subtitle
        || expected.artist != imported.artist
        || expected.demo_start_seconds != actual_demo_start
    {
        bail!(
            "song summary mismatch for {}",
            expected.source_path().display()
        );
    }
    Ok(())
}

fn cache_root_dir_internal() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("com", "rhythm-rs", "taiko-game")
        .ok_or_else(|| anyhow!("failed to determine app data directory for taiko-game"))?;
    Ok(dirs.data_local_dir().join("remote-resource-cache"))
}

fn endpoint_cache_hash(endpoint: &str) -> Result<String> {
    let mut parsed = Url::parse(endpoint)
        .with_context(|| format!("invalid --resource-endpoint URL: {endpoint}"))?;
    ensure_directory_url(&mut parsed);
    Ok(endpoint_cache_hash_url(&parsed))
}

fn endpoint_cache_hash_url(endpoint: &Url) -> String {
    sha256_hex(endpoint.as_str().as_bytes())
}

fn ensure_directory_url(endpoint: &mut Url) {
    if endpoint.path().ends_with('/') {
        return;
    }

    let mut path = endpoint.path().to_owned();
    path.push('/');
    endpoint.set_path(&path);
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::{Cursor, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Instant;

    use super::*;

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);
    const CACHE_LOCK_CHILD_ROLE_ENV: &str = "TAIKO_TEST_CACHE_LOCK_CHILD_ROLE";
    const CACHE_LOCK_CHILD_ROOT_ENV: &str = "TAIKO_TEST_CACHE_LOCK_CHILD_ROOT";
    const CACHE_LOCK_CHILD_TEST: &str =
        "resource::tests::disk_cache_os_file_lock_serializes_process_writers";
    const CACHE_LOCK_CHILD_TIMEOUT: Duration = Duration::from_secs(10);
    const CACHE_LOCK_BLOCK_OBSERVATION: Duration = Duration::from_millis(250);
    const RESOURCE_TEST_TJA: &[u8] = include_bytes!("../samples/Nosferatu.tja");

    struct CacheLockTestChild {
        label: &'static str,
        child: Option<Child>,
    }

    impl CacheLockTestChild {
        fn spawn(label: &'static str, role: &str, root: &Path) -> Self {
            let executable = std::env::current_exe().expect("locate current test executable");
            let child = Command::new(&executable)
                .arg(CACHE_LOCK_CHILD_TEST)
                .arg("--exact")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(CACHE_LOCK_CHILD_ROLE_ENV, role)
                .env(CACHE_LOCK_CHILD_ROOT_ENV, root)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| {
                    panic!(
                        "failed to spawn {label} child from {}: {error}",
                        executable.display()
                    )
                });
            Self {
                label,
                child: Some(child),
            }
        }

        fn ensure_running(&mut self) {
            let status = self
                .child
                .as_mut()
                .expect("child is available")
                .try_wait()
                .unwrap_or_else(|error| panic!("failed to poll {} child: {error}", self.label));
            if let Some(status) = status {
                self.fail_completed(status, "exited before its expected signal");
            }
        }

        fn wait_success(mut self, timeout: Duration) {
            let deadline = Instant::now() + timeout;
            loop {
                let status = self
                    .child
                    .as_mut()
                    .expect("child is available")
                    .try_wait()
                    .unwrap_or_else(|error| panic!("failed to poll {} child: {error}", self.label));
                if let Some(status) = status {
                    let diagnostics = self.take_diagnostics();
                    assert!(
                        status.success(),
                        "{} child failed with {status}\n{diagnostics}",
                        self.label
                    );
                    return;
                }
                if Instant::now() >= deadline {
                    self.terminate();
                    let diagnostics = self.take_diagnostics();
                    panic!("{} child exceeded {:?}\n{diagnostics}", self.label, timeout);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn fail_timeout(&mut self, context: &str, timeout: Duration) -> ! {
            self.terminate();
            let diagnostics = self.take_diagnostics();
            panic!(
                "{} child did not {context} within {timeout:?}\n{diagnostics}",
                self.label
            );
        }

        fn fail_completed(&mut self, status: ExitStatus, context: &str) -> ! {
            let diagnostics = self.take_diagnostics();
            panic!(
                "{} child {context} with {status}\n{diagnostics}",
                self.label
            );
        }

        fn terminate(&mut self) {
            let Some(child) = self.child.as_mut() else {
                return;
            };
            let _ = child.kill();
            let _ = child.wait();
        }

        fn take_diagnostics(&mut self) -> String {
            let Some(mut child) = self.child.take() else {
                return "(child output already collected)".to_owned();
            };
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            format!("stdout:\n{stdout}\nstderr:\n{stderr}")
        }
    }

    impl Drop for CacheLockTestChild {
        fn drop(&mut self) {
            self.terminate();
        }
    }

    struct TestHttpServer {
        endpoint: String,
        attempts: Arc<AtomicUsize>,
        thread: Option<JoinHandle<()>>,
    }

    impl TestHttpServer {
        fn spawn(
            expected_requests: usize,
            handler: impl Fn(usize, &mut TcpStream) + Send + 'static,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP server");
            let endpoint = format!("http://{}", listener.local_addr().expect("test HTTP addr"));
            let attempts = Arc::new(AtomicUsize::new(0));
            let server_attempts = Arc::clone(&attempts);
            let thread = std::thread::spawn(move || {
                for request_index in 0..expected_requests {
                    let (mut stream, _) = listener.accept().expect("accept test HTTP request");
                    read_http_request_head(&mut stream);
                    server_attempts.fetch_add(1, Ordering::SeqCst);
                    handler(request_index, &mut stream);
                }
            });
            Self {
                endpoint,
                attempts,
                thread: Some(thread),
            }
        }

        fn finish(mut self) {
            self.thread
                .take()
                .expect("test server thread")
                .join()
                .expect("test server did not panic");
        }
    }

    fn read_http_request_head(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set request timeout");
        let mut tail = VecDeque::with_capacity(4);
        loop {
            let mut byte = [0_u8; 1];
            let read = stream.read(&mut byte).expect("read HTTP request");
            assert_ne!(read, 0, "client closed before request headers completed");
            if tail.len() == 4 {
                tail.pop_front();
            }
            tail.push_back(byte[0]);
            if tail.iter().copied().eq(*b"\r\n\r\n") {
                return;
            }
        }
    }

    fn write_http_response_head(
        stream: &mut TcpStream,
        status: &str,
        content_length: u64,
        extra_headers: &str,
    ) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Length: {content_length}\r\n{extra_headers}\r\n"
        )
        .expect("write HTTP response head");
        stream.flush().expect("flush HTTP response head");
    }

    fn test_transport_policy() -> ResourceTransportPolicy {
        ResourceTransportPolicy {
            connect_timeout: Duration::from_millis(50),
            response_header_timeout: Duration::from_millis(500),
            chunk_idle_timeout: Duration::from_millis(100),
            server_stream_base_deadline: Duration::from_millis(500),
            server_stream_min_bytes_per_second: 1,
            server_stream_max_deadline: Duration::from_millis(500),
            stream_deadline_tolerance: Duration::ZERO,
            cancel_poll_interval: Duration::from_millis(5),
            transport_retry_delays: [Duration::from_millis(5), Duration::from_millis(10)],
            busy_retry_budget: Duration::from_millis(500),
            busy_max_attempts: 16,
            busy_retry_delays: [
                Duration::from_millis(5),
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(30),
                Duration::from_millis(30),
            ],
            busy_max_retry_after: Duration::from_millis(30),
            busy_jitter_seed: 0x5eed,
            busy_jitter_max_basis_points: 0,
        }
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taiko-game-resource-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn memory_only_backend() -> RemoteResourceBackend {
        RemoteResourceBackend::new("http://127.0.0.1:1", RemoteCacheMode::MemoryOnly)
            .expect("memory-only backend")
    }

    fn transport_backend(endpoint: &str, policy: ResourceTransportPolicy) -> RemoteResourceBackend {
        RemoteResourceBackend::new_with_transport_policy(
            endpoint,
            RemoteCacheMode::MemoryOnly,
            policy,
        )
        .expect("test transport backend")
    }

    fn disk_backend(root: &TestDir, policy: DiskCachePolicy) -> RemoteResourceBackend {
        let layout = DiskCacheLayout::from_root(root.path().join("cache"));
        let cache = DiskCache::open(layout, policy).expect("open test disk cache");
        let mut backend = memory_only_backend();
        backend.disk_cache = Some(Mutex::new(cache));
        backend
    }

    fn sample_local_song(root: &TestDir) -> SongEntry {
        std::fs::write(root.path().join("Nosferatu.tja"), RESOURCE_TEST_TJA)
            .expect("write sample chart");
        let library = load_local_song_library(root.path()).expect("load sample library");
        assert!(library.warnings.is_empty(), "{:?}", library.warnings);
        library.songs.into_iter().next().expect("sample song")
    }

    fn wait_for_cache_lock_child_signal(
        child: &mut CacheLockTestChild,
        signal: &Path,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            if signal.is_file() {
                return;
            }
            child.ensure_running();
            if Instant::now() >= deadline {
                child.fail_timeout(&format!("create signal {}", signal.display()), timeout);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_cache_lock_release_signal(signal: &Path, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while !signal.is_file() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for cache-lock release signal {}",
                signal.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run_cache_lock_holder_child(root: &Path) {
        let cache_root = root.join("cache");
        let _guard = lock_global_disk_cache_required(&cache_root)
            .expect("holder child must acquire cache-root lock");
        std::fs::write(root.join("holder.ready"), b"locked")
            .expect("holder child must publish ready signal");
        wait_for_cache_lock_release_signal(&root.join("holder.release"), CACHE_LOCK_CHILD_TIMEOUT);
    }

    fn run_cache_lock_writer_child(root: &Path) {
        let cache_root = root.join("cache");
        let layout =
            DiskCacheLayout::for_cache_root(cache_root, sha256_hex(b"process-test-endpoint"));
        let bytes = b"complete cross-process cache payload";
        let hash = sha256_hex(bytes);
        std::fs::write(root.join("writer.attempting"), b"opening")
            .expect("writer child must publish attempt signal");
        let mut cache = DiskCache::open(
            layout,
            DiskCachePolicy {
                max_bytes: 1024,
                max_entries: 4,
            },
        )
        .expect("writer child must open cache after holder releases it");
        std::fs::write(root.join("writer.acquired"), b"opened")
            .expect("writer child must publish acquisition signal");
        cache
            .store(RemoteResourceKind::Audio, &hash, bytes)
            .expect("writer child must atomically store cache entry");
        let loaded = cache
            .load(RemoteResourceKind::Audio, &hash)
            .expect("writer child must read its cache entry")
            .expect("writer child cache entry must exist");
        assert_eq!(loaded.as_ref(), bytes);
        std::fs::write(root.join("writer.done"), b"stored")
            .expect("writer child must publish completion signal");
    }

    #[test]
    fn ensure_directory_url_adds_trailing_slash() {
        let mut url = Url::parse("http://127.0.0.1:4150/api").expect("url");
        ensure_directory_url(&mut url);
        assert_eq!(url.as_str(), "http://127.0.0.1:4150/api/");
    }

    #[test]
    fn sha256_hex_is_stable() {
        let a = sha256_hex(b"abc");
        let b = sha256_hex(b"abc");
        let c = sha256_hex(b"abcd");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn remote_cache_defaults_to_global_disk_and_only_explicitly_uses_memory() {
        assert_eq!(remote_cache_mode(false), RemoteCacheMode::AppData);
        assert_eq!(remote_cache_mode(true), RemoteCacheMode::MemoryOnly);
    }

    #[test]
    fn local_fetches_use_only_typed_local_paths() {
        let root = TestDir::new("local-origin-fetch");
        let song = sample_local_song(&root);
        let backend = ResourceBackend::local(root.path().to_path_buf());

        backend
            .load_course_chart(&song, 0, &TjaImporter)
            .expect("load local chart");
        let audio = backend.load_song_audio(&song).expect("load local audio");
        let Some(SongAudioSource::FilePath(audio_path)) = audio else {
            panic!("local audio must remain a file path");
        };
        assert_eq!(Some(audio_path.as_path()), song.audio_path());
        assert!(song.remote_identity().is_none());
    }

    #[test]
    fn audio_absence_crosses_local_and_remote_resource_boundaries_without_io() {
        let root = TestDir::new("silent-origin-fetch");
        let mut local_song = sample_local_song(&root);
        let source_path = local_song.source_path().to_path_buf();
        local_song.origin = SongOrigin::Local {
            source_path,
            audio_path: None,
        };
        let local = ResourceBackend::local(root.path().to_path_buf());
        assert!(local
            .load_song_audio(&local_song)
            .expect("load silent local audio")
            .is_none());

        let mut remote_song = local_song;
        remote_song.origin = SongOrigin::Remote {
            identity: RemoteSongIdentity {
                song_id: "silent-song".to_owned(),
                source_id: "a".repeat(64),
                audio_id: None,
            },
            source_path: PathBuf::from("pack/silent.tja"),
            audio_path: None,
        };
        let remote = ResourceBackend::Remote(Box::new(memory_only_backend()));
        assert!(remote
            .load_song_audio(&remote_song)
            .expect("load silent remote audio")
            .is_none());

        let SongOrigin::Remote { audio_path, .. } = &mut remote_song.origin else {
            unreachable!()
        };
        *audio_path = Some(PathBuf::from("pack/partial.ogg"));
        assert!(remote
            .load_song_audio(&remote_song)
            .expect_err("partial remote audio locator must fail")
            .to_string()
            .contains("partial audio locator"));
    }

    #[test]
    fn remote_fetches_use_the_atomic_identity_without_network() {
        let root = TestDir::new("remote-origin-fetch");
        let mut song = sample_local_song(&root);
        let chart_bytes: Arc<[u8]> = Arc::from(RESOURCE_TEST_TJA);
        let audio_bytes: Arc<[u8]> = Arc::from(&b"verified audio bytes"[..]);
        let source_id = sha256_hex(chart_bytes.as_ref());
        let audio_id = sha256_hex(audio_bytes.as_ref());
        song.origin = SongOrigin::Remote {
            identity: RemoteSongIdentity {
                song_id: "remote-song".to_owned(),
                source_id: source_id.clone(),
                audio_id: Some(audio_id.clone()),
            },
            source_path: PathBuf::from("pack/Nosferatu.tja"),
            audio_path: Some(PathBuf::from("pack/Nosferatu.ogg")),
        };

        let remote = memory_only_backend();
        remote
            .put_memory_cached(&source_id, Arc::clone(&chart_bytes))
            .expect("cache chart");
        remote
            .put_memory_cached(&audio_id, Arc::clone(&audio_bytes))
            .expect("cache audio");
        let backend = ResourceBackend::Remote(Box::new(remote));

        backend
            .load_course_chart(&song, 0, &TjaImporter)
            .expect("load cached remote chart");
        let audio = backend
            .load_song_audio(&song)
            .expect("load cached remote audio");
        let Some(SongAudioSource::Bytes(actual_audio)) = audio else {
            panic!("remote audio must be fetched as bytes");
        };
        assert_eq!(actual_audio.as_ref(), audio_bytes.as_ref());
        assert!(song.remote_identity().expect("remote identity").matches(
            "remote-song",
            &source_id,
            Some(&audio_id)
        ));
    }

    #[test]
    fn endpoint_cache_hash_normalizes_trailing_slash() {
        let a = endpoint_cache_hash("http://127.0.0.1:4150").expect("hash a");
        let b = endpoint_cache_hash("http://127.0.0.1:4150/").expect("hash b");
        assert_eq!(a, b);
    }

    #[test]
    fn resource_semantics_mismatch_is_rejected() {
        let expected = expected_resource_semantics();
        assert_eq!(
            expected.audio_decoder_semantics_version,
            taiko_audio::AUDIO_DECODER_SEMANTICS_VERSION
        );
        assert_eq!(
            expected.audio_decoder_semantics_sha256,
            taiko_audio::AUDIO_DECODER_SEMANTICS_SHA256
        );
        assert_eq!(
            expected.importer_semantics_version,
            TJA_IMPORTER_SEMANTICS_VERSION
        );
        assert_eq!(
            expected.importer_semantics_sha256,
            TJA_IMPORTER_SEMANTICS_SHA256
        );

        let mut incompatible = expected;
        incompatible.audio_decoder_semantics_version += 1;
        let error = validate_resource_semantics(&incompatible).expect_err("incompatible semantics");
        assert!(error.to_string().contains("resource semantics mismatch"));
    }

    #[test]
    fn canonical_chart_hash_mismatch_is_rejected() {
        let chart = CanonicalChart {
            tempo_map: vec![rhythm_chart::TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            ..CanonicalChart::default()
        };
        let error =
            verify_canonical_chart_hash(&chart, &"0".repeat(64), Path::new("pack/song.tja"), 0)
                .expect_err("mismatched canonical hash");
        assert!(error.to_string().contains("canonical chart hash mismatch"));
    }

    #[test]
    fn course_summary_must_match_verified_canonical_chart() {
        let chart = CanonicalChart {
            tempo_map: vec![rhythm_chart::TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            ..CanonicalChart::default()
        };
        let mut summary = CourseEntry {
            index: 0,
            name: "Course 1".to_owned(),
            level: None,
            canonical_chart_hash: canonical_chart_hash(&chart).expect("canonical hash"),
            object_count: 0,
            branch_segment_count: 0,
            base_bpm: Some(120.0),
            branch_decisions: vec![],
        };
        verify_course_summary(&chart, &summary, Path::new("pack/song.tja"), 0)
            .expect("matching summary");

        summary.object_count = 1;
        let error = verify_course_summary(&chart, &summary, Path::new("pack/song.tja"), 0)
            .expect_err("forged summary");
        assert!(error.to_string().contains("course summary mismatch"));
    }

    #[test]
    fn bounded_reader_rejects_oversized_stream_without_size_hint() {
        let error = read_bounded_reader(Cursor::new(b"12345"), 4).expect_err("oversized stream");
        assert!(error.to_string().contains("exceeds 4 bytes"));
    }

    #[test]
    fn local_course_load_uses_the_bounded_chart_reader() {
        let root = TestDir::new("local-chart-bound");
        let chart_path = root.path().join("oversized.tja");
        std::fs::File::create(&chart_path)
            .expect("create oversized chart")
            .set_len(MAX_CHART_RESPONSE_BYTES.saturating_add(1))
            .expect("make oversized sparse chart");
        let backend = ResourceBackend::Local(LocalResourceBackend {
            songdir: root.path().to_path_buf(),
        });
        let song = SongEntry {
            origin: SongOrigin::Local {
                source_path: chart_path,
                audio_path: Some(root.path().join("unused.ogg")),
            },
            title: "oversized".to_owned(),
            subtitle: String::new(),
            artist: String::new(),
            demo_start_seconds: 0.0,
            courses: Vec::new(),
        };

        let error = backend
            .load_course_chart(&song, 0, &TjaImporter)
            .expect_err("oversized local chart must fail before parsing");

        assert!(format!("{error:#}").contains("byte limit"));
    }

    #[test]
    fn resource_deadline_mirrors_server_policy_with_client_tolerance() {
        let policy = ResourceTransportPolicy::default();
        assert_eq!(policy.connect_timeout, Duration::from_secs(5));
        assert_eq!(
            policy.response_header_timeout,
            Duration::from_secs(5 * 60 + 5)
        );
        assert_eq!(policy.busy_retry_budget, Duration::from_secs(315));
        assert_eq!(policy.busy_max_attempts, 16);
        assert_eq!(policy.busy_retry_delays, RESOURCE_BUSY_RETRY_DELAYS);
        assert_eq!(policy.busy_max_retry_after, Duration::from_secs(30));
        assert_eq!(policy.busy_jitter_max_basis_points, 2_500);
        assert_eq!(
            resource_stream_deadline(Some(0), MAX_AUDIO_RESPONSE_BYTES, policy),
            Duration::from_secs(15)
        );
        assert_eq!(
            resource_stream_deadline(Some(1), MAX_AUDIO_RESPONSE_BYTES, policy),
            Duration::from_secs(16)
        );
        assert_eq!(
            resource_stream_deadline(
                Some(2 * RESOURCE_SERVER_STREAM_MIN_BYTES_PER_SECOND),
                MAX_AUDIO_RESPONSE_BYTES,
                policy
            ),
            Duration::from_secs(17)
        );
        assert_eq!(
            resource_stream_deadline(None, MAX_LIBRARY_RESPONSE_BYTES, policy),
            Duration::from_secs(31)
        );
        assert_eq!(
            resource_stream_deadline(None, MAX_AUDIO_RESPONSE_BYTES, policy),
            Duration::from_secs(271)
        );
    }

    #[test]
    fn busy_admission_can_succeed_after_more_than_three_503_attempts() {
        let server = TestHttpServer::spawn(5, |request_index, stream| {
            if request_index < 4 {
                write_http_response_head(
                    stream,
                    "503 Service Unavailable",
                    0,
                    "Retry-After: 999\r\nConnection: close\r\n",
                );
            } else {
                write_http_response_head(stream, "200 OK", 8, "Connection: close\r\n");
                stream.write_all(b"complete").expect("write success body");
            }
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let body = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect("fifth admission request succeeds");

        assert_eq!(body.as_ref(), b"complete");
        assert_eq!(server.attempts.load(Ordering::SeqCst), 5);
        server.finish();
    }

    #[test]
    fn persistent_busy_server_stops_at_the_admission_attempt_cap() {
        let server = TestHttpServer::spawn(4, |_, stream| {
            write_http_response_head(
                stream,
                "503 Service Unavailable",
                0,
                "Retry-After: 0\r\nConnection: close\r\n",
            );
        });
        let mut policy = test_transport_policy();
        policy.busy_max_attempts = 4;
        policy.busy_retry_budget = Duration::from_secs(2);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("persistent 503 must hit the hard attempt limit");

        assert!(format!("{error:#}").contains(
            "resource server remained busy: reached the 4-attempt admission retry limit after 4 admission attempts"
        ));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 4);
        server.finish();
    }

    #[test]
    fn persistent_busy_server_stops_at_the_monotonic_wall_budget() {
        let server = TestHttpServer::spawn(2, |_, stream| {
            write_http_response_head(
                stream,
                "503 Service Unavailable",
                0,
                "Retry-After: 0\r\nConnection: close\r\n",
            );
        });
        let mut policy = test_transport_policy();
        policy.busy_retry_budget = Duration::from_millis(45);
        policy.busy_retry_delays = [Duration::from_millis(30); 6];
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("persistent 503 must hit the monotonic wall budget");

        assert!(format!("{error:#}")
            .contains("resource server remained busy: exhausted the 45ms admission retry budget"));
        assert!(started.elapsed() < Duration::from_millis(250));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
        server.finish();
    }

    #[test]
    fn busy_jitter_is_seeded_reproducible_additive_and_dispersed() {
        let mut policy = test_transport_policy();
        policy.busy_retry_delays = [Duration::from_secs(1); 6];
        policy.busy_max_retry_after = Duration::from_secs(30);
        policy.busy_jitter_max_basis_points = 2_500;
        policy.busy_jitter_seed = 0x1234_5678_9abc_def0;

        let first = (1..=8)
            .map(|response| busy_retry_delay(policy, response, None))
            .collect::<Vec<_>>();
        let second = (1..=8)
            .map(|response| busy_retry_delay(policy, response, None))
            .collect::<Vec<_>>();
        assert_eq!(first, second, "a fixed seed must reproduce the sequence");
        assert!(first.iter().all(|delay| {
            *delay >= Duration::from_secs(1) && *delay <= Duration::from_millis(1_250)
        }));

        let dispersed = (0_u64..20)
            .map(|seed| {
                let mut client_policy = policy;
                client_policy.busy_jitter_seed = seed;
                busy_retry_delay(client_policy, 1, None).as_nanos()
            })
            .collect::<BTreeSet<_>>();
        assert!(
            dispersed.len() >= 15,
            "twenty deterministic client seeds should not synchronize"
        );

        let retry_after_floor = busy_retry_delay(policy, 1, Some(Duration::from_secs(90)));
        assert!(retry_after_floor >= Duration::from_secs(30));
        assert!(retry_after_floor <= Duration::from_millis(37_500));
    }

    #[test]
    fn transport_failures_after_busy_response_keep_their_own_three_attempt_cap() {
        let server = TestHttpServer::spawn(4, |request_index, stream| {
            if request_index == 0 {
                write_http_response_head(
                    stream,
                    "503 Service Unavailable",
                    0,
                    "Retry-After: 0\r\nConnection: close\r\n",
                );
            } else {
                write_http_response_head(stream, "200 OK", 8, "Connection: close\r\n");
                stream.write_all(b"part").expect("write truncated body");
            }
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("three transport failures after 503 must stop");

        assert!(
            format!("{error:#}").contains("resource transport exhausted 3 consecutive attempts")
        );
        assert_eq!(server.attempts.load(Ordering::SeqCst), 4);
        server.finish();
    }

    #[test]
    fn async_transport_does_not_retry_non_503_http_errors() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            write_http_response_head(
                stream,
                "404 Not Found",
                0,
                "Retry-After: 1\r\nConnection: close\r\n",
            );
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("404 is permanent");

        assert!(format!("{error:#}").contains("404"));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn response_header_timeout_is_retryable_and_bounded() {
        let server = TestHttpServer::spawn(3, |_, stream| {
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set close observation timeout");
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte);
        });
        let mut policy = test_transport_policy();
        policy.response_header_timeout = Duration::from_millis(40);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("missing response headers exhaust retries");

        assert!(format!("{error:#}").contains(&format!(
            "response headers from {url} exceeded the verification-aware 40ms deadline"
        )));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
        server.finish();
    }

    #[test]
    fn legal_header_verification_may_outlive_connect_timeout() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            std::thread::sleep(Duration::from_millis(80));
            write_http_response_head(stream, "200 OK", 2, "Connection: close\r\n");
            stream.write_all(b"ok").expect("write delayed response");
        });
        let mut policy = test_transport_policy();
        policy.connect_timeout = Duration::from_millis(30);
        policy.response_header_timeout = Duration::from_millis(250);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();

        let body = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect("server verification completed within the header deadline");

        assert_eq!(body.as_ref(), b"ok");
        assert!(
            started.elapsed() > policy.connect_timeout,
            "the fixture must take longer than the connector timeout after accept"
        );
        assert!(started.elapsed() < policy.response_header_timeout);
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn partial_transport_body_retries_from_byte_zero() {
        let server = TestHttpServer::spawn(2, |request_index, stream| {
            write_http_response_head(stream, "200 OK", 8, "Connection: close\r\n");
            if request_index == 0 {
                stream.write_all(b"part").expect("write truncated body");
            } else {
                stream.write_all(b"complete").expect("write complete body");
            }
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let body = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect("second full body succeeds");

        assert_eq!(body.as_ref(), b"complete");
        assert_eq!(server.attempts.load(Ordering::SeqCst), 2);
        server.finish();
    }

    #[test]
    fn stalled_async_stream_cancels_and_drops_the_socket_promptly() {
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let server = TestHttpServer::spawn(1, move |_, stream| {
            write_http_response_head(stream, "200 OK", 1024, "Connection: keep-alive\r\n");
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set close observation timeout");
            let mut byte = [0_u8; 1];
            closed_tx
                .send(matches!(stream.read(&mut byte), Ok(0)))
                .expect("report client socket state");
        });
        let mut policy = test_transport_policy();
        policy.chunk_idle_timeout = Duration::from_secs(1);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();
        let is_cancelled = || started.elapsed() >= Duration::from_millis(60);

        let error = backend
            .download_payload_cancellable(&url, 2_048, &is_cancelled)
            .expect_err("cancelled stream");

        assert!(is_resource_load_cancelled(&error));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "cancellation must not wait for the body idle timeout"
        );
        assert!(
            closed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("server observed socket result"),
            "dropping the response future must close the incomplete HTTP socket"
        );
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn async_body_idle_timeout_is_retryable_and_bounded() {
        let server = TestHttpServer::spawn(3, |_, stream| {
            write_http_response_head(stream, "200 OK", 4, "Connection: keep-alive\r\n");
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set close observation timeout");
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte);
        });
        let mut policy = test_transport_policy();
        policy.chunk_idle_timeout = Duration::from_millis(40);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();

        let error = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect_err("idle body exhausts retries");

        assert!(format!("{error:#}").contains("was idle"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
        server.finish();
    }

    #[test]
    fn async_body_absolute_deadline_stops_a_non_idle_dribble() {
        let server = TestHttpServer::spawn(3, |_, stream| {
            write_http_response_head(stream, "200 OK", 100, "Connection: keep-alive\r\n");
            for _ in 0..100 {
                if stream.write_all(b"x").is_err() {
                    break;
                }
                stream.flush().expect("flush dribble byte");
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let mut policy = test_transport_policy();
        policy.chunk_idle_timeout = Duration::from_millis(100);
        policy.server_stream_max_deadline = Duration::from_millis(45);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();

        let error = backend
            .download_payload_cancellable(&url, 128, &never_cancelled)
            .expect_err("absolute transfer deadline exhausts retries");

        assert!(format!("{error:#}").contains("size-aware transfer deadline"));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
        server.finish();
    }

    #[test]
    fn oversized_content_length_is_permanent_and_never_retried() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            write_http_response_head(stream, "200 OK", 5, "Connection: close\r\n");
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let error = backend
            .download_payload_cancellable(&url, 4, &never_cancelled)
            .expect_err("oversized response");

        assert!(format!("{error:#}").contains("exceeds 4 bytes"));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn unknown_length_stream_hard_cap_is_permanent_and_never_retried() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\n12345\r\n0\r\n\r\n",
                )
                .expect("write oversized chunked response");
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");

        let error = backend
            .download_payload_cancellable(&url, 4, &never_cancelled)
            .expect_err("unknown-length response exceeds hard cap");

        assert!(format!("{error:#}").contains("exceeds 4 bytes"));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn downloaded_hash_mismatch_is_rejected_after_one_transport_attempt() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            write_http_response_head(stream, "200 OK", 8, "Connection: close\r\n");
            stream.write_all(b"tampered").expect("write body");
        });
        let backend = transport_backend(&server.endpoint, test_transport_policy());
        let url = backend.api_url("v1/test").expect("test URL");
        let bytes = backend
            .download_payload_cancellable(&url, 16, &never_cancelled)
            .expect("transport succeeds");
        let expected_hash = sha256_hex(b"expected");

        let error = verify_downloaded_resource(
            RemoteResourceKind::Chart,
            &expected_hash,
            &expected_hash,
            bytes,
        )
        .expect_err("hash mismatch must fail after transport");

        assert!(error.to_string().contains("hash mismatch"));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn retry_after_backoff_is_cancellable_before_another_request() {
        let server = TestHttpServer::spawn(1, |_, stream| {
            write_http_response_head(
                stream,
                "503 Service Unavailable",
                0,
                "Retry-After: 2\r\nConnection: close\r\n",
            );
        });
        let mut policy = test_transport_policy();
        policy.busy_max_retry_after = Duration::from_secs(2);
        let backend = transport_backend(&server.endpoint, policy);
        let url = backend.api_url("v1/test").expect("test URL");
        let started = Instant::now();
        let is_cancelled = || started.elapsed() >= Duration::from_millis(60);

        let error = backend
            .download_payload_cancellable(&url, 16, &is_cancelled)
            .expect_err("cancel Retry-After wait");

        assert!(is_resource_load_cancelled(&error));
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
        server.finish();
    }

    #[test]
    fn content_addressed_fetch_rejects_distinct_id_and_hash_without_network() {
        let backend = memory_only_backend();
        let resource_id = "a".repeat(64);
        let expected_hash = "b".repeat(64);
        let error = backend
            .fetch_cached_bytes(RemoteResourceKind::Chart, &resource_id, &expected_hash)
            .expect_err("non-content-addressed id");
        assert!(error.to_string().contains("id must equal"));
    }

    #[test]
    fn memory_cache_tampering_is_detected_before_use() {
        let backend = memory_only_backend();
        let expected_hash = sha256_hex(b"original");
        backend
            .memory_cache
            .lock()
            .expect("memory cache lock")
            .insert(expected_hash.clone(), Arc::from(&b"tampered"[..]));

        let error = backend
            .fetch_cached_bytes(RemoteResourceKind::Chart, &expected_hash, &expected_hash)
            .expect_err("tampered memory cache");
        assert!(error
            .to_string()
            .contains("memory cache content hash mismatch"));
    }

    #[test]
    fn disk_cache_enforces_global_quota_and_deterministic_access_lru() {
        let root = TestDir::new("disk-lru");
        let layout = DiskCacheLayout::from_root(root.path().join("cache"));
        let mut cache = DiskCache::open(
            layout.clone(),
            DiskCachePolicy {
                max_bytes: 4,
                max_entries: 2,
            },
        )
        .expect("open cache");
        let first = sha256_hex(b"aa");
        let second = sha256_hex(b"bb");
        let third = sha256_hex(b"cc");

        cache
            .store(RemoteResourceKind::Chart, &first, b"aa")
            .expect("store first");
        cache
            .store(RemoteResourceKind::Chart, &second, b"bb")
            .expect("store second");
        cache
            .load(RemoteResourceKind::Chart, &first)
            .expect("touch first")
            .expect("first exists");
        cache
            .store(RemoteResourceKind::Chart, &third, b"cc")
            .expect("store third");

        assert_eq!(cache.index.entries.len(), 2);
        assert_eq!(cache.total_bytes().expect("cache total"), 4);
        assert!(cache
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &first)));
        assert!(!cache
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &second)));
        assert!(cache
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &third)));
        assert!(!layout
            .blob_path(RemoteResourceKind::Chart, &second)
            .exists());
    }

    #[test]
    fn corrupt_disk_entry_is_purged_refetched_and_reverified() {
        let root = TestDir::new("disk-corrupt-recovery");
        let backend = disk_backend(
            &root,
            DiskCachePolicy {
                max_bytes: 64,
                max_entries: 4,
            },
        );
        let expected = Arc::<[u8]>::from(&b"original"[..]);
        let expected_hash = sha256_hex(expected.as_ref());
        backend
            .store_disk_cached(RemoteResourceKind::Chart, &expected_hash, expected.as_ref())
            .expect("seed cache");
        let path = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock")
            .layout
            .blob_path(RemoteResourceKind::Chart, &expected_hash);
        std::fs::write(&path, b"tampered").expect("inject same-sized bitrot");

        let mut downloads = 0;
        let recovered = backend
            .fetch_cached_bytes_with(
                RemoteResourceKind::Chart,
                &expected_hash,
                &expected_hash,
                || {
                    downloads += 1;
                    Ok(expected.clone())
                },
            )
            .expect("recover corrupt cache through verified download");

        assert_eq!(downloads, 1);
        assert_eq!(recovered.as_ref(), expected.as_ref());
        assert_eq!(
            std::fs::read(&path).expect("read repaired blob"),
            expected.as_ref()
        );
        let mut cache = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock");
        assert_eq!(
            cache
                .load(RemoteResourceKind::Chart, &expected_hash)
                .expect("load repaired cache")
                .expect("repaired entry")
                .as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn corrupt_disk_entry_with_bad_refetch_fails_closed_and_stays_purged() {
        let root = TestDir::new("disk-corrupt-bad-refetch");
        let backend = disk_backend(
            &root,
            DiskCachePolicy {
                max_bytes: 64,
                max_entries: 4,
            },
        );
        let expected_hash = sha256_hex(b"original");
        backend
            .store_disk_cached(RemoteResourceKind::Chart, &expected_hash, b"original")
            .expect("seed cache");
        let path = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock")
            .layout
            .blob_path(RemoteResourceKind::Chart, &expected_hash);
        std::fs::write(&path, b"tampered").expect("inject bitrot");

        let mut downloads = 0;
        let error = backend
            .fetch_cached_bytes_with(
                RemoteResourceKind::Chart,
                &expected_hash,
                &expected_hash,
                || {
                    downloads += 1;
                    Ok(Arc::<[u8]>::from(&b"still-bad"[..]))
                },
            )
            .expect_err("bad refetch must fail hash verification");

        assert_eq!(downloads, 1);
        assert!(error.to_string().contains("hash mismatch"));
        assert!(!path.exists());
        let cache = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock");
        assert!(!cache.index.entries.contains_key(
            &cache
                .layout
                .entry_key(RemoteResourceKind::Chart, &expected_hash)
        ));
    }

    #[test]
    fn oversized_disk_entry_is_purged_before_verified_refetch() {
        let root = TestDir::new("disk-oversized-recovery");
        let backend = disk_backend(
            &root,
            DiskCachePolicy {
                max_bytes: MAX_CHART_RESPONSE_BYTES.saturating_add(16),
                max_entries: 2,
            },
        );
        let expected_hash = sha256_hex(b"chart");
        backend
            .store_disk_cached(RemoteResourceKind::Chart, &expected_hash, b"chart")
            .expect("seed cache");
        let path = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock")
            .layout
            .blob_path(RemoteResourceKind::Chart, &expected_hash);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open cache blob")
            .set_len(MAX_CHART_RESPONSE_BYTES.saturating_add(1))
            .expect("make sparse oversized blob");

        let mut downloads = 0;
        let recovered = backend
            .fetch_cached_bytes_with(
                RemoteResourceKind::Chart,
                &expected_hash,
                &expected_hash,
                || {
                    downloads += 1;
                    Ok(Arc::<[u8]>::from(&b"chart"[..]))
                },
            )
            .expect("recover oversized cache blob");
        assert_eq!(downloads, 1);
        assert_eq!(recovered.as_ref(), b"chart");
        assert_eq!(std::fs::metadata(path).expect("repaired blob").len(), 5);
    }

    #[test]
    fn disk_cache_preflight_rejects_unadmittable_entry_without_partial_state() {
        let root = TestDir::new("disk-quota-preflight");
        let layout = DiskCacheLayout::from_root(root.path().join("cache"));
        let mut cache = DiskCache::open(
            layout.clone(),
            DiskCachePolicy {
                max_bytes: 3,
                max_entries: 1,
            },
        )
        .expect("open cache");
        let hash = sha256_hex(b"four");

        let error = cache
            .store(RemoteResourceKind::Chart, &hash, b"four")
            .expect_err("entry cannot fit hard quota");
        assert!(error.to_string().contains("hard quota"));
        assert!(cache.index.entries.is_empty());
        assert_eq!(cache.total_bytes().expect("cache total"), 0);
        assert!(!layout.blob_path(RemoteResourceKind::Chart, &hash).exists());
    }

    #[test]
    fn disk_cache_restart_preserves_accounting_and_lru_order() {
        let root = TestDir::new("disk-restart");
        let layout = DiskCacheLayout::from_root(root.path().join("cache"));
        let policy = DiskCachePolicy {
            max_bytes: 4,
            max_entries: 2,
        };
        let first = sha256_hex(b"aa");
        let second = sha256_hex(b"bb");
        let third = sha256_hex(b"cc");
        {
            let mut cache = DiskCache::open(layout.clone(), policy).expect("open initial cache");
            cache
                .store(RemoteResourceKind::Chart, &first, b"aa")
                .expect("store first");
            cache
                .store(RemoteResourceKind::Chart, &second, b"bb")
                .expect("store second");
            cache
                .load(RemoteResourceKind::Chart, &first)
                .expect("touch first")
                .expect("first exists");
        }

        let mut reopened = DiskCache::open(layout.clone(), policy).expect("reopen cache");
        assert_eq!(reopened.index.entries.len(), 2);
        assert_eq!(reopened.total_bytes().expect("cache total"), 4);
        reopened
            .store(RemoteResourceKind::Chart, &third, b"cc")
            .expect("store after restart");
        assert!(reopened
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &first)));
        assert!(!reopened
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &second)));
        assert!(reopened
            .index
            .entries
            .contains_key(&layout.entry_key(RemoteResourceKind::Chart, &third)));
        let persisted = load_cache_index(&layout).expect("reload persisted index");
        assert_eq!(persisted.entries, reopened.index.entries);
        assert_eq!(persisted.access_clock, reopened.index.access_clock);
    }

    #[test]
    fn two_endpoints_compete_under_one_global_root_quota() {
        let root = TestDir::new("disk-global-endpoints");
        let cache_root = root.path().join("cache");
        let first_layout =
            DiskCacheLayout::for_cache_root(cache_root.clone(), sha256_hex(b"first-endpoint"));
        let second_layout =
            DiskCacheLayout::for_cache_root(cache_root, sha256_hex(b"second-endpoint"));
        let policy = DiskCachePolicy {
            max_bytes: 2,
            max_entries: 1,
        };
        let first_hash = sha256_hex(b"aa");
        let second_hash = sha256_hex(b"bb");
        let mut first_cache =
            DiskCache::open(first_layout.clone(), policy).expect("open first endpoint cache");
        first_cache
            .store(RemoteResourceKind::Chart, &first_hash, b"aa")
            .expect("store first endpoint blob");

        let mut second_cache =
            DiskCache::open(second_layout.clone(), policy).expect("open second endpoint cache");
        second_cache
            .store(RemoteResourceKind::Chart, &second_hash, b"bb")
            .expect("store second endpoint blob");

        assert!(!first_layout
            .blob_path(RemoteResourceKind::Chart, &first_hash)
            .exists());
        assert!(second_layout
            .blob_path(RemoteResourceKind::Chart, &second_hash)
            .exists());
        assert!(first_cache
            .load(RemoteResourceKind::Chart, &first_hash)
            .expect("refresh first endpoint")
            .is_none());
        let persisted = load_cache_index(&second_layout).expect("load global index");
        assert_eq!(persisted.entries.len(), 1);
        assert_eq!(
            persisted.entries.keys().next(),
            Some(&second_layout.entry_key(RemoteResourceKind::Chart, &second_hash))
        );
    }

    #[test]
    fn endpoint_clear_removes_only_its_blobs_and_global_index_entries() {
        let root = TestDir::new("disk-clear-endpoint");
        let cache_root = root.path().join("cache");
        let first_endpoint = sha256_hex(b"first-endpoint");
        let second_endpoint = sha256_hex(b"second-endpoint");
        let first_layout =
            DiskCacheLayout::for_cache_root(cache_root.clone(), first_endpoint.clone());
        let second_layout =
            DiskCacheLayout::for_cache_root(cache_root.clone(), second_endpoint.clone());
        let policy = DiskCachePolicy {
            max_bytes: 64,
            max_entries: 8,
        };
        let first_hash = sha256_hex(b"first");
        let second_hash = sha256_hex(b"second");
        DiskCache::open(first_layout.clone(), policy)
            .expect("open first cache")
            .store(RemoteResourceKind::Chart, &first_hash, b"first")
            .expect("store first");
        DiskCache::open(second_layout.clone(), policy)
            .expect("open second cache")
            .store(RemoteResourceKind::Audio, &second_hash, b"second")
            .expect("store second");

        let cleared =
            clear_remote_cache_for_endpoint_at(cache_root.clone(), first_endpoint.clone())
                .expect("clear first endpoint");
        assert_eq!(
            cleared.removed_paths,
            vec![cache_root.join(&first_endpoint)]
        );
        assert!(cleared.missing_paths.is_empty());
        assert!(!cache_root.join(&first_endpoint).exists());
        assert!(cache_root.join(&second_endpoint).exists());

        let index = load_cache_index(&second_layout).expect("load updated global index");
        assert_eq!(index.entries.len(), 1);
        assert!(index
            .entries
            .contains_key(&second_layout.entry_key(RemoteResourceKind::Audio, &second_hash)));
        assert!(cache_root.join(CACHE_LOCK_FILE_NAME).is_file());
    }

    #[test]
    fn clear_all_replaces_global_index_and_keeps_only_coordination_files() {
        let root = TestDir::new("disk-clear-all");
        let cache_root = root.path().join("cache");
        let layout = DiskCacheLayout::for_cache_root(cache_root.clone(), sha256_hex(b"endpoint"));
        let hash = sha256_hex(b"chart");
        DiskCache::open(
            layout.clone(),
            DiskCachePolicy {
                max_bytes: 64,
                max_entries: 8,
            },
        )
        .expect("open cache")
        .store(RemoteResourceKind::Chart, &hash, b"chart")
        .expect("store chart");

        let cleared =
            clear_all_remote_cache_at(cache_root.clone()).expect("clear all endpoint caches");
        assert_eq!(
            cleared.removed_paths,
            vec![cache_root.join(&layout.endpoint_hash)]
        );
        assert!(cleared.missing_paths.is_empty());
        assert!(load_cache_index(&layout)
            .expect("load cleared index")
            .entries
            .is_empty());
        assert!(cache_root.join(CACHE_LOCK_FILE_NAME).is_file());
        assert!(layout.index_file.is_file());
    }

    #[test]
    fn busy_cross_process_cache_is_bounded_cancellable_and_recovers_after_release() {
        let root = TestDir::new("disk-cross-process-bounded");
        let cache_root = root.path().join("cache");
        let layout = DiskCacheLayout::from_root(cache_root.clone());
        let mut backend = disk_backend(
            &root,
            DiskCachePolicy {
                max_bytes: 1024,
                max_entries: 8,
            },
        );
        let short_lock_policy = DiskCacheLockPolicy {
            timeout: Duration::from_millis(100),
            poll_interval: Duration::from_millis(5),
        };
        backend.disk_cache_lock_policy = short_lock_policy;

        let holder_ready = root.path().join("holder.ready");
        let holder_release = root.path().join("holder.release");
        let mut holder = CacheLockTestChild::spawn("bounded holder", "holder", root.path());
        wait_for_cache_lock_child_signal(&mut holder, &holder_ready, CACHE_LOCK_CHILD_TIMEOUT);

        let deferred_layout = DiskCacheLayout::for_cache_root(
            cache_root.clone(),
            sha256_hex(b"deferred-cache-endpoint"),
        );
        let deferred_started = Instant::now();
        let mut deferred_cache = DiskCache::open_with_lock_policy(
            deferred_layout.clone(),
            DiskCachePolicy {
                max_bytes: 1024,
                max_entries: 8,
            },
            short_lock_policy,
        )
        .expect("busy initialization must defer instead of blocking startup");
        assert!(
            deferred_started.elapsed() < Duration::from_secs(1),
            "deferred initialization exceeded its bounded lock wait"
        );
        assert!(deferred_cache.index.entries.is_empty());
        assert!(
            !deferred_layout.chart_dir.exists(),
            "deferred initialization mutated an endpoint directory without the lock"
        );

        let downloaded = Arc::<[u8]>::from(&b"verified network payload"[..]);
        let downloaded_hash = sha256_hex(downloaded.as_ref());
        let download_attempts = AtomicUsize::new(0);
        let fetch_started = Instant::now();
        let fetched = backend
            .fetch_cached_bytes_with_cancellation(
                RemoteResourceKind::Audio,
                &downloaded_hash,
                &downloaded_hash,
                &never_cancelled,
                || {
                    download_attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(Arc::clone(&downloaded))
                },
            )
            .expect("a busy disk cache must fall through to verified network bytes");
        assert_eq!(fetched.as_ref(), downloaded.as_ref());
        assert_eq!(download_attempts.load(Ordering::SeqCst), 1);
        assert!(
            fetch_started.elapsed() < Duration::from_secs(1),
            "cache miss and persistence skip exceeded their bounded lock waits"
        );
        assert!(
            !layout
                .blob_path(RemoteResourceKind::Audio, &downloaded_hash)
                .exists(),
            "a timed-out persistence attempt mutated the locked cache"
        );
        assert_eq!(
            backend
                .get_memory_cached(&downloaded_hash)
                .expect("read memory cache")
                .expect("verified download remains in memory")
                .as_ref(),
            downloaded.as_ref()
        );

        let cancelled_bytes = Arc::<[u8]>::from(&b"cancelled payload"[..]);
        let cancelled_hash = sha256_hex(cancelled_bytes.as_ref());
        let cancelled_downloads = AtomicUsize::new(0);
        let cancellation_started = Instant::now();
        let error = backend
            .fetch_cached_bytes_with_cancellation(
                RemoteResourceKind::Audio,
                &cancelled_hash,
                &cancelled_hash,
                &|| cancellation_started.elapsed() >= Duration::from_millis(30),
                || {
                    cancelled_downloads.fetch_add(1, Ordering::SeqCst);
                    Ok(Arc::clone(&cancelled_bytes))
                },
            )
            .expect_err("cache-lock cancellation must stop before network fallback");
        assert!(is_resource_load_cancelled(&error));
        assert_eq!(cancelled_downloads.load(Ordering::SeqCst), 0);
        assert!(
            cancellation_started.elapsed() < Duration::from_secs(1),
            "cache-lock cancellation was not observed promptly"
        );

        std::fs::write(&holder_release, b"release").expect("publish cache-lock release signal");
        holder.wait_success(CACHE_LOCK_CHILD_TIMEOUT);
        let deferred_bytes = b"deferred cache recovers";
        let deferred_hash = sha256_hex(deferred_bytes);
        deferred_cache
            .store(RemoteResourceKind::Chart, &deferred_hash, deferred_bytes)
            .expect("deferred cache initializes and stores after holder release");
        assert_eq!(
            deferred_cache
                .load(RemoteResourceKind::Chart, &deferred_hash)
                .expect("read initialized deferred cache")
                .expect("deferred cache entry exists")
                .as_ref(),
            deferred_bytes
        );

        backend.disk_cache_lock_policy = DiskCacheLockPolicy::PRODUCTION;
        backend
            .store_disk_cached(
                RemoteResourceKind::Audio,
                &downloaded_hash,
                downloaded.as_ref(),
            )
            .expect("persistence recovers after holder release");
        let persisted = backend
            .load_disk_cached(RemoteResourceKind::Audio, &downloaded_hash)
            .expect("read recovered disk cache")
            .expect("recovered cache entry exists");
        assert_eq!(persisted.as_ref(), downloaded.as_ref());
        assert_eq!(
            std::fs::read(layout.blob_path(RemoteResourceKind::Audio, &downloaded_hash))
                .expect("read recovered cache blob"),
            downloaded.as_ref()
        );
    }

    #[test]
    fn disk_cache_os_file_lock_serializes_process_writers() {
        if let Some(role) = std::env::var_os(CACHE_LOCK_CHILD_ROLE_ENV) {
            let root = PathBuf::from(
                std::env::var_os(CACHE_LOCK_CHILD_ROOT_ENV)
                    .expect("cache-lock child root environment variable"),
            );
            match role.to_str().expect("cache-lock child role must be UTF-8") {
                "holder" => run_cache_lock_holder_child(&root),
                "writer" => run_cache_lock_writer_child(&root),
                unexpected => panic!("unexpected cache-lock child role `{unexpected}`"),
            }
            return;
        }

        let root = TestDir::new("disk-cross-process");
        let cache_root = root.path().join("cache");
        let holder_ready = root.path().join("holder.ready");
        let holder_release = root.path().join("holder.release");
        let writer_attempting = root.path().join("writer.attempting");
        let writer_acquired = root.path().join("writer.acquired");
        let writer_done = root.path().join("writer.done");

        let mut holder = CacheLockTestChild::spawn("holder", "holder", root.path());
        wait_for_cache_lock_child_signal(&mut holder, &holder_ready, CACHE_LOCK_CHILD_TIMEOUT);

        let lock_probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(cache_root.join(CACHE_LOCK_FILE_NAME))
            .expect("open holder's cache-root lock file");
        let lock_error = lock_probe
            .try_lock()
            .expect_err("holder process must own the OS cache-root lock");
        assert!(
            matches!(lock_error, std::fs::TryLockError::WouldBlock),
            "holder lock probe failed for an unexpected reason: {lock_error}"
        );

        let mut writer = CacheLockTestChild::spawn("writer", "writer", root.path());
        wait_for_cache_lock_child_signal(&mut writer, &writer_attempting, CACHE_LOCK_CHILD_TIMEOUT);

        let blocked_until = Instant::now() + CACHE_LOCK_BLOCK_OBSERVATION;
        while Instant::now() < blocked_until {
            holder.ensure_running();
            writer.ensure_running();
            assert!(
                !writer_acquired.exists(),
                "writer acquired cache root while holder still owned its OS lock"
            );
            assert!(
                !writer_done.exists(),
                "writer completed cache mutation while holder still owned its OS lock"
            );
            assert!(
                !cache_root.join("index-v3.json").exists(),
                "writer created a partial index while blocked on the OS lock"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        std::fs::write(&holder_release, b"release").expect("publish cache-lock release signal");
        holder.wait_success(CACHE_LOCK_CHILD_TIMEOUT);
        writer.wait_success(CACHE_LOCK_CHILD_TIMEOUT);
        assert!(writer_acquired.is_file());
        assert!(writer_done.is_file());

        lock_probe
            .try_lock()
            .expect("cache-root OS lock must be available after both children exit");

        let layout = DiskCacheLayout::for_cache_root(
            cache_root.clone(),
            sha256_hex(b"process-test-endpoint"),
        );
        let bytes = b"complete cross-process cache payload";
        let hash = sha256_hex(bytes);
        let entry_key = layout.entry_key(RemoteResourceKind::Audio, &hash);
        let index = load_cache_index(&layout).expect("load child-written cache index");
        assert_eq!(index.entries.len(), 1);
        assert_eq!(
            index.entries.get(&entry_key).map(|entry| entry.size_bytes),
            Some(u64::try_from(bytes.len()).expect("payload size fits u64"))
        );
        let blob_path = layout.blob_path(RemoteResourceKind::Audio, &hash);
        let blob = std::fs::read(&blob_path).expect("read child-written cache blob");
        assert_eq!(blob.len(), bytes.len());
        assert_eq!(sha256_hex(&blob), hash);
        assert_eq!(blob, bytes);

        let partial_files = WalkDir::new(&cache_root)
            .into_iter()
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("walk cross-process cache root")
            .into_iter()
            .filter(|entry| {
                entry.file_type().is_file() && entry.file_name().to_string_lossy().contains(".tmp-")
            })
            .map(|entry| entry.path().to_path_buf())
            .collect::<Vec<_>>();
        assert!(
            partial_files.is_empty(),
            "atomic cache writes left temporary files: {partial_files:?}"
        );
    }

    #[test]
    fn concurrent_same_hash_writers_leave_one_complete_blob_and_index_entry() {
        let root = TestDir::new("disk-concurrent");
        let backend = Arc::new(disk_backend(
            &root,
            DiskCachePolicy {
                max_bytes: 64,
                max_entries: 4,
            },
        ));
        let bytes = Arc::<[u8]>::from(&b"shared-content"[..]);
        let hash = sha256_hex(bytes.as_ref());
        let mut writers = Vec::new();
        for _ in 0..8 {
            let backend = Arc::clone(&backend);
            let bytes = Arc::clone(&bytes);
            let hash = hash.clone();
            writers.push(std::thread::spawn(move || {
                backend
                    .store_disk_cached(RemoteResourceKind::Audio, &hash, bytes.as_ref())
                    .expect("concurrent store")
            }));
        }
        for writer in writers {
            writer.join().expect("writer thread");
        }

        let cache = backend
            .disk_cache
            .as_ref()
            .expect("disk cache")
            .lock()
            .expect("cache lock");
        assert_eq!(cache.index.entries.len(), 1);
        let path = cache.layout.blob_path(RemoteResourceKind::Audio, &hash);
        assert_eq!(
            std::fs::read(&path).expect("read shared blob"),
            bytes.as_ref()
        );
        let files = std::fs::read_dir(&cache.layout.audio_dir)
            .expect("read audio cache")
            .collect::<std::io::Result<Vec<_>>>()
            .expect("collect cache entries");
        assert_eq!(files.len(), 1, "no atomic-write temp files remain");
    }

    #[test]
    fn memory_cache_evicts_oldest_entries_to_enforce_byte_budget() {
        let mut cache = MemoryCache::new(4);
        cache.insert("a".to_owned(), Arc::from(&b"12"[..]));
        cache.insert("b".to_owned(), Arc::from(&b"34"[..]));
        cache.insert("c".to_owned(), Arc::from(&b"56"[..]));

        assert!(cache.get("a").is_none());
        assert_eq!(cache.get("b").as_deref(), Some(&b"34"[..]));
        assert_eq!(cache.get("c").as_deref(), Some(&b"56"[..]));
        assert_eq!(cache.total_bytes, 4);

        cache.insert("oversized".to_owned(), Arc::from(&b"12345"[..]));
        assert!(cache.get("oversized").is_none());
        assert_eq!(cache.total_bytes, 4);
    }
}
