pub mod game;
pub mod menu;
pub mod topbar;

pub use game::*;
pub use menu::*;
use ratatui::layout::Rect;
pub use topbar::*;

use color_eyre::eyre::Result;

use crate::tui::Frame;

pub trait Component {
    fn new() -> Self;

    fn render(&self, _f: &mut Frame<'_>, _area: Rect) -> Result<()> {
        Ok(())
    }
}
