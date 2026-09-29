use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::types::LoopMode;

#[derive(Debug, Clone)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub api_key: Option<String>,
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

    pub fn state_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("yplayer")
    }

    /// Service socket: `YPLAY_SOCKET`, else `<state_dir>/yplay.sock`.
    pub fn socket_path() -> PathBuf {
        std::env::var_os("YPLAY_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| Self::state_dir().join("yplay.sock"))
    }

    pub fn worker_log_path(state_dir: &Path) -> PathBuf {
        state_dir.join("worker.log")
    }

    pub fn update_stamp_path(state_dir: &Path) -> PathBuf {
        state_dir.join("ytdlp_update_check")
    }
}

/// User configuration loaded from `~/.config/yplayer/config.toml`. Every field
/// is optional; the CLI overrides whatever is set here.
#[derive(Debug, Default, Deserialize)]
pub struct FileConfig {
    pub cache_dir: Option<String>,
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
    pub loop_mode: Option<LoopMode>,
    #[serde(default)]
    pub last_track_id: Option<String>,
    #[serde(default)]
    pub last_position: Option<f64>,
    #[serde(default)]
    pub last_context: Option<serde_json::Value>,
}

impl SessionState {
    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Written to `<path>.tmp` (created new, never followed) and renamed over
    /// `path`.
    pub fn save(&self, path: &std::path::Path) {
        use std::io::Write;
        let Ok(text) = serde_json::to_string_pretty(self) else {
            return;
        };
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        // A leftover from a crash is unlinked (unlink never follows a symlink).
        let _ = std::fs::remove_file(&tmp);
        let written = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if written.is_err() || std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
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
            loop_mode: Some(LoopMode::All),
            last_track_id: Some("abc123".to_string()),
            last_position: Some(42.5),
            last_context: Some(serde_json::json!({"album_id": 3})),
        };
        state.save(&path);

        let loaded = SessionState::load(&path);
        assert_eq!(loaded.volume, Some(65.0));
        assert_eq!(loaded.loop_mode, Some(LoopMode::All));
        assert_eq!(loaded.last_track_id.as_deref(), Some("abc123"));
        assert_eq!(loaded.last_position, Some(42.5));
        assert_eq!(
            loaded.last_context,
            Some(serde_json::json!({"album_id": 3}))
        );
    }

    #[test]
    fn old_session_file_with_sort_mode_still_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"volume": 65.0, "sort_mode": "RecentlyPlayed", "last_track_id": "abc123"}"#,
        )
        .unwrap();

        let loaded = SessionState::load(&path);
        assert_eq!(loaded.volume, Some(65.0));
        assert_eq!(loaded.last_track_id.as_deref(), Some("abc123"));
        assert!(loaded.loop_mode.is_none());
        assert!(loaded.last_position.is_none());
        assert!(loaded.last_context.is_none());
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
        assert!(cfg.cache_dir.is_none());
    }
}
