use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};
use taiko_resource_protocol::{song_content_identity_sha256, ResourceSemantics};
use thiserror::Error;

mod value;

pub use taiko_resource_protocol::{
    MAX_ARTIST_BYTES, MAX_COURSE_NAME_BYTES, MAX_SUBTITLE_BYTES, MAX_TITLE_BYTES,
};
pub use value::{
    BoundedString, BoundedText, BoundedVec, ClockProbeToken, ContentHash, InvitationToken,
    ProgressMilli, ResumeToken, RoomCode, SongId, ValueError,
};

pub const PROTOCOL_VERSION: u32 = 2;
/// Canonical semantic descriptor hashed by [`WIRE_SCHEMA_SHA256`].
///
/// Any wire shape, ordering contract, delivery guarantee, identity rule, typed
/// error, or protocol limit change must update this descriptor and digest
/// together. No compatibility branch for an older protocol version is retained.
pub const WIRE_SCHEMA_DESCRIPTOR: &str = concat!(
    "taiko-multiplayer-protocol/v2\n",
    "wire=strict-json-text+deny-unknown\n",
    "endpoint=/v2/multiplayer/ws;health=/v2/multiplayer/healthz\n",
    "authority=server-room-actor\n",
    "ordering=command-seq+room-revision+match-id+input-seq+state-seq\n",
    "input-policy=consume-dropped-contiguous+retry-gap\n",
    "drum-input-fields=side,zone;side=left|right;zone=don|kat;both-required\n",
    "delivery=bounded-reliable+coalesced-live\n",
    "identity=invitation+resume-token+session-lease\n",
    "welcome=protocol-version+wire-schema-sha256+heartbeat-interval+reconnect-grace+resumed",
    "+next-expected-command-seq\n",
    "membership=room-code+actor-id+resume-token+invitation-token;role-from-actor-variant\n",
    "heartbeat=client-nonce+server-nonce-ack\n",
    "clock=server-challenge-response+server-measured-quality\n",
    "clock-lease=absolute-server-expiry\n",
    "errors=typed-retryability+session-expired\n",
    "content-identity=song-id==resource-song-manifest-v1(source-id+audio-presence+optional-audio-id",
    "+full-semantics+ordered-canonical-course-hashes)\n",
    "resource-ids=lowercase-sha256-content-hashes;no-duplicate-content-hash-fields\n",
    "content-semantics=canonical-schema+importer+taiko-ruleset+audio-decoder;",
    "each=nonzero-version+sha256\n",
    "preparation-proof=source-id+required-nullable-audio-id+canonical-course-hash",
    "+full-match-semantics\n",
    "player-selection=course-id;branch-policy=authority-derived-automatic\n",
    "presentation-limits=shared-resource-contract\n",
    "snapshot-player-input-watermark=optional-input-seq;none-before-first-input;",
    "resume-reconciles-from-authoritative-snapshot\n",
    "final-replay-digest=sha256(server-authoritative-replay/v2+match+song+semantics",
    "+player-assignment+ordered-accepted-scoring-inputs);dropped-attempts-excluded\n",
    "limits=bounded-values-v1+reject-sequence-at-max-plus-one\n",
);
pub const WIRE_SCHEMA_SHA256: &str =
    "0792569ac7ed14846ad8bfcff1b7568b917505b34997df668db641925f437257";
pub const MAX_DISPLAY_NAME_BYTES: usize = 32;
pub const MAX_CLIENT_BUILD_BYTES: usize = 64;
pub const MAX_ROOM_CODE_BYTES: usize = 4;
pub const MAX_SECRET_TOKEN_BYTES: usize = 64;
pub const MAX_INPUT_BATCH_EVENTS: usize = 32;
pub const MAX_INPUT_EVENTS_PER_SECOND: u64 = 50;
pub const MAX_INPUT_BURST_EVENTS: u64 = 4;
pub const MAX_COURSES_PER_SONG: usize = 16;
pub const MAX_PLAYERS: usize = 4;
pub const MAX_SPECTATORS: usize = 64;
pub const MAX_PROTOCOL_ERROR_MESSAGE_BYTES: usize = 256;
pub const MAX_WIRE_MESSAGE_BYTES: usize = 256 * 1024;
pub const MIN_MATCH_PLAYERS: usize = 2;
pub const MIN_COUNTDOWN_MS: u32 = 500;
pub const MAX_COUNTDOWN_MS: u32 = 30_000;
pub const MAX_INPUT_LATENESS_MS: u32 = 1_000;

pub type Tick = i64;

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

macro_rules! sequence_id {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

sequence_id!(SessionId);
sequence_id!(PlayerId);
sequence_id!(SpectatorId);
sequence_id!(RoomRevision);
sequence_id!(MatchId);
sequence_id!(CommandSeq);
sequence_id!(InputSeq);
sequence_id!(StateSeq);

pub const FIRST_COMMAND_SEQ: CommandSeq = CommandSeq(1);
pub const FIRST_INPUT_SEQ: InputSeq = InputSeq(1);
pub const FIRST_ROOM_REVISION: RoomRevision = RoomRevision(1);
pub const FIRST_MATCH_ID: MatchId = MatchId(1);

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct CourseId(pub u32);

pub type DisplayName = BoundedString<MAX_DISPLAY_NAME_BYTES>;
pub type ClientBuild = BoundedString<MAX_CLIENT_BUILD_BYTES>;
pub type DisplayTitle = BoundedString<MAX_TITLE_BYTES>;
pub type CourseName = BoundedString<MAX_COURSE_NAME_BYTES>;
pub type ErrorMessage = BoundedString<MAX_PROTOCOL_ERROR_MESSAGE_BYTES>;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ActorId {
    Player(PlayerId),
    Spectator(SpectatorId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomRole {
    Player,
    Spectator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinRole {
    Player,
    Spectator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchSemantics {
    pub canonical_schema_version: u32,
    pub canonical_schema_digest: ContentHash,
    pub importer_semantics_version: u32,
    pub importer_semantics_digest: ContentHash,
    pub ruleset_version: u32,
    pub ruleset_digest: ContentHash,
    pub audio_decoder_semantics_version: u32,
    pub audio_decoder_semantics_digest: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CourseManifest {
    pub course_id: CourseId,
    pub name: CourseName,
    pub level: Option<u8>,
    pub canonical_chart_hash: ContentHash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerSelection {
    pub course_id: CourseId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SongManifest {
    pub song_id: SongId,
    pub source_id: ContentHash,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub audio_id: Option<ContentHash>,
    pub title: DisplayTitle,
    pub subtitle: BoundedText<MAX_SUBTITLE_BYTES>,
    pub artist: BoundedText<MAX_ARTIST_BYTES>,
    pub semantics: MatchSemantics,
    pub courses: BoundedVec<CourseManifest, MAX_COURSES_PER_SONG>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerCourseAssignment {
    pub player_id: PlayerId,
    pub selection: PlayerSelection,
    pub canonical_chart_hash: ContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MatchManifest {
    pub match_id: MatchId,
    pub song: SongManifest,
    pub assignments: BoundedVec<PlayerCourseAssignment, MAX_PLAYERS>,
    pub countdown_ms: u32,
    pub input_lateness_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolInvariantError {
    #[error("{0} semantic version must be non-zero")]
    ZeroSemanticVersion(&'static str),
    #[error("song manifest must contain at least one course")]
    NoCourses,
    #[error("course ids must be dense and ordered; expected {expected}, got {actual}")]
    NonDenseCourseId { expected: u32, actual: u32 },
    #[error("song_id does not match the immutable resource manifest identity")]
    SongIdentityMismatch,
    #[error("match id must be non-zero")]
    ZeroMatchId,
    #[error("match requires at least {MIN_MATCH_PLAYERS} player assignments")]
    TooFewAssignments,
    #[error("player {0:?} has more than one course assignment")]
    DuplicatePlayerAssignment(PlayerId),
    #[error("assignment for player {player_id:?} references unknown course {course_id:?}")]
    UnknownAssignedCourse {
        player_id: PlayerId,
        course_id: CourseId,
    },
    #[error("assignment for player {0:?} has the wrong canonical chart hash")]
    AssignedCourseHashMismatch(PlayerId),
    #[error("countdown {0}ms is outside the supported range")]
    InvalidCountdown(u32),
    #[error("input lateness {0}ms exceeds the protocol maximum")]
    InvalidInputLateness(u32),
    #[error("preparation proof does not match the selected song or course")]
    PreparationProofMismatch,
    #[error("room snapshot has no players")]
    RoomHasNoPlayers,
    #[error("room revision must be non-zero")]
    ZeroRoomRevision,
    #[error("room leader flags do not identify exactly the declared leader")]
    InvalidRoomLeader,
    #[error("room snapshot repeats player {0:?}")]
    DuplicatePlayer(PlayerId),
    #[error("player {0:?} has a zero input acknowledgement watermark")]
    ZeroPlayerInputWatermark(PlayerId),
    #[error("room snapshot repeats spectator {0:?}")]
    DuplicateSpectator(SpectatorId),
    #[error("player {player_id:?} selected unknown course {course_id:?}")]
    UnknownPreparedCourse {
        player_id: PlayerId,
        course_id: CourseId,
    },
    #[error("active match assignments do not match room players")]
    ActivePlayerAssignmentMismatch,
    #[error("player {0:?} preparation does not match the active assignment")]
    ActivePreparationMismatch(PlayerId),
    #[error("finished results do not match the immutable match assignments")]
    FinishedResultMismatch,
    #[error("score snapshot violates taiko score invariants")]
    InvalidScore,
    #[error("live-state sequence must be non-zero")]
    ZeroStateSequence,
    #[error("server tick must be non-negative")]
    NegativeServerTick,
    #[error("live state repeats player {0:?}")]
    DuplicateLivePlayer(PlayerId),
    #[error("live-state players do not match the immutable match assignments")]
    LivePlayerMismatch,
    #[error("a DNF live state must also be finished")]
    DnfPlayerNotFinished,
    #[error("final result violates score, timing, pass, or DNF invariants")]
    InvalidFinalResult,
    #[error("input acknowledgement sequence or server tick is inconsistent")]
    InvalidInputAcknowledgement,
}

impl MatchSemantics {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        for (name, version) in [
            ("canonical schema", self.canonical_schema_version),
            ("importer", self.importer_semantics_version),
            ("ruleset", self.ruleset_version),
            ("audio decoder", self.audio_decoder_semantics_version),
        ] {
            if version == 0 {
                return Err(ProtocolInvariantError::ZeroSemanticVersion(name));
            }
        }
        Ok(())
    }
}

impl SongManifest {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        self.semantics.validate()?;
        if self.courses.is_empty() {
            return Err(ProtocolInvariantError::NoCourses);
        }
        for (position, course) in self.courses.iter().enumerate() {
            let expected = u32::try_from(position).expect("bounded course count fits u32");
            if course.course_id.0 != expected {
                return Err(ProtocolInvariantError::NonDenseCourseId {
                    expected,
                    actual: course.course_id.0,
                });
            }
        }
        if self.derive_song_id()? != self.song_id {
            return Err(ProtocolInvariantError::SongIdentityMismatch);
        }
        Ok(())
    }

    pub fn derive_song_id(&self) -> Result<SongId, ProtocolInvariantError> {
        let semantics = ResourceSemantics {
            canonical_schema_version: self.semantics.canonical_schema_version,
            canonical_schema_sha256: self.semantics.canonical_schema_digest.to_string(),
            importer_semantics_version: self.semantics.importer_semantics_version,
            importer_semantics_sha256: self.semantics.importer_semantics_digest.to_string(),
            taiko_ruleset_version: self.semantics.ruleset_version,
            taiko_ruleset_sha256: self.semantics.ruleset_digest.to_string(),
            audio_decoder_semantics_version: self.semantics.audio_decoder_semantics_version,
            audio_decoder_semantics_sha256: self
                .semantics
                .audio_decoder_semantics_digest
                .to_string(),
        };
        let course_count = u32::try_from(self.courses.len())
            .map_err(|_| ProtocolInvariantError::SongIdentityMismatch)?;
        let digest = song_content_identity_sha256(
            self.source_id.as_str(),
            self.audio_id.as_ref().map(ContentHash::as_str),
            &semantics,
            self.courses
                .iter()
                .map(|course| course.canonical_chart_hash.as_str()),
            course_count,
        )
        .map_err(|_| ProtocolInvariantError::SongIdentityMismatch)?;
        SongId::parse(digest).map_err(|_| ProtocolInvariantError::SongIdentityMismatch)
    }
}

impl MatchManifest {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        self.song.validate()?;
        if self.match_id.0 == 0 {
            return Err(ProtocolInvariantError::ZeroMatchId);
        }
        if self.assignments.len() < MIN_MATCH_PLAYERS {
            return Err(ProtocolInvariantError::TooFewAssignments);
        }
        if !(MIN_COUNTDOWN_MS..=MAX_COUNTDOWN_MS).contains(&self.countdown_ms) {
            return Err(ProtocolInvariantError::InvalidCountdown(self.countdown_ms));
        }
        if self.input_lateness_ms > MAX_INPUT_LATENESS_MS {
            return Err(ProtocolInvariantError::InvalidInputLateness(
                self.input_lateness_ms,
            ));
        }

        let mut assigned_players = HashSet::with_capacity(self.assignments.len());
        for assignment in &self.assignments {
            if !assigned_players.insert(assignment.player_id) {
                return Err(ProtocolInvariantError::DuplicatePlayerAssignment(
                    assignment.player_id,
                ));
            }
            let Some(course) = self
                .song
                .courses
                .iter()
                .find(|course| course.course_id == assignment.selection.course_id)
            else {
                return Err(ProtocolInvariantError::UnknownAssignedCourse {
                    player_id: assignment.player_id,
                    course_id: assignment.selection.course_id,
                });
            };
            if course.canonical_chart_hash != assignment.canonical_chart_hash {
                return Err(ProtocolInvariantError::AssignedCourseHashMismatch(
                    assignment.player_id,
                ));
            }
        }
        Ok(())
    }
}

impl PreparationProof {
    pub fn validate_for(
        &self,
        song: &SongManifest,
        assignment: &PlayerCourseAssignment,
    ) -> Result<(), ProtocolInvariantError> {
        if self.source_id != song.source_id
            || self.audio_id != song.audio_id
            || self.semantics != song.semantics
            || self.canonical_chart_hash != assignment.canonical_chart_hash
        {
            return Err(ProtocolInvariantError::PreparationProofMismatch);
        }
        Ok(())
    }
}

impl PlayerPreparation {
    pub fn selection(&self) -> Option<&PlayerSelection> {
        match self {
            Self::Selecting => None,
            Self::Downloading { selection, .. }
            | Self::Verifying { selection }
            | Self::Loading { selection }
            | Self::Prepared { selection }
            | Self::Ready { selection } => Some(selection),
            Self::Failed { selection, .. } => selection.as_ref(),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

impl RoomStage {
    pub fn match_id(&self) -> Option<MatchId> {
        match self {
            Self::Lobby => None,
            Self::Preparing { match_id, .. } => Some(*match_id),
            Self::Countdown { manifest, .. }
            | Self::Playing { manifest, .. }
            | Self::Finalizing { manifest, .. }
            | Self::Finished { manifest, .. } => Some(manifest.match_id),
        }
    }

    pub fn song(&self) -> Option<&SongManifest> {
        match self {
            Self::Lobby => None,
            Self::Preparing { song, .. } => Some(song),
            Self::Countdown { manifest, .. }
            | Self::Playing { manifest, .. }
            | Self::Finalizing { manifest, .. }
            | Self::Finished { manifest, .. } => Some(&manifest.song),
        }
    }

    pub fn manifest(&self) -> Option<&MatchManifest> {
        match self {
            Self::Lobby | Self::Preparing { .. } => None,
            Self::Countdown { manifest, .. }
            | Self::Playing { manifest, .. }
            | Self::Finalizing { manifest, .. }
            | Self::Finished { manifest, .. } => Some(manifest),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockQuality {
    pub accepted_samples: u16,
    pub p95_rtt_ms: u32,
    pub jitter_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationProof {
    pub source_id: ContentHash,
    pub canonical_chart_hash: ContentHash,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub audio_id: Option<ContentHash>,
    pub semantics: MatchSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PlayerPreparation {
    Selecting,
    Downloading {
        selection: PlayerSelection,
        progress_milli: ProgressMilli,
    },
    Verifying {
        selection: PlayerSelection,
    },
    Loading {
        selection: PlayerSelection,
    },
    Prepared {
        selection: PlayerSelection,
    },
    Ready {
        selection: PlayerSelection,
    },
    Failed {
        selection: Option<PlayerSelection>,
        reason: ErrorMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PlayerConnection {
    Online,
    Reconnecting { grace_deadline_server_us: u64 },
    Dnf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerSnapshot {
    pub player_id: PlayerId,
    pub name: DisplayName,
    pub is_leader: bool,
    pub connection: PlayerConnection,
    pub preparation: PlayerPreparation,
    pub last_acked_input_seq: Option<InputSeq>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpectatorSnapshot {
    pub spectator_id: SpectatorId,
    pub name: DisplayName,
    pub connection: PlayerConnection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreSnapshot {
    pub score: u32,
    pub combo: u32,
    pub max_combo: u32,
    pub gauge_ppm: u32,
    pub pass_threshold_ppm: u32,
    pub great: u32,
    pub ok: u32,
    pub miss: u32,
    pub roll_hits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerLiveState {
    pub player_id: PlayerId,
    pub score: ScoreSnapshot,
    pub finished: bool,
    pub dnf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveStateSnapshot {
    pub match_id: MatchId,
    pub state_seq: StateSeq,
    pub server_tick: Tick,
    pub players: BoundedVec<PlayerLiveState, MAX_PLAYERS>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinalResult {
    pub player_id: PlayerId,
    pub course_id: CourseId,
    pub score: ScoreSnapshot,
    pub finish_tick: Tick,
    pub passed: bool,
    pub replay_digest: ContentHash,
    pub dnf: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "stage",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum RoomStage {
    Lobby,
    Preparing {
        match_id: MatchId,
        song: SongManifest,
    },
    Countdown {
        manifest: MatchManifest,
        start_at_server_us: u64,
    },
    Playing {
        manifest: MatchManifest,
        start_at_server_us: u64,
        server_tick: Tick,
    },
    Finalizing {
        manifest: MatchManifest,
        server_tick: Tick,
        deadline_server_us: u64,
    },
    Finished {
        manifest: MatchManifest,
        results: BoundedVec<FinalResult, MAX_PLAYERS>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomSnapshot {
    pub room_code: RoomCode,
    pub revision: RoomRevision,
    pub server_now_us: u64,
    pub leader_player_id: PlayerId,
    pub players: BoundedVec<PlayerSnapshot, MAX_PLAYERS>,
    pub spectators: BoundedVec<SpectatorSnapshot, MAX_SPECTATORS>,
    pub stage: RoomStage,
}

impl RoomSnapshot {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        if self.revision.0 == 0 {
            return Err(ProtocolInvariantError::ZeroRoomRevision);
        }
        if self.players.is_empty() {
            return Err(ProtocolInvariantError::RoomHasNoPlayers);
        }

        let mut player_ids = HashSet::with_capacity(self.players.len());
        let mut declared_leader_count = 0;
        for player in &self.players {
            if !player_ids.insert(player.player_id) {
                return Err(ProtocolInvariantError::DuplicatePlayer(player.player_id));
            }
            if player
                .last_acked_input_seq
                .is_some_and(|sequence| sequence.0 < FIRST_INPUT_SEQ.0)
            {
                return Err(ProtocolInvariantError::ZeroPlayerInputWatermark(
                    player.player_id,
                ));
            }
            if player.is_leader {
                declared_leader_count += 1;
                if player.player_id != self.leader_player_id {
                    return Err(ProtocolInvariantError::InvalidRoomLeader);
                }
            }
        }
        if declared_leader_count != 1 || !player_ids.contains(&self.leader_player_id) {
            return Err(ProtocolInvariantError::InvalidRoomLeader);
        }

        let mut spectator_ids = HashSet::with_capacity(self.spectators.len());
        for spectator in &self.spectators {
            if !spectator_ids.insert(spectator.spectator_id) {
                return Err(ProtocolInvariantError::DuplicateSpectator(
                    spectator.spectator_id,
                ));
            }
        }

        match &self.stage {
            RoomStage::Lobby => {}
            RoomStage::Preparing { match_id, song } => {
                if match_id.0 == 0 {
                    return Err(ProtocolInvariantError::ZeroMatchId);
                }
                song.validate()?;
                self.validate_preparing_selections(song)?;
            }
            RoomStage::Countdown { manifest, .. } => {
                manifest.validate()?;
                self.validate_active_assignments(manifest)?;
            }
            RoomStage::Playing {
                manifest,
                server_tick,
                ..
            }
            | RoomStage::Finalizing {
                manifest,
                server_tick,
                ..
            } => {
                if *server_tick < 0 {
                    return Err(ProtocolInvariantError::NegativeServerTick);
                }
                manifest.validate()?;
                self.validate_active_assignments(manifest)?;
            }
            RoomStage::Finished { manifest, results } => {
                manifest.validate()?;
                self.validate_active_assignments(manifest)?;
                validate_finished_results(manifest, results)?;
            }
        }
        Ok(())
    }

    fn validate_preparing_selections(
        &self,
        song: &SongManifest,
    ) -> Result<(), ProtocolInvariantError> {
        for player in &self.players {
            let Some(selection) = player.preparation.selection() else {
                continue;
            };
            if !song
                .courses
                .iter()
                .any(|course| course.course_id == selection.course_id)
            {
                return Err(ProtocolInvariantError::UnknownPreparedCourse {
                    player_id: player.player_id,
                    course_id: selection.course_id,
                });
            }
        }
        Ok(())
    }

    fn validate_active_assignments(
        &self,
        manifest: &MatchManifest,
    ) -> Result<(), ProtocolInvariantError> {
        let assignment_ids = manifest
            .assignments
            .iter()
            .map(|assignment| assignment.player_id)
            .collect::<HashSet<_>>();
        if assignment_ids != self.players.iter().map(|player| player.player_id).collect() {
            return Err(ProtocolInvariantError::ActivePlayerAssignmentMismatch);
        }

        for player in &self.players {
            let assignment = manifest
                .assignments
                .iter()
                .find(|assignment| assignment.player_id == player.player_id)
                .expect("set equality guarantees an assignment");
            if !player.preparation.is_ready()
                || player.preparation.selection() != Some(&assignment.selection)
            {
                return Err(ProtocolInvariantError::ActivePreparationMismatch(
                    player.player_id,
                ));
            }
        }
        Ok(())
    }
}

impl ScoreSnapshot {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        if self.gauge_ppm > 1_000_000
            || self.pass_threshold_ppm > 1_000_000
            || self.combo > self.max_combo
            || self.max_combo > self.great.saturating_add(self.ok)
        {
            return Err(ProtocolInvariantError::InvalidScore);
        }
        Ok(())
    }
}

impl LiveStateSnapshot {
    pub fn validate_for(&self, manifest: &MatchManifest) -> Result<(), ProtocolInvariantError> {
        manifest.validate()?;
        if self.match_id != manifest.match_id {
            return Err(ProtocolInvariantError::LivePlayerMismatch);
        }
        if self.state_seq.0 == 0 {
            return Err(ProtocolInvariantError::ZeroStateSequence);
        }
        if self.server_tick < 0 {
            return Err(ProtocolInvariantError::NegativeServerTick);
        }
        let assigned = manifest
            .assignments
            .iter()
            .map(|assignment| assignment.player_id)
            .collect::<HashSet<_>>();
        let mut live_players = HashSet::with_capacity(self.players.len());
        for player in &self.players {
            if !live_players.insert(player.player_id) {
                return Err(ProtocolInvariantError::DuplicateLivePlayer(
                    player.player_id,
                ));
            }
            player.score.validate()?;
            if player.dnf && !player.finished {
                return Err(ProtocolInvariantError::DnfPlayerNotFinished);
            }
        }
        if live_players != assigned {
            return Err(ProtocolInvariantError::LivePlayerMismatch);
        }
        Ok(())
    }
}

impl InputAck {
    pub fn validate(&self) -> Result<(), ProtocolInvariantError> {
        if self.server_tick < 0 {
            return Err(ProtocolInvariantError::InvalidInputAcknowledgement);
        }
        match self.highest_contiguous_seq {
            None if self.next_expected_seq == FIRST_INPUT_SEQ => Ok(()),
            Some(highest)
                if highest.0 >= FIRST_INPUT_SEQ.0
                    && highest.0 < u64::MAX
                    && self.next_expected_seq.0 == highest.0 + 1 =>
            {
                Ok(())
            }
            _ => Err(ProtocolInvariantError::InvalidInputAcknowledgement),
        }
    }
}

fn validate_finished_results(
    manifest: &MatchManifest,
    results: &BoundedVec<FinalResult, MAX_PLAYERS>,
) -> Result<(), ProtocolInvariantError> {
    if results.len() != manifest.assignments.len() {
        return Err(ProtocolInvariantError::FinishedResultMismatch);
    }
    let mut result_players = HashSet::with_capacity(results.len());
    for result in results {
        if !result_players.insert(result.player_id) {
            return Err(ProtocolInvariantError::FinishedResultMismatch);
        }
        let Some(assignment) = manifest
            .assignments
            .iter()
            .find(|assignment| assignment.player_id == result.player_id)
        else {
            return Err(ProtocolInvariantError::FinishedResultMismatch);
        };
        if assignment.selection.course_id != result.course_id {
            return Err(ProtocolInvariantError::FinishedResultMismatch);
        }
        result.score.validate()?;
        let expected_passed =
            !result.dnf && result.score.gauge_ppm >= result.score.pass_threshold_ppm;
        if result.finish_tick < 0 || result.passed != expected_passed {
            return Err(ProtocolInvariantError::InvalidFinalResult);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResumeRequest {
    pub room_code: RoomCode,
    pub actor_id: ActorId,
    pub token: ResumeToken,
    pub last_room_revision: RoomRevision,
    pub last_acked_command_seq: CommandSeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientHello {
    pub protocol_version: u32,
    pub wire_schema_sha256: ContentHash,
    pub client_build: ClientBuild,
    pub display_name: DisplayName,
    pub resume: Option<ResumeRequest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub seq: CommandSeq,
    pub expected_room_revision: Option<RoomRevision>,
    pub command: ClientCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "command",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ClientCommand {
    CreateRoom,
    JoinRoom {
        room_code: RoomCode,
        invitation_token: InvitationToken,
        role: JoinRole,
    },
    LeaveRoom,
    SelectSong {
        song_id: SongId,
    },
    SelectCourse {
        match_id: MatchId,
        selection: PlayerSelection,
    },
    ReportPreparation {
        match_id: MatchId,
        progress: PreparationProgress,
    },
    SetReady {
        match_id: MatchId,
        ready: bool,
        proof: Option<PreparationProof>,
    },
    StartMatch {
        match_id: MatchId,
    },
    Rematch {
        previous_match_id: MatchId,
    },
    ReturnToLobby {
        match_id: MatchId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PreparationProgress {
    Downloading {
        selection: PlayerSelection,
        progress_milli: ProgressMilli,
    },
    Verifying {
        selection: PlayerSelection,
    },
    Loading {
        selection: PlayerSelection,
    },
    Failed {
        selection: Option<PlayerSelection>,
        reason: ErrorMessage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvent {
    pub seq: InputSeq,
    pub tick: Tick,
    pub action: DrumAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Physical half of the drum. This remains part of authoritative replay data.
pub enum DrumSide {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Drum zone used by the authoritative taiko judge.
pub enum DrumZone {
    Don,
    Kat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Strict wire representation of one physical drum strike.
pub struct DrumAction {
    pub side: DrumSide,
    pub zone: DrumZone,
}

impl DrumAction {
    pub const LEFT_DON: Self = Self::new(DrumSide::Left, DrumZone::Don);
    pub const RIGHT_DON: Self = Self::new(DrumSide::Right, DrumZone::Don);
    pub const LEFT_KAT: Self = Self::new(DrumSide::Left, DrumZone::Kat);
    pub const RIGHT_KAT: Self = Self::new(DrumSide::Right, DrumZone::Kat);

    pub const fn new(side: DrumSide, zone: DrumZone) -> Self {
        Self { side, zone }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputBatch {
    pub match_id: MatchId,
    pub events: BoundedVec<InputEvent, MAX_INPUT_BATCH_EVENTS>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeSyncRequest {
    pub nonce: u64,
    pub client_send_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeSyncReceipt {
    pub nonce: u64,
    pub probe_token: ClockProbeToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Heartbeat {
    pub nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ClientMessage {
    Hello(ClientHello),
    Command(CommandEnvelope),
    Input(InputBatch),
    TimeSync(TimeSyncRequest),
    TimeSyncReceipt(TimeSyncReceipt),
    Heartbeat(Heartbeat),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerWelcome {
    pub protocol_version: u32,
    pub wire_schema_sha256: ContentHash,
    pub heartbeat_interval_ms: u32,
    pub reconnect_grace_ms: u32,
    pub resumed: bool,
    pub next_expected_command_seq: CommandSeq,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipGranted {
    pub room_code: RoomCode,
    pub actor_id: ActorId,
    pub resume_token: ResumeToken,
    pub invitation_token: InvitationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolErrorCode {
    UnsupportedProtocol,
    InvalidMessage,
    InvalidName,
    InvalidRoomCode,
    InvalidInvitation,
    RoomNotFound,
    RoomFull,
    SpectatorFull,
    AlreadyMember,
    NotMember,
    PermissionDenied,
    InvalidStage,
    StaleRevision,
    StaleMatch,
    InvalidCourse,
    NotPrepared,
    ClockNotReady,
    SequenceGap,
    InvalidInput,
    RateLimited,
    SlowConsumer,
    ResumeRejected,
    SessionExpired,
    SessionSuperseded,
    RoomClosed,
    ServerBusy,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: ErrorMessage,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum CommandOutcome {
    Applied {
        room_revision: Option<RoomRevision>,
    },
    Rejected {
        error: ProtocolError,
        current_room_revision: Option<RoomRevision>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandAck {
    pub seq: CommandSeq,
    pub next_expected_seq: CommandSeq,
    pub outcome: CommandOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum InputOutcome {
    Accepted,
    Rejected { error: ProtocolError },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputAck {
    pub match_id: MatchId,
    pub highest_contiguous_seq: Option<InputSeq>,
    pub next_expected_seq: InputSeq,
    pub server_tick: Tick,
    pub outcome: InputOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeSyncResponse {
    pub nonce: u64,
    pub client_send_us: u64,
    pub server_receive_us: u64,
    pub server_send_us: u64,
    pub probe_token: ClockProbeToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockProbeAck {
    pub nonce: u64,
    pub quality: ClockQuality,
    pub ready: bool,
    pub valid_until_server_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatAck {
    pub nonce: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ServerMessage {
    Welcome(ServerWelcome),
    MembershipGranted(MembershipGranted),
    CommandAck(CommandAck),
    RoomSnapshot(Box<RoomSnapshot>),
    LiveState(LiveStateSnapshot),
    InputAck(InputAck),
    TimeSync(TimeSyncResponse),
    ClockProbeAck(ClockProbeAck),
    HeartbeatAck(HeartbeatAck),
    Fatal(ProtocolError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(byte: char) -> ContentHash {
        ContentHash::parse(byte.to_string().repeat(64)).expect("valid hash")
    }

    fn hello() -> ClientMessage {
        ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            wire_schema_sha256: ContentHash::parse(WIRE_SCHEMA_SHA256).expect("schema hash"),
            client_build: ClientBuild::new("taiko-test").expect("client build"),
            display_name: DisplayName::new("alice").expect("display name"),
            resume: None,
        })
    }

    #[test]
    fn hello_has_stable_golden_json() {
        let raw = serde_json::to_string(&hello()).expect("serialize hello");
        assert_eq!(
            raw,
            r#"{"type":"hello","payload":{"protocol_version":2,"wire_schema_sha256":"0792569ac7ed14846ad8bfcff1b7568b917505b34997df668db641925f437257","client_build":"taiko-test","display_name":"alice","resume":null}}"#
        );
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&raw).expect("deserialize hello"),
            hello()
        );
    }

    #[test]
    fn command_has_stable_golden_json() {
        let message = ClientMessage::Command(CommandEnvelope {
            seq: CommandSeq(7),
            expected_room_revision: Some(RoomRevision(12)),
            command: ClientCommand::SelectCourse {
                match_id: MatchId(3),
                selection: PlayerSelection {
                    course_id: CourseId(4),
                },
            },
        });
        let raw = serde_json::to_string(&message).expect("serialize command");
        assert_eq!(
            raw,
            r#"{"type":"command","payload":{"seq":7,"expected_room_revision":12,"command":{"command":"select_course","data":{"match_id":3,"selection":{"course_id":4}}}}}"#
        );
        assert_eq!(
            serde_json::from_str::<ClientMessage>(&raw).expect("deserialize command"),
            message
        );
    }

    #[test]
    fn unknown_struct_fields_are_rejected() {
        let raw = r#"{"type":"hello","payload":{"protocol_version":2,"wire_schema_sha256":"0792569ac7ed14846ad8bfcff1b7568b917505b34997df668db641925f437257","client_build":"x","display_name":"alice","resume":null,"legacy":true}}"#;
        assert!(serde_json::from_str::<ClientMessage>(raw).is_err());
    }

    #[test]
    fn previous_v1_wire_shape_is_rejected() {
        let legacy = r#"{"type":"create_room"}"#;
        assert!(serde_json::from_str::<ClientMessage>(legacy).is_err());
    }

    #[test]
    fn room_stage_round_trip_preserves_match_epoch() {
        let mut song = SongManifest {
            song_id: SongId::parse("9".repeat(64)).expect("song id"),
            source_id: hash('a'),
            audio_id: Some(hash('b')),
            title: DisplayTitle::new("Song").expect("title"),
            subtitle: BoundedText::new("").expect("subtitle"),
            artist: BoundedText::new("Artist").expect("artist"),
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
                level: Some(9),
                canonical_chart_hash: hash('f'),
            }])
            .expect("course bound"),
        };
        song.song_id = song.derive_song_id().expect("derived song id");
        let stage = RoomStage::Preparing {
            match_id: MatchId(42),
            song,
        };
        let raw = serde_json::to_vec(&stage).expect("serialize stage");
        let decoded: RoomStage = serde_json::from_slice(&raw).expect("deserialize stage");
        assert_eq!(decoded, stage);
    }
}
