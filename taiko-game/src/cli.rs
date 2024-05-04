use std::path::PathBuf;

use clap::Parser;

use crate::utils::version;

#[derive(Parser, Debug)]
#[command(author, version = version(), about)]
pub struct AppArgs {
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
        default_value_t = 400
    )]
    pub tps: u16,

    #[arg(
        short,
        long,
        value_name = "AUTO",
        help = "Enable auto mode",
        default_value_t = false
    )]
    pub auto: bool,

    #[arg(
        long,
        value_name = "SEVOL",
        help = "The volume of the sound effects",
        default_value_t = 100
    )]
    pub sevol: u8,

    #[arg(
        long,
        value_name = "SONGVOL",
        help = "The volume of the song music",
        default_value_t = 100
    )]
    pub songvol: u8,
}
