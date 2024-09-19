use std::ops::{Deref, DerefMut};

use color_eyre::eyre::Result;
use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyEvent, MouseEvent,
    },
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend as Backend;
use serde::{Deserialize, Serialize};

pub type IO = std::io::Stdout;
pub fn io() -> IO {
    std::io::stdout()
}
pub type Frame<'a> = ratatui::Frame<'a>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Init,
    Quit,
    Error,
    Closed,
    Tick,
    Render,
    FocusGained,
    FocusLost,
    Paste(String),
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
}

pub struct Tui {
    pub terminal: ratatui::Terminal<Backend<IO>>,
    pub mouse: bool,
    pub paste: bool,
}

impl Tui {
    pub fn new() -> Result<Self> {
        let terminal = ratatui::Terminal::new(Backend::new(io()))?;
        let mouse = false;
        let paste = false;
        Ok(Self {
            terminal,
            mouse,
            paste,
        })
    }

    pub fn mouse(mut self, mouse: bool) -> Self {
        self.mouse = mouse;
        self
    }

    pub fn paste(mut self, paste: bool) -> Self {
        self.paste = paste;
        self
    }

    pub fn enter(&mut self) -> Result<()> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(io(), EnterAlternateScreen, cursor::Hide)?;
        crossterm::execute!(io(), EnableFocusChange)?;
        if self.mouse {
            crossterm::execute!(io(), EnableMouseCapture)?;
        }
        if self.paste {
            crossterm::execute!(io(), EnableBracketedPaste)?;
        }
        Ok(())
    }

    pub fn exit(&mut self) -> Result<()> {
        if crossterm::terminal::is_raw_mode_enabled()? {
            self.flush()?;
            if self.paste {
                crossterm::execute!(io(), DisableBracketedPaste)?;
            }
            if self.mouse {
                crossterm::execute!(io(), DisableMouseCapture)?;
            }
            crossterm::execute!(io(), DisableFocusChange)?;
            crossterm::execute!(io(), LeaveAlternateScreen, cursor::Show)?;
            crossterm::terminal::disable_raw_mode()?;
        }
        Ok(())
    }

    pub fn suspend(&mut self) -> Result<()> {
        self.exit()?;
        #[cfg(not(windows))]
        signal_hook::low_level::raise(signal_hook::consts::signal::SIGTSTP)?;
        Ok(())
    }

    pub fn resume(&mut self) -> Result<()> {
        self.enter()?;
        Ok(())
    }
}

impl Deref for Tui {
    type Target = ratatui::Terminal<Backend<IO>>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl DerefMut for Tui {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        self.exit().unwrap();
    }
}
