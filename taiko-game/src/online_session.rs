use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use taiko_multiplayer_protocol::{
    ActorId, BoundedVec, ClockProbeAck, ClockQuality, CommandAck, CommandEnvelope, CommandOutcome,
    CommandSeq, ContentHash, DrumAction, FinalResult, Heartbeat, InputAck, InputBatch, InputEvent,
    InputOutcome, InputSeq, LiveStateSnapshot, MatchId, MatchManifest, MembershipGranted,
    PlayerConnection, PlayerId, PlayerLiveState, PlayerPreparation, PlayerSelection,
    PreparationProgress, PreparationProof, ProtocolError, ProtocolErrorCode, RoomRevision,
    RoomRole, RoomSnapshot, RoomStage, ServerMessage, SongId, SongManifest, StateSeq,
    TimeSyncReceipt, TimeSyncRequest, TimeSyncResponse, FIRST_COMMAND_SEQ, FIRST_INPUT_SEQ,
    MAX_INPUT_BATCH_EVENTS, PROTOCOL_VERSION, WIRE_SCHEMA_SHA256,
};

use crate::online::{
    LocalPlayerRuntime, NetworkClient, NetworkEvent, OnlineClientConfig, PreparedMatch, RoomIntent,
};

const CLOCK_SAMPLE_WINDOW: usize = 64;
const CLOCK_MIN_READY_SAMPLES: u64 = 4;
const CLOCK_OUTLIER_MIN_SAMPLES: usize = 8;
const CLOCK_OUTLIER_MAD_MULTIPLIER: u64 = 6;
const CLOCK_OUTLIER_FIXED_MARGIN_US: u64 = 20_000;
const CLOCK_SLEW_LIMIT_US_PER_SECOND: i64 = 50_000;
const FAST_TIME_SYNC_INTERVAL_US: u64 = 250_000;
const STEADY_TIME_SYNC_INTERVAL_US: u64 = 2_000_000;
const COMMAND_RETRY_INTERVAL_US: u64 = 500_000;
const INPUT_RETRY_INTERVAL_US: u64 = 100_000;
const MAX_PENDING_COMMANDS: usize = 64;
const MAX_PENDING_INPUTS: usize = 512;
const MAX_PENDING_TIME_SYNC: usize = 16;
const MAX_PENDING_CLOCK_PROBE_ACKS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnlinePhase {
    Connecting,
    Joining,
    Lobby,
    Spectating,
    SelectingCourse,
    Downloading,
    Verifying,
    Loading,
    Prepared,
    Ready,
    Countdown,
    Playing,
    Finalizing,
    Results,
    Reconnecting,
    Failed,
}

impl OnlinePhase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting",
            Self::Joining => "Joining room",
            Self::Lobby => "Lobby",
            Self::Spectating => "Spectating (waiting for players)",
            Self::SelectingCourse => "Selecting course",
            Self::Downloading => "Downloading",
            Self::Verifying => "Verifying content",
            Self::Loading => "Loading chart and audio",
            Self::Prepared => "Prepared (not ready)",
            Self::Ready => "Ready",
            Self::Countdown => "Countdown",
            Self::Playing => "Playing",
            Self::Finalizing => "Finalizing authoritative result",
            Self::Results => "Results",
            Self::Reconnecting => "Reconnecting",
            Self::Failed => "Connection failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OnlineError {
    pub(crate) code: Option<ProtocolErrorCode>,
    pub(crate) message: String,
    pub(crate) retryable: bool,
}

impl OnlineError {
    fn protocol(error: ProtocolError) -> Self {
        Self {
            code: Some(error.code),
            message: error.message.into_string(),
            retryable: error.retryable,
        }
    }

    fn local(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: None,
            message: message.into(),
            retryable,
        }
    }

    pub(crate) fn display_message(&self) -> String {
        let message = match self.code {
            Some(code) => format!("{code:?}: {}", self.message),
            None => self.message.clone(),
        };
        if self.retryable {
            format!("{message} (retryable)")
        } else {
            message
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum DomainAction {
    SongChanged { song: Box<SongManifest> },
    PhaseChanged(OnlinePhase),
    PlaybackInvalidated(OnlinePlaybackInvalidation),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnlinePlaybackInvalidation {
    CountdownAborted,
    ScheduledStartChanged,
    MatchEpochChanged,
}

#[derive(Debug, Clone, Copy)]
struct ClockSample {
    round_trip_us: u64,
    offset_us: i64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ClockSyncEstimator {
    samples: VecDeque<ClockSample>,
    current_offset_us: i64,
    target_offset_us: i64,
    accepted_samples: u64,
    rejected_samples: u64,
    last_slew_local_us: Option<u64>,
}

impl ClockSyncEstimator {
    pub(crate) fn observe(
        &mut self,
        client_send_us: u64,
        server_receive_us: u64,
        server_send_us: u64,
        client_receive_us: u64,
    ) -> bool {
        if client_receive_us < client_send_us || server_send_us < server_receive_us {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return false;
        }

        let client_elapsed = client_receive_us - client_send_us;
        let server_elapsed = server_send_us - server_receive_us;
        if server_elapsed > client_elapsed {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return false;
        }
        let round_trip_us = client_elapsed - server_elapsed;

        if self.samples.len() >= CLOCK_OUTLIER_MIN_SAMPLES {
            let median = median_u64(self.samples.iter().map(|sample| sample.round_trip_us))
                .unwrap_or(round_trip_us);
            let mad = median_u64(
                self.samples
                    .iter()
                    .map(|sample| sample.round_trip_us.abs_diff(median)),
            )
            .unwrap_or_default()
            .max(1);
            let cutoff = median
                .saturating_add(mad.saturating_mul(CLOCK_OUTLIER_MAD_MULTIPLIER))
                .saturating_add(CLOCK_OUTLIER_FIXED_MARGIN_US);
            if round_trip_us > cutoff {
                self.rejected_samples = self.rejected_samples.saturating_add(1);
                return false;
            }
        }

        let left = i128::from(server_receive_us) - i128::from(client_send_us);
        let right = i128::from(server_send_us) - i128::from(client_receive_us);
        let offset = ((left + right) / 2).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64;
        if self.samples.len() == CLOCK_SAMPLE_WINDOW {
            self.samples.pop_front();
        }
        self.samples.push_back(ClockSample {
            round_trip_us,
            offset_us: offset,
        });
        self.accepted_samples = self.accepted_samples.saturating_add(1);
        self.target_offset_us =
            median_i64(self.samples.iter().map(|sample| sample.offset_us)).unwrap_or(offset);
        if self.accepted_samples == 1 {
            self.current_offset_us = self.target_offset_us;
        }
        true
    }

    pub(crate) fn tick(&mut self, local_now_us: u64) {
        let Some(previous_local_us) = self.last_slew_local_us.replace(local_now_us) else {
            return;
        };
        let elapsed_us = local_now_us.saturating_sub(previous_local_us);
        if elapsed_us == 0 {
            return;
        }
        let maximum_step = (i128::from(CLOCK_SLEW_LIMIT_US_PER_SECOND) * i128::from(elapsed_us)
            / 1_000_000)
            .max(1)
            .min(i128::from(i64::MAX)) as i64;
        let difference = self.target_offset_us - self.current_offset_us;
        self.current_offset_us = self
            .current_offset_us
            .saturating_add(difference.clamp(-maximum_step, maximum_step));
    }

    pub(crate) fn estimated_server_now_us(&self, local_now_us: u64) -> u64 {
        (i128::from(local_now_us) + i128::from(self.current_offset_us))
            .max(0)
            .min(i128::from(u64::MAX)) as u64
    }

    pub(crate) fn is_ready(&self) -> bool {
        self.accepted_samples >= CLOCK_MIN_READY_SAMPLES
    }

    pub(crate) fn quality(&self) -> ClockQuality {
        let p95_rtt_us = percentile_u64(self.samples.iter().map(|sample| sample.round_trip_us), 95)
            .unwrap_or_default();
        let mut previous = None;
        let jitter_values = self.samples.iter().filter_map(|sample| {
            let jitter = previous.map(|value: u64| value.abs_diff(sample.round_trip_us));
            previous = Some(sample.round_trip_us);
            jitter
        });
        let jitter_us = percentile_u64(jitter_values, 95).unwrap_or_default();
        ClockQuality {
            accepted_samples: self.accepted_samples.min(u64::from(u16::MAX)) as u16,
            p95_rtt_ms: micros_to_millis_ceil(p95_rtt_us),
            jitter_ms: micros_to_millis_ceil(jitter_us),
        }
    }

    #[cfg(test)]
    fn rejected_samples(&self) -> u64 {
        self.rejected_samples
    }
}

#[derive(Debug, Clone)]
struct PendingCommand {
    envelope: CommandEnvelope,
    sent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SubmittedInput {
    pub(crate) seq: InputSeq,
    pub(crate) tick: taiko_multiplayer_protocol::Tick,
}

pub(crate) struct OnlineDomain {
    network: NetworkClient,
    config: OnlineClientConfig,
    clock_origin: Instant,
    connected: bool,
    welcome_heartbeat_interval_us: u64,
    reconnect_grace_us: u64,
    reconnect_deadline_local_us: Option<u64>,
    awaiting_resume_snapshot: bool,
    phase: OnlinePhase,
    status_message: String,
    error: Option<OnlineError>,
    membership: Option<MembershipGranted>,
    pub(crate) snapshot: Option<RoomSnapshot>,
    pub(crate) live_states: HashMap<PlayerId, PlayerLiveState>,
    pub(crate) final_results: HashMap<PlayerId, FinalResult>,
    last_live_epoch: Option<(MatchId, StateSeq)>,
    active_match_id: Option<MatchId>,
    revision_hint: Option<RoomRevision>,
    next_command_seq: CommandSeq,
    last_acked_command_seq: CommandSeq,
    pending_commands: BTreeMap<CommandSeq, PendingCommand>,
    last_command_flush_us: Option<u64>,
    next_input_seq: InputSeq,
    last_submitted_input_tick: Option<taiko_multiplayer_protocol::Tick>,
    last_input_ack: Option<InputSeq>,
    pending_inputs: BTreeMap<InputSeq, InputEvent>,
    highest_sent_input: Option<InputSeq>,
    last_input_flush_us: Option<u64>,
    next_nonce: u64,
    pending_time_sync: BTreeMap<u64, u64>,
    pending_clock_probe_acks: BTreeSet<u64>,
    latest_clock_probe_ack: Option<ClockProbeAck>,
    last_time_sync_us: Option<u64>,
    last_heartbeat_us: Option<u64>,
    clock_sync: ClockSyncEstimator,
    pub(crate) prepared_match: Option<PreparedMatch>,
    pub(crate) local_player: Option<LocalPlayerRuntime>,
    pub(crate) local_course_index: usize,
    wants_ready: bool,
    pub(crate) pending_actions: Vec<DomainAction>,
}

impl OnlineDomain {
    pub(crate) fn connect(config: OnlineClientConfig) -> Result<Self> {
        let network = NetworkClient::connect(&config)?;
        Ok(Self::with_network(config, network))
    }

    fn with_network(config: OnlineClientConfig, network: NetworkClient) -> Self {
        Self {
            network,
            config,
            clock_origin: Instant::now(),
            connected: false,
            welcome_heartbeat_interval_us: 1_000_000,
            reconnect_grace_us: 15_000_000,
            reconnect_deadline_local_us: None,
            awaiting_resume_snapshot: false,
            phase: OnlinePhase::Connecting,
            status_message: "Connecting to authoritative server…".to_owned(),
            error: None,
            membership: None,
            snapshot: None,
            live_states: HashMap::new(),
            final_results: HashMap::new(),
            last_live_epoch: None,
            active_match_id: None,
            revision_hint: None,
            next_command_seq: FIRST_COMMAND_SEQ,
            last_acked_command_seq: CommandSeq(0),
            pending_commands: BTreeMap::new(),
            last_command_flush_us: None,
            next_input_seq: FIRST_INPUT_SEQ,
            last_submitted_input_tick: None,
            last_input_ack: None,
            pending_inputs: BTreeMap::new(),
            highest_sent_input: None,
            last_input_flush_us: None,
            next_nonce: 1,
            pending_time_sync: BTreeMap::new(),
            pending_clock_probe_acks: BTreeSet::new(),
            latest_clock_probe_ack: None,
            last_time_sync_us: None,
            last_heartbeat_us: None,
            clock_sync: ClockSyncEstimator::default(),
            prepared_match: None,
            local_player: None,
            local_course_index: 0,
            wants_ready: false,
            pending_actions: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_network(config: OnlineClientConfig, network: NetworkClient) -> Self {
        Self::with_network(config, network)
    }

    #[cfg(test)]
    pub(crate) fn assume_playing_player_for_test(
        &mut self,
        player_id: PlayerId,
        match_id: MatchId,
    ) {
        self.membership = Some(MembershipGranted {
            room_code: taiko_multiplayer_protocol::RoomCode::parse("TEST")
                .expect("static test room code"),
            actor_id: ActorId::Player(player_id),
            resume_token: taiko_multiplayer_protocol::ResumeToken::parse("a".repeat(64))
                .expect("static test resume token"),
            invitation_token: taiko_multiplayer_protocol::InvitationToken::parse("b".repeat(64))
                .expect("static test invitation token"),
        });
        self.active_match_id = Some(match_id);
        self.phase = OnlinePhase::Playing;
    }

    pub(crate) fn tick_network(&mut self) -> Result<()> {
        let now_us = self.local_now_us();
        self.tick_at(now_us)
    }

    fn tick_at(&mut self, now_us: u64) -> Result<()> {
        if self.is_terminal() {
            return Ok(());
        }
        self.clock_sync.tick(now_us);

        loop {
            match self.network.try_recv() {
                Ok(Some(event)) => {
                    self.handle_network_event(event, now_us)?;
                    if self.is_terminal() {
                        return Ok(());
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    self.set_fatal(OnlineError::local(error.to_string(), false));
                    return Ok(());
                }
            }
        }
        if let Some(live) = self.network.take_latest_live() {
            self.ingest_live_state(live);
        }

        if self
            .reconnect_deadline_local_us
            .is_some_and(|deadline| now_us >= deadline)
        {
            self.network.shutdown();
            self.set_fatal(OnlineError::local(
                "Reconnect grace period expired; the authoritative server released this slot.",
                false,
            ));
            return Ok(());
        }

        if !self.connected {
            return Ok(());
        }

        let heartbeat_interval_us = (self.welcome_heartbeat_interval_us / 2).max(100_000);
        if self
            .last_heartbeat_us
            .is_none_or(|last| now_us.saturating_sub(last) >= heartbeat_interval_us)
        {
            self.send_heartbeat(now_us);
        }

        let time_sync_interval = if self.clock_is_ready() {
            STEADY_TIME_SYNC_INTERVAL_US
        } else {
            FAST_TIME_SYNC_INTERVAL_US
        };
        if self
            .last_time_sync_us
            .is_none_or(|last| now_us.saturating_sub(last) >= time_sync_interval)
        {
            self.send_time_sync(now_us);
        }

        if !self.pending_commands.is_empty()
            && self
                .last_command_flush_us
                .is_none_or(|last| now_us.saturating_sub(last) >= COMMAND_RETRY_INTERVAL_US)
        {
            self.flush_commands(now_us);
        }
        if !self.pending_inputs.is_empty()
            && self
                .last_input_flush_us
                .is_none_or(|last| now_us.saturating_sub(last) >= INPUT_RETRY_INTERVAL_US)
        {
            self.flush_inputs(now_us);
        }
        Ok(())
    }

    fn handle_network_event(&mut self, event: NetworkEvent, now_us: u64) -> Result<()> {
        match event {
            NetworkEvent::Connecting { attempt, delay } => {
                self.connected = false;
                self.reset_clock_probe_state();
                if self.membership.is_some() {
                    self.set_phase(OnlinePhase::Reconnecting);
                    self.status_message = if delay.is_zero() {
                        format!("Reconnecting (attempt {attempt})…")
                    } else {
                        format!("Reconnect attempt {attempt} in {} ms…", delay.as_millis())
                    };
                } else {
                    self.set_phase(OnlinePhase::Connecting);
                    self.status_message = format!("Connecting (attempt {attempt})…");
                }
            }
            NetworkEvent::Disconnected {
                reason,
                next_attempt,
                retry_in,
            } => {
                self.connected = false;
                self.reset_clock_probe_state();
                if self.membership.is_some() {
                    self.reconnect_deadline_local_us
                        .get_or_insert(now_us.saturating_add(self.reconnect_grace_us));
                    self.set_phase(OnlinePhase::Reconnecting);
                    self.status_message = format!(
                        "Connection lost: {reason}. Attempt {next_attempt} in {} ms.",
                        retry_in.as_millis()
                    );
                } else {
                    self.set_phase(OnlinePhase::Connecting);
                    self.status_message = format!("Connection failed: {reason}; retrying…");
                }
            }
            NetworkEvent::Server(message) => self.handle_server_message(*message, now_us)?,
        }
        Ok(())
    }

    fn handle_server_message(&mut self, message: ServerMessage, now_us: u64) -> Result<()> {
        match message {
            ServerMessage::Welcome(welcome) => {
                let expected_hash =
                    ContentHash::parse(WIRE_SCHEMA_SHA256).expect("schema hash constant");
                if welcome.protocol_version != PROTOCOL_VERSION
                    || welcome.wire_schema_sha256 != expected_hash
                {
                    self.set_fatal(OnlineError::local(
                        "The server uses a different multiplayer protocol schema.",
                        false,
                    ));
                    return Ok(());
                }
                if let Err(message) = self.validate_welcome_context(&welcome) {
                    self.set_fatal(OnlineError::local(message, false));
                    return Ok(());
                }

                self.connected = true;
                self.reconnect_deadline_local_us = None;
                self.awaiting_resume_snapshot = welcome.resumed;
                self.welcome_heartbeat_interval_us =
                    u64::from(welcome.heartbeat_interval_ms).saturating_mul(1_000);
                self.reconnect_grace_us =
                    u64::from(welcome.reconnect_grace_ms).saturating_mul(1_000);
                self.reconcile_command_sequence(welcome.next_expected_command_seq);
                self.reset_clock_probe_state();
                self.last_heartbeat_us = None;

                if self.membership.is_none() {
                    self.set_phase(OnlinePhase::Joining);
                    self.status_message = "Connected; joining room…".to_owned();
                    if !self
                        .pending_commands
                        .values()
                        .any(|pending| is_membership_command(&pending.envelope))
                    {
                        let command = match &self.config.room_intent {
                            RoomIntent::Create => {
                                taiko_multiplayer_protocol::ClientCommand::CreateRoom
                            }
                            RoomIntent::Join {
                                room_code,
                                invitation_token,
                                role,
                            } => taiko_multiplayer_protocol::ClientCommand::JoinRoom {
                                room_code: room_code.clone(),
                                invitation_token: invitation_token.clone(),
                                role: *role,
                            },
                        };
                        self.queue_command(command, now_us)?;
                    }
                } else {
                    self.status_message = "Player identity resumed; synchronizing room…".to_owned();
                    self.flush_commands(now_us);
                    self.flush_inputs(now_us);
                }
                self.send_time_sync(now_us);
            }
            ServerMessage::MembershipGranted(granted) => {
                if let Err(message) = self.validate_membership_context(&granted) {
                    self.set_fatal(OnlineError::local(message, false));
                    return Ok(());
                }
                self.status_message = format!(
                    "Joined room {} as {}.",
                    granted.room_code,
                    match &granted.actor_id {
                        ActorId::Player(_) => "player",
                        ActorId::Spectator(_) => "spectator",
                    }
                );
                self.membership = Some(granted);
                self.refresh_resume_request();
            }
            ServerMessage::CommandAck(ack) => self.handle_command_ack(ack),
            ServerMessage::RoomSnapshot(snapshot) => self.ingest_snapshot(*snapshot),
            ServerMessage::LiveState(live) => self.ingest_live_state(live),
            ServerMessage::InputAck(ack) => self.handle_input_ack(ack),
            ServerMessage::TimeSync(response) => self.handle_time_sync(response, now_us),
            ServerMessage::ClockProbeAck(ack) => self.handle_clock_probe_ack(ack),
            ServerMessage::HeartbeatAck(_) => {}
            ServerMessage::Fatal(error) => self.set_fatal(OnlineError::protocol(error)),
        }
        Ok(())
    }

    fn validate_welcome_context(
        &self,
        welcome: &taiko_multiplayer_protocol::ServerWelcome,
    ) -> std::result::Result<(), String> {
        match (self.membership.is_some(), welcome.resumed) {
            (false, true) => {
                return Err(
                    "The server claimed to resume a connection without a local player identity."
                        .to_owned(),
                );
            }
            (true, false) => {
                return Err("The server did not resume the existing player identity.".to_owned());
            }
            _ => {}
        }

        if welcome.next_expected_command_seq.0 == 0 {
            return Err("Server sent a zero next-expected command sequence.".to_owned());
        }
        let minimum_next_expected = CommandSeq(
            self.last_acked_command_seq
                .0
                .saturating_add(1)
                .max(FIRST_COMMAND_SEQ.0),
        );
        if welcome.next_expected_command_seq < minimum_next_expected {
            return Err(format!(
                "Server regressed its handshake command watermark to {} below the acknowledged watermark {}.",
                welcome.next_expected_command_seq.0, minimum_next_expected.0
            ));
        }
        let maximum_next_expected = self.maximum_provable_next_expected_command_seq();
        if welcome.next_expected_command_seq > maximum_next_expected {
            return Err(format!(
                "Server advanced its handshake command watermark to {} beyond the client send watermark {}.",
                welcome.next_expected_command_seq.0, maximum_next_expected.0
            ));
        }
        Ok(())
    }

    fn maximum_provable_next_expected_command_seq(&self) -> CommandSeq {
        let highest_sent = self
            .pending_commands
            .iter()
            .rev()
            .find_map(|(seq, pending)| pending.sent.then_some(*seq))
            .unwrap_or(self.last_acked_command_seq)
            .max(self.last_acked_command_seq);
        CommandSeq(highest_sent.0.saturating_add(1).max(FIRST_COMMAND_SEQ.0))
    }

    fn validate_membership_context(
        &self,
        granted: &MembershipGranted,
    ) -> std::result::Result<(), String> {
        if !self.connected {
            return Err("Server granted room membership before a valid Welcome.".to_owned());
        }
        if let Some(existing) = &self.membership {
            if existing != granted {
                return Err(
                    "Server changed player identity or membership credentials during resume."
                        .to_owned(),
                );
            }
            return Ok(());
        }

        match &self.config.room_intent {
            RoomIntent::Create => {
                if !matches!(&granted.actor_id, ActorId::Player(_)) {
                    return Err(
                        "Server granted spectator identity for a create-room request.".to_owned(),
                    );
                }
            }
            RoomIntent::Join {
                room_code,
                invitation_token,
                role,
            } => {
                if granted.room_code != *room_code {
                    return Err(format!(
                        "Server granted membership in room {} instead of requested room {}.",
                        granted.room_code, room_code
                    ));
                }
                if granted.invitation_token != *invitation_token {
                    return Err(
                        "Server returned an invitation token different from the join request."
                            .to_owned(),
                    );
                }
                let actor_matches_role = matches!(
                    (role, &granted.actor_id),
                    (
                        taiko_multiplayer_protocol::JoinRole::Player,
                        ActorId::Player(_)
                    ) | (
                        taiko_multiplayer_protocol::JoinRole::Spectator,
                        ActorId::Spectator(_)
                    )
                );
                if !actor_matches_role {
                    return Err(format!(
                        "Server granted an actor identity that does not match the requested {role:?} role."
                    ));
                }
            }
        }
        Ok(())
    }

    fn reconcile_command_sequence(&mut self, next_expected: CommandSeq) {
        self.pending_commands.retain(|seq, _| *seq >= next_expected);
        for pending in self.pending_commands.values_mut() {
            pending.sent = false;
        }
        self.next_command_seq = CommandSeq(self.next_command_seq.0.max(next_expected.0));
        self.last_acked_command_seq = CommandSeq(
            self.last_acked_command_seq
                .0
                .max(next_expected.0.saturating_sub(1)),
        );
        self.refresh_resume_request();
    }

    fn handle_command_ack(&mut self, ack: CommandAck) {
        let was_pending = self.pending_commands.contains_key(&ack.seq);
        let rejected_initial_membership = self.membership.is_none()
            && self.pending_commands.get(&ack.seq).is_some_and(|pending| {
                is_membership_establishment_command_kind(&pending.envelope.command)
            })
            && matches!(&ack.outcome, CommandOutcome::Rejected { .. });
        let clock_not_ready = matches!(
            &ack.outcome,
            CommandOutcome::Rejected { error, .. }
                if error.code == ProtocolErrorCode::ClockNotReady
        );
        if let Err(message) = self.validate_command_ack_context(&ack, was_pending) {
            self.set_fatal(OnlineError::local(message, false));
            return;
        }
        self.pending_commands
            .retain(|seq, _| *seq >= ack.next_expected_seq);
        self.last_command_flush_us = None;
        self.last_acked_command_seq = CommandSeq(
            self.last_acked_command_seq
                .0
                .max(ack.next_expected_seq.0.saturating_sub(1)),
        );
        self.next_command_seq = CommandSeq(self.next_command_seq.0.max(ack.next_expected_seq.0));

        if was_pending {
            match ack.outcome {
                CommandOutcome::Applied { room_revision } => {
                    if let Some(revision) = room_revision {
                        self.revision_hint = Some(
                            self.revision_hint
                                .map_or(revision, |current| current.max(revision)),
                        );
                    }
                    self.error = None;
                }
                CommandOutcome::Rejected {
                    error,
                    current_room_revision,
                } => {
                    if let Some(revision) = current_room_revision {
                        self.revision_hint = Some(
                            self.revision_hint
                                .map_or(revision, |current| current.max(revision)),
                        );
                    }
                    let error = OnlineError::protocol(error);
                    if rejected_initial_membership {
                        self.set_fatal(error);
                        return;
                    }
                    self.status_message = format!("Command rejected: {}", error.display_message());
                    self.error = Some(error);
                }
            }
        }
        if clock_not_ready {
            self.latest_clock_probe_ack = None;
            self.last_time_sync_us = None;
        }
        self.refresh_resume_request();
    }

    fn validate_command_ack_context(
        &self,
        ack: &CommandAck,
        was_pending: bool,
    ) -> std::result::Result<(), String> {
        if ack.seq.0 == 0 || ack.next_expected_seq.0 == 0 {
            return Err("Server sent a zero command acknowledgement sequence.".to_owned());
        }
        let highest_issued = self.next_command_seq.0.saturating_sub(1);
        if ack.seq.0 > highest_issued {
            return Err(format!(
                "Server acknowledged command {} although the client has only issued through {}.",
                ack.seq.0, highest_issued
            ));
        }
        if ack.next_expected_seq > self.next_command_seq {
            return Err(format!(
                "Server advanced its command watermark to {} beyond the client watermark {}.",
                ack.next_expected_seq.0, self.next_command_seq.0
            ));
        }
        if matches!(ack.outcome, CommandOutcome::Applied { .. })
            && ack.seq.0.checked_add(1).map(CommandSeq) != Some(ack.next_expected_seq)
        {
            return Err(format!(
                "Applied command {} carried inconsistent next-expected sequence {}.",
                ack.seq.0, ack.next_expected_seq.0
            ));
        }
        if was_pending && ack.next_expected_seq.0 > ack.seq.0.saturating_add(1) {
            return Err(format!(
                "Server skipped pending command {} with watermark {}.",
                ack.seq.0, ack.next_expected_seq.0
            ));
        }
        Ok(())
    }

    fn ingest_snapshot(&mut self, snapshot: RoomSnapshot) {
        if let Err(error) = snapshot.validate() {
            self.set_fatal(OnlineError::local(
                format!("invalid authoritative room snapshot: {error}"),
                false,
            ));
            return;
        }
        if let Some(current) = self.snapshot.as_ref() {
            if snapshot.revision < current.revision
                || (snapshot.revision == current.revision && !self.awaiting_resume_snapshot)
            {
                return;
            }
        }
        if let Some(membership) = &self.membership {
            if snapshot.room_code != membership.room_code {
                self.set_fatal(OnlineError::local(
                    "Received a snapshot for a different room.",
                    false,
                ));
                return;
            }
            let contains_local_membership = match &membership.actor_id {
                ActorId::Player(player_id) => snapshot
                    .players
                    .iter()
                    .any(|player| player.player_id == *player_id),
                ActorId::Spectator(spectator_id) => snapshot
                    .spectators
                    .iter()
                    .any(|spectator| spectator.spectator_id == *spectator_id),
            };
            if !contains_local_membership {
                self.set_fatal(OnlineError::local(
                    "Authoritative snapshot omitted the granted local membership.",
                    false,
                ));
                return;
            }
        }

        let playback_invalidation = self.snapshot.as_ref().and_then(|current| {
            authoritative_playback_invalidation(&current.stage, &snapshot.stage)
        });
        self.revision_hint = Some(
            self.revision_hint
                .map_or(snapshot.revision, |current| current.max(snapshot.revision)),
        );
        let new_match_id = stage_match_id(&snapshot.stage);
        if let Some(reason) = playback_invalidation {
            self.queue_playback_invalidation(reason);
        }
        if new_match_id != self.active_match_id {
            self.reset_match_epoch(new_match_id);
            if let (Some(_), Some(song)) = (new_match_id, stage_song(&snapshot.stage).cloned()) {
                self.pending_actions.push(DomainAction::SongChanged {
                    song: Box::new(song),
                });
            }
        }

        if let Some(local_player) = self.local_player_snapshot_from(&snapshot) {
            self.last_input_ack = local_player.last_acked_input_seq;
            if let Some(ack) = self.last_input_ack {
                self.pending_inputs.retain(|seq, _| *seq > ack);
                self.next_input_seq = InputSeq(self.next_input_seq.0.max(ack.0.saturating_add(1)));
            }
        }

        self.final_results.clear();
        if let RoomStage::Finished { results, .. } = &snapshot.stage {
            self.final_results.extend(
                results
                    .iter()
                    .cloned()
                    .map(|result| (result.player_id, result)),
            );
        }

        let next_phase = self.phase_for_snapshot(&snapshot);
        self.snapshot = Some(snapshot);
        self.awaiting_resume_snapshot = false;
        self.set_phase(next_phase);
        self.status_message = self.phase.label().to_owned();
        self.refresh_resume_request();
    }

    fn queue_playback_invalidation(&mut self, reason: OnlinePlaybackInvalidation) {
        self.pending_actions
            .push(DomainAction::PlaybackInvalidated(reason));
    }

    fn ingest_live_state(&mut self, live: LiveStateSnapshot) {
        if Some(live.match_id) != self.active_match_id {
            return;
        }
        let Some(manifest) = self.current_manifest() else {
            return;
        };
        if let Err(error) = live.validate_for(manifest) {
            self.set_fatal(OnlineError::local(
                format!("Server sent invalid live state: {error}"),
                false,
            ));
            return;
        }
        if self.last_live_epoch.is_some_and(|(match_id, state_seq)| {
            match_id == live.match_id && state_seq >= live.state_seq
        }) {
            return;
        }
        self.last_live_epoch = Some((live.match_id, live.state_seq));
        self.live_states.extend(
            live.players
                .into_iter()
                .map(|state| (state.player_id, state)),
        );
    }

    fn handle_input_ack(&mut self, ack: InputAck) {
        if Some(ack.match_id) != self.active_match_id {
            return;
        }
        if let Err(error) = ack.validate() {
            self.set_fatal(OnlineError::local(
                format!("Server sent an invalid input acknowledgement: {error}"),
                false,
            ));
            return;
        }
        let highest_sent = self.highest_sent_input.map_or(0, |seq| seq.0);
        if ack
            .highest_contiguous_seq
            .is_some_and(|highest| highest.0 > highest_sent)
            || ack.next_expected_seq.0 > highest_sent.saturating_add(1)
        {
            self.set_fatal(OnlineError::local(
                format!(
                    "Server acknowledged input beyond the client send watermark (sent through {}, acknowledged through {}).",
                    highest_sent,
                    ack.highest_contiguous_seq.map_or(0, |seq| seq.0)
                ),
                false,
            ));
            return;
        }
        let rejection_consumed_pending = matches!(&ack.outcome, InputOutcome::Rejected { .. })
            && ack
                .highest_contiguous_seq
                .is_some_and(|highest| self.pending_inputs.range(..=highest).next().is_some());
        if let Some(highest) = ack.highest_contiguous_seq {
            if self.last_input_ack.is_none_or(|current| highest > current) {
                self.last_input_ack = Some(highest);
            }
            self.pending_inputs.retain(|seq, _| *seq > highest);
        }
        self.next_input_seq = InputSeq(self.next_input_seq.0.max(ack.next_expected_seq.0));
        match ack.outcome {
            InputOutcome::Accepted => {}
            InputOutcome::Rejected { error } if !rejection_consumed_pending => {
                let error = OnlineError::protocol(error);
                if error.retryable {
                    self.status_message = format!("Input rejected: {}", error.display_message());
                    self.error = Some(error);
                } else {
                    self.set_fatal(error);
                }
            }
            InputOutcome::Rejected { error } => {
                self.status_message = format!(
                    "Server dropped an unscorable input and continued: {}",
                    error.message
                );
                self.error = None;
            }
        }
        self.refresh_resume_request();
    }

    fn handle_time_sync(&mut self, response: TimeSyncResponse, local_receive_us: u64) {
        let Some(client_send_us) = self.pending_time_sync.remove(&response.nonce) else {
            return;
        };
        if client_send_us != response.client_send_us {
            return;
        }

        let nonce = response.nonce;
        let receipt = TimeSyncReceipt {
            nonce,
            probe_token: response.probe_token,
        };
        if self
            .network
            .try_send(taiko_multiplayer_protocol::ClientMessage::TimeSyncReceipt(
                receipt,
            ))
            .is_ok()
        {
            while self.pending_clock_probe_acks.len() >= MAX_PENDING_CLOCK_PROBE_ACKS {
                let Some(oldest) = self.pending_clock_probe_acks.first().copied() else {
                    break;
                };
                self.pending_clock_probe_acks.remove(&oldest);
            }
            self.pending_clock_probe_acks.insert(nonce);
        }

        self.clock_sync.observe(
            client_send_us,
            response.server_receive_us,
            response.server_send_us,
            local_receive_us,
        );
    }

    fn handle_clock_probe_ack(&mut self, ack: ClockProbeAck) {
        if self.pending_clock_probe_acks.remove(&ack.nonce) {
            self.latest_clock_probe_ack = Some(ack);
        }
    }

    fn reset_clock_probe_state(&mut self) {
        self.pending_time_sync.clear();
        self.pending_clock_probe_acks.clear();
        self.latest_clock_probe_ack = None;
        self.last_time_sync_us = None;
        self.clock_sync = ClockSyncEstimator::default();
    }

    fn phase_for_snapshot(&self, snapshot: &RoomSnapshot) -> OnlinePhase {
        match &snapshot.stage {
            RoomStage::Lobby => OnlinePhase::Lobby,
            RoomStage::Preparing { .. } => {
                let Some(player) = self.local_player_snapshot_from(snapshot) else {
                    return OnlinePhase::Spectating;
                };
                match &player.preparation {
                    PlayerPreparation::Selecting | PlayerPreparation::Failed { .. } => {
                        OnlinePhase::SelectingCourse
                    }
                    PlayerPreparation::Downloading { .. } => OnlinePhase::Downloading,
                    PlayerPreparation::Verifying { .. } => OnlinePhase::Verifying,
                    PlayerPreparation::Loading { .. } => OnlinePhase::Loading,
                    PlayerPreparation::Prepared { .. } => OnlinePhase::Prepared,
                    PlayerPreparation::Ready { .. } => OnlinePhase::Ready,
                }
            }
            RoomStage::Countdown { .. } => OnlinePhase::Countdown,
            RoomStage::Playing { .. } => OnlinePhase::Playing,
            RoomStage::Finalizing { .. } => OnlinePhase::Finalizing,
            RoomStage::Finished { .. } => OnlinePhase::Results,
        }
    }

    fn reset_match_epoch(&mut self, next_match_id: Option<MatchId>) {
        self.active_match_id = next_match_id;
        self.live_states.clear();
        self.final_results.clear();
        self.last_live_epoch = None;
        self.pending_inputs.clear();
        self.next_input_seq = FIRST_INPUT_SEQ;
        self.last_submitted_input_tick = None;
        self.last_input_ack = None;
        self.highest_sent_input = None;
        self.last_input_flush_us = None;
        self.prepared_match = None;
        self.local_player = None;
        self.local_course_index = 0;
        self.wants_ready = false;
        self.refresh_resume_request();
    }

    fn set_phase(&mut self, phase: OnlinePhase) {
        if self.phase != phase {
            self.phase = phase;
            self.pending_actions.push(DomainAction::PhaseChanged(phase));
        }
    }

    fn set_fatal(&mut self, error: OnlineError) {
        self.connected = false;
        self.status_message = error.display_message();
        self.error = Some(error);
        self.set_phase(OnlinePhase::Failed);
        self.network.shutdown();
    }

    fn queue_command(
        &mut self,
        command: taiko_multiplayer_protocol::ClientCommand,
        now_us: u64,
    ) -> Result<CommandSeq> {
        if let Some((seq, _)) = self
            .pending_commands
            .iter()
            .find(|(_, pending)| pending.envelope.command == command)
        {
            return Ok(*seq);
        }
        if let Some((seq, pending)) = self.pending_commands.iter_mut().rev().find(|(_, pending)| {
            !pending.sent && commands_share_replaceable_intent(&pending.envelope.command, &command)
        }) {
            pending.envelope.command = command;
            pending.envelope.expected_room_revision = None;
            return Ok(*seq);
        }
        if self.pending_commands.len() >= MAX_PENDING_COMMANDS {
            bail!("pending command capacity reached; wait for the server acknowledgement");
        }
        let seq = self.next_command_seq;
        self.next_command_seq = CommandSeq(
            self.next_command_seq
                .0
                .checked_add(1)
                .ok_or_else(|| anyhow!("command sequence exhausted"))?,
        );
        let envelope = CommandEnvelope {
            seq,
            expected_room_revision: None,
            command,
        };
        self.pending_commands.insert(
            seq,
            PendingCommand {
                envelope,
                sent: false,
            },
        );
        self.last_command_flush_us = None;
        if self.pending_commands.len() == 1 {
            self.flush_commands(now_us);
        }
        Ok(seq)
    }

    fn flush_commands(&mut self, now_us: u64) {
        let expected_revision = self.expected_revision();
        let Some(pending) = self.pending_commands.values_mut().next() else {
            return;
        };
        if !pending.sent {
            pending.envelope.expected_room_revision =
                if command_requires_global_revision(&pending.envelope.command) {
                    expected_revision
                } else {
                    None
                };
        }
        match self
            .network
            .try_send(taiko_multiplayer_protocol::ClientMessage::Command(
                pending.envelope.clone(),
            )) {
            Ok(()) => pending.sent = true,
            Err(error) => {
                self.status_message = format!("Command queued while transport is busy: {error}");
            }
        }
        self.last_command_flush_us = Some(now_us);
    }

    fn finalize_graceful_command_revisions(&mut self) -> Result<()> {
        let expected_revision = self.expected_revision();
        for pending in self.pending_commands.values_mut() {
            if !command_requires_global_revision(&pending.envelope.command) {
                if !pending.sent {
                    pending.envelope.expected_room_revision = None;
                }
                continue;
            }
            if pending.sent {
                if pending.envelope.expected_room_revision.is_none() {
                    bail!(
                        "cannot retry a revision-sensitive command without its original room revision"
                    );
                }
                continue;
            }
            pending.envelope.expected_room_revision =
                Some(expected_revision.ok_or_else(|| {
                    anyhow!(
                        "cannot flush a revision-sensitive command without an authoritative room revision"
                    )
                })?);
        }
        Ok(())
    }

    fn flush_inputs(&mut self, now_us: u64) {
        let Some(match_id) = self.active_match_id else {
            return;
        };
        let events = self.pending_inputs.values().copied().collect::<Vec<_>>();
        for chunk in events.chunks(MAX_INPUT_BATCH_EVENTS) {
            let events = BoundedVec::new(chunk.to_vec()).expect("chunk respects protocol bound");
            let last_seq = chunk.last().map(|event| event.seq);
            let batch = InputBatch { match_id, events };
            if self
                .network
                .try_send(taiko_multiplayer_protocol::ClientMessage::Input(batch))
                .is_err()
            {
                break;
            }
            if last_seq > self.highest_sent_input {
                self.highest_sent_input = last_seq;
            }
        }
        self.last_input_flush_us = Some(now_us);
    }

    fn flush_new_inputs(&mut self, now_us: u64) {
        let Some(match_id) = self.active_match_id else {
            return;
        };
        let events = self
            .pending_inputs
            .iter()
            .filter(|(seq, _)| self.highest_sent_input.is_none_or(|sent| **seq > sent))
            .map(|(_, event)| *event)
            .collect::<Vec<_>>();
        for chunk in events.chunks(MAX_INPUT_BATCH_EVENTS) {
            let bounded = BoundedVec::new(chunk.to_vec()).expect("chunk respects protocol bound");
            let last_seq = chunk.last().map(|event| event.seq);
            if self
                .network
                .try_send(taiko_multiplayer_protocol::ClientMessage::Input(
                    InputBatch {
                        match_id,
                        events: bounded,
                    },
                ))
                .is_err()
            {
                break;
            }
            self.highest_sent_input = last_seq;
        }
        self.last_input_flush_us = Some(now_us);
    }

    fn send_time_sync(&mut self, now_us: u64) {
        if self.pending_time_sync.len() >= MAX_PENDING_TIME_SYNC {
            return;
        }
        let nonce = self.take_nonce();
        let request = TimeSyncRequest {
            nonce,
            client_send_us: now_us,
        };
        if self
            .network
            .try_send(taiko_multiplayer_protocol::ClientMessage::TimeSync(request))
            .is_ok()
        {
            self.pending_time_sync.insert(nonce, now_us);
            self.last_time_sync_us = Some(now_us);
        }
    }

    fn send_heartbeat(&mut self, now_us: u64) {
        let heartbeat = Heartbeat {
            nonce: self.take_nonce(),
        };
        if self
            .network
            .try_send(taiko_multiplayer_protocol::ClientMessage::Heartbeat(
                heartbeat,
            ))
            .is_ok()
        {
            self.last_heartbeat_us = Some(now_us);
        }
    }

    fn take_nonce(&mut self) -> u64 {
        let nonce = self.next_nonce;
        self.next_nonce = self.next_nonce.wrapping_add(1);
        nonce
    }

    fn refresh_resume_request(&self) {
        let Some(membership) = &self.membership else {
            return;
        };
        let last_room_revision = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.revision)
            .unwrap_or(RoomRevision(0));
        self.network
            .set_resume(taiko_multiplayer_protocol::ResumeRequest {
                room_code: membership.room_code.clone(),
                actor_id: membership.actor_id.clone(),
                token: membership.resume_token.clone(),
                last_room_revision,
                last_acked_command_seq: self.last_acked_command_seq,
            });
    }

    fn expected_revision(&self) -> Option<RoomRevision> {
        match (
            self.snapshot.as_ref().map(|snapshot| snapshot.revision),
            self.revision_hint,
        ) {
            (Some(snapshot), Some(hint)) => Some(snapshot.max(hint)),
            (Some(snapshot), None) => Some(snapshot),
            (None, Some(hint)) => Some(hint),
            (None, None) => None,
        }
    }

    fn local_now_us(&self) -> u64 {
        self.clock_origin
            .elapsed()
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }

    fn local_player_snapshot_from<'a>(
        &self,
        snapshot: &'a RoomSnapshot,
    ) -> Option<&'a taiko_multiplayer_protocol::PlayerSnapshot> {
        let player_id = match &self.membership.as_ref()?.actor_id {
            ActorId::Player(player_id) => *player_id,
            ActorId::Spectator(_) => return None,
        };
        snapshot
            .players
            .iter()
            .find(|player| player.player_id == player_id)
    }

    pub(crate) fn select_song(&mut self, song_id: &str) -> Result<CommandSeq> {
        if !self.is_local_leader() {
            bail!("only the room leader can select a song");
        }
        let song_id = SongId::parse(song_id).context("invalid multiplayer song id")?;
        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::SelectSong { song_id },
            self.local_now_us(),
        )
    }

    pub(crate) fn select_course(&mut self, selection: PlayerSelection) -> Result<CommandSeq> {
        if self.role() != Some(RoomRole::Player) {
            bail!("spectators cannot select a course");
        }
        let match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("no match is being prepared"))?;
        let song = self
            .current_song()
            .ok_or_else(|| anyhow!("the match has no song manifest"))?;
        if !song
            .courses
            .iter()
            .any(|course| course.course_id == selection.course_id)
        {
            bail!("selected course is not present in the authoritative manifest");
        }
        let seq = self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::SelectCourse {
                match_id,
                selection,
            },
            self.local_now_us(),
        )?;
        self.wants_ready = true;
        Ok(seq)
    }

    pub(crate) fn report_preparation(
        &mut self,
        progress: PreparationProgress,
    ) -> Result<CommandSeq> {
        let match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("no match is being prepared"))?;
        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::ReportPreparation { match_id, progress },
            self.local_now_us(),
        )
    }

    pub(crate) fn set_ready(
        &mut self,
        ready: bool,
        proof: Option<PreparationProof>,
    ) -> Result<CommandSeq> {
        let match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("no match is being prepared"))?;
        if ready && proof.is_none() {
            bail!("ready=true requires a verified preparation proof");
        }
        if !ready && proof.is_some() {
            bail!("ready=false cannot include a preparation proof");
        }
        let seq = self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::SetReady {
                match_id,
                ready,
                proof,
            },
            self.local_now_us(),
        )?;
        self.wants_ready = ready;
        Ok(seq)
    }

    pub(crate) fn start_match(&mut self) -> Result<CommandSeq> {
        if !self.is_local_leader() {
            bail!("only the room leader can start the match");
        }
        let match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("no match is ready to start"))?;
        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::StartMatch { match_id },
            self.local_now_us(),
        )
    }

    pub(crate) fn rematch(&mut self) -> Result<CommandSeq> {
        if !self.is_local_leader() {
            bail!("only the room leader can start a rematch");
        }
        let previous_match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("there is no previous match"))?;
        if !matches!(
            self.snapshot.as_ref().map(|snapshot| &snapshot.stage),
            Some(RoomStage::Finished { .. })
        ) {
            bail!("rematch is only available after authoritative results");
        }
        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::Rematch { previous_match_id },
            self.local_now_us(),
        )
    }

    pub(crate) fn return_to_lobby(&mut self) -> Result<CommandSeq> {
        if !self.is_local_leader() {
            bail!("only the room leader can return to the lobby");
        }
        let match_id = self
            .current_match_id()
            .ok_or_else(|| anyhow!("there is no active match"))?;
        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::ReturnToLobby { match_id },
            self.local_now_us(),
        )
    }

    pub(crate) fn shutdown_gracefully(&mut self) -> Result<()> {
        if self.membership.is_none() {
            self.network.shutdown();
            self.connected = false;
            return Ok(());
        }
        if !self.connected {
            self.network.shutdown();
            self.connected = false;
            return Err(crate::online::GracefulShutdownError::NotConnected {
                phase: "disconnected with active room membership",
            }
            .into());
        }

        self.queue_command(
            taiko_multiplayer_protocol::ClientCommand::LeaveRoom,
            self.local_now_us(),
        )?;
        self.finalize_graceful_command_revisions()?;
        let messages = self
            .pending_commands
            .values()
            .map(|pending| {
                taiko_multiplayer_protocol::ClientMessage::Command(pending.envelope.clone())
            })
            .collect();
        let result = self.network.shutdown_gracefully(messages);
        self.connected = false;
        result
    }

    pub(crate) fn submit_input(
        &mut self,
        tick: taiko_multiplayer_protocol::Tick,
        action: DrumAction,
    ) -> Result<Option<SubmittedInput>> {
        if self.role() != Some(RoomRole::Player) {
            bail!("spectators cannot submit gameplay input");
        }
        if self.phase != OnlinePhase::Playing {
            bail!("gameplay input is only accepted during Playing");
        }
        if self.pending_inputs.len() >= MAX_PENDING_INPUTS {
            self.status_message = format!(
                "Input locally dropped: {} acknowledgements are still pending.",
                self.pending_inputs.len()
            );
            return Ok(None);
        }
        let seq = self.next_input_seq;
        self.next_input_seq = InputSeq(
            self.next_input_seq
                .0
                .checked_add(1)
                .ok_or_else(|| anyhow!("input sequence exhausted"))?,
        );
        let effective_tick = self
            .last_submitted_input_tick
            .map_or(tick, |previous| tick.max(previous));
        self.last_submitted_input_tick = Some(effective_tick);
        self.pending_inputs.insert(
            seq,
            InputEvent {
                seq,
                tick: effective_tick,
                action,
            },
        );
        self.flush_new_inputs(self.local_now_us());
        Ok(Some(SubmittedInput {
            seq,
            tick: effective_tick,
        }))
    }

    pub(crate) fn preparation_proof(&self, prepared: &PreparedMatch) -> Result<PreparationProof> {
        if !self.clock_is_ready() {
            let server_status = self
                .latest_clock_probe_ack
                .as_ref()
                .map_or(
                    "pending",
                    |ack| if ack.ready { "ready" } else { "not ready" },
                );
            bail!(
                "clock synchronization is not ready (local {}/{CLOCK_MIN_READY_SAMPLES} samples, server {server_status})",
                self.clock_sync.quality().accepted_samples,
            );
        }
        let song = self
            .current_song()
            .ok_or_else(|| anyhow!("missing authoritative song manifest"))?;
        if self.current_match_id() != Some(prepared.match_id) {
            bail!("prepared chart belongs to a stale match epoch");
        }
        let course = song
            .courses
            .iter()
            .find(|course| course.course_id == prepared.selection.course_id)
            .ok_or_else(|| anyhow!("prepared course is absent from the manifest"))?;
        Ok(PreparationProof {
            source_id: song.source_id.clone(),
            canonical_chart_hash: course.canonical_chart_hash.clone(),
            audio_id: song.audio_id.clone(),
            semantics: song.semantics.clone(),
        })
    }

    pub(crate) fn clock_is_ready(&self) -> bool {
        self.clock_sync.is_ready()
            && self.latest_clock_probe_ack.as_ref().is_some_and(|ack| {
                ack.ready && self.estimated_server_now_us() <= ack.valid_until_server_us
            })
    }

    pub(crate) fn phase(&self) -> OnlinePhase {
        self.phase
    }

    pub(crate) fn status_message(&self) -> &str {
        &self.status_message
    }

    #[cfg(test)]
    pub(crate) fn clock_status_line(&self) -> String {
        let local = self.clock_sync.quality();
        let server = self.latest_clock_probe_ack.as_ref();
        let readiness = if self.clock_is_ready() {
            "ready"
        } else {
            "checking"
        };
        match server {
            Some(ack) => {
                let lease_ms = ack
                    .valid_until_server_us
                    .saturating_sub(self.estimated_server_now_us())
                    .div_ceil(1_000);
                format!(
                    "{readiness}: local {} samples, p95 {} ms, jitter {} ms; server {} samples, p95 {} ms, jitter {} ms; lease {} ms",
                    local.accepted_samples,
                    local.p95_rtt_ms,
                    local.jitter_ms,
                    ack.quality.accepted_samples,
                    ack.quality.p95_rtt_ms,
                    ack.quality.jitter_ms,
                    lease_ms,
                )
            }
            None => format!(
                "{readiness}: local {} samples, p95 {} ms, jitter {} ms; awaiting server verification",
                local.accepted_samples, local.p95_rtt_ms, local.jitter_ms,
            ),
        }
    }

    pub(crate) fn error(&self) -> Option<&OnlineError> {
        self.error.as_ref()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.phase == OnlinePhase::Failed
    }

    pub(crate) fn room_code(&self) -> Option<&taiko_multiplayer_protocol::RoomCode> {
        self.membership
            .as_ref()
            .map(|membership| &membership.room_code)
    }

    pub(crate) fn invite(&self) -> Option<crate::invite::MultiplayerInvite> {
        let membership = self.membership.as_ref()?;
        Some(
            crate::invite::MultiplayerInvite::new(
                self.config.server_url.clone(),
                membership.room_code.clone(),
                membership.invitation_token.clone(),
            )
            .expect("online config stores a normalized server URL"),
        )
    }

    pub(crate) fn actor_id(&self) -> Option<&ActorId> {
        self.membership
            .as_ref()
            .map(|membership| &membership.actor_id)
    }

    #[cfg(test)]
    pub(crate) fn pending_command_count(&self) -> usize {
        self.pending_commands.len()
    }

    #[cfg(test)]
    pub(crate) fn pending_input_count(&self) -> usize {
        self.pending_inputs.len()
    }

    pub(crate) fn local_player_id(&self) -> Option<PlayerId> {
        match self.actor_id()? {
            ActorId::Player(player_id) => Some(*player_id),
            ActorId::Spectator(_) => None,
        }
    }

    pub(crate) fn role(&self) -> Option<RoomRole> {
        self.actor_id().map(|actor_id| match actor_id {
            ActorId::Player(_) => RoomRole::Player,
            ActorId::Spectator(_) => RoomRole::Spectator,
        })
    }

    pub(crate) fn is_local_leader(&self) -> bool {
        let Some(player_id) = self.local_player_id() else {
            return false;
        };
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.leader_player_id == player_id)
    }

    pub(crate) fn room_controls_enabled(&self) -> bool {
        self.connected && !matches!(self.phase, OnlinePhase::Reconnecting | OnlinePhase::Failed)
    }

    pub(crate) fn can_start_match(&self) -> bool {
        self.room_controls_enabled()
            && self.is_local_leader()
            && self.clock_is_ready()
            && self.snapshot.as_ref().is_some_and(|snapshot| {
                matches!(&snapshot.stage, RoomStage::Preparing { .. })
                    && snapshot.players.len() >= taiko_multiplayer_protocol::MIN_MATCH_PLAYERS
                    && snapshot.players.iter().all(|player| {
                        matches!(&player.connection, PlayerConnection::Online)
                            && player.preparation.is_ready()
                    })
            })
    }

    pub(crate) fn is_local_ready(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| self.local_player_snapshot_from(snapshot))
            .is_some_and(|player| matches!(&player.preparation, PlayerPreparation::Ready { .. }))
    }

    pub(crate) fn is_ready_command_pending(&self) -> bool {
        let match_id = self.current_match_id();
        self.pending_commands.values().any(|pending| {
            matches!(
                &pending.envelope.command,
                taiko_multiplayer_protocol::ClientCommand::SetReady {
                    match_id: pending_match_id,
                    ready: true,
                    ..
                } if Some(*pending_match_id) == match_id
            )
        })
    }

    pub(crate) fn should_auto_ready(&self) -> bool {
        self.wants_ready
            && !self.is_local_ready()
            && !self.is_ready_command_pending()
            && self.clock_is_ready()
    }

    pub(crate) fn local_selection(&self) -> Option<PlayerSelection> {
        if let Some(manifest) = self.current_manifest() {
            let player_id = self.local_player_id()?;
            if let Some(assignment) = manifest
                .assignments
                .iter()
                .find(|assignment| assignment.player_id == player_id)
            {
                return Some(assignment.selection);
            }
        }
        let player = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| self.local_player_snapshot_from(snapshot))?;
        preparation_selection(&player.preparation)
    }

    pub(crate) fn current_match_id(&self) -> Option<MatchId> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| stage_match_id(&snapshot.stage))
            .or(self.active_match_id)
    }

    pub(crate) fn current_song(&self) -> Option<&SongManifest> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| stage_song(&snapshot.stage))
    }

    pub(crate) fn current_manifest(&self) -> Option<&MatchManifest> {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| stage_manifest(&snapshot.stage))
    }

    pub(crate) fn estimated_server_now_us(&self) -> u64 {
        self.clock_sync.estimated_server_now_us(self.local_now_us())
    }

    pub(crate) fn estimated_server_tick(&self) -> taiko_multiplayer_protocol::Tick {
        self.estimated_server_tick_at(Instant::now())
    }

    pub(crate) fn estimated_server_tick_at(
        &self,
        observed_at: Instant,
    ) -> taiko_multiplayer_protocol::Tick {
        let observed_local_us = observed_at
            .saturating_duration_since(self.clock_origin)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64;
        let observed_server_us = self.clock_sync.estimated_server_now_us(observed_local_us);
        let Some(snapshot) = self.snapshot.as_ref() else {
            return 0;
        };
        match &snapshot.stage {
            RoomStage::Countdown {
                start_at_server_us, ..
            } => observed_server_us.saturating_sub(*start_at_server_us) as i64,
            RoomStage::Playing {
                start_at_server_us,
                server_tick,
                ..
            } => (*server_tick).max(observed_server_us.saturating_sub(*start_at_server_us) as i64),
            RoomStage::Finalizing { server_tick, .. } => *server_tick,
            RoomStage::Finished { results, .. } => results
                .iter()
                .map(|result| result.finish_tick)
                .max()
                .unwrap_or_default(),
            RoomStage::Lobby | RoomStage::Preparing { .. } => 0,
        }
    }

    pub(crate) fn countdown_remaining(&self) -> Option<Duration> {
        let snapshot = self.snapshot.as_ref()?;
        let RoomStage::Countdown {
            start_at_server_us, ..
        } = &snapshot.stage
        else {
            return None;
        };
        Some(Duration::from_micros(
            start_at_server_us.saturating_sub(self.estimated_server_now_us()),
        ))
    }

    #[cfg(test)]
    pub(crate) fn latest_live_epoch(&self) -> Option<(MatchId, StateSeq)> {
        self.last_live_epoch
    }

    #[cfg(test)]
    pub(crate) fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("Phase: {}", self.phase.label()),
            format!("Status: {}", self.status_message),
        ];
        if let Some(room_code) = self.room_code() {
            lines.push(format!("Room: {room_code}"));
        }
        if let Some(invite) = self.invite() {
            lines.push(format!("Invite: {invite}"));
        }
        if let Some(snapshot) = &self.snapshot {
            lines.push(format!(
                "Players: {}/{}  Spectators: {}",
                snapshot.players.len(),
                taiko_multiplayer_protocol::MAX_PLAYERS,
                snapshot.spectators.len()
            ));
        }
        lines.push("Esc / Ctrl-C: disconnect".to_owned());
        lines
    }
}

fn is_membership_command(envelope: &CommandEnvelope) -> bool {
    is_membership_command_kind(&envelope.command)
}

fn command_requires_global_revision(command: &taiko_multiplayer_protocol::ClientCommand) -> bool {
    use taiko_multiplayer_protocol::ClientCommand;
    match command {
        ClientCommand::SelectSong { .. }
        | ClientCommand::StartMatch { .. }
        | ClientCommand::Rematch { .. }
        | ClientCommand::ReturnToLobby { .. } => true,
        ClientCommand::CreateRoom
        | ClientCommand::JoinRoom { .. }
        | ClientCommand::LeaveRoom
        | ClientCommand::SelectCourse { .. }
        | ClientCommand::ReportPreparation { .. }
        | ClientCommand::SetReady { .. } => false,
    }
}

fn is_membership_command_kind(command: &taiko_multiplayer_protocol::ClientCommand) -> bool {
    matches!(
        command,
        taiko_multiplayer_protocol::ClientCommand::CreateRoom
            | taiko_multiplayer_protocol::ClientCommand::JoinRoom { .. }
            | taiko_multiplayer_protocol::ClientCommand::LeaveRoom
    )
}

fn is_membership_establishment_command_kind(
    command: &taiko_multiplayer_protocol::ClientCommand,
) -> bool {
    matches!(
        command,
        taiko_multiplayer_protocol::ClientCommand::CreateRoom
            | taiko_multiplayer_protocol::ClientCommand::JoinRoom { .. }
    )
}

fn commands_share_replaceable_intent(
    existing: &taiko_multiplayer_protocol::ClientCommand,
    replacement: &taiko_multiplayer_protocol::ClientCommand,
) -> bool {
    use taiko_multiplayer_protocol::ClientCommand;
    match (existing, replacement) {
        (ClientCommand::SelectSong { .. }, ClientCommand::SelectSong { .. }) => true,
        (
            ClientCommand::SelectCourse {
                match_id: existing, ..
            },
            ClientCommand::SelectCourse {
                match_id: replacement,
                ..
            },
        )
        | (
            ClientCommand::SetReady {
                match_id: existing, ..
            },
            ClientCommand::SetReady {
                match_id: replacement,
                ..
            },
        ) => existing == replacement,
        _ => false,
    }
}

fn stage_match_id(stage: &RoomStage) -> Option<MatchId> {
    match stage {
        RoomStage::Lobby => None,
        RoomStage::Preparing { match_id, .. } => Some(*match_id),
        RoomStage::Countdown { manifest, .. }
        | RoomStage::Playing { manifest, .. }
        | RoomStage::Finalizing { manifest, .. }
        | RoomStage::Finished { manifest, .. } => Some(manifest.match_id),
    }
}

fn authoritative_playback_invalidation(
    previous: &RoomStage,
    next: &RoomStage,
) -> Option<OnlinePlaybackInvalidation> {
    let previous_match_id = stage_match_id(previous);
    let next_match_id = stage_match_id(next);
    if previous_match_id.is_some() && previous_match_id != next_match_id {
        return Some(OnlinePlaybackInvalidation::MatchEpochChanged);
    }

    let scheduled_start = |stage: &RoomStage| match stage {
        RoomStage::Countdown {
            start_at_server_us, ..
        }
        | RoomStage::Playing {
            start_at_server_us, ..
        } => Some(*start_at_server_us),
        RoomStage::Lobby
        | RoomStage::Preparing { .. }
        | RoomStage::Finalizing { .. }
        | RoomStage::Finished { .. } => None,
    };
    if let (Some(previous_start), Some(next_start)) =
        (scheduled_start(previous), scheduled_start(next))
    {
        if previous_start != next_start {
            return Some(OnlinePlaybackInvalidation::ScheduledStartChanged);
        }
    }

    if matches!(previous, RoomStage::Countdown { .. })
        && !matches!(
            next,
            RoomStage::Countdown { .. } | RoomStage::Playing { .. }
        )
    {
        return Some(OnlinePlaybackInvalidation::CountdownAborted);
    }
    None
}

fn stage_song(stage: &RoomStage) -> Option<&SongManifest> {
    match stage {
        RoomStage::Lobby => None,
        RoomStage::Preparing { song, .. } => Some(song),
        RoomStage::Countdown { manifest, .. }
        | RoomStage::Playing { manifest, .. }
        | RoomStage::Finalizing { manifest, .. }
        | RoomStage::Finished { manifest, .. } => Some(&manifest.song),
    }
}

fn stage_manifest(stage: &RoomStage) -> Option<&MatchManifest> {
    match stage {
        RoomStage::Countdown { manifest, .. }
        | RoomStage::Playing { manifest, .. }
        | RoomStage::Finalizing { manifest, .. }
        | RoomStage::Finished { manifest, .. } => Some(manifest),
        RoomStage::Lobby | RoomStage::Preparing { .. } => None,
    }
}

fn preparation_selection(preparation: &PlayerPreparation) -> Option<PlayerSelection> {
    match preparation {
        PlayerPreparation::Selecting => None,
        PlayerPreparation::Downloading { selection, .. }
        | PlayerPreparation::Verifying { selection }
        | PlayerPreparation::Loading { selection }
        | PlayerPreparation::Prepared { selection }
        | PlayerPreparation::Ready { selection } => Some(*selection),
        PlayerPreparation::Failed { selection, .. } => *selection,
    }
}

fn micros_to_millis_ceil(value: u64) -> u32 {
    value
        .saturating_add(999)
        .saturating_div(1_000)
        .min(u64::from(u32::MAX)) as u32
}

fn median_u64(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn median_i64(values: impl IntoIterator<Item = i64>) -> Option<i64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn percentile_u64(values: impl IntoIterator<Item = u64>, percentile: usize) -> Option<u64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let index = (values.len() - 1).saturating_mul(percentile).div_ceil(100);
    values.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::online::{NetworkEvent, OnlineClientConfig, TestNetworkPeer};
    use taiko_multiplayer_protocol::{
        BoundedText, ClientMessage, ClockProbeToken, CourseId, CourseManifest, CourseName,
        DisplayTitle, InvitationToken, JoinRole, MatchSemantics, PlayerConnection,
        PlayerCourseAssignment, PlayerSnapshot, ProgressMilli, ResumeToken, RoomCode,
        ScoreSnapshot, SpectatorId, SpectatorSnapshot,
    };

    fn hash(character: char) -> ContentHash {
        ContentHash::parse(character.to_string().repeat(64)).expect("valid content hash")
    }

    fn song() -> SongManifest {
        let mut song = SongManifest {
            song_id: SongId::parse("9".repeat(64)).expect("song id"),
            source_id: hash('a'),
            audio_id: Some(hash('b')),
            title: DisplayTitle::new("Test Song").expect("title"),
            subtitle: BoundedText::new("").expect("subtitle"),
            artist: BoundedText::new("Tester").expect("artist"),
            semantics: MatchSemantics {
                canonical_schema_version: 1,
                canonical_schema_digest: hash('c'),
                importer_semantics_version: 1,
                importer_semantics_digest: hash('d'),
                ruleset_version: 1,
                ruleset_digest: hash('e'),
                audio_decoder_semantics_version: 1,
                audio_decoder_semantics_digest: hash('6'),
            },
            courses: BoundedVec::new(vec![CourseManifest {
                course_id: CourseId(0),
                name: CourseName::new("Oni").expect("course name"),
                level: Some(8),
                canonical_chart_hash: hash('f'),
            }])
            .expect("bounded courses"),
        };
        song.song_id = song.derive_song_id().expect("derived fixture song id");
        song
    }

    fn selection() -> PlayerSelection {
        PlayerSelection {
            course_id: CourseId(0),
        }
    }

    fn manifest() -> MatchManifest {
        MatchManifest {
            match_id: MatchId(7),
            song: song(),
            assignments: BoundedVec::new(vec![
                PlayerCourseAssignment {
                    player_id: PlayerId(1),
                    selection: selection(),
                    canonical_chart_hash: hash('f'),
                },
                PlayerCourseAssignment {
                    player_id: PlayerId(2),
                    selection: selection(),
                    canonical_chart_hash: hash('f'),
                },
            ])
            .expect("assignments"),
            countdown_ms: 500,
            input_lateness_ms: 120,
        }
    }

    fn active_snapshot(revision: u64, stage: RoomStage) -> RoomSnapshot {
        let mut snapshot = snapshot(
            revision,
            stage,
            PlayerPreparation::Ready {
                selection: selection(),
            },
        );
        snapshot.players = BoundedVec::new(vec![
            player(PlayerPreparation::Ready {
                selection: selection(),
            }),
            PlayerSnapshot {
                player_id: PlayerId(2),
                name: taiko_multiplayer_protocol::DisplayName::new("bob").expect("name"),
                is_leader: false,
                connection: PlayerConnection::Online,
                preparation: PlayerPreparation::Ready {
                    selection: selection(),
                },
                last_acked_input_seq: None,
            },
        ])
        .expect("active players");
        snapshot
    }

    fn player(preparation: PlayerPreparation) -> PlayerSnapshot {
        PlayerSnapshot {
            player_id: PlayerId(1),
            name: taiko_multiplayer_protocol::DisplayName::new("alice").expect("name"),
            is_leader: true,
            connection: PlayerConnection::Online,
            preparation,
            last_acked_input_seq: None,
        }
    }

    fn snapshot(revision: u64, stage: RoomStage, preparation: PlayerPreparation) -> RoomSnapshot {
        RoomSnapshot {
            room_code: RoomCode::parse("ABCD").expect("room code"),
            revision: RoomRevision(revision),
            server_now_us: 10_000,
            leader_player_id: PlayerId(1),
            players: BoundedVec::new(vec![player(preparation)]).expect("players"),
            spectators: BoundedVec::<
                SpectatorSnapshot,
                { taiko_multiplayer_protocol::MAX_SPECTATORS },
            >::default(),
            stage,
        }
    }

    fn welcome(resumed: bool, next_expected_command_seq: u64) -> ServerMessage {
        ServerMessage::Welcome(taiko_multiplayer_protocol::ServerWelcome {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("schema hash"),
            heartbeat_interval_ms: 1_000,
            reconnect_grace_ms: 10_000,
            resumed,
            next_expected_command_seq: CommandSeq(next_expected_command_seq),
        })
    }

    fn membership() -> ServerMessage {
        membership_for("ABCD", ActorId::Player(PlayerId(1)))
    }

    fn membership_for(room_code: &str, actor_id: ActorId) -> ServerMessage {
        ServerMessage::MembershipGranted(MembershipGranted {
            room_code: RoomCode::parse(room_code).expect("room code"),
            actor_id,
            resume_token: ResumeToken::parse("a".repeat(64)).expect("resume token"),
            invitation_token: InvitationToken::parse("b".repeat(64)).expect("invitation token"),
        })
    }

    fn domain() -> (OnlineDomain, TestNetworkPeer) {
        let config =
            OnlineClientConfig::create("https://example.test", "alice").expect("client config");
        let (network, peer) = NetworkClient::test_pair();
        (OnlineDomain::with_network(config, network), peer)
    }

    #[test]
    fn authoritative_playback_transition_invalidates_aborts_and_rescheduled_deadlines() {
        let old_countdown = RoomStage::Countdown {
            manifest: manifest(),
            start_at_server_us: 10_000,
        };
        let preparing_same_epoch = RoomStage::Preparing {
            match_id: MatchId(7),
            song: song(),
        };
        assert_eq!(
            authoritative_playback_invalidation(&old_countdown, &preparing_same_epoch),
            Some(OnlinePlaybackInvalidation::CountdownAborted)
        );

        // A reconnect can miss the intermediate Preparing snapshot. The changed
        // authoritative deadline is therefore part of playback continuity, not
        // merely a presentation update.
        let rescheduled_countdown = RoomStage::Countdown {
            manifest: manifest(),
            start_at_server_us: 20_000,
        };
        assert_eq!(
            authoritative_playback_invalidation(&old_countdown, &rescheduled_countdown),
            Some(OnlinePlaybackInvalidation::ScheduledStartChanged)
        );

        let naturally_started = RoomStage::Playing {
            manifest: manifest(),
            start_at_server_us: 10_000,
            server_tick: 0,
        };
        assert_eq!(
            authoritative_playback_invalidation(&old_countdown, &naturally_started),
            None
        );
        assert_eq!(
            authoritative_playback_invalidation(&old_countdown, &RoomStage::Lobby),
            Some(OnlinePlaybackInvalidation::MatchEpochChanged)
        );
    }

    #[test]
    fn same_epoch_countdown_abort_emits_a_typed_playback_invalidation() {
        let (mut domain, _peer) = domain();
        domain.ingest_snapshot(active_snapshot(
            1,
            RoomStage::Countdown {
                manifest: manifest(),
                start_at_server_us: 10_000,
            },
        ));
        domain.pending_actions.clear();

        domain.ingest_snapshot(active_snapshot(
            2,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
        ));

        assert!(domain.pending_actions.iter().any(|action| {
            matches!(
                action,
                DomainAction::PlaybackInvalidated(OnlinePlaybackInvalidation::CountdownAborted)
            )
        }));
    }

    fn establish(domain: &mut OnlineDomain, peer: &mut TestNetworkPeer) {
        peer.send_server(welcome(false, 1));
        domain.tick_at(100).expect("welcome");
        assert!(matches!(
            peer.try_recv_message(),
            Some(ClientMessage::Command(CommandEnvelope {
                seq: CommandSeq(1),
                command: taiko_multiplayer_protocol::ClientCommand::CreateRoom,
                ..
            }))
        ));
        while peer.try_recv_message().is_some() {}
        peer.send_server(membership());
        domain.tick_at(200).expect("membership");
    }

    fn make_local_clock_ready(domain: &mut OnlineDomain) {
        for index in 0..CLOCK_MIN_READY_SAMPLES {
            let client_send_us = index.saturating_mul(100_000);
            assert!(domain.clock_sync.observe(
                client_send_us,
                client_send_us.saturating_add(11_000),
                client_send_us.saturating_add(12_000),
                client_send_us.saturating_add(21_000),
            ));
        }
        assert!(domain.clock_sync.is_ready());
    }

    fn acknowledge_server_clock(domain: &mut OnlineDomain, nonce: u64, ready: bool) {
        domain.pending_clock_probe_acks.insert(nonce);
        domain.handle_clock_probe_ack(ClockProbeAck {
            nonce,
            quality: ClockQuality {
                accepted_samples: if ready {
                    CLOCK_MIN_READY_SAMPLES as u16
                } else {
                    1
                },
                p95_rtt_ms: 20,
                jitter_ms: 2,
            },
            ready,
            valid_until_server_us: u64::MAX,
        });
    }

    #[test]
    fn graceful_shutdown_sends_ordered_leave_after_pending_commands() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);

        domain.shutdown_gracefully().expect("graceful shutdown");
        let messages = peer
            .take_graceful_shutdown_messages()
            .expect("graceful control payload");
        let commands = messages
            .into_iter()
            .map(|message| match message {
                ClientMessage::Command(command) => command,
                other => panic!("unexpected graceful message: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].seq, CommandSeq(1));
        assert!(matches!(
            commands[0].command,
            taiko_multiplayer_protocol::ClientCommand::CreateRoom
        ));
        assert_eq!(commands[1].seq, CommandSeq(2));
        assert!(matches!(
            commands[1].command,
            taiko_multiplayer_protocol::ClientCommand::LeaveRoom
        ));
    }

    #[test]
    fn graceful_shutdown_binds_every_unsent_global_command_to_a_known_revision() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::CommandAck(CommandAck {
            seq: CommandSeq(1),
            next_expected_seq: CommandSeq(2),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        }));
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(300).expect("create acknowledgement");

        domain
            .select_song(&"9".repeat(64))
            .expect("send first song selection");
        let ClientMessage::Command(first_selection) =
            peer.try_recv_message().expect("first song selection")
        else {
            panic!("expected first song selection");
        };
        assert_eq!(first_selection.seq, CommandSeq(2));
        assert_eq!(
            first_selection.expected_room_revision,
            Some(RoomRevision(1))
        );

        domain
            .select_song(&"8".repeat(64))
            .expect("queue second song selection");
        assert!(
            peer.try_recv_message().is_none(),
            "the second global mutation must remain serialized behind the first"
        );
        domain.shutdown_gracefully().expect("graceful shutdown");

        let commands = peer
            .take_graceful_shutdown_messages()
            .expect("graceful control payload")
            .into_iter()
            .map(|message| match message {
                ClientMessage::Command(command) => command,
                other => panic!("unexpected graceful message: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[0], first_selection);
        assert_eq!(commands[1].seq, CommandSeq(3));
        assert_eq!(
            commands[1].expected_room_revision,
            Some(RoomRevision(1)),
            "an unsent global mutation must be stale-rejected rather than bypass revision checks"
        );
        assert!(matches!(
            &commands[1].command,
            taiko_multiplayer_protocol::ClientCommand::SelectSong { song_id }
                if song_id.as_str() == "8".repeat(64)
        ));
        assert_eq!(commands[2].seq, CommandSeq(4));
        assert_eq!(commands[2].expected_room_revision, None);
        assert!(matches!(
            commands[2].command,
            taiko_multiplayer_protocol::ClientCommand::LeaveRoom
        ));
    }

    #[test]
    fn graceful_shutdown_is_idempotent_before_room_membership() {
        let (mut domain, _peer) = domain();
        domain
            .shutdown_gracefully()
            .expect("there is no server-side membership to leave");
        assert!(!domain.connected);
    }

    #[test]
    fn graceful_shutdown_reports_unconfirmed_leave_after_disconnect() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_event(NetworkEvent::Disconnected {
            reason: "test link down".to_owned(),
            next_attempt: 2,
            retry_in: Duration::from_millis(250),
        });
        domain.tick_at(300).expect("process disconnect");

        let error = domain
            .shutdown_gracefully()
            .expect_err("active membership cannot be left gracefully without a connection");
        assert!(
            error
                .to_string()
                .contains("disconnected with active room membership"),
            "{error:#}"
        );
        assert!(!domain.connected);
    }

    #[test]
    fn rejected_create_or_join_fails_immediately_before_membership() {
        let configs = [
            OnlineClientConfig::create("https://example.test", "alice").expect("create config"),
            OnlineClientConfig::join(
                "https://example.test",
                "alice",
                "ABCD",
                &"b".repeat(64),
                JoinRole::Player,
            )
            .expect("join config"),
        ];

        for config in configs {
            let (network, mut peer) = NetworkClient::test_pair();
            let mut domain = OnlineDomain::with_network(config, network);
            peer.send_server(welcome(false, 1));
            domain.tick_at(100).expect("welcome");
            assert!(matches!(
                peer.try_recv_message(),
                Some(ClientMessage::Command(CommandEnvelope {
                    seq: CommandSeq(1),
                    command: taiko_multiplayer_protocol::ClientCommand::CreateRoom
                        | taiko_multiplayer_protocol::ClientCommand::JoinRoom { .. },
                    ..
                }))
            ));

            peer.send_server(ServerMessage::CommandAck(CommandAck {
                seq: CommandSeq(1),
                next_expected_seq: CommandSeq(2),
                outcome: CommandOutcome::Rejected {
                    error: ProtocolError {
                        code: ProtocolErrorCode::InvalidInvitation,
                        message: taiko_multiplayer_protocol::ErrorMessage::new(
                            "membership rejected",
                        )
                        .expect("error message"),
                        retryable: false,
                    },
                    current_room_revision: None,
                },
            }));
            domain.tick_at(200).expect("rejection");

            assert_eq!(domain.phase(), OnlinePhase::Failed);
            assert!(domain.is_terminal());
            assert!(domain.membership.is_none());
            assert!(
                domain.status_message().contains("membership rejected"),
                "{}",
                domain.status_message()
            );
        }
    }

    #[test]
    fn phase_transitions_follow_authoritative_snapshot_and_player_preparation() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(300).expect("lobby snapshot");
        assert_eq!(domain.phase(), OnlinePhase::Lobby);

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            2,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
            PlayerPreparation::Downloading {
                selection: selection(),
                progress_milli: ProgressMilli::new(500).expect("progress"),
            },
        ))));
        domain.tick_at(400).expect("preparing snapshot");
        assert_eq!(domain.phase(), OnlinePhase::Downloading);
        assert_eq!(domain.current_match_id(), Some(MatchId(7)));

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            3,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
            PlayerPreparation::Ready {
                selection: selection(),
            },
        ))));
        domain.tick_at(500).expect("ready snapshot");
        assert_eq!(domain.phase(), OnlinePhase::Ready);
    }

    #[test]
    fn command_sequences_and_revisions_are_monotonic() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::CommandAck(CommandAck {
            seq: CommandSeq(1),
            next_expected_seq: CommandSeq(2),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        }));
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(300).expect("ack and snapshot");

        domain.select_song(&"9".repeat(64)).expect("select song");
        let message = peer.try_recv_message().expect("outgoing command");
        match message {
            ClientMessage::Command(envelope) => {
                assert_eq!(envelope.seq, CommandSeq(2));
                assert_eq!(envelope.expected_room_revision, Some(RoomRevision(1)));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        let queued = domain
            .select_song(&"8".repeat(64))
            .expect("queue second song");
        assert_eq!(
            domain
                .select_song(&"8".repeat(64))
                .expect("deduplicate repeated selection"),
            queued
        );
        assert_eq!(
            domain
                .select_song(&"7".repeat(64))
                .expect("replace unsent selection intent"),
            queued
        );
        assert!(
            peer.try_recv_message().is_none(),
            "revision-sensitive commands must not be pipelined"
        );
        peer.send_server(ServerMessage::CommandAck(CommandAck {
            seq: CommandSeq(2),
            next_expected_seq: CommandSeq(3),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(2)),
            },
        }));
        domain.tick_at(400).expect("first selection ack");
        match peer.try_recv_message().expect("second serialized command") {
            ClientMessage::Command(envelope) => {
                assert_eq!(envelope.seq, CommandSeq(3));
                assert_eq!(envelope.expected_room_revision, Some(RoomRevision(2)));
                assert!(matches!(
                    envelope.command,
                    taiko_multiplayer_protocol::ClientCommand::SelectSong { song_id }
                        if song_id.as_str() == "7".repeat(64)
                ));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn player_scoped_preparation_commands_ignore_unrelated_global_revisions() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(1),
            next_expected_seq: CommandSeq(2),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        });
        domain.ingest_snapshot(snapshot(
            2,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
            PlayerPreparation::Selecting,
        ));

        domain.select_course(selection()).expect("select course");
        let ClientMessage::Command(select_course) =
            peer.try_recv_message().expect("select-course command")
        else {
            panic!("expected select-course command");
        };
        assert_eq!(select_course.seq, CommandSeq(2));
        assert_eq!(
            select_course.expected_room_revision, None,
            "another player's progress may advance the global revision concurrently"
        );
        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(2),
            next_expected_seq: CommandSeq(3),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(3)),
            },
        });

        domain
            .report_preparation(PreparationProgress::Downloading {
                selection: selection(),
                progress_milli: ProgressMilli::new(500).expect("progress"),
            })
            .expect("report preparation");
        let ClientMessage::Command(report_preparation) =
            peer.try_recv_message().expect("preparation command")
        else {
            panic!("expected preparation command");
        };
        assert_eq!(report_preparation.seq, CommandSeq(3));
        assert_eq!(report_preparation.expected_room_revision, None);
        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(3),
            next_expected_seq: CommandSeq(4),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(4)),
            },
        });

        domain.set_ready(false, None).expect("set ready");
        let ClientMessage::Command(set_ready) = peer.try_recv_message().expect("set-ready command")
        else {
            panic!("expected set-ready command");
        };
        assert_eq!(set_ready.seq, CommandSeq(4));
        assert_eq!(set_ready.expected_room_revision, None);
    }

    #[test]
    fn command_ack_cannot_advance_beyond_issued_commands() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);

        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(2),
            next_expected_seq: CommandSeq(3),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        });

        assert!(domain.is_terminal());
        assert!(domain.status_message().contains("only issued through"));
    }

    #[test]
    fn pending_command_window_is_hard_bounded() {
        let (mut domain, _) = domain();
        for match_id in 1..=MAX_PENDING_COMMANDS as u64 {
            domain
                .queue_command(
                    taiko_multiplayer_protocol::ClientCommand::ReturnToLobby {
                        match_id: MatchId(match_id),
                    },
                    match_id,
                )
                .expect("command inside bounded window");
        }
        assert_eq!(domain.pending_commands.len(), MAX_PENDING_COMMANDS);
        let error = domain
            .queue_command(
                taiko_multiplayer_protocol::ClientCommand::ReturnToLobby {
                    match_id: MatchId(MAX_PENDING_COMMANDS as u64 + 1),
                },
                999,
            )
            .expect_err("command window must reject unbounded growth");
        assert!(error.to_string().contains("capacity"));
        assert_eq!(domain.pending_commands.len(), MAX_PENDING_COMMANDS);
    }

    #[test]
    fn stale_snapshot_and_live_state_are_ignored() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(active_snapshot(
            2,
            RoomStage::Playing {
                manifest: manifest(),
                start_at_server_us: 10_000,
                server_tick: 100,
            },
        ))));
        domain.tick_at(300).expect("snapshot");

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        let live = LiveStateSnapshot {
            match_id: MatchId(7),
            state_seq: StateSeq(2),
            server_tick: 100,
            players: BoundedVec::new(vec![
                PlayerLiveState {
                    player_id: PlayerId(1),
                    score: ScoreSnapshot {
                        score: 10,
                        combo: 1,
                        max_combo: 1,
                        gauge_ppm: 100,
                        pass_threshold_ppm: 800_000,
                        great: 1,
                        ok: 0,
                        miss: 0,
                        roll_hits: 0,
                    },
                    finished: false,
                    dnf: false,
                },
                PlayerLiveState {
                    player_id: PlayerId(2),
                    score: ScoreSnapshot {
                        score: 0,
                        combo: 0,
                        max_combo: 0,
                        gauge_ppm: 0,
                        pass_threshold_ppm: 800_000,
                        great: 0,
                        ok: 0,
                        miss: 0,
                        roll_hits: 0,
                    },
                    finished: false,
                    dnf: false,
                },
            ])
            .expect("live players"),
        };
        peer.set_live(live.clone());
        domain.tick_at(400).expect("live");
        assert_eq!(
            domain.snapshot.as_ref().expect("snapshot").revision,
            RoomRevision(2)
        );
        assert_eq!(domain.live_states[&PlayerId(1)].score.score, 10);

        let mut stale_players = live.players.iter().cloned().collect::<Vec<_>>();
        stale_players[0].score.score = 1;
        let stale_live = LiveStateSnapshot {
            state_seq: StateSeq(1),
            players: BoundedVec::new(stale_players).expect("live players"),
            ..live
        };
        domain.ingest_live_state(stale_live);
        assert_eq!(domain.live_states[&PlayerId(1)].score.score, 10);
    }

    #[test]
    fn invalid_live_state_is_terminal() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(active_snapshot(
            2,
            RoomStage::Playing {
                manifest: manifest(),
                start_at_server_us: 10_000,
                server_tick: 100,
            },
        ))));
        domain.tick_at(300).expect("snapshot");

        peer.set_live(LiveStateSnapshot {
            match_id: MatchId(7),
            state_seq: StateSeq(1),
            server_tick: 100,
            players: BoundedVec::new(vec![PlayerLiveState {
                player_id: PlayerId(1),
                score: ScoreSnapshot {
                    score: 10,
                    combo: 2,
                    max_combo: 1,
                    gauge_ppm: 100,
                    pass_threshold_ppm: 800_000,
                    great: 1,
                    ok: 0,
                    miss: 0,
                    roll_hits: 0,
                },
                finished: false,
                dnf: false,
            }])
            .expect("live players"),
        });
        domain.tick_at(400).expect("invalid live handled");

        assert!(domain.is_terminal());
        assert!(domain
            .error()
            .expect("fatal error")
            .display_message()
            .to_ascii_lowercase()
            .contains("invalid live state"));
    }

    #[test]
    fn invalid_snapshot_is_rejected_before_revision_is_applied() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        let mut invalid = snapshot(9, RoomStage::Lobby, PlayerPreparation::Selecting);
        invalid.leader_player_id = PlayerId(99);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(invalid)));
        domain.tick_at(300).expect("snapshot handled");
        assert!(domain.is_terminal());
        assert!(domain.snapshot.is_none());
    }

    #[test]
    fn clock_sync_uses_four_timestamps_and_rejects_outlier() {
        let mut clock = ClockSyncEstimator::default();
        for index in 0..8 {
            let base = index * 100_000;
            assert!(clock.observe(base, base + 11_000, base + 12_000, base + 21_000));
        }
        assert_eq!(clock.estimated_server_now_us(1_000_000), 1_001_000);
        assert!(clock.is_ready());
        assert!(!clock.observe(900_000, 911_000, 912_000, 2_000_000));
        assert_eq!(clock.rejected_samples(), 1);
        assert!(clock.quality().p95_rtt_ms <= 21);
    }

    #[test]
    fn matching_time_sync_response_emits_exact_receipt_before_accepting_sample() {
        let (mut domain, mut peer) = domain();
        let nonce = 41;
        let client_send_us = 1_000;
        let probe_token = ClockProbeToken::parse("a".repeat(64)).expect("probe token");
        domain.pending_time_sync.insert(nonce, client_send_us);

        domain.handle_time_sync(
            TimeSyncResponse {
                nonce,
                client_send_us,
                server_receive_us: 11_000,
                server_send_us: 12_000,
                probe_token: probe_token.clone(),
            },
            21_000,
        );

        assert_eq!(
            peer.try_recv_message(),
            Some(ClientMessage::TimeSyncReceipt(TimeSyncReceipt {
                nonce,
                probe_token,
            }))
        );
        assert_eq!(domain.clock_sync.quality().accepted_samples, 1);
        assert!(domain.pending_clock_probe_acks.contains(&nonce));
    }

    #[test]
    fn clock_readiness_requires_local_and_server_evidence() {
        let (mut server_only, _) = domain();
        acknowledge_server_clock(&mut server_only, 1, true);
        assert!(
            !server_only.clock_is_ready(),
            "server-only must not be ready"
        );

        let (mut local_only, _) = domain();
        make_local_clock_ready(&mut local_only);
        assert!(!local_only.clock_is_ready(), "local-only must not be ready");

        acknowledge_server_clock(&mut local_only, 2, true);
        assert!(
            local_only.clock_is_ready(),
            "both independent views must be ready"
        );
        let status = local_only.clock_status_line();
        assert!(status.contains("ready: local"));
        assert!(status.contains("server 4 samples"));
    }

    #[test]
    fn expired_server_clock_lease_is_not_ready() {
        let (mut domain, _) = domain();
        make_local_clock_ready(&mut domain);
        domain.pending_clock_probe_acks.insert(3);
        domain.handle_clock_probe_ack(ClockProbeAck {
            nonce: 3,
            quality: ClockQuality {
                accepted_samples: CLOCK_MIN_READY_SAMPLES as u16,
                p95_rtt_ms: 20,
                jitter_ms: 2,
            },
            ready: true,
            valid_until_server_us: 0,
        });

        assert!(!domain.clock_is_ready());
    }

    #[test]
    fn pending_time_sync_capacity_pauses_new_probes_without_eviction() {
        let (mut domain, mut peer) = domain();
        for nonce in 0..MAX_PENDING_TIME_SYNC as u64 {
            domain.pending_time_sync.insert(nonce, nonce);
        }

        domain.send_time_sync(99_000);

        assert_eq!(domain.pending_time_sync.len(), MAX_PENDING_TIME_SYNC);
        assert_eq!(domain.pending_time_sync.first_key_value(), Some((&0, &0)));
        assert!(peer.try_recv_message().is_none());
    }

    #[test]
    fn clock_not_ready_rejection_forces_a_fresh_probe_before_retry() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(1),
            next_expected_seq: CommandSeq(2),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        });
        make_local_clock_ready(&mut domain);
        acknowledge_server_clock(&mut domain, 4, true);
        domain.last_time_sync_us = Some(123);
        let seq = domain
            .queue_command(taiko_multiplayer_protocol::ClientCommand::LeaveRoom, 300)
            .expect("queue command");

        domain.handle_command_ack(CommandAck {
            seq,
            next_expected_seq: CommandSeq(seq.0 + 1),
            outcome: CommandOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::ClockNotReady,
                    message: taiko_multiplayer_protocol::ErrorMessage::new(
                        "clock evidence expired",
                    )
                    .expect("message"),
                    retryable: true,
                },
                current_room_revision: Some(RoomRevision(1)),
            },
        });

        assert!(domain.latest_clock_probe_ack.is_none());
        assert!(domain.last_time_sync_us.is_none());
        assert!(!domain.clock_is_ready());
    }

    #[test]
    fn leader_cannot_start_until_local_and_server_clock_views_are_ready() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.ingest_snapshot(active_snapshot(
            1,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
        ));
        assert!(!domain.can_start_match());

        make_local_clock_ready(&mut domain);
        acknowledge_server_clock(&mut domain, 5, true);
        assert!(domain.can_start_match());
    }

    #[test]
    fn leader_cannot_start_while_a_ready_player_is_reconnecting() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        let mut preparing = active_snapshot(
            1,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
        );
        let players = preparing
            .players
            .iter()
            .cloned()
            .map(|mut player| {
                if player.player_id == PlayerId(2) {
                    player.connection = PlayerConnection::Reconnecting {
                        grace_deadline_server_us: 20_000_000,
                    };
                }
                player
            })
            .collect::<Vec<_>>();
        preparing.players = BoundedVec::new(players).expect("players");
        domain.ingest_snapshot(preparing);
        make_local_clock_ready(&mut domain);
        acknowledge_server_clock(&mut domain, 5, true);

        assert!(
            !domain.can_start_match(),
            "client start affordance must match the authority's online-and-ready gate"
        );
    }

    #[test]
    fn reconnecting_transport_disables_room_controls() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        assert!(domain.room_controls_enabled());

        domain
            .handle_network_event(
                NetworkEvent::Disconnected {
                    reason: "fixture link loss".to_owned(),
                    next_attempt: 2,
                    retry_in: Duration::from_millis(250),
                },
                1_000,
            )
            .expect("disconnect event");

        assert_eq!(domain.phase(), OnlinePhase::Reconnecting);
        assert!(!domain.room_controls_enabled());
    }

    #[test]
    fn welcome_resets_all_clock_evidence_for_the_new_transport_session() {
        let (mut domain, peer) = domain();
        make_local_clock_ready(&mut domain);
        acknowledge_server_clock(&mut domain, 90, true);
        domain.pending_time_sync.insert(91, 10);
        domain.pending_clock_probe_acks.insert(92);
        assert!(domain.clock_is_ready());

        peer.send_server(welcome(false, 1));
        domain.tick_at(100).expect("welcome");

        assert!(!domain.clock_sync.is_ready());
        assert!(domain.latest_clock_probe_ack.is_none());
        assert!(!domain.pending_time_sync.contains_key(&91));
        assert!(!domain.pending_clock_probe_acks.contains(&92));
        assert!(!domain.clock_is_ready());
    }

    #[test]
    fn auto_ready_proof_waits_for_both_clock_views() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.ingest_snapshot(snapshot(
            1,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
            PlayerPreparation::Prepared {
                selection: selection(),
            },
        ));
        let prepared = PreparedMatch {
            match_id: MatchId(7),
            selection: selection(),
            audio: Some(
                crate::audio::prepare_song_audio(crate::resource::SongAudioSource::Bytes(
                    include_bytes!("../assets/don.wav").to_vec().into(),
                ))
                .expect("fixture audio"),
            ),
        };
        domain.wants_ready = true;

        make_local_clock_ready(&mut domain);
        assert!(!domain.clock_is_ready());
        assert!(!domain.should_auto_ready());
        assert!(domain.preparation_proof(&prepared).is_err());
        assert!(!domain.is_ready_command_pending());

        acknowledge_server_clock(&mut domain, 93, true);
        assert!(domain.clock_is_ready());
        assert!(domain.should_auto_ready());
        domain.wants_ready = false;
        assert!(
            !domain.should_auto_ready(),
            "an explicit unready choice must survive preparation completion"
        );
        domain.wants_ready = true;
        let proof = domain
            .preparation_proof(&prepared)
            .expect("both clock views permit preparation proof");
        domain.set_ready(true, Some(proof)).expect("queue ready");
        assert!(domain.is_ready_command_pending());
        assert!(!domain.should_auto_ready());
    }

    #[test]
    fn reconnect_uses_resume_identity_and_preserves_pending_sequences() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(300).expect("snapshot");
        let resume = peer.resume_request().expect("resume request");
        assert_eq!(resume.actor_id, ActorId::Player(PlayerId(1)));
        assert_eq!(resume.last_room_revision, RoomRevision(1));

        make_local_clock_ready(&mut domain);
        acknowledge_server_clock(&mut domain, 94, true);
        assert!(domain.clock_is_ready());
        peer.send_event(NetworkEvent::Disconnected {
            reason: "test link down".to_owned(),
            next_attempt: 2,
            retry_in: Duration::from_millis(250),
        });
        domain.tick_at(400).expect("disconnect");
        assert_eq!(domain.phase(), OnlinePhase::Reconnecting);
        assert!(!domain.clock_is_ready());
        assert!(domain.pending_time_sync.is_empty());
        assert!(domain.pending_clock_probe_acks.is_empty());

        peer.send_server(welcome(true, 2));
        domain.tick_at(500).expect("resumed welcome");
        assert!(!domain.is_terminal());
        assert_eq!(domain.phase(), OnlinePhase::Reconnecting);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Lobby,
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(600).expect("equal resume snapshot");
        assert_eq!(domain.phase(), OnlinePhase::Lobby);
    }

    #[test]
    fn new_match_epoch_discards_old_live_and_unacked_input() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            1,
            RoomStage::Preparing {
                match_id: MatchId(7),
                song: song(),
            },
            PlayerPreparation::Ready {
                selection: selection(),
            },
        ))));
        domain.tick_at(300).expect("match seven");
        domain.live_states.insert(
            PlayerId(1),
            PlayerLiveState {
                player_id: PlayerId(1),
                score: ScoreSnapshot {
                    score: 1,
                    combo: 0,
                    max_combo: 0,
                    gauge_ppm: 0,
                    pass_threshold_ppm: 0,
                    great: 0,
                    ok: 0,
                    miss: 0,
                    roll_hits: 0,
                },
                finished: false,
                dnf: false,
            },
        );
        domain.pending_inputs.insert(
            InputSeq(1),
            InputEvent {
                seq: InputSeq(1),
                tick: 0,
                action: DrumAction::LEFT_DON,
            },
        );

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            2,
            RoomStage::Preparing {
                match_id: MatchId(8),
                song: song(),
            },
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(400).expect("match eight");
        assert!(domain.live_states.is_empty());
        assert!(domain.pending_inputs.is_empty());
        assert_eq!(domain.next_input_seq, InputSeq(1));
    }

    #[test]
    fn resume_after_lost_epoch_transition_snapshot_reconciles_from_new_authoritative_epoch() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(active_snapshot(
            1,
            RoomStage::Playing {
                manifest: manifest(),
                start_at_server_us: 10_000,
                server_tick: 100,
            },
        ))));
        domain.tick_at(300).expect("old match snapshot");
        assert_eq!(domain.active_match_id, Some(MatchId(7)));

        assert_eq!(
            domain
                .submit_input(110, DrumAction::LEFT_DON)
                .expect("old match input")
                .map(|submitted| submitted.seq),
            Some(FIRST_INPUT_SEQ)
        );
        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: Some(FIRST_INPUT_SEQ),
            next_expected_seq: InputSeq(2),
            server_tick: 110,
            outcome: InputOutcome::Accepted,
        });
        assert_eq!(domain.last_input_ack, Some(FIRST_INPUT_SEQ));
        assert_eq!(
            domain
                .submit_input(120, DrumAction::RIGHT_KAT)
                .expect("unacknowledged old match input")
                .map(|submitted| submitted.seq),
            Some(InputSeq(2))
        );
        assert!(domain.pending_inputs.contains_key(&InputSeq(2)));

        // The authority has already advanced to match 8, but that transition
        // snapshot is deliberately never delivered on this transport.
        peer.send_event(NetworkEvent::Disconnected {
            reason: "transition snapshot lost".to_owned(),
            next_attempt: 2,
            retry_in: Duration::from_millis(1),
        });
        domain
            .tick_at(400)
            .expect("disconnect after lost transition");
        let stale_resume = peer.resume_request().expect("same actor resume request");
        assert_eq!(stale_resume.actor_id, ActorId::Player(PlayerId(1)));
        assert_eq!(stale_resume.last_room_revision, RoomRevision(1));

        peer.send_server(welcome(true, 2));
        peer.send_server(membership());
        peer.send_server(ServerMessage::RoomSnapshot(Box::new(snapshot(
            2,
            RoomStage::Preparing {
                match_id: MatchId(8),
                song: song(),
            },
            PlayerPreparation::Selecting,
        ))));
        domain.tick_at(500).expect("authoritative resumed epoch");

        assert!(!domain.is_terminal());
        assert_eq!(domain.actor_id(), Some(&ActorId::Player(PlayerId(1))));
        assert_eq!(domain.active_match_id, Some(MatchId(8)));
        assert_eq!(domain.phase(), OnlinePhase::SelectingCourse);
        assert!(domain.pending_inputs.is_empty());
        assert_eq!(domain.next_input_seq, FIRST_INPUT_SEQ);
        assert_eq!(domain.last_input_ack, None);
        assert_eq!(
            peer.resume_request()
                .expect("resume state follows authoritative snapshot")
                .last_room_revision,
            RoomRevision(2)
        );
    }

    #[test]
    fn invalid_non_resumed_welcome_after_membership_is_terminal() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        peer.send_server(welcome(false, 2));
        domain.tick_at(300).expect("welcome handled");
        assert!(domain.is_terminal());
        assert_eq!(domain.phase(), OnlinePhase::Failed);
        assert!(domain
            .status_message()
            .contains("did not resume the existing player identity"));
    }

    #[test]
    fn fresh_connection_rejects_forged_resumed_welcome() {
        let (mut domain, peer) = domain();
        peer.send_server(welcome(true, 1));

        domain.tick_at(100).expect("forged welcome handled");

        assert!(domain.is_terminal());
        assert!(domain.membership.is_none());
        assert!(domain.pending_commands.is_empty());
        assert!(domain
            .status_message()
            .contains("without a local player identity"));
    }

    #[test]
    fn welcome_rejects_zero_next_expected_command_sequence() {
        let (mut domain, peer) = domain();
        peer.send_server(welcome(false, 0));

        domain.tick_at(100).expect("zero watermark handled");

        assert!(domain.is_terminal());
        assert!(domain.pending_commands.is_empty());
        assert!(domain
            .status_message()
            .contains("zero next-expected command sequence"));
    }

    #[test]
    fn resume_welcome_cannot_discard_an_unsent_pending_command() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        let unsent_seq = domain
            .queue_command(taiko_multiplayer_protocol::ClientCommand::LeaveRoom, 250)
            .expect("queue second command");
        assert_eq!(unsent_seq, CommandSeq(2));
        assert!(
            !domain
                .pending_commands
                .get(&unsent_seq)
                .expect("second command remains pending")
                .sent
        );
        let pending_before = domain.pending_commands.keys().copied().collect::<Vec<_>>();

        peer.send_server(welcome(true, 3));
        domain.tick_at(300).expect("forged watermark handled");

        assert!(domain.is_terminal());
        assert_eq!(
            domain.pending_commands.keys().copied().collect::<Vec<_>>(),
            pending_before,
            "invalid handshake must fail before reconciling pending commands"
        );
        assert!(domain
            .status_message()
            .contains("beyond the client send watermark 2"));
    }

    #[test]
    fn resume_welcome_cannot_regress_below_acknowledged_command_watermark() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.handle_command_ack(CommandAck {
            seq: CommandSeq(1),
            next_expected_seq: CommandSeq(2),
            outcome: CommandOutcome::Applied {
                room_revision: Some(RoomRevision(1)),
            },
        });
        assert_eq!(domain.last_acked_command_seq, CommandSeq(1));

        peer.send_server(welcome(true, 1));
        domain.tick_at(300).expect("regressed watermark handled");

        assert!(domain.is_terminal());
        assert!(domain
            .status_message()
            .contains("below the acknowledged watermark 2"));
    }

    #[test]
    fn first_membership_must_match_create_join_or_spectate_intent() {
        let invitation = "b".repeat(64);
        let cases = [
            (
                "create cannot become spectator",
                OnlineClientConfig::create("https://example.test", "alice").expect("create config"),
                membership_for("WXYZ", ActorId::Spectator(SpectatorId(9))),
                "spectator identity for a create-room request",
            ),
            (
                "join cannot enter another room",
                OnlineClientConfig::join(
                    "https://example.test",
                    "alice",
                    "ABCD",
                    &invitation,
                    JoinRole::Player,
                )
                .expect("join config"),
                membership_for("WXYZ", ActorId::Player(PlayerId(1))),
                "instead of requested room ABCD",
            ),
            (
                "player join cannot become spectator",
                OnlineClientConfig::join(
                    "https://example.test",
                    "alice",
                    "ABCD",
                    &invitation,
                    JoinRole::Player,
                )
                .expect("player config"),
                membership_for("ABCD", ActorId::Spectator(SpectatorId(9))),
                "requested Player role",
            ),
            (
                "join cannot receive a different invitation token",
                OnlineClientConfig::join(
                    "https://example.test",
                    "alice",
                    "ABCD",
                    &invitation,
                    JoinRole::Player,
                )
                .expect("join config"),
                ServerMessage::MembershipGranted(MembershipGranted {
                    room_code: RoomCode::parse("ABCD").expect("room code"),
                    actor_id: ActorId::Player(PlayerId(1)),
                    resume_token: ResumeToken::parse("a".repeat(64)).expect("resume token"),
                    invitation_token: InvitationToken::parse("c".repeat(64))
                        .expect("forged invitation token"),
                }),
                "invitation token different from the join request",
            ),
            (
                "spectate cannot become player",
                OnlineClientConfig::join(
                    "https://example.test",
                    "alice",
                    "ABCD",
                    &invitation,
                    JoinRole::Spectator,
                )
                .expect("spectator config"),
                membership_for("ABCD", ActorId::Player(PlayerId(1))),
                "requested Spectator role",
            ),
        ];

        for (case, config, forged_membership, expected_message) in cases {
            let (network, mut peer) = NetworkClient::test_pair();
            let mut domain = OnlineDomain::with_network(config, network);
            peer.send_server(welcome(false, 1));
            domain.tick_at(100).expect("welcome");
            while peer.try_recv_message().is_some() {}

            peer.send_server(forged_membership);
            domain.tick_at(200).expect("forged membership handled");

            assert!(domain.is_terminal(), "{case}");
            assert!(domain.membership.is_none(), "{case}");
            assert!(
                domain.status_message().contains(expected_message),
                "{case}: {}",
                domain.status_message()
            );
        }
    }

    #[test]
    fn authoritative_snapshot_must_contain_the_granted_local_membership() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        let mut other_player = player(PlayerPreparation::Selecting);
        other_player.player_id = PlayerId(2);
        let forged = RoomSnapshot {
            leader_player_id: PlayerId(2),
            players: BoundedVec::new(vec![other_player]).expect("one player"),
            ..snapshot(1, RoomStage::Lobby, PlayerPreparation::Selecting)
        };
        forged.validate().expect("fixture is structurally valid");

        peer.send_server(ServerMessage::RoomSnapshot(Box::new(forged)));
        domain.tick_at(300).expect("forged snapshot handled");

        assert!(domain.is_terminal());
        assert!(domain
            .status_message()
            .contains("omitted the granted local membership"));
    }

    #[test]
    fn resume_membership_cannot_rotate_identity_credentials() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);

        peer.send_server(welcome(true, 2));
        domain.tick_at(300).expect("resume welcome");
        peer.send_server(ServerMessage::MembershipGranted(MembershipGranted {
            room_code: RoomCode::parse("ABCD").expect("room code"),
            actor_id: ActorId::Player(PlayerId(1)),
            resume_token: ResumeToken::parse("c".repeat(64)).expect("forged resume token"),
            invitation_token: InvitationToken::parse("b".repeat(64)).expect("invitation token"),
        }));
        domain
            .tick_at(400)
            .expect("forged resumed membership handled");

        assert!(domain.is_terminal());
        assert!(domain
            .status_message()
            .contains("membership credentials during resume"));
    }

    #[test]
    fn mismatched_schema_welcome_is_terminal() {
        let (mut domain, peer) = domain();
        peer.send_server(ServerMessage::Welcome(
            taiko_multiplayer_protocol::ServerWelcome {
                protocol_version: PROTOCOL_VERSION,
                wire_schema_sha256: hash('f'),
                heartbeat_interval_ms: 1_000,
                reconnect_grace_ms: 10_000,
                resumed: false,
                next_expected_command_seq: FIRST_COMMAND_SEQ,
            },
        ));
        domain.tick_at(100).expect("welcome handled");
        assert!(domain.is_terminal());
        assert!(domain.status_message().contains("different"));
    }

    #[test]
    fn input_ack_removes_contiguous_prefix_and_timeout_retries_gap() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;

        assert_eq!(
            domain
                .submit_input(10, DrumAction::LEFT_DON)
                .expect("first input")
                .map(|submitted| submitted.seq),
            Some(FIRST_INPUT_SEQ)
        );
        match peer.try_recv_message().expect("first input batch") {
            ClientMessage::Input(batch) => {
                assert_eq!(batch.match_id, MatchId(7));
                assert_eq!(batch.events.len(), 1);
                assert_eq!(batch.events[0].seq, FIRST_INPUT_SEQ);
            }
            other => panic!("unexpected message: {other:?}"),
        }

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: Some(FIRST_INPUT_SEQ),
            next_expected_seq: InputSeq(2),
            server_tick: 10,
            outcome: InputOutcome::Accepted,
        });
        assert!(domain.pending_inputs.is_empty());

        domain
            .submit_input(20, DrumAction::RIGHT_KAT)
            .expect("second input");
        domain
            .submit_input(30, DrumAction::LEFT_DON)
            .expect("third input");
        while peer.try_recv_message().is_some() {}
        domain
            .tick_at(INPUT_RETRY_INTERVAL_US.saturating_mul(2))
            .expect("retry tick");
        let retried = std::iter::from_fn(|| peer.try_recv_message()).find_map(|message| {
            if let ClientMessage::Input(batch) = message {
                Some(batch)
            } else {
                None
            }
        });
        let retried = retried.expect("unacked input retry");
        assert_eq!(
            retried
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            vec![InputSeq(2), InputSeq(3)]
        );
    }

    #[test]
    fn submitted_input_ticks_are_monotonic_within_each_match_epoch() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;

        let first = domain
            .submit_input(20, DrumAction::LEFT_DON)
            .expect("first input")
            .expect("accepted first input");
        assert_eq!(first.tick, 20);
        let first_batch = match peer.try_recv_message().expect("first input batch") {
            ClientMessage::Input(batch) => batch,
            other => panic!("unexpected message: {other:?}"),
        };
        assert_eq!(first_batch.events[0].tick, 20);

        let clamped = domain
            .submit_input(19, DrumAction::RIGHT_KAT)
            .expect("clock-slew input")
            .expect("accepted clock-slew input");
        assert_eq!(clamped.tick, 20);
        let second_batch = match peer.try_recv_message().expect("second input batch") {
            ClientMessage::Input(batch) => batch,
            other => panic!("unexpected message: {other:?}"),
        };
        assert_eq!(second_batch.events[0].tick, 20);
        assert_eq!(second_batch.events[0].seq, InputSeq(2));

        domain.reset_match_epoch(Some(MatchId(8)));
        domain.phase = OnlinePhase::Playing;
        let next_match = domain
            .submit_input(3, DrumAction::RIGHT_DON)
            .expect("next match input")
            .expect("accepted next match input");
        assert_eq!(next_match.tick, 3);
    }

    #[test]
    fn submit_input_preserves_all_four_physical_actions_on_the_wire() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;

        for (index, action) in [
            DrumAction::LEFT_DON,
            DrumAction::RIGHT_DON,
            DrumAction::LEFT_KAT,
            DrumAction::RIGHT_KAT,
        ]
        .into_iter()
        .enumerate()
        {
            let seq = InputSeq(index as u64 + 1);
            assert_eq!(
                domain
                    .submit_input((index as i64 + 1) * 10, action)
                    .expect("input")
                    .map(|submitted| submitted.seq),
                Some(seq)
            );
            match peer.try_recv_message().expect("input batch") {
                ClientMessage::Input(batch) => {
                    assert_eq!(batch.match_id, MatchId(7));
                    assert_eq!(batch.events.len(), 1);
                    assert_eq!(batch.events[0].seq, seq);
                    assert_eq!(batch.events[0].action, action);
                }
                other => panic!("unexpected message: {other:?}"),
            }
        }
    }

    #[test]
    fn local_pending_input_window_drops_excess_without_ending_the_session() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;
        for sequence in 1..=MAX_PENDING_INPUTS as u64 {
            domain.pending_inputs.insert(
                InputSeq(sequence),
                InputEvent {
                    seq: InputSeq(sequence),
                    tick: sequence as i64,
                    action: DrumAction::LEFT_DON,
                },
            );
        }
        domain.next_input_seq = InputSeq(MAX_PENDING_INPUTS as u64 + 1);

        let outcome = domain
            .submit_input(999, DrumAction::RIGHT_KAT)
            .expect("backpressure is a nonfatal local drop");

        assert_eq!(outcome, None);
        assert_eq!(domain.pending_inputs.len(), MAX_PENDING_INPUTS);
        assert_eq!(
            domain.next_input_seq,
            InputSeq(MAX_PENDING_INPUTS as u64 + 1)
        );
        assert_eq!(domain.phase(), OnlinePhase::Playing);
        assert!(domain.status_message().contains("locally dropped"));
    }

    #[test]
    fn non_retryable_input_rejection_is_terminal() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;
        domain
            .submit_input(10, DrumAction::LEFT_DON)
            .expect("input");

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: None,
            next_expected_seq: FIRST_INPUT_SEQ,
            server_tick: 100,
            outcome: InputOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::InvalidInput,
                    message: taiko_multiplayer_protocol::ErrorMessage::new("late input")
                        .expect("message"),
                    retryable: false,
                },
            },
        });

        assert_eq!(domain.phase(), OnlinePhase::Failed);
        assert!(domain.is_terminal());
    }

    #[test]
    fn rejection_for_an_already_accepted_retry_is_ignored() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;
        domain
            .submit_input(10, DrumAction::LEFT_DON)
            .expect("input");

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: Some(FIRST_INPUT_SEQ),
            next_expected_seq: InputSeq(2),
            server_tick: 100,
            outcome: InputOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::InvalidStage,
                    message: taiko_multiplayer_protocol::ErrorMessage::new(
                        "stale retry arrived after finalizing",
                    )
                    .expect("message"),
                    retryable: false,
                },
            },
        });

        assert_eq!(domain.phase(), OnlinePhase::Playing);
        assert!(!domain.is_terminal());
        assert!(domain.pending_inputs.is_empty());
        assert_eq!(domain.last_input_ack, Some(FIRST_INPUT_SEQ));
    }

    #[test]
    fn consumed_drop_only_removes_its_prefix_and_keeps_playing() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;
        domain
            .submit_input(10, DrumAction::LEFT_DON)
            .expect("first input");
        domain
            .submit_input(20, DrumAction::RIGHT_KAT)
            .expect("second input");

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: Some(FIRST_INPUT_SEQ),
            next_expected_seq: InputSeq(2),
            server_tick: 100,
            outcome: InputOutcome::Rejected {
                error: ProtocolError {
                    code: ProtocolErrorCode::InvalidInput,
                    message: taiko_multiplayer_protocol::ErrorMessage::new(
                        "late input was dropped",
                    )
                    .expect("message"),
                    retryable: false,
                },
            },
        });

        assert_eq!(domain.phase(), OnlinePhase::Playing);
        assert!(!domain.is_terminal());
        assert!(!domain.pending_inputs.contains_key(&FIRST_INPUT_SEQ));
        assert!(domain.pending_inputs.contains_key(&InputSeq(2)));
        assert_eq!(domain.last_input_ack, Some(FIRST_INPUT_SEQ));
    }

    #[test]
    fn inconsistent_input_ack_is_terminal() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: None,
            next_expected_seq: InputSeq(2),
            server_tick: 100,
            outcome: InputOutcome::Accepted,
        });

        assert!(domain.is_terminal());
        assert!(domain
            .error()
            .expect("fatal error")
            .display_message()
            .contains("input acknowledgement"));
    }

    #[test]
    fn input_ack_cannot_exceed_the_client_send_watermark() {
        let (mut domain, mut peer) = domain();
        establish(&mut domain, &mut peer);
        domain.active_match_id = Some(MatchId(7));
        domain.phase = OnlinePhase::Playing;
        domain
            .submit_input(10, DrumAction::LEFT_DON)
            .expect("input");

        domain.handle_input_ack(InputAck {
            match_id: MatchId(7),
            highest_contiguous_seq: Some(InputSeq(2)),
            next_expected_seq: InputSeq(3),
            server_tick: 100,
            outcome: InputOutcome::Accepted,
        });

        assert!(domain.is_terminal());
        assert!(domain.status_message().contains("send watermark"));
    }

    #[test]
    fn join_config_keeps_invitation_secret_out_of_debug_output() {
        let config = OnlineClientConfig::join(
            "https://example.test",
            "alice",
            "ABCD",
            &"a".repeat(64),
            JoinRole::Player,
        )
        .expect("join config");
        assert!(!format!("{config:?}").contains(&"a".repeat(64)));
    }
}
