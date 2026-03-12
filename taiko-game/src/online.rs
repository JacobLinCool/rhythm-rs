use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::{SinkExt, StreamExt};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use reqwest::Url;
use rhythm_core::{ControlledEngine, Tick, TimedInput};
use rhythm_importer_tja::TjaImporter;
use rhythm_mode_taiko::{TaikoAction, TaikoMode, TaikoScoreState};
use taiko_multiplayer_protocol::{
    ClientHello, ClientMessage, FinalResultReport, HostSelectSongRequest, InputEvent,
    JoinRoomRequest, MatchSongSelection, PingPayload, PlayerStateUpdate, ReadyRequest, RoomPhase,
    RoomPlayerSnapshot, RoomRole, RoomSnapshot, ServerMessage, PROTOCOL_VERSION,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::audio::AudioEngine;
use crate::branch::BranchController;
use crate::cli::{OnlineAction, OnlineCommandArgs};
use crate::input::{map_game_hit, map_menu_intent, MenuIntent};
use crate::loader::{ResourceLocator, SongLibrary};
use crate::resource::{ResourceBackend, SongAudioSource};
use crate::screen::game_screen::{render_lane_view, LaneRenderOptions};
use crate::song_filter::SongFilter;
use crate::theme::Theme;
use crate::tui::{Frame, Tui, UiEvent};

const ONLINE_FPS: u32 = 120;
const ONLINE_TPS: u32 = 240;
const AUDIO_DRIFT_RESYNC_THRESHOLD_SECONDS: f64 = 0.2;
const AUDIO_DRIFT_RESYNC_INTERVAL: Duration = Duration::from_secs(1);
const PING_INTERVAL: Duration = Duration::from_millis(800);
const CLOCK_SAMPLE_WINDOW: usize = 64;
const CLOCK_OUTLIER_MAD_SCALE: f64 = 6.0;
const CLOCK_OUTLIER_FIXED_MARGIN_MS: f64 = 20.0;
const CLOCK_SLEW_MAX_PER_SECOND_MS: f64 = 50.0;
const CLOCK_DRIFT_RECALC_INTERVAL_MS: u64 = 4_000;
const CLOCK_DRIFT_PPM_LIMIT: f64 = 500.0;
const LOBBY_DEMO_DELAY: Duration = Duration::from_millis(500);
const LOBBY_FILTER_ROOT: &str = "/";

pub fn run_online_command(args: OnlineCommandArgs) -> Result<()> {
    let mut app = OnlineApp::new(args)?;
    let mut tui = Tui::new(ONLINE_TPS, ONLINE_FPS)?;
    tui.enter()?;

    while !app.should_quit {
        match tui.next_event()? {
            UiEvent::Tick => app.handle_tick()?,
            UiEvent::Frame => {
                tui.draw(|frame| app.render(frame))?;
            }
            UiEvent::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                app.handle_key(key)?;
            }
            UiEvent::Resize(width, height) => {
                tui.resize(Rect::new(0, 0, width, height))?;
            }
            UiEvent::Key(_) => {}
        }
    }

    tui.exit()?;
    app.shutdown();
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientMode {
    Player,
    Spectator,
}

#[derive(Debug)]
enum NetworkEvent {
    Server(Box<ServerMessage>),
    Closed(String),
}

struct NetworkClient {
    outbound: tokio_mpsc::UnboundedSender<ClientMessage>,
    inbound: Receiver<NetworkEvent>,
}

impl NetworkClient {
    fn connect(server: &str, name: &str, action: &OnlineAction) -> Result<Self> {
        let ws_url = multiplayer_ws_url(server)?;
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel::<ClientMessage>();
        let (event_tx, event_rx) = mpsc::channel::<NetworkEvent>();

        let hello = ClientMessage::Hello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            name: name.to_owned(),
        });
        let join = match action {
            OnlineAction::Create(_) => ClientMessage::CreateRoom,
            OnlineAction::Join(args) => ClientMessage::JoinRoom(JoinRoomRequest {
                room_code: args.room.to_ascii_uppercase(),
                spectate: false,
            }),
            OnlineAction::Spectate(args) => ClientMessage::JoinRoom(JoinRoomRequest {
                room_code: args.room.to_ascii_uppercase(),
                spectate: true,
            }),
        };

        thread::spawn(move || {
            let runtime = match Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = event_tx.send(NetworkEvent::Closed(format!(
                        "failed to initialize online runtime: {error}"
                    )));
                    return;
                }
            };

            runtime.block_on(async move {
                let stream = match connect_async(ws_url.as_str()).await {
                    Ok((stream, _)) => stream,
                    Err(error) => {
                        let _ = event_tx.send(NetworkEvent::Closed(format!(
                            "failed to connect to {}: {error}",
                            ws_url
                        )));
                        return;
                    }
                };

                let (mut write, mut read) = stream.split();
                for initial_message in [hello, join] {
                    let raw = match serde_json::to_string(&initial_message) {
                        Ok(raw) => raw,
                        Err(error) => {
                            let _ = event_tx.send(NetworkEvent::Closed(format!(
                                "failed to encode initial message: {error}"
                            )));
                            return;
                        }
                    };
                    if let Err(error) = write.send(WsMessage::Text(raw.into())).await {
                        let _ = event_tx.send(NetworkEvent::Closed(format!(
                            "failed to send initial message: {error}"
                        )));
                        return;
                    }
                }

                let mut outbound_rx = outbound_rx;
                loop {
                    tokio::select! {
                        outgoing = outbound_rx.recv() => {
                            let Some(outgoing) = outgoing else {
                                break;
                            };
                            let raw = match serde_json::to_string(&outgoing) {
                                Ok(raw) => raw,
                                Err(error) => {
                                    let _ = event_tx.send(NetworkEvent::Closed(format!("failed to encode outgoing message: {error}")));
                                    break;
                                }
                            };
                            if let Err(error) = write.send(WsMessage::Text(raw.into())).await {
                                let _ = event_tx.send(NetworkEvent::Closed(format!("failed to send outgoing message: {error}")));
                                break;
                            }
                        }
                        incoming = read.next() => {
                            let Some(incoming) = incoming else {
                                let _ = event_tx.send(NetworkEvent::Closed("connection closed".to_owned()));
                                break;
                            };
                            match incoming {
                                Ok(WsMessage::Text(raw)) => {
                                    match serde_json::from_str::<ServerMessage>(&raw) {
                                        Ok(message) => {
                                            if event_tx.send(NetworkEvent::Server(Box::new(message))).is_err() {
                                                break;
                                            }
                                        }
                                        Err(error) => {
                                            let _ = event_tx.send(NetworkEvent::Closed(format!("failed to decode server message: {error}")));
                                            break;
                                        }
                                    }
                                }
                                Ok(WsMessage::Close(frame)) => {
                                    let reason = frame
                                        .map(|f| f.reason.to_string())
                                        .unwrap_or_else(|| "server closed websocket".to_owned());
                                    let _ = event_tx.send(NetworkEvent::Closed(reason));
                                    break;
                                }
                                Ok(WsMessage::Ping(payload)) => {
                                    if let Err(error) = write.send(WsMessage::Pong(payload)).await {
                                        let _ = event_tx.send(NetworkEvent::Closed(format!("failed to send pong: {error}")));
                                        break;
                                    }
                                }
                                Ok(WsMessage::Pong(_)) | Ok(WsMessage::Binary(_)) | Ok(WsMessage::Frame(_)) => {}
                                Err(error) => {
                                    let _ = event_tx.send(NetworkEvent::Closed(format!("websocket error: {error}")));
                                    break;
                                }
                            }
                        }
                    }
                }
            });
        });

        Ok(Self {
            outbound: outbound_tx,
            inbound: event_rx,
        })
    }

    fn send(&self, message: ClientMessage) -> Result<()> {
        self.outbound
            .send(message)
            .map_err(|_| anyhow!("online connection is closed"))
    }

    fn try_recv(&self) -> Result<Option<NetworkEvent>> {
        match self.inbound.try_recv() {
            Ok(message) => Ok(Some(message)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                bail!("online connection channel disconnected")
            }
        }
    }
}

#[derive(Clone)]
struct PreparedMatch {
    selection: MatchSongSelection,
    branch_decisions: Vec<rhythm_importer_tja::BranchDecisionPoint>,
    chart: rhythm_chart::CanonicalChart,
    audio_source: SongAudioSource,
}

struct LocalPlayerRuntime {
    engine: ControlledEngine<TaikoMode>,
    branch_controller: BranchController,
    pending_inputs: Vec<TimedInput<TaikoAction>>,
    input_seq: u64,
    state_seq: u64,
    final_seq: u64,
    last_tick: Tick,
    last_output: rhythm_core::FrameOutput<TaikoMode>,
    sent_final: bool,
    music_started: bool,
}

#[derive(Debug, Clone)]
struct ClockSyncState {
    current_offset_ms: i64,
    target_offset_ms: i64,
    drift_ppm: f64,
    offset_median_ms: i64,
    p50_rtt_ms: f64,
    p95_rtt_ms: f64,
    jitter_ms: f64,
    accepted_samples: u64,
    rejected_samples: u64,
    large_correction_count: u64,
    rtt_window_ms: VecDeque<f64>,
    offset_window_ms: VecDeque<i64>,
    last_rtt_sample_ms: Option<f64>,
    last_slew_local_ms: Option<u64>,
    drift_anchor_local_ms: Option<u64>,
    drift_anchor_offset_ms: i64,
}

impl ClockSyncState {
    fn observe_sample(&mut self, client_send_ms: u64, client_receive_ms: u64, server_send_ms: u64) {
        if client_receive_ms < client_send_ms {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return;
        }

        let sample_rtt_ms = client_receive_ms.saturating_sub(client_send_ms) as f64;
        if !sample_rtt_ms.is_finite() || sample_rtt_ms < 0.0 {
            self.rejected_samples = self.rejected_samples.saturating_add(1);
            return;
        }

        if self.rtt_window_ms.len() >= 8 {
            let median_rtt =
                median_f64(self.rtt_window_ms.iter().copied()).unwrap_or(sample_rtt_ms);
            let mad = median_absolute_deviation_f64(self.rtt_window_ms.iter().copied(), median_rtt)
                .unwrap_or(0.0)
                .max(1.0);
            let outlier_cutoff =
                median_rtt + mad * CLOCK_OUTLIER_MAD_SCALE + CLOCK_OUTLIER_FIXED_MARGIN_MS;
            if sample_rtt_ms > outlier_cutoff {
                self.rejected_samples = self.rejected_samples.saturating_add(1);
                return;
            }
        }

        let midpoint_ms = client_send_ms.saturating_add(client_receive_ms) / 2;
        let sample_offset_ms = server_send_ms as i64 - midpoint_ms as i64;

        if let Some(prev_rtt_ms) = self.last_rtt_sample_ms {
            let deviation = (sample_rtt_ms - prev_rtt_ms).abs();
            self.jitter_ms = if self.accepted_samples == 0 {
                deviation
            } else {
                self.jitter_ms * 0.75 + deviation * 0.25
            };
        }
        self.last_rtt_sample_ms = Some(sample_rtt_ms);

        push_window(&mut self.rtt_window_ms, sample_rtt_ms, CLOCK_SAMPLE_WINDOW);
        push_window(
            &mut self.offset_window_ms,
            sample_offset_ms,
            CLOCK_SAMPLE_WINDOW,
        );

        self.p50_rtt_ms =
            percentile_f64(self.rtt_window_ms.iter().copied(), 0.5).unwrap_or(sample_rtt_ms);
        self.p95_rtt_ms =
            percentile_f64(self.rtt_window_ms.iter().copied(), 0.95).unwrap_or(sample_rtt_ms);
        self.offset_median_ms =
            median_i64(self.offset_window_ms.iter().copied()).unwrap_or(sample_offset_ms);
        self.target_offset_ms = self.offset_median_ms;
        self.accepted_samples = self.accepted_samples.saturating_add(1);

        if (self.target_offset_ms - self.current_offset_ms).abs() > 80 {
            self.large_correction_count = self.large_correction_count.saturating_add(1);
        }

        self.update_drift(client_receive_ms);
    }

    fn update_drift(&mut self, local_now_ms: u64) {
        let Some(anchor_local_ms) = self.drift_anchor_local_ms else {
            self.drift_anchor_local_ms = Some(local_now_ms);
            self.drift_anchor_offset_ms = self.offset_median_ms;
            return;
        };

        let elapsed_ms = local_now_ms.saturating_sub(anchor_local_ms);
        if elapsed_ms < CLOCK_DRIFT_RECALC_INTERVAL_MS {
            return;
        }

        let delta_offset_ms = self.offset_median_ms - self.drift_anchor_offset_ms;
        let drift_ppm = (delta_offset_ms as f64 / elapsed_ms as f64) * 1_000_000.0;
        let drift_ppm = drift_ppm.clamp(-CLOCK_DRIFT_PPM_LIMIT, CLOCK_DRIFT_PPM_LIMIT);
        self.drift_ppm = self.drift_ppm * 0.9 + drift_ppm * 0.1;

        self.drift_anchor_local_ms = Some(local_now_ms);
        self.drift_anchor_offset_ms = self.offset_median_ms;
    }

    fn tick(&mut self, local_now_ms: u64) {
        let Some(last_slew_local_ms) = self.last_slew_local_ms else {
            self.last_slew_local_ms = Some(local_now_ms);
            if self.accepted_samples > 0 {
                self.current_offset_ms = self.target_offset_ms;
            }
            return;
        };

        let dt_ms = local_now_ms.saturating_sub(last_slew_local_ms);
        self.last_slew_local_ms = Some(local_now_ms);
        if dt_ms == 0 {
            return;
        }

        let diff_ms = self.target_offset_ms - self.current_offset_ms;
        if diff_ms == 0 {
            return;
        }

        let max_step_ms = (CLOCK_SLEW_MAX_PER_SECOND_MS * dt_ms as f64 / 1000.0)
            .max(1.0)
            .round() as i64;
        let step_ms = diff_ms.clamp(-max_step_ms, max_step_ms);
        self.current_offset_ms = self.current_offset_ms.saturating_add(step_ms);
    }

    fn estimated_server_now_ms(&self, local_now_ms: u64) -> u64 {
        let mut server_now_ms = local_now_ms as i128 + self.current_offset_ms as i128;
        if let Some(anchor_local_ms) = self.drift_anchor_local_ms {
            let elapsed_ms = local_now_ms.saturating_sub(anchor_local_ms) as f64;
            let drift_ms = (elapsed_ms * self.drift_ppm / 1_000_000.0).round() as i128;
            server_now_ms += drift_ms;
        }
        server_now_ms.max(0) as u64
    }

    fn has_sample(&self) -> bool {
        self.accepted_samples > 0
    }
}

impl Default for ClockSyncState {
    fn default() -> Self {
        Self {
            current_offset_ms: 0,
            target_offset_ms: 0,
            drift_ppm: 0.0,
            offset_median_ms: 0,
            p50_rtt_ms: 0.0,
            p95_rtt_ms: 0.0,
            jitter_ms: 0.0,
            accepted_samples: 0,
            rejected_samples: 0,
            large_correction_count: 0,
            rtt_window_ms: VecDeque::new(),
            offset_window_ms: VecDeque::new(),
            last_rtt_sample_ms: None,
            last_slew_local_ms: None,
            drift_anchor_local_ms: None,
            drift_anchor_offset_ms: 0,
        }
    }
}

#[derive(Debug, Default)]
struct LobbySongBrowser {
    query: String,
    filter_error: Option<String>,
    filtered_song_indices: Vec<usize>,
    selection_index: usize,
}

impl LobbySongBrowser {
    fn new(song_count: usize) -> Self {
        Self {
            query: String::new(),
            filter_error: None,
            filtered_song_indices: (0..song_count).collect(),
            selection_index: 0,
        }
    }

    fn selected_song_index(&self) -> Option<usize> {
        self.filtered_song_indices
            .get(self.selection_index)
            .copied()
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered_song_indices.is_empty() {
            self.selection_index = 0;
            return;
        }

        let len = self.filtered_song_indices.len() as i32;
        let current = self.selection_index as i32;
        let next = (current + delta).rem_euclid(len);
        self.selection_index = next as usize;
    }

    fn sync_to_song_index(&mut self, song_index: usize) -> bool {
        let Some(next_position) = self
            .filtered_song_indices
            .iter()
            .position(|index| *index == song_index)
        else {
            return false;
        };
        self.selection_index = next_position;
        true
    }

    fn rebuild_filter(&mut self, songs: &[crate::loader::SongEntry]) {
        let previous_selected = self.selected_song_index();

        if self.query.is_empty() {
            self.filter_error = None;
            self.filtered_song_indices = (0..songs.len()).collect();
        } else {
            match SongFilter::parse(&self.query) {
                Ok(filter) => {
                    self.filter_error = None;
                    let root = Path::new(LOBBY_FILTER_ROOT);
                    self.filtered_song_indices = songs
                        .iter()
                        .enumerate()
                        .filter_map(|(index, song)| filter.matches(song, root).then_some(index))
                        .collect();
                }
                Err(error) => {
                    self.filter_error = Some(error);
                    self.filtered_song_indices.clear();
                }
            }
        }

        if let Some(previous_selected) = previous_selected {
            self.selection_index = self
                .filtered_song_indices
                .iter()
                .position(|index| *index == previous_selected)
                .unwrap_or_default();
        } else {
            self.selection_index = 0;
        }

        if self.selection_index >= self.filtered_song_indices.len() {
            self.selection_index = self.filtered_song_indices.len().saturating_sub(1);
        }
    }

    fn clear_query(&mut self, songs: &[crate::loader::SongEntry]) {
        self.query.clear();
        self.rebuild_filter(songs);
    }

    fn handle_search_key(&mut self, key: KeyEvent, songs: &[crate::loader::SongEntry]) -> bool {
        match key {
            KeyEvent {
                code: KeyCode::Esc, ..
            } if !self.query.is_empty() => {
                self.clear_query(songs);
                true
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            } => {
                if self.query.pop().is_some() {
                    self.rebuild_filter(songs);
                }
                true
            }
            KeyEvent {
                code: KeyCode::Delete,
                ..
            } => {
                if !self.query.is_empty() {
                    self.clear_query(songs);
                }
                true
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.query.is_empty() {
                    self.clear_query(songs);
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
                self.query.push(c);
                self.rebuild_filter(songs);
                true
            }
            _ => false,
        }
    }
}

struct OnlineApp {
    mode: ClientMode,
    network: NetworkClient,
    theme: Theme,
    should_quit: bool,
    status_message: String,
    error_message: Option<String>,
    room_code: Option<String>,
    actor_id: Option<String>,
    role: Option<RoomRole>,
    snapshot: Option<RoomSnapshot>,
    live_states: HashMap<String, PlayerStateUpdate>,
    final_results: HashMap<String, FinalResultReport>,
    ready: bool,
    lobby_song_browser: LobbySongBrowser,
    host_course_index: usize,
    lobby_demo_pending: Option<(Instant, usize)>,
    lobby_demo_playing_song: Option<usize>,
    resource_backend: ResourceBackend,
    song_library: SongLibrary,
    importer: TjaImporter,
    prepared_match: Option<PreparedMatch>,
    local_player: Option<LocalPlayerRuntime>,
    audio: AudioEngine,
    local_unix_base_ms: u64,
    local_mono_base: Instant,
    clock_sync: ClockSyncState,
    last_ping_sent: Instant,
    last_drift_sync: Instant,
}

impl OnlineApp {
    fn new(args: OnlineCommandArgs) -> Result<Self> {
        let (server, name, mode) = match &args.action {
            OnlineAction::Create(args) => (&args.server, &args.name, ClientMode::Player),
            OnlineAction::Join(args) => (&args.server, &args.name, ClientMode::Player),
            OnlineAction::Spectate(args) => (&args.server, &args.name, ClientMode::Spectator),
        };

        let network = NetworkClient::connect(server, name, &args.action)?;
        let resource_endpoint = resource_http_endpoint(server)?;
        let resource_backend = ResourceBackend::remote(&resource_endpoint, false)
            .context("failed to initialize remote resource backend")?;
        let song_library = resource_backend
            .load_song_library()
            .context("failed to load song library from server")?;

        let local_unix_base_ms = now_unix_ms();
        let local_mono_base = Instant::now();

        Ok(Self {
            mode,
            network,
            theme: Theme::taiko_vivid(Theme::detect()),
            should_quit: false,
            status_message: "connecting...".to_owned(),
            error_message: None,
            room_code: None,
            actor_id: None,
            role: None,
            snapshot: None,
            live_states: HashMap::new(),
            final_results: HashMap::new(),
            ready: false,
            lobby_song_browser: LobbySongBrowser::new(song_library.songs.len()),
            host_course_index: 0,
            lobby_demo_pending: None,
            lobby_demo_playing_song: None,
            resource_backend,
            song_library,
            importer: TjaImporter,
            prepared_match: None,
            local_player: None,
            audio: AudioEngine::new(100, 100)?,
            local_unix_base_ms,
            local_mono_base,
            clock_sync: ClockSyncState::default(),
            last_ping_sent: Instant::now(),
            last_drift_sync: Instant::now(),
        })
    }

    fn shutdown(&mut self) {
        let _ = self.audio.stop_song();
    }

    fn local_now_ms(&self) -> u64 {
        self.local_unix_base_ms
            .saturating_add(self.local_mono_base.elapsed().as_millis() as u64)
    }

    fn estimated_server_now_ms(&self) -> u64 {
        self.clock_sync.estimated_server_now_ms(self.local_now_ms())
    }

    fn handle_tick(&mut self) -> Result<()> {
        let local_now_ms = self.local_now_ms();
        self.clock_sync.tick(local_now_ms);

        while let Some(event) = self.network.try_recv()? {
            match event {
                NetworkEvent::Server(message) => self.handle_server_message(*message)?,
                NetworkEvent::Closed(reason) => {
                    self.error_message = Some(reason);
                    self.should_quit = true;
                    return Ok(());
                }
            }
        }

        self.tick_lobby_demo_preview()?;

        if self.last_ping_sent.elapsed() >= PING_INTERVAL {
            let nonce = local_now_ms;
            let client_send_ms = local_now_ms;
            self.network
                .send(ClientMessage::Ping(PingPayload {
                    nonce,
                    client_send_ms: Some(client_send_ms),
                    server_send_ms: None,
                }))
                .ok();
            self.last_ping_sent = Instant::now();
        }

        self.ensure_prepared_match()?;
        self.tick_player_runtime()?;
        self.tick_spectate_audio()?;
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            self.should_quit = true;
            return Ok(());
        }

        if self.error_message.is_some() {
            self.should_quit = true;
            return Ok(());
        }

        if self.current_phase() == RoomPhase::Lobby {
            self.handle_lobby_key(key)?;
            return Ok(());
        }

        if matches!(key.code, KeyCode::Esc) {
            self.should_quit = true;
            return Ok(());
        }

        if self.mode == ClientMode::Player
            && matches!(
                self.current_phase(),
                RoomPhase::Countdown | RoomPhase::Playing
            )
        {
            if let Some(action) = map_game_hit(key) {
                self.push_local_input(action)?;
            }
        }

        Ok(())
    }

    fn handle_lobby_key(&mut self, key: KeyEvent) -> Result<()> {
        if matches!(key.code, KeyCode::Char('r' | 'R')) && self.role == Some(RoomRole::Player) {
            self.ready = !self.ready;
            self.network
                .send(ClientMessage::Ready(ReadyRequest { ready: self.ready }))?;
            self.status_message = if self.ready {
                "ready sent".to_owned()
            } else {
                "unready sent".to_owned()
            };
            return Ok(());
        }

        let is_host = self.is_local_host();

        if self
            .lobby_song_browser
            .handle_search_key(key, &self.song_library.songs)
        {
            self.normalize_host_course_selection();
            return Ok(());
        }

        let Some(intent) = map_menu_intent(key) else {
            return Ok(());
        };

        if matches!(intent, MenuIntent::Quit | MenuIntent::Back) {
            self.should_quit = true;
            return Ok(());
        }

        if self.lobby_song_browser.filtered_song_indices.is_empty() {
            return Ok(());
        }

        match intent {
            MenuIntent::Up => {
                let previous = self.lobby_song_browser.selected_song_index();
                self.lobby_song_browser.move_selection(-1);
                if previous != self.lobby_song_browser.selected_song_index() {
                    self.host_course_index = 0;
                }
            }
            MenuIntent::Down => {
                let previous = self.lobby_song_browser.selected_song_index();
                self.lobby_song_browser.move_selection(1);
                if previous != self.lobby_song_browser.selected_song_index() {
                    self.host_course_index = 0;
                }
            }
            MenuIntent::Left => {
                let Some(song) = self.selected_lobby_song() else {
                    return Ok(());
                };
                let course_len = song.courses.len();
                if course_len > 0 {
                    if self.host_course_index == 0 {
                        self.host_course_index = course_len - 1;
                    } else {
                        self.host_course_index = self.host_course_index.saturating_sub(1);
                    }
                }
            }
            MenuIntent::Right => {
                let Some(song) = self.selected_lobby_song() else {
                    return Ok(());
                };
                let course_len = song.courses.len();
                if course_len > 0 {
                    self.host_course_index = (self.host_course_index + 1) % course_len;
                }
            }
            MenuIntent::Confirm => {
                if !is_host {
                    return Ok(());
                }
                let Some(song) = self.selected_lobby_song() else {
                    return Ok(());
                };
                let Some(source_id) = remote_locator_id(&song.source_locator) else {
                    bail!("host selection song is not a remote song");
                };
                self.network
                    .send(ClientMessage::HostSelectSong(HostSelectSongRequest {
                        source_id: source_id.to_owned(),
                        course_index: self.host_course_index,
                    }))?;
                self.status_message = "song selection sent".to_owned();
            }
            MenuIntent::Quit | MenuIntent::Back => {}
        }

        Ok(())
    }

    fn handle_server_message(&mut self, message: ServerMessage) -> Result<()> {
        match message {
            ServerMessage::Hello(hello) => {
                self.status_message = format!(
                    "connected (protocol={}, session={})",
                    hello.protocol_version, hello.session.session_id
                );
            }
            ServerMessage::Error(error) => {
                self.error_message = Some(format!("{}: {}", error.code, error.message));
            }
            ServerMessage::RoomCreated(created) => {
                self.room_code = Some(created.room_code.clone());
                self.actor_id = Some(created.player_id);
                self.role = Some(RoomRole::Player);
                self.status_message = format!("room created: {}", created.room_code);
            }
            ServerMessage::RoomJoined(joined) => {
                self.room_code = Some(joined.room_code.clone());
                self.actor_id = Some(joined.actor_id);
                self.role = Some(joined.role);
                self.status_message =
                    format!("joined room {} as {:?}", joined.room_code, joined.role);
            }
            ServerMessage::RoomSnapshot(snapshot) => {
                self.ingest_snapshot(snapshot);
            }
            ServerMessage::SongSelected(selection) => {
                self.status_message = format!(
                    "song selected: {} [{}]",
                    selection.title, selection.course_index
                );
                self.sync_host_selection_from_song(&selection);
            }
            ServerMessage::MatchCountdown(countdown) => {
                self.status_message =
                    format!("match countdown started: {} ms", countdown.start_at_ms);
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.phase = RoomPhase::Countdown;
                    snapshot.start_at_ms = Some(countdown.start_at_ms);
                    snapshot.song = Some(countdown.song.clone());
                }
                self.sync_host_selection_from_song(&countdown.song);
            }
            ServerMessage::MatchStarted(started) => {
                self.status_message = "match started".to_owned();
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.phase = RoomPhase::Playing;
                    snapshot.start_at_ms = Some(started.start_at_ms);
                }
            }
            ServerMessage::InputEvent(_) => {}
            ServerMessage::PlayerStateUpdate(update) => {
                self.live_states
                    .insert(update.player_id.clone(), update.state.clone());
            }
            ServerMessage::FinalResult(final_result) => {
                self.final_results
                    .insert(final_result.player_id.clone(), final_result.report.clone());
            }
            ServerMessage::Pong(pong) => {
                self.handle_pong(pong);
            }
        }

        Ok(())
    }

    fn handle_pong(&mut self, pong: PingPayload) {
        let Some(client_send_ms) = pong.client_send_ms else {
            return;
        };
        let Some(server_send_ms) = pong.server_send_ms else {
            return;
        };

        let client_receive_ms = self.local_now_ms();
        self.clock_sync
            .observe_sample(client_send_ms, client_receive_ms, server_send_ms);
    }

    fn ingest_snapshot(&mut self, snapshot: RoomSnapshot) {
        self.room_code = Some(snapshot.room_code.clone());
        self.ready = self
            .actor_id
            .as_deref()
            .and_then(|actor| {
                snapshot
                    .players
                    .iter()
                    .find(|player| player.player_id == actor)
                    .map(|player| player.ready)
            })
            .unwrap_or(false);

        for player in &snapshot.players {
            if let Some(state) = &player.last_state {
                self.live_states
                    .insert(player.player_id.clone(), state.clone());
            }
            if let Some(result) = &player.final_result {
                self.final_results
                    .insert(player.player_id.clone(), result.clone());
            }
        }

        if let Some(song) = &snapshot.song {
            self.sync_host_selection_from_song(song);
        }
        self.snapshot = Some(snapshot);
    }

    fn sync_host_selection_from_song(&mut self, song: &MatchSongSelection) {
        if let Some((song_idx, course_idx)) = self.find_song_and_course(song) {
            if !self.lobby_song_browser.sync_to_song_index(song_idx) {
                self.lobby_song_browser
                    .clear_query(&self.song_library.songs);
                let _ = self.lobby_song_browser.sync_to_song_index(song_idx);
            }
            self.host_course_index = course_idx;
        }
        self.normalize_host_course_selection();
    }

    fn is_local_host(&self) -> bool {
        self.snapshot
            .as_ref()
            .and_then(|snapshot| {
                let actor = self.actor_id.as_deref()?;
                snapshot
                    .players
                    .iter()
                    .find(|player| player.player_id == actor)
                    .map(|player| player.is_host)
            })
            .unwrap_or(false)
    }

    fn selected_lobby_song(&self) -> Option<&crate::loader::SongEntry> {
        let song_index = self.lobby_song_browser.selected_song_index()?;
        self.song_library.songs.get(song_index)
    }

    fn selected_lobby_song_index(&self) -> Option<usize> {
        self.lobby_song_browser.selected_song_index()
    }

    fn normalize_host_course_selection(&mut self) {
        let Some(song) = self.selected_lobby_song() else {
            self.host_course_index = 0;
            return;
        };

        if song.courses.is_empty() {
            self.host_course_index = 0;
            return;
        }

        if self.host_course_index >= song.courses.len() {
            self.host_course_index = song.courses.len() - 1;
        }
    }

    fn clear_lobby_demo(&mut self) -> Result<()> {
        self.lobby_demo_pending = None;
        self.lobby_demo_playing_song = None;
        self.audio.stop_song()?;
        Ok(())
    }

    fn tick_lobby_demo_preview(&mut self) -> Result<()> {
        if self.current_phase() != RoomPhase::Lobby {
            if self.lobby_demo_pending.is_some() || self.lobby_demo_playing_song.is_some() {
                self.clear_lobby_demo()?;
            }
            return Ok(());
        }

        let Some(song_index) = self.selected_lobby_song_index() else {
            if self.lobby_demo_pending.is_some() || self.lobby_demo_playing_song.is_some() {
                self.clear_lobby_demo()?;
            }
            return Ok(());
        };

        if self.lobby_demo_playing_song == Some(song_index) && !self.audio.is_song_finished() {
            self.lobby_demo_pending = None;
            return Ok(());
        }

        if self
            .lobby_demo_pending
            .is_none_or(|(_, pending_song_index)| pending_song_index != song_index)
        {
            self.audio.stop_song()?;
            self.lobby_demo_playing_song = None;
            self.lobby_demo_pending = Some((Instant::now() + LOBBY_DEMO_DELAY, song_index));
            return Ok(());
        }

        let Some((deadline, pending_song_index)) = self.lobby_demo_pending else {
            return Ok(());
        };
        if Instant::now() < deadline {
            return Ok(());
        }

        let song = self
            .song_library
            .songs
            .get(pending_song_index)
            .ok_or_else(|| anyhow!("invalid lobby demo song index {pending_song_index}"))?;
        let audio_source = self.resource_backend.load_song_audio(song)?;
        self.audio
            .play_song(audio_source, song.demo_start_seconds, true)?;
        self.lobby_demo_playing_song = Some(pending_song_index);
        self.lobby_demo_pending = None;
        Ok(())
    }

    fn ensure_prepared_match(&mut self) -> Result<()> {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return Ok(());
        };
        let Some(song) = snapshot.song.as_ref() else {
            self.prepared_match = None;
            self.local_player = None;
            return Ok(());
        };

        if self
            .prepared_match
            .as_ref()
            .is_some_and(|prepared| prepared.selection == *song)
        {
            return Ok(());
        }

        let Some((song_idx, course_idx)) = self.find_song_and_course(song) else {
            bail!(
                "selected song not found in local library: source_id={}, course_index={}",
                song.source_id,
                song.course_index
            );
        };

        let song_entry = &self.song_library.songs[song_idx];
        let chart_hash = song_entry
            .chart_content_hash
            .as_deref()
            .ok_or_else(|| anyhow!("song {} missing chart_content_hash", song.title))?;
        let audio_hash = song_entry
            .audio_content_hash
            .as_deref()
            .ok_or_else(|| anyhow!("song {} missing audio_content_hash", song.title))?;
        if chart_hash != song.chart_content_hash || audio_hash != song.audio_content_hash {
            bail!(
                "resource hash mismatch for {}: expected chart={} audio={}, local chart={} audio={}",
                song.title,
                song.chart_content_hash,
                song.audio_content_hash,
                chart_hash,
                audio_hash
            );
        }

        let chart = self
            .resource_backend
            .load_course_chart(song_entry, course_idx, &self.importer)
            .with_context(|| {
                format!(
                    "failed to load selected chart source_id={} course_index={}",
                    song.source_id, course_idx
                )
            })?;
        let audio_source = self
            .resource_backend
            .load_song_audio(song_entry)
            .with_context(|| format!("failed to load selected audio for {}", song.title))?;

        let course = song_entry
            .courses
            .get(course_idx)
            .ok_or_else(|| anyhow!("selected course index is out of range"))?;

        self.prepared_match = Some(PreparedMatch {
            selection: song.clone(),
            branch_decisions: course.branch_decisions.clone(),
            chart,
            audio_source,
        });
        self.local_player = None;

        Ok(())
    }

    fn tick_player_runtime(&mut self) -> Result<()> {
        if self.mode != ClientMode::Player {
            return Ok(());
        }

        if !matches!(
            self.current_phase(),
            RoomPhase::Countdown | RoomPhase::Playing | RoomPhase::Finished
        ) {
            self.local_player = None;
            return Ok(());
        }

        let Some(prepared) = self.prepared_match.as_ref() else {
            return Ok(());
        };

        if self.local_player.is_none() {
            let mut engine = ControlledEngine::<TaikoMode>::new_controlled(&prepared.chart)?;
            let initial = engine
                .step_to_with_controls(0, &[], &[])
                .context("failed to bootstrap local online engine")?;
            self.local_player = Some(LocalPlayerRuntime {
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
            });
        }

        let now_tick = self.estimated_server_tick().max(0);
        let phase = self.current_phase();
        let Some(runtime) = self.local_player.as_mut() else {
            return Ok(());
        };

        if matches!(phase, RoomPhase::Playing | RoomPhase::Finished) && !runtime.music_started {
            let start_seconds = (now_tick as f64 / 1_000_000.0).max(0.0);
            self.audio.stop_song()?;
            self.audio
                .play_song(prepared.audio_source.clone(), start_seconds, false)?;
            runtime.music_started = true;
        }

        let frame_tick = now_tick.max(runtime.last_tick);
        let controls = runtime
            .branch_controller
            .controls_for_tick(frame_tick, runtime.engine.score())
            .map_err(|error| anyhow!(error))?;
        let frame_inputs = collect_due_inputs(&mut runtime.pending_inputs, frame_tick);

        let output = runtime
            .engine
            .step_to_with_controls(frame_tick, &controls, &frame_inputs)
            .context("online player engine step failed")?;

        let replay_hash = output.replay_hash;
        let recent_judges = output.judges.clone();
        let state_score = output.score.clone();
        let state_frame_view = output.frame_view.clone();
        let frame_finished = output.finished;
        runtime.last_tick = frame_tick;
        runtime.last_output = output;
        runtime.state_seq = runtime.state_seq.saturating_add(1);

        self.network
            .send(ClientMessage::PlayerStateUpdate(PlayerStateUpdate {
                seq: runtime.state_seq,
                now_tick: frame_tick,
                score: state_score,
                frame_view: state_frame_view,
                recent_judges,
                replay_hash,
            }))?;

        if frame_finished && !runtime.sent_final {
            runtime.final_seq = runtime.final_seq.saturating_add(1);
            let final_result = runtime.engine.finalize();
            self.network
                .send(ClientMessage::FinalResult(FinalResultReport {
                    seq: runtime.final_seq,
                    finish_tick: frame_tick,
                    replay_hash,
                    result: final_result,
                }))?;
            runtime.sent_final = true;
        }

        Ok(())
    }

    fn tick_spectate_audio(&mut self) -> Result<()> {
        if self.mode != ClientMode::Spectator {
            return Ok(());
        }

        let phase = self.current_phase();
        if !matches!(phase, RoomPhase::Playing | RoomPhase::Finished) {
            return Ok(());
        }

        let Some(prepared) = self.prepared_match.as_ref() else {
            return Ok(());
        };

        let target_seconds = (self.estimated_server_tick() as f64 / 1_000_000.0).max(0.0);
        if self.audio.is_song_finished() {
            self.audio
                .play_song(prepared.audio_source.clone(), target_seconds, false)?;
            self.last_drift_sync = Instant::now();
            return Ok(());
        }

        if self.last_drift_sync.elapsed() >= AUDIO_DRIFT_RESYNC_INTERVAL {
            let current_seconds = self.audio.song_position_seconds();
            let drift = (current_seconds - target_seconds).abs();
            if drift > AUDIO_DRIFT_RESYNC_THRESHOLD_SECONDS {
                self.audio
                    .play_song(prepared.audio_source.clone(), target_seconds, false)?;
            }
            self.last_drift_sync = Instant::now();
        }

        Ok(())
    }

    fn push_local_input(&mut self, action: TaikoAction) -> Result<()> {
        if self.mode != ClientMode::Player {
            return Ok(());
        }
        let tick = self.estimated_server_tick().max(0);
        let Some(runtime) = self.local_player.as_mut() else {
            return Ok(());
        };

        runtime.pending_inputs.push(TimedInput { tick, action });
        runtime.pending_inputs.sort_by_key(|input| input.tick);
        runtime.input_seq = runtime.input_seq.saturating_add(1);

        self.network.send(ClientMessage::InputEvent(InputEvent {
            seq: runtime.input_seq,
            tick,
            action,
        }))?;

        match action {
            TaikoAction::Don => {
                self.audio.play_don()?;
            }
            TaikoAction::Kat => {
                self.audio.play_kat()?;
            }
        }

        Ok(())
    }

    fn current_phase(&self) -> RoomPhase {
        self.snapshot
            .as_ref()
            .map(|snapshot| snapshot.phase)
            .unwrap_or(RoomPhase::Lobby)
    }

    fn estimated_server_tick(&self) -> Tick {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return 0;
        };

        let Some(start_at_ms) = snapshot.start_at_ms else {
            return snapshot.server_tick;
        };

        let server_now_ms = self.estimated_server_now_ms();
        if server_now_ms <= start_at_ms {
            return 0;
        }
        server_now_ms
            .saturating_sub(start_at_ms)
            .saturating_mul(1000) as Tick
    }

    fn find_song_and_course(&self, selection: &MatchSongSelection) -> Option<(usize, usize)> {
        self.song_library
            .songs
            .iter()
            .enumerate()
            .find_map(|(song_idx, song)| {
                let source_id = remote_locator_id(&song.source_locator)?;
                if source_id != selection.source_id {
                    return None;
                }
                if song
                    .courses
                    .iter()
                    .any(|course| course.index == selection.course_index)
                {
                    Some((song_idx, selection.course_index))
                } else {
                    None
                }
            })
    }

    fn render(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(4),
                Constraint::Min(12),
                Constraint::Length(10),
            ])
            .split(area);

        self.render_header(frame, layout[0]);

        if self.current_phase() == RoomPhase::Lobby {
            self.render_lobby(frame, layout[1]);
        } else {
            self.render_multiplayer_lanes(frame, layout[1]);
        }

        self.render_ranking(frame, layout[2]);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let room_code = self.room_code.as_deref().unwrap_or("<pending>");
        let role = match self.role {
            Some(RoomRole::Player) => "player",
            Some(RoomRole::Spectator) => "spectator",
            None => "pending",
        };
        let server_now_ms = self.estimated_server_now_ms();
        let countdown = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.start_at_ms)
            .map(|start_at_ms| {
                if server_now_ms >= start_at_ms {
                    "started".to_owned()
                } else {
                    format!("t-{}ms", start_at_ms.saturating_sub(server_now_ms))
                }
            })
            .unwrap_or_else(|| "idle".to_owned());
        let clock_info = if self.clock_sync.has_sample() {
            format!(
                "RTT p50/p95 {:.1}/{:.1}ms | Jit {:.1}ms | Off cur/tgt/med {:+}/{:+}/{:+}ms | Drift {:+.1}ppm | ok/drop {}:{} | corr {}",
                self.clock_sync.p50_rtt_ms,
                self.clock_sync.p95_rtt_ms,
                self.clock_sync.jitter_ms,
                self.clock_sync.current_offset_ms,
                self.clock_sync.target_offset_ms,
                self.clock_sync.offset_median_ms,
                self.clock_sync.drift_ppm,
                self.clock_sync.accepted_samples,
                self.clock_sync.rejected_samples,
                self.clock_sync.large_correction_count
            )
        } else {
            "RTT syncing...".to_owned()
        };
        let controls = if self.current_phase() == RoomPhase::Lobby {
            if self.is_local_host() {
                "Lobby host = Type filter, Arrow select song, Left/Right course, Enter lock song, R ready, Esc quit"
            } else {
                "Lobby guest = R ready, Esc quit"
            }
        } else if self.mode == ClientMode::Player {
            "Playing = Don/Kat input, Esc quit"
        } else {
            "Spectate = Esc quit"
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("Online Room ", self.theme.label),
                Span::styled(room_code, self.theme.value),
                Span::styled(" | Phase ", self.theme.label),
                Span::styled(format!("{:?}", self.current_phase()), self.theme.value),
                Span::styled(" | Role ", self.theme.label),
                Span::styled(role, self.theme.value),
                Span::styled(" | Countdown ", self.theme.label),
                Span::styled(countdown, self.theme.value),
            ]),
            Line::from(vec![
                Span::styled("Status: ", self.theme.label),
                Span::styled(&self.status_message, self.theme.metadata),
            ]),
            Line::from(vec![
                Span::styled("Clock: ", self.theme.label),
                Span::styled(clock_info, self.theme.metadata),
                Span::styled(" | Controls: ", self.theme.label),
                Span::styled(controls, self.theme.metadata),
            ]),
        ];

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border)
                    .title("Taiko Online")
                    .title_style(self.theme.title),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(widget, area);
    }

    fn render_lobby(&self, frame: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
            .split(area);

        let server_selected_song_index = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.song.as_ref())
            .and_then(|song| {
                self.find_song_and_course(song)
                    .map(|(song_index, _)| song_index)
            });
        let list_items = if self.lobby_song_browser.filtered_song_indices.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "(no matching songs)",
                self.theme.warning,
            )))]
        } else {
            self.lobby_song_browser
                .filtered_song_indices
                .iter()
                .filter_map(|index| {
                    self.song_library
                        .songs
                        .get(*index)
                        .map(|song| (index, song))
                })
                .map(|(index, song)| {
                    let subtitle = if song.subtitle.trim().is_empty() {
                        String::new()
                    } else {
                        format!(" - {}", song.subtitle)
                    };
                    let mut spans = vec![
                        Span::styled(song.title.clone(), self.theme.text_primary),
                        Span::styled(subtitle, self.theme.text_secondary),
                    ];
                    if server_selected_song_index == Some(*index) {
                        spans.push(Span::styled(" [LOCKED]", self.theme.success));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect::<Vec<_>>()
        };

        let list_title = if self.is_local_host() {
            "Lobby Song Menu (Type to filter, Arrow move, Left/Right course, Enter lock)"
        } else {
            "Lobby Song Menu (Host selecting; Arrow to browse local list)"
        };
        let list = List::new(list_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border)
                    .title(list_title)
                    .title_style(self.theme.title),
            )
            .highlight_style(self.theme.selection)
            .highlight_symbol(">> ");

        let mut state = ListState::default();
        state.select(
            (!self.lobby_song_browser.filtered_song_indices.is_empty())
                .then_some(self.lobby_song_browser.selection_index),
        );
        frame.render_stateful_widget(list, chunks[0], &mut state);

        let kv_line = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{label}: "), self.theme.label),
                Span::styled(value, self.theme.value),
            ])
        };
        let text_line =
            |text: &str| Line::from(Span::styled(text.to_owned(), self.theme.text_primary));
        let mut info_lines = vec![
            kv_line(
                "Search",
                if self.lobby_song_browser.query.is_empty() {
                    "<empty>".to_owned()
                } else {
                    self.lobby_song_browser.query.clone()
                },
            ),
            kv_line(
                "Matches",
                format!(
                    "{}/{}",
                    self.lobby_song_browser.filtered_song_indices.len(),
                    self.song_library.songs.len()
                ),
            ),
        ];

        if let Some(error) = &self.lobby_song_browser.filter_error {
            info_lines.push(Line::from(vec![
                Span::styled("Filter error: ", self.theme.label),
                Span::styled(error.clone(), self.theme.error),
            ]));
        }

        if let Some(song) = self.selected_lobby_song() {
            let bpm = song
                .courses
                .first()
                .and_then(|course| course.base_bpm)
                .unwrap_or_default();
            let selected_course = song
                .courses
                .get(self.host_course_index)
                .map(|course| {
                    format!(
                        "{} (#{}, Lv {})",
                        course.name,
                        course.index + 1,
                        course
                            .level
                            .map_or_else(|| "?".to_owned(), |value| value.to_string())
                    )
                })
                .unwrap_or_else(|| "<invalid course>".to_owned());

            info_lines.extend(vec![
                text_line(""),
                kv_line("Selected", format!("{} / {}", song.title, selected_course)),
                kv_line("Subtitle", song.subtitle.clone()),
                kv_line("Artist", song.artist.clone()),
                kv_line("BPM", format!("{bpm:.2}")),
                kv_line("Chart", song.source_path.display().to_string()),
                kv_line("Audio", song.audio_path.display().to_string()),
                kv_line("Courses", song.courses.len().to_string()),
                kv_line("Branching", song.has_branching().to_string()),
            ]);
        } else {
            info_lines.extend(vec![
                text_line(""),
                text_line("No song matched current filter"),
            ]);
        }

        if let Some(snapshot) = &self.snapshot {
            info_lines.push(text_line(""));
            if let Some(song) = &snapshot.song {
                info_lines.push(kv_line(
                    "Server Song",
                    format!("{} [course {}]", song.title, song.course_index + 1),
                ));
            } else {
                info_lines.push(kv_line("Server Song", "<not selected>".to_owned()));
            }
            info_lines.push(text_line(""));
            info_lines.push(Line::from(Span::styled("Players:", self.theme.label)));
            for player in &snapshot.players {
                let status = if player.dnf {
                    "DNF"
                } else if !player.online {
                    "offline"
                } else if player.ready {
                    "ready"
                } else {
                    "waiting"
                };
                info_lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "- {}{}",
                            player.name,
                            if player.is_host { " [HOST]" } else { "" }
                        ),
                        self.theme.value,
                    ),
                    Span::styled(format!(" -> {status}"), self.theme.metadata),
                ]));
            }
            info_lines.push(kv_line("Spectators", snapshot.spectators.len().to_string()));
        } else {
            info_lines.push(text_line(""));
            info_lines.push(Line::from(Span::styled(
                "Waiting for room snapshot...",
                self.theme.metadata,
            )));
        }

        let info = Paragraph::new(info_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border)
                    .title("Lobby Info")
                    .title_style(self.theme.title),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(info, chunks[1]);
    }

    fn render_multiplayer_lanes(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            let widget = Paragraph::new("Waiting for room snapshot")
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.border),
                )
                .wrap(Wrap { trim: true });
            frame.render_widget(widget, area);
            return;
        };

        let player_count = snapshot.players.len().max(1);
        let rows = if player_count <= 2 { 1 } else { 2 };
        let cols = if player_count <= 1 { 1 } else { 2 };

        let row_constraints = (0..rows)
            .map(|_| Constraint::Ratio(1, rows as u32))
            .collect::<Vec<_>>();
        let row_areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(row_constraints)
            .split(area);

        let mut player_iter = snapshot.players.iter();
        for row_area in row_areas.iter().copied() {
            let col_constraints = (0..cols)
                .map(|_| Constraint::Ratio(1, cols as u32))
                .collect::<Vec<_>>();
            let col_areas = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(col_constraints)
                .split(row_area);
            for col_area in col_areas.iter().copied() {
                let Some(player) = player_iter.next() else {
                    break;
                };
                self.render_player_lane_tile(frame, col_area, player);
            }
        }
    }

    fn render_player_lane_tile(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        player: &RoomPlayerSnapshot,
    ) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.theme.border)
            .title(format!(
                "{}{}{}",
                player.name,
                if player.is_host { " [HOST]" } else { "" },
                if player.dnf { " [DNF]" } else { "" }
            ))
            .title_style(self.theme.title);
        frame.render_widget(block, area);

        let inner = Rect {
            x: area.x + 1,
            y: area.y + 1,
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(2),
        };
        if inner.width < 4 || inner.height < 6 {
            return;
        }

        let Some((frame_view, score)) = self.player_live_view(player) else {
            let widget = Paragraph::new("no live frame")
                .style(self.theme.metadata)
                .wrap(Wrap { trim: true });
            frame.render_widget(widget, inner);
            return;
        };

        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(5)])
            .split(inner);

        let header = Paragraph::new(vec![Line::from(vec![
            Span::styled("Score ", self.theme.label),
            Span::styled(format!("{}", score.score), self.theme.value),
            Span::styled(" | Combo ", self.theme.label),
            Span::styled(format!("{}", score.combo), self.theme.value),
            Span::styled(" | Gauge ", self.theme.label),
            Span::styled(format!("{:.1}%", score.gauge * 100.0), self.theme.value),
        ])]);
        frame.render_widget(header, split[0]);

        render_lane_view(
            &self.theme,
            frame,
            split[1],
            &frame_view,
            LaneRenderOptions::standard(1.0),
        );
    }

    fn player_live_view(
        &self,
        player: &RoomPlayerSnapshot,
    ) -> Option<(rhythm_mode_taiko::TaikoFrameView, TaikoScoreState)> {
        if self.role == Some(RoomRole::Player)
            && self.actor_id.as_deref() == Some(player.player_id.as_str())
        {
            if let Some(runtime) = &self.local_player {
                return Some((
                    runtime.last_output.frame_view.clone(),
                    runtime.last_output.score.clone(),
                ));
            }
        }

        let state = self
            .live_states
            .get(&player.player_id)
            .or(player.last_state.as_ref())?;
        Some((state.frame_view.clone(), state.score.clone()))
    }

    fn render_ranking(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };

        let mut rows = snapshot
            .players
            .iter()
            .map(|player| {
                let state = self
                    .live_states
                    .get(&player.player_id)
                    .or(player.last_state.as_ref());
                let result = self
                    .final_results
                    .get(&player.player_id)
                    .or(player.final_result.as_ref());
                let score = result
                    .map(|result| result.result.score)
                    .or_else(|| state.map(|state| state.score.score))
                    .unwrap_or_default();
                let max_combo = result
                    .map(|result| result.result.max_combo)
                    .or_else(|| state.map(|state| state.score.max_combo))
                    .unwrap_or_default();
                let finish_tick = result.map(|result| result.finish_tick).unwrap_or(i64::MAX);
                (
                    player.player_id.clone(),
                    player.name.clone(),
                    player.dnf,
                    score,
                    max_combo,
                    finish_tick,
                )
            })
            .collect::<Vec<_>>();

        rows.sort_by(|a, b| {
            a.2.cmp(&b.2)
                .then_with(|| b.3.cmp(&a.3))
                .then_with(|| b.4.cmp(&a.4))
                .then_with(|| a.5.cmp(&b.5))
        });

        let mut lines = Vec::new();
        lines.push(Line::from(vec![Span::styled(
            "Ranking (score > combo > finish_tick, DNF bottom)",
            self.theme.label,
        )]));
        for (rank, (_player_id, name, dnf, score, max_combo, finish_tick)) in
            rows.iter().enumerate()
        {
            let finish = if *finish_tick == i64::MAX {
                "-".to_owned()
            } else {
                format!("{:.3}s", *finish_tick as f64 / 1_000_000.0)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("#{} ", rank + 1), self.theme.label),
                Span::styled(name, self.theme.value),
                Span::styled(
                    format!(
                        " | score={} combo={} finish={}{}",
                        score,
                        max_combo,
                        finish,
                        if *dnf { " | DNF" } else { "" }
                    ),
                    if *dnf {
                        self.theme.error
                    } else {
                        self.theme.metadata
                    },
                ),
            ]));
        }

        if let Some(error) = &self.error_message {
            lines.push(Line::from(Span::styled(
                format!("Error: {error}"),
                self.theme.error,
            )));
        }

        let widget = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.theme.border)
                    .title("Match")
                    .title_style(self.theme.title),
            )
            .wrap(Wrap { trim: true });
        frame.render_widget(widget, area);
    }
}

fn collect_due_inputs(
    pending_inputs: &mut Vec<TimedInput<TaikoAction>>,
    now_tick: Tick,
) -> Vec<TimedInput<TaikoAction>> {
    if pending_inputs.is_empty() {
        return Vec::new();
    }

    let split = pending_inputs.partition_point(|input| input.tick <= now_tick);
    pending_inputs.drain(..split).collect()
}

fn push_window<T>(window: &mut VecDeque<T>, value: T, max_len: usize) {
    window.push_back(value);
    if window.len() > max_len {
        let _ = window.pop_front();
    }
}

fn median_i64(values: impl IntoIterator<Item = i64>) -> Option<i64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[values.len() / 2])
}

fn median_f64(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    percentile_f64(values, 0.5)
}

fn percentile_f64(values: impl IntoIterator<Item = f64>, percentile: f64) -> Option<f64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let percentile = percentile.clamp(0.0, 1.0);
    let idx = ((values.len() - 1) as f64 * percentile).round() as usize;
    values.get(idx).copied()
}

fn median_absolute_deviation_f64(
    values: impl IntoIterator<Item = f64>,
    median: f64,
) -> Option<f64> {
    let deviations = values
        .into_iter()
        .map(|value| (value - median).abs())
        .collect::<Vec<_>>();
    median_f64(deviations)
}

fn remote_locator_id(locator: &ResourceLocator) -> Option<&str> {
    match locator {
        ResourceLocator::RemoteId(id) => Some(id.as_str()),
        ResourceLocator::LocalPath(_) => None,
    }
}

fn resource_http_endpoint(server: &str) -> Result<String> {
    let mut url = Url::parse(server).with_context(|| format!("invalid --server URL: {server}"))?;
    match url.scheme() {
        "http" | "https" => {}
        "ws" => {
            url.set_scheme("http")
                .map_err(|_| anyhow!("failed to normalize ws URL to http"))?;
        }
        "wss" => {
            url.set_scheme("https")
                .map_err(|_| anyhow!("failed to normalize wss URL to https"))?;
        }
        scheme => bail!("unsupported URL scheme `{scheme}` for --server"),
    }

    if !url.path().ends_with('/') {
        let mut path = url.path().to_owned();
        path.push('/');
        url.set_path(&path);
    }
    Ok(url.to_string())
}

fn multiplayer_ws_url(server: &str) -> Result<Url> {
    let mut base = Url::parse(server).with_context(|| format!("invalid --server URL: {server}"))?;
    match base.scheme() {
        "http" => {
            base.set_scheme("ws")
                .map_err(|_| anyhow!("failed to convert http scheme to ws"))?;
        }
        "https" => {
            base.set_scheme("wss")
                .map_err(|_| anyhow!("failed to convert https scheme to wss"))?;
        }
        "ws" | "wss" => {}
        scheme => bail!("unsupported URL scheme `{scheme}` for --server"),
    }

    if !base.path().ends_with('/') {
        let mut path = base.path().to_owned();
        path.push('/');
        base.set_path(&path);
    }

    base.join("v1/multiplayer/ws")
        .context("failed to build multiplayer websocket URL")
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::LobbySongBrowser;
    use crate::loader::{CourseEntry, ResourceLocator, SongEntry};
    use std::path::PathBuf;

    fn test_song(title: &str) -> SongEntry {
        SongEntry {
            source_locator: ResourceLocator::LocalPath(PathBuf::from(format!("{title}.tja"))),
            audio_locator: ResourceLocator::LocalPath(PathBuf::from(format!("{title}.ogg"))),
            chart_content_hash: None,
            audio_content_hash: None,
            source_path: PathBuf::from(format!("{title}.tja")),
            audio_path: PathBuf::from(format!("{title}.ogg")),
            title: title.to_owned(),
            subtitle: String::new(),
            artist: "tester".to_owned(),
            demo_start_seconds: 0.0,
            courses: vec![CourseEntry {
                index: 0,
                name: "Oni".to_owned(),
                level: Some(8),
                object_count: 100,
                branch_segment_count: 0,
                base_bpm: Some(180.0),
                branch_decisions: vec![],
            }],
        }
    }

    #[test]
    fn lobby_song_browser_preserves_selection_when_filter_keeps_song() {
        let songs = vec![test_song("Alpha"), test_song("Bravo"), test_song("Charlie")];
        let mut browser = LobbySongBrowser::new(songs.len());
        assert!(browser.sync_to_song_index(1));

        browser.query = "bravo".to_owned();
        browser.rebuild_filter(&songs);

        assert_eq!(browser.filtered_song_indices, vec![1]);
        assert_eq!(browser.selected_song_index(), Some(1));
        assert!(browser.filter_error.is_none());
    }

    #[test]
    fn lobby_song_browser_invalid_filter_clears_results() {
        let songs = vec![test_song("Alpha"), test_song("Bravo")];
        let mut browser = LobbySongBrowser::new(songs.len());

        browser.query = "oni=11".to_owned();
        browser.rebuild_filter(&songs);

        assert!(browser.filtered_song_indices.is_empty());
        assert_eq!(browser.selected_song_index(), None);
        assert!(browser.filter_error.is_some());
    }
}
