use std::sync::{Arc, Mutex};

use crate::{
    action::Action,
    app::AppGlobalState,
    loader::Song,
    tui::{Event, Frame},
    uix::{Page, PageStates},
};
use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{prelude::*, widgets::*};
use taiko_core::constant::COURSE_TYPE;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone)]
pub struct SongMenuState {
    pub songs: Vec<Song>,
    pub song_selector: Arc<Mutex<ListState>>,
}

impl Default for SongMenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl SongMenuState {
    pub fn load(&mut self, songs: Vec<Song>) {
        self.songs = songs;
    }

    pub fn new() -> Self {
        let mut song_selector = ListState::default();
        song_selector.select(Some(0));
        let song_selector = Arc::new(Mutex::new(song_selector));

        Self {
            songs: vec![],
            song_selector,
        }
    }

    fn schedule_demo(&self, app: &mut AppGlobalState, idx: usize) {
        app.schedule_demo(self.songs[idx].clone());
    }

    fn select_prev(&mut self, app: &mut AppGlobalState) {
        let mut selector = self.song_selector.lock().unwrap();
        let idx = (selector.selected().unwrap_or(0) + self.songs.len() - 1) % self.songs.len();
        selector.select(Some(idx));
        self.schedule_demo(app, idx);
    }

    fn select_next(&mut self, app: &mut AppGlobalState) {
        let mut selector = self.song_selector.lock().unwrap();
        let idx = (selector.selected().unwrap_or(0) + 1) % self.songs.len();
        selector.select(Some(idx));
        self.schedule_demo(app, idx);
    }
}

pub(crate) trait SongMenu {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()>;
    async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()>;
    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()>;
}

impl SongMenu for PageStates {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        let items = self.songmenu.songs.iter().map(|s| {
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

        f.render_stateful_widget(list, area, &mut self.songmenu.song_selector.lock().unwrap());

        Ok(())
    }

    async fn handle(
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
                } => {
                    app.audio.play_effect(app.audio.effects.don()).await?;
                    self.coursemenu.song.replace(
                        self.songmenu.songs[self
                            .songmenu
                            .song_selector
                            .lock()
                            .unwrap()
                            .selected()
                            .unwrap()]
                        .clone(),
                    );
                    tx.send(Action::Switch(Page::CourseMenu))?;
                }

                KeyEvent {
                    code: KeyCode::Left,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    app.audio.play_effect(app.audio.effects.kat()).await?;
                    self.songmenu.select_prev(app);
                }

                KeyEvent {
                    code: KeyCode::Right,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    app.audio.play_effect(app.audio.effects.kat()).await?;
                    self.songmenu.select_next(app);
                }

                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        app.audio.play_effect(app.audio.effects.don()).await?;
                        self.coursemenu.song.replace(
                            self.songmenu.songs[self
                                .songmenu
                                .song_selector
                                .lock()
                                .unwrap()
                                .selected()
                                .unwrap()]
                            .clone(),
                        );
                        tx.send(Action::Switch(Page::CourseMenu))?;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' => {
                        app.audio.play_effect(app.audio.effects.kat()).await?;
                        self.songmenu.select_prev(app);
                    }
                    'k' | 'l' | ';' | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        app.audio.play_effect(app.audio.effects.kat()).await?;
                        self.songmenu.select_next(app);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        let idx = self
            .songmenu
            .song_selector
            .lock()
            .unwrap()
            .selected()
            .unwrap();
        self.songmenu.schedule_demo(app, idx);
        self.topbar.set_default_text();
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct CourseMenuState {
    pub song: Option<Song>,
    pub course_selector: Arc<Mutex<ListState>>,
}

impl Default for CourseMenuState {
    fn default() -> Self {
        Self::new()
    }
}

impl CourseMenuState {
    pub fn new() -> Self {
        let mut course_selector = ListState::default();
        course_selector.select(Some(0));
        let course_selector = Arc::new(Mutex::new(course_selector));

        Self {
            song: None,
            course_selector,
        }
    }

    fn select_prev(&mut self) {
        let mut selector = self.course_selector.lock().unwrap();
        let idx = (selector.selected().unwrap_or(0)
            + self.song.as_ref().unwrap().tja().courses.len()
            - 1)
            % self.song.as_ref().unwrap().tja().courses.len();
        selector.select(Some(idx));
    }

    fn select_next(&mut self) {
        let mut selector = self.course_selector.lock().unwrap();
        let idx = (selector.selected().unwrap_or(0) + 1)
            % self.song.as_ref().unwrap().tja().courses.len();
        selector.select(Some(idx));
    }
}

pub(crate) trait CourseMenu {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()>;
    async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()>;
    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()>;
}

impl CourseMenu for PageStates {
    fn render(&self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        if self.coursemenu.song.is_none() {
            return Ok(());
        }

        let song = self.coursemenu.song.as_ref().unwrap();
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

        f.render_stateful_widget(
            list,
            area,
            &mut self.coursemenu.course_selector.lock().unwrap(),
        );
        Ok(())
    }

    async fn handle(
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
                } => {
                    app.audio.play_effect(app.audio.effects.don()).await?;
                    self.game
                        .song
                        .replace(self.coursemenu.song.as_ref().unwrap().clone());
                    self.game.course.replace(
                        self.coursemenu
                            .song
                            .as_ref()
                            .unwrap()
                            .tja()
                            .courses
                            .get(
                                self.coursemenu
                                    .course_selector
                                    .lock()
                                    .unwrap()
                                    .selected()
                                    .unwrap(),
                            )
                            .unwrap()
                            .clone(),
                    );
                    tx.send(Action::Switch(Page::Game))?;
                }

                KeyEvent {
                    code: KeyCode::Left,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Up, ..
                } => {
                    app.audio.play_effect(app.audio.effects.kat()).await?;
                    self.coursemenu.select_prev();
                }

                KeyEvent {
                    code: KeyCode::Right,
                    ..
                }
                | KeyEvent {
                    code: KeyCode::Down,
                    ..
                } => {
                    app.audio.play_effect(app.audio.effects.kat()).await?;
                    self.coursemenu.select_next();
                }

                KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                } => match c {
                    ' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm' => {
                        app.audio.play_effect(app.audio.effects.don()).await?;
                        self.game
                            .song
                            .replace(self.coursemenu.song.as_ref().unwrap().clone());
                        self.game.course.replace(
                            self.coursemenu
                                .song
                                .as_ref()
                                .unwrap()
                                .tja()
                                .courses
                                .get(
                                    self.coursemenu
                                        .course_selector
                                        .lock()
                                        .unwrap()
                                        .selected()
                                        .unwrap(),
                                )
                                .unwrap()
                                .clone(),
                        );
                        tx.send(Action::Switch(Page::Game))?;
                    }
                    'd' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' => {
                        app.audio.play_effect(app.audio.effects.kat()).await?;
                        self.coursemenu.select_prev();
                    }
                    'k' | 'l' | ';' | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/' => {
                        app.audio.play_effect(app.audio.effects.kat()).await?;
                        self.coursemenu.select_next();
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        Ok(())
    }

    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        if self.page == Page::Game {
            let idx = self
                .songmenu
                .song_selector
                .lock()
                .unwrap()
                .selected()
                .unwrap();
            self.songmenu.schedule_demo(app, idx);
        }

        let tja = self.coursemenu.song.as_ref().unwrap().tja();
        self.topbar.set_song_text(
            tja.header.title.as_ref().unwrap(),
            tja.header.subtitle.as_ref().unwrap(),
        );
        Ok(())
    }
}
