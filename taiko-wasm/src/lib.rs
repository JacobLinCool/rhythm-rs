mod utils;

use serde_json::json;
use taiko_core::{DefaultTaikoEngine, GameSource, Hit, InputState, TaikoEngine};
use tja::{TJAParser, TJA};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct Engine(DefaultTaikoEngine);

#[wasm_bindgen]
pub fn parse(tja: String) -> String {
    let parser = TJAParser::new();
    let tja = parser.parse(&tja).unwrap();
    let tja = json!(tja);
    tja.to_string()
}

#[wasm_bindgen]
pub fn init(tja: String, difficulty: u8) -> Engine {
    let tja = serde_json::from_str::<TJA>(&tja).unwrap();
    let course = tja
        .courses
        .clone()
        .into_iter()
        .find(|c| c.course == difficulty as i32)
        .unwrap();

    let src = GameSource {
        difficulty,
        level: course.level.unwrap() as u8,
        scoreinit: course.scoreinit,
        scorediff: course.scorediff,
        notes: course.notes.clone(),
    };

    let engine = DefaultTaikoEngine::new(src);

    Engine(engine)
}

#[wasm_bindgen]
pub fn update(engine: &mut Engine, time: f64, hit: Option<u8>) -> String {
    let engine = &mut engine.0;
    let input = InputState {
        time,
        hit: hit.map(|h| match h {
            0 => Hit::Don,
            1 => Hit::Kat,
            _ => unreachable!(),
        }),
    };
    let mut out = engine.forward(input);
    for note in out.display.iter_mut() {
        if let Some(pos) = note.position(time) {
            note.visible_start = pos.0;
            note.visible_end = pos.1;
        } else {
            note.visible_start = 0.0;
            note.visible_end = 0.0;
        }
    }
    let out = json!(out);
    out.to_string()
}
