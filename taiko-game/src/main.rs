mod app;
mod audio;
#[cfg(test)]
mod bench;
mod branch;
mod cli;
mod input;
mod loader;
mod online;
mod perf;
mod resource;
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

use crate::cli::{CacheAction, CacheClearArgs, CacheCommandArgs, Cli, CliSubcommand};
use crate::resource::{
    cache_root_dir, clear_all_remote_cache, clear_remote_cache_for_endpoint, inspect_remote_cache,
};

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(subcommand) = cli.subcommand {
        return match subcommand {
            CliSubcommand::Server(server_args) => taiko_resource_server::run_server(server_args),
            CliSubcommand::Cache(cache_args) => run_cache_command(cache_args),
            CliSubcommand::Online(online_args) => online::run_online_command(online_args),
        };
    }

    let mut app = App::new(cli.args)?;

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

fn run_cache_command(args: CacheCommandArgs) -> Result<()> {
    let action = args.action.unwrap_or(CacheAction::List);
    match action {
        CacheAction::Path => {
            let root = cache_root_dir()?;
            println!("{}", root.display());
        }
        CacheAction::List => {
            let overview = inspect_remote_cache()?;
            println!("cache_root={}", overview.root.display());
            println!("cache_entries={}", overview.entries.len());
            if overview.entries.is_empty() {
                println!("(empty)");
            } else {
                for entry in &overview.entries {
                    println!(
                        "endpoint_hash={} charts={} ({} bytes) audio={} ({} bytes) index_entries={} path={}",
                        entry.endpoint_hash,
                        entry.chart_files,
                        entry.chart_bytes,
                        entry.audio_files,
                        entry.audio_bytes,
                        entry.index_entries,
                        entry.path.display()
                    );
                }
            }
            if !overview.warnings.is_empty() {
                println!("warnings={}", overview.warnings.len());
                for warning in &overview.warnings {
                    println!("- {warning}");
                }
            }
        }
        CacheAction::Clear(clear_args) => run_cache_clear(clear_args)?,
    }

    Ok(())
}

fn run_cache_clear(args: CacheClearArgs) -> Result<()> {
    let result = if args.all {
        clear_all_remote_cache()?
    } else {
        let endpoint = args
            .endpoint
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--endpoint or --all is required"))?;
        clear_remote_cache_for_endpoint(endpoint)?
    };

    println!("removed={}", result.removed_paths.len());
    for path in result.removed_paths {
        println!("removed_path={}", path.display());
    }
    println!("missing={}", result.missing_paths.len());
    for path in result.missing_paths {
        println!("missing_path={}", path.display());
    }

    Ok(())
}
