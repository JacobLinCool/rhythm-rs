use color_eyre::eyre::Result;
use kira::manager::backend::DefaultBackend;
use kira::manager::{AudioManager, AudioManagerSettings};
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::tween::Tween;
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::sound_effect::SoundEffect;

pub enum MusicInstruction {
    Play(Box<StaticSoundData>),
    Stop,
    Pause,
    Resume,
}

pub struct AppAudio {
    pub tx: mpsc::Sender<MusicInstruction>,
    tx_effect: mpsc::Sender<StaticSoundData>,
    task: JoinHandle<()>,
    task_effect: JoinHandle<()>,
    pub playing: watch::Receiver<Option<Arc<RwLock<StaticSoundHandle>>>>,
    pub effects: SoundEffect,
}

impl AppAudio {
    pub fn new() -> Result<Self> {
        let (tx, mut rx) = mpsc::channel(100);
        let (tx_effect, mut rx_effect) = mpsc::channel(100);
        let (playing_tx, playing) = watch::channel::<Option<Arc<RwLock<StaticSoundHandle>>>>(None);

        let mut player = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;
        let mut player_effect =
            AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())?;

        let task = tokio::spawn(async move {
            while let Some(sound) = rx.recv().await {
                match sound {
                    MusicInstruction::Play(sound) => {
                        if let Some(handle) = playing_tx.borrow().clone() {
                            handle.write().unwrap().stop(Tween::default()).unwrap();
                        }
                        let handle = player.play(*sound).unwrap();
                        playing_tx
                            .send(Some(Arc::new(RwLock::new(handle))))
                            .unwrap();
                        player.resume(Tween::default()).unwrap();
                    }
                    MusicInstruction::Stop => {
                        if let Some(handle) = playing_tx.borrow().clone() {
                            handle.write().unwrap().stop(Tween::default()).unwrap();
                        }
                        playing_tx.send(None).unwrap();
                    }
                    MusicInstruction::Pause => {
                        player.pause(Tween::default()).unwrap();
                    }
                    MusicInstruction::Resume => {
                        player.resume(Tween::default()).unwrap();
                    }
                }
            }
        });

        let task_effect = tokio::spawn(async move {
            while let Some(sound) = rx_effect.recv().await {
                player_effect.play(sound).unwrap();
            }
        });

        let effects = SoundEffect::default();

        Ok(Self {
            tx,
            tx_effect,
            task,
            task_effect,
            playing,
            effects,
        })
    }

    pub async fn play(&self, sound: StaticSoundData) -> Result<()> {
        self.tx
            .send(MusicInstruction::Play(Box::new(sound)))
            .await?;
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.tx.send(MusicInstruction::Stop).await?;
        Ok(())
    }

    pub async fn pause(&self) -> Result<()> {
        self.tx.send(MusicInstruction::Pause).await?;
        Ok(())
    }

    pub async fn resume(&self) -> Result<()> {
        self.tx.send(MusicInstruction::Resume).await?;
        Ok(())
    }

    pub fn playing_time(&self) -> Option<f64> {
        let playing = self.playing.borrow().clone();
        playing.as_ref()?;
        Some(playing.unwrap().read().unwrap().position())
    }

    pub fn is_playing(&self) -> bool {
        self.playing.borrow().is_some()
    }

    pub async fn play_effect(&self, sound: StaticSoundData) -> Result<()> {
        self.tx_effect.send(sound).await?;
        Ok(())
    }
}

impl Drop for AppAudio {
    fn drop(&mut self) {
        self.task.abort();
        self.task_effect.abort();
    }
}
