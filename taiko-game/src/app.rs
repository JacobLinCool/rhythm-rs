use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::{Constraint, Direction, Layout};
use rhythm_chart::{ticks_from_seconds, CanonicalChart, Object, ObjectKind, Tick};
use rhythm_core::TimedInput;
use rhythm_importer_tja::TjaImportLimits;
use rhythm_mode_taiko::{
    ScheduledTaikoInput, TaikoAction, TaikoBranchPolicy, TaikoFinalResult, TaikoJudge,
    TaikoJudgeKind, TaikoMode, TaikoRuntime, TaikoZone, LANE_KAT,
};
use sha2::{Digest, Sha256};

use taiko_multiplayer_protocol::{PlayerSelection, RoomRole};

use crate::audio::{AudioCapability, AudioEngine, AudioNotice, GameAudio};
use crate::audio_sync::{AudioSyncController, AudioSyncDecision};
use crate::cli::{CliArgs, MAX_TPS, MIN_TPS};
use crate::clipboard::SystemClipboard;
use crate::controller::{ControllerSlot, ControllerSource, ControllerStrike};
use crate::demo_preview::{
    event_is_current as demo_event_is_current, DemoPreviewCompletion, DemoPreviewIdentity,
    DemoPreviewTask,
};
use crate::drum_surface::DrumSurfaceLayout;
use crate::embedded_server_start::{
    event_is_current as embedded_start_event_is_current, EmbeddedServerStartCompletion,
    EmbeddedServerStartIdentity, EmbeddedServerStartTask, PreparedEmbeddedServer,
};
use crate::input::{
    collect_due_offline_inputs, enqueue_offline_input, is_game_pause_toggle_key,
    map_bound_game_hit, map_menu_intent, MenuIntent,
};
use crate::lan_controller::{
    discover_lan_ip, ActiveControllerSlots, ControllerSlotStatus, LanControllerConfig,
    LanControllers, PairingInvite, MAX_DISPATCH_AGE,
};
use crate::library_loading::{
    event_is_current as library_load_event_is_current, LibraryLoadCompletion, LibraryLoadTask,
};
use crate::loader::{CourseEntry, SongEntry, SongLibrary};
use crate::local_multiplayer::{
    map_local_course_key, map_local_game_hit, LocalCourseSelection, LocalMultiplayerResult,
    LocalMultiplayerSession, LocalPlayerId, LocalPlayerSpec,
};
use crate::localization::{pop_grapheme, Localizer, UiMessage, UiText};
use crate::offline_preparation::{
    event_is_current as offline_preparation_event_is_current, OfflinePreparationCompletion,
    OfflinePreparationIdentity, OfflinePreparationMode, OfflinePreparationRequest,
    OfflinePreparationTask, PreparedOfflineCharts, PreparedOfflineContent,
};
use crate::online_bootstrap::{
    event_is_current, BootstrapCompletion, BootstrapIdentity, OnlineBootstrapTask,
    PreparedBootstrap, PreparedBootstrapResources,
};
use crate::online_preparation::{
    validate_authoritative_song_identity, OnlinePreparationTask, PreparationCompletion,
    PreparationEvent, PreparationIdentity, PreparationRequest,
};
use crate::perf::{PerfMeter, PerfSnapshot};
use crate::preferences::{
    BindingSlot, PersonalBest, PlayerPreferences, PreferencesStore, RecentSongSelection,
    StoredGameMode, StoredScrollSpeed, UiLanguage, MAX_STORED_QUERY_BYTES,
};
use crate::resource::ResourceBackend;
use crate::screen;
use crate::song_filter::SongFilter;
use crate::theme::Theme;
use crate::tui::Frame;

const DEMO_DELAY: Duration = Duration::from_millis(500);
const RESULT_DELAY: Duration = Duration::from_millis(500);
const MAX_PENDING_CONTROLLER_STRIKES: usize = 1_024;
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
const MAX_SERVER_URL_INPUT_BYTES: usize = 2_048;
const MAX_INVITE_URL_INPUT_BYTES: usize = 4_096;
const MAX_AUTOPLAY_ROLL_SECONDS: usize = 15 * 60;
const AUTOPLAY_ROLL_HITS_PER_SECOND: usize = 16;
const AUTOPLAY_ROLL_INTERVAL_TICKS: Tick = 62_500;
const MAX_AUTOPLAY_EVENTS: usize = TjaImportLimits::DEFAULT.max_objects_per_course
    + MAX_AUTOPLAY_ROLL_SECONDS * AUTOPLAY_ROLL_HITS_PER_SECOND;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    ModeSelect,
    Controllers,
    Settings,
    SongMenu,
    LoadWarnings,
    CourseMenu,
    OfflinePreparation,
    Game,
    Result,
    LocalCourseSelect,
    LocalGame,
    LocalResult,
    Error,
    MultiplayerConnect,
    OnlineLobby,
    OnlineCourseSelect,
    OnlineMatch,
    OnlineResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorRecoveryTarget {
    ModeSelect,
    SongMenu,
    CourseMenu,
    LocalCourseSelect,
    MultiplayerConnect,
}

impl ErrorRecoveryTarget {
    pub(crate) const fn page(self) -> Page {
        match self {
            Self::ModeSelect => Page::ModeSelect,
            Self::SongMenu => Page::SongMenu,
            Self::CourseMenu => Page::CourseMenu,
            Self::LocalCourseSelect => Page::LocalCourseSelect,
            Self::MultiplayerConnect => Page::MultiplayerConnect,
        }
    }

    pub(crate) const fn label_key(self) -> UiText {
        match self {
            Self::ModeSelect => UiText::PlayModeSelection,
            Self::SongMenu => UiText::SongSelection,
            Self::CourseMenu => UiText::CourseSelection,
            Self::LocalCourseSelect => UiText::LocalCourseSelection,
            Self::MultiplayerConnect => UiText::OnlineConnection,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoverableErrorState {
    pub(crate) summary: UiText,
    pub(crate) technical_details: String,
    pub(crate) recovery: ErrorRecoveryTarget,
    pub(crate) retry: Option<ErrorRetryAction>,
    pub(crate) details_visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorRetryAction {
    PrepareSinglePlayer,
    PrepareLocalTwoPlayer,
}

impl ErrorRetryAction {
    pub(crate) const fn label_key(self) -> UiText {
        match self {
            Self::PrepareSinglePlayer => UiText::RetrySinglePreparation,
            Self::PrepareLocalTwoPlayer => UiText::RetryLocalPreparation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LeaveTarget {
    SinglePlayer,
    LocalTwoPlayer,
    OnlineMatch,
}

impl LeaveTarget {
    pub(crate) const fn destination_key(self) -> UiText {
        match self {
            Self::SinglePlayer | Self::LocalTwoPlayer => UiText::CourseSelection,
            Self::OnlineMatch => UiText::OnlineDisconnectDestination,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InviteCopyStatus {
    Copied,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsItem {
    Language,
    SongVolume,
    SeVolume,
    Calibration,
    ScrollSpeed,
    Demo,
    PlayerName,
    Binding {
        player_index: usize,
        slot: BindingSlot,
    },
    Save,
}

impl SettingsItem {
    pub(crate) const ALL: [Self; 16] = [
        Self::Language,
        Self::SongVolume,
        Self::SeVolume,
        Self::Calibration,
        Self::ScrollSpeed,
        Self::Demo,
        Self::PlayerName,
        Self::Binding {
            player_index: 0,
            slot: BindingSlot::LeftKat,
        },
        Self::Binding {
            player_index: 0,
            slot: BindingSlot::LeftDon,
        },
        Self::Binding {
            player_index: 0,
            slot: BindingSlot::RightDon,
        },
        Self::Binding {
            player_index: 0,
            slot: BindingSlot::RightKat,
        },
        Self::Binding {
            player_index: 1,
            slot: BindingSlot::LeftKat,
        },
        Self::Binding {
            player_index: 1,
            slot: BindingSlot::LeftDon,
        },
        Self::Binding {
            player_index: 1,
            slot: BindingSlot::RightDon,
        },
        Self::Binding {
            player_index: 1,
            slot: BindingSlot::RightKat,
        },
        Self::Save,
    ];
}

#[derive(Debug, Clone)]
pub(crate) struct SettingsState {
    pub(crate) draft: PlayerPreferences,
    pub(crate) selected: usize,
    pub(crate) capture: Option<(usize, BindingSlot)>,
    /// `(message, is_error)`
    pub(crate) status: Option<(String, bool)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControllerSetupItem {
    BindAddress,
    LanServer,
    TerminalPointer,
    PlayerOne,
    PlayerTwo,
    Back,
}

impl ControllerSetupItem {
    pub(crate) const ALL: [Self; 6] = [
        Self::BindAddress,
        Self::LanServer,
        Self::TerminalPointer,
        Self::PlayerOne,
        Self::PlayerTwo,
        Self::Back,
    ];
}

#[derive(Debug, Clone)]
pub(crate) struct ControllerSetupState {
    pub(crate) bind_ip: String,
    pub(crate) selected: usize,
    pub(crate) pointer_slot: Option<ControllerSlot>,
    pub(crate) invite_revealed: [bool; 2],
    pub(crate) last_test_action: [Option<(TaikoAction, Instant)>; 2],
    /// `(message, is_error)`
    pub(crate) notice: Option<(String, bool)>,
}

impl ControllerSetupState {
    fn new(bind_ip: String) -> Self {
        Self {
            bind_ip,
            selected: 0,
            pointer_slot: None,
            invite_revealed: [false; 2],
            last_test_action: [None; 2],
            notice: None,
        }
    }

    pub(crate) fn selected_item(&self) -> ControllerSetupItem {
        ControllerSetupItem::ALL[self.selected.min(ControllerSetupItem::ALL.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy)]
struct QueuedControllerStrike {
    ingress_sequence: u64,
    strike: ControllerStrike,
}

#[derive(Debug, Clone, Copy)]
struct ControllerDispatchClock {
    sampled_at: Instant,
    song_seconds: f64,
}

impl SettingsState {
    fn new(preferences: PlayerPreferences) -> Self {
        Self {
            draft: preferences,
            selected: 0,
            capture: None,
            status: None,
        }
    }

    pub(crate) fn selected_item(&self) -> SettingsItem {
        SettingsItem::ALL[self.selected.min(SettingsItem::ALL.len() - 1)]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameMode {
    SinglePlayer,
    LocalTwoPlayer,
    OnlineMultiplayer,
}

impl GameMode {
    pub const ALL: [Self; 3] = [
        Self::SinglePlayer,
        Self::LocalTwoPlayer,
        Self::OnlineMultiplayer,
    ];

    pub(crate) const fn label_key(self) -> UiText {
        match self {
            Self::SinglePlayer => UiText::ModeSingle,
            Self::LocalTwoPlayer => UiText::ModeLocal,
            Self::OnlineMultiplayer => UiText::ModeOnline,
        }
    }

    pub(crate) const fn description_key(self) -> UiText {
        match self {
            Self::SinglePlayer => UiText::ModeSingleDescription,
            Self::LocalTwoPlayer => UiText::ModeLocalDescription,
            Self::OnlineMultiplayer => UiText::ModeOnlineDescription,
        }
    }

    pub(crate) const fn controls_key(self) -> UiText {
        match self {
            Self::SinglePlayer => UiText::ModeSingleControls,
            Self::LocalTwoPlayer => UiText::ModeLocalControls,
            Self::OnlineMultiplayer => UiText::ModeOnlineControls,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectMode {
    Host,
    Create,
    Join,
    Spectate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectField {
    Mode,
    Server,
    Invite,
    Name,
    Confirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MultiplayerConnectStatus {
    StartingPrivateServer,
    LoadingAuthoritativeLibrary,
    PreparingSpectatorConnection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MultiplayerConnectError {
    ServerUrlTooLong { max_bytes: usize },
    InviteTooLong { max_bytes: usize },
    NameTooLong { max_bytes: usize },
    NameRequired,
    ServerRequired,
    InviteRequired,
    InvalidServer { reason: String },
    InvalidInvite { reason: String },
    LocalHostingCancelled,
    OnlineConnectionCancelled,
    Technical { reason: String },
}

pub struct MultiplayerConnectState {
    pub mode: ConnectMode,
    pub server: String,
    pub invite: String,
    pub name: String,
    pub focus: ConnectField,
    pub(crate) status: Option<MultiplayerConnectStatus>,
    pub(crate) error: Option<MultiplayerConnectError>,
    pub invite_revealed: bool,
}

enum OnlineConnectRequest {
    Host { name: String },
    Connect(Box<crate::online::OnlineClientConfig>),
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
    pub previous_best_score: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingSample {
    pub judge: TaikoJudgeKind,
    pub delta_tick: Tick,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OfflineLibraryNotice {
    Loading,
    NoPlayableCharts(OfflineLibraryEmptyReason),
    LoadFailed { reason: String },
    PreviousSongUnavailable,
    PreviousSongDoesNotMatchSearch,
    PreviousCourseUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OfflineLibraryEmptyReason {
    ImportWarning(String),
    ResourceEndpoint,
    LocalDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DemoPreviewNotice {
    Loading,
    Unavailable { reason: String },
    StopFailed { reason: String },
}

pub struct LoadedCourseChart {
    pub song_index: usize,
    pub course_index: usize,
    pub chart: CanonicalChart,
}

struct OfflineResourceState {
    backend: Arc<ResourceBackend>,
    songs: Vec<SongEntry>,
    filtered_song_indices: Vec<usize>,
    song_query: String,
    song_filter_error: Option<String>,
    song_index: usize,
    course_index: usize,
    load_warnings: Vec<String>,
    load_warnings_scroll: u16,
    loaded_course_chart: Option<LoadedCourseChart>,
    offline_library_status: Option<OfflineLibraryNotice>,
}

struct ResourceStateSlots<'a> {
    backend: &'a mut Arc<ResourceBackend>,
    songs: &'a mut Vec<SongEntry>,
    filtered_song_indices: &'a mut Vec<usize>,
    song_query: &'a mut String,
    song_filter_error: &'a mut Option<String>,
    song_index: &'a mut usize,
    course_index: &'a mut usize,
    load_warnings: &'a mut Vec<String>,
    load_warnings_scroll: &'a mut u16,
    loaded_course_chart: &'a mut Option<LoadedCourseChart>,
    offline_library_status: &'a mut Option<OfflineLibraryNotice>,
}

impl OfflineResourceState {
    fn install(
        slots: ResourceStateSlots<'_>,
        backend: Arc<ResourceBackend>,
        library: SongLibrary,
    ) -> Self {
        let ResourceStateSlots {
            backend: backend_slot,
            songs,
            filtered_song_indices,
            song_query,
            song_filter_error,
            song_index,
            course_index,
            load_warnings,
            load_warnings_scroll,
            loaded_course_chart,
            offline_library_status,
        } = slots;
        let offline = Self {
            backend: std::mem::replace(backend_slot, backend),
            songs: std::mem::replace(songs, library.songs),
            filtered_song_indices: std::mem::take(filtered_song_indices),
            song_query: std::mem::take(song_query),
            song_filter_error: song_filter_error.take(),
            song_index: *song_index,
            course_index: *course_index,
            load_warnings: std::mem::replace(load_warnings, library.warnings),
            load_warnings_scroll: *load_warnings_scroll,
            loaded_course_chart: loaded_course_chart.take(),
            offline_library_status: offline_library_status.take(),
        };

        *filtered_song_indices = (0..songs.len()).collect();
        *song_index = 0;
        *course_index = 0;
        *load_warnings_scroll = 0;
        offline
    }

    fn restore(self, slots: ResourceStateSlots<'_>) {
        *slots.backend = self.backend;
        *slots.songs = self.songs;
        *slots.filtered_song_indices = self.filtered_song_indices;
        *slots.song_query = self.song_query;
        *slots.song_filter_error = self.song_filter_error;
        *slots.song_index = self.song_index;
        *slots.course_index = self.course_index;
        *slots.load_warnings = self.load_warnings;
        *slots.load_warnings_scroll = self.load_warnings_scroll;
        *slots.loaded_course_chart = self.loaded_course_chart;
        *slots.offline_library_status = self.offline_library_status;
    }
}

pub struct GameSession {
    pub song_index: usize,
    pub course_name: String,
    pub canonical_chart_hash: String,
    pub chart_end_tick: Tick,
    pub has_audio: bool,
    pub runtime: TaikoRuntime,
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
    CalibrationOffset,
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
            Self::SeVolume => Self::CalibrationOffset,
            Self::CalibrationOffset => Self::ScrollSpeed,
            Self::ScrollSpeed => Self::AutoPlay,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::AutoPlay => Self::ScrollSpeed,
            Self::SongVolume => Self::AutoPlay,
            Self::SeVolume => Self::SongVolume,
            Self::CalibrationOffset => Self::SeVolume,
            Self::ScrollSpeed => Self::CalibrationOffset,
        }
    }
}

pub struct App {
    pub(crate) args: CliArgs,
    pub(crate) page: Page,
    pub(crate) mode_selection: usize,
    pub(crate) active_mode: Option<GameMode>,
    pub(crate) songs: Vec<SongEntry>,
    pub(crate) filtered_song_indices: Vec<usize>,
    pub(crate) song_query: String,
    pub(crate) song_filter_error: Option<String>,
    pub(crate) song_index: usize,
    pub(crate) course_index: usize,
    pub(crate) auto_play: bool,
    pub(crate) course_setting_focus: CourseSettingFocus,
    pub(crate) scroll_speed_setting: ScrollSpeedSetting,
    pub(crate) scroll_speed_vsync: f32,
    pub(crate) calibration_offset_ms: i32,
    pub(crate) viewport_width: u16,
    pub(crate) viewport_height: u16,
    pub(crate) controller_setup: ControllerSetupState,
    pub(crate) keyboard_repeat_is_distinguishable: bool,
    pub(crate) pointer_surface: Option<DrumSurfaceLayout>,
    pending_pointer_surface: Option<DrumSurfaceLayout>,
    pub(crate) game: Option<GameSession>,
    pub(crate) result: Option<ResultState>,
    pub(crate) local_course_selection: LocalCourseSelection,
    pub(crate) local_game: Option<LocalMultiplayerSession>,
    pub(crate) local_result: Option<LocalMultiplayerResult>,
    pub(crate) error_state: Option<RecoverableErrorState>,
    pub(crate) result_details_visible: bool,
    pub(crate) leave_confirmation: Option<LeaveTarget>,
    pub(crate) load_warnings: Vec<String>,
    pub(crate) load_warnings_scroll: u16,
    offline_library_status: Option<OfflineLibraryNotice>,
    pub(crate) perf_meter: PerfMeter,
    pub(crate) theme: Theme,
    pub(crate) should_quit: bool,
    pub(crate) demo_pending: Option<(Instant, usize)>,
    pub(crate) demo_playing_song: Option<usize>,
    demo_preview_status: Option<DemoPreviewNotice>,
    demo_preview_generation: u64,
    demo_preview_identity: Option<DemoPreviewIdentity>,
    demo_preview_task: DemoPreviewTask,
    loaded_course_chart: Option<LoadedCourseChart>,
    resource_backend: Arc<ResourceBackend>,
    audio: Box<dyn GameAudio>,
    audio_notice: Option<AudioNotice>,
    preferences_store: Option<PreferencesStore>,
    pub(crate) preferences: PlayerPreferences,
    pub(crate) settings: SettingsState,
    clipboard: SystemClipboard,
    lan_controllers: Option<LanControllers>,
    lan_controller_generation: u64,
    controller_input_sequence: u64,
    pending_controller_strikes: Vec<QueuedControllerStrike>,
    controller_input_drops: u64,
    pub(crate) invite_revealed: bool,
    pub(crate) invite_copy_status: Option<InviteCopyStatus>,
    pub(crate) mp_connect: MultiplayerConnectState,
    pub(crate) online: Option<crate::online_session::OnlineDomain>,
    embedded_server: Option<crate::online::EmbeddedServer>,
    embedded_server_generation: u64,
    embedded_server_start: EmbeddedServerStartTask,
    embedded_server_start_identity: Option<EmbeddedServerStartIdentity>,
    online_generation: u64,
    online_bootstrap: OnlineBootstrapTask,
    online_bootstrap_identity: Option<BootstrapIdentity>,
    online_preparation: OnlinePreparationTask,
    offline_resources: Option<OfflineResourceState>,
    offline_preparation_generation: u64,
    offline_preparation_identity: Option<OfflinePreparationIdentity>,
    offline_preparation: OfflinePreparationTask,
    library_load_generation: u64,
    library_load_identity: Option<u64>,
    library_load: LibraryLoadTask,
    resume_library_load_after_online: bool,
}

impl App {
    pub(crate) fn ui_language(&self) -> UiLanguage {
        if self.page == Page::Settings {
            self.settings.draft.ui_language
        } else {
            self.preferences.ui_language
        }
    }

    pub(crate) fn localizer(&self) -> Localizer {
        Localizer::new(self.ui_language())
    }

    pub(crate) fn text(&self, key: UiText) -> &'static str {
        self.localizer().text(key)
    }

    pub(crate) fn offline_library_status_text(&self) -> Option<String> {
        self.offline_library_status
            .as_ref()
            .map(|notice| match notice {
                OfflineLibraryNotice::Loading => self.text(UiText::LoadingSongLibrary).to_owned(),
                OfflineLibraryNotice::NoPlayableCharts(
                    OfflineLibraryEmptyReason::ImportWarning(reason),
                ) => self
                    .localizer()
                    .message(UiMessage::NoPlayableOfflineCharts { reason }),
                OfflineLibraryNotice::NoPlayableCharts(
                    OfflineLibraryEmptyReason::ResourceEndpoint,
                ) => self
                    .text(UiText::ResourceEndpointHasNoPlayableCharts)
                    .to_owned(),
                OfflineLibraryNotice::NoPlayableCharts(
                    OfflineLibraryEmptyReason::LocalDirectory,
                ) => self
                    .text(UiText::OfflineDirectoryHasNoPlayableCharts)
                    .to_owned(),
                OfflineLibraryNotice::LoadFailed { reason } => self
                    .localizer()
                    .message(UiMessage::SongLibraryLoadFailed { reason }),
                OfflineLibraryNotice::PreviousSongUnavailable => {
                    self.text(UiText::PreviousSongUnavailable).to_owned()
                }
                OfflineLibraryNotice::PreviousSongDoesNotMatchSearch => {
                    self.text(UiText::PreviousSongDoesNotMatchSearch).to_owned()
                }
                OfflineLibraryNotice::PreviousCourseUnavailable => {
                    self.text(UiText::PreviousCourseUnavailable).to_owned()
                }
            })
    }

    pub(crate) fn demo_preview_status_text(&self) -> Option<String> {
        self.demo_preview_status
            .as_ref()
            .map(|notice| match notice {
                DemoPreviewNotice::Loading => self.text(UiText::LoadingPreview).to_owned(),
                DemoPreviewNotice::Unavailable { reason } => self
                    .localizer()
                    .message(UiMessage::PreviewUnavailable { reason }),
                DemoPreviewNotice::StopFailed { reason } => self
                    .localizer()
                    .message(UiMessage::PreviewStopFailed { reason }),
            })
    }

    pub(crate) fn multiplayer_connect_status_text(&self) -> Option<&'static str> {
        self.mp_connect.status.map(|status| {
            self.text(match status {
                MultiplayerConnectStatus::StartingPrivateServer => UiText::StartingPrivateServer,
                MultiplayerConnectStatus::LoadingAuthoritativeLibrary => {
                    UiText::LoadingAuthoritativeLibrary
                }
                MultiplayerConnectStatus::PreparingSpectatorConnection => {
                    UiText::PreparingSpectatorConnection
                }
            })
        })
    }

    pub(crate) fn multiplayer_connect_error_text(&self) -> Option<String> {
        self.mp_connect.error.as_ref().map(|error| match error {
            MultiplayerConnectError::ServerUrlTooLong { max_bytes } => {
                self.localizer().message(UiMessage::Utf8ByteLimit {
                    field: self.text(UiText::Server),
                    max_bytes: *max_bytes,
                })
            }
            MultiplayerConnectError::InviteTooLong { max_bytes } => {
                self.localizer().message(UiMessage::Utf8ByteLimit {
                    field: self.text(UiText::Invite),
                    max_bytes: *max_bytes,
                })
            }
            MultiplayerConnectError::NameTooLong { max_bytes } => {
                self.localizer().message(UiMessage::Utf8ByteLimit {
                    field: self.text(UiText::Name),
                    max_bytes: *max_bytes,
                })
            }
            MultiplayerConnectError::NameRequired => self.text(UiText::NameRequired).to_owned(),
            MultiplayerConnectError::ServerRequired => self.text(UiText::ServerRequired).to_owned(),
            MultiplayerConnectError::InviteRequired => self.text(UiText::InviteRequired).to_owned(),
            MultiplayerConnectError::InvalidServer { reason } => {
                self.localizer().message(UiMessage::InvalidField {
                    field: self.text(UiText::Server),
                    reason,
                })
            }
            MultiplayerConnectError::InvalidInvite { reason } => {
                self.localizer().message(UiMessage::InvalidField {
                    field: self.text(UiText::Invite),
                    reason,
                })
            }
            MultiplayerConnectError::LocalHostingCancelled => {
                self.text(UiText::LocalHostingCancelled).to_owned()
            }
            MultiplayerConnectError::OnlineConnectionCancelled => {
                self.text(UiText::OnlineConnectionCancelled).to_owned()
            }
            MultiplayerConnectError::Technical { reason } => reason.clone(),
        })
    }

    pub fn new(args: CliArgs) -> Result<Self> {
        let preferences_store = PreferencesStore::for_current_user()?;
        let preferences = preferences_store
            .load()?
            .unwrap_or_else(|| preferences_from_cli(&args));
        let resource_backend = ResourceBackend::from_cli(&args)?;
        let mut app = Self::with_resources(
            args,
            resource_backend,
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        )?;
        app.preferences_store = Some(preferences_store);
        app.apply_preferences(preferences)?;
        app.begin_library_load()?;

        Ok(app)
    }

    fn with_resources(
        args: CliArgs,
        resource_backend: ResourceBackend,
        library: SongLibrary,
    ) -> Result<Self> {
        let audio = Box::new(AudioEngine::new(args.songvol, args.sevol)?);
        Self::with_resources_and_audio(args, resource_backend, library, audio)
    }

    fn with_resources_and_audio(
        args: CliArgs,
        resource_backend: ResourceBackend,
        library: SongLibrary,
        audio: Box<dyn GameAudio>,
    ) -> Result<Self> {
        if !(MIN_TPS..=MAX_TPS).contains(&args.tps) {
            bail!("--tps must be between {MIN_TPS} and {MAX_TPS}");
        }

        let preferences = preferences_from_cli(&args);
        let audio_notice = AudioNotice::from_capability(&audio.capability());
        let controller_bind_ip = discover_lan_ip()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST))
            .to_string();
        let mut app = Self {
            auto_play: false,
            course_setting_focus: CourseSettingFocus::AutoPlay,
            scroll_speed_setting: ScrollSpeedSetting::Manual(1.0),
            scroll_speed_vsync: 1.0,
            calibration_offset_ms: args.calibration_offset_ms,
            viewport_width: 120,
            viewport_height: 36,
            controller_setup: ControllerSetupState::new(controller_bind_ip),
            keyboard_repeat_is_distinguishable: false,
            pointer_surface: None,
            pending_pointer_surface: None,
            resource_backend: Arc::new(resource_backend),
            audio,
            audio_notice,
            preferences_store: None,
            preferences: preferences.clone(),
            settings: SettingsState::new(preferences),
            clipboard: SystemClipboard::default(),
            lan_controllers: None,
            lan_controller_generation: 0,
            controller_input_sequence: 0,
            pending_controller_strikes: Vec::new(),
            controller_input_drops: 0,
            invite_revealed: false,
            invite_copy_status: None,
            args,
            page: Page::ModeSelect,
            mode_selection: 0,
            active_mode: None,
            songs: library.songs,
            filtered_song_indices: Vec::new(),
            song_query: String::new(),
            song_filter_error: None,
            song_index: 0,
            course_index: 0,
            game: None,
            result: None,
            local_course_selection: LocalCourseSelection::default(),
            local_game: None,
            local_result: None,
            error_state: None,
            result_details_visible: false,
            leave_confirmation: None,
            load_warnings: library.warnings,
            load_warnings_scroll: 0,
            offline_library_status: None,
            perf_meter: PerfMeter::default(),
            theme: Theme::taiko_vivid(Theme::detect()),
            should_quit: false,
            demo_pending: None,
            demo_playing_song: None,
            demo_preview_status: None,
            demo_preview_generation: 0,
            demo_preview_identity: None,
            demo_preview_task: DemoPreviewTask::default(),
            loaded_course_chart: None,
            mp_connect: MultiplayerConnectState {
                mode: ConnectMode::Host,
                server: "http://127.0.0.1:4150".to_owned(),
                invite: String::new(),
                name: "Player".to_owned(),
                focus: ConnectField::Mode,
                status: None,
                error: None,
                invite_revealed: false,
            },
            online: None,
            embedded_server: None,
            embedded_server_generation: 0,
            embedded_server_start: EmbeddedServerStartTask::default(),
            embedded_server_start_identity: None,
            online_generation: 0,
            online_bootstrap: OnlineBootstrapTask::default(),
            online_bootstrap_identity: None,
            online_preparation: OnlinePreparationTask::default(),
            offline_resources: None,
            offline_preparation_generation: 0,
            offline_preparation_identity: None,
            offline_preparation: OfflinePreparationTask::default(),
            library_load_generation: 0,
            library_load_identity: None,
            library_load: LibraryLoadTask::default(),
            resume_library_load_after_online: false,
        };

        if !app.songs.is_empty() {
            app.rebuild_song_filter()?;
        }

        app.refresh_vsync_scroll_speed()?;
        Ok(app)
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub(crate) fn audio_capability(&self) -> AudioCapability {
        self.audio.capability()
    }

    pub(crate) fn audio_notice(&self) -> Option<&AudioNotice> {
        self.audio_notice.as_ref()
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        self.cancel_library_load();
        let _ = self.library_load.poll();
        self.cancel_offline_preparation();
        let _ = self.offline_preparation.poll();
        self.pointer_surface = None;
        self.pending_pointer_surface = None;
        let online_result = self.teardown_online();
        let controller_result = self
            .lan_controllers
            .take()
            .map_or(Ok(()), LanControllers::shutdown_and_join);
        match (online_result, controller_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
            (Err(online), Err(controller)) => Err(anyhow!(
                "{online}; LAN controller shutdown also failed: {controller}"
            )),
        }
    }

    pub fn handle_tick(&mut self) {
        if self.should_quit {
            return;
        }

        self.poll_demo_preview();

        let mut result = self
            .poll_library_load()
            .and_then(|()| self.poll_offline_preparation())
            .and_then(|()| self.poll_embedded_server_start())
            .and_then(|()| self.poll_online_bootstrap())
            .and_then(|()| self.poll_online_preparation())
            .and_then(|()| self.drain_controller_inputs())
            .and_then(|()| match self.page {
                Page::SongMenu | Page::LoadWarnings | Page::CourseMenu => self.tick_demo_preview(),
                Page::Game => self.tick_game(),
                Page::LocalGame => self.tick_local_game(),
                Page::OnlineLobby => self.tick_online_lobby(),
                Page::OnlineCourseSelect | Page::OnlineMatch => self.tick_online_match_phase(),
                Page::ModeSelect
                | Page::Controllers
                | Page::Settings
                | Page::OfflinePreparation
                | Page::Result
                | Page::LocalCourseSelect
                | Page::LocalResult
                | Page::Error
                | Page::MultiplayerConnect
                | Page::OnlineResult => Ok(()),
            });

        // Always tick online network if session exists
        if result.is_ok() {
            if let Some(online) = &mut self.online {
                if let Err(error) = online.tick_network() {
                    result = Err(error);
                } else if online.is_terminal() {
                    let message = online
                        .error()
                        .map(|error| error.display_message())
                        .unwrap_or_else(|| online.status_message().to_owned());
                    result = Err(anyhow!(message));
                }
            }
        }
        if result.is_ok() {
            // Process pending session actions
            result = self.process_online_actions();
        }

        if let Err(error) = result {
            self.set_error_state(error);
        }
        self.sync_controller_admission();
    }

    #[cfg(test)]
    pub fn handle_key(&mut self, key: KeyEvent) {
        self.handle_key_at(key, Instant::now());
    }

    pub fn handle_key_at(&mut self, key: KeyEvent, observed_at: Instant) {
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

        let previous_page = self.page;
        let result = match self.page {
            Page::ModeSelect => self.handle_mode_select_key(key),
            Page::Controllers => self.handle_controller_setup_key(key),
            Page::Settings => self.handle_settings_key(key),
            Page::SongMenu => self.handle_song_menu_key(key),
            Page::LoadWarnings => self.handle_load_warnings_key(key),
            Page::CourseMenu => self.handle_course_menu_key(key),
            Page::OfflinePreparation => self.handle_offline_preparation_key(key),
            Page::Game => self.handle_game_key(key, observed_at),
            Page::Result => self.handle_result_key(key),
            Page::LocalCourseSelect => self.handle_local_course_key(key),
            Page::LocalGame => self.handle_local_game_key(key, observed_at),
            Page::LocalResult => self.handle_local_result_key(key),
            Page::Error => self.handle_error_key(key),
            Page::MultiplayerConnect => self.handle_mp_connect_key(key),
            Page::OnlineLobby => self.handle_online_lobby_key(key),
            Page::OnlineCourseSelect => self.handle_online_course_key(key),
            Page::OnlineMatch => self.handle_online_match_key(key, observed_at),
            Page::OnlineResult => self.handle_online_result_key(key),
        };

        if self.page != previous_page {
            self.invalidate_pointer_surface();
        }

        if let Err(error) = result {
            self.set_error_state(error);
        }
        self.sync_controller_admission();
    }

    pub(crate) fn set_pointer_surface(&mut self, surface: DrumSurfaceLayout) {
        self.pending_pointer_surface = Some(surface);
    }

    pub(crate) fn commit_rendered_pointer_surface(&mut self) {
        self.pointer_surface = self.pending_pointer_surface.take();
    }

    pub(crate) fn terminal_pointer_slot(&self) -> Option<ControllerSlot> {
        self.controller_setup.pointer_slot
    }

    pub(crate) fn set_keyboard_repeat_capability(&mut self, distinguishable: bool) {
        self.keyboard_repeat_is_distinguishable = distinguishable;
    }

    pub(crate) fn terminal_pointer_capture_requested(&self) -> bool {
        self.pointer_surface.is_some_and(|surface| {
            self.controller_setup.pointer_slot == Some(surface.slot)
                && self.controller_active_slots().contains(surface.slot)
        })
    }

    pub(crate) fn invalidate_pointer_surface(&mut self) {
        self.pointer_surface = None;
        self.pending_pointer_surface = None;
    }

    pub fn handle_pointer_at(&mut self, event: MouseEvent, observed_at: Instant) {
        let Some(surface) = self.pointer_surface else {
            return;
        };
        let Some(action) = surface.hit_test(event) else {
            return;
        };
        let strike = ControllerStrike::local(
            surface.slot,
            ControllerSource::TerminalPointer,
            action,
            observed_at,
        );
        if let Err(error) = self.enqueue_controller_strike(strike) {
            self.set_error_state(error);
        }
    }

    fn controller_active_slots(&self) -> ActiveControllerSlots {
        if self.leave_confirmation.is_some() {
            return ActiveControllerSlots::NONE;
        }
        match self.page {
            Page::Controllers => ActiveControllerSlots::BOTH,
            Page::Game
                if self
                    .game
                    .as_ref()
                    .is_some_and(|game| !game.paused && !self.auto_play) =>
            {
                ActiveControllerSlots::ONE
            }
            Page::LocalGame if self.local_game.as_ref().is_some_and(|game| !game.paused) => {
                ActiveControllerSlots::BOTH
            }
            Page::OnlineMatch
                if self.online.as_ref().is_some_and(|online| {
                    online.phase() == crate::online_session::OnlinePhase::Playing
                        && online.local_player_id().is_some()
                }) =>
            {
                ActiveControllerSlots::ONE
            }
            _ => ActiveControllerSlots::NONE,
        }
    }

    fn drain_controller_inputs(&mut self) -> Result<()> {
        self.drain_controller_inputs_once().map(|_| ())
    }

    fn drain_controller_inputs_once(&mut self) -> Result<bool> {
        let active_slots = self.controller_active_slots();
        self.drain_controller_inputs_with_policy(active_slots, active_slots, true)
    }

    fn drain_controller_inputs_with_policy(
        &mut self,
        admission_slots: ActiveControllerSlots,
        dispatch_slots: ActiveControllerSlots,
        require_quiescent_snapshot: bool,
    ) -> Result<bool> {
        let ingress_is_stable = if let Some(controllers) = self.lan_controllers.as_mut() {
            controllers.set_active_slots(admission_slots);
            let before = require_quiescent_snapshot.then(|| controllers.ingress_snapshot());
            let queued = controllers.drain_inputs(dispatch_slots);
            let after = require_quiescent_snapshot.then(|| controllers.ingress_snapshot());
            let now = Instant::now();
            for strike in queued.strikes.into_iter().flatten() {
                if strike.generation != Some(self.lan_controller_generation)
                    || now.saturating_duration_since(strike.observed_at) > MAX_DISPATCH_AGE
                    || !dispatch_slots.contains(strike.slot)
                {
                    continue;
                }
                self.enqueue_controller_strike(strike)?;
            }
            !queued.saturated
                && match (before, after) {
                    (Some(before), Some(after)) => after.is_stable_since(before),
                    (None, None) => true,
                    _ => unreachable!("snapshot policy is internally consistent"),
                }
        } else {
            true
        };
        if !ingress_is_stable {
            return Ok(false);
        }

        self.pending_controller_strikes.sort_by(|left, right| {
            left.strike
                .observed_at
                .cmp(&right.strike.observed_at)
                .then(left.ingress_sequence.cmp(&right.ingress_sequence))
        });
        let pending = std::mem::take(&mut self.pending_controller_strikes);
        let dispatch_clock = ControllerDispatchClock {
            sampled_at: Instant::now(),
            song_seconds: self.audio.song_position_seconds(),
        };
        for queued in pending {
            self.dispatch_controller_strike(queued.strike, dispatch_clock)?;
        }
        Ok(true)
    }

    fn flush_controller_inputs_before_state_change(&mut self) -> Result<()> {
        let closing_slots = self.controller_active_slots();
        while !self.drain_controller_inputs_with_policy(
            ActiveControllerSlots::NONE,
            closing_slots,
            false,
        )? {
            // Admission is mutex-linearized as closed before the first drain,
            // so only the fixed-capacity queues can remain. At most a bounded
            // number of non-blocking passes is required.
        }
        Ok(())
    }

    fn sync_controller_admission(&self) {
        if let Some(controllers) = &self.lan_controllers {
            controllers.set_active_slots(self.controller_active_slots());
        }
    }

    fn enqueue_controller_strike(&mut self, strike: ControllerStrike) -> Result<()> {
        let ingress_sequence = self.controller_input_sequence;
        self.controller_input_sequence = self
            .controller_input_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("controller input sequence exhausted"))?;
        if self.pending_controller_strikes.len() >= MAX_PENDING_CONTROLLER_STRIKES {
            self.controller_input_drops = self.controller_input_drops.saturating_add(1);
            return Ok(());
        }
        self.pending_controller_strikes
            .push(QueuedControllerStrike {
                ingress_sequence,
                strike,
            });
        Ok(())
    }

    fn dispatch_controller_strike(
        &mut self,
        strike: ControllerStrike,
        dispatch_clock: ControllerDispatchClock,
    ) -> Result<()> {
        self.perf_meter
            .record_input_dispatch(Instant::now().saturating_duration_since(strike.observed_at));
        match self.page {
            Page::Controllers => {
                self.controller_setup.last_test_action[strike.slot.index()] =
                    Some((strike.action, Instant::now()));
                self.play_taiko_se(strike.action.zone);
            }
            Page::Game
                if strike.slot == ControllerSlot::One
                    && self.leave_confirmation.is_none()
                    && !self.auto_play
                    && self.game.as_ref().is_some_and(|game| !game.paused) =>
            {
                let Some(last_tick) = self.game.as_ref().map(|game| game.last_tick) else {
                    return Ok(());
                };
                let tick = chart_tick_from_audio_observation(
                    dispatch_clock.song_seconds,
                    dispatch_clock
                        .sampled_at
                        .saturating_duration_since(strike.observed_at)
                        .as_secs_f64(),
                    self.calibration_offset_ms,
                    last_tick,
                );
                let accepted = self.game.as_mut().is_some_and(|game| {
                    enqueue_offline_input(
                        &mut game.pending_inputs,
                        TimedInput {
                            tick,
                            action: strike.action,
                        },
                    )
                });
                if accepted {
                    self.play_taiko_se(strike.action.zone);
                }
            }
            Page::LocalGame
                if self.leave_confirmation.is_none()
                    && self.local_game.as_ref().is_some_and(|game| !game.paused) =>
            {
                let player = match strike.slot {
                    ControllerSlot::One => LocalPlayerId::One,
                    ControllerSlot::Two => LocalPlayerId::Two,
                };
                let last_tick = self.local_game.as_ref().map_or(0, |game| game.last_tick);
                let tick = chart_tick_from_audio_observation(
                    dispatch_clock.song_seconds,
                    dispatch_clock
                        .sampled_at
                        .saturating_duration_since(strike.observed_at)
                        .as_secs_f64(),
                    self.calibration_offset_ms,
                    last_tick,
                );
                let accepted = self.local_game.as_mut().is_some_and(|game| {
                    game.queue_input(
                        crate::local_multiplayer::LocalGameInput {
                            player,
                            hit: strike.action,
                        },
                        tick,
                    )
                });
                if accepted {
                    self.play_taiko_se(strike.action.zone);
                }
            }
            Page::OnlineMatch
                if strike.slot == ControllerSlot::One && self.leave_confirmation.is_none() =>
            {
                let Some(online) = self.online.as_mut() else {
                    return Ok(());
                };
                if online.phase() != crate::online_session::OnlinePhase::Playing
                    || online.local_player_id().is_none()
                {
                    return Ok(());
                }
                let tick = apply_calibration_to_tick(
                    online.estimated_server_tick_at(strike.observed_at),
                    self.calibration_offset_ms,
                )
                .max(0);
                let wire_action = crate::online::to_drum_action(strike.action);
                let Some(submitted) = online.submit_input(tick, wire_action)? else {
                    return Ok(());
                };
                let tick = submitted.tick;
                if let Some(runtime) = online.local_player.as_mut() {
                    runtime.pending_inputs.push(TimedInput {
                        tick,
                        action: strike.action,
                    });
                    runtime.pending_inputs.sort_by_key(|input| input.tick);
                }
                self.play_taiko_se(strike.action.zone);
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn controller_server_running(&self) -> bool {
        self.lan_controllers.is_some()
    }

    pub(crate) fn controller_endpoint(&self) -> Option<String> {
        self.lan_controllers.as_ref().map(LanControllers::endpoint)
    }

    pub(crate) fn controller_pairing_invite(&self, slot: ControllerSlot) -> Option<PairingInvite> {
        self.lan_controllers
            .as_ref()
            .and_then(|controllers| controllers.pairing_invite(slot, self.ui_language()))
    }

    pub(crate) fn controller_slot_statuses(&self) -> [ControllerSlotStatus; 2] {
        self.lan_controllers.as_ref().map_or(
            [
                ControllerSlotStatus {
                    paired: false,
                    connected: false,
                    accepted_hits: 0,
                    rejected_hits: 0,
                },
                ControllerSlotStatus {
                    paired: false,
                    connected: false,
                    accepted_hits: 0,
                    rejected_hits: 0,
                },
            ],
            LanControllers::status,
        )
    }

    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let size = frame.area();
        self.pending_pointer_surface = None;
        if screen::terminal_is_too_small_for_app(self, size) {
            screen::render_terminal_guard(self, frame, size);
            return;
        }
        self.viewport_height = size.height;
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
            Page::ModeSelect => screen::mode_select::render(self, frame, chunks[1]),
            Page::Controllers => screen::controllers::render(self, frame, chunks[1]),
            Page::Settings => screen::settings::render(self, frame, chunks[1]),
            Page::SongMenu => screen::song_menu::render(self, frame, chunks[1]),
            Page::LoadWarnings => screen::load_warnings_screen::render(self, frame, chunks[1]),
            Page::CourseMenu => screen::course_menu::render(self, frame, chunks[1]),
            Page::OfflinePreparation => screen::offline_preparation::render(self, frame, chunks[1]),
            Page::Game => {
                if let Some(surface) = screen::game_screen::render(self, frame, chunks[1]) {
                    self.set_pointer_surface(surface);
                }
            }
            Page::Result => screen::result_screen::render(self, frame, chunks[1]),
            Page::LocalCourseSelect => screen::local_course::render(self, frame, chunks[1]),
            Page::LocalGame => {
                if let Some(surface) = screen::local_game::render(self, frame, chunks[1]) {
                    self.set_pointer_surface(surface);
                }
            }
            Page::LocalResult => screen::local_result::render(self, frame, chunks[1]),
            Page::Error => screen::error_screen::render(self, frame, chunks[1]),
            Page::MultiplayerConnect => screen::mp_connect::render(self, frame, chunks[1]),
            Page::OnlineLobby => screen::online_lobby::render(self, frame, chunks[1]),
            Page::OnlineCourseSelect => screen::online_course::render(self, frame, chunks[1]),
            Page::OnlineMatch => {
                if let Some(surface) = screen::online_match::render(self, frame, chunks[1]) {
                    self.set_pointer_surface(surface);
                }
            }
            Page::OnlineResult => screen::online_result::render(self, frame, chunks[1]),
        }
        if let Some(target) = self.leave_confirmation {
            screen::render_leave_confirmation(self, frame, size, target);
        }
    }

    pub fn record_frame_time(&mut self, elapsed: Duration) {
        if matches!(self.page, Page::Game | Page::LocalGame | Page::OnlineMatch) {
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
            ScrollSpeedSetting::VSync => format!(
                "{} ({:.3}x)",
                self.text(UiText::VelocitySync),
                self.scroll_speed_vsync
            ),
        }
    }

    pub(crate) fn calibration_offset_label(&self) -> String {
        format_offset_ms(self.calibration_offset_ms)
    }

    fn handle_mode_select_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('c' | 'C'),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        ) {
            self.controller_setup.notice = None;
            self.page = Page::Controllers;
            return Ok(());
        }

        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('s' | 'S'),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        ) {
            self.settings = SettingsState::new(self.preferences.clone());
            self.page = Page::Settings;
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };
        match intent {
            MenuIntent::Quit | MenuIntent::Back => self.should_quit = true,
            MenuIntent::Up | MenuIntent::Left => {
                self.mode_selection =
                    wrapped_selection(self.mode_selection, GameMode::ALL.len(), -1);
                self.play_kat_se()?;
            }
            MenuIntent::Down | MenuIntent::Right => {
                self.mode_selection =
                    wrapped_selection(self.mode_selection, GameMode::ALL.len(), 1);
                self.play_kat_se()?;
            }
            MenuIntent::Confirm => {
                let mode = GameMode::ALL[self.mode_selection.min(GameMode::ALL.len() - 1)];
                self.persist_last_mode(mode)?;
                self.play_don_se()?;
                self.active_mode = Some(mode);
                match mode {
                    GameMode::SinglePlayer | GameMode::LocalTwoPlayer => {
                        self.page = Page::SongMenu;
                        if !self.songs.is_empty() {
                            self.schedule_demo();
                        }
                    }
                    GameMode::OnlineMultiplayer => {
                        self.reset_multiplayer_connect();
                        self.page = Page::MultiplayerConnect;
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_controller_setup_key(&mut self, key: KeyEvent) -> Result<()> {
        let selected = self.controller_setup.selected_item();
        if let Some(slot) = controller_item_slot(selected) {
            if self.controller_setup.invite_revealed[slot.index()] {
                match key {
                    KeyEvent {
                        code: KeyCode::Esc | KeyCode::Enter,
                        ..
                    } => {
                        self.controller_setup.invite_revealed[slot.index()] = false;
                        if matches!(key.code, KeyCode::Enter) {
                            self.play_don_se()?;
                        }
                    }
                    KeyEvent {
                        code: KeyCode::Char('c' | 'C'),
                        modifiers,
                        ..
                    } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        self.copy_controller_invite(slot);
                    }
                    KeyEvent {
                        code: KeyCode::Char('r' | 'R'),
                        modifiers,
                        ..
                    } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        self.rotate_controller_pairing(slot);
                    }
                    _ => {}
                }
                return Ok(());
            }
        }

        if matches!(key.code, KeyCode::Esc) {
            self.controller_setup.notice = None;
            self.page = Page::ModeSelect;
            return Ok(());
        }

        if selected == ControllerSetupItem::BindAddress {
            match key.code {
                KeyCode::Backspace => {
                    if self.lan_controllers.is_some() {
                        self.controller_setup.notice = Some((
                            self.text(UiText::ControllerStopBeforeEditing).to_owned(),
                            true,
                        ));
                    } else {
                        pop_grapheme(&mut self.controller_setup.bind_ip);
                        self.controller_setup.notice = None;
                    }
                    return Ok(());
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && (character.is_ascii_hexdigit() || matches!(character, '.' | ':'))
                        && self.controller_setup.bind_ip.len() < 45 =>
                {
                    if self.lan_controllers.is_some() {
                        self.controller_setup.notice = Some((
                            self.text(UiText::ControllerStopBeforeEditing).to_owned(),
                            true,
                        ));
                    } else {
                        self.controller_setup.bind_ip.push(character);
                        self.controller_setup.notice = None;
                    }
                    return Ok(());
                }
                _ => {}
            }
        }

        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('c' | 'C'),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        ) {
            if let Some(slot) = controller_item_slot(selected) {
                self.copy_controller_invite(slot);
            }
            return Ok(());
        }

        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('r' | 'R'),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        ) {
            if let Some(slot) = controller_item_slot(selected) {
                self.rotate_controller_pairing(slot);
            }
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };
        match intent {
            MenuIntent::Quit | MenuIntent::Back => {
                self.controller_setup.notice = None;
                self.page = Page::ModeSelect;
            }
            MenuIntent::Up => {
                self.controller_setup.selected = wrapped_selection(
                    self.controller_setup.selected,
                    ControllerSetupItem::ALL.len(),
                    -1,
                );
                self.play_kat_se()?;
            }
            MenuIntent::Down => {
                self.controller_setup.selected = wrapped_selection(
                    self.controller_setup.selected,
                    ControllerSetupItem::ALL.len(),
                    1,
                );
                self.play_kat_se()?;
            }
            MenuIntent::Left | MenuIntent::Right
                if selected == ControllerSetupItem::TerminalPointer =>
            {
                self.cycle_terminal_pointer(matches!(intent, MenuIntent::Right));
                self.play_kat_se()?;
            }
            MenuIntent::Confirm => {
                match selected {
                    ControllerSetupItem::BindAddress => {}
                    ControllerSetupItem::LanServer => self.toggle_lan_controller_server(),
                    ControllerSetupItem::TerminalPointer => {
                        self.cycle_terminal_pointer(true);
                    }
                    ControllerSetupItem::PlayerOne | ControllerSetupItem::PlayerTwo => {
                        let slot = controller_item_slot(selected)
                            .expect("player controller rows always map to a slot");
                        let index = slot.index();
                        if self.controller_pairing_invite(slot).is_some() {
                            self.controller_setup.invite_revealed[index] = true;
                        } else {
                            self.controller_setup.notice = Some((
                                self.text(UiText::ControllerNoUnusedInvite).to_owned(),
                                true,
                            ));
                        }
                    }
                    ControllerSetupItem::Back => {
                        self.controller_setup.notice = None;
                        self.page = Page::ModeSelect;
                    }
                }
                self.play_don_se()?;
            }
            MenuIntent::Left | MenuIntent::Right => {}
        }
        Ok(())
    }

    fn cycle_terminal_pointer(&mut self, forward: bool) {
        self.controller_setup.pointer_slot = match (self.controller_setup.pointer_slot, forward) {
            (None, true) | (Some(ControllerSlot::Two), false) => Some(ControllerSlot::One),
            (Some(ControllerSlot::One), true) | (None, false) => Some(ControllerSlot::Two),
            (Some(ControllerSlot::Two), true) | (Some(ControllerSlot::One), false) => None,
        };
        self.invalidate_pointer_surface();
        self.controller_setup.notice = None;
    }

    fn toggle_lan_controller_server(&mut self) {
        if let Some(controllers) = self.lan_controllers.take() {
            let result = controllers.shutdown_and_join();
            self.controller_setup.invite_revealed = [false; 2];
            self.controller_setup.notice = Some(match result {
                Ok(()) => (self.text(UiText::ControllerServerStopped).to_owned(), false),
                Err(error) => (
                    format!("{}: {error}", self.text(UiText::ControllerServerStopFailed)),
                    true,
                ),
            });
            return;
        }

        let bind_ip = match self.controller_setup.bind_ip.parse::<std::net::IpAddr>() {
            Ok(bind_ip) if !bind_ip.is_unspecified() && !bind_ip.is_multicast() => bind_ip,
            Ok(_) => {
                self.controller_setup.notice = Some((
                    self.text(UiText::ControllerBindMustBeExact).to_owned(),
                    true,
                ));
                return;
            }
            Err(error) => {
                self.controller_setup.notice = Some((
                    format!(
                        "{}: {error}",
                        self.text(UiText::ControllerInvalidBindAddress)
                    ),
                    true,
                ));
                return;
            }
        };

        let Some(generation) = self.lan_controller_generation.checked_add(1) else {
            self.controller_setup.notice = Some((
                self.text(UiText::ControllerGenerationExhausted).to_owned(),
                true,
            ));
            return;
        };
        match LanControllers::start(LanControllerConfig {
            bind_ip,
            generation,
        }) {
            Ok(controllers) => {
                self.lan_controller_generation = generation;
                self.lan_controllers = Some(controllers);
                self.controller_setup.invite_revealed = [false; 2];
                self.controller_setup.notice =
                    Some((self.text(UiText::ControllerServerStarted).to_owned(), false));
            }
            Err(error) => {
                self.controller_setup.notice = Some((
                    format!(
                        "{}: {error}",
                        self.text(UiText::ControllerServerStartFailed)
                    ),
                    true,
                ));
            }
        }
    }

    fn copy_controller_invite(&mut self, slot: ControllerSlot) {
        let Some(invite) = self.controller_pairing_invite(slot) else {
            self.controller_setup.notice = Some((
                self.text(UiText::ControllerInviteUnavailable).to_owned(),
                true,
            ));
            return;
        };
        self.controller_setup.notice = Some(match self.clipboard.copy_text(invite.expose()) {
            Ok(()) => (self.text(UiText::ControllerInviteCopied).to_owned(), false),
            Err(error) => (
                format!("{}: {error}", self.text(UiText::ControllerInviteCopyFailed)),
                true,
            ),
        });
    }

    fn rotate_controller_pairing(&mut self, slot: ControllerSlot) {
        let Some(controllers) = self.lan_controllers.as_ref() else {
            self.controller_setup.notice = Some((
                self.text(UiText::ControllerStartServerFirst).to_owned(),
                true,
            ));
            return;
        };
        match controllers.rotate_pairing(slot) {
            Ok(_) => {
                self.controller_setup.invite_revealed[slot.index()] = false;
                self.controller_setup.notice = Some((
                    self.text(UiText::ControllerPairingRotated).to_owned(),
                    false,
                ));
            }
            Err(error) => {
                self.controller_setup.notice = Some((
                    format!(
                        "{}: {error}",
                        self.text(UiText::ControllerPairingRotateFailed)
                    ),
                    true,
                ));
            }
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> Result<()> {
        if let Some((player_index, slot)) = self.settings.capture {
            match key.code {
                KeyCode::Esc => {
                    self.settings.capture = None;
                    self.settings.status =
                        Some((self.text(UiText::KeyCaptureCancelled).to_owned(), false));
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(issue) =
                        binding_candidate_issue(&self.settings.draft, player_index, slot, character)
                    {
                        let message = match issue {
                            BindingCandidateIssue::VisibleAsciiRequired => {
                                self.text(UiText::BindingVisibleAsciiRequired).to_owned()
                            }
                            BindingCandidateIssue::PauseKeyReserved => {
                                self.text(UiText::PauseKeyReserved).to_owned()
                            }
                            BindingCandidateIssue::AlreadyAssigned => {
                                self.localizer().message(UiMessage::BindingAlreadyAssigned {
                                    key: character.to_ascii_uppercase(),
                                })
                            }
                        };
                        self.settings.status = Some((message, true));
                        return Ok(());
                    }
                    match self
                        .settings
                        .draft
                        .set_binding(player_index, slot, character)
                    {
                        Ok(()) => {
                            self.settings.capture = None;
                            let binding = self.localizer().binding_slot(slot);
                            self.settings.status = Some((
                                self.localizer().message(UiMessage::BindingChanged {
                                    player: player_index + 1,
                                    binding,
                                    key: character.to_ascii_uppercase(),
                                }),
                                false,
                            ));
                            self.play_binding_test(slot)?;
                        }
                        Err(error) => {
                            self.settings.status = Some((
                                self.localizer().message(UiMessage::BindingChangeFailed {
                                    reason: &error.to_string(),
                                }),
                                true,
                            ));
                        }
                    }
                }
                _ => {
                    self.settings.status =
                        Some((self.text(UiText::VisibleKeyOrCancel).to_owned(), true));
                }
            }
            return Ok(());
        }

        if self.settings.selected_item() != SettingsItem::PlayerName {
            if let Some((player_index, slot)) = binding_for_key(&self.settings.draft, key) {
                let binding = self.localizer().binding_slot(slot);
                self.settings.status = Some((
                    self.localizer().message(UiMessage::BindingDetected {
                        player: player_index + 1,
                        binding,
                    }),
                    false,
                ));
                self.play_binding_test(slot)?;
                return Ok(());
            }
        }

        match key.code {
            KeyCode::Esc => {
                self.settings = SettingsState::new(self.preferences.clone());
                self.page = Page::ModeSelect;
            }
            KeyCode::Up | KeyCode::BackTab => {
                self.settings.selected =
                    wrapped_selection(self.settings.selected, SettingsItem::ALL.len(), -1);
                self.settings.status = None;
            }
            KeyCode::Down | KeyCode::Tab => {
                self.settings.selected =
                    wrapped_selection(self.settings.selected, SettingsItem::ALL.len(), 1);
                self.settings.status = None;
            }
            KeyCode::Left => self.adjust_settings_item(-1),
            KeyCode::Right => self.adjust_settings_item(1),
            KeyCode::Backspace if self.settings.selected_item() == SettingsItem::PlayerName => {
                pop_grapheme(&mut self.settings.draft.player_name);
                self.settings.status = None;
            }
            KeyCode::Char(character)
                if self.settings.selected_item() == SettingsItem::PlayerName
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                let next_len = self.settings.draft.player_name.len() + character.len_utf8();
                if next_len <= taiko_multiplayer_protocol::MAX_DISPLAY_NAME_BYTES {
                    self.settings.draft.player_name.push(character);
                    self.settings.status = None;
                } else {
                    self.settings.status = Some((
                        self.localizer().message(UiMessage::OnlineNameByteLimit {
                            max_bytes: taiko_multiplayer_protocol::MAX_DISPLAY_NAME_BYTES,
                        }),
                        true,
                    ));
                }
            }
            KeyCode::Enter => match self.settings.selected_item() {
                SettingsItem::Binding { player_index, slot } => {
                    self.settings.capture = Some((player_index, slot));
                    let binding = self.localizer().binding_slot(slot);
                    self.settings.status = Some((
                        self.localizer().message(UiMessage::PressNewBinding {
                            player: player_index + 1,
                            binding,
                        }),
                        false,
                    ));
                }
                SettingsItem::Save => self.save_settings(),
                SettingsItem::Demo => self.adjust_settings_item(1),
                SettingsItem::Language
                | SettingsItem::SongVolume
                | SettingsItem::SeVolume
                | SettingsItem::Calibration
                | SettingsItem::ScrollSpeed
                | SettingsItem::PlayerName => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn adjust_settings_item(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        match self.settings.selected_item() {
            SettingsItem::Language => {
                self.settings.draft.ui_language = self.settings.draft.ui_language.cycle(delta);
            }
            SettingsItem::SongVolume => {
                self.settings.draft.song_volume =
                    (i32::from(self.settings.draft.song_volume) + delta).clamp(0, 100) as u8;
            }
            SettingsItem::SeVolume => {
                self.settings.draft.se_volume =
                    (i32::from(self.settings.draft.se_volume) + delta).clamp(0, 100) as u8;
            }
            SettingsItem::Calibration => {
                self.settings.draft.calibration_offset_ms =
                    adjust_offset_ms(self.settings.draft.calibration_offset_ms, delta);
            }
            SettingsItem::ScrollSpeed => {
                let runtime = stored_scroll_speed_to_runtime(self.settings.draft.scroll_speed);
                self.settings.draft.scroll_speed =
                    runtime_scroll_speed_to_stored(cycle_scroll_speed_setting(runtime, delta));
            }
            SettingsItem::Demo => {
                self.settings.draft.demo_enabled = !self.settings.draft.demo_enabled;
            }
            SettingsItem::PlayerName | SettingsItem::Binding { .. } | SettingsItem::Save => {}
        }
        self.settings.status = None;
    }

    fn save_settings(&mut self) {
        let mut preferences = self.settings.draft.clone();
        preferences.player_name = preferences.player_name.trim().to_owned();
        let result = preferences
            .validate()
            .and_then(|()| self.save_preferences(&preferences))
            .and_then(|()| self.apply_preferences(preferences));
        match result {
            Ok(()) => {
                self.page = Page::ModeSelect;
            }
            Err(error) => {
                let details = format!("{error:#}");
                self.settings.status = Some((
                    self.localizer()
                        .message(UiMessage::SettingsNotSaved { details: &details }),
                    true,
                ));
            }
        }
    }

    fn play_binding_test(&mut self, slot: BindingSlot) -> Result<()> {
        match slot {
            BindingSlot::LeftDon | BindingSlot::RightDon => self.play_don_se(),
            BindingSlot::LeftKat | BindingSlot::RightKat => self.play_kat_se(),
        }
    }

    fn handle_song_menu_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(
            key,
            KeyEvent {
                code: KeyCode::Char('r' | 'R'),
                modifiers,
                ..
            } if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        ) && self.songs.is_empty()
            && self.library_load_identity.is_none()
        {
            self.begin_library_load()?;
            return Ok(());
        }
        if is_load_warnings_hotkey(key) {
            self.page = Page::LoadWarnings;
            self.load_warnings_scroll = 0;
            return Ok(());
        }

        if self.handle_song_menu_search_key(key) {
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit | MenuIntent::Back => self.return_to_mode_select()?,
            MenuIntent::Confirm => {
                if self.selected_song().is_some() {
                    match self.active_mode {
                        Some(GameMode::LocalTwoPlayer) => {
                            let course_count =
                                self.selected_song().map_or(0, |song| song.courses.len());
                            self.local_course_selection
                                .reset_to_course(self.course_index, course_count);
                            self.course_setting_focus = CourseSettingFocus::SongVolume;
                            self.persist_recent_song(self.course_index)?;
                            self.page = Page::LocalCourseSelect;
                        }
                        Some(GameMode::SinglePlayer) => {
                            self.persist_recent_song(self.course_index)?;
                            self.page = Page::CourseMenu;
                            self.refresh_vsync_scroll_speed()?;
                            self.schedule_demo();
                        }
                        Some(GameMode::OnlineMultiplayer) | None => {
                            bail!("song menu is active without an offline play mode");
                        }
                    }
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
                if pop_grapheme(&mut self.song_query) {
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
                if c.is_control() {
                    return true;
                }
                if push_bounded_utf8(&mut self.song_query, c, MAX_STORED_QUERY_BYTES) {
                    if let Err(error) = self.rebuild_song_filter() {
                        self.set_error_state(error);
                    }
                } else {
                    self.song_filter_error = Some(format!(
                        "Search text is limited to {MAX_STORED_QUERY_BYTES} UTF-8 bytes"
                    ));
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
                self.persist_recent_song(self.course_index)?;
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

    fn handle_game_key(&mut self, key: KeyEvent, observed_at: Instant) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) || is_game_pause_toggle_key(key) {
            self.flush_controller_inputs_before_state_change()?;
        }
        match leave_confirmation_action(self.leave_confirmation, LeaveTarget::SinglePlayer, key) {
            LeaveConfirmationAction::Open => {
                self.leave_confirmation = Some(LeaveTarget::SinglePlayer);
                return Ok(());
            }
            LeaveConfirmationAction::Confirm => {
                self.leave_confirmation = None;
                self.abort_game_to_course()?;
                return Ok(());
            }
            LeaveConfirmationAction::Cancel => {
                self.leave_confirmation = None;
            }
            LeaveConfirmationAction::None => {}
        }
        if matches!(key.code, KeyCode::Esc) {
            return Ok(());
        }

        if is_game_pause_toggle_key(key) {
            self.toggle_game_pause()?;
            return Ok(());
        }

        if self.game.as_ref().is_some_and(|game| game.paused) {
            return Ok(());
        }

        let Some(action) = map_bound_game_hit(key, self.preferences.player_one) else {
            return Ok(());
        };
        self.enqueue_controller_strike(ControllerStrike::local(
            ControllerSlot::One,
            ControllerSource::Keyboard,
            action,
            observed_at,
        ))
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
        if matches!(key.code, KeyCode::Char('d' | 'D') | KeyCode::Tab) {
            self.result_details_visible = !self.result_details_visible;
            return Ok(());
        }
        if let Some(intent) = map_menu_intent(key) {
            match intent {
                MenuIntent::Confirm => {
                    self.start_game()?;
                }
                MenuIntent::Back => {
                    self.page = Page::SongMenu;
                    self.result = None;
                    self.result_details_visible = false;
                    self.schedule_demo();
                }
                MenuIntent::Quit => self.return_to_mode_select()?,
                MenuIntent::Up | MenuIntent::Down | MenuIntent::Left | MenuIntent::Right => {}
            }
        }

        Ok(())
    }

    fn handle_local_course_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) {
            self.persist_recent_song(self.local_course_selection.course_index(LocalPlayerId::One))?;
            self.page = Page::SongMenu;
            self.schedule_demo();
            return Ok(());
        }
        if matches!(key.code, KeyCode::Tab) {
            self.course_setting_focus = self.course_setting_focus.next();
            if self.course_setting_focus == CourseSettingFocus::AutoPlay {
                self.course_setting_focus = self.course_setting_focus.next();
            }
            return Ok(());
        }
        if matches!(key.code, KeyCode::BackTab) {
            self.course_setting_focus = self.course_setting_focus.prev();
            if self.course_setting_focus == CourseSettingFocus::AutoPlay {
                self.course_setting_focus = self.course_setting_focus.prev();
            }
            return Ok(());
        }
        if matches!(key.code, KeyCode::Left) {
            self.adjust_course_setting(-1)?;
            return Ok(());
        }
        if matches!(key.code, KeyCode::Right) {
            self.adjust_course_setting(1)?;
            return Ok(());
        }

        let Some((player, intent)) = map_local_course_key(key) else {
            return Ok(());
        };
        let course_count = self.selected_song().map_or(0, |song| song.courses.len());
        let was_ready = self.local_course_selection.is_ready(player);
        self.local_course_selection
            .apply(player, intent, course_count);
        if matches!(
            intent,
            crate::local_multiplayer::LocalCourseIntent::ToggleReady
        ) {
            self.play_don_se()?;
        } else if !was_ready {
            self.play_kat_se()?;
        }

        if self.local_course_selection.both_ready() {
            self.start_local_game()?;
        }
        Ok(())
    }

    fn handle_local_game_key(&mut self, key: KeyEvent, observed_at: Instant) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) || is_game_pause_toggle_key(key) {
            self.flush_controller_inputs_before_state_change()?;
        }
        match leave_confirmation_action(self.leave_confirmation, LeaveTarget::LocalTwoPlayer, key) {
            LeaveConfirmationAction::Open => {
                self.leave_confirmation = Some(LeaveTarget::LocalTwoPlayer);
                return Ok(());
            }
            LeaveConfirmationAction::Confirm => {
                self.leave_confirmation = None;
                self.abort_local_game_to_courses()?;
                return Ok(());
            }
            LeaveConfirmationAction::Cancel => {
                self.leave_confirmation = None;
            }
            LeaveConfirmationAction::None => {}
        }
        if matches!(key.code, KeyCode::Esc) {
            return Ok(());
        }
        if is_game_pause_toggle_key(key) {
            self.toggle_local_game_pause()?;
            return Ok(());
        }
        if self.local_game.as_ref().is_some_and(|game| game.paused) {
            return Ok(());
        }

        let Some(input) = map_local_game_hit(
            key,
            self.preferences.player_one,
            self.preferences.player_two,
        ) else {
            return Ok(());
        };
        let slot = match input.player {
            LocalPlayerId::One => ControllerSlot::One,
            LocalPlayerId::Two => ControllerSlot::Two,
        };
        self.enqueue_controller_strike(ControllerStrike::local(
            slot,
            ControllerSource::Keyboard,
            input.action(),
            observed_at,
        ))
    }

    fn handle_local_result_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Char('d' | 'D') | KeyCode::Tab) {
            self.result_details_visible = !self.result_details_visible;
            return Ok(());
        }
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };
        match intent {
            MenuIntent::Confirm => {
                self.start_local_game()?;
            }
            MenuIntent::Back => {
                self.page = Page::SongMenu;
                self.local_result = None;
                self.result_details_visible = false;
                self.schedule_demo();
            }
            MenuIntent::Quit => self.return_to_mode_select()?,
            MenuIntent::Up | MenuIntent::Down | MenuIntent::Left | MenuIntent::Right => {}
        }
        Ok(())
    }

    fn handle_error_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Char('d' | 'D') | KeyCode::Tab) {
            if let Some(state) = &mut self.error_state {
                state.details_visible = !state.details_visible;
            }
            return Ok(());
        }
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit => self.should_quit = true,
            MenuIntent::Back => self.recover_from_error(),
            MenuIntent::Confirm
                if self
                    .error_state
                    .as_ref()
                    .and_then(|state| state.retry)
                    .is_some() =>
            {
                self.retry_from_error()?;
            }
            MenuIntent::Confirm => self.recover_from_error(),
            MenuIntent::Up | MenuIntent::Down | MenuIntent::Left | MenuIntent::Right => {}
        }

        Ok(())
    }

    fn handle_mp_connect_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::F(2))
            && matches!(
                self.mp_connect.mode,
                ConnectMode::Join | ConnectMode::Spectate
            )
        {
            self.mp_connect.invite_revealed = !self.mp_connect.invite_revealed;
            return Ok(());
        }
        if self.online_bootstrap_identity.is_some() || self.embedded_server_start_identity.is_some()
        {
            if matches!(key.code, KeyCode::Esc) {
                self.cancel_online_bootstrap();
                self.cancel_embedded_server_start();
                self.stop_embedded_server()?;
                self.page = Page::ModeSelect;
                self.active_mode = None;
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Esc => {
                self.return_to_mode_select()?;
                return Ok(());
            }
            KeyCode::Up => {
                let mp = &mut self.mp_connect;
                mp.focus = match mp.focus {
                    ConnectField::Mode => ConnectField::Mode,
                    ConnectField::Server => ConnectField::Mode,
                    ConnectField::Invite => ConnectField::Mode,
                    ConnectField::Name => match mp.mode {
                        ConnectMode::Host => ConnectField::Mode,
                        ConnectMode::Create => ConnectField::Server,
                        ConnectMode::Join | ConnectMode::Spectate => ConnectField::Invite,
                    },
                    ConnectField::Confirm => ConnectField::Name,
                };
                return Ok(());
            }
            KeyCode::Down | KeyCode::Tab => {
                let mp = &mut self.mp_connect;
                mp.focus = match mp.focus {
                    ConnectField::Mode => match mp.mode {
                        ConnectMode::Host => ConnectField::Name,
                        ConnectMode::Create => ConnectField::Server,
                        ConnectMode::Join | ConnectMode::Spectate => ConnectField::Invite,
                    },
                    ConnectField::Server | ConnectField::Invite => ConnectField::Name,
                    ConnectField::Name => ConnectField::Confirm,
                    ConnectField::Confirm => ConnectField::Confirm,
                };
                return Ok(());
            }
            _ => {}
        }

        let mp = &mut self.mp_connect;
        match mp.focus {
            ConnectField::Mode => match key.code {
                KeyCode::Left => {
                    mp.mode = match mp.mode {
                        ConnectMode::Host => ConnectMode::Spectate,
                        ConnectMode::Create => ConnectMode::Host,
                        ConnectMode::Join => ConnectMode::Create,
                        ConnectMode::Spectate => ConnectMode::Join,
                    };
                    mp.error = None;
                    mp.status = None;
                }
                KeyCode::Right => {
                    mp.mode = match mp.mode {
                        ConnectMode::Host => ConnectMode::Create,
                        ConnectMode::Create => ConnectMode::Join,
                        ConnectMode::Join => ConnectMode::Spectate,
                        ConnectMode::Spectate => ConnectMode::Host,
                    };
                    mp.error = None;
                    mp.status = None;
                }
                KeyCode::Enter => {
                    mp.focus = match mp.mode {
                        ConnectMode::Host => ConnectField::Name,
                        ConnectMode::Create => ConnectField::Server,
                        ConnectMode::Join | ConnectMode::Spectate => ConnectField::Invite,
                    };
                }
                _ => {}
            },
            ConnectField::Server => match key.code {
                KeyCode::Char(c) => {
                    if !push_bounded_utf8(&mut mp.server, c, MAX_SERVER_URL_INPUT_BYTES) {
                        mp.error = Some(MultiplayerConnectError::ServerUrlTooLong {
                            max_bytes: MAX_SERVER_URL_INPUT_BYTES,
                        });
                    } else {
                        mp.error = None;
                    }
                    mp.status = None;
                }
                KeyCode::Backspace => {
                    pop_grapheme(&mut mp.server);
                    mp.error = None;
                    mp.status = None;
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Name;
                }
                _ => {}
            },
            ConnectField::Invite => match key.code {
                KeyCode::Char(c) => {
                    if !push_bounded_utf8(&mut mp.invite, c, MAX_INVITE_URL_INPUT_BYTES) {
                        mp.error = Some(MultiplayerConnectError::InviteTooLong {
                            max_bytes: MAX_INVITE_URL_INPUT_BYTES,
                        });
                    } else {
                        mp.error = None;
                    }
                    mp.status = None;
                }
                KeyCode::Backspace => {
                    pop_grapheme(&mut mp.invite);
                    mp.error = None;
                    mp.status = None;
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Name;
                }
                _ => {}
            },
            ConnectField::Name => match key.code {
                KeyCode::Char(c) => {
                    if !push_bounded_utf8(
                        &mut mp.name,
                        c,
                        taiko_multiplayer_protocol::MAX_DISPLAY_NAME_BYTES,
                    ) {
                        mp.error = Some(MultiplayerConnectError::NameTooLong {
                            max_bytes: taiko_multiplayer_protocol::MAX_DISPLAY_NAME_BYTES,
                        });
                    } else {
                        mp.error = None;
                    }
                    mp.status = None;
                }
                KeyCode::Backspace => {
                    pop_grapheme(&mut mp.name);
                    mp.error = None;
                    mp.status = None;
                }
                KeyCode::Enter => {
                    mp.focus = ConnectField::Confirm;
                }
                _ => {}
            },
            ConnectField::Confirm => {
                if matches!(key.code, KeyCode::Enter) {
                    if mp.name.is_empty() {
                        mp.error = Some(MultiplayerConnectError::NameRequired);
                        return Ok(());
                    }
                    let request = match mp.mode {
                        ConnectMode::Host => OnlineConnectRequest::Host {
                            name: mp.name.clone(),
                        },
                        ConnectMode::Create if mp.server.is_empty() => {
                            mp.error = Some(MultiplayerConnectError::ServerRequired);
                            return Ok(());
                        }
                        ConnectMode::Create => OnlineConnectRequest::Connect(Box::new(
                            match crate::online::OnlineClientConfig::create(&mp.server, &mp.name) {
                                Ok(config) => config,
                                Err(error) => {
                                    mp.error = Some(MultiplayerConnectError::InvalidServer {
                                        reason: error.to_string(),
                                    });
                                    return Ok(());
                                }
                            },
                        )),
                        ConnectMode::Join | ConnectMode::Spectate if mp.invite.is_empty() => {
                            mp.error = Some(MultiplayerConnectError::InviteRequired);
                            return Ok(());
                        }
                        ConnectMode::Join | ConnectMode::Spectate => {
                            let invite = match crate::invite::MultiplayerInvite::parse(&mp.invite) {
                                Ok(invite) => invite,
                                Err(error) => {
                                    mp.error = Some(MultiplayerConnectError::InvalidInvite {
                                        reason: error.to_string(),
                                    });
                                    return Ok(());
                                }
                            };
                            let config = match crate::online::OnlineClientConfig::join(
                                invite.server().as_str(),
                                &mp.name,
                                invite.room_code().as_str(),
                                invite.invitation_token().expose(),
                                if mp.mode == ConnectMode::Join {
                                    taiko_multiplayer_protocol::JoinRole::Player
                                } else {
                                    taiko_multiplayer_protocol::JoinRole::Spectator
                                },
                            ) {
                                Ok(config) => config,
                                Err(error) => {
                                    mp.error = Some(MultiplayerConnectError::InvalidInvite {
                                        reason: error.to_string(),
                                    });
                                    return Ok(());
                                }
                            };
                            OnlineConnectRequest::Connect(Box::new(config))
                        }
                    };

                    let connect_result = match request {
                        OnlineConnectRequest::Host { name } => self.host_online_locally(&name),
                        OnlineConnectRequest::Connect(config) => {
                            self.connect_online_config(*config)
                        }
                    };
                    match connect_result {
                        Ok(()) => {}
                        Err(error) => {
                            self.mp_connect.error = Some(MultiplayerConnectError::Technical {
                                reason: error.to_string(),
                            });
                        }
                    }
                }
            }
        }

        Ok(())
    }

    // ── Online connection ─────────────────────────────────────────────

    fn reset_multiplayer_connect(&mut self) {
        self.mp_connect.mode = ConnectMode::Host;
        self.mp_connect.focus = ConnectField::Mode;
        self.mp_connect.status = None;
        self.mp_connect.error = None;
        self.mp_connect.invite_revealed = false;
    }

    fn host_online_locally(&mut self, name: &str) -> Result<()> {
        if self.embedded_server.is_some() || self.embedded_server_start_identity.is_some() {
            bail!("an embedded server is already active or starting");
        }
        let generation = self
            .embedded_server_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("embedded server generation exhausted"))?;
        let identity = EmbeddedServerStartIdentity { generation };
        self.embedded_server_start
            .start(identity, self.args.songdir.clone(), name.to_owned())?;
        self.embedded_server_generation = generation;
        self.embedded_server_start_identity = Some(identity);
        self.mp_connect.error = None;
        self.mp_connect.status = Some(MultiplayerConnectStatus::StartingPrivateServer);
        Ok(())
    }

    fn poll_embedded_server_start(&mut self) -> Result<()> {
        for event in self.embedded_server_start.poll() {
            if !embedded_start_event_is_current(self.embedded_server_start_identity, &event) {
                continue;
            }
            self.embedded_server_start_identity = None;
            self.mp_connect.status = None;
            match event.completion {
                EmbeddedServerStartCompletion::Completed(prepared) => {
                    if let Err(error) = self.activate_prepared_embedded_server(*prepared) {
                        self.mp_connect.error = Some(MultiplayerConnectError::Technical {
                            reason: error.to_string(),
                        });
                        self.page = Page::MultiplayerConnect;
                    }
                }
                EmbeddedServerStartCompletion::Cancelled => {
                    self.mp_connect.error = Some(MultiplayerConnectError::LocalHostingCancelled);
                    self.page = Page::MultiplayerConnect;
                }
                EmbeddedServerStartCompletion::Failed(error) => {
                    self.mp_connect.error = Some(MultiplayerConnectError::Technical {
                        reason: error.to_string(),
                    });
                    self.page = Page::MultiplayerConnect;
                }
            }
        }
        Ok(())
    }

    fn activate_prepared_embedded_server(
        &mut self,
        prepared: PreparedEmbeddedServer,
    ) -> Result<()> {
        let PreparedEmbeddedServer {
            server,
            server_url,
            display_name,
        } = prepared;
        let config =
            match crate::online::OnlineClientConfig::create(server_url.as_str(), &display_name) {
                Ok(config) => config,
                Err(error) => {
                    return match server.shutdown_and_join() {
                        Ok(()) => Err(error),
                        Err(shutdown_error) => Err(anyhow!(
                            "{error}; embedded server shutdown failed: {shutdown_error}"
                        )),
                    };
                }
            };
        self.embedded_server = Some(server);
        if let Err(error) = self.connect_online_config(config) {
            return match self.stop_embedded_server() {
                Ok(()) => Err(error),
                Err(shutdown_error) => Err(anyhow!(
                    "{error}; embedded server shutdown failed: {shutdown_error}"
                )),
            };
        };
        Ok(())
    }

    fn cancel_embedded_server_start(&mut self) {
        self.embedded_server_start.cancel();
        self.embedded_server_start_identity = None;
    }

    fn stop_embedded_server(&mut self) -> Result<()> {
        self.embedded_server
            .take()
            .map_or(Ok(()), crate::online::EmbeddedServer::shutdown_and_join)
    }

    pub(crate) fn connect_online_config(
        &mut self,
        config: crate::online::OnlineClientConfig,
    ) -> Result<()> {
        if self.online.is_some()
            || self.offline_resources.is_some()
            || self.online_bootstrap_identity.is_some()
        {
            bail!("an online session is already active");
        }

        let generation = self
            .online_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("online session generation exhausted"))?;
        let identity = BootstrapIdentity { generation };

        let requires_resources = config.requires_authoritative_resources();
        self.online_bootstrap
            .start(identity, config, self.args.resource_cache_memory_only)?;
        self.online_generation = generation;
        self.online_bootstrap_identity = Some(identity);
        self.mp_connect.error = None;
        self.mp_connect.status = Some(if requires_resources {
            MultiplayerConnectStatus::LoadingAuthoritativeLibrary
        } else {
            MultiplayerConnectStatus::PreparingSpectatorConnection
        });
        self.page = Page::MultiplayerConnect;

        Ok(())
    }

    fn poll_online_bootstrap(&mut self) -> Result<()> {
        for event in self.online_bootstrap.poll() {
            if !event_is_current(self.online_bootstrap_identity, &event) {
                continue;
            }

            self.online_bootstrap_identity = None;
            self.mp_connect.status = None;
            match event.completion {
                BootstrapCompletion::Completed(prepared) => {
                    if let Err(error) = self.activate_online_bootstrap(*prepared) {
                        let reason = match self.teardown_online() {
                            Ok(()) => error.to_string(),
                            Err(cleanup_error) => {
                                format!("{error}; online cleanup failed: {cleanup_error}")
                            }
                        };
                        self.mp_connect.error = Some(MultiplayerConnectError::Technical { reason });
                        self.page = Page::MultiplayerConnect;
                    }
                }
                BootstrapCompletion::Cancelled => {
                    self.mp_connect.error = Some(match self.stop_embedded_server() {
                        Ok(()) => MultiplayerConnectError::OnlineConnectionCancelled,
                        Err(error) => MultiplayerConnectError::Technical {
                            reason: format!(
                                "Online connection was cancelled; server shutdown failed: {error}"
                            ),
                        },
                    });
                    self.page = Page::MultiplayerConnect;
                }
                BootstrapCompletion::Failed(error) => {
                    let reason = match self.stop_embedded_server() {
                        Ok(()) => error.to_string(),
                        Err(cleanup_error) => {
                            format!("{error}; server shutdown failed: {cleanup_error}")
                        }
                    };
                    self.mp_connect.error = Some(MultiplayerConnectError::Technical { reason });
                    self.page = Page::MultiplayerConnect;
                }
            }
        }

        Ok(())
    }

    fn activate_online_bootstrap(&mut self, prepared: PreparedBootstrap) -> Result<()> {
        if self.online.is_some() || self.offline_resources.is_some() {
            bail!("an online session became active during bootstrap");
        }

        self.cancel_demo_preview_load();
        self.audio.stop_song()?;
        let (backend, library) = match prepared.resources {
            PreparedBootstrapResources::Authority { backend, library } => (backend, library),
            PreparedBootstrapResources::Spectator => {
                let (backend, library) = online_placeholder_resources(&self.args);
                (Arc::new(backend), library)
            }
        };
        let online = crate::online_session::OnlineDomain::connect(prepared.config)?;
        self.resume_library_load_after_online = self.library_load_identity.is_some();
        if self.resume_library_load_after_online {
            self.cancel_library_load();
        }
        let offline_resources =
            OfflineResourceState::install(self.resource_state_slots(), backend, library);
        self.demo_pending = None;
        self.demo_playing_song = None;
        self.offline_resources = Some(offline_resources);
        self.online = Some(online);
        self.invite_revealed = false;
        self.invite_copy_status = None;
        self.leave_confirmation = None;
        self.page = Page::OnlineLobby;

        Ok(())
    }

    fn cancel_online_bootstrap(&mut self) {
        self.online_bootstrap.cancel();
        self.online_bootstrap_identity = None;
        self.mp_connect.status = None;
    }

    fn disconnect_online(&mut self) -> Result<()> {
        let teardown_result = self.teardown_online();
        let library_result = self.resume_offline_library_load();
        self.page = Page::ModeSelect;
        self.active_mode = None;
        self.error_state = None;
        self.invite_revealed = false;
        self.invite_copy_status = None;
        self.leave_confirmation = None;
        combine_cleanup_results(teardown_result, library_result)
    }

    fn teardown_online(&mut self) -> Result<()> {
        self.cancel_embedded_server_start();
        self.cancel_online_bootstrap();
        self.cancel_demo_preview_load();
        self.online_preparation.cancel();
        let _ = self.online_preparation.poll();

        let had_online = self.online.is_some();
        let shutdown_result = self
            .online
            .as_mut()
            .map(crate::online_session::OnlineDomain::shutdown_gracefully)
            .transpose()
            .map(|shutdown| shutdown.unwrap_or(()));
        self.online = None;

        let audio_result = self.audio.stop_song();
        self.demo_pending = None;
        self.demo_playing_song = None;
        self.demo_preview_status = None;

        let restore_result = match self.offline_resources.take() {
            Some(offline) => {
                offline.restore(self.resource_state_slots());
                Ok(())
            }
            None if had_online => Err(anyhow!(
                "online session had no saved offline resource state to restore"
            )),
            None => Ok(()),
        };
        let embedded_server_result = self.stop_embedded_server();

        combine_cleanup_results(
            combine_cleanup_results(
                combine_cleanup_results(shutdown_result, audio_result),
                restore_result,
            ),
            embedded_server_result,
        )
    }

    fn resource_state_slots(&mut self) -> ResourceStateSlots<'_> {
        ResourceStateSlots {
            backend: &mut self.resource_backend,
            songs: &mut self.songs,
            filtered_song_indices: &mut self.filtered_song_indices,
            song_query: &mut self.song_query,
            song_filter_error: &mut self.song_filter_error,
            song_index: &mut self.song_index,
            course_index: &mut self.course_index,
            load_warnings: &mut self.load_warnings,
            load_warnings_scroll: &mut self.load_warnings_scroll,
            loaded_course_chart: &mut self.loaded_course_chart,
            offline_library_status: &mut self.offline_library_status,
        }
    }

    fn process_online_actions(&mut self) -> Result<()> {
        let actions = match &mut self.online {
            Some(online) if !online.pending_actions.is_empty() => {
                std::mem::take(&mut online.pending_actions)
            }
            _ => return Ok(()),
        };

        for action in actions {
            match action {
                crate::online_session::DomainAction::SongChanged { song } => {
                    if let Some(idx) = self
                        .songs
                        .iter()
                        .position(|entry| entry.song_id() == Some(song.song_id.as_str()))
                    {
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
                        online.local_course_index = 0;
                    }
                    let is_player = self
                        .online
                        .as_ref()
                        .is_some_and(|online| online.role() == Some(RoomRole::Player));
                    if is_player && matches!(self.page, Page::OnlineLobby | Page::OnlineResult) {
                        self.page = Page::OnlineCourseSelect;
                    }
                }
                crate::online_session::DomainAction::PhaseChanged(phase) => {
                    let is_player = self
                        .online
                        .as_ref()
                        .is_some_and(|online| online.role() == Some(RoomRole::Player));
                    self.page = online_page_after_phase(self.page, phase, is_player);
                }
                crate::online_session::DomainAction::PlaybackInvalidated(reason) => {
                    self.reset_online_match_playback(reason)?;
                }
            }
        }

        if !self.demo_preview_page_is_active() {
            self.stop_demo_preview_nonfatal();
        }
        Ok(())
    }

    fn reset_online_match_playback(
        &mut self,
        reason: crate::online_session::OnlinePlaybackInvalidation,
    ) -> Result<()> {
        self.audio
            .stop_song()
            .with_context(|| format!("failed to stop invalidated online playback: {reason:?}"))?;
        if let Some(runtime) = self
            .online
            .as_mut()
            .and_then(|online| online.local_player.as_mut())
        {
            runtime.music_started = false;
            runtime.audio_sync = None;
        }
        Ok(())
    }

    // ── Online page handlers ─────────────────────────────────────────

    fn tick_online_lobby(&mut self) -> Result<()> {
        self.tick_demo_preview()?;
        if let Some(online) = &self.online {
            if online.is_terminal() {
                let message = online
                    .error()
                    .map(|error| error.display_message())
                    .unwrap_or_else(|| online.status_message().to_owned());
                bail!("{message}");
            }
        }
        Ok(())
    }

    fn tick_online_match_phase(&mut self) -> Result<()> {
        if let Some(online) = &self.online {
            let phase = online.phase();
            match (self.page, phase) {
                (
                    Page::OnlineCourseSelect,
                    crate::online_session::OnlinePhase::Countdown
                    | crate::online_session::OnlinePhase::Playing,
                ) => {
                    self.page = Page::OnlineMatch;
                }
                (Page::OnlineMatch, crate::online_session::OnlinePhase::Results) => {
                    self.leave_confirmation = None;
                    self.page = Page::OnlineResult;
                }
                _ => {}
            }
            if online.is_terminal() {
                let message = online
                    .error()
                    .map(|error| error.display_message())
                    .unwrap_or_else(|| online.status_message().to_owned());
                bail!("{message}");
            }
        }

        // Prepare the match (load chart/audio) if not yet done
        self.ensure_online_match_prepared()?;
        // Tick the local player engine
        self.tick_online_player_runtime()?;

        Ok(())
    }

    fn ensure_online_match_prepared(&mut self) -> Result<()> {
        let Some(identity) = self.current_preparation_identity() else {
            self.online_preparation.cancel();
            return Ok(());
        };
        let online = self
            .online
            .as_ref()
            .expect("identity requires online domain");

        if let Some(prepared) = online.prepared_match.as_ref().filter(|prepared| {
            prepared.match_id == identity.match_id && prepared.selection == identity.selection
        }) {
            if online.should_auto_ready() {
                if let Ok(proof) = online.preparation_proof(prepared) {
                    self.online
                        .as_mut()
                        .expect("online domain still exists")
                        .set_ready(true, Some(proof))?;
                }
            }
            return Ok(());
        }
        if self.online_preparation.failure_reason(identity).is_some() {
            return Ok(());
        }

        if let Some(running) = self.online_preparation.identity() {
            if running != identity {
                self.online_preparation.cancel();
            }
            return Ok(());
        }

        let song_manifest = online
            .current_song()
            .cloned()
            .ok_or_else(|| anyhow!("online match has no authoritative song manifest"))?;
        let song_idx = self
            .songs
            .iter()
            .position(|song| song.song_id() == Some(song_manifest.song_id.as_str()))
            .ok_or_else(|| {
                anyhow!(
                    "authoritative song {} is not present in the remote library",
                    song_manifest.song_id
                )
            })?;
        let course_idx = usize::try_from(identity.selection.course_id.0)
            .context("course id cannot be represented by this client")?;
        let song_entry = &self.songs[song_idx];
        let course = song_entry
            .courses
            .iter()
            .find(|course| course.index == course_idx)
            .ok_or_else(|| anyhow!("authoritative course {course_idx} is not available"))?;
        let course_manifest = song_manifest
            .courses
            .iter()
            .find(|course| course.course_id == identity.selection.course_id)
            .ok_or_else(|| anyhow!("course is absent from authoritative manifest"))?;

        validate_authoritative_song_identity(song_entry, &song_manifest)?;
        if course.canonical_chart_hash != course_manifest.canonical_chart_hash.as_str() {
            bail!("downloaded multiplayer content does not match the authoritative manifest");
        }

        let started = self.online_preparation.start(PreparationRequest {
            identity,
            backend: Arc::clone(&self.resource_backend),
            song: song_entry.clone(),
            course_index: course_idx,
            branch_decisions: course.branch_decisions.clone(),
        })?;
        if !started {
            bail!("online preparation task violated its single-worker invariant");
        }
        Ok(())
    }

    fn current_preparation_identity(&self) -> Option<PreparationIdentity> {
        let online = self.online.as_ref()?;
        if online.role() != Some(RoomRole::Player) {
            return None;
        }
        Some(PreparationIdentity {
            session_generation: self.online_generation,
            match_id: online.current_match_id()?,
            selection: online.local_selection()?,
        })
    }

    fn poll_online_preparation(&mut self) -> Result<()> {
        let current_identity = self.current_preparation_identity();
        if self
            .online_preparation
            .identity()
            .is_some_and(|running| Some(running) != current_identity)
        {
            self.online_preparation.cancel();
        }

        for event in self.online_preparation.poll() {
            let identity = event.identity();
            if current_identity != Some(identity) {
                continue;
            }

            match event {
                PreparationEvent::Progress { progress, .. } => {
                    self.online
                        .as_mut()
                        .expect("current preparation identity requires online domain")
                        .report_preparation(progress)?;
                }
                PreparationEvent::Finished { completion, .. } => match completion {
                    PreparationCompletion::Prepared(prepared) => {
                        let prepared = *prepared;
                        if prepared.prepared_match.match_id != identity.match_id
                            || prepared.prepared_match.selection != identity.selection
                            || prepared.runtime.match_id != identity.match_id
                        {
                            bail!("online preparation returned mismatched match identity");
                        }

                        let online = self
                            .online
                            .as_mut()
                            .expect("current preparation identity requires online domain");
                        online.prepared_match = Some(prepared.prepared_match);
                        online.local_player = Some(prepared.runtime);
                        if online.should_auto_ready() {
                            let proof = online.preparation_proof(
                                online.prepared_match.as_ref().expect("prepared match"),
                            )?;
                            online.set_ready(true, Some(proof))?;
                        }
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

    fn tick_online_player_runtime(&mut self) -> Result<()> {
        let Some(online) = &mut self.online else {
            return Ok(());
        };

        let phase = online.phase();
        if !matches!(
            phase,
            crate::online_session::OnlinePhase::Countdown
                | crate::online_session::OnlinePhase::Playing
                | crate::online_session::OnlinePhase::Finalizing
                | crate::online_session::OnlinePhase::Results
        ) {
            return Ok(());
        }

        let Some(prepared) = online.prepared_match.as_ref() else {
            return Ok(());
        };
        let Some(runtime) = online.local_player.as_ref() else {
            return Ok(());
        };
        if runtime.match_id != prepared.match_id {
            bail!("local online engine belongs to a stale match epoch");
        }

        let now_tick = online.estimated_server_tick().max(0);

        if phase == crate::online_session::OnlinePhase::Countdown {
            let runtime = online.local_player.as_ref().unwrap();
            if !runtime.music_started {
                let delay = online
                    .countdown_remaining()
                    .ok_or_else(|| anyhow!("countdown phase is missing its start deadline"))?;
                let audio = prepared.audio.clone();
                self.audio.stop_song()?;
                self.audio
                    .play_prepared_song_scheduled(audio, 0.0, false, delay)?;
                self.perf_meter.clear();
                let scheduled_start = Instant::now()
                    .checked_add(delay)
                    .ok_or_else(|| anyhow!("online audio start deadline overflow"))?;
                let runtime = online.local_player.as_mut().unwrap();
                runtime.music_started = true;
                runtime.audio_sync = Some(AudioSyncController::started(scheduled_start));
            }
        }

        if matches!(
            phase,
            crate::online_session::OnlinePhase::Playing
                | crate::online_session::OnlinePhase::Finalizing
                | crate::online_session::OnlinePhase::Results
        ) {
            let runtime = online.local_player.as_ref().unwrap();
            if !runtime.music_started {
                let start_seconds = (now_tick as f64 / 1_000_000.0).max(0.0);
                let audio = prepared.audio.clone();
                self.audio.stop_song()?;
                self.audio.play_prepared_song(audio, start_seconds, false)?;
                self.perf_meter.clear();
                let runtime = online.local_player.as_mut().unwrap();
                runtime.music_started = true;
                runtime.audio_sync = Some(AudioSyncController::started(Instant::now()));
            }
        }

        if matches!(
            phase,
            crate::online_session::OnlinePhase::Playing
                | crate::online_session::OnlinePhase::Finalizing
        ) {
            let authoritative_seconds = now_tick as f64 / 1_000_000.0;
            let audio_seconds = self.audio.song_position_seconds();
            let decision = online
                .local_player
                .as_mut()
                .and_then(|runtime| runtime.audio_sync.as_mut())
                .map_or(AudioSyncDecision::None, |sync| {
                    sync.observe(Instant::now(), authoritative_seconds, audio_seconds)
                });
            match decision {
                AudioSyncDecision::None => {}
                AudioSyncDecision::SetPlaybackRate(rate) => {
                    self.audio.set_song_playback_rate(rate)?;
                }
                AudioSyncDecision::SeekTo(seconds) => {
                    self.audio.set_song_playback_rate(1.0)?;
                    self.audio.seek_song(seconds)?;
                }
            }
        }

        let runtime = online.local_player.as_mut().unwrap();
        let frame_tick = now_tick.max(runtime.last_tick);
        let frame_inputs =
            crate::online::collect_due_inputs(&mut runtime.pending_inputs, frame_tick);

        if let Some(input) = frame_inputs.last().copied() {
            runtime.input_flash = Some(InputFlashState {
                action: input.action,
                until_tick: frame_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        let scheduled_inputs = frame_inputs
            .iter()
            .copied()
            .map(ScheduledTaikoInput::unconditional)
            .collect::<Vec<_>>();
        let tick_started = Instant::now();
        let output = runtime
            .gameplay
            .advance_to(frame_tick, &scheduled_inputs)
            .context("online player engine step failed")?;
        self.perf_meter.record_tick(tick_started.elapsed());

        if let Some(judge) = latest_flashable_judge(&output.judges) {
            runtime.judge_flash = Some(JudgeFlashState {
                judge,
                until_tick: frame_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        runtime.last_tick = output.now;
        runtime.last_output = output;

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

        Ok(())
    }

    fn handle_online_lobby_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::F(2)) {
            if self
                .online
                .as_ref()
                .and_then(crate::online_session::OnlineDomain::invite)
                .is_some()
            {
                self.invite_revealed = !self.invite_revealed;
                self.invite_copy_status = None;
            }
            return Ok(());
        }
        if matches!(key.code, KeyCode::F(3)) {
            self.copy_online_invite();
            return Ok(());
        }

        // Search key handling (same as song menu)
        if self.handle_song_menu_search_key(key) {
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        match intent {
            MenuIntent::Quit | MenuIntent::Back => {
                self.disconnect_online()?;
            }
            MenuIntent::Up | MenuIntent::Left => {
                let _ = self.move_song_selection(-1);
            }
            MenuIntent::Down | MenuIntent::Right => {
                let _ = self.move_song_selection(1);
            }
            MenuIntent::Confirm => {
                let is_leader = self
                    .online
                    .as_ref()
                    .is_some_and(|online| online.is_local_leader());
                if is_leader {
                    let song_id = self
                        .selected_song()
                        .and_then(|song| song.song_id().map(ToOwned::to_owned))
                        .ok_or_else(|| {
                            anyhow!("online song selection requires a validated remote song id")
                        })?;
                    let online = self.online.as_mut().expect("online domain still exists");
                    online.select_song(&song_id)?;
                    online.local_course_index = 0;
                }
            }
        }

        Ok(())
    }

    fn copy_online_invite(&mut self) {
        let Some(invite) = self
            .online
            .as_ref()
            .and_then(crate::online_session::OnlineDomain::invite)
        else {
            self.invite_copy_status = Some(InviteCopyStatus::Failed(
                "No active room invite is available".to_owned(),
            ));
            return;
        };
        self.invite_copy_status = Some(match self.clipboard.copy_text(&invite.to_string()) {
            Ok(()) => InviteCopyStatus::Copied,
            Err(error) => InviteCopyStatus::Failed(format!("{error:#}")),
        });
    }

    fn handle_online_course_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };
        let controls_enabled = self
            .online
            .as_ref()
            .is_some_and(crate::online_session::OnlineDomain::room_controls_enabled);

        let course_len = self.selected_song().map(|s| s.courses.len()).unwrap_or(0);

        match intent {
            MenuIntent::Back => {
                if !controls_enabled {
                    return Ok(());
                }
                if let Some(online) = &mut self.online {
                    if online.is_local_ready() || online.is_ready_command_pending() {
                        online.set_ready(false, None)?;
                    }
                }
            }
            MenuIntent::Quit => {
                self.disconnect_online()?;
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
                if !controls_enabled {
                    return Ok(());
                }
                let Some(course) = self.selected_song().and_then(|song| {
                    self.online
                        .as_ref()
                        .and_then(|online| song.courses.get(online.local_course_index))
                }) else {
                    return Ok(());
                };
                let course_id = u32::try_from(course.index)
                    .context("course index exceeds the multiplayer protocol")?;
                let selection =
                    official_online_selection(taiko_multiplayer_protocol::CourseId(course_id));
                let (authoritative, can_start_match) = self
                    .online
                    .as_ref()
                    .map(|online| (online.local_selection(), online.can_start_match()))
                    .unwrap_or((None, false));
                let preparation_failed = self.online_preparation_failure().is_some();
                match online_course_confirm_action(
                    selection,
                    authoritative,
                    can_start_match,
                    preparation_failed,
                ) {
                    OnlineCourseConfirmAction::SelectCourse => {
                        if let Some(identity) = self
                            .current_preparation_identity()
                            .filter(|identity| identity.selection == selection)
                        {
                            self.online_preparation.retry(identity);
                        }
                        if let Some(online) = &mut self.online {
                            online.select_course(selection)?;
                        }
                    }
                    OnlineCourseConfirmAction::StartMatch => {
                        self.online
                            .as_mut()
                            .expect("online domain still exists")
                            .start_match()?;
                    }
                    OnlineCourseConfirmAction::None => {}
                }
            }
        }

        Ok(())
    }

    pub(crate) fn online_preparation_failure(&self) -> Option<&str> {
        let identity = self.current_preparation_identity()?;
        self.online_preparation.failure_reason(identity)
    }

    fn handle_online_match_key(&mut self, key: KeyEvent, observed_at: Instant) -> Result<()> {
        if matches!(key.code, KeyCode::Esc) {
            self.flush_controller_inputs_before_state_change()?;
        }
        match leave_confirmation_action(self.leave_confirmation, LeaveTarget::OnlineMatch, key) {
            LeaveConfirmationAction::Open => {
                self.leave_confirmation = Some(LeaveTarget::OnlineMatch);
                return Ok(());
            }
            LeaveConfirmationAction::Confirm => {
                self.leave_confirmation = None;
                self.disconnect_online()?;
                return Ok(());
            }
            LeaveConfirmationAction::Cancel => {
                self.leave_confirmation = None;
            }
            LeaveConfirmationAction::None => {}
        }
        if matches!(key.code, KeyCode::Esc) {
            return Ok(());
        }

        let Some(action) = map_bound_game_hit(key, self.preferences.player_one) else {
            return Ok(());
        };
        self.enqueue_controller_strike(ControllerStrike::local(
            ControllerSlot::One,
            ControllerSource::Keyboard,
            action,
            observed_at,
        ))
    }

    fn handle_online_result_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };
        let is_leader = self
            .online
            .as_ref()
            .is_some_and(crate::online_session::OnlineDomain::is_local_leader);
        let controls_enabled = self
            .online
            .as_ref()
            .is_some_and(crate::online_session::OnlineDomain::room_controls_enabled);
        if !online_result_control_allowed(is_leader, controls_enabled, intent) {
            return Ok(());
        }
        match intent {
            MenuIntent::Confirm => {
                if let Some(online) = &mut self.online {
                    online.rematch()?;
                }
            }
            MenuIntent::Back => {
                if let Some(online) = &mut self.online {
                    online.return_to_lobby()?;
                }
            }
            MenuIntent::Quit => self.disconnect_online()?,
            MenuIntent::Up | MenuIntent::Down | MenuIntent::Left | MenuIntent::Right => {}
        }
        Ok(())
    }

    fn start_game(&mut self) -> Result<()> {
        self.cancel_demo_preview_load();
        self.cancel_offline_preparation();
        self.persist_recent_song(self.course_index)?;
        let song_index = self
            .selected_song_index()
            .ok_or_else(|| anyhow!("no selected song"))?;
        let song = self
            .songs
            .get(song_index)
            .cloned()
            .ok_or_else(|| anyhow!("selected song is unavailable"))?;
        song.courses
            .get(self.course_index)
            .ok_or_else(|| anyhow!("selected course is unavailable"))?;
        let generation = self
            .offline_preparation_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("offline preparation generation exhausted"))?;
        let identity =
            OfflinePreparationIdentity::single(generation, song_index, self.course_index);
        self.offline_preparation.start(OfflinePreparationRequest {
            identity,
            backend: Arc::clone(&self.resource_backend),
            song,
        })?;
        self.audio.stop_song()?;
        self.demo_pending = None;
        self.demo_playing_song = None;
        self.demo_preview_status = None;
        self.offline_preparation_generation = generation;
        self.offline_preparation_identity = Some(identity);
        self.page = Page::OfflinePreparation;
        self.leave_confirmation = None;
        Ok(())
    }

    fn begin_library_load(&mut self) -> Result<()> {
        if self.offline_resources.is_some() {
            bail!("cannot load the offline library while online resources are installed");
        }
        self.cancel_library_load();
        let generation = self
            .library_load_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("song library generation exhausted"))?;
        self.library_load
            .start(generation, Arc::clone(&self.resource_backend))?;
        self.library_load_generation = generation;
        self.library_load_identity = Some(generation);
        self.offline_library_status = Some(OfflineLibraryNotice::Loading);
        Ok(())
    }

    fn cancel_library_load(&mut self) {
        self.library_load.cancel();
        self.library_load_identity = None;
    }

    fn poll_library_load(&mut self) -> Result<()> {
        for event in self.library_load.poll() {
            if !library_load_event_is_current(self.library_load_identity, &event) {
                continue;
            }
            self.library_load_identity = None;
            match event.completion {
                LibraryLoadCompletion::Completed(library) => {
                    if self.offline_resources.is_some() {
                        bail!("offline library completed while online resources were installed");
                    }
                    let library = *library;
                    self.songs = library.songs;
                    self.load_warnings = library.warnings;
                    self.filtered_song_indices = (0..self.songs.len()).collect();
                    self.song_index = 0;
                    self.course_index = 0;
                    self.loaded_course_chart = None;
                    self.rebuild_song_filter()?;
                    self.offline_library_status = library_status_for_contents(
                        &self.songs,
                        &self.load_warnings,
                        self.args.resource_endpoint.is_some(),
                    );
                    self.restore_persisted_selection()?;
                }
                LibraryLoadCompletion::Cancelled => {}
                LibraryLoadCompletion::Failed(error) => {
                    self.offline_library_status = Some(OfflineLibraryNotice::LoadFailed {
                        reason: format!("{error:#}"),
                    });
                }
            }
        }
        Ok(())
    }

    fn resume_offline_library_load(&mut self) -> Result<()> {
        if !self.resume_library_load_after_online {
            return Ok(());
        }
        if self.offline_resources.is_some() {
            bail!("cannot resume the offline library while online resources are installed");
        }
        self.resume_library_load_after_online = false;
        if let Err(error) = self.begin_library_load() {
            self.resume_library_load_after_online = true;
            return Err(error);
        }
        Ok(())
    }

    fn restore_persisted_selection(&mut self) -> Result<()> {
        self.mode_selection = self
            .preferences
            .last_mode
            .map(stored_mode_index)
            .unwrap_or_default();

        let Some(recent) = self.preferences.recent_song.clone() else {
            return Ok(());
        };

        self.song_query = recent.query;
        self.rebuild_song_filter()?;

        let persisted_song_index = self
            .songs
            .iter()
            .position(|song| stable_song_identity(song) == recent.song_identity);
        let Some(song_index) = persisted_song_index else {
            self.offline_library_status = Some(OfflineLibraryNotice::PreviousSongUnavailable);
            self.song_index = 0;
            self.course_index = 0;
            return Ok(());
        };
        let Some(filtered_index) = self
            .filtered_song_indices
            .iter()
            .position(|candidate| *candidate == song_index)
        else {
            self.offline_library_status =
                Some(OfflineLibraryNotice::PreviousSongDoesNotMatchSearch);
            self.song_index = 0;
            self.course_index = 0;
            return Ok(());
        };

        self.song_index = filtered_index;
        self.course_index = self.songs[song_index]
            .courses
            .iter()
            .position(|course| course.canonical_chart_hash == recent.course_identity)
            .unwrap_or_default();
        if self.songs[song_index]
            .courses
            .get(self.course_index)
            .is_none_or(|course| course.canonical_chart_hash != recent.course_identity)
        {
            self.offline_library_status = Some(OfflineLibraryNotice::PreviousCourseUnavailable);
        }
        self.refresh_vsync_scroll_speed()
    }

    pub(crate) fn offline_preparation_mode(&self) -> Option<OfflinePreparationMode> {
        self.offline_preparation_identity
            .map(|identity| identity.mode)
    }

    fn handle_offline_preparation_key(&mut self, key: KeyEvent) -> Result<()> {
        if !matches!(key.code, KeyCode::Esc) {
            return Ok(());
        }
        let mode = self.offline_preparation_mode();
        self.cancel_offline_preparation();
        self.page = match mode {
            Some(OfflinePreparationMode::LocalTwoPlayer) => Page::LocalCourseSelect,
            Some(OfflinePreparationMode::Single) | None => Page::CourseMenu,
        };
        self.schedule_demo();
        Ok(())
    }

    fn cancel_offline_preparation(&mut self) {
        self.offline_preparation.cancel();
        self.offline_preparation_identity = None;
    }

    fn poll_offline_preparation(&mut self) -> Result<()> {
        for event in self.offline_preparation.poll() {
            if !offline_preparation_event_is_current(self.offline_preparation_identity, &event) {
                continue;
            }
            match event.completion {
                OfflinePreparationCompletion::Completed(content) => {
                    self.activate_prepared_offline(event.identity, *content)?;
                    self.offline_preparation_identity = None;
                }
                OfflinePreparationCompletion::Cancelled => {
                    let mode = event.identity.mode;
                    self.offline_preparation_identity = None;
                    if self.page == Page::OfflinePreparation {
                        self.page = match mode {
                            OfflinePreparationMode::Single => Page::CourseMenu,
                            OfflinePreparationMode::LocalTwoPlayer => Page::LocalCourseSelect,
                        };
                    }
                }
                OfflinePreparationCompletion::Failed(error) => {
                    return Err(error).context("failed to prepare the offline match");
                }
            }
        }
        Ok(())
    }

    fn activate_prepared_offline(
        &mut self,
        identity: OfflinePreparationIdentity,
        content: PreparedOfflineContent,
    ) -> Result<()> {
        match (identity.mode, content.charts) {
            (OfflinePreparationMode::Single, PreparedOfflineCharts::Single(chart)) => {
                self.activate_prepared_single(identity, *chart, content.audio)
            }
            (OfflinePreparationMode::LocalTwoPlayer, PreparedOfflineCharts::Local(charts)) => {
                self.activate_prepared_local(identity, *charts, content.audio)
            }
            _ => bail!("offline preparation returned content for the wrong play mode"),
        }
    }

    fn activate_prepared_single(
        &mut self,
        identity: OfflinePreparationIdentity,
        chart: CanonicalChart,
        audio: Option<crate::audio::PreparedSongAudio>,
    ) -> Result<()> {
        let has_audio = audio.is_some();
        if self.selected_song_index() != Some(identity.song_index)
            || self.course_index != identity.course_indices[0]
        {
            bail!("offline preparation completed for a stale single-player selection");
        }
        let course = self
            .songs
            .get(identity.song_index)
            .and_then(|song| song.courses.get(identity.course_indices[0]))
            .cloned()
            .ok_or_else(|| anyhow!("prepared single-player course is no longer available"))?;
        let chart_end_tick = canonical_chart_end_tick(&chart);
        let autoplay_inputs = if self.auto_play {
            build_autoplay_events(&chart)?
        } else {
            Vec::new()
        };
        let mut runtime = TaikoRuntime::new(
            &chart,
            TaikoBranchPolicy::Automatic,
            course.branch_decisions.clone(),
        )
        .context("invalid taiko runtime for the selected chart")?;
        let initial_output = runtime
            .advance_to(0, &[])
            .context("failed to bootstrap game frame")?;
        let projection_span =
            crate::screen::game_screen::projection_span_for_viewport_width(self.viewport_width);
        self.scroll_speed_vsync = compute_vsync_scroll_speed(&chart, projection_span);
        self.loaded_course_chart = Some(LoadedCourseChart {
            song_index: identity.song_index,
            course_index: identity.course_indices[0],
            chart,
        });

        self.audio.stop_song()?;
        self.audio.play_prepared_song(audio, 0.0, false)?;
        self.perf_meter.clear();
        self.result = None;
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.game = Some(GameSession {
            song_index: identity.song_index,
            course_name: course.name,
            canonical_chart_hash: course.canonical_chart_hash,
            chart_end_tick,
            has_audio,
            runtime,
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
        self.page = Page::Game;
        Ok(())
    }

    fn activate_prepared_local(
        &mut self,
        identity: OfflinePreparationIdentity,
        charts: [CanonicalChart; 2],
        audio: Option<crate::audio::PreparedSongAudio>,
    ) -> Result<()> {
        let has_audio = audio.is_some();
        if self.selected_song_index() != Some(identity.song_index)
            || LocalPlayerId::ALL.map(|player| self.local_course_selection.course_index(player))
                != identity.course_indices
        {
            bail!("offline preparation completed for a stale local multiplayer selection");
        }
        let song = self
            .songs
            .get(identity.song_index)
            .ok_or_else(|| anyhow!("prepared local multiplayer song is no longer available"))?;
        let [first_chart, second_chart] = charts;
        let first_course = song
            .courses
            .get(identity.course_indices[0])
            .ok_or_else(|| anyhow!("P1 prepared course is no longer available"))?;
        let second_course = song
            .courses
            .get(identity.course_indices[1])
            .ok_or_else(|| anyhow!("P2 prepared course is no longer available"))?;
        let specs = [
            LocalPlayerSpec {
                course_name: first_course.name.clone(),
                chart: first_chart,
                branch_decisions: first_course.branch_decisions.clone(),
            },
            LocalPlayerSpec {
                course_name: second_course.name.clone(),
                chart: second_chart,
                branch_decisions: second_course.branch_decisions.clone(),
            },
        ];
        let session = LocalMultiplayerSession::new(
            identity.song_index,
            specs,
            TaikoBranchPolicy::Automatic,
            has_audio,
        )?;

        self.audio.stop_song()?;
        self.audio.play_prepared_song(audio, 0.0, false)?;
        self.perf_meter.clear();
        self.local_result = None;
        self.local_game = Some(session);
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.page = Page::LocalGame;
        Ok(())
    }

    fn abort_game_to_course(&mut self) -> Result<()> {
        self.audio.stop_song()?;
        self.game = None;
        self.leave_confirmation = None;
        self.page = Page::CourseMenu;
        self.schedule_demo();
        Ok(())
    }

    fn start_local_game(&mut self) -> Result<()> {
        self.cancel_demo_preview_load();
        self.cancel_offline_preparation();
        self.persist_recent_song(self.local_course_selection.course_index(LocalPlayerId::One))?;
        let song_index = self
            .selected_song_index()
            .ok_or_else(|| anyhow!("no selected song for local multiplayer"))?;
        let song = self
            .songs
            .get(song_index)
            .cloned()
            .ok_or_else(|| anyhow!("invalid selected song for local multiplayer"))?;
        let course_indices =
            LocalPlayerId::ALL.map(|player| self.local_course_selection.course_index(player));
        for (player, course_index) in LocalPlayerId::ALL.into_iter().zip(course_indices) {
            if song.courses.get(course_index).is_none() {
                bail!(
                    "{} selected invalid course index {course_index}",
                    player.label()
                );
            }
        }
        let generation = self
            .offline_preparation_generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("offline preparation generation exhausted"))?;
        let identity = OfflinePreparationIdentity::local(generation, song_index, course_indices);
        self.offline_preparation.start(OfflinePreparationRequest {
            identity,
            backend: Arc::clone(&self.resource_backend),
            song,
        })?;
        self.audio.stop_song()?;
        self.demo_pending = None;
        self.demo_playing_song = None;
        self.demo_preview_status = None;
        self.offline_preparation_generation = generation;
        self.offline_preparation_identity = Some(identity);
        self.page = Page::OfflinePreparation;
        self.leave_confirmation = None;
        Ok(())
    }

    fn abort_local_game_to_courses(&mut self) -> Result<()> {
        self.audio.stop_song()?;
        self.local_game = None;
        self.local_course_selection.clear_ready();
        self.leave_confirmation = None;
        self.page = Page::LocalCourseSelect;
        Ok(())
    }

    fn toggle_local_game_pause(&mut self) -> Result<()> {
        let Some(game) = self.local_game.as_mut() else {
            return Ok(());
        };
        if game.paused {
            self.audio.resume_song()?;
            game.paused = false;
        } else {
            self.audio.pause_song()?;
            game.paused = true;
            for player in &mut game.players {
                player.input_flash = None;
                player.judge_flash = None;
            }
        }
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
            self.cancel_demo_preview_load();
            return Ok(());
        }

        if self.demo_playing_song == Some(song_index) {
            return Ok(());
        }

        let Some(identity) = self
            .demo_preview_identity
            .filter(|identity| identity.song_index == song_index)
        else {
            self.demo_pending = None;
            return Ok(());
        };
        let Some(song) = self.songs.get(song_index).cloned() else {
            self.demo_preview_identity = None;
            self.demo_pending = None;
            self.demo_preview_status = Some(DemoPreviewNotice::Unavailable {
                reason: format!("invalid song index {song_index}"),
            });
            return Ok(());
        };
        let backend = Arc::clone(&self.resource_backend);
        if let Err(error) = self.demo_preview_task.start(identity, backend, song) {
            self.demo_preview_identity = None;
            self.demo_preview_status = Some(DemoPreviewNotice::Unavailable {
                reason: error.to_string(),
            });
        } else {
            self.demo_preview_status = Some(DemoPreviewNotice::Loading);
        }
        self.demo_pending = None;
        Ok(())
    }

    fn poll_demo_preview(&mut self) {
        for event in self.demo_preview_task.poll() {
            if !demo_event_is_current(self.demo_preview_identity, &event) {
                continue;
            }
            self.demo_preview_identity = None;

            match event.completion {
                DemoPreviewCompletion::Completed(prepared)
                    if self.args.demo
                        && self.selected_song_index() == Some(event.identity.song_index)
                        && self.demo_preview_page_is_active() =>
                {
                    match self.audio.play_prepared_song(
                        Some(prepared.audio),
                        prepared.start_seconds,
                        true,
                    ) {
                        Ok(()) => {
                            self.demo_playing_song = Some(event.identity.song_index);
                            self.demo_preview_status = None;
                        }
                        Err(error) => {
                            self.demo_playing_song = None;
                            self.demo_preview_status = Some(DemoPreviewNotice::Unavailable {
                                reason: error.to_string(),
                            });
                        }
                    }
                }
                DemoPreviewCompletion::Completed(_) | DemoPreviewCompletion::Cancelled => {
                    self.demo_preview_status = None;
                }
                DemoPreviewCompletion::Failed(error) => {
                    self.demo_playing_song = None;
                    self.demo_preview_status = Some(DemoPreviewNotice::Unavailable {
                        reason: error.to_string(),
                    });
                }
            }
        }
    }

    fn demo_preview_page_is_active(&self) -> bool {
        matches!(
            self.page,
            Page::SongMenu | Page::LoadWarnings | Page::CourseMenu | Page::OnlineLobby
        )
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
                .runtime
                .advance_to(now_tick, &[])
                .context("engine step failed while waiting for song end")?;
            game.last_tick = output.now;
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

        let frame_inputs = collect_due_offline_inputs(&mut game.pending_inputs, now_tick);
        let autoplay_events =
            collect_due_autoplay_events(&game.autoplay_inputs, &mut game.autoplay_cursor, now_tick);
        let mut scheduled_inputs = frame_inputs
            .iter()
            .copied()
            .map(ScheduledTaikoInput::unconditional)
            .chain(
                autoplay_events
                    .iter()
                    .copied()
                    .map(scheduled_autoplay_input),
            )
            .collect::<Vec<_>>();
        scheduled_inputs.sort_by_key(|input| input.input.tick);

        let start = Instant::now();
        let output = game
            .runtime
            .advance_to(now_tick, &scheduled_inputs)
            .context("engine step failed")?;
        self.perf_meter.record_tick(start.elapsed());

        let selected_autoplay = autoplay_events
            .iter()
            .copied()
            .filter(|event| {
                event.input.tick <= output.now
                    && game
                        .runtime
                        .input_is_enabled(scheduled_autoplay_input(*event))
            })
            .collect::<Vec<_>>();
        for event in &selected_autoplay {
            match event.input.action.zone {
                TaikoZone::Don => self.play_don_se()?,
                TaikoZone::Kat => self.play_kat_se()?,
            }
        }
        if let Some(input) = frame_inputs
            .iter()
            .copied()
            .filter(|input| input.tick <= output.now)
            .chain(selected_autoplay.iter().map(|event| event.input))
            .max_by_key(|input| input.tick)
        {
            game.input_flash = Some(InputFlashState {
                action: input.action,
                until_tick: now_tick.saturating_add(HIT_FLASH_TICKS),
            });
        }

        game.last_tick = output.now;
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

    fn tick_local_game(&mut self) -> Result<()> {
        let mut game = self
            .local_game
            .take()
            .ok_or_else(|| anyhow!("local multiplayer game state is missing"))?;
        if game.paused {
            self.local_game = Some(game);
            return Ok(());
        }

        let now_tick = self.current_chart_tick(game.last_tick);
        let start = Instant::now();
        game.advance_to(now_tick)?;
        self.perf_meter.record_tick(start.elapsed());

        if !game.all_finished() || (game.has_audio && !self.audio.is_song_finished()) {
            game.result_delay_deadline = None;
            self.local_game = Some(game);
            return Ok(());
        }

        let now = Instant::now();
        let deadline = game.result_delay_deadline.get_or_insert(now + RESULT_DELAY);
        if now < *deadline {
            self.local_game = Some(game);
            return Ok(());
        }

        self.audio.stop_song()?;
        let song = self
            .songs
            .get(game.song_index)
            .ok_or_else(|| anyhow!("invalid song index at local multiplayer result"))?;
        self.local_result = Some(LocalMultiplayerResult {
            title: song.title.clone(),
            subtitle: song.subtitle.clone(),
            players: game.results(),
        });
        self.local_game = None;
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.page = Page::LocalResult;
        Ok(())
    }

    fn finish_game_with_session(&mut self, game: GameSession) -> Result<()> {
        self.audio.stop_song()?;

        let (title, subtitle) = self
            .songs
            .get(game.song_index)
            .map(|song| (song.title.clone(), song.subtitle.clone()))
            .ok_or_else(|| anyhow!("invalid song index at result"))?;
        let final_result = game.runtime.finalize();
        let personal_best = PersonalBest {
            score: final_result.score,
            accuracy_ppm: result_accuracy_ppm(&final_result),
            cleared: final_result.passed,
            full_combo: final_result.miss == 0
                && final_result.great.saturating_add(final_result.ok) > 0,
        };
        let preferences_before_result = self.preferences.clone();
        let previous_best = self
            .preferences
            .record_personal_best(&game.canonical_chart_hash, personal_best)?;
        if let Err(error) = self.save_preferences(&self.preferences) {
            self.preferences = preferences_before_result;
            return Err(error).context("failed to save personal best");
        }

        self.result = Some(ResultState {
            title,
            subtitle,
            course_name: game.course_name,
            final_result,
            replay_hash: game.runtime.replay_hash(),
            branch_controls: game.runtime.emitted_controls(),
            timing_samples: game.timing_samples,
            perf: self.perf_meter.snapshot(),
            previous_best_score: previous_best.map(|best| best.score),
        });
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.page = Page::Result;
        self.game = None;
        Ok(())
    }

    fn finish_game_when_audio_done(&mut self, mut game: GameSession) -> Result<()> {
        if game.has_audio && !self.audio.is_song_finished() {
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
        self.current_chart_tick_at(min_tick, Instant::now())
    }

    fn current_chart_tick_at(&self, min_tick: Tick, observed_at: Instant) -> Tick {
        let song_seconds = self.audio.song_position_seconds();
        let handling_delay_seconds = Instant::now()
            .saturating_duration_since(observed_at)
            .as_secs_f64();
        chart_tick_from_audio_observation(
            song_seconds,
            handling_delay_seconds,
            self.calibration_offset_ms,
            min_tick,
        )
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

        let persistent = match self.course_setting_focus {
            CourseSettingFocus::AutoPlay => {
                self.auto_play = delta > 0;
                false
            }
            CourseSettingFocus::SongVolume => {
                let next = (i32::from(self.args.songvol) + delta).clamp(0, 100) as u8;
                self.args.songvol = next;
                self.audio.set_song_volume(next);
                true
            }
            CourseSettingFocus::SeVolume => {
                let next = (i32::from(self.args.sevol) + delta).clamp(0, 100) as u8;
                self.args.sevol = next;
                self.audio.set_se_volume(next);
                true
            }
            CourseSettingFocus::CalibrationOffset => {
                self.calibration_offset_ms = adjust_offset_ms(self.calibration_offset_ms, delta);
                true
            }
            CourseSettingFocus::ScrollSpeed => {
                self.scroll_speed_setting =
                    cycle_scroll_speed_setting(self.scroll_speed_setting, delta);
                true
            }
        };
        if persistent {
            self.persist_runtime_preferences()?;
        }
        Ok(())
    }

    fn schedule_demo(&mut self) {
        if !self.args.demo
            || self.filtered_song_indices.is_empty()
            || !self.audio_capability().is_available()
        {
            self.cancel_demo_preview_load();
            let _ = self.audio.stop_song();
            self.demo_playing_song = None;
            return;
        }

        let Some(selected_song_index) = self.selected_song_index() else {
            self.cancel_demo_preview_load();
            let _ = self.audio.stop_song();
            self.demo_playing_song = None;
            return;
        };

        // Keep current preview running when the selected song does not change,
        // e.g. Song Menu -> Course Menu transition for the same song.
        if self.demo_playing_song == Some(selected_song_index) && !self.audio.is_song_finished() {
            self.demo_pending = None;
            return;
        }

        if self
            .demo_preview_identity
            .is_some_and(|identity| identity.song_index == selected_song_index)
        {
            return;
        }

        self.cancel_demo_preview_load();
        let Some(generation) = self.demo_preview_generation.checked_add(1) else {
            self.demo_preview_status = Some(DemoPreviewNotice::Unavailable {
                reason: "request generation exhausted".to_owned(),
            });
            let _ = self.audio.stop_song();
            self.demo_playing_song = None;
            return;
        };
        self.demo_preview_generation = generation;
        self.demo_preview_identity = Some(DemoPreviewIdentity {
            generation,
            song_index: selected_song_index,
        });
        self.demo_pending = Some((Instant::now() + DEMO_DELAY, selected_song_index));
        self.demo_playing_song = None;
        self.demo_preview_status = None;
        let _ = self.audio.stop_song();
    }

    fn cancel_demo_preview_load(&mut self) {
        self.demo_preview_task.cancel();
        self.demo_preview_identity = None;
        self.demo_pending = None;
        self.demo_preview_status = None;
    }

    fn stop_demo_preview_nonfatal(&mut self) {
        self.cancel_demo_preview_load();
        self.demo_playing_song = None;
        if let Err(error) = self.audio.stop_song() {
            self.demo_preview_status = Some(DemoPreviewNotice::StopFailed {
                reason: error.to_string(),
            });
        }
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

        self.scroll_speed_vsync = self
            .selected_course_chart()
            .map(|chart| compute_vsync_scroll_speed(chart, projection_span))
            .unwrap_or(1.0);
        Ok(())
    }

    fn set_error_state(&mut self, error: anyhow::Error) {
        let retry = self
            .offline_preparation_identity
            .map(|identity| match identity.mode {
                OfflinePreparationMode::Single => ErrorRetryAction::PrepareSinglePlayer,
                OfflinePreparationMode::LocalTwoPlayer => ErrorRetryAction::PrepareLocalTwoPlayer,
            });
        let recovery = if let Some(identity) = self.offline_preparation_identity {
            match identity.mode {
                OfflinePreparationMode::Single => ErrorRecoveryTarget::CourseMenu,
                OfflinePreparationMode::LocalTwoPlayer => ErrorRecoveryTarget::LocalCourseSelect,
            }
        } else if self.page == Page::Error {
            self.error_state
                .as_ref()
                .map_or(ErrorRecoveryTarget::ModeSelect, |state| state.recovery)
        } else {
            error_recovery_target(self.page)
        };
        let summary = player_error_summary(self.page);
        let mut technical_details = format!("{error:#}");
        if let Err(cleanup_error) =
            combine_cleanup_results(self.teardown_online(), self.resume_offline_library_load())
        {
            technical_details.push_str(&format!(
                "\nAdditionally failed to clean up the online session: {cleanup_error:#}"
            ));
        }
        self.cancel_offline_preparation();
        self.page = Page::Error;
        self.error_state = Some(RecoverableErrorState {
            summary,
            technical_details,
            recovery,
            retry,
            details_visible: false,
        });
        self.result = None;
        self.game = None;
        self.local_game = None;
        self.local_result = None;
        self.local_course_selection.clear_ready();
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.invite_revealed = false;
        self.invite_copy_status = None;
        self.loaded_course_chart = None;
    }

    fn retry_from_error(&mut self) -> Result<()> {
        let Some(state) = self.error_state.take() else {
            return Ok(());
        };
        self.page = state.recovery.page();
        match state.retry {
            Some(ErrorRetryAction::PrepareSinglePlayer) => self.start_game(),
            Some(ErrorRetryAction::PrepareLocalTwoPlayer) => self.start_local_game(),
            None => {
                self.error_state = Some(state);
                self.recover_from_error();
                Ok(())
            }
        }
    }

    fn recover_from_error(&mut self) {
        let recovery = self
            .error_state
            .take()
            .map_or(ErrorRecoveryTarget::ModeSelect, |state| state.recovery);
        self.page = recovery.page();
        match recovery {
            ErrorRecoveryTarget::ModeSelect => {
                self.active_mode = None;
            }
            ErrorRecoveryTarget::SongMenu => {
                self.schedule_demo();
            }
            ErrorRecoveryTarget::CourseMenu => {
                self.active_mode = Some(GameMode::SinglePlayer);
                self.schedule_demo();
            }
            ErrorRecoveryTarget::LocalCourseSelect => {
                self.active_mode = Some(GameMode::LocalTwoPlayer);
                self.local_course_selection.clear_ready();
            }
            ErrorRecoveryTarget::MultiplayerConnect => {
                self.active_mode = Some(GameMode::OnlineMultiplayer);
                self.reset_multiplayer_connect();
            }
        }
    }

    fn return_to_mode_select(&mut self) -> Result<()> {
        let teardown_result = self.teardown_online();
        let library_result = self.resume_offline_library_load();
        combine_cleanup_results(teardown_result, library_result)?;
        self.game = None;
        self.result = None;
        self.local_game = None;
        self.local_result = None;
        self.local_course_selection.reset();
        self.result_details_visible = false;
        self.leave_confirmation = None;
        self.invite_revealed = false;
        self.invite_copy_status = None;
        self.page = Page::ModeSelect;
        self.active_mode = None;
        self.error_state = None;
        Ok(())
    }

    fn save_preferences(&self, preferences: &PlayerPreferences) -> Result<()> {
        match &self.preferences_store {
            Some(store) => store.save(preferences),
            None => Ok(()),
        }
    }

    fn persist_last_mode(&mut self, mode: GameMode) -> Result<()> {
        let mut next = self.preferences.clone();
        next.last_mode = Some(stored_game_mode(mode));
        self.save_preferences(&next)?;
        self.preferences = next;
        Ok(())
    }

    fn persist_recent_song(&mut self, course_index: usize) -> Result<()> {
        let selection = self
            .selected_song()
            .and_then(|song| recent_song_selection(song, course_index, &self.song_query))
            .ok_or_else(|| anyhow!("the selected song or course is no longer available"))?;
        let mut next = self.preferences.clone();
        next.recent_song = Some(selection);
        self.save_preferences(&next)?;
        self.preferences = next;
        Ok(())
    }

    fn apply_preferences(&mut self, preferences: PlayerPreferences) -> Result<()> {
        preferences.validate()?;
        self.args.songvol = preferences.song_volume;
        self.args.sevol = preferences.se_volume;
        self.args.calibration_offset_ms = preferences.calibration_offset_ms;
        self.args.demo = preferences.demo_enabled;
        self.calibration_offset_ms = preferences.calibration_offset_ms;
        self.scroll_speed_setting = stored_scroll_speed_to_runtime(preferences.scroll_speed);
        self.mp_connect.name = preferences.player_name.clone();
        self.audio.set_song_volume(preferences.song_volume);
        self.audio.set_se_volume(preferences.se_volume);
        self.mode_selection = preferences
            .last_mode
            .map(stored_mode_index)
            .unwrap_or_default();
        self.preferences = preferences.clone();
        self.settings = SettingsState::new(preferences);
        self.refresh_vsync_scroll_speed()
    }

    fn persist_runtime_preferences(&mut self) -> Result<()> {
        self.preferences.song_volume = self.args.songvol;
        self.preferences.se_volume = self.args.sevol;
        self.preferences.calibration_offset_ms = self.calibration_offset_ms;
        self.preferences.scroll_speed = runtime_scroll_speed_to_stored(self.scroll_speed_setting);
        self.preferences.demo_enabled = self.args.demo;
        self.preferences.player_name = self.mp_connect.name.clone();
        self.save_preferences(&self.preferences)
    }

    fn play_don_se(&mut self) -> Result<()> {
        self.play_taiko_se(TaikoZone::Don);
        Ok(())
    }

    fn play_kat_se(&mut self) -> Result<()> {
        self.play_taiko_se(TaikoZone::Kat);
        Ok(())
    }

    fn play_taiko_se(&mut self, zone: TaikoZone) {
        if self.audio_notice.is_some() {
            return;
        }

        let result = match zone {
            TaikoZone::Don => self.audio.play_don(),
            TaikoZone::Kat => self.audio.play_kat(),
        };
        if let Err(error) = result {
            self.audio_notice = Some(AudioNotice::SoundEffectsDisabled {
                reason: Arc::<str>::from(format!("{error:#}")),
            });
        }
    }
}

fn online_placeholder_resources(args: &CliArgs) -> (ResourceBackend, SongLibrary) {
    (
        ResourceBackend::local(args.songdir.clone()),
        SongLibrary {
            songs: Vec::new(),
            warnings: Vec::new(),
        },
    )
}

fn library_status_for_contents(
    songs: &[SongEntry],
    warnings: &[String],
    using_remote_resources: bool,
) -> Option<OfflineLibraryNotice> {
    if !songs.is_empty() {
        return None;
    }
    Some(OfflineLibraryNotice::NoPlayableCharts(
        if let Some(first_warning) = warnings.first() {
            OfflineLibraryEmptyReason::ImportWarning(first_warning.clone())
        } else if using_remote_resources {
            OfflineLibraryEmptyReason::ResourceEndpoint
        } else {
            OfflineLibraryEmptyReason::LocalDirectory
        },
    ))
}

fn stable_song_identity(song: &SongEntry) -> String {
    let mut digest = Sha256::new();
    digest.update(b"taiko-game/song-identity/v1\0");
    for course in &song.courses {
        digest.update((course.canonical_chart_hash.len() as u64).to_le_bytes());
        digest.update(course.canonical_chart_hash.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn stored_mode_index(mode: StoredGameMode) -> usize {
    match mode {
        StoredGameMode::SinglePlayer => 0,
        StoredGameMode::LocalTwoPlayer => 1,
        StoredGameMode::OnlineMultiplayer => 2,
    }
}

fn stored_game_mode(mode: GameMode) -> StoredGameMode {
    match mode {
        GameMode::SinglePlayer => StoredGameMode::SinglePlayer,
        GameMode::LocalTwoPlayer => StoredGameMode::LocalTwoPlayer,
        GameMode::OnlineMultiplayer => StoredGameMode::OnlineMultiplayer,
    }
}

fn recent_song_selection(
    song: &SongEntry,
    course_index: usize,
    query: &str,
) -> Option<RecentSongSelection> {
    let course = song.courses.get(course_index)?;
    Some(RecentSongSelection {
        song_identity: stable_song_identity(song),
        query: query.to_owned(),
        course_identity: course.canonical_chart_hash.clone(),
    })
}

fn preferences_from_cli(args: &CliArgs) -> PlayerPreferences {
    let mut preferences = PlayerPreferences::default();
    preferences.song_volume = args.songvol;
    preferences.se_volume = args.sevol;
    preferences.calibration_offset_ms = args.calibration_offset_ms;
    preferences.demo_enabled = args.demo;
    preferences
}

fn canonical_chart_end_tick(chart: &CanonicalChart) -> Tick {
    chart
        .objects
        .iter()
        .map(|object| object.end_tick.max(object.start_tick))
        .chain(chart.events.iter().map(|event| event.tick))
        .max()
        .unwrap_or_default()
}

fn result_accuracy_ppm(result: &TaikoFinalResult) -> u32 {
    let total = u64::from(
        result
            .great
            .saturating_add(result.ok)
            .saturating_add(result.miss),
    );
    if total == 0 {
        return 0;
    }
    let weighted = u64::from(result.great)
        .saturating_mul(1_000_000)
        .saturating_add(u64::from(result.ok).saturating_mul(500_000));
    u32::try_from(weighted / total).unwrap_or(1_000_000)
}

fn stored_scroll_speed_to_runtime(scroll_speed: StoredScrollSpeed) -> ScrollSpeedSetting {
    match scroll_speed {
        StoredScrollSpeed::Manual(speed) => ScrollSpeedSetting::Manual(speed),
        StoredScrollSpeed::VelocitySync => ScrollSpeedSetting::VSync,
    }
}

fn runtime_scroll_speed_to_stored(scroll_speed: ScrollSpeedSetting) -> StoredScrollSpeed {
    match scroll_speed {
        ScrollSpeedSetting::Manual(speed) => StoredScrollSpeed::Manual(speed),
        ScrollSpeedSetting::VSync => StoredScrollSpeed::VelocitySync,
    }
}

fn binding_for_key(preferences: &PlayerPreferences, key: KeyEvent) -> Option<(usize, BindingSlot)> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    [preferences.player_one, preferences.player_two]
        .into_iter()
        .enumerate()
        .find_map(|(player_index, bindings)| {
            bindings
                .slot_for_key(character)
                .map(|slot| (player_index, slot))
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingCandidateIssue {
    VisibleAsciiRequired,
    PauseKeyReserved,
    AlreadyAssigned,
}

fn binding_candidate_issue(
    preferences: &PlayerPreferences,
    player_index: usize,
    slot: BindingSlot,
    key: char,
) -> Option<BindingCandidateIssue> {
    if !key.is_ascii() || key.is_ascii_control() || key.is_ascii_whitespace() {
        return Some(BindingCandidateIssue::VisibleAsciiRequired);
    }
    if key.eq_ignore_ascii_case(&'p') {
        return Some(BindingCandidateIssue::PauseKeyReserved);
    }

    let normalized = key.to_ascii_lowercase();
    let current = match player_index {
        0 => preferences.player_one.key(slot),
        1 => preferences.player_two.key(slot),
        _ => return Some(BindingCandidateIssue::AlreadyAssigned),
    };
    if current.to_ascii_lowercase() == normalized {
        return None;
    }
    [preferences.player_one, preferences.player_two]
        .into_iter()
        .enumerate()
        .flat_map(|(candidate_player, bindings)| {
            BindingSlot::ALL
                .into_iter()
                .map(move |candidate_slot| (candidate_player, candidate_slot, bindings))
        })
        .any(|(candidate_player, candidate_slot, bindings)| {
            (candidate_player, candidate_slot) != (player_index, slot)
                && bindings.key(candidate_slot).to_ascii_lowercase() == normalized
        })
        .then_some(BindingCandidateIssue::AlreadyAssigned)
}

fn push_bounded_utf8(target: &mut String, character: char, max_bytes: usize) -> bool {
    if target.len().saturating_add(character.len_utf8()) > max_bytes {
        return false;
    }
    target.push(character);
    true
}

fn error_recovery_target(page: Page) -> ErrorRecoveryTarget {
    match page {
        Page::ModeSelect | Page::Controllers | Page::Settings | Page::Error => {
            ErrorRecoveryTarget::ModeSelect
        }
        Page::SongMenu | Page::LoadWarnings => ErrorRecoveryTarget::SongMenu,
        Page::CourseMenu | Page::OfflinePreparation | Page::Game | Page::Result => {
            ErrorRecoveryTarget::CourseMenu
        }
        Page::LocalCourseSelect | Page::LocalGame | Page::LocalResult => {
            ErrorRecoveryTarget::LocalCourseSelect
        }
        Page::MultiplayerConnect
        | Page::OnlineLobby
        | Page::OnlineCourseSelect
        | Page::OnlineMatch
        | Page::OnlineResult => ErrorRecoveryTarget::MultiplayerConnect,
    }
}

fn player_error_summary(page: Page) -> UiText {
    match page {
        Page::OfflinePreparation => UiText::SelectedMatchCouldNotBePrepared,
        Page::Game | Page::LocalGame => UiText::GameplayStoppedRequiredService,
        Page::MultiplayerConnect
        | Page::OnlineLobby
        | Page::OnlineCourseSelect
        | Page::OnlineMatch
        | Page::OnlineResult => UiText::OnlineSessionInterrupted,
        Page::Settings => UiText::PlayerSettingsCouldNotBeApplied,
        Page::ModeSelect
        | Page::Controllers
        | Page::SongMenu
        | Page::LoadWarnings
        | Page::CourseMenu
        | Page::Result
        | Page::LocalCourseSelect
        | Page::LocalResult
        | Page::Error => UiText::TaikoCouldNotContinue,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaveConfirmationAction {
    None,
    Open,
    Confirm,
    Cancel,
}

fn leave_confirmation_action(
    active: Option<LeaveTarget>,
    target: LeaveTarget,
    key: KeyEvent,
) -> LeaveConfirmationAction {
    if matches!(key.code, KeyCode::Esc) {
        if active == Some(target) {
            LeaveConfirmationAction::Confirm
        } else {
            LeaveConfirmationAction::Open
        }
    } else if active == Some(target) {
        LeaveConfirmationAction::Cancel
    } else {
        LeaveConfirmationAction::None
    }
}

fn combine_cleanup_results(first: Result<()>, second: Result<()>) -> Result<()> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(anyhow!("{first}; {second}")),
    }
}

fn wrapped_selection(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    current
        .checked_add_signed(delta)
        .map_or_else(|| len - 1, |next| next % len)
}

fn controller_item_slot(item: ControllerSetupItem) -> Option<ControllerSlot> {
    match item {
        ControllerSetupItem::PlayerOne => Some(ControllerSlot::One),
        ControllerSetupItem::PlayerTwo => Some(ControllerSlot::Two),
        ControllerSetupItem::BindAddress
        | ControllerSetupItem::LanServer
        | ControllerSetupItem::TerminalPointer
        | ControllerSetupItem::Back => None,
    }
}

fn online_result_control_allowed(
    is_leader: bool,
    controls_enabled: bool,
    intent: MenuIntent,
) -> bool {
    matches!(intent, MenuIntent::Quit)
        || (controls_enabled
            && is_leader
            && matches!(intent, MenuIntent::Confirm | MenuIntent::Back))
}

fn online_page_after_phase(
    page: Page,
    phase: crate::online_session::OnlinePhase,
    is_player: bool,
) -> Page {
    use crate::online_session::OnlinePhase;

    match phase {
        OnlinePhase::Lobby | OnlinePhase::Spectating => Page::OnlineLobby,
        OnlinePhase::SelectingCourse
        | OnlinePhase::Downloading
        | OnlinePhase::Verifying
        | OnlinePhase::Loading
        | OnlinePhase::Prepared
        | OnlinePhase::Ready
            if is_player
                && matches!(
                    page,
                    Page::OnlineLobby | Page::OnlineMatch | Page::OnlineResult
                ) =>
        {
            Page::OnlineCourseSelect
        }
        OnlinePhase::Countdown | OnlinePhase::Playing | OnlinePhase::Finalizing
            if matches!(
                page,
                Page::OnlineLobby | Page::OnlineCourseSelect | Page::OnlineResult
            ) =>
        {
            Page::OnlineMatch
        }
        OnlinePhase::Results
            if matches!(
                page,
                Page::OnlineLobby | Page::OnlineCourseSelect | Page::OnlineMatch
            ) =>
        {
            Page::OnlineResult
        }
        OnlinePhase::Connecting
        | OnlinePhase::Joining
        | OnlinePhase::SelectingCourse
        | OnlinePhase::Downloading
        | OnlinePhase::Verifying
        | OnlinePhase::Loading
        | OnlinePhase::Prepared
        | OnlinePhase::Ready
        | OnlinePhase::Countdown
        | OnlinePhase::Playing
        | OnlinePhase::Finalizing
        | OnlinePhase::Results
        | OnlinePhase::Reconnecting
        | OnlinePhase::Failed => page,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OnlineCourseConfirmAction {
    SelectCourse,
    StartMatch,
    None,
}

fn online_course_confirm_action(
    highlighted: PlayerSelection,
    authoritative: Option<PlayerSelection>,
    can_start_match: bool,
    preparation_failed: bool,
) -> OnlineCourseConfirmAction {
    if preparation_failed || authoritative != Some(highlighted) {
        OnlineCourseConfirmAction::SelectCourse
    } else if can_start_match {
        OnlineCourseConfirmAction::StartMatch
    } else {
        OnlineCourseConfirmAction::None
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

fn chart_tick_from_audio_observation(
    current_audio_seconds: f64,
    handling_delay_seconds: f64,
    calibration_offset_ms: i32,
    min_tick: Tick,
) -> Tick {
    let calibration_seconds = f64::from(calibration_offset_ms) / 1_000.0;
    let observed_chart_seconds =
        (current_audio_seconds - handling_delay_seconds - calibration_seconds).max(0.0);
    ticks_from_seconds(observed_chart_seconds).max(min_tick)
}

fn apply_calibration_to_tick(tick: Tick, calibration_offset_ms: i32) -> Tick {
    let calibration_tick = ticks_from_seconds(f64::from(calibration_offset_ms) / 1_000.0);
    tick.saturating_sub(calibration_tick)
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

fn build_interval_histogram(chart: &CanonicalChart) -> Vec<(Tick, u32)> {
    let interval_capacity = chart
        .objects
        .len()
        .saturating_mul(2)
        .saturating_add(chart.tempo_map.len())
        .saturating_add(chart.signatures.len());
    let mut intervals = Vec::<Tick>::with_capacity(interval_capacity);
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
            collect_interval(&mut intervals, next_tick.saturating_sub(prev));
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
        collect_interval(
            &mut intervals,
            object.end_tick.saturating_sub(object.start_tick),
        );
    }

    intervals.sort_unstable();
    let mut histogram = Vec::<(Tick, u32)>::new();
    for interval in intervals {
        if let Some((last_interval, count)) = histogram.last_mut() {
            if *last_interval == interval {
                *count = count.saturating_add(1);
                continue;
            }
        }
        histogram.push((interval, 1));
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

fn collect_interval(intervals: &mut Vec<Tick>, delta: Tick) {
    if delta > 0 {
        intervals.push(delta);
    }
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

fn collect_due_autoplay_events(
    inputs: &[AutoplayInputEvent],
    cursor: &mut usize,
    now_tick: Tick,
) -> Vec<AutoplayInputEvent> {
    let start = *cursor;
    while *cursor < inputs.len() && inputs[*cursor].input.tick <= now_tick {
        *cursor += 1;
    }
    inputs[start..*cursor].to_vec()
}

fn scheduled_autoplay_input(event: AutoplayInputEvent) -> ScheduledTaikoInput {
    event.branch_segment_id.map_or_else(
        || ScheduledTaikoInput::unconditional(event.input),
        |segment_id| ScheduledTaikoInput::for_route(event.input, segment_id, event.branch_route_id),
    )
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

fn official_online_selection(course_id: taiko_multiplayer_protocol::CourseId) -> PlayerSelection {
    PlayerSelection { course_id }
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

fn build_autoplay_events(chart: &rhythm_chart::CanonicalChart) -> Result<Vec<AutoplayInputEvent>> {
    let mut inputs = Vec::new();
    inputs
        .try_reserve(chart.objects.len().min(MAX_AUTOPLAY_EVENTS))
        .context("failed to reserve autoplay input events")?;

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
                    TaikoAction::LEFT_KAT
                } else {
                    TaikoAction::LEFT_DON
                };
                push_autoplay_event(
                    &mut inputs,
                    AutoplayInputEvent {
                        input: TimedInput {
                            tick: object.start_tick,
                            action,
                        },
                        branch_segment_id: object.branch_segment_id,
                        branch_route_id: object.branch_route_id,
                    },
                )?;
            }
            ObjectKind::Roll | ObjectKind::Hold => {
                let mut tick = object.start_tick;
                let max_hits = usize::from(object.required_hits);
                let mut emitted_hits = 0_usize;

                while tick <= object.end_tick && (max_hits == 0 || emitted_hits < max_hits) {
                    push_autoplay_event(
                        &mut inputs,
                        AutoplayInputEvent {
                            input: TimedInput {
                                tick,
                                action: TaikoAction::LEFT_DON,
                            },
                            branch_segment_id: object.branch_segment_id,
                            branch_route_id: object.branch_route_id,
                        },
                    )?;
                    emitted_hits = emitted_hits.saturating_add(1);
                    let Some(next_tick) = tick.checked_add(AUTOPLAY_ROLL_INTERVAL_TICKS) else {
                        break;
                    };
                    tick = next_tick;
                }
            }
            ObjectKind::Slide | ObjectKind::Touch => {}
        }
    }

    inputs.sort_by_key(|event| event.input.tick);
    Ok(inputs)
}

fn push_autoplay_event(
    inputs: &mut Vec<AutoplayInputEvent>,
    event: AutoplayInputEvent,
) -> Result<()> {
    if inputs.len() >= MAX_AUTOPLAY_EVENTS {
        bail!("autoplay expansion exceeds the supported maximum of {MAX_AUTOPLAY_EVENTS} inputs");
    }
    inputs.push(event);
    Ok(())
}

#[cfg(test)]
pub(crate) fn build_autoplay_inputs(
    chart: &rhythm_chart::CanonicalChart,
) -> Result<Vec<TimedInput<TaikoAction>>> {
    Ok(build_autoplay_events(chart)?
        .into_iter()
        .map(|event| event.input)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use rhythm_chart::{
        CanonicalChart, ChartMetadata, LaneOrRegion, Object, ObjectKind, TempoChange,
        TimeSignatureChange,
    };
    use rhythm_core::BasicEngine;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::audio::{AudioCapability, AudioNotice, GameAudio, PreparedSongAudio};

    static NEXT_UI_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

    struct TestGameAudio {
        capability: AudioCapability,
        position_seconds: f64,
        finished: bool,
        paused: bool,
        se_failure: Option<Arc<str>>,
        se_attempts: Option<Arc<AtomicU64>>,
        stop_attempts: Option<Arc<AtomicU64>>,
    }

    impl Default for TestGameAudio {
        fn default() -> Self {
            Self {
                capability: AudioCapability::Available,
                position_seconds: 0.0,
                finished: false,
                paused: false,
                se_failure: None,
                se_attempts: None,
                stop_attempts: None,
            }
        }
    }

    impl GameAudio for TestGameAudio {
        fn capability(&self) -> AudioCapability {
            self.capability.clone()
        }

        fn play_prepared_song(
            &mut self,
            _prepared: Option<PreparedSongAudio>,
            start_seconds: f64,
            _looping: bool,
        ) -> Result<()> {
            self.position_seconds = start_seconds;
            self.finished = false;
            self.paused = false;
            Ok(())
        }

        fn play_prepared_song_scheduled(
            &mut self,
            prepared: Option<PreparedSongAudio>,
            start_seconds: f64,
            looping: bool,
            _delay: Duration,
        ) -> Result<()> {
            self.play_prepared_song(prepared, start_seconds, looping)
        }

        fn stop_song(&mut self) -> Result<()> {
            if let Some(attempts) = &self.stop_attempts {
                attempts.fetch_add(1, Ordering::Relaxed);
            }
            self.finished = true;
            self.paused = false;
            Ok(())
        }

        fn pause_song(&mut self) -> Result<()> {
            self.paused = true;
            Ok(())
        }

        fn resume_song(&mut self) -> Result<()> {
            self.paused = false;
            Ok(())
        }

        fn seek_song(&mut self, seconds: f64) -> Result<()> {
            self.position_seconds = seconds;
            Ok(())
        }

        fn set_song_playback_rate(&mut self, _rate: f64) -> Result<()> {
            Ok(())
        }

        fn song_position_seconds(&self) -> f64 {
            self.position_seconds
        }

        fn is_song_finished(&self) -> bool {
            self.finished
        }

        fn set_song_volume(&mut self, _volume: u8) {}

        fn set_se_volume(&mut self, _volume: u8) {}

        fn play_don(&mut self) -> Result<()> {
            self.play_se()
        }

        fn play_kat(&mut self) -> Result<()> {
            self.play_se()
        }
    }

    impl TestGameAudio {
        fn play_se(&self) -> Result<()> {
            if let Some(attempts) = &self.se_attempts {
                attempts.fetch_add(1, Ordering::Relaxed);
            }
            match &self.se_failure {
                Some(reason) => bail!("{reason}"),
                None => Ok(()),
            }
        }
    }

    struct LocalUiFixture {
        path: std::path::PathBuf,
    }

    impl LocalUiFixture {
        fn create() -> Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "taiko-local-ui-{}-{}",
                std::process::id(),
                NEXT_UI_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            std::fs::write(path.join("don.wav"), include_bytes!("../assets/don.wav"))?;
            std::fs::write(
                path.join("local.tja"),
                concat!(
                    "TITLE:Local UI Test\n",
                    "SUBTITLE:Two Players\n",
                    "BPM:120\n",
                    "WAVE:don.wav\n",
                    "COURSE:Easy\n",
                    "LEVEL:1\n",
                    "#START\n",
                    "0010,\n",
                    "#END\n",
                    "COURSE:Oni\n",
                    "LEVEL:1\n",
                    "#START\n",
                    "0020,\n",
                    "#END\n",
                ),
            )?;
            Ok(Self { path })
        }
    }

    impl Drop for LocalUiFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn test_args(songdir: std::path::PathBuf) -> CliArgs {
        CliArgs {
            songdir,
            resource_endpoint: None,
            resource_cache_memory_only: true,
            tps: 120,
            calibration_offset_ms: 0,
            demo: false,
            songvol: 0,
            sevol: 0,
        }
    }

    fn test_app(songdir: std::path::PathBuf, library: SongLibrary) -> App {
        App::with_resources_and_audio(
            test_args(songdir.clone()),
            ResourceBackend::local(songdir),
            library,
            Box::<TestGameAudio>::default(),
        )
        .expect("build test app")
    }

    fn test_online_player_runtime() -> Result<crate::online::LocalPlayerRuntime> {
        let imported = rhythm_importer_tja::TjaImporter.import_song(
            concat!(
                "TITLE:Online playback lifecycle\n",
                "BPM:120\n",
                "COURSE:Oni\n",
                "LEVEL:1\n",
                "#START\n",
                "0,\n",
                "#END\n",
            )
            .as_bytes(),
        )?;
        let course = imported
            .courses
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("online playback fixture has no course"))?;
        let mut gameplay = TaikoRuntime::new(
            &course.chart,
            TaikoBranchPolicy::Automatic,
            course.branch_decisions,
        )?;
        let initial = gameplay.advance_to(0, &[])?;
        Ok(crate::online::LocalPlayerRuntime {
            match_id: taiko_multiplayer_protocol::MatchId(7),
            gameplay,
            pending_inputs: Vec::new(),
            last_tick: 0,
            last_output: initial,
            music_started: true,
            audio_sync: Some(AudioSyncController::started(Instant::now())),
            judge_flash: None,
            input_flash: None,
        })
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render_text(app: &mut App) -> String {
        render_text_at(app, 120, 36)
    }

    fn render_text_at(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        terminal
            .draw(|frame| app.render(frame))
            .expect("render app");
        app.commit_rendered_pointer_surface();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    fn render_styles_at(app: &mut App, width: u16, height: u16) -> Vec<ratatui::style::Style> {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("create test terminal");
        terminal
            .draw(|frame| app.render(frame))
            .expect("render app");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::style)
            .collect()
    }

    fn rendered_text_contains(text: &str, expected: &str) -> bool {
        let compact_text = text.replace(' ', "");
        let compact_expected = expected.replace(' ', "");
        compact_text.contains(&compact_expected)
    }

    fn wait_for_page(app: &mut App, expected: Page) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.handle_tick();
            if app.page == expected {
                return Ok(());
            }
            if app.page == Page::Error {
                let details = app
                    .error_state
                    .as_ref()
                    .map(|state| state.technical_details.as_str())
                    .unwrap_or("missing error details");
                bail!("reached error page while waiting for {expected:?}: {details}");
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for {expected:?}; current page is {:?}",
                    app.page
                );
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn due_input_collection_keeps_future_inputs() {
        let mut pending = vec![
            TimedInput {
                tick: 10,
                action: TaikoAction::LEFT_DON,
            },
            TimedInput {
                tick: 20,
                action: TaikoAction::LEFT_KAT,
            },
        ];

        let due = collect_due_offline_inputs(&mut pending, 10);
        assert_eq!(due.len(), 1);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn online_result_controls_are_leader_only_except_disconnect() {
        assert!(online_result_control_allowed(
            true,
            true,
            MenuIntent::Confirm
        ));
        assert!(online_result_control_allowed(true, true, MenuIntent::Back));
        assert!(online_result_control_allowed(true, true, MenuIntent::Quit));
        assert!(!online_result_control_allowed(
            false,
            true,
            MenuIntent::Confirm
        ));
        assert!(!online_result_control_allowed(
            false,
            true,
            MenuIntent::Back
        ));
        assert!(online_result_control_allowed(false, true, MenuIntent::Quit));
        assert!(!online_result_control_allowed(
            true,
            false,
            MenuIntent::Confirm
        ));
        assert!(!online_result_control_allowed(
            true,
            false,
            MenuIntent::Back
        ));
        assert!(online_result_control_allowed(true, false, MenuIntent::Quit));
    }

    #[test]
    fn online_phase_navigation_supports_rematch_and_late_spectators() {
        assert_eq!(
            online_page_after_phase(
                Page::OnlineResult,
                crate::online_session::OnlinePhase::SelectingCourse,
                true,
            ),
            Page::OnlineCourseSelect,
            "a rematch must leave the previous result page"
        );
        assert_eq!(
            online_page_after_phase(
                Page::OnlineLobby,
                crate::online_session::OnlinePhase::Results,
                false,
            ),
            Page::OnlineResult,
            "a spectator joining a finished match must see its result"
        );
    }

    #[test]
    fn invalidated_online_playback_stops_audio_and_rearms_the_runtime() -> Result<()> {
        let missing = std::env::temp_dir().join(format!(
            "taiko-online-playback-reset-{}-{}",
            std::process::id(),
            NEXT_UI_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let stop_attempts = Arc::new(AtomicU64::new(0));
        let audio = TestGameAudio {
            stop_attempts: Some(Arc::clone(&stop_attempts)),
            ..TestGameAudio::default()
        };
        let mut app = App::with_resources_and_audio(
            test_args(missing.clone()),
            ResourceBackend::local(missing),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
            Box::new(audio),
        )?;

        let config = crate::online::OnlineClientConfig::create("http://127.0.0.1:4150", "alice")?;
        let (network, _peer) = crate::online::NetworkClient::test_pair();
        let mut online = crate::online_session::OnlineDomain::with_test_network(config, network);
        online.local_player = Some(test_online_player_runtime()?);
        online
            .pending_actions
            .push(crate::online_session::DomainAction::PlaybackInvalidated(
                crate::online_session::OnlinePlaybackInvalidation::CountdownAborted,
            ));
        app.online = Some(online);
        let stops_before_invalidation = stop_attempts.load(Ordering::Relaxed);

        app.process_online_actions()?;

        assert!(
            stop_attempts.load(Ordering::Relaxed) > stops_before_invalidation,
            "playback invalidation must issue an audio stop"
        );
        let runtime = app
            .online
            .as_ref()
            .and_then(|online| online.local_player.as_ref())
            .expect("same-epoch preparation retains its compiled runtime");
        assert!(
            !runtime.music_started,
            "the next authoritative countdown must enter the scheduling gate again"
        );
        assert!(runtime.audio_sync.is_none());
        Ok(())
    }

    #[test]
    fn online_course_confirm_updates_changed_selection_before_starting() {
        let authoritative = official_online_selection(taiko_multiplayer_protocol::CourseId(1));
        let highlighted = official_online_selection(taiko_multiplayer_protocol::CourseId(2));

        assert_eq!(
            online_course_confirm_action(highlighted, Some(authoritative), true, false),
            OnlineCourseConfirmAction::SelectCourse,
        );
        assert_eq!(
            online_course_confirm_action(highlighted, Some(highlighted), true, false),
            OnlineCourseConfirmAction::StartMatch,
        );
        assert_eq!(
            online_course_confirm_action(highlighted, Some(highlighted), false, false),
            OnlineCourseConfirmAction::None,
        );
        assert_eq!(
            online_course_confirm_action(highlighted, Some(highlighted), true, true),
            OnlineCourseConfirmAction::SelectCourse,
            "retrying failed preparation takes precedence over starting"
        );
    }

    #[test]
    fn official_online_selection_contains_only_the_course_identity() {
        assert_eq!(
            official_online_selection(taiko_multiplayer_protocol::CourseId(7)),
            PlayerSelection {
                course_id: taiko_multiplayer_protocol::CourseId(7),
            }
        );
    }

    #[test]
    fn cleanup_error_combines_shutdown_and_audio_failures() {
        let error = combine_cleanup_results(
            Err(anyhow!("shutdown failed")),
            Err(anyhow!("audio stop failed")),
        )
        .expect_err("cleanup should retain both failures");
        let message = error.to_string();
        assert!(message.contains("shutdown failed"));
        assert!(message.contains("audio stop failed"));
    }

    #[test]
    fn top_level_mode_selection_wraps_without_hidden_cli_state() {
        assert_eq!(GameMode::ALL.len(), 3);
        assert_eq!(GameMode::ALL[0], GameMode::SinglePlayer);
        assert_eq!(GameMode::ALL[1], GameMode::LocalTwoPlayer);
        assert_eq!(GameMode::ALL[2], GameMode::OnlineMultiplayer);
        assert_eq!(wrapped_selection(0, GameMode::ALL.len(), -1), 2);
        assert_eq!(wrapped_selection(2, GameMode::ALL.len(), 1), 0);
    }

    #[test]
    fn controller_setup_is_reachable_in_game_and_assigns_the_terminal_pointer() {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );

        app.handle_key(key(KeyCode::Char('c')));
        assert_eq!(app.page, Page::Controllers);
        let rendered = render_text(&mut app);
        assert!(rendered.contains("Controller Setup"));
        assert!(rendered.contains("Trusted LAN only"));
        assert!(rendered.contains("Terminal pointer"));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(
            app.controller_setup.selected_item(),
            ControllerSetupItem::TerminalPointer
        );
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.controller_setup.pointer_slot, Some(ControllerSlot::One));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.controller_setup.pointer_slot, Some(ControllerSlot::Two));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.controller_setup.pointer_slot, None);
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.page, Page::ModeSelect);
    }

    #[test]
    fn controller_setup_is_responsive_at_minimum_width_in_all_languages() {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        app.page = Page::Controllers;
        app.controller_setup.bind_ip = "2001:db8:1234:5678:9abc:def0:1234:5678".to_owned();
        app.controller_setup.pointer_slot = Some(ControllerSlot::One);
        app.controller_setup.notice = Some(("VISIBLE-CONTROLLER-NOTICE".to_owned(), false));

        for language in UiLanguage::ALL {
            app.preferences.ui_language = language;
            let rendered = render_text_at(&mut app, 80, 24);
            assert!(!rendered.contains(app.text(UiText::TerminalTooSmall)));
            assert!(rendered.contains("2001:db8:1234:5678:9abc:def0:1234:5678"));
            assert!(rendered.contains("VISIBLE-CONTROLLER-NOTICE"));
        }
    }

    #[test]
    fn loopback_controller_bind_is_explicitly_local_only_in_every_language() {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        app.page = Page::Controllers;
        app.controller_setup.bind_ip = std::net::Ipv4Addr::LOCALHOST.to_string();

        for (language, visible_prefix) in UiLanguage::ALL.into_iter().zip([
            "127.0.0.1 is local-only",
            "127.0.0.1 只能在本機使用",
            "127.0.0.1 はこの端末専用",
        ]) {
            app.preferences.ui_language = language;
            let rendered = render_text_at(&mut app, 80, 24);
            assert!(
                rendered_text_contains(&rendered, visible_prefix),
                "missing loopback-only warning for {language:?}:\n{rendered}"
            );
        }
    }

    #[test]
    fn controller_setup_manages_real_pairing_without_exposing_hidden_tokens() -> Result<()> {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        app.controller_setup.bind_ip = std::net::Ipv4Addr::LOCALHOST.to_string();
        app.handle_key(key(KeyCode::Char('c')));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(
            app.controller_setup.selected_item(),
            ControllerSetupItem::LanServer
        );
        app.handle_key(key(KeyCode::Enter));
        assert!(app.controller_server_running());
        let first_p1 = app
            .controller_pairing_invite(ControllerSlot::One)
            .context("P1 pairing invite")?;
        let p2 = app
            .controller_pairing_invite(ControllerSlot::Two)
            .context("P2 pairing invite")?;
        assert_ne!(first_p1.expose(), p2.expose());
        let first_p1_token = first_p1
            .expose()
            .split_once("#token=")
            .expect("P1 pairing URL fragment")
            .1
            .to_owned();
        let p2_token = p2
            .expose()
            .split_once("#token=")
            .expect("P2 pairing URL fragment")
            .1
            .to_owned();
        let token_is_visible =
            |rendered: &str, token: &str| rendered.replace(' ', "").contains(token);
        let hidden = render_text(&mut app);
        assert!(!token_is_visible(&hidden, &first_p1_token));
        assert!(!token_is_visible(&hidden, &p2_token));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(
            app.controller_setup.selected_item(),
            ControllerSetupItem::PlayerOne
        );
        let still_hidden = render_text(&mut app);
        assert!(still_hidden.contains("[hidden"));
        assert!(!token_is_visible(&still_hidden, &first_p1_token));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.controller_setup.invite_revealed[ControllerSlot::One.index()]);
        assert!(app.controller_pairing_invite(ControllerSlot::One).is_some());
        let revealed = render_text_at(&mut app, 180, 50);
        assert!(rendered_text_contains(
            &revealed,
            app.text(UiText::ControllerPairingQr)
        ));
        assert!(!token_is_visible(&revealed, &first_p1_token));
        let qr_styles = render_styles_at(&mut app, 180, 50);
        assert!(
            qr_styles
                .iter()
                .filter(|style| style.bg == Some(ratatui::style::Color::White))
                .count()
                > 100
        );
        assert!(qr_styles.iter().any(|style| {
            style.fg == Some(ratatui::style::Color::Black)
                && style.bg == Some(ratatui::style::Color::White)
        }));

        app.handle_key(key(KeyCode::Char('r')));
        let replacement = app
            .controller_pairing_invite(ControllerSlot::One)
            .context("rotated P1 pairing invite")?;
        assert_ne!(replacement.expose(), first_p1.expose());
        let replacement_token = replacement
            .expose()
            .split_once("#token=")
            .expect("rotated pairing URL fragment")
            .1
            .to_owned();
        let rotated_hidden = render_text(&mut app);
        assert!(!token_is_visible(&rotated_hidden, &replacement_token));
        assert!(!token_is_visible(&rotated_hidden, &first_p1_token));

        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Enter));
        assert!(!app.controller_server_running());
        Ok(())
    }

    #[test]
    fn terminal_pointer_surface_preserves_all_four_single_player_actions() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.controller_setup.pointer_slot = Some(ControllerSlot::One);

        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        wait_for_page(&mut app, Page::Game)?;
        let _ = render_text(&mut app);
        let surface = app.pointer_surface.context("rendered pointer surface")?;

        for area in surface.pad_areas() {
            app.handle_pointer_at(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: area.x + area.width / 2,
                    row: area.y + area.height / 2,
                    modifiers: KeyModifiers::NONE,
                },
                Instant::now(),
            );
        }
        app.drain_controller_inputs()?;

        let actions = app
            .game
            .as_ref()
            .context("single-player session")?
            .pending_inputs
            .iter()
            .map(|input| input.action)
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            vec![
                TaikoAction::LEFT_KAT,
                TaikoAction::LEFT_DON,
                TaikoAction::RIGHT_DON,
                TaikoAction::RIGHT_KAT,
            ]
        );
        Ok(())
    }

    #[test]
    fn controller_setup_is_a_live_non_scoring_four_pad_diagnostic() -> Result<()> {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        app.handle_key(key(KeyCode::Char('c')));
        app.controller_setup.pointer_slot = Some(ControllerSlot::One);
        let _ = render_text(&mut app);
        let surface = app.pointer_surface.context("controller test surface")?;

        for area in surface.pad_areas() {
            app.handle_pointer_at(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: area.x + area.width / 2,
                    row: area.y + area.height / 2,
                    modifiers: KeyModifiers::NONE,
                },
                Instant::now(),
            );
        }
        app.drain_controller_inputs()?;

        assert_eq!(app.page, Page::Controllers);
        assert!(app.game.is_none());
        assert!(app.local_game.is_none());
        assert!(app.online.is_none());
        assert_eq!(
            app.controller_setup.last_test_action[ControllerSlot::One.index()]
                .map(|(action, _)| action),
            Some(TaikoAction::RIGHT_KAT)
        );
        assert!(app.controller_setup.last_test_action[ControllerSlot::Two.index()].is_none());
        Ok(())
    }

    #[test]
    fn mixed_controller_sources_dispatch_by_observation_time_exactly_once() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        wait_for_page(&mut app, Page::Game)?;
        app.audio.seek_song(1.0)?;

        let now = Instant::now();
        let older = now - Duration::from_millis(20);
        let newer = now - Duration::from_millis(5);
        app.enqueue_controller_strike(ControllerStrike::local(
            ControllerSlot::One,
            ControllerSource::Keyboard,
            TaikoAction::RIGHT_KAT,
            newer,
        ))?;
        app.enqueue_controller_strike(ControllerStrike::lan(
            ControllerSlot::One,
            7,
            TaikoAction::LEFT_DON,
            older,
            1,
            app.lan_controller_generation,
        ))?;
        app.drain_controller_inputs()?;

        let pending = &app
            .game
            .as_ref()
            .context("single-player game")?
            .pending_inputs;
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].action, TaikoAction::LEFT_DON);
        assert_eq!(pending[1].action, TaikoAction::RIGHT_KAT);
        assert!(pending[0].tick <= pending[1].tick);
        Ok(())
    }

    #[test]
    fn simultaneous_keyboard_strikes_share_one_audio_clock_sample_and_tick() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        wait_for_page(&mut app, Page::Game)?;
        app.audio.seek_song(1.0)?;

        let observed_at = Instant::now() - Duration::from_millis(5);
        app.handle_game_key(key(KeyCode::Char('s')), observed_at)?;
        app.handle_game_key(key(KeyCode::Char('d')), observed_at)?;
        app.drain_controller_inputs()?;

        let pending = &app
            .game
            .as_ref()
            .context("single-player game")?
            .pending_inputs;
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].tick, pending[1].tick);
        assert_eq!(pending[0].action, TaikoAction::LEFT_DON);
        assert_eq!(pending[1].action, TaikoAction::RIGHT_DON);
        Ok(())
    }

    #[test]
    fn controller_ingress_is_bounded_and_accounts_for_overflow() -> Result<()> {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        for sequence in 0..MAX_PENDING_CONTROLLER_STRIKES + 17 {
            app.enqueue_controller_strike(ControllerStrike::local(
                ControllerSlot::One,
                ControllerSource::Keyboard,
                TaikoAction::LEFT_DON,
                Instant::now() + Duration::from_nanos(sequence as u64),
            ))?;
        }
        assert_eq!(
            app.pending_controller_strikes.len(),
            MAX_PENDING_CONTROLLER_STRIKES
        );
        assert_eq!(app.controller_input_drops, 17);
        Ok(())
    }

    #[test]
    fn gameplay_state_changes_flush_earlier_physical_hits() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        wait_for_page(&mut app, Page::Game)?;

        app.handle_game_key(key(KeyCode::Char('s')), Instant::now())?;
        app.handle_game_key(key(KeyCode::Char('p')), Instant::now())?;
        let game = app.game.as_ref().context("paused game")?;
        assert!(game.paused);
        assert_eq!(game.pending_inputs.len(), 1);

        app.handle_game_key(key(KeyCode::Char('p')), Instant::now())?;
        app.handle_game_key(key(KeyCode::Char('s')), Instant::now())?;
        app.handle_game_key(key(KeyCode::Esc), Instant::now())?;
        let game = app.game.as_ref().context("leave-confirmation game")?;
        assert!(!game.paused);
        assert_eq!(game.pending_inputs.len(), 2);
        assert_eq!(app.leave_confirmation, Some(LeaveTarget::SinglePlayer));
        Ok(())
    }

    #[test]
    fn gameplay_state_barrier_closes_admission_before_ignoring_newer_frames() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        wait_for_page(&mut app, Page::Game)?;
        app.lan_controller_generation = 1;
        app.lan_controllers = Some(LanControllers::start(LanControllerConfig {
            bind_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            generation: 1,
        })?);

        let guard = app
            .lan_controllers
            .as_ref()
            .context("LAN controllers")?
            .begin_test_in_flight_frame();
        app.handle_game_key(key(KeyCode::Char('s')), Instant::now())?;
        app.handle_game_key(key(KeyCode::Char('p')), Instant::now())?;

        let game = app.game.as_ref().context("paused game")?;
        assert!(game.paused);
        assert_eq!(game.pending_inputs.len(), 1);
        assert!(!app
            .lan_controllers
            .as_ref()
            .context("LAN controllers")?
            .test_slot_is_active(ControllerSlot::One));
        drop(guard);
        Ok(())
    }

    #[test]
    fn terminal_pointer_assignment_routes_only_to_the_selected_local_player() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        app.controller_setup.pointer_slot = Some(ControllerSlot::Two);

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('j')));
        wait_for_page(&mut app, Page::LocalGame)?;
        let undersized_pointer_view = render_text_at(&mut app, 80, 27);
        assert!(undersized_pointer_view.contains("Required: at least 80 × 30"));
        let _ = render_text(&mut app);
        let surface = app.pointer_surface.context("P2 pointer surface")?;
        assert_eq!(surface.slot, ControllerSlot::Two);
        let right_kat = surface.pad_areas()[3];
        app.handle_pointer_at(
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: right_kat.x + right_kat.width / 2,
                row: right_kat.y + right_kat.height / 2,
                modifiers: KeyModifiers::NONE,
            },
            Instant::now(),
        );
        app.drain_controller_inputs()?;

        let game = app.local_game.as_ref().context("local session")?;
        assert!(game.players[LocalPlayerId::One.index()]
            .pending_inputs
            .is_empty());
        assert_eq!(
            game.players[LocalPlayerId::Two.index()]
                .pending_inputs
                .iter()
                .map(|input| input.action)
                .collect::<Vec<_>>(),
            vec![TaikoAction::RIGHT_KAT]
        );
        Ok(())
    }

    #[test]
    fn unavailable_audio_backend_still_builds_and_renders_the_mode_selector() -> Result<()> {
        let missing = std::env::temp_dir().join(format!(
            "taiko-no-audio-ui-library-{}",
            NEXT_UI_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let audio = AudioEngine::new_with_factory(100, 100, || {
            Err(anyhow!("fixture has no CoreAudio output device"))
        })?;
        let mut app = App::with_resources_and_audio(
            test_args(missing.clone()),
            ResourceBackend::local(missing),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
            Box::new(audio),
        )?;

        assert_eq!(app.page, Page::ModeSelect);
        assert!(matches!(
            app.audio_capability(),
            AudioCapability::Unavailable { ref reason }
                if reason.as_ref() == "fixture has no CoreAudio output device"
        ));
        assert!(matches!(
            app.audio_notice(),
            Some(AudioNotice::OutputUnavailable { reason })
                if reason.as_ref() == "fixture has no CoreAudio output device"
        ));
        let rendered = render_text(&mut app);
        assert!(rendered.contains(app.text(UiText::ChoosePlayMode)));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.page, Page::ModeSelect);
        assert!(app.error_state.is_none());
        Ok(())
    }

    #[test]
    fn sound_effect_failure_is_disabled_and_reported_after_one_attempt() -> Result<()> {
        let missing = std::env::temp_dir().join(format!(
            "taiko-failed-se-ui-library-{}",
            NEXT_UI_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let attempts = Arc::new(AtomicU64::new(0));
        let audio = TestGameAudio {
            se_failure: Some(Arc::<str>::from("fixture sound-effect device failure")),
            se_attempts: Some(Arc::clone(&attempts)),
            ..TestGameAudio::default()
        };
        let mut app = App::with_resources_and_audio(
            test_args(missing.clone()),
            ResourceBackend::local(missing),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
            Box::new(audio),
        )?;

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Up));
        app.handle_key(key(KeyCode::Down));

        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        assert_eq!(app.page, Page::ModeSelect);
        assert!(app.error_state.is_none());
        assert!(matches!(
            app.audio_notice(),
            Some(AudioNotice::SoundEffectsDisabled { reason })
                if reason.as_ref().contains("fixture sound-effect device failure")
        ));
        Ok(())
    }

    #[test]
    fn mode_and_online_connection_pages_render_and_follow_in_game_navigation() {
        let missing = std::env::temp_dir().join(format!(
            "taiko-empty-ui-library-{}",
            NEXT_UI_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut app = test_app(
            missing,
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );

        assert_eq!(app.page, Page::ModeSelect);
        let mode_text = render_text(&mut app);
        assert!(mode_text.contains("Choose Play Mode"));
        assert!(mode_text.contains("Single Player"));
        assert!(mode_text.contains("Local Two Player"));
        assert!(mode_text.contains("Online Multiplayer"));

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.active_mode, Some(GameMode::LocalTwoPlayer));
        assert_eq!(app.page, Page::SongMenu);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.page, Page::ModeSelect);
        assert_eq!(app.active_mode, None);

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.active_mode, Some(GameMode::OnlineMultiplayer));
        assert_eq!(app.page, Page::MultiplayerConnect);
        let online_text = render_text(&mut app);
        assert!(online_text.contains("Host here"));
        assert!(online_text.contains("Create"));
        assert!(online_text.contains("Join"));
        assert!(online_text.contains("Spectate"));
    }

    #[test]
    fn cjk_ui_renders_at_supported_terminal_sizes_without_corruption() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let library = ResourceBackend::local(fixture.path.clone()).load_song_library()?;

        for (
            language,
            mode_anchor,
            settings_anchor,
            local_course_anchor,
            score_anchor,
            guard_anchor,
        ) in [
            (
                UiLanguage::TraditionalChinese,
                "選擇遊玩模式",
                "玩家偏好設定",
                "選擇共用設定",
                "分數",
                "終端機尺寸太小",
            ),
            (
                UiLanguage::Japanese,
                "プレイモードを選択",
                "プレイヤー設定",
                "共通設定を選び",
                "スコア",
                "ターミナルが小さすぎます",
            ),
        ] {
            let mut app = App::with_resources_and_audio(
                test_args(fixture.path.clone()),
                ResourceBackend::local(fixture.path.clone()),
                library.clone(),
                Box::<TestGameAudio>::default(),
            )?;
            app.preferences.ui_language = language;

            let mode_text = render_text_at(&mut app, 80, 24);
            assert!(
                rendered_text_contains(&mode_text, mode_anchor),
                "missing {mode_anchor:?} in rendered buffer: {mode_text:?}"
            );
            assert!(!rendered_text_contains(&mode_text, guard_anchor));
            assert!(!mode_text.contains('\u{fffd}'));

            app.handle_key(key(KeyCode::Char('s')));
            assert_eq!(app.page, Page::Settings);
            let settings_text = render_text_at(&mut app, 120, 36);
            assert!(rendered_text_contains(&settings_text, settings_anchor));
            assert!(!rendered_text_contains(&settings_text, guard_anchor));
            assert!(!settings_text.contains('\u{fffd}'));

            app.handle_key(key(KeyCode::Esc));
            app.handle_key(key(KeyCode::Down));
            app.handle_key(key(KeyCode::Enter));
            assert_eq!(app.page, Page::SongMenu);
            app.handle_key(key(KeyCode::Enter));
            assert_eq!(app.page, Page::LocalCourseSelect);
            let local_course_text = render_text_at(&mut app, 80, 24);
            assert!(rendered_text_contains(
                &local_course_text,
                local_course_anchor
            ));
            assert!(!rendered_text_contains(&local_course_text, guard_anchor));
            app.handle_key(key(KeyCode::Char('f')));
            app.handle_key(key(KeyCode::Char('j')));
            assert_eq!(app.page, Page::OfflinePreparation);
            wait_for_page(&mut app, Page::LocalGame)?;

            let local_game_text = render_text_at(&mut app, 80, 27);
            assert!(
                local_game_text
                    .replace(' ', "")
                    .matches(score_anchor)
                    .count()
                    >= 2
            );
            assert!(!rendered_text_contains(&local_game_text, guard_anchor));
            assert!(!local_game_text.contains('\u{fffd}'));
        }

        Ok(())
    }

    #[test]
    fn restored_course_survives_mode_and_song_confirmation() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;
        let song = app.songs.first().context("fixture song")?;
        let oni = song.courses.get(1).context("fixture Oni course")?;
        app.preferences.last_mode = Some(StoredGameMode::SinglePlayer);
        app.preferences.recent_song = Some(RecentSongSelection {
            song_identity: stable_song_identity(song),
            query: String::new(),
            course_identity: oni.canonical_chart_hash.clone(),
        });

        app.restore_persisted_selection()?;
        assert_eq!(app.mode_selection, 0);
        assert_eq!(app.course_index, 1);

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.page, Page::SongMenu);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.page, Page::CourseMenu);
        assert_eq!(
            app.course_index, 1,
            "confirming the restored song must not reset its course"
        );
        Ok(())
    }

    #[test]
    fn song_search_enforces_the_persisted_utf8_byte_limit() {
        let mut app = test_app(
            "unused".into(),
            SongLibrary {
                songs: Vec::new(),
                warnings: Vec::new(),
            },
        );
        app.page = Page::SongMenu;
        for _ in 0..MAX_STORED_QUERY_BYTES {
            app.handle_key(key(KeyCode::Char('界')));
        }

        assert!(app.song_query.len() <= MAX_STORED_QUERY_BYTES);
        assert!(app.song_query.is_char_boundary(app.song_query.len()));
        assert!(app
            .song_filter_error
            .as_deref()
            .is_some_and(|error| error.contains("UTF-8 bytes")));
    }

    #[test]
    fn single_player_hud_prioritizes_play_information_without_debug_telemetry() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;

        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.page, Page::OfflinePreparation);
        wait_for_page(&mut app, Page::Game)?;

        let hud = render_text(&mut app);
        for player_facing_label in [
            "Local UI Test",
            "LIVE SCORE",
            "SCORE",
            "COMBO",
            "SOUL",
            "KEEP THE RHYTHM",
            "DON",
            "KAT",
            "KEY HOLD-REPEAT IS UNSAFE HERE",
        ] {
            assert!(
                hud.contains(player_facing_label),
                "missing player-facing HUD label {player_facing_label:?}"
            );
        }
        for debug_label in [
            "Replay:",
            "Tick now:",
            "Note offset:",
            "tick 0.00",
            "frame 0.00",
            "color:on",
        ] {
            assert!(
                !hud.contains(debug_label),
                "debug telemetry leaked into HUD: {debug_label:?}"
            );
        }

        let observed_at = Instant::now();
        for _ in 0..crate::input::MAX_OFFLINE_PENDING_INPUTS {
            app.handle_game_key(key(KeyCode::Char('s')), observed_at)?;
        }
        for _ in 0..64 {
            app.handle_game_key(key(KeyCode::Char('s')), observed_at)?;
        }
        app.drain_controller_inputs()?;
        let pending = &app
            .game
            .as_ref()
            .context("single-player game")?
            .pending_inputs;
        assert_eq!(
            pending.len(),
            crate::input::MAX_OFFLINE_PENDING_INPUTS,
            "single-player terminal bursts must not grow the pending window"
        );
        assert!(pending.windows(2).all(|pair| pair[0].tick <= pair[1].tick));
        Ok(())
    }

    #[test]
    fn nosferatu_visual_speed_keeps_paired_bpm_scroll_sections_equal() {
        let chart = rhythm_importer_tja::TjaImporter
            .import_all(include_bytes!("../samples/Nosferatu.tja"))
            .expect("import Nosferatu regression fixture")
            .into_iter()
            .find(|chart| {
                chart
                    .objects
                    .iter()
                    .any(|object| object.scroll_scaled == 630_000)
            })
            .expect("Nosferatu fixture contains its alternating-speed course");
        let slow_scroll_note = chart
            .objects
            .iter()
            .find(|object| object.scroll_scaled == 630_000)
            .expect("Nosferatu fixture contains BPM 400 × scroll 0.63 note");
        let normal_scroll_note = chart
            .objects
            .iter()
            .find(|object| object.scroll_scaled == 1_260_000)
            .expect("Nosferatu fixture contains BPM 200 × scroll 1.26 note");

        let mut engine = BasicEngine::<TaikoMode>::new_basic(&chart).expect("compile Nosferatu");
        let output = engine.step_to(0, &[]).expect("render initial frame");
        let speed_for = |id| {
            output
                .frame_view
                .notes
                .iter()
                .find(|note| note.id == id)
                .map(|note| note.visual_speed_scaled)
        };

        assert_eq!(
            speed_for(normal_scroll_note.id),
            Some(1_260_000),
            "BPM 200 × scroll 1.26 establishes the reference visual speed"
        );
        assert_eq!(
            speed_for(slow_scroll_note.id),
            Some(1_260_000),
            "BPM 400 × scroll 0.63 must render at the same visual speed"
        );
    }

    #[test]
    fn local_two_player_tui_reaches_dual_runtime_with_disjoint_inputs() -> Result<()> {
        let fixture = LocalUiFixture::create()?;
        let backend = ResourceBackend::local(fixture.path.clone());
        let library = backend.load_song_library()?;
        let mut app = App::with_resources_and_audio(
            test_args(fixture.path.clone()),
            backend,
            library,
            Box::<TestGameAudio>::default(),
        )?;

        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.page, Page::SongMenu);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.page, Page::LocalCourseSelect);

        let course_text = render_text(&mut app);
        assert!(course_text.contains("P1 — CHOOSING"));
        assert!(course_text.contains("P2 — CHOOSING"));
        assert!(course_text.contains("W/S choose, F ready"));

        app.handle_key(key(KeyCode::Char('f')));
        assert!(app.local_course_selection.is_ready(LocalPlayerId::One));
        assert_eq!(app.page, Page::LocalCourseSelect);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.page, Page::OfflinePreparation);
        wait_for_page(&mut app, Page::LocalGame)?;
        assert!(app.local_game.is_some());

        let undersized = render_text_at(&mut app, 80, 24);
        assert!(undersized.contains("Terminal too small"));
        assert!(undersized.contains("Required: at least 80 × 27"));
        let minimum_size = render_text_at(&mut app, 80, 27);
        assert!(!minimum_size.contains("Terminal too small"));
        assert!(minimum_size.matches("SCORE").count() >= 2);
        assert!(minimum_size.matches("SOUL").count() >= 2);
        assert!(minimum_size.contains("P1 // Easy"));
        assert!(minimum_size.contains("P2 // Easy"));

        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('k')));
        app.drain_controller_inputs()?;
        let game = app.local_game.as_ref().context("local game session")?;
        assert_eq!(
            game.players[LocalPlayerId::One.index()]
                .pending_inputs
                .len(),
            1
        );
        assert_eq!(
            game.players[LocalPlayerId::Two.index()]
                .pending_inputs
                .len(),
            1
        );
        let game_text = render_text(&mut app);
        assert!(game_text.contains("P1"));
        assert!(game_text.contains("P2"));
        assert!(game_text.contains("A=LEFT KAT"));
        assert!(game_text.contains("F=RIGHT KAT"));
        assert!(game_text.contains("J=LEFT KAT"));
        assert!(game_text.contains(";=RIGHT KAT"));
        assert!(game_text.contains("KEY HOLD-REPEAT IS UNSAFE HERE"));
        assert!(!game_text.contains("Shared clock"));
        assert!(!game_text.contains("Offset"));
        Ok(())
    }

    #[test]
    fn multiplayer_placeholder_does_not_scan_the_local_song_directory() {
        let args = CliArgs {
            songdir: "\0must-not-be-scanned".into(),
            resource_endpoint: None,
            resource_cache_memory_only: false,
            tps: 120,
            calibration_offset_ms: 0,
            demo: false,
            songvol: 100,
            sevol: 100,
        };

        let (backend, library) = online_placeholder_resources(&args);
        assert!(matches!(backend, ResourceBackend::Local(_)));
        assert!(library.songs.is_empty());
        assert!(library.warnings.is_empty());
    }

    #[test]
    fn online_resource_install_and_cleanup_restore_the_complete_offline_view() {
        let backend_for = |songdir: &str| {
            Arc::new(
                ResourceBackend::from_cli(&CliArgs {
                    songdir: songdir.into(),
                    resource_endpoint: None,
                    resource_cache_memory_only: false,
                    tps: 120,
                    calibration_offset_ms: 0,
                    demo: false,
                    songvol: 100,
                    sevol: 100,
                })
                .expect("create local backend"),
            )
        };
        let offline_backend = backend_for("offline-songs");
        let online_backend = backend_for("online-songs");
        let mut backend = Arc::clone(&offline_backend);
        let mut songs = Vec::new();
        let mut filtered_song_indices = vec![4, 8];
        let mut song_query = "offline query".to_owned();
        let mut song_filter_error = Some("offline filter error".to_owned());
        let mut song_index = 3;
        let mut course_index = 2;
        let mut load_warnings = vec!["offline warning".to_owned()];
        let mut load_warnings_scroll = 7;
        let mut loaded_course_chart: Option<LoadedCourseChart> = None;
        let mut offline_library_status = Some(OfflineLibraryNotice::LoadFailed {
            reason: "offline library unavailable".to_owned(),
        });

        let slots = ResourceStateSlots {
            backend: &mut backend,
            songs: &mut songs,
            filtered_song_indices: &mut filtered_song_indices,
            song_query: &mut song_query,
            song_filter_error: &mut song_filter_error,
            song_index: &mut song_index,
            course_index: &mut course_index,
            load_warnings: &mut load_warnings,
            load_warnings_scroll: &mut load_warnings_scroll,
            loaded_course_chart: &mut loaded_course_chart,
            offline_library_status: &mut offline_library_status,
        };
        let offline = OfflineResourceState::install(
            slots,
            Arc::clone(&online_backend),
            SongLibrary {
                songs: Vec::new(),
                warnings: vec!["online warning".to_owned()],
            },
        );

        assert!(Arc::ptr_eq(&backend, &online_backend));
        assert!(filtered_song_indices.is_empty());
        assert!(song_query.is_empty());
        assert_eq!(song_filter_error, None);
        assert_eq!(song_index, 0);
        assert_eq!(course_index, 0);
        assert_eq!(load_warnings, vec!["online warning"]);
        assert_eq!(load_warnings_scroll, 0);
        assert_eq!(offline_library_status, None);

        offline.restore(ResourceStateSlots {
            backend: &mut backend,
            songs: &mut songs,
            filtered_song_indices: &mut filtered_song_indices,
            song_query: &mut song_query,
            song_filter_error: &mut song_filter_error,
            song_index: &mut song_index,
            course_index: &mut course_index,
            load_warnings: &mut load_warnings,
            load_warnings_scroll: &mut load_warnings_scroll,
            loaded_course_chart: &mut loaded_course_chart,
            offline_library_status: &mut offline_library_status,
        });

        assert!(Arc::ptr_eq(&backend, &offline_backend));
        assert_eq!(filtered_song_indices, vec![4, 8]);
        assert_eq!(song_query, "offline query");
        assert_eq!(song_filter_error.as_deref(), Some("offline filter error"));
        assert_eq!(song_index, 3);
        assert_eq!(course_index, 2);
        assert_eq!(load_warnings, vec!["offline warning"]);
        assert_eq!(load_warnings_scroll, 7);
        assert_eq!(
            offline_library_status,
            Some(OfflineLibraryNotice::LoadFailed {
                reason: "offline library unavailable".to_owned(),
            })
        );
        assert!(loaded_course_chart.is_none());
    }

    #[test]
    fn empty_offline_song_directory_becomes_a_visible_nonfatal_status() {
        let status = library_status_for_contents(&[], &[], false);
        assert_eq!(
            status,
            Some(OfflineLibraryNotice::NoPlayableCharts(
                OfflineLibraryEmptyReason::LocalDirectory
            ))
        );
    }

    #[test]
    fn autoplay_events_preserve_route_gate_for_shared_runtime() {
        let inputs = vec![
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 90,
                    action: TaikoAction::LEFT_DON,
                },
                branch_segment_id: Some(42),
                branch_route_id: 0,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 90,
                    action: TaikoAction::LEFT_KAT,
                },
                branch_segment_id: Some(42),
                branch_route_id: 2,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 100,
                    action: TaikoAction::LEFT_DON,
                },
                branch_segment_id: Some(42),
                branch_route_id: 0,
            },
            AutoplayInputEvent {
                input: TimedInput {
                    tick: 100,
                    action: TaikoAction::LEFT_KAT,
                },
                branch_segment_id: Some(42),
                branch_route_id: 2,
            },
        ];

        let mut cursor = 0_usize;
        let out = collect_due_autoplay_events(&inputs, &mut cursor, 100);
        let scheduled = out
            .into_iter()
            .map(scheduled_autoplay_input)
            .collect::<Vec<_>>();

        assert_eq!(scheduled.len(), 4);
        assert_eq!(scheduled[0].input.tick, 90);
        assert_eq!(
            scheduled[0].required_route,
            Some(rhythm_mode_taiko::TaikoInputRoute {
                segment_id: 42,
                route_id: 0,
            })
        );
        assert_eq!(
            scheduled[3].required_route,
            Some(rhythm_mode_taiko::TaikoInputRoute {
                segment_id: 42,
                route_id: 2,
            })
        );
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
    fn autoplay_roll_rate_matches_the_ruleset_reserve_across_tempos() {
        assert_eq!(AUTOPLAY_ROLL_HITS_PER_SECOND, 16);
        assert_eq!(AUTOPLAY_ROLL_INTERVAL_TICKS, 62_500);
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

        let inputs = build_autoplay_inputs(&chart).expect("bounded autoplay inputs");
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].tick, 0);
        assert_eq!(inputs[1].tick, 62_500);
        assert_eq!(inputs[2].tick, 125_000);
    }

    #[test]
    fn autoplay_rejects_adversarial_unbounded_roll_expansion() {
        let chart = CanonicalChart {
            metadata: ChartMetadata::default(),
            tempo_map: vec![TempoChange {
                tick: 0,
                micros_per_quarter: 1,
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
                end_tick: Tick::MAX,
                lane_or_region: LaneOrRegion::None,
                flags: 0,
                required_hits: 0,
                slide_to: None,
                scroll_scaled: 1_000_000,
                branch_segment_id: None,
                branch_route_id: 0,
            }],
            events: Vec::new(),
        };

        let error = build_autoplay_inputs(&chart).expect_err("expansion must be bounded");
        assert!(error.to_string().contains("supported maximum"));
    }

    #[test]
    fn input_timestamp_removes_handler_delay_from_judgement_tick() {
        assert_eq!(
            chart_tick_from_audio_observation(2.000, 0.125, 0, 0),
            1_875_000
        );
    }

    #[test]
    fn calibration_is_one_explicit_offset_and_never_rewinds_runtime() {
        assert_eq!(
            chart_tick_from_audio_observation(2.000, 0.0, 35, 0),
            1_965_000
        );
        assert_eq!(
            chart_tick_from_audio_observation(0.010, 0.0, 35, 50_000),
            50_000
        );
        assert_eq!(apply_calibration_to_tick(2_000_000, 35), 1_965_000);
        assert_eq!(apply_calibration_to_tick(2_000_000, -35), 2_035_000);
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
    fn binding_candidate_validation_is_typed_before_preferences_mutation() {
        let preferences = PlayerPreferences::default();
        assert_eq!(
            binding_candidate_issue(&preferences, 0, BindingSlot::LeftKat, 'p',),
            Some(BindingCandidateIssue::PauseKeyReserved)
        );
        assert_eq!(
            binding_candidate_issue(&preferences, 0, BindingSlot::LeftKat, 'j',),
            Some(BindingCandidateIssue::AlreadyAssigned)
        );
        assert_eq!(
            binding_candidate_issue(&preferences, 0, BindingSlot::LeftKat, 'é',),
            Some(BindingCandidateIssue::VisibleAsciiRequired)
        );
        assert_eq!(
            binding_candidate_issue(&preferences, 0, BindingSlot::LeftKat, 'a',),
            None
        );
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
