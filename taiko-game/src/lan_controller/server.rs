use std::future::{Future, IntoFuture};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskContext, Poll};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use axum::extract::connect_info::Connected;
use axum::extract::ws::{
    close_code, CloseFrame, Message as WsMessage, Utf8Bytes, WebSocket, WebSocketUpgrade,
};
use axum::extract::{ConnectInfo, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::serve::{IncomingStream, Listener};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch, OwnedSemaphorePermit, Semaphore};

use crate::controller::{ControllerSlot, ControllerSource, ControllerStrike};

use super::protocol::{ClientMessage, ErrorCode, ServerMessage, WireSlot, PROTOCOL_VERSION};
use super::{
    ActiveControllerSlots, ControllerSlotStatus, LanControllerConfig, LanControllers,
    PER_SLOT_QUEUE_CAPACITY,
};

pub(super) const WEBSOCKET_SUBPROTOCOL: &str = "taiko-controller-v1";

const MAX_WIRE_MESSAGE_BYTES: usize = 512;
const MAX_CONTROLLER_CONNECTIONS: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_HEADER_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(12);
const FORCED_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(750);
const PENDING_SESSION_TTL: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_millis(500);
const PING_INTERVAL: Duration = Duration::from_secs(2);
const CONNECTION_LEASE: Duration = Duration::from_secs(10);
const MESSAGE_RATE_PER_SECOND: u64 = 64;
const MESSAGE_BURST: u64 = 32;
const RATE_CREDIT_SCALE: u64 = 1_000_000;
const TOKEN_BYTES: usize = 32;
const TOKEN_HEX_BYTES: usize = TOKEN_BYTES * 2;

const CONTROLLER_HTML: &str = include_str!("controller.html");
const CONTROLLER_CSS: &str = include_str!("controller.css");
const CONTROLLER_JS: &str = include_str!("controller.js");

#[derive(Clone)]
pub(super) struct ServerControl {
    registry: Arc<Mutex<RegistryState>>,
    active_slots: Arc<AtomicU8>,
    in_flight_frames: Arc<AtomicUsize>,
    completed_frames: Arc<AtomicU64>,
    generation: u64,
}

impl ServerControl {
    fn new(generation: u64) -> Result<Self> {
        Ok(Self {
            registry: Arc::new(Mutex::new(RegistryState::new()?)),
            active_slots: Arc::new(AtomicU8::new(ActiveControllerSlots::NONE.bits())),
            in_flight_frames: Arc::new(AtomicUsize::new(0)),
            completed_frames: Arc::new(AtomicU64::new(0)),
            generation,
        })
    }

    pub(super) fn pairing_token(&self, slot: ControllerSlot) -> Option<String> {
        self.registry
            .lock()
            .expect("LAN controller registry mutex poisoned")
            .slots[slot.index()]
        .pairing_token
        .as_ref()
        .map(SecretToken::expose)
    }

    pub(super) fn rotate_pairing(&self, slot: ControllerSlot) -> Result<String> {
        let token = SecretToken::generate().context("failed to generate LAN pairing token")?;
        let exposed = token.expose();
        let mut registry = self
            .registry
            .lock()
            .expect("LAN controller registry mutex poisoned");
        registry.ensure_token_is_unique(&token)?;
        registry.slots[slot.index()].revoke(ConnectionCloseReason::Revoked);
        registry.slots[slot.index()].pairing_token = Some(token);
        Ok(exposed)
    }

    pub(super) fn set_active_slots(&self, slots: ActiveControllerSlots) {
        // Admission and gate changes share the registry mutex. Once this call
        // returns, every strike admitted under the previous gate is already in
        // its bounded queue, and every later admission observes the new gate.
        let _registry = self
            .registry
            .lock()
            .expect("LAN controller registry mutex poisoned");
        self.active_slots.store(slots.bits(), Ordering::Release);
    }

    pub(super) fn status(&self) -> [ControllerSlotStatus; 2] {
        let registry = self
            .registry
            .lock()
            .expect("LAN controller registry mutex poisoned");
        std::array::from_fn(|index| registry.slots[index].status())
    }

    pub(super) fn record_rejected(&self, slot: ControllerSlot) {
        let mut registry = self
            .registry
            .lock()
            .expect("LAN controller registry mutex poisoned");
        registry.slots[slot.index()].rejected_hits =
            registry.slots[slot.index()].rejected_hits.saturating_add(1);
    }

    pub(super) fn stop(&self) {
        self.set_active_slots(ActiveControllerSlots::NONE);
        let mut registry = self
            .registry
            .lock()
            .expect("LAN controller registry mutex poisoned");
        for slot in &mut registry.slots {
            slot.revoke(ConnectionCloseReason::ServerStopping);
            slot.pairing_token = None;
        }
    }

    pub(super) fn is_active(&self, slot: ControllerSlot) -> bool {
        let slots = ActiveControllerSlots::from_bits(self.active_slots.load(Ordering::Acquire));
        slots.contains(slot)
    }

    pub(super) fn ingress_snapshot(&self) -> FrameIngressSnapshot {
        // The order is part of the linearization contract. A frame that starts
        // after the in-flight load is newer than this snapshot. A frame that
        // finishes between the two loads changes `completed_frames`, forcing
        // the application to drain again before dispatching queued inputs.
        let in_flight = self.in_flight_frames.load(Ordering::SeqCst);
        let completed = self.completed_frames.load(Ordering::SeqCst);
        FrameIngressSnapshot {
            in_flight,
            completed,
        }
    }

    pub(super) fn begin_frame_ingress(&self) -> FrameIngressGuard {
        FrameIngressGuard::new(
            Arc::clone(&self.in_flight_frames),
            Arc::clone(&self.completed_frames),
        )
    }

    pub(super) fn authorize_dispatch(
        &self,
        strike: &ControllerStrike,
        expected_slot: ControllerSlot,
        dispatch_slots: ActiveControllerSlots,
        now: Instant,
        maximum_age: Duration,
    ) -> bool {
        let Ok(mut registry) = self.registry.lock() else {
            return false;
        };
        let record = &mut registry.slots[expected_slot.index()];
        let ControllerSource::Lan { .. } = strike.source else {
            record.rejected_hits = record.rejected_hits.saturating_add(1);
            return false;
        };
        let generation_matches = strike.generation == Some(self.generation);
        let slot_matches = strike.slot == expected_slot;
        let active = dispatch_slots.contains(expected_slot);
        let fresh = now.saturating_duration_since(strike.observed_at) <= maximum_age;
        // Connection fencing is an admission rule. Once `admit_strike`
        // commits a hit to the bounded queue and the server acknowledges it,
        // a later resume must not revoke that accepted hit before dispatch.
        if generation_matches && slot_matches && active && fresh {
            record.accepted_hits = record.accepted_hits.saturating_add(1);
            true
        } else {
            record.rejected_hits = record.rejected_hits.saturating_add(1);
            false
        }
    }

    fn admit_frame(
        &self,
        slot: ControllerSlot,
        connection_id: u64,
        received_at: Instant,
    ) -> MessageAdmission {
        let Ok(mut registry) = self.registry.lock() else {
            return MessageAdmission::ServerStopping;
        };
        let slot_record = &mut registry.slots[slot.index()];
        if slot_record
            .connection
            .as_ref()
            .is_none_or(|connection| connection.id != connection_id)
        {
            slot_record.rejected_hits = slot_record.rejected_hits.saturating_add(1);
            return MessageAdmission::Fenced;
        }
        let Some(rate_limiter) = slot_record.rate_limiter.as_mut() else {
            return MessageAdmission::ServerStopping;
        };
        if rate_limiter.allow(received_at) {
            MessageAdmission::Accepted
        } else {
            slot_record.rejected_hits = slot_record.rejected_hits.saturating_add(1);
            MessageAdmission::RateLimited
        }
    }

    fn admit_strike(
        &self,
        slot: ControllerSlot,
        connection_id: u64,
        sender: &SyncSender<ControllerStrike>,
        strike: ControllerStrike,
    ) -> StrikeAdmission {
        let Ok(mut registry) = self.registry.lock() else {
            return StrikeAdmission::ServerStopping;
        };
        let slot_record = &mut registry.slots[slot.index()];
        if slot_record
            .connection
            .as_ref()
            .is_none_or(|connection| connection.id != connection_id)
        {
            slot_record.rejected_hits = slot_record.rejected_hits.saturating_add(1);
            return StrikeAdmission::Fenced;
        }
        if !self.is_active(slot) {
            slot_record.rejected_hits = slot_record.rejected_hits.saturating_add(1);
            return StrikeAdmission::Inactive;
        }
        match sender.try_send(strike) {
            Ok(()) => StrikeAdmission::Accepted,
            Err(TrySendError::Full(_)) => {
                slot_record.rejected_hits = slot_record.rejected_hits.saturating_add(1);
                StrikeAdmission::QueueFull
            }
            Err(TrySendError::Disconnected(_)) => StrikeAdmission::ServerStopping,
        }
    }

    fn authenticate_pair(&self, candidate: &str) -> Result<AuthSession, AuthenticationError> {
        let candidate =
            SecretToken::parse(candidate).map_err(|_| AuthenticationError::Unauthorized)?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| AuthenticationError::Internal)?;

        let matches = registry.slots.each_ref().map(|slot| {
            slot.pairing_token
                .as_ref()
                .is_some_and(|token| token == &candidate)
        });
        let slot_index = match matches {
            [true, false] => 0,
            [false, true] => 1,
            _ => return Err(AuthenticationError::Unauthorized),
        };
        let now = Instant::now();
        if registry.slots[slot_index].pending_session_expired(now) {
            registry.slots[slot_index].clear_pending_session(ConnectionCloseReason::Revoked);
        }
        if registry.slots[slot_index].session_token.is_none() {
            let session_token =
                SecretToken::generate().map_err(|_| AuthenticationError::Internal)?;
            registry
                .ensure_token_is_unique(&session_token)
                .map_err(|_| AuthenticationError::Internal)?;
            let record = &mut registry.slots[slot_index];
            record.session_token = Some(session_token);
            record.session_committed = false;
            record.pending_since = Some(now);
            record.rate_limiter = Some(MessageRateLimiter::new(now));
        }

        let slot = ControllerSlot::ALL[slot_index];
        let session_token = registry.slots[slot_index]
            .session_token
            .as_ref()
            .expect("pairing initialized a pending session")
            .expose();
        let (connection_id, close_rx) = registry.install_connection(slot)?;
        Ok(AuthSession {
            slot,
            connection_id,
            session_token: Some(session_token),
            commit_required: true,
            close_rx,
        })
    }

    fn authenticate_resume(&self, candidate: &str) -> Result<AuthSession, AuthenticationError> {
        let candidate =
            SecretToken::parse(candidate).map_err(|_| AuthenticationError::Unauthorized)?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| AuthenticationError::Internal)?;
        let matches = registry.slots.each_ref().map(|slot| {
            slot.session_token
                .as_ref()
                .is_some_and(|token| token == &candidate)
        });
        let slot_index = match matches {
            [true, false] => 0,
            [false, true] => 1,
            _ => return Err(AuthenticationError::Unauthorized),
        };
        let now = Instant::now();
        if registry.slots[slot_index].pending_session_expired(now) {
            registry.slots[slot_index].clear_pending_session(ConnectionCloseReason::Revoked);
            return Err(AuthenticationError::Unauthorized);
        }
        let commit_required = !registry.slots[slot_index].session_committed;
        let slot = ControllerSlot::ALL[slot_index];
        let (connection_id, close_rx) = registry.install_connection(slot)?;
        Ok(AuthSession {
            slot,
            connection_id,
            session_token: None,
            commit_required,
            close_rx,
        })
    }

    fn commit_pairing(
        &self,
        slot: ControllerSlot,
        connection_id: u64,
    ) -> Result<(), AuthenticationError> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| AuthenticationError::Internal)?;
        let record = &mut registry.slots[slot.index()];
        if record
            .connection
            .as_ref()
            .is_none_or(|connection| connection.id != connection_id)
            || record.session_token.is_none()
            || record.pending_session_expired(Instant::now())
        {
            return Err(AuthenticationError::Unauthorized);
        }
        record.session_committed = true;
        record.pending_since = None;
        record.pairing_token = None;
        Ok(())
    }

    fn disconnect(&self, slot: ControllerSlot, connection_id: u64) {
        let Ok(mut registry) = self.registry.lock() else {
            return;
        };
        let slot = &mut registry.slots[slot.index()];
        if slot
            .connection
            .as_ref()
            .is_some_and(|connection| connection.id == connection_id)
        {
            slot.connection = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FrameIngressSnapshot {
    pub(super) in_flight: usize,
    pub(super) completed: u64,
}

pub(crate) struct FrameIngressGuard {
    counter: Arc<AtomicUsize>,
    completed: Arc<AtomicU64>,
}

impl FrameIngressGuard {
    fn new(counter: Arc<AtomicUsize>, completed: Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self { counter, completed }
    }
}

impl Drop for FrameIngressGuard {
    fn drop(&mut self) {
        finish_frame_ingress(&self.counter, &self.completed, || {});
    }
}

fn finish_frame_ingress(
    counter: &AtomicUsize,
    completed: &AtomicU64,
    between_publication_and_release: impl FnOnce(),
) {
    // Publish completion before releasing the in-flight marker. A snapshot
    // taken between these two operations must still observe `in_flight > 0`;
    // one taken after release must observe the changed completion epoch.
    completed.fetch_add(1, Ordering::SeqCst);
    between_publication_and_release();
    let previous = counter.fetch_sub(1, Ordering::SeqCst);
    debug_assert!(previous > 0, "frame ingress counter underflow");
}

#[derive(Clone)]
struct SecretToken([u8; TOKEN_BYTES]);

impl SecretToken {
    fn generate() -> Result<Self> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).context("operating-system randomness unavailable")?;
        Ok(Self(bytes))
    }

    fn parse(value: &str) -> Result<Self> {
        if value.len() != TOKEN_HEX_BYTES {
            bail!("token has an invalid length");
        }
        let mut bytes = [0_u8; TOKEN_BYTES];
        hex::decode_to_slice(value, &mut bytes).context("token is not lowercase hexadecimal")?;
        if hex::encode(bytes) != value {
            bail!("token is not canonical lowercase hexadecimal");
        }
        Ok(Self(bytes))
    }

    fn expose(&self) -> String {
        hex::encode(self.0)
    }
}

impl PartialEq for SecretToken {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.0.ct_eq(&other.0))
    }
}

impl Eq for SecretToken {}

impl std::fmt::Debug for SecretToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretToken([REDACTED])")
    }
}

struct RegistryState {
    slots: [SlotRecord; 2],
    next_connection_id: u64,
}

impl RegistryState {
    fn new() -> Result<Self> {
        let one = SecretToken::generate().context("failed to generate P1 pairing token")?;
        let two = SecretToken::generate().context("failed to generate P2 pairing token")?;
        if one == two {
            bail!("operating-system randomness returned duplicate LAN pairing tokens");
        }
        Ok(Self {
            slots: [SlotRecord::new(one), SlotRecord::new(two)],
            next_connection_id: 1,
        })
    }

    fn ensure_token_is_unique(&self, candidate: &SecretToken) -> Result<()> {
        let duplicate = self.slots.iter().any(|slot| {
            slot.pairing_token
                .as_ref()
                .is_some_and(|token| token == candidate)
                || slot
                    .session_token
                    .as_ref()
                    .is_some_and(|token| token == candidate)
        });
        if duplicate {
            bail!("operating-system randomness returned a duplicate LAN controller token");
        }
        Ok(())
    }

    fn install_connection(
        &mut self,
        slot: ControllerSlot,
    ) -> Result<(u64, watch::Receiver<Option<ConnectionCloseReason>>), AuthenticationError> {
        let connection_id = self.next_connection_id;
        self.next_connection_id = self
            .next_connection_id
            .checked_add(1)
            .ok_or(AuthenticationError::Internal)?;
        let (close_tx, close_rx) = watch::channel(None);
        if let Some(previous) = self.slots[slot.index()]
            .connection
            .replace(ActiveConnection {
                id: connection_id,
                close_tx,
            })
        {
            previous
                .close_tx
                .send_replace(Some(ConnectionCloseReason::Fenced));
        }
        Ok((connection_id, close_rx))
    }
}

struct SlotRecord {
    pairing_token: Option<SecretToken>,
    session_token: Option<SecretToken>,
    session_committed: bool,
    pending_since: Option<Instant>,
    connection: Option<ActiveConnection>,
    rate_limiter: Option<MessageRateLimiter>,
    accepted_hits: u64,
    rejected_hits: u64,
}

impl SlotRecord {
    fn new(pairing_token: SecretToken) -> Self {
        Self {
            pairing_token: Some(pairing_token),
            session_token: None,
            session_committed: false,
            pending_since: None,
            connection: None,
            rate_limiter: None,
            accepted_hits: 0,
            rejected_hits: 0,
        }
    }

    fn revoke(&mut self, reason: ConnectionCloseReason) {
        if let Some(connection) = self.connection.take() {
            connection.close_tx.send_replace(Some(reason));
        }
        self.session_token = None;
        self.session_committed = false;
        self.pending_since = None;
        self.pairing_token = None;
        self.rate_limiter = None;
    }

    fn pending_session_expired(&self, now: Instant) -> bool {
        !self.session_committed
            && self.session_token.is_some()
            && self
                .pending_since
                .is_none_or(|started| now.saturating_duration_since(started) > PENDING_SESSION_TTL)
    }

    fn clear_pending_session(&mut self, reason: ConnectionCloseReason) {
        if self.session_committed {
            return;
        }
        if let Some(connection) = self.connection.take() {
            connection.close_tx.send_replace(Some(reason));
        }
        self.session_token = None;
        self.pending_since = None;
        self.rate_limiter = None;
    }

    fn status(&self) -> ControllerSlotStatus {
        ControllerSlotStatus {
            paired: self.session_committed,
            connected: self.connection.is_some(),
            accepted_hits: self.accepted_hits,
            rejected_hits: self.rejected_hits,
        }
    }
}

struct ActiveConnection {
    id: u64,
    close_tx: watch::Sender<Option<ConnectionCloseReason>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionCloseReason {
    Fenced,
    Revoked,
    ServerStopping,
}

struct AuthSession {
    slot: ControllerSlot,
    connection_id: u64,
    session_token: Option<String>,
    commit_required: bool,
    close_rx: watch::Receiver<Option<ConnectionCloseReason>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthenticationError {
    Unauthorized,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrikeAdmission {
    Accepted,
    Inactive,
    Fenced,
    QueueFull,
    ServerStopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageAdmission {
    Accepted,
    Fenced,
    RateLimited,
    ServerStopping,
}

impl From<AuthenticationError> for anyhow::Error {
    fn from(value: AuthenticationError) -> Self {
        match value {
            AuthenticationError::Unauthorized => anyhow!("controller token is unauthorized"),
            AuthenticationError::Internal => anyhow!("controller authentication state failed"),
        }
    }
}

struct MessageRateLimiter {
    credit: u64,
    last_refill: Instant,
}

impl MessageRateLimiter {
    fn new(now: Instant) -> Self {
        Self {
            credit: MESSAGE_BURST.saturating_mul(RATE_CREDIT_SCALE),
            last_refill: now,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        let elapsed_us = u64::try_from(now.saturating_duration_since(self.last_refill).as_micros())
            .unwrap_or(u64::MAX);
        self.last_refill = now;
        let capacity = MESSAGE_BURST.saturating_mul(RATE_CREDIT_SCALE);
        self.credit = self
            .credit
            .saturating_add(elapsed_us.saturating_mul(MESSAGE_RATE_PER_SECOND))
            .min(capacity);
        if self.credit < RATE_CREDIT_SCALE {
            return false;
        }
        self.credit -= RATE_CREDIT_SCALE;
        true
    }
}

#[derive(Clone, Debug)]
struct ConnectionInfo {
    peer: SocketAddr,
    header_complete: Arc<AtomicBool>,
}

impl ConnectionInfo {
    fn mark_header_complete(&self) {
        self.header_complete.store(true, Ordering::Release);
    }
}

impl Connected<IncomingStream<'_, AdmissionListener>> for ConnectionInfo {
    fn connect_info(stream: IncomingStream<'_, AdmissionListener>) -> Self {
        stream.remote_addr().clone()
    }
}

struct AdmissionListener {
    listener: TcpListener,
    permits: Arc<Semaphore>,
}

impl AdmissionListener {
    fn new(listener: TcpListener) -> Self {
        Self {
            listener,
            permits: Arc::new(Semaphore::new(MAX_CONTROLLER_CONNECTIONS)),
        }
    }
}

impl Listener for AdmissionListener {
    type Io = AdmittedTcpStream;
    type Addr = ConnectionInfo;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let permit = Arc::clone(&self.permits)
                .acquire_owned()
                .await
                .expect("LAN connection admission semaphore cannot close");
            match self.listener.accept().await {
                Ok((stream, peer)) => {
                    let header_complete = Arc::new(AtomicBool::new(false));
                    return (
                        AdmittedTcpStream::new(stream, permit, Arc::clone(&header_complete)),
                        ConnectionInfo {
                            peer,
                            header_complete,
                        },
                    );
                }
                Err(error) if is_transient_accept_error(&error) => {}
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        let peer = self.listener.local_addr()?;
        Ok(ConnectionInfo {
            peer,
            header_complete: Arc::new(AtomicBool::new(true)),
        })
    }
}

fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

struct AdmittedTcpStream {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
    header_complete: Arc<AtomicBool>,
    header_deadline: Pin<Box<tokio::time::Sleep>>,
    idle_deadline: Pin<Box<tokio::time::Sleep>>,
}

impl AdmittedTcpStream {
    fn new(
        stream: TcpStream,
        permit: OwnedSemaphorePermit,
        header_complete: Arc<AtomicBool>,
    ) -> Self {
        Self {
            stream,
            _permit: permit,
            header_complete,
            header_deadline: Box::pin(tokio::time::sleep(HTTP_HEADER_TIMEOUT)),
            idle_deadline: Box::pin(tokio::time::sleep(CONNECTION_IDLE_TIMEOUT)),
        }
    }

    fn poll_deadlines(&mut self, context: &mut TaskContext<'_>) -> io::Result<()> {
        if !self.header_complete.load(Ordering::Acquire)
            && self.header_deadline.as_mut().poll(context).is_ready()
        {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "LAN controller HTTP header deadline exceeded",
            ));
        }
        if self.idle_deadline.as_mut().poll(context).is_ready() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "LAN controller connection idle deadline exceeded",
            ));
        }
        Ok(())
    }

    fn refresh_idle_deadline(&mut self) {
        self.idle_deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + CONNECTION_IDLE_TIMEOUT);
    }
}

impl AsyncRead for AdmittedTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Err(error) = self.poll_deadlines(context) {
            return Poll::Ready(Err(error));
        }
        let before = buffer.filled().len();
        match Pin::new(&mut self.stream).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                if buffer.filled().len() > before {
                    self.refresh_idle_deadline();
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncWrite for AdmittedTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.stream).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

#[derive(Clone)]
struct HttpState {
    authority: Arc<str>,
    origin: Arc<str>,
    control: ServerControl,
    input_senders: [SyncSender<ControllerStrike>; 2],
    shutdown: watch::Sender<bool>,
}

pub(super) fn start(config: LanControllerConfig) -> Result<LanControllers> {
    validate_bind_ip(config.bind_ip)?;
    let control = ServerControl::new(config.generation)?;
    let (p1_tx, p1_rx) = mpsc::sync_channel(PER_SLOT_QUEUE_CAPACITY);
    let (p2_tx, p2_rx) = mpsc::sync_channel(PER_SLOT_QUEUE_CAPACITY);
    let input_senders = [p1_tx, p2_tx];
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (shutdown_broadcast, _) = watch::channel(false);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let thread_control = control.clone();
    let thread_shutdown = shutdown_broadcast.clone();

    let thread = thread::Builder::new()
        .name("taiko-lan-controller".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let message = format!("failed to initialize LAN controller runtime: {error}");
                    let _ = ready_tx.send(Err(message.clone()));
                    return Err(anyhow!(message));
                }
            };

            runtime.block_on(async move {
                let bind_address = SocketAddr::new(config.bind_ip, 0);
                let listener = match tokio::net::TcpListener::bind(bind_address).await {
                    Ok(listener) => listener,
                    Err(error) => {
                        let message =
                            format!("failed to bind LAN controller to {bind_address}: {error}");
                        let _ = ready_tx.send(Err(message.clone()));
                        return Err(anyhow!(message));
                    }
                };
                let address = match listener.local_addr() {
                    Ok(address) => address,
                    Err(error) => {
                        let message = format!("failed to read LAN controller address: {error}");
                        let _ = ready_tx.send(Err(message.clone()));
                        return Err(anyhow!(message));
                    }
                };
                let authority: Arc<str> = Arc::from(address.to_string());
                let origin: Arc<str> = Arc::from(format!("http://{authority}"));
                let mut forced_shutdown_rx = thread_shutdown.subscribe();
                let state = HttpState {
                    authority,
                    origin,
                    control: thread_control,
                    input_senders,
                    shutdown: thread_shutdown,
                };
                let app = Router::new()
                    .route("/controller", get(controller_page))
                    .route("/controller/", get(controller_page))
                    .route("/controller/controller.css", get(controller_css))
                    .route("/controller/controller.js", get(controller_js))
                    .route("/controller/ws", get(controller_ws))
                    .with_state(state);

                if ready_tx.send(Ok(address)).is_err() {
                    return Err(anyhow!("LAN controller starter was dropped"));
                }

                let server = axum::serve(
                    AdmissionListener::new(listener),
                    app.into_make_service_with_connect_info::<ConnectionInfo>(),
                )
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx.await;
                })
                .into_future();
                tokio::pin!(server);
                tokio::select! {
                    result = &mut server => {
                        result.context("LAN controller HTTP server failed")
                    }
                    changed = forced_shutdown_rx.changed() => {
                        let _ = changed;
                        match tokio::time::timeout(FORCED_SHUTDOWN_TIMEOUT, &mut server).await {
                            Ok(result) => result.context("LAN controller HTTP server failed"),
                            Err(_) => Ok(()),
                        }
                    }
                }
            })
        })
        .context("failed to spawn LAN controller thread")?;

    let address = match ready_rx.recv() {
        Ok(Ok(address)) => address,
        Ok(Err(message)) => return Err(join_startup_failure(thread, message)),
        Err(error) => {
            return Err(join_startup_failure(
                thread,
                format!("LAN controller stopped before reporting its address: {error}"),
            ));
        }
    };

    Ok(LanControllers {
        endpoint: format!("http://{address}"),
        control,
        input_receivers: [p1_rx, p2_rx],
        shutdown: Some(shutdown_tx),
        shutdown_broadcast,
        thread: Some(thread),
    })
}

fn join_startup_failure(thread: thread::JoinHandle<Result<()>>, message: String) -> anyhow::Error {
    match thread.join() {
        Ok(Ok(())) => anyhow!(message),
        Ok(Err(error)) => error,
        Err(_) => anyhow!("{message}; LAN controller thread panicked before startup completed"),
    }
}

fn validate_bind_ip(bind_ip: IpAddr) -> Result<()> {
    if bind_ip.is_unspecified() {
        bail!("LAN controller must bind one exact interface address");
    }
    if bind_ip.is_multicast() {
        bail!("LAN controller cannot bind a multicast address");
    }
    if matches!(bind_ip, IpAddr::V4(address) if address.is_broadcast()) {
        bail!("LAN controller cannot bind the IPv4 broadcast address");
    }
    if matches!(bind_ip, IpAddr::V6(address) if address.is_unicast_link_local()) {
        bail!("IPv6 link-local binding requires an interface scope unsupported by this API");
    }
    Ok(())
}

async fn controller_page(
    ConnectInfo(connection): ConnectInfo<ConnectionInfo>,
    headers: HeaderMap,
    State(state): State<HttpState>,
) -> Response {
    connection.mark_header_complete();
    static_response(
        &headers,
        &state,
        "text/html; charset=utf-8",
        CONTROLLER_HTML,
    )
}

async fn controller_css(
    ConnectInfo(connection): ConnectInfo<ConnectionInfo>,
    headers: HeaderMap,
    State(state): State<HttpState>,
) -> Response {
    connection.mark_header_complete();
    static_response(&headers, &state, "text/css; charset=utf-8", CONTROLLER_CSS)
}

async fn controller_js(
    ConnectInfo(connection): ConnectInfo<ConnectionInfo>,
    headers: HeaderMap,
    State(state): State<HttpState>,
) -> Response {
    connection.mark_header_complete();
    static_response(
        &headers,
        &state,
        "text/javascript; charset=utf-8",
        CONTROLLER_JS,
    )
}

fn static_response(
    headers: &HeaderMap,
    state: &HttpState,
    content_type: &'static str,
    body: &'static str,
) -> Response {
    if !has_exact_header(headers, header::HOST, &state.authority) {
        return security_response(StatusCode::MISDIRECTED_REQUEST, state, "text/plain", "");
    }
    security_response(StatusCode::OK, state, content_type, body)
}

fn security_response(
    status: StatusCode,
    state: &HttpState,
    content_type: &'static str,
    body: &'static str,
) -> Response {
    let content_security_policy = format!(
        "default-src 'none'; script-src 'self'; style-src 'self'; \
         connect-src 'self' ws://{}; base-uri 'none'; form-action 'none'; \
         frame-ancestors 'none'; object-src 'none'; img-src 'none'; font-src 'none'",
        state.authority
    );
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONNECTION, "close")
        .header(header::CACHE_CONTROL, "no-store")
        .header("content-security-policy", content_security_policy)
        .header("cross-origin-resource-policy", "same-origin")
        .header(
            "permissions-policy",
            "camera=(), microphone=(), geolocation=()",
        )
        .header("referrer-policy", "no-referrer")
        .header("x-content-type-options", "nosniff")
        .header("x-frame-options", "DENY")
        .body(Body::from(body))
        .expect("static controller response headers are valid")
}

async fn controller_ws(
    ws: WebSocketUpgrade,
    ConnectInfo(connection): ConnectInfo<ConnectionInfo>,
    headers: HeaderMap,
    State(state): State<HttpState>,
) -> Response {
    connection.mark_header_complete();
    let _peer = connection.peer;
    if !has_exact_header(&headers, header::HOST, &state.authority)
        || !has_exact_header(&headers, header::ORIGIN, &state.origin)
    {
        return security_response(StatusCode::FORBIDDEN, &state, "text/plain", "");
    }
    if !has_exact_header(
        &headers,
        header::SEC_WEBSOCKET_PROTOCOL,
        WEBSOCKET_SUBPROTOCOL,
    ) {
        return security_response(StatusCode::BAD_REQUEST, &state, "text/plain", "");
    }
    ws.protocols([WEBSOCKET_SUBPROTOCOL])
        .max_message_size(MAX_WIRE_MESSAGE_BYTES)
        .max_frame_size(MAX_WIRE_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(state, socket))
        .into_response()
}

fn has_exact_header(headers: &HeaderMap, name: HeaderName, expected: &str) -> bool {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return false;
    };
    values.next().is_none() && value.as_bytes() == expected.as_bytes()
}

async fn handle_socket(state: HttpState, socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let mut shutdown_rx = state.shutdown.subscribe();
    if *shutdown_rx.borrow() {
        fail_and_close(&mut sender, ErrorCode::ServerStopping, close_code::AWAY).await;
        return;
    }
    let handshake_deadline = tokio::time::sleep(HANDSHAKE_TIMEOUT);
    tokio::pin!(handshake_deadline);

    let first = tokio::select! {
        _ = &mut handshake_deadline => {
            fail_and_close(&mut sender, ErrorCode::InvalidHandshake, close_code::POLICY).await;
            return;
        }
        changed = shutdown_rx.changed() => {
            let _ = changed;
            fail_and_close(&mut sender, ErrorCode::ServerStopping, close_code::AWAY).await;
            return;
        }
        incoming = receiver.next() => incoming,
    };
    let Some(Ok(WsMessage::Text(raw))) = first else {
        fail_and_close(
            &mut sender,
            ErrorCode::InvalidHandshake,
            close_code::UNSUPPORTED,
        )
        .await;
        return;
    };
    let message = match serde_json::from_str::<ClientMessage>(&raw) {
        Ok(message) => message,
        Err(_) => {
            fail_and_close(&mut sender, ErrorCode::InvalidHandshake, close_code::POLICY).await;
            return;
        }
    };
    let authentication = match message {
        ClientMessage::Pair { protocol, token } if protocol == PROTOCOL_VERSION => {
            state.control.authenticate_pair(&token)
        }
        ClientMessage::Resume {
            protocol,
            session_token,
        } if protocol == PROTOCOL_VERSION => state.control.authenticate_resume(&session_token),
        ClientMessage::Pair { .. }
        | ClientMessage::Resume { .. }
        | ClientMessage::ReadyAck { .. }
        | ClientMessage::Hit { .. } => Err(AuthenticationError::Unauthorized),
    };
    let mut session = match authentication {
        Ok(session) => session,
        Err(AuthenticationError::Unauthorized) => {
            fail_and_close(&mut sender, ErrorCode::Unauthorized, close_code::POLICY).await;
            return;
        }
        Err(AuthenticationError::Internal) => {
            fail_and_close(&mut sender, ErrorCode::InvalidHandshake, close_code::POLICY).await;
            return;
        }
    };

    if send_server_message(
        &mut sender,
        &ServerMessage::Ready {
            protocol: PROTOCOL_VERSION,
            slot: WireSlot::from(session.slot),
            connection_id: session.connection_id,
            session_token: session.session_token.take(),
            commit_required: session.commit_required,
        },
    )
    .await
    .is_err()
    {
        state
            .control
            .disconnect(session.slot, session.connection_id);
        return;
    }

    if session.commit_required {
        handshake_deadline
            .as_mut()
            .reset(tokio::time::Instant::now() + HANDSHAKE_TIMEOUT);
        let acknowledgement = tokio::select! {
            _ = &mut handshake_deadline => None,
            changed = session.close_rx.changed() => {
                let _ = changed;
                None
            }
            changed = shutdown_rx.changed() => {
                let _ = changed;
                None
            }
            incoming = receiver.next() => incoming,
        };
        let acknowledged = matches!(
            acknowledgement,
            Some(Ok(WsMessage::Text(raw)))
                if matches!(
                    serde_json::from_str::<ClientMessage>(&raw),
                    Ok(ClientMessage::ReadyAck { connection_id })
                        if connection_id == session.connection_id
                )
        );
        if !acknowledged
            || state
                .control
                .commit_pairing(session.slot, session.connection_id)
                .is_err()
        {
            fail_and_close(&mut sender, ErrorCode::InvalidHandshake, close_code::POLICY).await;
            state
                .control
                .disconnect(session.slot, session.connection_id);
            return;
        }
        if send_server_message(
            &mut sender,
            &ServerMessage::Ready {
                protocol: PROTOCOL_VERSION,
                slot: WireSlot::from(session.slot),
                connection_id: session.connection_id,
                session_token: None,
                commit_required: false,
            },
        )
        .await
        .is_err()
        {
            state
                .control
                .disconnect(session.slot, session.connection_id);
            return;
        }
    }

    let mut expected_seq = 1_u64;
    let mut last_pong = Instant::now();
    let mut ping_nonce = 0_u64;
    let mut expected_pong: Option<[u8; 8]> = None;
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.tick().await;

    loop {
        tokio::select! {
            biased;
            changed = session.close_rx.changed() => {
                let reason = if changed.is_err() {
                    ConnectionCloseReason::ServerStopping
                } else {
                    session
                        .close_rx
                        .borrow_and_update()
                        .unwrap_or(ConnectionCloseReason::ServerStopping)
                };
                let code = match reason {
                    ConnectionCloseReason::Fenced | ConnectionCloseReason::Revoked => {
                        ErrorCode::Unauthorized
                    }
                    ConnectionCloseReason::ServerStopping => ErrorCode::ServerStopping,
                };
                fail_and_close(&mut sender, code, close_code::POLICY).await;
                break;
            }
            changed = shutdown_rx.changed() => {
                let _ = changed;
                fail_and_close(
                    &mut sender,
                    ErrorCode::ServerStopping,
                    close_code::AWAY,
                )
                .await;
                break;
            }
            _ = ping.tick() => {
                if last_pong.elapsed() > CONNECTION_LEASE {
                    fail_and_close(
                        &mut sender,
                        ErrorCode::InvalidMessage,
                        close_code::POLICY,
                    )
                    .await;
                    break;
                }
                if expected_pong.is_none() {
                    ping_nonce = ping_nonce.wrapping_add(1);
                    let nonce = ping_nonce.to_be_bytes();
                    expected_pong = Some(nonce);
                    if send_ws_message(&mut sender, WsMessage::Ping(nonce.to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
            incoming = receiver.next() => {
                let ingress_guard = state.control.begin_frame_ingress();
                // This is the authoritative input timestamp. It deliberately precedes
                // JSON parsing, locking, rate checks, and cross-thread queueing.
                let received_at = Instant::now();
                let Some(Ok(incoming)) = incoming else {
                    break;
                };
                if !matches!(incoming, WsMessage::Close(_)) {
                    match state.control.admit_frame(
                        session.slot,
                        session.connection_id,
                        received_at,
                    ) {
                        MessageAdmission::Accepted => {}
                        MessageAdmission::Fenced => {
                            drop(ingress_guard);
                            fail_and_close(
                                &mut sender,
                                ErrorCode::Unauthorized,
                                close_code::POLICY,
                            )
                            .await;
                            break;
                        }
                        MessageAdmission::RateLimited => {
                            drop(ingress_guard);
                            fail_and_close(
                                &mut sender,
                                ErrorCode::RateLimited,
                                close_code::POLICY,
                            )
                            .await;
                            break;
                        }
                        MessageAdmission::ServerStopping => {
                            drop(ingress_guard);
                            fail_and_close(
                                &mut sender,
                                ErrorCode::ServerStopping,
                                close_code::AWAY,
                            )
                            .await;
                            break;
                        }
                    }
                }
                match incoming {
                    WsMessage::Text(raw) => {
                        let message = match serde_json::from_str::<ClientMessage>(&raw) {
                            Ok(ClientMessage::Hit { seq, action }) => (seq, action),
                            Ok(
                                ClientMessage::Pair { .. }
                                | ClientMessage::Resume { .. }
                                | ClientMessage::ReadyAck { .. },
                            )
                            | Err(_) => {
                                state.control.record_rejected(session.slot);
                                drop(ingress_guard);
                                fail_and_close(
                                    &mut sender,
                                    ErrorCode::InvalidMessage,
                                    close_code::PROTOCOL,
                                )
                                .await;
                                break;
                            }
                        };
                        let (seq, action) = message;
                        if seq != expected_seq {
                            state.control.record_rejected(session.slot);
                            drop(ingress_guard);
                            fail_and_close(
                                &mut sender,
                                ErrorCode::OutOfSequence,
                                close_code::PROTOCOL,
                            )
                            .await;
                            break;
                        }
                        let Some(next_seq) = expected_seq.checked_add(1) else {
                            state.control.record_rejected(session.slot);
                            drop(ingress_guard);
                            fail_and_close(
                                &mut sender,
                                ErrorCode::OutOfSequence,
                                close_code::PROTOCOL,
                            )
                            .await;
                            break;
                        };
                        expected_seq = next_seq;

                        let strike = ControllerStrike::lan(
                            session.slot,
                            session.connection_id,
                            action.into_action(),
                            received_at,
                            seq,
                            state.control.generation,
                        );
                        let admission = state.control.admit_strike(
                            session.slot,
                            session.connection_id,
                            &state.input_senders[session.slot.index()],
                            strike,
                        );
                        drop(ingress_guard);
                        match admission {
                            StrikeAdmission::Accepted => {
                                if send_server_message(
                                    &mut sender,
                                    &ServerMessage::Ack {
                                        next_seq,
                                        accepted: true,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            StrikeAdmission::Inactive => {
                                if send_server_message(
                                    &mut sender,
                                    &ServerMessage::Ack {
                                        next_seq,
                                        accepted: false,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                            }
                            StrikeAdmission::Fenced => {
                                fail_and_close(
                                    &mut sender,
                                    ErrorCode::Unauthorized,
                                    close_code::POLICY,
                                )
                                .await;
                                break;
                            }
                            StrikeAdmission::QueueFull => {
                                fail_and_close(
                                    &mut sender,
                                    ErrorCode::QueueFull,
                                    close_code::POLICY,
                                )
                                .await;
                                break;
                            }
                            StrikeAdmission::ServerStopping => {
                                fail_and_close(
                                    &mut sender,
                                    ErrorCode::ServerStopping,
                                    close_code::AWAY,
                                )
                                .await;
                                break;
                            }
                        }
                    }
                    WsMessage::Pong(payload)
                        if expected_pong
                            .take()
                            .is_some_and(|expected| payload.as_ref() == expected) =>
                    {
                        last_pong = received_at;
                    }
                    WsMessage::Pong(_) | WsMessage::Ping(_) => {
                        state.control.record_rejected(session.slot);
                        drop(ingress_guard);
                        fail_and_close(
                            &mut sender,
                            ErrorCode::InvalidMessage,
                            close_code::POLICY,
                        )
                        .await;
                        break;
                    }
                    WsMessage::Close(_) => break,
                    WsMessage::Binary(_) => {
                        state.control.record_rejected(session.slot);
                        drop(ingress_guard);
                        fail_and_close(
                            &mut sender,
                            ErrorCode::InvalidMessage,
                            close_code::UNSUPPORTED,
                        )
                        .await;
                        break;
                    }
                }
            }
        }
    }

    state
        .control
        .disconnect(session.slot, session.connection_id);
}

async fn send_server_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    message: &ServerMessage,
) -> Result<()> {
    let raw = serde_json::to_string(message).context("failed to encode controller response")?;
    send_ws_message(sender, WsMessage::Text(raw.into())).await
}

async fn send_ws_message(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    message: WsMessage,
) -> Result<()> {
    tokio::time::timeout(WRITE_TIMEOUT, sender.send(message))
        .await
        .context("controller websocket write timed out")?
        .context("controller websocket write failed")
}

async fn fail_and_close(
    sender: &mut futures_util::stream::SplitSink<WebSocket, WsMessage>,
    code: ErrorCode,
    close_code: u16,
) {
    let _ = send_server_message(sender, &ServerMessage::Error { code }).await;
    let _ = send_ws_message(
        sender,
        WsMessage::Close(Some(CloseFrame {
            code: close_code,
            reason: Utf8Bytes::from_static("controller connection closed"),
        })),
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::Ipv4Addr;

    use reqwest::header as reqwest_header;
    use rhythm_mode_taiko::TaikoAction;
    use serde_json::Value;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::Request;
    use tokio_tungstenite::tungstenite::{Error as TungsteniteError, Message as ClientWsMessage};
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

    use super::*;

    type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    fn websocket_request(
        endpoint: &str,
        origin: Option<&str>,
        include_subprotocol: bool,
    ) -> Request<()> {
        let websocket_url = endpoint.replacen("http://", "ws://", 1) + "/controller/ws";
        let mut request = websocket_url
            .into_client_request()
            .expect("websocket request");
        if let Some(origin) = origin {
            request
                .headers_mut()
                .insert(header::ORIGIN, origin.parse().expect("origin header"));
        }
        if include_subprotocol {
            request.headers_mut().insert(
                header::SEC_WEBSOCKET_PROTOCOL,
                WEBSOCKET_SUBPROTOCOL
                    .parse()
                    .expect("websocket subprotocol header"),
            );
        }
        request
    }

    async fn next_json(socket: &mut ClientSocket) -> Value {
        let message = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("websocket response deadline")
            .expect("websocket response")
            .expect("valid websocket frame");
        let ClientWsMessage::Text(raw) = message else {
            panic!("expected a text websocket response, got {message:?}");
        };
        serde_json::from_str(&raw).expect("valid server JSON")
    }

    #[test]
    fn rate_limiter_has_exact_burst_and_refill_boundaries() {
        let start = Instant::now();
        let mut limiter = MessageRateLimiter::new(start);
        for _ in 0..MESSAGE_BURST {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start));
        let one_credit_later =
            start + Duration::from_micros(RATE_CREDIT_SCALE / MESSAGE_RATE_PER_SECOND + 1);
        assert!(limiter.allow(one_credit_later));
        assert!(!limiter.allow(one_credit_later));
    }

    #[test]
    fn ingress_snapshot_records_each_completed_frame_after_it_leaves_flight() {
        let control = ServerControl::new(1).expect("control");
        let before = control.ingress_snapshot();
        assert_eq!(before.in_flight, 0);

        let guard = control.begin_frame_ingress();
        let during = control.ingress_snapshot();
        assert_eq!(during.in_flight, 1);
        assert_eq!(during.completed, before.completed);

        drop(guard);
        let after = control.ingress_snapshot();
        assert_eq!(after.in_flight, 0);
        assert_eq!(after.completed, before.completed + 1);
    }

    #[test]
    fn ingress_snapshot_cannot_look_stable_between_completion_and_release() {
        let control = ServerControl::new(2).expect("control");
        let before = control.ingress_snapshot();
        control.in_flight_frames.fetch_add(1, Ordering::SeqCst);

        let counter = Arc::clone(&control.in_flight_frames);
        let completed = Arc::clone(&control.completed_frames);
        let (published_tx, published_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let finisher = std::thread::spawn(move || {
            finish_frame_ingress(&counter, &completed, || {
                published_tx.send(()).expect("publish test checkpoint");
                release_rx.recv().expect("release test checkpoint");
            });
        });

        published_rx.recv().expect("completion publication");
        let between = control.ingress_snapshot();
        assert_eq!(between.in_flight, 1);
        assert_eq!(between.completed, before.completed + 1);

        release_tx.send(()).expect("release frame");
        finisher.join().expect("frame finisher");
        let after = control.ingress_snapshot();
        assert_eq!(after.in_flight, 0);
        assert_eq!(after.completed, before.completed + 1);
    }

    #[test]
    fn pairing_tokens_commit_only_after_ack_and_retry_reuses_pending_session() {
        let control = ServerControl::new(42).expect("control");
        let p1 = control
            .pairing_token(ControllerSlot::One)
            .expect("P1 token");
        let p2 = control
            .pairing_token(ControllerSlot::Two)
            .expect("P2 token");
        assert_ne!(p1, p2);
        assert_eq!(p1.len(), TOKEN_HEX_BYTES);
        assert_eq!(p2.len(), TOKEN_HEX_BYTES);

        let first = control.authenticate_pair(&p1).expect("begin pairing P1");
        assert_eq!(first.slot, ControllerSlot::One);
        let session_token = first.session_token.clone().expect("pending session token");
        assert!(first.commit_required);
        assert!(control.pairing_token(ControllerSlot::One).is_some());
        assert!(!control.status()[ControllerSlot::One.index()].paired);

        let retry = control
            .authenticate_pair(&p1)
            .expect("retry pending pairing");
        assert_eq!(retry.session_token.as_deref(), Some(session_token.as_str()));
        assert!(retry.commit_required);
        control
            .commit_pairing(ControllerSlot::One, retry.connection_id)
            .expect("commit acknowledged pairing");
        assert!(control.pairing_token(ControllerSlot::One).is_none());
        assert!(control.status()[ControllerSlot::One.index()].paired);
        assert_eq!(
            control.authenticate_pair(&p1).err(),
            Some(AuthenticationError::Unauthorized)
        );
        assert_eq!(
            format!("{:?}", SecretToken::parse(&p2).expect("parse token")),
            "SecretToken([REDACTED])"
        );
    }

    #[test]
    fn resume_fences_previous_socket_and_rotate_revokes_session() {
        let control = ServerControl::new(7).expect("control");
        let token = control
            .pairing_token(ControllerSlot::Two)
            .expect("P2 token");
        let paired = control.authenticate_pair(&token).expect("pair");
        let session_token = paired.session_token.clone().expect("session token");
        let mut first_close = paired.close_rx.clone();

        let resumed = control.authenticate_resume(&session_token).expect("resume");
        assert_eq!(resumed.slot, ControllerSlot::Two);
        assert_eq!(
            *first_close.borrow_and_update(),
            Some(ConnectionCloseReason::Fenced)
        );

        let mut resumed_close = resumed.close_rx.clone();
        let replacement = control.rotate_pairing(ControllerSlot::Two).expect("rotate");
        assert_eq!(replacement.len(), TOKEN_HEX_BYTES);
        assert_eq!(
            *resumed_close.borrow_and_update(),
            Some(ConnectionCloseReason::Revoked)
        );
        assert_eq!(
            control.authenticate_resume(&session_token).err(),
            Some(AuthenticationError::Unauthorized)
        );
    }

    #[test]
    fn per_slot_rate_limit_survives_session_resume() {
        let control = ServerControl::new(11).expect("control");
        let pairing_token = control
            .pairing_token(ControllerSlot::Two)
            .expect("P2 token");
        let paired = control.authenticate_pair(&pairing_token).expect("pair P2");
        let session_token = paired.session_token.clone().expect("session token");
        let received_at = Instant::now();
        for _ in 0..MESSAGE_BURST {
            assert_eq!(
                control.admit_frame(ControllerSlot::Two, paired.connection_id, received_at,),
                MessageAdmission::Accepted
            );
        }
        assert_eq!(
            control.admit_frame(ControllerSlot::Two, paired.connection_id, received_at,),
            MessageAdmission::RateLimited
        );

        let resumed = control
            .authenticate_resume(&session_token)
            .expect("resume P2");
        assert_eq!(
            control.admit_frame(ControllerSlot::Two, paired.connection_id, received_at,),
            MessageAdmission::Fenced
        );
        assert_eq!(
            control.admit_frame(ControllerSlot::Two, resumed.connection_id, received_at,),
            MessageAdmission::RateLimited
        );
    }

    #[test]
    fn strike_admission_is_linearized_with_resume_and_queue_bounds() {
        let control = ServerControl::new(17).expect("control");
        control.set_active_slots(ActiveControllerSlots::ONE);
        let pairing_token = control
            .pairing_token(ControllerSlot::One)
            .expect("P1 pairing token");
        let first = control.authenticate_pair(&pairing_token).expect("pair P1");
        let session_token = first.session_token.clone().expect("session token");
        let (input_tx, input_rx) = mpsc::sync_channel(1);
        let strike = |connection_id, sequence| {
            ControllerStrike::lan(
                ControllerSlot::One,
                connection_id,
                TaikoAction::LEFT_DON,
                Instant::now(),
                sequence,
                17,
            )
        };

        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                first.connection_id,
                &input_tx,
                strike(first.connection_id, 1),
            ),
            StrikeAdmission::Accepted
        );
        let resumed = control
            .authenticate_resume(&session_token)
            .expect("resume P1");
        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                first.connection_id,
                &input_tx,
                strike(first.connection_id, 2),
            ),
            StrikeAdmission::Fenced
        );
        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                resumed.connection_id,
                &input_tx,
                strike(resumed.connection_id, 1),
            ),
            StrikeAdmission::QueueFull
        );
        let queued_before_resume = input_rx.recv().expect("queued first strike");
        assert!(control.authorize_dispatch(
            &queued_before_resume,
            ControllerSlot::One,
            ActiveControllerSlots::ONE,
            Instant::now(),
            crate::lan_controller::MAX_DISPATCH_AGE,
        ));
        control.set_active_slots(ActiveControllerSlots::NONE);
        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                resumed.connection_id,
                &input_tx,
                strike(resumed.connection_id, 2),
            ),
            StrikeAdmission::Inactive
        );
    }

    #[test]
    fn closing_admission_keeps_already_queued_strikes_dispatchable() {
        let control = ServerControl::new(18).expect("control");
        control.set_active_slots(ActiveControllerSlots::ONE);
        let token = control
            .pairing_token(ControllerSlot::One)
            .expect("P1 pairing token");
        let session = control.authenticate_pair(&token).expect("pair P1");
        let (input_tx, input_rx) = mpsc::sync_channel(1);
        let strike = ControllerStrike::lan(
            ControllerSlot::One,
            session.connection_id,
            TaikoAction::RIGHT_DON,
            Instant::now(),
            1,
            18,
        );

        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                session.connection_id,
                &input_tx,
                strike,
            ),
            StrikeAdmission::Accepted
        );
        control.set_active_slots(ActiveControllerSlots::NONE);
        let queued = input_rx.recv().expect("strike admitted before gate close");
        assert!(control.authorize_dispatch(
            &queued,
            ControllerSlot::One,
            ActiveControllerSlots::ONE,
            Instant::now(),
            crate::lan_controller::MAX_DISPATCH_AGE,
        ));
        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                session.connection_id,
                &input_tx,
                ControllerStrike::lan(
                    ControllerSlot::One,
                    session.connection_id,
                    TaikoAction::RIGHT_DON,
                    Instant::now(),
                    2,
                    18,
                ),
            ),
            StrikeAdmission::Inactive
        );
    }

    #[test]
    fn full_p1_queue_does_not_block_p2() {
        let control = ServerControl::new(23).expect("control");
        control.set_active_slots(ActiveControllerSlots::BOTH);
        let p1_token = control
            .pairing_token(ControllerSlot::One)
            .expect("P1 token");
        let p2_token = control
            .pairing_token(ControllerSlot::Two)
            .expect("P2 token");
        let p1 = control.authenticate_pair(&p1_token).expect("pair P1");
        let p2 = control.authenticate_pair(&p2_token).expect("pair P2");
        let (p1_tx, _p1_rx) = mpsc::sync_channel(1);
        let (p2_tx, p2_rx) = mpsc::sync_channel(1);
        let strike = |slot, connection_id, action, sequence| {
            ControllerStrike::lan(slot, connection_id, action, Instant::now(), sequence, 23)
        };

        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                p1.connection_id,
                &p1_tx,
                strike(
                    ControllerSlot::One,
                    p1.connection_id,
                    TaikoAction::LEFT_DON,
                    1,
                ),
            ),
            StrikeAdmission::Accepted
        );
        assert_eq!(
            control.admit_strike(
                ControllerSlot::One,
                p1.connection_id,
                &p1_tx,
                strike(
                    ControllerSlot::One,
                    p1.connection_id,
                    TaikoAction::RIGHT_DON,
                    2,
                ),
            ),
            StrikeAdmission::QueueFull
        );
        assert_eq!(
            control.admit_strike(
                ControllerSlot::Two,
                p2.connection_id,
                &p2_tx,
                strike(
                    ControllerSlot::Two,
                    p2.connection_id,
                    TaikoAction::LEFT_KAT,
                    1,
                ),
            ),
            StrikeAdmission::Accepted
        );
        let p2_strike = p2_rx.recv().expect("P2 strike");
        assert_eq!(p2_strike.slot, ControllerSlot::Two);
        assert_eq!(p2_strike.action, TaikoAction::LEFT_KAT);
    }

    #[test]
    fn bind_address_is_exact_and_rejects_ambiguous_addresses() {
        assert!(validate_bind_ip(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)).is_ok());
        assert!(validate_bind_ip(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)).is_err());
        assert!(validate_bind_ip(IpAddr::V4(std::net::Ipv4Addr::BROADCAST)).is_err());
        assert!(validate_bind_ip(IpAddr::V4(std::net::Ipv4Addr::new(224, 0, 0, 1))).is_err());
        assert!(validate_bind_ip(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)).is_ok());
        assert!(validate_bind_ip(IpAddr::V6("fe80::1".parse().expect("IPv6"))).is_err());
    }

    #[test]
    fn exact_header_validation_rejects_missing_foreign_and_duplicate_values() {
        let mut headers = HeaderMap::new();
        assert!(!has_exact_header(&headers, header::HOST, "127.0.0.1:1234"));
        headers.insert(header::HOST, "evil.example".parse().expect("header"));
        assert!(!has_exact_header(&headers, header::HOST, "127.0.0.1:1234"));
        headers.insert(header::HOST, "127.0.0.1:1234".parse().expect("header"));
        assert!(has_exact_header(&headers, header::HOST, "127.0.0.1:1234"));
        headers.append(header::HOST, "127.0.0.1:1234".parse().expect("header"));
        assert!(!has_exact_header(&headers, header::HOST, "127.0.0.1:1234"));
    }

    #[test]
    fn browser_assets_are_localized_accessible_and_have_no_external_dependencies() {
        assert!(CONTROLLER_JS.contains("\"pointerdown\""));
        assert!(!CONTROLLER_JS.contains("addEventListener(\"click\""));
        assert!(CONTROLLER_JS.contains("const connection = new WebSocket"));
        assert!(CONTROLLER_JS.contains("connection !== socket"));
        assert!(CONTROLLER_JS
            .contains("const languageParams = new URLSearchParams(window.location.search);"));
        assert!(CONTROLLER_JS
            .contains("const params = new URLSearchParams(window.location.hash.slice(1));"));
        assert!(CONTROLLER_JS.contains("const pairingToken = params.get(\"token\");"));
        assert!(!CONTROLLER_JS.contains("languageParams.get(\"token\")"));
        assert!(!CONTROLLER_JS.contains("window.location.href"));
        assert!(CONTROLLER_JS.contains("\"zh-Hant\": Object.freeze"));
        assert!(CONTROLLER_JS.contains("ja: Object.freeze"));
        assert!(!CONTROLLER_JS.contains("innerHTML"));
        for key in [
            "title",
            "eyebrow",
            "pairing",
            "connecting",
            "drumControls",
            "leftKat",
            "leftDon",
            "rightDon",
            "rightKat",
            "left",
            "right",
            "kat",
            "don",
            "rim",
            "center",
            "trustedLan",
            "completeLink",
            "reconnectFailed",
            "reconnectingIn",
            "reconnecting",
            "connectionInterrupted",
            "invalidResponse",
            "completingPairing",
            "player1",
            "player2",
            "ready",
            "waitingGameplay",
            "sessionExpired",
            "pairingExpired",
            "handshakeFailed",
            "serverStopped",
            "rateLimited",
            "queueFull",
            "rejected",
            "waitingGame",
            "paused",
        ] {
            assert_eq!(
                CONTROLLER_JS.matches(&format!("\n      {key}:")).count(),
                3,
                "{key} must have one translation in every locale"
            );
        }
        for action in ["left_kat", "left_don", "right_don", "right_kat"] {
            assert!(CONTROLLER_HTML.contains(action));
        }
        for key in [
            "title",
            "eyebrow",
            "pairing",
            "connecting",
            "drumControls",
            "leftKat",
            "leftDon",
            "rightDon",
            "rightKat",
            "left",
            "right",
            "kat",
            "don",
            "rim",
            "center",
            "trustedLan",
        ] {
            assert!(
                CONTROLLER_HTML.contains(&format!("\"{key}\"")),
                "controller HTML must bind {key}"
            );
        }
        assert_eq!(CONTROLLER_HTML.matches("aria-pressed=\"false\"").count(), 4);
        assert_eq!(CONTROLLER_HTML.matches("data-i18n-aria-label=").count(), 5);
        assert!(!CONTROLLER_HTML.contains("https://"));
        assert!(!CONTROLLER_JS.contains("wss:"));
        assert!(!CONTROLLER_JS.contains("client_timestamp"));
    }

    #[test]
    fn partial_http_client_cannot_block_controller_shutdown() {
        let controllers = LanControllers::start(LanControllerConfig {
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            generation: 90,
        })
        .expect("start controller server");
        let address: SocketAddr = controllers
            .endpoint()
            .strip_prefix("http://")
            .expect("HTTP endpoint")
            .parse()
            .expect("controller socket address");
        let mut partial = std::net::TcpStream::connect(address).expect("partial HTTP connection");
        partial
            .write_all(b"G")
            .expect("write incomplete HTTP request");

        let started = Instant::now();
        controllers
            .shutdown_and_join()
            .expect("bounded controller shutdown");
        assert!(
            started.elapsed() <= FORCED_SHUTDOWN_TIMEOUT + Duration::from_millis(500),
            "partial HTTP client exceeded bounded shutdown deadline"
        );
    }

    #[tokio::test]
    async fn controller_server_lifetime_is_not_limited_by_shutdown_deadline() {
        let controllers = LanControllers::start(LanControllerConfig {
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            generation: 90,
        })
        .expect("start controller server");
        let endpoint = controllers.endpoint();
        tokio::time::sleep(FORCED_SHUTDOWN_TIMEOUT + Duration::from_millis(150)).await;
        let response = reqwest::get(format!("{endpoint}/controller/"))
            .await
            .expect("server remains reachable after shutdown-drain interval");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        controllers
            .shutdown_and_join()
            .expect("controller shutdown");
    }

    #[tokio::test]
    async fn real_http_websocket_resume_receipt_and_shutdown_flow() {
        let mut controllers = LanControllers::start(LanControllerConfig {
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            generation: 91,
        })
        .expect("start controller server");
        controllers.set_active_slots(ActiveControllerSlots::ONE);
        let endpoint = controllers.endpoint();

        let client = reqwest::Client::new();
        let page = client
            .get(format!("{endpoint}/controller/"))
            .send()
            .await
            .expect("controller page");
        assert_eq!(page.status(), reqwest::StatusCode::OK);
        assert_eq!(
            page.headers()
                .get(reqwest_header::CACHE_CONTROL)
                .expect("cache-control"),
            "no-store"
        );
        assert!(page
            .headers()
            .get("content-security-policy")
            .expect("CSP")
            .to_str()
            .expect("CSP text")
            .contains("frame-ancestors 'none'"));
        assert!(page
            .headers()
            .get(reqwest_header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none());
        assert!(page
            .text()
            .await
            .expect("controller HTML")
            .contains("left_kat"));

        let foreign_host = client
            .get(format!("{endpoint}/controller/"))
            .header(reqwest_header::HOST, "attacker.invalid")
            .send()
            .await
            .expect("foreign Host response");
        assert_eq!(
            foreign_host.status(),
            reqwest::StatusCode::MISDIRECTED_REQUEST
        );

        let missing_origin = connect_async(websocket_request(&endpoint, None, true)).await;
        match missing_origin {
            Err(TungsteniteError::Http(response)) => {
                assert_eq!(response.status().as_u16(), StatusCode::FORBIDDEN.as_u16());
            }
            result => panic!("missing Origin must be rejected, got {result:?}"),
        }
        let missing_subprotocol =
            connect_async(websocket_request(&endpoint, Some(&endpoint), false)).await;
        match missing_subprotocol {
            Err(TungsteniteError::Http(response)) => {
                assert_eq!(response.status().as_u16(), StatusCode::BAD_REQUEST.as_u16());
            }
            result => panic!("missing subprotocol must be rejected, got {result:?}"),
        }

        let pairing_url = controllers
            .pairing_invite(ControllerSlot::One, crate::preferences::UiLanguage::English)
            .expect("P1 invite");
        let pairing_token = pairing_url
            .expose()
            .split_once("#token=")
            .expect("fragment token")
            .1
            .to_owned();
        let (mut first_socket, first_response) =
            connect_async(websocket_request(&endpoint, Some(&endpoint), true))
                .await
                .expect("first websocket");
        assert_eq!(
            first_response
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .expect("selected subprotocol"),
            WEBSOCKET_SUBPROTOCOL
        );
        first_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "pair",
                    "protocol": PROTOCOL_VERSION,
                    "token": pairing_token,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("pair message");
        let first_ready = next_json(&mut first_socket).await;
        assert_eq!(first_ready["type"], "ready");
        assert_eq!(first_ready["slot"], "p1");
        assert_eq!(first_ready["commit_required"], true);
        let session_token = first_ready["session_token"]
            .as_str()
            .expect("new session token")
            .to_owned();
        first_socket
            .close(None)
            .await
            .expect("close before ready ack");

        let (mut retry_socket, _) =
            connect_async(websocket_request(&endpoint, Some(&endpoint), true))
                .await
                .expect("pair retry websocket");
        retry_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "pair",
                    "protocol": PROTOCOL_VERSION,
                    "token": pairing_token,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("pair retry message");
        let retry_ready = next_json(&mut retry_socket).await;
        assert_eq!(retry_ready["type"], "ready");
        assert_eq!(retry_ready["commit_required"], true);
        assert_eq!(
            retry_ready["session_token"].as_str(),
            Some(session_token.as_str())
        );
        let retry_connection_id = retry_ready["connection_id"]
            .as_u64()
            .expect("retry connection id");
        retry_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "ready_ack",
                    "connection_id": retry_connection_id,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("ready acknowledgement");
        let committed = next_json(&mut retry_socket).await;
        assert_eq!(committed["type"], "ready");
        assert_eq!(committed["commit_required"], false);
        assert!(committed.get("session_token").is_none());

        let old_earliest_receipt = Instant::now();
        retry_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "hit",
                    "seq": 1,
                    "action": "left_don",
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("pre-resume hit message");
        let old_acknowledgement = next_json(&mut retry_socket).await;
        let old_latest_receipt = Instant::now();
        assert_eq!(old_acknowledgement["type"], "ack");
        assert_eq!(old_acknowledgement["next_seq"], 2);
        assert_eq!(old_acknowledgement["accepted"], true);

        let (mut resumed_socket, _) =
            connect_async(websocket_request(&endpoint, Some(&endpoint), true))
                .await
                .expect("resumed websocket");
        resumed_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "resume",
                    "protocol": PROTOCOL_VERSION,
                    "session_token": session_token,
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("resume message");
        let resumed_ready = next_json(&mut resumed_socket).await;
        assert_eq!(resumed_ready["type"], "ready");
        assert_eq!(resumed_ready["commit_required"], false);
        assert!(resumed_ready.get("session_token").is_none());
        let resumed_connection_id = resumed_ready["connection_id"]
            .as_u64()
            .expect("connection id");

        let fenced = next_json(&mut retry_socket).await;
        assert_eq!(fenced["type"], "error");
        assert_eq!(fenced["code"], "unauthorized");

        let new_earliest_receipt = Instant::now();
        resumed_socket
            .send(ClientWsMessage::Text(
                serde_json::json!({
                    "type": "hit",
                    "seq": 1,
                    "action": "right_kat",
                })
                .to_string()
                .into(),
            ))
            .await
            .expect("hit message");
        let acknowledgement = next_json(&mut resumed_socket).await;
        let new_latest_receipt = Instant::now();
        assert_eq!(acknowledgement["type"], "ack");
        assert_eq!(acknowledgement["next_seq"], 2);
        assert_eq!(acknowledgement["accepted"], true);

        let batch = controllers.drain_inputs(ActiveControllerSlots::ONE);
        assert!(!batch.saturated);
        let [p1, p2] = batch.strikes;
        assert_eq!(p1.len(), 2);
        assert!(p2.is_empty());
        let old_strike = &p1[0];
        assert_eq!(old_strike.slot, ControllerSlot::One);
        assert_eq!(old_strike.action, TaikoAction::LEFT_DON);
        assert_eq!(old_strike.sequence, Some(1));
        assert_eq!(old_strike.generation, Some(91));
        assert!(old_strike.observed_at >= old_earliest_receipt);
        assert!(old_strike.observed_at <= old_latest_receipt);
        assert!(matches!(
            old_strike.source,
            crate::controller::ControllerSource::Lan { connection_id }
                if connection_id == retry_connection_id
        ));
        let new_strike = &p1[1];
        assert_eq!(new_strike.slot, ControllerSlot::One);
        assert_eq!(new_strike.action, TaikoAction::RIGHT_KAT);
        assert_eq!(new_strike.sequence, Some(1));
        assert_eq!(new_strike.generation, Some(91));
        assert!(new_strike.observed_at >= new_earliest_receipt);
        assert!(new_strike.observed_at <= new_latest_receipt);
        assert!(matches!(
            new_strike.source,
            crate::controller::ControllerSource::Lan { connection_id }
                if connection_id == resumed_connection_id
        ));
        let [p1_status, p2_status] = controllers.status();
        assert_eq!(p1_status.accepted_hits, 2);
        assert_eq!(p1_status.rejected_hits, 0);
        assert!(p1_status.paired);
        assert!(p1_status.connected);
        assert_eq!(p2_status.accepted_hits, 0);

        controllers
            .shutdown_and_join()
            .expect("graceful controller shutdown");
        let stopped = next_json(&mut resumed_socket).await;
        assert_eq!(stopped["type"], "error");
        assert_eq!(stopped["code"], "server_stopping");
    }
}
