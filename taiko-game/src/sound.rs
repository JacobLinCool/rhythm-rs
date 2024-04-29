use rodio::{
    self,
    dynamic_mixer::{mixer, DynamicMixer, DynamicMixerController},
    Decoder, Source,
};
use std::{fs::File, io::BufReader, path::Path};

pub struct SoundData {
    buffer: Vec<f32>,
    sample_rate: u32,
    channels: u16,
}

impl SoundData {
    pub fn load_from_path(
        file_path: impl AsRef<Path>,
    ) -> Result<Self, rodio::decoder::DecoderError> {
        let file = File::open(&file_path)
            .expect(format!("Failed to open file: {:?}", file_path.as_ref()).as_str());
        Self::load_from_file(file)
    }

    pub fn load_from_file(file: File) -> Result<Self, rodio::decoder::DecoderError> {
        let decoder = Decoder::new(BufReader::new(file))?;
        let decoder = decoder.convert_samples::<f32>();
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();
        let buffer: Vec<f32> = decoder.collect();
        Ok(Self::load(buffer, sample_rate, channels))
    }

    pub fn load_from_buffer(buffer: Vec<u8>) -> Result<Self, rodio::decoder::DecoderError> {
        let decoder = Decoder::new(std::io::Cursor::new(buffer))?;
        let decoder = decoder.convert_samples::<f32>();
        let channels = decoder.channels();
        let sample_rate = decoder.sample_rate();
        let buffer: Vec<f32> = decoder.collect();
        Ok(Self::load(buffer, sample_rate, channels))
    }

    pub fn load(data: Vec<f32>, sample_rate: impl Into<u32>, channels: impl Into<u16>) -> Self {
        Self {
            buffer: data,
            sample_rate: sample_rate.into(),
            channels: channels.into(),
        }
    }
}

/// A sound player that plays sound effects and background music.
/// Which supports playing multiple sounds at the same time.
pub(crate) trait SoundPlayer {
    /// Plays a sound effect.
    async fn play_effect(&mut self, effect: &SoundData);

    /// Plays a background music.
    async fn play_music(&mut self, music: &SoundData);

    /// Plays a background music from a specific time.
    async fn play_music_from(&mut self, music: &SoundData, time: f64);

    /// Get the paused state of the background music.
    async fn is_music_paused(&self) -> bool;

    /// Stops the background music.
    async fn stop_music(&mut self);

    /// Pauses the background music.
    async fn pause_music(&mut self);

    /// Resumes the background music.
    async fn resume_music(&mut self);

    /// Gets the current playing time of the background music.
    async fn get_music_time(&self) -> f64;

    /// Sets the volume of the sound player.
    async fn set_volume(&mut self, volume: f32);

    /// Gets the volume of the sound player.
    async fn get_volume(&self) -> f32;
}

use rodio::{OutputStream, Sink};
use std::sync::{Arc, Mutex};
use tokio::{sync::Mutex as AsyncMutex, time::Instant};

pub struct RodioSoundPlayer {
    sink: Arc<AsyncMutex<Sink>>,
    controller: Arc<DynamicMixerController<f32>>,
    output_stream: OutputStream,
    music_start: Option<Instant>,
    music_time: f64,
}

impl RodioSoundPlayer {
    pub fn new() -> anyhow::Result<Self> {
        let (output_stream, stream_handle) = OutputStream::try_default()?;
        let sink = Sink::try_new(&stream_handle)?;
        let (controller, mixer) = mixer(2, 44100);
        Ok(Self {
            sink: Arc::new(AsyncMutex::new(sink)),
            controller,
            output_stream,
            music_start: None,
            music_time: 0.0,
        })
    }
}

impl SoundPlayer for RodioSoundPlayer {
    async fn play_effect(&mut self, effect: &SoundData) {
        let source = rodio::buffer::SamplesBuffer::new(
            effect.channels,
            effect.sample_rate,
            effect.buffer.clone(),
        );
        self.controller.add(source);
    }

    async fn play_music(&mut self, music: &SoundData) {
        let sink = self.sink.lock().await;

        let source = rodio::buffer::SamplesBuffer::new(
            music.channels,
            music.sample_rate,
            music.buffer.clone(),
        );

        let (controller, mixer) = mixer::<f32>(2, 44100);
        sink.append(mixer);
        self.controller = controller;
        self.controller.add(source);

        sink.play();
        self.music_start = Some(Instant::now());
        self.music_time = 0.0;
    }

    async fn play_music_from(&mut self, music: &SoundData, time: f64) {
        let sink = self.sink.lock().await;
        let data = music
            .buffer
            .clone()
            .into_iter()
            .skip((time * music.sample_rate as f64 * music.channels as f64) as usize)
            .collect::<Vec<f32>>();

        let source = rodio::buffer::SamplesBuffer::new(music.channels, music.sample_rate, data);

        let (controller, mixer) = mixer::<f32>(2, 44100);
        sink.append(mixer);
        self.controller = controller;
        self.controller.add(source);

        sink.play();
        self.music_start = Some(Instant::now());
        self.music_time = time;
    }

    async fn is_music_paused(&self) -> bool {
        let sink = self.sink.lock().await;
        sink.is_paused()
    }

    async fn stop_music(&mut self) {
        let sink = self.sink.lock().await;
        sink.stop();
        self.music_start = None;
    }

    async fn pause_music(&mut self) {
        let sink = self.sink.lock().await;
        sink.pause();
        if let Some(start) = self.music_start {
            self.music_time += start.elapsed().as_secs_f64();
        }
    }

    async fn resume_music(&mut self) {
        let sink = self.sink.lock().await;
        sink.play();
        self.music_start = Some(Instant::now());
    }

    async fn get_music_time(&self) -> f64 {
        let current = if let Some(start) = self.music_start {
            start.elapsed().as_secs_f64()
        } else {
            0.0
        };
        self.music_time + current
    }

    async fn set_volume(&mut self, volume: f32) {
        let sink = self.sink.lock().await;
        sink.set_volume(volume);
    }

    async fn get_volume(&self) -> f32 {
        let sink = self.sink.lock().await;
        sink.volume()
    }
}
