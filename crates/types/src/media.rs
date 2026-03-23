use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// All metadata needed to load and play a track.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackInfo {
    pub id: i64,
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_frames: u64,
    pub sample_rate: u32,
    pub channels: u8,
    pub bpm: Option<f64>,
    pub key: Option<String>,  // Camelot notation e.g. "8A"
}
