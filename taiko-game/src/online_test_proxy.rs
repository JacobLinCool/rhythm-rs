use std::collections::VecDeque;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use taiko_multiplayer_protocol::{
    ActorId, ClientMessage, CommandOutcome, CommandSeq, InputOutcome, InputSeq, MatchId,
    ProtocolErrorCode, ServerMessage,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message as WsMessage;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_HEADER_LIMIT: usize = 16 * 1024;
const OLD_TRANSPORT_FENCE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultKind {
    Command,
    Input,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct AckLossEvidence {
    pub(crate) command_ack_dropped: bool,
    pub(crate) command_resume_observed: bool,
    pub(crate) command_old_transport_fenced: bool,
    pub(crate) input_ack_dropped: bool,
    pub(crate) input_resume_observed: bool,
    pub(crate) input_replay_observed: bool,
    pub(crate) input_replay_acknowledged: bool,
    pub(crate) input_old_transport_fenced: bool,
}

#[derive(Debug)]
struct CommandFault {
    actor_id: ActorId,
    seq: CommandSeq,
}

#[derive(Debug)]
struct InputFault {
    actor_id: ActorId,
    match_id: MatchId,
    seq: InputSeq,
}

#[derive(Debug, Default)]
struct ProxyState {
    evidence: AckLossEvidence,
    command: Option<CommandFault>,
    input_armed: bool,
    input: Option<InputFault>,
    pending_resumes: VecDeque<(FaultKind, ActorId)>,
    failure: Option<String>,
}

impl ProxyState {
    fn record_failure(&mut self, error: &anyhow::Error) {
        self.failure.get_or_insert_with(|| format!("{error:#}"));
    }

    fn observe_client_message(
        &mut self,
        actor_id: Option<&ActorId>,
        message: &ClientMessage,
    ) -> Result<()> {
        match message {
            ClientMessage::Hello(hello) => {
                let Some(resume) = &hello.resume else {
                    return Ok(());
                };
                let Some(index) = self
                    .pending_resumes
                    .iter()
                    .position(|(_, actor)| *actor == resume.actor_id)
                else {
                    return Ok(());
                };
                let (kind, _) = self
                    .pending_resumes
                    .remove(index)
                    .expect("resume index came from this queue");
                match kind {
                    FaultKind::Command => self.evidence.command_resume_observed = true,
                    FaultKind::Input => self.evidence.input_resume_observed = true,
                }
            }
            ClientMessage::Command(envelope)
                if matches!(
                    envelope.command,
                    taiko_multiplayer_protocol::ClientCommand::SelectSong { .. }
                ) =>
            {
                if self.command.is_none() {
                    let actor_id = actor_id
                        .cloned()
                        .context("SelectSong arrived before membership identity was observed")?;
                    self.command = Some(CommandFault {
                        actor_id,
                        seq: envelope.seq,
                    });
                }
            }
            ClientMessage::Input(batch) => {
                if self.input_armed && self.input.is_none() {
                    let actor_id = actor_id
                        .cloned()
                        .context("input arrived before membership identity was observed")?;
                    let event = batch
                        .events
                        .first()
                        .context("production client sent an empty input batch")?;
                    self.input = Some(InputFault {
                        actor_id,
                        match_id: batch.match_id,
                        seq: event.seq,
                    });
                    self.input_armed = false;
                } else if self.evidence.input_resume_observed
                    && self.input.as_ref().is_some_and(|target| {
                        target.match_id == batch.match_id
                            && batch.events.iter().any(|event| event.seq == target.seq)
                    })
                {
                    self.evidence.input_replay_observed = true;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn observe_server_message(
        &mut self,
        actor_id: &mut Option<ActorId>,
        message: &ServerMessage,
    ) -> Result<Option<FaultKind>> {
        match message {
            ServerMessage::MembershipGranted(granted) => {
                *actor_id = Some(granted.actor_id.clone());
            }
            ServerMessage::CommandAck(ack)
                if self
                    .command
                    .as_ref()
                    .is_some_and(|target| target.seq == ack.seq) =>
            {
                if !matches!(ack.outcome, CommandOutcome::Applied { .. }) {
                    bail!(
                        "target SelectSong command was not applied: {:?}",
                        ack.outcome
                    );
                }
                if !self.evidence.command_ack_dropped {
                    let target = self.command.as_ref().expect("command target was matched");
                    self.evidence.command_ack_dropped = true;
                    self.pending_resumes
                        .push_back((FaultKind::Command, target.actor_id.clone()));
                    return Ok(Some(FaultKind::Command));
                }
            }
            ServerMessage::InputAck(ack)
                if self.input.as_ref().is_some_and(|target| {
                    target.match_id == ack.match_id
                        && ack
                            .highest_contiguous_seq
                            .is_some_and(|highest| highest >= target.seq)
                }) =>
            {
                if !matches!(ack.outcome, InputOutcome::Accepted) {
                    bail!(
                        "target production input was not accepted: {:?}",
                        ack.outcome
                    );
                }
                if !self.evidence.input_ack_dropped {
                    let target = self.input.as_ref().expect("input target was matched");
                    self.evidence.input_ack_dropped = true;
                    self.pending_resumes
                        .push_back((FaultKind::Input, target.actor_id.clone()));
                    return Ok(Some(FaultKind::Input));
                }
                if self.evidence.input_replay_observed {
                    self.evidence.input_replay_acknowledged = true;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    fn mark_fenced(&mut self, kind: FaultKind) {
        match kind {
            FaultKind::Command => self.evidence.command_old_transport_fenced = true,
            FaultKind::Input => self.evidence.input_old_transport_fenced = true,
        }
    }
}

pub(crate) struct AckLossProxy {
    base_url: String,
    state: Arc<Mutex<ProxyState>>,
    shutdown: Option<oneshot::Sender<()>>,
    finished: std_mpsc::Receiver<std::result::Result<(), String>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl AckLossProxy {
    pub(crate) fn start(upstream_url: &str) -> Result<Self> {
        let upstream = parse_upstream(upstream_url)?;
        let state = Arc::new(Mutex::new(ProxyState::default()));
        let thread_state = Arc::clone(&state);
        let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
        let (finished_tx, finished_rx) = std_mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("taiko-ack-loss-proxy".to_owned())
            .spawn(move || {
                let result = (|| -> Result<()> {
                    let runtime = Builder::new_multi_thread()
                        .enable_all()
                        .build()
                        .context("failed to initialize ACK-loss proxy runtime")?;
                    runtime.block_on(run_proxy(upstream, thread_state, ready_tx, shutdown_rx))
                })();
                let _ = finished_tx.send(result.map_err(|error| format!("{error:#}")));
            })
            .context("failed to spawn ACK-loss proxy thread")?;

        let address = ready_rx
            .recv_timeout(STARTUP_TIMEOUT)
            .context("ACK-loss proxy startup timed out")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            base_url: format!("http://{address}/"),
            state,
            shutdown: Some(shutdown_tx),
            finished: finished_rx,
            thread: Some(thread),
        })
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(crate) fn arm_input_ack_loss(&self) -> Result<()> {
        let mut state = self.state.lock().expect("ACK-loss proxy mutex poisoned");
        if state.input_armed || state.input.is_some() {
            bail!("input ACK-loss fault was already armed or consumed");
        }
        state.input_armed = true;
        Ok(())
    }

    pub(crate) fn evidence(&self) -> Result<AckLossEvidence> {
        let state = self.state.lock().expect("ACK-loss proxy mutex poisoned");
        if let Some(failure) = &state.failure {
            bail!("ACK-loss proxy handler failed: {failure}");
        }
        Ok(state.evidence)
    }

    pub(crate) fn shutdown_and_wait(mut self) -> Result<()> {
        let shutdown = self
            .shutdown
            .take()
            .context("ACK-loss proxy was already shut down")?;
        let _ = shutdown.send(());
        let result = self
            .finished
            .recv_timeout(SHUTDOWN_TIMEOUT)
            .context("ACK-loss proxy shutdown timed out")?;
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("ACK-loss proxy thread panicked"))?;
        }
        result.map_err(anyhow::Error::msg)
    }
}

impl Drop for AckLossProxy {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

fn parse_upstream(upstream_url: &str) -> Result<SocketAddr> {
    let url = Url::parse(upstream_url).context("failed to parse ACK-loss proxy upstream URL")?;
    if url.scheme() != "http" {
        bail!("ACK-loss proxy requires a plain HTTP loopback upstream");
    }
    let host = url
        .host_str()
        .context("ACK-loss proxy upstream URL has no host")?;
    let port = url
        .port_or_known_default()
        .context("ACK-loss proxy upstream URL has no port")?;
    SocketAddr::from_str(&format!("{host}:{port}"))
        .context("ACK-loss proxy upstream must be a numeric loopback address")
}

async fn run_proxy(
    upstream: SocketAddr,
    state: Arc<Mutex<ProxyState>>,
    ready: std_mpsc::SyncSender<std::result::Result<SocketAddr, String>>,
    mut shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = ready.send(Err(format!("failed to bind ACK-loss proxy: {error}")));
            return Err(error).context("failed to bind ACK-loss proxy");
        }
    };
    let address = listener
        .local_addr()
        .context("failed to inspect ACK-loss proxy address")?;
    if ready.send(Ok(address)).is_err() {
        return Ok(());
    }

    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("ACK-loss proxy accept failed")?;
                let task_state = Arc::clone(&state);
                tasks.spawn(async move {
                    if let Err(error) = handle_connection(stream, upstream, Arc::clone(&task_state)).await {
                        task_state
                            .lock()
                            .expect("ACK-loss proxy mutex poisoned")
                            .record_failure(&error);
                    }
                });
            }
            completed = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Err(error)) = completed {
                    let error = anyhow!("ACK-loss proxy task panicked: {error}");
                    state
                        .lock()
                        .expect("ACK-loss proxy mutex poisoned")
                        .record_failure(&error);
                }
            }
        }
    }

    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                return Err(error).context("ACK-loss proxy task panicked during shutdown");
            }
        }
    }
    Ok(())
}

async fn handle_connection(
    mut downstream: TcpStream,
    upstream: SocketAddr,
    state: Arc<Mutex<ProxyState>>,
) -> Result<()> {
    if request_is_multiplayer_websocket(&downstream).await? {
        proxy_websocket(downstream, upstream, state).await
    } else {
        let mut upstream = TcpStream::connect(upstream)
            .await
            .context("ACK-loss HTTP tunnel failed to connect upstream")?;
        tokio::io::copy_bidirectional(&mut downstream, &mut upstream)
            .await
            .context("ACK-loss HTTP tunnel failed")?;
        Ok(())
    }
}

async fn request_is_multiplayer_websocket(stream: &TcpStream) -> Result<bool> {
    let mut buffer = vec![0_u8; REQUEST_HEADER_LIMIT];
    loop {
        let read = stream
            .peek(&mut buffer)
            .await
            .context("failed to inspect proxied request")?;
        if read == 0 {
            bail!("proxied client closed before sending a request");
        }
        if let Some(header_end) = buffer[..read]
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
        {
            let header = std::str::from_utf8(&buffer[..header_end + 4])
                .context("proxied request header was not UTF-8")?;
            let mut lines = header.split("\r\n");
            let request_line = lines
                .next()
                .context("proxied request had no request line")?;
            let path = request_line
                .split_ascii_whitespace()
                .nth(1)
                .context("proxied request line had no path")?;
            let upgrades_websocket = lines.any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("upgrade")
                        && value.trim().eq_ignore_ascii_case("websocket")
                })
            });
            return Ok(path == "/v2/multiplayer/ws" && upgrades_websocket);
        }
        if read == buffer.len() {
            bail!("proxied request headers exceed {REQUEST_HEADER_LIMIT} bytes");
        }
        tokio::task::yield_now().await;
    }
}

async fn proxy_websocket(
    downstream: TcpStream,
    upstream: SocketAddr,
    state: Arc<Mutex<ProxyState>>,
) -> Result<()> {
    let mut downstream = tokio_tungstenite::accept_async(downstream)
        .await
        .context("ACK-loss proxy downstream WebSocket handshake failed")?;
    let upstream_url = format!("ws://{upstream}/v2/multiplayer/ws");
    let (mut upstream, _) = tokio_tungstenite::connect_async(&upstream_url)
        .await
        .context("ACK-loss proxy upstream WebSocket handshake failed")?;
    let mut actor_id = None;

    loop {
        tokio::select! {
            message = downstream.next() => {
                let Some(message) = message else {
                    let _ = upstream.send(WsMessage::Close(None)).await;
                    return Ok(());
                };
                let message = message.context("ACK-loss proxy downstream WebSocket read failed")?;
                if let Some(protocol) = decode_client_message(&message)? {
                    state
                        .lock()
                        .expect("ACK-loss proxy mutex poisoned")
                        .observe_client_message(actor_id.as_ref(), &protocol)?;
                }
                let is_close = matches!(message, WsMessage::Close(_));
                upstream
                    .send(message)
                    .await
                    .context("ACK-loss proxy upstream WebSocket write failed")?;
                if is_close {
                    return Ok(());
                }
            }
            message = upstream.next() => {
                let Some(message) = message else {
                    let _ = downstream.send(WsMessage::Close(None)).await;
                    return Ok(());
                };
                let message = message.context("ACK-loss proxy upstream WebSocket read failed")?;
                let fault = if let Some(protocol) = decode_server_message(&message)? {
                    state
                        .lock()
                        .expect("ACK-loss proxy mutex poisoned")
                        .observe_server_message(&mut actor_id, &protocol)?
                } else {
                    None
                };
                if let Some(kind) = fault {
                    downstream
                        .send(WsMessage::Close(None))
                        .await
                        .context("ACK-loss proxy failed to close the faulted client transport")?;
                    wait_for_old_transport_fence(upstream, kind, state).await?;
                    return Ok(());
                }
                let is_close = matches!(message, WsMessage::Close(_));
                downstream
                    .send(message)
                    .await
                    .context("ACK-loss proxy downstream WebSocket write failed")?;
                if is_close {
                    return Ok(());
                }
            }
        }
    }
}

async fn wait_for_old_transport_fence(
    mut upstream: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
    kind: FaultKind,
    state: Arc<Mutex<ProxyState>>,
) -> Result<()> {
    tokio::time::timeout(OLD_TRANSPORT_FENCE_TIMEOUT, async {
        loop {
            let message = upstream
                .next()
                .await
                .context("old upstream transport closed before it was fenced")?
                .context("old upstream transport failed before it was fenced")?;
            match message {
                WsMessage::Ping(payload) => {
                    upstream
                        .send(WsMessage::Pong(payload))
                        .await
                        .context("failed to pong on the retained old transport")?;
                }
                WsMessage::Text(_) | WsMessage::Binary(_) => {
                    if let Some(ServerMessage::Fatal(error)) = decode_server_message(&message)? {
                        if error.code == ProtocolErrorCode::SessionSuperseded {
                            state
                                .lock()
                                .expect("ACK-loss proxy mutex poisoned")
                                .mark_fenced(kind);
                            return Ok(());
                        }
                    }
                }
                WsMessage::Pong(_) | WsMessage::Frame(_) => {}
                WsMessage::Close(frame) => {
                    bail!("old transport closed without SessionSuperseded: {frame:?}");
                }
            }
        }
    })
    .await
    .context("timed out waiting for SessionSuperseded on the old transport")?
}

fn decode_client_message(message: &WsMessage) -> Result<Option<ClientMessage>> {
    match message {
        WsMessage::Text(raw) => serde_json::from_str(raw)
            .map(Some)
            .context("ACK-loss proxy failed to decode a client protocol message"),
        WsMessage::Binary(raw) => serde_json::from_slice(raw)
            .map(Some)
            .context("ACK-loss proxy failed to decode a binary client protocol message"),
        _ => Ok(None),
    }
}

fn decode_server_message(message: &WsMessage) -> Result<Option<ServerMessage>> {
    match message {
        WsMessage::Text(raw) => serde_json::from_str(raw)
            .map(Some)
            .context("ACK-loss proxy failed to decode a server protocol message"),
        WsMessage::Binary(raw) => serde_json::from_slice(raw)
            .map(Some)
            .context("ACK-loss proxy failed to decode a binary server protocol message"),
        _ => Ok(None),
    }
}
