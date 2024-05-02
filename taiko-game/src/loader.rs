use std::path::PathBuf;

use color_eyre::eyre::Result;
use glob::glob;
use kira::sound::static_sound::StaticSoundData;
use tja::{TJAParser, TJA};

use crate::utils::read_utf8_or_shiftjis;

pub struct PlaylistLoader {
    path: PathBuf,
}

impl PlaylistLoader {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn list(&self) -> Result<Vec<Song>> {
        let mut playlists = Vec::new();

        for entry in glob(&format!("{}/**/*.tja", self.path.to_string_lossy())).unwrap() {
            if let Ok(path) = entry {
                let parser = TJAParser::new();
                let mut tja = parser
                    .parse(&read_utf8_or_shiftjis(&path)?)
                    .map_err(|e| color_eyre::eyre::eyre!("Failed to parse TJA file: {}", e))?;

                tja.courses.sort_by_key(|course| course.course);

                if tja.header.title.is_none() || tja.header.title.as_ref().unwrap().is_empty() {
                    tja.header.title.replace(path.file_stem().unwrap().to_string_lossy().to_string());
                }

                if tja.header.subtitle.is_none() {
                    tja.header.subtitle.replace(String::new());
                }

                let music_path =
                    if let Some(wave) = tja.header.wave.clone().filter(|s| !s.is_empty()) {
                        let path = path.parent().unwrap().join(wave);
                        path
                    } else {
                        path.with_extension("ogg")
                    };

                playlists.push(Song { tja, music_path });
            }
        }

        Ok(playlists)
    }
}

#[derive(Debug, Clone)]
pub struct Song {
    tja: TJA,
    music_path: PathBuf,
}

impl Song {
    pub fn tja(&self) -> &TJA {
        &self.tja
    }

    pub async fn music(&self) -> Result<StaticSoundData> {
        if !self.music_path.exists() {
            return Err(color_eyre::eyre::eyre!(
                "Music file not found: {:?}",
                self.music_path
            ));
        }

        let data = StaticSoundData::from_file(&self.music_path, Default::default())?;
        Ok(data)
    }
}
