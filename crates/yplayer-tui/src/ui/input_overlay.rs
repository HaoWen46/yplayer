use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme;
use crate::app::App;

/// Draw a small one-line input prompt centered at the bottom of the screen.
fn draw_input_prompt(f: &mut Frame, title: &str, value: &str, hint: &str) {
    let area = f.area();

    // Single-line bar near the bottom: full width, 3 rows tall
    let prompt_height = 3u16;
    let prompt_y = area.height.saturating_sub(prompt_height + 2); // above footer+player
    let popup_area = Rect::new(
        area.x + 2,
        prompt_y,
        area.width.saturating_sub(4),
        prompt_height,
    );

    f.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(format!(" {} ", title))
        .borders(Borders::ALL)
        .border_style(theme::search_border_style());

    let inner = block.inner(popup_area);
    f.render_widget(block, popup_area);

    // Input line with cursor
    let cursor = "_";
    let text = format!("> {}{}", value, cursor);
    let spans = vec![
        Span::styled(text, theme::title_style()),
        Span::styled(format!("  {}", hint), theme::dim_style()),
    ];
    let input = Paragraph::new(Line::from(spans));
    f.render_widget(input, inner);
}

/// Prompt for a YouTube URL to download.
pub fn draw_download(f: &mut Frame, app: &App) {
    draw_input_prompt(
        f,
        "Download URL",
        &app.download_input,
        "(Enter to start, Esc to cancel)",
    );
}

/// Prompt for a new track title (rename).
pub fn draw_rename(f: &mut Frame, app: &App) {
    draw_input_prompt(
        f,
        "Rename Track",
        &app.rename_input,
        "(Enter to save, Esc to cancel)",
    );
}
