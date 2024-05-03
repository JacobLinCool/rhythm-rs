use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kira::{
    clock::{ClockHandle, ClockSpeed},
    manager::{backend::DefaultBackend, AudioManager, AudioManagerSettings},
    sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
    track::{TrackBuilder, TrackHandle},
    tween::Tween,
    Volume,
};
use ratatui::prelude::Rect;
use ratatui::widgets::canvas::{Canvas, Rectangle};
use ratatui::{prelude::*, widgets::*};
use rhythm_core::Rhythm;
use serde::{de, Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap, io::Cursor, ops::Range, rc::Rc, time::Duration};
use std::{fs, io, path::PathBuf, time::Instant};
use taiko_core::constant::{COURSE_TYPE, GUAGE_FULL_THRESHOLD, GUAGE_PASS_THRESHOLD, RANGE_GREAT};
use tokio::sync::mpsc;
use tracing::instrument::WithSubscriber;

use rhythm_core::Note;
use taiko_core::{
    DefaultTaikoEngine, GameSource, Hit, InputState, Judgement, OutputState, TaikoEngine,
};
use tja::{TJACourse, TJAParser, TaikoNote, TaikoNoteType, TaikoNoteVariant, TJA};

use crate::component::*;
use crate::utils::read_utf8_or_shiftjis;
use crate::{action::Action, tui};
use crate::{
    assets::{DON_WAV, KAT_WAV},
    loader::{PlaylistLoader, Song},
};
use crate::{cli::AppArgs, latency::LatencyMeter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    None,
    SongMenu,
    CourseMenu,
    GameScreen,
    GameResult,
    Error,
}

pub struct App {
    pub state: AppGlobalState,
    pub topbar: TopBar,
    pub songmenu: SongMenu,
    pub coursemenu: CourseMenu,
    pub game: GameScreen,
    pub result: GameResult,
    pub error: ErrorPage,
    pub page: Page,
}

pub struct AppGlobalState {
    pub args: AppArgs,
    pub player: AudioManager,
    pub player_clock: ClockHandle,
    pub playing: Option<StaticSoundHandle>,
    pub sounds: HashMap<String, StaticSoundData>,
    pub effect_track: TrackHandle,
    pub next_demo: Option<(Instant, usize)>,
    pub loader: PlaylistLoader,
    pub songs: Option<Vec<Song>>,
    pub song_selector: ListState,
    pub selected_song: Option<Song>,
    pub course_selector: ListState,
    pub selected_course: Option<TJACourse>,
    pub pending_quit: bool,
    pub pending_suspend: bool,
    pub lm: LatencyMeter,
    pub taiko: Option<DefaultTaikoEngine>,
    pub output: OutputState,
    pub enter_countdown: i16,
}

impl AppGlobalState {
    pub fn player_time(&self) -> f64 {
        if self.enter_countdown <= 0 {
            self.enter_countdown as f64 / self.args.tps as f64
        } else if let Some(music) = &self.playing {
            music.position()
        } else {
            0.0
        }
    }

    pub fn schedule_demo(&mut self) {
        self.next_demo
            .replace((Instant::now(), self.song_selector.selected().unwrap_or(0)));
    }

    pub async fn play_demo(&mut self) -> Result<()> {
        let now = Instant::now();
        let elapsed = now
            .duration_since(self.next_demo.as_ref().unwrap().0)
            .as_secs_f64();
        if elapsed < 0.5 {
            return Ok(());
        }

        if self.next_demo.as_ref().unwrap().1 != self.song_selector.selected().unwrap_or(0) {
            return Ok(());
        }

        let (_, selected) = self.next_demo.take().unwrap();
        let song = self.songs.as_ref().unwrap()[selected].clone();

        let demostart = song.tja().header.demostart.unwrap_or(0.0) as f64;
        let settings = StaticSoundSettings::new()
            .loop_region(demostart..)
            .playback_region(demostart..);
        let music = song.music().await?;
        if let Some(mut playing) = self.playing.take() {
            playing.stop(Tween::default())?;
        }
        self.playing
            .replace(self.player.play(music.with_settings(settings))?);
        self.player.resume(Tween::default())?;
        Ok(())
    }
}

impl App {
    pub fn new(args: AppArgs) -> Result<Self> {
        let mut song_selector = ListState::default();
        song_selector.select(Some(0));

        let mut course_selector = ListState::default();
        course_selector.select(None);

        let loader = PlaylistLoader::new(args.songdir.clone());

        let mut player = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
        let effect_track = player.add_sub_track(TrackBuilder::new())?;
        let player_clock = player.add_clock(ClockSpeed::TicksPerSecond(10.0))?;

        let mut sounds = HashMap::new();
        sounds.insert(
            "don".to_owned(),
            StaticSoundData::from_cursor(
                Cursor::new(DON_WAV.to_vec()),
                StaticSoundSettings::new().volume(args.sevol as f64 / 100.0),
            )?,
        );
        sounds.insert(
            "kat".to_owned(),
            StaticSoundData::from_cursor(
                Cursor::new(KAT_WAV.to_vec()),
                StaticSoundSettings::new().volume(args.sevol as f64 / 100.0),
            )?,
        );

        let state = AppGlobalState {
            args,
            songs: None,
            song_selector,
            selected_song: None,
            course_selector,
            selected_course: None,
            player,
            player_clock,
            effect_track,
            sounds,
            playing: None,
            pending_quit: false,
            pending_suspend: false,
            lm: LatencyMeter::new(),
            taiko: None,
            output: OutputState {
                finished: false,
                score: 0,
                current_combo: 0,
                max_combo: 0,
                gauge: 0.0,
                judgement: None,
                display: vec![],
            },
            enter_countdown: 0,
            loader,
            next_demo: None,
        };

        let topbar = TopBar::new();
        let songmenu = SongMenu::new();
        let coursemenu = CourseMenu::new();
        let game = GameScreen::new();
        let result = GameResult::new();
        let error = ErrorPage::new();
        let page = Page::None;

        Ok(Self {
            state,
            topbar,
            songmenu,
            coursemenu,
            game,
            result,
            error,
            page,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();

        let mut tui = tui::Tui::new()?
            .tick_rate(self.state.args.tps.into())
            .frame_rate(60.0);
        tui.enter()?;

        loop {
            if self.page == Page::None {
                action_tx.send(Action::Switch(Page::SongMenu))?;
            }

            if let Some(e) = tui.next().await {
                match e {
                    tui::Event::Quit => action_tx.send(Action::Quit)?,
                    tui::Event::Tick => action_tx.send(Action::Tick)?,
                    tui::Event::Render => action_tx.send(Action::Render)?,
                    tui::Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
                    tui::Event::Key(key) => match self.page {
                        Page::SongMenu => {
                            self.songmenu
                                .handle(&mut self.state, e, action_tx.clone())?;
                        }
                        Page::CourseMenu => {
                            self.coursemenu
                                .handle(&mut self.state, e, action_tx.clone())?;
                        }
                        Page::GameScreen => {
                            self.game.handle(&mut self.state, e, action_tx.clone())?;
                        }
                        Page::GameResult => {
                            self.result.handle(&mut self.state, e, action_tx.clone())?;
                        }
                        Page::Error => {
                            self.error.handle(&mut self.state, e, action_tx.clone())?;
                        }
                        Page::None => {}
                    },
                    _ => {}
                }
            }

            while let Ok(action) = action_rx.try_recv() {
                if action != Action::Tick && action != Action::Render {
                    log::debug!("{action:?}");
                }
                match action {
                    Action::Tick => {
                        self.state.lm.tick();

                        if self.state.enter_countdown < 0 {
                            self.state.enter_countdown += 1;
                        } else if self.state.enter_countdown == 0 {
                            self.state.player.resume(Tween::default())?;
                            self.state.enter_countdown = 1;
                        }

                        if self.state.next_demo.is_some() {
                            if self.page == Page::SongMenu || self.page == Page::CourseMenu {
                                self.state.play_demo().await?;
                            } else {
                                self.state.next_demo.take();
                            }
                        }

                        let e = tui::Event::Tick;
                        match self.page {
                            Page::SongMenu => {
                                self.songmenu
                                    .handle(&mut self.state, e, action_tx.clone())?;
                            }
                            Page::CourseMenu => {
                                self.coursemenu
                                    .handle(&mut self.state, e, action_tx.clone())?;
                            }
                            Page::GameScreen => {
                                self.game.handle(&mut self.state, e, action_tx.clone())?;
                            }
                            Page::GameResult => {
                                self.result.handle(&mut self.state, e, action_tx.clone())?;
                            }
                            Page::Error => {
                                self.error.handle(&mut self.state, e, action_tx.clone())?;
                            }
                            Page::None => {}
                        }
                    }
                    Action::Quit => self.state.pending_quit = true,
                    Action::Suspend => self.state.pending_suspend = true,
                    Action::Resume => self.state.pending_suspend = false,
                    Action::Resize(w, h) => tui.resize(Rect::new(0, 0, w, h))?,
                    Action::Render => {
                        tui.draw(|f| {
                            let size = f.size();
                            let chunks = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints(
                                    [Constraint::Length(1), Constraint::Fill(size.height - 1)]
                                        .as_ref(),
                                )
                                .split(size);

                            self.topbar.render(&mut self.state, f, chunks[0]).unwrap();

                            match self.page {
                                Page::SongMenu => {
                                    self.songmenu.render(&mut self.state, f, chunks[1]).unwrap();
                                }
                                Page::CourseMenu => {
                                    self.coursemenu
                                        .render(&mut self.state, f, chunks[1])
                                        .unwrap();
                                }
                                Page::GameScreen => {
                                    self.game.render(&mut self.state, f, chunks[1]).unwrap();
                                }
                                Page::GameResult => {
                                    self.result.render(&mut self.state, f, chunks[1]).unwrap();
                                }
                                Page::Error => {
                                    self.error.render(&mut self.state, f, chunks[1]).unwrap();
                                }
                                Page::None => {}
                            }
                        })?;
                    }
                    Action::Switch(page) => {
                        if self.page != page {
                            match self.page {
                                Page::SongMenu => {
                                    self.songmenu.leave(&mut self.state, page).await?;
                                }
                                Page::CourseMenu => {
                                    self.coursemenu.leave(&mut self.state, page).await?;
                                }
                                Page::GameScreen => {
                                    self.game.leave(&mut self.state, page).await?;
                                }
                                Page::GameResult => {
                                    self.result.leave(&mut self.state, page).await?;
                                }
                                Page::Error => {
                                    self.error.leave(&mut self.state, page).await?;
                                }
                                Page::None => {}
                            }

                            self.page = page;

                            match self.page {
                                Page::SongMenu => {
                                    self.songmenu.enter(&mut self.state).await?;
                                }
                                Page::CourseMenu => {
                                    self.coursemenu.enter(&mut self.state).await?;
                                }
                                Page::GameScreen => {
                                    self.game.enter(&mut self.state).await?;
                                }
                                Page::GameResult => {
                                    self.result.enter(&mut self.state).await?;
                                }
                                Page::Error => {
                                    self.error.enter(&mut self.state).await?;
                                }
                                Page::None => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            if self.state.pending_suspend {
                tui.suspend()?;
                action_tx.send(Action::Resume)?;
                tui = tui::Tui::new()?
                    .tick_rate(self.state.args.tps.into())
                    .frame_rate(self.state.args.tps.into());
                tui.enter()?;
            } else if self.state.pending_quit {
                tui.stop()?;
                break;
            }
        }
        tui.exit()?;
        Ok(())
    }
}

fn list_songs(dir: &PathBuf) -> io::Result<Vec<(String, PathBuf)>> {
    let mut songs = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            if let Some(ext) = entry.path().extension() {
                if ext == "tja" {
                    let name = entry
                        .file_name()
                        .to_string_lossy()
                        .strip_suffix(".tja")
                        .unwrap()
                        .to_string();
                    songs.push((name, entry.path()));
                }
            }
        }
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{:?} not found", dir),
        ));
    }

    Ok(songs)
}
