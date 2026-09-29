use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item};

use crate::types::LoopMode;

#[derive(Debug, Clone)]
pub struct Config {
    pub cache_dir: PathBuf,
    pub api_key: Option<String>,
    pub volume: Option<f64>,
    /// Pinned worker Python (e.g. a venv), overriding the .venv auto-discovery.
    pub worker_python: Option<String>,
    /// Play every track at a similar loudness (`level_loudness`, default on).
    pub level_loudness: bool,
    /// Folder of `config.toml` and `pending-move.json` (the service's state
    /// dir); `None` keeps settings changes in memory only.
    pub settings_dir: Option<PathBuf>,
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
            level_loudness: true,
            settings_dir: None,
        }
    }

    /// `<settings_dir>/config.toml`, where settings changes are saved.
    pub fn config_file(&self) -> Option<PathBuf> {
        self.settings_dir.as_ref().map(|d| d.join("config.toml"))
    }

    /// `<settings_dir>/pending-move.json`: a `library.move` for the next start.
    pub fn pending_move_file(&self) -> Option<PathBuf> {
        self.settings_dir
            .as_ref()
            .map(|d| d.join("pending-move.json"))
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
    pub level_loudness: Option<bool>,
}

impl FileConfig {
    /// Load the config file, returning defaults if it is missing or invalid.
    pub fn load() -> Self {
        match Self::path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    /// `load` from `path`.
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("yplayer").join("config.toml"))
    }
}

/// A `config.toml` key the service writes.
#[derive(Debug, Clone, Copy)]
pub enum SettingEdit<'a> {
    LevelLoudness(bool),
    /// `None` removes the key.
    ApiKey(Option<&'a str>),
    CacheDir(&'a Path),
}

/// Apply `edits` to the config file at `path` (created if missing), keeping
/// comments, formatting and unknown keys; written to a temp file (0600) and
/// renamed. A file that is not valid TOML is left alone (an error).
pub fn save_settings(path: &Path, edits: &[SettingEdit]) -> std::io::Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut doc: DocumentMut = text.parse().map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} is not valid TOML: {e}", path.display()),
        )
    })?;
    for edit in edits {
        match *edit {
            SettingEdit::LevelLoudness(on) => set_key(&mut doc, "level_loudness", on.into()),
            SettingEdit::ApiKey(Some(key)) => set_key(&mut doc, "api_key", key.into()),
            SettingEdit::ApiKey(None) => {
                doc.remove("api_key");
            }
            SettingEdit::CacheDir(dir) => {
                set_key(&mut doc, "cache_dir", dir.to_string_lossy().as_ref().into())
            }
        }
    }
    write_private(path, doc.to_string().as_bytes())
}

/// Set a top-level key, keeping the comments around an existing value.
fn set_key(doc: &mut DocumentMut, key: &str, mut value: toml_edit::Value) {
    match doc.get_mut(key).and_then(Item::as_value_mut) {
        Some(old) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        None => {
            doc.insert(key, Item::Value(value));
        }
    }
}

/// Write `bytes` to `<path>.tmp` (created new with mode 0600, never
/// followed) and rename it over `path`.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    // A leftover from a crash is unlinked (unlink never follows a symlink).
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// A `library.move` request, applied when the service next starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingMove {
    pub from: PathBuf,
    pub to: PathBuf,
}

impl PendingMove {
    /// Write it to `path` (0600).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string(self).map_err(std::io::Error::other)?;
        write_private(path, text.as_bytes())
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
        assert!(cfg.level_loudness.is_none());
    }

    const HAND_WRITTEN: &str = r#"# yplayer settings
worker_python = "/repo/.venv/bin/python3"  # written by install.sh

# My key.
api_key = "old-key"   # from the console
level_loudness = true # even out
future_option = [1, 2]

[experimental]
thing = "値"
"#;

    #[test]
    fn save_settings_keeps_comments_and_unknown_keys() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, HAND_WRITTEN).unwrap();

        let music = Path::new("/Volumes/外付け/yt-audio");
        save_settings(
            &path,
            &[
                SettingEdit::LevelLoudness(false),
                SettingEdit::ApiKey(Some("new-key")),
                SettingEdit::CacheDir(music),
            ],
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        for kept in [
            "# yplayer settings\n",
            "worker_python = \"/repo/.venv/bin/python3\"  # written by install.sh\n",
            "# My key.\napi_key = \"new-key\"   # from the console\n",
            "level_loudness = false # even out\n",
            "future_option = [1, 2]\n",
            "[experimental]\nthing = \"値\"\n",
        ] {
            assert!(text.contains(kept), "{kept:?} missing from:\n{text}");
        }
        // A new key stays top-level, before the table.
        assert!(text.find("cache_dir").unwrap() < text.find("[experimental]").unwrap());
        let cfg = FileConfig::load_from(&path);
        assert_eq!(cfg.level_loudness, Some(false));
        assert_eq!(cfg.api_key.as_deref(), Some("new-key"));
        assert_eq!(cfg.cache_dir.as_deref(), Some("/Volumes/外付け/yt-audio"));
        assert_eq!(
            cfg.worker_python.as_deref(),
            Some("/repo/.venv/bin/python3")
        );
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(!dir.path().join("config.toml.tmp").exists());

        save_settings(&path, &[SettingEdit::ApiKey(None)]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"), "{text}");
        assert!(text.contains("level_loudness = false # even out\n"));
        assert!(FileConfig::load_from(&path).api_key.is_none());
    }

    #[test]
    fn save_settings_creates_a_missing_file_and_refuses_invalid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        save_settings(&path, &[SettingEdit::LevelLoudness(true)]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "level_loudness = true\n"
        );

        std::fs::write(&path, "api_key = \"unterminated\n").unwrap();
        assert!(save_settings(&path, &[SettingEdit::LevelLoudness(false)]).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "api_key = \"unterminated\n"
        );
    }

    #[test]
    fn pending_move_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pending-move.json");
        let request = PendingMove {
            from: "/Users/me/Music/yt-audio".into(),
            to: "/Volumes/音楽/yt-audio".into(),
        };
        request.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::json!({"from": "/Users/me/Music/yt-audio", "to": "/Volumes/音楽/yt-audio"})
        );
        assert_eq!(serde_json::from_str::<PendingMove>(&text).unwrap(), request);
    }
}
