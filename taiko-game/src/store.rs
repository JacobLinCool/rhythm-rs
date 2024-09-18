use kira::sound::static_sound::StaticSoundData;
use std::collections::HashMap;
use std::io;

pub struct SoundStore {
    songs: HashMap<String, StaticSoundData>,
}

impl Default for SoundStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SoundStore {
    pub fn new() -> Self {
        SoundStore {
            songs: HashMap::new(),
        }
    }

    pub fn insert_vec(&mut self, id: &str, vec: Vec<u8>) {
        let cursor = io::Cursor::new(vec);
        let data = StaticSoundData::from_cursor(cursor, Default::default()).unwrap();
        self.songs.insert(id.to_string(), data);
    }

    pub fn insert(&mut self, id: &str, data: StaticSoundData) {
        self.songs.insert(id.to_string(), data);
    }

    pub fn get(&self, id: &str) -> Option<StaticSoundData> {
        self.songs.get(id).cloned()
    }
}
