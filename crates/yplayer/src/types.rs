use serde::{Deserialize, Serialize};

// DB TEXT: "complete" | "downloading" | "failed"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackState {
    Complete,
    Downloading,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String, // YouTube video id (11 chars) or legacy id
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<i64>, // seconds
    pub webpage_url: Option<String>,
    pub audio_path: Option<String>, // DB stores '' for unknown; exposed as None
    pub format: Option<String>,     // file extension, e.g. "webm", "m4a", "mp3"
    pub file_size: Option<i64>,
    pub added_at: Option<i64>, // unix seconds
    pub last_played: Option<i64>,
    pub state: TrackState,
    pub thumb_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub track_ids: Vec<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoopMode {
    None,
    Single,
    All,
    Shuffle,
}
