use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use reqwest::Url;

use crate::latest_background::{BackgroundCompletion, BackgroundEvent, LatestBackgroundTask};
use crate::online::EmbeddedServer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EmbeddedServerStartIdentity {
    pub(crate) generation: u64,
}

pub(crate) struct PreparedEmbeddedServer {
    pub(crate) server: EmbeddedServer,
    pub(crate) server_url: Url,
    pub(crate) display_name: String,
}

pub(crate) type EmbeddedServerStartCompletion = BackgroundCompletion<PreparedEmbeddedServer>;
pub(crate) type EmbeddedServerStartEvent =
    BackgroundEvent<EmbeddedServerStartIdentity, PreparedEmbeddedServer>;

pub(crate) struct EmbeddedServerStartTask {
    task: LatestBackgroundTask<EmbeddedServerStartIdentity, PreparedEmbeddedServer>,
}

impl Default for EmbeddedServerStartTask {
    fn default() -> Self {
        Self {
            task: LatestBackgroundTask::new("taiko-embedded-server-start"),
        }
    }
}

impl EmbeddedServerStartTask {
    pub(crate) fn start(
        &mut self,
        identity: EmbeddedServerStartIdentity,
        songdir: PathBuf,
        display_name: String,
    ) -> Result<()> {
        self.task.start(identity, move |cancelled| {
            prepare_embedded_server(songdir, display_name, cancelled)
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self) -> Vec<EmbeddedServerStartEvent> {
        self.task.poll()
    }
}

pub(crate) fn event_is_current(
    current: Option<EmbeddedServerStartIdentity>,
    event: &EmbeddedServerStartEvent,
) -> bool {
    crate::latest_background::event_is_current(current, event)
}

fn prepare_embedded_server(
    songdir: PathBuf,
    display_name: String,
    cancelled: &AtomicBool,
) -> Result<Option<PreparedEmbeddedServer>> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    let (server, server_url) = EmbeddedServer::start_local(songdir)?;
    if cancelled.load(Ordering::Acquire) {
        server.shutdown_and_join()?;
        return Ok(None);
    }
    Ok(Some(PreparedEmbeddedServer {
        server,
        server_url,
        display_name,
    }))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn starting_embedded_host_never_blocks_the_ui_thread() {
        let mut task = EmbeddedServerStartTask::default();
        let identity = EmbeddedServerStartIdentity { generation: 1 };
        let missing = std::env::temp_dir().join(format!(
            "taiko-missing-embedded-host-{}",
            std::process::id()
        ));

        let started = Instant::now();
        task.start(identity, missing, "host".to_owned())
            .expect("spawn embedded server preparation");
        assert!(started.elapsed() < Duration::from_millis(100));

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(event) = task.poll().into_iter().next() {
                assert!(event_is_current(Some(identity), &event));
                assert!(matches!(
                    event.completion,
                    EmbeddedServerStartCompletion::Failed(_)
                ));
                break;
            }
            assert!(
                Instant::now() < deadline,
                "startup failure was not reported"
            );
            std::thread::yield_now();
        }
    }
}
