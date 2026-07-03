use anyhow::Result;
use crossterm::event;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
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
    DownloadDone { track: Track },
    DownloadFailed { url: String, error: String },
}

pub struct App {
    pub mode: ViewMode,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub selection: usize,
    pub offset: usize,
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
    status_msg_until: Option<std::time::Instant>,
    // Delete confirmation: Some(Instant) = waiting for 2nd press, expires after 3s
    pub confirm_delete_until: Option<std::time::Instant>,
    // Background download tasks report results here; run() owns the receiver.
    worker_tx: mpsc::UnboundedSender<WorkerEvent>,
    worker_rx: Option<mpsc::UnboundedReceiver<WorkerEvent>>,
    // Persistent worker actor handle; set by run() (needs a Tokio runtime).
    worker: Option<WorkerHandle>,
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
            offset: 0,
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
            status_msg_until: None,
            confirm_delete_until: None,
            worker_tx,
            worker_rx: Some(worker_rx),
            worker: None,
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
        self.status_msg = Some(msg.into());
        self.status_msg_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
    }

    fn clear_expired_status(&mut self) {
        if let Some(until) = self.status_msg_until {
            if std::time::Instant::now() >= until {
                self.status_msg = None;
                self.status_msg_until = None;
            }
        }
        // Also expire delete confirmation
        if let Some(until) = self.confirm_delete_until {
            if std::time::Instant::now() >= until {
                self.confirm_delete_until = None;
                if self.status_msg.as_deref() == Some("Press d again to delete, Esc to cancel") {
                    self.status_msg = None;
                    self.status_msg_until = None;
                }
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
            Action::Tick => {
                self.player.poll_status().await;
                self.check_loop().await;
                self.clear_expired_status();
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
                    self.playing = Some(track);
                }
                Err(e) => {
                    self.set_status(format!("Playback error: {}", e));
                }
            }
        } else {
            self.set_status("No audio file for this track".to_string());
        }
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
                    use rand::Rng;
                    let mut rng = rand::thread_rng();
                    let next = rng.gen_range(0..self.tracks.len());
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
            use fuzzy_matcher::skim::SkimMatcherV2;
            use fuzzy_matcher::FuzzyMatcher;

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
                    if score > 0 {
                        Some((score, t))
                    } else {
                        None
                    }
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
                self.set_status(format!(
                    "Download failed: {} ({})",
                    truncate_chars(&url, 30),
                    error
                ));
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
    if let Some(id) = &state.last_track_id {
        if let Some(pos) = app.tracks.iter().position(|t| &t.id == id) {
            app.selection = pos;
        }
    }

    // Spawn the persistent worker actor (needs the Tokio runtime, so not in App::new).
    app.worker = Some(WorkerHandle::spawn(app.config.clone()));

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
    use super::truncate_chars;
    use super::App;
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
