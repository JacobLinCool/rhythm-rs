use anyhow::Result;
use clap::Parser;
use taiko_resource_server::{run_server, ServerArgs};

#[derive(Debug, Parser)]
#[command(author, version, about = "Taiko remote resource HTTP server")]
struct Cli {
    #[command(flatten)]
    server: ServerArgs,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    run_server(cli.server)
}
