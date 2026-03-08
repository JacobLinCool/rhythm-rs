use std::io::Cursor;

use anyhow::{Context, Result};
use kira::manager::{backend::DefaultBackend, AudioManager, AudioManagerSettings};
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::sound::PlaybackState;
use kira::tween::Tween;

use crate::resource::SongAudioSource;

pub struct AudioEngine {
    manager: AudioManager<DefaultBackend>,
    song: Option<StaticSoundHandle>,
    don_se: StaticSoundData,
    kat_se: StaticSoundData,
    song_volume: f64,
    se_volume: f64,
}

impl AudioEngine {
    pub fn new(song_volume: u8, se_volume: u8) -> Result<Self> {
        let manager = AudioManager::new(AudioManagerSettings::default())
            .context("failed to initialize audio backend")?;

        let se_gain = f64::from(se_volume) / 100.0;
        let don_se =
            StaticSoundData::from_cursor(Cursor::new(include_bytes!("../assets/don.wav").to_vec()))
                .context("failed to load built-in don SE")?;

        let kat_se =
            StaticSoundData::from_cursor(Cursor::new(include_bytes!("../assets/kat.wav").to_vec()))
                .context("failed to load built-in kat SE")?;

        Ok(Self {
            manager,
            song: None,
            don_se,
            kat_se,
            song_volume: f64::from(song_volume) / 100.0,
            se_volume: se_gain,
        })
    }

    pub fn play_song(
        &mut self,
        source: SongAudioSource,
        start_seconds: f64,
        looping: bool,
    ) -> Result<()> {
        self.stop_song()?;

        let base = match source {
            SongAudioSource::FilePath(path) => StaticSoundData::from_file(&path)
                .with_context(|| format!("failed to open audio file {}", path.display()))?,
            SongAudioSource::Bytes(bytes) => StaticSoundData::from_cursor(Cursor::new(bytes))
                .context("failed to decode remote audio stream")?,
        };
        let mut data = base.volume(self.song_volume);

        if start_seconds > 0.0 {
            data = data.start_position(start_seconds);
            if looping {
                data = data.loop_region(start_seconds..);
            }
        } else if looping {
            data = data.loop_region(..);
        }

        self.song = Some(self.manager.play(data).context("failed to play song")?);
        Ok(())
    }

    pub fn stop_song(&mut self) -> Result<()> {
        if let Some(mut song) = self.song.take() {
            song.stop(Tween::default());
        }
        Ok(())
    }

    pub fn song_position_seconds(&self) -> f64 {
        self.song
            .as_ref()
            .map_or(0.0, kira::sound::static_sound::StaticSoundHandle::position)
    }

    pub fn is_song_finished(&self) -> bool {
        self.song
            .as_ref()
            .is_none_or(|song| song.state() == PlaybackState::Stopped)
    }

    pub fn set_song_volume(&mut self, volume: u8) {
        self.song_volume = f64::from(volume) / 100.0;
        if let Some(song) = self.song.as_mut() {
            song.set_volume(self.song_volume, Tween::default());
        }
    }

    pub fn set_se_volume(&mut self, volume: u8) {
        self.se_volume = f64::from(volume) / 100.0;
    }

    pub fn play_don(&mut self) -> Result<()> {
        let _ = self
            .manager
            .play(self.don_se.clone().volume(self.se_volume))
            .context("failed to play don SE")?;
        Ok(())
    }

    pub fn play_kat(&mut self) -> Result<()> {
        let _ = self
            .manager
            .play(self.kat_se.clone().volume(self.se_volume))
            .context("failed to play kat SE")?;
        Ok(())
    }
}
