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
        default_value_t = 500
    )]
    pub tps: u64,

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
        default_value_t = 1.0
    )]
    pub sevol: f64,

    #[arg(
        long,
        value_name = "SONGVOL",
        help = "The volume of the song music",
        default_value_t = 1.0
    )]
    pub songvol: f64,

    #[arg(
        long,
        value_name = "TRACK_OFFSET",
        help = "The track offset of the game, this is used to adjust the timing of the notes, if the notes are too early, increase this value, if the notes are too late, decrease this value. The unit is in seconds.",
        default_value_t = 0.0
    )]
    pub track_offset: f64,

    #[arg(
        long,
        value_name = "LATENCY_GATE",
        help = "The latency gate of the game, if the latency is higher than this value, the game will panic",
        default_value_t = 10000
    )]
    pub latency_gate: u16,

    #[arg(
        long,
        value_name = "ECO",
        help = "Enable eco mode. In the ECO mode, the CPU usage will be about 10% of the normal mode, however, latency will be higher in some cases.",
        default_value_t = false
    )]
    pub eco: bool,

    #[arg(long, value_name = "ADDR", help = "Host a multiplayer game")]
    pub host: Option<String>,

    #[arg(long, value_name = "ADDR", help = "Connect to a multiplayer game")]
    pub connect: Option<String>,
}
