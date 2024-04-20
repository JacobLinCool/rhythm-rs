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
    chart: Vec<(i32, f32)>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: {} <input.tja> [output.json]", args[0]);
        return;
    }
    let filepath = &args.get(1).unwrap();
    let raw = fs::read_to_string(filepath).unwrap();

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
                && (note.note_type == TaikoNoteType::SmallCombo
                    || note.note_type == TaikoNoteType::BigCombo)
            {
                for i in (0..note.duration() as usize).step_by(80) {
                    chart.push((
                        1,
                        note.start as f32 / 1000.0 - tja.header.offset.unwrap() + i as f32 / 1000.0,
                    ));
                }
                0
            } else if note.variant() == TaikoNoteVariant::Both
                && (note.note_type == TaikoNoteType::Balloon
                    || note.note_type == TaikoNoteType::Yam)
            {
                let step = note.duration() as f32 / note.volume as f32;
                for i in 0..note.volume() {
                    chart.push((
                        1,
                        note.start as f32 / 1000.0 - tja.header.offset.unwrap()
                            + i as f32 * step / 1000.0,
                    ));
                }
                0
            } else {
                0
            };
            if t != 0 {
                chart.push((t, note.start as f32 / 1000.0 - tja.header.offset.unwrap()));
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
