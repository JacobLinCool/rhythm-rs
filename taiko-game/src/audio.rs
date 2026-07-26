use std::error::Error;
use std::fmt;
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use kira::backend::Backend;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::sound::PlaybackState;
use kira::{AudioManager, AudioManagerSettings, Decibels, DefaultBackend, StartTime, Tween};

use crate::resource::SongAudioSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioCapability {
    Available,
    Unavailable { reason: Arc<str> },
}

impl AudioCapability {
    pub(crate) fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    fn unavailable_reason(&self) -> Option<Arc<str>> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(Arc::clone(reason)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioRequirement {
    SongPlayback,
    SoundEffects,
}

impl fmt::Display for AudioRequirement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SongPlayback => "song playback",
            Self::SoundEffects => "sound effects",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioDeviceUnavailable {
    pub(crate) requirement: AudioRequirement,
    pub(crate) reason: Arc<str>,
}

impl fmt::Display for AudioDeviceUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} requires an audio output device, but audio is unavailable: {}",
            self.requirement, self.reason
        )
    }
}

impl Error for AudioDeviceUnavailable {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AudioNotice {
    OutputUnavailable { reason: Arc<str> },
    SoundEffectsDisabled { reason: Arc<str> },
}

impl AudioNotice {
    pub(crate) fn from_capability(capability: &AudioCapability) -> Option<Self> {
        capability
            .unavailable_reason()
            .map(|reason| Self::OutputUnavailable { reason })
    }
}

pub(crate) trait GameAudio {
    fn capability(&self) -> AudioCapability;
    fn play_prepared_song(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
    ) -> Result<()>;
    fn play_prepared_song_scheduled(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
        delay: Duration,
    ) -> Result<()>;
    fn stop_song(&mut self) -> Result<()>;
    fn pause_song(&mut self) -> Result<()>;
    fn resume_song(&mut self) -> Result<()>;
    fn seek_song(&mut self, seconds: f64) -> Result<()>;
    fn set_song_playback_rate(&mut self, rate: f64) -> Result<()>;
    fn song_position_seconds(&self) -> f64;
    fn is_song_finished(&self) -> bool;
    fn set_song_volume(&mut self, volume: u8);
    fn set_se_volume(&mut self, volume: u8);
    fn play_don(&mut self) -> Result<()>;
    fn play_kat(&mut self) -> Result<()>;
}

#[derive(Clone)]
pub(crate) struct PreparedSongAudio {
    data: StaticSoundData,
}

pub struct AudioEngine<B: Backend = DefaultBackend> {
    manager: Option<AudioManager<B>>,
    capability: AudioCapability,
    song: Option<StaticSoundHandle>,
    silent_clock: Option<SilentPlaybackClock>,
    don_se: StaticSoundData,
    kat_se: StaticSoundData,
    song_volume: Decibels,
    se_volume: Decibels,
}

impl AudioEngine<DefaultBackend> {
    pub fn new(song_volume: u8, se_volume: u8) -> Result<Self> {
        Self::new_with_factory(song_volume, se_volume, || {
            AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
                .context("failed to initialize audio backend")
        })
    }

    pub(crate) fn new_with_factory(
        song_volume: u8,
        se_volume: u8,
        factory: impl FnOnce() -> Result<AudioManager<DefaultBackend>>,
    ) -> Result<Self> {
        match factory() {
            Ok(manager) => Self::with_manager(manager, song_volume, se_volume),
            Err(error) => Self::without_manager(
                Arc::<str>::from(format!("{error:#}")),
                song_volume,
                se_volume,
            ),
        }
    }
}

impl<B: Backend> AudioEngine<B> {
    fn with_manager(manager: AudioManager<B>, song_volume: u8, se_volume: u8) -> Result<Self> {
        Self::build(
            Some(manager),
            AudioCapability::Available,
            song_volume,
            se_volume,
        )
    }

    fn without_manager(reason: Arc<str>, song_volume: u8, se_volume: u8) -> Result<Self> {
        Self::build(
            None,
            AudioCapability::Unavailable { reason },
            song_volume,
            se_volume,
        )
    }

    fn build(
        manager: Option<AudioManager<B>>,
        capability: AudioCapability,
        song_volume: u8,
        se_volume: u8,
    ) -> Result<Self> {
        let don_se =
            StaticSoundData::from_cursor(Cursor::new(include_bytes!("../assets/don.wav").to_vec()))
                .context("failed to load built-in don SE")?;

        let kat_se =
            StaticSoundData::from_cursor(Cursor::new(include_bytes!("../assets/kat.wav").to_vec()))
                .context("failed to load built-in kat SE")?;

        Ok(Self {
            manager,
            capability,
            song: None,
            silent_clock: None,
            don_se,
            kat_se,
            song_volume: percentage_to_decibels(song_volume),
            se_volume: percentage_to_decibels(se_volume),
        })
    }

    pub(crate) fn capability(&self) -> AudioCapability {
        self.capability.clone()
    }

    fn manager_for(
        &mut self,
        requirement: AudioRequirement,
    ) -> std::result::Result<&mut AudioManager<B>, AudioDeviceUnavailable> {
        let reason = self
            .capability
            .unavailable_reason()
            .unwrap_or_else(|| Arc::<str>::from("audio manager is not initialized"));
        self.manager.as_mut().ok_or(AudioDeviceUnavailable {
            requirement,
            reason,
        })
    }

    #[cfg(test)]
    pub fn play_song(
        &mut self,
        source: Option<SongAudioSource>,
        start_seconds: f64,
        looping: bool,
    ) -> Result<()> {
        validate_position(start_seconds)?;
        let prepared = source.map(prepare_song_audio).transpose()?;
        self.play_prepared_song(prepared, start_seconds, looping)
    }

    pub(crate) fn play_prepared_song(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
    ) -> Result<()> {
        self.play_prepared_song_scheduled(prepared, start_seconds, looping, Duration::ZERO)
    }

    pub(crate) fn play_prepared_song_scheduled(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
        delay: Duration,
    ) -> Result<()> {
        validate_position(start_seconds)?;
        let silent_clock = prepared
            .is_none()
            .then(|| SilentPlaybackClock::new_scheduled(start_seconds, Instant::now(), delay))
            .transpose()?;
        self.stop_song()?;
        match prepared {
            Some(prepared) => {
                let data = configure_song(
                    prepared.data,
                    self.song_volume,
                    start_seconds,
                    looping,
                    delay,
                );
                self.song = Some(
                    self.manager_for(AudioRequirement::SongPlayback)?
                        .play(data)
                        .context("failed to play song")?,
                );
            }
            None => {
                self.silent_clock = silent_clock;
            }
        }
        Ok(())
    }

    pub fn stop_song(&mut self) -> Result<()> {
        if let Some(mut song) = self.song.take() {
            song.stop(Tween::default());
        }
        self.silent_clock = None;
        Ok(())
    }

    pub fn pause_song(&mut self) -> Result<()> {
        if let Some(song) = self.song.as_mut() {
            song.pause(Tween::default());
        }
        if let Some(clock) = self.silent_clock.as_mut() {
            clock.pause(Instant::now());
        }
        Ok(())
    }

    pub fn resume_song(&mut self) -> Result<()> {
        if let Some(song) = self.song.as_mut() {
            song.resume(Tween::default());
        }
        if let Some(clock) = self.silent_clock.as_mut() {
            clock.resume(Instant::now())?;
        }
        Ok(())
    }

    pub fn seek_song(&mut self, seconds: f64) -> Result<()> {
        validate_position(seconds)?;
        if let Some(song) = self.song.as_mut() {
            song.seek_to(seconds);
        }
        if let Some(clock) = self.silent_clock.as_mut() {
            clock.seek(seconds, Instant::now());
        }
        Ok(())
    }

    pub fn set_song_playback_rate(&mut self, rate: f64) -> Result<()> {
        validate_playback_rate(rate)?;
        if let Some(song) = self.song.as_mut() {
            song.set_playback_rate(rate, Tween::default());
        }
        if let Some(clock) = self.silent_clock.as_mut() {
            clock.set_rate(rate, Instant::now());
        }
        Ok(())
    }

    pub fn song_position_seconds(&self) -> f64 {
        if let Some(song) = self.song.as_ref() {
            song.position()
        } else {
            self.silent_clock
                .as_ref()
                .map_or(0.0, |clock| clock.position(Instant::now()))
        }
    }

    pub fn is_song_finished(&self) -> bool {
        self.song.as_ref().map_or_else(
            || self.silent_clock.is_none(),
            |song| song.state() == PlaybackState::Stopped,
        )
    }

    pub fn set_song_volume(&mut self, volume: u8) {
        self.song_volume = percentage_to_decibels(volume);
        if let Some(song) = self.song.as_mut() {
            song.set_volume(self.song_volume, Tween::default());
        }
    }

    pub fn set_se_volume(&mut self, volume: u8) {
        self.se_volume = percentage_to_decibels(volume);
    }

    pub fn play_don(&mut self) -> Result<()> {
        let sound = self.don_se.volume(self.se_volume);
        let _ = self
            .manager_for(AudioRequirement::SoundEffects)?
            .play(sound)
            .context("failed to play don SE")?;
        Ok(())
    }

    pub fn play_kat(&mut self) -> Result<()> {
        let sound = self.kat_se.volume(self.se_volume);
        let _ = self
            .manager_for(AudioRequirement::SoundEffects)?
            .play(sound)
            .context("failed to play kat SE")?;
        Ok(())
    }
}

impl<B: Backend> GameAudio for AudioEngine<B> {
    fn capability(&self) -> AudioCapability {
        AudioEngine::capability(self)
    }

    fn play_prepared_song(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
    ) -> Result<()> {
        AudioEngine::play_prepared_song(self, prepared, start_seconds, looping)
    }

    fn play_prepared_song_scheduled(
        &mut self,
        prepared: Option<PreparedSongAudio>,
        start_seconds: f64,
        looping: bool,
        delay: Duration,
    ) -> Result<()> {
        AudioEngine::play_prepared_song_scheduled(self, prepared, start_seconds, looping, delay)
    }

    fn stop_song(&mut self) -> Result<()> {
        AudioEngine::stop_song(self)
    }

    fn pause_song(&mut self) -> Result<()> {
        AudioEngine::pause_song(self)
    }

    fn resume_song(&mut self) -> Result<()> {
        AudioEngine::resume_song(self)
    }

    fn seek_song(&mut self, seconds: f64) -> Result<()> {
        AudioEngine::seek_song(self, seconds)
    }

    fn set_song_playback_rate(&mut self, rate: f64) -> Result<()> {
        AudioEngine::set_song_playback_rate(self, rate)
    }

    fn song_position_seconds(&self) -> f64 {
        AudioEngine::song_position_seconds(self)
    }

    fn is_song_finished(&self) -> bool {
        AudioEngine::is_song_finished(self)
    }

    fn set_song_volume(&mut self, volume: u8) {
        AudioEngine::set_song_volume(self, volume);
    }

    fn set_se_volume(&mut self, volume: u8) {
        AudioEngine::set_se_volume(self, volume);
    }

    fn play_don(&mut self) -> Result<()> {
        AudioEngine::play_don(self)
    }

    fn play_kat(&mut self) -> Result<()> {
        AudioEngine::play_kat(self)
    }
}

#[derive(Debug, Clone, Copy)]
struct SilentPlaybackClock {
    state: SilentPlaybackState,
    rate: f64,
}

#[derive(Debug, Clone, Copy)]
enum SilentPlaybackState {
    Running {
        anchor: Instant,
        position_seconds: f64,
    },
    Paused {
        position_seconds: f64,
        resume_delay: Duration,
    },
}

impl SilentPlaybackClock {
    fn new_scheduled(position_seconds: f64, now: Instant, delay: Duration) -> Result<Self> {
        validate_position(position_seconds)?;
        let anchor = now
            .checked_add(delay)
            .ok_or_else(|| anyhow::anyhow!("silent song start deadline overflow"))?;
        Ok(Self {
            state: SilentPlaybackState::Running {
                anchor,
                position_seconds,
            },
            rate: 1.0,
        })
    }

    fn position(self, now: Instant) -> f64 {
        match self.state {
            SilentPlaybackState::Running {
                anchor,
                position_seconds,
            } => position_seconds + now.saturating_duration_since(anchor).as_secs_f64() * self.rate,
            SilentPlaybackState::Paused {
                position_seconds, ..
            } => position_seconds,
        }
    }

    fn pause(&mut self, now: Instant) {
        if let SilentPlaybackState::Running { anchor, .. } = self.state {
            self.state = SilentPlaybackState::Paused {
                position_seconds: self.position(now),
                resume_delay: anchor.saturating_duration_since(now),
            };
        }
    }

    fn resume(&mut self, now: Instant) -> Result<()> {
        if let SilentPlaybackState::Paused {
            position_seconds,
            resume_delay,
        } = self.state
        {
            self.state = SilentPlaybackState::Running {
                anchor: now
                    .checked_add(resume_delay)
                    .ok_or_else(|| anyhow::anyhow!("silent song resume deadline overflow"))?,
                position_seconds,
            };
        }
        Ok(())
    }

    fn seek(&mut self, position_seconds: f64, now: Instant) {
        self.state = match self.state {
            SilentPlaybackState::Running { anchor, .. } => SilentPlaybackState::Running {
                anchor: anchor.max(now),
                position_seconds,
            },
            SilentPlaybackState::Paused { resume_delay, .. } => SilentPlaybackState::Paused {
                position_seconds,
                resume_delay,
            },
        };
    }

    fn set_rate(&mut self, rate: f64, now: Instant) {
        if let SilentPlaybackState::Running { anchor, .. } = self.state {
            self.state = SilentPlaybackState::Running {
                anchor: anchor.max(now),
                position_seconds: self.position(now),
            };
        }
        self.rate = rate;
    }
}

fn validate_position(seconds: f64) -> Result<()> {
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("song position must be finite and non-negative");
    }
    Ok(())
}

fn validate_playback_rate(rate: f64) -> Result<()> {
    if !rate.is_finite() || rate <= 0.0 {
        bail!("song playback rate must be finite and positive");
    }
    Ok(())
}

pub(crate) fn prepare_song_audio(source: SongAudioSource) -> Result<PreparedSongAudio> {
    prepare_song_audio_cancellable(source, &|| false)
}

pub(crate) fn prepare_song_audio_cancellable(
    source: SongAudioSource,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<PreparedSongAudio> {
    #[cfg(test)]
    PREPARE_INVOCATIONS.with(|count| count.set(count.get().saturating_add(1)));

    let decoded = match source {
        SongAudioSource::FilePath(path) => {
            taiko_audio::decode_file_cancellable(&path, is_cancelled)
                .with_context(|| format!("failed to decode audio file {}", path.display()))?
        }
        SongAudioSource::Bytes(bytes) => taiko_audio::decode_bytes_cancellable(bytes, is_cancelled)
            .context("failed to decode remote audio stream")?,
    };
    Ok(PreparedSongAudio {
        data: decoded.into_static_sound_data(),
    })
}

#[cfg(test)]
std::thread_local! {
    static PREPARE_INVOCATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn percentage_to_decibels(percentage: u8) -> Decibels {
    let amplitude = f32::from(percentage) / 100.0;
    if amplitude == 0.0 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * amplitude.log10())
    }
}

fn configure_song(
    base: StaticSoundData,
    volume: Decibels,
    start_seconds: f64,
    looping: bool,
    delay: Duration,
) -> StaticSoundData {
    let mut data = base.volume(volume);

    if start_seconds > 0.0 {
        data = data.start_position(start_seconds);
        if looping {
            data = data.loop_region(start_seconds..);
        }
    } else if looping {
        data = data.loop_region(..);
    }

    if delay.is_zero() {
        data
    } else {
        data.start_time(StartTime::Delayed(delay))
    }
}

#[cfg(test)]
mod tests {
    use kira::backend::mock::{MockBackend, MockBackendSettings};
    use kira::sound::{PlaybackPosition, Region};
    use kira::Value;

    use super::*;

    const TEST_SAMPLE_RATE: u32 = 44_100;

    fn test_engine(song_volume: u8, se_volume: u8) -> AudioEngine<MockBackend> {
        let manager = AudioManager::<MockBackend>::new(AudioManagerSettings {
            backend_settings: MockBackendSettings {
                sample_rate: TEST_SAMPLE_RATE,
            },
            ..AudioManagerSettings::default()
        })
        .expect("mock audio manager should initialize");
        AudioEngine::with_manager(manager, song_volume, se_volume)
            .expect("built-in sound effects should decode")
    }

    fn process_audio(engine: &mut AudioEngine<MockBackend>, batches: usize) {
        for _ in 0..batches {
            let manager = engine
                .manager
                .as_mut()
                .expect("mock engine has an audio manager");
            manager.backend_mut().on_start_processing();
            manager.backend_mut().process();
        }
    }

    fn assert_amplitude(percentage: u8) {
        let expected = f32::from(percentage) / 100.0;
        let actual = percentage_to_decibels(percentage).as_amplitude();
        assert!(
            (actual - expected).abs() <= f32::EPSILON * 4.0,
            "{percentage}% mapped to amplitude {actual}, expected {expected}"
        );
    }

    #[test]
    fn percentage_volume_preserves_linear_amplitude() {
        for percentage in [0, 1, 25, 50, 75, 100, u8::MAX] {
            assert_amplitude(percentage);
        }
    }

    #[test]
    fn song_configuration_preserves_seek_loop_and_volume() {
        let base =
            StaticSoundData::from_cursor(Cursor::new(include_bytes!("../assets/don.wav").to_vec()))
                .expect("built-in WAV should decode");
        let volume = percentage_to_decibels(40);
        let configured = configure_song(base, volume, 0.025, true, Duration::ZERO);

        assert_eq!(
            configured.settings.start_position,
            PlaybackPosition::Seconds(0.025)
        );
        assert_eq!(
            configured.settings.loop_region,
            Some(Region {
                start: PlaybackPosition::Seconds(0.025),
                ..Region::default()
            })
        );
        assert_eq!(configured.settings.volume, Value::Fixed(volume));
    }

    #[test]
    fn mock_backend_exercises_song_lifecycle_and_sound_effect_scheduling() {
        let mut engine = test_engine(50, 25);
        assert_eq!(engine.capability(), AudioCapability::Available);
        engine
            .play_song(
                Some(SongAudioSource::Bytes(
                    include_bytes!("../assets/don.wav").to_vec().into(),
                )),
                0.01,
                true,
            )
            .expect("song should be scheduled");
        process_audio(&mut engine, 1);

        assert_eq!(
            engine.song.as_ref().map(StaticSoundHandle::state),
            Some(PlaybackState::Playing)
        );
        assert!(engine.song_position_seconds() >= 0.01);

        engine.pause_song().expect("pause should be scheduled");
        process_audio(&mut engine, 8);
        assert_eq!(
            engine.song.as_ref().map(StaticSoundHandle::state),
            Some(PlaybackState::Paused)
        );

        engine.resume_song().expect("resume should be scheduled");
        process_audio(&mut engine, 8);
        assert_eq!(
            engine.song.as_ref().map(StaticSoundHandle::state),
            Some(PlaybackState::Playing)
        );

        engine.set_song_volume(80);
        engine.set_se_volume(60);
        engine.play_don().expect("don SFX should be scheduled");
        engine.play_kat().expect("kat SFX should be scheduled");
        process_audio(&mut engine, 1);

        engine.stop_song().expect("stop should be scheduled");
        assert!(engine.song.is_none());
        assert!(engine.is_song_finished());
        assert_eq!(engine.song_position_seconds(), 0.0);

        let local_song = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join("don.wav");
        engine
            .play_song(Some(SongAudioSource::FilePath(local_song)), 0.0, false)
            .expect("local song should load and be scheduled");
        process_audio(&mut engine, 1);
        assert_eq!(
            engine.song.as_ref().map(StaticSoundHandle::state),
            Some(PlaybackState::Playing)
        );
    }

    #[test]
    fn silent_clock_schedule_pause_seek_and_rate_are_monotonic_and_deterministic() {
        let start = Instant::now();
        let mut pending = SilentPlaybackClock::new_scheduled(1.0, start, Duration::from_secs(5))
            .expect("pending silent clock");
        pending.set_rate(2.0, start + Duration::from_secs(1));
        pending.seek(4.0, start + Duration::from_secs(2));
        assert_eq!(pending.position(start + Duration::from_secs(4)), 4.0);
        pending.pause(start + Duration::from_secs(3));
        pending
            .resume(start + Duration::from_secs(10))
            .expect("resume pending silent clock");
        assert_eq!(pending.position(start + Duration::from_secs(11)), 4.0);
        assert_eq!(pending.position(start + Duration::from_secs(13)), 6.0);

        let mut clock = SilentPlaybackClock::new_scheduled(3.0, start, Duration::from_secs(5))
            .expect("scheduled silent clock");

        assert_eq!(clock.position(start + Duration::from_secs(4)), 3.0);
        assert_eq!(clock.position(start + Duration::from_secs(7)), 5.0);

        clock.set_rate(0.5, start + Duration::from_secs(7));
        assert_eq!(clock.position(start + Duration::from_secs(11)), 7.0);
        clock.pause(start + Duration::from_secs(11));
        assert_eq!(clock.position(start + Duration::from_secs(100)), 7.0);

        clock.seek(9.0, start + Duration::from_secs(12));
        assert_eq!(clock.position(start + Duration::from_secs(100)), 9.0);
        clock
            .resume(start + Duration::from_secs(20))
            .expect("resume silent clock");
        assert_eq!(clock.position(start + Duration::from_secs(22)), 10.0);
    }

    #[test]
    fn silent_playback_uses_no_decoder_and_remains_active_until_stopped() {
        PREPARE_INVOCATIONS.with(|count| count.set(0));
        let mut engine = test_engine(100, 100);
        engine
            .play_song(None, 2.0, false)
            .expect("start silent playback");
        assert!(engine.song.is_none());
        assert!(engine.silent_clock.is_some());
        assert!(!engine.is_song_finished());
        assert!(engine.song_position_seconds() >= 2.0);

        engine.seek_song(4.0).expect("seek silent playback");
        engine
            .set_song_playback_rate(0.5)
            .expect("rate silent playback");
        engine.pause_song().expect("pause silent playback");
        let paused = engine.song_position_seconds();
        assert_eq!(engine.song_position_seconds(), paused);
        engine.resume_song().expect("resume silent playback");
        engine.stop_song().expect("stop silent playback");
        assert!(engine.is_song_finished());
        assert_eq!(engine.song_position_seconds(), 0.0);
        PREPARE_INVOCATIONS.with(|count| assert_eq!(count.get(), 0));

        assert!(engine.seek_song(-1.0).is_err());
        assert!(engine.set_song_playback_rate(0.0).is_err());
        assert!(engine.set_song_playback_rate(f64::NAN).is_err());
    }

    #[test]
    fn backend_factory_failure_becomes_an_explicit_unavailable_capability() {
        let engine = AudioEngine::<DefaultBackend>::new_with_factory(100, 100, || {
            Err(anyhow::anyhow!("fixture has no audio output device"))
        })
        .expect("backend discovery failure must not prevent engine construction");

        assert!(engine.manager.is_none());
        assert_eq!(
            engine.capability(),
            AudioCapability::Unavailable {
                reason: Arc::<str>::from("fixture has no audio output device")
            }
        );
    }

    #[test]
    fn unavailable_backend_still_runs_the_deterministic_silent_clock() {
        let mut engine = AudioEngine::<MockBackend>::without_manager(
            Arc::<str>::from("fixture has no audio output device"),
            100,
            100,
        )
        .expect("built-in sound effects should decode");

        engine
            .play_prepared_song_scheduled(None, 2.0, false, Duration::from_secs(60))
            .expect("silent playback must not require an audio manager");
        assert!(engine.manager.is_none());
        assert!(engine.song.is_none());
        assert!(!engine.is_song_finished());

        engine.pause_song().expect("pause silent playback");
        assert_eq!(engine.song_position_seconds(), 2.0);
        engine.seek_song(4.0).expect("seek silent playback");
        engine
            .set_song_playback_rate(0.5)
            .expect("change silent playback rate");
        engine.resume_song().expect("resume silent playback");
        assert_eq!(engine.song_position_seconds(), 4.0);

        engine.stop_song().expect("stop silent playback");
        assert!(engine.is_song_finished());
        assert_eq!(engine.song_position_seconds(), 0.0);
        assert!(engine.manager.is_none());
    }

    #[test]
    fn unavailable_backend_returns_typed_errors_for_audio_required_operations() {
        let mut engine = AudioEngine::<MockBackend>::without_manager(
            Arc::<str>::from("fixture has no audio output device"),
            100,
            100,
        )
        .expect("built-in sound effects should decode");
        let prepared = prepare_song_audio(SongAudioSource::Bytes(
            include_bytes!("../assets/don.wav").to_vec().into(),
        ))
        .expect("decode fixture");

        let music_error = engine
            .play_prepared_song(Some(prepared), 0.0, false)
            .expect_err("music playback must not silently fall back");
        assert_eq!(
            music_error
                .downcast_ref::<AudioDeviceUnavailable>()
                .expect("music error should retain its type")
                .requirement,
            AudioRequirement::SongPlayback
        );

        let se_error = engine
            .play_don()
            .expect_err("sound effects require an output device");
        assert_eq!(
            se_error
                .downcast_ref::<AudioDeviceUnavailable>()
                .expect("SE error should retain its type")
                .requirement,
            AudioRequirement::SoundEffects
        );
    }

    #[test]
    fn prepared_audio_can_be_scheduled_without_starting_early() {
        let prepared = prepare_song_audio(SongAudioSource::Bytes(
            include_bytes!("../assets/don.wav").to_vec().into(),
        ))
        .expect("decode fixture");
        let mut engine = test_engine(100, 100);
        engine
            .play_prepared_song_scheduled(Some(prepared), 0.0, false, Duration::from_secs(10))
            .expect("schedule prepared audio");
        process_audio(&mut engine, 2);

        assert_eq!(
            engine.song.as_ref().map(StaticSoundHandle::position),
            Some(0.0),
            "mock backend must not advance audio before the delayed start"
        );
        assert!(!engine.is_song_finished());
    }

    #[test]
    fn playing_prepared_audio_does_not_decode_again() {
        PREPARE_INVOCATIONS.with(|count| count.set(0));
        let prepared = prepare_song_audio(SongAudioSource::Bytes(
            include_bytes!("../assets/don.wav").to_vec().into(),
        ))
        .expect("fixture should decode during preparation");
        PREPARE_INVOCATIONS.with(|count| assert_eq!(count.get(), 1));

        let mut engine = test_engine(100, 100);
        engine
            .play_prepared_song(Some(prepared), 0.0, false)
            .expect("prepared data should be scheduled directly");
        PREPARE_INVOCATIONS.with(|count| {
            assert_eq!(
                count.get(),
                1,
                "the playback transition must not invoke an encoded-byte decoder"
            );
        });
    }
}
