use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use clap::Args;
use rayon::prelude::*;
use rhythm_importer_tja::{ImportedSong, TjaImporter};
use sha2::{Digest, Sha256};
use taiko_resource_protocol::{
    ResourceBranchDecisionPoint, ResourceCourse, ResourceLibraryDocument, ResourceSong, API_VERSION,
};
use walkdir::WalkDir;

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
    chart_files: Arc<HashMap<String, PathBuf>>,
    audio_files: Arc<HashMap<String, PathBuf>>,
}

#[derive(Debug)]
enum IndexResult {
    Song {
        song: ResourceSong,
        chart_path: PathBuf,
        audio_path: PathBuf,
    },
    Warning(String),
}

#[derive(Debug)]
struct IndexedIndexResult {
    index: usize,
    result: IndexResult,
}

pub fn run_server(args: ServerArgs) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize tokio runtime")?;

    runtime.block_on(run_server_async(args))
}

pub async fn run_server_async(args: ServerArgs) -> Result<()> {
    let state = build_state(&args.songdir)?;

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/library", get(get_library))
        .route("/v1/charts/{id}", get(get_chart))
        .route("/v1/audio/{id}", get(get_audio))
        .with_state(state.clone());

    let bind_addr = format!("{}:{}", args.host, args.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;

    println!(
        "taiko-resource-server listening on http://{} (songs={}, warnings={})",
        bind_addr,
        state.library.songs.len(),
        state.library.warnings.len()
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP server failed")?;

    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn build_state(songdir: &Path) -> Result<ServerState> {
    if !songdir.exists() {
        bail!("song directory not found: {}", songdir.display());
    }

    let mut chart_paths = WalkDir::new(songdir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().to_path_buf())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tja"))
        })
        .collect::<Vec<_>>();

    chart_paths.sort();

    let mut indexed_results = chart_paths
        .into_par_iter()
        .enumerate()
        .map(|(index, chart_path)| IndexedIndexResult {
            index,
            result: index_song(songdir, chart_path),
        })
        .collect::<Vec<_>>();

    indexed_results.sort_by_key(|entry| entry.index);

    let mut songs = Vec::new();
    let mut warnings = Vec::new();
    let mut chart_files = HashMap::new();
    let mut audio_files = HashMap::new();

    for entry in indexed_results {
        match entry.result {
            IndexResult::Song {
                song,
                chart_path,
                audio_path,
            } => {
                chart_files.insert(song.source_id.clone(), chart_path);
                audio_files
                    .entry(song.audio_id.clone())
                    .or_insert(audio_path);
                songs.push(song);
            }
            IndexResult::Warning(warning) => warnings.push(warning),
        }
    }

    songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.source_path.clone()));

    Ok(ServerState {
        library: Arc::new(ResourceLibraryDocument {
            api_version: API_VERSION,
            songs,
            warnings,
        }),
        chart_files: Arc::new(chart_files),
        audio_files: Arc::new(audio_files),
    })
}

fn index_song(songdir: &Path, chart_path: PathBuf) -> IndexResult {
    let raw = match std::fs::read(&chart_path) {
        Ok(raw) => raw,
        Err(error) => {
            return IndexResult::Warning(format!("skip {}: {error}", chart_path.display()));
        }
    };

    let importer = TjaImporter;
    let imported = match importer.import_song(&raw) {
        Ok(imported) => imported,
        Err(error) => {
            return IndexResult::Warning(format!("skip {}: {error}", chart_path.display()));
        }
    };

    match build_song(songdir, chart_path.clone(), imported) {
        Ok((song, audio_path)) => IndexResult::Song {
            song,
            chart_path,
            audio_path,
        },
        Err(error) => IndexResult::Warning(format!("skip {}: {error}", chart_path.display())),
    }
}

fn build_song(
    songdir: &Path,
    source_path: PathBuf,
    imported: ImportedSong,
) -> Result<(ResourceSong, PathBuf)> {
    if imported.courses.is_empty() {
        bail!("chart has no playable course");
    }

    let parent = source_path.parent().unwrap_or_else(|| Path::new("."));
    let audio_path = imported
        .audio_path
        .as_ref()
        .map(|path| parent.join(path))
        .unwrap_or_else(|| source_path.with_extension("ogg"));

    let source_rel = normalized_rel_path(songdir, &source_path);
    let audio_rel = normalized_rel_path(songdir, &audio_path);

    let courses = imported
        .courses
        .into_iter()
        .enumerate()
        .map(|(index, course)| {
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

            ResourceCourse {
                index,
                name,
                level: course.chart.metadata.difficulty_level,
                object_count: course.chart.objects.len(),
                branch_segment_count: course.chart.branch_segments.len(),
                base_bpm,
                branch_decisions: course
                    .branch_decisions
                    .into_iter()
                    .map(|decision| ResourceBranchDecisionPoint {
                        segment_id: decision.segment_id,
                        decision_tick: decision.decision_tick,
                        route_count: decision.route_count,
                        hint: decision.hint,
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();

    let title = if imported.title.trim().is_empty() {
        source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map_or_else(|| "Untitled".to_owned(), ToOwned::to_owned)
    } else {
        imported.title
    };

    let song = ResourceSong {
        source_path: source_rel.clone(),
        source_id: make_resource_id("chart", &source_rel),
        audio_path: audio_rel.clone(),
        audio_id: make_resource_id("audio", &audio_rel),
        title,
        subtitle: imported.subtitle,
        artist: imported.artist,
        demo_start_seconds: imported.demo_start_seconds.unwrap_or(0.0).max(0.0),
        courses,
    };

    Ok((song, audio_path))
}

fn normalized_rel_path(songdir: &Path, path: &Path) -> String {
    let raw = path.strip_prefix(songdir).unwrap_or(path);
    raw.to_string_lossy().replace('\\', "/")
}

fn make_resource_id(kind: &str, rel_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(rel_path.as_bytes());
    hex::encode(hasher.finalize())
}

async fn healthz() -> &'static str {
    "ok"
}

async fn get_library(State(state): State<ServerState>) -> Json<ResourceLibraryDocument> {
    Json((*state.library).clone())
}

async fn get_chart(
    AxumPath(id): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    serve_binary(&state.chart_files, &id, "application/octet-stream").await
}

async fn get_audio(
    AxumPath(id): AxumPath<String>,
    State(state): State<ServerState>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(path) = state.audio_files.get(&id) else {
        return Err((StatusCode::NOT_FOUND, format!("unknown audio id: {id}")));
    };
    let content_type = audio_content_type(path);
    serve_binary(&state.audio_files, &id, content_type).await
}

async fn serve_binary(
    files: &HashMap<String, PathBuf>,
    id: &str,
    content_type: &'static str,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let Some(path) = files.get(id) else {
        return Err((StatusCode::NOT_FOUND, format!("unknown resource id: {id}")));
    };

    let bytes = tokio::fs::read(path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            (
                StatusCode::NOT_FOUND,
                format!("resource file missing: {}", path.display()),
            )
        } else {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read {}: {error}", path.display()),
            )
        }
    })?;

    let headers = [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))];
    Ok((headers, bytes))
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
    use super::*;

    #[test]
    fn normalized_rel_path_uses_songdir_prefix() {
        let songdir = Path::new("/songs");
        let path = Path::new("/songs/pack/a.tja");
        assert_eq!(normalized_rel_path(songdir, path), "pack/a.tja");
    }

    #[test]
    fn resource_id_is_stable() {
        let a = make_resource_id("chart", "pack/a.tja");
        let b = make_resource_id("chart", "pack/a.tja");
        let c = make_resource_id("audio", "pack/a.tja");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
