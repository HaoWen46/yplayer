use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub uploader: Option<String>,
    pub duration: Option<i64>,
    pub webpage_url: Option<String>,
    pub audio_path: Option<String>,
    pub format: Option<String>,
    pub file_size: Option<i64>,
    pub added_at: Option<i64>,
    pub last_played: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Album {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub track_count: usize,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumTrack {
    pub album_id: i64,
    pub track_id: String,
    pub position: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopMode {
    None,
    Single,
    All,
    Shuffle,
}

impl LoopMode {
    pub fn toggle(self) -> Self {
        match self {
            LoopMode::None => LoopMode::Single,
            LoopMode::Single => LoopMode::All,
            LoopMode::All => LoopMode::Shuffle,
            LoopMode::Shuffle => LoopMode::None,
        }
    }

    pub fn status_text(&self) -> &'static str {
        match self {
            LoopMode::None => "",
            LoopMode::Single => "[LOOP: SINGLE]",
            LoopMode::All => "[LOOP: ALL]",
            LoopMode::Shuffle => "[SHUFFLE]",
        }
    }
}

/// How the library list is sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortMode {
    Title,
    RecentlyPlayed,
    RecentlyAdded,
    Uploader,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::Title => SortMode::RecentlyPlayed,
            SortMode::RecentlyPlayed => SortMode::RecentlyAdded,
            SortMode::RecentlyAdded => SortMode::Uploader,
            SortMode::Uploader => SortMode::Title,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SortMode::Title => "A-Z",
            SortMode::RecentlyPlayed => "Recently Played",
            SortMode::RecentlyAdded => "Recently Added",
            SortMode::Uploader => "By Artist",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ViewMode {
    Library,
    Albums,
    AlbumDetail {
        album_id: i64,
        album_name: String,
    },
    Playlist {
        url: String,
    },
    Search,
    /// URL input for in-TUI download
    DownloadInput,
    /// Rename input for the currently selected track
    RenameInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackState {
    pub track_index: usize,
    pub file_path: String,
    pub paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
}
