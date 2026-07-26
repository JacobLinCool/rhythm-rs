use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
#[cfg(test)]
use rhythm_chart::CanonicalChart;
use rhythm_importer_tja::{BranchDecisionPoint, ImportedSong, TjaImportLimits, TjaImporter};
pub use taiko_resource_protocol::canonical_chart_sha256 as canonical_chart_hash;
use walkdir::WalkDir;

const MAX_LOCAL_TJA_FILES: usize = 4_096;

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
    pub canonical_chart_hash: String,
    pub object_count: usize,
    pub branch_segment_count: usize,
    pub base_bpm: Option<f64>,
    pub branch_decisions: Vec<BranchDecisionPoint>,
}

#[derive(Debug, Clone)]
pub struct SongEntry {
    pub origin: SongOrigin,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub demo_start_seconds: f64,
    pub courses: Vec<CourseEntry>,
}

impl SongEntry {
    pub fn song_id(&self) -> Option<&str> {
        self.remote_identity()
            .map(|identity| identity.song_id.as_str())
    }

    pub fn remote_identity(&self) -> Option<&RemoteSongIdentity> {
        match &self.origin {
            SongOrigin::Local { .. } => None,
            SongOrigin::Remote { identity, .. } => Some(identity),
        }
    }

    pub fn source_path(&self) -> &Path {
        match &self.origin {
            SongOrigin::Local { source_path, .. } | SongOrigin::Remote { source_path, .. } => {
                source_path
            }
        }
    }

    pub fn audio_path(&self) -> Option<&Path> {
        match &self.origin {
            SongOrigin::Local { audio_path, .. } | SongOrigin::Remote { audio_path, .. } => {
                audio_path.as_deref()
            }
        }
    }

    pub fn has_branching(&self) -> bool {
        self.courses
            .iter()
            .any(|course| !course.branch_decisions.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SongOrigin {
    Local {
        source_path: PathBuf,
        audio_path: Option<PathBuf>,
    },
    Remote {
        identity: RemoteSongIdentity,
        source_path: PathBuf,
        audio_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSongIdentity {
    pub song_id: String,
    pub source_id: String,
    pub audio_id: Option<String>,
}

impl RemoteSongIdentity {
    pub fn matches(&self, song_id: &str, source_id: &str, audio_id: Option<&str>) -> bool {
        self.song_id == song_id
            && self.source_id == source_id
            && self.audio_id.as_deref() == audio_id
    }
}

#[derive(Debug)]
enum LoadLibraryResult {
    Song(Box<SongEntry>),
    Warning(String),
}

pub fn load_song_library(songdir: &Path) -> Result<SongLibrary> {
    let chart_paths = discover_chart_paths(songdir, MAX_LOCAL_TJA_FILES)?;
    let mut songs = Vec::new();
    let mut warnings = Vec::new();
    songs
        .try_reserve_exact(chart_paths.len())
        .context("failed to reserve local song catalog")?;
    warnings
        .try_reserve_exact(chart_paths.len())
        .context("failed to reserve local song warnings")?;

    // Index sequentially so the 16 MiB per-chart read budget is also the
    // catalog-wide peak read budget instead of being multiplied by a thread pool.
    for chart_path in chart_paths {
        match load_single_song_entry(chart_path) {
            LoadLibraryResult::Song(song) => songs.push(*song),
            LoadLibraryResult::Warning(warning) => warnings.push(warning),
        }
    }

    songs.sort_by_cached_key(|song| (song.title.to_lowercase(), song.source_path().to_path_buf()));

    Ok(SongLibrary { songs, warnings })
}

fn discover_chart_paths(songdir: &Path, max_chart_files: usize) -> Result<Vec<PathBuf>> {
    let metadata = std::fs::metadata(songdir)
        .with_context(|| format!("failed to inspect song directory {}", songdir.display()))?;
    if !metadata.is_dir() {
        bail!("song directory is not a directory: {}", songdir.display());
    }

    collect_chart_paths(WalkDir::new(songdir), songdir, max_chart_files)
}

fn collect_chart_paths<I>(
    entries: I,
    songdir: &Path,
    max_chart_files: usize,
) -> Result<Vec<PathBuf>>
where
    I: IntoIterator<Item = std::result::Result<walkdir::DirEntry, walkdir::Error>>,
{
    let mut chart_paths = Vec::new();
    for entry in entries {
        let entry = entry
            .with_context(|| format!("failed to traverse song directory {}", songdir.display()))?;
        if !entry.file_type().is_file()
            || !entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("tja"))
        {
            continue;
        }
        if chart_paths.len() >= max_chart_files {
            bail!(
                "song directory {} exceeds the local catalog limit of {max_chart_files} TJA files",
                songdir.display()
            );
        }
        chart_paths.push(entry.into_path());
    }
    chart_paths.sort();
    Ok(chart_paths)
}

/// Read one local chart without ever accepting more bytes than the importer's
/// default raw-source budget.
///
/// The metadata check rejects already-oversized regular files before reserving
/// their declared size. `Read::take(max + 1)` then closes the race where a file
/// grows between the metadata check and the read.
pub(crate) fn read_chart_file_bounded(path: &Path) -> Result<Vec<u8>> {
    read_chart_file_bounded_with_limit(path, TjaImportLimits::DEFAULT.max_raw_bytes)
}

fn read_chart_file_bounded_with_limit(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    let file =
        File::open(path).with_context(|| format!("failed to open chart {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect chart {}", path.display()))?;
    if !metadata.is_file() {
        bail!("chart is not a regular file: {}", path.display());
    }

    let max_bytes_u64 = u64::try_from(max_bytes).context("chart byte limit does not fit in u64")?;
    if metadata.len() > max_bytes_u64 {
        bail!(
            "chart {} is {} bytes, exceeding the {max_bytes}-byte limit",
            path.display(),
            metadata.len()
        );
    }

    let declared_len = usize::try_from(metadata.len())
        .with_context(|| format!("chart size does not fit in usize: {}", path.display()))?;
    let mut raw = Vec::new();
    raw.try_reserve_exact(declared_len)
        .with_context(|| format!("failed to reserve chart buffer for {}", path.display()))?;

    let mut bounded = file.take(max_bytes_u64.saturating_add(1));
    bounded
        .read_to_end(&mut raw)
        .with_context(|| format!("failed to read chart {}", path.display()))?;
    if raw.len() > max_bytes {
        bail!(
            "chart {} grew beyond the {max_bytes}-byte limit while being read",
            path.display()
        );
    }
    Ok(raw)
}

#[cfg(test)]
fn load_course_chart(
    source_path: &Path,
    course_index: usize,
    importer: &TjaImporter,
) -> Result<CanonicalChart> {
    let raw = read_chart_file_bounded(source_path)?;
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
    let raw = match read_chart_file_bounded(&chart_path) {
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
        Ok(song) => LoadLibraryResult::Song(Box::new(song)),
        Err(error) => LoadLibraryResult::Warning(format!("skip {}: {error}", chart_path.display())),
    }
}

fn build_song_entry(source_path: PathBuf, imported: ImportedSong) -> Result<SongEntry> {
    if imported.courses.is_empty() {
        bail!("chart has no playable course: {}", source_path.display());
    }

    let parent = source_path.parent().unwrap_or_else(|| Path::new("."));
    let audio_path = imported.audio_path.as_ref().map(|path| parent.join(path));

    let courses = imported
        .courses
        .into_iter()
        .enumerate()
        .map(|(index, course)| {
            let canonical_chart_hash = canonical_chart_hash(&course.chart)?;
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

            Ok(CourseEntry {
                index,
                name,
                level: course.chart.metadata.difficulty_level,
                canonical_chart_hash,
                object_count: course.chart.objects.len(),
                branch_segment_count: course.chart.branch_segments.len(),
                base_bpm,
                branch_decisions: course.branch_decisions,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let title = if imported.title.trim().is_empty() {
        source_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map_or_else(|| "Untitled".to_owned(), ToOwned::to_owned)
    } else {
        imported.title
    };

    Ok(SongEntry {
        origin: SongOrigin::Local {
            source_path,
            audio_path,
        },
        title,
        subtitle: imported.subtitle,
        artist: imported.artist,
        demo_start_seconds: imported.demo_start_seconds.unwrap_or(0.0),
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

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "taiko-loader-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            fs::create_dir_all(&path).expect("create test directory");
            Self { path }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn song_origin_cannot_represent_partial_or_conflicting_remote_identity() {
        let song = SongEntry {
            origin: SongOrigin::Remote {
                identity: RemoteSongIdentity {
                    song_id: "song".to_owned(),
                    source_id: "source".to_owned(),
                    audio_id: Some("audio".to_owned()),
                },
                source_path: PathBuf::from("pack/song.tja"),
                audio_path: Some(PathBuf::from("pack/song.ogg")),
            },
            title: "Song".to_owned(),
            subtitle: String::new(),
            artist: String::new(),
            demo_start_seconds: 0.0,
            courses: Vec::new(),
        };

        let identity = song.remote_identity().expect("remote identity");
        assert_eq!(song.song_id(), Some("song"));
        assert!(identity.matches("song", "source", Some("audio")));
        assert!(!identity.matches("other", "source", Some("audio")));
        assert!(!identity.matches("song", "other", Some("audio")));
        assert!(!identity.matches("song", "source", Some("other")));
    }

    #[test]
    fn local_song_origin_has_typed_paths_and_no_remote_identity() {
        let source_path = PathBuf::from("songs/local.tja");
        let audio_path = PathBuf::from("songs/local.ogg");
        let song = SongEntry {
            origin: SongOrigin::Local {
                source_path: source_path.clone(),
                audio_path: Some(audio_path.clone()),
            },
            title: "Local".to_owned(),
            subtitle: String::new(),
            artist: String::new(),
            demo_start_seconds: 0.0,
            courses: Vec::new(),
        };

        assert_eq!(song.source_path(), source_path);
        assert_eq!(song.audio_path(), Some(audio_path.as_path()));
        assert_eq!(song.song_id(), None);
        assert_eq!(song.remote_identity(), None);
    }

    #[test]
    fn empty_wave_does_not_guess_a_same_stem_audio_file() {
        let temp_root = TestDir::new("silent-song");
        let chart_path = temp_root.path.join("silent.tja");
        fs::write(
            &chart_path,
            b"TITLE:Silent\nWAVE:\nBPM:120\nCOURSE:Oni\n#START\n1,\n#END\n",
        )
        .expect("write silent chart");
        fs::write(temp_root.path.join("silent.ogg"), b"must not be guessed")
            .expect("write misleading same-stem audio");

        let library = load_song_library(&temp_root.path).expect("load library");
        assert!(library.warnings.is_empty());
        let song = library.songs.first().expect("silent song");
        assert_eq!(song.audio_path(), None);
        assert_eq!(
            song.origin,
            SongOrigin::Local {
                source_path: chart_path,
                audio_path: None,
            }
        );
    }

    #[test]
    fn summary_matches_lazy_loaded_course_chart() {
        let temp_root = TestDir::new("summary");
        let chart_path = temp_root.path.join("sample.tja");
        fs::write(&chart_path, SAMPLE_TJA).expect("write sample chart");

        let library = load_song_library(&temp_root.path).expect("load library");
        let song = library.songs.first().expect("song entry");
        let course = song.courses.first().expect("course summary");

        let importer = TjaImporter;
        let chart = load_course_chart(song.source_path(), course.index, &importer)
            .expect("load selected course chart");

        assert_eq!(course.object_count, chart.objects.len());
        assert_eq!(course.branch_segment_count, chart.branch_segments.len());
        assert_eq!(
            course.canonical_chart_hash,
            canonical_chart_hash(&chart).expect("canonical chart hash")
        );
        let expected_bpm = chart
            .tempo_map
            .first()
            .map(|tempo| 60_000_000.0 / tempo.micros_per_quarter as f64);
        assert_eq!(course.base_bpm, expected_bpm);
    }

    #[test]
    fn oversized_sparse_chart_is_rejected_before_reading() {
        let temp_root = TestDir::new("oversized");
        let chart_path = temp_root.path.join("oversized.tja");
        let file = fs::File::create(&chart_path).expect("create sparse chart");
        file.set_len(
            u64::try_from(TjaImportLimits::DEFAULT.max_raw_bytes).expect("limit fits") + 1,
        )
        .expect("size sparse chart");

        let error = read_chart_file_bounded(&chart_path).expect_err("reject oversized chart");
        assert!(
            error.to_string().contains("exceeding the"),
            "unexpected error: {error:#}"
        );

        let library = load_song_library(&temp_root.path).expect("load bounded library");
        assert!(library.songs.is_empty());
        assert_eq!(library.warnings.len(), 1);
        assert!(library.warnings[0].contains("exceeding the"));
    }

    #[test]
    fn discovery_fails_as_soon_as_injected_limit_is_exceeded() {
        let temp_root = TestDir::new("catalog-limit");
        for name in ["a.tja", "b.TJA", "c.tja"] {
            fs::write(temp_root.path.join(name), b"").expect("write chart placeholder");
        }
        fs::write(temp_root.path.join("ignored.txt"), b"").expect("write non-chart");

        let exact = discover_chart_paths(&temp_root.path, 3).expect("accept exact catalog limit");
        assert_eq!(exact.len(), 3);

        let error = discover_chart_paths(&temp_root.path, 2).expect_err("reject oversized catalog");
        assert!(
            error.to_string().contains("limit of 2 TJA files"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn songdir_must_be_a_directory() {
        let temp_root = TestDir::new("not-directory");
        let file_path = temp_root.path.join("songs");
        fs::write(&file_path, b"not a directory").expect("write plain file");

        let error = load_song_library(&file_path).expect_err("reject non-directory");
        assert!(
            error.to_string().contains("is not a directory"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    fn walkdir_errors_are_propagated() {
        let temp_root = TestDir::new("walk-error");
        let removed_root = temp_root.path.join("removed-before-walk");
        fs::create_dir(&removed_root).expect("create walk root");
        let pending_walk = WalkDir::new(&removed_root).into_iter();
        fs::remove_dir(&removed_root).expect("remove walk root");

        let error = collect_chart_paths(pending_walk, &removed_root, 1)
            .expect_err("propagate deferred walk error");
        assert!(
            error
                .to_string()
                .contains("failed to traverse song directory"),
            "unexpected error: {error:#}"
        );
    }

    #[test]
    #[ignore = "release catalog validation requires the local ignored taiko-game/songs fixture"]
    fn built_in_catalog_contains_all_41_songs_without_warnings() {
        let songdir = Path::new(env!("CARGO_MANIFEST_DIR")).join("songs");
        let library = load_song_library(&songdir).expect("load built-in song catalog");

        assert!(
            library.warnings.is_empty(),
            "built-in catalog warnings:\n{}",
            library.warnings.join("\n")
        );
        assert_eq!(
            library.songs.len(),
            41,
            "every checked-in TJA must produce one playable catalog entry"
        );
        assert!(
            library.songs.iter().all(|song| !song.courses.is_empty()),
            "every built-in song must expose at least one playable course"
        );
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
