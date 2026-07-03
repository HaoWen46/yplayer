use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::types::SortMode;

#[derive(Debug, Clone)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub api_key: Option<String>,
    pub format: String,
    pub native: bool,
    pub embed_meta: bool,
    pub audio_quality: Option<String>,
    pub player: Option<String>,
    pub volume: Option<f64>,
    /// Pinned worker Python (e.g. a venv), overriding the .venv auto-discovery.
    pub worker_python: Option<String>,
}

impl Config {
    pub fn new(cache_dir: Option<String>, api_key: Option<String>) -> Self {
        let cache_dir = cache_dir.map(PathBuf::from).unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("Music")
                .join("yt-audio")
        });

        let api_key = api_key.or_else(|| std::env::var("YT_API_KEY").ok());

        Self {
            cache_dir,
            api_key,
            format: "mp3".to_string(),
            native: false,
            embed_meta: true,
            audio_quality: None,
            player: None,
            volume: None,
            worker_python: None,
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.cache_dir.join(".yplayer.db")
    }

    /// Where per-session state (volume, sort, last track) is stored.
    pub fn state_path(&self) -> PathBuf {
        self.cache_dir.join(".yplayer_state.json")
    }
}

/// User configuration loaded from `~/.config/yplayer/config.toml`. Every field
/// is optional; the CLI overrides whatever is set here.
#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub cache_dir: Option<String>,
    pub format: Option<String>,
    pub volume: Option<f64>,
    pub api_key: Option<String>,
    pub worker_python: Option<String>,
}

impl FileConfig {
    /// Load the config file, returning defaults if it is missing or invalid.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("yplayer").join("config.toml"))
    }
}

/// Mutable per-session state, persisted to the cache dir on quit so volume,
/// sort order, and the last track survive across launches.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub volume: Option<f64>,
    #[serde(default)]
    pub sort_mode: Option<SortMode>,
    #[serde(default)]
    pub last_track_id: Option<String>,
}

impl SessionState {
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: &std::path::Path) {
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_state_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = SessionState {
            volume: Some(65.0),
            sort_mode: Some(SortMode::RecentlyPlayed),
            last_track_id: Some("abc123".to_string()),
        };
        state.save(&path);

        let loaded = SessionState::load(&path);
        assert_eq!(loaded.volume, Some(65.0));
        assert_eq!(loaded.sort_mode, Some(SortMode::RecentlyPlayed));
        assert_eq!(loaded.last_track_id.as_deref(), Some("abc123"));
    }

    #[test]
    fn missing_state_file_is_default() {
        let loaded = SessionState::load(std::path::Path::new("/nonexistent/state.json"));
        assert!(loaded.volume.is_none());
        assert!(loaded.last_track_id.is_none());
    }

    #[test]
    fn file_config_parses_partial_toml() {
        let cfg: FileConfig = toml::from_str("volume = 0.5\nformat = \"opus\"\n").unwrap();
        assert_eq!(cfg.volume, Some(0.5));
        assert_eq!(cfg.format.as_deref(), Some("opus"));
        assert!(cfg.cache_dir.is_none());
    }
}
