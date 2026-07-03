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

            // Selection arrow
            if is_selected {
                spans.push(Span::styled("\u{2192} ", theme::arrow_style()));
            } else {
                spans.push(Span::raw("  "));
            }

            // Playing indicator
            if is_playing {
                spans.push(Span::styled("\u{25b6} ", theme::playing_indicator_style()));
            }

            // Title
            let title = &track.title;
            spans.push(Span::styled(title.as_str(), theme::title_style()));

            // Separator + uploader
            if let Some(ref uploader) = track.uploader {
                spans.push(Span::styled(" \u{2014} ", theme::separator_style()));
                spans.push(Span::styled(uploader.as_str(), theme::uploader_style()));
            }

            // Right side: cached + duration
            let mut right_parts: Vec<Span> = Vec::new();
            if track.audio_path.is_some() {
                right_parts.push(Span::styled(" \u{2713}", theme::cached_style()));
            }
            if let Some(dur) = track.duration {
                right_parts.push(Span::styled(
                    format!(" {}", format_duration(dur)),
                    theme::duration_style(),
                ));
            } else {
                right_parts.push(Span::styled(" ?:??", theme::duration_style()));
            }

            // Combine: we put the right parts at the end
            spans.extend(right_parts);

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

fn format_duration(sec: i64) -> String {
    let h = sec / 3600;
    let m = (sec % 3600) / 60;
    let s = sec % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}
