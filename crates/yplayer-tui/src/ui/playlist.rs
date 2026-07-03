use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};

use super::theme;
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .tracks
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let is_selected = i == app.selection;
            let is_playing = app.playing_index() == Some(i);

            let mut spans: Vec<Span> = Vec::new();

            if is_selected {
                spans.push(Span::styled("\u{2192} ", theme::arrow_style()));
            } else {
                spans.push(Span::raw("  "));
            }

            if is_playing {
                spans.push(Span::styled("\u{25b6} ", theme::playing_indicator_style()));
            }

            spans.push(Span::styled("[PL] ", theme::tag_style()));
            spans.push(Span::styled(track.title.as_str(), theme::title_style()));

            if let Some(ref uploader) = track.uploader {
                spans.push(Span::styled(" \u{2014} ", theme::separator_style()));
                spans.push(Span::styled(uploader.as_str(), theme::uploader_style()));
            }

            // Right side
            if track.audio_path.is_some() {
                spans.push(Span::styled(" \u{2713}", theme::cached_style()));
            } else {
                spans.push(Span::styled(" \u{2026}", theme::dim_style()));
            }

            if let Some(dur) = track.duration {
                let h = dur / 3600;
                let m = (dur % 3600) / 60;
                let s = dur % 60;
                let formatted = if h > 0 {
                    format!(" {}:{:02}:{:02}", h, m, s)
                } else {
                    format!(" {}:{:02}", m, s)
                };
                spans.push(Span::styled(formatted, theme::duration_style()));
            }

            let style = if is_selected {
                theme::selection_style()
            } else {
                theme::normal_style()
            };

            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let list = List::new(items).block(Block::default().borders(Borders::NONE));
    let mut state = ListState::default();
    state.select(Some(app.selection));
    f.render_stateful_widget(list, area, &mut state);
}
