use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use rhythm_chart::CanonicalChart;
use rhythm_importer_tja::{BranchDecisionPoint, ImportedSong, TjaImporter};
use walkdir::WalkDir;

#[derive(Debug, Clone)]
pub struct SongLibrary {
    pub songs: Vec<SongEntry>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CourseEntry {
    pub index: usize,
    pub name: String,
    pub level: Option<u8>,
    pub object_count: usize,
    pub branch_segment_count: usize,
    pub base_bpm: Option<f64>,
    pub branch_decisions: Vec<BranchDecisionPoint>,
}

#[derive(Debug, Clone)]
pub struct SongEntry {
    pub source_locator: ResourceLocator,
    pub audio_locator: ResourceLocator,
    pub source_path: PathBuf,
    pub audio_path: PathBuf,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub demo_start_seconds: f64,
    pub courses: Vec<CourseEntry>,
}

impl SongEntry {
    pub fn has_branching(&self) -> bool {
        self.courses
            .iter()
            .any(|course| !course.branch_decisions.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceLocator {
    LocalPath(PathBuf),
    RemoteId(String),
}

#[derive(Debug)]
enum LoadLibraryResult {
    Song(SongEntry),
    Warning(String),
}

#[derive(Debug)]
struct IndexedLoadLibraryResult {
    index: usize,
    result: LoadLibraryResult,
}

pub fn load_song_library(songdir: &Path) -> Result<SongLibrary> {
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
        .map(|(index, chart_path)| IndexedLoadLibraryResult {
            index,
            result: load_single_song_entry(chart_path),
        })
        .collect::<Vec<_>>();

    indexed_results.sort_by_key(|entry| entry.index);

    let mut songs = Vec::with_capacity(indexed_results.len());
    let mut warnings = Vec::new();

    for entry in indexed_results {
        match entry.result {
            LoadLibraryResult::Song(song) => songs.push(song),
            LoadLibraryResult::Warning(warning) => warnings.push(warning),
        }
    }

    songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.source_path.clone()));

    Ok(SongLibrary { songs, warnings })
}

pub fn load_course_chart(
    source_path: &Path,
    course_index: usize,
    importer: &TjaImporter,
) -> Result<CanonicalChart> {
    let raw = std::fs::read(source_path)
        .with_context(|| format!("failed to read chart {}", source_path.display()))?;
    let imported = importer
        .import_song(&raw)
        .with_context(|| format!("failed to parse chart {}", source_path.display()))?;
    let course = imported
        .courses
        .into_iter()
        .nth(course_index)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "course index {course_index} out of range for {}",
                source_path.display()
            )
        })?;
    Ok(course.chart)
}

fn load_single_song_entry(chart_path: PathBuf) -> LoadLibraryResult {
    let raw = match std::fs::read(&chart_path) {
        Ok(raw) => raw,
        Err(error) => {
            return LoadLibraryResult::Warning(format!("skip {}: {error}", chart_path.display()));
        }
    };

    let importer = TjaImporter;
    let imported = match importer.import_song(&raw) {
        Ok(imported) => imported,
        Err(error) => {
            return LoadLibraryResult::Warning(format!("skip {}: {error}", chart_path.display()));
        }
    };

    match build_song_entry(chart_path.clone(), imported) {
        Ok(song) => LoadLibraryResult::Song(song),
        Err(error) => LoadLibraryResult::Warning(format!("skip {}: {error}", chart_path.display())),
    }
}

fn build_song_entry(source_path: PathBuf, imported: ImportedSong) -> Result<SongEntry> {
    if imported.courses.is_empty() {
        bail!("chart has no playable course: {}", source_path.display());
    }

    let parent = source_path.parent().unwrap_or_else(|| Path::new("."));
    let audio_path = imported
        .audio_path
        .as_ref()
        .map(|path| parent.join(path))
        .unwrap_or_else(|| source_path.with_extension("ogg"));

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

            CourseEntry {
                index,
                name,
                level: course.chart.metadata.difficulty_level,
                object_count: course.chart.objects.len(),
                branch_segment_count: course.chart.branch_segments.len(),
                base_bpm,
                branch_decisions: course.branch_decisions,
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

    Ok(SongEntry {
        source_locator: ResourceLocator::LocalPath(source_path.clone()),
        audio_locator: ResourceLocator::LocalPath(audio_path.clone()),
        source_path,
        audio_path,
        title,
        subtitle: imported.subtitle,
        artist: imported.artist,
        demo_start_seconds: imported.demo_start_seconds.unwrap_or(0.0).max(0.0),
        courses,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::time::Instant;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const SAMPLE_TJA: &[u8] = include_bytes!("../samples/Nosferatu.tja");

    #[test]
    fn summary_matches_lazy_loaded_course_chart() {
        let temp_root = std::env::temp_dir().join(format!(
            "taiko-loader-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&temp_root).expect("create temp root");

        let chart_path = temp_root.join("sample.tja");
        fs::write(&chart_path, SAMPLE_TJA).expect("write sample chart");

        let library = load_song_library(&temp_root).expect("load library");
        let song = library.songs.first().expect("song entry");
        let course = song.courses.first().expect("course summary");

        let importer = TjaImporter;
        let chart = load_course_chart(&song.source_path, course.index, &importer)
            .expect("load selected course chart");

        assert_eq!(course.object_count, chart.objects.len());
        assert_eq!(course.branch_segment_count, chart.branch_segments.len());
        let expected_bpm = chart
            .tempo_map
            .first()
            .map(|tempo| 60_000_000.0 / tempo.micros_per_quarter as f64);
        assert_eq!(course.base_bpm, expected_bpm);

        fs::remove_file(&chart_path).expect("remove chart");
        fs::remove_dir(&temp_root).expect("remove temp root");
    }

    #[test]
    #[ignore = "bench_smoke"]
    fn bench_smoke_load_song_library_real_songdir() {
        let Ok(songdir) = std::env::var("TAIKO_SONGDIR") else {
            eprintln!("bench_smoke_loader: set TAIKO_SONGDIR to enable this benchmark");
            return;
        };

        let start = Instant::now();
        let library = load_song_library(Path::new(&songdir)).expect("load library");
        let elapsed = start.elapsed().as_secs_f64();

        eprintln!(
            "bench_smoke_loader: songs={} warnings={} elapsed={elapsed:.3}s",
            library.songs.len(),
            library.warnings.len(),
        );
    }
}
