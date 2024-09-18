use anyhow::Result;
use taiko_core::{Hit, Personalization};
use taiko_streaming::{generate_uid, StreamingClient, WebSocketStreamingClient};

#[tokio::main]
async fn main() -> Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:13030".to_string());

    let uid = generate_uid();

    let client =
        WebSocketStreamingClient::<Hit, Personalization>::new(addr.clone(), uid.clone()).await?;
    println!("Connected to server at {}", addr);

    // Enable the PONG feature
    let client_clone = client.clone();
    tokio::spawn(async move {
        let _ = client_clone.enable_pong().await;
    });

    // Spawn a task for receiving events
    let mut rx = client.rx().await;
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            if event.0 == uid {
                continue;
            }
            println!("Received event: {:?}", event);
        }
    });

    // Sending events in the main loop
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        interval.tick().await;
        let peers = client
            .collect_peers(std::time::Duration::from_millis(100))
            .await?;
        println!("Peers: {:?}", peers);
        for peer in &peers {
            let latency = client.estimate_latency(peer).await?;
            println!("Estimated latency to {}: {:.2} ms", peer, latency * 1000.0);
            let time_offset = client.estimate_time_offset(peer).await?;
            println!(
                "Estimated time offset to {}: {:.2} ms",
                peer,
                time_offset * 1000.0
            );
        }
    }
}
