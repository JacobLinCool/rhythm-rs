use std::time::Instant;

use anyhow::{Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rhythm_chart::{BranchDecisionPoint, CanonicalChart, Tick};
use rhythm_core::{FrameOutput, TimedInput};
use rhythm_mode_taiko::{
    ScheduledTaikoInput, TaikoAction, TaikoBranchPolicy, TaikoFinalResult, TaikoJudge, TaikoMode,
    TaikoRuntime,
};

#[cfg(test)]
use crate::input::MAX_OFFLINE_PENDING_INPUTS;
use crate::input::{collect_due_offline_inputs, enqueue_offline_input, map_bound_game_hit};
use crate::preferences::DrumBindings;

const HIT_FLASH_TICKS: Tick = 200_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalPlayerId {
    One,
    Two,
}

impl LocalPlayerId {
    pub(crate) const ALL: [Self; 2] = [Self::One, Self::Two];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::One => 0,
            Self::Two => 1,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::One => "P1",
            Self::Two => "P2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalCourseIntent {
    Move(isize),
    ToggleReady,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LocalGameInput {
    pub(crate) player: LocalPlayerId,
    pub(crate) hit: TaikoAction,
}

impl LocalGameInput {
    pub(crate) const fn action(self) -> TaikoAction {
        self.hit
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalCourseSelection {
    course_indices: [usize; 2],
    ready: [bool; 2],
}

impl LocalCourseSelection {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn reset_to_course(&mut self, course_index: usize, course_count: usize) {
        self.reset();
        let course_index = course_index.min(course_count.saturating_sub(1));
        self.course_indices = [course_index, course_index];
    }

    pub(crate) const fn course_index(&self, player: LocalPlayerId) -> usize {
        self.course_indices[player.index()]
    }

    pub(crate) const fn is_ready(&self, player: LocalPlayerId) -> bool {
        self.ready[player.index()]
    }

    pub(crate) fn apply(
        &mut self,
        player: LocalPlayerId,
        intent: LocalCourseIntent,
        course_count: usize,
    ) {
        if course_count == 0 {
            return;
        }

        let index = player.index();
        match intent {
            LocalCourseIntent::Move(delta) if !self.ready[index] => {
                self.course_indices[index] =
                    wrapped_index(self.course_indices[index], course_count, delta);
            }
            LocalCourseIntent::ToggleReady => {
                self.ready[index] = !self.ready[index];
            }
            LocalCourseIntent::Move(_) => {}
        }
    }

    pub(crate) fn both_ready(&self) -> bool {
        self.ready.into_iter().all(|ready| ready)
    }

    pub(crate) fn clear_ready(&mut self) {
        self.ready = [false, false];
    }
}

pub(crate) struct LocalPlayerSpec {
    pub(crate) course_name: String,
    pub(crate) chart: CanonicalChart,
    pub(crate) branch_decisions: Vec<BranchDecisionPoint>,
}

pub(crate) struct LocalPlayerSession {
    pub(crate) course_name: String,
    pub(crate) runtime: TaikoRuntime,
    pub(crate) last_output: FrameOutput<TaikoMode>,
    pub(crate) last_judge: Option<TaikoJudge>,
    pub(crate) pending_inputs: Vec<TimedInput<TaikoAction>>,
    pub(crate) judge_flash: Option<(TaikoJudge, Tick)>,
    pub(crate) input_flash: Option<(TaikoAction, Tick)>,
}

pub(crate) struct LocalMultiplayerSession {
    pub(crate) song_index: usize,
    pub(crate) has_audio: bool,
    pub(crate) players: [LocalPlayerSession; 2],
    pub(crate) last_tick: Tick,
    pub(crate) result_delay_deadline: Option<Instant>,
    pub(crate) paused: bool,
}

impl LocalMultiplayerSession {
    pub(crate) fn new(
        song_index: usize,
        specs: [LocalPlayerSpec; 2],
        branch_policy: TaikoBranchPolicy,
        has_audio: bool,
    ) -> Result<Self> {
        let [one, two] = specs;
        Ok(Self {
            song_index,
            has_audio,
            players: [
                build_player_session(one, branch_policy)?,
                build_player_session(two, branch_policy)?,
            ],
            last_tick: 0,
            result_delay_deadline: None,
            paused: false,
        })
    }

    #[must_use]
    pub(crate) fn queue_input(&mut self, input: LocalGameInput, tick: Tick) -> bool {
        let action = input.action();
        enqueue_offline_input(
            &mut self.players[input.player.index()].pending_inputs,
            TimedInput { tick, action },
        )
    }

    pub(crate) fn advance_to(&mut self, now_tick: Tick) -> Result<()> {
        for player in &mut self.players {
            let due = collect_due_offline_inputs(&mut player.pending_inputs, now_tick);
            let scheduled = due
                .iter()
                .copied()
                .map(ScheduledTaikoInput::unconditional)
                .collect::<Vec<_>>();
            let output = player
                .runtime
                .advance_to(now_tick, &scheduled)
                .context("local multiplayer engine step failed")?;

            if let Some(input) = due
                .iter()
                .copied()
                .filter(|input| input.tick <= output.now)
                .max_by_key(|input| input.tick)
            {
                player.input_flash =
                    Some((input.action, output.now.saturating_add(HIT_FLASH_TICKS)));
            }

            player.last_judge = latest_non_ignored_judge(&output.judges);
            if let Some(judge) = latest_flashable_judge(&output.judges) {
                player.judge_flash = Some((judge, output.now.saturating_add(HIT_FLASH_TICKS)));
            }
            player.last_output = output;
            expire_flashes(player);
        }
        self.last_tick = now_tick;
        Ok(())
    }

    pub(crate) fn all_finished(&self) -> bool {
        self.players
            .iter()
            .all(|player| player.last_output.finished)
    }

    pub(crate) fn results(&self) -> [LocalPlayerResult; 2] {
        std::array::from_fn(|index| {
            let player = &self.players[index];
            LocalPlayerResult {
                course_name: player.course_name.clone(),
                final_result: player.runtime.finalize(),
                replay_hash: player.runtime.replay_hash(),
                branch_controls: player.runtime.emitted_controls(),
            }
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct LocalPlayerResult {
    pub(crate) course_name: String,
    pub(crate) final_result: TaikoFinalResult,
    pub(crate) replay_hash: u64,
    pub(crate) branch_controls: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct LocalMultiplayerResult {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) players: [LocalPlayerResult; 2],
}

pub(crate) fn map_local_course_key(key: KeyEvent) -> Option<(LocalPlayerId, LocalCourseIntent)> {
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }

    match key.code {
        KeyCode::Char('w' | 'W') => Some((LocalPlayerId::One, LocalCourseIntent::Move(-1))),
        KeyCode::Char('s' | 'S') => Some((LocalPlayerId::One, LocalCourseIntent::Move(1))),
        KeyCode::Char('f' | 'F') => Some((LocalPlayerId::One, LocalCourseIntent::ToggleReady)),
        KeyCode::Up => Some((LocalPlayerId::Two, LocalCourseIntent::Move(-1))),
        KeyCode::Down => Some((LocalPlayerId::Two, LocalCourseIntent::Move(1))),
        KeyCode::Char('j' | 'J') | KeyCode::Enter => {
            Some((LocalPlayerId::Two, LocalCourseIntent::ToggleReady))
        }
        _ => None,
    }
}

pub(crate) fn map_local_game_hit(
    key: KeyEvent,
    player_one: DrumBindings,
    player_two: DrumBindings,
) -> Option<LocalGameInput> {
    map_bound_game_hit(key, player_one)
        .map(|hit| LocalGameInput {
            player: LocalPlayerId::One,
            hit,
        })
        .or_else(|| {
            map_bound_game_hit(key, player_two).map(|hit| LocalGameInput {
                player: LocalPlayerId::Two,
                hit,
            })
        })
}

fn build_player_session(
    spec: LocalPlayerSpec,
    branch_policy: TaikoBranchPolicy,
) -> Result<LocalPlayerSession> {
    let mut runtime = TaikoRuntime::new(&spec.chart, branch_policy, spec.branch_decisions)
        .context("invalid taiko runtime for a local multiplayer chart")?;
    let initial_output = runtime
        .advance_to(0, &[])
        .context("failed to bootstrap a local multiplayer frame")?;
    Ok(LocalPlayerSession {
        course_name: spec.course_name,
        runtime,
        last_output: initial_output,
        last_judge: None,
        pending_inputs: Vec::with_capacity(32),
        judge_flash: None,
        input_flash: None,
    })
}

fn wrapped_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    current
        .checked_add_signed(delta)
        .map_or_else(|| len - 1, |next| next % len)
}

fn expire_flashes(player: &mut LocalPlayerSession) {
    if player
        .input_flash
        .is_some_and(|(_, until)| player.last_output.now > until)
    {
        player.input_flash = None;
    }
    if player
        .judge_flash
        .is_some_and(|(_, until)| player.last_output.now > until)
    {
        player.judge_flash = None;
    }
}

fn latest_non_ignored_judge(judges: &[TaikoJudge]) -> Option<TaikoJudge> {
    judges
        .iter()
        .rev()
        .copied()
        .find(|judge| !matches!(judge, TaikoJudge::Ignored))
}

fn latest_flashable_judge(judges: &[TaikoJudge]) -> Option<TaikoJudge> {
    judges.iter().rev().copied().find(|judge| {
        matches!(
            judge,
            TaikoJudge::Great { .. }
                | TaikoJudge::Ok { .. }
                | TaikoJudge::Miss { .. }
                | TaikoJudge::MissExpired
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhythm_chart::{
        ChartMetadata, Lane, LaneOrRegion, LaneRole, Object, ObjectKind, TempoChange, SCROLL_SCALE,
    };
    use rhythm_mode_taiko::LANE_DON;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn local_test_chart() -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata {
                title: "Local isolation".to_owned(),
                difficulty_name: Some("Oni".to_owned()),
                difficulty_level: Some(1),
                ..ChartMetadata::default()
            },
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: Vec::new(),
            lanes: vec![Lane {
                id: LANE_DON,
                name: "Don".to_owned(),
                role: LaneRole::TaikoDon,
            }],
            branch_segments: Vec::new(),
            objects: vec![Object {
                id: 1,
                kind: ObjectKind::Tap,
                start_tick: 100_000,
                end_tick: 100_000,
                lane_or_region: LaneOrRegion::Lane(LANE_DON),
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

    #[test]
    fn local_game_exposes_four_disjoint_pad_hits_per_player() {
        let mappings = [
            ('a', LocalPlayerId::One, TaikoAction::LEFT_KAT),
            ('s', LocalPlayerId::One, TaikoAction::LEFT_DON),
            ('d', LocalPlayerId::One, TaikoAction::RIGHT_DON),
            ('f', LocalPlayerId::One, TaikoAction::RIGHT_KAT),
            ('j', LocalPlayerId::Two, TaikoAction::LEFT_KAT),
            ('k', LocalPlayerId::Two, TaikoAction::LEFT_DON),
            ('l', LocalPlayerId::Two, TaikoAction::RIGHT_DON),
            (';', LocalPlayerId::Two, TaikoAction::RIGHT_KAT),
        ];

        for (character, player, hit) in mappings {
            let expected = LocalGameInput { player, hit };
            assert_eq!(
                map_local_game_hit(
                    key(KeyCode::Char(character)),
                    DrumBindings::player_one_default(),
                    DrumBindings::player_two_default(),
                ),
                Some(expected)
            );
            assert_eq!(expected.action(), hit);

            if character.is_ascii_alphabetic() {
                assert_eq!(
                    map_local_game_hit(
                        key(KeyCode::Char(character.to_ascii_uppercase())),
                        DrumBindings::player_one_default(),
                        DrumBindings::player_two_default(),
                    ),
                    Some(expected)
                );
            }
        }

        assert_eq!(
            map_local_game_hit(
                key(KeyCode::Char(' ')),
                DrumBindings::player_one_default(),
                DrumBindings::player_two_default(),
            ),
            None
        );
    }

    #[test]
    fn ready_players_cannot_change_course_until_they_unlock() {
        let mut selection = LocalCourseSelection::default();
        selection.apply(LocalPlayerId::One, LocalCourseIntent::ToggleReady, 4);
        selection.apply(LocalPlayerId::One, LocalCourseIntent::Move(1), 4);
        assert_eq!(selection.course_index(LocalPlayerId::One), 0);

        selection.apply(LocalPlayerId::One, LocalCourseIntent::ToggleReady, 4);
        selection.apply(LocalPlayerId::One, LocalCourseIntent::Move(1), 4);
        assert_eq!(selection.course_index(LocalPlayerId::One), 1);
    }

    #[test]
    fn both_players_must_be_ready() {
        let mut selection = LocalCourseSelection::default();
        selection.apply(LocalPlayerId::One, LocalCourseIntent::ToggleReady, 1);
        assert!(!selection.both_ready());
        selection.apply(LocalPlayerId::Two, LocalCourseIntent::ToggleReady, 1);
        assert!(selection.both_ready());
    }

    #[test]
    fn one_players_input_never_enters_the_other_runtime() {
        let chart = local_test_chart();
        let spec = |name: &str| LocalPlayerSpec {
            course_name: name.to_owned(),
            chart: chart.clone(),
            branch_decisions: Vec::new(),
        };
        let mut session = LocalMultiplayerSession::new(
            0,
            [spec("P1 course"), spec("P2 course")],
            TaikoBranchPolicy::Disabled,
            true,
        )
        .expect("build local multiplayer session");
        assert!(session.queue_input(
            LocalGameInput {
                player: LocalPlayerId::One,
                hit: TaikoAction::LEFT_DON,
            },
            100_000,
        ));
        session.advance_to(300_000).expect("advance both players");

        let [p1, p2] = session.results();
        assert_eq!(p1.final_result.great, 1);
        assert_eq!(p1.final_result.miss, 0);
        assert_eq!(p2.final_result.great, 0);
        assert_eq!(p2.final_result.miss, 1);
        assert_ne!(p1.replay_hash, p2.replay_hash);
    }

    #[test]
    fn both_local_player_queues_reject_inputs_after_the_512_event_window() {
        let chart = local_test_chart();
        let spec = |name: &str| LocalPlayerSpec {
            course_name: name.to_owned(),
            chart: chart.clone(),
            branch_decisions: Vec::new(),
        };
        let mut session = LocalMultiplayerSession::new(
            0,
            [spec("P1 course"), spec("P2 course")],
            TaikoBranchPolicy::Disabled,
            true,
        )
        .expect("build local multiplayer session");

        for player in LocalPlayerId::ALL {
            for tick in 0..MAX_OFFLINE_PENDING_INPUTS {
                assert!(session.queue_input(
                    LocalGameInput {
                        player,
                        hit: TaikoAction::LEFT_DON,
                    },
                    Tick::try_from(MAX_OFFLINE_PENDING_INPUTS - tick).expect("test tick fits"),
                ));
            }
            for _ in 0..64 {
                assert!(!session.queue_input(
                    LocalGameInput {
                        player,
                        hit: TaikoAction::RIGHT_KAT,
                    },
                    0,
                ));
            }
            let pending = &session.players[player.index()].pending_inputs;
            assert_eq!(pending.len(), MAX_OFFLINE_PENDING_INPUTS);
            assert!(pending.windows(2).all(|pair| pair[0].tick <= pair[1].tick));
            assert!(pending.iter().all(|input| input.tick > 0));
        }
    }
}
