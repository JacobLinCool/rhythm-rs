use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

pub(crate) const MIN_TPS: u32 = 60;
pub(crate) const MAX_TPS: u32 = 1_000;

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
        global = true,
        default_value_t = false,
        help = "Use memory-only cache for remote or multiplayer authority resources (disable app-data disk cache)"
    )]
    pub resource_cache_memory_only: bool,

    #[arg(
        long,
        value_name = "N",
        default_value_t = 240,
        value_parser = clap::value_parser!(u32).range(MIN_TPS as i64..=MAX_TPS as i64),
        help = "Logic ticks per second (60-1000)"
    )]
    pub tps: u32,

    #[arg(
        long,
        value_name = "MS",
        default_value_t = 0,
        allow_hyphen_values = true,
        value_parser = clap::value_parser!(i32).range(-500..=500),
        help = "Initial input-to-chart calibration in milliseconds; adjustable in Settings"
    )]
    pub calibration_offset_ms: i32,

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

#[derive(Debug, Parser)]
#[command(author, version, about = "Playable TUI taiko game")]
pub struct Cli {
    #[command(subcommand)]
    pub subcommand: Option<CliSubcommand>,

    #[command(flatten)]
    pub args: CliArgs,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn player_multiplayer_is_not_a_cli_subcommand() {
        assert!(Cli::try_parse_from(["taiko", "multiplayer"]).is_err());
    }

    #[test]
    fn calibration_offset_is_explicit_milliseconds_with_strict_bounds() {
        let parsed = Cli::try_parse_from(["taiko", "--calibration-offset-ms", "-125"])
            .expect("valid calibration");
        assert_eq!(parsed.args.calibration_offset_ms, -125);
        assert!(Cli::try_parse_from(["taiko", "--calibration-offset-ms", "501"]).is_err());
        assert!(Cli::try_parse_from(["taiko", "--track-offset", "0.125"]).is_err());
    }

    #[test]
    fn scheduler_rate_has_strict_practical_bounds() {
        assert_eq!(
            Cli::try_parse_from(["taiko", "--tps", "60"])
                .expect("minimum TPS")
                .args
                .tps,
            MIN_TPS
        );
        assert_eq!(
            Cli::try_parse_from(["taiko", "--tps", "1000"])
                .expect("maximum TPS")
                .args
                .tps,
            MAX_TPS
        );
        assert!(Cli::try_parse_from(["taiko", "--tps", "59"]).is_err());
        assert!(Cli::try_parse_from(["taiko", "--tps", "1001"]).is_err());
    }

    #[test]
    fn slowest_supported_tick_is_well_inside_the_lan_input_freshness_window() {
        let slowest_tick = std::time::Duration::from_secs_f64(1.0 / f64::from(MIN_TPS));
        assert!(
            slowest_tick < crate::lan_controller::MAX_DISPATCH_AGE,
            "the minimum TPS must not make a fresh phone hit expire before its first logic tick"
        );
    }
}
