use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use anyhow::{bail, Context, Result};
use rhythm_importer_tja::BranchDecisionPoint;
use rhythm_mode_taiko::{TaikoBranchPolicy, TaikoRuntime};
use taiko_multiplayer_protocol::{
    ErrorMessage, MatchId, PlayerSelection, PreparationProgress, ProgressMilli, SongManifest,
};

use crate::loader::SongEntry;
use crate::online::{LocalPlayerRuntime, PreparedMatch};
use crate::resource::{is_resource_load_cancelled, ResourceBackend};

const PREPARATION_EVENT_CAPACITY: usize = 6;

pub(crate) fn validate_authoritative_song_identity(
    song: &SongEntry,
    manifest: &SongManifest,
) -> Result<()> {
    let identity = song
        .remote_identity()
        .ok_or_else(|| anyhow::anyhow!("authoritative song resolved to a local library entry"))?;
    if !identity.matches(
        manifest.song_id.as_str(),
        manifest.source_id.as_str(),
        manifest.audio_id.as_ref().map(|audio_id| audio_id.as_str()),
    ) {
        bail!("downloaded multiplayer content does not match the authoritative manifest");
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparationIdentity {
    pub(crate) session_generation: u64,
    pub(crate) match_id: MatchId,
    pub(crate) selection: PlayerSelection,
}

pub(crate) struct PreparationRequest {
    pub(crate) identity: PreparationIdentity,
    pub(crate) backend: Arc<ResourceBackend>,
    pub(crate) song: SongEntry,
    pub(crate) course_index: usize,
    pub(crate) branch_decisions: Vec<BranchDecisionPoint>,
}

pub(crate) struct PreparedOnlineState {
    pub(crate) prepared_match: PreparedMatch,
    pub(crate) runtime: LocalPlayerRuntime,
}

pub(crate) enum PreparationCompletion {
    Prepared(Box<PreparedOnlineState>),
    Cancelled,
    Failed(anyhow::Error),
}

pub(crate) enum PreparationEvent {
    Progress {
        identity: PreparationIdentity,
        progress: PreparationProgress,
    },
    Finished {
        identity: PreparationIdentity,
        completion: PreparationCompletion,
    },
}

impl PreparationEvent {
    pub(crate) fn identity(&self) -> PreparationIdentity {
        match self {
            Self::Progress { identity, .. } | Self::Finished { identity, .. } => *identity,
        }
    }

    fn is_finished(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

struct RunningPreparation {
    identity: PreparationIdentity,
    cancelled: Arc<AtomicBool>,
    events: Receiver<PreparationEvent>,
    thread: JoinHandle<()>,
}

#[derive(Default)]
pub(crate) struct OnlinePreparationTask {
    running: Option<RunningPreparation>,
    failure: Option<PreparationFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparationFailure {
    identity: PreparationIdentity,
    reason: String,
}

impl OnlinePreparationTask {
    pub(crate) fn identity(&self) -> Option<PreparationIdentity> {
        self.running.as_ref().map(|running| running.identity)
    }

    pub(crate) fn start(&mut self, request: PreparationRequest) -> Result<bool> {
        let identity = request.identity;
        self.start_job(identity, move |events, cancelled| {
            run_preparation(request, &events, &cancelled);
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.failure = None;
        if let Some(running) = &self.running {
            running.cancelled.store(true, Ordering::Release);
        }
    }

    pub(crate) fn failure_reason(&self, identity: PreparationIdentity) -> Option<&str> {
        self.failure
            .as_ref()
            .filter(|failure| failure.identity == identity)
            .map(|failure| failure.reason.as_str())
    }

    pub(crate) fn retry(&mut self, identity: PreparationIdentity) -> bool {
        if self.running.is_some() {
            return false;
        }
        if self
            .failure
            .as_ref()
            .is_some_and(|failure| failure.identity == identity)
        {
            self.failure = None;
            return true;
        }
        false
    }

    pub(crate) fn poll(&mut self) -> Vec<PreparationEvent> {
        let Some(running) = &self.running else {
            return Vec::new();
        };

        let mut events = Vec::with_capacity(PREPARATION_EVENT_CAPACITY);
        let mut disconnected = false;
        loop {
            match running.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }

        let finished = disconnected || events.iter().any(PreparationEvent::is_finished);
        if finished {
            let running = self
                .running
                .take()
                .expect("running preparation was just observed");
            let worker_panicked = running.thread.join().is_err();
            if !events.iter().any(PreparationEvent::is_finished) {
                let reason = if worker_panicked {
                    "online preparation worker panicked"
                } else {
                    "online preparation worker exited without a completion event"
                };
                events.push(PreparationEvent::Finished {
                    identity: running.identity,
                    completion: PreparationCompletion::Failed(anyhow::anyhow!(reason)),
                });
            }
        }
        if let Some((identity, reason)) = events.iter().find_map(|event| match event {
            PreparationEvent::Finished {
                identity,
                completion: PreparationCompletion::Failed(error),
            } => Some((*identity, format!("{error:#}"))),
            _ => None,
        }) {
            self.failure = Some(PreparationFailure { identity, reason });
        }

        events
    }

    fn start_job(
        &mut self,
        identity: PreparationIdentity,
        job: impl FnOnce(SyncSender<PreparationEvent>, Arc<AtomicBool>) + Send + 'static,
    ) -> Result<bool> {
        if self.running.is_some() {
            return Ok(false);
        }
        if self
            .failure
            .as_ref()
            .is_some_and(|failure| failure.identity == identity)
        {
            return Ok(false);
        }
        self.failure = None;

        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (event_tx, event_rx) = mpsc::sync_channel(PREPARATION_EVENT_CAPACITY);
        let thread = thread::Builder::new()
            .name("taiko-online-preparation".to_owned())
            .spawn(move || job(event_tx, worker_cancelled))
            .context("failed to start online preparation worker")?;
        self.running = Some(RunningPreparation {
            identity,
            cancelled,
            events: event_rx,
            thread,
        });
        Ok(true)
    }
}

impl Drop for OnlinePreparationTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

fn run_preparation(
    request: PreparationRequest,
    events: &SyncSender<PreparationEvent>,
    cancelled: &AtomicBool,
) {
    let identity = request.identity;
    let completion = match prepare(request, events, cancelled) {
        Ok(Some(prepared)) => PreparationCompletion::Prepared(Box::new(prepared)),
        Ok(None) => PreparationCompletion::Cancelled,
        Err(error) => {
            let _ = send_progress(
                events,
                identity,
                PreparationProgress::Failed {
                    selection: Some(identity.selection),
                    reason: ErrorMessage::new("local content preparation failed")
                        .expect("static preparation failure reason is bounded"),
                },
            );
            PreparationCompletion::Failed(error)
        }
    };
    let _ = events.send(PreparationEvent::Finished {
        identity,
        completion,
    });
}

fn prepare(
    request: PreparationRequest,
    events: &SyncSender<PreparationEvent>,
    cancelled: &AtomicBool,
) -> Result<Option<PreparedOnlineState>> {
    if is_cancelled(cancelled) {
        return Ok(None);
    }
    if !send_progress(
        events,
        request.identity,
        PreparationProgress::Downloading {
            selection: request.identity.selection,
            progress_milli: ProgressMilli::new(0).expect("zero progress is valid"),
        },
    ) {
        return Ok(None);
    }

    let chart = match request.backend.load_course_chart_cancellable(
        &request.song,
        request.course_index,
        &rhythm_importer_tja::TjaImporter,
        &|| is_cancelled(cancelled),
    ) {
        Ok(chart) => chart,
        Err(error) if is_resource_load_cancelled(&error) => return Ok(None),
        Err(error) => return Err(error.context("failed to load verified online chart")),
    };
    if is_cancelled(cancelled) {
        return Ok(None);
    }
    let audio_source = match request
        .backend
        .load_song_audio_cancellable(&request.song, &|| is_cancelled(cancelled))
    {
        Ok(audio) => audio,
        Err(error) if is_resource_load_cancelled(&error) => return Ok(None),
        Err(error) => return Err(error.context("failed to load verified online audio")),
    };
    if is_cancelled(cancelled) {
        return Ok(None);
    }

    if !send_progress(
        events,
        request.identity,
        PreparationProgress::Downloading {
            selection: request.identity.selection,
            progress_milli: ProgressMilli::new(ProgressMilli::MAX)
                .expect("maximum progress is valid"),
        },
    ) {
        return Ok(None);
    }
    if !send_progress(
        events,
        request.identity,
        PreparationProgress::Verifying {
            selection: request.identity.selection,
        },
    ) {
        return Ok(None);
    }
    if is_cancelled(cancelled) {
        return Ok(None);
    }
    if !send_progress(
        events,
        request.identity,
        PreparationProgress::Loading {
            selection: request.identity.selection,
        },
    ) {
        return Ok(None);
    }

    let audio = match audio_source {
        Some(audio_source) => {
            let audio_result = crate::audio::prepare_song_audio_cancellable(audio_source, &|| {
                is_cancelled(cancelled)
            });
            if is_cancelled(cancelled) {
                return Ok(None);
            }
            Some(audio_result.context("failed to decode and validate online audio")?)
        }
        None => None,
    };
    let mut gameplay = TaikoRuntime::new(
        &chart,
        TaikoBranchPolicy::Automatic,
        request.branch_decisions,
    )
    .context("invalid authoritative taiko runtime")?;
    let initial = gameplay
        .advance_to(0, &[])
        .context("failed to compile the online chart")?;
    if is_cancelled(cancelled) {
        return Ok(None);
    }

    Ok(Some(PreparedOnlineState {
        prepared_match: PreparedMatch {
            match_id: request.identity.match_id,
            selection: request.identity.selection,
            audio,
        },
        runtime: LocalPlayerRuntime {
            match_id: request.identity.match_id,
            gameplay,
            pending_inputs: Vec::new(),
            last_tick: 0,
            last_output: initial,
            music_started: false,
            audio_sync: None,
            judge_flash: None,
            input_flash: None,
        },
    }))
}

fn send_progress(
    events: &SyncSender<PreparationEvent>,
    identity: PreparationIdentity,
    progress: PreparationProgress,
) -> bool {
    events
        .send(PreparationEvent::Progress { identity, progress })
        .is_ok()
}

fn is_cancelled(cancelled: &AtomicBool) -> bool {
    cancelled.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use taiko_multiplayer_protocol::{
        BoundedText, BoundedVec, ContentHash, CourseId, CourseManifest, CourseName, DisplayTitle,
        MatchSemantics, SongId,
    };

    use super::*;
    use crate::cli::CliArgs;
    use crate::loader::{RemoteSongIdentity, SongOrigin};

    fn hash(character: char) -> ContentHash {
        ContentHash::parse(character.to_string().repeat(64)).expect("valid content hash")
    }

    fn song_manifest() -> SongManifest {
        let mut song = SongManifest {
            song_id: SongId::parse("9".repeat(64)).expect("song id"),
            source_id: hash('a'),
            audio_id: Some(hash('b')),
            title: DisplayTitle::new("Test Song").expect("title"),
            subtitle: BoundedText::new("").expect("subtitle"),
            artist: BoundedText::new("Tester").expect("artist"),
            semantics: MatchSemantics {
                canonical_schema_version: 1,
                canonical_schema_digest: hash('c'),
                importer_semantics_version: 1,
                importer_semantics_digest: hash('d'),
                ruleset_version: 1,
                ruleset_digest: hash('e'),
                audio_decoder_semantics_version: 1,
                audio_decoder_semantics_digest: hash('6'),
            },
            courses: BoundedVec::new(vec![CourseManifest {
                course_id: CourseId(0),
                name: CourseName::new("Oni").expect("course name"),
                level: Some(8),
                canonical_chart_hash: hash('f'),
            }])
            .expect("bounded courses"),
        };
        song.song_id = song.derive_song_id().expect("derived fixture song id");
        song
    }

    fn remote_song(manifest: &SongManifest) -> SongEntry {
        SongEntry {
            origin: SongOrigin::Remote {
                identity: RemoteSongIdentity {
                    song_id: manifest.song_id.to_string(),
                    source_id: manifest.source_id.to_string(),
                    audio_id: manifest.audio_id.as_ref().map(ToString::to_string),
                },
                source_path: "pack/song.tja".into(),
                audio_path: Some("pack/song.ogg".into()),
            },
            title: "Test Song".to_owned(),
            subtitle: String::new(),
            artist: "Tester".to_owned(),
            demo_start_seconds: 0.0,
            courses: Vec::new(),
        }
    }

    fn identity(generation: u64, match_id: u64) -> PreparationIdentity {
        PreparationIdentity {
            session_generation: generation,
            match_id: MatchId(match_id),
            selection: PlayerSelection {
                course_id: CourseId(2),
            },
        }
    }

    #[test]
    fn authoritative_manifest_requires_the_complete_atomic_remote_identity() {
        let manifest = song_manifest();
        let song = remote_song(&manifest);
        validate_authoritative_song_identity(&song, &manifest).expect("matching identity");

        for mismatched in [
            RemoteSongIdentity {
                song_id: "0".repeat(64),
                source_id: manifest.source_id.to_string(),
                audio_id: manifest.audio_id.as_ref().map(ToString::to_string),
            },
            RemoteSongIdentity {
                song_id: manifest.song_id.to_string(),
                source_id: "1".repeat(64),
                audio_id: manifest.audio_id.as_ref().map(ToString::to_string),
            },
            RemoteSongIdentity {
                song_id: manifest.song_id.to_string(),
                source_id: manifest.source_id.to_string(),
                audio_id: Some("2".repeat(64)),
            },
        ] {
            let mut forged = song.clone();
            let SongOrigin::Remote { identity, .. } = &mut forged.origin else {
                unreachable!("fixture is remote");
            };
            *identity = mismatched;
            let error = validate_authoritative_song_identity(&forged, &manifest)
                .expect_err("every identity component is authoritative");
            assert!(error
                .to_string()
                .contains("does not match the authoritative manifest"));
        }
    }

    #[test]
    fn authoritative_manifest_rejects_a_local_song_entry() {
        let manifest = song_manifest();
        let mut song = remote_song(&manifest);
        song.origin = SongOrigin::Local {
            source_path: "songs/local.tja".into(),
            audio_path: Some("songs/local.ogg".into()),
        };

        let error = validate_authoritative_song_identity(&song, &manifest)
            .expect_err("local entry has no remote identity");
        assert!(error
            .to_string()
            .contains("resolved to a local library entry"));
    }

    fn fixture_request(
        label: &str,
        audio: &[u8],
        identity: PreparationIdentity,
    ) -> (std::path::PathBuf, PreparationRequest) {
        let temp_root = std::env::temp_dir().join(format!(
            "taiko-online-preparation-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir(&temp_root).expect("create fixture directory");
        std::fs::write(
            temp_root.join("fixture.tja"),
            include_bytes!("../samples/Nosferatu.tja"),
        )
        .expect("write chart fixture");
        std::fs::write(temp_root.join("Nosferatu.ogg"), audio).expect("write audio fixture");

        let args = CliArgs {
            songdir: temp_root.clone(),
            resource_endpoint: None,
            resource_cache_memory_only: false,
            tps: 120,
            calibration_offset_ms: 0,
            demo: false,
            songvol: 100,
            sevol: 100,
        };
        let backend =
            Arc::new(ResourceBackend::from_cli(&args).expect("create local resource backend"));
        let song = backend
            .load_song_library()
            .expect("load fixture library")
            .songs
            .into_iter()
            .next()
            .expect("fixture song");
        let course = song.courses.first().expect("fixture course");
        let request = PreparationRequest {
            identity,
            backend,
            course_index: course.index,
            branch_decisions: course.branch_decisions.clone(),
            song,
        };
        (temp_root, request)
    }

    #[test]
    fn task_allows_only_one_background_job() {
        let first = identity(1, 7);
        let second = identity(1, 8);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let mut task = OnlinePreparationTask::default();

        assert!(task
            .start_job(first, move |events, _| {
                release_rx.recv().expect("release worker");
                events
                    .send(PreparationEvent::Finished {
                        identity: first,
                        completion: PreparationCompletion::Cancelled,
                    })
                    .expect("send completion");
            })
            .expect("start first job"));
        assert!(!task
            .start_job(second, |_, _| unreachable!("second job must not start"))
            .expect("reject overlapping job"));
        assert_eq!(task.identity(), Some(first));

        release_tx.send(()).expect("release first worker");
        for _ in 0..100 {
            if task.poll().iter().any(PreparationEvent::is_finished) {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(task.identity(), None);
    }

    #[test]
    fn identity_distinguishes_reconnected_sessions_with_reused_match_ids() {
        assert_ne!(identity(1, 7), identity(2, 7));
    }

    #[test]
    fn cancellation_is_visible_to_the_single_worker() {
        let identity = identity(3, 9);
        let (started_tx, started_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let mut task = OnlinePreparationTask::default();
        task.start_job(identity, move |events, cancelled| {
            started_tx.send(()).expect("announce worker start");
            release_rx.recv().expect("release worker");
            events
                .send(PreparationEvent::Finished {
                    identity,
                    completion: if cancelled.load(Ordering::Acquire) {
                        PreparationCompletion::Cancelled
                    } else {
                        PreparationCompletion::Failed(anyhow::anyhow!("worker missed cancellation"))
                    },
                })
                .expect("send completion");
        })
        .expect("start worker");

        started_rx.recv().expect("worker started");
        task.cancel();
        release_tx.send(()).expect("release worker");

        let completion = loop {
            if let Some(event) = task.poll().into_iter().find(PreparationEvent::is_finished) {
                break event;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(matches!(
            completion,
            PreparationEvent::Finished {
                completion: PreparationCompletion::Cancelled,
                ..
            }
        ));
    }

    #[test]
    fn failed_identity_is_nonfatal_latched_and_explicitly_retryable() {
        let identity = identity(7, 21);
        let attempts = Arc::new(AtomicUsize::new(0));
        let mut task = OnlinePreparationTask::default();
        let first_attempts = Arc::clone(&attempts);
        assert!(task
            .start_job(identity, move |events, _| {
                first_attempts.fetch_add(1, Ordering::Relaxed);
                events
                    .send(PreparationEvent::Finished {
                        identity,
                        completion: PreparationCompletion::Failed(anyhow::anyhow!(
                            "fixture codec failure"
                        )),
                    })
                    .expect("send failure");
            })
            .expect("start failing worker"));

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let events = task.poll();
            if events.iter().any(PreparationEvent::is_finished) {
                assert!(matches!(
                    events.as_slice(),
                    [PreparationEvent::Finished {
                        completion: PreparationCompletion::Failed(_),
                        ..
                    }]
                ));
                break;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(
            task.failure_reason(identity),
            Some("fixture codec failure"),
            "the adapter can render a local failure without terminating the room"
        );

        let suppressed_attempts = Arc::clone(&attempts);
        assert!(
            !task
                .start_job(identity, move |_, _| {
                    suppressed_attempts.fetch_add(1, Ordering::Relaxed);
                })
                .expect("latched start is a normal no-op"),
            "the same failed identity must not start again on the next tick"
        );
        assert_eq!(
            attempts.load(Ordering::Relaxed),
            1,
            "the failure latch must prevent an automatic retry storm"
        );

        assert!(task.retry(identity), "explicit retry must clear the latch");
        let retry_attempts = Arc::clone(&attempts);
        assert!(task
            .start_job(identity, move |events, _| {
                retry_attempts.fetch_add(1, Ordering::Relaxed);
                events
                    .send(PreparationEvent::Finished {
                        identity,
                        completion: PreparationCompletion::Cancelled,
                    })
                    .expect("send retry completion");
            })
            .expect("start explicit retry"));
        while task.identity().is_some() {
            let _ = task.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        assert_eq!(task.failure_reason(identity), None);
    }

    #[test]
    fn different_identity_supersedes_a_failure_latch() {
        let failed = identity(8, 22);
        let replacement = identity(8, 23);
        let mut task = OnlinePreparationTask::default();
        assert!(task
            .start_job(failed, move |events, _| {
                events
                    .send(PreparationEvent::Finished {
                        identity: failed,
                        completion: PreparationCompletion::Failed(anyhow::anyhow!("failed")),
                    })
                    .expect("send failure");
            })
            .expect("start failing worker"));
        while task.identity().is_some() {
            let _ = task.poll();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(task.failure_reason(failed).is_some());

        assert!(task
            .start_job(replacement, move |events, _| {
                events
                    .send(PreparationEvent::Finished {
                        identity: replacement,
                        completion: PreparationCompletion::Cancelled,
                    })
                    .expect("send replacement completion");
            })
            .expect("a different selection or match starts immediately"));
        assert_eq!(task.failure_reason(failed), None);
    }

    #[test]
    fn worker_loads_chart_and_builds_shared_runtime_off_ui_thread() {
        let identity = PreparationIdentity {
            session_generation: 4,
            match_id: MatchId(11),
            selection: PlayerSelection {
                course_id: CourseId(0),
            },
        };
        let (temp_root, request) =
            fixture_request("valid", include_bytes!("../assets/don.wav"), identity);
        let mut task = OnlinePreparationTask::default();
        assert!(task.start(request).expect("start preparation"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut progress = Vec::new();
        let prepared = loop {
            let mut prepared = None;
            for event in task.poll() {
                match event {
                    PreparationEvent::Progress { progress: next, .. } => progress.push(next),
                    PreparationEvent::Finished { completion, .. } => match completion {
                        PreparationCompletion::Prepared(result) => prepared = Some(result),
                        PreparationCompletion::Cancelled => panic!("preparation was cancelled"),
                        PreparationCompletion::Failed(error) => {
                            panic!("preparation failed: {error}")
                        }
                    },
                }
            }
            if let Some(prepared) = prepared {
                break prepared;
            }
            assert!(
                Instant::now() < deadline,
                "preparation did not finish before deadline"
            );
            std::thread::sleep(Duration::from_millis(1));
        };

        assert_eq!(
            progress,
            vec![
                PreparationProgress::Downloading {
                    selection: identity.selection,
                    progress_milli: ProgressMilli::new(0).expect("progress"),
                },
                PreparationProgress::Downloading {
                    selection: identity.selection,
                    progress_milli: ProgressMilli::new(ProgressMilli::MAX).expect("progress"),
                },
                PreparationProgress::Verifying {
                    selection: identity.selection,
                },
                PreparationProgress::Loading {
                    selection: identity.selection,
                },
            ],
            "the production worker must emit the exact server preparation barrier order"
        );
        assert_eq!(prepared.prepared_match.match_id, identity.match_id);
        assert_eq!(prepared.prepared_match.selection, identity.selection);
        assert_eq!(prepared.runtime.match_id, identity.match_id);
        std::fs::remove_dir_all(&temp_root).expect("remove fixture directory");
    }

    #[test]
    fn corrupt_audio_never_produces_the_prepared_state_required_for_ready() {
        let identity = PreparationIdentity {
            session_generation: 5,
            match_id: MatchId(12),
            selection: PlayerSelection {
                course_id: CourseId(0),
            },
        };
        let (temp_root, request) = fixture_request("corrupt", b"not an audio stream", identity);
        let mut task = OnlinePreparationTask::default();
        assert!(task.start(request).expect("start preparation"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut progress = Vec::new();
        let failure = loop {
            let mut failure = None;
            for event in task.poll() {
                match event {
                    PreparationEvent::Progress { progress: next, .. } => progress.push(next),
                    PreparationEvent::Finished { completion, .. } => match completion {
                        PreparationCompletion::Prepared(_) => {
                            panic!("corrupt audio reached Prepared and could be marked Ready")
                        }
                        PreparationCompletion::Cancelled => panic!("preparation was cancelled"),
                        PreparationCompletion::Failed(error) => failure = Some(error),
                    },
                }
            }
            if let Some(failure) = failure {
                break failure;
            }
            assert!(
                Instant::now() < deadline,
                "corrupt preparation did not fail before deadline"
            );
            std::thread::sleep(Duration::from_millis(1));
        };

        assert!(
            failure
                .to_string()
                .contains("failed to decode and validate online audio"),
            "{failure:#}"
        );
        assert!(matches!(
            progress.last(),
            Some(PreparationProgress::Failed {
                selection: Some(selection),
                ..
            }) if *selection == identity.selection
        ));
        std::fs::remove_dir_all(&temp_root).expect("remove fixture directory");
    }
}
