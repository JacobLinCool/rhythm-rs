use crate::common::*;
use anyhow::Result;
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use rmp_serde::{decode, encode};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::{accept_async, connect_async, tungstenite::protocol::Message};

#[derive(Debug, Clone)]
pub struct WebSocketStreamingServer {
    addr: String,
    sender: broadcast::Sender<Message>,
}

#[async_trait]
impl StreamingServer for WebSocketStreamingServer {
    fn new(addr: String) -> Result<Self> {
        let (tx, _rx) = broadcast::channel(1000);
        Ok(WebSocketStreamingServer { addr, sender: tx })
    }

    async fn start(&self) -> Result<()> {
        let listener = TcpListener::bind(&self.addr).await?;

        while let Ok((stream, _)) = listener.accept().await {
            let sender = self.sender.clone();
            tokio::spawn(async move {
                if let Ok(ws_stream) = accept_async(stream).await {
                    let (mut ws_tx, mut ws_rx) = ws_stream.split();
                    let mut rx = sender.subscribe();

                    tokio::spawn(async move {
                        while let Some(Ok(msg)) = ws_rx.next().await {
                            if let Message::Binary(_) = msg {
                                sender.send(msg).unwrap();
                            }
                        }
                    });

                    while let Ok(msg) = rx.recv().await {
                        let res = ws_tx.send(msg).await;
                        if let Err(
                            tokio_tungstenite::tungstenite::Error::AlreadyClosed
                            | tokio_tungstenite::tungstenite::Error::ConnectionClosed,
                        ) = res
                        {
                            break;
                        }
                        if let Err(e) = res {
                            eprintln!("Error sending message: {:?}", e);
                        }
                    }
                }
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct WebSocketStreamingClient<H: StreamableData, P: StreamableData> {
    uid: String,
    tx: mpsc::Sender<StreamingEvent<H, P>>,
    sender: broadcast::Sender<SentEvent<H, P>>,
}

#[async_trait]
impl<H: StreamableData + 'static, P: StreamableData + 'static> StreamingClient<H, P>
    for WebSocketStreamingClient<H, P>
{
    async fn new(addr: String, uid: String) -> Result<Self> {
        let url = format!("ws://{}/ws", addr);
        let (ws_stream, _) = connect_async(url).await?;
        let (mut ws_tx, mut ws_rx) = ws_stream.split();

        let (tx, mut event_rx) = mpsc::channel::<StreamingEvent<H, P>>(100);

        let (sender, _rx) = broadcast::channel(100);

        let uid_clone = uid.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                let uid = uid_clone.clone();
                let packet = (uid, event);
                if let Ok(packet) = encode::to_vec(&packet) {
                    let msg = Message::Binary(packet);
                    let _ = ws_tx.send(msg).await;
                }
            }
        });

        let sender_clone = sender.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_rx.next().await {
                if let Message::Binary(bin_msg) = msg {
                    let event = decode::from_slice::<SentEvent<H, P>>(&bin_msg).unwrap();
                    sender_clone.send(event).unwrap();
                }
            }
        });

        Ok(WebSocketStreamingClient { uid, tx, sender })
    }

    async fn send(&self, event: StreamingEvent<H, P>) -> Result<()> {
        self.tx.send(event).await?;
        Ok(())
    }

    async fn rx(&self) -> broadcast::Receiver<SentEvent<H, P>> {
        self.sender.subscribe()
    }

    fn uid(&self) -> &str {
        &self.uid
    }
}
