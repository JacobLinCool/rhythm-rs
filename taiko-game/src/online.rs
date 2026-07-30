#[cfg(test)]
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
#[cfg(test)]
use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use rhythm_core::{Tick, TimedInput};
use rhythm_mode_taiko::{TaikoAction, TaikoMode, TaikoRuntime, TaikoSide, TaikoZone};
use taiko_multiplayer_protocol::{
    ClientBuild, ClientHello, ClientMessage, ContentHash, DisplayName, DrumAction, DrumSide,
    DrumZone, InvitationToken, JoinRole, LiveStateSnapshot, ResumeRequest, RoomCode, ServerMessage,
    MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, WIRE_SCHEMA_SHA256,
};
#[cfg(test)]
use taiko_multiplayer_protocol::{
    CourseId, FinalResult, MatchId, PlayerId, PlayerLiveState, PlayerSelection, RoomRole,
    RoomSnapshot, StateSeq,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::sync::watch;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::invite::MultiplayerInvite;
#[cfg(test)]
use crate::loader::SongEntry;
#[cfg(test)]
use crate::online_preparation::{
    validate_authoritative_song_identity, OnlinePreparationTask, PreparationCompletion,
    PreparationEvent, PreparationIdentity, PreparationRequest,
};
#[cfg(test)]
use crate::resource::ResourceBackend;

const OUTBOUND_CAPACITY: usize = 256;
const RELIABLE_INBOUND_CAPACITY: usize = 256;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const GRACEFUL_FLUSH_TIMEOUT: Duration = Duration::from_secs(10);
const GRACEFUL_LEAVE_RETRY_INTERVAL: Duration = Duration::from_millis(500);
const GRACEFUL_COMPLETION_WAIT: Duration = Duration::from_secs(16);
const MIN_SERVER_SILENCE_TIMEOUT: Duration = Duration::from_secs(5);
const STABLE_CONNECTION_WINDOW: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoomIntent {
    Create,
    Join {
        room_code: RoomCode,
        invitation_token: InvitationToken,
        role: JoinRole,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct ReconnectPolicy {
    pub(crate) initial_delay: Duration,
    pub(crate) maximum_delay: Duration,
    pub(crate) maximum_attempts: u32,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(250),
            maximum_delay: Duration::from_secs(5),
            maximum_attempts: 32,
        }
    }
}

impl ReconnectPolicy {
    pub(crate) fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt <= 1 {
            return Duration::ZERO;
        }
        let exponent = attempt.saturating_sub(2).min(31);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        self.initial_delay
            .saturating_mul(multiplier)
            .min(self.maximum_delay)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct OnlineClientConfig {
    pub(crate) server_url: Url,
    pub(crate) display_name: DisplayName,
    pub(crate) client_build: ClientBuild,
    pub(crate) room_intent: RoomIntent,
    pub(crate) reconnect: ReconnectPolicy,
}

impl OnlineClientConfig {
    pub(crate) fn create(server_url: &str, display_name: &str) -> Result<Self> {
        Self::new(server_url, display_name, RoomIntent::Create)
    }

    pub(crate) fn join(
        server_url: &str,
        display_name: &str,
        room_code: &str,
        invitation_token: &str,
        role: JoinRole,
    ) -> Result<Self> {
        Self::new(
            server_url,
            display_name,
            RoomIntent::Join {
                room_code: RoomCode::parse(room_code).context("invalid room code")?,
                invitation_token: InvitationToken::parse(invitation_token)
                    .context("invalid invitation token")?,
                role,
            },
        )
    }

    fn new(server_url: &str, display_name: &str, room_intent: RoomIntent) -> Result<Self> {
        let server_url = MultiplayerInvite::normalize_server(server_url)?;
        multiplayer_ws_url(server_url.as_str())?;
        Ok(Self {
            server_url,
            display_name: DisplayName::new(display_name).context("invalid display name")?,
            client_build: ClientBuild::new(format!("taiko-game/{}", env!("CARGO_PKG_VERSION")))
                .expect("package version is a valid client build"),
            room_intent,
            reconnect: ReconnectPolicy::default(),
        })
    }

    pub(crate) fn requires_authoritative_resources(&self) -> bool {
        !matches!(
            &self.room_intent,
            RoomIntent::Join {
                role: JoinRole::Spectator,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttemptPlan {
    ordinal: u32,
    delay: Duration,
}

struct ReconnectBudget<'a> {
    policy: &'a ReconnectPolicy,
    attempts_started: u32,
    backoff_attempt: u32,
}

impl<'a> ReconnectBudget<'a> {
    fn new(policy: &'a ReconnectPolicy) -> Self {
        Self {
            policy,
            attempts_started: 0,
            backoff_attempt: 1,
        }
    }

    fn begin_attempt(&mut self) -> Option<AttemptPlan> {
        if self.attempts_started >= self.policy.maximum_attempts {
            return None;
        }
        self.attempts_started += 1;
        Some(AttemptPlan {
            ordinal: self.attempts_started,
            delay: self.policy.delay_for_attempt(self.backoff_attempt),
        })
    }

    fn record_failure(&mut self, stable_connection: bool) -> Option<AttemptPlan> {
        if stable_connection {
            self.attempts_started = 0;
            self.backoff_attempt = 1;
        }
        if self.attempts_started >= self.policy.maximum_attempts {
            return None;
        }
        if !stable_connection {
            self.backoff_attempt = self.backoff_attempt.saturating_add(1);
        }
        Some(AttemptPlan {
            ordinal: self.attempts_started + 1,
            delay: self.policy.delay_for_attempt(self.backoff_attempt),
        })
    }

    fn attempts_started(&self) -> u32 {
        self.attempts_started
    }
}

struct ConnectionHealthTracker {
    affiliated_at: Option<tokio::time::Instant>,
    stable: bool,
}

impl ConnectionHealthTracker {
    fn new() -> Self {
        Self {
            affiliated_at: None,
            stable: false,
        }
    }

    fn observe(&mut self, message: &ServerMessage, observed_at: tokio::time::Instant) {
        match message {
            ServerMessage::MembershipGranted(_) => {
                self.affiliated_at.get_or_insert(observed_at);
            }
            ServerMessage::HeartbeatAck(_)
                if self.affiliated_at.is_some_and(|affiliated_at| {
                    observed_at
                        .checked_duration_since(affiliated_at)
                        .is_some_and(|duration| duration >= STABLE_CONNECTION_WINDOW)
                }) =>
            {
                self.stable = true;
            }
            _ => {}
        }
    }

    fn is_stable(&self) -> bool {
        self.stable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransportFault {
    pub(crate) message: String,
    pub(crate) terminal: bool,
}

#[derive(Debug)]
pub(crate) enum NetworkEvent {
    Connecting {
        attempt: u32,
        delay: Duration,
    },
    Server(Box<ServerMessage>),
    Disconnected {
        reason: String,
        next_attempt: u32,
        retry_in: Duration,
    },
}

enum TransportCommand {
    Message(Box<ClientMessage>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GracefulShutdownError {
    AlreadyStopped,
    ControlChannelClosed,
    NotConnected { phase: &'static str },
    MissingLeave,
    LeaveNotLast,
    MessageWrite { index: usize, reason: String },
    ControlWrite { reason: String },
    LeaveRejected { reason: String },
    PeerClosedBeforeLeaveAck,
    PeerProtocol { reason: String },
    CloseWrite { reason: String },
    FlushTimedOut,
    CompletionChannelClosed,
    CompletionWaitTimedOut,
}

impl std::fmt::Display for GracefulShutdownError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyStopped => formatter.write_str("online transport is already stopped"),
            Self::ControlChannelClosed => {
                formatter.write_str("online transport control channel is closed")
            }
            Self::NotConnected { phase } => {
                write!(
                    formatter,
                    "cannot leave gracefully while transport is {phase}"
                )
            }
            Self::MissingLeave => {
                formatter.write_str("graceful shutdown batch is missing LeaveRoom")
            }
            Self::LeaveNotLast => {
                formatter.write_str("LeaveRoom must be the final graceful shutdown command")
            }
            Self::MessageWrite { index, reason } => {
                write!(
                    formatter,
                    "failed to write graceful message {}: {reason}",
                    index + 1
                )
            }
            Self::ControlWrite { reason } => {
                write!(
                    formatter,
                    "failed to write graceful control frame: {reason}"
                )
            }
            Self::LeaveRejected { reason } => {
                write!(formatter, "server rejected LeaveRoom: {reason}")
            }
            Self::PeerClosedBeforeLeaveAck => {
                formatter.write_str("server closed before acknowledging LeaveRoom")
            }
            Self::PeerProtocol { reason } => {
                write!(
                    formatter,
                    "invalid server response during graceful shutdown: {reason}"
                )
            }
            Self::CloseWrite { reason } => {
                write!(formatter, "failed to flush websocket Close frame: {reason}")
            }
            Self::FlushTimedOut => formatter.write_str("graceful websocket flush timed out"),
            Self::CompletionChannelClosed => {
                formatter.write_str("transport stopped without graceful completion")
            }
            Self::CompletionWaitTimedOut => {
                formatter.write_str("timed out waiting for graceful transport completion")
            }
        }
    }
}

impl std::error::Error for GracefulShutdownError {}

type GracefulShutdownResult = std::result::Result<(), GracefulShutdownError>;

#[derive(Clone)]
struct GracefulShutdownRequest {
    messages: Arc<[ClientMessage]>,
    completion: SyncSender<GracefulShutdownResult>,
}

impl GracefulShutdownRequest {
    fn complete(&self, result: GracefulShutdownResult) {
        let _ = self.completion.try_send(result);
    }
}

#[derive(Clone)]
enum TransportControl {
    Running,
    ImmediateShutdown,
    GracefulShutdown(GracefulShutdownRequest),
}

pub(crate) struct NetworkClient {
    outbound: tokio_mpsc::Sender<TransportCommand>,
    reliable_inbound: Receiver<NetworkEvent>,
    latest_live: Arc<Mutex<Option<LiveStateSnapshot>>>,
    resume: Arc<Mutex<Option<ResumeRequest>>>,
    fault: Arc<Mutex<Option<TransportFault>>>,
    control: watch::Sender<TransportControl>,
    stopped: Arc<AtomicBool>,
    #[cfg(test)]
    test_graceful_completion: Option<GracefulShutdownResult>,
}

impl NetworkClient {
    pub(crate) fn connect(config: &OnlineClientConfig) -> Result<Self> {
        let ws_url = multiplayer_ws_url(config.server_url.as_str())?;
        let schema_hash =
            ContentHash::parse(WIRE_SCHEMA_SHA256).expect("wire schema constant is valid");
        let (outbound_tx, outbound_rx) = tokio_mpsc::channel(OUTBOUND_CAPACITY);
        let (control_tx, control_rx) = watch::channel(TransportControl::Running);
        let (event_tx, event_rx) = mpsc::sync_channel(RELIABLE_INBOUND_CAPACITY);
        let latest_live = Arc::new(Mutex::new(None));
        let resume = Arc::new(Mutex::new(None));
        let fault = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));

        let thread_live = Arc::clone(&latest_live);
        let thread_resume = Arc::clone(&resume);
        let thread_fault = Arc::clone(&fault);
        let display_name = config.display_name.clone();
        let client_build = config.client_build.clone();
        let reconnect = config.reconnect.clone();

        thread::Builder::new()
            .name("taiko-online-transport".to_owned())
            .spawn(move || {
                let runtime = match Builder::new_current_thread().enable_all().build() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        set_transport_fault(
                            &thread_fault,
                            format!("failed to initialize network runtime: {error}"),
                            true,
                        );
                        return;
                    }
                };

                runtime.block_on(run_transport_supervisor(
                    ws_url,
                    display_name,
                    client_build,
                    schema_hash,
                    reconnect,
                    outbound_rx,
                    control_rx,
                    event_tx,
                    thread_live,
                    thread_resume,
                    thread_fault,
                ));
            })
            .context("failed to spawn online transport thread")?;

        Ok(Self {
            outbound: outbound_tx,
            reliable_inbound: event_rx,
            latest_live,
            resume,
            fault,
            control: control_tx,
            stopped,
            #[cfg(test)]
            test_graceful_completion: None,
        })
    }

    pub(crate) fn try_send(&self, message: ClientMessage) -> Result<()> {
        self.outbound
            .try_send(TransportCommand::Message(Box::new(message)))
            .map_err(|error| match error {
                tokio_mpsc::error::TrySendError::Full(_) => {
                    anyhow!("online writer queue is full")
                }
                tokio_mpsc::error::TrySendError::Closed(_) => {
                    anyhow!("online transport is closed")
                }
            })
    }

    pub(crate) fn try_recv(&self) -> Result<Option<NetworkEvent>> {
        if let Some(fault) = self
            .fault
            .lock()
            .expect("transport fault mutex poisoned")
            .take()
        {
            if fault.terminal {
                bail!(fault.message);
            }
            return Ok(Some(NetworkEvent::Disconnected {
                reason: fault.message,
                next_attempt: 0,
                retry_in: Duration::ZERO,
            }));
        }

        match self.reliable_inbound.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                bail!("online transport event channel disconnected")
            }
        }
    }

    pub(crate) fn take_latest_live(&self) -> Option<LiveStateSnapshot> {
        self.latest_live
            .lock()
            .expect("latest live-state mutex poisoned")
            .take()
    }

    pub(crate) fn set_resume(&self, resume: ResumeRequest) {
        *self.resume.lock().expect("resume mutex poisoned") = Some(resume);
    }

    pub(crate) fn shutdown(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            let _ = self.control.send(TransportControl::ImmediateShutdown);
        }
    }

    pub(crate) fn shutdown_gracefully(&self, messages: Vec<ClientMessage>) -> Result<()> {
        validate_graceful_messages(&messages)?;
        if self.stopped.swap(true, Ordering::AcqRel) {
            return Err(GracefulShutdownError::AlreadyStopped.into());
        }
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let request = GracefulShutdownRequest {
            messages: messages.into(),
            completion: completion_tx,
        };
        self.control
            .send(TransportControl::GracefulShutdown(request.clone()))
            .map_err(|_| GracefulShutdownError::ControlChannelClosed)?;

        #[cfg(test)]
        if let Some(result) = self.test_graceful_completion.clone() {
            request.complete(result);
        }

        match completion_rx.recv_timeout(GRACEFUL_COMPLETION_WAIT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(error.into()),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(GracefulShutdownError::CompletionChannelClosed.into())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.control.send(TransportControl::ImmediateShutdown);
                Err(GracefulShutdownError::CompletionWaitTimedOut.into())
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn test_pair() -> (Self, TestNetworkPeer) {
        let (outbound_tx, outbound_rx) = tokio_mpsc::channel(OUTBOUND_CAPACITY);
        let (control_tx, control_rx) = watch::channel(TransportControl::Running);
        let (event_tx, event_rx) = mpsc::sync_channel(RELIABLE_INBOUND_CAPACITY);
        let latest_live = Arc::new(Mutex::new(None));
        let resume = Arc::new(Mutex::new(None));
        let fault = Arc::new(Mutex::new(None));
        let stopped = Arc::new(AtomicBool::new(false));
        (
            Self {
                outbound: outbound_tx,
                reliable_inbound: event_rx,
                latest_live: Arc::clone(&latest_live),
                resume: Arc::clone(&resume),
                fault,
                control: control_tx,
                stopped,
                test_graceful_completion: Some(Ok(())),
            },
            TestNetworkPeer {
                outbound: outbound_rx,
                events: event_tx,
                latest_live,
                resume,
                control: control_rx,
            },
        )
    }
}

impl Drop for NetworkClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
pub(crate) struct TestNetworkPeer {
    outbound: tokio_mpsc::Receiver<TransportCommand>,
    events: SyncSender<NetworkEvent>,
    latest_live: Arc<Mutex<Option<LiveStateSnapshot>>>,
    resume: Arc<Mutex<Option<ResumeRequest>>>,
    control: watch::Receiver<TransportControl>,
}

#[cfg(test)]
impl TestNetworkPeer {
    pub(crate) fn send_event(&self, event: NetworkEvent) {
        self.events
            .try_send(event)
            .expect("test event queue has room");
    }

    pub(crate) fn send_server(&self, message: ServerMessage) {
        self.send_event(NetworkEvent::Server(Box::new(message)));
    }

    pub(crate) fn set_live(&self, live: LiveStateSnapshot) {
        *self.latest_live.lock().expect("test live mutex poisoned") = Some(live);
    }

    pub(crate) fn try_recv_message(&mut self) -> Option<ClientMessage> {
        self.outbound
            .try_recv()
            .ok()
            .map(|TransportCommand::Message(message)| *message)
    }

    pub(crate) fn resume_request(&self) -> Option<ResumeRequest> {
        self.resume
            .lock()
            .expect("test resume mutex poisoned")
            .clone()
    }

    pub(crate) fn take_graceful_shutdown_messages(&mut self) -> Option<Vec<ClientMessage>> {
        self.control.has_changed().ok()?;
        match self.control.borrow_and_update().clone() {
            TransportControl::GracefulShutdown(request) => Some(request.messages.to_vec()),
            TransportControl::Running | TransportControl::ImmediateShutdown => None,
        }
    }
}

fn validate_graceful_messages(
    messages: &[ClientMessage],
) -> std::result::Result<taiko_multiplayer_protocol::CommandSeq, GracefulShutdownError> {
    let leave_positions = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| {
            matches!(
                message,
                ClientMessage::Command(taiko_multiplayer_protocol::CommandEnvelope {
                    command: taiko_multiplayer_protocol::ClientCommand::LeaveRoom,
                    ..
                })
            )
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let Some(&leave_index) = leave_positions.last() else {
        return Err(GracefulShutdownError::MissingLeave);
    };
    if leave_positions.len() != 1 || leave_index + 1 != messages.len() {
        return Err(GracefulShutdownError::LeaveNotLast);
    }
    let ClientMessage::Command(envelope) = &messages[leave_index] else {
        unreachable!("leave position only matches command envelopes");
    };
    Ok(envelope.seq)
}

#[allow(clippy::too_many_arguments)]
async fn run_transport_supervisor(
    ws_url: Url,
    display_name: DisplayName,
    client_build: ClientBuild,
    schema_hash: ContentHash,
    reconnect: ReconnectPolicy,
    mut outbound_rx: tokio_mpsc::Receiver<TransportCommand>,
    mut control_rx: watch::Receiver<TransportControl>,
    event_tx: SyncSender<NetworkEvent>,
    latest_live: Arc<Mutex<Option<LiveStateSnapshot>>>,
    resume: Arc<Mutex<Option<ResumeRequest>>>,
    fault: Arc<Mutex<Option<TransportFault>>>,
) {
    let mut budget = ReconnectBudget::new(&reconnect);
    loop {
        if complete_disconnected_control(&control_rx, "not connected") {
            return;
        }
        let Some(attempt) = budget.begin_attempt() else {
            set_transport_fault(
                &fault,
                format!(
                    "reconnect attempt budget exhausted after {} attempts",
                    budget.attempts_started()
                ),
                true,
            );
            return;
        };
        if emit_reliable(
            &event_tx,
            NetworkEvent::Connecting {
                attempt: attempt.ordinal,
                delay: attempt.delay,
            },
            &fault,
        )
        .is_err()
        {
            return;
        }

        if !attempt.delay.is_zero() {
            let sleep = tokio::time::sleep(attempt.delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    _ = &mut sleep => break,
                    changed = control_rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        if complete_disconnected_control(&control_rx, "waiting to reconnect") {
                            return;
                        }
                    }
                    command = outbound_rx.recv() => match command {
                        None => return,
                        Some(TransportCommand::Message(_)) => {
                            // Reliable messages live in OnlineDomain and are replayed
                            // after Welcome. Never retain stale heartbeats/time-sync here.
                        }
                    }
                }
            }
        }

        while outbound_rx.try_recv().is_ok() {}

        let hello = ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: schema_hash.clone(),
            client_build: client_build.clone(),
            display_name: display_name.clone(),
            resume: resume.lock().expect("resume mutex poisoned").clone(),
        });

        match run_one_connection(
            &ws_url,
            hello,
            &schema_hash,
            &mut outbound_rx,
            &mut control_rx,
            &event_tx,
            &latest_live,
            &fault,
        )
        .await
        {
            ConnectionExit::Shutdown => return,
            ConnectionExit::Terminal(message) => {
                set_transport_fault(&fault, message, true);
                return;
            }
            ConnectionExit::Retry {
                reason,
                stable_connection,
            } => {
                let Some(next) = budget.record_failure(stable_connection) else {
                    if complete_disconnected_control(&control_rx, "connection lost") {
                        return;
                    }
                    set_transport_fault(
                        &fault,
                        format!(
                            "reconnect attempt budget exhausted after {} attempts: {reason}",
                            budget.attempts_started()
                        ),
                        true,
                    );
                    return;
                };
                if emit_reliable(
                    &event_tx,
                    NetworkEvent::Disconnected {
                        reason,
                        next_attempt: next.ordinal,
                        retry_in: next.delay,
                    },
                    &fault,
                )
                .is_err()
                {
                    return;
                }
            }
        }
    }
}

enum ConnectionExit {
    Shutdown,
    Retry {
        reason: String,
        stable_connection: bool,
    },
    Terminal(String),
}

fn complete_disconnected_control(
    control_rx: &watch::Receiver<TransportControl>,
    phase: &'static str,
) -> bool {
    match control_rx.borrow().clone() {
        TransportControl::Running => false,
        TransportControl::ImmediateShutdown => true,
        TransportControl::GracefulShutdown(request) => {
            request.complete(Err(GracefulShutdownError::NotConnected { phase }));
            true
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_one_connection(
    ws_url: &Url,
    hello: ClientMessage,
    expected_schema_hash: &ContentHash,
    outbound_rx: &mut tokio_mpsc::Receiver<TransportCommand>,
    control_rx: &mut watch::Receiver<TransportControl>,
    event_tx: &SyncSender<NetworkEvent>,
    latest_live: &Arc<Mutex<Option<LiveStateSnapshot>>>,
    fault: &Arc<Mutex<Option<TransportFault>>>,
) -> ConnectionExit {
    let ws_config = WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(16 * 1024)
        .max_write_buffer_size(MAX_WIRE_MESSAGE_BYTES.saturating_add(16 * 1024))
        .max_message_size(Some(MAX_WIRE_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_WIRE_MESSAGE_BYTES));
    let connection = tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_async_with_config(ws_url.as_str(), Some(ws_config), true),
    );
    tokio::pin!(connection);
    let connection = loop {
        tokio::select! {
            result = &mut connection => break result,
            changed = control_rx.changed() => {
                if changed.is_err() {
                    return ConnectionExit::Shutdown;
                }
                if complete_disconnected_control(control_rx, "connecting") {
                    return ConnectionExit::Shutdown;
                }
            }
        }
    };
    let (stream, _) = match connection {
        Ok(Ok(connection)) => connection,
        Ok(Err(error)) => {
            return ConnectionExit::Retry {
                reason: format!("connection to {ws_url} failed: {error}"),
                stable_connection: false,
            };
        }
        Err(_) => {
            return ConnectionExit::Retry {
                reason: format!("connection to {ws_url} timed out"),
                stable_connection: false,
            };
        }
    };
    let (mut write, mut read) = stream.split();

    if let Err(error) = send_wire_message(&mut write, &hello).await {
        return ConnectionExit::Retry {
            reason: error,
            stable_connection: false,
        };
    }

    let handshake_deadline = tokio::time::Instant::now() + HANDSHAKE_TIMEOUT;
    let first = loop {
        let incoming = tokio::select! {
            result = tokio::time::timeout_at(handshake_deadline, read.next()) => result,
            changed = control_rx.changed() => {
                if changed.is_err() {
                    return ConnectionExit::Shutdown;
                }
                let control = control_rx.borrow().clone();
                match control {
                    TransportControl::Running => continue,
                    TransportControl::ImmediateShutdown => {
                        let _ = send_ws_frame(&mut write, WsMessage::Close(None)).await;
                        return ConnectionExit::Shutdown;
                    }
                    TransportControl::GracefulShutdown(request) => {
                        let _ = send_ws_frame(&mut write, WsMessage::Close(None)).await;
                        request.complete(Err(GracefulShutdownError::NotConnected {
                            phase: "performing the protocol handshake",
                        }));
                        return ConnectionExit::Shutdown;
                    }
                }
            }
        };
        let message = match incoming {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => {
                return ConnectionExit::Retry {
                    reason: format!("handshake read failed: {error}"),
                    stable_connection: false,
                };
            }
            Ok(None) => {
                return ConnectionExit::Retry {
                    reason: "server closed during handshake".to_owned(),
                    stable_connection: false,
                };
            }
            Err(_) => {
                return ConnectionExit::Retry {
                    reason: "server handshake timed out".to_owned(),
                    stable_connection: false,
                };
            }
        };
        match message {
            WsMessage::Ping(payload) => {
                if let Err(error) = send_ws_frame(&mut write, WsMessage::Pong(payload)).await {
                    return ConnectionExit::Retry {
                        reason: format!("failed to send handshake pong: {error}"),
                        stable_connection: false,
                    };
                }
            }
            WsMessage::Pong(_) => {}
            WsMessage::Close(frame) => {
                return ConnectionExit::Retry {
                    reason: frame
                        .map(|frame| frame.reason.to_string())
                        .unwrap_or_else(|| "server closed during handshake".to_owned()),
                    stable_connection: false,
                };
            }
            other => break other,
        }
    };
    let first = match decode_server_message(first) {
        Ok(message) => message,
        Err(error) => return ConnectionExit::Terminal(error),
    };
    let server_silence_timeout = match &first {
        ServerMessage::Welcome(welcome)
            if welcome.protocol_version == PROTOCOL_VERSION
                && &welcome.wire_schema_sha256 == expected_schema_hash =>
        {
            Duration::from_millis(u64::from(welcome.heartbeat_interval_ms).saturating_mul(3))
                .max(MIN_SERVER_SILENCE_TIMEOUT)
        }
        ServerMessage::Welcome(welcome) => {
            return ConnectionExit::Terminal(format!(
                "server protocol schema mismatch: version={} schema={}",
                welcome.protocol_version, welcome.wire_schema_sha256
            ));
        }
        ServerMessage::Fatal(error) if error.retryable => {
            return ConnectionExit::Retry {
                reason: protocol_error_reason(error),
                stable_connection: false,
            };
        }
        ServerMessage::Fatal(_) => {
            if emit_server_message(first.clone(), event_tx, latest_live, fault).is_err() {
                return ConnectionExit::Terminal("local reliable reader is too slow".to_owned());
            }
            return ConnectionExit::Shutdown;
        }
        _ => {
            return ConnectionExit::Terminal(
                "protocol violation: first server message was not Welcome".to_owned(),
            );
        }
    };
    let mut connection_health = ConnectionHealthTracker::new();
    if emit_server_message(first, event_tx, latest_live, fault).is_err() {
        return ConnectionExit::Terminal("local reliable reader is too slow".to_owned());
    }

    let server_silence = tokio::time::sleep(server_silence_timeout);
    tokio::pin!(server_silence);
    loop {
        tokio::select! {
            biased;
            changed = control_rx.changed() => {
                if changed.is_err() {
                    return ConnectionExit::Shutdown;
                }
                let control = control_rx.borrow().clone();
                match control {
                    TransportControl::Running => {}
                    TransportControl::ImmediateShutdown => {
                        let _ = send_ws_frame(&mut write, WsMessage::Close(None)).await;
                        return ConnectionExit::Shutdown;
                    }
                    TransportControl::GracefulShutdown(request) => {
                        let result = flush_graceful_shutdown(
                            &mut write,
                            &mut read,
                            &request.messages,
                        )
                        .await;
                        request.complete(result);
                        return ConnectionExit::Shutdown;
                    }
                }
            }
            _ = &mut server_silence => {
                return ConnectionExit::Retry {
                    reason: "server stopped acknowledging the connection".to_owned(),
                    stable_connection: connection_health.is_stable(),
                };
            }
            outgoing = outbound_rx.recv() => {
                match outgoing {
                    Some(TransportCommand::Message(message)) => {
                        if let Err(error) = send_wire_message(&mut write, &message).await {
                            return ConnectionExit::Retry {
                                reason: error,
                                stable_connection: connection_health.is_stable(),
                            };
                        }
                    }
                    None => {
                        let _ = send_ws_frame(&mut write, WsMessage::Close(None)).await;
                        return ConnectionExit::Shutdown;
                    }
                }
            }
            incoming = read.next() => {
                let message = match incoming {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => {
                        return ConnectionExit::Retry {
                            reason: format!("websocket read failed: {error}"),
                            stable_connection: connection_health.is_stable(),
                        };
                    }
                    None => {
                        return ConnectionExit::Retry {
                            reason: "server closed the websocket".to_owned(),
                            stable_connection: connection_health.is_stable(),
                        };
                    }
                };
                server_silence
                    .as_mut()
                    .reset(tokio::time::Instant::now() + server_silence_timeout);
                match message {
                    WsMessage::Ping(payload) => {
                        if let Err(error) = send_ws_frame(&mut write, WsMessage::Pong(payload)).await {
                            return ConnectionExit::Retry {
                                reason: format!("failed to send websocket pong: {error}"),
                                stable_connection: connection_health.is_stable(),
                            };
                        }
                    }
                    WsMessage::Pong(_) => {}
                    WsMessage::Close(frame) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .unwrap_or_else(|| "server closed the websocket".to_owned());
                        return ConnectionExit::Retry {
                            reason,
                            stable_connection: connection_health.is_stable(),
                        };
                    }
                    other => {
                        let server_message = match decode_server_message(other) {
                            Ok(message) => message,
                            Err(error) => return ConnectionExit::Terminal(error),
                        };
                        connection_health.observe(
                            &server_message,
                            tokio::time::Instant::now(),
                        );
                        match server_message {
                            ServerMessage::Fatal(error) if error.retryable => {
                                return ConnectionExit::Retry {
                                    reason: protocol_error_reason(&error),
                                    stable_connection: connection_health.is_stable(),
                                };
                            }
                            fatal @ ServerMessage::Fatal(_) => {
                                if emit_server_message(fatal, event_tx, latest_live, fault).is_err() {
                                    return ConnectionExit::Terminal(
                                        "local reliable reader is too slow".to_owned(),
                                    );
                                }
                                return ConnectionExit::Shutdown;
                            }
                            message => {
                                if emit_server_message(message, event_tx, latest_live, fault).is_err() {
                                    return ConnectionExit::Terminal(
                                        "local reliable reader is too slow".to_owned(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn flush_graceful_shutdown<S, R, ReadError>(
    write: &mut S,
    read: &mut R,
    messages: &[ClientMessage],
) -> GracefulShutdownResult
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
    R: futures_util::Stream<Item = std::result::Result<WsMessage, ReadError>> + Unpin,
    ReadError: std::fmt::Display,
{
    let leave_seq = validate_graceful_messages(messages)?;
    let leave_index = messages
        .len()
        .checked_sub(1)
        .ok_or(GracefulShutdownError::MissingLeave)?;
    let leave_message = &messages[leave_index];
    tokio::time::timeout(GRACEFUL_FLUSH_TIMEOUT, async {
        for (index, message) in messages.iter().enumerate() {
            send_wire_message(write, message)
                .await
                .map_err(|reason| GracefulShutdownError::MessageWrite { index, reason })?;
        }

        let mut leave_retry = tokio::time::interval_at(
            tokio::time::Instant::now() + GRACEFUL_LEAVE_RETRY_INTERVAL,
            GRACEFUL_LEAVE_RETRY_INTERVAL,
        );
        leave_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                incoming = read.next() => {
                    let incoming = incoming
                        .ok_or(GracefulShutdownError::PeerClosedBeforeLeaveAck)?
                        .map_err(|error| GracefulShutdownError::PeerProtocol {
                            reason: format!("websocket read failed: {error}"),
                        })?;
                    match incoming {
                        WsMessage::Ping(payload) => {
                            send_ws_frame(write, WsMessage::Pong(payload))
                                .await
                                .map_err(|reason| GracefulShutdownError::ControlWrite { reason })?;
                        }
                        WsMessage::Pong(_) => {}
                        WsMessage::Close(_) => {
                            return Err(GracefulShutdownError::PeerClosedBeforeLeaveAck);
                        }
                        text @ WsMessage::Text(_) => {
                            let message = decode_server_message(text)
                                .map_err(|reason| GracefulShutdownError::PeerProtocol { reason })?;
                            match message {
                                ServerMessage::CommandAck(ack) if ack.seq == leave_seq => {
                                    match ack.outcome {
                                        taiko_multiplayer_protocol::CommandOutcome::Applied { .. } => break,
                                        taiko_multiplayer_protocol::CommandOutcome::Rejected {
                                            error,
                                            ..
                                        } => {
                                            return Err(GracefulShutdownError::LeaveRejected {
                                                reason: protocol_error_reason(&error),
                                            });
                                        }
                                    }
                                }
                                ServerMessage::Fatal(error) => {
                                    return Err(GracefulShutdownError::PeerProtocol {
                                        reason: protocol_error_reason(&error),
                                    });
                                }
                                _ => {}
                            }
                        }
                        other => {
                            return Err(GracefulShutdownError::PeerProtocol {
                                reason: format!("unexpected websocket frame: {other:?}"),
                            });
                        }
                    }
                }
                _ = leave_retry.tick() => {
                    send_wire_message(write, leave_message)
                        .await
                        .map_err(|reason| GracefulShutdownError::MessageWrite {
                            index: leave_index,
                            reason,
                        })?;
                }
            }
        }

        send_ws_frame(write, WsMessage::Close(None))
            .await
            .map_err(|reason| GracefulShutdownError::CloseWrite { reason })
    })
    .await
    .map_err(|_| GracefulShutdownError::FlushTimedOut)?
}

async fn send_wire_message<S>(
    write: &mut S,
    message: &ClientMessage,
) -> std::result::Result<(), String>
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let raw = serde_json::to_string(message).map_err(|error| format!("encode failed: {error}"))?;
    if raw.len() > MAX_WIRE_MESSAGE_BYTES {
        return Err(format!(
            "outgoing protocol message exceeds {MAX_WIRE_MESSAGE_BYTES} bytes"
        ));
    }
    send_ws_frame(write, WsMessage::Text(raw.into())).await
}

async fn send_ws_frame<S>(write: &mut S, message: WsMessage) -> std::result::Result<(), String>
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    tokio::time::timeout(WRITE_TIMEOUT, write.send(message))
        .await
        .map_err(|_| "websocket write timed out".to_owned())?
        .map_err(|error| format!("websocket write failed: {error}"))
}

fn protocol_error_reason(error: &taiko_multiplayer_protocol::ProtocolError) -> String {
    format!("{:?}: {}", error.code, error.message)
}

fn decode_server_message(message: WsMessage) -> std::result::Result<ServerMessage, String> {
    let WsMessage::Text(raw) = message else {
        return Err("protocol violation: expected a JSON text frame".to_owned());
    };
    serde_json::from_str(&raw).map_err(|error| format!("invalid server message: {error}"))
}

fn emit_server_message(
    message: ServerMessage,
    event_tx: &SyncSender<NetworkEvent>,
    latest_live: &Arc<Mutex<Option<LiveStateSnapshot>>>,
    fault: &Arc<Mutex<Option<TransportFault>>>,
) -> std::result::Result<(), ()> {
    match message {
        ServerMessage::LiveState(live) => {
            let mut slot = latest_live.lock().expect("latest live mutex poisoned");
            if slot.as_ref().is_none_or(|current| {
                current.match_id != live.match_id || current.state_seq < live.state_seq
            }) {
                *slot = Some(live);
            }
            Ok(())
        }
        reliable => emit_reliable(event_tx, NetworkEvent::Server(Box::new(reliable)), fault),
    }
}

fn emit_reliable(
    event_tx: &SyncSender<NetworkEvent>,
    event: NetworkEvent,
    fault: &Arc<Mutex<Option<TransportFault>>>,
) -> std::result::Result<(), ()> {
    match event_tx.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::TrySendError::Full(_)) => {
            set_transport_fault(
                fault,
                "local reliable event queue overflowed; closing slow client".to_owned(),
                true,
            );
            Err(())
        }
        Err(mpsc::TrySendError::Disconnected(_)) => Err(()),
    }
}

fn set_transport_fault(
    fault: &Arc<Mutex<Option<TransportFault>>>,
    message: String,
    terminal: bool,
) {
    let mut slot = fault.lock().expect("transport fault mutex poisoned");
    if slot.is_none() || terminal {
        *slot = Some(TransportFault { message, terminal });
    }
}

#[derive(Clone)]
pub(crate) struct PreparedMatch {
    pub(crate) match_id: taiko_multiplayer_protocol::MatchId,
    pub(crate) selection: taiko_multiplayer_protocol::PlayerSelection,
    pub(crate) audio: Option<crate::audio::PreparedSongAudio>,
}

pub(crate) struct LocalPlayerRuntime {
    pub(crate) match_id: taiko_multiplayer_protocol::MatchId,
    pub(crate) gameplay: TaikoRuntime,
    pub(crate) pending_inputs: Vec<TimedInput<TaikoAction>>,
    pub(crate) last_tick: Tick,
    pub(crate) last_output: rhythm_core::FrameOutput<TaikoMode>,
    pub(crate) music_started: bool,
    pub(crate) audio_sync: Option<crate::audio_sync::AudioSyncController>,
    pub(crate) judge_flash: Option<crate::app::JudgeFlashState>,
    pub(crate) input_flash: Option<crate::app::InputFlashState>,
}

pub(crate) fn collect_due_inputs(
    pending_inputs: &mut Vec<TimedInput<TaikoAction>>,
    now_tick: Tick,
) -> Vec<TimedInput<TaikoAction>> {
    let split_at = pending_inputs.partition_point(|input| input.tick <= now_tick);
    pending_inputs.drain(..split_at).collect()
}

pub(crate) const fn to_drum_action(action: TaikoAction) -> DrumAction {
    DrumAction::new(
        match action.side {
            TaikoSide::Left => DrumSide::Left,
            TaikoSide::Right => DrumSide::Right,
        },
        match action.zone {
            TaikoZone::Don => DrumZone::Don,
            TaikoZone::Kat => DrumZone::Kat,
        },
    )
}

pub(crate) fn resource_http_endpoint(server: &str) -> Result<String> {
    let mut url = Url::parse(server).with_context(|| format!("invalid server URL: {server}"))?;
    match url.scheme() {
        "http" | "https" => {}
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow!("failed to map ws URL to http"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow!("failed to map wss URL to https"))?;
        }
        scheme => bail!("unsupported server URL scheme: {scheme}"),
    }
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn multiplayer_ws_url(server: &str) -> Result<Url> {
    let mut base = Url::parse(server).with_context(|| format!("invalid server URL: {server}"))?;
    match base.scheme() {
        "http" => {
            base.set_scheme("ws")
                .map_err(|_| anyhow!("failed to map http URL to ws"))?;
        }
        "https" => {
            base.set_scheme("wss")
                .map_err(|_| anyhow!("failed to map https URL to wss"))?;
        }
        "ws" | "wss" => {}
        scheme => bail!("unsupported server URL scheme: {scheme}"),
    }
    base.set_query(None);
    base.set_fragment(None);
    if !base.path().ends_with('/') {
        let path = format!("{}/", base.path());
        base.set_path(&path);
    }
    base.join("v2/multiplayer/ws")
        .context("failed to construct multiplayer WebSocket URL")
}

#[cfg(test)]
enum HeadlessResources {
    Player {
        backend: Arc<ResourceBackend>,
        songs: Vec<SongEntry>,
    },
    Spectator,
}

#[cfg(test)]
impl HeadlessResources {
    fn songs(&self) -> &[SongEntry] {
        match self {
            Self::Player { songs, .. } => songs,
            Self::Spectator => &[],
        }
    }

    fn player_backend(&self) -> Option<&Arc<ResourceBackend>> {
        match self {
            Self::Player { backend, .. } => Some(backend),
            Self::Spectator => None,
        }
    }
}

#[cfg(test)]
fn load_headless_resources(
    config: &OnlineClientConfig,
    memory_only_cache: bool,
) -> Result<HeadlessResources> {
    if !config.requires_authoritative_resources() {
        return Ok(HeadlessResources::Spectator);
    }

    let endpoint = resource_http_endpoint(config.server_url.as_str())?;
    let backend = Arc::new(ResourceBackend::remote(&endpoint, memory_only_cache)?);
    let library = backend
        .load_song_library()
        .context("failed to load the authoritative remote song library")?;
    if library.songs.is_empty() {
        bail!("the authoritative server has no playable songs");
    }
    for warning in library.warnings {
        eprintln!("Resource warning: {warning}");
    }
    Ok(HeadlessResources::Player {
        backend,
        songs: library.songs,
    })
}

#[cfg(test)]
struct HeadlessOnlineClient {
    domain: crate::online_session::OnlineDomain,
    resources: HeadlessResources,
    song_index: usize,
    course_index: usize,
    preparation: OnlinePreparationTask,
    session_generation: u64,
}

#[cfg(test)]
impl HeadlessOnlineClient {
    fn connect(config: OnlineClientConfig, memory_only_cache: bool) -> Result<Self> {
        let resources = load_headless_resources(&config, memory_only_cache)?;
        let domain = crate::online_session::OnlineDomain::connect(config)?;
        Ok(Self {
            domain,
            resources,
            song_index: 0,
            course_index: 0,
            preparation: OnlinePreparationTask::default(),
            session_generation: 1,
        })
    }

    fn tick(&mut self) -> Result<()> {
        self.domain.tick_network()?;
        self.process_domain_actions()?;
        self.poll_preparation()?;
        self.ensure_preparation()
    }

    fn process_domain_actions(&mut self) -> Result<()> {
        for action in std::mem::take(&mut self.domain.pending_actions) {
            if let crate::online_session::DomainAction::SongChanged { song } = action {
                if matches!(self.resources, HeadlessResources::Spectator) {
                    continue;
                }
                self.song_index = self
                    .resources
                    .songs()
                    .iter()
                    .position(|entry| entry.song_id() == Some(song.song_id.as_str()))
                    .ok_or_else(|| {
                        anyhow!(
                            "authoritative song {} is absent from the remote library",
                            song.song_id
                        )
                    })?;
                self.course_index = 0;
            }
        }
        Ok(())
    }

    fn preparation_identity(&self) -> Option<PreparationIdentity> {
        if self.domain.role() != Some(RoomRole::Player) {
            return None;
        }
        Some(PreparationIdentity {
            session_generation: self.session_generation,
            match_id: self.domain.current_match_id()?,
            selection: self.domain.local_selection()?,
        })
    }

    fn ensure_preparation(&mut self) -> Result<()> {
        let Some(identity) = self.preparation_identity() else {
            self.preparation.cancel();
            return Ok(());
        };

        if let Some(prepared) = self.domain.prepared_match.as_ref().filter(|prepared| {
            prepared.match_id == identity.match_id && prepared.selection == identity.selection
        }) {
            if self.domain.should_auto_ready() {
                let proof = self.domain.preparation_proof(prepared)?;
                self.domain.set_ready(true, Some(proof))?;
            }
            return Ok(());
        }
        if self.preparation.failure_reason(identity).is_some() {
            return Ok(());
        }

        if let Some(running) = self.preparation.identity() {
            if running != identity {
                self.preparation.cancel();
            }
            return Ok(());
        }

        let song_manifest = self
            .domain
            .current_song()
            .cloned()
            .ok_or_else(|| anyhow!("online match has no authoritative song manifest"))?;
        let song_index = self
            .resources
            .songs()
            .iter()
            .position(|song| song.song_id() == Some(song_manifest.song_id.as_str()))
            .ok_or_else(|| {
                anyhow!(
                    "authoritative song {} is absent from the remote library",
                    song_manifest.song_id
                )
            })?;
        let course_index = usize::try_from(identity.selection.course_id.0)
            .context("course id cannot be represented by this client")?;
        let song = self.resources.songs()[song_index].clone();
        let course = song
            .courses
            .iter()
            .find(|course| course.index == course_index)
            .cloned()
            .ok_or_else(|| anyhow!("authoritative course {course_index} is unavailable"))?;
        let course_manifest = song_manifest
            .courses
            .iter()
            .find(|course| course.course_id == identity.selection.course_id)
            .ok_or_else(|| anyhow!("course is absent from the authoritative manifest"))?;
        validate_authoritative_song_identity(&song, &song_manifest)?;
        if course.canonical_chart_hash != course_manifest.canonical_chart_hash.as_str() {
            bail!("downloaded multiplayer content does not match the authoritative manifest");
        }

        let backend = self
            .resources
            .player_backend()
            .ok_or_else(|| anyhow!("player session has no authoritative resource backend"))?;
        if !self.preparation.start(PreparationRequest {
            identity,
            backend: Arc::clone(backend),
            song,
            course_index,
            branch_decisions: course.branch_decisions,
        })? {
            bail!("online preparation task violated its single-worker invariant");
        }
        Ok(())
    }

    fn poll_preparation(&mut self) -> Result<()> {
        for event in self.preparation.poll() {
            let identity = event.identity();
            if self.preparation_identity() != Some(identity) {
                continue;
            }
            match event {
                PreparationEvent::Progress { progress, .. } => {
                    self.domain.report_preparation(progress)?;
                }
                PreparationEvent::Finished { completion, .. } => match completion {
                    PreparationCompletion::Prepared(prepared) => {
                        let prepared = *prepared;
                        if prepared.prepared_match.match_id != identity.match_id
                            || prepared.prepared_match.selection != identity.selection
                            || prepared.runtime.match_id != identity.match_id
                        {
                            bail!("online preparation returned a mismatched match identity");
                        }
                        self.domain.prepared_match = Some(prepared.prepared_match);
                        self.domain.local_player = Some(prepared.runtime);
                    }
                    PreparationCompletion::Cancelled => {}
                    PreparationCompletion::Failed(error) => {
                        drop(error);
                    }
                },
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(false);
        }
        if !self.domain.room_controls_enabled() {
            return Ok(!matches!(key.code, KeyCode::Esc));
        }
        if self.domain.phase() == crate::online_session::OnlinePhase::Playing {
            if let Some(action) = crate::input::map_bound_game_hit(
                key,
                crate::preferences::DrumBindings::player_one_default(),
            ) {
                let tick = self.domain.estimated_server_tick().max(0);
                self.domain.submit_input(tick, to_drum_action(action))?;
                return Ok(true);
            }
        }

        match key.code {
            KeyCode::Esc
                if self.domain.phase() == crate::online_session::OnlinePhase::Results
                    && self.domain.is_local_leader() =>
            {
                self.domain.return_to_lobby()?;
            }
            KeyCode::Esc => return Ok(false),
            KeyCode::Up | KeyCode::Left => self.move_selection(-1)?,
            KeyCode::Down | KeyCode::Right => self.move_selection(1)?,
            KeyCode::Enter => self.confirm_selection()?,
            KeyCode::Char('r' | 'R') => self.toggle_ready()?,
            _ => {}
        }
        Ok(true)
    }

    fn move_selection(&mut self, delta: isize) -> Result<()> {
        use crate::online_session::OnlinePhase;
        match self.domain.phase() {
            OnlinePhase::Lobby => {
                self.song_index =
                    wrapped_index(self.song_index, self.resources.songs().len(), delta);
                self.course_index = 0;
            }
            OnlinePhase::SelectingCourse
            | OnlinePhase::Downloading
            | OnlinePhase::Verifying
            | OnlinePhase::Loading
            | OnlinePhase::Prepared
            | OnlinePhase::Ready => {
                let course_count = self.authoritative_song_entry()?.courses.len();
                self.course_index = wrapped_index(self.course_index, course_count, delta);
            }
            _ => {}
        }
        Ok(())
    }

    fn confirm_selection(&mut self) -> Result<()> {
        use crate::online_session::OnlinePhase;
        let phase = self.domain.phase();
        match phase {
            OnlinePhase::Lobby if self.domain.is_local_leader() => {
                let song_id = self.resources.songs()[self.song_index]
                    .song_id()
                    .ok_or_else(|| anyhow!("remote song is missing its validated song id"))?;
                self.domain.select_song(song_id)?;
            }
            OnlinePhase::SelectingCourse
            | OnlinePhase::Downloading
            | OnlinePhase::Verifying
            | OnlinePhase::Loading
            | OnlinePhase::Prepared
            | OnlinePhase::Ready => {
                if self.domain.role() != Some(RoomRole::Player) {
                    return Ok(());
                }
                let highlighted = self.highlighted_course_selection()?;
                let retry_identity = self.preparation_identity().filter(|identity| {
                    identity.selection == highlighted
                        && self.preparation.failure_reason(*identity).is_some()
                });
                let action = headless_course_confirm_action(
                    phase,
                    self.domain.role(),
                    highlighted,
                    self.domain.local_selection(),
                    self.domain.can_start_match(),
                    retry_identity.is_some(),
                );
                match action {
                    HeadlessCourseConfirmAction::SelectCourse => {
                        if let Some(identity) = retry_identity {
                            self.preparation.retry(identity);
                        }
                        self.domain.select_course(highlighted)?;
                    }
                    HeadlessCourseConfirmAction::StartMatch => {
                        self.domain.start_match()?;
                    }
                    HeadlessCourseConfirmAction::None => {}
                }
            }
            OnlinePhase::Results if self.domain.is_local_leader() => {
                self.domain.rematch()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn highlighted_course_selection(&self) -> Result<PlayerSelection> {
        let song = self.authoritative_song_entry()?;
        let course = song
            .courses
            .get(self.course_index)
            .ok_or_else(|| anyhow!("selected course index is out of range"))?;
        let course_id =
            u32::try_from(course.index).context("course index exceeds the multiplayer protocol")?;
        Ok(PlayerSelection {
            course_id: CourseId(course_id),
        })
    }

    fn toggle_ready(&mut self) -> Result<()> {
        let retry_identity = self
            .preparation_identity()
            .filter(|identity| self.preparation.failure_reason(*identity).is_some());
        match headless_ready_action(
            self.domain.role(),
            retry_identity.is_some(),
            self.domain.is_local_ready(),
            self.domain.is_ready_command_pending(),
            self.domain.prepared_match.is_some(),
        ) {
            HeadlessReadyAction::RetryPreparation => {
                let identity = retry_identity
                    .expect("retry action requires a matching failed preparation identity");
                if !self.preparation.retry(identity) {
                    bail!("failed preparation could not enter its explicit retry state");
                }
                self.domain.select_course(identity.selection)?;
            }
            HeadlessReadyAction::SetUnready => {
                self.domain.set_ready(false, None)?;
            }
            HeadlessReadyAction::SetReady => {
                let prepared = self
                    .domain
                    .prepared_match
                    .as_ref()
                    .expect("ready action requires prepared content");
                let proof = self.domain.preparation_proof(prepared)?;
                self.domain.set_ready(true, Some(proof))?;
            }
            HeadlessReadyAction::None => {}
        }
        Ok(())
    }

    fn authoritative_song_entry(&self) -> Result<&SongEntry> {
        let Some(manifest) = self.domain.current_song() else {
            return self
                .resources
                .songs()
                .get(self.song_index)
                .ok_or_else(|| anyhow!("selected song index is out of range"));
        };
        self.resources
            .songs()
            .iter()
            .find(|song| song.song_id() == Some(manifest.song_id.as_str()))
            .ok_or_else(|| {
                anyhow!(
                    "authoritative song {} is absent from the remote library",
                    manifest.song_id
                )
            })
    }

    fn summary_lines(&self) -> Vec<String> {
        let mut lines = self.domain.summary_lines();
        lines.retain(|line| !line.starts_with("Esc / Ctrl-C:"));
        lines.extend(headless_score_lines(
            self.domain.snapshot.as_ref(),
            self.domain.latest_live_epoch(),
            &self.domain.live_states,
            &self.domain.final_results,
        ));
        if let Some(song) = self
            .domain
            .current_song()
            .and_then(|manifest| {
                self.resources
                    .songs()
                    .iter()
                    .find(|song| song.song_id() == Some(manifest.song_id.as_str()))
            })
            .or_else(|| self.resources.songs().get(self.song_index))
        {
            lines.push(format!(
                "Song: {} ({}/{})",
                song.title,
                self.song_index.saturating_add(1),
                self.resources.songs().len()
            ));
            if let Some(course) = song.courses.get(self.course_index) {
                lines.push(format!(
                    "Course: {} ({}/{})",
                    course.name,
                    self.course_index.saturating_add(1),
                    song.courses.len()
                ));
            }
        }
        if let Some(identity) = self.preparation_identity() {
            if let Some(reason) = self.preparation.failure_reason(identity) {
                lines.push(format!("Local preparation failed: {reason}"));
                lines.push("Press Enter or R on this course to retry.".to_owned());
            }
        }
        lines.push(self.command_help());
        lines
    }

    fn command_help(&self) -> String {
        use crate::online_session::OnlinePhase;
        let phase = self.domain.phase();
        let highlighted_selection = self.highlighted_course_selection().ok();
        let selection_changed = highlighted_selection
            .is_some_and(|selection| self.domain.local_selection() != Some(selection));
        let preparation_failed = highlighted_selection.is_some_and(|selection| {
            self.preparation_identity().is_some_and(|identity| {
                identity.selection == selection
                    && self.preparation.failure_reason(identity).is_some()
            })
        });
        let confirm_action =
            highlighted_selection.map_or(HeadlessCourseConfirmAction::None, |selection| {
                headless_course_confirm_action(
                    phase,
                    self.domain.role(),
                    selection,
                    self.domain.local_selection(),
                    self.domain.can_start_match(),
                    preparation_failed,
                )
            });
        let contextual = match self.domain.phase() {
            OnlinePhase::Lobby if self.domain.is_local_leader() => "up/down: song, enter: select",
            OnlinePhase::SelectingCourse
            | OnlinePhase::Downloading
            | OnlinePhase::Verifying
            | OnlinePhase::Loading
            | OnlinePhase::Prepared
            | OnlinePhase::Ready
                if self.domain.role() == Some(RoomRole::Player) && preparation_failed =>
            {
                "up/down: course, enter/r: retry preparation"
            }
            OnlinePhase::SelectingCourse
            | OnlinePhase::Downloading
            | OnlinePhase::Verifying
            | OnlinePhase::Loading
            | OnlinePhase::Prepared
            | OnlinePhase::Ready
                if self.domain.role() == Some(RoomRole::Player) && selection_changed =>
            {
                "up/down: course, enter: apply changed selection"
            }
            OnlinePhase::Prepared | OnlinePhase::Ready
                if confirm_action == HeadlessCourseConfirmAction::StartMatch =>
            {
                "r: ready/unready, enter: start"
            }
            OnlinePhase::Prepared | OnlinePhase::Ready => {
                "up/down: course, enter: apply change; r: ready/unready"
            }
            OnlinePhase::SelectingCourse
            | OnlinePhase::Downloading
            | OnlinePhase::Verifying
            | OnlinePhase::Loading
                if self.domain.role() == Some(RoomRole::Player) =>
            {
                "up/down: course, enter: apply selection"
            }
            OnlinePhase::Playing if self.domain.role() == Some(RoomRole::Player) => {
                "key f/j: Don, key d/k: Kat"
            }
            OnlinePhase::Results if self.domain.is_local_leader() => "enter: rematch, esc: lobby",
            _ => "wait for the authoritative room state",
        };
        format!("Commands: {contextual}; q / Ctrl-C: disconnect")
    }

    fn shutdown(&mut self) -> Result<()> {
        self.preparation.cancel();
        self.domain.shutdown_gracefully()
    }
}

#[cfg(test)]
fn headless_score_lines(
    snapshot: Option<&RoomSnapshot>,
    latest_live_epoch: Option<(MatchId, StateSeq)>,
    live_states: &HashMap<PlayerId, PlayerLiveState>,
    final_results: &HashMap<PlayerId, FinalResult>,
) -> Vec<String> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };

    let mut lines = Vec::new();
    if final_results.is_empty() {
        if let Some((match_id, state_seq)) = latest_live_epoch {
            lines.push(format!(
                "Live state: match={} sequence={}",
                match_id.0, state_seq.0
            ));
        }
        for player in &snapshot.players {
            let Some(live) = live_states.get(&player.player_id) else {
                continue;
            };
            let status = if live.dnf {
                "dnf"
            } else if live.finished {
                "finished"
            } else {
                "playing"
            };
            lines.push(format!(
                "Live {} (P{}): score={} combo={} max_combo={} gauge_ppm={} \
                 pass_threshold_ppm={} great={} ok={} miss={} roll_hits={} status={status}",
                player.name,
                player.player_id.0,
                live.score.score,
                live.score.combo,
                live.score.max_combo,
                live.score.gauge_ppm,
                live.score.pass_threshold_ppm,
                live.score.great,
                live.score.ok,
                live.score.miss,
                live.score.roll_hits,
            ));
        }
        return lines;
    }

    for player in &snapshot.players {
        let Some(result) = final_results.get(&player.player_id) else {
            continue;
        };
        let outcome = if result.dnf {
            "DNF"
        } else if result.passed {
            "PASS"
        } else {
            "FAIL"
        };
        lines.push(format!(
            "Result {} (P{}): {outcome} course={} score={} combo={} max_combo={} \
             gauge_ppm={} pass_threshold_ppm={} great={} ok={} miss={} roll_hits={} \
             finish_tick={} replay={}",
            player.name,
            player.player_id.0,
            result.course_id.0,
            result.score.score,
            result.score.combo,
            result.score.max_combo,
            result.score.gauge_ppm,
            result.score.pass_threshold_ppm,
            result.score.great,
            result.score.ok,
            result.score.miss,
            result.score.roll_hits,
            result.finish_tick,
            result.replay_digest,
        ));
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum HeadlessCourseConfirmAction {
    None,
    SelectCourse,
    StartMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
enum HeadlessReadyAction {
    None,
    RetryPreparation,
    SetUnready,
    SetReady,
}

#[cfg(test)]
fn headless_ready_action(
    role: Option<RoomRole>,
    preparation_failed: bool,
    is_local_ready: bool,
    ready_command_pending: bool,
    has_prepared_content: bool,
) -> HeadlessReadyAction {
    if role != Some(RoomRole::Player) {
        return HeadlessReadyAction::None;
    }
    if preparation_failed {
        return HeadlessReadyAction::RetryPreparation;
    }
    if is_local_ready || ready_command_pending {
        return HeadlessReadyAction::SetUnready;
    }
    if has_prepared_content {
        return HeadlessReadyAction::SetReady;
    }
    HeadlessReadyAction::None
}

#[cfg(test)]
fn headless_course_confirm_action(
    phase: crate::online_session::OnlinePhase,
    role: Option<RoomRole>,
    highlighted: PlayerSelection,
    authoritative: Option<PlayerSelection>,
    can_start_match: bool,
    preparation_failed: bool,
) -> HeadlessCourseConfirmAction {
    use crate::online_session::OnlinePhase;
    if role != Some(RoomRole::Player)
        || !matches!(
            phase,
            OnlinePhase::SelectingCourse
                | OnlinePhase::Downloading
                | OnlinePhase::Verifying
                | OnlinePhase::Loading
                | OnlinePhase::Prepared
                | OnlinePhase::Ready
        )
    {
        return HeadlessCourseConfirmAction::None;
    }
    if preparation_failed || authoritative != Some(highlighted) {
        return HeadlessCourseConfirmAction::SelectCourse;
    }
    if matches!(phase, OnlinePhase::Prepared | OnlinePhase::Ready) && can_start_match {
        return HeadlessCourseConfirmAction::StartMatch;
    }
    HeadlessCourseConfirmAction::None
}

#[cfg(test)]
fn wrapped_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta.is_negative() {
        current.checked_sub(delta.unsigned_abs()).unwrap_or(len - 1) % len
    } else {
        current.saturating_add(delta as usize) % len
    }
}

pub(crate) struct EmbeddedServer {
    shutdown: Option<taiko_resource_server::ServerShutdown>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl EmbeddedServer {
    pub(crate) fn start_local(songdir: PathBuf) -> Result<(Self, Url)> {
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let server_args = taiko_resource_server::ServerArgs {
            songdir,
            host: "127.0.0.1".to_owned(),
            port: 0,
        };
        let thread = std::thread::Builder::new()
            .name("taiko-embedded-server".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .context("failed to initialize embedded server runtime")?;
                let started = runtime.block_on(
                    taiko_resource_server::start_server_background_controlled(server_args),
                );
                let (address, handle, shutdown) = match started {
                    Ok(started) => started,
                    Err(error) => {
                        let _ = ready_tx.send(Err(format!("{error:#}")));
                        return Err(error);
                    }
                };
                if ready_tx.send(Ok((address, shutdown))).is_err() {
                    handle.abort();
                    return Err(anyhow!("embedded server starter was dropped"));
                }
                runtime
                    .block_on(handle)
                    .context("embedded server task panicked")?
            })
            .context("failed to spawn embedded server thread")?;

        let (address, shutdown) = ready_rx
            .recv()
            .context("embedded server stopped before reporting its address")?
            .map_err(anyhow::Error::msg)?;
        let advertised = match MultiplayerInvite::normalize_server(&format!("http://{address}")) {
            Ok(advertised) => advertised,
            Err(error) => {
                shutdown.shutdown();
                let shutdown_result = thread
                    .join()
                    .map_err(|_| anyhow!("embedded server thread panicked"))
                    .and_then(|result| result.context("embedded server stopped with an error"));
                return match shutdown_result {
                    Ok(()) => Err(error),
                    Err(shutdown_error) => Err(anyhow!(
                        "{error}; embedded server shutdown failed: {shutdown_error}"
                    )),
                };
            }
        };

        Ok((
            Self {
                shutdown: Some(shutdown),
                thread: Some(thread),
            },
            advertised,
        ))
    }

    pub(crate) fn shutdown_and_join(mut self) -> Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.shutdown();
        }
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| anyhow!("embedded server thread panicked"))?
            .context("embedded server stopped with an error")
    }
}

impl Drop for EmbeddedServer {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown.shutdown();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::mpsc as std_mpsc;
    use std::time::Instant;

    use super::*;
    use crate::online_test_proxy::AckLossProxy;

    const HEADLESS_E2E_STEP_TIMEOUT: Duration = Duration::from_secs(10);
    const HEADLESS_E2E_MATCH_TIMEOUT: Duration = Duration::from_secs(15);
    const HEADLESS_E2E_NOTE_TICK: Tick = 1_000_000;
    const HEADLESS_E2E_INPUT_LEAD_TICKS: Tick = 20_000;
    static NEXT_HEADLESS_E2E_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    struct HeadlessE2eSongFixture {
        path: PathBuf,
    }

    impl HeadlessE2eSongFixture {
        fn create() -> Result<Self> {
            Self::create_with_tja(concat!(
                "TITLE:Production Loopback\n",
                "BPM:600\n",
                "WAVE:don.wav\n",
                "COURSE:Easy\n",
                "LEVEL:1\n",
                "#START\n",
                "0,\n",
                "0,\n",
                "0010,\n",
                "#END\n",
                "COURSE:Oni\n",
                "LEVEL:1\n",
                "#START\n",
                "0,\n",
                "0,\n",
                "0020,\n",
                "#END\n",
            ))
        }

        fn create_ack_loss() -> Result<Self> {
            Self::create_with_tja(concat!(
                "TITLE:Production ACK Loss\n",
                "BPM:600\n",
                "WAVE:don.wav\n",
                "COURSE:Easy\n",
                "LEVEL:1\n",
                "#START\n",
                "0,\n",
                "0,\n",
                "0010,\n",
                "5000,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0008,\n",
                "#END\n",
                "COURSE:Oni\n",
                "LEVEL:1\n",
                "#START\n",
                "0,\n",
                "0,\n",
                "0020,\n",
                "5000,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0,\n",
                "0008,\n",
                "#END\n",
            ))
        }

        fn create_with_tja(tja: &str) -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "taiko-headless-e2e-{}-{}",
                std::process::id(),
                NEXT_HEADLESS_E2E_FIXTURE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ));
            std::fs::create_dir(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            std::fs::write(path.join("don.wav"), include_bytes!("../assets/don.wav"))
                .context("failed to write the end-to-end WAV fixture")?;
            std::fs::write(path.join("loopback.tja"), tja)
                .context("failed to write the end-to-end TJA fixture")?;
            Ok(Self { path })
        }
    }

    impl Drop for HeadlessE2eSongFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct HeadlessE2eServer {
        base_url: Url,
        shutdown: Option<taiko_resource_server::ServerShutdown>,
        finished: std_mpsc::Receiver<std::result::Result<(), String>>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl HeadlessE2eServer {
        fn start(songdir: PathBuf) -> Result<Self> {
            let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
            let (finished_tx, finished_rx) = std_mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("taiko-headless-e2e-server".to_owned())
                .spawn(move || {
                    let setup = (|| -> Result<_> {
                        let runtime = Builder::new_multi_thread()
                            .enable_all()
                            .build()
                            .context("failed to initialize the end-to-end server runtime")?;
                        let started = runtime.block_on(
                            taiko_resource_server::start_server_background_controlled(
                                taiko_resource_server::ServerArgs {
                                    songdir,
                                    host: "127.0.0.1".to_owned(),
                                    port: 0,
                                },
                            ),
                        )?;
                        Ok((runtime, started))
                    })();
                    let (runtime, (address, handle, shutdown)) = match setup {
                        Ok(started) => started,
                        Err(error) => {
                            let message = format!("{error:#}");
                            let _ = ready_tx.send(Err(message.clone()));
                            let _ = finished_tx.send(Err(message));
                            return;
                        }
                    };
                    if ready_tx.send(Ok((address, shutdown))).is_err() {
                        handle.abort();
                        let _ = finished_tx.send(Err(
                            "end-to-end test dropped the server startup result".to_owned(),
                        ));
                        return;
                    }
                    let result = runtime
                        .block_on(handle)
                        .map_err(|error| format!("end-to-end server task panicked: {error}"))
                        .and_then(|result| {
                            result.map_err(|error| format!("end-to-end server failed: {error:#}"))
                        });
                    let _ = finished_tx.send(result);
                })
                .context("failed to spawn the end-to-end server thread")?;

            let (address, shutdown) = ready_rx
                .recv_timeout(Duration::from_secs(5))
                .context("end-to-end server startup timed out")?
                .map_err(anyhow::Error::msg)?;
            Ok(Self {
                base_url: Url::parse(&format!("http://{address}/"))
                    .context("failed to construct the end-to-end server URL")?,
                shutdown: Some(shutdown),
                finished: finished_rx,
                thread: Some(thread),
            })
        }

        fn shutdown_and_wait(mut self) -> Result<()> {
            self.shutdown
                .take()
                .expect("running fixture owns its shutdown sender")
                .shutdown();
            let result = self
                .finished
                .recv_timeout(Duration::from_secs(5))
                .context("end-to-end server shutdown timed out")?;
            if let Some(thread) = self.thread.take() {
                thread
                    .join()
                    .map_err(|_| anyhow!("end-to-end server thread panicked"))?;
            }
            result.map_err(anyhow::Error::msg)
        }
    }

    impl Drop for HeadlessE2eServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                shutdown.shutdown();
            }
        }
    }

    fn ensure_headless_client_healthy(
        client: &HeadlessOnlineClient,
        label: &'static str,
    ) -> Result<()> {
        if client.domain.is_terminal() {
            bail!("{label} failed: {}", client.domain.status_message());
        }
        if let Some(identity) = client.preparation_identity() {
            if let Some(reason) = client.preparation.failure_reason(identity) {
                bail!("{label} preparation failed: {reason}");
            }
        }
        Ok(())
    }

    fn wait_for_headless_client(
        client: &mut HeadlessOnlineClient,
        label: &'static str,
        timeout: Duration,
        mut predicate: impl FnMut(&HeadlessOnlineClient) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            client
                .tick()
                .with_context(|| format!("{label} tick failed"))?;
            ensure_headless_client_healthy(client, label)?;
            if predicate(client) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "{label} timed out after {} ms; {}",
                    timeout.as_millis(),
                    client.summary_lines().join(" | "),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_headless_pair(
        host: &mut HeadlessOnlineClient,
        guest: &mut HeadlessOnlineClient,
        label: &'static str,
        timeout: Duration,
        mut predicate: impl FnMut(&HeadlessOnlineClient, &HeadlessOnlineClient) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            host.tick()
                .with_context(|| format!("{label}: host tick failed"))?;
            guest
                .tick()
                .with_context(|| format!("{label}: guest tick failed"))?;
            ensure_headless_client_healthy(host, "host")?;
            ensure_headless_client_healthy(guest, "guest")?;
            if predicate(host, guest) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "{label} timed out after {} ms; host [{}]; guest [{}]",
                    timeout.as_millis(),
                    host.summary_lines().join(" | "),
                    guest.summary_lines().join(" | "),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn drive_headless_pair_inputs_to_results(
        host: &mut HeadlessOnlineClient,
        guest: &mut HeadlessOnlineClient,
    ) -> Result<()> {
        use crate::online_session::OnlinePhase;

        let deadline = Instant::now() + HEADLESS_E2E_MATCH_TIMEOUT;
        let send_at = HEADLESS_E2E_NOTE_TICK.saturating_sub(HEADLESS_E2E_INPUT_LEAD_TICKS);
        let mut host_submitted = false;
        let mut guest_submitted = false;
        loop {
            host.tick()
                .context("authoritative input/result drive: host tick failed")?;
            guest
                .tick()
                .context("authoritative input/result drive: guest tick failed")?;
            ensure_headless_client_healthy(host, "host")?;
            ensure_headless_client_healthy(guest, "guest")?;

            if !host_submitted && host.domain.phase() == OnlinePhase::Playing {
                let tick = host.domain.estimated_server_tick();
                if tick >= send_at {
                    if tick >= HEADLESS_E2E_NOTE_TICK + rhythm_mode_taiko::OK_WINDOW_TICKS {
                        bail!(
                            "host reached tick {tick} before its production Don input was submitted"
                        );
                    }
                    if !host.handle_key(crossterm::event::KeyEvent::new(
                        KeyCode::Char('s'),
                        KeyModifiers::NONE,
                    ))? {
                        bail!("host production Don key unexpectedly stopped the client");
                    }
                    host_submitted = true;
                }
            }
            if !guest_submitted && guest.domain.phase() == OnlinePhase::Playing {
                let tick = guest.domain.estimated_server_tick();
                if tick >= send_at {
                    if tick >= HEADLESS_E2E_NOTE_TICK + rhythm_mode_taiko::OK_WINDOW_TICKS {
                        bail!(
                            "guest reached tick {tick} before its production Kat input was submitted"
                        );
                    }
                    if !guest.handle_key(crossterm::event::KeyEvent::new(
                        KeyCode::Char('a'),
                        KeyModifiers::NONE,
                    ))? {
                        bail!("guest production Kat key unexpectedly stopped the client");
                    }
                    guest_submitted = true;
                }
            }

            if host.domain.phase() == OnlinePhase::Results
                && guest.domain.phase() == OnlinePhase::Results
            {
                if !host_submitted || !guest_submitted {
                    bail!(
                        "match finished before both production inputs were submitted \
                         (host={host_submitted}, guest={guest_submitted})"
                    );
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "authoritative input/result drive timed out after {} ms \
                     (host_submitted={host_submitted}, guest_submitted={guest_submitted}); \
                     host [{}]; guest [{}]",
                    HEADLESS_E2E_MATCH_TIMEOUT.as_millis(),
                    host.summary_lines().join(" | "),
                    guest.summary_lines().join(" | "),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn command_message(
        seq: u64,
        command: taiko_multiplayer_protocol::ClientCommand,
    ) -> ClientMessage {
        ClientMessage::Command(taiko_multiplayer_protocol::CommandEnvelope {
            seq: taiko_multiplayer_protocol::CommandSeq(seq),
            expected_room_revision: None,
            command,
        })
    }

    fn test_welcome() -> ServerMessage {
        ServerMessage::Welcome(taiko_multiplayer_protocol::ServerWelcome {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("wire schema hash"),
            heartbeat_interval_ms: 1_000,
            reconnect_grace_ms: 15_000,
            resumed: false,
            next_expected_command_seq: taiko_multiplayer_protocol::FIRST_COMMAND_SEQ,
        })
    }

    fn test_membership() -> ServerMessage {
        ServerMessage::MembershipGranted(taiko_multiplayer_protocol::MembershipGranted {
            room_code: RoomCode::parse("ABCD").expect("room code"),
            actor_id: taiko_multiplayer_protocol::ActorId::Player(
                taiko_multiplayer_protocol::PlayerId(1),
            ),
            resume_token: taiko_multiplayer_protocol::ResumeToken::parse("a".repeat(64))
                .expect("resume token"),
            invitation_token: InvitationToken::parse("b".repeat(64)).expect("invitation token"),
        })
    }

    async fn send_test_server_message(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        message: ServerMessage,
    ) -> Result<()> {
        let raw = serde_json::to_string(&message)?;
        socket.send(WsMessage::Text(raw.into())).await?;
        Ok(())
    }

    async fn receive_test_client_message(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    ) -> Result<ClientMessage> {
        loop {
            let frame = socket
                .next()
                .await
                .ok_or_else(|| anyhow!("client closed before sending a message"))??;
            match frame {
                WsMessage::Text(raw) => return Ok(serde_json::from_str(&raw)?),
                WsMessage::Ping(payload) => {
                    socket.send(WsMessage::Pong(payload)).await?;
                }
                WsMessage::Pong(_) => {}
                WsMessage::Close(_) => bail!("client closed before sending a message"),
                other => bail!("unexpected client frame: {other:?}"),
            }
        }
    }

    #[test]
    fn reconnect_backoff_is_exponential_and_capped() {
        let policy = ReconnectPolicy::default();
        assert_eq!(policy.delay_for_attempt(1), Duration::ZERO);
        assert_eq!(policy.delay_for_attempt(2), Duration::from_millis(250));
        assert_eq!(policy.delay_for_attempt(3), Duration::from_millis(500));
        assert_eq!(policy.delay_for_attempt(4), Duration::from_secs(1));
        assert_eq!(policy.delay_for_attempt(20), Duration::from_secs(5));
    }

    #[test]
    fn unhealthy_welcome_flapping_consumes_attempts_and_increases_backoff() {
        let policy = ReconnectPolicy {
            initial_delay: Duration::from_millis(25),
            maximum_delay: Duration::from_secs(1),
            maximum_attempts: 3,
        };
        let mut budget = ReconnectBudget::new(&policy);

        for ordinal in 1..=3 {
            let attempt = budget.begin_attempt().expect("attempt remains");
            assert_eq!(attempt.ordinal, ordinal);
            if ordinal == 1 {
                assert_eq!(attempt.delay, Duration::ZERO);
            } else {
                assert_eq!(
                    attempt.delay,
                    policy.delay_for_attempt(ordinal),
                    "an unhealthy Welcome must not reset backoff"
                );
            }
            let next = budget.record_failure(false);
            assert_eq!(next.is_some(), ordinal < 3);
        }
        assert!(budget.begin_attempt().is_none());
        assert_eq!(budget.attempts_started(), 3);
    }

    #[test]
    fn only_stable_heartbeat_evidence_resets_attempts_and_backoff() {
        let policy = ReconnectPolicy {
            initial_delay: Duration::from_millis(25),
            maximum_delay: Duration::from_secs(1),
            maximum_attempts: 3,
        };
        let base = tokio::time::Instant::now();
        let heartbeat =
            ServerMessage::HeartbeatAck(taiko_multiplayer_protocol::HeartbeatAck { nonce: 1 });
        let mut health = ConnectionHealthTracker::new();
        health.observe(&heartbeat, base + STABLE_CONNECTION_WINDOW);
        assert!(
            !health.is_stable(),
            "Welcome and heartbeat traffic without affiliation is not stable"
        );
        health.observe(&test_membership(), base);
        health.observe(
            &heartbeat,
            base + STABLE_CONNECTION_WINDOW - Duration::from_millis(1),
        );
        assert!(
            !health.is_stable(),
            "a short Welcome session must not reset retry state"
        );

        let mut budget = ReconnectBudget::new(&policy);
        assert_eq!(budget.begin_attempt().expect("first").ordinal, 1);
        assert_eq!(
            budget
                .record_failure(health.is_stable())
                .expect("second")
                .ordinal,
            2
        );
        assert_eq!(budget.begin_attempt().expect("second").ordinal, 2);

        health.observe(&heartbeat, base + STABLE_CONNECTION_WINDOW);
        assert!(health.is_stable());
        let reset = budget
            .record_failure(health.is_stable())
            .expect("reset attempt");
        assert_eq!(reset.ordinal, 1);
        assert_eq!(reset.delay, Duration::ZERO);
        assert_eq!(budget.begin_attempt().expect("new first").ordinal, 1);
    }

    #[test]
    fn endpoint_mapping_is_strict() {
        assert_eq!(
            multiplayer_ws_url("https://example.test/base")
                .expect("valid URL")
                .as_str(),
            "wss://example.test/base/v2/multiplayer/ws"
        );
        assert_eq!(
            resource_http_endpoint("wss://example.test/base?x=1").expect("valid URL"),
            "https://example.test/base"
        );
        assert!(multiplayer_ws_url("ftp://example.test").is_err());
    }

    #[test]
    fn spectator_headless_resources_do_not_contact_http_authority() {
        let config = OnlineClientConfig::join(
            "http://127.0.0.1:1",
            "viewer",
            "ABCD",
            &"a".repeat(64),
            JoinRole::Spectator,
        )
        .expect("spectator config");

        let resources = load_headless_resources(&config, true).expect("resource-free spectator");

        assert!(!config.requires_authoritative_resources());
        assert!(matches!(resources, HeadlessResources::Spectator));
    }

    #[test]
    fn player_and_creator_configs_require_authoritative_resources() {
        let create =
            OnlineClientConfig::create("https://example.test", "host").expect("create config");
        let join = OnlineClientConfig::join(
            "https://example.test",
            "player",
            "ABCD",
            &"a".repeat(64),
            JoinRole::Player,
        )
        .expect("join config");

        assert!(create.requires_authoritative_resources());
        assert!(join.requires_authoritative_resources());
    }

    #[test]
    fn complete_invite_builds_player_connection_config() {
        let invite = MultiplayerInvite::parse(&format!(
            "taiko://join?server=https%3A%2F%2Fexample.test&room=ABCD&token={}",
            "a".repeat(64)
        ))
        .expect("parse complete invite");
        let config = OnlineClientConfig::join(
            invite.server().as_str(),
            "alice",
            invite.room_code().as_str(),
            invite.invitation_token().expose(),
            JoinRole::Player,
        )
        .expect("build player config");
        assert!(matches!(
            config.room_intent,
            RoomIntent::Join {
                role: JoinRole::Player,
                ..
            }
        ));
    }

    #[test]
    fn embedded_ui_server_uses_loopback_and_an_ephemeral_port() -> Result<()> {
        let fixture = HeadlessE2eSongFixture::create()?;
        let (server, endpoint) = EmbeddedServer::start_local(fixture.path.clone())?;
        assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
        assert_ne!(endpoint.port_or_known_default(), Some(0));
        server.shutdown_and_join()
    }

    #[test]
    fn due_input_collection_preserves_future_events() {
        let mut inputs = vec![
            TimedInput {
                tick: 10,
                action: TaikoAction::LEFT_DON,
            },
            TimedInput {
                tick: 20,
                action: TaikoAction::RIGHT_KAT,
            },
        ];
        let due = collect_due_inputs(&mut inputs, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].tick, 20);
    }

    #[test]
    fn gameplay_to_wire_conversion_preserves_all_four_actions() {
        for (gameplay, wire) in [
            (TaikoAction::LEFT_DON, DrumAction::LEFT_DON),
            (TaikoAction::RIGHT_DON, DrumAction::RIGHT_DON),
            (TaikoAction::LEFT_KAT, DrumAction::LEFT_KAT),
            (TaikoAction::RIGHT_KAT, DrumAction::RIGHT_KAT),
        ] {
            assert_eq!(to_drum_action(gameplay), wire);
        }
    }

    #[test]
    fn writer_queue_is_bounded() {
        let (client, _peer) = NetworkClient::test_pair();
        for nonce in 0..OUTBOUND_CAPACITY {
            client
                .try_send(ClientMessage::Heartbeat(
                    taiko_multiplayer_protocol::Heartbeat {
                        nonce: nonce as u64,
                    },
                ))
                .expect("queue has declared capacity");
        }
        assert!(
            client
                .try_send(ClientMessage::Heartbeat(
                    taiko_multiplayer_protocol::Heartbeat {
                        nonce: OUTBOUND_CAPACITY as u64,
                    },
                ))
                .is_err(),
            "the writer must apply backpressure instead of growing"
        );
    }

    #[test]
    fn graceful_batch_requires_one_final_leave_command() {
        let heartbeat =
            ClientMessage::Heartbeat(taiko_multiplayer_protocol::Heartbeat { nonce: 1 });
        assert_eq!(
            validate_graceful_messages(std::slice::from_ref(&heartbeat)),
            Err(GracefulShutdownError::MissingLeave)
        );
        let leave = command_message(1, taiko_multiplayer_protocol::ClientCommand::LeaveRoom);
        assert_eq!(
            validate_graceful_messages(&[leave.clone(), heartbeat]),
            Err(GracefulShutdownError::LeaveNotLast)
        );
        assert_eq!(
            validate_graceful_messages(&[leave.clone(), leave]),
            Err(GracefulShutdownError::LeaveNotLast)
        );
    }

    #[test]
    fn test_pair_simulates_graceful_transport_completion() {
        let (client, mut peer) = NetworkClient::test_pair();
        let messages = vec![command_message(
            1,
            taiko_multiplayer_protocol::ClientCommand::LeaveRoom,
        )];

        client
            .shutdown_gracefully(messages.clone())
            .expect("simulated transport completion");
        assert_eq!(peer.take_graceful_shutdown_messages(), Some(messages));
    }

    #[test]
    fn graceful_shutdown_returns_only_after_real_server_acknowledges_leave() {
        let (address_tx, address_rx) = std_mpsc::sync_channel(1);
        let (leave_tx, leave_rx) = std_mpsc::sync_channel(1);
        let (release_ack_tx, release_ack_rx) = std_mpsc::sync_channel(0);
        let server = thread::spawn(move || -> Result<()> {
            let runtime = Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                address_tx.send(listener.local_addr()?)?;
                let (stream, _) = listener.accept().await?;
                let mut socket = tokio_tungstenite::accept_async(stream).await?;
                assert!(matches!(
                    receive_test_client_message(&mut socket).await?,
                    ClientMessage::Hello(_)
                ));
                send_test_server_message(&mut socket, test_welcome()).await?;

                loop {
                    let frame = socket
                        .next()
                        .await
                        .ok_or_else(|| anyhow!("client closed without a Close frame"))??;
                    match frame {
                        WsMessage::Text(raw) => {
                            let message: ClientMessage = serde_json::from_str(&raw)?;
                            let ClientMessage::Command(envelope) = message else {
                                continue;
                            };
                            if matches!(
                                envelope.command,
                                taiko_multiplayer_protocol::ClientCommand::LeaveRoom
                            ) {
                                leave_tx.send(envelope.seq)?;
                                release_ack_rx
                                    .recv_timeout(Duration::from_secs(2))
                                    .context(
                                        "test did not release the LeaveRoom acknowledgement",
                                    )?;
                            }
                            send_test_server_message(
                                &mut socket,
                                ServerMessage::CommandAck(taiko_multiplayer_protocol::CommandAck {
                                    seq: envelope.seq,
                                    next_expected_seq: taiko_multiplayer_protocol::CommandSeq(
                                        envelope.seq.0 + 1,
                                    ),
                                    outcome: taiko_multiplayer_protocol::CommandOutcome::Applied {
                                        room_revision: None,
                                    },
                                }),
                            )
                            .await?;
                        }
                        WsMessage::Close(_) => break,
                        WsMessage::Ping(payload) => {
                            socket.send(WsMessage::Pong(payload)).await?;
                        }
                        WsMessage::Pong(_) => {}
                        other => bail!("unexpected client frame: {other:?}"),
                    }
                }
                Ok(())
            })
        });

        let address = address_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server address");
        let mut config =
            OnlineClientConfig::create(&format!("http://{address}"), "alice").expect("config");
        config.reconnect.maximum_attempts = 1;
        let client = NetworkClient::connect(&config).expect("network client");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match client.try_recv() {
                Ok(Some(NetworkEvent::Server(message)))
                    if matches!(*message, ServerMessage::Welcome(_)) =>
                {
                    break;
                }
                Ok(_) => {}
                Err(error) => panic!("transport failed before welcome: {error}"),
            }
            assert!(Instant::now() < deadline, "welcome timed out");
            thread::yield_now();
        }

        let messages = vec![
            command_message(1, taiko_multiplayer_protocol::ClientCommand::CreateRoom),
            command_message(2, taiko_multiplayer_protocol::ClientCommand::LeaveRoom),
        ];
        let (shutdown_tx, shutdown_rx) = std_mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            let result = client
                .shutdown_gracefully(messages)
                .map_err(|error| format!("{error:#}"));
            let _ = shutdown_tx.send(result);
        });
        assert_eq!(
            leave_rx.recv_timeout(Duration::from_secs(2)),
            Ok(taiko_multiplayer_protocol::CommandSeq(2)),
            "server must observe the final LeaveRoom command"
        );
        assert!(
            matches!(
                shutdown_rx.recv_timeout(Duration::from_millis(150)),
                Err(std_mpsc::RecvTimeoutError::Timeout)
            ),
            "graceful shutdown returned before LeaveRoom was acknowledged"
        );
        release_ack_tx
            .send(())
            .expect("release LeaveRoom acknowledgement");
        shutdown_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("graceful shutdown did not complete after LeaveRoom acknowledgement")
            .map_err(anyhow::Error::msg)
            .expect("graceful shutdown");
        shutdown.join().expect("shutdown caller thread");
        server
            .join()
            .expect("server thread")
            .expect("server completed");
    }

    #[test]
    fn graceful_shutdown_retries_leave_until_acknowledged() {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime
            .block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                let address = listener.local_addr()?;
                let server = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await?;
                    let mut socket = tokio_tungstenite::accept_async(stream).await?;
                    let first = receive_test_client_message(&mut socket).await?;
                    let second = tokio::time::timeout(
                        Duration::from_secs(2),
                        receive_test_client_message(&mut socket),
                    )
                    .await
                    .context(
                        "client did not retry LeaveRoom after its acknowledgement was lost",
                    )??;
                    assert_eq!(second, first, "LeaveRoom retry must be byte-equivalent");

                    let ClientMessage::Command(envelope) = first else {
                        bail!("expected LeaveRoom command");
                    };
                    assert!(matches!(
                        envelope.command,
                        taiko_multiplayer_protocol::ClientCommand::LeaveRoom
                    ));
                    send_test_server_message(
                        &mut socket,
                        ServerMessage::CommandAck(taiko_multiplayer_protocol::CommandAck {
                            seq: envelope.seq,
                            next_expected_seq: taiko_multiplayer_protocol::CommandSeq(
                                envelope.seq.0 + 1,
                            ),
                            outcome: taiko_multiplayer_protocol::CommandOutcome::Applied {
                                room_revision: None,
                            },
                        }),
                    )
                    .await?;

                    let frame = tokio::time::timeout(Duration::from_secs(2), socket.next())
                        .await
                        .context(
                            "client did not close after the retried LeaveRoom was acknowledged",
                        )?
                        .ok_or_else(|| anyhow!("client disconnected without a Close frame"))??;
                    assert!(matches!(frame, WsMessage::Close(_)));
                    Ok::<(), anyhow::Error>(())
                });

                let (socket, _) =
                    connect_async_with_config(format!("ws://{address}/multiplayer"), None, true)
                        .await?;
                let (mut write, mut read) = socket.split();
                let leave =
                    command_message(1, taiko_multiplayer_protocol::ClientCommand::LeaveRoom);
                flush_graceful_shutdown(&mut write, &mut read, &[leave])
                    .await
                    .map_err(|error| anyhow!("graceful shutdown failed: {error}"))?;
                server.await.context("test server task panicked")??;
                Ok::<(), anyhow::Error>(())
            })
            .expect("graceful LeaveRoom retry");
    }

    #[test]
    fn real_welcome_flapping_stops_exactly_at_total_attempt_limit() {
        const MAX_ATTEMPTS: u32 = 3;
        let (address_tx, address_rx) = std_mpsc::sync_channel(1);
        let (attempts_tx, attempts_rx) = std_mpsc::sync_channel(1);
        let server = thread::spawn(move || -> Result<()> {
            let runtime = Builder::new_current_thread().enable_all().build()?;
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                address_tx.send(listener.local_addr()?)?;
                let mut accepted = 0_u32;
                while accepted < MAX_ATTEMPTS {
                    let (stream, _) = listener.accept().await?;
                    accepted += 1;
                    let mut socket = tokio_tungstenite::accept_async(stream).await?;
                    assert!(matches!(
                        receive_test_client_message(&mut socket).await?,
                        ClientMessage::Hello(_)
                    ));
                    send_test_server_message(&mut socket, test_welcome()).await?;
                    socket.send(WsMessage::Close(None)).await?;
                }
                if tokio::time::timeout(Duration::from_millis(300), listener.accept())
                    .await
                    .is_ok()
                {
                    accepted += 1;
                }
                attempts_tx.send(accepted)?;
                Ok(())
            })
        });

        let address = address_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("server address");
        let mut config =
            OnlineClientConfig::create(&format!("http://{address}"), "alice").expect("config");
        config.reconnect = ReconnectPolicy {
            initial_delay: Duration::ZERO,
            maximum_delay: Duration::ZERO,
            maximum_attempts: MAX_ATTEMPTS,
        };
        let client = NetworkClient::connect(&config).expect("network client");
        let deadline = Instant::now() + Duration::from_secs(3);
        let terminal = loop {
            if let Err(error) = client.try_recv() {
                break error;
            }
            assert!(Instant::now() < deadline, "terminal fault timed out");
            thread::yield_now();
        };
        assert!(
            terminal
                .to_string()
                .contains("budget exhausted after 3 attempts"),
            "unexpected terminal fault: {terminal}"
        );
        assert_eq!(
            attempts_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("attempt count"),
            MAX_ATTEMPTS
        );
        server
            .join()
            .expect("server thread")
            .expect("server completed");
    }

    #[test]
    fn two_headless_players_complete_a_real_production_loopback_match() -> Result<()> {
        use crate::online_session::OnlinePhase;

        let fixture = HeadlessE2eSongFixture::create()?;
        let server = HeadlessE2eServer::start(fixture.path.clone())?;
        let mut host = HeadlessOnlineClient::connect(
            OnlineClientConfig::create(server.base_url.as_str(), "host")?,
            true,
        )?;
        assert!(matches!(
            host.resources.player_backend().map(Arc::as_ref),
            Some(ResourceBackend::Remote(_))
        ));
        assert_eq!(
            host.resources.songs().len(),
            1,
            "fixture exposes exactly one song"
        );
        assert_eq!(
            host.resources.songs()[0].courses.len(),
            2,
            "fixture exposes two independently selectable courses"
        );
        wait_for_headless_client(
            &mut host,
            "host room creation",
            HEADLESS_E2E_STEP_TIMEOUT,
            |client| {
                client.domain.phase() == OnlinePhase::Lobby
                    && client
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 1)
            },
        )?;

        let invite = host
            .domain
            .invite()
            .context("host did not receive a production invite")?;
        let copied_invite = invite.to_string();
        let invite = MultiplayerInvite::parse(&copied_invite)
            .context("production invite did not survive copy/paste parsing")?;
        let guest_config = OnlineClientConfig::join(
            invite.server().as_str(),
            "guest",
            invite.room_code().as_str(),
            invite.invitation_token().expose(),
            JoinRole::Player,
        )?;
        let mut guest = HeadlessOnlineClient::connect(guest_config, true)?;
        assert!(matches!(
            guest.resources.player_backend().map(Arc::as_ref),
            Some(ResourceBackend::Remote(_))
        ));
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "two-player room convergence",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::Lobby
                    && guest.domain.phase() == OnlinePhase::Lobby
                    && host
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 2)
                    && guest
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 2)
            },
        )?;
        assert!(host.domain.is_local_leader());
        assert!(!guest.domain.is_local_leader());

        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "authoritative song selection",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::SelectingCourse
                    && guest.domain.phase() == OnlinePhase::SelectingCourse
                    && host.domain.current_match_id().is_some()
                    && host.domain.current_match_id() == guest.domain.current_match_id()
            },
        )?;

        host.course_index = 0;
        guest.course_index = 1;
        let host_selection = host.highlighted_course_selection()?;
        let guest_selection = guest.highlighted_course_selection()?;
        assert_ne!(
            host_selection.course_id, guest_selection.course_id,
            "the two production clients must exercise independent course assignment"
        );
        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        assert!(guest.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);

        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "verified chart/audio preparation and clock readiness",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.is_local_ready()
                    && guest.domain.is_local_ready()
                    && host.domain.clock_is_ready()
                    && guest.domain.clock_is_ready()
                    && host.domain.prepared_match.is_some()
                    && guest.domain.prepared_match.is_some()
                    && host.domain.local_player.is_some()
                    && guest.domain.local_player.is_some()
            },
        )?;

        let match_id = host
            .domain
            .current_match_id()
            .context("prepared room lost its match id")?;
        for (label, client, selection) in [
            ("host", &host, host_selection),
            ("guest", &guest, guest_selection),
        ] {
            let prepared = client
                .domain
                .prepared_match
                .as_ref()
                .with_context(|| format!("{label} never decoded production audio"))?;
            assert_eq!(prepared.match_id, match_id);
            assert_eq!(prepared.selection, selection);
            let runtime = client
                .domain
                .local_player
                .as_ref()
                .with_context(|| format!("{label} never built the production chart runtime"))?;
            assert_eq!(runtime.match_id, match_id);
        }
        let ready_snapshot = host
            .domain
            .snapshot
            .as_ref()
            .context("host is missing the authoritative ready snapshot")?;
        assert!(matches!(
            ready_snapshot.stage,
            taiko_multiplayer_protocol::RoomStage::Preparing { .. }
        ));
        assert!(ready_snapshot.players.iter().all(|player| matches!(
            player.preparation,
            taiko_multiplayer_protocol::PlayerPreparation::Ready { .. }
        )));
        assert!(host.domain.can_start_match());
        assert!(!guest.domain.can_start_match());

        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "both production clients entering Playing",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::Playing
                    && guest.domain.phase() == OnlinePhase::Playing
            },
        )?;
        drive_headless_pair_inputs_to_results(&mut host, &mut guest)?;
        assert_eq!(
            host.domain.final_results, guest.domain.final_results,
            "both production replicas must converge on identical final results"
        );
        assert_eq!(host.domain.final_results.len(), 2);
        let mut result_courses = host
            .domain
            .final_results
            .values()
            .map(|result| result.course_id)
            .collect::<Vec<_>>();
        result_courses.sort_unstable();
        let mut selected_courses = vec![host_selection.course_id, guest_selection.course_id];
        selected_courses.sort_unstable();
        assert_eq!(result_courses, selected_courses);
        for result in host.domain.final_results.values() {
            assert!(!result.dnf, "the production runtime must finish naturally");
            assert_eq!(
                result.score.great + result.score.ok,
                1,
                "each production WebSocket input must produce one authoritative hit"
            );
            assert_eq!(
                result.score.miss, 0,
                "the correctly typed production input must prevent the natural miss"
            );
            assert!(result.finish_tick > 0);
        }

        guest
            .shutdown()
            .context("guest graceful LeaveRoom failed")?;
        host.shutdown().context("host graceful LeaveRoom failed")?;
        server.shutdown_and_wait()
    }

    #[test]
    fn production_ack_loss_resume_is_exactly_once_and_fences_old_transports() -> Result<()> {
        use crate::online_session::OnlinePhase;

        const ROLL_INPUT_TICK: Tick = 1_400_000;
        const ROLL_END_TICK: Tick = 7_900_000;

        let fixture = HeadlessE2eSongFixture::create_ack_loss()?;
        let server = HeadlessE2eServer::start(fixture.path.clone())?;
        let proxy = AckLossProxy::start(server.base_url.as_str())?;
        let mut host = HeadlessOnlineClient::connect(
            OnlineClientConfig::create(proxy.base_url(), "host")?,
            true,
        )?;
        wait_for_headless_client(
            &mut host,
            "ACK-loss host room creation",
            HEADLESS_E2E_STEP_TIMEOUT,
            |client| {
                client.domain.phase() == OnlinePhase::Lobby
                    && client
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 1)
            },
        )?;

        let invite = host
            .domain
            .invite()
            .context("ACK-loss host did not receive an invite")?;
        let copied_invite = invite.to_string();
        let invite = MultiplayerInvite::parse(&copied_invite)
            .context("ACK-loss invite did not survive copy/paste parsing")?;
        let guest_config = OnlineClientConfig::join(
            invite.server().as_str(),
            "guest",
            invite.room_code().as_str(),
            invite.invitation_token().expose(),
            JoinRole::Player,
        )?;
        let mut guest = HeadlessOnlineClient::connect(guest_config, true)?;
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "ACK-loss two-player room and clock convergence",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::Lobby
                    && guest.domain.phase() == OnlinePhase::Lobby
                    && host.domain.clock_is_ready()
                    && guest.domain.clock_is_ready()
                    && host
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 2)
                    && guest
                        .domain
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.players.len() == 2)
            },
        )?;

        let host_actor = host
            .domain
            .actor_id()
            .cloned()
            .context("ACK-loss host has no production actor identity")?;
        let revision_before = host
            .domain
            .snapshot
            .as_ref()
            .context("ACK-loss host has no lobby snapshot")?
            .revision;
        assert_eq!(host.domain.pending_command_count(), 0);
        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);

        let deadline = Instant::now() + HEADLESS_E2E_STEP_TIMEOUT;
        let mut saw_reconnecting_with_reset_clock = false;
        loop {
            host.tick().context("ACK-loss host tick failed")?;
            guest.tick().context("ACK-loss guest tick failed")?;
            ensure_headless_client_healthy(&host, "ACK-loss host")?;
            ensure_headless_client_healthy(&guest, "ACK-loss guest")?;
            let evidence = proxy.evidence()?;
            if evidence.command_ack_dropped && host.domain.phase() == OnlinePhase::Reconnecting {
                assert!(
                    !host.domain.clock_is_ready(),
                    "the resumed transport must not inherit clock evidence"
                );
                saw_reconnecting_with_reset_clock = true;
            }
            if host.domain.phase() == OnlinePhase::SelectingCourse
                && guest.domain.phase() == OnlinePhase::SelectingCourse
                && evidence.command_ack_dropped
                && evidence.command_resume_observed
                && evidence.command_old_transport_fenced
            {
                break;
            }
            if Instant::now() >= deadline {
                bail!(
                    "production command ACK-loss resume timed out; evidence={evidence:?}; \
                     host [{}]; guest [{}]",
                    host.summary_lines().join(" | "),
                    guest.summary_lines().join(" | "),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            saw_reconnecting_with_reset_clock,
            "the production client never exposed reconnecting state with reset clock evidence"
        );
        assert_eq!(host.domain.actor_id(), Some(&host_actor));
        assert_eq!(
            host.domain
                .snapshot
                .as_ref()
                .context("resumed host has no authoritative snapshot")?
                .revision,
            taiko_multiplayer_protocol::RoomRevision(revision_before.0 + 1),
            "SelectSong must advance the authoritative room revision exactly once"
        );
        assert_eq!(
            host.domain.pending_command_count(),
            0,
            "resumed Welcome must reconcile the applied command whose ACK was lost"
        );
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "post-resume clock evidence",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, _| host.domain.clock_is_ready(),
        )?;

        host.course_index = 0;
        guest.course_index = 1;
        let host_selection = host.highlighted_course_selection()?;
        let guest_selection = guest.highlighted_course_selection()?;
        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        assert!(guest.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "ACK-loss production preparation",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.is_local_ready()
                    && guest.domain.is_local_ready()
                    && host.domain.clock_is_ready()
                    && guest.domain.clock_is_ready()
                    && host.domain.prepared_match.is_some()
                    && guest.domain.prepared_match.is_some()
                    && host.domain.local_player.is_some()
                    && guest.domain.local_player.is_some()
            },
        )?;
        let host_player_id = host
            .domain
            .local_player_id()
            .context("ACK-loss host has no player identity")?;
        let guest_player_id = guest
            .domain
            .local_player_id()
            .context("ACK-loss guest has no player identity")?;

        assert!(host.handle_key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))?);
        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "ACK-loss clients entering Playing",
            HEADLESS_E2E_STEP_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::Playing
                    && guest.domain.phase() == OnlinePhase::Playing
            },
        )?;
        let playing_revision = host
            .domain
            .snapshot
            .as_ref()
            .context("ACK-loss host has no Playing snapshot")?
            .revision;

        let deadline = Instant::now() + HEADLESS_E2E_MATCH_TIMEOUT;
        let normal_send_at = HEADLESS_E2E_NOTE_TICK.saturating_sub(HEADLESS_E2E_INPUT_LEAD_TICKS);
        let mut host_note_submitted = false;
        let mut guest_note_submitted = false;
        let mut roll_input_submitted = false;
        let mut saw_input_reconnecting_with_reset_clock = false;
        loop {
            host.tick().context("input ACK-loss host tick failed")?;
            guest.tick().context("input ACK-loss guest tick failed")?;
            ensure_headless_client_healthy(&host, "input ACK-loss host")?;
            ensure_headless_client_healthy(&guest, "input ACK-loss guest")?;

            if !host_note_submitted
                && host.domain.phase() == OnlinePhase::Playing
                && host.domain.estimated_server_tick() >= normal_send_at
            {
                assert!(host.handle_key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('s'),
                    KeyModifiers::NONE,
                ))?);
                host_note_submitted = true;
            }
            if !guest_note_submitted
                && guest.domain.phase() == OnlinePhase::Playing
                && guest.domain.estimated_server_tick() >= normal_send_at
            {
                assert!(guest.handle_key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('a'),
                    KeyModifiers::NONE,
                ))?);
                guest_note_submitted = true;
            }

            let host_tick = host.domain.estimated_server_tick();
            if host_note_submitted
                && guest_note_submitted
                && !roll_input_submitted
                && host.domain.pending_input_count() == 0
                && guest.domain.pending_input_count() == 0
                && host_tick >= ROLL_INPUT_TICK
            {
                if host_tick >= ROLL_END_TICK {
                    bail!(
                        "production input ACK-loss was not armed before the drumroll ended \
                         (tick={host_tick})"
                    );
                }
                proxy.arm_input_ack_loss()?;
                assert!(host.handle_key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('s'),
                    KeyModifiers::NONE,
                ))?);
                assert_eq!(
                    host.domain.pending_input_count(),
                    1,
                    "the faulted production input must remain pending until authority evidence"
                );
                roll_input_submitted = true;
            }

            let evidence = proxy.evidence()?;
            if evidence.input_ack_dropped && host.domain.phase() == OnlinePhase::Reconnecting {
                assert!(
                    !host.domain.clock_is_ready(),
                    "input reconnect must discard clock evidence from the old transport"
                );
                saw_input_reconnecting_with_reset_clock = true;
            }
            if roll_input_submitted
                && evidence.input_ack_dropped
                && evidence.input_resume_observed
                && evidence.input_replay_observed
                && evidence.input_replay_acknowledged
                && evidence.input_old_transport_fenced
                && host.domain.pending_input_count() == 0
                && host.domain.clock_is_ready()
                && host.domain.phase() == OnlinePhase::Playing
            {
                break;
            }
            if host.domain.phase() == OnlinePhase::Results
                || guest.domain.phase() == OnlinePhase::Results
            {
                bail!(
                    "match finished before production input ACK-loss recovery completed; \
                     evidence={evidence:?}"
                );
            }
            if Instant::now() >= deadline {
                bail!(
                    "production input ACK-loss resume timed out; evidence={evidence:?}; \
                     host_note={host_note_submitted}, guest_note={guest_note_submitted}, \
                     roll={roll_input_submitted}; host [{}]; guest [{}]",
                    host.summary_lines().join(" | "),
                    guest.summary_lines().join(" | "),
                );
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            saw_input_reconnecting_with_reset_clock,
            "the input fault never exposed reconnecting state with reset clock evidence"
        );
        assert_eq!(host.domain.actor_id(), Some(&host_actor));
        assert_eq!(
            host.domain
                .snapshot
                .as_ref()
                .context("resumed Playing host has no snapshot")?
                .revision,
            playing_revision,
            "input replay and live-session replacement must not mutate room revision"
        );
        assert_eq!(host.domain.pending_input_count(), 0);

        wait_for_headless_pair(
            &mut host,
            &mut guest,
            "ACK-loss authoritative results",
            HEADLESS_E2E_MATCH_TIMEOUT,
            |host, guest| {
                host.domain.phase() == OnlinePhase::Results
                    && guest.domain.phase() == OnlinePhase::Results
            },
        )?;
        assert_eq!(host.domain.final_results, guest.domain.final_results);
        let host_result = host
            .domain
            .final_results
            .get(&host_player_id)
            .context("ACK-loss host result is missing")?;
        assert_eq!(host_result.course_id, host_selection.course_id);
        assert_eq!(host_result.score.great + host_result.score.ok, 1);
        assert_eq!(host_result.score.miss, 0);
        assert_eq!(
            host_result.score.roll_hits, 1,
            "the authority must score the ACK-lost and replayed roll input exactly once"
        );
        let guest_result = host
            .domain
            .final_results
            .get(&guest_player_id)
            .context("ACK-loss guest result is missing")?;
        assert_eq!(guest_result.course_id, guest_selection.course_id);
        assert_eq!(guest_result.score.great + guest_result.score.ok, 1);
        assert_eq!(guest_result.score.miss, 0);
        assert_eq!(guest_result.score.roll_hits, 0);

        guest
            .shutdown()
            .context("ACK-loss guest graceful LeaveRoom failed")?;
        host.shutdown()
            .context("ACK-loss host graceful LeaveRoom failed")?;
        proxy.shutdown_and_wait()?;
        server.shutdown_and_wait()
    }

    #[test]
    fn live_state_slot_coalesces_to_latest_value() {
        let (client, peer) = NetworkClient::test_pair();
        let live = |sequence| LiveStateSnapshot {
            match_id: taiko_multiplayer_protocol::MatchId(1),
            state_seq: taiko_multiplayer_protocol::StateSeq(sequence),
            server_tick: sequence as i64,
            players: taiko_multiplayer_protocol::BoundedVec::default(),
        };
        peer.set_live(live(1));
        peer.set_live(live(2));
        assert_eq!(
            client
                .take_latest_live()
                .expect("latest live state")
                .state_seq,
            taiko_multiplayer_protocol::StateSeq(2)
        );
        assert!(client.take_latest_live().is_none());
    }

    fn headless_score_snapshot() -> RoomSnapshot {
        RoomSnapshot {
            room_code: RoomCode::parse("ABCD").expect("room code"),
            revision: taiko_multiplayer_protocol::RoomRevision(1),
            server_now_us: 10,
            leader_player_id: PlayerId(1),
            players: vec![taiko_multiplayer_protocol::PlayerSnapshot {
                player_id: PlayerId(1),
                name: DisplayName::new("alice").expect("display name"),
                is_leader: true,
                connection: taiko_multiplayer_protocol::PlayerConnection::Online,
                preparation: taiko_multiplayer_protocol::PlayerPreparation::Selecting,
                last_acked_input_seq: None,
            }]
            .try_into()
            .expect("bounded players"),
            spectators: taiko_multiplayer_protocol::BoundedVec::default(),
            stage: taiko_multiplayer_protocol::RoomStage::Lobby,
        }
    }

    fn headless_score() -> taiko_multiplayer_protocol::ScoreSnapshot {
        taiko_multiplayer_protocol::ScoreSnapshot {
            score: 12_340,
            combo: 7,
            max_combo: 8,
            gauge_ppm: 700_000,
            pass_threshold_ppm: 600_000,
            great: 5,
            ok: 2,
            miss: 1,
            roll_hits: 3,
        }
    }

    #[test]
    fn headless_live_summary_is_role_independent_and_changes_for_every_accepted_epoch() {
        let snapshot = headless_score_snapshot();
        let mut live_states = HashMap::from([(
            PlayerId(1),
            PlayerLiveState {
                player_id: PlayerId(1),
                score: headless_score(),
                finished: false,
                dnf: false,
            },
        )]);
        let first = headless_score_lines(
            Some(&snapshot),
            Some((MatchId(4), StateSeq(9))),
            &live_states,
            &HashMap::new(),
        );
        assert!(first
            .iter()
            .any(|line| line == "Live state: match=4 sequence=9"));
        assert!(first.iter().any(|line| {
            line.contains("Live alice (P1): score=12340")
                && line.contains("gauge_ppm=700000")
                && line.contains("roll_hits=3")
                && line.ends_with("status=playing")
        }));

        live_states
            .get_mut(&PlayerId(1))
            .expect("live player")
            .score
            .combo = 8;
        let second = headless_score_lines(
            Some(&snapshot),
            Some((MatchId(4), StateSeq(10))),
            &live_states,
            &HashMap::new(),
        );
        assert_ne!(
            first, second,
            "the headless output diff must observe each accepted live-state epoch"
        );
    }

    #[test]
    fn headless_final_summary_replaces_live_score_with_authoritative_result() {
        let snapshot = headless_score_snapshot();
        let live_states = HashMap::from([(
            PlayerId(1),
            PlayerLiveState {
                player_id: PlayerId(1),
                score: headless_score(),
                finished: false,
                dnf: false,
            },
        )]);
        let final_results = HashMap::from([(
            PlayerId(1),
            FinalResult {
                player_id: PlayerId(1),
                course_id: CourseId(3),
                score: headless_score(),
                finish_tick: 5_000,
                passed: true,
                replay_digest: ContentHash::parse("a".repeat(64)).expect("replay digest"),
                dnf: false,
            },
        )]);

        let lines = headless_score_lines(
            Some(&snapshot),
            Some((MatchId(4), StateSeq(10))),
            &live_states,
            &final_results,
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Result alice (P1): PASS course=3 score=12340"));
        assert!(lines[0].contains("finish_tick=5000"));
        assert!(lines[0].contains(&"a".repeat(64)));
    }

    #[test]
    fn headless_selection_wraps_in_both_directions() {
        assert_eq!(wrapped_index(0, 4, -1), 3);
        assert_eq!(wrapped_index(3, 4, 1), 0);
        assert_eq!(wrapped_index(2, 4, -1), 1);
        assert_eq!(wrapped_index(2, 0, 1), 0);
    }

    #[test]
    fn headless_course_confirm_applies_changed_selection_in_every_preparation_phase() {
        use crate::online_session::OnlinePhase;
        let old = PlayerSelection {
            course_id: CourseId(1),
        };
        let highlighted = PlayerSelection {
            course_id: CourseId(2),
        };
        for phase in [
            OnlinePhase::SelectingCourse,
            OnlinePhase::Downloading,
            OnlinePhase::Verifying,
            OnlinePhase::Loading,
            OnlinePhase::Prepared,
            OnlinePhase::Ready,
        ] {
            assert_eq!(
                headless_course_confirm_action(
                    phase,
                    Some(RoomRole::Player),
                    highlighted,
                    Some(old),
                    true,
                    false,
                ),
                HeadlessCourseConfirmAction::SelectCourse,
                "changed selection must win over start in {phase:?}"
            );
        }
        assert_eq!(
            headless_course_confirm_action(
                OnlinePhase::Prepared,
                Some(RoomRole::Player),
                highlighted,
                Some(highlighted),
                true,
                false,
            ),
            HeadlessCourseConfirmAction::StartMatch
        );
        assert_eq!(
            headless_course_confirm_action(
                OnlinePhase::Ready,
                Some(RoomRole::Player),
                highlighted,
                Some(highlighted),
                false,
                false,
            ),
            HeadlessCourseConfirmAction::None,
            "a same-selection guest must not issue a command"
        );
        assert_eq!(
            headless_course_confirm_action(
                OnlinePhase::Prepared,
                Some(RoomRole::Spectator),
                highlighted,
                Some(old),
                true,
                false,
            ),
            HeadlessCourseConfirmAction::None
        );
        assert_eq!(
            headless_course_confirm_action(
                OnlinePhase::SelectingCourse,
                Some(RoomRole::Player),
                highlighted,
                Some(highlighted),
                false,
                true,
            ),
            HeadlessCourseConfirmAction::SelectCourse,
            "Enter must explicitly retry a failed same-selection preparation"
        );
    }

    #[test]
    fn headless_ready_key_retries_a_latched_preparation_failure() {
        assert_eq!(
            headless_ready_action(Some(RoomRole::Player), true, false, false, false),
            HeadlessReadyAction::RetryPreparation,
            "R must retry even though no prepared content exists yet"
        );
        assert_eq!(
            headless_ready_action(Some(RoomRole::Player), true, true, true, true),
            HeadlessReadyAction::RetryPreparation,
            "a latched failure takes priority over ready toggling"
        );
        assert_eq!(
            headless_ready_action(Some(RoomRole::Player), false, true, false, true),
            HeadlessReadyAction::SetUnready
        );
        assert_eq!(
            headless_ready_action(Some(RoomRole::Player), false, false, false, true),
            HeadlessReadyAction::SetReady
        );
        assert_eq!(
            headless_ready_action(Some(RoomRole::Spectator), true, false, false, false),
            HeadlessReadyAction::None
        );
    }
}
