use std::path::PathBuf;

use clap::Parser;

use crate::utils::version;

#[derive(Parser, Debug)]
#[command(author, version = version(), about)]
pub struct Cli {
    #[arg(
        short,
        long,
        value_name = "PATH",
        help = "Path to the song directory",
        default_value = "./songs"
    )]
    pub songdir: PathBuf,

    #[arg(
        short,
        long,
        value_name = "TICK_RATE",
        help = "The tick rate of the game",
        default_value_t = 200
    )]
    pub fps: u8,
}
