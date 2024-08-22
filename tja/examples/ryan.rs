//! This example demonstrates how to convert TJA file to a custom JSON format.
//! The custom JSON output can be used to generate audio with
//! https://huggingface.co/spaces/ryanlinjui/taiko-music-generator

use rhythm_core::Note;
use serde::{Deserialize, Serialize};
use std::fs;
use tja::{TJAParser, TaikoNoteType, TaikoNoteVariant};

#[derive(Serialize, Deserialize)]
struct RyanChart {
    data: Vec<RyanChartInner>,
}

#[derive(Serialize, Deserialize)]
struct RyanChartInner {
    course: i32,
    chart: Vec<(i32, f32, f32, u16)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: {} <input.tja> [output.json]", args[0]);
        return;
    }
    let filepath = &args.get(1).unwrap();
    let raw = read_utf8_or_shiftjis(filepath).unwrap();

    let parser = TJAParser::new();
    let tja = parser.parse(&raw).unwrap();

    let mut ryan_chart = RyanChart { data: Vec::new() };

    for course in tja.courses.iter() {
        let mut chart = Vec::new();
        for note in course.notes.iter() {
            let t = if note.variant() == TaikoNoteVariant::Don
                && note.note_type == TaikoNoteType::Small
            {
                1
            } else if note.variant() == TaikoNoteVariant::Kat
                && note.note_type == TaikoNoteType::Small
            {
                2
            } else if note.variant() == TaikoNoteVariant::Don
                && note.note_type == TaikoNoteType::Big
            {
                3
            } else if note.variant() == TaikoNoteVariant::Kat
                && note.note_type == TaikoNoteType::Big
            {
                4
            } else if note.variant() == TaikoNoteVariant::Both
                && note.note_type == TaikoNoteType::SmallCombo
            {
                chart.push((
                    5,
                    note.start as f32 - tja.header.offset.unwrap(),
                    note.start as f32 - tja.header.offset.unwrap() + note.duration() as f32,
                    0,
                ));
                0
            } else if note.variant() == TaikoNoteVariant::Both
                && note.note_type == TaikoNoteType::BigCombo
            {
                chart.push((
                    6,
                    note.start as f32 - tja.header.offset.unwrap(),
                    note.start as f32 - tja.header.offset.unwrap() + note.duration() as f32,
                    0,
                ));
                0
            } else if note.variant() == TaikoNoteVariant::Both
                && (note.note_type == TaikoNoteType::Balloon
                    || note.note_type == TaikoNoteType::Yam)
            {
                chart.push((
                    7,
                    note.start as f32 - tja.header.offset.unwrap(),
                    note.start as f32 - tja.header.offset.unwrap() + note.duration() as f32,
                    note.volume(),
                ));
                0
            } else {
                0
            };
            if t != 0 {
                chart.push((
                    t,
                    note.start as f32 - tja.header.offset.unwrap(),
                    note.start as f32 - tja.header.offset.unwrap() + note.duration() as f32,
                    0,
                ));
            }
        }
        ryan_chart.data.push(RyanChartInner {
            course: course.course,
            chart,
        });

        let json = serde_json::to_string_pretty(&ryan_chart).unwrap();
        fs::write(args.get(2).unwrap_or(&"output.json".to_string()), json).unwrap();
    }
}

pub fn read_utf8_or_shiftjis<P: AsRef<std::path::Path>>(path: P) -> Result<String, std::io::Error> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)?;
    let encoding = if !encoding_rs::UTF_8.decode_without_bom_handling(&bytes).1 {
        encoding_rs::UTF_8
    } else {
        encoding_rs::SHIFT_JIS
    };

    let (cow, _, _) = encoding.decode(&bytes);
    Ok(cow.into_owned())
}
