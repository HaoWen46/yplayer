use ratatui::style::{Color, Modifier, Style};

// Core palette — mirrors the Python curses color pairs
pub fn title_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

pub fn uploader_style() -> Style {
    Style::default().fg(Color::Yellow)
}

pub fn duration_style() -> Style {
    Style::default().fg(Color::Magenta)
}

pub fn cached_style() -> Style {
    Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD)
}

pub fn tag_style() -> Style {
    Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD)
}

pub fn header_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

pub fn selection_style() -> Style {
    Style::default()
        .fg(Color::White)
        .bg(Color::Blue)
        .add_modifier(Modifier::BOLD)
}

pub fn playback_style() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

pub fn playing_indicator_style() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

pub fn arrow_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub fn separator_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn key_hint_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub fn desc_hint_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::DIM)
}

pub fn border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn search_border_style() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn gauge_filled_style() -> Style {
    Style::default().fg(Color::Green)
}

pub fn dim_style() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

pub fn warn_style() -> Style {
    Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD)
}

pub fn error_style() -> Style {
    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
}

pub fn normal_style() -> Style {
    Style::default()
}
