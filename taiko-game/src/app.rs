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
use tokio::sync::mpsc;

use rhythm_core::Note;
use tja::{TJACourse, TJAParser, TaikoNote, TaikoNoteType, TaikoNoteVariant, TJA};

use crate::assets::{DON_WAV, KAT_WAV};
use crate::sound::{SoundData, SoundPlayer};
use crate::{action::Action, sound, tui};

pub struct App {
    songs: Vec<(String, PathBuf)>,
    song: Option<TJA>,
    song_selector: ListState,
    course: Option<TJACourse>,
    game: Option<Rhythm<TaikoNote>>,
    course_selector: ListState,
    player: sound::RodioSoundPlayer,
    sounds: HashMap<String, sound::SoundData>,
    music: Option<sound::SoundData>,
    fps: u8,
    pending_quit: bool,
    pending_suspend: bool,
    ticks: Vec<Instant>,
    playing: bool,
    score: i32,
    last_hit: i32,
    last_hit_show: i32,
    combo: i32,
}

impl App {
    pub fn new(dir: PathBuf, fps: u8) -> Result<Self> {
        let mut song_selector = ListState::default();
        song_selector.select(Some(0));

        let mut course_selector = ListState::default();
        course_selector.select(None);

        let songs = list_songs(dir).unwrap();

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
            game: None,
            course_selector,
            player: sound::RodioSoundPlayer::new().unwrap(),
            sounds,
            music: None,
            pending_quit: false,
            pending_suspend: false,
            ticks: Vec::new(),
            playing: false,
            score: 0,
            last_hit: 0,
            last_hit_show: 0,
            combo: 0,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();

        let mut tui = tui::Tui::new()?.tick_rate(self.fps.into()).frame_rate(60.0);
        tui.enter()?;

        loop {
            if let Some(e) = tui.next().await {
                if self.music.is_some() && self.game.is_some() && !self.playing {
                    self.player.stop_music().await;
                    self.player.play_music(self.music.as_ref().unwrap()).await;
                    self.playing = true;
                    self.score = 0;
                    self.combo = 0;
                }

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
                            } => {
                                if self.game.is_some() {
                                    self.game = None;
                                    self.course = None;
                                    self.score = 0;
                                    self.playing = false;
                                    self.player.stop_music().await;
                                } else if self.song.is_some() {
                                    self.song = None;
                                    self.music = None;
                                    self.score = 0;
                                    self.player.stop_music().await;
                                } else {
                                    action_tx.send(Action::Quit)?;
                                }
                            }
                            KeyEvent {
                                code: KeyCode::Up, ..
                            }
                            | KeyEvent {
                                code: KeyCode::Left,
                                ..
                            } => {
                                if self.song.is_none() {
                                    let selected = self.song_selector.selected().unwrap_or(0);
                                    self.song_selector.select(Some(
                                        (selected + self.songs.len() - 1) % self.songs.len(),
                                    ));
                                } else if self.game.is_none() {
                                    let selected = self.course_selector.selected().unwrap_or(0);
                                    self.course_selector.select(Some(
                                        (selected + self.song.as_ref().unwrap().courses.len() - 1)
                                            % self.song.as_ref().unwrap().courses.len(),
                                    ));
                                }
                            }
                            KeyEvent {
                                code: KeyCode::Down,
                                ..
                            }
                            | KeyEvent {
                                code: KeyCode::Right,
                                ..
                            } => {
                                if self.song.is_none() {
                                    let selected = self.song_selector.selected().unwrap_or(0);
                                    self.song_selector.select(Some(
                                        (selected + self.songs.len() + 1) % self.songs.len(),
                                    ));
                                } else if self.game.is_none() {
                                    let selected = self.course_selector.selected().unwrap_or(0);
                                    self.course_selector.select(Some(
                                        (selected + self.song.as_ref().unwrap().courses.len() + 1)
                                            % self.song.as_ref().unwrap().courses.len(),
                                    ));
                                }
                            }
                            KeyEvent {
                                code: KeyCode::Enter,
                                ..
                            } => {
                                if self.song.is_none() {
                                    let selected = self.song_selector.selected().unwrap_or(0);
                                    let content =
                                        fs::read_to_string(&self.songs[selected].1).unwrap();
                                    let parser = TJAParser::new();
                                    let song = parser.parse(content).map_err(|e| {
                                        io::Error::new(io::ErrorKind::InvalidData, e)
                                    })?;

                                    let fallback_ogg = self.songs[selected].1.with_extension("ogg");
                                    let rel = song.header.wave.clone().unwrap_or(
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
                                } else if self.game.is_none() {
                                    let selected = self.course_selector.selected().unwrap_or(0);
                                    let mut course = self
                                        .song
                                        .as_ref()
                                        .unwrap()
                                        .courses
                                        .get(selected)
                                        .unwrap()
                                        .clone();

                                    let offset =
                                        self.song.as_ref().unwrap().header.offset.unwrap_or(0.0)
                                            as f64;
                                    for note in course.notes.iter_mut() {
                                        if note.variant == TaikoNoteVariant::Don
                                            || note.variant == TaikoNoteVariant::Kat
                                        {
                                            note.start -= note.duration / 2.0;
                                        }
                                        // note.start -= 100.0; // 100ms offset for audio delay
                                        note.start -= offset * 1000.0;
                                    }

                                    let rhythm = Rhythm::new(course.notes.clone());

                                    self.course.replace(course);
                                    self.game.replace(rhythm);
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
                                if self.game.is_some() {
                                    if let Some((note, t)) =
                                        self.game.as_mut().unwrap().hit(TaikoNoteVariant::Don)
                                    {
                                        self.score += if (t - note.duration() / 2.0).abs()
                                            < note.duration() / 3.0
                                        {
                                            self.last_hit = 2;
                                            self.course.clone().unwrap().scoreinit.unwrap_or(1000)
                                        } else {
                                            self.last_hit = 1;
                                            self.course.clone().unwrap().scoreinit.unwrap_or(1000)
                                                / 2
                                        };
                                        self.combo += 1;
                                        self.last_hit_show = 4;
                                    }
                                    if let Some((note, t)) =
                                        self.game.as_mut().unwrap().hit(TaikoNoteVariant::Both)
                                    {
                                        self.score += 100;
                                    }
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
                                if self.game.is_some() {
                                    if let Some((note, t)) =
                                        self.game.as_mut().unwrap().hit(TaikoNoteVariant::Kat)
                                    {
                                        self.score += if (t - note.duration() / 2.0).abs()
                                            < note.duration() / 3.0
                                        {
                                            self.last_hit = 2;
                                            self.course.clone().unwrap().scoreinit.unwrap_or(1000)
                                        } else {
                                            self.last_hit = 1;
                                            self.course.clone().unwrap().scoreinit.unwrap_or(1000)
                                                / 2
                                        };
                                        self.combo += 1;
                                        self.last_hit_show = 4;
                                    }
                                    if let Some((note, t)) =
                                        self.game.as_mut().unwrap().hit(TaikoNoteVariant::Both)
                                    {
                                        self.score += 100;
                                    }
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
                        if self.game.is_some() {
                            let rhythm = self.game.as_mut().unwrap();
                            let notes =
                                rhythm.advance_time(player_time * 1000.0 - rhythm.current_time());
                            for note in notes {
                                if note.variant == TaikoNoteVariant::Don
                                    || note.variant == TaikoNoteVariant::Kat
                                {
                                    self.combo = 0;
                                }
                            }
                            // auto play:
                            // for note in notes {
                            //     match note.variant() {
                            //         0 | 2 => self.player.play_effect(&self.sounds["don"]).await,
                            //         1 => self.player.play_effect(&self.sounds["kat"]).await,
                            //         _ => {}
                            //     }
                            // }
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
                            } else if player_time <= 0.0 {
                                song_name.unwrap().to_string()
                            } else {
                                format!(
                                    "{} | {:.1} secs | {} pts | {} combo",
                                    song_name.unwrap(),
                                    player_time,
                                    self.score,
                                    self.combo
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
                                    .highlight_style(Style::default().add_modifier(Modifier::BOLD));
                                self.song_selector.select(Some(
                                    self.song_selector.selected().unwrap_or(0) % self.songs.len(),
                                ));

                                f.render_stateful_widget(list, chunks[1], &mut self.song_selector);
                            } else if self.game.is_none() {
                                let song = self.song.as_ref().unwrap();
                                let names = song
                                    .courses
                                    .iter()
                                    .map(|course| format!("{}", course.course));
                                let list = List::new(names)
                                    .block(
                                        Block::default()
                                            .borders(Borders::ALL)
                                            .title("Select a Course"),
                                    )
                                    .highlight_style(Style::default().add_modifier(Modifier::BOLD));
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
                                        [Constraint::Length(1), Constraint::Min(1)].as_ref(),
                                    )
                                    .split(chunks[1]);

                                // draw the notes
                                let course = self.course.as_ref().unwrap();
                                let played = if self.playing { player_time } else { 0.0 };
                                let notes = course
                                    .notes
                                    .iter()
                                    .filter_map(|note| {
                                        let x = (note.start
                                            + (if note.variant == TaikoNoteVariant::Both {
                                                0.0
                                            } else {
                                                note.duration / 2.0
                                            })
                                            - played * 1000.0)
                                            / note.speed as f64
                                            * 0.06;

                                        if note.variant == TaikoNoteVariant::Invisible {
                                            None
                                        } else if note.volume == 0 {
                                            None
                                        } else if note.variant == TaikoNoteVariant::Both {
                                            Some((note, x))
                                        } else if x > -0.05 && x <= 1.0 {
                                            Some((note, x))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>();

                                let selected = format!(
                                    "{} {} {:?}",
                                    self.course.as_ref().unwrap().course,
                                    notes.len(),
                                    &notes
                                );
                                let block = Block::default().title(
                                    block::Title::from(selected.dim()).alignment(Alignment::Center),
                                );

                                f.render_widget(block, vertical_chunks[0]);

                                let mut spans: Vec<Span> =
                                    vec![Span::raw(" "); vertical_chunks[1].width as usize];
                                let hit_span = (0.05 * vertical_chunks[1].width as f64) as usize;
                                spans[hit_span] =
                                    Span::styled(" ", Style::default().bg(Color::Green));
                                for (note, x) in &notes {
                                    let x =
                                        ((x + 0.05) * (vertical_chunks[1].width as f64)) as usize;
                                    let color = match note.variant {
                                        TaikoNoteVariant::Don => Color::Red,
                                        TaikoNoteVariant::Kat => Color::Blue,
                                        TaikoNoteVariant::Both => Color::Yellow,
                                        _ => Color::White,
                                    };
                                    if x < vertical_chunks[1].width as usize {
                                        match note.note_type {
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
                                                let end = (((note.start + note.duration
                                                    - played * 1000.0)
                                                    / note.speed as f64
                                                    * 0.06
                                                    + 0.05)
                                                    * (vertical_chunks[1].width as f64))
                                                    as usize;
                                                let mut x = x;
                                                while x < end
                                                    && x < vertical_chunks[1].width as usize
                                                {
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
                                    vec![Span::raw(" "); vertical_chunks[1].width as usize];
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
                                f.render_widget(paragraph, vertical_chunks[1]);
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
    }
    Ok(songs)
}
