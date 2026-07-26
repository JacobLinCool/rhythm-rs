mod app;
mod audio;
mod audio_sync;
#[cfg(test)]
mod bench;
mod cli;
mod clipboard;
mod demo_preview;
mod embedded_server_start;
mod input;
mod invite;
mod latest_background;
mod library_loading;
mod loader;
mod local_multiplayer;
mod localization;
mod offline_preparation;
mod online;
mod online_bootstrap;
mod online_preparation;
mod online_session;
#[cfg(test)]
mod online_test_proxy;
mod perf;
mod preferences;
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
        };
    }

    run_app(App::new(cli.args)?)
}

pub(crate) fn run_app(mut app: App) -> Result<()> {
    let run_result = run_app_tui(&mut app);
    let shutdown_result = app.shutdown();
    merge_results(run_result, shutdown_result, "application shutdown")
}

fn run_app_tui(app: &mut App) -> Result<()> {
    let mut tui = Tui::new(app.args.tps, 120)?;
    tui.enter()?;

    let run_result = (|| {
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
                UiEvent::Key { event, observed_at } => {
                    if is_physical_key_press(event.kind) {
                        app.handle_key_at(event, observed_at);
                    }
                }
                UiEvent::Resize(width, height) => {
                    tui.resize(ratatui::layout::Rect::new(0, 0, width, height))?;
                }
            }
        }
        Ok(())
    })();

    merge_results(run_result, tui.exit(), "terminal shutdown")
}

fn is_physical_key_press(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press)
}

fn merge_results(first: Result<()>, second: Result<()>, second_label: &str) -> Result<()> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(anyhow::anyhow!(
            "{first}; {second_label} also failed: {second}"
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::is_physical_key_press;
    use crossterm::event::KeyEventKind;

    #[test]
    fn gameplay_does_not_turn_os_key_repeat_into_drum_hits() {
        assert!(is_physical_key_press(KeyEventKind::Press));
        assert!(!is_physical_key_press(KeyEventKind::Repeat));
        assert!(!is_physical_key_press(KeyEventKind::Release));
    }
}
