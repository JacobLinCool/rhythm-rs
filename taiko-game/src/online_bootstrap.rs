//! GUI online bootstrap, backed by the bounded latest-request worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::latest_background::{BackgroundCompletion, BackgroundEvent, LatestBackgroundTask};
use crate::loader::SongLibrary;
use crate::online::OnlineClientConfig;
use crate::resource::{is_resource_load_cancelled, ResourceBackend};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BootstrapIdentity {
    pub(crate) generation: u64,
}

pub(crate) struct PreparedBootstrap {
    pub(crate) config: OnlineClientConfig,
    pub(crate) resources: PreparedBootstrapResources,
}

pub(crate) enum PreparedBootstrapResources {
    Authority {
        backend: Arc<ResourceBackend>,
        library: SongLibrary,
    },
    Spectator,
}

pub(crate) type BootstrapCompletion = BackgroundCompletion<PreparedBootstrap>;
pub(crate) type BootstrapEvent = BackgroundEvent<BootstrapIdentity, PreparedBootstrap>;

pub(crate) struct OnlineBootstrapTask {
    task: LatestBackgroundTask<BootstrapIdentity, PreparedBootstrap>,
}

impl Default for OnlineBootstrapTask {
    fn default() -> Self {
        Self {
            task: LatestBackgroundTask::new("taiko-online-bootstrap"),
        }
    }
}

impl OnlineBootstrapTask {
    pub(crate) fn start(
        &mut self,
        identity: BootstrapIdentity,
        config: OnlineClientConfig,
        memory_only_cache: bool,
    ) -> Result<()> {
        self.task.start(identity, move |cancelled| {
            prepare_bootstrap(config, memory_only_cache, cancelled)
        })
    }

    pub(crate) fn cancel(&mut self) {
        self.task.cancel();
    }

    pub(crate) fn poll(&mut self) -> Vec<BootstrapEvent> {
        self.task.poll()
    }
}

pub(crate) fn event_is_current(current: Option<BootstrapIdentity>, event: &BootstrapEvent) -> bool {
    crate::latest_background::event_is_current(current, event)
}

fn prepare_bootstrap(
    config: OnlineClientConfig,
    memory_only_cache: bool,
    cancelled: &AtomicBool,
) -> Result<Option<PreparedBootstrap>> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    if !config.requires_authoritative_resources() {
        return Ok(Some(PreparedBootstrap {
            config,
            resources: PreparedBootstrapResources::Spectator,
        }));
    }

    let endpoint = crate::online::resource_http_endpoint(config.server_url.as_str())?;
    let backend = Arc::new(ResourceBackend::remote(&endpoint, memory_only_cache)?);
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }

    let library = match backend.load_song_library_cancellable(&|| cancelled.load(Ordering::Acquire))
    {
        Ok(library) => library,
        Err(error) if is_resource_load_cancelled(&error) => return Ok(None),
        Err(error) => {
            return Err(error).context("failed to load authoritative online song library");
        }
    };
    if cancelled.load(Ordering::Acquire) {
        return Ok(None);
    }
    validate_authoritative_library(&library)?;

    Ok(Some(PreparedBootstrap {
        config,
        resources: PreparedBootstrapResources::Authority { backend, library },
    }))
}

fn validate_authoritative_library(library: &SongLibrary) -> Result<()> {
    if library.songs.is_empty() {
        anyhow::bail!("the authoritative server has no playable songs");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn empty_authority_catalog_fails_closed() {
        let error = validate_authoritative_library(&SongLibrary {
            songs: Vec::new(),
            warnings: Vec::new(),
        })
        .expect_err("empty authority catalog must be rejected");
        assert_eq!(
            error.to_string(),
            "the authoritative server has no playable songs"
        );
    }

    #[test]
    fn spectator_bootstrap_never_touches_the_resource_endpoint() {
        let config = OnlineClientConfig::join(
            "http://127.0.0.1:1",
            "spectator",
            "ABCD",
            &"a".repeat(64),
            taiko_multiplayer_protocol::JoinRole::Spectator,
        )
        .expect("spectator config");
        let prepared = prepare_bootstrap(config, true, &AtomicBool::new(false))
            .expect("spectator bootstrap")
            .expect("not cancelled");

        assert!(matches!(
            prepared.resources,
            PreparedBootstrapResources::Spectator
        ));
    }

    fn read_request_head(stream: &mut TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set request timeout");
        let mut request = Vec::new();
        let mut tail = VecDeque::with_capacity(4);
        loop {
            let mut byte = [0_u8; 1];
            let read = stream.read(&mut byte).expect("read request");
            assert_ne!(read, 0, "request ended before its headers");
            request.push(byte[0]);
            if tail.len() == 4 {
                tail.pop_front();
            }
            tail.push_back(byte[0]);
            if tail.iter().copied().eq(*b"\r\n\r\n") {
                return request;
            }
        }
    }

    #[test]
    fn generation_cancellation_interrupts_library_bootstrap_and_closes_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind bootstrap HTTP fixture");
        let address = listener.local_addr().expect("bootstrap fixture address");
        let (headers_tx, headers_rx) = mpsc::sync_channel(1);
        let (closed_tx, closed_rx) = mpsc::sync_channel(1);
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept library request");
            let request = read_request_head(&mut stream);
            assert!(
                request.starts_with(b"GET /v1/library HTTP/1.1\r\n"),
                "bootstrap must request the library route"
            );
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: keep-alive\r\n\r\n",
                )
                .expect("write stalled library response");
            stream.flush().expect("flush response headers");
            headers_tx.send(()).expect("report response headers");

            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .expect("set close observation timeout");
            let mut byte = [0_u8; 1];
            let closed = match stream.read(&mut byte) {
                Ok(0) => true,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::BrokenPipe
                    ) =>
                {
                    true
                }
                _ => false,
            };
            closed_tx.send(closed).expect("report socket close");
        });

        let config = OnlineClientConfig::create(&format!("http://{address}"), "bootstrap-test")
            .expect("bootstrap config");
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = result_tx.send(prepare_bootstrap(config, true, &worker_cancelled));
        });
        headers_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("library response started");
        let cancellation_started = Instant::now();
        cancelled.store(true, Ordering::Release);

        match result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("bootstrap cancellation result")
        {
            Ok(None) => {}
            Ok(Some(_)) => panic!("cancelled bootstrap must not complete"),
            Err(error) => panic!("cancelled bootstrap returned an error: {error:#}"),
        }
        assert!(
            cancellation_started.elapsed() < Duration::from_millis(500),
            "bootstrap cancellation should be observed by the 25ms transport poll"
        );
        assert!(
            closed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("server socket observation"),
            "cancelled library response must close its socket"
        );

        worker.join().expect("bootstrap worker");
        server.join().expect("bootstrap fixture server");
    }
}
