use anyhow::Result;
use crossterm::event;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::stdout;
use std::time::Duration;

use crate::cache::index::CacheIndex;
use crate::cache::scanner;
use crate::config::Config;
use crate::download::bridge::Bridge;
use crate::events::{self, Action};
use crate::player::mpv::MpvPlayer;
use crate::types::{Album, LoopMode, SortMode, Track, ViewMode};
use crate::ui;

pub struct App {
    pub mode: ViewMode,
    pub tracks: Vec<Track>,
    pub albums: Vec<Album>,
    pub selection: usize,
    pub offset: usize,
    pub playing_index: Option<usize>,
    pub loop_mode: LoopMode,
    pub sort_mode: SortMode,
    pub player: MpvPlayer,
    pub db: CacheIndex,
    pub config: Config,
    pub should_quit: bool,
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
}

impl App {
    pub fn new(cfg: Config, db: CacheIndex) -> Self {
        Self {
            mode: ViewMode::Library,
            tracks: Vec::new(),
            albums: Vec::new(),
            selection: 0,
            offset: 0,
            playing_index: None,
            loop_mode: LoopMode::None,
            sort_mode: SortMode::Title,
            player: MpvPlayer::new(),
            db,
            config: cfg,
            should_quit: false,
            search_query: String::new(),
            search_results: Vec::new(),
            search_selection: 0,
            download_input: String::new(),
            rename_input: String::new(),
            prev_mode: None,
            status_msg: None,
            status_msg_until: None,
            confirm_delete_until: None,
        }
    }

    pub fn load_library(&mut self) {
        self.tracks = self.db.list_tracks_sorted(self.sort_mode).unwrap_or_default();
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
                self.playing_index = None;
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
            Action::GoBack => {
                match &self.mode {
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
                }
            }
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
                        self.confirm_delete_until = Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(3),
                        );
                        self.status_msg =
                            Some("Press d again to delete, Esc to cancel".to_string());
                        self.status_msg_until = Some(
                            std::time::Instant::now() + std::time::Duration::from_secs(3),
                        );
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
                if self.player.is_playing() {
                    let new_vol = (self.player.volume + 5.0).min(100.0);
                    let _ = self.player.set_volume(new_vol).await;
                }
            }
            Action::VolumeDown => {
                if self.player.is_playing() {
                    let new_vol = (self.player.volume - 5.0).max(0.0);
                    let _ = self.player.set_volume(new_vol).await;
                }
            }
            Action::NextTrack => {
                if let Some(idx) = self.playing_index {
                    if self.tracks.is_empty() {
                        return;
                    }
                    let next = (idx + 1) % self.tracks.len();
                    self.selection = next;
                    self.play_track(next).await;
                }
            }
            Action::PrevTrack => {
                if let Some(idx) = self.playing_index {
                    let prev = if idx == 0 {
                        self.tracks.len().saturating_sub(1)
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
            }
        }
    }

    async fn play_track(&mut self, idx: usize) {
        if idx >= self.tracks.len() {
            return;
        }
        let track = &self.tracks[idx];
        if let Some(ref path) = track.audio_path {
            if !std::path::Path::new(path).exists() {
                self.set_status(format!("File not found: {}", path));
                return;
            }
            match self.player.play(path, self.config.volume).await {
                Ok(()) => {
                    self.playing_index = Some(idx);
                    let _ = self.db.update_last_played(&track.id);
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

        // Stop if we're playing this track
        if self.playing_index == Some(self.selection) {
            self.player.stop().await;
            self.playing_index = None;
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

        if let Some(ref mut pi) = self.playing_index {
            if *pi > self.selection {
                *pi -= 1;
            }
        }

        self.set_status(format!("Deleted: {}", track_title));
    }

    async fn check_loop(&mut self) {
        if self.playing_index.is_none() {
            return;
        }
        if self.player.is_playing() {
            return;
        }

        match self.loop_mode {
            LoopMode::Single => {
                if let Some(idx) = self.playing_index {
                    self.play_track(idx).await;
                }
            }
            LoopMode::All => {
                if let Some(idx) = self.playing_index {
                    if self.tracks.is_empty() {
                        self.playing_index = None;
                        return;
                    }
                    let next = (idx + 1) % self.tracks.len();
                    self.selection = next;
                    self.play_track(next).await;
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
                self.playing_index = None;
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

    async fn execute_download(&mut self) {
        let url = self.download_input.trim().to_string();
        // Return to previous mode first so UI shows
        self.mode = self.prev_mode.take().unwrap_or(ViewMode::Library);
        self.download_input.clear();

        if url.is_empty() {
            return;
        }

        self.set_status(format!("Downloading {}…", truncate_chars(&url, 40)));

        match Bridge::new(&self.config).await {
            Ok(mut bridge) => match bridge.download(&url, &self.config).await {
                Ok(result) => {
                    let title = result.track.title.clone();
                    let _ = self.db.upsert_track(&result.track);
                    self.load_library();
                    self.set_status(format!("Downloaded: {}", title));
                }
                Err(e) => {
                    self.set_status(format!("Download failed: {}", e));
                }
            },
            Err(e) => {
                self.set_status(format!("Worker error: {}", e));
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
    app.load_library();

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

    let tick_rate = Duration::from_millis(50);

    while !app.should_quit {
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(tick_rate)? {
            if let event::Event::Key(key) = event::read()? {
                if key.kind != crossterm::event::KeyEventKind::Press {
                    continue;
                }

                match app.mode {
                    ViewMode::Search => match key.code {
                        crossterm::event::KeyCode::Esc => {
                            app.handle_action(Action::Quit).await;
                        }
                        crossterm::event::KeyCode::Enter => {
                            app.handle_action(Action::Select).await;
                        }
                        crossterm::event::KeyCode::Up => {
                            app.handle_action(Action::MoveUp).await;
                        }
                        crossterm::event::KeyCode::Down => {
                            app.handle_action(Action::MoveDown).await;
                        }
                        _ => {
                            app.handle_search_input(key);
                        }
                    },
                    ViewMode::DownloadInput => match key.code {
                        crossterm::event::KeyCode::Esc => {
                            app.mode = app.prev_mode.take().unwrap_or(ViewMode::Library);
                            app.download_input.clear();
                        }
                        crossterm::event::KeyCode::Enter => {
                            app.execute_download().await;
                        }
                        _ => {
                            app.handle_download_input(key);
                        }
                    },
                    ViewMode::RenameInput => match key.code {
                        crossterm::event::KeyCode::Esc => {
                            app.mode = app.prev_mode.take().unwrap_or(ViewMode::Library);
                            app.rename_input.clear();
                        }
                        crossterm::event::KeyCode::Enter => {
                            app.execute_rename();
                        }
                        _ => {
                            app.handle_rename_input(key);
                        }
                    },
                    _ => {
                        // Cancel pending delete confirmation on non-d/Esc keys
                        let is_d = key.code == crossterm::event::KeyCode::Char('d');
                        let is_esc = key.code == crossterm::event::KeyCode::Esc;
                        if !is_d && !is_esc && app.confirm_delete_until.is_some() {
                            app.confirm_delete_until = None;
                            if app.status_msg.as_deref()
                                == Some("Press d again to delete, Esc to cancel")
                            {
                                app.status_msg = None;
                            }
                        }
                        if let Some(action) = events::map_key_public(key) {
                            app.handle_action(action).await;
                        }
                    }
                }
            }
        } else {
            app.handle_action(Action::Tick).await;
        }
    }

    app.player.stop().await;
    // Terminal restored by TerminalGuard's Drop.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::truncate_chars;

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
