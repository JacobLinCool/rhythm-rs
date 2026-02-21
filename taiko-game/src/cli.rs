use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BranchPolicy {
    Auto,
    Accuracy,
    Roll,
    Score,
    FixedRoute,
    None,
}

#[derive(Debug, Parser)]
#[command(author, version, about = "Playable TUI taiko game")]
pub struct CliArgs {
    #[arg(
        long,
        value_name = "PATH",
        default_value = "./taiko-game/songs",
        help = "Song directory; scanned recursively for .tja"
    )]
    pub songdir: PathBuf,

    #[arg(
        long,
        value_name = "N",
        default_value_t = 240,
        help = "Logic ticks per second"
    )]
    pub tps: u32,

    #[arg(
        long,
        default_value_t = 0.0,
        help = "Initial note offset in seconds (positive delays notes; adjustable in Course Menu)"
    )]
    pub track_offset: f64,

    #[arg(
        long,
        action = clap::ArgAction::Set,
        default_value_t = true,
        help = "Enable song demo preview in song/course menu"
    )]
    pub demo: bool,

    #[arg(
        long,
        default_value_t = 100,
        value_parser = clap::value_parser!(u8).range(0..=100),
        help = "Song volume percentage"
    )]
    pub songvol: u8,

    #[arg(
        long,
        default_value_t = 100,
        value_parser = clap::value_parser!(u8).range(0..=100),
        help = "SE volume percentage"
    )]
    pub sevol: u8,
}
