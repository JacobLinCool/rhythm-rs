#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]

pub mod action;
pub mod app;
pub mod assets;
pub mod cli;
pub mod component;
pub mod latency;
pub mod loader;
pub mod tui;
pub mod utils;

use clap::Parser;
use cli::AppArgs;
use color_eyre::eyre::Result;

use crate::{
    app::App,
    utils::{initialize_logging, initialize_panic_handler, version},
};

async fn tokio_main() -> Result<()> {
    initialize_logging()?;

    initialize_panic_handler()?;

    let args = AppArgs::parse();
    let mut app = App::new(args)?;
    app.run().await?;

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    if let Err(e) = tokio_main().await {
        eprintln!(
            "{} error: Application failed to start",
            env!("CARGO_PKG_NAME")
        );
        Err(e)
    } else {
        Ok(())
    }
}
