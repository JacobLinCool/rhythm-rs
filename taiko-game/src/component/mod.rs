pub mod error;
pub mod game;
pub mod menu;
pub mod topbar;

pub use error::*;
pub use game::*;
pub use menu::*;
use ratatui::layout::Rect;
pub use topbar::*;

use color_eyre::eyre::Result;
use crossterm::event::{KeyEvent, MouseEvent};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    action::Action,
    app::{App, AppGlobalState, Page},
    tui::{Event, Frame},
};

pub(crate) trait Component {
    #[allow(unused_variables)]
    fn new() -> Self;

    #[allow(unused_variables)]
    fn handle(
        &mut self,
        app: &mut AppGlobalState,
        event: Event,
        tx: UnboundedSender<Action>,
    ) -> Result<()> {
        Ok(())
    }

    #[allow(unused_variables)]
    fn render(&mut self, app: &mut AppGlobalState, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        Ok(())
    }

    #[allow(unused_variables)]
    async fn enter(&mut self, app: &mut AppGlobalState) -> Result<()> {
        Ok(())
    }

    #[allow(unused_variables)]
    async fn leave(&mut self, app: &mut AppGlobalState, next: Page) -> Result<()> {
        Ok(())
    }
}
