use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::latest_background::{BackgroundCompletion, BackgroundEvent, LatestBackgroundTask};
use crate::loader::SongLibrary;
use crate::resource::{is_resource_load_cancelled, ResourceBackend};

pub(crate) type LibraryLoadCompletion = BackgroundCompletion<SongLibrary>;
pub(crate) type LibraryLoadEvent = BackgroundEvent<u64, SongLibrary>;

pub(crate) struct LibraryLoadTask {
    task: LatestBackgroundTask<u64, SongLibrary>,
}

impl Default for LibraryLoadTask {
    fn default() -> Self {
        Self {
            task: LatestBackgroundTask::new("taiko-song-library"),
        }
    }
}

impl LibraryLoadTask {
    pub(crate) fn start(&mut self, generation: u64, backend: Arc<ResourceBackend>) -> Result<()> {
        self.task.start(generation, move |cancelled| {
            let result =
                backend.load_song_library_cancellable(&|| cancelled.load(Ordering::Acquire));
            match result {
                Ok(library) => Ok(Some(library)),
                Err(error) if is_resource_load_cancelled(&error) => Ok(None),
                Err(error) => Err(error).context("failed to load song library"),
            }
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self) -> Vec<LibraryLoadEvent> {
        self.task.poll()
    }
}

pub(crate) fn event_is_current(generation: Option<u64>, event: &LibraryLoadEvent) -> bool {
    crate::latest_background::event_is_current(generation, event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_fences_stale_library_results() {
        let stale = LibraryLoadEvent {
            identity: 2,
            completion: LibraryLoadCompletion::Cancelled,
        };
        assert!(!event_is_current(Some(3), &stale));
        assert!(event_is_current(Some(2), &stale));
    }
}
