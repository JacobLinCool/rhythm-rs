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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    None,
    SongMenu,
    CourseMenu,
    Game,
    Result,
}

#[derive(Clone)]
pub struct PageStates {
    pub topbar: TopBar,

    pub page: Page,
    pub songmenu: SongMenuState,
    pub coursemenu: CourseMenuState,
    pub game: GameState,
    pub result: GameResultState,
}

pub enum RendererAction {
    Render(PageStates),
    Resize(u16, u16),
    Exit,
}

pub struct Renderer {
    rx: UnboundedReceiver<RendererAction>,
}

impl Renderer {
    pub fn new(rx: UnboundedReceiver<RendererAction>) -> Result<Self> {
        Ok(Self { rx })
    }

    pub async fn run(&mut self) -> Result<()> {
        let mut tui = Tui::new()?;

        tui.enter()?;

        while let Some(action) = self.rx.recv().await {
            match action {
                RendererAction::Exit => {
                    break;
                }
                RendererAction::Render(state) => {
                    self.render(&mut tui, state)?;
                }
                RendererAction::Resize(w, h) => {
                    tui.resize(Rect::new(0, 0, w, h));
                }
            }
        }

        tui.exit()?;

        Ok(())
    }

    pub fn render(&mut self, tui: &mut Tui, state: PageStates) -> Result<()> {
        tui.draw(|f| {
            let size = f.size();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Fill(size.height - 1)].as_ref())
                .split(size);

            state.topbar.render(f, chunks[0]).unwrap();

            match state.page {
                Page::SongMenu => {
                    SongMenu::render(&state, f, chunks[1]).unwrap();
                }
                Page::CourseMenu => {
                    CourseMenu::render(&state, f, chunks[1]).unwrap();
                }
                Page::Game => {
                    GameScreen::render(&state, f, chunks[1]).unwrap();
                }
                Page::Result => {
                    GameResult::render(&state, f, chunks[1]).unwrap();
                }
                _ => {}
            }
        })?;

        Ok(())
    }
}

pub struct UI {
    pub state: PageStates,
    handle: Option<tokio::task::JoinHandle<()>>,
    action_tx: Option<UnboundedSender<RendererAction>>,
}

impl UI {
    pub fn new() -> Result<Self> {
        let state = PageStates {
            topbar: TopBar::new(),
            page: Page::None,
            songmenu: SongMenuState::new(),
            coursemenu: CourseMenuState::new(),
            game: GameState::new(),
            result: GameResultState::new(),
        };

        Ok(Self {
            state,
            handle: None,
            action_tx: None,
        })
    }

    pub fn render(&mut self) -> Result<()> {
        if let Some(tx) = self.action_tx.as_ref() {
            tx.send(RendererAction::Render(self.state.clone()))?;
        }

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

    pub fn resize(&mut self, w: u16, h: u16) -> Result<()> {
        if let Some(tx) = self.action_tx.as_ref() {
            tx.send(RendererAction::Resize(w, h))?;
        }

        Ok(())
    }

    pub fn enter(&mut self) -> Result<()> {
        if self.handle.is_none() {
            let (tx, rx) = unbounded_channel();
            let mut renderer = Renderer::new(rx)?;
            let handle = tokio::task::spawn(async move {
                renderer.run().await.unwrap();
            });
            self.handle = Some(handle);
            self.action_tx = Some(tx);
        }

        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        if let Some(tx) = self.action_tx.take() {
            tx.send(RendererAction::Exit).unwrap();
        }

        Ok(())
    }
}
