//! This example demonstrates how to convert TJA file to a hit sequence format, in 10 ms resolution.

use rhythm_core::Note;
use serde::Serialize;
use glob::glob;
use std::{fs, path::Path};
use tja::{TJAParser, TaikoNoteVariant};

#[derive(Serialize)]
struct HitSeq {
    hitseq: Vec<i8>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage: {} <input> <output-dir>", args[0]);
        return;
    }
    let matcher = args.get(1).unwrap();
    let outdir = args.get(2).unwrap();

    let targets = glob(matcher).unwrap().filter_map(Result::ok);
    for target in targets {
        let filename = target.file_name().unwrap().to_str().unwrap();
        let noext = Path::new(filename).file_stem().unwrap().to_str().unwrap();
        let out = format!("{}/{}.json", outdir, noext);
        let tja = read_utf8_or_shiftjis(&target).unwrap();
        run(&tja, &out);
    }
}

fn run(tja: &str, out: &str) {
    let parser = TJAParser::new();
    let mut tja = parser.parse(tja).unwrap();
    tja.courses.sort_by_key(|course| 10 - course.course);

    let mut hitseq = Vec::<i8>::new();

    for course in tja.courses.iter() {
        let difficulty = course.course * 3 + course.level.unwrap_or(0);
        for note in course.notes.iter() {
            if note.variant() == TaikoNoteVariant::Don || note.variant() == TaikoNoteVariant::Kat {
                let time =
                    ((note.start() - tja.header.offset.unwrap_or(0.0) as f64) * 100.0) as usize;
                if hitseq.len() <= time {
                    hitseq.resize(time + 1, 0);
                }
                hitseq[time] = difficulty as i8;
            } else if note.variant() == TaikoNoteVariant::Both {
                let start =
                    ((note.start() - tja.header.offset.unwrap_or(0.0) as f64) * 100.0) as usize;
                let end = ((note.start() + note.duration()
                    - tja.header.offset.unwrap_or(0.0) as f64)
                    * 100.0) as usize;
                if hitseq.len() <= end {
                    hitseq.resize(end + 1, 0);
                }
                for i in start..=end {
                    if hitseq[i] == 0 {
                        hitseq[i] = 30 + course.course as i8;
                    }
                }
            }
        }
    }

    let hitseq = HitSeq { hitseq };
    let json = serde_json::to_string(&hitseq).unwrap();
    fs::write(out, json).unwrap();
}

pub fn read_utf8_or_shiftjis<P: AsRef<std::path::Path>>(path: P) -> Result<String, String> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).unwrap();
    let encoding = if !encoding_rs::UTF_8.decode_without_bom_handling(&bytes).1 {
        encoding_rs::UTF_8
    } else {
        encoding_rs::SHIFT_JIS
    };

    let (cow, _, _) = encoding.decode(&bytes);
    Ok(cow.into_owned())
}