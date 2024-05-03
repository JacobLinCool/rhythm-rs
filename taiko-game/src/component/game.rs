use crate::{
    action::Action,
    app::{App, AppGlobalState, Page},
    tui::{Event, Frame},
};
use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use kira::tween::Tween;
use ratatui::{
    prelude::*,
    widgets::{
        canvas::{Canvas, Rectangle},
        *,
    },
};
use rhythm_core::Note;
use taiko_core::{
    constant::{COURSE_TYPE, GUAGE_FULL_THRESHOLD, GUAGE_PASS_THRESHOLD, RANGE_GREAT},
    DefaultTaikoEngine, Final, GameSource, Hit, InputState, Judgement, TaikoEngine,
};
use tja::{TaikoNote, TaikoNoteType, TaikoNoteVariant};
use tokio::sync::mpsc::UnboundedSender;

use super::Component;

pub struct GameScreen {
    last_hit: i32,
    last_hit_show: i32,
    hit: Option<Hit>,
    guage_color_change: i32,
    last_hit_type: Option<Hit>,
    hit_show: i32,
    auto_play: Option<Vec<TaikoNote>>,
    auto_play_combo_sleep: u16,
    last_player_time: f64,
    player_frozen: u16,
}

impl Component for GameScreen {
    fn new() -> Self {
        Self {
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
        }
    }

    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(5)].as_ref())
            .split(area);

        let guage_chunk = vertical_chunks[0];
        let game_zone = vertical_chunks[1];

        let difficulty = app.selected_course.as_ref().unwrap().course as usize;
        let level = app.selected_course.as_ref().unwrap().level.unwrap_or(0) as usize;
        let guage_color = if app.output.gauge == 1.0 {
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
        } else if app.output.gauge
            >= (GUAGE_PASS_THRESHOLD[difficulty][level] / GUAGE_FULL_THRESHOLD[difficulty][level])
        {
            Color::Yellow
        } else {
            Color::White
        };

        let guage_splits = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Fill(1), Constraint::Length(4)].as_ref())
            .split(guage_chunk);

        let guage = Canvas::default()
            .paint(|ctx| {
                ctx.draw(&Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: app.output.gauge,
                    height: 1.0,
                    color: guage_color,
                });
            })
            .x_bounds([0.0, 1.0])
            .y_bounds([0.0, 1.0]);
        f.render_widget(guage, guage_splits[0]);

        let soul = Text::styled(
            " 魂",
            Style::default().fg(if app.output.gauge == 1.0 {
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

        let mut spans: Vec<Span> = vec![Span::raw(" "); game_zone.width as usize];
        let hit_span = (0.1 * game_zone.width as f64) as usize;
        spans[hit_span] = Span::styled(" ", Style::default().bg(Color::Green));
        if hit_span > 0 {
            spans[hit_span - 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        if hit_span < game_zone.width as usize - 1 {
            spans[hit_span + 1] = Span::styled(" ", Style::default().bg(hit_color));
        }
        for note in app.output.display.iter() {
            let pos = note.position(app.player_time());
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

        let hit_reflection_color = if self.last_hit_type.is_some() && self.hit_show > 0 {
            self.hit_show -= 1;
            match self.last_hit_type.as_ref().unwrap() {
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

    fn handle(
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
                } => tx.send(Action::Switch(Page::CourseMenu))?,
                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        app.player.play(app.sounds["don"].clone())?;
                        self.hit.replace(Hit::Don);
                        self.last_hit_type.replace(Hit::Don);
                        self.hit_show = app.args.tps as i32 / 40;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' | 'k' | 'l' | ';'
                    | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        app.player.play(app.sounds["kat"].clone())?;
                        self.hit.replace(Hit::Kat);
                        self.last_hit_type.replace(Hit::Kat);
                        self.hit_show = app.args.tps as i32 / 40;
                    }
                    _ => {}
                },
                _ => {}
            },
            Event::Tick => {
                let player_time = app.player_time();
                if self.last_player_time == player_time {
                    self.player_frozen += 1;
                    if self.player_frozen >= app.args.tps / 2 && app.output.finished {
                        tx.send(Action::Switch(Page::GameResult))?;
                    }
                } else {
                    self.player_frozen = 0;
                }
                self.last_player_time = player_time;

                if self.auto_play.is_some() {
                    while let Some(note) = self.auto_play.as_mut().unwrap().first() {
                        if player_time > note.start + note.duration {
                            self.auto_play.as_mut().unwrap().remove(0);
                            continue;
                        }

                        if note.variant == TaikoNoteVariant::Don {
                            if (note.start - player_time).abs() < RANGE_GREAT {
                                app.player.play(app.sounds["don"].clone())?;
                                self.hit.replace(Hit::Don);
                                self.last_hit_type.replace(Hit::Don);
                                self.hit_show = app.args.tps as i32 / 40;
                                self.auto_play.as_mut().unwrap().remove(0);
                            } else {
                                break;
                            }
                        } else if note.variant == TaikoNoteVariant::Kat {
                            if (note.start - player_time).abs() < RANGE_GREAT {
                                app.player.play(app.sounds["kat"].clone())?;
                                self.hit.replace(Hit::Kat);
                                self.last_hit_type.replace(Hit::Kat);
                                self.hit_show = app.args.tps as i32 / 40;
                                self.auto_play.as_mut().unwrap().remove(0);
                            } else {
                                break;
                            }
                        } else if note.variant == TaikoNoteVariant::Both {
                            if player_time > note.start {
                                if self.auto_play_combo_sleep == 0 {
                                    app.player.play(app.sounds["don"].clone())?;
                                    self.hit.replace(Hit::Don);
                                    self.last_hit_type.replace(Hit::Don);
                                    self.hit_show = app.args.tps as i32 / 40;
                                    self.auto_play_combo_sleep = app.args.tps / 20;
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

                app.output = app.taiko.as_mut().unwrap().forward(input);
                if app.output.judgement.is_some() {
                    self.last_hit = match app.output.judgement.unwrap() {
                        Judgement::Great => 1,
                        Judgement::Ok => 2,
                        Judgement::Miss => 3,
                        _ => 0,
                    };
                    self.last_hit_show = 6;
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        app.enter_countdown = app.args.tps as i16 * -3;

        let song = app.selected_song.as_ref().unwrap();

        let selected = app.course_selector.selected().unwrap_or(0);
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

        if app.args.auto {
            self.auto_play.replace(course.notes.clone());
        }

        app.selected_course.replace(course);
        app.taiko.replace(DefaultTaikoEngine::new(source));

        if let Some(mut playing) = app.playing.take() {
            playing.stop(Tween::default())?;
        }
        app.player.pause(Tween::default())?;
        app.playing.replace(app.player.play(song.music().await?)?);

        Ok(())
    }

    async fn leave(&mut self, app: &mut AppGlobalState, next: Page) -> Result<()> {
        if next == Page::CourseMenu {
            app.taiko.take();
            app.selected_course.take();
        }
        if let Some(mut playing) = app.playing.take() {
            playing.stop(Tween::default())?;
        }
        Ok(())
    }
}

pub struct GameResult {
    result: Option<Final>,
}

impl Component for GameResult {
    fn new() -> Self {
        Self { result: None }
    }

    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        if self.result.is_none() {
            return Ok(());
        }

        let result = self.result.as_ref().unwrap();

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

    fn handle(
        &mut self,
        app: &mut AppGlobalState,
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

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        self.result.replace(app.taiko.as_mut().unwrap().finalize());
        Ok(())
    }

    async fn leave(&mut self, app: &mut AppGlobalState, next: Page) -> Result<()> {
        if next == Page::SongMenu {
            app.taiko.take();
            app.selected_course.take();
            app.selected_song.take();
        }
        if let Some(mut playing) = app.playing.take() {
            playing.stop(Tween::default())?;
        }
        Ok(())
    }
}
