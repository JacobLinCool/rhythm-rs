use std::{fs, io, io::Read, path::PathBuf};

use color_eyre::eyre::Result;
use glob::glob;
use kira::sound::static_sound::StaticSoundData;
use sha2::{Digest, Sha256};
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

        for path in glob(&format!("{}/**/*.tja", self.path.to_string_lossy()))?.flatten() {
            let parser = TJAParser::new();
            let mut tja = parser
                .parse(&read_utf8_or_shiftjis(&path)?)
                .map_err(|e| color_eyre::eyre::eyre!("Failed to parse TJA file: {}", e))?;

            tja.courses.sort_by_key(|course| course.course);

            if tja.header.title.is_none() || tja.header.title.as_ref().unwrap().is_empty() {
                tja.header
                    .title
                    .replace(path.file_stem().unwrap().to_string_lossy().to_string());
            }

            if tja.header.subtitle.is_none() {
                tja.header.subtitle.replace(String::new());
            }

            let music_path = if let Some(wave) = tja.header.wave.clone().filter(|s| !s.is_empty()) {
                let path = path.parent().unwrap().join(wave);
                path
            } else {
                path.with_extension("ogg")
            };

            playlists.push(Song {
                tja,
                music_path,
                music_sha256: None,
            });
        }

        Ok(playlists)
    }
}

#[derive(Debug, Clone)]
pub struct Song {
    tja: TJA,
    music_path: PathBuf,
    music_sha256: Option<String>,
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

    pub async fn music_bin(&self) -> Result<Vec<u8>> {
        let mut file = fs::File::open(&self.music_path)?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        Ok(buffer)
    }

    pub async fn music_sha256(&mut self) -> Result<String> {
        if let Some(sha256) = &self.music_sha256 {
            return Ok(sha256.clone());
        }

        let input = fs::File::open(&self.music_path)?;
        let mut reader = io::BufReader::new(input);

        let digest = {
            let mut hasher = Sha256::new();
            let mut buffer = [0; 1024];
            loop {
                let count = reader.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
            }
            hasher.finalize()
        };

        let sha256 = format!("{:x}", digest);
        self.music_sha256.replace(sha256.clone());
        Ok(sha256)
    }
}
