use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::prelude::Rect;
use ratatui::widgets::canvas::{Canvas, Rectangle};
use ratatui::{prelude::*, widgets::*};
use rhythm_core::Rhythm;
use rodio::{source::Source, Decoder, OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{fs, io, path::PathBuf, time::Instant};
use taiko_core::constant::COURSE_TYPE;
use tokio::sync::mpsc;

use rhythm_core::Note;
use taiko_core::{
    DefaultTaikoEngine, GameSource, Hit, InputState, Judgement, OutputState, TaikoEngine,
};
use tja::{TJACourse, TJAParser, TaikoNote, TaikoNoteType, TaikoNoteVariant, TJA};

use crate::assets::{DON_WAV, KAT_WAV};
use crate::sound::{SoundData, SoundPlayer};
use crate::{action::Action, sound, tui};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    SongSelect,
    CourseSelect,
    Game,
}

pub struct App {
    songs: Vec<(String, PathBuf)>,
    song: Option<TJA>,
    song_selector: ListState,
    course: Option<TJACourse>,
    course_selector: ListState,
    player: sound::RodioSoundPlayer,
    sounds: HashMap<String, sound::SoundData>,
    music: Option<sound::SoundData>,
    fps: u8,
    pending_quit: bool,
    pending_suspend: bool,
    ticks: Vec<Instant>,
    taiko: Option<DefaultTaikoEngine>,
    last_hit: i32,
    last_hit_show: i32,
    output: OutputState,
}

impl App {
    pub fn new(dir: PathBuf, fps: u8) -> Result<Self> {
        let mut song_selector = ListState::default();
        song_selector.select(Some(0));

        let mut course_selector = ListState::default();
        course_selector.select(None);

        let songs = list_songs(dir)?;
        if songs.is_empty() {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No songs found").into());
        }

        let mut sounds = HashMap::new();
        sounds.insert(
            "don".to_owned(),
            SoundData::load_from_buffer(DON_WAV.to_vec())?,
        );
        sounds.insert(
            "kat".to_owned(),
            SoundData::load_from_buffer(KAT_WAV.to_vec())?,
        );

        Ok(Self {
            songs,
            fps,
            song: None,
            song_selector,
            course: None,
            course_selector,
            player: sound::RodioSoundPlayer::new().unwrap(),
            sounds,
            music: None,
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

    async fn enter_course_menu(&mut self) -> Result<()> {
        let selected = self.song_selector.selected().unwrap_or(0);
        let content = fs::read_to_string(&self.songs[selected].1).unwrap();
        let parser = TJAParser::new();
        let mut song = parser
            .parse(content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        song.courses.sort_by_key(|course| course.course);

        let fallback_ogg = self.songs[selected].1.with_extension("ogg");
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
        let path = self.songs[selected].1.parent().unwrap().join(rel);

        self.music.replace(sound::SoundData::load_from_path(path)?);
        self.song.replace(song);
        self.player
            .play_music_from(
                self.music.as_ref().unwrap(),
                self.song
                    .clone()
                    .unwrap()
                    .header
                    .demostart
                    .unwrap_or(0.0)
                    .into(),
            )
            .await;

        Ok(())
    }

    async fn leave_course_menu(&mut self) {
        self.song.take();
        self.music.take();
        self.player.stop_music().await;
    }

    async fn enter_game(&mut self) -> Result<()> {
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
            level: course.level.unwrap() as u8,
            scoreinit: course.scoreinit,
            scorediff: course.scorediff,
            notes: course.notes.clone(),
        };

        self.course.replace(course);
        self.taiko.replace(DefaultTaikoEngine::new(source));

        self.player.stop_music().await;
        self.player.play_music(self.music.as_ref().unwrap()).await;

        Ok(())
    }

    async fn leave_game(&mut self) {
        self.taiko.take();
        self.course.take();
        self.player.stop_music().await;
    }

    pub async fn run(&mut self) -> Result<()> {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();

        let mut tui = tui::Tui::new()?.tick_rate(self.fps.into()).frame_rate(60.0);
        tui.enter()?;

        let mut hit: Option<Hit> = None;

        loop {
            let player_time = self.player.get_music_time().await;
            if let Some(e) = tui.next().await {
                match e {
                    tui::Event::Quit => action_tx.send(Action::Quit)?,
                    tui::Event::Tick => action_tx.send(Action::Tick)?,
                    tui::Event::Render => action_tx.send(Action::Render)?,
                    tui::Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
                    tui::Event::Key(key) => {
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
                                    self.leave_course_menu().await;
                                }
                                Page::Game => {
                                    self.leave_game().await;
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
                                    self.song_selector.select(Some(
                                        (selected + self.songs.len() - 1) % self.songs.len(),
                                    ));
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
                                    self.song_selector.select(Some(
                                        (selected + self.songs.len() + 1) % self.songs.len(),
                                    ));
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
                            } => {
                                // don
                                self.player.play_effect(&self.sounds["don"]).await;
                                if self.taiko.is_some() {
                                    hit.replace(Hit::Don);
                                }
                            }
                            KeyEvent {
                                code: KeyCode::Char('d'),
                                ..
                            }
                            | KeyEvent {
                                code: KeyCode::Char('k'),
                                ..
                            } => {
                                // kat
                                self.player.play_effect(&self.sounds["kat"]).await;
                                if self.taiko.is_some() {
                                    hit.replace(Hit::Kat);
                                }
                            }
                            _ => {}
                        }
                    }
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

                        let player_time = self.player.get_music_time().await;
                        if self.taiko.is_some() {
                            let taiko = self.taiko.as_mut().unwrap();

                            let input: InputState<Hit> = InputState {
                                time: player_time,
                                hit: hit.take(),
                            };

                            self.output = taiko.forward(input);
                            if self.output.judgement.is_some() {
                                self.last_hit = match self.output.judgement.unwrap() {
                                    Judgement::Ok => 1,
                                    Judgement::Great => 2,
                                    _ => 0,
                                };
                                self.last_hit_show = 4;
                            }
                        }
                    }
                    Action::Quit => self.pending_quit = true,
                    Action::Suspend => self.pending_suspend = true,
                    Action::Resume => self.pending_suspend = false,
                    Action::Resize(w, h) => tui.resize(Rect::new(0, 0, w, h))?,
                    Action::Render => {
                        let fps = if !self.ticks.is_empty() {
                            self.ticks.len() as f64
                                / (self.ticks[self.ticks.len() - 1] - self.ticks[0]).as_secs_f64()
                        } else {
                            0.0
                        };
                        let player_time = self.player.get_music_time().await;

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
                                block::Title::from(format!("{:.2} frames per sec", fps).dim())
                                    .alignment(Alignment::Right),
                            );
                            f.render_widget(topbar_right, chunks[0]);

                            let topbar_left_content = if song_name.is_none() {
                                "Taiko on Terminal!!".to_owned()
                            } else if self.taiko.is_none() {
                                song_name.unwrap().to_string()
                            } else {
                                format!(
                                    "{} | {:.1} secs | {} pts | {} combo (max: {})",
                                    song_name.unwrap(),
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
                                            COURSE_TYPE[course.course as usize]
                                        } else {
                                            "Unknown"
                                        }
                                    )
                                });
                                let list = List::new(names)
                                    .block(
                                        Block::default()
                                            .borders(Borders::ALL)
                                            .title("Select a Course"),
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

                                let guage = Canvas::default()
                                    .paint(|ctx| {
                                        ctx.draw(&Rectangle {
                                            x: 0.0,
                                            y: 0.0,
                                            width: self.output.gauge,
                                            height: 1.0,
                                            color: Color::White,
                                        });
                                    })
                                    .x_bounds([0.0, 1.0])
                                    .y_bounds([0.0, 1.0]);
                                f.render_widget(guage, guage_chunk);

                                let mut spans: Vec<Span> =
                                    vec![Span::raw(" "); game_zone.width as usize];
                                let hit_span = (0.1 * game_zone.width as f64) as usize;
                                spans[hit_span] =
                                    Span::styled(" ", Style::default().bg(Color::Green));
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

                                let mut spans: Vec<Span> =
                                    vec![Span::raw(" "); game_zone.width as usize];
                                let hit_color = if self.last_hit_show == 0 {
                                    Color::Black
                                } else {
                                    match self.last_hit {
                                        1 => Color::White,
                                        2 => Color::Yellow,
                                        _ => Color::Black,
                                    }
                                };
                                if self.last_hit_show > 0 {
                                    self.last_hit_show -= 1;
                                }
                                spans[hit_span] = Span::styled("|", Style::default().bg(hit_color));
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
                    .tick_rate(self.fps.into())
                    .frame_rate(self.fps.into());
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
