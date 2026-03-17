use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BranchPolicy {
    Auto,
    Accuracy,
    Roll,
    Score,
    FixedRoute,
    None,
}

#[derive(Debug, Clone, Args)]
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
        value_name = "URL",
        help = "Remote resource endpoint base URL; when set, songs/charts/audio are fetched via HTTP"
    )]
    pub resource_endpoint: Option<String>,

    #[arg(
        long,
        default_value_t = false,
        requires = "resource_endpoint",
        help = "Use memory-only cache for remote resources (disable app-data disk cache)"
    )]
    pub resource_cache_memory_only: bool,

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

#[derive(Debug, Subcommand)]
pub enum CliSubcommand {
    /// Run remote resource HTTP server.
    Server(taiko_resource_server::ServerArgs),
    /// Inspect or manage remote resource cache.
    Cache(CacheCommandArgs),
    /// Play online multiplayer or spectate.
    Online(OnlineCommandArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CacheCommandArgs {
    #[command(subcommand)]
    pub action: Option<CacheAction>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum CacheAction {
    /// Print cache root path.
    Path,
    /// Print cache entries and size summary.
    List,
    /// Clear cache entries.
    Clear(CacheClearArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CacheClearArgs {
    #[arg(
        long,
        value_name = "URL",
        conflicts_with = "all",
        required_unless_present = "all",
        help = "Clear cache for one remote endpoint"
    )]
    pub endpoint: Option<String>,

    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "endpoint",
        required_unless_present = "endpoint",
        help = "Clear all endpoint caches"
    )]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct OnlineCommandArgs {
    #[arg(
        long,
        default_value_t = false,
        help = "Run without TUI or audio; log events to stdout and read commands from stdin"
    )]
    pub headless: bool,

    #[command(subcommand)]
    pub action: OnlineAction,
}

#[derive(Debug, Clone, Subcommand)]
pub enum OnlineAction {
    /// Create a new private multiplayer room.
    Create(OnlineCreateArgs),
    /// Join a multiplayer room as player.
    Join(OnlineJoinArgs),
    /// Join a multiplayer room as spectator.
    Spectate(OnlineSpectateArgs),
}

#[derive(Debug, Clone, Args)]
pub struct OnlineCreateArgs {
    #[arg(
        long,
        value_name = "URL",
        help = "Server base URL, e.g. https://example.com"
    )]
    pub server: String,
    #[arg(long, value_name = "NICK", help = "Display name")]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
pub struct OnlineJoinArgs {
    #[arg(
        long,
        value_name = "URL",
        help = "Server base URL, e.g. https://example.com"
    )]
    pub server: String,
    #[arg(long, value_name = "CODE", help = "Room code")]
    pub room: String,
    #[arg(long, value_name = "NICK", help = "Display name")]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
pub struct OnlineSpectateArgs {
    #[arg(
        long,
        value_name = "URL",
        help = "Server base URL, e.g. https://example.com"
    )]
    pub server: String,
    #[arg(long, value_name = "CODE", help = "Room code")]
    pub room: String,
    #[arg(long, value_name = "NICK", help = "Display name")]
    pub name: String,
}

#[derive(Debug, Parser)]
#[command(author, version, about = "Playable TUI taiko game")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Option<CliSubcommand>,

    #[command(flatten)]
    pub args: CliArgs,
}
