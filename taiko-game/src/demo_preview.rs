//! Asynchronous song-preview loading for both local and online GUI menus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::audio::{prepare_song_audio, PreparedSongAudio};
use crate::latest_background::{BackgroundCompletion, BackgroundEvent, LatestBackgroundTask};
use crate::loader::SongEntry;
use crate::resource::ResourceBackend;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DemoPreviewIdentity {
    pub(crate) generation: u64,
    pub(crate) song_index: usize,
}

pub(crate) struct PreparedDemoPreview {
    pub(crate) audio: PreparedSongAudio,
    pub(crate) start_seconds: f64,
}

pub(crate) type DemoPreviewCompletion = BackgroundCompletion<PreparedDemoPreview>;
pub(crate) type DemoPreviewEvent = BackgroundEvent<DemoPreviewIdentity, PreparedDemoPreview>;

pub(crate) struct DemoPreviewTask {
    task: LatestBackgroundTask<DemoPreviewIdentity, PreparedDemoPreview>,
}

impl Default for DemoPreviewTask {
    fn default() -> Self {
        Self {
            task: LatestBackgroundTask::new("taiko-demo-preview"),
        }
    }
}

impl DemoPreviewTask {
    pub(crate) fn start(
        &mut self,
        identity: DemoPreviewIdentity,
        backend: Arc<ResourceBackend>,
        song: SongEntry,
    ) -> Result<()> {
        self.start_job(identity, move |cancelled| {
            prepare_preview(&backend, &song, cancelled)
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self) -> Vec<DemoPreviewEvent> {
        self.task.poll()
    }

    fn start_job(
        &mut self,
        identity: DemoPreviewIdentity,
        job: impl FnOnce(&AtomicBool) -> Result<Option<PreparedDemoPreview>> + Send + 'static,
    ) -> Result<()> {
        self.task.start(identity, job)
    }
}

pub(crate) fn event_is_current(
    current: Option<DemoPreviewIdentity>,
    event: &DemoPreviewEvent,
) -> bool {
    crate::latest_background::event_is_current(current, event)
}

fn prepare_preview(
    backend: &ResourceBackend,
    song: &SongEntry,
    cancelled: &AtomicBool,
) -> Result<Option<PreparedDemoPreview>> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let audio_source = backend
        .load_song_audio(song)
        .with_context(|| format!("failed to load preview for {}", song.title))?;
    let Some(audio_source) = audio_source else {
        return Ok(None);
    };
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let audio = prepare_song_audio(audio_source)
        .with_context(|| format!("failed to decode preview for {}", song.title))?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    Ok(Some(PreparedDemoPreview {
        audio,
        start_seconds: song.demo_start_seconds,
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::{Duration, Instant};

    use super::*;

    fn identity(generation: u64, song_index: usize) -> DemoPreviewIdentity {
        DemoPreviewIdentity {
            generation,
            song_index,
        }
    }

    #[test]
    fn slow_preview_loader_never_blocks_the_calling_thread() {
        let identity = identity(1, 4);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let mut task = DemoPreviewTask::default();

        let started_at = Instant::now();
        task.start_job(identity, move |_| {
            release_rx.recv().expect("release loader");
            Ok(None)
        })
        .expect("start preview");
        assert!(started_at.elapsed() < Duration::from_millis(100));

        release_tx.send(()).expect("release loader");
        let deadline = Instant::now() + Duration::from_secs(2);
        while task.poll().is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn rapid_selection_changes_coalesce_to_the_latest_preview() {
        let first = identity(1, 1);
        let superseded = identity(2, 2);
        let latest = identity(3, 3);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        let (latest_started_tx, latest_started_rx) = mpsc::sync_channel(0);
        let mut task = DemoPreviewTask::default();

        task.start_job(first, move |_| {
            release_rx.recv().expect("release first");
            Ok(None)
        })
        .expect("start first");
        task.start_job(superseded, |_| {
            panic!("superseded queued preview must never start")
        })
        .expect("queue superseded");
        task.start_job(latest, move |_| {
            latest_started_tx.send(()).expect("announce latest");
            Ok(None)
        })
        .expect("replace queued preview");
        assert!(matches!(
            latest_started_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));

        release_tx.send(()).expect("release first");
        let deadline = Instant::now() + Duration::from_secs(2);
        while latest_started_rx.try_recv().is_err() {
            let _ = task.poll();
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn stale_preview_result_cannot_replace_the_current_selection() {
        let stale = DemoPreviewEvent {
            identity: identity(8, 2),
            completion: DemoPreviewCompletion::Cancelled,
        };
        let current = DemoPreviewEvent {
            identity: identity(9, 3),
            completion: DemoPreviewCompletion::Cancelled,
        };

        assert!(!event_is_current(Some(current.identity), &stale));
        assert!(event_is_current(Some(current.identity), &current));
    }
}
