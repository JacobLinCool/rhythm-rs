use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use directories::ProjectDirs;
use reqwest::blocking::Client;
use reqwest::Url;
use rhythm_chart::CanonicalChart;
use rhythm_importer_tja::{BranchDecisionPoint, TjaImporter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use taiko_resource_protocol::{ResourceLibraryDocument, API_VERSION};
use walkdir::WalkDir;

use crate::cli::CliArgs;
use crate::loader::{
    load_course_chart as load_local_course_chart, load_song_library as load_local_song_library,
    CourseEntry, ResourceLocator, SongEntry, SongLibrary,
};

const CACHE_INDEX_VERSION: u32 = 1;

#[derive(Clone)]
pub enum SongAudioSource {
    FilePath(PathBuf),
    Bytes(Arc<[u8]>),
}

pub enum ResourceBackend {
    Local(LocalResourceBackend),
    Remote(RemoteResourceBackend),
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

#[derive(Debug)]
struct DiskCacheLayout {
    chart_dir: PathBuf,
    audio_dir: PathBuf,
    index_file: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheIndexDocument {
    version: u32,
    entries: HashMap<String, String>,
}

pub struct RemoteResourceBackend {
    endpoint: Url,
    client: Client,
    memory_cache: Mutex<HashMap<String, Arc<[u8]>>>,
    resource_hash_index: Mutex<HashMap<String, String>>,
    disk_cache: Option<DiskCacheLayout>,
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
    pub fn from_cli(args: &CliArgs) -> Result<Self> {
        match args.resource_endpoint.as_deref() {
            None => Ok(Self::Local(LocalResourceBackend {
                songdir: args.songdir.clone(),
            })),
            Some(endpoint) => {
                let cache_mode = if args.resource_cache_memory_only {
                    RemoteCacheMode::MemoryOnly
                } else {
                    RemoteCacheMode::AppData
                };
                Ok(Self::Remote(RemoteResourceBackend::new(
                    endpoint, cache_mode,
                )?))
            }
        }
    }

    pub fn load_song_library(&self) -> Result<SongLibrary> {
        match self {
            Self::Local(local) => load_local_song_library(&local.songdir),
            Self::Remote(remote) => remote.load_song_library(),
        }
    }

    pub fn load_course_chart(
        &self,
        song: &SongEntry,
        course_index: usize,
        importer: &TjaImporter,
    ) -> Result<CanonicalChart> {
        match self {
            Self::Local(_) => {
                let ResourceLocator::LocalPath(path) = &song.source_locator else {
                    bail!("local backend received non-local source locator");
                };
                load_local_course_chart(path, course_index, importer)
            }
            Self::Remote(remote) => remote.load_course_chart(song, course_index, importer),
        }
    }

    pub fn load_song_audio(&self, song: &SongEntry) -> Result<SongAudioSource> {
        match self {
            Self::Local(_) => {
                let ResourceLocator::LocalPath(path) = &song.audio_locator else {
                    bail!("local backend received non-local audio locator");
                };
                Ok(SongAudioSource::FilePath(path.clone()))
            }
            Self::Remote(remote) => remote.load_song_audio(song),
        }
    }
}

pub fn cache_root_dir() -> Result<PathBuf> {
    cache_root_dir_internal()
}

pub fn cache_dir_for_endpoint(endpoint: &str) -> Result<PathBuf> {
    let endpoint_hash = endpoint_cache_hash(endpoint)?;
    Ok(cache_root_dir_internal()?.join(endpoint_hash))
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
    let root = cache_root_dir_internal()?;
    if !root.exists() {
        return Ok(RemoteCacheClearResult {
            removed_paths: Vec::new(),
            missing_paths: vec![root],
        });
    }

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

    Ok(RemoteCacheClearResult {
        removed_paths,
        missing_paths: Vec::new(),
    })
}

pub fn clear_remote_cache_for_endpoint(endpoint: &str) -> Result<RemoteCacheClearResult> {
    let cache_dir = cache_dir_for_endpoint(endpoint)?;
    if !cache_dir.exists() {
        return Ok(RemoteCacheClearResult {
            removed_paths: Vec::new(),
            missing_paths: vec![cache_dir],
        });
    }

    std::fs::remove_dir_all(&cache_dir)
        .with_context(|| format!("failed to remove cache dir {}", cache_dir.display()))?;
    Ok(RemoteCacheClearResult {
        removed_paths: vec![cache_dir],
        missing_paths: Vec::new(),
    })
}

impl RemoteResourceBackend {
    fn new(endpoint: &str, cache_mode: RemoteCacheMode) -> Result<Self> {
        let mut endpoint = Url::parse(endpoint)
            .with_context(|| format!("invalid --resource-endpoint URL: {endpoint}"))?;
        ensure_directory_url(&mut endpoint);

        let client = Client::builder()
            .build()
            .context("failed to initialize HTTP client")?;

        let (disk_cache, resource_hash_index) = match cache_mode {
            RemoteCacheMode::MemoryOnly => (None, HashMap::new()),
            RemoteCacheMode::AppData => {
                let layout = DiskCacheLayout::from_endpoint(&endpoint)?;
                std::fs::create_dir_all(&layout.chart_dir).with_context(|| {
                    format!(
                        "failed to create chart cache dir {}",
                        layout.chart_dir.display()
                    )
                })?;
                std::fs::create_dir_all(&layout.audio_dir).with_context(|| {
                    format!(
                        "failed to create audio cache dir {}",
                        layout.audio_dir.display()
                    )
                })?;
                let index = load_cache_index(&layout)?;
                (Some(layout), index.entries)
            }
        };

        Ok(Self {
            endpoint,
            client,
            memory_cache: Mutex::new(HashMap::new()),
            resource_hash_index: Mutex::new(resource_hash_index),
            disk_cache,
        })
    }

    fn load_song_library(&self) -> Result<SongLibrary> {
        let url = self.api_url("v1/library")?;
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("resource server rejected {url}"))?;

        let document = response
            .json::<ResourceLibraryDocument>()
            .with_context(|| format!("failed to parse library payload from {url}"))?;

        if document.api_version != API_VERSION {
            bail!(
                "unsupported resource API version {} (expected {API_VERSION})",
                document.api_version
            );
        }

        let mut songs = Vec::with_capacity(document.songs.len());
        for song in document.songs {
            if song.source_id.trim().is_empty() {
                bail!("library entry `{}` has empty source_id", song.source_path);
            }
            if song.audio_id.trim().is_empty() {
                bail!("library entry `{}` has empty audio_id", song.source_path);
            }
            if song.courses.is_empty() {
                bail!(
                    "library entry `{}` has no playable course",
                    song.source_path
                );
            }

            songs.push(SongEntry {
                source_locator: ResourceLocator::RemoteId(song.source_id),
                audio_locator: ResourceLocator::RemoteId(song.audio_id),
                source_path: PathBuf::from(song.source_path),
                audio_path: PathBuf::from(song.audio_path),
                title: song.title,
                subtitle: song.subtitle,
                artist: song.artist,
                demo_start_seconds: song.demo_start_seconds.max(0.0),
                courses: song
                    .courses
                    .into_iter()
                    .map(|course| CourseEntry {
                        index: course.index,
                        name: course.name,
                        level: course.level,
                        object_count: course.object_count,
                        branch_segment_count: course.branch_segment_count,
                        base_bpm: course.base_bpm,
                        branch_decisions: course
                            .branch_decisions
                            .into_iter()
                            .map(|decision| BranchDecisionPoint {
                                segment_id: decision.segment_id,
                                decision_tick: decision.decision_tick,
                                route_count: decision.route_count,
                                hint: decision.hint,
                            })
                            .collect(),
                    })
                    .collect(),
            });
        }

        songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.source_path.clone()));

        Ok(SongLibrary {
            songs,
            warnings: document.warnings,
        })
    }

    fn load_course_chart(
        &self,
        song: &SongEntry,
        course_index: usize,
        importer: &TjaImporter,
    ) -> Result<CanonicalChart> {
        let ResourceLocator::RemoteId(source_id) = &song.source_locator else {
            bail!("remote backend received non-remote source locator");
        };

        let raw = self.fetch_cached_bytes(RemoteResourceKind::Chart, source_id)?;
        let imported = importer.import_song(raw.as_ref()).with_context(|| {
            format!(
                "failed to parse remote chart {}",
                song.source_path.display()
            )
        })?;

        imported
            .courses
            .into_iter()
            .nth(course_index)
            .map(|course| course.chart)
            .ok_or_else(|| {
                anyhow!(
                    "course index {course_index} out of range for {}",
                    song.source_path.display()
                )
            })
    }

    fn load_song_audio(&self, song: &SongEntry) -> Result<SongAudioSource> {
        let ResourceLocator::RemoteId(audio_id) = &song.audio_locator else {
            bail!("remote backend received non-remote audio locator");
        };

        let bytes = self.fetch_cached_bytes(RemoteResourceKind::Audio, audio_id)?;
        Ok(SongAudioSource::Bytes(bytes))
    }

    fn fetch_cached_bytes(&self, kind: RemoteResourceKind, resource_id: &str) -> Result<Arc<[u8]>> {
        if resource_id.trim().is_empty() {
            bail!("remote resource id cannot be empty");
        }

        let resource_key = kind.resource_key(resource_id);
        if let Some(content_hash) = self.lookup_cached_hash(&resource_key)? {
            if let Some(bytes) = self.get_memory_cached(&content_hash)? {
                return Ok(bytes);
            }

            if let Some(bytes) = self.load_disk_cached(kind, &content_hash)? {
                self.put_memory_cached(&content_hash, bytes.clone())?;
                return Ok(bytes);
            }
        }

        let bytes = self.download_resource(kind, resource_id)?;
        let content_hash = sha256_hex(bytes.as_ref());

        self.put_memory_cached(&content_hash, bytes.clone())?;
        self.update_hash_index(&resource_key, &content_hash)?;
        self.store_disk_cached(kind, &content_hash, bytes.as_ref())?;

        Ok(bytes)
    }

    fn lookup_cached_hash(&self, resource_key: &str) -> Result<Option<String>> {
        let guard = self
            .resource_hash_index
            .lock()
            .map_err(|_| anyhow!("resource hash index lock poisoned"))?;
        Ok(guard.get(resource_key).cloned())
    }

    fn update_hash_index(&self, resource_key: &str, content_hash: &str) -> Result<()> {
        let mut guard = self
            .resource_hash_index
            .lock()
            .map_err(|_| anyhow!("resource hash index lock poisoned"))?;
        let changed = guard
            .insert(resource_key.to_owned(), content_hash.to_owned())
            .as_deref()
            != Some(content_hash);
        if changed {
            self.flush_cache_index(&guard)?;
        }
        Ok(())
    }

    fn get_memory_cached(&self, content_hash: &str) -> Result<Option<Arc<[u8]>>> {
        let guard = self
            .memory_cache
            .lock()
            .map_err(|_| anyhow!("memory cache lock poisoned"))?;
        Ok(guard.get(content_hash).cloned())
    }

    fn put_memory_cached(&self, content_hash: &str, bytes: Arc<[u8]>) -> Result<()> {
        let mut guard = self
            .memory_cache
            .lock()
            .map_err(|_| anyhow!("memory cache lock poisoned"))?;
        guard.entry(content_hash.to_owned()).or_insert(bytes);
        Ok(())
    }

    fn load_disk_cached(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
    ) -> Result<Option<Arc<[u8]>>> {
        let Some(layout) = self.disk_cache.as_ref() else {
            return Ok(None);
        };

        let cache_path = layout.blob_path(kind, content_hash);
        if !cache_path.exists() {
            return Ok(None);
        }

        let bytes = std::fs::read(&cache_path)
            .with_context(|| format!("failed to read cache blob {}", cache_path.display()))?;
        if sha256_hex(&bytes) != content_hash {
            bail!(
                "cache blob hash mismatch: {} (expected {content_hash})",
                cache_path.display()
            );
        }

        Ok(Some(bytes.into()))
    }

    fn store_disk_cached(
        &self,
        kind: RemoteResourceKind,
        content_hash: &str,
        bytes: &[u8],
    ) -> Result<()> {
        let Some(layout) = self.disk_cache.as_ref() else {
            return Ok(());
        };

        let cache_path = layout.blob_path(kind, content_hash);
        if cache_path.exists() {
            return Ok(());
        }

        write_atomic(&cache_path, bytes)
            .with_context(|| format!("failed to write cache blob {}", cache_path.display()))
    }

    fn flush_cache_index(&self, entries: &HashMap<String, String>) -> Result<()> {
        let Some(layout) = self.disk_cache.as_ref() else {
            return Ok(());
        };

        let document = CacheIndexDocument {
            version: CACHE_INDEX_VERSION,
            entries: entries.clone(),
        };
        let raw = serde_json::to_vec(&document).context("failed to serialize cache index")?;
        write_atomic(&layout.index_file, &raw).with_context(|| {
            format!(
                "failed to write cache index {}",
                layout.index_file.display()
            )
        })
    }

    fn download_resource(&self, kind: RemoteResourceKind, resource_id: &str) -> Result<Arc<[u8]>> {
        let url = self.api_url(&format!("v1/{}/{resource_id}", kind.route_segment()))?;
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("failed to request {url}"))?
            .error_for_status()
            .with_context(|| format!("resource server rejected {url}"))?;
        let body = response
            .bytes()
            .with_context(|| format!("failed to read response body from {url}"))?;
        Ok(body.to_vec().into())
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

    fn resource_key(self, resource_id: &str) -> String {
        format!("{}/{}", self.route_segment(), resource_id)
    }
}

impl DiskCacheLayout {
    fn from_endpoint(endpoint: &Url) -> Result<Self> {
        let root = cache_root_dir_internal()?.join(endpoint_cache_hash_url(endpoint));

        Ok(Self {
            chart_dir: root.join("charts"),
            audio_dir: root.join("audio"),
            index_file: root.join("index-v1.json"),
        })
    }

    fn blob_path(&self, kind: RemoteResourceKind, content_hash: &str) -> PathBuf {
        match kind {
            RemoteResourceKind::Chart => self.chart_dir.join(content_hash),
            RemoteResourceKind::Audio => self.audio_dir.join(content_hash),
        }
    }
}

fn summarize_cache_dir(path: &Path, endpoint_hash: &str) -> Result<RemoteCacheSummary> {
    let chart_dir = path.join("charts");
    let audio_dir = path.join("audio");
    let index_file = path.join("index-v1.json");

    let (chart_files, chart_bytes) = directory_blob_stats(&chart_dir)?;
    let (audio_files, audio_bytes) = directory_blob_stats(&audio_dir)?;
    let index_entries = if index_file.exists() {
        let raw = std::fs::read(&index_file)
            .with_context(|| format!("failed to read {}", index_file.display()))?;
        let index = serde_json::from_slice::<CacheIndexDocument>(&raw)
            .with_context(|| format!("failed to parse {}", index_file.display()))?;
        if index.version != CACHE_INDEX_VERSION {
            bail!(
                "unsupported cache index version {} in {}",
                index.version,
                index_file.display()
            );
        }
        index.entries.len()
    } else {
        0
    };

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
        return Ok(CacheIndexDocument {
            version: CACHE_INDEX_VERSION,
            entries: HashMap::new(),
        });
    }

    let raw = std::fs::read(&layout.index_file)
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

    Ok(parsed)
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create dir {}", parent.display()))?;

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let temp_path = path.with_extension(format!("tmp-{}-{nonce}", std::process::id()));

    std::fs::write(&temp_path, content)
        .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
    std::fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to move temp file {} to {}",
            temp_path.display(),
            path.display()
        )
    })?;

    Ok(())
}

fn sha256_hex(input: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input);
    hex::encode(hasher.finalize())
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
    use super::*;

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
    fn endpoint_cache_hash_normalizes_trailing_slash() {
        let a = endpoint_cache_hash("http://127.0.0.1:4150").expect("hash a");
        let b = endpoint_cache_hash("http://127.0.0.1:4150/").expect("hash b");
        assert_eq!(a, b);
    }
}
