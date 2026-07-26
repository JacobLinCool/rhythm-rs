use std::collections::{HashMap, VecDeque};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use taiko_multiplayer_protocol::{
    ActorId, BoundedText, BoundedVec, CommandAck, CommandEnvelope, CommandOutcome, CommandSeq,
    ContentHash, CourseId, CourseManifest, CourseName, DisplayName, DisplayTitle, ErrorMessage,
    FinalResult, InputAck, InputBatch, InputOutcome, InputSeq, InvitationToken, JoinRole,
    LiveStateSnapshot, MatchId, MatchManifest, MatchSemantics, MembershipGranted, PlayerConnection,
    PlayerCourseAssignment, PlayerId, PlayerPreparation, PlayerSelection, PlayerSnapshot,
    PreparationProgress, PreparationProof, ProgressMilli, ProtocolError, ProtocolErrorCode,
    ResumeRequest, ResumeToken, RoomCode, RoomRevision, RoomSnapshot, RoomStage, ServerMessage,
    SessionId, SongId, SongManifest, SpectatorId, SpectatorSnapshot, Tick, FIRST_INPUT_SEQ,
    FIRST_MATCH_ID, FIRST_ROOM_REVISION, MAX_COURSES_PER_SONG, MAX_PLAYERS, MAX_SPECTATORS,
};
use taiko_resource_protocol::ResourceLibraryDocument;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch};

use super::clock::ProcessClock;
use super::clock_evidence::VerifiedClockQuality;
use super::limits::{
    COMMAND_ACK_CACHE_CAPACITY, FINALIZATION_GRACE, FINISHED_TTL, INPUT_LATENESS,
    LIVE_STATE_INTERVAL, LOBBY_IDLE_TTL, MATCH_COUNTDOWN, MATCH_TTL, RECONNECT_GRACE,
    RELIABLE_OUTBOUND_CAPACITY, ROOM_MAILBOX_CAPACITY, ROOM_MAX_LIFETIME, ROOM_MAX_PLAYERS,
    ROOM_MAX_SPECTATORS, ROOM_REQUEST_TIMEOUT, SESSION_LEASE,
};
use super::match_runtime::{AuthoritativeMatchRuntime, InputDropReason, MatchRuntimeError};

#[derive(Debug, Clone)]
pub(crate) struct SessionOutbound {
    reliable_tx: mpsc::Sender<ServerMessage>,
    live_tx: watch::Sender<Option<LiveStateSnapshot>>,
    close_tx: watch::Sender<Option<ProtocolError>>,
}

pub(crate) struct SessionTransport {
    pub(crate) reliable_rx: mpsc::Receiver<ServerMessage>,
    pub(crate) live_rx: watch::Receiver<Option<LiveStateSnapshot>>,
    pub(crate) close_rx: watch::Receiver<Option<ProtocolError>>,
}

impl SessionOutbound {
    pub(crate) fn channel() -> (Self, SessionTransport) {
        let (reliable_tx, reliable_rx) = mpsc::channel(RELIABLE_OUTBOUND_CAPACITY);
        let (live_tx, live_rx) = watch::channel(None);
        let (close_tx, close_rx) = watch::channel(None);
        (
            Self {
                reliable_tx,
                live_tx,
                close_tx,
            },
            SessionTransport {
                reliable_rx,
                live_rx,
                close_rx,
            },
        )
    }

    pub(crate) fn send_reliable(&self, message: ServerMessage) -> Result<(), OutboundError> {
        self.reliable_tx
            .try_send(message)
            .map_err(|error| match error {
                TrySendError::Full(_) => OutboundError::Full,
                TrySendError::Closed(_) => OutboundError::Closed,
            })
    }

    #[allow(dead_code)]
    pub(crate) fn publish_live(&self, snapshot: LiveStateSnapshot) {
        self.live_tx.send_replace(Some(snapshot));
    }

    pub(crate) fn close(&self, reason: ProtocolError) {
        if self.close_tx.borrow().is_none() {
            self.close_tx.send_replace(Some(reason));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutboundError {
    Full,
    Closed,
}

#[derive(Clone)]
pub(crate) struct SessionEndpoint {
    pub(crate) session_id: SessionId,
    pub(crate) name: DisplayName,
    pub(crate) outbound: SessionOutbound,
}

#[derive(Clone)]
pub(crate) struct MatchCatalog {
    songs: HashMap<SongId, CatalogSong>,
}

#[derive(Clone)]
struct CatalogSong {
    manifest: SongManifest,
}

impl MatchCatalog {
    pub(crate) fn from_library(library: &ResourceLibraryDocument) -> Result<Self> {
        library.validate().context(
            "resource library must satisfy the shared presentation and identity contract",
        )?;
        let semantics = MatchSemantics {
            canonical_schema_version: library.semantics.canonical_schema_version,
            canonical_schema_digest: ContentHash::parse(
                library.semantics.canonical_schema_sha256.clone(),
            )
            .context("invalid canonical schema digest")?,
            importer_semantics_version: library.semantics.importer_semantics_version,
            importer_semantics_digest: ContentHash::parse(
                library.semantics.importer_semantics_sha256.clone(),
            )
            .context("invalid importer semantics digest")?,
            ruleset_version: library.semantics.taiko_ruleset_version,
            ruleset_digest: ContentHash::parse(library.semantics.taiko_ruleset_sha256.clone())
                .context("invalid taiko ruleset digest")?,
            audio_decoder_semantics_version: library.semantics.audio_decoder_semantics_version,
            audio_decoder_semantics_digest: ContentHash::parse(
                library.semantics.audio_decoder_semantics_sha256.clone(),
            )
            .context("invalid audio decoder semantics digest")?,
        };

        let mut songs = HashMap::with_capacity(library.songs.len());
        for resource in &library.songs {
            let song_id =
                SongId::parse(resource.song_id.clone()).context("invalid multiplayer song id")?;
            let courses = resource
                .courses
                .iter()
                .map(|course| {
                    let course_id = CourseId(course.index);
                    Ok(CourseManifest {
                        course_id,
                        name: CourseName::new(course.name.clone())
                            .context("course name exceeds multiplayer limit")?,
                        level: course.level,
                        canonical_chart_hash: ContentHash::parse(
                            course.canonical_chart_hash.clone(),
                        )
                        .context("invalid canonical chart digest")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let manifest = SongManifest {
                song_id: song_id.clone(),
                source_id: ContentHash::parse(resource.source_id.clone())
                    .context("invalid source content digest")?,
                audio_id: resource
                    .audio_id
                    .as_ref()
                    .map(|audio_id| {
                        ContentHash::parse(audio_id.clone()).context("invalid audio content digest")
                    })
                    .transpose()?,
                title: DisplayTitle::new(resource.title.clone())
                    .context("title exceeds multiplayer limit")?,
                subtitle: BoundedText::new(resource.subtitle.clone())
                    .context("subtitle exceeds multiplayer limit")?,
                artist: BoundedText::new(resource.artist.clone())
                    .context("artist exceeds multiplayer limit")?,
                semantics: semantics.clone(),
                courses: BoundedVec::<_, MAX_COURSES_PER_SONG>::try_from(courses)
                    .context("too many multiplayer courses")?,
            };
            manifest
                .validate()
                .map_err(|error| anyhow!("invalid song manifest: {error}"))?;
            if songs.insert(song_id, CatalogSong { manifest }).is_some() {
                return Err(anyhow!("duplicate song id in multiplayer catalog"));
            }
        }
        Ok(Self { songs })
    }

    fn song(&self, song_id: &SongId) -> Option<&CatalogSong> {
        self.songs.get(song_id)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Admission {
    pub(crate) actor_id: ActorId,
    pub(crate) resume_token: ResumeToken,
    pub(crate) invitation_token: InvitationToken,
    pub(crate) next_expected_command_seq: CommandSeq,
    pub(crate) superseded_session: Option<(SessionId, SessionOutbound)>,
    activation: Activation,
}

#[derive(Debug, Clone)]
enum Activation {
    Joined(CommandAck),
    Resumed(Vec<CommandAck>),
}

#[derive(Debug, Clone)]
pub(crate) struct ControlResult {
    pub(crate) ack: CommandAck,
    pub(crate) left_room: bool,
    pub(crate) command_consumed: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum RoomLifecycleEvent {
    Closed {
        room_code: RoomCode,
        generation: u64,
    },
}

#[derive(Clone)]
pub(crate) struct RoomHandle {
    room_code: RoomCode,
    generation: u64,
    command_tx: mpsc::Sender<RoomCommand>,
}

const REQUEST_PENDING: u8 = 0;
const REQUEST_CLAIMED: u8 = 1;
const REQUEST_CANCELLED: u8 = 2;

#[derive(Clone)]
struct RequestGate(Arc<AtomicU8>);

impl RequestGate {
    fn new() -> Self {
        Self(Arc::new(AtomicU8::new(REQUEST_PENDING)))
    }

    fn claim(&self) -> bool {
        self.0
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_CLAIMED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_CANCELLED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }
}

impl RoomHandle {
    pub(crate) fn room_code(&self) -> &RoomCode {
        &self.room_code
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    async fn request<T>(
        &self,
        build: impl FnOnce(RequestGate, oneshot::Sender<Result<T, ProtocolError>>) -> RoomCommand,
    ) -> Result<T, ProtocolError> {
        self.request_with_timeout(ROOM_REQUEST_TIMEOUT, build).await
    }

    async fn request_with_timeout<T>(
        &self,
        request_timeout: std::time::Duration,
        build: impl FnOnce(RequestGate, oneshot::Sender<Result<T, ProtocolError>>) -> RoomCommand,
    ) -> Result<T, ProtocolError> {
        // The gate is the request's linearization point. A deadline may cancel
        // only a still-pending command; after the actor claims it, the caller
        // waits for the authoritative reply so it can never observe a timeout
        // followed by a hidden state mutation.
        let gate = RequestGate::new();
        let (reply_tx, mut reply_rx) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + request_timeout;
        tokio::time::timeout_at(
            deadline,
            self.command_tx.send(build(gate.clone(), reply_tx)),
        )
        .await
        .map_err(|_| protocol_error(ProtocolErrorCode::ServerBusy, "room is busy", true))?
        .map_err(|_| room_closed_error())?;
        match tokio::time::timeout_at(deadline, &mut reply_rx).await {
            Ok(reply) => reply.map_err(|_| room_closed_error())?,
            Err(_) if gate.cancel() => Err(protocol_error(
                ProtocolErrorCode::ServerBusy,
                "room request expired before execution",
                true,
            )),
            Err(_) => reply_rx.await.map_err(|_| room_closed_error())?,
        }
    }

    pub(crate) async fn join(
        &self,
        endpoint: SessionEndpoint,
        role: JoinRole,
        invitation_token: InvitationToken,
        envelope: CommandEnvelope,
        creator: bool,
    ) -> Result<Admission, ProtocolError> {
        self.request(|gate, reply| RoomCommand::Join {
            endpoint,
            role,
            invitation_token,
            envelope,
            creator,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn resume(
        &self,
        endpoint: SessionEndpoint,
        request: ResumeRequest,
    ) -> Result<Admission, ProtocolError> {
        self.request(|gate, reply| RoomCommand::Resume {
            endpoint,
            request,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn activate(&self, admission: Admission) -> Result<(), ProtocolError> {
        self.request(|gate, reply| RoomCommand::Activate {
            admission,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn control(
        &self,
        actor_id: ActorId,
        session_id: SessionId,
        envelope: CommandEnvelope,
        verified_clock: VerifiedClockQuality,
    ) -> Result<ControlResult, ProtocolError> {
        self.request(|gate, reply| RoomCommand::Control {
            actor_id,
            session_id,
            envelope,
            verified_clock,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn input(
        &self,
        actor_id: ActorId,
        session_id: SessionId,
        batch: InputBatch,
    ) -> Result<InputAck, ProtocolError> {
        self.request(|gate, reply| RoomCommand::Input {
            actor_id,
            session_id,
            batch,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn heartbeat(
        &self,
        actor_id: ActorId,
        session_id: SessionId,
    ) -> Result<(), ProtocolError> {
        self.request(|gate, reply| RoomCommand::Heartbeat {
            actor_id,
            session_id,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn update_clock_evidence(
        &self,
        actor_id: ActorId,
        session_id: SessionId,
        verified_clock: VerifiedClockQuality,
    ) -> Result<(), ProtocolError> {
        self.request(|gate, reply| RoomCommand::ClockEvidence {
            actor_id,
            session_id,
            verified_clock,
            gate,
            reply,
        })
        .await
    }

    pub(crate) async fn disconnect(&self, actor_id: ActorId, session_id: SessionId) {
        let _ = self
            .command_tx
            .send(RoomCommand::Disconnect {
                actor_id,
                session_id,
            })
            .await;
    }
}

pub(crate) fn spawn_room(
    room_code: RoomCode,
    generation: u64,
    invitation_token: InvitationToken,
    catalog: Arc<MatchCatalog>,
    authoritative_catalog: crate::AuthoritativeCatalog,
    clock: ProcessClock,
    lifecycle_tx: mpsc::Sender<RoomLifecycleEvent>,
) -> RoomHandle {
    let (command_tx, command_rx) = mpsc::channel(ROOM_MAILBOX_CAPACITY);
    let handle = RoomHandle {
        room_code: room_code.clone(),
        generation,
        command_tx,
    };
    let actor = RoomActor {
        generation,
        core: RoomCore::new(
            room_code,
            invitation_token,
            catalog,
            authoritative_catalog,
            clock,
            Instant::now(),
        ),
        command_rx,
        lifecycle_tx,
    };
    tokio::spawn(actor.run());
    handle
}

enum RoomCommand {
    Join {
        endpoint: SessionEndpoint,
        role: JoinRole,
        invitation_token: InvitationToken,
        envelope: CommandEnvelope,
        creator: bool,
        gate: RequestGate,
        reply: oneshot::Sender<Result<Admission, ProtocolError>>,
    },
    Resume {
        endpoint: SessionEndpoint,
        request: ResumeRequest,
        gate: RequestGate,
        reply: oneshot::Sender<Result<Admission, ProtocolError>>,
    },
    Activate {
        admission: Admission,
        gate: RequestGate,
        reply: oneshot::Sender<Result<(), ProtocolError>>,
    },
    Control {
        actor_id: ActorId,
        session_id: SessionId,
        envelope: CommandEnvelope,
        verified_clock: VerifiedClockQuality,
        gate: RequestGate,
        reply: oneshot::Sender<Result<ControlResult, ProtocolError>>,
    },
    Input {
        actor_id: ActorId,
        session_id: SessionId,
        batch: InputBatch,
        gate: RequestGate,
        reply: oneshot::Sender<Result<InputAck, ProtocolError>>,
    },
    Heartbeat {
        actor_id: ActorId,
        session_id: SessionId,
        gate: RequestGate,
        reply: oneshot::Sender<Result<(), ProtocolError>>,
    },
    ClockEvidence {
        actor_id: ActorId,
        session_id: SessionId,
        verified_clock: VerifiedClockQuality,
        gate: RequestGate,
        reply: oneshot::Sender<Result<(), ProtocolError>>,
    },
    Disconnect {
        actor_id: ActorId,
        session_id: SessionId,
    },
}

impl RoomCommand {
    fn claim(&self) -> bool {
        match self {
            Self::Join { gate, .. }
            | Self::Resume { gate, .. }
            | Self::Activate { gate, .. }
            | Self::Control { gate, .. }
            | Self::Input { gate, .. }
            | Self::Heartbeat { gate, .. }
            | Self::ClockEvidence { gate, .. } => gate.claim(),
            Self::Disconnect { .. } => true,
        }
    }
}

struct RoomActor {
    generation: u64,
    core: RoomCore,
    command_rx: mpsc::Receiver<RoomCommand>,
    lifecycle_tx: mpsc::Sender<RoomLifecycleEvent>,
}

impl RoomActor {
    async fn run(mut self) {
        while !self.core.closed {
            let deadline = self.core.next_deadline();
            let timer = tokio::time::sleep_until(deadline.into());
            tokio::pin!(timer);
            tokio::select! {
                biased;
                _ = &mut timer => self.core.advance_time(Instant::now()),
                command = self.command_rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    self.handle(command, Instant::now());
                }
            }
            self.core.flush_snapshots(Instant::now());
        }

        self.core.close_all(room_closed_error());
        let _ = self
            .lifecycle_tx
            .send(RoomLifecycleEvent::Closed {
                room_code: self.core.room_code.clone(),
                generation: self.generation,
            })
            .await;
    }

    fn handle(&mut self, command: RoomCommand, now: Instant) {
        if !command.claim() {
            return;
        }
        self.core.advance_time(now);
        match command {
            RoomCommand::Join {
                endpoint,
                role,
                invitation_token,
                envelope,
                creator,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.join(
                    endpoint,
                    role,
                    &invitation_token,
                    envelope,
                    creator,
                    now,
                ));
            }
            RoomCommand::Resume {
                endpoint,
                request,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.resume(endpoint, request, now));
            }
            RoomCommand::Activate {
                admission,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.activate(admission, now));
            }
            RoomCommand::Control {
                actor_id,
                session_id,
                envelope,
                verified_clock,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.control(
                    &actor_id,
                    session_id,
                    envelope,
                    verified_clock,
                    now,
                ));
            }
            RoomCommand::Input {
                actor_id,
                session_id,
                batch,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.input(&actor_id, session_id, batch, now));
            }
            RoomCommand::Heartbeat {
                actor_id,
                session_id,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.heartbeat(&actor_id, session_id, now));
            }
            RoomCommand::ClockEvidence {
                actor_id,
                session_id,
                verified_clock,
                gate: _,
                reply,
            } => {
                let _ = reply.send(self.core.update_clock_evidence(
                    &actor_id,
                    session_id,
                    verified_clock,
                ));
            }
            RoomCommand::Disconnect {
                actor_id,
                session_id,
            } => self.core.disconnect(&actor_id, session_id, now),
        }
    }
}

#[derive(Clone)]
struct Link {
    resume_token: ResumeToken,
    active_session_id: SessionId,
    active_endpoint: Option<SessionEndpoint>,
    pending_endpoint: Option<SessionEndpoint>,
    reconnect_deadline: Option<Instant>,
    lease_deadline: Option<Instant>,
}

impl Link {
    fn pending(endpoint: SessionEndpoint, resume_token: ResumeToken, now: Instant) -> Self {
        Self {
            resume_token,
            active_session_id: endpoint.session_id,
            active_endpoint: None,
            pending_endpoint: Some(endpoint),
            reconnect_deadline: None,
            lease_deadline: Some(now + SESSION_LEASE),
        }
    }

    fn is_online(&self) -> bool {
        self.active_endpoint.is_some() || self.pending_endpoint.is_some()
    }

    fn can_resume_at(&self, now: Instant) -> bool {
        self.is_online()
            || self
                .reconnect_deadline
                .is_some_and(|deadline| now < deadline)
    }

    fn endpoint(&self) -> Option<&SessionEndpoint> {
        self.active_endpoint
            .as_ref()
            .or(self.pending_endpoint.as_ref())
    }

    fn activate(&mut self, session_id: SessionId) -> Result<SessionEndpoint, ProtocolError> {
        if self.active_session_id != session_id {
            return Err(protocol_error(
                ProtocolErrorCode::SessionSuperseded,
                "session was superseded",
                false,
            ));
        }
        let endpoint = self.pending_endpoint.take().ok_or_else(|| {
            protocol_error(
                ProtocolErrorCode::InvalidMessage,
                "membership is not awaiting activation",
                false,
            )
        })?;
        self.active_endpoint = Some(endpoint.clone());
        Ok(endpoint)
    }

    fn replace(
        &mut self,
        endpoint: SessionEndpoint,
        now: Instant,
    ) -> Option<(SessionId, SessionOutbound)> {
        let superseded = self
            .active_endpoint
            .take()
            .or_else(|| self.pending_endpoint.take())
            .map(|old| (old.session_id, old.outbound));
        self.active_session_id = endpoint.session_id;
        self.pending_endpoint = Some(endpoint);
        self.reconnect_deadline = None;
        self.lease_deadline = Some(now + SESSION_LEASE);
        superseded
    }

    fn disconnect(&mut self, session_id: SessionId, now: Instant) -> bool {
        if self.active_session_id != session_id || !self.is_online() {
            return false;
        }
        self.active_endpoint = None;
        self.pending_endpoint = None;
        self.lease_deadline = None;
        self.reconnect_deadline = Some(now + RECONNECT_GRACE);
        true
    }

    fn renew(&mut self, session_id: SessionId, now: Instant) -> Result<(), ProtocolError> {
        if self.active_session_id != session_id || !self.is_online() {
            return Err(protocol_error(
                ProtocolErrorCode::SessionSuperseded,
                "session was superseded",
                false,
            ));
        }
        self.lease_deadline = Some(now + SESSION_LEASE);
        Ok(())
    }

    fn connection(&self, clock: &ProcessClock) -> PlayerConnection {
        if self.is_online() {
            PlayerConnection::Online
        } else if let Some(deadline) = self.reconnect_deadline {
            PlayerConnection::Reconnecting {
                grace_deadline_server_us: clock.server_us(deadline),
            }
        } else {
            PlayerConnection::Dnf
        }
    }
}

#[derive(Clone)]
struct CachedCommand {
    envelope: CommandEnvelope,
    ack: CommandAck,
}

#[derive(Clone)]
struct CommandWindow {
    next_expected: CommandSeq,
    cache: VecDeque<CachedCommand>,
}

impl CommandWindow {
    fn with_admission(envelope: CommandEnvelope, ack: CommandAck) -> Self {
        let mut cache = VecDeque::with_capacity(COMMAND_ACK_CACHE_CAPACITY);
        let next_expected = ack.next_expected_seq;
        cache.push_back(CachedCommand { envelope, ack });
        Self {
            next_expected,
            cache,
        }
    }

    fn duplicate(&self, envelope: &CommandEnvelope) -> Result<Option<CommandAck>, ProtocolError> {
        if envelope.seq >= self.next_expected {
            return Ok(None);
        }
        let Some(cached) = self
            .cache
            .iter()
            .find(|cached| cached.envelope.seq == envelope.seq)
        else {
            return Err(protocol_error(
                ProtocolErrorCode::SequenceGap,
                "command is older than the bounded replay window",
                false,
            ));
        };
        if cached.envelope != *envelope {
            return Err(protocol_error(
                ProtocolErrorCode::InvalidMessage,
                "command sequence was reused with different contents",
                false,
            ));
        }
        Ok(Some(cached.ack.clone()))
    }

    fn record(&mut self, envelope: CommandEnvelope, ack: CommandAck) {
        self.next_expected = ack.next_expected_seq;
        self.cache.push_back(CachedCommand { envelope, ack });
        if self.cache.len() > COMMAND_ACK_CACHE_CAPACITY {
            self.cache.pop_front();
        }
    }

    fn replay_after(&self, acknowledged: CommandSeq) -> Vec<CommandAck> {
        self.cache
            .iter()
            .filter(|cached| cached.ack.seq > acknowledged)
            .map(|cached| cached.ack.clone())
            .collect()
    }

    fn can_replay_after(&self, acknowledged: CommandSeq) -> bool {
        self.cache.front().is_none_or(|oldest| {
            acknowledged
                .0
                .checked_add(1)
                .is_some_and(|next| next >= oldest.envelope.seq.0)
        })
    }
}

#[derive(Clone)]
struct Player {
    id: PlayerId,
    name: DisplayName,
    joined_order: u64,
    link: Link,
    preparation: PlayerPreparation,
    last_acked_input: Option<InputSeq>,
    commands: CommandWindow,
    clock_evidence: Option<VerifiedClockQuality>,
    dnf: bool,
    departed: bool,
}

#[derive(Clone)]
struct Spectator {
    id: SpectatorId,
    name: DisplayName,
    link: Link,
    commands: CommandWindow,
}

#[derive(Clone)]
enum StageState {
    Lobby,
    Preparing {
        match_id: MatchId,
        song: SongManifest,
    },
    Countdown {
        manifest: MatchManifest,
        start_at: Instant,
    },
    Playing {
        manifest: MatchManifest,
        start_at: Instant,
        timeout_at: Instant,
    },
    Finalizing {
        manifest: MatchManifest,
        deadline: Instant,
    },
    Finished {
        manifest: MatchManifest,
        results: BoundedVec<FinalResult, MAX_PLAYERS>,
        finished_at: Instant,
    },
}

impl StageState {
    fn match_id(&self) -> Option<MatchId> {
        match self {
            Self::Lobby => None,
            Self::Preparing { match_id, .. } => Some(*match_id),
            Self::Countdown { manifest, .. }
            | Self::Playing { manifest, .. }
            | Self::Finalizing { manifest, .. }
            | Self::Finished { manifest, .. } => Some(manifest.match_id),
        }
    }
}

struct RoomCore {
    room_code: RoomCode,
    invitation_token: InvitationToken,
    catalog: Arc<MatchCatalog>,
    authoritative_catalog: crate::AuthoritativeCatalog,
    match_runtime: Option<AuthoritativeMatchRuntime>,
    next_live_state_at: Option<Instant>,
    clock: ProcessClock,
    stage: StageState,
    revision: RoomRevision,
    next_match_id: MatchId,
    next_player_id: u64,
    next_spectator_id: u64,
    next_join_order: u64,
    leader: Option<PlayerId>,
    players: Vec<Player>,
    spectators: Vec<Spectator>,
    created_at: Instant,
    last_activity_at: Instant,
    ever_had_player: bool,
    snapshot_dirty: bool,
    closed: bool,
}

impl RoomCore {
    fn new(
        room_code: RoomCode,
        invitation_token: InvitationToken,
        catalog: Arc<MatchCatalog>,
        authoritative_catalog: crate::AuthoritativeCatalog,
        clock: ProcessClock,
        now: Instant,
    ) -> Self {
        Self {
            room_code,
            invitation_token,
            catalog,
            authoritative_catalog,
            match_runtime: None,
            next_live_state_at: None,
            clock,
            stage: StageState::Lobby,
            revision: RoomRevision(FIRST_ROOM_REVISION.0 - 1),
            next_match_id: FIRST_MATCH_ID,
            next_player_id: 1,
            next_spectator_id: 1,
            next_join_order: 1,
            leader: None,
            players: Vec::new(),
            spectators: Vec::new(),
            created_at: now,
            last_activity_at: now,
            ever_had_player: false,
            snapshot_dirty: false,
            closed: false,
        }
    }

    fn join(
        &mut self,
        endpoint: SessionEndpoint,
        role: JoinRole,
        invitation_token: &InvitationToken,
        envelope: CommandEnvelope,
        creator: bool,
        now: Instant,
    ) -> Result<Admission, ProtocolError> {
        if invitation_token != &self.invitation_token {
            return Err(protocol_error(
                ProtocolErrorCode::InvalidInvitation,
                "invitation token is invalid",
                false,
            ));
        }
        let expected_command = if creator {
            matches!(
                envelope.command,
                taiko_multiplayer_protocol::ClientCommand::CreateRoom
            )
        } else {
            matches!(
                envelope.command,
                taiko_multiplayer_protocol::ClientCommand::JoinRoom { .. }
            )
        };
        if !expected_command {
            return Err(protocol_error(
                ProtocolErrorCode::InvalidMessage,
                "admission command does not match the requested operation",
                false,
            ));
        }
        if creator
            && (role != JoinRole::Player || !self.players.is_empty() || !self.spectators.is_empty())
        {
            return Err(protocol_error(
                ProtocolErrorCode::InvalidMessage,
                "room creator must be the first player",
                false,
            ));
        }

        let resume_token = generate_resume_token()?;
        let actor_id = match role {
            JoinRole::Player => {
                if self.players.len() >= ROOM_MAX_PLAYERS {
                    return Err(protocol_error(
                        ProtocolErrorCode::RoomFull,
                        "player capacity reached",
                        false,
                    ));
                }
                if !matches!(self.stage, StageState::Lobby | StageState::Preparing { .. }) {
                    return Err(protocol_error(
                        ProtocolErrorCode::InvalidStage,
                        "players cannot join during an active or finished match",
                        false,
                    ));
                }
                let id = PlayerId(self.allocate_player_id()?);
                let actor_id = ActorId::Player(id);
                self.bump_revision()?;
                let ack = applied_ack(envelope.seq, next_command_seq(envelope.seq)?, self.revision);
                let joined_order = self.allocate_join_order()?;
                self.players.push(Player {
                    id,
                    name: endpoint.name.clone(),
                    joined_order,
                    link: Link::pending(endpoint, resume_token.clone(), now),
                    preparation: PlayerPreparation::Selecting,
                    last_acked_input: None,
                    commands: CommandWindow::with_admission(envelope.clone(), ack),
                    clock_evidence: None,
                    dnf: false,
                    departed: false,
                });
                self.ever_had_player = true;
                if self.leader.is_none() {
                    self.leader = Some(id);
                }
                actor_id
            }
            JoinRole::Spectator => {
                if self.spectators.len() >= ROOM_MAX_SPECTATORS {
                    return Err(protocol_error(
                        ProtocolErrorCode::SpectatorFull,
                        "spectator capacity reached",
                        false,
                    ));
                }
                let id = SpectatorId(self.allocate_spectator_id()?);
                let actor_id = ActorId::Spectator(id);
                self.bump_revision()?;
                let ack = applied_ack(envelope.seq, next_command_seq(envelope.seq)?, self.revision);
                self.spectators.push(Spectator {
                    id,
                    name: endpoint.name.clone(),
                    link: Link::pending(endpoint, resume_token.clone(), now),
                    commands: CommandWindow::with_admission(envelope.clone(), ack),
                });
                actor_id
            }
        };

        self.last_activity_at = now;
        self.snapshot_dirty = true;
        let ack = self
            .command_window(&actor_id)?
            .cache
            .back()
            .expect("admission ack was cached")
            .ack
            .clone();
        Ok(Admission {
            actor_id,
            resume_token,
            invitation_token: self.invitation_token.clone(),
            next_expected_command_seq: ack.next_expected_seq,
            superseded_session: None,
            activation: Activation::Joined(ack),
        })
    }

    fn resume(
        &mut self,
        endpoint: SessionEndpoint,
        request: ResumeRequest,
        now: Instant,
    ) -> Result<Admission, ProtocolError> {
        if request.room_code != self.room_code {
            return Err(protocol_error(
                ProtocolErrorCode::ResumeRejected,
                "resume room does not match",
                false,
            ));
        }
        let (token, deadline, resume_eligible, next_expected, can_replay, replay) =
            match &request.actor_id {
                ActorId::Player(id) => {
                    let player = self.player(*id)?;
                    (
                        player.link.resume_token.clone(),
                        player.link.reconnect_deadline,
                        !player.departed && player.link.can_resume_at(now),
                        player.commands.next_expected,
                        player
                            .commands
                            .can_replay_after(request.last_acked_command_seq),
                        player.commands.replay_after(request.last_acked_command_seq),
                    )
                }
                ActorId::Spectator(id) => {
                    let spectator = self.spectator(*id)?;
                    (
                        spectator.link.resume_token.clone(),
                        spectator.link.reconnect_deadline,
                        spectator.link.can_resume_at(now),
                        spectator.commands.next_expected,
                        spectator
                            .commands
                            .can_replay_after(request.last_acked_command_seq),
                        spectator
                            .commands
                            .replay_after(request.last_acked_command_seq),
                    )
                }
            };
        if token != request.token
            || !resume_eligible
            || deadline.is_some_and(|deadline| now >= deadline)
            || request.last_room_revision > self.revision
            || request.last_acked_command_seq.0 >= next_expected.0
            || !can_replay
        {
            return Err(protocol_error(
                ProtocolErrorCode::ResumeRejected,
                "resume credentials or sequence watermarks are invalid",
                false,
            ));
        }

        let was_reconnecting = deadline.is_some();
        let is_pre_start = matches!(self.stage, StageState::Preparing { .. });
        let (superseded_session, readiness_downgraded) = match &request.actor_id {
            ActorId::Player(id) => {
                let player = self.player_mut(*id)?;
                player.clock_evidence = None;
                let readiness_downgraded = if is_pre_start {
                    match player.preparation {
                        PlayerPreparation::Ready { selection } => {
                            player.preparation = PlayerPreparation::Prepared { selection };
                            true
                        }
                        _ => false,
                    }
                } else {
                    false
                };
                (player.link.replace(endpoint, now), readiness_downgraded)
            }
            ActorId::Spectator(id) => (self.spectator_mut(*id)?.link.replace(endpoint, now), false),
        };
        if was_reconnecting || readiness_downgraded {
            self.bump_revision()?;
            self.reconcile_leader();
            self.snapshot_dirty = true;
        }
        self.last_activity_at = now;
        Ok(Admission {
            actor_id: request.actor_id,
            resume_token: token,
            invitation_token: self.invitation_token.clone(),
            next_expected_command_seq: next_expected,
            superseded_session,
            activation: Activation::Resumed(replay),
        })
    }

    fn activate(&mut self, admission: Admission, now: Instant) -> Result<(), ProtocolError> {
        let session_id = self
            .link(&admission.actor_id)?
            .pending_endpoint
            .as_ref()
            .ok_or_else(|| {
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "membership has no pending transport",
                    false,
                )
            })?
            .session_id;
        let endpoint = self.link_mut(&admission.actor_id)?.activate(session_id)?;
        let granted = ServerMessage::MembershipGranted(MembershipGranted {
            room_code: self.room_code.clone(),
            actor_id: admission.actor_id.clone(),
            resume_token: admission.resume_token,
            invitation_token: admission.invitation_token,
        });
        let mut messages = vec![granted];
        match admission.activation {
            Activation::Joined(ack) => messages.push(ServerMessage::CommandAck(ack)),
            Activation::Resumed(acks) => {
                messages.extend(acks.into_iter().map(ServerMessage::CommandAck));
            }
        }
        messages.push(ServerMessage::RoomSnapshot(Box::new(self.snapshot(now)?)));
        for message in messages {
            if endpoint.outbound.send_reliable(message).is_err() {
                endpoint.outbound.close(slow_consumer_error());
                self.disconnect(&admission.actor_id, session_id, now);
                return Err(slow_consumer_error());
            }
        }
        Ok(())
    }

    fn control(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        envelope: CommandEnvelope,
        verified_clock: VerifiedClockQuality,
        now: Instant,
    ) -> Result<ControlResult, ProtocolError> {
        self.ensure_active_session(actor_id, session_id)?;
        let response_endpoint = self
            .link(actor_id)?
            .active_endpoint
            .clone()
            .expect("active session has an endpoint");
        if let Some(ack) = self.command_window(actor_id)?.duplicate(&envelope)? {
            self.send_via_endpoint(
                actor_id,
                response_endpoint,
                ServerMessage::CommandAck(ack.clone()),
                now,
            );
            return Ok(ControlResult {
                ack,
                left_room: false,
                command_consumed: false,
            });
        }
        let next_expected = self.command_window(actor_id)?.next_expected;
        if envelope.seq != next_expected {
            let ack = rejected_ack(
                envelope.seq,
                next_expected,
                protocol_error(
                    ProtocolErrorCode::SequenceGap,
                    "command sequence is not contiguous",
                    true,
                ),
                Some(self.revision),
            );
            self.send_via_endpoint(
                actor_id,
                response_endpoint,
                ServerMessage::CommandAck(ack.clone()),
                now,
            );
            return Ok(ControlResult {
                ack,
                left_room: false,
                command_consumed: false,
            });
        }

        let following = next_command_seq(envelope.seq)?;
        let outcome = if envelope
            .expected_room_revision
            .is_some_and(|expected| expected != self.revision)
        {
            Err(protocol_error(
                ProtocolErrorCode::StaleRevision,
                "room revision is stale",
                true,
            ))
        } else {
            self.apply_command(actor_id, &envelope.command, verified_clock, now)
        };
        let (ack, left_room) = match outcome {
            Ok(left_room) => (
                applied_ack(envelope.seq, following, self.revision),
                left_room,
            ),
            Err(error) => (
                rejected_ack(envelope.seq, following, error, Some(self.revision)),
                false,
            ),
        };

        if !left_room {
            self.command_window_mut(actor_id)?
                .record(envelope, ack.clone());
        }
        self.send_via_endpoint(
            actor_id,
            response_endpoint,
            ServerMessage::CommandAck(ack.clone()),
            now,
        );
        self.last_activity_at = now;
        Ok(ControlResult {
            ack,
            left_room,
            command_consumed: true,
        })
    }

    fn input(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        batch: InputBatch,
        now: Instant,
    ) -> Result<InputAck, ProtocolError> {
        self.ensure_active_session(actor_id, session_id)?;
        let ActorId::Player(player_id) = actor_id else {
            return Err(protocol_error(
                ProtocolErrorCode::PermissionDenied,
                "spectators cannot submit input",
                false,
            ));
        };
        let expected_match = self.stage.match_id();
        if expected_match != Some(batch.match_id) {
            let error = protocol_error(
                ProtocolErrorCode::StaleMatch,
                "input references a stale match",
                false,
            );
            return self.reject_input(actor_id, session_id, batch.match_id, error, now);
        }
        let is_playing = matches!(self.stage, StageState::Playing { .. });
        if !matches!(
            self.stage,
            StageState::Playing { .. }
                | StageState::Finalizing { .. }
                | StageState::Finished { .. }
        ) {
            let error = protocol_error(
                ProtocolErrorCode::InvalidStage,
                "input is only accepted while playing",
                false,
            );
            return self.reject_input(actor_id, session_id, batch.match_id, error, now);
        }

        if is_playing {
            if let Err(error) = self
                .match_runtime
                .as_mut()
                .ok_or_else(missing_match_runtime_error)?
                .synchronize(now)
            {
                return self.reject_runtime_input(actor_id, session_id, &batch, error, now);
            }
        }

        let acceptance = match self
            .match_runtime
            .as_mut()
            .expect("runtime existence was established")
            .accept_input(*player_id, &batch, now)
        {
            Ok(acceptance) => acceptance,
            Err(error) => {
                return self.reject_runtime_input(actor_id, session_id, &batch, error, now);
            }
        };
        self.player_mut(*player_id)?.last_acked_input = acceptance.highest_contiguous_seq;
        let outcome = acceptance
            .first_dropped_reason
            .map_or(InputOutcome::Accepted, |reason| InputOutcome::Rejected {
                error: input_drop_protocol_error(reason),
            });
        let ack = InputAck {
            match_id: batch.match_id,
            highest_contiguous_seq: acceptance.highest_contiguous_seq,
            next_expected_seq: acceptance.next_expected_seq,
            server_tick: acceptance.server_tick,
            outcome,
        };
        self.send_to_actor(
            actor_id,
            session_id,
            ServerMessage::InputAck(ack.clone()),
            now,
        );
        Ok(ack)
    }

    fn reject_input(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        match_id: MatchId,
        error: ProtocolError,
        now: Instant,
    ) -> Result<InputAck, ProtocolError> {
        let last = match actor_id {
            ActorId::Player(player_id) => self.player(*player_id)?.last_acked_input,
            ActorId::Spectator(_) => None,
        };
        let next_expected_seq =
            InputSeq(last.map_or(FIRST_INPUT_SEQ.0, |seq| seq.0.saturating_add(1)));
        let server_tick = match self.match_runtime.as_ref() {
            Some(runtime) => match runtime.current_server_tick(now) {
                Ok(tick) => tick,
                Err(MatchRuntimeError::MatchNotStarted) => 0,
                Err(error) => return Err(runtime_input_error(&error)),
            },
            None => 0,
        };
        let ack = InputAck {
            match_id,
            highest_contiguous_seq: last,
            next_expected_seq,
            server_tick,
            outcome: InputOutcome::Rejected { error },
        };
        self.send_to_actor(
            actor_id,
            session_id,
            ServerMessage::InputAck(ack.clone()),
            now,
        );
        Ok(ack)
    }

    fn reject_runtime_input(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        batch: &InputBatch,
        error: MatchRuntimeError,
        now: Instant,
    ) -> Result<InputAck, ProtocolError> {
        let fatal = runtime_error_is_fatal(&error);
        let protocol = runtime_input_error(&error);
        if let (ActorId::Player(player_id), Some(reason)) =
            (actor_id, recoverable_input_drop_reason(&error))
        {
            let acceptance = self
                .match_runtime
                .as_mut()
                .ok_or_else(missing_match_runtime_error)?
                .discard_input(*player_id, batch, now, reason)
                .map_err(|discard_error| runtime_input_error(&discard_error))?;
            self.player_mut(*player_id)?.last_acked_input = acceptance.highest_contiguous_seq;
            let ack = InputAck {
                match_id: batch.match_id,
                highest_contiguous_seq: acceptance.highest_contiguous_seq,
                next_expected_seq: acceptance.next_expected_seq,
                server_tick: acceptance.server_tick,
                outcome: InputOutcome::Rejected { error: protocol },
            };
            self.send_to_actor(
                actor_id,
                session_id,
                ServerMessage::InputAck(ack.clone()),
                now,
            );
            return Ok(ack);
        }
        let ack = self.reject_input(actor_id, session_id, batch.match_id, protocol, now)?;
        if fatal {
            self.closed = true;
        }
        Ok(ack)
    }

    fn heartbeat(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        now: Instant,
    ) -> Result<(), ProtocolError> {
        self.link_mut(actor_id)?.renew(session_id, now)
    }

    fn update_clock_evidence(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        verified_clock: VerifiedClockQuality,
    ) -> Result<(), ProtocolError> {
        self.ensure_active_session(actor_id, session_id)?;
        if let ActorId::Player(player_id) = actor_id {
            self.player_mut(*player_id)?.clock_evidence = Some(verified_clock);
        }
        Ok(())
    }

    fn disconnect(&mut self, actor_id: &ActorId, session_id: SessionId, now: Instant) {
        let disconnected = self
            .link_mut(actor_id)
            .is_ok_and(|link| link.disconnect(session_id, now));
        if !disconnected {
            return;
        }
        if let ActorId::Player(player_id) = actor_id {
            if let Ok(player) = self.player_mut(*player_id) {
                player.clock_evidence = None;
            }
        }
        if matches!(self.stage, StageState::Countdown { .. })
            && matches!(actor_id, ActorId::Player(_))
        {
            self.cancel_countdown();
        }
        if self.bump_revision().is_err() {
            self.closed = true;
            return;
        }
        self.reconcile_leader();
        self.snapshot_dirty = true;
    }

    fn apply_command(
        &mut self,
        actor_id: &ActorId,
        command: &taiko_multiplayer_protocol::ClientCommand,
        verified_clock: VerifiedClockQuality,
        now: Instant,
    ) -> Result<bool, ProtocolError> {
        use taiko_multiplayer_protocol::ClientCommand;

        match command {
            ClientCommand::CreateRoom | ClientCommand::JoinRoom { .. } => Err(protocol_error(
                ProtocolErrorCode::AlreadyMember,
                "session is already a room member",
                false,
            )),
            ClientCommand::LeaveRoom => {
                self.leave(actor_id, now)?;
                Ok(true)
            }
            ClientCommand::SelectSong { song_id } => {
                self.select_song(actor_id, song_id, now)?;
                Ok(false)
            }
            ClientCommand::SelectCourse {
                match_id,
                selection,
            } => {
                self.select_course(actor_id, *match_id, *selection, now)?;
                Ok(false)
            }
            ClientCommand::ReportPreparation { match_id, progress } => {
                self.report_preparation(actor_id, *match_id, progress.clone(), now)?;
                Ok(false)
            }
            ClientCommand::SetReady {
                match_id,
                ready,
                proof,
            } => {
                self.set_ready(
                    actor_id,
                    *match_id,
                    *ready,
                    proof.as_ref(),
                    verified_clock,
                    now,
                )?;
                Ok(false)
            }
            ClientCommand::StartMatch { match_id } => {
                self.start_match(actor_id, *match_id, verified_clock, now)?;
                Ok(false)
            }
            ClientCommand::Rematch { previous_match_id } => {
                self.rematch(actor_id, *previous_match_id, now)?;
                Ok(false)
            }
            ClientCommand::ReturnToLobby { match_id } => {
                self.return_to_lobby(actor_id, *match_id, now)?;
                Ok(false)
            }
        }
    }

    fn leave(&mut self, actor_id: &ActorId, now: Instant) -> Result<(), ProtocolError> {
        match actor_id {
            ActorId::Player(id) => {
                let Some(index) = self.players.iter().position(|player| player.id == *id) else {
                    return Err(not_member_error());
                };
                if matches!(
                    self.stage,
                    StageState::Playing { .. }
                        | StageState::Finalizing { .. }
                        | StageState::Finished { .. }
                ) {
                    let player = &mut self.players[index];
                    player.link.active_endpoint = None;
                    player.link.pending_endpoint = None;
                    player.link.reconnect_deadline = None;
                    player.link.lease_deadline = None;
                    player.departed = true;
                    if matches!(
                        self.stage,
                        StageState::Playing { .. } | StageState::Finalizing { .. }
                    ) {
                        if let Err(error) = self.mark_runtime_dnf(*id, now) {
                            let protocol = runtime_input_error(&error);
                            self.fail_runtime(error);
                            return Err(protocol);
                        }
                    }
                } else {
                    self.players.remove(index);
                    if matches!(self.stage, StageState::Countdown { .. }) {
                        self.cancel_countdown();
                    }
                }
                self.reconcile_leader();
                if self.players.is_empty() {
                    self.closed = true;
                }
            }
            ActorId::Spectator(id) => {
                let Some(index) = self
                    .spectators
                    .iter()
                    .position(|spectator| spectator.id == *id)
                else {
                    return Err(not_member_error());
                };
                self.spectators.remove(index);
            }
        }
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn select_song(
        &mut self,
        actor_id: &ActorId,
        song_id: &SongId,
        _now: Instant,
    ) -> Result<(), ProtocolError> {
        self.require_leader(actor_id)?;
        if !matches!(self.stage, StageState::Lobby | StageState::Preparing { .. }) {
            return Err(invalid_stage_error());
        }
        let song = self
            .catalog
            .song(song_id)
            .ok_or_else(|| {
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "song id is not in the server catalog",
                    false,
                )
            })?
            .manifest
            .clone();
        let match_id = self.allocate_match_id()?;
        for player in &mut self.players {
            player.preparation = PlayerPreparation::Selecting;
            player.last_acked_input = None;
            player.dnf = false;
        }
        self.match_runtime = None;
        self.next_live_state_at = None;
        self.stage = StageState::Preparing { match_id, song };
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn select_course(
        &mut self,
        actor_id: &ActorId,
        match_id: MatchId,
        selection: PlayerSelection,
        _now: Instant,
    ) -> Result<(), ProtocolError> {
        let ActorId::Player(player_id) = actor_id else {
            return Err(permission_error());
        };
        let song = self.preparing_song(match_id)?.clone();
        self.validate_selection(&song, selection)?;
        let player = self.player_mut(*player_id)?;
        let next = PlayerPreparation::Downloading {
            selection,
            progress_milli: ProgressMilli::new(0).expect("zero progress is valid"),
        };
        if player.preparation == next {
            return Ok(());
        }
        player.preparation = next;
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn report_preparation(
        &mut self,
        actor_id: &ActorId,
        match_id: MatchId,
        progress: PreparationProgress,
        _now: Instant,
    ) -> Result<(), ProtocolError> {
        let ActorId::Player(player_id) = actor_id else {
            return Err(permission_error());
        };
        let song = self.preparing_song(match_id)?.clone();
        let current = self.player(*player_id)?.preparation.clone();
        let next = match progress {
            PreparationProgress::Downloading {
                selection,
                progress_milli,
            } => {
                self.validate_selection(&song, selection)?;
                let current_progress = match &current {
                    PlayerPreparation::Downloading {
                        selection: current_selection,
                        progress_milli,
                    } if *current_selection == selection => progress_milli.get(),
                    _ => {
                        return Err(protocol_error(
                            ProtocolErrorCode::InvalidStage,
                            "downloading progress requires the selected course",
                            false,
                        ));
                    }
                };
                if progress_milli.get() < current_progress {
                    return Err(protocol_error(
                        ProtocolErrorCode::InvalidMessage,
                        "preparation progress cannot move backwards",
                        false,
                    ));
                }
                PlayerPreparation::Downloading {
                    selection,
                    progress_milli,
                }
            }
            PreparationProgress::Verifying { selection } => {
                self.validate_selection(&song, selection)?;
                if !matches!(
                    current,
                    PlayerPreparation::Downloading {
                        selection: current_selection,
                        progress_milli,
                    } if current_selection == selection
                        && progress_milli.get() == ProgressMilli::MAX
                ) {
                    return Err(protocol_error(
                        ProtocolErrorCode::NotPrepared,
                        "download must complete before verification",
                        false,
                    ));
                }
                PlayerPreparation::Verifying { selection }
            }
            PreparationProgress::Loading { selection } => {
                self.validate_selection(&song, selection)?;
                if !matches!(
                    current,
                    PlayerPreparation::Verifying {
                        selection: current_selection
                    } if current_selection == selection
                ) {
                    return Err(protocol_error(
                        ProtocolErrorCode::NotPrepared,
                        "verification must complete before loading",
                        false,
                    ));
                }
                PlayerPreparation::Loading { selection }
            }
            PreparationProgress::Failed { selection, reason } => {
                if let Some(selection) = selection {
                    self.validate_selection(&song, selection)?;
                }
                PlayerPreparation::Failed { selection, reason }
            }
        };
        if current == next {
            return Ok(());
        }
        self.player_mut(*player_id)?.preparation = next;
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn set_ready(
        &mut self,
        actor_id: &ActorId,
        match_id: MatchId,
        ready: bool,
        proof: Option<&PreparationProof>,
        verified_clock: VerifiedClockQuality,
        now: Instant,
    ) -> Result<(), ProtocolError> {
        let ActorId::Player(player_id) = actor_id else {
            return Err(permission_error());
        };
        let song = self.preparing_song(match_id)?.clone();
        let current = self.player(*player_id)?.preparation.clone();
        if !ready {
            if proof.is_some() {
                return Err(protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "unready command must not include a proof",
                    false,
                ));
            }
            let PlayerPreparation::Ready { selection } = current else {
                return Ok(());
            };
            self.player_mut(*player_id)?.preparation = PlayerPreparation::Prepared { selection };
            self.bump_revision()?;
            self.snapshot_dirty = true;
            return Ok(());
        }

        let selection = match current {
            PlayerPreparation::Loading { selection }
            | PlayerPreparation::Prepared { selection }
            | PlayerPreparation::Ready { selection } => selection,
            _ => {
                return Err(protocol_error(
                    ProtocolErrorCode::NotPrepared,
                    "course must be loaded before becoming ready",
                    false,
                ));
            }
        };
        let proof = proof.ok_or_else(|| {
            protocol_error(
                ProtocolErrorCode::NotPrepared,
                "ready command requires a preparation proof",
                false,
            )
        })?;
        let assignment = self.assignment(*player_id, &song, selection)?;
        proof.validate_for(&song, &assignment).map_err(|_| {
            protocol_error(
                ProtocolErrorCode::NotPrepared,
                "preparation proof does not match the manifest",
                false,
            )
        })?;
        if !verified_clock.is_ready_at(now) {
            return Err(protocol_error(
                ProtocolErrorCode::ClockNotReady,
                "server-verified clock path quality is insufficient or expired",
                true,
            ));
        }
        self.player_mut(*player_id)?.clock_evidence = Some(verified_clock);
        let next = PlayerPreparation::Ready { selection };
        if self.player(*player_id)?.preparation == next {
            return Ok(());
        }
        self.player_mut(*player_id)?.preparation = next;
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn start_match(
        &mut self,
        actor_id: &ActorId,
        match_id: MatchId,
        leader_clock: VerifiedClockQuality,
        now: Instant,
    ) -> Result<(), ProtocolError> {
        self.require_leader(actor_id)?;
        let song = self.preparing_song(match_id)?.clone();
        if self
            .players
            .iter()
            .any(|player| !player.link.is_online() || player.dnf || !player.preparation.is_ready())
        {
            return Err(protocol_error(
                ProtocolErrorCode::NotPrepared,
                "every player must be online and ready",
                true,
            ));
        }
        let ActorId::Player(leader_id) = actor_id else {
            return Err(permission_error());
        };
        self.player_mut(*leader_id)?.clock_evidence = Some(leader_clock);
        if self.players.iter().any(|player| {
            player
                .clock_evidence
                .is_none_or(|evidence| !evidence.is_ready_at(now))
        }) {
            return Err(protocol_error(
                ProtocolErrorCode::ClockNotReady,
                "every player needs fresh server-verified clock path quality before start",
                true,
            ));
        }
        let assignments = self
            .players
            .iter()
            .map(|player| {
                let selection = *player
                    .preparation
                    .selection()
                    .expect("ready player has a selection");
                self.assignment(player.id, &song, selection)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let manifest = MatchManifest {
            match_id,
            song,
            assignments: BoundedVec::<_, MAX_PLAYERS>::try_from(assignments)
                .expect("room player capacity matches protocol"),
            countdown_ms: u32::try_from(MATCH_COUNTDOWN.as_millis()).expect("countdown fits u32"),
            input_lateness_ms: u32::try_from(INPUT_LATENESS.as_millis())
                .expect("input lateness fits u32"),
        };
        manifest.validate().map_err(|error| {
            protocol_error(
                ProtocolErrorCode::NotPrepared,
                match error {
                    taiko_multiplayer_protocol::ProtocolInvariantError::TooFewAssignments => {
                        "at least two players are required"
                    }
                    _ => "match manifest is invalid",
                },
                false,
            )
        })?;
        let start_at = now + MATCH_COUNTDOWN;
        let runtime = AuthoritativeMatchRuntime::new(
            manifest.clone(),
            self.authoritative_catalog.clone(),
            start_at,
        )
        .map_err(match_start_error)?;
        self.stage = StageState::Countdown { manifest, start_at };
        self.match_runtime = Some(runtime);
        self.next_live_state_at = Some(start_at);
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn rematch(
        &mut self,
        actor_id: &ActorId,
        previous_match_id: MatchId,
        now: Instant,
    ) -> Result<(), ProtocolError> {
        self.require_leader(actor_id)?;
        let song = match &self.stage {
            StageState::Finished { manifest, .. } if manifest.match_id == previous_match_id => {
                manifest.song.clone()
            }
            StageState::Finished { .. } => return Err(stale_match_error()),
            _ => return Err(invalid_stage_error()),
        };
        self.purge_unrecoverable_players(now);
        let match_id = self.allocate_match_id()?;
        for player in &mut self.players {
            player.preparation = PlayerPreparation::Selecting;
            player.last_acked_input = None;
            player.dnf = false;
        }
        self.match_runtime = None;
        self.next_live_state_at = None;
        self.stage = StageState::Preparing { match_id, song };
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn return_to_lobby(
        &mut self,
        actor_id: &ActorId,
        match_id: MatchId,
        now: Instant,
    ) -> Result<(), ProtocolError> {
        self.require_leader(actor_id)?;
        if self.stage.match_id() != Some(match_id) {
            return Err(stale_match_error());
        }
        if !matches!(
            self.stage,
            StageState::Preparing { .. }
                | StageState::Countdown { .. }
                | StageState::Finished { .. }
        ) {
            return Err(invalid_stage_error());
        }
        self.purge_unrecoverable_players(now);
        for player in &mut self.players {
            player.preparation = PlayerPreparation::Selecting;
            player.last_acked_input = None;
            player.dnf = false;
        }
        self.match_runtime = None;
        self.next_live_state_at = None;
        self.stage = StageState::Lobby;
        self.bump_revision()?;
        self.snapshot_dirty = true;
        Ok(())
    }

    fn purge_unrecoverable_players(&mut self, now: Instant) {
        self.players
            .retain(|player| !player.departed && player.link.can_resume_at(now));
        self.reconcile_leader();
    }

    fn next_deadline(&self) -> Instant {
        let mut deadline = self.created_at + ROOM_MAX_LIFETIME;
        if matches!(self.stage, StageState::Lobby | StageState::Preparing { .. }) {
            deadline = deadline.min(self.last_activity_at + LOBBY_IDLE_TTL);
        }
        match &self.stage {
            StageState::Countdown { start_at, .. } => deadline = deadline.min(*start_at),
            StageState::Playing { timeout_at, .. } => deadline = deadline.min(*timeout_at),
            StageState::Finalizing {
                deadline: finalizing,
                ..
            } => deadline = deadline.min(*finalizing),
            StageState::Finished { finished_at, .. } => {
                deadline = deadline.min(*finished_at + FINISHED_TTL);
            }
            StageState::Lobby | StageState::Preparing { .. } => {}
        }
        if let Some(next_live_state_at) = self.next_live_state_at {
            deadline = deadline.min(next_live_state_at);
        }
        for link in self
            .players
            .iter()
            .map(|player| &player.link)
            .chain(self.spectators.iter().map(|spectator| &spectator.link))
        {
            if let Some(lease) = link.lease_deadline {
                deadline = deadline.min(lease);
            }
            if let Some(grace) = link.reconnect_deadline {
                deadline = deadline.min(grace);
            }
        }
        deadline
    }

    fn advance_time(&mut self, now: Instant) {
        if self.closed {
            return;
        }

        let expired_leases = self
            .players
            .iter()
            .filter(|player| {
                player
                    .link
                    .lease_deadline
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|player| (ActorId::Player(player.id), player.link.active_session_id))
            .chain(
                self.spectators
                    .iter()
                    .filter(|spectator| {
                        spectator
                            .link
                            .lease_deadline
                            .is_some_and(|deadline| now >= deadline)
                    })
                    .map(|spectator| {
                        (
                            ActorId::Spectator(spectator.id),
                            spectator.link.active_session_id,
                        )
                    }),
            )
            .collect::<Vec<_>>();
        for (actor_id, session_id) in expired_leases {
            if let Ok(link) = self.link(&actor_id) {
                if let Some(endpoint) = link.endpoint() {
                    endpoint.outbound.close(protocol_error(
                        ProtocolErrorCode::SessionExpired,
                        "heartbeat lease expired; reconnect with the resume token",
                        true,
                    ));
                }
            }
            self.disconnect(&actor_id, session_id, now);
        }

        let expired_players = self
            .players
            .iter()
            .filter(|player| {
                player
                    .link
                    .reconnect_deadline
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|player| player.id)
            .collect::<Vec<_>>();
        let expired_spectators = self
            .spectators
            .iter()
            .filter(|spectator| {
                spectator
                    .link
                    .reconnect_deadline
                    .is_some_and(|deadline| now >= deadline)
            })
            .map(|spectator| spectator.id)
            .collect::<Vec<_>>();
        let mut grace_changed = false;
        if !expired_players.is_empty() {
            let retain_players = matches!(
                self.stage,
                StageState::Playing { .. }
                    | StageState::Finalizing { .. }
                    | StageState::Finished { .. }
            );
            if retain_players {
                for id in expired_players {
                    if let Ok(player) = self.player_mut(id) {
                        player.link.reconnect_deadline = None;
                        player.departed = true;
                    }
                    if matches!(
                        self.stage,
                        StageState::Playing { .. } | StageState::Finalizing { .. }
                    ) {
                        if let Err(error) = self.mark_runtime_dnf(id, now) {
                            self.fail_runtime(error);
                            return;
                        }
                    }
                    grace_changed = true;
                }
            } else {
                self.players
                    .retain(|player| !expired_players.contains(&player.id));
                grace_changed = true;
                if matches!(self.stage, StageState::Countdown { .. }) {
                    self.cancel_countdown();
                }
            }
        }
        if !expired_spectators.is_empty() {
            self.spectators
                .retain(|spectator| !expired_spectators.contains(&spectator.id));
            grace_changed = true;
        }
        if grace_changed {
            if self.bump_revision().is_err() {
                self.closed = true;
                return;
            }
            self.reconcile_leader();
            self.snapshot_dirty = true;
        }

        if let StageState::Countdown { manifest, start_at } = self.stage.clone() {
            if now >= start_at {
                self.stage = StageState::Playing {
                    manifest,
                    start_at,
                    timeout_at: start_at + MATCH_TTL,
                };
                self.next_live_state_at = Some(start_at);
                self.bump_revision_or_close();
            }
        }

        let match_timed_out = matches!(
            self.stage,
            StageState::Playing { timeout_at, .. } if now >= timeout_at
        );
        let live_due = self
            .next_live_state_at
            .is_some_and(|deadline| now >= deadline);
        if matches!(self.stage, StageState::Playing { .. })
            && (live_due || match_timed_out)
            && self.advance_authoritative_runtime(now).is_err()
        {
            return;
        }

        if matches!(self.stage, StageState::Playing { .. })
            && self
                .match_runtime
                .as_ref()
                .is_some_and(AuthoritativeMatchRuntime::all_finished)
        {
            self.begin_finalizing(now);
        } else if match_timed_out && matches!(self.stage, StageState::Playing { .. }) {
            let player_ids = self
                .players
                .iter()
                .map(|player| player.id)
                .collect::<Vec<_>>();
            for player_id in player_ids {
                if let Err(error) = self.mark_runtime_dnf(player_id, now) {
                    self.fail_runtime(error);
                    return;
                }
            }
            self.begin_finalizing(now);
        }

        if let StageState::Finalizing { deadline, .. } = &self.stage {
            if now >= *deadline {
                self.finish_authoritative_match(now);
            }
        }

        if (self.ever_had_player && self.players.is_empty())
            || now >= self.created_at + ROOM_MAX_LIFETIME
            || (matches!(self.stage, StageState::Lobby | StageState::Preparing { .. })
                && now >= self.last_activity_at + LOBBY_IDLE_TTL)
            || matches!(
                self.stage,
                StageState::Finished { finished_at, .. }
                    if now >= finished_at + FINISHED_TTL
            )
        {
            self.closed = true;
        }
    }

    fn bump_revision_or_close(&mut self) {
        if self.bump_revision().is_err() {
            self.closed = true;
        } else {
            self.snapshot_dirty = true;
        }
    }

    fn advance_authoritative_runtime(&mut self, now: Instant) -> Result<(), ()> {
        let live = match self.match_runtime.as_mut() {
            Some(runtime) => runtime.advance(now),
            None => Err(MatchRuntimeError::MatchNotFinished),
        };
        match live {
            Ok(live) => {
                self.publish_live(live);
                self.next_live_state_at = Some(now + LIVE_STATE_INTERVAL);
                Ok(())
            }
            Err(error) => {
                self.fail_runtime(error);
                Err(())
            }
        }
    }

    fn publish_live(&self, live: LiveStateSnapshot) {
        for endpoint in self
            .players
            .iter()
            .filter_map(|player| player.link.active_endpoint.as_ref())
            .chain(
                self.spectators
                    .iter()
                    .filter_map(|spectator| spectator.link.active_endpoint.as_ref()),
            )
        {
            endpoint.outbound.publish_live(live.clone());
        }
    }

    fn mark_runtime_dnf(
        &mut self,
        player_id: PlayerId,
        now: Instant,
    ) -> Result<(), MatchRuntimeError> {
        let marked = self
            .match_runtime
            .as_mut()
            .ok_or(MatchRuntimeError::MatchNotFinished)?
            .mark_dnf(player_id, now)?;
        if marked {
            self.player_mut(player_id)
                .map_err(|_| MatchRuntimeError::UnknownPlayer(player_id))?
                .dnf = true;
        }
        Ok(())
    }

    fn begin_finalizing(&mut self, now: Instant) {
        let StageState::Playing { manifest, .. } = self.stage.clone() else {
            return;
        };
        self.stage = StageState::Finalizing {
            manifest,
            deadline: now + FINALIZATION_GRACE,
        };
        self.next_live_state_at = None;
        self.bump_revision_or_close();
    }

    fn finish_authoritative_match(&mut self, now: Instant) {
        let StageState::Finalizing { manifest, .. } = self.stage.clone() else {
            return;
        };
        let results = match self
            .match_runtime
            .as_ref()
            .ok_or(MatchRuntimeError::MatchNotFinished)
            .and_then(AuthoritativeMatchRuntime::final_results)
        {
            Ok(results) => results,
            Err(error) => {
                self.fail_runtime(error);
                return;
            }
        };
        self.stage = StageState::Finished {
            manifest,
            results,
            finished_at: now,
        };
        self.bump_revision_or_close();
    }

    fn fail_runtime(&mut self, error: MatchRuntimeError) {
        eprintln!(
            "closing multiplayer room {} after authoritative runtime failure: {error}",
            self.room_code
        );
        self.close_all(protocol_error(
            ProtocolErrorCode::Internal,
            "authoritative match runtime failed",
            false,
        ));
        self.closed = true;
    }

    fn cancel_countdown(&mut self) {
        let StageState::Countdown { manifest, .. } = self.stage.clone() else {
            return;
        };
        for player in &mut self.players {
            if let PlayerPreparation::Ready { selection } = player.preparation {
                player.preparation = PlayerPreparation::Prepared { selection };
            }
        }
        self.stage = StageState::Preparing {
            match_id: manifest.match_id,
            song: manifest.song,
        };
        self.match_runtime = None;
        self.next_live_state_at = None;
    }

    fn snapshot(&self, now: Instant) -> Result<RoomSnapshot, ProtocolError> {
        let leader = self.leader.ok_or_else(|| {
            protocol_error(
                ProtocolErrorCode::Internal,
                "room has no player leader",
                false,
            )
        })?;
        let players = self
            .players
            .iter()
            .map(|player| PlayerSnapshot {
                player_id: player.id,
                name: player.name.clone(),
                is_leader: player.id == leader,
                connection: if player.dnf {
                    PlayerConnection::Dnf
                } else {
                    player.link.connection(&self.clock)
                },
                preparation: player.preparation.clone(),
                last_acked_input_seq: player.last_acked_input,
            })
            .collect::<Vec<_>>();
        let spectators = self
            .spectators
            .iter()
            .map(|spectator| SpectatorSnapshot {
                spectator_id: spectator.id,
                name: spectator.name.clone(),
                connection: spectator.link.connection(&self.clock),
            })
            .collect::<Vec<_>>();
        let snapshot = RoomSnapshot {
            room_code: self.room_code.clone(),
            revision: self.revision,
            server_now_us: self.clock.server_us(now),
            leader_player_id: leader,
            players: BoundedVec::<_, MAX_PLAYERS>::try_from(players)
                .expect("room enforces player capacity"),
            spectators: BoundedVec::<_, MAX_SPECTATORS>::try_from(spectators)
                .expect("room enforces spectator capacity"),
            stage: self.stage_snapshot(now)?,
        };
        snapshot.validate().map_err(|_| {
            protocol_error(
                ProtocolErrorCode::Internal,
                "room state violates protocol invariants",
                false,
            )
        })?;
        Ok(snapshot)
    }

    fn stage_snapshot(&self, now: Instant) -> Result<RoomStage, ProtocolError> {
        Ok(match &self.stage {
            StageState::Lobby => RoomStage::Lobby,
            StageState::Preparing { match_id, song } => RoomStage::Preparing {
                match_id: *match_id,
                song: song.clone(),
            },
            StageState::Countdown { manifest, start_at } => RoomStage::Countdown {
                manifest: manifest.clone(),
                start_at_server_us: self.clock.server_us(*start_at),
            },
            StageState::Playing {
                manifest, start_at, ..
            } => RoomStage::Playing {
                manifest: manifest.clone(),
                start_at_server_us: self.clock.server_us(*start_at),
                server_tick: self.match_server_tick(now)?,
            },
            StageState::Finalizing { manifest, deadline } => RoomStage::Finalizing {
                manifest: manifest.clone(),
                server_tick: self.match_server_tick(now)?,
                deadline_server_us: self.clock.server_us(*deadline),
            },
            StageState::Finished {
                manifest, results, ..
            } => RoomStage::Finished {
                manifest: manifest.clone(),
                results: results.clone(),
            },
        })
    }

    fn match_server_tick(&self, now: Instant) -> Result<Tick, ProtocolError> {
        self.match_runtime
            .as_ref()
            .ok_or_else(missing_match_runtime_error)?
            .current_server_tick(now)
            .map_err(|error| runtime_input_error(&error))
    }

    fn flush_snapshots(&mut self, now: Instant) {
        while self.snapshot_dirty && !self.closed && !self.players.is_empty() {
            self.snapshot_dirty = false;
            let snapshot = match self.snapshot(now) {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    self.close_all(error);
                    self.closed = true;
                    return;
                }
            };
            let targets = self
                .players
                .iter()
                .filter_map(|player| {
                    player
                        .link
                        .active_endpoint
                        .clone()
                        .map(|endpoint| (ActorId::Player(player.id), endpoint))
                })
                .chain(self.spectators.iter().filter_map(|spectator| {
                    spectator
                        .link
                        .active_endpoint
                        .clone()
                        .map(|endpoint| (ActorId::Spectator(spectator.id), endpoint))
                }))
                .collect::<Vec<_>>();
            for (actor_id, endpoint) in targets {
                if endpoint
                    .outbound
                    .send_reliable(ServerMessage::RoomSnapshot(Box::new(snapshot.clone())))
                    .is_err()
                {
                    endpoint.outbound.close(slow_consumer_error());
                    self.disconnect(&actor_id, endpoint.session_id, now);
                }
            }
        }
    }

    fn close_all(&self, reason: ProtocolError) {
        for endpoint in self
            .players
            .iter()
            .filter_map(|player| player.link.endpoint())
            .chain(
                self.spectators
                    .iter()
                    .filter_map(|spectator| spectator.link.endpoint()),
            )
        {
            endpoint.outbound.close(reason.clone());
        }
    }

    fn send_via_endpoint(
        &mut self,
        actor_id: &ActorId,
        endpoint: SessionEndpoint,
        message: ServerMessage,
        now: Instant,
    ) {
        if endpoint.outbound.send_reliable(message).is_err() {
            endpoint.outbound.close(slow_consumer_error());
            self.disconnect(actor_id, endpoint.session_id, now);
        }
    }

    fn send_to_actor(
        &mut self,
        actor_id: &ActorId,
        session_id: SessionId,
        message: ServerMessage,
        now: Instant,
    ) {
        let endpoint = self
            .link(actor_id)
            .ok()
            .and_then(Link::endpoint)
            .filter(|endpoint| endpoint.session_id == session_id)
            .cloned();
        let Some(endpoint) = endpoint else {
            return;
        };
        if endpoint.outbound.send_reliable(message).is_err() {
            endpoint.outbound.close(slow_consumer_error());
            self.disconnect(actor_id, session_id, now);
        }
    }

    fn validate_selection(
        &self,
        song: &SongManifest,
        selection: PlayerSelection,
    ) -> Result<(), ProtocolError> {
        if !song
            .courses
            .iter()
            .any(|course| course.course_id == selection.course_id)
        {
            return Err(protocol_error(
                ProtocolErrorCode::InvalidCourse,
                "course id is not in the selected song",
                false,
            ));
        }
        Ok(())
    }

    fn assignment(
        &self,
        player_id: PlayerId,
        song: &SongManifest,
        selection: PlayerSelection,
    ) -> Result<PlayerCourseAssignment, ProtocolError> {
        self.validate_selection(song, selection)?;
        let course = song
            .courses
            .iter()
            .find(|course| course.course_id == selection.course_id)
            .expect("selection validation found the course");
        Ok(PlayerCourseAssignment {
            player_id,
            selection,
            canonical_chart_hash: course.canonical_chart_hash.clone(),
        })
    }

    fn preparing_song(&self, match_id: MatchId) -> Result<&SongManifest, ProtocolError> {
        match &self.stage {
            StageState::Preparing {
                match_id: current,
                song,
            } if *current == match_id => Ok(song),
            StageState::Preparing { .. } => Err(stale_match_error()),
            _ => Err(invalid_stage_error()),
        }
    }

    fn require_leader(&self, actor_id: &ActorId) -> Result<(), ProtocolError> {
        match actor_id {
            ActorId::Player(id) if Some(*id) == self.leader => Ok(()),
            _ => Err(permission_error()),
        }
    }

    fn reconcile_leader(&mut self) {
        let current_online = self.leader.is_some_and(|leader| {
            self.players
                .iter()
                .find(|player| player.id == leader)
                .is_some_and(|player| player.link.is_online() && !player.dnf)
        });
        if current_online {
            return;
        }
        self.leader = self
            .players
            .iter()
            .filter(|player| player.link.is_online() && !player.dnf)
            .min_by_key(|player| player.joined_order)
            .or_else(|| self.players.iter().min_by_key(|player| player.joined_order))
            .map(|player| player.id);
    }

    fn ensure_active_session(
        &self,
        actor_id: &ActorId,
        session_id: SessionId,
    ) -> Result<(), ProtocolError> {
        let link = self.link(actor_id)?;
        if link.active_session_id != session_id || link.active_endpoint.is_none() {
            return Err(protocol_error(
                ProtocolErrorCode::SessionSuperseded,
                "session was superseded or disconnected",
                false,
            ));
        }
        Ok(())
    }

    fn link(&self, actor_id: &ActorId) -> Result<&Link, ProtocolError> {
        match actor_id {
            ActorId::Player(id) => Ok(&self.player(*id)?.link),
            ActorId::Spectator(id) => Ok(&self.spectator(*id)?.link),
        }
    }

    fn link_mut(&mut self, actor_id: &ActorId) -> Result<&mut Link, ProtocolError> {
        match actor_id {
            ActorId::Player(id) => Ok(&mut self.player_mut(*id)?.link),
            ActorId::Spectator(id) => Ok(&mut self.spectator_mut(*id)?.link),
        }
    }

    fn command_window(&self, actor_id: &ActorId) -> Result<&CommandWindow, ProtocolError> {
        match actor_id {
            ActorId::Player(id) => Ok(&self.player(*id)?.commands),
            ActorId::Spectator(id) => Ok(&self.spectator(*id)?.commands),
        }
    }

    fn command_window_mut(
        &mut self,
        actor_id: &ActorId,
    ) -> Result<&mut CommandWindow, ProtocolError> {
        match actor_id {
            ActorId::Player(id) => Ok(&mut self.player_mut(*id)?.commands),
            ActorId::Spectator(id) => Ok(&mut self.spectator_mut(*id)?.commands),
        }
    }

    fn player(&self, id: PlayerId) -> Result<&Player, ProtocolError> {
        self.players
            .iter()
            .find(|player| player.id == id)
            .ok_or_else(not_member_error)
    }

    fn player_mut(&mut self, id: PlayerId) -> Result<&mut Player, ProtocolError> {
        self.players
            .iter_mut()
            .find(|player| player.id == id)
            .ok_or_else(not_member_error)
    }

    fn spectator(&self, id: SpectatorId) -> Result<&Spectator, ProtocolError> {
        self.spectators
            .iter()
            .find(|spectator| spectator.id == id)
            .ok_or_else(not_member_error)
    }

    fn spectator_mut(&mut self, id: SpectatorId) -> Result<&mut Spectator, ProtocolError> {
        self.spectators
            .iter_mut()
            .find(|spectator| spectator.id == id)
            .ok_or_else(not_member_error)
    }

    fn allocate_player_id(&mut self) -> Result<u64, ProtocolError> {
        let id = self.next_player_id;
        self.next_player_id = self
            .next_player_id
            .checked_add(1)
            .ok_or_else(id_exhausted)?;
        Ok(id)
    }

    fn allocate_spectator_id(&mut self) -> Result<u64, ProtocolError> {
        let id = self.next_spectator_id;
        self.next_spectator_id = self
            .next_spectator_id
            .checked_add(1)
            .ok_or_else(id_exhausted)?;
        Ok(id)
    }

    fn allocate_join_order(&mut self) -> Result<u64, ProtocolError> {
        let order = self.next_join_order;
        self.next_join_order = self
            .next_join_order
            .checked_add(1)
            .ok_or_else(id_exhausted)?;
        Ok(order)
    }

    fn allocate_match_id(&mut self) -> Result<MatchId, ProtocolError> {
        let id = self.next_match_id;
        self.next_match_id = MatchId(
            self.next_match_id
                .0
                .checked_add(1)
                .ok_or_else(id_exhausted)?,
        );
        Ok(id)
    }

    fn bump_revision(&mut self) -> Result<(), ProtocolError> {
        self.revision = RoomRevision(self.revision.0.checked_add(1).ok_or_else(id_exhausted)?);
        Ok(())
    }
}

fn next_command_seq(seq: CommandSeq) -> Result<CommandSeq, ProtocolError> {
    seq.0
        .checked_add(1)
        .map(CommandSeq)
        .ok_or_else(id_exhausted)
}

fn applied_ack(
    seq: CommandSeq,
    next_expected_seq: CommandSeq,
    revision: RoomRevision,
) -> CommandAck {
    CommandAck {
        seq,
        next_expected_seq,
        outcome: CommandOutcome::Applied {
            room_revision: Some(revision),
        },
    }
}

fn rejected_ack(
    seq: CommandSeq,
    next_expected_seq: CommandSeq,
    error: ProtocolError,
    current_room_revision: Option<RoomRevision>,
) -> CommandAck {
    CommandAck {
        seq,
        next_expected_seq,
        outcome: CommandOutcome::Rejected {
            error,
            current_room_revision,
        },
    }
}

pub(crate) fn protocol_error(
    code: ProtocolErrorCode,
    message: &'static str,
    retryable: bool,
) -> ProtocolError {
    ProtocolError {
        code,
        message: ErrorMessage::new(message).expect("static protocol error message is bounded"),
        retryable,
    }
}

fn match_start_error(error: MatchRuntimeError) -> ProtocolError {
    match error {
        MatchRuntimeError::UnknownCourse { .. } | MatchRuntimeError::CourseHashMismatch { .. } => {
            protocol_error(
                ProtocolErrorCode::InvalidCourse,
                "selected course is invalid",
                false,
            )
        }
        MatchRuntimeError::InvalidManifest(_) => protocol_error(
            ProtocolErrorCode::NotPrepared,
            "authoritative match manifest is invalid",
            false,
        ),
        MatchRuntimeError::AutomaticBranching { .. }
        | MatchRuntimeError::NegativeBranchDecision { .. }
        | MatchRuntimeError::ChartCompile { .. }
        | MatchRuntimeError::UnknownSong(_)
        | MatchRuntimeError::ManifestResourceMismatch { .. } => protocol_error(
            ProtocolErrorCode::Internal,
            "server authoritative catalog is inconsistent",
            false,
        ),
        _ => protocol_error(
            ProtocolErrorCode::Internal,
            "authoritative match runtime could not start",
            false,
        ),
    }
}

fn recoverable_input_drop_reason(error: &MatchRuntimeError) -> Option<InputDropReason> {
    match error {
        MatchRuntimeError::LateInput { .. } => Some(InputDropReason::Late),
        MatchRuntimeError::FutureInput { .. } => Some(InputDropReason::Future),
        MatchRuntimeError::InputRateExceeded { .. } => Some(InputDropReason::RateLimited),
        MatchRuntimeError::PlayerFinished(_) => Some(InputDropReason::PlayerFinished),
        _ => None,
    }
}

fn input_drop_protocol_error(reason: InputDropReason) -> ProtocolError {
    match reason {
        InputDropReason::Late | InputDropReason::Future => protocol_error(
            ProtocolErrorCode::InvalidInput,
            "input was dropped outside the authoritative timeline window",
            false,
        ),
        InputDropReason::RateLimited => protocol_error(
            ProtocolErrorCode::RateLimited,
            "input was dropped by the authoritative physical rate limit",
            false,
        ),
        InputDropReason::PlayerFinished => protocol_error(
            ProtocolErrorCode::InvalidStage,
            "input was dropped after the player finished",
            false,
        ),
    }
}

fn runtime_input_error(error: &MatchRuntimeError) -> ProtocolError {
    match error {
        MatchRuntimeError::WrongMatch { .. } => stale_match_error(),
        MatchRuntimeError::UnknownPlayer(_) => not_member_error(),
        MatchRuntimeError::SequenceGap { .. } => protocol_error(
            ProtocolErrorCode::SequenceGap,
            "input sequence is not contiguous",
            true,
        ),
        MatchRuntimeError::EmptyInputBatch
        | MatchRuntimeError::InputSequenceBeforeStart
        | MatchRuntimeError::InputSequenceExhausted
        | MatchRuntimeError::NonContiguousBatch { .. }
        | MatchRuntimeError::ConflictingDuplicate { .. }
        | MatchRuntimeError::NegativeInputTick { .. }
        | MatchRuntimeError::NonMonotonicInputTick { .. }
        | MatchRuntimeError::LateInput { .. }
        | MatchRuntimeError::FutureInput { .. } => protocol_error(
            ProtocolErrorCode::InvalidInput,
            "input batch violates the authoritative timeline",
            false,
        ),
        MatchRuntimeError::InputRateExceeded { .. } => protocol_error(
            ProtocolErrorCode::RateLimited,
            "input exceeds the authoritative physical rate limit",
            false,
        ),
        MatchRuntimeError::PlayerFinished(_) => protocol_error(
            ProtocolErrorCode::InvalidStage,
            "player has already finished",
            false,
        ),
        MatchRuntimeError::InputBudgetExceeded(_) => protocol_error(
            ProtocolErrorCode::RateLimited,
            "player input budget is exhausted",
            false,
        ),
        MatchRuntimeError::MatchNotStarted => protocol_error(
            ProtocolErrorCode::InvalidStage,
            "match countdown has not finished",
            true,
        ),
        _ => protocol_error(
            ProtocolErrorCode::Internal,
            "authoritative input processing failed",
            false,
        ),
    }
}

fn runtime_error_is_fatal(error: &MatchRuntimeError) -> bool {
    matches!(
        error,
        MatchRuntimeError::InvalidManifest(_)
            | MatchRuntimeError::UnknownSong(_)
            | MatchRuntimeError::ManifestResourceMismatch { .. }
            | MatchRuntimeError::UnknownCourse { .. }
            | MatchRuntimeError::CourseHashMismatch { .. }
            | MatchRuntimeError::AutomaticBranching { .. }
            | MatchRuntimeError::NegativeBranchDecision { .. }
            | MatchRuntimeError::ChartCompile { .. }
            | MatchRuntimeError::ClockOverflow
            | MatchRuntimeError::EngineStep { .. }
            | MatchRuntimeError::StateSequenceExhausted
            | MatchRuntimeError::MatchNotFinished
            | MatchRuntimeError::NonFiniteScore(_)
            | MatchRuntimeError::ScoreFractionOutOfRange { .. }
            | MatchRuntimeError::InvalidReplayDigest
    )
}

fn missing_match_runtime_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::Internal,
        "authoritative match runtime is missing",
        false,
    )
}

fn invalid_stage_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::InvalidStage,
        "command is not valid in the current room stage",
        false,
    )
}

fn stale_match_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::StaleMatch,
        "command references a stale match",
        false,
    )
}

fn permission_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::PermissionDenied,
        "actor does not have permission for this command",
        false,
    )
}

fn not_member_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::NotMember,
        "actor is not a room member",
        false,
    )
}

fn room_closed_error() -> ProtocolError {
    protocol_error(ProtocolErrorCode::RoomClosed, "room is closed", false)
}

fn slow_consumer_error() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::SlowConsumer,
        "reliable outbound queue is full",
        true,
    )
}

fn id_exhausted() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::Internal,
        "monotonic identifier space is exhausted",
        false,
    )
}

fn generate_resume_token() -> Result<ResumeToken, ProtocolError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        protocol_error(
            ProtocolErrorCode::Internal,
            "secure token generation failed",
            false,
        )
    })?;
    ResumeToken::parse(hex::encode(bytes)).map_err(|_| id_exhausted())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use rhythm_chart::{
        CanonicalChart, ChartMetadata, Lane, LaneOrRegion, LaneRole, Object, ObjectKind,
        TempoChange, SCROLL_SCALE,
    };
    use taiko_multiplayer_protocol::{
        ClientCommand, DrumAction, InputEvent, ProgressMilli, ProtocolErrorCode, RoomStage,
    };
    use taiko_resource_protocol::{
        canonical_chart_sha256, song_manifest_sha256, ResourceBranchDecisionPoint, ResourceCourse,
        ResourceSong,
    };

    struct Fixture {
        core: RoomCore,
        now: Instant,
        room_code: RoomCode,
        host: ActorId,
        guest: ActorId,
        host_token: ResumeToken,
        _host_transport: SessionTransport,
        _guest_transport: SessionTransport,
    }

    fn digest(byte: char) -> ContentHash {
        ContentHash::parse(byte.to_string().repeat(64)).expect("valid digest")
    }

    fn catalogs() -> (Arc<MatchCatalog>, crate::AuthoritativeCatalog) {
        let resource_semantics = crate::current_resource_semantics();
        let authoritative_courses = vec![
            test_authoritative_course(0, "Normal", 3, 1_000_000),
            test_authoritative_course(1, "Hard", 7, 1_500_000),
        ];
        let resource_courses = authoritative_courses
            .iter()
            .map(|course| course.manifest.clone())
            .collect::<Vec<_>>();
        let source_id = digest('b').to_string();
        let audio_id = digest('c').to_string();
        let song_id = song_manifest_sha256(
            &source_id,
            Some(&audio_id),
            &resource_semantics,
            &resource_courses,
        )
        .expect("song manifest hash");
        let resource_song = ResourceSong {
            song_id: song_id.clone(),
            source_path: "test/song.tja".to_owned(),
            source_id,
            audio_path: Some("test/song.ogg".to_owned()),
            audio_id: Some(audio_id),
            title: "Test Song".to_owned(),
            subtitle: String::new(),
            artist: "Artist".to_owned(),
            demo_start_seconds: 0.0,
            courses: resource_courses,
        };
        let library = ResourceLibraryDocument {
            api_version: taiko_resource_protocol::API_VERSION,
            wire_schema_sha256: taiko_resource_protocol::WIRE_SCHEMA_SHA256.to_owned(),
            semantics: resource_semantics.clone(),
            songs: vec![resource_song.clone()],
            warnings: Vec::new(),
        };
        library.validate().expect("coherent resource fixture");
        let authoritative_song = Arc::new(crate::AuthoritativeSong {
            manifest: resource_song,
            courses: authoritative_courses.into_boxed_slice(),
        });
        let authoritative_catalog = crate::AuthoritativeCatalog {
            semantics: resource_semantics,
            songs_by_id: Arc::new(HashMap::from([(song_id, authoritative_song)])),
        };
        let match_catalog =
            Arc::new(MatchCatalog::from_library(&library).expect("multiplayer catalog"));
        (match_catalog, authoritative_catalog)
    }

    fn test_resource_course(index: u32, name: &str, level: u8) -> ResourceCourse {
        let note_tick = 1_000_000 + Tick::from(index) * 500_000;
        let chart = test_chart(name, level, note_tick);
        test_course_manifest(index, name, level, &chart, &[])
    }

    fn test_authoritative_course(
        index: u32,
        name: &str,
        level: u8,
        note_tick: Tick,
    ) -> crate::AuthoritativeCourse {
        let chart = test_chart(name, level, note_tick);
        let branch_decisions = Vec::<rhythm_chart::BranchDecisionPoint>::new();
        let manifest =
            test_course_manifest(index, name, level, &chart, branch_decisions.as_slice());
        crate::AuthoritativeCourse {
            manifest,
            chart: Arc::new(chart),
            branch_decisions: branch_decisions.into(),
        }
    }

    fn test_chart(name: &str, level: u8, note_tick: Tick) -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata {
                title: "Test Song".to_owned(),
                difficulty_name: Some(name.to_owned()),
                difficulty_level: Some(level),
                ..ChartMetadata::default()
            },
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: Vec::new(),
            lanes: vec![
                Lane {
                    id: 0,
                    name: "Don".to_owned(),
                    role: LaneRole::TaikoDon,
                },
                Lane {
                    id: 1,
                    name: "Kat".to_owned(),
                    role: LaneRole::TaikoKat,
                },
            ],
            branch_segments: Vec::new(),
            objects: vec![Object {
                id: 1,
                kind: ObjectKind::Tap,
                start_tick: note_tick,
                end_tick: note_tick,
                lane_or_region: LaneOrRegion::Lane(0),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: SCROLL_SCALE,
                branch_segment_id: None,
                branch_route_id: 0,
            }],
            events: Vec::new(),
        }
    }

    fn test_course_manifest(
        index: u32,
        name: &str,
        level: u8,
        chart: &CanonicalChart,
        branch_decisions: &[rhythm_chart::BranchDecisionPoint],
    ) -> ResourceCourse {
        ResourceCourse {
            index,
            name: name.to_owned(),
            level: Some(level),
            canonical_chart_hash: canonical_chart_sha256(chart).expect("canonical chart hash"),
            object_count: u32::try_from(chart.objects.len()).expect("object count"),
            branch_segment_count: u32::try_from(chart.branch_segments.len())
                .expect("branch segment count"),
            base_bpm: Some(120.0),
            branch_decisions: branch_decisions
                .iter()
                .map(|decision| ResourceBranchDecisionPoint {
                    segment_id: decision.segment_id,
                    decision_tick: decision.decision_tick,
                    default_route_id: decision.default_route_id,
                    route_count: decision.route_count,
                    hint: decision.hint.clone(),
                })
                .collect(),
        }
    }

    fn endpoint(session_id: u64, name: &str) -> (SessionEndpoint, SessionTransport) {
        let (outbound, transport) = SessionOutbound::channel();
        (
            SessionEndpoint {
                session_id: SessionId(session_id),
                name: DisplayName::new(name).expect("name"),
                outbound,
            },
            transport,
        )
    }

    fn envelope(
        seq: u64,
        revision: Option<RoomRevision>,
        command: ClientCommand,
    ) -> CommandEnvelope {
        CommandEnvelope {
            seq: CommandSeq(seq),
            expected_room_revision: revision,
            command,
        }
    }

    fn fixture() -> Fixture {
        let now = Instant::now();
        let room_code = RoomCode::parse("ABCD").expect("room");
        let invitation = InvitationToken::parse("3".repeat(64)).expect("invite");
        let (catalog, authoritative_catalog) = catalogs();
        let clock = ProcessClock::from_epoch(now, 1_000_000);
        let mut core = RoomCore::new(
            room_code.clone(),
            invitation.clone(),
            catalog,
            authoritative_catalog,
            clock,
            now,
        );
        let (host_endpoint, host_transport) = endpoint(1, "host");
        let host_admission = core
            .join(
                host_endpoint,
                JoinRole::Player,
                &invitation,
                envelope(1, None, ClientCommand::CreateRoom),
                true,
                now,
            )
            .expect("host joins");
        let host = host_admission.actor_id.clone();
        let host_token = host_admission.resume_token.clone();
        core.activate(host_admission, now).expect("host activates");

        let (guest_endpoint, guest_transport) = endpoint(2, "guest");
        let guest_admission = core
            .join(
                guest_endpoint,
                JoinRole::Player,
                &invitation,
                envelope(
                    1,
                    None,
                    ClientCommand::JoinRoom {
                        room_code: room_code.clone(),
                        invitation_token: invitation.clone(),
                        role: JoinRole::Player,
                    },
                ),
                false,
                now,
            )
            .expect("guest joins");
        let guest = guest_admission.actor_id.clone();
        core.activate(guest_admission, now)
            .expect("guest activates");
        core.flush_snapshots(now);
        Fixture {
            core,
            now,
            room_code,
            host,
            guest,
            host_token,
            _host_transport: host_transport,
            _guest_transport: guest_transport,
        }
    }

    fn selection(course_id: u32) -> PlayerSelection {
        PlayerSelection {
            course_id: CourseId(course_id),
        }
    }

    fn proof(song: &SongManifest, course_id: CourseId) -> PreparationProof {
        let course = song
            .courses
            .iter()
            .find(|course| course.course_id == course_id)
            .expect("course");
        PreparationProof {
            source_id: song.source_id.clone(),
            canonical_chart_hash: course.canonical_chart_hash.clone(),
            audio_id: song.audio_id.clone(),
            semantics: song.semantics.clone(),
        }
    }

    fn verified_clock(now: Instant) -> VerifiedClockQuality {
        VerifiedClockQuality::ready_for_tests(now)
    }

    fn fixture_song_id(fixture: &Fixture) -> SongId {
        fixture
            .core
            .catalog
            .songs
            .keys()
            .next()
            .expect("fixture catalog song")
            .clone()
    }

    fn prepare_countdown(fixture: &mut Fixture) -> MatchManifest {
        let song_id = fixture_song_id(fixture);
        fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                envelope(
                    2,
                    Some(fixture.core.revision),
                    ClientCommand::SelectSong { song_id },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("select song");
        let match_id = fixture.core.stage.match_id().expect("match");
        fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                envelope(
                    3,
                    Some(fixture.core.revision),
                    ClientCommand::SelectCourse {
                        match_id,
                        selection: selection(0),
                    },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("host course");
        fixture
            .core
            .control(
                &fixture.guest,
                SessionId(2),
                envelope(
                    2,
                    Some(fixture.core.revision),
                    ClientCommand::SelectCourse {
                        match_id,
                        selection: selection(1),
                    },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("guest course");
        fixture.core.player_mut(PlayerId(1)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(0),
        };
        fixture.core.player_mut(PlayerId(2)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(1),
        };
        let song = fixture.core.preparing_song(match_id).unwrap().clone();
        fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                envelope(
                    4,
                    Some(fixture.core.revision),
                    ClientCommand::SetReady {
                        match_id,
                        ready: true,
                        proof: Some(proof(&song, CourseId(0))),
                    },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("host ready");
        fixture
            .core
            .control(
                &fixture.guest,
                SessionId(2),
                envelope(
                    3,
                    Some(fixture.core.revision),
                    ClientCommand::SetReady {
                        match_id,
                        ready: true,
                        proof: Some(proof(&song, CourseId(1))),
                    },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("guest ready");
        fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                envelope(
                    5,
                    Some(fixture.core.revision),
                    ClientCommand::StartMatch { match_id },
                ),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("start");
        match &fixture.core.stage {
            StageState::Countdown { manifest, .. } => manifest.clone(),
            _ => panic!("expected countdown"),
        }
    }

    fn prepare_players_for_current_match(fixture: &mut Fixture, now: Instant) -> MatchId {
        let match_id = fixture.core.stage.match_id().expect("preparing match");
        fixture
            .core
            .select_course(&fixture.host, match_id, selection(0), now)
            .expect("host course");
        fixture
            .core
            .select_course(&fixture.guest, match_id, selection(1), now)
            .expect("guest course");
        fixture.core.player_mut(PlayerId(1)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(0),
        };
        fixture.core.player_mut(PlayerId(2)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(1),
        };
        let song = fixture.core.preparing_song(match_id).unwrap().clone();
        fixture
            .core
            .set_ready(
                &fixture.host,
                match_id,
                true,
                Some(&proof(&song, CourseId(0))),
                verified_clock(now),
                now,
            )
            .expect("host ready");
        fixture
            .core
            .set_ready(
                &fixture.guest,
                match_id,
                true,
                Some(&proof(&song, CourseId(1))),
                verified_clock(now),
                now,
            )
            .expect("guest ready");
        match_id
    }

    fn select_song_and_prepare_players(fixture: &mut Fixture) -> MatchId {
        let song_id = fixture_song_id(fixture);
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("select song");
        let now = fixture.now;
        prepare_players_for_current_match(fixture, now)
    }

    fn disable_session_leases(fixture: &mut Fixture) {
        for player in &mut fixture.core.players {
            player.link.lease_deadline = None;
        }
    }

    fn drain_reliable(transport: &mut SessionTransport) {
        while transport.reliable_rx.try_recv().is_ok() {}
    }

    #[test]
    fn snapshots_preserve_leader_and_command_idempotency() {
        let mut fixture = fixture();
        let initial = fixture.core.snapshot(fixture.now).expect("snapshot");
        initial.validate().expect("valid snapshot");
        assert_eq!(initial.leader_player_id, PlayerId(1));

        let command = envelope(
            2,
            Some(fixture.core.revision),
            ClientCommand::SelectSong {
                song_id: fixture_song_id(&fixture),
            },
        );
        let first = fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                command.clone(),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("first");
        let revision = fixture.core.revision;
        let duplicate = fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                command,
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("duplicate");
        assert_eq!(duplicate.ack, first.ack);
        assert_eq!(fixture.core.revision, revision);

        let reused = fixture.core.control(
            &fixture.host,
            SessionId(1),
            envelope(2, Some(revision), ClientCommand::LeaveRoom),
            verified_clock(fixture.now),
            fixture.now,
        );
        assert_eq!(
            reused.expect_err("different body must fail").code,
            ProtocolErrorCode::InvalidMessage
        );
    }

    #[test]
    fn catalog_requires_the_validated_resource_contract() {
        let resource_semantics = crate::current_resource_semantics();
        let mut course = test_resource_course(0, "Broken", 5);
        course.branch_segment_count = 1;
        let library = ResourceLibraryDocument {
            api_version: taiko_resource_protocol::API_VERSION,
            wire_schema_sha256: taiko_resource_protocol::WIRE_SCHEMA_SHA256.to_owned(),
            semantics: resource_semantics,
            songs: vec![ResourceSong {
                song_id: "a".repeat(64),
                source_path: "broken.tja".to_owned(),
                source_id: digest('b').to_string(),
                audio_path: Some("broken.ogg".to_owned()),
                audio_id: Some(digest('c').to_string()),
                title: "Broken Branch Metadata".to_owned(),
                subtitle: String::new(),
                artist: "Test".to_owned(),
                demo_start_seconds: 0.0,
                courses: vec![course],
            }],
            warnings: Vec::new(),
        };

        let error = MatchCatalog::from_library(&library)
            .err()
            .expect("missing branch decisions must fail catalog construction");
        assert!(format!("{error:#}").contains("branch decision count"));
    }

    #[test]
    fn every_valid_resource_presentation_boundary_builds_the_multiplayer_catalog() {
        let semantics = crate::current_resource_semantics();
        let courses = vec![test_resource_course(
            0,
            &"c".repeat(taiko_resource_protocol::MAX_COURSE_NAME_BYTES),
            5,
        )];
        let source_id = digest('b').to_string();
        let audio_id = digest('c').to_string();
        let song_id = song_manifest_sha256(&source_id, Some(&audio_id), &semantics, &courses)
            .expect("song identity");
        let library = ResourceLibraryDocument {
            api_version: taiko_resource_protocol::API_VERSION,
            wire_schema_sha256: taiko_resource_protocol::WIRE_SCHEMA_SHA256.to_owned(),
            semantics,
            songs: vec![ResourceSong {
                song_id: song_id.clone(),
                source_path: "boundary.tja".to_owned(),
                source_id,
                audio_path: Some("boundary.ogg".to_owned()),
                audio_id: Some(audio_id),
                title: "t".repeat(taiko_resource_protocol::MAX_TITLE_BYTES),
                subtitle: "s".repeat(taiko_resource_protocol::MAX_SUBTITLE_BYTES),
                artist: "a".repeat(taiko_resource_protocol::MAX_ARTIST_BYTES),
                demo_start_seconds: 0.0,
                courses,
            }],
            warnings: Vec::new(),
        };

        library
            .validate()
            .expect("boundary resource presentation is valid");
        let catalog = MatchCatalog::from_library(&library)
            .expect("every resource-valid presentation must convert to multiplayer");
        let song_id = SongId::parse(song_id).expect("song id");
        let manifest = &catalog.song(&song_id).expect("catalog song").manifest;
        assert_eq!(
            manifest.title.as_str().len(),
            taiko_resource_protocol::MAX_TITLE_BYTES
        );
        assert_eq!(
            manifest.courses[0].name.as_str().len(),
            taiko_resource_protocol::MAX_COURSE_NAME_BYTES
        );

        let mut padded = library;
        padded.songs[0].title = " padded".to_owned();
        assert!(padded.validate().is_err());
        assert!(
            MatchCatalog::from_library(&padded).is_err(),
            "surrounding whitespace must fail before catalog conversion"
        );
    }

    #[test]
    fn one_player_cannot_start_a_match() {
        let mut fixture = fixture();
        fixture
            .core
            .leave(&fixture.guest, fixture.now)
            .expect("guest leaves lobby");
        let song_id = fixture_song_id(&fixture);
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("host selects song");
        let match_id = fixture.core.stage.match_id().expect("preparing match");
        fixture
            .core
            .select_course(&fixture.host, match_id, selection(0), fixture.now)
            .expect("host course");
        fixture.core.player_mut(PlayerId(1)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(0),
        };
        let song = fixture.core.preparing_song(match_id).unwrap().clone();
        fixture
            .core
            .set_ready(
                &fixture.host,
                match_id,
                true,
                Some(&proof(&song, CourseId(0))),
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("host ready");
        let revision = fixture.core.revision;

        let error = fixture
            .core
            .start_match(
                &fixture.host,
                match_id,
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect_err("one player must not start");
        assert_eq!(error.code, ProtocolErrorCode::NotPrepared);
        assert_eq!(fixture.core.revision, revision);
        assert!(matches!(
            fixture.core.stage,
            StageState::Preparing {
                match_id: current,
                ..
            } if current == match_id
        ));
    }

    #[test]
    fn ready_requires_fresh_server_verified_clock_evidence() {
        let mut fixture = fixture();
        let song_id = fixture_song_id(&fixture);
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("select song");
        let match_id = fixture.core.stage.match_id().expect("preparing match");
        fixture
            .core
            .select_course(&fixture.host, match_id, selection(0), fixture.now)
            .expect("select course");
        fixture.core.player_mut(PlayerId(1)).unwrap().preparation = PlayerPreparation::Prepared {
            selection: selection(0),
        };
        let song = fixture.core.preparing_song(match_id).unwrap().clone();
        let evidence = verified_clock(fixture.now);
        let error = fixture
            .core
            .set_ready(
                &fixture.host,
                match_id,
                true,
                Some(&proof(&song, CourseId(0))),
                evidence,
                fixture.now + Duration::from_secs(11),
            )
            .expect_err("expired server evidence must not ready a player");
        assert_eq!(error.code, ProtocolErrorCode::ClockNotReady);
        assert!(matches!(
            fixture.core.player(PlayerId(1)).expect("host").preparation,
            PlayerPreparation::Prepared { .. }
        ));
    }

    #[test]
    fn start_requires_fresh_clock_evidence_from_every_player() {
        let mut fixture = fixture();
        let match_id = select_song_and_prepare_players(&mut fixture);
        let later = fixture.now + Duration::from_secs(11);
        let error = fixture
            .core
            .start_match(&fixture.host, match_id, verified_clock(later), later)
            .expect_err("the guest clock lease expired before start");
        assert_eq!(error.code, ProtocolErrorCode::ClockNotReady);
        assert!(matches!(
            fixture.core.stage,
            StageState::Preparing {
                match_id: current,
                ..
            } if current == match_id
        ));

        fixture
            .core
            .update_clock_evidence(&fixture.guest, SessionId(2), verified_clock(later))
            .expect("fresh guest evidence");
        fixture
            .core
            .start_match(&fixture.host, match_id, verified_clock(later), later)
            .expect("fresh evidence from both players starts the match");
        assert!(matches!(fixture.core.stage, StageState::Countdown { .. }));
    }

    #[test]
    fn resumed_ready_player_must_reestablish_clock_evidence_before_start() {
        let mut fixture = fixture();
        let match_id = select_song_and_prepare_players(&mut fixture);
        let guest_token = fixture
            .core
            .player(PlayerId(2))
            .expect("guest")
            .link
            .resume_token
            .clone();
        fixture
            .core
            .disconnect(&fixture.guest, SessionId(2), fixture.now);

        let (replacement, replacement_transport) = endpoint(3, "guest-resumed");
        let admission = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.guest.clone(),
                    token: guest_token,
                    last_room_revision: fixture.core.revision,
                    last_acked_command_seq: CommandSeq(1),
                },
                fixture.now + Duration::from_secs(1),
            )
            .expect("resume guest");
        fixture
            .core
            .activate(admission, fixture.now + Duration::from_secs(1))
            .expect("activate resumed guest");
        assert!(
            fixture
                .core
                .player(PlayerId(2))
                .expect("resumed guest")
                .clock_evidence
                .is_none(),
            "clock evidence is transport-session scoped"
        );
        assert!(matches!(
            fixture
                .core
                .player(PlayerId(2))
                .expect("resumed guest")
                .preparation,
            PlayerPreparation::Prepared { .. }
        ));

        let error = fixture
            .core
            .start_match(
                &fixture.host,
                match_id,
                verified_clock(fixture.now + Duration::from_secs(1)),
                fixture.now + Duration::from_secs(1),
            )
            .expect_err("resumed player must become ready again on the new transport");
        assert_eq!(error.code, ProtocolErrorCode::NotPrepared);

        let resumed_at = fixture.now + Duration::from_secs(1);
        fixture
            .core
            .update_clock_evidence(&fixture.guest, SessionId(3), verified_clock(resumed_at))
            .expect("fresh resumed clock evidence");
        let song = fixture
            .core
            .preparing_song(match_id)
            .expect("preparing song")
            .clone();
        fixture
            .core
            .set_ready(
                &fixture.guest,
                match_id,
                true,
                Some(&proof(&song, CourseId(1))),
                verified_clock(resumed_at),
                resumed_at,
            )
            .expect("resumed player reasserts ready with fresh evidence");
        fixture
            .core
            .start_match(
                &fixture.host,
                match_id,
                verified_clock(resumed_at),
                resumed_at,
            )
            .expect("fresh evidence and renewed readiness permit start");
        assert!(matches!(fixture.core.stage, StageState::Countdown { .. }));
        drop(replacement_transport);
    }

    #[test]
    fn duplicate_ready_ack_remains_idempotent_after_clock_evidence_expires() {
        let mut fixture = fixture();
        prepare_countdown(&mut fixture);
        let cached = fixture
            .core
            .command_window(&fixture.host)
            .expect("host command window")
            .cache
            .iter()
            .find(|cached| {
                matches!(
                    cached.envelope.command,
                    ClientCommand::SetReady { ready: true, .. }
                )
            })
            .expect("cached ready command")
            .clone();
        let later = fixture.now + Duration::from_secs(11);
        let duplicate = fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                cached.envelope,
                verified_clock(fixture.now),
                later,
            )
            .expect("duplicate returns its original result");
        assert_eq!(duplicate.ack, cached.ack);
    }

    #[test]
    fn late_spectator_does_not_change_player_readiness() {
        let mut fixture = fixture();
        let match_id = select_song_and_prepare_players(&mut fixture);
        let before = fixture.core.snapshot(fixture.now).expect("ready snapshot");
        let before_preparation = before
            .players
            .iter()
            .map(|player| (player.player_id, player.preparation.clone()))
            .collect::<HashMap<_, _>>();
        assert!(before
            .players
            .iter()
            .all(|player| player.preparation.is_ready()));

        let invitation = InvitationToken::parse("3".repeat(64)).expect("invite");
        let (spectator_endpoint, _spectator_transport) = endpoint(3, "late-spectator");
        let admission = fixture
            .core
            .join(
                spectator_endpoint,
                JoinRole::Spectator,
                &invitation,
                envelope(
                    1,
                    None,
                    ClientCommand::JoinRoom {
                        room_code: fixture.room_code.clone(),
                        invitation_token: invitation.clone(),
                        role: JoinRole::Spectator,
                    },
                ),
                false,
                fixture.now,
            )
            .expect("late spectator joins");
        fixture
            .core
            .activate(admission, fixture.now)
            .expect("late spectator activates");

        let after = fixture
            .core
            .snapshot(fixture.now)
            .expect("post-join snapshot");
        assert_eq!(after.spectators.len(), 1);
        assert!(matches!(
            after.stage,
            RoomStage::Preparing {
                match_id: current,
                ..
            } if current == match_id
        ));
        assert_eq!(
            after
                .players
                .iter()
                .map(|player| (player.player_id, player.preparation.clone()))
                .collect::<HashMap<_, _>>(),
            before_preparation
        );
        fixture
            .core
            .start_match(
                &fixture.host,
                match_id,
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("spectator does not block start");
    }

    #[test]
    fn resuming_player_becomes_leader_when_every_previous_link_was_offline() {
        let mut fixture = fixture();
        let guest_token = fixture
            .core
            .player(PlayerId(2))
            .expect("guest")
            .link
            .resume_token
            .clone();
        fixture
            .core
            .disconnect(&fixture.host, SessionId(1), fixture.now);
        assert_eq!(fixture.core.leader, Some(PlayerId(2)));
        fixture
            .core
            .disconnect(&fixture.guest, SessionId(2), fixture.now);
        assert!(fixture
            .core
            .players
            .iter()
            .all(|player| !player.link.is_online()));

        let (replacement, _replacement_transport) = endpoint(3, "guest");
        let admission = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.guest.clone(),
                    token: guest_token,
                    last_room_revision: fixture.core.revision,
                    last_acked_command_seq: CommandSeq(1),
                },
                fixture.now + Duration::from_secs(1),
            )
            .expect("guest resumes");
        fixture
            .core
            .activate(admission, fixture.now + Duration::from_secs(1))
            .expect("guest activates");

        assert_eq!(fixture.core.leader, Some(PlayerId(2)));
        let snapshot = fixture
            .core
            .snapshot(fixture.now + Duration::from_secs(1))
            .expect("snapshot");
        assert_eq!(snapshot.leader_player_id, PlayerId(2));
        assert_eq!(
            snapshot
                .players
                .iter()
                .filter(|player| player.is_leader)
                .count(),
            1
        );
    }

    #[test]
    fn heartbeat_lease_expiry_uses_typed_session_expired_error() {
        let mut fixture = fixture();
        fixture
            .core
            .player_mut(PlayerId(2))
            .expect("guest")
            .link
            .lease_deadline = None;
        fixture.core.advance_time(fixture.now + SESSION_LEASE);

        let reason = fixture
            ._host_transport
            .close_rx
            .borrow()
            .clone()
            .expect("host transport closes");
        assert_eq!(reason.code, ProtocolErrorCode::SessionExpired);
        assert!(reason.retryable);
        assert!(fixture
            .core
            .player(PlayerId(1))
            .expect("retained host")
            .link
            .reconnect_deadline
            .is_some());
    }

    #[test]
    fn room_capacity_supports_four_players_and_sixty_four_spectators_exactly() {
        let mut fixture = fixture();
        let invitation = InvitationToken::parse("3".repeat(64)).expect("invite");
        let mut transports = Vec::new();

        for session_id in 3_u64..=4 {
            let (endpoint, transport) = endpoint(session_id, &format!("player-{session_id}"));
            let admission = fixture
                .core
                .join(
                    endpoint,
                    JoinRole::Player,
                    &invitation,
                    envelope(
                        1,
                        None,
                        ClientCommand::JoinRoom {
                            room_code: fixture.room_code.clone(),
                            invitation_token: invitation.clone(),
                            role: JoinRole::Player,
                        },
                    ),
                    false,
                    fixture.now,
                )
                .expect("player slot is available");
            fixture
                .core
                .activate(admission, fixture.now)
                .expect("player activates");
            transports.push(transport);
        }

        let (overflow_player, overflow_transport) = endpoint(5, "overflow-player");
        let player_error = fixture
            .core
            .join(
                overflow_player,
                JoinRole::Player,
                &invitation,
                envelope(
                    1,
                    None,
                    ClientCommand::JoinRoom {
                        room_code: fixture.room_code.clone(),
                        invitation_token: invitation.clone(),
                        role: JoinRole::Player,
                    },
                ),
                false,
                fixture.now,
            )
            .expect_err("fifth player must be rejected");
        assert_eq!(player_error.code, ProtocolErrorCode::RoomFull);
        drop(overflow_transport);

        for index in 0_u64..u64::try_from(MAX_SPECTATORS).expect("spectator limit fits u64") {
            let session_id = 100 + index;
            let (endpoint, transport) = endpoint(session_id, &format!("spectator-{}", index + 1));
            let admission = fixture
                .core
                .join(
                    endpoint,
                    JoinRole::Spectator,
                    &invitation,
                    envelope(
                        1,
                        None,
                        ClientCommand::JoinRoom {
                            room_code: fixture.room_code.clone(),
                            invitation_token: invitation.clone(),
                            role: JoinRole::Spectator,
                        },
                    ),
                    false,
                    fixture.now,
                )
                .expect("spectator slot is available");
            fixture
                .core
                .activate(admission, fixture.now)
                .expect("spectator activates");
            transports.push(transport);
        }

        let (overflow_spectator, overflow_transport) = endpoint(999, "overflow-spectator");
        let spectator_error = fixture
            .core
            .join(
                overflow_spectator,
                JoinRole::Spectator,
                &invitation,
                envelope(
                    1,
                    None,
                    ClientCommand::JoinRoom {
                        room_code: fixture.room_code.clone(),
                        invitation_token: invitation.clone(),
                        role: JoinRole::Spectator,
                    },
                ),
                false,
                fixture.now,
            )
            .expect_err("sixty-fifth spectator must be rejected");
        assert_eq!(spectator_error.code, ProtocolErrorCode::SpectatorFull);
        drop(overflow_transport);

        fixture.core.flush_snapshots(fixture.now);
        let snapshot = fixture.core.snapshot(fixture.now).expect("snapshot");
        snapshot.validate().expect("capacity snapshot is valid");
        assert_eq!(snapshot.players.len(), MAX_PLAYERS);
        assert_eq!(snapshot.spectators.len(), MAX_SPECTATORS);
        assert_eq!(snapshot.leader_player_id, PlayerId(1));
        assert_eq!(
            snapshot
                .players
                .iter()
                .filter(|player| player.is_leader)
                .count(),
            1
        );
    }

    #[test]
    fn monotonic_countdown_advances_without_inbound_messages() {
        let mut fixture = fixture();
        let manifest = prepare_countdown(&mut fixture);
        fixture.core.advance_time(fixture.now + MATCH_COUNTDOWN);
        let snapshot = fixture
            .core
            .snapshot(fixture.now + MATCH_COUNTDOWN)
            .expect("snapshot");
        snapshot.validate().expect("valid playing snapshot");
        assert!(matches!(
            snapshot.stage,
            RoomStage::Playing {
                manifest: playing,
                ..
            } if playing == manifest
        ));
    }

    #[test]
    fn snapshots_serialize_all_room_times_in_the_process_clock_epoch() {
        let mut fixture = fixture();
        let lobby = fixture.core.snapshot(fixture.now).expect("lobby snapshot");
        assert_eq!(lobby.server_now_us, 1_000_000);

        let manifest = prepare_countdown(&mut fixture);
        let countdown = fixture
            .core
            .snapshot(fixture.now)
            .expect("countdown snapshot");
        let RoomStage::Countdown {
            start_at_server_us, ..
        } = countdown.stage
        else {
            panic!("expected countdown snapshot");
        };
        assert_eq!(
            start_at_server_us,
            1_000_000
                + u64::try_from(MATCH_COUNTDOWN.as_micros()).expect("countdown fits server time")
        );

        let finalization_delay = Duration::from_secs(42);
        let match_start = fixture.now + MATCH_COUNTDOWN;
        fixture.core.advance_time(match_start);
        fixture.core.stage = StageState::Finalizing {
            manifest,
            deadline: match_start + finalization_delay,
        };
        fixture
            .core
            .disconnect(&fixture.guest, SessionId(2), match_start);
        let finalizing = fixture
            .core
            .snapshot(match_start)
            .expect("finalizing snapshot");
        let RoomStage::Finalizing {
            deadline_server_us, ..
        } = finalizing.stage
        else {
            panic!("expected finalizing snapshot");
        };
        assert_eq!(
            deadline_server_us,
            1_000_000
                + u64::try_from(MATCH_COUNTDOWN.as_micros()).expect("countdown fits server time")
                + u64::try_from(finalization_delay.as_micros())
                    .expect("finalization delay fits server time")
        );
        let guest = finalizing
            .players
            .iter()
            .find(|player| player.player_id == PlayerId(2))
            .expect("guest snapshot");
        assert_eq!(
            guest.connection,
            PlayerConnection::Reconnecting {
                grace_deadline_server_us: 1_000_000
                    + u64::try_from(MATCH_COUNTDOWN.as_micros())
                        .expect("countdown fits server time")
                    + u64::try_from(RECONNECT_GRACE.as_micros())
                        .expect("reconnect grace fits server time"),
            }
        );
    }

    #[test]
    fn authoritative_input_produces_live_and_final_server_results() {
        let mut fixture = fixture();
        let manifest = prepare_countdown(&mut fixture);
        let start = fixture.now + MATCH_COUNTDOWN;
        fixture.core.advance_time(start);
        let live_sequence_before_input = fixture
            ._host_transport
            .live_rx
            .borrow()
            .as_ref()
            .map(|live| live.state_seq);

        let hit_batch = InputBatch {
            match_id: manifest.match_id,
            events: BoundedVec::new(vec![taiko_multiplayer_protocol::InputEvent {
                seq: FIRST_INPUT_SEQ,
                tick: 1_000_000,
                action: taiko_multiplayer_protocol::DrumAction::LEFT_DON,
            }])
            .expect("input batch"),
        };
        let ack = fixture
            .core
            .input(
                &fixture.host,
                SessionId(1),
                hit_batch.clone(),
                start + Duration::from_millis(800),
            )
            .expect("input accepted");
        assert!(matches!(ack.outcome, InputOutcome::Accepted));
        assert_eq!(ack.highest_contiguous_seq, Some(FIRST_INPUT_SEQ));
        assert_eq!(
            fixture.core.player(PlayerId(1)).unwrap().last_acked_input,
            Some(FIRST_INPUT_SEQ)
        );
        assert_eq!(
            fixture
                ._host_transport
                .live_rx
                .borrow()
                .as_ref()
                .map(|live| live.state_seq),
            live_sequence_before_input,
            "input batches must not amplify spectator fan-out beyond the live timer"
        );

        fixture.core.advance_time(start + Duration::from_secs(3));
        let live = fixture
            ._host_transport
            .live_rx
            .borrow()
            .clone()
            .expect("authoritative live state");
        assert_eq!(live.match_id, manifest.match_id);
        assert_eq!(live.players.len(), 2);
        assert!(
            live.players
                .iter()
                .find(|player| player.player_id == PlayerId(1))
                .expect("host live score")
                .score
                .score
                > 0
        );

        fixture
            .core
            .advance_time(start + Duration::from_secs(3) + FINALIZATION_GRACE);
        let StageState::Finished { results, .. } = &fixture.core.stage else {
            panic!("authoritative match must finish");
        };
        let host = results
            .iter()
            .find(|result| result.player_id == PlayerId(1))
            .expect("host result");
        let guest = results
            .iter()
            .find(|result| result.player_id == PlayerId(2))
            .expect("guest result");
        assert!(host.score.score > 0);
        assert_eq!(host.score.great, 1);
        assert!(!host.dnf);
        assert_eq!(guest.score.miss, 1);
        assert!(!guest.dnf);
        assert_ne!(
            host.replay_digest.as_str(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let duplicate = fixture
            .core
            .input(
                &fixture.host,
                SessionId(1),
                hit_batch,
                start + Duration::from_secs(3) + FINALIZATION_GRACE,
            )
            .expect("a delayed exact retry remains idempotently accepted after finish");
        assert!(matches!(duplicate.outcome, InputOutcome::Accepted));
        assert_eq!(duplicate.highest_contiguous_seq, Some(FIRST_INPUT_SEQ));
        assert_eq!(duplicate.next_expected_seq, InputSeq(2));
    }

    #[test]
    fn late_input_is_consumed_as_dropped_and_does_not_deadlock_the_sequence() {
        let mut fixture = fixture();
        let manifest = prepare_countdown(&mut fixture);
        let start = fixture.now + MATCH_COUNTDOWN;
        let receive_at = start + Duration::from_secs(1);
        fixture.core.advance_time(start);

        let late = InputBatch {
            match_id: manifest.match_id,
            events: BoundedVec::new(vec![taiko_multiplayer_protocol::InputEvent {
                seq: FIRST_INPUT_SEQ,
                tick: 500_000,
                action: taiko_multiplayer_protocol::DrumAction::LEFT_DON,
            }])
            .expect("late input batch"),
        };
        let dropped = fixture
            .core
            .input(&fixture.host, SessionId(1), late.clone(), receive_at)
            .expect("late input receives a typed drop acknowledgement");
        assert!(
            matches!(
                &dropped.outcome,
                InputOutcome::Rejected {
                    error: ProtocolError {
                        code: ProtocolErrorCode::InvalidInput,
                        retryable: false,
                        ..
                    }
                }
            ),
            "unexpected drop outcome: {:?}",
            dropped.outcome
        );
        assert_eq!(dropped.highest_contiguous_seq, Some(FIRST_INPUT_SEQ));
        assert_eq!(dropped.next_expected_seq, InputSeq(2));

        let replayed = fixture
            .core
            .input(
                &fixture.host,
                SessionId(1),
                late,
                receive_at + Duration::from_millis(10),
            )
            .expect("lost drop acknowledgement can be replayed idempotently");
        assert!(matches!(replayed.outcome, InputOutcome::Rejected { .. }));
        assert_eq!(replayed.highest_contiguous_seq, Some(FIRST_INPUT_SEQ));

        let next = InputBatch {
            match_id: manifest.match_id,
            events: BoundedVec::new(vec![taiko_multiplayer_protocol::InputEvent {
                seq: InputSeq(2),
                tick: 1_000_000,
                action: taiko_multiplayer_protocol::DrumAction::RIGHT_KAT,
            }])
            .expect("next input batch"),
        };
        let accepted = fixture
            .core
            .input(
                &fixture.host,
                SessionId(1),
                next,
                receive_at + Duration::from_millis(20),
            )
            .expect("sequence continues after a dropped input");
        assert!(matches!(accepted.outcome, InputOutcome::Accepted));
        assert_eq!(accepted.highest_contiguous_seq, Some(InputSeq(2)));
        assert_eq!(
            fixture.core.player(PlayerId(1)).unwrap().last_acked_input,
            Some(InputSeq(2))
        );
    }

    #[test]
    fn resume_fences_old_session_and_grace_reclaims_slot() {
        let mut fixture = fixture();
        fixture
            .core
            .disconnect(&fixture.host, SessionId(1), fixture.now);
        assert_eq!(fixture.core.leader, Some(PlayerId(2)));
        let (replacement, replacement_transport) = endpoint(3, "host");
        let admission = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.host.clone(),
                    token: fixture.host_token.clone(),
                    last_room_revision: fixture.core.revision,
                    last_acked_command_seq: CommandSeq(1),
                },
                fixture.now + Duration::from_secs(1),
            )
            .expect("resume");
        fixture
            .core
            .activate(admission, fixture.now + Duration::from_secs(1))
            .expect("activate");
        fixture
            .core
            .disconnect(&fixture.host, SessionId(1), fixture.now);
        assert!(fixture.core.player(PlayerId(1)).unwrap().link.is_online());
        fixture.core.disconnect(
            &fixture.host,
            SessionId(3),
            fixture.now + Duration::from_secs(1),
        );
        fixture
            .core
            .advance_time(fixture.now + Duration::from_secs(1) + RECONNECT_GRACE);
        assert!(fixture.core.player(PlayerId(1)).is_err());
        assert_eq!(fixture.core.leader, Some(PlayerId(2)));
        fixture
            .core
            .snapshot(fixture.now + RECONNECT_GRACE)
            .expect("snapshot")
            .validate()
            .expect("valid snapshot");
        drop(replacement_transport);
    }

    #[test]
    fn resume_after_missed_match_epoch_transition_uses_authoritative_snapshot() {
        let mut fixture = fixture();
        let song_id = fixture_song_id(&fixture);
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("old match epoch");
        let old_match_id = fixture.core.stage.match_id().expect("old match id");
        fixture
            .core
            .player_mut(PlayerId(1))
            .expect("host")
            .last_acked_input = Some(InputSeq(9));
        let stale_revision = fixture.core.revision;
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("new match epoch");
        let new_match_id = fixture.core.stage.match_id().expect("new match id");
        assert!(new_match_id > old_match_id);
        assert_eq!(
            fixture
                .core
                .player(PlayerId(1))
                .expect("host")
                .last_acked_input,
            None,
            "the new match has its own input sequence epoch"
        );

        fixture
            .core
            .disconnect(&fixture.host, SessionId(1), fixture.now);
        let (replacement, mut replacement_transport) = endpoint(3, "host");
        let admission = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.host.clone(),
                    token: fixture.host_token.clone(),
                    last_room_revision: stale_revision,
                    last_acked_command_seq: CommandSeq(1),
                },
                fixture.now + Duration::from_secs(1),
            )
            .expect("a stale client resumes without asserting an ambiguous input watermark");
        assert_eq!(admission.actor_id, fixture.host);
        fixture
            .core
            .activate(admission, fixture.now + Duration::from_secs(1))
            .expect("activation sends the authoritative new-epoch snapshot");

        assert!(matches!(
            replacement_transport
                .reliable_rx
                .try_recv()
                .expect("resumed membership"),
            ServerMessage::MembershipGranted(_)
        ));
        let snapshot = loop {
            match replacement_transport
                .reliable_rx
                .try_recv()
                .expect("authoritative resume snapshot")
            {
                ServerMessage::RoomSnapshot(snapshot) => break *snapshot,
                ServerMessage::CommandAck(_) => {}
                other => panic!("unexpected resume message: {other:?}"),
            }
        };
        let host = snapshot
            .players
            .iter()
            .find(|player| player.player_id == PlayerId(1))
            .expect("host snapshot");
        assert_eq!(host.last_acked_input_seq, None);
        assert_eq!(snapshot.stage.match_id(), Some(new_match_id));
        drop(replacement_transport);
    }

    #[test]
    fn rematch_uses_new_match_epoch_and_clears_round_state() {
        let mut fixture = fixture();
        let first = prepare_countdown(&mut fixture);
        for player in &mut fixture.core.players {
            player.link.lease_deadline = None;
        }
        let start = fixture.now + MATCH_COUNTDOWN;
        fixture.core.advance_time(start);
        fixture.core.advance_time(start + MATCH_TTL);
        fixture
            .core
            .advance_time(start + MATCH_TTL + FINALIZATION_GRACE);
        assert!(matches!(fixture.core.stage, StageState::Finished { .. }));
        fixture
            .core
            .control(
                &fixture.host,
                SessionId(1),
                envelope(
                    6,
                    Some(fixture.core.revision),
                    ClientCommand::Rematch {
                        previous_match_id: first.match_id,
                    },
                ),
                verified_clock(start + MATCH_TTL + FINALIZATION_GRACE),
                start + MATCH_TTL + FINALIZATION_GRACE,
            )
            .expect("rematch");
        let snapshot = fixture
            .core
            .snapshot(start + MATCH_TTL + FINALIZATION_GRACE)
            .expect("snapshot");
        snapshot.validate().expect("valid snapshot");
        assert!(matches!(
            snapshot.stage,
            RoomStage::Preparing { match_id, .. }
                if match_id.0 > first.match_id.0
        ));
        assert!(snapshot
            .players
            .iter()
            .all(|player| player.preparation == PlayerPreparation::Selecting));
    }

    #[test]
    fn active_leave_revokes_resume_and_is_purged_only_after_finished_snapshot() {
        let mut fixture = fixture();
        let manifest = prepare_countdown(&mut fixture);
        disable_session_leases(&mut fixture);
        let guest_token = fixture
            .core
            .player(PlayerId(2))
            .expect("guest")
            .link
            .resume_token
            .clone();
        let start = fixture.now + MATCH_COUNTDOWN;
        fixture.core.advance_time(start);
        let leave_at = start + Duration::from_millis(10);

        let result = fixture
            .core
            .control(
                &fixture.guest,
                SessionId(2),
                envelope(4, Some(fixture.core.revision), ClientCommand::LeaveRoom),
                verified_clock(leave_at),
                leave_at,
            )
            .expect("active leave is acknowledged");
        assert!(result.left_room);
        let guest = fixture.core.player(PlayerId(2)).expect("retained guest");
        assert!(guest.departed);
        assert!(!guest.link.is_online());
        assert!(guest.link.reconnect_deadline.is_none());

        let (replacement, _replacement_transport) = endpoint(3, "departed-guest");
        let error = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.guest.clone(),
                    token: guest_token,
                    last_room_revision: fixture.core.revision,
                    last_acked_command_seq: CommandSeq(3),
                },
                leave_at + Duration::from_millis(1),
            )
            .expect_err("leave permanently revokes this resume token");
        assert_eq!(error.code, ProtocolErrorCode::ResumeRejected);

        let finalizing_at = start + Duration::from_secs(3);
        fixture.core.advance_time(finalizing_at);
        fixture
            .core
            .advance_time(finalizing_at + FINALIZATION_GRACE);
        let finished_at = finalizing_at + FINALIZATION_GRACE;
        let finished = fixture
            .core
            .snapshot(finished_at)
            .expect("finished snapshot");
        finished.validate().expect("valid finished snapshot");
        assert_eq!(finished.players.len(), 2);
        assert!(finished
            .players
            .iter()
            .any(|player| player.player_id == PlayerId(2)));
        let RoomStage::Finished {
            manifest: finished_manifest,
            results,
        } = &finished.stage
        else {
            panic!("expected immutable finished snapshot");
        };
        assert_eq!(finished_manifest.match_id, manifest.match_id);
        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .find(|result| result.player_id == PlayerId(2))
                .expect("guest result")
                .dnf
        );

        fixture
            .core
            .rematch(&fixture.host, manifest.match_id, finished_at)
            .expect("remaining leader starts rematch");
        assert!(fixture.core.player(PlayerId(2)).is_err());
        assert_eq!(fixture.core.players.len(), 1);
        assert_eq!(fixture.core.leader, Some(PlayerId(1)));
        assert!(matches!(
            fixture.core.stage,
            StageState::Preparing { match_id, .. } if match_id > manifest.match_id
        ));
    }

    #[test]
    fn active_grace_expiry_is_purged_only_after_finished_snapshot() {
        let mut fixture = fixture();
        let manifest = prepare_countdown(&mut fixture);
        disable_session_leases(&mut fixture);
        let guest_token = fixture
            .core
            .player(PlayerId(2))
            .expect("guest")
            .link
            .resume_token
            .clone();
        let start = fixture.now + MATCH_COUNTDOWN;
        fixture.core.advance_time(start);
        fixture.core.disconnect(&fixture.guest, SessionId(2), start);
        let grace_deadline = start + RECONNECT_GRACE;
        let finalizing_at = start + Duration::from_secs(3);
        fixture.core.advance_time(finalizing_at);
        let finished_at = finalizing_at + FINALIZATION_GRACE;
        fixture.core.advance_time(finished_at);
        let before_expiry = fixture
            .core
            .snapshot(finished_at)
            .expect("finished snapshot before reconnect grace expires");
        before_expiry
            .validate()
            .expect("valid pre-expiry finished snapshot");
        assert_eq!(before_expiry.players.len(), 2);
        assert!(
            !fixture
                .core
                .player(PlayerId(2))
                .expect("recoverable guest")
                .departed
        );
        let RoomStage::Finished {
            manifest: before_manifest,
            results: before_results,
        } = &before_expiry.stage
        else {
            panic!("match finishes while the guest remains resumable");
        };
        assert_eq!(before_manifest.match_id, manifest.match_id);
        assert_eq!(before_results.len(), 2);

        fixture.core.advance_time(grace_deadline);

        let guest = fixture.core.player(PlayerId(2)).expect("retained guest");
        assert!(guest.departed);
        assert!(guest.link.reconnect_deadline.is_none());
        let (replacement, _replacement_transport) = endpoint(3, "expired-guest");
        let error = fixture
            .core
            .resume(
                replacement,
                ResumeRequest {
                    room_code: fixture.room_code.clone(),
                    actor_id: fixture.guest.clone(),
                    token: guest_token,
                    last_room_revision: fixture.core.revision,
                    last_acked_command_seq: CommandSeq(3),
                },
                grace_deadline,
            )
            .expect_err("expired grace permanently revokes this resume token");
        assert_eq!(error.code, ProtocolErrorCode::ResumeRejected);

        let after_expiry = fixture
            .core
            .snapshot(grace_deadline)
            .expect("finished snapshot after reconnect grace expires");
        after_expiry
            .validate()
            .expect("valid post-expiry finished snapshot");
        assert_eq!(after_expiry.players.len(), 2);
        let RoomStage::Finished {
            manifest: finished_manifest,
            results,
        } = &after_expiry.stage
        else {
            panic!("expected immutable finished snapshot");
        };
        assert_eq!(finished_manifest.match_id, manifest.match_id);
        assert_eq!(results, before_results);

        fixture
            .core
            .rematch(&fixture.host, manifest.match_id, grace_deadline)
            .expect("remaining leader starts rematch");
        assert!(fixture.core.player(PlayerId(2)).is_err());
        assert_eq!(fixture.core.players.len(), 1);
        assert_eq!(fixture.core.leader, Some(PlayerId(1)));
    }

    #[test]
    fn one_hundred_rematches_reject_every_previous_match_epoch() {
        let mut fixture = fixture();
        let mut match_id = select_song_and_prepare_players(&mut fixture);
        fixture
            .core
            .start_match(
                &fixture.host,
                match_id,
                verified_clock(fixture.now),
                fixture.now,
            )
            .expect("initial match starts");
        disable_session_leases(&mut fixture);
        drain_reliable(&mut fixture._host_transport);
        drain_reliable(&mut fixture._guest_transport);
        let mut round_now = fixture.now;
        let mut finished_match_ids = Vec::new();

        for rematch_index in 0..100 {
            let start = round_now + MATCH_COUNTDOWN;
            fixture.core.advance_time(start);
            let finalizing_at = start + Duration::from_secs(3);
            fixture.core.advance_time(finalizing_at);
            assert!(matches!(
                fixture.core.stage,
                StageState::Finalizing {
                    ref manifest, ..
                } if manifest.match_id == match_id
            ));
            let finished_at = finalizing_at + FINALIZATION_GRACE;
            fixture.core.advance_time(finished_at);
            assert!(matches!(
                fixture.core.stage,
                StageState::Finished {
                    ref manifest, ..
                } if manifest.match_id == match_id
            ));
            finished_match_ids.push(match_id);

            fixture
                .core
                .rematch(&fixture.host, match_id, finished_at)
                .expect("rematch advances the epoch");
            let next_match_id = fixture.core.stage.match_id().expect("new preparing epoch");
            assert!(next_match_id > match_id);

            for stale_match_id in &finished_match_ids {
                let revision = fixture.core.revision;
                let ack = fixture
                    .core
                    .input(
                        &fixture.host,
                        SessionId(1),
                        InputBatch {
                            match_id: *stale_match_id,
                            events: BoundedVec::new(vec![InputEvent {
                                seq: FIRST_INPUT_SEQ,
                                tick: 0,
                                action: DrumAction::LEFT_DON,
                            }])
                            .expect("single input"),
                        },
                        finished_at,
                    )
                    .expect("stale input receives a typed rejection");
                ack.validate().expect("valid rejection acknowledgement");
                let InputOutcome::Rejected { error } = ack.outcome else {
                    panic!("old match input must be rejected");
                };
                assert_eq!(error.code, ProtocolErrorCode::StaleMatch);
                assert_eq!(fixture.core.revision, revision);
                assert_eq!(fixture.core.stage.match_id(), Some(next_match_id));
                drain_reliable(&mut fixture._host_transport);
            }

            if rematch_index < 99 {
                match_id = prepare_players_for_current_match(&mut fixture, finished_at);
                assert_eq!(match_id, next_match_id);
                fixture
                    .core
                    .start_match(
                        &fixture.host,
                        match_id,
                        verified_clock(finished_at),
                        finished_at,
                    )
                    .expect("next match starts");
                round_now = finished_at;
            }
        }

        assert_eq!(finished_match_ids.len(), 100);
        assert!(finished_match_ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(matches!(
            fixture.core.stage,
            StageState::Preparing { match_id: current, .. }
                if finished_match_ids.iter().all(|finished| *finished < current)
        ));
    }

    #[test]
    fn outbound_control_queue_is_bounded() {
        let (outbound, _transport) = SessionOutbound::channel();
        let message =
            ServerMessage::HeartbeatAck(taiko_multiplayer_protocol::HeartbeatAck { nonce: 1 });
        for _ in 0..RELIABLE_OUTBOUND_CAPACITY {
            outbound
                .send_reliable(message.clone())
                .expect("queue has capacity");
        }
        assert_eq!(outbound.send_reliable(message), Err(OutboundError::Full));
    }

    #[tokio::test]
    async fn enqueued_command_waits_for_the_authoritative_reply() {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let handle = RoomHandle {
            room_code: RoomCode::parse("ABCD").expect("room"),
            generation: 1,
            command_tx,
        };
        let responder = tokio::spawn(async move {
            let Some(RoomCommand::Heartbeat { gate, reply, .. }) = command_rx.recv().await else {
                panic!("expected heartbeat command");
            };
            assert!(gate.claim(), "actor claims the request before its deadline");
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = reply.send(Ok(()));
        });

        handle
            .request_with_timeout(Duration::from_millis(20), |gate, reply| {
                RoomCommand::Heartbeat {
                    actor_id: ActorId::Player(PlayerId(1)),
                    session_id: SessionId(1),
                    gate,
                    reply,
                }
            })
            .await
            .expect("an enqueued command must not time out while awaiting its reply");
        responder.await.expect("responder");
    }

    #[tokio::test]
    async fn expired_unclaimed_request_cannot_apply_later() {
        let (command_tx, mut command_rx) = mpsc::channel(1);
        let handle = RoomHandle {
            room_code: RoomCode::parse("ABCD").expect("room"),
            generation: 1,
            command_tx,
        };

        let error = handle
            .request_with_timeout::<()>(Duration::from_millis(10), |gate, reply| {
                RoomCommand::Heartbeat {
                    actor_id: ActorId::Player(PlayerId(1)),
                    session_id: SessionId(1),
                    gate,
                    reply,
                }
            })
            .await
            .expect_err("unclaimed request expires");
        assert_eq!(error.code, ProtocolErrorCode::ServerBusy);

        let command = command_rx.recv().await.expect("queued request");
        assert!(
            !command.claim(),
            "a request cancelled at its deadline cannot later be applied"
        );
    }

    #[test]
    fn preparation_progress_never_moves_backwards() {
        let mut fixture = fixture();
        let song_id = fixture_song_id(&fixture);
        fixture
            .core
            .select_song(&fixture.host, &song_id, fixture.now)
            .expect("song");
        let match_id = fixture.core.stage.match_id().expect("match");
        fixture
            .core
            .select_course(&fixture.host, match_id, selection(0), fixture.now)
            .expect("course");
        fixture
            .core
            .report_preparation(
                &fixture.host,
                match_id,
                PreparationProgress::Downloading {
                    selection: selection(0),
                    progress_milli: ProgressMilli::new(500).expect("progress"),
                },
                fixture.now,
            )
            .expect("progress");
        let error = fixture
            .core
            .report_preparation(
                &fixture.host,
                match_id,
                PreparationProgress::Downloading {
                    selection: selection(0),
                    progress_milli: ProgressMilli::new(499).expect("progress"),
                },
                fixture.now,
            )
            .expect_err("regression rejected");
        assert_eq!(error.code, ProtocolErrorCode::InvalidMessage);
    }
}
