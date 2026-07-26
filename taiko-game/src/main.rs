mod app;
mod audio;
mod audio_sync;
#[cfg(test)]
mod bench;
mod cli;
mod clipboard;
mod controller;
mod controller_qr;
mod demo_preview;
mod drum_surface;
mod embedded_server_start;
mod input;
mod invite;
mod lan_controller;
mod latest_background;
mod library_loading;
mod loader;
mod local_multiplayer;
mod localization;
mod macos_trackpad;
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
#[cfg(unix)]
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    vec::Vec,
};

use anyhow::Result;
use app::App;
use clap::Parser;
use crossterm::event::KeyEventKind;
#[cfg(unix)]
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGTERM},
    flag, low_level, SigId,
};
use tui::{Tui, UiEvent};

use crate::cli::{CacheAction, CacheClearArgs, CacheCommandArgs, Cli, CliSubcommand};
use crate::macos_trackpad::{MacTrackpad, MacTrackpadHit};
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
    let shutdown_signal = ShutdownSignal::install()?;
    let mut tui = Tui::new(app.args.tps, 120)?;
    let mut mac_trackpad = MacTrackpad::open();
    app.set_mac_trackpad_availability(mac_trackpad.availability());
    tui.enter()?;
    app.set_keyboard_repeat_capability(tui.keyboard_repeat_is_distinguishable());

    let run_result = (|| {
        loop {
            if app.should_quit() || shutdown_signal.is_requested() {
                break;
            }

            mac_trackpad.set_target(app.mac_trackpad_capture_target())?;
            tui.set_mouse_capture(app.terminal_pointer_capture_requested())?;
            let event = tui.next_event()?;
            let trackpad = mac_trackpad.drain()?;
            app.record_mac_trackpad_drops(trackpad.dropped);
            if let Some(event_time) = ui_event_observed_at(event) {
                let split = trackpad
                    .hits
                    .partition_point(|hit| hit.observed_at <= event_time);
                dispatch_mac_trackpad_hits(app, &trackpad.hits[..split]);
                handle_ui_event(app, &mut tui, event)?;
                mac_trackpad.set_target(app.mac_trackpad_capture_target())?;
                dispatch_mac_trackpad_hits(app, &trackpad.hits[split..]);
            } else {
                dispatch_mac_trackpad_hits(app, &trackpad.hits);
                handle_ui_event(app, &mut tui, event)?;
                mac_trackpad.set_target(app.mac_trackpad_capture_target())?;
            }
        }
        Ok(())
    })();

    let trackpad_shutdown = mac_trackpad.shutdown();
    let input_result = merge_results(run_result, trackpad_shutdown, "Mac trackpad input shutdown");
    merge_results(input_result, tui.exit(), "terminal shutdown")
}

fn ui_event_observed_at(event: UiEvent) -> Option<Instant> {
    match event {
        UiEvent::Key { observed_at, .. } | UiEvent::Pointer { observed_at, .. } => {
            Some(observed_at)
        }
        UiEvent::Tick | UiEvent::Frame | UiEvent::Resize(_, _) => None,
    }
}

fn dispatch_mac_trackpad_hits(app: &mut App, hits: &[MacTrackpadHit]) {
    for hit in hits.iter().copied() {
        app.handle_mac_trackpad_hit(hit);
    }
}

fn handle_ui_event(app: &mut App, tui: &mut Tui, event: UiEvent) -> Result<()> {
    match event {
        UiEvent::Tick => app.handle_tick(),
        UiEvent::Frame => {
            let start = Instant::now();
            tui.draw(|frame| app.render(frame))?;
            app.commit_rendered_pointer_surface();
            app.record_frame_time(start.elapsed());
        }
        UiEvent::Key { event, observed_at } => {
            if is_physical_key_press(event.kind) {
                app.handle_key_at(event, observed_at);
            }
        }
        UiEvent::Pointer { event, observed_at } => {
            app.handle_pointer_at(event, observed_at);
        }
        UiEvent::Resize(width, height) => {
            app.invalidate_pointer_surface();
            tui.resize(ratatui::layout::Rect::new(0, 0, width, height))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
struct ShutdownSignal {
    requested: Arc<AtomicBool>,
    registrations: Vec<SigId>,
}

#[cfg(unix)]
impl ShutdownSignal {
    fn install() -> Result<Self> {
        let requested = Arc::new(AtomicBool::new(false));
        let registrations = [SIGHUP, SIGINT, SIGTERM]
            .into_iter()
            .map(|signal| flag::register(signal, Arc::clone(&requested)))
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Self {
            requested,
            registrations,
        })
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for ShutdownSignal {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            low_level::unregister(registration);
        }
    }
}

#[cfg(not(unix))]
struct ShutdownSignal;

#[cfg(not(unix))]
impl ShutdownSignal {
    fn install() -> Result<Self> {
        Ok(Self)
    }

    const fn is_requested(&self) -> bool {
        false
    }
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
    use super::{is_physical_key_press, ShutdownSignal};
    use crossterm::event::KeyEventKind;

    #[test]
    fn gameplay_does_not_turn_os_key_repeat_into_drum_hits() {
        assert!(is_physical_key_press(KeyEventKind::Press));
        assert!(!is_physical_key_press(KeyEventKind::Repeat));
        assert!(!is_physical_key_press(KeyEventKind::Release));
    }

    #[cfg(unix)]
    #[test]
    fn shutdown_signal_flag_is_observed_without_running_code_in_the_handler() {
        let shutdown = ShutdownSignal {
            requested: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            registrations: Vec::new(),
        };
        assert!(!shutdown.is_requested());
        shutdown
            .requested
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(shutdown.is_requested());
    }
}
