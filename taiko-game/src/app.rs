use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use rhythm_chart::{ticks_from_seconds, CanonicalChart, Object, ObjectKind, TempoChange, Tick};
use rhythm_core::{ControlledEngine, TimedInput};
use rhythm_mode_taiko::{
    TaikoAction, TaikoFinalResult, TaikoJudge, TaikoJudgeKind, TaikoMode, LANE_KAT,
};

use taiko_multiplayer_protocol::RoomPhase;

use crate::audio::AudioEngine;
use crate::branch::BranchController;
use crate::cli::{BranchPolicy, CliArgs};
use crate::input::{is_game_pause_toggle_key, map_game_hit, map_menu_intent, MenuIntent};
use crate::loader::{CourseEntry, SongEntry};
use crate::perf::{PerfMeter, PerfSnapshot};
use crate::resource::ResourceBackend;
use crate::screen;
use crate::song_filter::SongFilter;
use crate::theme::Theme;
use crate::tui::Frame;

const DEMO_DELAY: Duration = Duration::from_millis(500);
const RESULT_DELAY: Duration = Duration::from_millis(500);
const HIT_FLASH_TICKS: Tick = 200_000;
const OFFSET_STEP_MS: i32 = 5;
const OFFSET_MIN_MS: i32 = -500;
const OFFSET_MAX_MS: i32 = 500;
const SCROLL_SPEED_STEP: f32 = 0.1;
const VSYNC_SPEED_MIN: f32 = 1.0;
const VSYNC_SPEED_MAX: f32 = 2.0;
const SCROLL_SPEED_MIN_UNITS: i32 = 5;
const SCROLL_SPEED_MAX_UNITS: i32 = 40;
const SCROLL_SPEED_VSYNC_SLOT: i32 = SCROLL_SPEED_MAX_UNITS - SCROLL_SPEED_MIN_UNITS + 1;
const LOOKAHEAD_TICKS: Tick = 2_000_000;
const VSYNC_CANDIDATE_TOP_INTERVALS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    SongMenu,
    LoadWarnings,
    CourseMenu,
    Game,
    Result,
    Error,
    MultiplayerConnect,
    OnlineLobby,
    OnlineCourseSelect,
    OnlineMatch,
    OnlineResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectMode {
    Create,
    Join,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectField {
    Mode,
    Server,
    RoomCode,
    Name,
    Confirm,
}

pub struct MultiplayerConnectState {
    pub mode: ConnectMode,
    pub server: String,
    pub room_code: String,
    pub name: String,
    pub focus: ConnectField,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResultState {
    pub title: String,
    pub subtitle: String,
    pub course_name: String,
    pub final_result: TaikoFinalResult,
    pub replay_hash: u64,
    pub branch_controls: usize,
    pub timing_samples: Vec<TimingSample>,
    pub perf: PerfSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingSample {
    pub judge: TaikoJudgeKind,
    pub delta_tick: Tick,
}

pub struct LoadedCourseChart {
    pub song_index: usize,
    pub course_index: usize,
    pub chart: CanonicalChart,
}

pub struct GameSession {
    pub song_index: usize,
    pub course_name: String,
    pub engine: ControlledEngine<TaikoMode>,
    pub branch_controller: BranchController,
    pub last_output: rhythm_core::FrameOutput<TaikoMode>,
    pub last_judge: Option<TaikoJudge>,
    pub last_tick: Tick,
    pub autoplay_inputs: Vec<AutoplayInputEvent>,
    pub autoplay_cursor: usize,
    pub pending_inputs: Vec<TimedInput<TaikoAction>>,
    pub timing_samples: Vec<TimingSample>,
    pub judge_flash: Option<JudgeFlashState>,
    pub input_flash: Option<InputFlashState>,
    pub result_delay_deadline: Option<Instant>,
    pub paused: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct JudgeFlashState {
    pub judge: TaikoJudge,
    pub until_tick: Tick,
}

#[derive(Debug, Clone, Copy)]
pub struct InputFlashState {
    pub action: TaikoAction,
    pub until_tick: Tick,
}

#[derive(Debug, Clone, Copy)]
pub struct AutoplayInputEvent {
    pub input: TimedInput<TaikoAction>,
    pub branch_segment_id: Option<u32>,
    pub branch_route_id: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CourseSettingFocus {
    AutoPlay,
    SongVolume,
    SeVolume,
    NoteOffset,
    MusicOffset,
    ScrollSpeed,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ScrollSpeedSetting {
    Manual(f32),
    /// VSync = Velocity Sync, i.e. adjust scroll speed so that note travel time matches the screen projection time
    VSync,
}

impl CourseSettingFocus {
    fn next(self) -> Self {
        match self {
            Self::AutoPlay => Self::SongVolume,
            Self::SongVolume => Self::SeVolume,
            Self::SeVolume => Self::NoteOffset,
            Self::NoteOffset => Self::MusicOffset,
            Self::MusicOffset => Self::ScrollSpeed,
            Self::ScrollSpeed => Self::AutoPlay,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::AutoPlay => Self::ScrollSpeed,
            Self::SongVolume => Self::AutoPlay,
            Self::SeVolume => Self::SongVolume,
            Self::NoteOffset => Self::SeVolume,
            Self::MusicOffset => Self::NoteOffset,
            Self::ScrollSpeed => Self::MusicOffset,
        }
    }
}

pub struct App {
    pub(crate) args: CliArgs,
    pub(crate) page: Page,
    pub(crate) songs: Vec<SongEntry>,
    pub(crate) filtered_song_indices: Vec<usize>,
    pub(crate) song_query: String,
    pub(crate) song_filter_error: Option<String>,
    pub(crate) song_index: usize,
    pub(crate) course_index: usize,
    pub(crate) branch_policy: BranchPolicy,
    pub(crate) fixed_route: u8,
    pub(crate) auto_play: bool,
    pub(crate) course_setting_focus: CourseSettingFocus,
    pub(crate) scroll_speed_setting: ScrollSpeedSetting,
    pub(crate) scroll_speed_vsync: f32,
    pub(crate) note_offset_ms: i32,
    pub(crate) music_offset_ms: i32,
    pub(crate) viewport_width: u16,
    pub(crate) game: Option<GameSession>,
    pub(crate) result: Option<ResultState>,
    pub(crate) error_message: Option<String>,
    pub(crate) load_warnings: Vec<String>,
    pub(crate) load_warnings_scroll: u16,
    pub(crate) perf_meter: PerfMeter,
    pub(crate) theme: Theme,
    pub(crate) should_quit: bool,
    pub(crate) demo_pending: Option<(Instant, usize)>,
    pub(crate) demo_playing_song: Option<usize>,
    loaded_course_chart: Option<LoadedCourseChart>,
    resource_backend: ResourceBackend,
    audio: AudioEngine,
    pub(crate) mp_connect: MultiplayerConnectState,
    pub(crate) online: Option<crate::online_session::OnlineSession>,
}

impl App {
    pub fn new(args: CliArgs) -> Result<Self> {
        if args.tps == 0 {
            bail!("--tps must be > 0");
        }

        let using_remote_resources = args.resource_endpoint.is_some();
        let resource_backend = ResourceBackend::from_cli(&args)?;
        let library = resource_backend
            .load_song_library()
            .context("failed to load song library")?;

        let mut app = Self {
            branch_policy: BranchPolicy::Auto,
            fixed_route: 0,
            auto_play: false,
            course_setting_focus: CourseSettingFocus::AutoPlay,
            scroll_speed_setting: ScrollSpeedSetting::Manual(1.0),
            scroll_speed_vsync: 1.0,
            note_offset_ms: quantize_offset_ms(args.track_offset),
            music_offset_ms: 0,
            viewport_width: 120,
            resource_backend,
            audio: AudioEngine::new(args.songvol, args.sevol)?,
            args,
            page: Page::SongMenu,
            songs: library.songs,
            filtered_song_indices: Vec::new(),
            song_query: String::new(),
            song_filter_error: None,
            song_index: 0,
            course_index: 0,
            game: None,
            result: None,
            error_message: None,
            load_warnings: library.warnings,
            load_warnings_scroll: 0,
            perf_meter: PerfMeter::default(),
            theme: Theme::taiko_vivid(Theme::detect()),
            should_quit: false,
            demo_pending: None,
            demo_playing_song: None,
            loaded_course_chart: None,
            mp_connect: MultiplayerConnectState {
                mode: ConnectMode::Create,
                server: "http://127.0.0.1:4150".to_owned(),
                room_code: String::new(),
                name: "Player".to_owned(),
                focus: ConnectField::Mode,
                error: None,
            },
            online: None,
        };

        if app.songs.is_empty() {
            app.page = Page::Error;
            app.error_message = Some(if app.load_warnings.is_empty() {
                if using_remote_resources {
                    "no playable charts found from remote resource endpoint".to_owned()
                } else {
                    "no .tja charts found in song directory".to_owned()
                }
            } else {
                format!(
                    "no playable charts loaded, first error: {}",
                    app.load_warnings[0]
                )
            });
        } else {
            app.rebuild_song_filter()?;
        }

        app.refresh_vsync_scroll_speed()?;

        Ok(app)
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn handle_tick(&mut self) {
        if self.should_quit {
            return;
        }

        let result = match self.page {
            Page::SongMenu | Page::LoadWarnings | Page::CourseMenu => self.tick_demo_preview(),
            Page::Game => self.tick_game(),
            Page::OnlineLobby => self.tick_online_lobby(),
            Page::OnlineCourseSelect | Page::OnlineMatch => self.tick_online_match_phase(),
            Page::Result | Page::Error | Page::MultiplayerConnect | Page::OnlineResult => Ok(()),
        };

        // Always tick online network if session exists
        if let Some(online) = &mut self.online {
            if let Err(e) = online.tick_network() {
                self.set_error_state(e);
                return;
            }
        }
        // Process pending session actions
        self.process_online_actions();

        if let Err(error) = result {
            self.set_error_state(error);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.should_quit {
            return;
        }

        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
        ) {
            self.should_quit = true;
            return;
        }

        let result = match self.page {
            Page::SongMenu => self.handle_song_menu_key(key),
            Page::LoadWarnings => self.handle_load_warnings_key(key),
            Page::CourseMenu => self.handle_course_menu_key(key),
            Page::Game => self.handle_game_key(key),
            Page::Result => self.handle_result_key(key),
            Page::Error => self.handle_error_key(key),
            Page::MultiplayerConnect => self.handle_mp_connect_key(key),
            Page::OnlineLobby => self.handle_online_lobby_key(key),
            Page::OnlineCourseSelect => self.handle_online_course_key(key),
            Page::OnlineMatch => self.handle_online_match_key(key),
            Page::OnlineResult => self.handle_online_result_key(key),
        };

        if let Err(error) = result {
            self.set_error_state(error);
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let size = frame.area();
        if size.width != self.viewport_width {
            self.viewport_width = size.width;
            if let Err(error) = self.refresh_vsync_scroll_speed() {
                self.set_error_state(error);
            }
        }
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(size);

        screen::render_topbar(self, frame, chunks[0]);

        match self.page {
            Page::SongMenu => screen::song_menu::render(self, frame, chunks[1]),
            Page::LoadWarnings => screen::load_warnings_screen::render(self, frame, chunks[1]),
            Page::CourseMenu => screen::course_menu::render(self, frame, chunks[1]),
            Page::Game => screen::game_screen::render(self, frame, chunks[1]),
            Page::Result => screen::result_screen::render(self, frame, chunks[1]),
            Page::Error => screen::error_screen::render(self, frame, chunks[1]),
            Page::MultiplayerConnect => screen::mp_connect::render(self, frame, chunks[1]),
            Page::OnlineLobby => screen::online_lobby::render(self, frame, chunks[1]),
            Page::OnlineCourseSelect => screen::online_course::render(self, frame, chunks[1]),
            Page::OnlineMatch => screen::online_match::render(self, frame, chunks[1]),
            Page::OnlineResult => screen::online_result::render(self, frame, chunks[1]),
        }
    }

    pub fn record_frame_time(&mut self, elapsed: Duration) {
        if matches!(self.page, Page::Game) {
            self.perf_meter.record_frame(elapsed);
        }
    }

    pub(crate) fn selected_song(&self) -> Option<&SongEntry> {
        self.selected_song_index()
            .and_then(|index| self.songs.get(index))
    }

    pub(crate) fn selected_song_index(&self) -> Option<usize> {
        self.filtered_song_indices.get(self.song_index).copied()
    }

    pub(crate) fn visible_song_count(&self) -> usize {
        self.filtered_song_indices.len()
    }

    pub(crate) fn selected_course(&self) -> Option<&CourseEntry> {
        self.selected_song()
            .and_then(|song| song.courses.get(self.course_index))
    }

    fn selected_course_chart(&self) -> Option<&CanonicalChart> {
        let song_index = self.selected_song_index()?;
        let course_index = self.course_index;
        self.loaded_course_chart.as_ref().and_then(|loaded| {
            (loaded.song_index == song_index && loaded.course_index == course_index)
                .then_some(&loaded.chart)
        })
    }

    pub(crate) fn effective_scroll_speed(&self) -> f32 {
        match self.scroll_speed_setting {
            ScrollSpeedSetting::Manual(speed) => speed,
            ScrollSpeedSetting::VSync => self.scroll_speed_vsync,
        }
    }

    pub(crate) fn scroll_speed_label(&self) -> String {
        match self.scroll_speed_setting {
            ScrollSpeedSetting::Manual(speed) => format!("{speed:.1}x"),
            ScrollSpeedSetting::VSync => format!("V-Sync ({:.3}x)", self.scroll_speed_vsync),
        }
    }

    pub(crate) fn note_offset_label(&self) -> String {
        format_offset_ms(self.note_offset_ms)
    }

    pub(crate) fn music_offset_label(&self) -> String {
        format_offset_ms(self.music_offset_ms)
    }

    pub(crate) fn total_offset_label(&self) -> String {
        format_offset_ms(self.note_offset_ms.saturating_add(self.music_offset_ms))
    }

    fn handle_song_menu_key(&mut self, key: KeyEvent) -> Result<()> {
        if is_load_warnings_hotkey(key) {
            self.page = Page::LoadWarnings;
            self.load_warnings_scroll = 0;
            return Ok(());
        }

        // 'm' opens multiplayer connect screen
        if matches!(key.code, KeyCode::Char('m'))
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.mp_connect.error = None;
            self.mp_connect.focus = ConnectField::Mode;
            self.page = Page::MultiplayerConnect;
            return Ok(());
        }

        if self.handle_song_menu_search_key(key) {
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit | MenuIntent::Back => self.should_quit = true,
            MenuIntent::Confirm => {
                if self.selected_song().is_some() {
                    self.course_index = 0;
                    self.page = Page::CourseMenu;
                    self.refresh_vsync_scroll_speed()?;
                    self.schedule_demo();
                }
            }
            MenuIntent::Up => {
                self.move_song_selection(-1)?;
                self.play_kat_se()?;
            }
            MenuIntent::Down => {
                self.move_song_selection(1)?;
                self.play_kat_se()?;
            }
            MenuIntent::Left => {
                self.move_song_selection(-1)?;
                self.play_kat_se()?;
            }
            MenuIntent::Right => {
                self.move_song_selection(1)?;
                self.play_kat_se()?;
            }
        }

        Ok(())
    }

    fn handle_load_warnings_key(&mut self, key: KeyEvent) -> Result<()> {
        if is_load_warnings_hotkey(key) {
            self.page = Page::SongMenu;
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit => self.should_quit = true,
            MenuIntent::Back | MenuIntent::Confirm => self.page = Page::SongMenu,
            MenuIntent::Up => {
                self.load_warnings_scroll = self.load_warnings_scroll.saturating_sub(1);
            }
            MenuIntent::Down => {
                self.load_warnings_scroll = self.load_warnings_scroll.saturating_add(1);
            }
            MenuIntent::Left => {
                self.load_warnings_scroll = self.load_warnings_scroll.saturating_sub(10);
            }
            MenuIntent::Right => {
                self.load_warnings_scroll = self.load_warnings_scroll.saturating_add(10);
            }
        }

        Ok(())
    }

    fn handle_song_menu_search_key(&mut self, key: KeyEvent) -> bool {
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } if !self.song_query.is_empty() => {
                self.song_query.clear();
                if let Err(error) = self.rebuild_song_filter() {
                    self.set_error_state(error);
                }
                true
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                if self.song_query.pop().is_some() {
                    if let Err(error) = self.rebuild_song_filter() {
                        self.set_error_state(error);
                    }
                }
                true
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                if !self.song_query.is_empty() {
                    self.song_query.clear();
                    if let Err(error) = self.rebuild_song_filter() {
                        self.set_error_state(error);
                    }
                }
                true
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.song_query.is_empty() {
                    self.song_query.clear();
                    if let Err(error) = self.rebuild_song_filter() {
                        self.set_error_state(error);
                    }
                }
                true
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if !modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT) =>
            {
                self.song_query.push(c);
                if let Err(error) = self.rebuild_song_filter() {
                    self.set_error_state(error);
                }
                true
            }
            _ => false,
        }
    }

    fn handle_course_menu_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Tab) {
            self.course_setting_focus = self.course_setting_focus.next();
            return Ok(());
        }
        if matches!(key.code, KeyCode::BackTab) {
            self.course_setting_focus = self.course_setting_focus.prev();
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit => self.should_quit = true,
            MenuIntent::Back => {
                self.page = Page::SongMenu;
                self.schedule_demo();
            }
            MenuIntent::Confirm => {
                self.play_don_se()?;
                self.start_game()?;
            }
            MenuIntent::Up => {
                self.move_course_selection(-1)?;
                self.play_kat_se()?;
            }
            MenuIntent::Down => {
                self.move_course_selection(1)?;
                self.play_kat_se()?;
            }
            MenuIntent::Left => self.adjust_course_setting(-1)?,
            MenuIntent::Right => self.adjust_course_setting(1)?,
        }

        Ok(())
    }

    fn handle_game_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Esc,
                ..
            }
        ) {
            self.abort_game_to_course()?;
            return Ok(());
        }

        if is_game_pause_toggle_key(key) {
            self.toggle_game_pause()?;
            return Ok(());
        }

        if self.game.as_ref().is_some_and(|game| game.paused) {
            return Ok(());
        }

        let Some(action) = map_game_hit(key) else {
            return Ok(());
        };

        let Some(last_tick) = self.game.as_ref().map(|game| game.last_tick) else {
            return Ok(());
        };

        match action {
            TaikoAction::Don => self.play_don_se()?,
            TaikoAction::Kat => self.play_kat_se()?,
        }

        let tick = self.current_chart_tick(last_tick);
        if let Some(game) = self.game.as_mut() {
            game.pending_inputs.push(TimedInput { tick, action });
        }
        Ok(())
    }

    fn toggle_game_pause(&mut self) -> Result<()> {
        let Some(game) = self.game.as_mut() else {
            return Ok(());
        };

        if game.paused {
            self.audio.resume_song()?;
            game.paused = false;
        } else {
            self.audio.pause_song()?;
            game.paused = true;
            game.input_flash = None;
            game.judge_flash = None;
        }

        Ok(())
    }

    fn handle_result_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) {
            self.page = Page::SongMenu;
            self.result = None;
            self.schedule_demo();
        }

        Ok(())
    }

    fn handle_error_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit => self.should_quit = true,
            MenuIntent::Back | MenuIntent::Confirm => {
                if self.songs.is_empty() {
                    self.should_quit = true;
                } else {
                    self.page = Page::SongMenu;
                    self.error_message = None;
                    self.schedule_demo();
                }
            }
            MenuIntent::Up | MenuIntent::Down | MenuIntent::Left | MenuIntent::Right => {}
        }

        Ok(())
    }

    fn handle_mp_connect_key(&mut self, key: KeyEvent) -> Result<()> {
        let mp = &mut self.mp_connect;

        match key.code {
            KeyCode::Esc => {
                self.page = Page::SongMenu;
                self.schedule_demo();
                return Ok(());
            }
            KeyCode::Up => {
                mp.focus = match mp.focus {
                    ConnectField::Mode => ConnectField::Mode,
                    ConnectField::Server => ConnectField::Mode,
                    ConnectField::RoomCode => ConnectField::Server,
                    ConnectField::Name => {
                        if mp.mode == ConnectMode::Join {
                            ConnectField::RoomCode
                        } else {
                            ConnectField::Server
                        }
                    }
                    ConnectField::Confirm => ConnectField::Name,
                };
                return Ok(());
            }
            KeyCode::Down | KeyCode::Tab => {
                mp.focus = match mp.focus {
                    ConnectField::Mode => ConnectField::Server,
                    ConnectField::Server => {
                        if mp.mode == ConnectMode::Join {
                            ConnectField::RoomCode
                        } else {
                            ConnectField::Name
                        }
                    }
                    ConnectField::RoomCode => ConnectField::Name,
                    ConnectField::Name => ConnectField::Confirm,
                    ConnectField::Confirm => ConnectField::Confirm,
                };
                return Ok(());
            }
            _ => {}
        }

        match mp.focus {
            ConnectField::Mode => match key.code {
                KeyCode::Left | KeyCode::Right => {
                    mp.mode = match mp.mode {
                        ConnectMode::Create => ConnectMode::Join,
                        ConnectMode::Join => ConnectMode::Create,
                    };
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Server;
                }
                _ => {}
            },
            ConnectField::Server => match key.code {
                KeyCode::Char(c) => mp.server.push(c),
                KeyCode::Backspace => {
                    mp.server.pop();
                }
                KeyCode::Enter => {
                    mp.focus = if mp.mode == ConnectMode::Join {
                        ConnectField::RoomCode
                    } else {
                        ConnectField::Name
                    };
                }
                _ => {}
            },
            ConnectField::RoomCode => match key.code {
                KeyCode::Char(c) => mp.room_code.push(c.to_ascii_uppercase()),
                KeyCode::Backspace => {
                    mp.room_code.pop();
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Name;
                }
                _ => {}
            },
            ConnectField::Name => match key.code {
                KeyCode::Char(c) => mp.name.push(c),
                KeyCode::Backspace => {
                    mp.name.pop();
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Confirm;
                }
                _ => {}
            },
            ConnectField::Confirm => {
                if matches!(key.code, KeyCode::Enter) {
                    if mp.server.is_empty() || mp.name.is_empty() {
                        mp.error = Some("Server and Name are required".to_owned());
                        return Ok(());
                    }
                    if mp.mode == ConnectMode::Join && mp.room_code.is_empty() {
                        mp.error = Some("Room Code is required for Join".to_owned());
                        return Ok(());
                    }

                    let action = match mp.mode {
                        ConnectMode::Create => {
                            crate::cli::OnlineAction::Create(crate::cli::OnlineCreateArgs {
                                server: mp.server.clone(),
                                name: mp.name.clone(),
                            })
                        }
                        ConnectMode::Join => {
                            crate::cli::OnlineAction::Join(crate::cli::OnlineJoinArgs {
                                server: mp.server.clone(),
                                room: mp.room_code.clone(),
                                name: mp.name.clone(),
                            })
                        }
                    };

                    match self.connect_online(&action) {
                        Ok(()) => {}
                        Err(e) => {
                            self.mp_connect.error = Some(format!("{e}"));
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // ── Online connection ─────────────────────────────────────────────

    fn connect_online(&mut self, action: &crate::cli::OnlineAction) -> Result<()> {
        let (server, name) = match action {
            crate::cli::OnlineAction::Create(a) => (&a.server, &a.name),
            crate::cli::OnlineAction::Join(a) => (&a.server, &a.name),
            crate::cli::OnlineAction::Spectate(a) => (&a.server, &a.name),
        };

        // Connect WebSocket
        let network = crate::online::NetworkClient::connect(server, name, action)?;

        // Load remote song library
        let endpoint = crate::online::resource_http_endpoint(server)?;
        let remote_backend = ResourceBackend::remote(&endpoint, true)?;
        let library = remote_backend.load_song_library()?;

        // Replace app songs with remote library
        self.songs = library.songs;
        self.filtered_song_indices = (0..self.songs.len()).collect();
        self.song_query.clear();
        self.song_filter_error = None;
        self.song_index = 0;
        self.resource_backend = remote_backend;

        // Stop any playing demo
        self.audio.stop_song()?;
        self.demo_pending = None;
        self.demo_playing_song = None;

        // Create online session
        self.online = Some(crate::online_session::OnlineSession::new(network));
        self.page = Page::OnlineLobby;

        Ok(())
    }

    fn disconnect_online(&mut self) {
        self.online = None;
        self.page = Page::SongMenu;
        // Reload local songs
        let _ = self.audio.stop_song();
        self.demo_pending = None;
        self.demo_playing_song = None;
        // Note: local songs reload would require re-running the loader.
        // For now, the songs list stays as remote. User can restart the app
        // to get local songs back, or we reload here if needed.
    }

    fn process_online_actions(&mut self) {
        let actions = match &mut self.online {
            Some(online) if !online.pending_actions.is_empty() => {
                std::mem::take(&mut online.pending_actions)
            }
            _ => return,
        };

        for action in actions {
            match action {
                crate::online_session::SessionAction::SongSelected {
                    source_id,
                    course_index,
                } => {
                    // Sync song selection: find the song in our list
                    if let Some(idx) = self.songs.iter().position(|s| {
                        crate::online::remote_locator_id(&s.source_locator)
                            .is_some_and(|id| id == source_id)
                    }) {
                        // Update filter to show this song
                        if !self.filtered_song_indices.contains(&idx) {
                            self.song_query.clear();
                            self.filtered_song_indices = (0..self.songs.len()).collect();
                        }
                        if let Some(pos) = self.filtered_song_indices.iter().position(|&i| i == idx)
                        {
                            self.song_index = pos;
                        }
                    }
                    if let Some(online) = &mut self.online {
                        online.host_course_index = course_index;
                        online.local_course_index = 0;
                        online.lobby_sub_state = crate::online::LobbySubState::SelectingCourse;
                    }
                    // Auto-transition to course select page
                    if self.page == Page::OnlineLobby {
                        self.page = Page::OnlineCourseSelect;
                    }
                }
                crate::online_session::SessionAction::PhaseChanged(phase) => match phase {
                    RoomPhase::Countdown | RoomPhase::Playing => {
                        if matches!(self.page, Page::OnlineLobby | Page::OnlineCourseSelect) {
                            self.page = Page::OnlineMatch;
                        }
                    }
                    RoomPhase::Finished => {
                        if self.page == Page::OnlineMatch {
                            self.page = Page::OnlineResult;
                        }
                    }
                    _ => {}
                },
            }
        }
    }

    // ── Online page handlers ─────────────────────────────────────────

    fn tick_online_lobby(&mut self) -> Result<()> {
        self.tick_demo_preview()?;
        // Check if online session got a phase change
        if let Some(online) = &self.online {
            if online.is_disconnected() {
                let msg = online
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "disconnected".to_owned());
                self.online = None;
                self.error_message = Some(msg);
                self.page = Page::Error;
            }
        }
        Ok(())
    }

    fn tick_online_match_phase(&mut self) -> Result<()> {
        // Check phase transitions from server
        if let Some(online) = &self.online {
            let phase = online.current_phase();
            match (self.page, phase) {
                (Page::OnlineCourseSelect, RoomPhase::Countdown | RoomPhase::Playing) => {
                    self.page = Page::OnlineMatch;
                }
                (Page::OnlineMatch, RoomPhase::Finished) => {
                    self.page = Page::OnlineResult;
                }
                _ => {}
            }
            if online.is_disconnected() {
                let msg = online
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "disconnected".to_owned());
                self.online = None;
                self.error_message = Some(msg);
                self.page = Page::Error;
                return Ok(());
            }
        }

        // Prepare the match (load chart/audio) if not yet done
        self.ensure_online_match_prepared()?;
        // Tick the local player engine
        self.tick_online_player_runtime()?;

        Ok(())
    }

    fn ensure_online_match_prepared(&mut self) -> Result<()> {
        let Some(online) = &self.online else {
            return Ok(());
        };
        let Some(snapshot) = online.snapshot.as_ref() else {
            return Ok(());
        };
        let Some(song_sel) = snapshot.song.as_ref() else {
            return Ok(());
        };

        // Already prepared for this song?
        if online
            .prepared_match
            .as_ref()
            .is_some_and(|p| p.selection == *song_sel)
        {
            return Ok(());
        }

        // Find the song in our local library
        let song_sel = song_sel.clone();
        let (song_idx, course_idx) = self
            .songs
            .iter()
            .enumerate()
            .find_map(|(idx, song)| {
                let source_id = crate::online::remote_locator_id(&song.source_locator)?;
                if source_id != song_sel.source_id {
                    return None;
                }
                if song
                    .courses
                    .iter()
                    .any(|c| c.index == song_sel.course_index)
                {
                    Some((idx, song_sel.course_index))
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                anyhow!(
                    "selected song not found: source_id={}, course={}",
                    song_sel.source_id,
                    song_sel.course_index
                )
            })?;

        let song_entry = &self.songs[song_idx];
        let chart = self
            .resource_backend
            .load_course_chart(song_entry, course_idx, &rhythm_importer_tja::TjaImporter)
            .context("failed to load online match chart")?;
        let audio_source = self
            .resource_backend
            .load_song_audio(song_entry)
            .context("failed to load online match audio")?;

        let course = song_entry
            .courses
            .get(course_idx)
            .ok_or_else(|| anyhow!("course index out of range"))?;

        if let Some(online) = &mut self.online {
            online.prepared_match = Some(crate::online::PreparedMatch {
                selection: song_sel,
                branch_decisions: course.branch_decisions.clone(),
                chart,
                audio_source,
            });
            online.local_player = None;
        }

        Ok(())
    }

    fn tick_online_player_runtime(&mut self) -> Result<()> {
        let Some(online) = &mut self.online else {
            return Ok(());
        };

        let phase = online.current_phase();
        if !matches!(
            phase,
            RoomPhase::Countdown | RoomPhase::Playing | RoomPhase::Finished
        ) {
            online.local_player = None;
            return Ok(());
        }

        let Some(prepared) = online.prepared_match.as_ref() else {
            return Ok(());
        };

        // Initialize the local player runtime if needed
        if online.local_player.is_none() {
            let mut engine =
                ControlledEngine::<rhythm_mode_taiko::TaikoMode>::new_controlled(&prepared.chart)?;
            let initial = engine
                .step_to_with_controls(0, &[], &[])
                .context("failed to bootstrap online engine")?;
            online.local_player = Some(crate::online::LocalPlayerRuntime {
                engine,
                branch_controller: BranchController::new(
                    crate::cli::BranchPolicy::Auto,
                    0,
                    prepared.branch_decisions.clone(),
                ),
                pending_inputs: Vec::new(),
                input_seq: 0,
                state_seq: 0,
                final_seq: 0,
                last_tick: 0,
                last_output: initial,
                sent_final: false,
                music_started: false,
                judge_flash: None,
                input_flash: None,
            });
        }

        let now_tick = online.estimated_server_tick().max(0);

        // Start music when playing
        if matches!(phase, RoomPhase::Playing | RoomPhase::Finished) {
            let runtime = online.local_player.as_ref().unwrap();
            if !runtime.music_started {
                let start_seconds = (now_tick as f64 / 1_000_000.0).max(0.0);
                let audio_source = prepared.audio_source.clone();
                self.audio.stop_song()?;
                self.audio.play_song(audio_source, start_seconds, false)?;
                let runtime = online.local_player.as_mut().unwrap();
                runtime.music_started = true;
            }
        }

        let runtime = online.local_player.as_mut().unwrap();
        let frame_tick = now_tick.max(runtime.last_tick);
        let controls = runtime
            .branch_controller
            .controls_for_tick(frame_tick, runtime.engine.score())
            .map_err(|error| anyhow!(error))?;
        let frame_inputs =
            crate::online::collect_due_inputs(&mut runtime.pending_inputs, frame_tick);

        if let Some(input) = frame_inputs.last().copied() {
            runtime.input_flash = Some(InputFlashState {
                action: input.action,
                until_tick: frame_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        let output = runtime
            .engine
            .step_to_with_controls(frame_tick, &controls, &frame_inputs)
            .context("online player engine step failed")?;

        if let Some(judge) = latest_flashable_judge(&output.judges) {
            runtime.judge_flash = Some(JudgeFlashState {
                judge,
                until_tick: frame_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        let replay_hash = output.replay_hash;
        let recent_judges = output.judges.clone();
        let state_score = output.score.clone();
        let state_frame_view = output.frame_view.clone();
        let frame_finished = output.finished;
        runtime.last_tick = frame_tick;
        runtime.last_output = output;
        runtime.state_seq = runtime.state_seq.saturating_add(1);

        if runtime
            .input_flash
            .is_some_and(|flash| runtime.last_output.now > flash.until_tick)
        {
            runtime.input_flash = None;
        }
        if runtime
            .judge_flash
            .is_some_and(|flash| runtime.last_output.now > flash.until_tick)
        {
            runtime.judge_flash = None;
        }

        online.network.send(
            taiko_multiplayer_protocol::ClientMessage::PlayerStateUpdate(
                taiko_multiplayer_protocol::PlayerStateUpdate {
                    seq: runtime.state_seq,
                    now_tick: frame_tick,
                    score: state_score,
                    frame_view: state_frame_view,
                    recent_judges,
                    replay_hash,
                },
            ),
        )?;

        if frame_finished && !runtime.sent_final {
            runtime.final_seq = runtime.final_seq.saturating_add(1);
            let final_result = runtime.engine.finalize();
            online
                .network
                .send(taiko_multiplayer_protocol::ClientMessage::FinalResult(
                    taiko_multiplayer_protocol::FinalResultReport {
                        seq: runtime.final_seq,
                        finish_tick: frame_tick,
                        replay_hash,
                        result: final_result,
                    },
                ))?;
            runtime.sent_final = true;
        }

        Ok(())
    }

    fn handle_online_lobby_key(&mut self, key: KeyEvent) -> Result<()> {
        // Search key handling (same as song menu)
        if self.handle_song_menu_search_key(key) {
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit | MenuIntent::Back => {
                self.disconnect_online();
            }
            MenuIntent::Up | MenuIntent::Left => {
                let _ = self.move_song_selection(-1);
            }
            MenuIntent::Down | MenuIntent::Right => {
                let _ = self.move_song_selection(1);
            }
            MenuIntent::Confirm => {
                let is_host = self.online.as_ref().is_some_and(|o| o.is_local_host());
                if is_host {
                    if let Some(song) = self.selected_song() {
                        if let Some(source_id) =
                            crate::online::remote_locator_id(&song.source_locator)
                        {
                            let source_id = source_id.to_owned();
                            if let Some(online) = &self.online {
                                let _ = online.network.send(
                                    taiko_multiplayer_protocol::ClientMessage::HostSelectSong(
                                        taiko_multiplayer_protocol::HostSelectSongRequest {
                                            source_id,
                                            course_index: 0,
                                        },
                                    ),
                                );
                            }
                            if let Some(online) = &mut self.online {
                                online.local_course_index = 0;
                                online.lobby_sub_state =
                                    crate::online::LobbySubState::SelectingCourse;
                            }
                            self.page = Page::OnlineCourseSelect;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn handle_online_course_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        let course_len = self.selected_song().map(|s| s.courses.len()).unwrap_or(0);

        match intent {
            MenuIntent::Back => {
                // Cancel ready if was ready, go back to lobby
                if let Some(online) = &mut self.online {
                    if online.ready {
                        online.ready = false;
                        let _ =
                            online
                                .network
                                .send(taiko_multiplayer_protocol::ClientMessage::Ready(
                                    taiko_multiplayer_protocol::ReadyRequest { ready: false },
                                ));
                    }
                    online.lobby_sub_state = crate::online::LobbySubState::BrowsingSongs;
                }
                self.page = Page::OnlineLobby;
            }
            MenuIntent::Quit => {
                self.disconnect_online();
            }
            MenuIntent::Up | MenuIntent::Left => {
                if let Some(online) = &mut self.online {
                    if course_len > 0 {
                        if online.local_course_index == 0 {
                            online.local_course_index = course_len - 1;
                        } else {
                            online.local_course_index -= 1;
                        }
                    }
                }
            }
            MenuIntent::Down | MenuIntent::Right => {
                if let Some(online) = &mut self.online {
                    if course_len > 0 {
                        online.local_course_index = (online.local_course_index + 1) % course_len;
                    }
                }
            }
            MenuIntent::Confirm => {
                // Confirm course → auto-ready
                if let Some(online) = &mut self.online {
                    if online.role == Some(taiko_multiplayer_protocol::RoomRole::Player)
                        && !online.ready
                    {
                        online.ready = true;
                        let _ =
                            online
                                .network
                                .send(taiko_multiplayer_protocol::ClientMessage::Ready(
                                    taiko_multiplayer_protocol::ReadyRequest { ready: true },
                                ));
                    }
                }
            }
        }

        Ok(())
    }

    fn handle_online_match_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) {
            self.disconnect_online();
            return Ok(());
        }

        // Forward game inputs (don/kat)
        let Some(online) = &mut self.online else {
            return Ok(());
        };
        let phase = online.current_phase();
        if !matches!(phase, RoomPhase::Countdown | RoomPhase::Playing) {
            return Ok(());
        }
        if let Some(action) = map_game_hit(key) {
            let tick = online.estimated_server_tick().max(0);
            if let Some(runtime) = online.local_player.as_mut() {
                runtime.pending_inputs.push(TimedInput { tick, action });
                runtime.pending_inputs.sort_by_key(|i| i.tick);
                runtime.input_seq = runtime.input_seq.saturating_add(1);

                online
                    .network
                    .send(taiko_multiplayer_protocol::ClientMessage::InputEvent(
                        taiko_multiplayer_protocol::InputEvent {
                            seq: runtime.input_seq,
                            tick,
                            action,
                        },
                    ))?;
            }

            match action {
                TaikoAction::Don => self.audio.play_don()?,
                TaikoAction::Kat => self.audio.play_kat()?,
            }
        }

        Ok(())
    }

    fn handle_online_result_key(&mut self, _key: KeyEvent) -> Result<()> {
        // Any key goes back to lobby
        self.page = Page::OnlineLobby;
        if let Some(online) = &mut self.online {
            online.ready = false;
            online.lobby_sub_state = crate::online::LobbySubState::BrowsingSongs;
            online.prepared_match = None;
            online.local_player = None;
            online.live_states.clear();
            online.final_results.clear();
        }
        Ok(())
    }

    fn start_game(&mut self) -> Result<()> {
        self.refresh_vsync_scroll_speed()?;
        self.ensure_selected_course_chart_loaded()?;
        let selected_song_index = self
            .selected_song_index()
            .ok_or_else(|| anyhow!("no selected song"))?;
        let (audio_source, course_name, branch_decisions, engine, initial_output, autoplay_inputs) = {
            let song = self
                .selected_song()
                .ok_or_else(|| anyhow!("no selected song"))?;
            let course = song
                .courses
                .get(self.course_index)
                .ok_or_else(|| anyhow!("no selected course"))?;
            let chart = self
                .selected_course_chart()
                .ok_or_else(|| anyhow!("selected course chart is not loaded"))?;

            let mut engine = ControlledEngine::<TaikoMode>::new_controlled(chart)?;
            let initial_output = engine
                .step_to_with_controls(0, &[], &[])
                .context("failed to bootstrap game frame")?;
            let autoplay_inputs = if self.auto_play {
                build_autoplay_events(chart)
            } else {
                Vec::new()
            };
            let audio_source = self.resource_backend.load_song_audio(song)?;

            (
                audio_source,
                course.name.clone(),
                course.branch_decisions.clone(),
                engine,
                initial_output,
                autoplay_inputs,
            )
        };

        self.audio.stop_song()?;
        self.audio.play_song(audio_source, 0.0, false)?;

        let branch_controller =
            BranchController::new(self.branch_policy, self.fixed_route, branch_decisions);

        self.perf_meter.clear();
        self.demo_pending = None;
        self.demo_playing_song = None;
        self.page = Page::Game;
        self.result = None;

        self.game = Some(GameSession {
            song_index: selected_song_index,
            course_name,
            engine,
            branch_controller,
            last_output: initial_output,
            last_judge: None,
            last_tick: 0,
            autoplay_inputs,
            autoplay_cursor: 0,
            pending_inputs: Vec::with_capacity(32),
            timing_samples: Vec::with_capacity(4096),
            judge_flash: None,
            input_flash: None,
            result_delay_deadline: None,
            paused: false,
        });

        Ok(())
    }

    fn abort_game_to_course(&mut self) -> Result<()> {
        self.audio.stop_song()?;
        self.game = None;
        self.page = Page::CourseMenu;
        self.schedule_demo();
        Ok(())
    }

    fn tick_demo_preview(&mut self) -> Result<()> {
        if !self.args.demo {
            return Ok(());
        }

        let Some((deadline, song_index)) = self.demo_pending else {
            return Ok(());
        };

        if Instant::now() < deadline {
            return Ok(());
        }

        if self.selected_song_index() != Some(song_index) {
            return Ok(());
        }

        if self.demo_playing_song == Some(song_index) {
            return Ok(());
        }

        let song = self
            .songs
            .get(song_index)
            .ok_or_else(|| anyhow!("invalid demo song index {song_index}"))?;
        let audio_source = self.resource_backend.load_song_audio(song)?;

        self.audio
            .play_song(audio_source, song.demo_start_seconds, true)?;
        self.demo_playing_song = Some(song_index);
        self.demo_pending = None;
        Ok(())
    }

    fn tick_game(&mut self) -> Result<()> {
        let mut game = self
            .game
            .take()
            .ok_or_else(|| anyhow!("game state is missing"))?;

        if game.paused {
            self.game = Some(game);
            return Ok(());
        }

        let now_tick = self.current_chart_tick(game.last_tick);

        // Keep the UI clock/projection moving after chart finish, but wait for
        // the song playback to end before entering the result screen.
        if game.last_output.finished {
            let output = game
                .engine
                .step_to_with_controls(now_tick, &[], &[])
                .context("engine step failed while waiting for song end")?;
            game.last_tick = now_tick;
            game.last_output = output;
            if game
                .input_flash
                .is_some_and(|flash| game.last_output.now > flash.until_tick)
            {
                game.input_flash = None;
            }
            if game
                .judge_flash
                .is_some_and(|flash| game.last_output.now > flash.until_tick)
            {
                game.judge_flash = None;
            }

            return self.finish_game_when_audio_done(game);
        }

        let controls = game
            .branch_controller
            .controls_for_tick(now_tick, game.engine.score())
            .map_err(|error| anyhow!(error))?;

        let mut frame_inputs = collect_due_inputs(&mut game.pending_inputs, now_tick);
        let auto_start = frame_inputs.len();
        collect_autoplay_inputs(
            &game.autoplay_inputs,
            &mut game.autoplay_cursor,
            now_tick,
            &game.branch_controller,
            &mut frame_inputs,
        );
        for input in &frame_inputs[auto_start..] {
            match input.action {
                TaikoAction::Don => self.play_don_se()?,
                TaikoAction::Kat => self.play_kat_se()?,
            }
        }

        frame_inputs.sort_by_key(|input| input.tick);
        if let Some(input) = frame_inputs.last().copied() {
            game.input_flash = Some(InputFlashState {
                action: input.action,
                until_tick: now_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        let start = Instant::now();
        let output = game
            .engine
            .step_to_with_controls(now_tick, &controls, &frame_inputs)
            .context("engine step failed")?;
        self.perf_meter.record_tick(start.elapsed());

        game.last_tick = now_tick;
        game.last_judge = latest_non_ignored_judge(&output.judges);
        record_timing_samples(&mut game.timing_samples, &output.judges);
        if let Some(judge) = latest_flashable_judge(&output.judges) {
            game.judge_flash = Some(JudgeFlashState {
                judge,
                until_tick: now_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }
        game.last_output = output;
        if game
            .input_flash
            .is_some_and(|flash| game.last_output.now > flash.until_tick)
        {
            game.input_flash = None;
        }
        if game
            .judge_flash
            .is_some_and(|flash| game.last_output.now > flash.until_tick)
        {
            game.judge_flash = None;
        }

        if game.last_output.finished {
            self.finish_game_when_audio_done(game)?;
        } else {
            self.game = Some(game);
        }

        Ok(())
    }

    fn finish_game_with_session(&mut self, game: GameSession) -> Result<()> {
        self.audio.stop_song()?;

        let song = self
            .songs
            .get(game.song_index)
            .ok_or_else(|| anyhow!("invalid song index at result"))?;

        self.result = Some(ResultState {
            title: song.title.clone(),
            subtitle: song.subtitle.clone(),
            course_name: game.course_name,
            final_result: game.engine.finalize(),
            replay_hash: game.engine.replay_hash(),
            branch_controls: game.branch_controller.emitted_controls(),
            timing_samples: game.timing_samples,
            perf: self.perf_meter.snapshot(),
        });
        self.page = Page::Result;
        self.game = None;
        Ok(())
    }

    fn finish_game_when_audio_done(&mut self, mut game: GameSession) -> Result<()> {
        if !self.audio.is_song_finished() {
            game.result_delay_deadline = None;
            self.game = Some(game);
            return Ok(());
        }

        let now = Instant::now();
        let deadline = game.result_delay_deadline.get_or_insert(now + RESULT_DELAY);

        if now >= *deadline {
            self.finish_game_with_session(game)?;
        } else {
            self.game = Some(game);
        }

        Ok(())
    }

    fn current_chart_tick(&self, min_tick: Tick) -> Tick {
        let song_seconds = self.audio.song_position_seconds();
        let total_offset_seconds =
            f64::from(self.note_offset_ms.saturating_add(self.music_offset_ms)) / 1000.0;
        let chart_seconds = (song_seconds - total_offset_seconds).max(0.0);
        let tick = ticks_from_seconds(chart_seconds);
        tick.max(min_tick)
    }

    fn move_song_selection(&mut self, delta: i32) -> Result<()> {
        if self.filtered_song_indices.is_empty() {
            return Ok(());
        }

        let len = self.filtered_song_indices.len() as i32;
        let mut next = self.song_index as i32 + delta;
        if next < 0 {
            next += len;
        }
        self.song_index = (next % len) as usize;
        self.course_index = 0;
        self.schedule_demo();
        self.refresh_vsync_scroll_speed()?;
        Ok(())
    }

    fn move_course_selection(&mut self, delta: i32) -> Result<()> {
        let Some(song) = self.selected_song() else {
            return Ok(());
        };
        if song.courses.is_empty() {
            return Ok(());
        }

        let len = song.courses.len() as i32;
        let mut next = self.course_index as i32 + delta;
        if next < 0 {
            next += len;
        }
        self.course_index = (next % len) as usize;
        self.refresh_vsync_scroll_speed()?;
        Ok(())
    }

    fn adjust_course_setting(&mut self, delta: i32) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }

        match self.course_setting_focus {
            CourseSettingFocus::AutoPlay => {
                self.auto_play = delta > 0;
            }
            CourseSettingFocus::SongVolume => {
                let next = (i32::from(self.args.songvol) + delta).clamp(0, 100) as u8;
                self.args.songvol = next;
                self.audio.set_song_volume(next);
            }
            CourseSettingFocus::SeVolume => {
                let next = (i32::from(self.args.sevol) + delta).clamp(0, 100) as u8;
                self.args.sevol = next;
                self.audio.set_se_volume(next);
            }
            CourseSettingFocus::NoteOffset => {
                self.note_offset_ms = adjust_offset_ms(self.note_offset_ms, delta);
            }
            CourseSettingFocus::MusicOffset => {
                self.music_offset_ms = adjust_offset_ms(self.music_offset_ms, delta);
            }
            CourseSettingFocus::ScrollSpeed => {
                self.scroll_speed_setting =
                    cycle_scroll_speed_setting(self.scroll_speed_setting, delta);
            }
        }
        Ok(())
    }

    fn schedule_demo(&mut self) {
        if !self.args.demo || self.filtered_song_indices.is_empty() {
            self.demo_pending = None;
            self.demo_playing_song = None;
            let _ = self.audio.stop_song();
            return;
        }

        let Some(selected_song_index) = self.selected_song_index() else {
            self.demo_pending = None;
            self.demo_playing_song = None;
            let _ = self.audio.stop_song();
            return;
        };

        // Keep current preview running when the selected song does not change,
        // e.g. Song Menu -> Course Menu transition for the same song.
        if self.demo_playing_song == Some(selected_song_index) && !self.audio.is_song_finished() {
            self.demo_pending = None;
            return;
        }

        self.demo_pending = Some((Instant::now() + DEMO_DELAY, selected_song_index));
        self.demo_playing_song = None;
        let _ = self.audio.stop_song();
    }

    fn rebuild_song_filter(&mut self) -> Result<()> {
        let previous_selected = self.selected_song_index();

        match SongFilter::parse(&self.song_query) {
            Ok(filter) => {
                self.song_filter_error = None;
                self.filtered_song_indices = self
                    .songs
                    .iter()
                    .enumerate()
                    .filter_map(|(index, song)| {
                        filter.matches(song, &self.args.songdir).then_some(index)
                    })
                    .collect();
            }
            Err(error) => {
                self.song_filter_error = Some(error);
                self.filtered_song_indices.clear();
            }
        }

        if let Some(previous) = previous_selected {
            if let Some(next_pos) = self
                .filtered_song_indices
                .iter()
                .position(|index| *index == previous)
            {
                self.song_index = next_pos;
            } else {
                self.song_index = 0;
                self.course_index = 0;
            }
        } else {
            self.song_index = 0;
            self.course_index = 0;
        }

        if self.song_index >= self.filtered_song_indices.len() {
            self.song_index = self.filtered_song_indices.len().saturating_sub(1);
        }

        if previous_selected != self.selected_song_index() {
            self.course_index = 0;
            self.schedule_demo();
        } else if self.filtered_song_indices.is_empty() {
            self.schedule_demo();
        }

        self.refresh_vsync_scroll_speed()?;
        Ok(())
    }

    fn refresh_vsync_scroll_speed(&mut self) -> Result<()> {
        // V-Sync speed is only used in Course/Game pages. Avoid chart IO/parsing
        // while browsing Song Menu or Load Warnings to keep startup/navigation fast.
        if !matches!(self.page, Page::CourseMenu | Page::Game) {
            self.scroll_speed_vsync = 1.0;
            return Ok(());
        }

        let projection_span =
            crate::screen::game_screen::projection_span_for_viewport_width(self.viewport_width);
        if self.selected_course().is_none() {
            self.scroll_speed_vsync = 1.0;
            self.loaded_course_chart = None;
            return Ok(());
        }

        self.ensure_selected_course_chart_loaded()?;
        self.scroll_speed_vsync = self
            .selected_course_chart()
            .map(|chart| compute_vsync_scroll_speed(chart, projection_span))
            .unwrap_or(1.0);
        Ok(())
    }

    fn ensure_selected_course_chart_loaded(&mut self) -> Result<()> {
        let Some(song_index) = self.selected_song_index() else {
            self.loaded_course_chart = None;
            return Ok(());
        };
        self.ensure_course_chart_loaded(song_index, self.course_index)
    }

    fn ensure_course_chart_loaded(&mut self, song_index: usize, course_index: usize) -> Result<()> {
        if self.loaded_course_chart.as_ref().is_some_and(|loaded| {
            loaded.song_index == song_index && loaded.course_index == course_index
        }) {
            return Ok(());
        }

        let song = self
            .songs
            .get(song_index)
            .ok_or_else(|| anyhow!("invalid song index {song_index}"))?;
        let importer = rhythm_importer_tja::TjaImporter;
        let chart = self
            .resource_backend
            .load_course_chart(song, course_index, &importer)?;

        self.loaded_course_chart = Some(LoadedCourseChart {
            song_index,
            course_index,
            chart,
        });
        Ok(())
    }

    fn set_error_state(&mut self, error: anyhow::Error) {
        self.page = Page::Error;
        self.error_message = Some(error.to_string());
        self.result = None;
        self.game = None;
        self.loaded_course_chart = None;
        let _ = self.audio.stop_song();
    }

    fn play_don_se(&mut self) -> Result<()> {
        self.audio.play_don()
    }

    fn play_kat_se(&mut self) -> Result<()> {
        self.audio.play_kat()
    }
}

fn cycle_scroll_speed_setting(current: ScrollSpeedSetting, delta: i32) -> ScrollSpeedSetting {
    if delta == 0 {
        return current;
    }

    let total_slots = SCROLL_SPEED_VSYNC_SLOT + 1;
    let current_slot = match current {
        ScrollSpeedSetting::Manual(speed) => scroll_speed_to_units(speed) - SCROLL_SPEED_MIN_UNITS,
        ScrollSpeedSetting::VSync => SCROLL_SPEED_VSYNC_SLOT,
    };

    let next_slot = (current_slot + delta).rem_euclid(total_slots);
    if next_slot == SCROLL_SPEED_VSYNC_SLOT {
        ScrollSpeedSetting::VSync
    } else {
        let units = next_slot + SCROLL_SPEED_MIN_UNITS;
        ScrollSpeedSetting::Manual(units as f32 * SCROLL_SPEED_STEP)
    }
}

fn scroll_speed_to_units(speed: f32) -> i32 {
    ((speed / SCROLL_SPEED_STEP).round() as i32)
        .clamp(SCROLL_SPEED_MIN_UNITS, SCROLL_SPEED_MAX_UNITS)
}

fn adjust_offset_ms(current: i32, delta: i32) -> i32 {
    let step = delta.signum() * OFFSET_STEP_MS;
    (current + step).clamp(OFFSET_MIN_MS, OFFSET_MAX_MS)
}

fn quantize_offset_ms(seconds: f64) -> i32 {
    let raw_ms = (seconds * 1000.0).round() as i32;
    let units = ((raw_ms as f64) / (OFFSET_STEP_MS as f64)).round() as i32;
    (units * OFFSET_STEP_MS).clamp(OFFSET_MIN_MS, OFFSET_MAX_MS)
}

fn format_offset_ms(ms: i32) -> String {
    format!("{:+}ms", ms)
}

fn is_load_warnings_hotkey(key: KeyEvent) -> bool {
    matches!(
        key,
        KeyEvent {
            code: KeyCode::Char('w' | 'W'),
            modifiers,
            ..
        } if modifiers.contains(KeyModifiers::CONTROL)
    )
}

pub(crate) fn compute_vsync_scroll_speed(chart: &CanonicalChart, projection_span: usize) -> f32 {
    if projection_span == 0 {
        return VSYNC_SPEED_MIN;
    }

    let q_max = (LOOKAHEAD_TICKS / projection_span as Tick).max(1);
    let q_min = ((LOOKAHEAD_TICKS as f64) / (projection_span as f64 * VSYNC_SPEED_MAX as f64))
        .ceil() as Tick;
    let q_min = q_min.max(1);
    if q_min > q_max {
        return VSYNC_SPEED_MIN;
    }

    let histogram = build_interval_histogram(chart);
    if histogram.is_empty() {
        return VSYNC_SPEED_MIN;
    }

    let mut candidates = Vec::new();
    candidates.push(q_min);
    candidates.push(q_max);

    let mut ranked_diffs = histogram
        .iter()
        .map(|(diff, count)| (*diff, *count))
        .collect::<Vec<_>>();
    ranked_diffs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut gcd_all = 0_i64;
    for (diff, _) in &ranked_diffs {
        gcd_all = gcd_ticks(gcd_all, *diff);
    }
    if gcd_all > 0 {
        collect_divisor_candidates(gcd_all, q_min, q_max, &mut candidates);
    }

    for (diff, _) in ranked_diffs.iter().take(VSYNC_CANDIDATE_TOP_INTERVALS) {
        collect_divisor_candidates(*diff, q_min, q_max, &mut candidates);
    }

    candidates.sort_unstable();
    candidates.dedup();

    let mut best_q = q_max;
    let mut best_exact = 0_u64;
    let mut best_penalty = u128::MAX;

    for q in candidates {
        let mut exact = 0_u64;
        let mut penalty = 0_u128;

        for (diff, count) in &histogram {
            let remainder = diff % q;
            if remainder == 0 {
                exact += u64::from(*count);
            }
            let cost = remainder.min(q - remainder) as u128;
            penalty += cost * u128::from(*count);
        }

        if exact > best_exact
            || (exact == best_exact
                && (penalty < best_penalty || (penalty == best_penalty && q > best_q)))
        {
            best_q = q;
            best_exact = exact;
            best_penalty = penalty;
        }
    }

    let speed = LOOKAHEAD_TICKS as f64 / (projection_span as f64 * best_q as f64);
    speed.clamp(VSYNC_SPEED_MIN as f64, VSYNC_SPEED_MAX as f64) as f32
}

fn build_interval_histogram(chart: &CanonicalChart) -> HashMap<Tick, u32> {
    let mut histogram = HashMap::<Tick, u32>::new();
    let mut previous_tick: Option<Tick> = None;
    let mut object_idx = 0_usize;
    let mut tempo_idx = 0_usize;
    let mut signature_idx = 0_usize;

    let mut next_object = next_unique_object_start_tick(&chart.objects, &mut object_idx);
    let mut next_tempo = chart.tempo_map.get(tempo_idx).map(|tempo| tempo.tick);
    let mut next_signature = chart.signatures.get(signature_idx).map(|sig| sig.tick);

    loop {
        let mut next_tick = Tick::MAX;
        let mut has_next = false;

        if let Some(tick) = next_object {
            next_tick = next_tick.min(tick);
            has_next = true;
        }
        if let Some(tick) = next_tempo {
            next_tick = next_tick.min(tick);
            has_next = true;
        }
        if let Some(tick) = next_signature {
            next_tick = next_tick.min(tick);
            has_next = true;
        }

        if !has_next {
            break;
        }

        if let Some(prev) = previous_tick {
            add_interval(&mut histogram, next_tick.saturating_sub(prev));
        }
        previous_tick = Some(next_tick);

        if next_object == Some(next_tick) {
            next_object = next_unique_object_start_tick(&chart.objects, &mut object_idx);
        }
        if next_tempo == Some(next_tick) {
            while chart
                .tempo_map
                .get(tempo_idx)
                .is_some_and(|tempo| tempo.tick == next_tick)
            {
                tempo_idx += 1;
            }
            next_tempo = chart.tempo_map.get(tempo_idx).map(|tempo| tempo.tick);
        }
        if next_signature == Some(next_tick) {
            while chart
                .signatures
                .get(signature_idx)
                .is_some_and(|sig| sig.tick == next_tick)
            {
                signature_idx += 1;
            }
            next_signature = chart.signatures.get(signature_idx).map(|sig| sig.tick);
        }
    }

    for object in &chart.objects {
        add_interval(
            &mut histogram,
            object.end_tick.saturating_sub(object.start_tick),
        );
    }

    histogram
}

fn next_unique_object_start_tick(objects: &[Object], index: &mut usize) -> Option<Tick> {
    let first = objects.get(*index)?;
    let tick = first.start_tick;
    *index += 1;
    while objects
        .get(*index)
        .is_some_and(|object| object.start_tick == tick)
    {
        *index += 1;
    }
    Some(tick)
}

fn add_interval(histogram: &mut HashMap<Tick, u32>, delta: Tick) {
    if delta <= 0 {
        return;
    }
    let entry = histogram.entry(delta).or_insert(0);
    *entry = entry.saturating_add(1);
}

fn collect_divisor_candidates(value: Tick, min_q: Tick, max_q: Tick, out: &mut Vec<Tick>) {
    if value <= 0 {
        return;
    }

    let mut divisor = 1_i64;
    while divisor * divisor <= value {
        if value % divisor == 0 {
            let pair = value / divisor;
            if (min_q..=max_q).contains(&divisor) {
                out.push(divisor);
            }
            if pair != divisor && (min_q..=max_q).contains(&pair) {
                out.push(pair);
            }
        }
        divisor += 1;
    }
}

fn gcd_ticks(mut lhs: Tick, mut rhs: Tick) -> Tick {
    lhs = lhs.abs();
    rhs = rhs.abs();
    if lhs == 0 {
        return rhs;
    }
    if rhs == 0 {
        return lhs;
    }
    while rhs != 0 {
        let r = lhs % rhs;
        lhs = rhs;
        rhs = r;
    }
    lhs
}

fn collect_due_inputs(
    pending: &mut Vec<TimedInput<TaikoAction>>,
    now_tick: Tick,
) -> Vec<TimedInput<TaikoAction>> {
    let mut due = Vec::with_capacity(pending.len());
    let mut remaining = Vec::with_capacity(pending.len());

    for input in pending.drain(..) {
        if input.tick <= now_tick {
            due.push(input);
        } else {
            remaining.push(input);
        }
    }

    *pending = remaining;
    due
}

fn collect_autoplay_inputs(
    inputs: &[AutoplayInputEvent],
    cursor: &mut usize,
    now_tick: Tick,
    branch_controller: &BranchController,
    out: &mut Vec<TimedInput<TaikoAction>>,
) {
    while *cursor < inputs.len() && inputs[*cursor].input.tick <= now_tick {
        let event = inputs[*cursor];
        if autoplay_event_enabled(event, branch_controller) {
            out.push(event.input);
        }
        *cursor += 1;
    }
}

fn autoplay_event_enabled(event: AutoplayInputEvent, branch_controller: &BranchController) -> bool {
    let Some(segment_id) = event.branch_segment_id else {
        return true;
    };

    event.branch_route_id == branch_controller.route_for_tick(segment_id, event.input.tick)
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

fn record_timing_samples(out: &mut Vec<TimingSample>, judges: &[TaikoJudge]) {
    for judge in judges {
        let kind = judge.kind();
        let Some(delta_tick) = judge.timing_delta_tick() else {
            continue;
        };
        if matches!(
            kind,
            TaikoJudgeKind::Great | TaikoJudgeKind::Ok | TaikoJudgeKind::Miss
        ) {
            out.push(TimingSample {
                judge: kind,
                delta_tick,
            });
        }
    }
}

fn build_autoplay_events(chart: &rhythm_chart::CanonicalChart) -> Vec<AutoplayInputEvent> {
    let mut inputs = Vec::new();

    for object in &chart.objects {
        match object.kind {
            ObjectKind::Tap => {
                let lane = match object.lane_or_region {
                    rhythm_chart::LaneOrRegion::Lane(v) | rhythm_chart::LaneOrRegion::Region(v) => {
                        v
                    }
                    rhythm_chart::LaneOrRegion::None => continue,
                };
                let action = if lane == LANE_KAT {
                    TaikoAction::Kat
                } else {
                    TaikoAction::Don
                };
                inputs.push(AutoplayInputEvent {
                    input: TimedInput {
                        tick: object.start_tick,
                        action,
                    },
                    branch_segment_id: object.branch_segment_id,
                    branch_route_id: object.branch_route_id,
                });
            }
            ObjectKind::Roll | ObjectKind::Hold => {
                let mut tick = object.start_tick;
                let max_hits = usize::from(object.required_hits);
                let mut emitted_hits = 0_usize;

                while tick <= object.end_tick && (max_hits == 0 || emitted_hits < max_hits) {
                    inputs.push(AutoplayInputEvent {
                        input: TimedInput {
                            tick,
                            action: TaikoAction::Don,
                        },
                        branch_segment_id: object.branch_segment_id,
                        branch_route_id: object.branch_route_id,
                    });
                    emitted_hits = emitted_hits.saturating_add(1);
                    let micros_per_quarter = tempo_micros_per_quarter_at(&chart.tempo_map, tick);
                    let interval = autoplay_roll_interval_ticks(micros_per_quarter);
                    tick = tick.saturating_add(interval);
                }
            }
            ObjectKind::Slide | ObjectKind::Touch => {}
        }
    }

    inputs.sort_by_key(|event| event.input.tick);
    inputs
}

#[cfg(test)]
pub(crate) fn build_autoplay_inputs(
    chart: &rhythm_chart::CanonicalChart,
) -> Vec<TimedInput<TaikoAction>> {
    build_autoplay_events(chart)
        .into_iter()
        .map(|event| event.input)
        .collect()
}

fn autoplay_roll_interval_ticks(micros_per_quarter: u32) -> Tick {
    (i64::from(micros_per_quarter) / 8).max(1)
}

fn tempo_micros_per_quarter_at(tempo_map: &[TempoChange], tick: Tick) -> u32 {
    if tempo_map.is_empty() {
        return 500_000;
    }
    let idx = tempo_map.partition_point(|tempo| tempo.tick <= tick);
    if idx == 0 {
        tempo_map[0].micros_per_quarter
    } else {
        tempo_map[idx - 1].micros_per_quarter
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhythm_chart::{
        CanonicalChart, ChartMetadata, LaneOrRegion, Object, ObjectKind, TempoChange,
        TimeSignatureChange,
    };
    use rhythm_importer_tja::BranchDecisionPoint;

    #[test]
    fn due_input_collection_keeps_future_inputs() {
        let mut pending = vec![
            TimedInput {
                tick: 10,
                action: TaikoAction::Don,
            },
            TimedInput {
                tick: 20,
                action: TaikoAction::Kat,
            },
        ];

        let due = collect_due_inputs(&mut pending, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn autoplay_branch_filter_respects_route_at_event_tick() {
        let mut controller = BranchController::new(
            BranchPolicy::FixedRoute,
            2,
            vec![BranchDecisionPoint {
                segment_id: 42,
                decision_tick: 100,
                route_count: 3,
                hint: None,
            }],
        );
        let _ = controller
            .controls_for_tick(100, &rhythm_mode_taiko::TaikoScoreState::default())
            .expect("controls");

        let inputs = vec![
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 90,
                    action: TaikoAction::Don,
                },
                branch_segment_id: Some(42),
                branch_route_id: 0,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 90,
                    action: TaikoAction::Kat,
                },
                branch_segment_id: Some(42),
                branch_route_id: 2,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 100,
                    action: TaikoAction::Don,
                },
                branch_segment_id: Some(42),
                branch_route_id: 0,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 100,
                    action: TaikoAction::Kat,
                },
                branch_segment_id: Some(42),
                branch_route_id: 2,
            },
        ];

        let mut cursor = 0_usize;
        let mut out = Vec::new();
        collect_autoplay_inputs(&inputs, &mut cursor, 100, &controller, &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].tick, 90);
        assert_eq!(out[0].action, TaikoAction::Don);
        assert_eq!(out[1].tick, 100);
        assert_eq!(out[1].action, TaikoAction::Kat);
    }

    #[test]
    fn latest_flashable_judge_ignores_rollhit() {
        let judges = [
            TaikoJudge::RollHit,
            TaikoJudge::Ignored,
            TaikoJudge::Great { delta_tick: -3_000 },
            TaikoJudge::RollHit,
        ];
        assert_eq!(
            latest_flashable_judge(&judges),
            Some(TaikoJudge::Great { delta_tick: -3_000 })
        );

        let only_roll = [TaikoJudge::RollHit, TaikoJudge::Ignored];
        assert_eq!(latest_flashable_judge(&only_roll), None);
    }

    #[test]
    fn latest_non_ignored_judge_keeps_rollhit() {
        let judges = [TaikoJudge::Ignored, TaikoJudge::RollHit];
        assert_eq!(latest_non_ignored_judge(&judges), Some(TaikoJudge::RollHit));
    }

    #[test]
    fn record_timing_samples_captures_signed_delta_for_tap_judges() {
        let judges = vec![
            TaikoJudge::Great {
                delta_tick: -12_000,
            },
            TaikoJudge::Ok { delta_tick: 8_000 },
            TaikoJudge::Miss { delta_tick: 95_000 },
            TaikoJudge::MissExpired,
            TaikoJudge::RollHit,
            TaikoJudge::Ignored,
        ];
        let mut out = Vec::new();
        record_timing_samples(&mut out, &judges);
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0],
            TimingSample {
                judge: TaikoJudgeKind::Great,
                delta_tick: -12_000
            }
        );
        assert_eq!(
            out[1],
            TimingSample {
                judge: TaikoJudgeKind::Ok,
                delta_tick: 8_000
            }
        );
        assert_eq!(
            out[2],
            TimingSample {
                judge: TaikoJudgeKind::Miss,
                delta_tick: 95_000
            }
        );
    }

    #[test]
    fn autoplay_roll_interval_follows_bpm_formula() {
        assert_eq!(autoplay_roll_interval_ticks(500_000), 62_500);
        assert_eq!(autoplay_roll_interval_ticks(400_000), 50_000);
        assert_eq!(autoplay_roll_interval_ticks(250_000), 31_250);
    }

    #[test]
    fn autoplay_finite_roll_stops_after_required_hits() {
        let chart = CanonicalChart {
            metadata: ChartMetadata::default(),
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            }],
            signatures: vec![TimeSignatureChange {
                tick: 0,
                numerator: 4,
                denominator: 4,
            }],
            lanes: Vec::new(),
            branch_segments: Vec::new(),
            objects: vec![Object {
                id: 1,
                kind: ObjectKind::Roll,
                start_tick: 0,
                end_tick: 1_000_000,
                lane_or_region: LaneOrRegion::None,
                flags: 0,
                required_hits: 3,
                slide_to: None,
                scroll_scaled: 1_000_000,
                branch_segment_id: None,
                branch_route_id: 0,
            }],
            events: Vec::new(),
        };

        let inputs = build_autoplay_inputs(&chart);
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].tick, 0);
        assert_eq!(inputs[1].tick, 62_500);
        assert_eq!(inputs[2].tick, 125_000);
    }

    #[test]
    fn tempo_lookup_uses_latest_change_at_tick() {
        let tempo_map = vec![
            TempoChange {
                tick: 0,
                micros_per_quarter: 500_000,
            },
            TempoChange {
                tick: 1_000_000,
                micros_per_quarter: 400_000,
            },
        ];
        assert_eq!(tempo_micros_per_quarter_at(&tempo_map, 0), 500_000);
        assert_eq!(tempo_micros_per_quarter_at(&tempo_map, 999_999), 500_000);
        assert_eq!(tempo_micros_per_quarter_at(&tempo_map, 1_000_000), 400_000);
    }

    #[test]
    fn scroll_speed_cycle_wraps_and_includes_vsync_slot() {
        assert_eq!(
            cycle_scroll_speed_setting(ScrollSpeedSetting::Manual(0.5), -1),
            ScrollSpeedSetting::VSync
        );
        assert_eq!(
            cycle_scroll_speed_setting(ScrollSpeedSetting::VSync, -1),
            ScrollSpeedSetting::Manual(4.0)
        );
        assert_eq!(
            cycle_scroll_speed_setting(ScrollSpeedSetting::Manual(4.0), 1),
            ScrollSpeedSetting::VSync
        );
        assert_eq!(
            cycle_scroll_speed_setting(ScrollSpeedSetting::VSync, 1),
            ScrollSpeedSetting::Manual(0.5)
        );
    }

    #[test]
    fn offset_adjustment_uses_5ms_step_with_clamp() {
        assert_eq!(adjust_offset_ms(0, 1), 5);
        assert_eq!(adjust_offset_ms(0, -1), -5);
        assert_eq!(adjust_offset_ms(498, 1), 500);
        assert_eq!(adjust_offset_ms(500, 1), 500);
        assert_eq!(adjust_offset_ms(-498, -1), -500);
        assert_eq!(adjust_offset_ms(-500, -1), -500);
    }

    #[test]
    fn quantize_offset_rounds_to_nearest_5ms_and_clamps() {
        assert_eq!(quantize_offset_ms(0.003), 5);
        assert_eq!(quantize_offset_ms(-0.003), -5);
        assert_eq!(quantize_offset_ms(0.0), 0);
        assert_eq!(quantize_offset_ms(0.499), 500);
        assert_eq!(quantize_offset_ms(0.8), 500);
        assert_eq!(quantize_offset_ms(-0.8), -500);
    }

    #[test]
    fn vsync_speed_stays_in_valid_range_for_mixed_timing_chart() {
        let chart = sample_mixed_chart();
        let speed = compute_vsync_scroll_speed(&chart, 120);
        assert!((VSYNC_SPEED_MIN..=VSYNC_SPEED_MAX).contains(&speed));
    }

    fn sample_mixed_chart() -> CanonicalChart {
        CanonicalChart {
            metadata: ChartMetadata::default(),
            tempo_map: vec![
                TempoChange {
                    tick: 0,
                    micros_per_quarter: 500_000,
                },
                TempoChange {
                    tick: 2_000_000,
                    micros_per_quarter: 400_000,
                },
            ],
            signatures: vec![
                TimeSignatureChange {
                    tick: 0,
                    numerator: 4,
                    denominator: 4,
                },
                TimeSignatureChange {
                    tick: 4_000_000,
                    numerator: 3,
                    denominator: 4,
                },
            ],
            lanes: Vec::new(),
            branch_segments: Vec::new(),
            objects: vec![
                Object {
                    id: 1,
                    kind: ObjectKind::Tap,
                    start_tick: 500_000,
                    end_tick: 500_000,
                    lane_or_region: LaneOrRegion::None,
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 2,
                    kind: ObjectKind::Tap,
                    start_tick: 666_667,
                    end_tick: 666_667,
                    lane_or_region: LaneOrRegion::None,
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 3,
                    kind: ObjectKind::Roll,
                    start_tick: 2_200_000,
                    end_tick: 2_650_000,
                    lane_or_region: LaneOrRegion::None,
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
                Object {
                    id: 4,
                    kind: ObjectKind::Tap,
                    start_tick: 4_166_667,
                    end_tick: 4_166_667,
                    lane_or_region: LaneOrRegion::None,
                    flags: 0,
                    required_hits: 0,
                    slide_to: None,
                    scroll_scaled: 1_000_000,
                    branch_segment_id: None,
                    branch_route_id: 0,
                },
            ],
            events: Vec::new(),
        }
    }
}
