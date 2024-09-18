pub mod action;
pub mod app;
pub mod cli;
pub mod component;
pub mod latency;
pub mod loader;
pub mod store;
pub mod tui;
pub mod utils;

pub mod audio;
pub mod init;
pub mod input;
pub mod sound_effect;
pub mod uix;

use clap::Parser;
use color_eyre::eyre::Result;

use crate::{app::App, cli::AppArgs};

#[tokio::main]
async fn main() -> Result<()> {
    init::init()?;

    let args = AppArgs::parse();
    let mut app = App::new(args).await?;
    app.run().await?;

    Ok(())
}
