use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    MoveDown,
    MoveUp,
    PageDown,
    PageUp,
    JumpFirst,
    JumpLast,
    NextPanel,
    PrevPanel,
    JumpPanel(usize),
    ToggleSort,
    ReverseSort,
    StartSearch,
    StartFilter,
    StartSaveAs,
    FollowLink,
    NavigateBack,
    InputChar(char),
    Backspace,
    CursorLeft,
    CursorRight,
    ToggleHelp,
    Dismiss,
    Noop,
}

pub fn map_key_with_input_mode(key: KeyEvent, input_mode_active: bool) -> KeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::Quit;
    }
    if input_mode_active {
        return match key.code {
            KeyCode::Enter => KeyAction::FollowLink,
            KeyCode::Esc => KeyAction::Dismiss,
            KeyCode::Backspace => KeyAction::Backspace,
            KeyCode::Left => KeyAction::CursorLeft,
            KeyCode::Right => KeyAction::CursorRight,
            KeyCode::Char(c) if !c.is_control() => KeyAction::InputChar(c),
            _ => KeyAction::Noop,
        };
    }
    match key.code {
        KeyCode::Char('q') => KeyAction::Quit,
        KeyCode::Char('/') => KeyAction::StartSearch,
        KeyCode::Char('f') => KeyAction::StartFilter,
        KeyCode::Char('w') => KeyAction::StartSaveAs,
        KeyCode::Enter => KeyAction::FollowLink,
        KeyCode::Char('b') => KeyAction::NavigateBack,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Char('j') | KeyCode::Down => KeyAction::MoveDown,
        KeyCode::Char('k') | KeyCode::Up => KeyAction::MoveUp,
        KeyCode::PageDown => KeyAction::PageDown,
        KeyCode::PageUp => KeyAction::PageUp,
        KeyCode::Char('g') | KeyCode::Home => KeyAction::JumpFirst,
        KeyCode::Char('G') | KeyCode::End => KeyAction::JumpLast,
        KeyCode::Tab | KeyCode::Char(']') => KeyAction::NextPanel,
        KeyCode::BackTab | KeyCode::Char('[') => KeyAction::PrevPanel,
        KeyCode::Char('1') => KeyAction::JumpPanel(1),
        KeyCode::Char('2') => KeyAction::JumpPanel(2),
        KeyCode::Char('3') => KeyAction::JumpPanel(3),
        KeyCode::Char('4') => KeyAction::JumpPanel(4),
        KeyCode::Char('5') => KeyAction::JumpPanel(5),
        KeyCode::Char('s') => KeyAction::ToggleSort,
        KeyCode::Char('S') => KeyAction::ReverseSort,
        KeyCode::Char('?') => KeyAction::ToggleHelp,
        KeyCode::Esc => KeyAction::Dismiss,
        KeyCode::Char(c) if !c.is_control() => KeyAction::InputChar(c),
        _ => KeyAction::Noop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_mode_treats_g_as_text_not_navigation() {
        let key = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(
            map_key_with_input_mode(key, true),
            KeyAction::InputChar('g')
        );
    }
}
