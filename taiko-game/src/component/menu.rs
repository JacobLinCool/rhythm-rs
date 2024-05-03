use crate::{
    action::Action,
    app::{App, AppGlobalState, Page},
    tui::{Event, Frame},
    utils::{select_next, select_prev},
};
use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{prelude::*, widgets::*};
use taiko_core::constant::COURSE_TYPE;
use tokio::sync::mpsc::UnboundedSender;

use super::Component;

pub struct SongMenu {}

impl Component for SongMenu {
    fn new() -> Self {
        Self {}
    }

    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let items = app.songs.as_ref().unwrap().iter().map(|s| {
            let title = Span::styled(
                s.tja().header.title.as_ref().unwrap().to_string(),
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

            Line::from(vec![title, subtitle])
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
        app.song_selector.select(Some(
            app.song_selector.selected().unwrap_or(0) % app.songs.as_ref().unwrap().len(),
        ));

        f.render_stateful_widget(list, area, &mut app.song_selector);

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
                } => tx.send(Action::Quit)?,

                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => tx.send(Action::Switch(Page::CourseMenu))?,

                KeyEvent {
                    code: KeyCode::Left,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    select_prev(&mut app.song_selector, 0..app.songs.as_ref().unwrap().len())?;
                    app.schedule_demo();
                }

                KeyEvent {
                    code: KeyCode::Right,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    select_next(&mut app.song_selector, 0..app.songs.as_ref().unwrap().len())?;
                    app.schedule_demo();
                }

                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        tx.send(Action::Switch(Page::CourseMenu))?;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' => {
                        select_prev(&mut app.song_selector, 0..app.songs.as_ref().unwrap().len())?;
                        app.schedule_demo();
                    }
                    'k' | 'l' | ';' | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        select_next(&mut app.song_selector, 0..app.songs.as_ref().unwrap().len())?;
                        app.schedule_demo();
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        let songs = app.loader.list().await?;
        app.songs.replace(songs);
        Ok(())
    }
}

pub struct CourseMenu {}

impl Component for CourseMenu {
    fn new() -> Self {
        Self {}
    }

    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let song = app.selected_song.as_ref().unwrap();
        let names = song.tja().courses.iter().map(|course| {
            (if course.course < COURSE_TYPE.len() as i32 {
                format!(
                    "{:<8} ({})",
                    COURSE_TYPE[course.course as usize],
                    course.level.unwrap_or(0)
                )
            } else {
                "Unknown".to_owned()
            })
            .to_string()
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
        app.course_selector.select(Some(
            app.course_selector.selected().unwrap_or(0) % song.tja().courses.len(),
        ));

        f.render_stateful_widget(list, area, &mut app.course_selector);

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
                } => tx.send(Action::Switch(Page::SongMenu))?,

                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => tx.send(Action::Switch(Page::GameScreen))?,

                KeyEvent {
                    code: KeyCode::Left,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Up, ..
                } => select_prev(
                    &mut app.course_selector,
                    0..app.selected_song.as_ref().unwrap().tja().courses.len(),
                )?,

                KeyEvent {
                    code: KeyCode::Right,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => select_next(
                    &mut app.course_selector,
                    0..app.selected_song.as_ref().unwrap().tja().courses.len(),
                )?,

                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        tx.send(Action::Switch(Page::GameScreen))?;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' => {
                        select_prev(
                            &mut app.course_selector,
                            0..app.selected_song.as_ref().unwrap().tja().courses.len(),
                        )?;
                    }
                    'k' | 'l' | ';' | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        select_next(
                            &mut app.course_selector,
                            0..app.selected_song.as_ref().unwrap().tja().courses.len(),
                        )?;
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        let selected = app.song_selector.selected().unwrap_or(0);
        let song = app.songs.as_ref().unwrap()[selected].clone();
        app.selected_song.replace(song);
        if app.playing.is_none() {
            app.schedule_demo();
        }
        Ok(())
    }

    async fn leave(&mut self, app: &mut AppGlobalState, next: Page) -> Result<()> {
        if next == Page::SongMenu {
            app.selected_song.take();
        }
        Ok(())
    }
}
