use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::App;
use super::theme;

pub fn draw_overlay(f: &mut Frame, app: &App) {
    let area = f.area();

    // Center a popup — 60% width, 60% height
    let popup_width = (area.width as f32 * 0.6) as u16;
    let popup_height = (area.height as f32 * 0.6) as u16;
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;
    let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);

    // Clear the area behind the popup
    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Search ")
        .borders(Borders::ALL)
        .border_style(theme::search_border_style());

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    // Search input
    let input_text = format!("> {}_", app.search_query);
    let input = Paragraph::new(Span::styled(input_text, theme::title_style()));
    f.render_widget(input, chunks[0]);

    // Separator
    let sep = Paragraph::new(Span::styled(
        "\u{2500}".repeat(chunks[1].width as usize),
        theme::border_style(),
    ));
    f.render_widget(sep, chunks[1]);

    // Results
    let items: Vec<ListItem> = app
        .search_results
        .iter()
        .enumerate()
        .map(|(i, track)| {
            let is_selected = i == app.search_selection;

            let mut spans = vec![
                Span::styled(&track.title, theme::title_style()),
            ];
            if let Some(ref up) = track.uploader {
                spans.push(Span::styled(" \u{2014} ", theme::separator_style()));
                spans.push(Span::styled(up.as_str(), theme::uploader_style()));
            }
            if track.audio_path.is_some() {
                spans.push(Span::styled(" \u{2713}", theme::cached_style()));
            }

            let style = if is_selected {
                theme::selection_style()
            } else {
                theme::normal_style()
            };

            ListItem::new(Line::from(spans)).style(style)
        })
        .collect();

    let list = List::new(items);
    let mut state = ListState::default();
    if !app.search_results.is_empty() {
        state.select(Some(app.search_selection));
    }
    f.render_stateful_widget(list, chunks[2], &mut state);
}
