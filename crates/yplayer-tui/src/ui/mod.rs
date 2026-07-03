pub mod albums;
pub mod help;
pub mod input_overlay;
pub mod library;
pub mod lyrics;
pub mod player_bar;
pub mod playlist;
pub mod search;
pub mod theme;

use crate::app::App;
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &App) {
    use ratatui::layout::{Constraint, Direction, Layout};

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // header
            Constraint::Min(4),    // main content
            Constraint::Length(3), // player bar
            Constraint::Length(1), // footer hints
        ])
        .split(f.area());

    // Header
    draw_header(f, app, chunks[0]);

    // Main content depends on view mode. When the lyrics pane is toggled on and
    // we're in a list view, it replaces the list.
    let list_mode = matches!(
        app.mode,
        crate::types::ViewMode::Library
            | crate::types::ViewMode::Albums
            | crate::types::ViewMode::AlbumDetail { .. }
    );
    if app.show_lyrics && list_mode {
        lyrics::draw(f, app, chunks[1]);
    } else {
        match &app.mode {
            crate::types::ViewMode::Library => library::draw(f, app, chunks[1]),
            crate::types::ViewMode::Albums => albums::draw_list(f, app, chunks[1]),
            crate::types::ViewMode::AlbumDetail { .. } => albums::draw_detail(f, app, chunks[1]),
            crate::types::ViewMode::Playlist { .. } => playlist::draw(f, app, chunks[1]),
            crate::types::ViewMode::Search => {
                library::draw(f, app, chunks[1]);
                search::draw_overlay(f, app);
            }
            crate::types::ViewMode::DownloadInput => {
                library::draw(f, app, chunks[1]);
                input_overlay::draw_download(f, app);
            }
            crate::types::ViewMode::RenameInput => {
                library::draw(f, app, chunks[1]);
                input_overlay::draw_rename(f, app);
            }
        }
    }

    // Player bar
    player_bar::draw(f, app, chunks[2]);

    // Footer key hints (or the active status message)
    draw_footer(f, app, chunks[3]);

    // Help overlay is drawn last so it sits above everything, including the bar.
    if app.show_help {
        help::draw_overlay(f);
    }
}

fn draw_header(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};

    let mut spans = match &app.mode {
        crate::types::ViewMode::Library => vec![
            Span::styled("Yplayer \u{2014} Library", theme::header_style()),
            Span::styled(format!("  [{}]", app.sort_mode.label()), theme::dim_style()),
        ],
        crate::types::ViewMode::Albums => vec![Span::styled(
            "Yplayer \u{2014} Albums",
            theme::header_style(),
        )],
        crate::types::ViewMode::AlbumDetail { album_name, .. } => vec![Span::styled(
            format!("Yplayer \u{2014} {}", album_name),
            theme::header_style(),
        )],
        crate::types::ViewMode::Playlist { .. } => vec![Span::styled(
            "Yplayer \u{2014} Playlist",
            theme::header_style(),
        )],
        crate::types::ViewMode::Search => vec![Span::styled(
            "Yplayer \u{2014} Search",
            theme::header_style(),
        )],
        crate::types::ViewMode::DownloadInput => vec![Span::styled(
            "Yplayer \u{2014} Download",
            theme::header_style(),
        )],
        crate::types::ViewMode::RenameInput => vec![Span::styled(
            "Yplayer \u{2014} Rename",
            theme::header_style(),
        )],
    };

    let playback_status = app.playback_status_text();
    if !playback_status.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(playback_status, theme::playback_style()));
    }

    let loop_status = app.loop_mode.status_text();
    if !loop_status.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(loop_status, theme::playback_style()));
    }

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(theme::border_style()),
    );
    f.render_widget(header, area);
}

fn draw_footer(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    use ratatui::text::{Line, Span};
    use ratatui::widgets::Paragraph;

    // An active status message takes over the footer (severity-colored) so it is
    // visible even while a track is playing — including the delete confirmation.
    if let Some(ref status) = app.status_msg {
        use crate::app::Severity;
        let style = match app.status_severity {
            Severity::Info => theme::normal_style(),
            Severity::Warn => theme::warn_style(),
            Severity::Error => theme::error_style(),
        };
        let msg = Paragraph::new(Line::from(Span::styled(format!("  {}", status), style)));
        f.render_widget(msg, area);
        return;
    }

    let hints: Vec<(&str, &str)> = match &app.mode {
        crate::types::ViewMode::Library => vec![
            ("\u{2191}/\u{2193}", "move"),
            ("Enter", "play"),
            ("d", "delete"),
            ("r", "rename"),
            ("D", "download"),
            ("S", "sort"),
            ("a", "albums"),
            ("/", "search"),
            ("Space", "pause"),
            ("l", "loop"),
            ("?", "help"),
            ("q", "quit"),
        ],
        crate::types::ViewMode::Albums => vec![
            ("\u{2191}/\u{2193}", "select"),
            ("Enter", "open"),
            ("b", "back"),
            ("Space", "pause"),
            ("s", "stop"),
            ("q", "quit"),
        ],
        crate::types::ViewMode::AlbumDetail { .. } => vec![
            ("\u{2191}/\u{2193}", "select"),
            ("Enter", "play"),
            ("d", "delete"),
            ("r", "rename"),
            ("b", "back"),
            ("Space", "pause"),
            ("l", "loop"),
            ("q", "quit"),
        ],
        crate::types::ViewMode::Playlist { .. } => vec![
            ("\u{2191}/\u{2193}", "move"),
            ("Enter", "play"),
            ("/", "search"),
            ("Space", "pause"),
            ("s", "stop"),
            ("l", "loop"),
            ("n/p", "next/prev"),
            ("q", "quit"),
        ],
        crate::types::ViewMode::Search => vec![
            ("type", "filter"),
            ("Enter", "play"),
            ("Esc", "close"),
            ("\u{2191}/\u{2193}", "navigate"),
        ],
        crate::types::ViewMode::DownloadInput => vec![("Enter", "download"), ("Esc", "cancel")],
        crate::types::ViewMode::RenameInput => vec![("Enter", "save"), ("Esc", "cancel")],
    };

    let mut spans: Vec<Span> = Vec::new();
    for (i, (key, desc)) in hints.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("   "));
        }
        spans.push(Span::styled(*key, theme::key_hint_style()));
        spans.push(Span::styled(format!(" {}", desc), theme::desc_hint_style()));
    }

    let footer = Paragraph::new(Line::from(spans));
    f.render_widget(footer, area);
}
