use crossterm::event::{Event, EventStream};
use futures::{FutureExt, StreamExt};
use tokio::sync::broadcast;

pub struct InputMixer {
    tx: broadcast::Sender<Event>,
}

impl Default for InputMixer {
    fn default() -> Self {
        Self::new()
    }
}

impl InputMixer {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(100);
        Self { tx }
    }

    pub fn listen_local_input(&self) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let mut reader = EventStream::new();
            while let Some(Ok(event)) = reader.next().fuse().await {
                tx.send(event).unwrap();
            }
        });
    }

    pub fn rx(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn send(&self, event: Event) {
        self.tx.send(event).unwrap();
    }
}
