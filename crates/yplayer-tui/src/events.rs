use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    Select,
    PauseResume,
    Stop,
    ToggleLoop,
    CycleSortMode,
    SwitchToAlbums,
    GoBack,
    Delete,
    Rename,
    StartSearch,
    StartDownload,
    Quit,
    SeekForward,
    SeekBackward,
    VolumeUp,
    VolumeDown,
    NextTrack,
    PrevTrack,
    ToggleHelp,
    Tick,
}

pub fn map_key_public(key: KeyEvent) -> Option<Action> {
    map_key(key)
}

fn map_key(key: KeyEvent) -> Option<Action> {
    // Ignore key release events
    if key.kind != crossterm::event::KeyEventKind::Press {
        return None;
    }

    match key.code {
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::Quit),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Enter => Some(Action::Select),
        KeyCode::Char(' ') => Some(Action::PauseResume),
        KeyCode::Char('s') => Some(Action::Stop),
        KeyCode::Char('l') => Some(Action::ToggleLoop),
        KeyCode::Char('S') => Some(Action::CycleSortMode),
        KeyCode::Char('a') => Some(Action::SwitchToAlbums),
        KeyCode::Char('b') => Some(Action::GoBack),
        KeyCode::Char('d') => Some(Action::Delete),
        KeyCode::Char('r') => Some(Action::Rename),
        KeyCode::Char('/') => Some(Action::StartSearch),
        KeyCode::Char('D') => Some(Action::StartDownload),
        KeyCode::Left => Some(Action::SeekBackward),
        KeyCode::Right => Some(Action::SeekForward),
        KeyCode::Char('+') | KeyCode::Char('=') => Some(Action::VolumeUp),
        KeyCode::Char('-') => Some(Action::VolumeDown),
        KeyCode::Char('n') => Some(Action::NextTrack),
        KeyCode::Char('p') => Some(Action::PrevTrack),
        KeyCode::Char('?') => Some(Action::ToggleHelp),
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        _ => None,
    }
}
