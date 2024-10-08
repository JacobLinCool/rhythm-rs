use crate::note::{TaikoNote, TaikoNoteType, TaikoNoteVariant};
use crate::tja::{TJACourse, TJAHeader, TJA};

pub struct TJAParser {}

impl Default for TJAParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TJAParser {
    pub fn new() -> Self {
        Self {}
    }

    pub fn parse(&self, tja_content: impl AsRef<str>) -> Result<TJA, &'static str> {
        let mut tja = TJA {
            header: TJAHeader::new(),
            courses: Vec::new(),
        };

        let mut course: Option<TJACourse> = None;
        let mut balloons = Vec::new();
        let mut time_ms = 0.0;
        let mut bpm = 60.0;
        let mut scroll = 1.0;
        let mut measure = (4, 4);
        let mut segments: Vec<(f32, f32, Vec<char>)> = Vec::new();
        let mut current_combo: Option<TaikoNote> = None;

        for mut line in tja_content.as_ref().lines() {
            if let Some(pair) = line.split_once("//") {
                line = pair.0;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if course.is_none() {
                if let Some((key, value)) = line.split_once(':') {
                    let key = key.trim();
                    let value = value.trim();

                    match key {
                        "TITLE" => tja.header.title = Some(value.to_string()),
                        "SUBTITLE" => tja.header.subtitle = Some(value.to_string()),
                        "BPM" => tja.header.bpm = value.parse().ok(),
                        "WAVE" => tja.header.wave = Some(value.to_string()),
                        "OFFSET" => tja.header.offset = value.parse().ok(),
                        "DEMOSTART" => tja.header.demostart = value.parse().ok(),
                        "SONGVOL" => tja.header.songvol = value.parse().ok(),
                        "SEVOL" => tja.header.sevol = value.parse().ok(),
                        "STYLE" => tja.header.style = Some(value.to_string()),
                        "GENRE" => tja.header.genre = Some(value.to_string()),
                        "ARTIST" => tja.header.artist = Some(value.to_string()),
                        "COURSE" => {
                            course = Some(TJACourse::new(parse_course(value)));
                            balloons.clear();
                            time_ms = 0.0;
                            bpm = tja.header.bpm.unwrap_or(60.0);
                            scroll = 1.0;
                            measure = (4, 4);
                            segments.clear();
                            current_combo = None;
                        }
                        _ => {}
                    }
                }
            } else if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();

                match key {
                    "LEVEL" => course.as_mut().unwrap().level = value.parse().ok(),
                    "BALLOON" => {
                        for balloon in value.split(',') {
                            let count = balloon.parse().unwrap_or(0);
                            balloons.push(count);
                        }
                        balloons.reverse();
                    }
                    "SCOREINIT" => course.as_mut().unwrap().scoreinit = value.parse().ok(),
                    "SCOREDIFF" => course.as_mut().unwrap().scorediff = value.parse().ok(),
                    _ => {}
                }
            } else if let Some(raw) = line.strip_prefix('#') {
                let mut iter = raw.split_whitespace();
                let key = iter.next();
                if key.is_none() {
                    continue;
                }
                let key = key.unwrap();
                let value = iter.next();
                match key {
                    "GOGOSTART" => course.as_mut().unwrap().notes.push(TaikoNote {
                        start: time_ms,
                        duration: 0.0,
                        volume: 1,
                        variant: TaikoNoteVariant::Invisible,
                        note_type: TaikoNoteType::GogoStart,
                        speed: bpm * scroll,
                    }),
                    "GOGOEND" => course.as_mut().unwrap().notes.push(TaikoNote {
                        start: time_ms,
                        duration: 0.0,
                        volume: 1,
                        variant: TaikoNoteVariant::Invisible,
                        note_type: TaikoNoteType::GogoEnd,
                        speed: bpm * scroll,
                    }),
                    "BPMCHANGE" => {
                        bpm = value
                            .unwrap()
                            .parse()
                            .unwrap_or(tja.header.bpm.unwrap_or(0.0));
                    }
                    "MEASURE" => {
                        let (beat, note) = value.unwrap().split_once('/').unwrap();
                        let beat = beat.parse().unwrap_or(4);
                        let note = note.parse().unwrap_or(4);
                        measure = (beat, note);
                    }
                    "SCROLL" => {
                        scroll = value.unwrap().parse().unwrap_or(1.0);
                    }
                    "DELAY" => {
                        let delay = value.unwrap().parse().unwrap_or(0.0);
                        time_ms += delay;
                    }
                    "START" => {
                        // #[cfg(debug_assertions)]
                        // println!("{:?}", course);
                    }
                    "END" => {
                        tja.courses.push(course.take().unwrap());
                    }
                    _ => {}
                }
            } else {
                let last_part = line.strip_suffix(',');
                let segment = (
                    bpm,
                    scroll,
                    last_part.unwrap_or(line).chars().collect::<Vec<char>>(),
                );
                segments.push(segment);

                if last_part.is_some() {
                    let notes = segments.iter().map(|(_, _, s)| s.len()).sum::<usize>();
                    if notes == 0 {
                        if segments.is_empty() {
                            segments.push((bpm, scroll, vec!['0']));
                        } else if segments.len() == 1 {
                            segments.get_mut(0).unwrap().2.push('0');
                        }
                    }

                    // #[cfg(debug_assertions)]
                    // println!("{:?}", segments);

                    let notes = segments.iter().map(|(_, _, s)| s.len()).sum::<usize>();

                    let mut first = true;
                    for (bpm, scroll, segment) in segments.iter() {
                        let duration = (60.0 / *bpm as f64)
                            * (measure.0 as f64 / measure.1 as f64)
                            * (4.0 / notes as f64);

                        // bar line
                        if first {
                            course.as_mut().unwrap().notes.push(TaikoNote {
                                start: time_ms,
                                duration: 0.0,
                                volume: 0,
                                variant: TaikoNoteVariant::Invisible,
                                note_type: TaikoNoteType::BarLine,
                                speed: bpm * scroll,
                            });
                            first = false;
                        }

                        for c in segment.iter() {
                            match c {
                                '1' => {
                                    course.as_mut().unwrap().notes.push(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: 1,
                                        variant: TaikoNoteVariant::Don,
                                        note_type: TaikoNoteType::Small,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '2' => {
                                    course.as_mut().unwrap().notes.push(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: 1,
                                        variant: TaikoNoteVariant::Kat,
                                        note_type: TaikoNoteType::Small,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '3' => {
                                    course.as_mut().unwrap().notes.push(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: 1,
                                        variant: TaikoNoteVariant::Don,
                                        note_type: TaikoNoteType::Big,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '4' => {
                                    course.as_mut().unwrap().notes.push(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: 1,
                                        variant: TaikoNoteVariant::Kat,
                                        note_type: TaikoNoteType::Big,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '5' => {
                                    current_combo = Some(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: u16::MAX,
                                        variant: TaikoNoteVariant::Both,
                                        note_type: TaikoNoteType::SmallCombo,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '6' => {
                                    current_combo = Some(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: u16::MAX,
                                        variant: TaikoNoteVariant::Both,
                                        note_type: TaikoNoteType::BigCombo,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '7' => {
                                    current_combo = Some(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: balloons.pop().unwrap_or(5),
                                        variant: TaikoNoteVariant::Both,
                                        note_type: TaikoNoteType::Balloon,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                '8' => {
                                    if let Some(mut combo) = current_combo.take() {
                                        combo.duration = time_ms - combo.start;
                                        course.as_mut().unwrap().notes.push(combo);
                                    }
                                }
                                '9' => {
                                    current_combo = Some(TaikoNote {
                                        start: time_ms,
                                        duration: 0.0,
                                        volume: balloons.pop().unwrap_or(5),
                                        variant: TaikoNoteVariant::Both,
                                        note_type: TaikoNoteType::Yam,
                                        speed: { *bpm } * scroll,
                                    });
                                }
                                _ => {}
                            };
                            time_ms += duration;
                        }
                    }

                    segments.clear();
                }
            }
        }

        Ok(tja)
    }
}

fn parse_course(course: &str) -> i32 {
    match course.to_lowercase().as_str() {
        "easy" => 0,
        "normal" => 1,
        "hard" => 2,
        "oni" => 3,
        "edit" => 4,
        _ => course.parse().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;
    use std::fs;

    #[test]
    fn parse_tja_nosferatu() {
        const TJA_FILE: &str = "./samples/Nosferatu.tja";
        const JSON_FILE: &str = "./fixtures/Nosferatu-parsed-1.json";

        let raw = fs::read_to_string(TJA_FILE).unwrap();
        let parser = TJAParser::new();
        let tja: TJA = parser.parse(&raw).unwrap();
        let tja_json = serde_json::to_string_pretty(&tja).unwrap();
        // fs::write(JSON_FILE, &tja_json).unwrap();

        let expected = fs::read_to_string(JSON_FILE).unwrap();
        assert_eq!(tja_json, expected);
    }
}
