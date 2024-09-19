use crate::{
    action::Action,
    app::AppGlobalState,
    component::{
        Component, CourseMenu, CourseMenuState, GameResult, GameResultState, GameScreen, GameState,
        SongMenu, SongMenuState, TopBar,
    },
    tui::{Event, Tui},
};
use color_eyre::eyre::Result;
use ratatui::layout::{Constraint, Direction, Layout};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    None,
    SongMenu,
    CourseMenu,
    Game,
    Result,
}

pub struct PageStates {
    pub topbar: TopBar,

    pub page: Page,
    pub songmenu: SongMenuState,
    pub coursemenu: CourseMenuState,
    pub game: GameState,
    pub result: GameResultState,
}

pub struct UI {
    pub tui: Tui,
    pub state: PageStates,
}

impl UI {
    pub fn new() -> Result<Self> {
        Ok(Self {
            tui: Tui::new()?,
            state: PageStates {
                topbar: TopBar::new(),
                page: Page::None,
                songmenu: SongMenuState::new(),
                coursemenu: CourseMenuState::new(),
                game: GameState::new(),
                result: GameResultState::new(),
            },
        })
    }

    pub fn render(&mut self) -> Result<()> {
        self.tui.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Fill(size.height - 1)].as_ref())
                .split(size);

            self.state.topbar.render(f, chunks[0]).unwrap();

            match self.state.page {
                Page::SongMenu => {
                    SongMenu::render(&self.state, f, chunks[1]).unwrap();
                }
                Page::CourseMenu => {
                    CourseMenu::render(&self.state, f, chunks[1]).unwrap();
                }
                Page::Game => {
                    GameScreen::render(&self.state, f, chunks[1]).unwrap();
                }
                Page::Result => {
                    GameResult::render(&self.state, f, chunks[1]).unwrap();
                }
                _ => {}
            }
        })?;

        Ok(())
    }

    pub async fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()> {
        match self.state.page {
            Page::SongMenu => {
                SongMenu::handle(&mut self.state, app, event, tx).await?;
            }
            Page::CourseMenu => {
                CourseMenu::handle(&mut self.state, app, event, tx).await?;
            }
            Page::Game => {
                GameScreen::handle(&mut self.state, app, event, tx).await?;
            }
            Page::Result => {
                GameResult::handle(&mut self.state, app, event, tx).await?;
            }
            _ => {}
        };

        Ok(())
    }

    pub async fn switch_page(&mut self, app: &mut AppGlobalState, page: Page) -> Result<()> {
        match page {
            Page::SongMenu => {
                SongMenu::enter(&mut self.state, app).await?;
            }
            Page::CourseMenu => {
                CourseMenu::enter(&mut self.state, app).await?;
            }
            Page::Game => {
                GameScreen::enter(&mut self.state, app).await?;
            }
            Page::Result => {
                GameResult::enter(&mut self.state, app).await?;
            }
            _ => {}
        };
        self.state.page = page;
        Ok(())
    }

    pub fn enter(&mut self) -> Result<()> {
        self.tui.enter()?;
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        self.tui.exit()
    }
}
