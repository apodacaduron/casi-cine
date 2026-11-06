use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub root: String,
    pub created_at: String,
    pub updated_at: String,
    pub aspect_ratio: String,
    pub format_confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: i64,
    pub path: String,
    pub filename: String,
    pub kind: String,
    pub size_bytes: i64,
    pub duration_seconds: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub captured_at: Option<String>,
    pub timestamp_source: String,
    pub timestamp_uncertain: bool,
    pub duplicate_of: Option<i64>,
    pub manual_timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineItem {
    pub id: String,
    pub media_id: i64,
    pub source_path: String,
    pub kind: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub duration_seconds: f64,
    pub captured_at: Option<String>,
    pub locked: bool,
    pub excluded: bool,
    pub output_aspect: String,
    pub audio_policy: String,
    pub framing_mode: String,
    pub focal_x: f64,
    pub focal_y: f64,
}

#[derive(Debug, Serialize)]
pub struct Dependency {
    pub name: String,
    pub available: bool,
    pub path: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobProgress {
    pub stage: String,
    pub fraction: f64,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CleanupResult {
    pub bytes_freed: u64,
    pub files_removed: u64,
    pub repaired_references: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CullingItem {
    pub media_id: i64,
    pub source_path: String,
    pub filename: String,
    pub kind: String,
    pub selected: bool,
    pub user_override: Option<bool>,
    pub group_id: String,
    pub group_kind: String,
    pub score: f64,
    pub reason: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    pub duration_seconds: f64,
    pub captured_at: Option<String>,
    pub sharpness: Option<f64>,
    pub exposure: Option<f64>,
    pub visual_hash: Option<String>,
    pub has_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CullingReport {
    pub items: Vec<CullingItem>,
    pub selected_count: usize,
    pub rejected_count: usize,
    pub groups: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicEngineStatus {
    pub installed: bool,
    pub root: Option<String>,
    pub model_bytes: u64,
    pub engine: String,
    pub license: String,
    pub memory_note: String,
    pub download_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicRequest {
    pub mood: String,
    pub duration_seconds: u32,
    pub candidates: u8,
    pub seed: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicTrack {
    pub path: String,
    pub prompt: String,
    pub mood: String,
    pub duration_seconds: u32,
    pub seed: u32,
    pub cached: bool,
    pub engine: String,
    pub disclaimer: String,
}
