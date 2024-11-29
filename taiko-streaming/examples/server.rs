use anyhow::Result;
use taiko_streaming::{StreamingServer, WebSocketStreamingServer};

#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:13030".to_string());

    let server = WebSocketStreamingServer::new(addr)?;
    server.start().await?;

    Ok(())
}
