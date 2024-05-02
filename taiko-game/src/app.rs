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
use std::{collections::HashMap, io::Cursor, ops::Range, time::Duration};
use std::{fs, io, path::PathBuf, time::Instant};
use taiko_core::constant::{COURSE_TYPE, GUAGE_FULL_THRESHOLD, GUAGE_PASS_THRESHOLD, RANGE_GREAT};
use tokio::sync::mpsc;
use tracing::instrument::WithSubscriber;

use rhythm_core::Note;
use taiko_core::{
    DefaultTaikoEngine, GameSource, Hit, InputState, Judgement, OutputState, TaikoEngine,
};
use tja::{TJACourse, TJAParser, TaikoNote, TaikoNoteType, TaikoNoteVariant, TJA};

use crate::cli::AppArgs;
use crate::utils::read_utf8_or_shiftjis;
use crate::{action::Action, tui};
use crate::{
    assets::{DON_WAV, KAT_WAV},
    loader::{PlaylistLoader, Song},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    SongSelect,
    CourseSelect,
    Game,
}

pub struct App {
    args: AppArgs,
    songs: Option<Vec<Song>>,
    song: Option<Song>,
    song_selector: ListState,
    course: Option<TJACourse>,
    course_selector: ListState,
    player: AudioManager,
    player_clock: ClockHandle,
    sounds: HashMap<String, StaticSoundData>,
    effect_track: TrackHandle,
    playing: Option<StaticSoundHandle>,
    pending_quit: bool,
    pending_suspend: bool,
    ticks: Vec<Instant>,
    taiko: Option<DefaultTaikoEngine>,
    last_hit: i32,
    last_hit_show: i32,
    output: OutputState,
    hit: Option<Hit>,
    auto_play: Option<Vec<TaikoNote>>,
    auto_play_combo_sleep: u16,
    guage_color_change: i32,
    enter_countdown: i16,
    last_hit_type: Option<Hit>,
    hit_show: i32,
    loader: PlaylistLoader,
    next_demo: Option<(Instant, usize)>,
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

        Ok(Self {
            args,
            songs: None,
            song: None,
            song_selector,
            course: None,
            course_selector,
            player,
            player_clock,
            effect_track,
            sounds,
            playing: None,
            pending_quit: false,
            pending_suspend: false,
            ticks: Vec::new(),
            taiko: None,
            last_hit: 0,
            last_hit_show: 0,
            output: OutputState {
                finished: false,
                score: 0,
                current_combo: 0,
                max_combo: 0,
                gauge: 0.0,
                judgement: None,
                display: vec![],
            },
            hit: None,
            auto_play: None,
            auto_play_combo_sleep: 0,
            guage_color_change: 0,
            enter_countdown: 0,
            last_hit_type: None,
            hit_show: 0,
            loader,
            next_demo: None,
        })
    }

    pub fn page(&self) -> Page {
        if self.song.is_none() {
            Page::SongSelect
        } else if self.taiko.is_none() {
            Page::CourseSelect
        } else {
            Page::Game
        }
    }

    async fn enter_song_menu(&mut self) -> Result<()> {
        let songs = self.loader.list().await?;
        self.songs.replace(songs);
        Ok(())
    }

    async fn enter_course_menu(&mut self) -> Result<()> {
        let selected = self.song_selector.selected().unwrap_or(0);
        let song = self.songs.as_ref().unwrap()[selected].clone();
        self.song.replace(song);
        if self.playing.is_none() {
            self.schedule_demo();
        }
        Ok(())
    }

    async fn leave_course_menu(&mut self) -> Result<()> {
        self.song.take();
        Ok(())
    }

    async fn enter_game(&mut self) -> Result<()> {
        self.enter_countdown = self.args.tps as i16 * -3;

        let song = self.song.as_ref().unwrap();

        let selected = self.course_selector.selected().unwrap_or(0);
        let mut course = song.tja().courses.get(selected).unwrap().clone();

        let offset = song.tja().header.offset.unwrap_or(0.0) as f64;
        for note in course.notes.iter_mut() {
            note.start -= offset;
        }

        let source = GameSource {
            difficulty: course.course as u8,
            level: course.level.unwrap_or(0) as u8,
            scoreinit: course.scoreinit,
            scorediff: course.scorediff,
            notes: course.notes.clone(),
        };

        if self.args.auto {
            self.auto_play.replace(course.notes.clone());
        }

        self.course.replace(course);
        self.taiko.replace(DefaultTaikoEngine::new(source));

        if let Some(mut playing) = self.playing.take() {
            playing.stop(Tween::default())?;
        }
        self.player.pause(Tween::default())?;
        self.playing.replace(self.player.play(song.music().await?)?);

        Ok(())
    }

    async fn leave_game(&mut self) -> Result<()> {
        self.taiko.take();
        self.course.take();
        if let Some(mut playing) = self.playing.take() {
            playing.stop(Tween::default())?;
        }
        Ok(())
    }

    fn schedule_demo(&mut self) {
        self.next_demo
            .replace((Instant::now(), self.song_selector.selected().unwrap_or(0)));
    }

    async fn play_demo(&mut self) -> Result<()> {
        if self.taiko.is_some() || self.next_demo.is_none() {
            return Ok(());
        }

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

    async fn handle_key_event(
        &mut self,
        key: &KeyEvent,
        action_tx: &mpsc::UnboundedSender<Action>,
    ) -> Result<()> {
        match key {
            KeyEvent {
                code: KeyCode::Char('c'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Esc, ..
            } => match self.page() {
                Page::SongSelect => action_tx.send(Action::Quit)?,
                Page::CourseSelect => {
                    self.leave_course_menu().await?;
                    self.enter_song_menu().await?;
                }
                Page::Game => {
                    self.leave_game().await?;
                    self.enter_course_menu().await?;
                }
            },
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Left,
                ..
            } => match self.page() {
                Page::SongSelect => {
                    select_prev(
                        &mut self.song_selector,
                        0..self.songs.as_ref().unwrap().len(),
                    )?;
                    self.schedule_demo();
                }
                Page::CourseSelect => {
                    select_prev(
                        &mut self.course_selector,
                        0..self.song.as_ref().unwrap().tja().courses.len(),
                    )?;
                }
                _ => {}
            },
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Right,
                ..
            } => match self.page() {
                Page::SongSelect => {
                    select_next(
                        &mut self.song_selector,
                        0..self.songs.as_ref().unwrap().len(),
                    )?;
                    self.schedule_demo();
                }
                Page::CourseSelect => {
                    select_next(
                        &mut self.course_selector,
                        0..self.song.as_ref().unwrap().tja().courses.len(),
                    )?;
                }
                _ => {}
            },
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                if self.page() == Page::SongSelect {
                    self.enter_course_menu().await?
                } else if self.page() == Page::CourseSelect {
                    self.enter_game().await?
                }
            }
            KeyEvent {
                code: KeyCode::Char(' '),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('f'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('g'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('c'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('v'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('b'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('m'),
                ..
            } => {
                // don
                if self.page() == Page::SongSelect {
                    self.enter_course_menu().await?
                } else if self.page() == Page::CourseSelect {
                    self.enter_game().await?
                } else {
                    self.player.play(self.sounds["don"].clone())?;
                    if self.taiko.is_some() {
                        self.hit.replace(Hit::Don);
                        self.last_hit_type.replace(Hit::Don);
                        self.hit_show = self.args.tps as i32 / 40;
                    }
                }
            }
            KeyEvent {
                code: KeyCode::Char('d'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('e'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('r'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('t'),
                ..
            } => {
                // kat (left)
                if self.page() == Page::SongSelect {
                    select_prev(
                        &mut self.song_selector,
                        0..self.songs.as_ref().unwrap().len(),
                    )?;
                    self.schedule_demo();
                } else if self.page() == Page::CourseSelect {
                    select_prev(
                        &mut self.course_selector,
                        0..self.song.as_ref().unwrap().tja().courses.len(),
                    )?;
                } else {
                    self.player.play(self.sounds["kat"].clone())?;
                    if self.taiko.is_some() {
                        self.hit.replace(Hit::Kat);
                        self.last_hit_type.replace(Hit::Kat);
                        self.hit_show = self.args.tps as i32 / 40;
                    }
                }
            }
            KeyEvent {
                code: KeyCode::Char('y'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('u'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('i'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('o'),
                ..
            } => {
                // kat (right)
                if self.page() == Page::SongSelect {
                    select_next(
                        &mut self.song_selector,
                        0..self.songs.as_ref().unwrap().len(),
                    )?;
                    self.schedule_demo();
                } else if self.page() == Page::CourseSelect {
                    select_next(
                        &mut self.course_selector,
                        0..self.song.as_ref().unwrap().tja().courses.len(),
                    )?;
                } else {
                    self.player.play(self.sounds["kat"].clone())?;
                    if self.taiko.is_some() {
                        self.hit.replace(Hit::Kat);
                        self.last_hit_type.replace(Hit::Kat);
                        self.hit_show = self.args.tps as i32 / 40;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub async fn run(&mut self) -> Result<()> {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();

        let mut tui = tui::Tui::new()?
            .tick_rate(self.args.tps.into())
            .frame_rate(60.0);
        tui.enter()?;

        loop {
            if self.songs.is_none() {
                self.enter_song_menu().await?;
                self.schedule_demo();
            }

            let player_time = if self.enter_countdown <= 0 {
                self.enter_countdown as f64 / self.args.tps as f64
            } else if let Some(music) = &self.playing {
                music.position()
            } else {
                0.0
            };
            if let Some(e) = tui.next().await {
                match e {
                    tui::Event::Quit => action_tx.send(Action::Quit)?,
                    tui::Event::Tick => action_tx.send(Action::Tick)?,
                    tui::Event::Render => action_tx.send(Action::Render)?,
                    tui::Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
                    tui::Event::Key(key) => self.handle_key_event(&key, &action_tx).await?,
                    _ => {}
                }
            }

            while let Ok(action) = action_rx.try_recv() {
                if action != Action::Tick && action != Action::Render {
                    log::debug!("{action:?}");
                }
                match action {
                    Action::Tick => {
                        self.ticks.push(Instant::now());
                        if self.ticks.len() > 50 {
                            self.ticks.remove(0);
                        }

                        if self.enter_countdown < 0 {
                            self.enter_countdown += 1;
                        } else if self.enter_countdown == 0 {
                            self.player.resume(Tween::default())?;
                            self.enter_countdown = 1;
                        }

                        if self.next_demo.is_some() {
                            self.play_demo().await?;
                        }

                        if self.taiko.is_some() {
                            let taiko = self.taiko.as_mut().unwrap();

                            if self.auto_play.is_some() {
                                while let Some(note) = self.auto_play.as_mut().unwrap().first() {
                                    if player_time > note.start + note.duration {
                                        self.auto_play.as_mut().unwrap().remove(0);
                                        continue;
                                    }

                                    if note.variant == TaikoNoteVariant::Don {
                                        if (note.start - player_time).abs() < RANGE_GREAT {
                                            self.player.play(self.sounds["don"].clone())?;
                                            self.hit.replace(Hit::Don);
                                            self.last_hit_type.replace(Hit::Don);
                                            self.hit_show = self.args.tps as i32 / 40;
                                            self.auto_play.as_mut().unwrap().remove(0);
                                        } else {
                                            break;
                                        }
                                    } else if note.variant == TaikoNoteVariant::Kat {
                                        if (note.start - player_time).abs() < RANGE_GREAT {
                                            self.player.play(self.sounds["kat"].clone())?;
                                            self.hit.replace(Hit::Kat);
                                            self.last_hit_type.replace(Hit::Kat);
                                            self.hit_show = self.args.tps as i32 / 40;
                                            self.auto_play.as_mut().unwrap().remove(0);
                                        } else {
                                            break;
                                        }
                                    } else if note.variant == TaikoNoteVariant::Both {
                                        if player_time > note.start {
                                            if self.auto_play_combo_sleep == 0 {
                                                self.player.play(self.sounds["don"].clone())?;
                                                self.hit.replace(Hit::Don);
                                                self.last_hit_type.replace(Hit::Don);
                                                self.hit_show = self.args.tps as i32 / 40;
                                                self.auto_play_combo_sleep = self.args.tps / 20;
                                            } else {
                                                self.auto_play_combo_sleep -= 1;
                                            }
                                            break;
                                        } else {
                                            break;
                                        }
                                    } else {
                                        self.auto_play.as_mut().unwrap().remove(0);
                                    }
                                }
                            }

                            let input: InputState<Hit> = InputState {
                                time: player_time,
                                hit: self.hit.take(),
                            };

                            self.output = taiko.forward(input);
                            if self.output.judgement.is_some() {
                                self.last_hit = match self.output.judgement.unwrap() {
                                    Judgement::Great => 1,
                                    Judgement::Ok => 2,
                                    Judgement::Miss => 3,
                                    _ => 0,
                                };
                                self.last_hit_show = 6;
                            }
                        }
                    }
                    Action::Quit => self.pending_quit = true,
                    Action::Suspend => self.pending_suspend = true,
                    Action::Resume => self.pending_suspend = false,
                    Action::Resize(w, h) => tui.resize(Rect::new(0, 0, w, h))?,
                    Action::Render => {
                        let tps = if !self.ticks.is_empty() {
                            self.ticks.len() as f64
                                / (self.ticks[self.ticks.len() - 1] - self.ticks[0]).as_secs_f64()
                        } else {
                            0.0
                        };

                        let song_name: Option<String> = if self.song.is_none() {
                            None
                        } else {
                            self.song.as_ref().unwrap().tja().header.title.clone()
                        };

                        tui.draw(|f| {
                            let size = f.size();
                            let chunks = Layout::default()
                                .direction(Direction::Vertical)
                                .constraints(
                                    [Constraint::Length(1), Constraint::Fill(size.height - 1)]
                                        .as_ref(),
                                )
                                .split(size);

                            let topbar_right = Block::default().title(
                                block::Title::from(format!("{:.2} tps", tps).dim())
                                    .alignment(Alignment::Right),
                            );
                            f.render_widget(topbar_right, chunks[0]);

                            let topbar_left_content = if song_name.is_none() {
                                format!(
                                    "Taiko on Terminal! v{} {}",
                                    env!("CARGO_PKG_VERSION"),
                                    env!("VERGEN_GIT_DESCRIBE")
                                )
                            } else if self.taiko.is_none() {
                                song_name.unwrap().to_string()
                            } else {
                                format!(
                                    "{} ({}) | {:.1} secs | {} pts | {} combo (max: {})",
                                    song_name.unwrap(),
                                    if self.course.as_ref().unwrap().course
                                        < COURSE_TYPE.len() as i32
                                    {
                                        COURSE_TYPE[self.course.as_ref().unwrap().course as usize]
                                    } else {
                                        "Unknown"
                                    },
                                    player_time,
                                    self.output.score,
                                    self.output.current_combo,
                                    self.output.max_combo
                                )
                            };
                            let topbar_left = Block::default().title(
                                block::Title::from(topbar_left_content.dim())
                                    .alignment(Alignment::Left),
                            );
                            f.render_widget(topbar_left, chunks[0]);

                            if self.song.is_none() {
                                let items = self.songs.as_ref().unwrap().iter().map(|s| {
                                    let title = Span::styled(
                                        format!("{}", s.tja().header.title.as_ref().unwrap()),
                                        Style::default(),
                                    );
                                    let tw = (f.size().width as f32 * 0.4) as usize;
                                    let w = title.width();
                                    let w = if w > tw { 0 } else { tw - w };
                                    let subtitle = Span::styled(
                                        format!(
                                            " {}{}",
                                            " ".repeat(w),
                                            s.tja().header.subtitle.as_ref().unwrap()
                                        ),
                                        Style::default().dim(),
                                    );

                                    let line = Line::from(vec![title, subtitle]);
                                    line
                                });
                                let list = List::new(items)
                                    .block(
                                        Block::default()
                                            .borders(Borders::ALL)
                                            .title("Select a Song"),
                                    )
                                    .highlight_style(
                                        Style::default()
                                            .fg(Color::Yellow)
                                            .add_modifier(Modifier::BOLD),
                                    );
                                self.song_selector.select(Some(
                                    self.song_selector.selected().unwrap_or(0)
                                        % self.songs.as_ref().unwrap().len(),
                                ));

                                f.render_stateful_widget(list, chunks[1], &mut self.song_selector);
                            } else if self.taiko.is_none() {
                                let song = self.song.as_ref().unwrap();
                                let names = song.tja().courses.iter().map(|course| {
                                    format!(
                                        "{}",
                                        if course.course < COURSE_TYPE.len() as i32 {
                                            format!(
                                                "{:<8} ({})",
                                                COURSE_TYPE[course.course as usize],
                                                course.level.unwrap_or(0)
                                            )
                                        } else {
                                            "Unknown".to_owned()
                                        }
                                    )
                                });
                                let list = List::new(names)
                                    .block(
                                        Block::default()
                                            .borders(Borders::ALL)
                                            .title("Select a Difficulty"),
                                    )
                                    .highlight_style(
                                        Style::default()
                                            .fg(Color::Yellow)
                                            .add_modifier(Modifier::BOLD),
                                    );
                                self.course_selector.select(Some(
                                    self.course_selector.selected().unwrap_or(0)
                                        % song.tja().courses.len(),
                                ));

                                f.render_stateful_widget(
                                    list,
                                    chunks[1],
                                    &mut self.course_selector,
                                );
                            } else {
                                let vertical_chunks = Layout::default()
                                    .direction(Direction::Vertical)
                                    .constraints(
                                        [Constraint::Length(1), Constraint::Length(5)].as_ref(),
                                    )
                                    .split(chunks[1]);

                                let guage_chunk = vertical_chunks[0];
                                let game_zone = vertical_chunks[1];

                                let difficulty = self.course.as_ref().unwrap().course as usize;
                                let level =
                                    self.course.as_ref().unwrap().level.unwrap_or(0) as usize;
                                let guage_color = if self.output.gauge == 1.0 {
                                    self.guage_color_change += 1;
                                    if self.guage_color_change >= 20 {
                                        self.guage_color_change = 0;
                                    }
                                    if self.guage_color_change >= 15 {
                                        Color::Cyan
                                    } else if self.guage_color_change >= 10 {
                                        Color::Yellow
                                    } else if self.guage_color_change >= 5 {
                                        Color::Green
                                    } else {
                                        Color::White
                                    }
                                } else if self.output.gauge
                                    >= (GUAGE_PASS_THRESHOLD[difficulty][level]
                                        / GUAGE_FULL_THRESHOLD[difficulty][level])
                                {
                                    Color::Yellow
                                } else {
                                    Color::White
                                };

                                let guage_splits = Layout::default()
                                    .direction(Direction::Horizontal)
                                    .constraints(
                                        [Constraint::Fill(1), Constraint::Length(4)].as_ref(),
                                    )
                                    .split(guage_chunk);

                                let guage = Canvas::default()
                                    .paint(|ctx| {
                                        ctx.draw(&Rectangle {
                                            x: 0.0,
                                            y: 0.0,
                                            width: self.output.gauge,
                                            height: 1.0,
                                            color: guage_color,
                                        });
                                    })
                                    .x_bounds([0.0, 1.0])
                                    .y_bounds([0.0, 1.0]);
                                f.render_widget(guage, guage_splits[0]);

                                let soul = Text::styled(
                                    " 魂",
                                    Style::default().fg(if self.output.gauge == 1.0 {
                                        guage_color
                                    } else {
                                        Color::Black
                                    }),
                                );
                                f.render_widget(soul, guage_splits[1]);

                                let hit_color = if self.last_hit_show == 0 {
                                    Color::Black
                                } else {
                                    match self.last_hit {
                                        1 => Color::Yellow,
                                        2 => Color::White,
                                        3 => Color::Blue,
                                        _ => Color::Black,
                                    }
                                };
                                if self.last_hit_show > 0 {
                                    self.last_hit_show -= 1;
                                }

                                let mut spans: Vec<Span> =
                                    vec![Span::raw(" "); game_zone.width as usize];
                                let hit_span = (0.1 * game_zone.width as f64) as usize;
                                spans[hit_span] =
                                    Span::styled(" ", Style::default().bg(Color::Green));
                                if hit_span > 0 {
                                    spans[hit_span - 1] =
                                        Span::styled(" ", Style::default().bg(hit_color));
                                }
                                if hit_span < game_zone.width as usize - 1 {
                                    spans[hit_span + 1] =
                                        Span::styled(" ", Style::default().bg(hit_color));
                                }
                                for note in self.output.display.iter() {
                                    let pos = note.position(player_time);
                                    if pos.is_none() {
                                        continue;
                                    }
                                    let (start, end) = pos.unwrap();
                                    let x = (start * (game_zone.width as f64)) as usize;
                                    let color = match TaikoNoteVariant::from(note.variant()) {
                                        TaikoNoteVariant::Don => Color::Red,
                                        TaikoNoteVariant::Kat => Color::Blue,
                                        TaikoNoteVariant::Both => Color::Yellow,
                                        _ => Color::White,
                                    };
                                    if x < game_zone.width as usize {
                                        match note.inner.note_type {
                                            TaikoNoteType::Small => {
                                                spans[x] =
                                                    Span::styled("o", Style::default().bg(color));
                                            }
                                            TaikoNoteType::Big => {
                                                spans[x] =
                                                    Span::styled("O", Style::default().bg(color));
                                            }
                                            TaikoNoteType::SmallCombo
                                            | TaikoNoteType::BigCombo
                                            | TaikoNoteType::Balloon
                                            | TaikoNoteType::Yam => {
                                                let end = (end * (game_zone.width as f64)) as usize;
                                                let mut x = x;
                                                while x < end && x < game_zone.width as usize {
                                                    spans[x] = Span::styled(
                                                        " ",
                                                        Style::default().bg(color),
                                                    );
                                                    x += 1;
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }

                                let note_line = Line::from(spans);

                                let hit_reflection_color =
                                    if self.last_hit_type.is_some() && self.hit_show > 0 {
                                        self.hit_show -= 1;
                                        match self.last_hit_type.as_ref().unwrap() {
                                            Hit::Don => Color::Red,
                                            Hit::Kat => Color::Cyan,
                                        }
                                    } else {
                                        Color::White
                                    };

                                let mut spans: Vec<Span> =
                                    vec![Span::raw(" "); game_zone.width as usize];
                                spans[hit_span] = Span::styled(
                                    "|",
                                    Style::default().fg(hit_reflection_color).bg(hit_color),
                                );
                                if hit_span > 0 {
                                    spans[hit_span - 1] =
                                        Span::styled(" ", Style::default().bg(hit_color));
                                }
                                if hit_span < game_zone.width as usize - 1 {
                                    spans[hit_span + 1] =
                                        Span::styled(" ", Style::default().bg(hit_color));
                                }
                                let hit_line = Line::from(spans);

                                let paragraph =
                                    Paragraph::new(vec![hit_line.clone(), note_line, hit_line])
                                        .block(Block::default().borders(Borders::ALL));
                                f.render_widget(paragraph, game_zone);
                            }
                        })?;
                    }
                    _ => {}
                }
            }
            if self.pending_suspend {
                tui.suspend()?;
                action_tx.send(Action::Resume)?;
                tui = tui::Tui::new()?
                    .tick_rate(self.args.tps.into())
                    .frame_rate(self.args.tps.into());
                tui.enter()?;
            } else if self.pending_quit {
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

fn select_next(list: &mut ListState, range: Range<usize>) -> Result<()> {
    list.select(Some((list.selected().unwrap_or(0) + 1) % range.end));
    Ok(())
}

fn select_prev(list: &mut ListState, range: Range<usize>) -> Result<()> {
    list.select(Some(
        (list.selected().unwrap_or(0) + range.end - 1) % range.end,
    ));
    Ok(())
}
