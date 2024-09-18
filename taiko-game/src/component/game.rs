use crate::{
    action::Action,
    app::AppGlobalState,
    loader::Song,
    tui::{Event, Frame},
    uix::{Page, PageStates},
};
use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kira::sound::static_sound::StaticSoundSettings;
use ratatui::{
    prelude::*,
    widgets::{
        canvas::{Canvas, Rectangle}, Block, Borders, Cell, Paragraph, Row, Table
    },
};
use rhythm_core::note::Note;
use taiko_core::{
    constant::{COURSE_TYPE, GUAGE_FULL_THRESHOLD, GUAGE_PASS_THRESHOLD},
    DefaultTaikoEngine, Final, GameSource, Hit, InputState, Judgement, OutputState, TaikoEngine,
};
use tja::{TJACourse, TaikoNote, TaikoNoteType, TaikoNoteVariant};
use tokio::sync::mpsc::UnboundedSender;

pub struct GameState {
    pub song: Option<Song>,
    pub course: Option<TJACourse>,
    taiko: Option<DefaultTaikoEngine>,
    output: OutputState,
    last_hit: i32,
    last_hit_show: i32,
    hit: Option<Hit>,
    guage_color_change: i32,
    last_hit_type: Option<Hit>,
    hit_show: i32,
    auto_play: Option<Vec<TaikoNote>>,
    auto_play_combo_sleep: u64,
    last_player_time: f64,
    player_frozen: u64,
    enter_countdown: i32,
    guage_color: Color,
    song_title: String,
}

impl Default for GameState {
    fn default() -> Self {
        Self::new()
    }
}

impl GameState {
    pub fn new() -> Self {
        Self {
            song: None,
            course: None,
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
            last_hit: 0,
            last_hit_show: 0,
            hit: None,
            guage_color_change: 0,
            last_hit_type: None,
            hit_show: 0,
            auto_play: None,
            auto_play_combo_sleep: 0,
            last_player_time: 0.0,
            player_frozen: 0,
            enter_countdown: 0,
            guage_color: Color::White,
            song_title: String::new(),
        }
    }
}

pub(crate) trait GameScreen {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()>;
    async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()>;
    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()>;
}

impl GameScreen for PageStates {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        if self.game.course.is_none() || self.game.song.is_none() {
            return Ok(());
        }

        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(5)].as_ref())
            .split(area);

        let guage_chunk = vertical_chunks[0];
        let game_zone = vertical_chunks[1];

        let guage_splits = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Fill(1), Constraint::Length(4)].as_ref())
            .split(guage_chunk);

        let guage = Canvas::default()
            .paint(|ctx| {
                ctx.draw(&Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: self.game.output.gauge,
                    height: 1.0,
                    color: self.game.guage_color,
                });
            })
            .x_bounds([0.0, 1.0])
            .y_bounds([0.0, 1.0]);
        f.render_widget(guage, guage_splits[0]);

        let soul = Text::styled(
            " 魂",
            Style::default().fg(if self.game.output.gauge == 1.0 {
                self.game.guage_color
            } else {
                Color::Black
            }),
        );
        f.render_widget(soul, guage_splits[1]);

        let hit_color = if self.game.last_hit_show == 0 {
            Color::Black
        } else {
            match self.game.last_hit {
                1 => Color::Yellow,
                2 => Color::White,
                3 => Color::Blue,
                _ => Color::Black,
            }
        };

        let mut spans: Vec<Span> = vec![Span::raw(" "); game_zone.width as usize];
        let hit_span = (0.1 * game_zone.width as f64) as usize;
        spans[hit_span] = Span::styled(" ", Style::default().bg(Color::Green));
        if hit_span > 0 {
            spans[hit_span - 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        if hit_span < game_zone.width as usize - 1 {
            spans[hit_span + 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        for note in self.game.output.display.iter() {
            let pos = note.position(self.game.last_player_time);
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
                        spans[x] = Span::styled("o", Style::default().bg(color));
                    }
                    TaikoNoteType::Big => {
                        spans[x] = Span::styled("O", Style::default().bg(color));
                    }
                    TaikoNoteType::SmallCombo
                    | TaikoNoteType::BigCombo
                    | TaikoNoteType::Balloon
                    | TaikoNoteType::Yam => {
                        let end = (end * (game_zone.width as f64)) as usize;
                        let mut x = x;
                        while x < end && x < game_zone.width as usize {
                            spans[x] = Span::styled(" ", Style::default().bg(color));
                            x += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        let note_line = Line::from(spans);

        let hit_reflection_color = if self.game.last_hit_type.is_some() && self.game.hit_show > 0 {
            match self.game.last_hit_type.as_ref().unwrap() {
                Hit::Don => Color::Red,
                Hit::Kat => Color::Cyan,
            }
        } else {
            Color::White
        };

        let mut spans: Vec<Span> = vec![Span::raw(" "); game_zone.width as usize];
        spans[hit_span] =
            Span::styled("|", Style::default().fg(hit_reflection_color).bg(hit_color));
        if hit_span > 0 {
            spans[hit_span - 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        if hit_span < game_zone.width as usize - 1 {
            spans[hit_span + 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        let hit_line = Line::from(spans);

        let paragraph = Paragraph::new(vec![hit_line.clone(), note_line, hit_line])
            .block(Block::default().borders(Borders::ALL));
        f.render_widget(paragraph, game_zone);

        Ok(())
    }

    async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()> {
        match event {
            Event::Key(e) => match e {
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Esc, ..
                } => {
                    app.audio.stop().await?;
                    tx.send(Action::Switch(Page::CourseMenu))?
                }
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        app.audio.play_effect(app.audio.effects.don()).await?;
                        self.game.hit.replace(Hit::Don);
                        self.game.last_hit_type.replace(Hit::Don);
                        self.game.hit_show = app.args.tps as i32 / 4;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' | 'k' | 'l' | ';'
                    | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        app.audio.play_effect(app.audio.effects.kat()).await?;
                        self.game.hit.replace(Hit::Kat);
                        self.game.last_hit_type.replace(Hit::Kat);
                        self.game.hit_show = app.args.tps as i32 / 4;
                    }
                    _ => {}
                },
                _ => {}
            },
            Event::Tick => {
                if self.game.enter_countdown < 0 {
                    self.game.enter_countdown += 1;
                } else if self.game.enter_countdown == 0 {
                    app.audio.resume().await?;
                    self.game.enter_countdown = 1;
                }

                let player_time = if self.game.enter_countdown <= 0 {
                    self.game.enter_countdown as f64 / app.args.tps as f64
                } else if let Some(pos) = app.audio.playing_time() {
                    pos
                } else {
                    0.0
                };

                if self.game.last_player_time == player_time {
                    self.game.player_frozen += 1;
                    if self.game.player_frozen >= app.args.tps / 2 {
                        app.audio.stop().await?;
                        let result = self.game.taiko.as_ref().unwrap().finalize();
                        self.result.result.replace(result);
                        tx.send(Action::Switch(Page::Result))?;
                    }
                } else {
                    self.game.player_frozen = 0;
                }
                self.game.last_player_time = player_time;

                if self.game.auto_play.is_some() {
                    while let Some(note) = self.game.auto_play.as_mut().unwrap().first() {
                        if player_time > note.start + note.duration {
                            self.game.auto_play.as_mut().unwrap().remove(0);
                            continue;
                        }

                        if note.variant == TaikoNoteVariant::Don {
                            if (note.start - player_time) < 0.02
                                && (player_time - note.start) < 0.05
                            {
                                app.audio.play_effect(app.audio.effects.don()).await?;
                                self.game.hit.replace(Hit::Don);
                                self.game.last_hit_type.replace(Hit::Don);
                                self.game.hit_show = app.args.tps as i32 / 4;
                                self.game.auto_play.as_mut().unwrap().remove(0);
                            } else {
                                break;
                            }
                        } else if note.variant == TaikoNoteVariant::Kat {
                            if (note.start - player_time) < 0.02
                                && (player_time - note.start) < 0.05
                            {
                                app.audio.play_effect(app.audio.effects.kat()).await?;
                                self.game.hit.replace(Hit::Kat);
                                self.game.last_hit_type.replace(Hit::Kat);
                                self.game.hit_show = app.args.tps as i32 / 4;
                                self.game.auto_play.as_mut().unwrap().remove(0);
                            } else {
                                break;
                            }
                        } else if note.variant == TaikoNoteVariant::Both {
                            if player_time > note.start {
                                if self.game.auto_play_combo_sleep == 0 {
                                    app.audio.play_effect(app.audio.effects.don()).await?;
                                    self.game.hit.replace(Hit::Don);
                                    self.game.last_hit_type.replace(Hit::Don);
                                    self.game.hit_show = app.args.tps as i32 / 4;
                                    self.game.auto_play_combo_sleep = app.args.tps / 20;
                                } else {
                                    self.game.auto_play_combo_sleep -= 1;
                                }
                                break;
                            } else {
                                break;
                            }
                        } else {
                            self.game.auto_play.as_mut().unwrap().remove(0);
                        }
                    }
                }

                let input: InputState<Hit> = InputState {
                    time: player_time,
                    hit: self.game.hit.take(),
                };

                self.game.output = self.game.taiko.as_mut().unwrap().forward(input);
                if self.game.output.judgement.is_some() {
                    self.game.last_hit = match self.game.output.judgement.unwrap() {
                        Judgement::Great => 1,
                        Judgement::Ok => 2,
                        Judgement::Miss => 3,
                        _ => 0,
                    };
                    self.game.last_hit_show = app.args.tps as i32 / 10;
                }

                let course = self.game.course.as_ref().unwrap();
                let difficulty = course.course as usize;
                let level = course.level.unwrap_or(0) as usize;
                let interval = app.args.tps as i32 / 3;
                self.game.guage_color = if self.game.output.gauge == 1.0 {
                    self.game.guage_color_change += 1;
                    if self.game.guage_color_change >= interval {
                        self.game.guage_color_change = 0;
                    }
                    if self.game.guage_color_change >= interval * 3 / 4 {
                        Color::Cyan
                    } else if self.game.guage_color_change >= interval * 2 / 4 {
                        Color::Yellow
                    } else if self.game.guage_color_change >= interval / 4 {
                        Color::Green
                    } else {
                        Color::White
                    }
                } else if self.game.output.gauge
                    >= (GUAGE_PASS_THRESHOLD[difficulty][level]
                        / GUAGE_FULL_THRESHOLD[difficulty][level])
                {
                    Color::Yellow
                } else {
                    Color::White
                };

                if self.game.last_hit_show > 0 {
                    self.game.last_hit_show -= 1;
                }
                if self.game.last_hit_type.is_some() && self.game.hit_show > 0 {
                    self.game.hit_show -= 1;
                }

                self.topbar.set_game_text(
                    &self.game.song_title,
                    COURSE_TYPE[course.course as usize],
                    player_time,
                    self.game.output.score,
                    self.game.output.current_combo,
                    self.game.output.max_combo,
                );
            }
            _ => {}
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        let song = self.game.song.as_ref().unwrap();
        let course = self.game.course.as_mut().unwrap();

        let offset = song.tja().header.offset.unwrap_or(0.0) as f64;
        for note in course.notes.iter_mut() {
            note.start -= offset;
            note.start += app.args.track_offset;
        }

        let source = GameSource {
            difficulty: course.course as u8,
            level: course.level.unwrap_or(0) as u8,
            scoreinit: course.scoreinit,
            scorediff: course.scorediff,
            notes: course.notes.clone(),
        };
        self.game.taiko.replace(DefaultTaikoEngine::new(source));

        if app.args.auto {
            self.game.auto_play.replace(course.notes.clone());
        } else {
            self.game.auto_play.take();
        }

        if let Some(token) = &app.schedule_cancellation {
            token.cancel();
        }

        if app.audio.is_playing() {
            app.audio.stop().await?;
        }

        let settings = StaticSoundSettings::new().volume(app.args.songvol);
        app.audio
            .play(song.music().await?.with_settings(settings))
            .await?;
        app.audio.pause().await?;

        self.game.enter_countdown = app.args.tps as i32 * -3;

        self.game.song_title = song.tja().header.title.clone().unwrap();

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct GameResultState {
    result: Option<Final>,
}

impl Default for GameResultState {
    fn default() -> Self {
        Self::new()
    }
}

impl GameResultState {
    pub fn new() -> Self {
        Self { result: None }
    }
}

pub(crate) trait GameResult {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()>;
    async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()>;
    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()>;
}

impl GameResult for PageStates {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        if self.result.result.is_none() {
            return Ok(());
        }

        let result = self.result.result.as_ref().unwrap();

        let table = Table::new(
            vec![
                Row::new(vec![
                    Cell::from("Score"),
                    Cell::from(format!("{}", result.score)),
                    Cell::from("Max Combo"),
                    Cell::from(format!("{}", result.max_combo)),
                ]),
                Row::new(vec![
                    Cell::from("Great"),
                    Cell::from(format!("{}", result.greats)),
                    Cell::from("Good"),
                    Cell::from(format!("{}", result.goods)),
                ]),
                Row::new(vec![
                    Cell::from("Miss"),
                    Cell::from(format!("{}", result.misses)),
                    Cell::from("魂"),
                    Cell::from(format!("{:.1}%", result.gauge * 100.0)).style(Style::default().fg(
                        if result.passed {
                            Color::Yellow
                        } else {
                            Color::White
                        },
                    )),
                ]),
            ],
            vec![
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Fill(1),
                Constraint::Fill(1),
            ],
        );

        f.render_widget(table, area);

        Ok(())
    }

    async fn handle(
        &mut self,
        _app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()> {
        if let Event::Key(e) = event {
            match e {
                KeyEvent {
                    code: KeyCode::Char('c'),
                    modifiers: KeyModifiers::CONTROL,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Esc, ..
                }
                | KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => tx.send(Action::Switch(Page::SongMenu))?,

                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        tx.send(Action::Switch(Page::SongMenu))?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }

    async fn enter(&mut self, _app: &mut AppGlobalState) -> Result<()> {
        let tja = self.game.song.as_ref().unwrap().tja();
        self.topbar.set_text(format!(
            "{} ({})",
            tja.header.title.as_ref().unwrap(),
            COURSE_TYPE[self.game.course.as_ref().unwrap().course as usize]
        ));
        Ok(())
    }
}
