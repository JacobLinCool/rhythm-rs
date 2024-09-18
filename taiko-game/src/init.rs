use color_eyre::eyre::Result;
use directories::ProjectDirs;
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use tracing::error;
use tracing_error::ErrorLayer;
use tracing_subscriber::{
    self, prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt, Layer,
};

pub static PROJECT_NAME: OnceCell<String> = OnceCell::new();
pub static LOG_ENV: OnceCell<String> = OnceCell::new();
pub static LOG_FILE: OnceCell<String> = OnceCell::new();

pub fn init() -> Result<()> {
    init_globals();
    init_panic_handler()?;
    init_logging()?;
    Ok(())
}

fn init_globals() {
    PROJECT_NAME
        .set(env!("CARGO_CRATE_NAME").to_uppercase().to_string())
        .unwrap();
    LOG_ENV
        .set(format!("{}_LOGLEVEL", PROJECT_NAME.get().unwrap()))
        .unwrap();
    LOG_FILE
        .set(format!("{}.log", env!("CARGO_PKG_NAME")))
        .unwrap();
}

fn init_panic_handler() -> Result<()> {
    let (panic_hook, eyre_hook) = color_eyre::config::HookBuilder::default()
        .panic_section(format!(
            "This is a bug. Consider reporting it at {}",
            env!("CARGO_PKG_REPOSITORY")
        ))
        .capture_span_trace_by_default(false)
        .display_location_section(false)
        .display_env_section(false)
        .into_hooks();
    eyre_hook.install()?;
    std::panic::set_hook(Box::new(move |panic_info| {
        if let Ok(mut t) = crate::tui::Tui::new() {
            if let Err(r) = t.exit() {
                error!("Unable to exit Terminal: {:?}", r);
            }
        }

        #[cfg(not(debug_assertions))]
        {
            use human_panic::{handle_dump, metadata, print_msg};
            let meta = metadata!();
            let file_path = handle_dump(&meta, panic_info);
            print_msg(file_path, &meta)
                .expect("human-panic: printing error message to console failed");
            eprintln!("{}", panic_hook.panic_report(panic_info)); // prints color-eyre stack trace to stderr
        }
        let msg = format!("{}", panic_hook.panic_report(panic_info));
        log::error!("Error: {}", strip_ansi_escapes::strip_str(msg));

        #[cfg(debug_assertions)]
        {
            // Better Panic stacktrace that is only enabled when debugging.
            better_panic::Settings::auto()
                .most_recent_first(false)
                .lineno_suffix(true)
                .verbosity(better_panic::Verbosity::Full)
                .create_panic_handler()(panic_info);
        }

        std::process::exit(1);
    }));
    Ok(())
}

fn init_logging() -> Result<()> {
    let data_dir = project_directory().data_local_dir().to_path_buf();
    std::fs::create_dir_all(&data_dir)?;
    let log_path = data_dir.join(LOG_FILE.get().unwrap());
    let log_file = std::fs::File::create(log_path)?;

    std::env::set_var(
        "RUST_LOG",
        std::env::var("RUST_LOG")
            .or_else(|_| std::env::var(LOG_ENV.get().unwrap()))
            .unwrap_or_else(|_| format!("{}=info", env!("CARGO_CRATE_NAME"))),
    );
    let file_subscriber = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env());
    tracing_subscriber::registry()
        .with(file_subscriber)
        .with(ErrorLayer::default())
        .init();
    Ok(())
}

pub fn project_directory() -> ProjectDirs {
    if let Some(d) = ProjectDirs::from("cool", "jacoblin", env!("CARGO_PKG_NAME")) {
        d
    } else if let Some(d) = std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .and_then(|h| ProjectDirs::from_path(h.join(format!(".{}", env!("CARGO_PKG_NAME")))))
    {
        d
    } else {
        panic!("Could not determine the user's home directory");
    }
}
