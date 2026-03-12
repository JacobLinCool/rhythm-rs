use rhythm_chart::{BranchDecisionHint, Tick};
use serde::{Deserialize, Serialize};

pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLibraryDocument {
    pub api_version: u32,
    pub songs: Vec<ResourceSong>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceSong {
    pub source_path: String,
    pub source_id: String,
    pub chart_content_hash: String,
    pub audio_path: String,
    pub audio_id: String,
    pub audio_content_hash: String,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub demo_start_seconds: f64,
    pub courses: Vec<ResourceCourse>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceCourse {
    pub index: usize,
    pub name: String,
    pub level: Option<u8>,
    pub object_count: usize,
    pub branch_segment_count: usize,
    pub base_bpm: Option<f64>,
    pub branch_decisions: Vec<ResourceBranchDecisionPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceBranchDecisionPoint {
    pub segment_id: u32,
    pub decision_tick: Tick,
    pub route_count: u8,
    pub hint: Option<BranchDecisionHint>,
}
