use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::app::App;
use super::theme;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(theme::border_style());

    let inner = block.inner(area);
    f.render_widget(block, area);

    if !app.player.is_playing() {
        // Show status message if present, otherwise "No track playing"
        if let Some(ref status) = app.status_msg {
            let msg = Paragraph::new(Span::styled(
                format!("  {}", status),
                ratatui::style::Style::default().fg(ratatui::style::Color::Red),
            ));
            f.render_widget(msg, inner);
        } else {
            let msg = Paragraph::new(Span::styled(
                "  No track playing",
                theme::dim_style(),
            ));
            f.render_widget(msg, inner);
        }
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    // Row 1: Now playing info
    let track_title = app
        .playing_index
        .and_then(|i| app.tracks.get(i))
        .map(|t| {
            let mut s = format!("  \u{25b6} {}", t.title);
            if let Some(ref up) = t.uploader {
                s.push_str(&format!(" \u{2014} {}", up));
            }
            s
        })
        .unwrap_or_else(|| "  \u{25b6} Playing...".to_string());

    let info_line = Line::from(vec![
        Span::styled(track_title, theme::title_style()),
    ]);
    f.render_widget(Paragraph::new(info_line), chunks[0]);

    // Row 2: Progress bar + time + volume
    let position = app.player.position;
    let duration = app.player.duration;
    let ratio = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let time_str = format!(
        "  {} / {}",
        fmt_time(position),
        fmt_time(duration)
    );

    let vol_str = format!("Vol: {}%", app.player.volume as u32);

    let loop_str = app.loop_mode.status_text();

    let pause_str = if app.player.is_paused { "[PAUSED]" } else { "" };

    let label = format!(
        "{}  {}  {}  {}",
        time_str,
        pause_str,
        loop_str,
        vol_str,
    );

    let gauge = Gauge::default()
        .ratio(ratio)
        .gauge_style(theme::gauge_filled_style())
        .label(Span::styled(label, theme::normal_style()));

    f.render_widget(gauge, chunks[1]);
}

fn fmt_time(seconds: f64) -> String {
    let total = seconds as i64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{}:{:02}:{:02}", h, m, s)
    } else {
        format!("{}:{:02}", m, s)
    }
}
