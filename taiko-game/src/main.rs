mod app;
mod audio;
#[cfg(test)]
mod bench;
mod branch;
mod cli;
mod input;
mod loader;
mod perf;
mod screen;
mod song_filter;
mod theme;
mod tui;

use std::time::Instant;

use anyhow::Result;
use app::App;
use clap::Parser;
use crossterm::event::KeyEventKind;
use tui::{Tui, UiEvent};

use crate::cli::CliArgs;

fn main() -> Result<()> {
    let args = CliArgs::parse();
    let mut app = App::new(args)?;

    let mut tui = Tui::new(app.args.tps, 120)?;
    tui.enter()?;

    loop {
        if app.should_quit() {
            break;
        }

        match tui.next_event()? {
            UiEvent::Tick => app.handle_tick(),
            UiEvent::Frame => {
                let start = Instant::now();
                tui.draw(|frame| app.render(frame))?;
                app.record_frame_time(start.elapsed());
            }
            UiEvent::Key(key) => {
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    app.handle_key(key);
                }
            }
            UiEvent::Resize(width, height) => {
                tui.resize(ratatui::layout::Rect::new(0, 0, width, height))?;
            }
        }
    }

    tui.exit()?;
    Ok(())
}
