use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::Context;
use taiko_multiplayer_protocol::{
    ActorId, ClientHello, ClientMessage, CommandAck, CommandEnvelope, CommandOutcome, CommandSeq,
    ContentHash, HeartbeatAck, InputAck, InputOutcome, InvitationToken, JoinRole, ProtocolError,
    ProtocolErrorCode, ResumeRequest, RoomCode, ServerMessage, ServerWelcome, SessionId,
    TimeSyncRequest, TimeSyncResponse, FIRST_COMMAND_SEQ, FIRST_INPUT_SEQ, PROTOCOL_VERSION,
    WIRE_SCHEMA_SHA256,
};
use taiko_resource_protocol::{
    canonical_chart_sha256, ResourceBranchDecisionPoint, ResourceLibraryDocument,
};
use tokio::sync::{mpsc, Mutex};

use super::clock::ProcessClock;
use super::clock_evidence::{ClockProbeError, ClockProbeVerifier, VerifiedClockQuality};
use super::limits::{
    COMMAND_ACK_CACHE_CAPACITY, HEARTBEAT_INTERVAL, MAX_CONCURRENT_ROOMS, MAX_CONCURRENT_SESSIONS,
    RECONNECT_GRACE, REGISTRY_LIFECYCLE_CAPACITY, SESSION_MESSAGE_BURST,
    SESSION_MESSAGE_RATE_PER_SECOND,
};
use super::room_actor::{
    protocol_error, spawn_room, Admission, MatchCatalog, RoomHandle, RoomLifecycleEvent,
    SessionEndpoint, SessionOutbound,
};

const ROOM_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const ROOM_CODE_ATTEMPTS: usize = 128;

#[derive(Clone)]
pub(crate) struct MultiplayerRegistry {
    inner: Arc<Mutex<RegistryState>>,
    catalog: Arc<MatchCatalog>,
    authoritative_catalog: crate::AuthoritativeCatalog,
    lifecycle_tx: mpsc::Sender<RoomLifecycleEvent>,
    clock: ProcessClock,
}

struct RegistryState {
    next_session_id: u64,
    next_room_generation: u64,
    sessions: HashMap<SessionId, SessionRecord>,
    rooms: HashMap<RoomCode, RoomHandle>,
}

struct SessionRecord {
    outbound: SessionOutbound,
    hello: Option<ClientHello>,
    membership: Option<Membership>,
    next_expected_command: CommandSeq,
    command_cache: VecDeque<(CommandEnvelope, CommandAck)>,
    ingress: MessageRateLimiter,
    clock_probes: ClockProbeVerifier,
}

fn record_consumed_session_command(
    session: &mut SessionRecord,
    envelope: CommandEnvelope,
    ack: CommandAck,
) -> Result<(), ProtocolError> {
    let following = envelope
        .seq
        .0
        .checked_add(1)
        .map(CommandSeq)
        .ok_or_else(id_exhausted)?;
    if ack.seq != envelope.seq
        || session.next_expected_command != envelope.seq
        || ack.next_expected_seq != following
    {
        return Err(protocol_error(
            ProtocolErrorCode::Internal,
            "consumed command acknowledgement violated the session sequence invariant",
            false,
        ));
    }
    session.next_expected_command = ack.next_expected_seq;
    session.command_cache.push_back((envelope, ack));
    if session.command_cache.len() > COMMAND_ACK_CACHE_CAPACITY {
        session.command_cache.pop_front();
    }
    Ok(())
}

const RATE_CREDIT_SCALE: u64 = 1_000_000;

struct MessageRateLimiter {
    credit: u64,
    last_refill: Instant,
}

impl MessageRateLimiter {
    fn new(now: Instant) -> Self {
        Self {
            credit: SESSION_MESSAGE_BURST.saturating_mul(RATE_CREDIT_SCALE),
            last_refill: now,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        let elapsed_us = u64::try_from(now.saturating_duration_since(self.last_refill).as_micros())
            .unwrap_or(u64::MAX);
        self.last_refill = now;
        let capacity = SESSION_MESSAGE_BURST.saturating_mul(RATE_CREDIT_SCALE);
        self.credit = self
            .credit
            .saturating_add(elapsed_us.saturating_mul(SESSION_MESSAGE_RATE_PER_SECOND))
            .min(capacity);
        if self.credit < RATE_CREDIT_SCALE {
            return false;
        }
        self.credit -= RATE_CREDIT_SCALE;
        true
    }
}

#[derive(Clone)]
struct Membership {
    actor_id: ActorId,
    room: RoomHandle,
}

fn validate_authoritative_catalog_coherence(
    library: &ResourceLibraryDocument,
    authoritative_catalog: &crate::AuthoritativeCatalog,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        library.semantics == *authoritative_catalog.semantics(),
        "resource and authoritative catalogs have different ResourceSemantics"
    );

    let resource_song_ids = library
        .songs
        .iter()
        .map(|song| song.song_id.as_str())
        .collect::<BTreeSet<_>>();
    let authoritative_song_ids = authoritative_catalog
        .songs_by_id
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if resource_song_ids != authoritative_song_ids {
        let missing = resource_song_ids
            .difference(&authoritative_song_ids)
            .copied()
            .collect::<Vec<_>>();
        let unexpected = authoritative_song_ids
            .difference(&resource_song_ids)
            .copied()
            .collect::<Vec<_>>();
        anyhow::bail!(
            "resource and authoritative catalog song ID sets differ; \
             missing authoritative IDs: {missing:?}; unexpected authoritative IDs: {unexpected:?}"
        );
    }

    for resource_song in &library.songs {
        let authoritative_song = authoritative_catalog
            .song(&resource_song.song_id)
            .with_context(|| {
                format!(
                    "authoritative song {} disappeared after song ID set validation",
                    resource_song.song_id
                )
            })?;
        anyhow::ensure!(
            authoritative_song.manifest() == resource_song,
            "authoritative song {} retains a ResourceSong manifest different from the resource catalog",
            resource_song.song_id
        );
        anyhow::ensure!(
            authoritative_song.courses().len() == resource_song.courses.len(),
            "authoritative song {} retains {} courses, but its resource manifest declares {}",
            resource_song.song_id,
            authoritative_song.courses().len(),
            resource_song.courses.len()
        );

        for (position, (authoritative_course, resource_course)) in authoritative_song
            .courses()
            .iter()
            .zip(&resource_song.courses)
            .enumerate()
        {
            anyhow::ensure!(
                authoritative_course.manifest() == resource_course,
                "authoritative song {} course {position} retains a ResourceCourse manifest different from the resource catalog",
                resource_song.song_id
            );

            let recomputed_chart_hash = canonical_chart_sha256(authoritative_course.chart())
                .with_context(|| {
                    format!(
                        "authoritative song {} course {position} retained an invalid canonical chart",
                        resource_song.song_id
                    )
                })?;
            anyhow::ensure!(
                recomputed_chart_hash == resource_course.canonical_chart_hash,
                "authoritative song {} course {position} canonical chart hash drift: expected {}, recomputed {}",
                resource_song.song_id,
                resource_course.canonical_chart_hash,
                recomputed_chart_hash
            );

            let object_count = u32::try_from(authoritative_course.chart().objects.len())
                .with_context(|| {
                    format!(
                        "authoritative song {} course {position} object count exceeds u32",
                        resource_song.song_id
                    )
                })?;
            anyhow::ensure!(
                object_count == resource_course.object_count,
                "authoritative song {} course {position} object count drift: expected {}, retained {}",
                resource_song.song_id,
                resource_course.object_count,
                object_count
            );

            let branch_segment_count = u32::try_from(
                authoritative_course.chart().branch_segments.len(),
            )
            .with_context(|| {
                format!(
                    "authoritative song {} course {position} branch segment count exceeds u32",
                    resource_song.song_id
                )
            })?;
            anyhow::ensure!(
                branch_segment_count == resource_course.branch_segment_count,
                "authoritative song {} course {position} branch segment count drift: expected {}, retained {}",
                resource_song.song_id,
                resource_course.branch_segment_count,
                branch_segment_count
            );

            anyhow::ensure!(
                authoritative_course.branch_decisions().len()
                    == resource_course.branch_decisions.len(),
                "authoritative song {} course {position} retains {} branch decisions, but its resource manifest declares {}",
                resource_song.song_id,
                authoritative_course.branch_decisions().len(),
                resource_course.branch_decisions.len()
            );
            for (decision_index, (authoritative_decision, resource_decision)) in
                authoritative_course
                    .branch_decisions()
                    .iter()
                    .zip(&resource_course.branch_decisions)
                    .enumerate()
            {
                anyhow::ensure!(
                    branch_decisions_match(authoritative_decision, resource_decision),
                    "authoritative song {} course {position} branch decision {decision_index} differs from its resource manifest",
                    resource_song.song_id
                );
            }
        }
    }

    Ok(())
}

fn branch_decisions_match(
    authoritative: &rhythm_chart::BranchDecisionPoint,
    resource: &ResourceBranchDecisionPoint,
) -> bool {
    authoritative.segment_id == resource.segment_id
        && authoritative.decision_tick == resource.decision_tick
        && authoritative.default_route_id == resource.default_route_id
        && authoritative.route_count == resource.route_count
        && authoritative.hint.as_ref() == resource.hint.as_ref()
}

impl MultiplayerRegistry {
    pub(crate) fn try_new(
        library: &ResourceLibraryDocument,
        authoritative_catalog: crate::AuthoritativeCatalog,
    ) -> anyhow::Result<Self> {
        let catalog = MatchCatalog::from_library(library)?;
        validate_authoritative_catalog_coherence(library, &authoritative_catalog)?;
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel(REGISTRY_LIFECYCLE_CAPACITY);
        let inner = Arc::new(Mutex::new(RegistryState {
            next_session_id: 1,
            next_room_generation: 1,
            sessions: HashMap::new(),
            rooms: HashMap::new(),
        }));
        tokio::spawn(reap_closed_rooms(Arc::downgrade(&inner), lifecycle_rx));
        Ok(Self {
            inner,
            catalog: Arc::new(catalog),
            authoritative_catalog,
            lifecycle_tx,
            clock: ProcessClock::from_system_time()?,
        })
    }

    pub(crate) async fn register_session(
        &self,
        outbound: SessionOutbound,
    ) -> Result<SessionId, ProtocolError> {
        let mut state = self.inner.lock().await;
        if state.sessions.len() >= MAX_CONCURRENT_SESSIONS {
            return Err(protocol_error(
                ProtocolErrorCode::ServerBusy,
                "server session capacity is reached",
                true,
            ));
        }
        let session_id = SessionId(state.next_session_id);
        state.next_session_id = state
            .next_session_id
            .checked_add(1)
            .ok_or_else(id_exhausted)?;
        if state
            .sessions
            .insert(
                session_id,
                SessionRecord {
                    outbound,
                    hello: None,
                    membership: None,
                    next_expected_command: FIRST_COMMAND_SEQ,
                    command_cache: VecDeque::with_capacity(COMMAND_ACK_CACHE_CAPACITY),
                    ingress: MessageRateLimiter::new(Instant::now()),
                    clock_probes: ClockProbeVerifier::default(),
                },
            )
            .is_some()
        {
            return Err(id_exhausted());
        }
        Ok(session_id)
    }

    pub(crate) async fn remove_session(&self, session_id: SessionId) {
        let membership = self
            .inner
            .lock()
            .await
            .sessions
            .remove(&session_id)
            .and_then(|session| session.membership);
        if let Some(membership) = membership {
            membership
                .room
                .disconnect(membership.actor_id, session_id)
                .await;
        }
    }

    pub(crate) async fn session_is_member(&self, session_id: SessionId) -> bool {
        self.inner
            .lock()
            .await
            .sessions
            .get(&session_id)
            .is_some_and(|session| session.membership.is_some())
    }

    pub(crate) async fn handle_client_message(
        &self,
        session_id: SessionId,
        message: ClientMessage,
    ) {
        if !self
            .consume_message_budget(session_id, Instant::now())
            .await
        {
            return;
        }
        let receive_us = self.clock.now_us();
        match message {
            ClientMessage::Hello(hello) => {
                self.handle_hello(session_id, hello).await;
            }
            ClientMessage::Command(envelope) => {
                if !self.require_hello(session_id).await {
                    return;
                }
                self.handle_command(session_id, envelope).await;
            }
            ClientMessage::Input(batch) => {
                if !self.require_hello(session_id).await {
                    return;
                }
                self.handle_input(session_id, batch).await;
            }
            ClientMessage::TimeSync(request) => {
                if !self.require_hello(session_id).await {
                    return;
                }
                self.handle_time_sync_request(session_id, request, receive_us)
                    .await;
            }
            ClientMessage::TimeSyncReceipt(receipt) => {
                if !self.require_hello(session_id).await {
                    return;
                }
                self.handle_time_sync_receipt(session_id, receipt).await;
            }
            ClientMessage::Heartbeat(heartbeat) => {
                if !self.require_hello(session_id).await {
                    return;
                }
                let snapshot = self.session_snapshot(session_id).await;
                let Some((outbound, membership, _)) = snapshot else {
                    return;
                };
                if let Some(membership) = membership {
                    if let Err(error) = membership
                        .room
                        .heartbeat(membership.actor_id, session_id)
                        .await
                    {
                        outbound.close(error);
                        return;
                    }
                }
                send_or_close(
                    &outbound,
                    ServerMessage::HeartbeatAck(HeartbeatAck {
                        nonce: heartbeat.nonce,
                    }),
                );
            }
        }
    }

    async fn handle_time_sync_request(
        &self,
        session_id: SessionId,
        request: TimeSyncRequest,
        server_receive_us: u64,
    ) {
        let issued = {
            let mut state = self.inner.lock().await;
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return;
            };
            let outbound = session.outbound.clone();
            match session.clock_probes.issue(request.nonce, Instant::now()) {
                Ok(probe_token) => Ok((outbound, probe_token)),
                Err(error) => Err((outbound, error)),
            }
        };
        let (outbound, probe_token) = match issued {
            Ok(issued) => issued,
            Err((outbound, error)) => {
                fail_session(&outbound, clock_probe_protocol_error(error));
                return;
            }
        };
        send_or_close(
            &outbound,
            ServerMessage::TimeSync(TimeSyncResponse {
                nonce: request.nonce,
                client_send_us: request.client_send_us,
                server_receive_us,
                server_send_us: self.clock.now_us(),
                probe_token,
            }),
        );
    }

    async fn handle_time_sync_receipt(
        &self,
        session_id: SessionId,
        receipt: taiko_multiplayer_protocol::TimeSyncReceipt,
    ) {
        let observed_at = Instant::now();
        let acknowledged = {
            let mut state = self.inner.lock().await;
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return;
            };
            let outbound = session.outbound.clone();
            let membership = session.membership.clone();
            match session
                .clock_probes
                .acknowledge(&receipt, observed_at, &self.clock)
            {
                Ok(acknowledged) => Ok((outbound, membership, acknowledged)),
                Err(error) => Err((outbound, error)),
            }
        };
        match acknowledged {
            Ok((outbound, membership, acknowledged)) => {
                if let Some(membership) = membership {
                    if let Err(error) = membership
                        .room
                        .update_clock_evidence(
                            membership.actor_id,
                            session_id,
                            acknowledged.verified,
                        )
                        .await
                    {
                        outbound.close(error);
                        return;
                    }
                }
                send_or_close(&outbound, ServerMessage::ClockProbeAck(acknowledged.ack));
            }
            Err((outbound, error)) => {
                fail_session(&outbound, clock_probe_protocol_error(error));
            }
        }
    }

    pub(crate) fn uptime(&self) -> Duration {
        self.clock.uptime()
    }

    async fn consume_message_budget(&self, session_id: SessionId, now: Instant) -> bool {
        let outbound = {
            let mut state = self.inner.lock().await;
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return false;
            };
            if session.ingress.allow(now) {
                return true;
            }
            session.outbound.clone()
        };
        fail_session(
            &outbound,
            protocol_error(
                ProtocolErrorCode::RateLimited,
                "session message rate exceeded",
                false,
            ),
        );
        false
    }

    async fn handle_hello(&self, session_id: SessionId, hello: ClientHello) {
        let outbound = {
            let state = self.inner.lock().await;
            let Some(session) = state.sessions.get(&session_id) else {
                return;
            };
            if session.hello.is_some() {
                let outbound = session.outbound.clone();
                drop(state);
                fail_session(
                    &outbound,
                    protocol_error(
                        ProtocolErrorCode::InvalidMessage,
                        "hello was already received",
                        false,
                    ),
                );
                return;
            }
            session.outbound.clone()
        };

        if hello.protocol_version != PROTOCOL_VERSION
            || hello.wire_schema_sha256.as_str() != WIRE_SCHEMA_SHA256
        {
            fail_session(
                &outbound,
                protocol_error(
                    ProtocolErrorCode::UnsupportedProtocol,
                    "protocol version or wire schema does not match",
                    false,
                ),
            );
            return;
        }
        if hello.display_name.as_str().trim().is_empty() {
            fail_session(
                &outbound,
                protocol_error(
                    ProtocolErrorCode::InvalidName,
                    "display name cannot be blank",
                    false,
                ),
            );
            return;
        }

        let resume = hello.resume.clone();
        if let Some(request) = resume {
            self.resume_hello(session_id, hello, request, outbound)
                .await;
            return;
        }

        let next_expected = {
            let mut state = self.inner.lock().await;
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return;
            };
            if session.hello.is_some() {
                return;
            }
            session.hello = Some(hello);
            session.next_expected_command
        };
        send_or_close(&outbound, self.welcome(false, next_expected));
    }

    async fn resume_hello(
        &self,
        session_id: SessionId,
        hello: ClientHello,
        request: ResumeRequest,
        outbound: SessionOutbound,
    ) {
        let room = {
            let state = self.inner.lock().await;
            state.rooms.get(&request.room_code).cloned()
        };
        let Some(room) = room else {
            fail_session(
                &outbound,
                protocol_error(
                    ProtocolErrorCode::ResumeRejected,
                    "resume room no longer exists",
                    false,
                ),
            );
            return;
        };
        let endpoint = SessionEndpoint {
            session_id,
            name: hello.display_name.clone(),
            outbound: outbound.clone(),
        };
        let admission = match room.resume(endpoint, request).await {
            Ok(admission) => admission,
            Err(error) => {
                fail_session(&outbound, error);
                return;
            }
        };

        let session_exists = {
            let mut state = self.inner.lock().await;
            match state.sessions.get_mut(&session_id) {
                Some(session) => {
                    session.hello = Some(hello);
                    session.membership = Some(Membership {
                        actor_id: admission.actor_id.clone(),
                        room: room.clone(),
                    });
                    session.next_expected_command = admission.next_expected_command_seq;
                    if let Some((old_session_id, _)) = &admission.superseded_session {
                        if let Some(old_session) = state.sessions.get_mut(old_session_id) {
                            old_session.membership = None;
                        }
                    }
                    true
                }
                None => false,
            }
        };
        if !session_exists {
            room.disconnect(admission.actor_id.clone(), session_id)
                .await;
            return;
        }

        if let Some((_, old_outbound)) = &admission.superseded_session {
            old_outbound.close(protocol_error(
                ProtocolErrorCode::SessionSuperseded,
                "session was replaced by a resumed connection",
                false,
            ));
        }
        send_or_close(
            &outbound,
            self.welcome(true, admission.next_expected_command_seq),
        );
        if let Err(error) = room.activate(admission).await {
            outbound.close(error);
            self.clear_membership(session_id).await;
        }
    }

    async fn handle_command(&self, session_id: SessionId, envelope: CommandEnvelope) {
        let snapshot = self.command_session_snapshot(session_id).await;
        let Some((outbound, membership, name, verified_clock)) = snapshot else {
            return;
        };
        if let Some(membership) = membership {
            if let Some(ack) = self.cached_session_response(session_id, &envelope).await {
                send_or_close(&outbound, ServerMessage::CommandAck(ack));
                return;
            }
            let result = membership
                .room
                .control(
                    membership.actor_id.clone(),
                    session_id,
                    envelope.clone(),
                    verified_clock,
                )
                .await;
            match result {
                Ok(result) => {
                    let record_error = if let Some(session) =
                        self.inner.lock().await.sessions.get_mut(&session_id)
                    {
                        let record_error = if result.command_consumed {
                            record_consumed_session_command(session, envelope, result.ack.clone())
                                .err()
                        } else if result.left_room {
                            Some(protocol_error(
                                ProtocolErrorCode::Internal,
                                "room reported an unconsumed LeaveRoom command",
                                false,
                            ))
                        } else {
                            None
                        };
                        if result.left_room {
                            session.membership = None;
                        }
                        record_error
                    } else {
                        None
                    };
                    if let Some(error) = record_error {
                        outbound.close(error);
                    }
                }
                Err(error) => {
                    let ack = rejected_ack(
                        envelope.seq,
                        envelope.seq,
                        error.clone(),
                        envelope.expected_room_revision,
                    );
                    send_or_close(&outbound, ServerMessage::CommandAck(ack));
                    if matches!(error.code, ProtocolErrorCode::RoomClosed) {
                        self.clear_membership(session_id).await;
                    }
                }
            }
            return;
        }

        let Some(name) = name else {
            fail_session(
                &outbound,
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "hello is required before commands",
                    false,
                ),
            );
            return;
        };
        self.handle_admission_command(session_id, name, outbound, envelope)
            .await;
    }

    async fn handle_admission_command(
        &self,
        session_id: SessionId,
        name: taiko_multiplayer_protocol::DisplayName,
        outbound: SessionOutbound,
        envelope: CommandEnvelope,
    ) {
        match self.admission_sequence(session_id, &envelope).await {
            AdmissionSequence::Duplicate(ack) | AdmissionSequence::Rejected(ack) => {
                send_or_close(&outbound, ServerMessage::CommandAck(ack));
                return;
            }
            AdmissionSequence::Fresh => {}
            AdmissionSequence::Missing => return,
        }
        if envelope.expected_room_revision.is_some() {
            self.reject_admission(
                session_id,
                envelope,
                protocol_error(
                    ProtocolErrorCode::StaleRevision,
                    "admission commands must not carry a room revision",
                    false,
                ),
                outbound,
            )
            .await;
            return;
        }

        use taiko_multiplayer_protocol::ClientCommand;
        match &envelope.command {
            ClientCommand::CreateRoom => {
                self.create_room(session_id, name, outbound, envelope).await;
            }
            ClientCommand::JoinRoom {
                room_code,
                invitation_token,
                role,
            } => {
                self.join_room(
                    session_id,
                    name,
                    outbound,
                    envelope.clone(),
                    room_code.clone(),
                    invitation_token.clone(),
                    *role,
                )
                .await;
            }
            _ => {
                self.reject_admission(
                    session_id,
                    envelope,
                    protocol_error(
                        ProtocolErrorCode::NotMember,
                        "command requires room membership",
                        false,
                    ),
                    outbound,
                )
                .await;
            }
        }
    }

    async fn create_room(
        &self,
        session_id: SessionId,
        name: taiko_multiplayer_protocol::DisplayName,
        outbound: SessionOutbound,
        envelope: CommandEnvelope,
    ) {
        let (room, invitation_token) = match self.allocate_room().await {
            Ok(room) => room,
            Err(error) => {
                self.reject_admission(session_id, envelope, error, outbound)
                    .await;
                return;
            }
        };
        let endpoint = SessionEndpoint {
            session_id,
            name,
            outbound: outbound.clone(),
        };
        let admission = room
            .join(
                endpoint,
                JoinRole::Player,
                invitation_token,
                envelope.clone(),
                true,
            )
            .await;
        match admission {
            Ok(admission) => {
                self.commit_admission(session_id, room, admission, outbound)
                    .await;
            }
            Err(error) => {
                self.remove_room_if_current(&room).await;
                self.reject_admission(session_id, envelope, error, outbound)
                    .await;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn join_room(
        &self,
        session_id: SessionId,
        name: taiko_multiplayer_protocol::DisplayName,
        outbound: SessionOutbound,
        envelope: CommandEnvelope,
        room_code: RoomCode,
        invitation_token: InvitationToken,
        role: JoinRole,
    ) {
        let room = {
            let state = self.inner.lock().await;
            state.rooms.get(&room_code).cloned()
        };
        let Some(room) = room else {
            self.reject_admission(
                session_id,
                envelope,
                protocol_error(
                    ProtocolErrorCode::RoomNotFound,
                    "room code does not exist",
                    false,
                ),
                outbound,
            )
            .await;
            return;
        };
        let endpoint = SessionEndpoint {
            session_id,
            name,
            outbound: outbound.clone(),
        };
        match room
            .join(endpoint, role, invitation_token, envelope.clone(), false)
            .await
        {
            Ok(admission) => {
                self.commit_admission(session_id, room, admission, outbound)
                    .await;
            }
            Err(error) => {
                self.reject_admission(session_id, envelope, error, outbound)
                    .await;
            }
        }
    }

    async fn commit_admission(
        &self,
        session_id: SessionId,
        room: RoomHandle,
        admission: Admission,
        outbound: SessionOutbound,
    ) {
        let committed = {
            let mut state = self.inner.lock().await;
            match state.sessions.get_mut(&session_id) {
                Some(session) if session.membership.is_none() => {
                    session.membership = Some(Membership {
                        actor_id: admission.actor_id.clone(),
                        room: room.clone(),
                    });
                    session.next_expected_command = admission.next_expected_command_seq;
                    true
                }
                Some(_) | None => false,
            }
        };
        if !committed {
            room.disconnect(admission.actor_id, session_id).await;
            return;
        }
        if let Err(error) = room.activate(admission).await {
            outbound.close(error);
            self.clear_membership(session_id).await;
        }
    }

    async fn reject_admission(
        &self,
        session_id: SessionId,
        envelope: CommandEnvelope,
        error: ProtocolError,
        outbound: SessionOutbound,
    ) {
        let ack = {
            let mut state = self.inner.lock().await;
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return;
            };
            let following = match envelope.seq.0.checked_add(1) {
                Some(next) => CommandSeq(next),
                None => {
                    outbound.close(id_exhausted());
                    return;
                }
            };
            let ack = rejected_ack(envelope.seq, following, error, None);
            if let Err(error) = record_consumed_session_command(session, envelope, ack.clone()) {
                outbound.close(error);
                return;
            }
            ack
        };
        send_or_close(&outbound, ServerMessage::CommandAck(ack));
    }

    async fn handle_input(
        &self,
        session_id: SessionId,
        batch: taiko_multiplayer_protocol::InputBatch,
    ) {
        let snapshot = self.session_snapshot(session_id).await;
        let Some((outbound, membership, _)) = snapshot else {
            return;
        };
        if let Some(membership) = membership {
            match membership
                .room
                .input(membership.actor_id, session_id, batch.clone())
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    send_or_close(
                        &outbound,
                        ServerMessage::InputAck(InputAck {
                            match_id: batch.match_id,
                            highest_contiguous_seq: None,
                            next_expected_seq: FIRST_INPUT_SEQ,
                            server_tick: 0,
                            outcome: InputOutcome::Rejected { error },
                        }),
                    );
                }
            }
        } else {
            send_or_close(
                &outbound,
                ServerMessage::InputAck(InputAck {
                    match_id: batch.match_id,
                    highest_contiguous_seq: None,
                    next_expected_seq: FIRST_INPUT_SEQ,
                    server_tick: 0,
                    outcome: InputOutcome::Rejected {
                        error: protocol_error(
                            ProtocolErrorCode::NotMember,
                            "input requires room membership",
                            false,
                        ),
                    },
                }),
            );
        }
    }

    async fn require_hello(&self, session_id: SessionId) -> bool {
        let snapshot = self.session_snapshot(session_id).await;
        let Some((outbound, _, hello)) = snapshot else {
            return false;
        };
        if hello.is_some() {
            true
        } else {
            fail_session(
                &outbound,
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "hello must be the first message",
                    false,
                ),
            );
            false
        }
    }

    async fn admission_sequence(
        &self,
        session_id: SessionId,
        envelope: &CommandEnvelope,
    ) -> AdmissionSequence {
        let state = self.inner.lock().await;
        let Some(session) = state.sessions.get(&session_id) else {
            return AdmissionSequence::Missing;
        };
        if envelope.seq < session.next_expected_command {
            let Some((cached_envelope, ack)) = session
                .command_cache
                .iter()
                .find(|(cached, _)| cached.seq == envelope.seq)
            else {
                return AdmissionSequence::Rejected(rejected_ack(
                    envelope.seq,
                    session.next_expected_command,
                    protocol_error(
                        ProtocolErrorCode::SequenceGap,
                        "command is older than the bounded replay window",
                        false,
                    ),
                    None,
                ));
            };
            if cached_envelope == envelope {
                return AdmissionSequence::Duplicate(ack.clone());
            }
            return AdmissionSequence::Rejected(rejected_ack(
                envelope.seq,
                session.next_expected_command,
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "command sequence was reused with different contents",
                    false,
                ),
                None,
            ));
        }
        if envelope.seq > session.next_expected_command {
            return AdmissionSequence::Rejected(rejected_ack(
                envelope.seq,
                session.next_expected_command,
                protocol_error(
                    ProtocolErrorCode::SequenceGap,
                    "command sequence is not contiguous",
                    true,
                ),
                None,
            ));
        }
        AdmissionSequence::Fresh
    }

    async fn cached_session_response(
        &self,
        session_id: SessionId,
        envelope: &CommandEnvelope,
    ) -> Option<CommandAck> {
        let state = self.inner.lock().await;
        let session = state.sessions.get(&session_id)?;
        let (cached, ack) = session
            .command_cache
            .iter()
            .find(|(cached, _)| cached.seq == envelope.seq)?;
        if cached == envelope {
            Some(ack.clone())
        } else {
            Some(rejected_ack(
                envelope.seq,
                session.next_expected_command,
                protocol_error(
                    ProtocolErrorCode::InvalidMessage,
                    "command sequence was reused with different contents",
                    false,
                ),
                None,
            ))
        }
    }

    async fn allocate_room(&self) -> Result<(RoomHandle, InvitationToken), ProtocolError> {
        let invitation_token = generate_invitation_token()?;
        let mut state = self.inner.lock().await;
        if state.rooms.len() >= MAX_CONCURRENT_ROOMS {
            return Err(protocol_error(
                ProtocolErrorCode::ServerBusy,
                "server room capacity is reached",
                true,
            ));
        }
        let generation = state.next_room_generation;
        state.next_room_generation = state
            .next_room_generation
            .checked_add(1)
            .ok_or_else(id_exhausted)?;
        for _ in 0..ROOM_CODE_ATTEMPTS {
            let room_code = generate_room_code()?;
            if state.rooms.contains_key(&room_code) {
                continue;
            }
            let room = spawn_room(
                room_code.clone(),
                generation,
                invitation_token.clone(),
                self.catalog.clone(),
                self.authoritative_catalog.clone(),
                self.clock.clone(),
                self.lifecycle_tx.clone(),
            );
            state.rooms.insert(room_code, room.clone());
            return Ok((room, invitation_token));
        }
        Err(protocol_error(
            ProtocolErrorCode::ServerBusy,
            "unable to allocate a unique room code",
            true,
        ))
    }

    async fn remove_room_if_current(&self, room: &RoomHandle) {
        let mut state = self.inner.lock().await;
        if state
            .rooms
            .get(room.room_code())
            .is_some_and(|current| current.generation() == room.generation())
        {
            state.rooms.remove(room.room_code());
        }
    }

    async fn clear_membership(&self, session_id: SessionId) {
        if let Some(session) = self.inner.lock().await.sessions.get_mut(&session_id) {
            session.membership = None;
        }
    }

    async fn session_snapshot(
        &self,
        session_id: SessionId,
    ) -> Option<(
        SessionOutbound,
        Option<Membership>,
        Option<taiko_multiplayer_protocol::DisplayName>,
    )> {
        self.inner
            .lock()
            .await
            .sessions
            .get(&session_id)
            .map(|session| {
                (
                    session.outbound.clone(),
                    session.membership.clone(),
                    session
                        .hello
                        .as_ref()
                        .map(|hello| hello.display_name.clone()),
                )
            })
    }

    async fn command_session_snapshot(
        &self,
        session_id: SessionId,
    ) -> Option<(
        SessionOutbound,
        Option<Membership>,
        Option<taiko_multiplayer_protocol::DisplayName>,
        VerifiedClockQuality,
    )> {
        let mut state = self.inner.lock().await;
        let session = state.sessions.get_mut(&session_id)?;
        Some((
            session.outbound.clone(),
            session.membership.clone(),
            session
                .hello
                .as_ref()
                .map(|hello| hello.display_name.clone()),
            session.clock_probes.verified_quality(Instant::now()),
        ))
    }

    fn welcome(&self, resumed: bool, next_expected_command_seq: CommandSeq) -> ServerMessage {
        ServerMessage::Welcome(ServerWelcome {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256)
                .expect("protocol schema digest is valid"),
            heartbeat_interval_ms: u32::try_from(HEARTBEAT_INTERVAL.as_millis())
                .expect("heartbeat interval fits u32"),
            reconnect_grace_ms: u32::try_from(RECONNECT_GRACE.as_millis())
                .expect("reconnect grace fits u32"),
            resumed,
            next_expected_command_seq,
        })
    }
}

enum AdmissionSequence {
    Fresh,
    Duplicate(CommandAck),
    Rejected(CommandAck),
    Missing,
}

async fn reap_closed_rooms(
    inner: Weak<Mutex<RegistryState>>,
    mut lifecycle_rx: mpsc::Receiver<RoomLifecycleEvent>,
) {
    while let Some(event) = lifecycle_rx.recv().await {
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let RoomLifecycleEvent::Closed {
            room_code,
            generation,
        } = event;
        let outbounds = {
            let mut state = inner.lock().await;
            if state
                .rooms
                .get(&room_code)
                .is_none_or(|room| room.generation() != generation)
            {
                continue;
            }
            state.rooms.remove(&room_code);
            state
                .sessions
                .values_mut()
                .filter_map(|session| {
                    let matches_room = session.membership.as_ref().is_some_and(|membership| {
                        membership.room.generation() == generation
                            && membership.room.room_code() == &room_code
                    });
                    if matches_room {
                        session.membership = None;
                        Some(session.outbound.clone())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        for outbound in outbounds {
            outbound.close(protocol_error(
                ProtocolErrorCode::RoomClosed,
                "room was closed by the server",
                false,
            ));
        }
    }
}

fn send_or_close(outbound: &SessionOutbound, message: ServerMessage) {
    if outbound.send_reliable(message).is_err() {
        outbound.close(protocol_error(
            ProtocolErrorCode::SlowConsumer,
            "reliable outbound queue is full",
            true,
        ));
    }
}

fn fail_session(outbound: &SessionOutbound, error: ProtocolError) {
    outbound.close(error);
}

fn clock_probe_protocol_error(error: ClockProbeError) -> ProtocolError {
    let (code, retryable) = match error {
        ClockProbeError::EntropyUnavailable => (ProtocolErrorCode::Internal, true),
        ClockProbeError::PendingCapacity => (ProtocolErrorCode::ServerBusy, true),
        ClockProbeError::DuplicateNonce
        | ClockProbeError::UnknownNonce
        | ClockProbeError::TokenMismatch => (ProtocolErrorCode::InvalidMessage, false),
    };
    ProtocolError {
        code,
        message: taiko_multiplayer_protocol::ErrorMessage::new(error.to_string())
            .expect("clock probe errors satisfy the protocol message bound"),
        retryable,
    }
}

fn rejected_ack(
    seq: CommandSeq,
    next_expected_seq: CommandSeq,
    error: ProtocolError,
    current_room_revision: Option<taiko_multiplayer_protocol::RoomRevision>,
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

fn generate_room_code() -> Result<RoomCode, ProtocolError> {
    let mut random = [0_u8; 4];
    getrandom::fill(&mut random).map_err(|_| id_exhausted())?;
    let code = random
        .into_iter()
        .map(|byte| ROOM_CODE_ALPHABET[usize::from(byte) % ROOM_CODE_ALPHABET.len()] as char)
        .collect::<String>();
    RoomCode::parse(code).map_err(|_| id_exhausted())
}

fn generate_invitation_token() -> Result<InvitationToken, ProtocolError> {
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|_| id_exhausted())?;
    InvitationToken::parse(hex::encode(random)).map_err(|_| id_exhausted())
}

fn id_exhausted() -> ProtocolError {
    protocol_error(
        ProtocolErrorCode::Internal,
        "monotonic identifier or secure token allocation failed",
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiplayer::room_actor::SessionTransport;
    use rhythm_chart::{
        BranchDecisionHint, BranchDecisionPoint, BranchSegment, CanonicalChart, ChartMetadata,
        Lane, LaneOrRegion, LaneRole, Object, ObjectKind, TempoChange, SCROLL_SCALE,
    };
    use taiko_multiplayer_protocol::{
        ClientBuild, ClientCommand, ClientHello, DisplayName, MembershipGranted, RoomSnapshot,
        TimeSyncReceipt, TimeSyncRequest,
    };
    use taiko_resource_protocol::{
        canonical_chart_sha256, song_manifest_sha256, ResourceBranchDecisionPoint, ResourceCourse,
        ResourceSong, API_VERSION, WIRE_SCHEMA_SHA256 as RESOURCE_WIRE_SCHEMA_SHA256,
    };

    fn library() -> ResourceLibraryDocument {
        ResourceLibraryDocument {
            api_version: API_VERSION,
            wire_schema_sha256: RESOURCE_WIRE_SCHEMA_SHA256.to_owned(),
            semantics: crate::current_resource_semantics(),
            songs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn authoritative_catalog(library: &ResourceLibraryDocument) -> crate::AuthoritativeCatalog {
        crate::AuthoritativeCatalog {
            semantics: library.semantics.clone(),
            songs_by_id: Arc::new(HashMap::new()),
        }
    }

    fn fixture_hash(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    fn coherent_library_and_catalog() -> (ResourceLibraryDocument, crate::AuthoritativeCatalog) {
        let semantics = crate::current_resource_semantics();
        let decisions = vec![BranchDecisionPoint {
            segment_id: 7,
            decision_tick: 100_000,
            default_route_id: 0,
            route_count: 3,
            hint: Some(BranchDecisionHint::Score { low: 1, high: 2 }),
        }];
        let chart = CanonicalChart {
            metadata: ChartMetadata {
                title: "Registry Coherence".to_owned(),
                difficulty_name: Some("Oni".to_owned()),
                difficulty_level: Some(8),
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
            branch_segments: vec![BranchSegment {
                id: 7,
                default_route_id: 0,
                route_count: 3,
                decision_hint: decisions[0].hint.clone(),
            }],
            objects: (0_u8..3)
                .map(|route_id| Object {
                    id: u32::from(route_id) + 1,
                    kind: ObjectKind::Tap,
                    start_tick: 1_000_000,
                    end_tick: 1_000_000,
                    lane_or_region: LaneOrRegion::Lane(if route_id == 0 { 0 } else { 1 }),
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: SCROLL_SCALE,
                    branch_segment_id: Some(7),
                    branch_route_id: route_id,
                })
                .collect(),
            events: Vec::new(),
        };
        let course = ResourceCourse {
            index: 0,
            name: "Oni".to_owned(),
            level: Some(8),
            canonical_chart_hash: canonical_chart_sha256(&chart).expect("canonical chart hash"),
            object_count: u32::try_from(chart.objects.len()).expect("object count"),
            branch_segment_count: u32::try_from(chart.branch_segments.len())
                .expect("branch segment count"),
            base_bpm: Some(120.0),
            branch_decisions: decisions
                .iter()
                .map(|decision| ResourceBranchDecisionPoint {
                    segment_id: decision.segment_id,
                    decision_tick: decision.decision_tick,
                    default_route_id: decision.default_route_id,
                    route_count: decision.route_count,
                    hint: decision.hint.clone(),
                })
                .collect(),
        };
        let source_id = fixture_hash(0x21);
        let audio_id = fixture_hash(0x22);
        let song_id = song_manifest_sha256(
            &source_id,
            Some(&audio_id),
            &semantics,
            std::slice::from_ref(&course),
        )
        .expect("song manifest hash");
        let resource_song = ResourceSong {
            song_id: song_id.clone(),
            source_path: "registry/song.tja".to_owned(),
            source_id,
            audio_path: Some("registry/song.ogg".to_owned()),
            audio_id: Some(audio_id),
            title: "Registry Coherence".to_owned(),
            subtitle: String::new(),
            artist: "Test".to_owned(),
            demo_start_seconds: 0.0,
            courses: vec![course.clone()],
        };
        let library = ResourceLibraryDocument {
            api_version: API_VERSION,
            wire_schema_sha256: RESOURCE_WIRE_SCHEMA_SHA256.to_owned(),
            semantics: semantics.clone(),
            songs: vec![resource_song.clone()],
            warnings: Vec::new(),
        };
        library.validate().expect("coherent resource library");
        let authoritative_song = Arc::new(crate::AuthoritativeSong {
            manifest: resource_song,
            courses: vec![crate::AuthoritativeCourse {
                manifest: course,
                chart: Arc::new(chart),
                branch_decisions: decisions.into(),
            }]
            .into_boxed_slice(),
        });
        let authoritative_catalog = crate::AuthoritativeCatalog {
            semantics,
            songs_by_id: Arc::new(HashMap::from([(song_id, authoritative_song)])),
        };
        (library, authoritative_catalog)
    }

    fn hello(name: &str, resume: Option<ResumeRequest>) -> ClientMessage {
        ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("schema"),
            client_build: ClientBuild::new("registry-test").expect("build"),
            display_name: DisplayName::new(name).expect("name"),
            resume,
        })
    }

    async fn receive(transport: &mut SessionTransport) -> ServerMessage {
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                message = transport.reliable_rx.recv() => {
                    message.expect("reliable channel open")
                }
                changed = transport.close_rx.changed() => {
                    changed.expect("close channel open");
                    panic!("session closed: {:?}", transport.close_rx.borrow().clone());
                }
            }
        })
        .await
        .expect("message timeout")
    }

    async fn fresh_session(
        registry: &MultiplayerRegistry,
        name: &str,
    ) -> (SessionId, SessionTransport) {
        let (outbound, mut transport) = SessionOutbound::channel();
        let session_id = registry.register_session(outbound).await.expect("session");
        registry
            .handle_client_message(session_id, hello(name, None))
            .await;
        assert!(matches!(
            receive(&mut transport).await,
            ServerMessage::Welcome(_)
        ));
        (session_id, transport)
    }

    async fn create_room(
        registry: &MultiplayerRegistry,
        session_id: SessionId,
        transport: &mut SessionTransport,
    ) -> (MembershipGranted, CommandAck, RoomSnapshot) {
        let create = CommandEnvelope {
            seq: FIRST_COMMAND_SEQ,
            expected_room_revision: None,
            command: ClientCommand::CreateRoom,
        };
        registry
            .handle_client_message(session_id, ClientMessage::Command(create))
            .await;
        let ServerMessage::MembershipGranted(membership) = receive(transport).await else {
            panic!("expected membership");
        };
        let ServerMessage::CommandAck(ack) = receive(transport).await else {
            panic!("expected command ack");
        };
        let ServerMessage::RoomSnapshot(snapshot) = receive(transport).await else {
            panic!("expected room snapshot");
        };
        snapshot.validate().expect("valid room snapshot");
        (membership, ack, *snapshot)
    }

    #[test]
    fn session_message_budget_is_deterministic_and_bounded() {
        let start = Instant::now();
        let mut limiter = MessageRateLimiter::new(start);
        for _ in 0..SESSION_MESSAGE_BURST {
            assert!(limiter.allow(start));
        }
        assert!(!limiter.allow(start));
        let one_token_later =
            start + Duration::from_micros(1_000_000 / SESSION_MESSAGE_RATE_PER_SECOND);
        assert!(limiter.allow(one_token_later));
        assert!(!limiter.allow(one_token_later));
    }

    #[tokio::test]
    async fn registry_accepts_a_coherent_retained_authoritative_catalog() {
        let (library, catalog) = coherent_library_and_catalog();
        MultiplayerRegistry::try_new(&library, catalog)
            .expect("coherent retained catalog must establish authority");
    }

    #[tokio::test]
    async fn registry_rejects_equal_song_counts_with_different_song_id_sets() {
        let (library, mut catalog) = coherent_library_and_catalog();
        let resource_song_id = library.songs[0].song_id.clone();
        let replacement_id = if resource_song_id == fixture_hash(0) {
            fixture_hash(1)
        } else {
            fixture_hash(0)
        };
        let retained_song = catalog
            .songs_by_id
            .values()
            .next()
            .expect("retained song")
            .clone();
        catalog.songs_by_id = Arc::new(HashMap::from([(replacement_id, retained_song)]));

        let error = MultiplayerRegistry::try_new(&library, catalog)
            .err()
            .expect("different song ID sets must fail closed");
        assert!(
            format!("{error:#}").contains("song ID sets differ"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn registry_rejects_retained_canonical_chart_hash_drift() {
        let (library, mut catalog) = coherent_library_and_catalog();
        let song_id = library.songs[0].song_id.clone();
        let songs = Arc::make_mut(&mut catalog.songs_by_id);
        let song = Arc::make_mut(songs.get_mut(&song_id).expect("retained song"));
        Arc::make_mut(&mut song.courses[0].chart)
            .metadata
            .title
            .push_str(" drift");

        let error = MultiplayerRegistry::try_new(&library, catalog)
            .err()
            .expect("retained canonical chart drift must fail closed");
        assert!(
            format!("{error:#}").contains("canonical chart hash drift"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn registry_rejects_retained_branch_decision_drift() {
        let (library, mut catalog) = coherent_library_and_catalog();
        let song_id = library.songs[0].song_id.clone();
        let songs = Arc::make_mut(&mut catalog.songs_by_id);
        let song = Arc::make_mut(songs.get_mut(&song_id).expect("retained song"));
        Arc::make_mut(&mut song.courses[0].branch_decisions)[0].decision_tick += 1;

        let error = MultiplayerRegistry::try_new(&library, catalog)
            .err()
            .expect("retained branch decision drift must fail closed");
        assert!(
            format!("{error:#}").contains("branch decision 0 differs"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn registry_session_capacity_is_hard_bounded() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        for _ in 0..MAX_CONCURRENT_SESSIONS {
            let (outbound, _transport) = SessionOutbound::channel();
            registry
                .register_session(outbound)
                .await
                .expect("slot is available");
        }
        let (overflow, _transport) = SessionOutbound::channel();
        let error = registry
            .register_session(overflow)
            .await
            .expect_err("session capacity is enforced");
        assert_eq!(error.code, ProtocolErrorCode::ServerBusy);
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn registry_room_capacity_is_hard_bounded() {
        fn deterministic_room_code(mut value: usize) -> RoomCode {
            let mut bytes = [ROOM_CODE_ALPHABET[0]; 4];
            for byte in bytes.iter_mut().rev() {
                *byte = ROOM_CODE_ALPHABET[value % ROOM_CODE_ALPHABET.len()];
                value /= ROOM_CODE_ALPHABET.len();
            }
            RoomCode::parse(String::from_utf8(bytes.to_vec()).expect("ASCII room code"))
                .expect("generated room code")
        }

        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let dummy_code = deterministic_room_code(0);
        let dummy = spawn_room(
            dummy_code,
            1,
            InvitationToken::parse("3".repeat(64)).expect("invitation"),
            registry.catalog.clone(),
            registry.authoritative_catalog.clone(),
            registry.clock.clone(),
            registry.lifecycle_tx.clone(),
        );
        {
            let mut state = registry.inner.lock().await;
            for index in 0..MAX_CONCURRENT_ROOMS {
                state
                    .rooms
                    .insert(deterministic_room_code(index), dummy.clone());
            }
        }

        let error = match registry.allocate_room().await {
            Err(error) => error,
            Ok(_) => panic!("room capacity is enforced"),
        };
        assert_eq!(error.code, ProtocolErrorCode::ServerBusy);
        assert!(error.retryable);
    }

    #[tokio::test]
    async fn clock_probe_receipts_are_single_use_and_session_bound() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let (session_id, mut transport) = fresh_session(&registry, "clock-client").await;

        registry
            .handle_client_message(
                session_id,
                ClientMessage::TimeSync(TimeSyncRequest {
                    nonce: 77,
                    client_send_us: 123,
                }),
            )
            .await;
        let ServerMessage::TimeSync(response) = receive(&mut transport).await else {
            panic!("expected clock challenge");
        };
        assert_eq!(response.nonce, 77);
        assert_eq!(response.client_send_us, 123);
        let receipt = TimeSyncReceipt {
            nonce: response.nonce,
            probe_token: response.probe_token,
        };
        registry
            .handle_client_message(session_id, ClientMessage::TimeSyncReceipt(receipt.clone()))
            .await;
        let ServerMessage::ClockProbeAck(ack) = receive(&mut transport).await else {
            panic!("expected verified clock acknowledgement");
        };
        assert_eq!(ack.nonce, 77);
        assert_eq!(ack.quality.accepted_samples, 1);
        assert!(!ack.ready);

        let (second_id, _second_transport) = fresh_session(&registry, "fresh-clock").await;
        let second_quality = registry
            .command_session_snapshot(second_id)
            .await
            .expect("fresh session")
            .3;
        assert!(!second_quality.is_ready_at(Instant::now()));

        registry
            .handle_client_message(session_id, ClientMessage::TimeSyncReceipt(receipt))
            .await;
        tokio::time::timeout(Duration::from_secs(1), transport.close_rx.changed())
            .await
            .expect("replay closes promptly")
            .expect("close channel remains open");
        let close = transport
            .close_rx
            .borrow_and_update()
            .clone()
            .expect("typed close reason");
        assert_eq!(close.code, ProtocolErrorCode::InvalidMessage);
        assert!(!close.retryable);
    }

    #[tokio::test]
    async fn hello_create_duplicate_and_resume_round_trip() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let (session_id, mut transport) = fresh_session(&registry, "host").await;
        let (membership, original_ack, snapshot) =
            create_room(&registry, session_id, &mut transport).await;

        registry
            .handle_client_message(
                session_id,
                ClientMessage::Command(CommandEnvelope {
                    seq: FIRST_COMMAND_SEQ,
                    expected_room_revision: None,
                    command: ClientCommand::CreateRoom,
                }),
            )
            .await;
        let ServerMessage::CommandAck(duplicate_ack) = receive(&mut transport).await else {
            panic!("expected duplicate ack");
        };
        assert_eq!(duplicate_ack, original_ack);

        registry.remove_session(session_id).await;
        let (outbound, mut resumed_transport) = SessionOutbound::channel();
        let resumed_session = registry
            .register_session(outbound)
            .await
            .expect("resumed session");
        registry
            .handle_client_message(
                resumed_session,
                hello(
                    "host",
                    Some(ResumeRequest {
                        room_code: membership.room_code.clone(),
                        actor_id: membership.actor_id.clone(),
                        token: membership.resume_token.clone(),
                        last_room_revision: snapshot.revision,
                        last_acked_command_seq: original_ack.seq,
                    }),
                ),
            )
            .await;
        let ServerMessage::Welcome(welcome) = receive(&mut resumed_transport).await else {
            panic!("expected welcome");
        };
        assert!(welcome.resumed);
        assert_eq!(
            welcome.next_expected_command_seq,
            original_ack.next_expected_seq
        );
        let ServerMessage::MembershipGranted(resumed_membership) =
            receive(&mut resumed_transport).await
        else {
            panic!("expected resumed membership");
        };
        assert_eq!(resumed_membership.actor_id, membership.actor_id);
        assert_eq!(resumed_membership.resume_token, membership.resume_token);
        let ServerMessage::RoomSnapshot(resumed_snapshot) = receive(&mut resumed_transport).await
        else {
            panic!("expected resumed snapshot");
        };
        resumed_snapshot.validate().expect("valid resumed snapshot");
    }

    #[tokio::test]
    async fn lost_leave_ack_is_replayed_after_membership_is_cleared() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let (host_id, mut host_transport) = fresh_session(&registry, "host").await;
        let (host_membership, _, _) = create_room(&registry, host_id, &mut host_transport).await;
        let (session_id, mut transport) = fresh_session(&registry, "guest").await;
        registry
            .handle_client_message(
                session_id,
                ClientMessage::Command(CommandEnvelope {
                    seq: FIRST_COMMAND_SEQ,
                    expected_room_revision: None,
                    command: ClientCommand::JoinRoom {
                        room_code: host_membership.room_code,
                        invitation_token: host_membership.invitation_token,
                        role: JoinRole::Player,
                    },
                }),
            )
            .await;
        assert!(matches!(
            receive(&mut transport).await,
            ServerMessage::MembershipGranted(_)
        ));
        let ServerMessage::CommandAck(join_ack) = receive(&mut transport).await else {
            panic!("expected join acknowledgement");
        };
        let ServerMessage::RoomSnapshot(snapshot) = receive(&mut transport).await else {
            panic!("expected joined room snapshot");
        };
        let leave = CommandEnvelope {
            seq: join_ack.next_expected_seq,
            expected_room_revision: Some(snapshot.revision),
            command: ClientCommand::LeaveRoom,
        };

        registry
            .handle_client_message(session_id, ClientMessage::Command(leave.clone()))
            .await;
        let ServerMessage::CommandAck(original_ack) = receive(&mut transport).await else {
            panic!("expected original leave acknowledgement");
        };
        assert!(matches!(
            original_ack.outcome,
            CommandOutcome::Applied { .. }
        ));
        assert!(
            registry
                .inner
                .lock()
                .await
                .sessions
                .get(&session_id)
                .expect("session remains registered")
                .membership
                .is_none(),
            "successful leave must clear membership before a retry"
        );

        // Treat the first acknowledgement as lost and retry the identical
        // terminal command on the still-open session.
        registry
            .handle_client_message(session_id, ClientMessage::Command(leave.clone()))
            .await;
        let ServerMessage::CommandAck(replayed_ack) = receive(&mut transport).await else {
            panic!("expected replayed leave acknowledgement");
        };
        assert_eq!(replayed_ack, original_ack);

        let conflicting = CommandEnvelope {
            command: ClientCommand::CreateRoom,
            ..leave
        };
        registry
            .handle_client_message(session_id, ClientMessage::Command(conflicting))
            .await;
        let ServerMessage::CommandAck(conflict_ack) = receive(&mut transport).await else {
            panic!("expected conflicting retry rejection");
        };
        let CommandOutcome::Rejected { error, .. } = conflict_ack.outcome else {
            panic!("same sequence with different contents must be rejected");
        };
        assert_eq!(error.code, ProtocolErrorCode::InvalidMessage);
        assert_eq!(
            conflict_ack.next_expected_seq,
            original_ack.next_expected_seq
        );
    }

    #[tokio::test]
    async fn non_consuming_gap_and_old_duplicate_never_poison_session_progress() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let (host_id, mut host_transport) = fresh_session(&registry, "host").await;
        let (host_membership, _, _) = create_room(&registry, host_id, &mut host_transport).await;
        let (guest_id, mut guest_transport) = fresh_session(&registry, "guest").await;
        let join = CommandEnvelope {
            seq: FIRST_COMMAND_SEQ,
            expected_room_revision: None,
            command: ClientCommand::JoinRoom {
                room_code: host_membership.room_code,
                invitation_token: host_membership.invitation_token,
                role: JoinRole::Player,
            },
        };
        registry
            .handle_client_message(guest_id, ClientMessage::Command(join.clone()))
            .await;
        assert!(matches!(
            receive(&mut guest_transport).await,
            ServerMessage::MembershipGranted(_)
        ));
        let ServerMessage::CommandAck(join_ack) = receive(&mut guest_transport).await else {
            panic!("expected join acknowledgement");
        };
        let ServerMessage::RoomSnapshot(snapshot) = receive(&mut guest_transport).await else {
            panic!("expected joined room snapshot");
        };

        let leave = CommandEnvelope {
            seq: CommandSeq(3),
            expected_room_revision: Some(snapshot.revision),
            command: ClientCommand::LeaveRoom,
        };
        registry
            .handle_client_message(guest_id, ClientMessage::Command(leave.clone()))
            .await;
        let ServerMessage::CommandAck(gap_ack) = receive(&mut guest_transport).await else {
            panic!("expected future-sequence rejection");
        };
        assert_eq!(gap_ack.next_expected_seq, CommandSeq(2));
        assert!(matches!(
            gap_ack.outcome,
            CommandOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::SequenceGap,
                    ..
                },
                ..
            }
        ));

        registry
            .handle_client_message(
                guest_id,
                ClientMessage::Command(CommandEnvelope {
                    seq: CommandSeq(2),
                    expected_room_revision: None,
                    command: ClientCommand::SetReady {
                        match_id: taiko_multiplayer_protocol::MatchId(1),
                        ready: false,
                        proof: None,
                    },
                }),
            )
            .await;
        let ServerMessage::CommandAck(filler_ack) = receive(&mut guest_transport).await else {
            panic!("expected contiguous filler acknowledgement");
        };
        assert_eq!(filler_ack.next_expected_seq, CommandSeq(3));
        assert!(matches!(
            filler_ack.outcome,
            CommandOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::InvalidStage,
                    ..
                },
                ..
            }
        ));

        registry
            .handle_client_message(guest_id, ClientMessage::Command(join))
            .await;
        let ServerMessage::CommandAck(duplicate_join_ack) = receive(&mut guest_transport).await
        else {
            panic!("expected old duplicate acknowledgement");
        };
        assert_eq!(duplicate_join_ack, join_ack);
        assert_eq!(
            registry
                .inner
                .lock()
                .await
                .sessions
                .get(&guest_id)
                .expect("guest session")
                .next_expected_command,
            CommandSeq(3),
            "an old room-cache duplicate must not regress the session watermark"
        );

        registry
            .handle_client_message(guest_id, ClientMessage::Command(leave))
            .await;
        let ServerMessage::CommandAck(leave_ack) = receive(&mut guest_transport).await else {
            panic!("expected retried LeaveRoom acknowledgement");
        };
        assert_eq!(leave_ack.next_expected_seq, CommandSeq(4));
        assert!(matches!(leave_ack.outcome, CommandOutcome::Applied { .. }));
    }

    #[tokio::test]
    async fn rejected_invitation_consumes_sequence_before_successful_retry() {
        let library = library();
        let registry = MultiplayerRegistry::try_new(&library, authoritative_catalog(&library))
            .expect("registry");
        let (host_id, mut host_transport) = fresh_session(&registry, "host").await;
        let (membership, _, _) = create_room(&registry, host_id, &mut host_transport).await;
        let (guest_id, mut guest_transport) = fresh_session(&registry, "guest").await;

        let wrong_join = ClientCommand::JoinRoom {
            room_code: membership.room_code.clone(),
            invitation_token: InvitationToken::parse("d".repeat(64)).expect("wrong invite"),
            role: JoinRole::Player,
        };
        registry
            .handle_client_message(
                guest_id,
                ClientMessage::Command(CommandEnvelope {
                    seq: CommandSeq(1),
                    expected_room_revision: None,
                    command: wrong_join,
                }),
            )
            .await;
        let ServerMessage::CommandAck(rejected) = receive(&mut guest_transport).await else {
            panic!("expected rejection");
        };
        assert!(matches!(
            rejected.outcome,
            CommandOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::InvalidInvitation,
                    ..
                },
                ..
            }
        ));
        assert_eq!(rejected.next_expected_seq, CommandSeq(2));

        registry
            .handle_client_message(
                guest_id,
                ClientMessage::Command(CommandEnvelope {
                    seq: CommandSeq(2),
                    expected_room_revision: None,
                    command: ClientCommand::JoinRoom {
                        room_code: membership.room_code,
                        invitation_token: membership.invitation_token,
                        role: JoinRole::Player,
                    },
                }),
            )
            .await;
        assert!(matches!(
            receive(&mut guest_transport).await,
            ServerMessage::MembershipGranted(_)
        ));
        let ServerMessage::CommandAck(applied) = receive(&mut guest_transport).await else {
            panic!("expected applied join");
        };
        assert_eq!(applied.seq, CommandSeq(2));
        assert_eq!(applied.next_expected_seq, CommandSeq(3));
        let ServerMessage::RoomSnapshot(snapshot) = receive(&mut guest_transport).await else {
            panic!("expected snapshot");
        };
        snapshot.validate().expect("valid snapshot");
    }

    #[test]
    fn clock_probe_capacity_is_retryable_but_forgery_is_terminal() {
        let capacity = clock_probe_protocol_error(ClockProbeError::PendingCapacity);
        assert_eq!(capacity.code, ProtocolErrorCode::ServerBusy);
        assert!(capacity.retryable);

        for violation in [
            ClockProbeError::DuplicateNonce,
            ClockProbeError::UnknownNonce,
            ClockProbeError::TokenMismatch,
        ] {
            let error = clock_probe_protocol_error(violation);
            assert_eq!(error.code, ProtocolErrorCode::InvalidMessage);
            assert!(!error.retryable);
        }
    }
}
