use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use rhythm_chart::CanonicalChart;

use crate::audio::{prepare_song_audio_cancellable, PreparedSongAudio};
use crate::latest_background::{BackgroundCompletion, BackgroundEvent, LatestBackgroundTask};
use crate::loader::SongEntry;
use crate::resource::{is_resource_load_cancelled, ResourceBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OfflinePreparationMode {
    Single,
    LocalTwoPlayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OfflinePreparationIdentity {
    pub(crate) generation: u64,
    pub(crate) mode: OfflinePreparationMode,
    pub(crate) song_index: usize,
    pub(crate) course_indices: [usize; 2],
}

impl OfflinePreparationIdentity {
    pub(crate) const fn single(generation: u64, song_index: usize, course_index: usize) -> Self {
        Self {
            generation,
            mode: OfflinePreparationMode::Single,
            song_index,
            course_indices: [course_index, course_index],
        }
    }

    pub(crate) const fn local(
        generation: u64,
        song_index: usize,
        course_indices: [usize; 2],
    ) -> Self {
        Self {
            generation,
            mode: OfflinePreparationMode::LocalTwoPlayer,
            song_index,
            course_indices,
        }
    }
}

pub(crate) struct OfflinePreparationRequest {
    pub(crate) identity: OfflinePreparationIdentity,
    pub(crate) backend: Arc<ResourceBackend>,
    pub(crate) song: SongEntry,
}

pub(crate) enum PreparedOfflineCharts {
    Single(Box<CanonicalChart>),
    Local(Box<[CanonicalChart; 2]>),
}

pub(crate) struct PreparedOfflineContent {
    pub(crate) charts: PreparedOfflineCharts,
    pub(crate) audio: Option<PreparedSongAudio>,
}

pub(crate) type OfflinePreparationCompletion = BackgroundCompletion<PreparedOfflineContent>;
pub(crate) type OfflinePreparationEvent =
    BackgroundEvent<OfflinePreparationIdentity, PreparedOfflineContent>;

pub(crate) struct OfflinePreparationTask {
    task: LatestBackgroundTask<OfflinePreparationIdentity, PreparedOfflineContent>,
}

impl Default for OfflinePreparationTask {
    fn default() -> Self {
        Self {
            task: LatestBackgroundTask::new("taiko-offline-preparation"),
        }
    }
}

impl OfflinePreparationTask {
    pub(crate) fn start(&mut self, request: OfflinePreparationRequest) -> Result<()> {
        let identity = request.identity;
        self.task
            .start(identity, move |cancelled| prepare(request, cancelled))
    }

    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self) -> Vec<OfflinePreparationEvent> {
        self.task.poll()
    }
}

pub(crate) fn event_is_current(
    current: Option<OfflinePreparationIdentity>,
    event: &OfflinePreparationEvent,
) -> bool {
    crate::latest_background::event_is_current(current, event)
}

fn prepare(
    request: OfflinePreparationRequest,
    cancelled: &AtomicBool,
) -> Result<Option<PreparedOfflineContent>> {
    let is_cancelled = || cancelled.load(Ordering::Acquire);
    if is_cancelled() {
        return Ok(None);
    }

    let importer = rhythm_importer_tja::TjaImporter;
    let charts = match request.identity.mode {
        OfflinePreparationMode::Single => {
            let Some(chart) = load_chart(
                &request,
                request.identity.course_indices[0],
                &importer,
                &is_cancelled,
            )?
            else {
                return Ok(None);
            };
            if is_cancelled() {
                return Ok(None);
            }
            PreparedOfflineCharts::Single(Box::new(chart))
        }
        OfflinePreparationMode::LocalTwoPlayer => {
            let Some(first) = load_chart(
                &request,
                request.identity.course_indices[0],
                &importer,
                &is_cancelled,
            )?
            else {
                return Ok(None);
            };
            if is_cancelled() {
                return Ok(None);
            }
            let Some(second) = load_chart(
                &request,
                request.identity.course_indices[1],
                &importer,
                &is_cancelled,
            )?
            else {
                return Ok(None);
            };
            if is_cancelled() {
                return Ok(None);
            }
            PreparedOfflineCharts::Local(Box::new([first, second]))
        }
    };

    let source = match request
        .backend
        .load_song_audio_cancellable(&request.song, &is_cancelled)
    {
        Ok(source) => source,
        Err(error) if is_resource_load_cancelled(&error) => return Ok(None),
        Err(error) => return Err(error.context("failed to load offline song audio")),
    };
    if is_cancelled() {
        return Ok(None);
    }
    let audio = match source {
        Some(source) => match prepare_song_audio_cancellable(source, &is_cancelled) {
            Ok(audio) => Some(audio),
            Err(error) if is_resource_load_cancelled(&error) => return Ok(None),
            Err(error) => {
                return Err(error.context("failed to decode offline song audio"));
            }
        },
        None => None,
    };
    if is_cancelled() {
        return Ok(None);
    }

    Ok(Some(PreparedOfflineContent { charts, audio }))
}

fn load_chart(
    request: &OfflinePreparationRequest,
    course_index: usize,
    importer: &rhythm_importer_tja::TjaImporter,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<Option<CanonicalChart>> {
    match request.backend.load_course_chart_cancellable(
        &request.song,
        course_index,
        importer,
        is_cancelled,
    ) {
        Ok(chart) => Ok(Some(chart)),
        Err(error) if is_resource_load_cancelled(&error) => Ok(None),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to load course {course_index} for {}",
                request.song.title
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_include_every_selection_dimension() {
        let first = OfflinePreparationIdentity::local(7, 3, [0, 1]);
        assert_ne!(first, OfflinePreparationIdentity::local(8, 3, [0, 1]));
        assert_ne!(first, OfflinePreparationIdentity::local(7, 4, [0, 1]));
        assert_ne!(first, OfflinePreparationIdentity::local(7, 3, [1, 0]));
        assert_ne!(first, OfflinePreparationIdentity::single(7, 3, 0));
    }

    #[test]
    fn stale_completion_cannot_be_applied_to_new_selection() {
        let current = OfflinePreparationIdentity::single(9, 2, 1);
        let stale = OfflinePreparationEvent {
            identity: OfflinePreparationIdentity::single(8, 2, 1),
            completion: OfflinePreparationCompletion::Cancelled,
        };
        assert!(!event_is_current(Some(current), &stale));
    }
}
