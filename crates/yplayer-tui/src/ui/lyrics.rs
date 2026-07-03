use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use super::theme;
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::border_style())
        .title(" Lyrics ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.playing.is_none() {
        placeholder(f, inner, "  Nothing playing");
        return;
    }

    let lines = match &app.lyrics {
        None => {
            placeholder(f, inner, "  Fetching lyrics\u{2026}");
            return;
        }
        Some(l) if l.is_empty() => {
            placeholder(f, inner, "  No synced lyrics for this track");
            return;
        }
        Some(l) => l,
    };

    let height = inner.height as usize;
    if height == 0 {
        return;
    }

    // Window the lyrics so the active line sits near the middle.
    let active = app.current_lyric.saturating_sub(1);
    let start = active.saturating_sub(height / 2);
    let end = (start + height).min(lines.len());
    let start = end.saturating_sub(height); // keep a full window when near the end

    let rendered: Vec<Line> = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, (_t, text))| {
            let idx = start + i;
            let style = if app.current_lyric > 0 && idx == active {
                theme::title_style()
            } else {
                theme::dim_style()
            };
            Line::from(Span::styled(format!("  {}", text), style))
        })
        .collect();

    f.render_widget(Paragraph::new(rendered), inner);
}

fn placeholder(f: &mut Frame, area: Rect, msg: &str) {
    f.render_widget(
        Paragraph::new(Span::styled(msg.to_string(), theme::dim_style())),
        area,
    );
}
