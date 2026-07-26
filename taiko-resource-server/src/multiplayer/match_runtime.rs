use std::collections::{BTreeMap, VecDeque};
use std::time::Instant;

use rhythm_core::{StepError, TimedInput};
use rhythm_mode_taiko::{
    ScheduledTaikoInput, TaikoAction, TaikoBranchError, TaikoBranchPolicy, TaikoMode, TaikoRuntime,
    TaikoRuntimeBuildError, TaikoScoreState, TaikoSide, TaikoZone,
};
use sha2::{Digest, Sha256};
use taiko_multiplayer_protocol::{
    BoundedVec, ContentHash, CourseId, DrumAction, DrumSide, DrumZone, FinalResult, InputBatch,
    InputEvent, InputSeq, LiveStateSnapshot, MatchId, MatchManifest, PlayerCourseAssignment,
    PlayerId, PlayerLiveState, ProtocolInvariantError, ScoreSnapshot, StateSeq, Tick,
    FIRST_INPUT_SEQ, MAX_INPUT_BURST_EVENTS, MAX_INPUT_EVENTS_PER_SECOND, MAX_PLAYERS,
};
use thiserror::Error;

use super::limits::MATCH_TTL;
use crate::{AuthoritativeCatalog, AuthoritativeCourse, AuthoritativeSong};

const REPLAY_DIGEST_DOMAIN: &[u8] = b"taiko-server-authoritative-replay/v2\0";

/// Clock-estimation error tolerated ahead of the server receive time.
///
/// Inputs remain uncommitted until their timestamp passes the independent
/// lateness watermark. This bound only rejects impossible or badly
/// synchronized client timestamps before they consume the room's input budget.
pub(crate) const MAX_INPUT_FUTURE_TICKS: Tick = 250_000;

/// A hard per-player match budget for a fifteen-minute match at the
/// authoritative 50-inputs/second ceiling plus the initial burst allowance.
pub(crate) const MAX_PROCESSED_INPUT_EVENTS: usize =
    (MATCH_TTL.as_secs() * MAX_INPUT_EVENTS_PER_SECOND + MAX_INPUT_BURST_EVENTS) as usize;

/// Authoritative physical-input envelope.
///
/// Four immediately adjacent strikes allow dual-key/controller bursts without
/// granting an unbounded same-tick roll score. After that burst, credit refills
/// at 50 strikes per second. Validation is based on signed match timestamps,
/// not packet arrival cadence, and is committed atomically with the batch.
pub(crate) const INPUT_RATE_INTERVAL_TICKS: Tick = 1_000_000 / MAX_INPUT_EVENTS_PER_SECOND as Tick;
const MAX_INPUT_RATE_CREDIT_TICKS: Tick =
    MAX_INPUT_BURST_EVENTS as Tick * INPUT_RATE_INTERVAL_TICKS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InputAcceptance {
    pub(crate) highest_contiguous_seq: Option<InputSeq>,
    pub(crate) next_expected_seq: InputSeq,
    pub(crate) server_tick: Tick,
    pub(crate) newly_accepted: usize,
    pub(crate) first_dropped_reason: Option<InputDropReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputDropReason {
    Late,
    Future,
    RateLimited,
    PlayerFinished,
}

#[derive(Debug, Error)]
pub(crate) enum MatchRuntimeError {
    #[error("invalid match manifest: {0}")]
    InvalidManifest(#[from] ProtocolInvariantError),
    #[error("authoritative catalog does not contain song {0}")]
    UnknownSong(String),
    #[error("match manifest field {field} does not match the authoritative catalog")]
    ManifestResourceMismatch { field: &'static str },
    #[error("player {player_id:?} references unknown authoritative course {course_id:?}")]
    UnknownCourse {
        player_id: PlayerId,
        course_id: CourseId,
    },
    #[error("player {player_id:?} course {course_id:?} chart hash does not match the catalog")]
    CourseHashMismatch {
        player_id: PlayerId,
        course_id: CourseId,
    },
    #[error("player {player_id:?} cannot initialize authoritative automatic branching: {source}")]
    AutomaticBranching {
        player_id: PlayerId,
        #[source]
        source: TaikoBranchError,
    },
    #[error("player {player_id:?} has a negative branch decision tick {tick}")]
    NegativeBranchDecision { player_id: PlayerId, tick: Tick },
    #[error("player {player_id:?} chart cannot compile: {source}")]
    ChartCompile {
        player_id: PlayerId,
        #[source]
        source: rhythm_core::CompileError,
    },
    #[error("server monotonic clock precedes the match start")]
    MatchNotStarted,
    #[error("server tick exceeds the protocol tick range")]
    ClockOverflow,
    #[error("input references match {actual:?}; active match is {expected:?}")]
    WrongMatch { expected: MatchId, actual: MatchId },
    #[error("player {0:?} is not assigned to this match")]
    UnknownPlayer(PlayerId),
    #[error("input batch must contain at least one event")]
    EmptyInputBatch,
    #[error("input sequence must start at {FIRST_INPUT_SEQ:?}")]
    InputSequenceBeforeStart,
    #[error("input sequence cannot advance beyond u64::MAX")]
    InputSequenceExhausted,
    #[error("input batch is not strictly contiguous: expected {expected:?}, got {actual:?}")]
    NonContiguousBatch {
        expected: InputSeq,
        actual: InputSeq,
    },
    #[error("input sequence gap: expected {expected:?}, got {actual:?}")]
    SequenceGap {
        expected: InputSeq,
        actual: InputSeq,
    },
    #[error("replayed input {seq:?} differs from the accepted event")]
    ConflictingDuplicate { seq: InputSeq },
    #[error("input tick must be non-negative, got {tick}")]
    NegativeInputTick { tick: Tick },
    #[error("input tick regressed from {previous} to {actual}")]
    NonMonotonicInputTick { previous: Tick, actual: Tick },
    #[error("input tick {tick} is older than committed server tick {committed_tick}")]
    LateInput { tick: Tick, committed_tick: Tick },
    #[error("input tick {tick} exceeds maximum accepted future tick {maximum_tick}")]
    FutureInput { tick: Tick, maximum_tick: Tick },
    #[error("player {0:?} has already finished")]
    PlayerFinished(PlayerId),
    #[error("player {0:?} exceeded the authoritative input budget")]
    InputBudgetExceeded(PlayerId),
    #[error("input at tick {tick} exceeds the authoritative physical input rate")]
    InputRateExceeded { tick: Tick },
    #[error("player {player_id:?} engine step failed: {source}")]
    EngineStep {
        player_id: PlayerId,
        #[source]
        source: StepError,
    },
    #[error("live-state sequence exhausted")]
    StateSequenceExhausted,
    #[error("final results requested before every player finished or became DNF")]
    MatchNotFinished,
    #[error("score state for player {0:?} contains a non-finite gauge value")]
    NonFiniteScore(PlayerId),
    #[error("score state for player {player_id:?} has out-of-range fraction {value}")]
    ScoreFractionOutOfRange { player_id: PlayerId, value: f32 },
    #[error("failed to encode the replay digest")]
    InvalidReplayDigest,
}

pub(crate) struct AuthoritativeMatchRuntime {
    manifest: MatchManifest,
    start_at: Instant,
    input_lateness_ticks: Tick,
    players: BTreeMap<PlayerId, PlayerRuntime>,
    next_state_seq: StateSeq,
}

struct PlayerRuntime {
    runtime: TaikoRuntime,
    processed: Vec<ProcessedInput>,
    pending: VecDeque<InputEvent>,
    replay_hasher: Sha256,
    input_rate: InputRateState,
    finished_at: Option<Tick>,
    dnf: bool,
}

#[derive(Debug, Clone, Copy)]
struct ProcessedInput {
    event: InputEvent,
    dropped_reason: Option<InputDropReason>,
}

#[derive(Debug, Clone, Copy)]
struct InputRateState {
    credit_ticks: Tick,
    last_tick: Option<Tick>,
}

impl Default for InputRateState {
    fn default() -> Self {
        Self {
            credit_ticks: MAX_INPUT_RATE_CREDIT_TICKS,
            last_tick: None,
        }
    }
}

impl AuthoritativeMatchRuntime {
    pub(crate) fn new(
        manifest: MatchManifest,
        catalog: AuthoritativeCatalog,
        start_at: Instant,
    ) -> Result<Self, MatchRuntimeError> {
        manifest.validate()?;
        validate_catalog_identity(&manifest, &catalog)?;

        let song = catalog
            .song(manifest.song.song_id.as_str())
            .ok_or_else(|| MatchRuntimeError::UnknownSong(manifest.song.song_id.to_string()))?;
        let mut players = BTreeMap::new();

        for assignment in &manifest.assignments {
            let course = authoritative_course(song, assignment)?;
            if let Some(decision) = course
                .branch_decisions()
                .iter()
                .find(|decision| decision.decision_tick < 0)
            {
                return Err(MatchRuntimeError::NegativeBranchDecision {
                    player_id: assignment.player_id,
                    tick: decision.decision_tick,
                });
            }
            let runtime = TaikoRuntime::new(
                course.chart(),
                TaikoBranchPolicy::Automatic,
                course.branch_decisions().to_vec(),
            )
            .map_err(|error| match error {
                TaikoRuntimeBuildError::Branch(source) => MatchRuntimeError::AutomaticBranching {
                    player_id: assignment.player_id,
                    source,
                },
                TaikoRuntimeBuildError::Compile(source) => MatchRuntimeError::ChartCompile {
                    player_id: assignment.player_id,
                    source,
                },
            })?;

            players.insert(
                assignment.player_id,
                PlayerRuntime {
                    runtime,
                    processed: Vec::new(),
                    pending: VecDeque::new(),
                    replay_hasher: replay_hasher(&manifest, assignment),
                    input_rate: InputRateState::default(),
                    finished_at: None,
                    dnf: false,
                },
            );
        }

        Ok(Self {
            input_lateness_ticks: Tick::from(manifest.input_lateness_ms) * 1_000,
            manifest,
            start_at,
            players,
            next_state_seq: StateSeq(1),
        })
    }

    pub(crate) fn current_server_tick(&self, now: Instant) -> Result<Tick, MatchRuntimeError> {
        let elapsed = now
            .checked_duration_since(self.start_at)
            .ok_or(MatchRuntimeError::MatchNotStarted)?;
        Tick::try_from(elapsed.as_micros()).map_err(|_| MatchRuntimeError::ClockOverflow)
    }

    pub(crate) fn accept_input(
        &mut self,
        player_id: PlayerId,
        batch: &InputBatch,
        now: Instant,
    ) -> Result<InputAcceptance, MatchRuntimeError> {
        if batch.match_id != self.manifest.match_id {
            return Err(MatchRuntimeError::WrongMatch {
                expected: self.manifest.match_id,
                actual: batch.match_id,
            });
        }
        if batch.events.is_empty() {
            return Err(MatchRuntimeError::EmptyInputBatch);
        }
        let server_tick = self.current_server_tick(now)?;
        let player = self
            .players
            .get(&player_id)
            .ok_or(MatchRuntimeError::UnknownPlayer(player_id))?;
        let validated = validate_input_batch(player_id, player, batch, server_tick)?;

        let player = self
            .players
            .get_mut(&player_id)
            .expect("player existence was established without yielding");
        for event in validated.events.iter().copied() {
            hash_input(&mut player.replay_hasher, event);
            player.processed.push(ProcessedInput {
                event,
                dropped_reason: None,
            });
            player.pending.push_back(event);
        }
        player.input_rate = validated.input_rate;

        Ok(InputAcceptance {
            highest_contiguous_seq: player.processed.last().map(|input| input.event.seq),
            next_expected_seq: next_input_seq(&player.processed)?,
            server_tick,
            newly_accepted: validated.events.len(),
            first_dropped_reason: validated.first_replayed_drop,
        })
    }

    /// Consumes a structurally valid contiguous batch without scoring it.
    ///
    /// Recoverable timeline policy failures (late, too-far-ahead, physical
    /// rate, or input after the player has finished) must advance the input
    /// watermark. Otherwise an ACK lost across reconnect leaves the client
    /// permanently stuck retrying the same unscorable sequence.
    pub(crate) fn discard_input(
        &mut self,
        player_id: PlayerId,
        batch: &InputBatch,
        now: Instant,
        reason: InputDropReason,
    ) -> Result<InputAcceptance, MatchRuntimeError> {
        if batch.match_id != self.manifest.match_id {
            return Err(MatchRuntimeError::WrongMatch {
                expected: self.manifest.match_id,
                actual: batch.match_id,
            });
        }
        if batch.events.is_empty() {
            return Err(MatchRuntimeError::EmptyInputBatch);
        }
        let server_tick = self.current_server_tick(now)?;
        let player = self
            .players
            .get(&player_id)
            .ok_or(MatchRuntimeError::UnknownPlayer(player_id))?;
        let validated = validate_input_sequence(player_id, player, batch)?;

        let player = self
            .players
            .get_mut(&player_id)
            .expect("player existence was established without yielding");
        for event in validated.events.iter().copied() {
            player.processed.push(ProcessedInput {
                event,
                dropped_reason: Some(reason),
            });
        }
        let first_dropped_reason = if validated.events.is_empty() {
            validated.first_replayed_drop
        } else {
            validated.first_replayed_drop.or(Some(reason))
        };
        Ok(InputAcceptance {
            highest_contiguous_seq: player.processed.last().map(|input| input.event.seq),
            next_expected_seq: next_input_seq(&player.processed)?,
            server_tick,
            newly_accepted: 0,
            first_dropped_reason,
        })
    }

    /// Advances authoritative engines for input-lateness validation without
    /// producing or publishing an extra live-state frame.
    pub(crate) fn synchronize(&mut self, now: Instant) -> Result<(), MatchRuntimeError> {
        self.advance_players_to_watermark(now).map(|_| ())
    }

    /// Advances every engine to the immutable lateness watermark and emits one
    /// latest-value snapshot. Callers can safely coalesce these snapshots.
    pub(crate) fn advance(&mut self, now: Instant) -> Result<LiveStateSnapshot, MatchRuntimeError> {
        let watermark = self.advance_players_to_watermark(now)?;

        let players = self
            .players
            .iter()
            .map(|(player_id, player)| {
                Ok(PlayerLiveState {
                    player_id: *player_id,
                    score: score_snapshot(*player_id, player.runtime.score())?,
                    finished: player.finished_at.is_some(),
                    dnf: player.dnf,
                })
            })
            .collect::<Result<Vec<_>, MatchRuntimeError>>()?;
        let players = BoundedVec::<_, MAX_PLAYERS>::new(players)
            .expect("validated match player count fits the protocol bound");
        let state_seq = self.next_state_seq;
        self.next_state_seq = StateSeq(
            state_seq
                .0
                .checked_add(1)
                .ok_or(MatchRuntimeError::StateSequenceExhausted)?,
        );

        Ok(LiveStateSnapshot {
            match_id: self.manifest.match_id,
            state_seq,
            server_tick: watermark,
            players,
        })
    }

    fn advance_players_to_watermark(&mut self, now: Instant) -> Result<Tick, MatchRuntimeError> {
        let current_tick = self.current_server_tick(now)?;
        let watermark = current_tick
            .saturating_sub(self.input_lateness_ticks)
            .max(0);
        for (player_id, player) in &mut self.players {
            advance_player(*player_id, player, watermark)?;
        }
        Ok(watermark)
    }

    pub(crate) fn mark_dnf(
        &mut self,
        player_id: PlayerId,
        now: Instant,
    ) -> Result<bool, MatchRuntimeError> {
        let current_tick = self.current_server_tick(now)?;
        let watermark = current_tick
            .saturating_sub(self.input_lateness_ticks)
            .max(0);
        let player = self
            .players
            .get_mut(&player_id)
            .ok_or(MatchRuntimeError::UnknownPlayer(player_id))?;
        if player.dnf {
            return Ok(false);
        }
        if player.finished_at.is_some() {
            return Ok(false);
        }

        let drain_tick = player
            .pending
            .back()
            .map_or(watermark, |event| watermark.max(event.tick));
        advance_player(player_id, player, drain_tick)?;
        if player.finished_at.is_none() {
            debug_assert!(
                player.pending.is_empty(),
                "all accepted pending input must be drained before DNF"
            );
            player.finished_at = Some(drain_tick);
            player.dnf = true;
            return Ok(true);
        }
        Ok(false)
    }

    pub(crate) fn all_finished(&self) -> bool {
        self.players
            .values()
            .all(|player| player.finished_at.is_some())
    }

    pub(crate) fn final_results(
        &self,
    ) -> Result<BoundedVec<FinalResult, MAX_PLAYERS>, MatchRuntimeError> {
        if !self.all_finished() {
            return Err(MatchRuntimeError::MatchNotFinished);
        }
        let mut results = Vec::with_capacity(self.manifest.assignments.len());
        for assignment in &self.manifest.assignments {
            let player = self
                .players
                .get(&assignment.player_id)
                .expect("validated assignments and runtime players are identical");
            let final_score = player.runtime.finalize();
            let replay_digest =
                ContentHash::parse(hex::encode(player.replay_hasher.clone().finalize()))
                    .map_err(|_| MatchRuntimeError::InvalidReplayDigest)?;
            results.push(FinalResult {
                player_id: assignment.player_id,
                course_id: assignment.selection.course_id,
                score: score_snapshot(assignment.player_id, player.runtime.score())?,
                finish_tick: player
                    .finished_at
                    .expect("all-finished precondition was checked"),
                passed: !player.dnf && final_score.passed,
                replay_digest,
                dnf: player.dnf,
            });
        }
        Ok(BoundedVec::new(results).expect("validated match player count fits the protocol bound"))
    }
}

fn validate_catalog_identity(
    manifest: &MatchManifest,
    catalog: &AuthoritativeCatalog,
) -> Result<(), MatchRuntimeError> {
    let semantics = catalog.semantics();
    let expected = &manifest.song.semantics;
    for (matches, field) in [
        (
            expected.canonical_schema_version == semantics.canonical_schema_version,
            "semantics.canonical_schema_version",
        ),
        (
            expected.canonical_schema_digest.as_str() == semantics.canonical_schema_sha256,
            "semantics.canonical_schema_digest",
        ),
        (
            expected.importer_semantics_version == semantics.importer_semantics_version,
            "semantics.importer_semantics_version",
        ),
        (
            expected.importer_semantics_digest.as_str() == semantics.importer_semantics_sha256,
            "semantics.importer_semantics_digest",
        ),
        (
            expected.ruleset_version == semantics.taiko_ruleset_version,
            "semantics.ruleset_version",
        ),
        (
            expected.ruleset_digest.as_str() == semantics.taiko_ruleset_sha256,
            "semantics.ruleset_digest",
        ),
        (
            expected.audio_decoder_semantics_version == semantics.audio_decoder_semantics_version,
            "semantics.audio_decoder_semantics_version",
        ),
        (
            expected.audio_decoder_semantics_digest.as_str()
                == semantics.audio_decoder_semantics_sha256,
            "semantics.audio_decoder_semantics_digest",
        ),
    ] {
        if !matches {
            return Err(MatchRuntimeError::ManifestResourceMismatch { field });
        }
    }

    let song = catalog
        .song(manifest.song.song_id.as_str())
        .ok_or_else(|| MatchRuntimeError::UnknownSong(manifest.song.song_id.to_string()))?;
    let resource = song.manifest();
    for (matches, field) in [
        (
            manifest.song.song_id.as_str() == resource.song_id,
            "song.song_id",
        ),
        (
            manifest.song.source_id.as_str() == resource.source_id,
            "song.source_id",
        ),
        (
            manifest.song.audio_id.as_ref().map(ContentHash::as_str)
                == resource.audio_id.as_deref(),
            "song.audio_id",
        ),
        (manifest.song.title.as_str() == resource.title, "song.title"),
        (
            manifest.song.subtitle.as_str() == resource.subtitle,
            "song.subtitle",
        ),
        (
            manifest.song.artist.as_str() == resource.artist,
            "song.artist",
        ),
        (
            manifest.song.courses.len() == resource.courses.len(),
            "song.courses.length",
        ),
    ] {
        if !matches {
            return Err(MatchRuntimeError::ManifestResourceMismatch { field });
        }
    }
    for (course, resource_course) in manifest.song.courses.iter().zip(&resource.courses) {
        for (matches, field) in [
            (
                course.course_id.0 == resource_course.index,
                "song.courses.course_id",
            ),
            (
                course.name.as_str() == resource_course.name,
                "song.courses.name",
            ),
            (course.level == resource_course.level, "song.courses.level"),
            (
                course.canonical_chart_hash.as_str() == resource_course.canonical_chart_hash,
                "song.courses.canonical_chart_hash",
            ),
        ] {
            if !matches {
                return Err(MatchRuntimeError::ManifestResourceMismatch { field });
            }
        }
    }
    Ok(())
}

fn authoritative_course<'a>(
    song: &'a AuthoritativeSong,
    assignment: &PlayerCourseAssignment,
) -> Result<&'a AuthoritativeCourse, MatchRuntimeError> {
    let index = usize::try_from(assignment.selection.course_id.0).map_err(|_| {
        MatchRuntimeError::UnknownCourse {
            player_id: assignment.player_id,
            course_id: assignment.selection.course_id,
        }
    })?;
    let course = song
        .courses()
        .get(index)
        .filter(|course| course.manifest().index == assignment.selection.course_id.0)
        .ok_or(MatchRuntimeError::UnknownCourse {
            player_id: assignment.player_id,
            course_id: assignment.selection.course_id,
        })?;
    if course.manifest().canonical_chart_hash != assignment.canonical_chart_hash.as_str() {
        return Err(MatchRuntimeError::CourseHashMismatch {
            player_id: assignment.player_id,
            course_id: assignment.selection.course_id,
        });
    }
    Ok(course)
}

fn validate_input_batch(
    player_id: PlayerId,
    player: &PlayerRuntime,
    batch: &InputBatch,
    server_tick: Tick,
) -> Result<ValidatedInput, MatchRuntimeError> {
    let sequence = validate_input_sequence(player_id, player, batch)?;
    if player.finished_at.is_some() && !sequence.events.is_empty() {
        return Err(MatchRuntimeError::PlayerFinished(player_id));
    }
    let mut input_rate = player.input_rate;
    let maximum_tick = server_tick
        .checked_add(MAX_INPUT_FUTURE_TICKS)
        .ok_or(MatchRuntimeError::ClockOverflow)?;

    for event in &sequence.events {
        if event.tick < player.runtime.now() {
            return Err(MatchRuntimeError::LateInput {
                tick: event.tick,
                committed_tick: player.runtime.now(),
            });
        }
        if event.tick > maximum_tick {
            return Err(MatchRuntimeError::FutureInput {
                tick: event.tick,
                maximum_tick,
            });
        }
        consume_input_rate_credit(&mut input_rate, event.tick)?;
    }

    Ok(ValidatedInput {
        events: sequence.events,
        input_rate,
        first_replayed_drop: sequence.first_replayed_drop,
    })
}

fn validate_input_sequence(
    player_id: PlayerId,
    player: &PlayerRuntime,
    batch: &InputBatch,
) -> Result<ValidatedSequence, MatchRuntimeError> {
    let mut expected_in_batch = None;
    for event in &batch.events {
        if event.seq.0 < FIRST_INPUT_SEQ.0 {
            return Err(MatchRuntimeError::InputSequenceBeforeStart);
        }
        if event.seq.0 == u64::MAX {
            return Err(MatchRuntimeError::InputSequenceExhausted);
        }
        if let Some(expected) = expected_in_batch {
            if event.seq != expected {
                return Err(MatchRuntimeError::NonContiguousBatch {
                    expected,
                    actual: event.seq,
                });
            }
        }
        expected_in_batch = Some(InputSeq(
            event
                .seq
                .0
                .checked_add(1)
                .ok_or(MatchRuntimeError::InputSequenceExhausted)?,
        ));
    }

    let mut next = next_input_seq(&player.processed)?;
    let mut previous_tick = player.processed.last().map(|input| input.event.tick);
    let mut new_events = Vec::new();
    let mut first_replayed_drop = None;

    for event in &batch.events {
        if event.seq.0 < next.0 {
            let history_index = usize::try_from(event.seq.0 - FIRST_INPUT_SEQ.0)
                .expect("processed input budget fits usize");
            let Some(processed) = player.processed.get(history_index) else {
                return Err(MatchRuntimeError::ConflictingDuplicate { seq: event.seq });
            };
            if processed.event != *event {
                return Err(MatchRuntimeError::ConflictingDuplicate { seq: event.seq });
            }
            first_replayed_drop = first_replayed_drop.or(processed.dropped_reason);
            continue;
        }
        if event.seq != next {
            return Err(MatchRuntimeError::SequenceGap {
                expected: next,
                actual: event.seq,
            });
        }
        if event.tick < 0 {
            return Err(MatchRuntimeError::NegativeInputTick { tick: event.tick });
        }
        if let Some(previous) = previous_tick {
            if event.tick < previous {
                return Err(MatchRuntimeError::NonMonotonicInputTick {
                    previous,
                    actual: event.tick,
                });
            }
        }
        previous_tick = Some(event.tick);
        new_events.push(*event);
        next = InputSeq(
            next.0
                .checked_add(1)
                .ok_or(MatchRuntimeError::InputSequenceExhausted)?,
        );
    }

    if player
        .processed
        .len()
        .checked_add(new_events.len())
        .is_none_or(|count| count > MAX_PROCESSED_INPUT_EVENTS)
    {
        return Err(MatchRuntimeError::InputBudgetExceeded(player_id));
    }
    Ok(ValidatedSequence {
        events: new_events,
        first_replayed_drop,
    })
}

struct ValidatedInput {
    events: Vec<InputEvent>,
    input_rate: InputRateState,
    first_replayed_drop: Option<InputDropReason>,
}

struct ValidatedSequence {
    events: Vec<InputEvent>,
    first_replayed_drop: Option<InputDropReason>,
}

fn consume_input_rate_credit(
    state: &mut InputRateState,
    tick: Tick,
) -> Result<(), MatchRuntimeError> {
    if let Some(previous_tick) = state.last_tick {
        let elapsed = tick.saturating_sub(previous_tick);
        state.credit_ticks = state
            .credit_ticks
            .saturating_add(elapsed)
            .min(MAX_INPUT_RATE_CREDIT_TICKS);
    }
    state.last_tick = Some(tick);
    if state.credit_ticks < INPUT_RATE_INTERVAL_TICKS {
        return Err(MatchRuntimeError::InputRateExceeded { tick });
    }
    state.credit_ticks -= INPUT_RATE_INTERVAL_TICKS;
    Ok(())
}

fn next_input_seq(processed: &[ProcessedInput]) -> Result<InputSeq, MatchRuntimeError> {
    match processed.last() {
        None => Ok(FIRST_INPUT_SEQ),
        Some(input) => input
            .event
            .seq
            .0
            .checked_add(1)
            .map(InputSeq)
            .ok_or(MatchRuntimeError::InputSequenceExhausted),
    }
}

fn advance_player(
    player_id: PlayerId,
    player: &mut PlayerRuntime,
    watermark: Tick,
) -> Result<(), MatchRuntimeError> {
    if player.finished_at.is_some() {
        return Ok(());
    }
    let mut inputs = Vec::new();
    while player
        .pending
        .front()
        .is_some_and(|event| event.tick <= watermark)
    {
        let event = player
            .pending
            .pop_front()
            .expect("front existence was checked");
        inputs.push(ScheduledTaikoInput::unconditional(TimedInput {
            tick: event.tick,
            action: taiko_action(event.action),
        }));
    }
    let frame = player
        .runtime
        .advance_to(watermark, &inputs)
        .map_err(|source| MatchRuntimeError::EngineStep { player_id, source })?;
    record_finish(player, &frame);
    if player.finished_at.is_some() {
        player.pending.clear();
    }
    Ok(())
}

fn record_finish(player: &mut PlayerRuntime, frame: &rhythm_core::FrameOutput<TaikoMode>) {
    if frame.finished && player.finished_at.is_none() {
        player.finished_at = Some(frame.now);
    }
}

fn score_snapshot(
    player_id: PlayerId,
    score: &TaikoScoreState,
) -> Result<ScoreSnapshot, MatchRuntimeError> {
    Ok(ScoreSnapshot {
        score: score.score,
        combo: score.combo,
        max_combo: score.max_combo,
        gauge_ppm: fraction_to_ppm(player_id, score.gauge)?,
        pass_threshold_ppm: fraction_to_ppm(player_id, score.pass_threshold)?,
        great: score.great,
        ok: score.ok,
        miss: score.miss,
        roll_hits: score.roll_hits,
    })
}

fn fraction_to_ppm(player_id: PlayerId, value: f32) -> Result<u32, MatchRuntimeError> {
    if !value.is_finite() {
        return Err(MatchRuntimeError::NonFiniteScore(player_id));
    }
    if !(0.0..=1.0).contains(&value) {
        return Err(MatchRuntimeError::ScoreFractionOutOfRange { player_id, value });
    }
    Ok((value * 1_000_000.0).round() as u32)
}

fn replay_hasher(manifest: &MatchManifest, assignment: &PlayerCourseAssignment) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(REPLAY_DIGEST_DOMAIN);
    hash_u64(&mut hasher, manifest.match_id.0);
    hash_string(&mut hasher, manifest.song.song_id.as_str());
    let semantics = &manifest.song.semantics;
    hash_u32(&mut hasher, semantics.canonical_schema_version);
    hash_string(&mut hasher, semantics.canonical_schema_digest.as_str());
    hash_u32(&mut hasher, semantics.importer_semantics_version);
    hash_string(&mut hasher, semantics.importer_semantics_digest.as_str());
    hash_u32(&mut hasher, semantics.ruleset_version);
    hash_string(&mut hasher, semantics.ruleset_digest.as_str());
    hash_u32(&mut hasher, semantics.audio_decoder_semantics_version);
    hash_string(
        &mut hasher,
        semantics.audio_decoder_semantics_digest.as_str(),
    );
    hash_u64(&mut hasher, assignment.player_id.0);
    hash_u32(&mut hasher, assignment.selection.course_id.0);
    hash_string(&mut hasher, assignment.canonical_chart_hash.as_str());
    hasher
}

fn hash_input(hasher: &mut Sha256, event: InputEvent) {
    hasher.update([0x49]);
    hash_u64(hasher, event.seq.0);
    hasher.update(event.tick.to_be_bytes());
    hasher.update([match event.action.side {
        DrumSide::Left => 0,
        DrumSide::Right => 1,
    }]);
    hasher.update([match event.action.zone {
        DrumZone::Don => 0,
        DrumZone::Kat => 1,
    }]);
}

fn taiko_action(action: DrumAction) -> TaikoAction {
    TaikoAction::new(
        match action.side {
            DrumSide::Left => TaikoSide::Left,
            DrumSide::Right => TaikoSide::Right,
        },
        match action.zone {
            DrumZone::Don => TaikoZone::Don,
            DrumZone::Kat => TaikoZone::Kat,
        },
    )
}

fn hash_string(hasher: &mut Sha256, value: &str) {
    hash_u64(
        hasher,
        u64::try_from(value.len()).expect("bounded protocol string fits u64"),
    );
    hasher.update(value.as_bytes());
}

fn hash_u32(hasher: &mut Sha256, value: u32) {
    hasher.update(value.to_be_bytes());
}

fn hash_u64(hasher: &mut Sha256, value: u64) {
    hasher.update(value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use rhythm_chart::{
        BranchDecisionHint, BranchDecisionPoint, BranchSegment, CanonicalChart, ChartMetadata,
        Lane, LaneOrRegion, LaneRole, Object, ObjectKind, TempoChange, SCROLL_SCALE,
    };
    use taiko_multiplayer_protocol::{
        BoundedText, CourseManifest, CourseName, DisplayTitle, MatchSemantics, PlayerSelection,
        SongId, SongManifest, FIRST_MATCH_ID, MAX_COURSES_PER_SONG, MAX_INPUT_BATCH_EVENTS,
    };
    use taiko_resource_protocol::{
        canonical_chart_sha256, song_manifest_sha256, ResourceBranchDecisionPoint, ResourceCourse,
        ResourceSemantics, ResourceSong,
    };

    use super::*;

    fn hash(byte: u8) -> String {
        format!("{byte:02x}").repeat(32)
    }

    fn content_hash(byte: u8) -> ContentHash {
        ContentHash::parse(hash(byte)).expect("test digest")
    }

    fn chart(note_tick: Tick) -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata {
                title: "Runtime Test".to_owned(),
                difficulty_name: Some("Oni".to_owned()),
                difficulty_level: Some(1),
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

    fn branch_chart(note_tick: Tick) -> (CanonicalChart, Vec<BranchDecisionPoint>) {
        let mut chart = chart(note_tick);
        chart.branch_segments = vec![BranchSegment {
            id: 7,
            default_route_id: 0,
            route_count: 3,
            decision_hint: Some(BranchDecisionHint::Score { low: 1, high: 2 }),
        }];
        chart.objects = (0_u8..3)
            .map(|route_id| Object {
                id: u32::from(route_id) + 1,
                kind: ObjectKind::Tap,
                start_tick: note_tick,
                end_tick: note_tick,
                lane_or_region: LaneOrRegion::Lane(if route_id == 0 { 0 } else { 1 }),
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: SCROLL_SCALE,
                branch_segment_id: Some(7),
                branch_route_id: route_id,
            })
            .collect();
        let decisions = vec![BranchDecisionPoint {
            segment_id: 7,
            decision_tick: 100_000,
            default_route_id: 0,
            route_count: 3,
            hint: chart.branch_segments[0].decision_hint.clone(),
        }];
        (chart, decisions)
    }

    fn semantics() -> ResourceSemantics {
        crate::current_resource_semantics()
    }

    fn authoritative_course(
        index: u32,
        chart: CanonicalChart,
        decisions: Vec<BranchDecisionPoint>,
    ) -> AuthoritativeCourse {
        let chart_hash = canonical_chart_sha256(&chart).expect("canonical chart hash");
        AuthoritativeCourse {
            manifest: ResourceCourse {
                index,
                name: format!("Course {index}"),
                level: Some(1),
                canonical_chart_hash: chart_hash,
                object_count: u32::try_from(chart.objects.len()).expect("object count"),
                branch_segment_count: u32::try_from(chart.branch_segments.len())
                    .expect("branch count"),
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
            },
            chart: Arc::new(chart),
            branch_decisions: decisions.into(),
        }
    }

    fn fixture(branching_second_course: bool) -> (AuthoritativeCatalog, MatchManifest, Instant) {
        let resource_semantics = semantics();
        let (second_chart, second_decisions) = if branching_second_course {
            branch_chart(2_000_000)
        } else {
            (chart(2_000_000), Vec::new())
        };
        let authoritative_courses = vec![
            authoritative_course(0, chart(1_000_000), Vec::new()),
            authoritative_course(1, second_chart, second_decisions),
        ];
        let resource_courses = authoritative_courses
            .iter()
            .map(|course| course.manifest.clone())
            .collect::<Vec<_>>();
        let source_id = hash(0x21);
        let audio_id = hash(0x22);
        let song_id = song_manifest_sha256(
            &source_id,
            Some(&audio_id),
            &resource_semantics,
            &resource_courses,
        )
        .expect("derived song id");
        let resource_song = ResourceSong {
            song_id: song_id.clone(),
            source_path: "song.tja".to_owned(),
            source_id,
            audio_path: Some("song.ogg".to_owned()),
            audio_id: Some(audio_id),
            title: "Runtime Test".to_owned(),
            subtitle: String::new(),
            artist: "Test".to_owned(),
            demo_start_seconds: 0.0,
            courses: resource_courses,
        };
        let song = Arc::new(AuthoritativeSong {
            manifest: resource_song.clone(),
            courses: authoritative_courses.into_boxed_slice(),
        });
        let catalog = AuthoritativeCatalog {
            semantics: resource_semantics.clone(),
            songs_by_id: Arc::new(HashMap::from([(song_id.clone(), song)])),
        };

        let protocol_semantics = MatchSemantics {
            canonical_schema_version: resource_semantics.canonical_schema_version,
            canonical_schema_digest: ContentHash::parse(
                resource_semantics.canonical_schema_sha256.clone(),
            )
            .expect("schema hash"),
            importer_semantics_version: resource_semantics.importer_semantics_version,
            importer_semantics_digest: ContentHash::parse(
                resource_semantics.importer_semantics_sha256.clone(),
            )
            .expect("importer hash"),
            ruleset_version: resource_semantics.taiko_ruleset_version,
            ruleset_digest: ContentHash::parse(resource_semantics.taiko_ruleset_sha256.clone())
                .expect("ruleset hash"),
            audio_decoder_semantics_version: resource_semantics.audio_decoder_semantics_version,
            audio_decoder_semantics_digest: ContentHash::parse(
                resource_semantics.audio_decoder_semantics_sha256.clone(),
            )
            .expect("audio decoder semantics hash"),
        };
        let courses = resource_song
            .courses
            .iter()
            .map(|course| CourseManifest {
                course_id: CourseId(course.index),
                name: CourseName::new(course.name.clone()).expect("course name"),
                level: course.level,
                canonical_chart_hash: ContentHash::parse(course.canonical_chart_hash.clone())
                    .expect("chart hash"),
            })
            .collect::<Vec<_>>();
        let song_manifest = SongManifest {
            song_id: SongId::parse(song_id).expect("song id"),
            source_id: ContentHash::parse(resource_song.source_id).expect("source id"),
            audio_id: resource_song
                .audio_id
                .map(ContentHash::parse)
                .transpose()
                .expect("audio id"),
            title: DisplayTitle::new(resource_song.title).expect("title"),
            subtitle: BoundedText::new(resource_song.subtitle).expect("subtitle"),
            artist: BoundedText::new(resource_song.artist).expect("artist"),
            semantics: protocol_semantics,
            courses: BoundedVec::<_, MAX_COURSES_PER_SONG>::new(courses).expect("courses"),
        };
        let assignments = vec![
            PlayerCourseAssignment {
                player_id: PlayerId(1),
                selection: PlayerSelection {
                    course_id: CourseId(0),
                },
                canonical_chart_hash: song_manifest.courses[0].canonical_chart_hash.clone(),
            },
            PlayerCourseAssignment {
                player_id: PlayerId(2),
                selection: PlayerSelection {
                    course_id: CourseId(1),
                },
                canonical_chart_hash: song_manifest.courses[1].canonical_chart_hash.clone(),
            },
        ];
        let manifest = MatchManifest {
            match_id: FIRST_MATCH_ID,
            song: song_manifest,
            assignments: BoundedVec::new(assignments).expect("assignments"),
            countdown_ms: 500,
            input_lateness_ms: 250,
        };
        (catalog, manifest, Instant::now())
    }

    fn batch(events: &[(u64, Tick, DrumAction)]) -> InputBatch {
        InputBatch {
            match_id: FIRST_MATCH_ID,
            events: BoundedVec::<_, MAX_INPUT_BATCH_EVENTS>::new(
                events
                    .iter()
                    .map(|(seq, tick, action)| InputEvent {
                        seq: InputSeq(*seq),
                        tick: *tick,
                        action: *action,
                    })
                    .collect(),
            )
            .expect("batch"),
        }
    }

    #[test]
    fn protocol_actions_map_all_physical_sides_and_zones_without_loss() {
        for (wire, domain) in [
            (DrumAction::LEFT_DON, TaikoAction::LEFT_DON),
            (DrumAction::RIGHT_DON, TaikoAction::RIGHT_DON),
            (DrumAction::LEFT_KAT, TaikoAction::LEFT_KAT),
            (DrumAction::RIGHT_KAT, TaikoAction::RIGHT_KAT),
        ] {
            assert_eq!(taiko_action(wire), domain);
        }
    }

    #[test]
    fn score_fraction_conversion_rejects_out_of_range_values() {
        let player_id = PlayerId(1);
        assert_eq!(fraction_to_ppm(player_id, 0.0).expect("zero"), 0);
        assert_eq!(fraction_to_ppm(player_id, 1.0).expect("one"), 1_000_000);
        assert!(matches!(
            fraction_to_ppm(player_id, -f32::EPSILON),
            Err(MatchRuntimeError::ScoreFractionOutOfRange {
                player_id: PlayerId(1),
                ..
            })
        ));
        assert!(matches!(
            fraction_to_ppm(player_id, 1.000_001),
            Err(MatchRuntimeError::ScoreFractionOutOfRange {
                player_id: PlayerId(1),
                ..
            })
        ));
        assert!(matches!(
            fraction_to_ppm(player_id, f32::NAN),
            Err(MatchRuntimeError::NonFiniteScore(PlayerId(1)))
        ));
    }

    #[test]
    fn catalog_identity_checks_audio_decoder_semantics() {
        let (catalog, mut manifest, _) = fixture(false);
        manifest.song.semantics.audio_decoder_semantics_digest = content_hash(0x7f);

        assert!(matches!(
            validate_catalog_identity(&manifest, &catalog),
            Err(MatchRuntimeError::ManifestResourceMismatch {
                field: "semantics.audio_decoder_semantics_digest"
            })
        ));
    }

    #[test]
    fn different_courses_are_simulated_by_the_server() {
        let (catalog, manifest, start) = fixture(false);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        runtime
            .accept_input(
                PlayerId(1),
                &batch(&[(1, 1_000_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(1_000_000),
            )
            .expect("course zero input");
        runtime
            .accept_input(
                PlayerId(2),
                &batch(&[(1, 2_000_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(2_000_000),
            )
            .expect("course one input");
        runtime
            .advance(start + Duration::from_micros(2_500_000))
            .expect("advance");

        assert!(runtime.all_finished());
        let results = runtime.final_results().expect("results");
        assert_eq!(results[0].course_id, CourseId(0));
        assert_eq!(results[1].course_id, CourseId(1));
        assert_eq!(results[0].score.great, 1);
        assert_eq!(results[1].score.great, 1);
    }

    #[test]
    fn automatic_branching_selects_the_score_hint_route_before_input() {
        let (catalog, manifest, start) = fixture(true);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        runtime
            .accept_input(
                PlayerId(2),
                &batch(&[(1, 2_000_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(2_000_000),
            )
            .expect("automatic branch input");
        runtime
            .mark_dnf(PlayerId(1), start + Duration::from_micros(2_500_000))
            .expect("dnf");
        runtime
            .advance(start + Duration::from_micros(2_500_000))
            .expect("advance");
        let result = &runtime.final_results().expect("results")[1];
        assert_eq!(result.score.great, 1);
        assert!(!result.dnf);
    }

    #[test]
    fn duplicate_is_idempotent_but_gap_and_conflict_are_rejected() {
        let (catalog, manifest, start) = fixture(false);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        let first = batch(&[(1, 1_000_000, DrumAction::LEFT_DON)]);
        let accepted = runtime
            .accept_input(
                PlayerId(1),
                &first,
                start + Duration::from_micros(1_000_000),
            )
            .expect("first");
        assert_eq!(accepted.newly_accepted, 1);
        let duplicate = runtime
            .accept_input(
                PlayerId(1),
                &first,
                start + Duration::from_micros(1_050_000),
            )
            .expect("duplicate");
        assert_eq!(duplicate.newly_accepted, 0);
        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(3, 1_010_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(1_050_000),
            ),
            Err(MatchRuntimeError::SequenceGap { .. })
        ));
        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(1, 1_000_000, DrumAction::RIGHT_DON)]),
                start + Duration::from_micros(1_050_000),
            ),
            Err(MatchRuntimeError::ConflictingDuplicate { .. })
        ));
    }

    #[test]
    fn physical_input_rate_is_bounded_and_rejected_batches_are_atomic() {
        let (catalog, manifest, start) = fixture(false);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        let burst = batch(&[
            (1, 500_000, DrumAction::LEFT_DON),
            (2, 500_000, DrumAction::RIGHT_KAT),
            (3, 500_000, DrumAction::LEFT_DON),
            (4, 500_000, DrumAction::RIGHT_KAT),
        ]);
        let oversized_burst = batch(&[
            (1, 500_000, DrumAction::LEFT_DON),
            (2, 500_000, DrumAction::RIGHT_KAT),
            (3, 500_000, DrumAction::LEFT_DON),
            (4, 500_000, DrumAction::RIGHT_KAT),
            (5, 500_000, DrumAction::LEFT_DON),
        ]);

        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &oversized_burst,
                start + Duration::from_micros(500_000),
            ),
            Err(MatchRuntimeError::InputRateExceeded { tick: 500_000 })
        ));

        let accepted = runtime
            .accept_input(PlayerId(1), &burst, start + Duration::from_micros(500_000))
            .expect("rejected batch did not consume sequence or rate credit");
        assert_eq!(accepted.highest_contiguous_seq, Some(InputSeq(4)));
        assert_eq!(accepted.newly_accepted, 4);

        let duplicate = runtime
            .accept_input(PlayerId(1), &burst, start + Duration::from_micros(510_000))
            .expect("exact retry is idempotent");
        assert_eq!(duplicate.newly_accepted, 0);

        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(5, 519_999, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(519_999),
            ),
            Err(MatchRuntimeError::InputRateExceeded { tick: 519_999 })
        ));
        let refilled = runtime
            .accept_input(
                PlayerId(1),
                &batch(&[(5, 520_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(520_000),
            )
            .expect("one token refills after twenty milliseconds");
        assert_eq!(refilled.highest_contiguous_seq, Some(InputSeq(5)));
    }

    #[test]
    fn dnf_drains_every_previously_accepted_input_before_finalizing() {
        let (catalog, manifest, start) = fixture(false);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        runtime
            .accept_input(
                PlayerId(1),
                &batch(&[(1, 1_000_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(1_000_000),
            )
            .expect("input is accepted inside the lateness window");

        runtime
            .mark_dnf(PlayerId(1), start + Duration::from_micros(1_000_000))
            .expect("leaving player is finalized");
        runtime
            .mark_dnf(PlayerId(2), start + Duration::from_micros(1_000_000))
            .expect("other player is finalized");

        let results = runtime.final_results().expect("all players are final");
        let player = results
            .iter()
            .find(|result| result.player_id == PlayerId(1))
            .expect("player result");
        assert_eq!(
            player.score.great, 1,
            "an Accepted input must never be discarded by immediate DNF"
        );
    }

    #[test]
    fn late_future_and_regressing_ticks_are_rejected_atomically() {
        let (catalog, manifest, start) = fixture(false);
        let mut runtime =
            AuthoritativeMatchRuntime::new(manifest, catalog, start).expect("runtime");
        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(1, 350_001, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(100_000),
            ),
            Err(MatchRuntimeError::FutureInput { .. })
        ));
        runtime
            .accept_input(
                PlayerId(1),
                &batch(&[(1, 500_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(500_000),
            )
            .expect("first");
        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(2, 499_999, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(500_000),
            ),
            Err(MatchRuntimeError::NonMonotonicInputTick { .. })
        ));
        runtime
            .advance(start + Duration::from_micros(1_000_000))
            .expect("commit to 750k");
        assert!(matches!(
            runtime.accept_input(
                PlayerId(1),
                &batch(&[(2, 700_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(1_000_000),
            ),
            Err(MatchRuntimeError::LateInput { .. })
        ));
        let accepted = runtime
            .accept_input(
                PlayerId(1),
                &batch(&[(2, 800_000, DrumAction::LEFT_DON)]),
                start + Duration::from_micros(1_000_000),
            )
            .expect("sequence was not consumed by rejected batches");
        assert_eq!(accepted.highest_contiguous_seq, Some(InputSeq(2)));
    }

    #[test]
    fn replay_digest_is_deterministic_across_advance_cadence_and_retries() {
        let (catalog_a, manifest_a, start_a) = fixture(false);
        let (catalog_b, mut manifest_b, start_b) = fixture(false);
        manifest_b.match_id = manifest_a.match_id;
        let mut a =
            AuthoritativeMatchRuntime::new(manifest_a, catalog_a, start_a).expect("runtime a");
        let mut b =
            AuthoritativeMatchRuntime::new(manifest_b, catalog_b, start_b).expect("runtime b");
        let inputs = batch(&[
            (1, 995_000, DrumAction::LEFT_DON),
            (2, 1_000_000, DrumAction::LEFT_DON),
        ]);
        for (runtime, start) in [(&mut a, start_a), (&mut b, start_b)] {
            runtime
                .accept_input(
                    PlayerId(1),
                    &inputs,
                    start + Duration::from_micros(1_000_000),
                )
                .expect("inputs");
        }
        b.accept_input(
            PlayerId(1),
            &inputs,
            start_b + Duration::from_micros(1_010_000),
        )
        .expect("retry");
        a.mark_dnf(PlayerId(2), start_a + Duration::from_micros(2_500_000))
            .expect("dnf a");
        b.advance(start_b + Duration::from_micros(1_250_000))
            .expect("intermediate b");
        b.advance(start_b + Duration::from_micros(1_750_000))
            .expect("intermediate b");
        b.mark_dnf(PlayerId(2), start_b + Duration::from_micros(2_500_000))
            .expect("dnf b");
        a.advance(start_a + Duration::from_micros(2_500_000))
            .expect("finish a");
        b.advance(start_b + Duration::from_micros(2_500_000))
            .expect("finish b");

        let result_a = &a.final_results().expect("results a")[0];
        let result_b = &b.final_results().expect("results b")[0];
        assert_eq!(result_a.replay_digest, result_b.replay_digest);
        assert_eq!(result_a.score, result_b.score);
        assert_eq!(result_a.finish_tick, result_b.finish_tick);
    }

    #[test]
    fn replay_digest_retains_side_even_when_authoritative_score_is_equal() {
        let (catalog_left, manifest_left, start_left) = fixture(false);
        let (catalog_right, manifest_right, start_right) = fixture(false);
        let mut left = AuthoritativeMatchRuntime::new(manifest_left, catalog_left, start_left)
            .expect("left runtime");
        let mut right = AuthoritativeMatchRuntime::new(manifest_right, catalog_right, start_right)
            .expect("right runtime");

        left.accept_input(
            PlayerId(1),
            &batch(&[(1, 1_000_000, DrumAction::LEFT_DON)]),
            start_left + Duration::from_micros(1_000_000),
        )
        .expect("left input");
        right
            .accept_input(
                PlayerId(1),
                &batch(&[(1, 1_000_000, DrumAction::RIGHT_DON)]),
                start_right + Duration::from_micros(1_000_000),
            )
            .expect("right input");

        left.mark_dnf(PlayerId(2), start_left + Duration::from_micros(2_500_000))
            .expect("left dnf");
        right
            .mark_dnf(PlayerId(2), start_right + Duration::from_micros(2_500_000))
            .expect("right dnf");
        left.advance(start_left + Duration::from_micros(2_500_000))
            .expect("finish left");
        right
            .advance(start_right + Duration::from_micros(2_500_000))
            .expect("finish right");

        let left_result = &left.final_results().expect("left results")[0];
        let right_result = &right.final_results().expect("right results")[0];
        assert_eq!(left_result.score, right_result.score);
        assert_eq!(left_result.score.great, 1);
        assert_ne!(left_result.replay_digest, right_result.replay_digest);
    }
}
