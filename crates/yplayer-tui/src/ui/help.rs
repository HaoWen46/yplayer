use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use super::theme;

type Section = (&'static str, &'static [(&'static str, &'static str)]);

const SECTIONS: &[Section] = &[
    (
        "Playback",
        &[
            ("Enter", "play selected"),
            ("Space", "pause / resume"),
            ("s", "stop"),
            ("n / p", "next / previous track"),
            ("\u{2190} / \u{2192}", "seek -5s / +5s"),
            ("+ / -", "volume up / down"),
            ("l", "cycle loop: single / all / shuffle"),
        ],
    ),
    (
        "Library",
        &[
            ("\u{2191}/\u{2193}  j/k", "move selection"),
            ("PgUp / PgDn", "jump by 10"),
            ("S", "cycle sort order"),
            ("a", "albums view"),
            ("b", "back"),
            ("/", "fuzzy search"),
        ],
    ),
    (
        "Manage",
        &[
            ("D", "download from a URL"),
            ("d, d", "delete selected (press twice)"),
            ("r", "rename selected"),
        ],
    ),
    ("General", &[("?", "toggle this help"), ("q / Esc", "quit")]),
];

pub fn draw_overlay(f: &mut Frame) {
    let area = centered_rect(62, 82, f.area());
    f.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    for (title, binds) in SECTIONS {
        lines.push(Line::from(Span::styled(
            format!("  {title}"),
            theme::playback_style(),
        )));
        for (key, desc) in *binds {
            lines.push(Line::from(vec![
                Span::styled(format!("    {key:<14}"), theme::key_hint_style()),
                Span::styled((*desc).to_string(), theme::desc_hint_style()),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        "  press any key to close",
        theme::dim_style(),
    )));

    let help = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(theme::search_border_style())
            .title(" Keybindings "),
    );
    f.render_widget(help, area);
}

fn centered_rect(pct_x: u16, pct_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}
