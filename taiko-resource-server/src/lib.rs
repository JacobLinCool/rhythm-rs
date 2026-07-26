use std::collections::HashMap;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::ConnectInfo;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use clap::Args;
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, Stream, StreamExt};
use rhythm_chart::{CANONICAL_SCHEMA_SHA256, CANONICAL_SCHEMA_VERSION};
use rhythm_core::Mode;
use rhythm_importer_tja::{
    BranchDecisionPoint, ImportedSong, TjaImporter, TJA_IMPORTER_SEMANTICS_SHA256,
    TJA_IMPORTER_SEMANTICS_VERSION,
};
use rhythm_mode_taiko::{
    TaikoBranchController, TaikoBranchPolicy, TaikoMode, TAIKO_RULESET_SHA256,
    TAIKO_RULESET_VERSION,
};
use sha2::{Digest, Sha256};
use taiko_multiplayer_protocol::{
    ClientMessage, ErrorMessage, ProtocolError, ProtocolErrorCode, ServerMessage,
    MAX_WIRE_MESSAGE_BYTES,
};
use taiko_resource_protocol::{
    canonical_chart_sha256, song_manifest_sha256, validate_sha256, ResourceBranchDecisionPoint,
    ResourceCourse, ResourceLibraryDocument, ResourceSemantics, ResourceSong, API_VERSION,
    MAX_AUDIO_RESPONSE_BYTES, MAX_CHART_RESPONSE_BYTES, MAX_COURSES_PER_SONG,
    MAX_LIBRARY_RESPONSE_BYTES, MAX_WARNINGS_PER_LIBRARY, MAX_WARNING_BYTES, WIRE_SCHEMA_SHA256,
};
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use walkdir::WalkDir;

mod multiplayer;

use crate::multiplayer::{
    MultiplayerRegistry, SessionOutbound, SESSION_HANDSHAKE_TIMEOUT, UNAFFILIATED_SESSION_TTL,
};

#[cfg(not(any(unix, windows)))]
compile_error!(
    "taiko-resource-server resource snapshots require Unix unlink semantics or Windows delete-on-close"
);

const WEBSOCKET_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const WEBSOCKET_PROTOCOL_CLOSE_TIMEOUT: Duration = Duration::from_secs(6);
const RESOURCE_STREAM_CHUNK_BYTES: usize = 64 * 1024;
const MAX_CONCURRENT_RESOURCE_STREAMS: usize = 16;
const MAX_RESOURCE_STREAMS_PER_CLIENT: usize = 8;
const RESOURCE_SNAPSHOT_BUDGET_BYTES: u64 = 512 * 1024 * 1024;
const RESOURCE_SNAPSHOT_BUDGET_UNITS: usize =
    RESOURCE_SNAPSHOT_BUDGET_BYTES as usize / RESOURCE_STREAM_CHUNK_BYTES;
const RESOURCE_STREAM_RETRY_AFTER_SECONDS: u64 = 1;
const RESOURCE_VERIFICATION_BASE_DEADLINE: Duration = Duration::from_secs(10);
const RESOURCE_VERIFICATION_MIN_BYTES_PER_SECOND: u64 = 1024 * 1024;
const RESOURCE_VERIFICATION_MAX_DEADLINE: Duration = Duration::from_secs(5 * 60);
const RESOURCE_STREAM_BASE_DEADLINE: Duration = Duration::from_secs(10);
const RESOURCE_STREAM_MIN_BYTES_PER_SECOND: u64 = 1024 * 1024;
const RESOURCE_STREAM_MAX_DEADLINE: Duration = Duration::from_secs(5 * 60);
const MAX_CATALOG_CHART_FILES: usize = 4_096;
const MAX_CANONICAL_CHART_BYTES_PER_COURSE: u64 = 8 * 1024 * 1024;
const MAX_CANONICAL_CHART_BYTES_TOTAL: u64 = 64 * 1024 * 1024;
const MAX_CANONICAL_OBJECTS_PER_COURSE: usize = 250_000;
const MAX_CANONICAL_OBJECTS_TOTAL: usize = 1_000_000;
const _: () = assert!(taiko_audio::MAX_ENCODED_AUDIO_BYTES == MAX_AUDIO_RESPONSE_BYTES);
const _: () =
    assert!(RESOURCE_SNAPSHOT_BUDGET_BYTES.is_multiple_of(RESOURCE_STREAM_CHUNK_BYTES as u64));
const _: () = assert!(MAX_AUDIO_RESPONSE_BYTES <= RESOURCE_SNAPSHOT_BUDGET_BYTES);

#[derive(Debug, Clone, Copy)]
struct ResourceStreamPolicy {
    verification_base_deadline: Duration,
    verification_minimum_bytes_per_second: u64,
    verification_max_deadline: Duration,
    transfer_base_deadline: Duration,
    minimum_bytes_per_second: u64,
    transfer_max_deadline: Duration,
}

impl ResourceStreamPolicy {
    const PRODUCTION: Self = Self {
        verification_base_deadline: RESOURCE_VERIFICATION_BASE_DEADLINE,
        verification_minimum_bytes_per_second: RESOURCE_VERIFICATION_MIN_BYTES_PER_SECOND,
        verification_max_deadline: RESOURCE_VERIFICATION_MAX_DEADLINE,
        transfer_base_deadline: RESOURCE_STREAM_BASE_DEADLINE,
        minimum_bytes_per_second: RESOURCE_STREAM_MIN_BYTES_PER_SECOND,
        transfer_max_deadline: RESOURCE_STREAM_MAX_DEADLINE,
    };

    fn verification_deadline(self, content_length: u64) -> Duration {
        size_aware_deadline(
            self.verification_base_deadline,
            self.verification_minimum_bytes_per_second,
            self.verification_max_deadline,
            content_length,
        )
    }

    fn transfer_deadline(self, content_length: u64) -> Duration {
        size_aware_deadline(
            self.transfer_base_deadline,
            self.minimum_bytes_per_second,
            self.transfer_max_deadline,
            content_length,
        )
    }

    #[cfg(test)]
    fn fixed(verification_deadline: Duration, transfer_deadline: Duration) -> Self {
        Self {
            verification_base_deadline: verification_deadline,
            verification_minimum_bytes_per_second: 1,
            verification_max_deadline: verification_deadline,
            transfer_base_deadline: transfer_deadline,
            minimum_bytes_per_second: 1,
            transfer_max_deadline: transfer_deadline,
        }
    }
}

fn size_aware_deadline(
    base: Duration,
    minimum_bytes_per_second: u64,
    maximum: Duration,
    content_length: u64,
) -> Duration {
    debug_assert!(minimum_bytes_per_second > 0);
    let content_seconds =
        content_length.saturating_add(minimum_bytes_per_second - 1) / minimum_bytes_per_second;
    base.saturating_add(Duration::from_secs(content_seconds))
        .min(maximum)
}

#[derive(Debug, Clone, Args)]
pub struct ServerArgs {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "./taiko-game/songs",
        help = "Song directory; scanned recursively for .tja"
    )]
    pub songdir: PathBuf,

    #[arg(long, value_name = "HOST", default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, value_name = "PORT", default_value_t = 4150)]
    pub port: u16,
}

#[derive(Clone)]
struct ServerState {
    library: Arc<ResourceLibraryDocument>,
    library_response: CachedLibraryResponse,
    chart_files: Arc<HashMap<String, BlobFile>>,
    audio_files: Arc<HashMap<String, BlobFile>>,
    resource_stream_admission: Arc<ResourceStreamAdmission>,
    authoritative_catalog: AuthoritativeCatalog,
    multiplayer: MultiplayerRegistry,
}

#[derive(Clone)]
struct CachedLibraryResponse {
    body: Arc<Bytes>,
    content_length: HeaderValue,
}

impl CachedLibraryResponse {
    fn new(raw: Vec<u8>) -> Self {
        Self {
            content_length: HeaderValue::try_from(raw.len().to_string())
                .expect("a usize decimal is always a valid HTTP header value"),
            body: Arc::new(Bytes::from(raw)),
        }
    }

    fn response(&self) -> Response {
        let headers = [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_LENGTH, self.content_length.clone()),
        ];
        (headers, Body::from(self.body_clone())).into_response()
    }

    fn body_clone(&self) -> Bytes {
        self.body.as_ref().clone()
    }
}

#[derive(Debug, Clone)]
pub struct AuthoritativeCatalog {
    semantics: ResourceSemantics,
    songs_by_id: Arc<HashMap<String, Arc<AuthoritativeSong>>>,
}

impl AuthoritativeCatalog {
    pub fn semantics(&self) -> &ResourceSemantics {
        &self.semantics
    }

    pub fn len(&self) -> usize {
        self.songs_by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.songs_by_id.is_empty()
    }

    pub fn song(&self, song_id: &str) -> Option<&Arc<AuthoritativeSong>> {
        self.songs_by_id.get(song_id)
    }
}

#[derive(Debug, Clone)]
pub struct AuthoritativeSong {
    manifest: ResourceSong,
    courses: Box<[AuthoritativeCourse]>,
}

impl AuthoritativeSong {
    pub fn manifest(&self) -> &ResourceSong {
        &self.manifest
    }

    pub fn courses(&self) -> &[AuthoritativeCourse] {
        &self.courses
    }
}

#[derive(Debug, Clone)]
pub struct AuthoritativeCourse {
    manifest: ResourceCourse,
    chart: Arc<rhythm_chart::CanonicalChart>,
    branch_decisions: Arc<[BranchDecisionPoint]>,
}

impl AuthoritativeCourse {
    pub fn manifest(&self) -> &ResourceCourse {
        &self.manifest
    }

    pub fn chart(&self) -> &Arc<rhythm_chart::CanonicalChart> {
        &self.chart
    }

    pub fn branch_decisions(&self) -> &[BranchDecisionPoint] {
        &self.branch_decisions
    }
}

#[derive(Debug, Clone)]
struct BlobFile {
    path: PathBuf,
    expected_hash: String,
}

/// A private, immutable-by-construction copy of one indexed resource.
///
/// Verification hashes the bytes while copying them into this server-owned
/// snapshot. The response is then streamed from the snapshot, never from the
/// mutable song-directory inode that was verified.
struct VerifiedResourceSnapshot {
    file: tokio::fs::File,
    content_length: u64,
    snapshot_budget: OwnedSemaphorePermit,
}

impl VerifiedResourceSnapshot {
    async fn open(
        blob: &BlobFile,
        id: &str,
        max_bytes: u64,
        snapshot_budget: Arc<Semaphore>,
        policy: ResourceStreamPolicy,
    ) -> std::result::Result<Self, ResourceHttpError> {
        let preparation = async {
            let file = tokio::fs::File::open(&blob.path).await.map_err(|error| {
                eprintln!("failed to open resource {}: {error}", blob.path.display());
                if error.kind() == io::ErrorKind::NotFound {
                    ResourceHttpError::new(StatusCode::NOT_FOUND, "resource file missing")
                } else {
                    ResourceHttpError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to open resource file",
                    )
                }
            })?;
            let initial_length = file
                .metadata()
                .await
                .map_err(|error| {
                    eprintln!("failed to stat resource {}: {error}", blob.path.display());
                    ResourceHttpError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "failed to stat resource file",
                    )
                })?
                .len();
            if initial_length > max_bytes {
                return Err(resource_changed(id));
            }
            let reservation_units = snapshot_reservation_units(initial_length)?;
            let reservation = snapshot_budget
                .try_acquire_many_owned(reservation_units)
                .map_err(|_| ResourceHttpError::snapshot_capacity_exhausted())?;
            Ok((file, initial_length, reservation))
        };
        let (mut file, initial_length, snapshot_budget) =
            resource_operation_with_deadline(preparation, policy.verification_base_deadline)
                .await?;

        let verification = async {
            let (mut snapshot, _snapshot_path) =
                create_resource_snapshot().await.map_err(|error| {
                    eprintln!("failed to create resource snapshot: {error}");
                    ResourceHttpError::snapshot_unavailable(
                        "failed to create private resource snapshot",
                    )
                })?;
            let mut buffer = vec![0_u8; RESOURCE_STREAM_CHUNK_BYTES];
            let mut hasher = Sha256::new();
            let mut content_length = 0_u64;
            loop {
                let read = file.read(&mut buffer).await.map_err(|error| {
                    eprintln!("failed to verify resource {}: {error}", blob.path.display());
                    ResourceHttpError::snapshot_unavailable("failed to read resource file")
                })?;
                if read == 0 {
                    break;
                }
                let read_len = read;
                let read = u64::try_from(read_len).map_err(|_| resource_changed(id))?;
                content_length = content_length
                    .checked_add(read)
                    .ok_or_else(|| resource_changed(id))?;
                if content_length > initial_length || content_length > max_bytes {
                    return Err(resource_changed(id));
                }
                hasher.update(&buffer[..read_len]);
                snapshot
                    .write_all(&buffer[..read_len])
                    .await
                    .map_err(|error| {
                        eprintln!("failed to write private resource snapshot: {error}");
                        ResourceHttpError::snapshot_unavailable(
                            "failed to write private resource snapshot",
                        )
                    })?;
            }

            let actual_hash = hex::encode(hasher.finalize());
            if content_length != initial_length || actual_hash != blob.expected_hash {
                return Err(resource_changed(id));
            }
            snapshot.flush().await.map_err(|error| {
                eprintln!("failed to flush private resource snapshot: {error}");
                ResourceHttpError::snapshot_unavailable(
                    "failed to prepare private resource snapshot",
                )
            })?;
            snapshot.seek(SeekFrom::Start(0)).await.map_err(|error| {
                eprintln!("failed to rewind private resource snapshot: {error}");
                ResourceHttpError::snapshot_unavailable("failed to prepare resource stream")
            })?;

            Ok(Self {
                file: snapshot,
                content_length,
                snapshot_budget,
            })
        };
        resource_operation_with_deadline(verification, policy.verification_deadline(initial_length))
            .await
    }

    fn into_body(self, permits: ResourceStreamPermits, deadline: Duration) -> Body {
        let Self {
            file,
            content_length,
            snapshot_budget,
        } = self;
        let reader = file.take(content_length);
        let (sender, receiver) = mpsc::channel(1);
        let terminal_error = Arc::new(Mutex::new(None));
        tokio::spawn(produce_resource_body(
            reader,
            sender,
            Arc::clone(&terminal_error),
            permits,
            tokio::time::Instant::now() + deadline,
            snapshot_budget,
        ));
        Body::from_stream(ResourceBodyStream {
            receiver,
            terminal_error,
            finished: false,
        })
    }
}

async fn resource_operation_with_deadline<F, T>(
    operation: F,
    deadline: Duration,
) -> std::result::Result<T, ResourceHttpError>
where
    F: Future<Output = std::result::Result<T, ResourceHttpError>>,
{
    match tokio::time::timeout(deadline, operation).await {
        Ok(result) => result,
        Err(_) => Err(ResourceHttpError::resource_verification_timed_out()),
    }
}

async fn create_resource_snapshot() -> io::Result<(tokio::fs::File, PathBuf)> {
    const CREATE_ATTEMPTS: usize = 4;
    #[cfg(windows)]
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;

    for _ in 0..CREATE_ATTEMPTS {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random)
            .map_err(|error| io::Error::other(format!("snapshot entropy unavailable: {error}")))?;
        let path = std::env::temp_dir().join(format!(
            "taiko-resource-snapshot-{}-{}",
            std::process::id(),
            hex::encode(random)
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        #[cfg(windows)]
        options.custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        match options.open(&path).await {
            Ok(file) => {
                #[cfg(unix)]
                {
                    if let Err(error) = tokio::fs::remove_file(&path).await {
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return Err(error);
                    }
                    return Ok((file, path));
                }
                #[cfg(windows)]
                {
                    return Ok((file, path));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique private resource snapshot",
    ))
}

fn snapshot_reservation_units(content_length: u64) -> std::result::Result<u32, ResourceHttpError> {
    let unit_bytes =
        u64::try_from(RESOURCE_STREAM_CHUNK_BYTES).expect("stream chunk size fits in u64");
    let units = content_length.div_ceil(unit_bytes).max(1);
    u32::try_from(units).map_err(|_| ResourceHttpError::snapshot_capacity_exhausted())
}

#[derive(Debug)]
struct ResourceStreamAdmission {
    global: Arc<Semaphore>,
    snapshot_budget: Arc<Semaphore>,
    per_client: Mutex<HashMap<IpAddr, Weak<Semaphore>>>,
    max_per_client: usize,
}

impl ResourceStreamAdmission {
    fn new(max_global: usize, max_per_client: usize) -> Self {
        assert!(
            max_global > 0,
            "global resource stream limit must be positive"
        );
        assert!(
            max_per_client > 0 && max_per_client <= max_global,
            "per-client resource stream limit must be within the global limit"
        );
        Self {
            global: Arc::new(Semaphore::new(max_global)),
            snapshot_budget: Arc::new(Semaphore::new(RESOURCE_SNAPSHOT_BUDGET_UNITS)),
            per_client: Mutex::new(HashMap::new()),
            max_per_client,
        }
    }

    #[cfg(test)]
    fn with_snapshot_budget_units(
        max_global: usize,
        max_per_client: usize,
        snapshot_budget_units: usize,
    ) -> Self {
        assert!(snapshot_budget_units > 0);
        let mut admission = Self::new(max_global, max_per_client);
        admission.snapshot_budget = Arc::new(Semaphore::new(snapshot_budget_units));
        admission
    }

    fn snapshot_budget(&self) -> Arc<Semaphore> {
        Arc::clone(&self.snapshot_budget)
    }

    fn try_acquire(
        &self,
        client_ip: IpAddr,
    ) -> std::result::Result<ResourceStreamPermits, ResourceHttpError> {
        let client_limiter = {
            let mut clients = self
                .per_client
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            clients.retain(|_, limiter| limiter.strong_count() > 0);
            match clients.get(&client_ip).and_then(Weak::upgrade) {
                Some(limiter) => limiter,
                None => {
                    let limiter = Arc::new(Semaphore::new(self.max_per_client));
                    clients.insert(client_ip, Arc::downgrade(&limiter));
                    limiter
                }
            }
        };
        let client = client_limiter
            .try_acquire_owned()
            .map_err(|_| ResourceHttpError::stream_capacity_exhausted_for_client())?;
        let global = Arc::clone(&self.global)
            .try_acquire_owned()
            .map_err(|_| ResourceHttpError::stream_capacity_exhausted())?;
        Ok(ResourceStreamPermits {
            _global: global,
            _client: client,
        })
    }

    #[cfg(test)]
    fn available_global_permits(&self) -> usize {
        self.global.available_permits()
    }

    #[cfg(test)]
    fn available_snapshot_units(&self) -> usize {
        self.snapshot_budget.available_permits()
    }
}

#[derive(Debug)]
struct ResourceStreamPermits {
    _global: OwnedSemaphorePermit,
    _client: OwnedSemaphorePermit,
}

/// The response-side half of a bounded producer/consumer stream.
///
/// Admission permits live exclusively in the producer task, so its absolute deadline remains
/// effective even while this stream is not being polled by the HTTP stack.
struct ResourceBodyStream {
    receiver: mpsc::Receiver<Bytes>,
    terminal_error: Arc<Mutex<Option<io::Error>>>,
    finished: bool,
}

impl Stream for ResourceBodyStream {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        if self.finished {
            return Poll::Ready(None);
        }
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(None) => {
                let terminal_error = self
                    .terminal_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                self.finished = true;
                match terminal_error {
                    Some(error) => Poll::Ready(Some(Err(error))),
                    None => Poll::Ready(None),
                }
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn produce_resource_body(
    mut reader: tokio::io::Take<tokio::fs::File>,
    sender: mpsc::Sender<Bytes>,
    terminal_error: Arc<Mutex<Option<io::Error>>>,
    permits: ResourceStreamPermits,
    deadline: tokio::time::Instant,
    snapshot_budget: OwnedSemaphorePermit,
) {
    let _permits = permits;
    let _snapshot_budget = snapshot_budget;
    let mut buffer = vec![0_u8; RESOURCE_STREAM_CHUNK_BYTES];
    loop {
        let read = tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                record_resource_stream_error(
                    &terminal_error,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "resource stream exceeded its transfer deadline",
                    ),
                );
                return;
            }
            _ = sender.closed() => return,
            result = reader.read(&mut buffer) => {
                match result {
                    Ok(read) => read,
                    Err(error) => {
                        record_resource_stream_error(&terminal_error, error);
                        return;
                    }
                }
            }
        };
        if read == 0 {
            return;
        }
        let chunk = Bytes::copy_from_slice(&buffer[..read]);
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => {
                record_resource_stream_error(
                    &terminal_error,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "resource stream exceeded its transfer deadline",
                    ),
                );
                return;
            }
            _ = sender.closed() => return,
            result = sender.send(chunk) => {
                if result.is_err() {
                    return;
                }
            }
        }
    }
}

fn record_resource_stream_error(terminal_error: &Mutex<Option<io::Error>>, error: io::Error) {
    *terminal_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
}

#[cfg(test)]
fn resource_stream_deadline(content_length: u64) -> Duration {
    ResourceStreamPolicy::PRODUCTION.transfer_deadline(content_length)
}

fn resource_changed(id: &str) -> ResourceHttpError {
    ResourceHttpError::new(
        StatusCode::CONFLICT,
        format!("resource changed after indexing: {id}"),
    )
}

#[derive(Debug)]
struct ResourceHttpError {
    status: StatusCode,
    message: String,
    retry_after: Option<HeaderValue>,
}

impl ResourceHttpError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }

    fn stream_capacity_exhausted() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "resource stream capacity exhausted".to_owned(),
            retry_after: Some(
                HeaderValue::try_from(RESOURCE_STREAM_RETRY_AFTER_SECONDS.to_string())
                    .expect("a u64 decimal is always a valid HTTP header value"),
            ),
        }
    }

    fn stream_capacity_exhausted_for_client() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: "resource stream capacity exhausted for this client".to_owned(),
            retry_after: Some(
                HeaderValue::try_from(RESOURCE_STREAM_RETRY_AFTER_SECONDS.to_string())
                    .expect("a u64 decimal is always a valid HTTP header value"),
            ),
        }
    }

    fn snapshot_capacity_exhausted() -> Self {
        Self::retryable_unavailable("resource snapshot capacity exhausted")
    }

    fn snapshot_unavailable(message: impl Into<String>) -> Self {
        Self::retryable_unavailable(message)
    }

    fn resource_verification_timed_out() -> Self {
        Self::retryable_unavailable("resource verification timed out")
    }

    fn retryable_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
            retry_after: Some(
                HeaderValue::try_from(RESOURCE_STREAM_RETRY_AFTER_SECONDS.to_string())
                    .expect("a u64 decimal is always a valid HTTP header value"),
            ),
        }
    }
}

impl IntoResponse for ResourceHttpError {
    fn into_response(self) -> Response {
        let mut response = (self.status, self.message).into_response();
        if let Some(retry_after) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, retry_after);
        }
        response
    }
}

#[derive(Debug)]
enum IndexResult {
    Song {
        song: Box<ResourceSong>,
        authoritative_song: Arc<AuthoritativeSong>,
        chart_path: PathBuf,
        audio_path: Option<PathBuf>,
    },
    Warning(String),
}

#[derive(Debug, Clone, Copy, Default)]
struct CatalogFootprint {
    canonical_bytes: u64,
    objects: usize,
}

impl CatalogFootprint {
    fn checked_without(self, previous: Self, replacement: Self) -> Result<CatalogFootprint> {
        let canonical_bytes = self
            .canonical_bytes
            .checked_sub(previous.canonical_bytes)
            .and_then(|value| value.checked_add(replacement.canonical_bytes))
            .context("catalog canonical byte accounting overflowed")?;
        let objects = self
            .objects
            .checked_sub(previous.objects)
            .and_then(|value| value.checked_add(replacement.objects))
            .context("catalog object accounting overflowed")?;
        if canonical_bytes > MAX_CANONICAL_CHART_BYTES_TOTAL {
            bail!(
                "authoritative catalog needs {canonical_bytes} canonical JSON bytes; maximum is \
                 {MAX_CANONICAL_CHART_BYTES_TOTAL}"
            );
        }
        if objects > MAX_CANONICAL_OBJECTS_TOTAL {
            bail!(
                "authoritative catalog contains {objects} chart objects; maximum is \
                 {MAX_CANONICAL_OBJECTS_TOTAL}"
            );
        }
        Ok(Self {
            canonical_bytes,
            objects,
        })
    }
}

struct BoundedByteCounter {
    bytes: u64,
    maximum: u64,
}

impl io::Write for BoundedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = u64::try_from(buffer.len())
            .map_err(|_| io::Error::other("serialized chart length exceeds u64"))?;
        let next = self
            .bytes
            .checked_add(written)
            .ok_or_else(|| io::Error::other("serialized chart length overflowed"))?;
        if next > self.maximum {
            return Err(io::Error::other(format!(
                "canonical chart exceeds {maximum} serialized bytes",
                maximum = self.maximum
            )));
        }
        self.bytes = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn authoritative_song_footprint(song: &AuthoritativeSong) -> Result<CatalogFootprint> {
    let mut footprint = CatalogFootprint::default();
    for course in song.courses() {
        let chart = course.chart();
        if chart.objects.len() > MAX_CANONICAL_OBJECTS_PER_COURSE {
            bail!(
                "course {} contains {} chart objects; maximum is \
                 {MAX_CANONICAL_OBJECTS_PER_COURSE}",
                course.manifest().index,
                chart.objects.len()
            );
        }
        let mut counter = BoundedByteCounter {
            bytes: 0,
            maximum: MAX_CANONICAL_CHART_BYTES_PER_COURSE,
        };
        serde_json::to_writer(&mut counter, chart.as_ref()).with_context(|| {
            format!(
                "course {} canonical chart exceeds the authority memory budget",
                course.manifest().index
            )
        })?;
        footprint.canonical_bytes = footprint
            .canonical_bytes
            .checked_add(counter.bytes)
            .context("song canonical byte accounting overflowed")?;
        footprint.objects = footprint
            .objects
            .checked_add(chart.objects.len())
            .context("song object accounting overflowed")?;
    }
    Ok(footprint)
}

pub fn run_server(args: ServerArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize tokio runtime")?;

    runtime.block_on(run_server_async(args))
}

pub async fn run_server_async(args: ServerArgs) -> Result<()> {
    let (addr, handle) = start_server_background(args).await?;
    println!("taiko-resource-server listening on http://{addr}");
    handle.await.context("server task panicked")??;
    Ok(())
}

pub async fn start_server_background(
    args: ServerArgs,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    start_server_with_shutdown(args, shutdown_signal()).await
}

pub struct ServerShutdown {
    sender: Option<tokio::sync::oneshot::Sender<()>>,
}

impl ServerShutdown {
    pub fn shutdown(mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ServerShutdown {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(());
        }
    }
}

pub async fn start_server_background_controlled(
    args: ServerArgs,
) -> Result<(
    std::net::SocketAddr,
    tokio::task::JoinHandle<Result<()>>,
    ServerShutdown,
)> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let shutdown = async move {
        let _ = receiver.await;
    };
    let (address, handle) = start_server_with_shutdown(args, shutdown).await?;
    Ok((
        address,
        handle,
        ServerShutdown {
            sender: Some(sender),
        },
    ))
}

async fn start_server_with_shutdown(
    args: ServerArgs,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(std::net::SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    let state = build_state(&args.songdir)?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/library", get(get_library))
        .route("/v1/charts/{id}", get(get_chart))
        .route("/v1/audio/{id}", get(get_audio))
        .route("/v2/multiplayer/healthz", get(multiplayer_healthz))
        .route("/v2/multiplayer/ws", get(multiplayer_ws))
        .with_state(state.clone());

    let bind_addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;
    let actual_addr = listener.local_addr()?;

    println!(
        "taiko-resource-server listening on http://{} (songs={}, authoritative_songs={}, warnings={})",
        actual_addr,
        state.library.songs.len(),
        state.authoritative_catalog.len(),
        state.library.warnings.len()
    );

    let handle = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown)
        .await
        .context("HTTP server failed")?;
        Ok(())
    });

    Ok((actual_addr, handle))
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn build_state(songdir: &Path) -> Result<ServerState> {
    if !songdir.exists() {
        bail!("song directory not found: {}", songdir.display());
    }
    let songdir = std::fs::canonicalize(songdir)
        .with_context(|| format!("failed to resolve song directory {}", songdir.display()))?;
    if !songdir.is_dir() {
        bail!("song directory is not a directory: {}", songdir.display());
    }
    let semantics = current_resource_semantics();

    let mut chart_paths = Vec::new();
    for entry in WalkDir::new(&songdir) {
        let entry = entry.with_context(|| {
            format!(
                "failed to scan song directory while indexing {}",
                songdir.display()
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let is_tja = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tja"));
        if !is_tja {
            continue;
        }
        if chart_paths.len() == MAX_CATALOG_CHART_FILES {
            bail!(
                "song directory contains more than {MAX_CATALOG_CHART_FILES} TJA files; split the \
                 catalog or raise the reviewed server limit"
            );
        }
        chart_paths.push(path.to_path_buf());
    }

    chart_paths.sort();

    let mut unique_songs =
        HashMap::<String, (Box<ResourceSong>, Arc<AuthoritativeSong>, CatalogFootprint)>::new();
    let mut catalog_footprint = CatalogFootprint::default();
    let mut warnings = Vec::new();
    let mut chart_files = HashMap::new();
    let mut audio_files = HashMap::new();

    for chart_path in chart_paths {
        match index_song(&songdir, chart_path, &semantics) {
            IndexResult::Song {
                song,
                authoritative_song,
                chart_path,
                audio_path,
            } => {
                chart_files
                    .entry(song.source_id.clone())
                    .or_insert_with(|| BlobFile {
                        path: chart_path,
                        expected_hash: song.source_id.clone(),
                    });
                if let (Some(audio_id), Some(audio_path)) = (&song.audio_id, audio_path) {
                    audio_files
                        .entry(audio_id.clone())
                        .or_insert_with(|| BlobFile {
                            path: audio_path,
                            expected_hash: audio_id.clone(),
                        });
                }
                catalog_footprint = retain_deterministic_song(
                    &mut unique_songs,
                    song,
                    authoritative_song,
                    catalog_footprint,
                )?;
            }
            IndexResult::Warning(warning) => warnings.push(warning),
        }
    }
    warnings = bounded_warnings(warnings);

    let mut authoritative_songs = HashMap::with_capacity(unique_songs.len());
    let mut songs = Vec::with_capacity(unique_songs.len());
    for (song_id, (song, authoritative_song, _)) in unique_songs {
        authoritative_songs.insert(song_id, authoritative_song);
        songs.push(*song);
    }
    songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.source_path.clone()));
    if songs.is_empty() {
        bail!(
            "song directory contains no playable songs ({} chart warning(s)); fix the catalog \
             before starting the authority",
            warnings.len()
        );
    }

    let library = ResourceLibraryDocument {
        api_version: API_VERSION,
        wire_schema_sha256: WIRE_SCHEMA_SHA256.to_owned(),
        semantics,
        songs,
        warnings,
    };
    library
        .validate()
        .context("generated resource library violates the v1 contract")?;
    let library_raw =
        serde_json::to_vec(&library).context("failed to encode generated resource library")?;
    let library_size = u64::try_from(library_raw.len()).unwrap_or(u64::MAX);
    if library_size > MAX_LIBRARY_RESPONSE_BYTES {
        bail!(
            "generated resource library is {library_size} bytes; maximum is {MAX_LIBRARY_RESPONSE_BYTES}"
        );
    }
    let library_response = CachedLibraryResponse::new(library_raw);
    let library = Arc::new(library);
    let authoritative_catalog = AuthoritativeCatalog {
        semantics: library.semantics.clone(),
        songs_by_id: Arc::new(authoritative_songs),
    };
    debug_assert_eq!(authoritative_catalog.len(), library.songs.len());

    let multiplayer = MultiplayerRegistry::try_new(library.as_ref(), authoritative_catalog.clone())
        .context("failed to initialize multiplayer registry")?;

    Ok(ServerState {
        multiplayer,
        library,
        library_response,
        chart_files: Arc::new(chart_files),
        audio_files: Arc::new(audio_files),
        resource_stream_admission: Arc::new(ResourceStreamAdmission::new(
            MAX_CONCURRENT_RESOURCE_STREAMS,
            MAX_RESOURCE_STREAMS_PER_CLIENT,
        )),
        authoritative_catalog,
    })
}

fn retain_deterministic_song(
    songs: &mut HashMap<String, (Box<ResourceSong>, Arc<AuthoritativeSong>, CatalogFootprint)>,
    song: Box<ResourceSong>,
    authoritative_song: Arc<AuthoritativeSong>,
    current_footprint: CatalogFootprint,
) -> Result<CatalogFootprint> {
    let song_id = song.song_id.clone();
    let should_retain = songs
        .get(&song_id)
        .is_none_or(|existing| song.source_path < existing.0.source_path);
    if !should_retain {
        return Ok(current_footprint);
    }
    let replacement_footprint = authoritative_song_footprint(&authoritative_song)?;
    let previous_footprint = songs
        .get(&song_id)
        .map_or_else(CatalogFootprint::default, |existing| existing.2);
    let next_footprint =
        current_footprint.checked_without(previous_footprint, replacement_footprint)?;
    songs.insert(song_id, (song, authoritative_song, replacement_footprint));
    Ok(next_footprint)
}

fn index_song(songdir: &Path, chart_path: PathBuf, semantics: &ResourceSemantics) -> IndexResult {
    let chart_label = normalized_rel_path(songdir, &chart_path)
        .unwrap_or_else(|_| "<invalid-chart-path>".to_owned());
    let chart_path = match std::fs::canonicalize(&chart_path) {
        Ok(path) if path.starts_with(songdir) => path,
        Ok(path) => {
            eprintln!(
                "skip chart {}: resolves outside song directory as {}",
                chart_label,
                path.display()
            );
            return IndexResult::Warning(format!(
                "skip {chart_label}: chart resolves outside song directory"
            ));
        }
        Err(error) => {
            eprintln!("skip chart {}: {error}", chart_path.display());
            return IndexResult::Warning(format!("skip {chart_label}: failed to resolve chart"));
        }
    };
    let raw = match read_file_bounded(&chart_path, MAX_CHART_RESPONSE_BYTES) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("skip chart {}: {error:#}", chart_path.display());
            return IndexResult::Warning(format!("skip {chart_label}: failed to read chart"));
        }
    };

    let importer = TjaImporter;
    let imported = match importer.import_song(&raw) {
        Ok(imported) => imported,
        Err(error) => {
            return IndexResult::Warning(format!("skip {chart_label}: {error}"));
        }
    };

    match build_song(songdir, chart_path.clone(), raw, imported, semantics) {
        Ok((song, audio_path, authoritative_song)) => IndexResult::Song {
            song: Box::new(song),
            authoritative_song: Arc::new(authoritative_song),
            chart_path,
            audio_path,
        },
        Err(error) => {
            eprintln!("skip chart {}: {error:#}", chart_path.display());
            IndexResult::Warning(format!("skip {chart_label}: {error}"))
        }
    }
}

fn build_song(
    songdir: &Path,
    source_path: PathBuf,
    chart_raw: Vec<u8>,
    imported: ImportedSong,
    semantics: &ResourceSemantics,
) -> Result<(ResourceSong, Option<PathBuf>, AuthoritativeSong)> {
    if imported.courses.is_empty() {
        bail!("chart has no playable course");
    }
    if imported.courses.len() > MAX_COURSES_PER_SONG {
        bail!(
            "chart has {} courses; maximum is {MAX_COURSES_PER_SONG}",
            imported.courses.len()
        );
    }

    let source_path =
        std::fs::canonicalize(&source_path).context("failed to resolve chart file")?;
    ensure_within_songdir(songdir, &source_path, "chart")?;
    let parent = source_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("chart path has no parent: {}", source_path.display()))?;
    let source_rel = normalized_rel_path(songdir, &source_path)?;
    let chart_content_hash = sha256_hex(&chart_raw);
    let (audio_path, audio_rel, audio_content_hash) = match imported.audio_path.as_deref() {
        Some(relative_audio_path) => {
            let requested_audio_path = parent.join(relative_audio_path);
            let audio_path = std::fs::canonicalize(&requested_audio_path)
                .context("failed to resolve audio file")?;
            ensure_within_songdir(songdir, &audio_path, "audio")?;
            let audio_rel = normalized_rel_path(songdir, &audio_path)?;
            let audio_content_hash = validate_and_hash_audio_snapshot(&audio_path)?;
            (Some(audio_path), Some(audio_rel), Some(audio_content_hash))
        }
        None => (None, None, None),
    };

    let mut courses = Vec::with_capacity(imported.courses.len());
    let mut authoritative_courses = Vec::with_capacity(imported.courses.len());
    for (index, course) in imported.courses.into_iter().enumerate() {
        <TaikoMode as Mode>::compile(&course.chart)
            .with_context(|| format!("course {} cannot compile for taiko mode", index + 1))?;
        let canonical_chart_hash = canonical_chart_sha256(&course.chart)
            .context("failed to encode canonical chart for hashing")?;
        let name = course
            .chart
            .metadata
            .difficulty_name
            .clone()
            .unwrap_or_else(|| format!("Course {}", index + 1));
        let base_bpm = course
            .chart
            .tempo_map
            .first()
            .map(|tempo| 60_000_000.0 / tempo.micros_per_quarter as f64);
        TaikoBranchController::new(
            TaikoBranchPolicy::Automatic,
            course.branch_decisions.clone(),
        )
        .with_context(|| {
            format!(
                "course {} cannot run with the official multiplayer automatic branch policy",
                index + 1
            )
        })?;
        let branch_decisions = Arc::<[BranchDecisionPoint]>::from(course.branch_decisions);
        let manifest = ResourceCourse {
            index: u32::try_from(index).context("course index exceeds u32")?,
            name,
            level: course.chart.metadata.difficulty_level,
            canonical_chart_hash,
            object_count: u32::try_from(course.chart.objects.len())
                .context("course object count exceeds u32")?,
            branch_segment_count: u32::try_from(course.chart.branch_segments.len())
                .context("branch segment count exceeds u32")?,
            base_bpm,
            branch_decisions: branch_decisions
                .iter()
                .map(|decision| ResourceBranchDecisionPoint {
                    segment_id: decision.segment_id,
                    decision_tick: decision.decision_tick,
                    default_route_id: decision.default_route_id,
                    route_count: decision.route_count,
                    hint: decision.hint.clone(),
                })
                .collect(),
        };
        authoritative_courses.push(AuthoritativeCourse {
            manifest: manifest.clone(),
            chart: Arc::new(course.chart),
            branch_decisions,
        });
        courses.push(manifest);
    }

    let title = if imported.title.trim().is_empty() {
        source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map_or_else(|| "Untitled".to_owned(), ToOwned::to_owned)
    } else {
        imported.title
    };

    let demo_start_seconds = imported.demo_start_seconds.unwrap_or(0.0);
    if !demo_start_seconds.is_finite() || demo_start_seconds < 0.0 {
        bail!("DEMOSTART must be a finite non-negative number");
    }

    let song_id = song_manifest_sha256(
        &chart_content_hash,
        audio_content_hash.as_deref(),
        semantics,
        &courses,
    )
    .context("failed to compute song manifest identity")?;
    let song = ResourceSong {
        song_id,
        source_path: source_rel.clone(),
        source_id: chart_content_hash,
        audio_path: audio_rel,
        audio_id: audio_content_hash,
        title,
        subtitle: imported.subtitle,
        artist: imported.artist,
        demo_start_seconds,
        courses,
    };
    song.validate(semantics)
        .context("song metadata violates the resource v1 contract")?;
    let authoritative_song = AuthoritativeSong {
        manifest: song.clone(),
        courses: authoritative_courses.into_boxed_slice(),
    };

    Ok((song, audio_path, authoritative_song))
}

fn current_resource_semantics() -> ResourceSemantics {
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

fn ensure_within_songdir(songdir: &Path, path: &Path, kind: &str) -> Result<()> {
    if !path.starts_with(songdir) {
        bail!("{kind} path escapes song directory");
    }
    Ok(())
}

fn normalized_rel_path(songdir: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(songdir)
        .with_context(|| "path is outside song directory")?;
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            bail!("resource path is not normalized");
        };
        let component = component
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("resource path is not valid UTF-8"))?;
        if component.contains('\\') || component.chars().any(char::is_control) {
            bail!("resource path contains an unsupported component");
        }
        components.push(component);
    }
    if components.is_empty() {
        bail!("resource path cannot be empty");
    }
    Ok(components.join("/"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn bounded_warnings(mut warnings: Vec<String>) -> Vec<String> {
    let omitted = warnings.len().saturating_sub(MAX_WARNINGS_PER_LIBRARY);
    warnings.truncate(MAX_WARNINGS_PER_LIBRARY);
    for warning in &mut warnings {
        *warning = sanitize_bounded_warning(warning);
    }
    if omitted > 0 {
        let summary = format!("{omitted} additional indexing warnings omitted");
        if let Some(last) = warnings.last_mut() {
            *last = summary;
        }
    }
    warnings
}

fn sanitize_bounded_warning(warning: &str) -> String {
    let mut sanitized = warning
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect::<String>();
    if sanitized.len() <= MAX_WARNING_BYTES {
        return sanitized;
    }

    let suffix = "…";
    let mut boundary = MAX_WARNING_BYTES.saturating_sub(suffix.len());
    while !sanitized.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    sanitized.truncate(boundary);
    sanitized.push_str(suffix);
    sanitized
}

fn read_file_bounded(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open file {}", path.display()))?;
    let length = file
        .metadata()
        .with_context(|| format!("failed to stat file {}", path.display()))?
        .len();
    if length > max_bytes {
        bail!(
            "file {} exceeds maximum size of {max_bytes} bytes",
            path.display()
        );
    }

    use std::io::Read;
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or_default());
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read file {}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max_bytes {
        bail!(
            "file {} exceeds maximum size of {max_bytes} bytes",
            path.display()
        );
    }
    Ok(bytes)
}

fn validate_and_hash_audio_snapshot(path: &Path) -> Result<String> {
    let audio = read_file_bounded(path, MAX_AUDIO_RESPONSE_BYTES)
        .context("failed to read bounded audio snapshot")?;
    let content_hash = sha256_hex(&audio);
    taiko_audio::validate_bytes(audio).context("audio cannot be decoded within gameplay limits")?;
    Ok(content_hash)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn get_library(State(state): State<ServerState>) -> Response {
    state.library_response.response()
}

async fn get_chart(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxumPath(id): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<impl IntoResponse, ResourceHttpError> {
    serve_binary(
        &state.chart_files,
        &state.resource_stream_admission,
        peer.ip(),
        &id,
        "application/octet-stream",
        MAX_CHART_RESPONSE_BYTES,
    )
    .await
}

async fn get_audio(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    AxumPath(id): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<impl IntoResponse, ResourceHttpError> {
    if validate_sha256(&id).is_err() {
        return Err(ResourceHttpError::new(
            StatusCode::BAD_REQUEST,
            "resource id must be a lowercase SHA-256 digest",
        ));
    }
    let Some(blob) = state.audio_files.get(&id) else {
        return Err(ResourceHttpError::new(
            StatusCode::NOT_FOUND,
            "unknown audio id",
        ));
    };
    let content_type = audio_content_type(&blob.path);
    serve_binary(
        &state.audio_files,
        &state.resource_stream_admission,
        peer.ip(),
        &id,
        content_type,
        MAX_AUDIO_RESPONSE_BYTES,
    )
    .await
}

async fn multiplayer_healthz(State(state): State<ServerState>) -> Result<String, StatusCode> {
    let uptime = state.multiplayer.uptime();
    Ok(format!("ok uptime_ms={}", uptime.as_millis()))
}

async fn multiplayer_ws(
    ws: WebSocketUpgrade,
    State(state): State<ServerState>,
) -> impl IntoResponse {
    ws.max_message_size(MAX_WIRE_MESSAGE_BYTES)
        .max_frame_size(MAX_WIRE_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_multiplayer_socket(state, socket))
}

async fn handle_multiplayer_socket(state: ServerState, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let (outbound, mut transport) = SessionOutbound::channel();
    let socket_outbound = outbound.clone();
    let session_id = match state.multiplayer.register_session(outbound).await {
        Ok(session_id) => session_id,
        Err(error) => {
            let _ = send_server_message(&mut sender, &ServerMessage::Fatal(error)).await;
            return;
        }
    };
    let handshake_timeout = tokio::time::sleep(SESSION_HANDSHAKE_TIMEOUT);
    let unaffiliated_timeout = tokio::time::sleep(UNAFFILIATED_SESSION_TTL);
    tokio::pin!(handshake_timeout);
    tokio::pin!(unaffiliated_timeout);
    let mut hello_received = false;
    let mut affiliated = false;

    let mut write_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                changed = transport.close_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let reason = transport.close_rx.borrow_and_update().clone();
                    if let Some(reason) = reason {
                        let _ = send_server_message(
                            &mut sender,
                            &ServerMessage::Fatal(reason),
                        )
                        .await;
                        let _ = tokio::time::timeout(
                            WEBSOCKET_WRITE_TIMEOUT,
                            sender.send(WsMessage::Close(None)),
                        )
                        .await;
                        break;
                    }
                }
                reliable = transport.reliable_rx.recv() => {
                    let Some(message) = reliable else {
                        break;
                    };
                    if let Err(error) = send_server_message(&mut sender, &message).await {
                        eprintln!("failed to send reliable multiplayer message: {error:#}");
                        break;
                    }
                }
                changed = transport.live_rx.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let snapshot = transport.live_rx.borrow_and_update().clone();
                    if let Some(snapshot) = snapshot {
                        let message = ServerMessage::LiveState(snapshot);
                        if let Err(error) = send_server_message(&mut sender, &message).await {
                            eprintln!("failed to send live multiplayer state: {error:#}");
                            break;
                        }
                    }
                }
            }
        }
    });

    let mut protocol_close_requested = false;
    let mut writer_finished = false;
    loop {
        tokio::select! {
            biased;
            writer_result = &mut write_task => {
                writer_finished = true;
                if let Err(error) = writer_result {
                    eprintln!("multiplayer websocket writer task failed: {error}");
                }
                break;
            }
            _ = &mut handshake_timeout, if !hello_received => {
                socket_outbound.close(session_expired_error(
                    "client hello was not received before the handshake deadline",
                ));
                protocol_close_requested = true;
                break;
            }
            _ = &mut unaffiliated_timeout, if !affiliated => {
                socket_outbound.close(session_expired_error(
                    "session did not join or create a room before its admission deadline",
                ));
                protocol_close_requested = true;
                break;
            }
            incoming = receiver.next() => {
                let Some(Ok(incoming)) = incoming else {
                    break;
                };
                match incoming {
                    WsMessage::Text(raw) => match serde_json::from_str::<ClientMessage>(&raw) {
                        Ok(message) => {
                            hello_received |= matches!(&message, ClientMessage::Hello(_));
                            state
                                .multiplayer
                                .handle_client_message(session_id, message)
                                .await;
                            let is_member =
                                state.multiplayer.session_is_member(session_id).await;
                            if affiliated && !is_member {
                                unaffiliated_timeout.as_mut().reset(
                                    tokio::time::Instant::now()
                                        + UNAFFILIATED_SESSION_TTL,
                                );
                            }
                            affiliated = is_member;
                        }
                        Err(error) => {
                            eprintln!("failed to decode multiplayer message: {error}");
                            socket_outbound.close(invalid_transport_message());
                            protocol_close_requested = true;
                            break;
                        }
                    },
                    WsMessage::Binary(_) => {
                        socket_outbound.close(invalid_transport_message());
                        protocol_close_requested = true;
                        break;
                    }
                    WsMessage::Close(_) => break,
                    // Axum automatically responds to WebSocket Ping frames with Pong.
                    WsMessage::Ping(_) | WsMessage::Pong(_) => {}
                }
            }
        }
    }

    state.multiplayer.remove_session(session_id).await;
    if writer_finished {
        return;
    }
    if protocol_close_requested {
        if tokio::time::timeout(WEBSOCKET_PROTOCOL_CLOSE_TIMEOUT, &mut write_task)
            .await
            .is_err()
        {
            write_task.abort();
        }
    } else {
        write_task.abort();
    }
}

async fn send_server_message(
    sender: &mut SplitSink<WebSocket, WsMessage>,
    message: &ServerMessage,
) -> Result<()> {
    let raw = serde_json::to_string(message).context("failed to encode multiplayer message")?;
    tokio::time::timeout(
        WEBSOCKET_WRITE_TIMEOUT,
        sender.send(WsMessage::Text(raw.into())),
    )
    .await
    .context("websocket write timed out")?
    .context("websocket write failed")
}

fn invalid_transport_message() -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::InvalidMessage,
        message: ErrorMessage::new("expected a protocol v2 JSON text message")
            .expect("static protocol error message satisfies its bound"),
        retryable: false,
    }
}

fn session_expired_error(message: &'static str) -> ProtocolError {
    ProtocolError {
        code: ProtocolErrorCode::SessionExpired,
        message: ErrorMessage::new(message)
            .expect("static session expiry message satisfies its protocol bound"),
        retryable: true,
    }
}

async fn serve_binary(
    files: &HashMap<String, BlobFile>,
    resource_stream_admission: &ResourceStreamAdmission,
    client_ip: IpAddr,
    id: &str,
    content_type: &'static str,
    max_bytes: u64,
) -> std::result::Result<Response, ResourceHttpError> {
    serve_binary_with_policy(
        files,
        resource_stream_admission,
        client_ip,
        id,
        content_type,
        max_bytes,
        ResourceStreamPolicy::PRODUCTION,
    )
    .await
}

async fn serve_binary_with_policy(
    files: &HashMap<String, BlobFile>,
    resource_stream_admission: &ResourceStreamAdmission,
    client_ip: IpAddr,
    id: &str,
    content_type: &'static str,
    max_bytes: u64,
    policy: ResourceStreamPolicy,
) -> std::result::Result<Response, ResourceHttpError> {
    if validate_sha256(id).is_err() {
        return Err(ResourceHttpError::new(
            StatusCode::BAD_REQUEST,
            "resource id must be a lowercase SHA-256 digest",
        ));
    }
    let Some(blob) = files.get(id) else {
        return Err(ResourceHttpError::new(
            StatusCode::NOT_FOUND,
            "unknown resource id",
        ));
    };
    if blob.expected_hash != id {
        return Err(ResourceHttpError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "resource index violates content-addressed identity",
        ));
    }

    let permits = resource_stream_admission.try_acquire(client_ip)?;
    let verified = VerifiedResourceSnapshot::open(
        blob,
        id,
        max_bytes,
        resource_stream_admission.snapshot_budget(),
        policy,
    )
    .await?;
    let content_length = verified.content_length;
    let body = verified.into_body(permits, policy.transfer_deadline(content_length));

    let headers = [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (
            header::CONTENT_LENGTH,
            HeaderValue::try_from(content_length.to_string())
                .expect("a u64 decimal is always a valid HTTP header value"),
        ),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        ),
    ];
    Ok((headers, body).into_response())
}

fn audio_content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);

    match ext.as_deref() {
        Some("ogg") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("flac") => "audio/flac",
        Some("opus") => "audio/opus",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;

    use super::*;

    const TEST_TJA_HEADER: &str = "TITLE:Fixture\nBPM:120\n";
    const TEST_AUDIO: &[u8] = include_bytes!("../../taiko-game/assets/don.wav");
    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(1);

    fn test_tja(audio_path: &str) -> Vec<u8> {
        format!("{TEST_TJA_HEADER}WAVE:{audio_path}\nCOURSE:Oni\nLEVEL:1\n#START\n1,\n#END\n")
            .into_bytes()
    }

    fn test_tja_with_unsupported_branch(audio_path: &str) -> Vec<u8> {
        format!(
            "{TEST_TJA_HEADER}WAVE:{audio_path}\nCOURSE:Oni\nLEVEL:1\n#START\n\
             #BRANCHSTART x,1,2\n#N\n1,\n#E\n2,\n#M\n1,\n#BRANCHEND\n#END\n"
        )
        .into_bytes()
    }

    fn test_client_ip() -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    }

    fn test_admission(max_global: usize) -> ResourceStreamAdmission {
        ResourceStreamAdmission::new(max_global, max_global)
    }

    async fn wait_for_global_permits(admission: &ResourceStreamAdmission, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while admission.available_global_permits() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("resource admission did not reach the expected capacity");
    }

    async fn wait_for_snapshot_units(admission: &ResourceStreamAdmission, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while admission.available_snapshot_units() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("snapshot admission did not reach the expected capacity");
    }

    #[derive(Clone)]
    struct TestResourceRouteState {
        slow_files: Arc<HashMap<String, BlobFile>>,
        replacement_files: Arc<HashMap<String, BlobFile>>,
        slow_id: Arc<str>,
        replacement_id: Arc<str>,
        admission: Arc<ResourceStreamAdmission>,
    }

    async fn test_resource_route(
        State(state): State<TestResourceRouteState>,
        ConnectInfo(peer): ConnectInfo<SocketAddr>,
        AxumPath(kind): AxumPath<String>,
    ) -> std::result::Result<Response, ResourceHttpError> {
        let (files, id, policy) = match kind.as_str() {
            "slow" => (
                state.slow_files.as_ref(),
                state.slow_id.as_ref(),
                ResourceStreamPolicy::fixed(Duration::from_secs(2), Duration::from_millis(500)),
            ),
            "replacement" => (
                state.replacement_files.as_ref(),
                state.replacement_id.as_ref(),
                ResourceStreamPolicy::fixed(Duration::from_secs(2), Duration::from_secs(2)),
            ),
            _ => {
                return Err(ResourceHttpError::new(
                    StatusCode::NOT_FOUND,
                    "unknown test resource",
                ));
            }
        };
        serve_binary_with_policy(
            files,
            state.admission.as_ref(),
            peer.ip(),
            id,
            "application/octet-stream",
            u64::MAX,
            policy,
        )
        .await
    }

    async fn open_raw_http_get(address: SocketAddr, path: &str) -> tokio::net::TcpStream {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect test HTTP server");
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write test HTTP request");
        stream
    }

    async fn read_raw_http_headers(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        const HEADER_TERMINATOR: &[u8] = b"\r\n\r\n";
        const MAX_TEST_HEADER_BYTES: usize = 64 * 1024;

        let mut headers = Vec::new();
        while !headers.ends_with(HEADER_TERMINATOR) {
            assert!(
                headers.len() < MAX_TEST_HEADER_BYTES,
                "test HTTP response headers exceeded {MAX_TEST_HEADER_BYTES} bytes"
            );
            let mut byte = [0_u8; 1];
            tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut byte))
                .await
                .expect("HTTP response header deadline")
                .expect("read HTTP response header");
            headers.push(byte[0]);
        }
        headers
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let nonce = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "taiko-resource-server-{label}-{}-{nonce}",
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

    #[test]
    fn normalized_rel_path_uses_songdir_prefix() {
        let songdir = Path::new("/songs");
        let path = Path::new("/songs/pack/a.tja");
        assert_eq!(
            normalized_rel_path(songdir, path).expect("relative path"),
            "pack/a.tja"
        );
    }

    #[test]
    fn normalized_rel_path_rejects_escape() {
        assert!(normalized_rel_path(Path::new("/songs"), Path::new("/other/a.tja")).is_err());
    }

    #[tokio::test]
    async fn repeated_library_responses_share_one_immutable_payload_and_exact_headers() {
        let document = ResourceLibraryDocument {
            api_version: API_VERSION,
            wire_schema_sha256: WIRE_SCHEMA_SHA256.to_owned(),
            semantics: current_resource_semantics(),
            songs: Vec::new(),
            warnings: Vec::new(),
        };
        let raw = serde_json::to_vec(&document).expect("serialize fixture library");
        let cached = CachedLibraryResponse::new(raw.clone());

        let first_shared_body = cached.body_clone();
        let second_shared_body = cached.body_clone();
        assert_eq!(first_shared_body.as_ptr(), second_shared_body.as_ptr());
        assert_eq!(first_shared_body.as_ptr(), cached.body.as_ptr());

        let first = cached.response();
        let second = cached.response();
        for response in [&first, &second] {
            assert_eq!(
                response.headers().get(header::CONTENT_TYPE),
                Some(&HeaderValue::from_static("application/json"))
            );
            assert_eq!(
                response.headers().get(header::CONTENT_LENGTH),
                Some(
                    &HeaderValue::try_from(raw.len().to_string())
                        .expect("fixture length is a valid header")
                )
            );
        }

        let first_raw = axum::body::to_bytes(first.into_body(), raw.len())
            .await
            .expect("read first cached response");
        let second_raw = axum::body::to_bytes(second.into_body(), raw.len())
            .await
            .expect("read second cached response");
        assert_eq!(first_raw, second_raw);
        let decoded: ResourceLibraryDocument =
            serde_json::from_slice(&first_raw).expect("decode cached response");
        assert_eq!(decoded, document);
    }

    #[test]
    fn wave_parent_traversal_is_rejected_during_indexing() {
        let root = TestDir::new("wave-parent-escape");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        std::fs::write(root.path().join("outside.wav"), b"private").expect("write outside audio");
        let chart_path = songdir.join("escape.tja");
        std::fs::write(&chart_path, test_tja("../outside.wav")).expect("write chart");
        let songdir = std::fs::canonicalize(songdir).expect("canonical song directory");

        let IndexResult::Warning(warning) =
            index_song(&songdir, chart_path, &current_resource_semantics())
        else {
            panic!("escaping WAVE path was indexed");
        };
        assert!(
            warning.contains("audio path escapes song directory"),
            "{warning}"
        );
        let root_display = root.path().to_string_lossy();
        assert!(
            !warning.contains(root_display.as_ref()),
            "wire warning leaked server path: {warning}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn wave_symlink_to_outside_songdir_is_rejected_during_indexing() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("wave-symlink-escape");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        let outside_audio = root.path().join("outside.wav");
        std::fs::write(&outside_audio, b"private").expect("write outside audio");
        symlink(&outside_audio, songdir.join("linked.wav")).expect("create audio symlink");
        let chart_path = songdir.join("escape.tja");
        std::fs::write(&chart_path, test_tja("linked.wav")).expect("write chart");
        let songdir = std::fs::canonicalize(songdir).expect("canonical song directory");

        let IndexResult::Warning(warning) =
            index_song(&songdir, chart_path, &current_resource_semantics())
        else {
            panic!("symlinked WAVE path was indexed");
        };
        assert!(
            warning.contains("audio path escapes song directory"),
            "{warning}"
        );
    }

    #[test]
    fn indexed_resource_ids_are_content_hashes() {
        let root = TestDir::new("content-address");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        let audio_path = songdir.join("fixture.wav");
        std::fs::write(&audio_path, TEST_AUDIO).expect("write audio");
        let chart_raw = test_tja("fixture.wav");
        let chart_path = songdir.join("fixture.tja");
        std::fs::write(&chart_path, &chart_raw).expect("write chart");
        let imported = TjaImporter
            .import_song(&chart_raw)
            .expect("import fixture chart");
        let expected_canonical_hash =
            canonical_chart_sha256(&imported.courses[0].chart).expect("canonical hash");
        let songdir = std::fs::canonicalize(songdir).expect("canonical song directory");

        let semantics = current_resource_semantics();
        assert_eq!(
            semantics.audio_decoder_semantics_version,
            taiko_audio::AUDIO_DECODER_SEMANTICS_VERSION
        );
        assert_eq!(
            semantics.audio_decoder_semantics_sha256,
            taiko_audio::AUDIO_DECODER_SEMANTICS_SHA256
        );
        assert_eq!(
            semantics.importer_semantics_version,
            TJA_IMPORTER_SEMANTICS_VERSION
        );
        assert_eq!(
            semantics.importer_semantics_sha256,
            TJA_IMPORTER_SEMANTICS_SHA256
        );
        let (song, indexed_audio_path, authoritative_song) = build_song(
            &songdir,
            chart_path,
            chart_raw.clone(),
            imported,
            &semantics,
        )
        .expect("build song");

        assert_eq!(song.source_id, sha256_hex(&chart_raw));
        assert_eq!(song.audio_id, Some(sha256_hex(TEST_AUDIO)));
        assert_eq!(
            song.song_id,
            song_manifest_sha256(
                &song.source_id,
                song.audio_id.as_deref(),
                &semantics,
                &song.courses,
            )
            .expect("song manifest hash")
        );
        assert_eq!(
            song.courses[0].canonical_chart_hash,
            expected_canonical_hash
        );
        assert_eq!(
            authoritative_song.courses()[0]
                .manifest()
                .canonical_chart_hash,
            expected_canonical_hash
        );
        assert_eq!(
            canonical_chart_sha256(authoritative_song.courses()[0].chart())
                .expect("retained chart hash"),
            expected_canonical_hash
        );
        assert_eq!(
            indexed_audio_path,
            Some(std::fs::canonicalize(audio_path).unwrap())
        );

        let mut later = song.clone();
        later.source_path = "z-pack/fixture.tja".to_owned();
        let mut later_authoritative = authoritative_song.clone();
        later_authoritative.manifest = later.clone();
        let mut earlier = song;
        earlier.source_path = "a-pack/fixture.tja".to_owned();
        let mut earlier_authoritative = authoritative_song;
        earlier_authoritative.manifest = earlier.clone();
        let mut unique = HashMap::new();
        let footprint = retain_deterministic_song(
            &mut unique,
            Box::new(later),
            Arc::new(later_authoritative),
            CatalogFootprint::default(),
        )
        .expect("retain later fixture");
        let footprint = retain_deterministic_song(
            &mut unique,
            Box::new(earlier),
            Arc::new(earlier_authoritative),
            footprint,
        )
        .expect("replace with deterministic earlier fixture");
        let retained = unique.values().next().expect("deduplicated song");
        assert_eq!(unique.len(), 1);
        assert_eq!(retained.0.source_path, "a-pack/fixture.tja");
        assert_eq!(
            retained.0.source_path,
            retained.1.manifest().source_path,
            "library and authoritative catalog must retain the same manifest"
        );
        assert_eq!(footprint.canonical_bytes, retained.2.canonical_bytes);
        assert_eq!(footprint.objects, retained.2.objects);
    }

    #[test]
    fn indexing_rejects_audio_that_cannot_be_decoded_for_gameplay() {
        let root = TestDir::new("corrupt-audio");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        std::fs::write(songdir.join("corrupt.wav"), b"not an audio stream")
            .expect("write corrupt audio");
        let chart_path = songdir.join("corrupt.tja");
        std::fs::write(&chart_path, test_tja("corrupt.wav")).expect("write chart");
        let songdir = std::fs::canonicalize(songdir).expect("canonical song directory");

        let IndexResult::Warning(warning) =
            index_song(&songdir, chart_path, &current_resource_semantics())
        else {
            panic!("corrupt audio was admitted to the server catalog");
        };
        assert!(
            warning.contains("audio cannot be decoded within gameplay limits"),
            "{warning}"
        );
    }

    #[tokio::test]
    async fn indexing_preserves_explicit_audio_absence_without_guessing_same_stem_files() {
        let root = TestDir::new("silent-song");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        let chart_raw = test_tja("");
        let chart_path = songdir.join("silent.tja");
        std::fs::write(&chart_path, &chart_raw).expect("write silent chart");
        std::fs::write(songdir.join("silent.ogg"), b"must never be guessed")
            .expect("write misleading same-stem audio");

        let imported = TjaImporter
            .import_song(&chart_raw)
            .expect("import silent chart");
        let canonical_songdir = std::fs::canonicalize(&songdir).expect("canonical song directory");
        let semantics = current_resource_semantics();
        let (song, indexed_audio_path, _) = build_song(
            &canonical_songdir,
            chart_path,
            chart_raw,
            imported,
            &semantics,
        )
        .expect("index silent song");

        assert_eq!(indexed_audio_path, None);
        assert_eq!(song.audio_path, None);
        assert_eq!(song.audio_id, None);
        assert_eq!(
            song.song_id,
            song_manifest_sha256(&song.source_id, None, &semantics, &song.courses)
                .expect("silent song manifest hash")
        );

        let state = build_state(&songdir).expect("build silent catalog");
        assert_eq!(state.library.songs.len(), 1);
        assert!(state.audio_files.is_empty());
    }

    #[test]
    fn indexing_rejects_an_unsupported_branch_condition_before_catalog_retention() {
        let root = TestDir::new("unsupported-branch");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create song directory");
        std::fs::write(songdir.join("fixture.wav"), TEST_AUDIO).expect("write valid audio");
        let chart_path = songdir.join("unsupported.tja");
        std::fs::write(&chart_path, test_tja_with_unsupported_branch("fixture.wav"))
            .expect("write branching chart");
        let songdir = std::fs::canonicalize(songdir).expect("canonical song directory");

        let IndexResult::Warning(warning) =
            index_song(&songdir, chart_path, &current_resource_semantics())
        else {
            panic!("an unsupported branch condition was indexed");
        };
        assert!(
            warning.contains("unsupported branch condition kind"),
            "{warning}"
        );
    }

    #[test]
    fn authority_refuses_to_start_with_an_empty_playable_catalog() {
        let root = TestDir::new("empty-catalog");
        let songdir = root.path().join("songs");
        std::fs::create_dir(&songdir).expect("create empty song directory");

        let error = match build_state(&songdir) {
            Ok(_) => panic!("empty authority must fail closed"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("contains no playable songs"));
    }

    #[test]
    fn indexing_rejects_oversized_files_before_allocating_them() {
        let root = TestDir::new("bounded-read");
        let path = root.path().join("too-large.bin");
        std::fs::write(&path, b"12345").expect("write fixture");
        let error = read_file_bounded(&path, 4).expect_err("oversized file");
        assert!(error.to_string().contains("exceeds maximum size"));
    }

    #[test]
    fn canonical_catalog_budget_is_checked_before_retention() {
        let current = CatalogFootprint {
            canonical_bytes: MAX_CANONICAL_CHART_BYTES_TOTAL,
            objects: MAX_CANONICAL_OBJECTS_TOTAL,
        };
        let replacement = CatalogFootprint {
            canonical_bytes: 1,
            objects: 1,
        };
        let bytes_error = current
            .checked_without(
                CatalogFootprint {
                    canonical_bytes: 0,
                    objects: 1,
                },
                replacement,
            )
            .expect_err("aggregate canonical byte limit");
        assert!(bytes_error.to_string().contains("canonical JSON bytes"));

        let objects_error = current
            .checked_without(
                CatalogFootprint {
                    canonical_bytes: 1,
                    objects: 0,
                },
                replacement,
            )
            .expect_err("aggregate object limit");
        assert!(objects_error.to_string().contains("chart objects"));
    }

    #[test]
    fn canonical_size_counter_fails_without_retaining_the_serialized_payload() {
        let mut counter = BoundedByteCounter {
            bytes: 0,
            maximum: 3,
        };
        let error = io::Write::write_all(&mut counter, b"four")
            .expect_err("counter must reject the first oversized write");
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(counter.bytes, 0);
        assert!(error.to_string().contains("exceeds 3 serialized bytes"));
    }

    #[test]
    fn resource_stream_admission_preserves_capacity_for_other_clients() {
        let admission = ResourceStreamAdmission::new(2, 1);
        let first_ip = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 1));
        let second_ip = IpAddr::V4(std::net::Ipv4Addr::new(192, 0, 2, 2));

        let first = admission.try_acquire(first_ip).expect("first client slot");
        let error = admission
            .try_acquire(first_ip)
            .expect_err("one client must not consume the other reserved opportunity");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("for this client"));

        let second = admission
            .try_acquire(second_ip)
            .expect("different client can use remaining global slot");
        assert_eq!(admission.available_global_permits(), 0);
        drop(first);
        let replacement = admission
            .try_acquire(first_ip)
            .expect("client can retry after releasing its stream");
        drop(second);
        drop(replacement);
        assert_eq!(admission.available_global_permits(), 2);
    }

    #[tokio::test]
    async fn expired_resource_producer_releases_admission_while_body_is_never_polled() {
        let root = TestDir::new("stream-deadline");
        let path = root.path().join("blob.bin");
        let contents = vec![7_u8; RESOURCE_STREAM_CHUNK_BYTES * 3];
        std::fs::write(&path, &contents).expect("write fixture");
        let expected_hash = sha256_hex(&contents);
        let blob = BlobFile {
            path,
            expected_hash: expected_hash.clone(),
        };
        let admission = test_admission(1);
        let verified = VerifiedResourceSnapshot::open(
            &blob,
            &expected_hash,
            u64::try_from(contents.len()).expect("fixture length"),
            admission.snapshot_budget(),
            ResourceStreamPolicy::PRODUCTION,
        )
        .await
        .expect("verify fixture");
        let permits = admission
            .try_acquire(test_client_ip())
            .expect("stream admission");
        let body = verified.into_body(permits, Duration::from_millis(20));
        assert_eq!(admission.available_global_permits(), 0);

        wait_for_global_permits(&admission, 1).await;
        let replacement = admission
            .try_acquire(test_client_ip())
            .expect("deadline must release capacity for a replacement");
        drop(replacement);
        assert_eq!(admission.available_global_permits(), 1);

        let error = axum::body::to_bytes(body, contents.len())
            .await
            .expect_err("timed-out producer must leave an explicit body error");
        assert!(
            error
                .to_string()
                .contains("resource stream exceeded its transfer deadline"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn partial_snapshot_timeout_is_retryable_and_releases_all_admission() {
        let admission = test_admission(1);
        let snapshot_budget = admission.snapshot_budget();
        let initial_snapshot_units = admission.available_snapshot_units();
        let partial_snapshot_path = Arc::new(Mutex::new(None));
        let operation_snapshot_path = Arc::clone(&partial_snapshot_path);
        let operation = async {
            let _permits = admission
                .try_acquire(test_client_ip())
                .expect("verification admission");
            let _snapshot_budget = snapshot_budget
                .try_acquire_many_owned(1)
                .expect("snapshot budget");
            let (mut snapshot, path) = create_resource_snapshot()
                .await
                .map_err(|error| ResourceHttpError::snapshot_unavailable(error.to_string()))?;
            *operation_snapshot_path
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
            snapshot
                .write_all(b"real partial snapshot")
                .await
                .map_err(|error| ResourceHttpError::snapshot_unavailable(error.to_string()))?;
            std::future::pending::<std::result::Result<(), ResourceHttpError>>().await
        };
        let result = resource_operation_with_deadline(operation, Duration::from_millis(100)).await;
        let Err(error) = result else {
            panic!("stalled verification did not time out");
        };

        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("verification timed out"));
        let response = error.into_response();
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(
                &HeaderValue::try_from(RESOURCE_STREAM_RETRY_AFTER_SECONDS.to_string())
                    .expect("retry delay is a valid header")
            )
        );
        assert_eq!(admission.available_global_permits(), 1);
        assert_eq!(admission.available_snapshot_units(), initial_snapshot_units);
        let path = partial_snapshot_path
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .expect("partial snapshot path was captured");
        assert!(
            !path.exists(),
            "timed-out partial snapshot remained named at {}",
            path.display()
        );
    }

    #[tokio::test]
    async fn snapshot_budget_rejects_excess_disk_reservation_and_recovers_after_drop() {
        let root = TestDir::new("snapshot-budget");
        let path = root.path().join("blob.bin");
        let contents = vec![9_u8; RESOURCE_STREAM_CHUNK_BYTES * 2];
        std::fs::write(&path, &contents).expect("write fixture");
        let expected_hash = sha256_hex(&contents);
        let files = HashMap::from([(
            expected_hash.clone(),
            BlobFile {
                path,
                expected_hash: expected_hash.clone(),
            },
        )]);
        let admission = ResourceStreamAdmission::with_snapshot_budget_units(2, 2, 2);
        let max_bytes = u64::try_from(contents.len()).expect("fixture length fits u64");

        let first = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect("first response reserves the complete snapshot");
        assert_eq!(admission.available_snapshot_units(), 0);

        let error = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect_err("aggregate snapshot budget must reject a second response");
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("snapshot capacity exhausted"));
        assert!(error.retry_after.is_some());
        assert_eq!(admission.available_global_permits(), 1);

        drop(first);
        wait_for_global_permits(&admission, 2).await;
        wait_for_snapshot_units(&admission, 2).await;

        let replacement = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect("released snapshot budget admits a replacement");
        drop(replacement);
        wait_for_global_permits(&admission, 2).await;
        wait_for_snapshot_units(&admission, 2).await;
    }

    #[tokio::test]
    async fn stalled_tcp_reader_expires_and_replacement_request_succeeds() {
        let root = TestDir::new("stalled-tcp-reader");
        let slow_path = root.path().join("slow.bin");
        let slow_contents = vec![5_u8; 16 * 1024 * 1024];
        std::fs::write(&slow_path, &slow_contents).expect("write slow fixture");
        let slow_id = sha256_hex(&slow_contents);
        let replacement_path = root.path().join("replacement.bin");
        let replacement_contents = b"replacement";
        std::fs::write(&replacement_path, replacement_contents).expect("write replacement fixture");
        let replacement_id = sha256_hex(replacement_contents);
        let admission = Arc::new(test_admission(1));
        let state = TestResourceRouteState {
            slow_files: Arc::new(HashMap::from([(
                slow_id.clone(),
                BlobFile {
                    path: slow_path,
                    expected_hash: slow_id.clone(),
                },
            )])),
            replacement_files: Arc::new(HashMap::from([(
                replacement_id.clone(),
                BlobFile {
                    path: replacement_path,
                    expected_hash: replacement_id.clone(),
                },
            )])),
            slow_id: slow_id.into(),
            replacement_id: replacement_id.into(),
            admission: Arc::clone(&admission),
        };
        let app = Router::new()
            .route("/resource/{kind}", get(test_resource_route))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind test HTTP server");
        let address = listener.local_addr().expect("test HTTP address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test HTTP server");
        });

        let mut slow_socket = open_raw_http_get(address, "/resource/slow").await;
        let slow_headers = read_raw_http_headers(&mut slow_socket).await;
        assert!(slow_headers.starts_with(b"HTTP/1.1 200 OK\r\n"));
        let slow_headers = String::from_utf8(slow_headers).expect("ASCII HTTP response headers");
        assert!(
            slow_headers
                .to_ascii_lowercase()
                .contains(&format!("content-length: {}", slow_contents.len())),
            "{slow_headers}"
        );
        wait_for_global_permits(&admission, 0).await;
        let observed_active_at = tokio::time::Instant::now();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            admission.available_global_permits(),
            0,
            "the producer completed normally before its shortened deadline; \
             this fixture no longer creates TCP backpressure"
        );
        wait_for_global_permits(&admission, 1).await;
        assert!(
            observed_active_at.elapsed() >= Duration::from_millis(250),
            "admission was released too early to be attributable to the 500 ms stream deadline"
        );

        let mut truncated_body = Vec::new();
        let _read_result = tokio::time::timeout(
            Duration::from_secs(2),
            slow_socket.read_to_end(&mut truncated_body),
        )
        .await
        .expect("timed-out slow response must close its HTTP connection");
        assert!(
            truncated_body.len() < slow_contents.len(),
            "the slow response delivered its full Content-Length instead of terminating early"
        );

        let mut replacement_socket = open_raw_http_get(address, "/resource/replacement").await;
        let mut response = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            replacement_socket.read_to_end(&mut response),
        )
        .await
        .expect("replacement response deadline")
        .expect("read replacement response");
        assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(response.ends_with(replacement_contents));
        wait_for_global_permits(&admission, 1).await;

        server.abort();
        let _ = server.await;
    }

    #[test]
    fn resource_verification_and_stream_deadlines_are_bounded_and_size_aware() {
        let policy = ResourceStreamPolicy::PRODUCTION;
        assert_eq!(
            policy.verification_deadline(0),
            RESOURCE_VERIFICATION_BASE_DEADLINE
        );
        assert!(
            policy.verification_deadline(RESOURCE_VERIFICATION_MIN_BYTES_PER_SECOND)
                > policy.verification_deadline(0)
        );
        assert_eq!(
            policy.verification_deadline(u64::MAX),
            RESOURCE_VERIFICATION_MAX_DEADLINE
        );

        assert_eq!(resource_stream_deadline(0), RESOURCE_STREAM_BASE_DEADLINE);
        assert!(
            resource_stream_deadline(RESOURCE_STREAM_MIN_BYTES_PER_SECOND)
                > resource_stream_deadline(0)
        );
        assert_eq!(
            resource_stream_deadline(u64::MAX),
            RESOURCE_STREAM_MAX_DEADLINE
        );
    }

    #[tokio::test]
    async fn serve_time_hash_verification_rejects_tampered_blob() {
        let root = TestDir::new("serve-tamper");
        let path = root.path().join("blob.tja");
        std::fs::write(&path, b"original").expect("write original blob");
        let expected_hash = sha256_hex(b"original");
        std::fs::write(&path, b"tampered").expect("tamper blob");
        let files = HashMap::from([(
            expected_hash.clone(),
            BlobFile {
                path,
                expected_hash: expected_hash.clone(),
            },
        )]);

        let admission = test_admission(1);
        let result = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            MAX_CHART_RESPONSE_BYTES,
        )
        .await;
        let Err(error) = result else {
            panic!("tampered blob was served");
        };
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("changed after indexing"));
    }

    #[tokio::test]
    async fn verified_response_streams_the_private_snapshot_after_same_inode_overwrite() {
        let root = TestDir::new("serve-snapshot");
        let path = root.path().join("blob.tja");
        let original = b"original";
        let replacement = b"tampered";
        assert_eq!(original.len(), replacement.len());
        std::fs::write(&path, original).expect("write original blob");
        let expected_hash = sha256_hex(original);
        let blob = BlobFile {
            path: path.clone(),
            expected_hash: expected_hash.clone(),
        };

        let admission = test_admission(1);
        let verified = VerifiedResourceSnapshot::open(
            &blob,
            &expected_hash,
            u64::try_from(original.len()).expect("fixture length"),
            admission.snapshot_budget(),
            ResourceStreamPolicy::PRODUCTION,
        )
        .await
        .expect("verify and snapshot original bytes");
        std::fs::write(&path, replacement).expect("overwrite the indexed inode in place");
        assert_eq!(
            std::fs::read(&path).expect("read overwritten source"),
            replacement
        );

        let permits = admission
            .try_acquire(test_client_ip())
            .expect("stream admission");
        let body = verified.into_body(permits, Duration::from_secs(1));
        let streamed = axum::body::to_bytes(body, original.len())
            .await
            .expect("stream snapshot");

        assert_eq!(streamed.as_ref(), original);
        assert_eq!(sha256_hex(&streamed), expected_hash);
        assert_eq!(admission.available_global_permits(), 1);
    }

    #[tokio::test]
    async fn serve_time_size_limit_rejects_oversized_blob_and_releases_permit() {
        let root = TestDir::new("serve-oversized");
        let path = root.path().join("blob.bin");
        let contents = b"oversized";
        std::fs::write(&path, contents).expect("write fixture");
        let expected_hash = sha256_hex(contents);
        let files = HashMap::from([(
            expected_hash.clone(),
            BlobFile {
                path,
                expected_hash: expected_hash.clone(),
            },
        )]);
        let admission = test_admission(1);

        let result = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            u64::try_from(contents.len() - 1).expect("fixture length fits u64"),
        )
        .await;
        let Err(error) = result else {
            panic!("oversized blob was served");
        };
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("changed after indexing"));
        assert_eq!(admission.available_global_permits(), 1);
    }

    #[tokio::test]
    async fn resource_route_rejects_non_digest_ids() {
        let files = HashMap::new();
        let admission = test_admission(1);
        let result = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            "../secret",
            "application/octet-stream",
            MAX_CHART_RESPONSE_BYTES,
        )
        .await;
        let Err(error) = result else {
            panic!("invalid resource id was accepted");
        };
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert!(error.message.contains("lowercase SHA-256"));
        assert_eq!(admission.available_global_permits(), 1);
    }

    #[tokio::test]
    async fn large_resource_streams_in_fixed_bounded_chunks_with_exact_headers() {
        let root = TestDir::new("large-stream");
        let path = root.path().join("large.bin");
        let mut contents = vec![0_u8; RESOURCE_STREAM_CHUNK_BYTES * 3 + 17];
        for (index, byte) in contents.iter_mut().enumerate() {
            *byte = u8::try_from(index % 251).expect("modulo 251 fits in u8");
        }
        std::fs::write(&path, &contents).expect("write large fixture");
        let expected_hash = sha256_hex(&contents);
        let files = HashMap::from([(
            expected_hash.clone(),
            BlobFile {
                path,
                expected_hash: expected_hash.clone(),
            },
        )]);
        let admission = test_admission(1);
        let max_bytes = u64::try_from(contents.len()).expect("fixture length fits u64");

        let response = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect("serve large resource");

        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/octet-stream"))
        );
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH),
            Some(
                &HeaderValue::try_from(contents.len().to_string())
                    .expect("fixture length is a valid header")
            )
        );
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL),
            Some(&HeaderValue::from_static(
                "public, max-age=31536000, immutable"
            ))
        );
        assert_eq!(admission.available_global_permits(), 0);

        let mut body = response.into_body().into_data_stream();
        let mut streamed_hash = Sha256::new();
        let mut streamed_bytes = 0_usize;
        let mut chunks = 0_usize;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.expect("read streamed chunk");
            assert!(!chunk.is_empty());
            assert!(chunk.len() <= RESOURCE_STREAM_CHUNK_BYTES);
            streamed_bytes += chunk.len();
            streamed_hash.update(&chunk);
            chunks += 1;
        }

        assert!(chunks > 1, "large fixture must require multiple chunks");
        assert_eq!(streamed_bytes, contents.len());
        assert_eq!(hex::encode(streamed_hash.finalize()), expected_hash);
        assert_eq!(admission.available_global_permits(), 1);
        drop(body);
        assert_eq!(admission.available_global_permits(), 1);
    }

    #[tokio::test]
    async fn resource_stream_cap_returns_503_and_partial_body_drop_releases_permit() {
        let root = TestDir::new("stream-permit");
        let path = root.path().join("blob.bin");
        let contents = vec![7_u8; RESOURCE_STREAM_CHUNK_BYTES * 4];
        std::fs::write(&path, &contents).expect("write fixture");
        let expected_hash = sha256_hex(&contents);
        let files = HashMap::from([(
            expected_hash.clone(),
            BlobFile {
                path,
                expected_hash: expected_hash.clone(),
            },
        )]);
        let admission = test_admission(1);
        let max_bytes = u64::try_from(contents.len()).expect("fixture length fits u64");

        let first = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect("first stream");
        assert_eq!(admission.available_global_permits(), 0);

        let second = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await;
        let Err(error) = second else {
            panic!("stream beyond hard cap was accepted");
        };
        assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(error.message.contains("capacity exhausted"));
        let response = error.into_response();
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(
                &HeaderValue::try_from(RESOURCE_STREAM_RETRY_AFTER_SECONDS.to_string())
                    .expect("retry delay is a valid header")
            )
        );

        let mut first_body = first.into_body().into_data_stream();
        let first_chunk = first_body
            .next()
            .await
            .expect("first body chunk")
            .expect("read first body chunk");
        assert!(!first_chunk.is_empty());
        assert!(first_chunk.len() <= RESOURCE_STREAM_CHUNK_BYTES);
        assert_eq!(admission.available_global_permits(), 0);
        drop(first_body);
        wait_for_global_permits(&admission, 1).await;

        let replacement = serve_binary(
            &files,
            &admission,
            test_client_ip(),
            &expected_hash,
            "application/octet-stream",
            max_bytes,
        )
        .await
        .expect("replacement stream after drop");
        assert_eq!(admission.available_global_permits(), 0);
        drop(replacement);
        wait_for_global_permits(&admission, 1).await;
    }
}
