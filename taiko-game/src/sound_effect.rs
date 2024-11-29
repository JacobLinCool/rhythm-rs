use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
use once_cell::sync::Lazy;
use std::io::Cursor;
use std::sync::RwLock;

pub static DON_SOUND: Lazy<StaticSoundData> = Lazy::new(|| {
    let cursor = Cursor::new(include_bytes!("../assets/don.wav"));
    StaticSoundData::from_cursor(cursor, Default::default()).unwrap()
});

pub static KAT_SOUND: Lazy<StaticSoundData> = Lazy::new(|| {
    let cursor = Cursor::new(include_bytes!("../assets/kat.wav"));
    StaticSoundData::from_cursor(cursor, Default::default()).unwrap()
});

pub struct SoundEffect {
    don: StaticSoundData,
    kat: StaticSoundData,
    vomume: RwLock<f64>,
}

impl Default for SoundEffect {
    fn default() -> Self {
        Self {
            don: DON_SOUND.clone(),
            kat: KAT_SOUND.clone(),
            vomume: RwLock::new(1.0),
        }
    }
}

impl SoundEffect {
    pub fn don(&self) -> StaticSoundData {
        self.don
            .with_settings(StaticSoundSettings::default().volume(*self.vomume.read().unwrap()))
    }

    pub fn kat(&self) -> StaticSoundData {
        self.kat
            .with_settings(StaticSoundSettings::default().volume(*self.vomume.read().unwrap()))
    }

    pub fn set_volume(&self, volume: f64) {
        *self.vomume.write().unwrap() = volume;
    }

    pub fn volume(&self) -> f64 {
        *self.vomume.read().unwrap()
    }
}
