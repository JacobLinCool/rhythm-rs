use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kira::{
    manager::{backend::DefaultBackend, AudioManager, AudioManagerSettings},
    sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings},
    track::{TrackBuilder, TrackHandle},
    tween::Tween,
};
use ratatui::prelude::Rect;
use ratatui::widgets::canvas::{Canvas, Rectangle};
use ratatui::{prelude::*, widgets::*};
use rhythm_core::Rhythm;
use serde::{de, Deserialize, Serialize};
use std::{collections::HashMap, io::Cursor, time::Duration};
use std::{fs, io, path::PathBuf, time::Instant};
use taiko_core::constant::{COURSE_TYPE, GUAGE_FULL_THRESHOLD, GUAGE_PASS_THRESHOLD, RANGE_GREAT};
use tokio::sync::mpsc;
use tracing::instrument::WithSubscriber;

use rhythm_core::Note;
use taiko_core::{
    DefaultTaikoEngine, GameSource, Hit, InputState, Judgement, OutputState, TaikoEngine,
};
use tja::{TJACourse, TJAParser, TaikoNote, TaikoNoteType, TaikoNoteVariant, TJA};

use crate::assets::{DON_WAV, KAT_WAV};
use crate::cli::AppArgs;
use crate::utils::read_utf8_or_shiftjis;
use crate::{action::Action, tui};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    SongSelect,
    CourseSelect,
    Game,
}

pub struct App {
    args: AppArgs,
    songs: Vec<(String, PathBuf)>,
    song: Option<TJA>,
    song_selector: ListState,
    course: Option<TJACourse>,
    course_selector: ListState,
    player: AudioManager,
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
}

impl App {
    pub fn new(args: AppArgs) -> Result<Self> {
        let mut song_selector = ListState::default();
        song_selector.select(Some(0));

        let mut course_selector = ListState::default();
        course_selector.select(None);

        let songs = list_songs(args.songdir.clone())?;
        if songs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No songs found").into());
        }

        let mut player = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
        let effect_track = player.add_sub_track(TrackBuilder::new())?;

        let mut sounds = HashMap::new();
        sounds.insert(
            "don".to_owned(),
            StaticSoundData::from_cursor(
                Cursor::new(DON_WAV.to_vec()),
                StaticSoundSettings::default(),
            )?,
        );
        sounds.insert(
            "kat".to_owned(),
            StaticSoundData::from_cursor(
                Cursor::new(KAT_WAV.to_vec()),
                StaticSoundSettings::default(),
            )?,
        );

        Ok(Self {
            args,
            songs,
            song: None,
            song_selector,
            course: None,
            course_selector,
            player,
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

    fn music_path(&self) -> Result<PathBuf> {
        if let Some(song) = &self.song {
            let fallback_ogg = self.songs[self.song_selector.selected().unwrap()]
                .1
                .with_extension("ogg");
            let rel = song
                .header
                .wave
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or(
                    fallback_ogg
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                );
            Ok(self.songs[self.song_selector.selected().unwrap()]
                .1
                .parent()
                .unwrap()
                .join(rel))
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No song is available").into())
        }
    }

    async fn enter_course_menu(&mut self) -> Result<()> {
        let selected = self.song_selector.selected().unwrap_or(0);
        let content = read_utf8_or_shiftjis(&self.songs[selected].1).unwrap();
        let parser = TJAParser::new();
        let mut song = parser
            .parse(content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        song.courses.sort_by_key(|course| course.course);
        self.song.replace(song);

        let path = self.music_path()?;

        let demostart = self.song.clone().unwrap().header.demostart.unwrap_or(0.0) as f64;
        let music =
            StaticSoundData::from_file(path, StaticSoundSettings::new().loop_region(demostart..))?;
        self.playing.replace(self.player.play(music)?);
        self.player.resume(Tween::default())?;
        Ok(())
    }

    async fn leave_course_menu(&mut self) -> Result<()> {
        self.song.take();
        if let Some(mut playing) = self.playing.take() {
            playing.stop(Tween {
                duration: Duration::from_secs_f32(0.5),
                ..Default::default()
            })?;
        }
        Ok(())
    }

    async fn enter_game(&mut self) -> Result<()> {
        self.enter_countdown = self.args.tps as i16 * -3;

        let selected = self.course_selector.selected().unwrap_or(0);
        let mut course = self
            .song
            .as_ref()
            .unwrap()
            .courses
            .get(selected)
            .unwrap()
            .clone();

        let offset = self.song.as_ref().unwrap().header.offset.unwrap_or(0.0) as f64;
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
        let music = StaticSoundData::from_file(
            self.songs[self.song_selector.selected().unwrap()]
                .1
                .with_extension("ogg"),
            StaticSoundSettings::default(),
        )?;
        self.player.pause(Tween::default())?;
        self.playing.replace(self.player.play(music)?);

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
                    let selected = self.song_selector.selected().unwrap_or(0);
                    self.song_selector
                        .select(Some((selected + self.songs.len() - 1) % self.songs.len()));
                }
                Page::CourseSelect => {
                    let selected = self.course_selector.selected().unwrap_or(0);
                    self.course_selector.select(Some(
                        (selected + self.song.as_ref().unwrap().courses.len() - 1)
                            % self.song.as_ref().unwrap().courses.len(),
                    ));
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
                    let selected = self.song_selector.selected().unwrap_or(0);
                    self.song_selector
                        .select(Some((selected + self.songs.len() + 1) % self.songs.len()));
                }
                Page::CourseSelect => {
                    let selected = self.course_selector.selected().unwrap_or(0);
                    self.course_selector.select(Some(
                        (selected + self.song.as_ref().unwrap().courses.len() + 1)
                            % self.song.as_ref().unwrap().courses.len(),
                    ));
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
                self.player.play(self.sounds["don"].clone())?;
                if self.taiko.is_some() {
                    self.hit.replace(Hit::Don);
                    self.last_hit_type.replace(Hit::Don);
                    self.hit_show = self.args.tps as i32 / 40;
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
            }
            | KeyEvent {
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
                // kat
                self.player.play(self.sounds["kat"].clone())?;
                if self.taiko.is_some() {
                    self.hit.replace(Hit::Kat);
                    self.last_hit_type.replace(Hit::Kat);
                    self.hit_show = self.args.tps as i32 / 40;
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
                            let fallback = &self.songs[self.song_selector.selected().unwrap()].0;
                            let title = self.song.as_ref().unwrap().header.title.clone();
                            if title.is_none() || title.clone().unwrap().is_empty() {
                                Some(fallback.clone())
                            } else {
                                title.clone()
                            }
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
                                let names = self.songs.iter().map(|(name, _)| name.clone());
                                let list = List::new(names)
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
                                    self.song_selector.selected().unwrap_or(0) % self.songs.len(),
                                ));

                                f.render_stateful_widget(list, chunks[1], &mut self.song_selector);
                            } else if self.taiko.is_none() {
                                let song = self.song.as_ref().unwrap();
                                let names = song.courses.iter().map(|course| {
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
                                        % song.courses.len(),
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

fn list_songs(dir: PathBuf) -> io::Result<Vec<(String, PathBuf)>> {
    let mut songs = Vec::new();
    if dir.is_dir() {
        for entry in fs::read_dir(&dir)? {
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
