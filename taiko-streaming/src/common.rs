use anyhow::Result;
use async_trait::async_trait;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use taiko_core::InputState;
use tja::TJA;
use tokio::sync::broadcast;

pub type SongHash = String;
pub type SentEvent<H, P> = (String, StreamingEvent<H, P>);

pub trait StreamableData: Clone + std::fmt::Debug + Serialize + for<'de> Deserialize<'de> + Send + Sync {}
impl<T> StreamableData for T where T: Clone + std::fmt::Debug + Serialize + for<'de> Deserialize<'de> + Send + Sync {}

/// `StreamingEvent` represents the types of events that can occur in the streaming session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StreamingEvent<H, P> {
    /// Event for selecting a song.
    /// Contains the TJA string (song metadata and notes) and a hash of the song for uniqueness.
    SongSelect(TJA, SongHash),

    /// Event to request a song's data from another peer if the song is not in the local cache.
    SongRequest(SongHash),

    /// Event to send the raw song data (binary format).
    SongData(SongHash, Vec<u8>),

    /// Event to notify the other peer that the song preparation is complete.
    SongReady(SongHash),

    /// Event to notify that the personalization of a peer is complete.
    Personalized(P),

    /// Event for transmitting input state (like key press).
    Input(InputState<H>),

    /// Event for transmitting a ping request.
    /// The `u32` is a random id for the ping request.
    Ping(u32),

    /// Event for transmitting a ping response.
    /// The `u32` is the random id of the ping request.
    /// The `f64` is the timestamp of the ping response.
    Pong(u32, f64),
}

/// Trait representing a streaming server for managing client connections and game events.
#[async_trait]
pub trait StreamingServer {
    /// Creates a new `StreamingServer` bound to the specified address.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address (IP:PORT) where the server will listen for connections.
    ///
    /// # Returns
    ///
    /// A new instance of `StreamingServer`.
    ///
    /// # Example
    ///
    /// ```rust
    /// let server = StreamingServer::new("127.0.0.1:8080".to_string()).unwrap();
    /// ```
    fn new(addr: String) -> Result<Self>
    where
        Self: Sized;

    /// Starts the server to accept incoming connections and handle events.
    ///
    /// # Returns
    ///
    /// A `Result` that resolves when the server is running or an error occurs.
    ///
    /// # Example
    ///
    /// ```rust
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let server = StreamingServer::new("127.0.0.1:8080".to_string())?;
    ///     server.start().await?;
    ///     Ok(())
    /// }
    /// ```
    async fn start(&self) -> Result<()>;
}

/// Trait representing a client in a streaming session that can send and receive game events.
#[async_trait]
pub trait StreamingClient<
    H: StreamableData,
    P: StreamableData,
>
{
    /// Creates a new `StreamingClient` with the specified address and unique client ID.
    ///
    /// # Arguments
    ///
    /// * `addr` - The address (IP:PORT) of the server to connect to.
    /// * `uid` - A unique identifier for the client.
    ///
    /// # Returns
    ///
    /// A new instance of `StreamingClient`.
    ///
    /// # Example
    ///
    /// ```rust
    /// let client = StreamingClient::new("127.0.0.1:8080".to_string(), "user123".to_string()).await.unwrap();
    /// ```
    async fn new(addr: String, uid: String) -> Result<Self>
    where
        Self: Sized;

    /// Sends an event to the server or another peer in the session.
    ///
    /// # Arguments
    ///
    /// * `event` - The `StreamingEvent` to be sent.
    ///
    /// # Returns
    ///
    /// A `Result` that resolves when the event is successfully sent or an error occurs.
    ///
    /// # Example
    ///
    /// ```rust
    /// #[tokio::main]
    /// async fn main() -> Result<()> {
    ///     let client = StreamingClient::new("127.0.0.1:8080".to_string(), "user123".to_string()).await?;
    ///
    ///     // Send a song selection event
    ///     let tja_string = "TJA data here".to_string();
    ///     let song_hash = "unique_song_hash".to_string();
    ///     client.send(StreamingEvent::SongSelect(tja_string, song_hash)).await?;
    ///
    ///     Ok(())
    /// }
    /// ```
    async fn send(&self, event: StreamingEvent<H, P>) -> Result<()>;

    /// Receives events from the server or another peer in the session.
    /// The returned `broadcast::Receiver` can be used to receive events asynchronously.
    async fn rx(&self) -> broadcast::Receiver<SentEvent<H, P>>;

    /// Returns the unique ID of the client.
    /// This ID is used to identify the client in the session.
    fn uid(&self) -> &str;

    async fn collect_peers(&self, timeout: std::time::Duration) -> Result<HashSet<String>> {
        let id = rand::thread_rng().gen_range(0..=u32::MAX);
        self.send(StreamingEvent::Ping(id)).await?;
        let mut rx = self.rx().await;
        let mut peers = HashSet::new();
        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            match tokio::time::timeout(timeout - start.elapsed(), rx.recv()).await {
                Ok(Ok((uid, _))) => {
                    if uid == self.uid() {
                        continue;
                    }
                    peers.insert(uid);
                }
                _ => break,
            }
        }
        Ok(peers)
    }

    async fn estimate_latency(&self, other: &str) -> Result<f64> {
        let start = std::time::Instant::now();
        let id = rand::thread_rng().gen_range(0..=u32::MAX);
        self.send(StreamingEvent::Ping(id)).await?;
        let mut rx = self.rx().await;
        while let Ok((uid, e)) = rx.recv().await {
            if let StreamingEvent::Pong(n, _) = e {
                if n == id && uid == other {
                    let elapsed = start.elapsed().as_secs_f64() / 2.0;
                    return Ok(elapsed);
                }
            }
        }
        Err(anyhow::anyhow!("Failed to estimate latency"))
    }

    async fn estimate_time_offset(&self, other: &str) -> Result<f64> {
        let start = std::time::Instant::now();
        let id = rand::thread_rng().gen_range(0..=u32::MAX);
        self.send(StreamingEvent::Ping(id)).await?;
        let mut rx = self.rx().await;
        while let Ok((uid, e)) = rx.recv().await {
            if let StreamingEvent::Pong(n, t) = e {
                if n == id && uid == other {
                    let remote_time = t + start.elapsed().as_secs_f64() / 2.0;
                    let local_time = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_secs_f64();
                    let offset = remote_time - local_time;
                    return Ok(offset);
                }
            }
        }
        Err(anyhow::anyhow!("Failed to estimate latency"))
    }

    async fn enable_pong(&self) -> Result<()> {
        let mut rx = self.rx().await;
        while let Ok((uid, e)) = rx.recv().await {
            if uid == self.uid() {
                continue;
            }
            if let StreamingEvent::Ping(n) = e {
                let t = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                self.send(StreamingEvent::Pong(n, t)).await?;
            }
        }
        Ok(())
    }
}
