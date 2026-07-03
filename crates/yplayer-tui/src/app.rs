use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::stdout;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::cache::index::CacheIndex;
use crate::cache::scanner;
use crate::config::{Config, SessionState};
use crate::download::worker::WorkerHandle;
use crate::events::{self, Action};
use crate::player::mpv::MpvPlayer;
use crate::types::{Album, LoopMode, SortMode, Track, ViewMode};
use crate::ui;

/// Result of a background download task, delivered to the event loop.
#[derive(Debug)]
pub enum WorkerEvent {
    DownloadDone {
        track: Track,
    },
    DownloadFailed {
        url: String,
        error: String,
    },
    LyricsReady {
        track_id: String,
        // Some = a definitive answer (empty vec means "genuinely none"), which is
        // cached. None = a transient fetch error, which is not cached so it retries.
        result: Option<Vec<(f64, String)>>,
    },
}

/// Parse LRC text into `(seconds, line)` pairs, sorted by time. Metadata tags
/// like `[ar:...]` are skipped; a line may carry several timestamps.
pub fn parse_lrc(lrc: &str) -> Vec<(f64, String)> {
    let mut out: Vec<(f64, String)> = Vec::new();
    for line in lrc.lines() {
        let mut rest = line;
        let mut times: Vec<f64> = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else {
                break;
            };
            if let Some(t) = parse_lrc_time(&stripped[..close]) {
                times.push(t);
            }
            rest = &stripped[close + 1..];
        }
        let text = rest.trim();
        for t in times {
            out.push((t, text.to_string()));
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Parse an LRC timestamp tag like `mm:ss.xx`; returns None for metadata tags.
fn parse_lrc_time(tag: &str) -> Option<f64> {
    let (m, s) = tag.split_once(':')?;
    let mins: f64 = m.trim().parse().ok()?;
    let secs: f64 = s.trim().parse().ok()?;
    Some(mins * 60.0 + secs)
}

/// Severity of a transient status message; controls its color in the footer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

pub struct App {
    pub mode: ViewMode,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub selection: usize,
    pub playing: Option<Track>,
    pub loop_mode: LoopMode,
    pub sort_mode: SortMode,
    // Authoritative session volume (0..100); persists across tracks and launches.
    pub volume: f64,
    pub player: MpvPlayer,
    pub db: CacheIndex,
    pub config: Config,
    pub should_quit: bool,
    // Set whenever visible state changes; gates redraws so the loop is idle-quiet.
    pub dirty: bool,
    // Search state
    pub search_query: String,
    pub search_results: Vec<Track>,
    pub search_selection: usize,
    // Download input state
    pub download_input: String,
    // Rename input state
    pub rename_input: String,
    // Previous mode for returning from overlays
    prev_mode: Option<ViewMode>,
    // Status message (shown briefly)
    pub status_msg: Option<String>,
    pub status_severity: Severity,
    status_msg_until: Option<std::time::Instant>,
    // Whether the help overlay is open.
    pub show_help: bool,
    // Synced lyrics for the current track (None = fetching, Some(empty) = none found).
    pub lyrics: Option<Vec<(f64, String)>>,
    lyrics_track_id: Option<String>,
    lyrics_cache: std::collections::HashMap<String, Vec<(f64, String)>>,
    lyrics_inflight: std::collections::HashSet<String>,
    pub show_lyrics: bool,
    // Count of lyric lines whose timestamp has passed (0 = before the first).
    pub current_lyric: usize,
    // Delete confirmation: Some(Instant) = waiting for 2nd press, expires after 3s
    pub confirm_delete_until: Option<std::time::Instant>,
    // Background download tasks report results here; run() owns the receiver.
    worker_tx: mpsc::UnboundedSender<WorkerEvent>,
    worker_rx: Option<mpsc::UnboundedReceiver<WorkerEvent>>,
    // Persistent worker actor handle; set by run() (needs a Tokio runtime).
    worker: Option<WorkerHandle>,
    // A separate worker for lyrics, so a small HTTP GET never queues behind a
    // long download (and vice-versa) on the single serial worker.
    lyrics_worker: Option<WorkerHandle>,
}

impl App {
    pub fn new(cfg: Config, db: CacheIndex) -> Self {
        let (worker_tx, worker_rx) = mpsc::unbounded_channel();
        let volume = cfg
            .volume
            .map(|v| (v * 100.0).clamp(0.0, 100.0))
            .unwrap_or(100.0);
        Self {
            mode: ViewMode::Library,
            tracks: Vec::new(),
            albums: Vec::new(),
            selection: 0,
            playing: None,
            loop_mode: LoopMode::None,
            sort_mode: SortMode::Title,
            volume,
            player: MpvPlayer::new(),
            db,
            config: cfg,
            should_quit: false,
            dirty: true,
            search_query: String::new(),
            search_results: Vec::new(),
            search_selection: 0,
            download_input: String::new(),
            rename_input: String::new(),
            prev_mode: None,
            status_msg: None,
            status_severity: Severity::Info,
            status_msg_until: None,
            show_help: false,
            lyrics: None,
            lyrics_track_id: None,
            lyrics_cache: std::collections::HashMap::new(),
            lyrics_inflight: std::collections::HashSet::new(),
            show_lyrics: false,
            current_lyric: 0,
            confirm_delete_until: None,
            worker_tx,
            worker_rx: Some(worker_rx),
            worker: None,
            lyrics_worker: None,
        }
    }

    pub fn load_library(&mut self) {
        self.tracks = self
            .db
            .list_tracks_sorted(self.sort_mode)
            .unwrap_or_default();
        // Prune tracks whose audio files no longer exist
        self.tracks.retain(|t| {
            t.audio_path
                .as_ref()
                .map(|p| std::path::Path::new(p).exists())
                .unwrap_or(false)
        });
        // Keep the selection in range: retain() may have shrunk the list below it.
        if self.selection >= self.tracks.len() {
            self.selection = self.tracks.len().saturating_sub(1);
        }
    }

    pub fn load_albums(&mut self) {
        self.albums = self.db.list_albums().unwrap_or_default();
    }

    pub fn playback_status_text(&self) -> &'static str {
        if !self.player.is_playing() {
            ""
        } else if self.player.is_paused {
            "[PAUSED]"
        } else {
            "[PLAYING]"
        }
    }

    pub fn item_count(&self) -> usize {
        match self.mode {
            ViewMode::Albums => self.albums.len(),
            _ => self.tracks.len(),
        }
    }

    /// Position of the currently-playing track within the visible `tracks`, if
    /// present. Playback identity is keyed by track id, so list swaps (sort,
    /// album navigation) never leave a stale index behind.
    pub fn playing_index(&self) -> Option<usize> {
        let id = &self.playing.as_ref()?.id;
        self.tracks.iter().position(|t| &t.id == id)
    }

    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.set_status_sev(msg, Severity::Info);
    }

    pub fn set_status_sev(&mut self, msg: impl Into<String>, sev: Severity) {
        self.status_msg = Some(msg.into());
        self.status_severity = sev;
        self.status_msg_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
    }

    fn clear_expired_status(&mut self) {
        if let Some(until) = self.status_msg_until
            && std::time::Instant::now() >= until
        {
            self.status_msg = None;
            self.status_msg_until = None;
        }
        // Also expire delete confirmation
        if let Some(until) = self.confirm_delete_until
            && std::time::Instant::now() >= until
        {
            self.confirm_delete_until = None;
            if self.status_msg.as_deref() == Some("Press d again to delete, Esc to cancel") {
                self.status_msg = None;
                self.status_msg_until = None;
            }
        }
    }

    pub async fn handle_action(&mut self, action: Action) {
        // Any non-tick action is user intent that can change the view.
        if !matches!(action, Action::Tick) {
            self.dirty = true;
        }
        match action {
            Action::Quit => {
                match self.mode {
                    ViewMode::Search => {
                        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                        self.search_query.clear();
                        self.search_results.clear();
                        self.search_selection = 0;
                    }
                    ViewMode::DownloadInput => {
                        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                        self.download_input.clear();
                    }
                    ViewMode::RenameInput => {
                        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                        self.rename_input.clear();
                    }
                    _ => {
                        if self.confirm_delete_until.is_some() {
                            // A delete is pending: Esc/quit cancels the confirmation
                            // instead of exiting the whole app.
                            self.confirm_delete_until = None;
                            if self.status_msg.as_deref()
                                == Some("Press d again to delete, Esc to cancel")
                            {
                                self.status_msg = None;
                                self.status_msg_until = None;
                            }
                        } else {
                            self.should_quit = true;
                        }
                    }
                }
            }
            Action::MoveUp => {
                if self.mode == ViewMode::Search {
                    if self.search_selection > 0 {
                        self.search_selection -= 1;
                    }
                } else if self.selection > 0 {
                    self.selection -= 1;
                }
            }
            Action::MoveDown => {
                if self.mode == ViewMode::Search {
                    if self.search_selection + 1 < self.search_results.len() {
                        self.search_selection += 1;
                    }
                } else {
                    let max = self.item_count().saturating_sub(1);
                    if self.selection < max {
                        self.selection += 1;
                    }
                }
            }
            Action::PageUp => {
                self.selection = self.selection.saturating_sub(10);
            }
            Action::PageDown => {
                let max = self.item_count().saturating_sub(1);
                self.selection = (self.selection + 10).min(max);
            }
            Action::Select => {
                if self.mode == ViewMode::Search {
                    if let Some(result) = self.search_results.get(self.search_selection) {
                        let target_id = result.id.clone();
                        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                        self.search_query.clear();
                        self.search_results.clear();
                        self.search_selection = 0;
                        if let Some(pos) = self.tracks.iter().position(|t| t.id == target_id) {
                            self.selection = pos;
                            self.play_track(pos).await;
                        }
                    }
                } else if self.mode == ViewMode::Albums {
                    self.enter_album().await;
                } else {
                    self.play_track(self.selection).await;
                }
            }
            Action::PauseResume => {
                if self.player.is_playing() {
                    let _ = self.player.pause_resume().await;
                }
            }
            Action::Stop => {
                self.player.stop().await;
                self.playing = None;
            }
            Action::ToggleLoop => {
                self.loop_mode = self.loop_mode.toggle();
            }
            Action::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            Action::CycleSortMode => {
                if matches!(self.mode, ViewMode::Library | ViewMode::Search) {
                    self.sort_mode = self.sort_mode.next();
                    self.load_library();
                    self.selection = 0;
                    self.set_status(format!("Sort: {}", self.sort_mode.label()));
                }
            }
            Action::SwitchToAlbums => {
                if self.mode == ViewMode::Library {
                    self.load_albums();
                    if !self.albums.is_empty() {
                        self.mode = ViewMode::Albums;
                        self.selection = 0;
                    }
                }
            }
            Action::GoBack => match &self.mode {
                ViewMode::AlbumDetail { .. } => {
                    self.load_albums();
                    self.mode = ViewMode::Albums;
                    self.selection = 0;
                }
                ViewMode::Albums => {
                    self.mode = ViewMode::Library;
                    self.load_library();
                    self.selection = 0;
                }
                ViewMode::Search => {
                    self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                    self.search_query.clear();
                    self.search_results.clear();
                    self.search_selection = 0;
                }
                ViewMode::DownloadInput => {
                    self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                    self.download_input.clear();
                }
                ViewMode::RenameInput => {
                    self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                    self.rename_input.clear();
                }
                _ => {}
            },
            Action::Delete => {
                if matches!(self.mode, ViewMode::Library | ViewMode::AlbumDetail { .. })
                    && !self.tracks.is_empty()
                {
                    if self.confirm_delete_until.is_some() {
                        // Second press — execute
                        self.confirm_delete_until = None;
                        self.status_msg = None;
                        self.delete_selected().await;
                    } else {
                        // First press — request confirmation
                        self.confirm_delete_until =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
                        self.status_msg =
                            Some("Press d again to delete, Esc to cancel".to_string());
                        self.status_severity = Severity::Warn;
                        self.status_msg_until =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
                    }
                }
            }
            Action::Rename => {
                if matches!(self.mode, ViewMode::Library | ViewMode::AlbumDetail { .. })
                    && !self.tracks.is_empty()
                {
                    let current_title = self.tracks[self.selection].title.clone();
                    self.rename_input = current_title;
                    self.prev_mode = Some(self.mode.clone());
                    self.mode = ViewMode::RenameInput;
                }
            }
            Action::StartSearch => {
                if self.mode != ViewMode::Search {
                    self.prev_mode = Some(self.mode.clone());
                    self.mode = ViewMode::Search;
                    self.search_query.clear();
                    self.search_results = self.tracks.clone();
                    self.search_selection = 0;
                }
            }
            Action::StartDownload => {
                if matches!(self.mode, ViewMode::Library | ViewMode::AlbumDetail { .. }) {
                    self.prev_mode = Some(self.mode.clone());
                    self.mode = ViewMode::DownloadInput;
                    self.download_input.clear();
                }
            }
            Action::SeekForward => {
                if self.player.is_playing() {
                    let _ = self.player.seek(5.0).await;
                }
            }
            Action::SeekBackward => {
                if self.player.is_playing() {
                    let _ = self.player.seek(-5.0).await;
                }
            }
            Action::VolumeUp => {
                self.volume = (self.volume + 5.0).min(100.0);
                if self.player.is_playing() {
                    let _ = self.player.set_volume(self.volume).await;
                }
            }
            Action::VolumeDown => {
                self.volume = (self.volume - 5.0).max(0.0);
                if self.player.is_playing() {
                    let _ = self.player.set_volume(self.volume).await;
                }
            }
            Action::NextTrack => {
                if self.tracks.is_empty() {
                    return;
                }
                if let Some(idx) = self.playing_index() {
                    let next = (idx + 1) % self.tracks.len();
                    self.selection = next;
                    self.play_track(next).await;
                }
            }
            Action::PrevTrack => {
                if self.tracks.is_empty() {
                    return;
                }
                if let Some(idx) = self.playing_index() {
                    let prev = if idx == 0 {
                        self.tracks.len() - 1
                    } else {
                        idx - 1
                    };
                    self.selection = prev;
                    self.play_track(prev).await;
                }
            }
            Action::ToggleLyrics => {
                self.show_lyrics = !self.show_lyrics;
            }
            Action::Tick => {
                self.player.poll_status().await;
                self.check_loop().await;
                self.clear_expired_status();
                self.update_current_lyric();
                // Redraw only when something is actually animating or expiring,
                // so a fully idle app doesn't repaint.
                if self.player.is_playing()
                    || self.status_msg.is_some()
                    || self.confirm_delete_until.is_some()
                {
                    self.dirty = true;
                }
            }
        }
    }

    /// Handle one terminal event (key or resize). Key dispatch is mode-aware;
    /// a handled key marks the view dirty so the loop repaints exactly once.
    pub async fn on_terminal_event(&mut self, ev: event::Event) {
        match ev {
            event::Event::Key(key) => {
                if key.kind != event::KeyEventKind::Press {
                    return;
                }
                self.dirty = true;
                // While the help overlay is open, any key dismisses it.
                if self.show_help {
                    self.show_help = false;
                    return;
                }
                match self.mode {
                    ViewMode::Search => match key.code {
                        event::KeyCode::Esc => self.handle_action(Action::Quit).await,
                        event::KeyCode::Enter => self.handle_action(Action::Select).await,
                        event::KeyCode::Up => self.handle_action(Action::MoveUp).await,
                        event::KeyCode::Down => self.handle_action(Action::MoveDown).await,
                        _ => self.handle_search_input(key),
                    },
                    ViewMode::DownloadInput => match key.code {
                        event::KeyCode::Esc => {
                            self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                            self.download_input.clear();
                        }
                        event::KeyCode::Enter => self.execute_download(),
                        _ => self.handle_download_input(key),
                    },
                    ViewMode::RenameInput => match key.code {
                        event::KeyCode::Esc => {
                            self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
                            self.rename_input.clear();
                        }
                        event::KeyCode::Enter => self.execute_rename(),
                        _ => self.handle_rename_input(key),
                    },
                    _ => {
                        // Cancel a pending delete confirmation on any non-d/Esc key.
                        let is_d = key.code == event::KeyCode::Char('d');
                        let is_esc = key.code == event::KeyCode::Esc;
                        if !is_d && !is_esc && self.confirm_delete_until.is_some() {
                            self.confirm_delete_until = None;
                            if self.status_msg.as_deref()
                                == Some("Press d again to delete, Esc to cancel")
                            {
                                self.status_msg = None;
                            }
                        }
                        if let Some(action) = events::map_key_public(key) {
                            self.handle_action(action).await;
                        }
                    }
                }
            }
            event::Event::Resize(_, _) => {
                self.dirty = true;
            }
            _ => {}
        }
    }

    async fn play_track(&mut self, idx: usize) {
        if idx >= self.tracks.len() {
            return;
        }
        let track = self.tracks[idx].clone();
        if let Some(ref path) = track.audio_path {
            if !std::path::Path::new(path).exists() {
                self.set_status(format!("File not found: {}", path));
                return;
            }
            match self.player.play(path, Some(self.volume / 100.0)).await {
                Ok(()) => {
                    let _ = self.db.update_last_played(&track.id);
                    self.load_lyrics_for(&track);
                    self.playing = Some(track);
                }
                Err(e) => {
                    self.set_status_sev(format!("Playback error: {}", e), Severity::Error);
                }
            }
        } else {
            self.set_status("No audio file for this track".to_string());
        }
    }

    /// Set up lyrics for a newly-playing track: use the session cache if present,
    /// otherwise clear and fetch in the background.
    fn load_lyrics_for(&mut self, track: &Track) {
        self.current_lyric = 0;
        self.lyrics_track_id = Some(track.id.clone());
        if let Some(cached) = self.lyrics_cache.get(&track.id) {
            self.lyrics = Some(cached.clone());
            return;
        }
        self.lyrics = None; // None = still fetching
        // Don't spawn a second fetch for a track already being fetched.
        if self.lyrics_inflight.contains(&track.id) {
            return;
        }
        let Some(worker) = self.lyrics_worker.clone() else {
            return;
        };
        self.lyrics_inflight.insert(track.id.clone());
        let tx = self.worker_tx.clone();
        let track_id = track.id.clone();
        let name = track.title.clone();
        let artist = track.uploader.clone();
        let duration = track.duration;
        tokio::spawn(async move {
            let result = match worker.lyrics(name, artist, duration).await {
                Ok(Some(lrc)) => Some(parse_lrc(&lrc)), // found
                Ok(None) => Some(Vec::new()),           // definitively none
                Err(_) => None,                         // transient error — don't cache
            };
            let _ = tx.send(WorkerEvent::LyricsReady { track_id, result });
        });
    }

    async fn enter_album(&mut self) {
        if self.selection >= self.albums.len() {
            return;
        }
        let album = &self.albums[self.selection];
        let album_id = album.id;
        let album_name = album.name.clone();

        if let Ok(tracks) = self.db.get_album_tracks(album_id) {
            self.tracks = tracks;
            self.mode = ViewMode::AlbumDetail {
                album_id,
                album_name,
            };
            self.selection = 0;
        }
    }

    async fn delete_selected(&mut self) {
        if self.tracks.is_empty() {
            return;
        }
        let track = &self.tracks[self.selection];

        // Stop if we're deleting the track that's currently playing.
        if self.playing.as_ref().map(|p| p.id.as_str()) == Some(track.id.as_str()) {
            self.player.stop().await;
            self.playing = None;
        }

        // Delete the audio file + sidecar + parent dir if empty
        if let Some(ref path) = track.audio_path {
            let audio_path = std::path::Path::new(path);
            let _ = std::fs::remove_file(audio_path);
            let sidecar = audio_path.with_extension("json");
            let _ = std::fs::remove_file(&sidecar);
            if let Some(parent) = audio_path.parent() {
                let meta = parent.join("meta.json");
                let _ = std::fs::remove_file(&meta);
                let _ = std::fs::remove_dir(parent); // only removes if empty
            }
        }

        let track_id = track.id.clone();
        let track_title = track.title.clone();
        let _ = self.db.delete_track(&track_id);

        self.tracks.remove(self.selection);
        if self.selection >= self.tracks.len() && self.selection > 0 {
            self.selection -= 1;
        }
        // No index fixup needed: playback identity is keyed by track id.

        self.set_status(format!("Deleted: {}", track_title));
    }

    async fn check_loop(&mut self) {
        if self.playing.is_none() {
            return;
        }
        if self.player.is_playing() {
            return;
        }

        match self.loop_mode {
            LoopMode::Single => match self.playing_index() {
                Some(idx) => self.play_track(idx).await,
                None => self.playing = None,
            },
            LoopMode::All => {
                if self.tracks.is_empty() {
                    self.playing = None;
                    return;
                }
                match self.playing_index() {
                    Some(idx) => {
                        let next = (idx + 1) % self.tracks.len();
                        self.selection = next;
                        self.play_track(next).await;
                    }
                    None => self.playing = None,
                }
            }
            LoopMode::Shuffle => {
                if !self.tracks.is_empty() {
                    let next = rand::random_range(0..self.tracks.len());
                    self.selection = next;
                    self.play_track(next).await;
                }
            }
            LoopMode::None => {
                self.playing = None;
            }
        }
    }

    fn handle_search_input(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            crossterm::event::KeyCode::Char(c) => {
                self.search_query.push(c);
                self.update_search_results();
            }
            crossterm::event::KeyCode::Backspace => {
                self.search_query.pop();
                self.update_search_results();
            }
            _ => {}
        }
    }

    fn update_search_results(&mut self) {
        if self.search_query.is_empty() {
            self.search_results = self.tracks.clone();
        } else {
            use fuzzy_matcher::FuzzyMatcher;
            use fuzzy_matcher::skim::SkimMatcherV2;

            let matcher = SkimMatcherV2::default();
            let query = &self.search_query;

            let mut scored: Vec<(i64, &Track)> = self
                .tracks
                .iter()
                .filter_map(|t| {
                    let title_score = matcher.fuzzy_match(&t.title, query).unwrap_or(0);
                    let uploader_score = t
                        .uploader
                        .as_ref()
                        .and_then(|u| matcher.fuzzy_match(u, query))
                        .unwrap_or(0);
                    let score = title_score.max(uploader_score);
                    if score > 0 { Some((score, t)) } else { None }
                })
                .collect();

            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.search_results = scored.into_iter().map(|(_, t)| t.clone()).collect();
        }
        self.search_selection = 0;
    }

    fn handle_download_input(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            crossterm::event::KeyCode::Char(c) => {
                self.download_input.push(c);
            }
            crossterm::event::KeyCode::Backspace => {
                self.download_input.pop();
            }
            _ => {}
        }
    }

    fn handle_rename_input(&mut self, key: crossterm::event::KeyEvent) {
        match key.code {
            crossterm::event::KeyCode::Char(c) => {
                self.rename_input.push(c);
            }
            crossterm::event::KeyCode::Backspace => {
                self.rename_input.pop();
            }
            _ => {}
        }
    }

    fn execute_download(&mut self) {
        let url = self.download_input.trim().to_string();
        // Return to previous mode first so the UI shows immediately.
        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
        self.download_input.clear();

        if url.is_empty() {
            return;
        }

        self.set_status(format!("Downloading {}…", truncate_chars(&url, 40)));

        // Run the download through the persistent worker actor, off the event
        // loop; the result comes back over the channel so the UI stays responsive.
        let Some(worker) = self.worker.clone() else {
            self.set_status("Worker not ready".to_string());
            return;
        };
        let tx = self.worker_tx.clone();
        tokio::spawn(async move {
            let ev = match worker.download(url.clone()).await {
                Ok(track) => WorkerEvent::DownloadDone { track },
                Err(error) => WorkerEvent::DownloadFailed { url, error },
            };
            let _ = tx.send(ev);
        });
    }

    fn on_worker_event(&mut self, ev: WorkerEvent) {
        self.dirty = true;
        match ev {
            WorkerEvent::DownloadDone { track } => {
                let title = track.title.clone();
                let _ = self.db.upsert_track(&track);
                self.load_library();
                self.set_status(format!("Downloaded: {}", title));
            }
            WorkerEvent::DownloadFailed { url, error } => {
                self.set_status_sev(
                    format!("Download failed: {} ({})", truncate_chars(&url, 30), error),
                    Severity::Error,
                );
            }
            WorkerEvent::LyricsReady { track_id, result } => {
                self.lyrics_inflight.remove(&track_id);
                let is_current = self.lyrics_track_id.as_deref() == Some(track_id.as_str());
                match result {
                    // Definitive answer (found, or genuinely none): cache it.
                    Some(lines) => {
                        self.lyrics_cache.insert(track_id, lines.clone());
                        if is_current {
                            self.lyrics = Some(lines);
                            self.current_lyric = 0;
                        }
                    }
                    // Transient error: don't cache (so it retries next play); show
                    // the empty state rather than a stuck "Fetching…".
                    None => {
                        if is_current {
                            self.lyrics = Some(Vec::new());
                        }
                    }
                }
            }
        }
    }

    /// Recompute which lyric line is active from the playback position; marks the
    /// frame dirty only when it changes (a few times a minute), avoiding flicker.
    fn update_current_lyric(&mut self) {
        let Some(ref lines) = self.lyrics else {
            return;
        };
        if lines.is_empty() {
            return;
        }
        let pos = self.player.position;
        let passed = lines.partition_point(|(t, _)| *t <= pos);
        if passed != self.current_lyric {
            self.current_lyric = passed;
            if self.show_lyrics {
                self.dirty = true;
            }
        }
    }

    fn execute_rename(&mut self) {
        let new_title = self.rename_input.trim().to_string();
        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
        self.rename_input.clear();

        if new_title.is_empty() || self.selection >= self.tracks.len() {
            return;
        }

        let track_id = self.tracks[self.selection].id.clone();
        match self.db.rename_track(&track_id, &new_title) {
            Ok(()) => {
                self.tracks[self.selection].title = new_title.clone();
                self.set_status(format!("Renamed to: {}", new_title));
            }
            Err(e) => {
                self.set_status(format!("Rename failed: {}", e));
            }
        }
    }
}

/// Truncate to at most `max_chars` characters (not bytes), so status messages
/// never slice through a multi-byte UTF-8 codepoint (e.g. CJK titles or emoji).
fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Restore the terminal to its normal state. Safe to call more than once.
fn restore_terminal() -> std::io::Result<()> {
    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

/// RAII guard that restores the terminal on any exit path from `run` —
/// including early `?` returns and panics unwinding through it.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore_terminal();
    }
}

pub async fn run(cfg: Config) -> Result<()> {
    let db = CacheIndex::open(&cfg.db_path())?;

    // Incremental scan: only indexes newly added files
    let count = scanner::scan_and_index(&cfg.cache_dir, &db)?;
    if count > 0 {
        eprintln!("info: indexed {} new tracks", count);
    }

    let mut app = App::new(cfg, db);

    // Restore persisted session state (volume, sort order) before the first load.
    let state = SessionState::load(&app.config.state_path());
    if let Some(v) = state.volume {
        app.volume = v.clamp(0.0, 100.0);
    }
    if let Some(sm) = state.sort_mode {
        app.sort_mode = sm;
    }
    app.load_library();
    if let Some(id) = &state.last_track_id
        && let Some(pos) = app.tracks.iter().position(|t| &t.id == id)
    {
        app.selection = pos;
    }

    // Spawn the persistent worker actors (need the Tokio runtime, so not in App::new).
    // Downloads and lyrics get separate workers so neither blocks the other.
    app.worker = Some(WorkerHandle::spawn(app.config.clone()));
    app.lyrics_worker = Some(WorkerHandle::spawn(app.config.clone()));

    // Restore the terminal before printing any panic message, so a crash never
    // leaves the shell in raw mode / the alternate screen.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        default_hook(info);
    }));

    enable_raw_mode()?;
    // From here on, any return or unwind restores the terminal via Drop.
    let _guard = TerminalGuard;
    stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // run() owns the receiver end; App keeps the sender to hand to spawned tasks.
    let mut worker_rx = app.worker_rx.take().expect("worker_rx already taken");
    let mut reader = event::EventStream::new();
    // Position/loop polling cadence. Much slower than the old 50ms busy-loop —
    // mpv playback only needs a few refreshes per second.
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    while !app.should_quit {
        if app.dirty {
            terminal.draw(|f| ui::draw(f, &app))?;
            app.dirty = false;
        }

        tokio::select! {
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(ev)) => app.on_terminal_event(ev).await,
                    Some(Err(_)) => {}   // transient read error; keep going
                    None => break,        // input stream closed
                }
            }
            _ = tick.tick() => {
                app.handle_action(Action::Tick).await;
            }
            Some(wev) = worker_rx.recv() => {
                app.on_worker_event(wev);
            }
        }
    }

    app.player.stop().await;

    // Persist session state so volume / sort / last track survive the next launch.
    let last_track_id = app
        .playing
        .as_ref()
        .map(|t| t.id.clone())
        .or_else(|| app.tracks.get(app.selection).map(|t| t.id.clone()));
    SessionState {
        volume: Some(app.volume),
        sort_mode: Some(app.sort_mode),
        last_track_id,
    }
    .save(&app.config.state_path());
    // Terminal restored by TerminalGuard's Drop.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::App;
    use super::truncate_chars;
    use crate::cache::index::CacheIndex;
    use crate::config::Config;
    use crate::types::Track;

    fn track(id: &str) -> Track {
        Track {
            id: id.to_string(),
            title: id.to_string(),
            uploader: None,
            duration: None,
            webpage_url: None,
            audio_path: None,
            format: None,
            file_size: None,
            added_at: None,
            last_played: None,
        }
    }

    fn test_app() -> (App, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = CacheIndex::open(&dir.path().join("t.db")).unwrap();
        let cfg = Config::new(Some(dir.path().to_string_lossy().to_string()), None);
        (App::new(cfg, db), dir)
    }

    #[test]
    fn playing_index_follows_track_id_across_reorder() {
        let (mut app, _dir) = test_app();
        app.tracks = vec![track("a"), track("b"), track("c")];
        app.playing = Some(track("b"));
        assert_eq!(app.playing_index(), Some(1));

        // Re-sorting / swapping the visible list must not desync playback.
        app.tracks = vec![track("c"), track("a"), track("b")];
        assert_eq!(app.playing_index(), Some(2));

        // When the playing track isn't in the current view, there is no index.
        app.tracks = vec![track("a"), track("c")];
        assert_eq!(app.playing_index(), None);

        // Nothing playing -> no index.
        app.playing = None;
        assert_eq!(app.playing_index(), None);
    }

    #[test]
    fn parse_lrc_sorts_and_skips_metadata() {
        let lrc = "[ar:Zutomayo]\n[00:12.50]first\n[00:15.00][01:00.00]repeat\n[00:03.00]early\nno timestamp\n";
        let parsed = super::parse_lrc(lrc);
        assert_eq!(
            parsed,
            vec![
                (3.0, "early".to_string()),
                (12.5, "first".to_string()),
                (15.0, "repeat".to_string()),
                (60.0, "repeat".to_string()),
            ]
        );
    }

    #[test]
    fn truncate_chars_is_char_boundary_safe() {
        // ASCII truncates by character count.
        assert_eq!(truncate_chars("hello world", 5), "hello");
        // Fewer characters than the limit returns the whole string.
        assert_eq!(truncate_chars("hi", 5), "hi");
        // Multi-byte CJK must never slice mid-codepoint (a byte slice at 4 would panic).
        let jp = "ずっと真夜中でいいのに。";
        assert_eq!(truncate_chars(jp, 4), "ずっと真");
        assert_eq!(truncate_chars(jp, 100), jp);
        // Emoji are counted as characters, not bytes.
        assert_eq!(truncate_chars("🎧🎵🎶", 2), "🎧🎵");
    }
}
