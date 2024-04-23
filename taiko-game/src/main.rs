#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_variables)]

pub mod action;
pub mod app;
pub mod assets;
pub mod cli;
pub mod sound;
pub mod tui;
pub mod utils;

use clap::Parser;
use cli::Cli;
use color_eyre::eyre::Result;

use crate::{
    app::App,
    utils::{initialize_logging, initialize_panic_handler, version},
};

async fn tokio_main() -> Result<()> {
    initialize_logging()?;

    initialize_panic_handler()?;

    let args = Cli::parse();
    let mut app = App::new(args.songdir, args.fps)?;
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
