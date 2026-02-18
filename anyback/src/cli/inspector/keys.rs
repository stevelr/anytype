use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Quit,
    CopyObjectId,
    OpenInAnytype,
    MoveDown,
    MoveUp,
    HalfPageDown,
    HalfPageUp,
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
    OpenInEditor,
    FollowLink,
    NavigateBack,
    InputChar(char),
    Backspace,
    CursorLeft,
    CursorRight,
    CursorStart,
    CursorEnd,
    KillToEnd,
    ToggleHelp,
    Dismiss,
    Noop,
}

pub fn map_key_with_input_mode(key: KeyEvent, input_mode_active: bool) -> KeyAction {
    if input_mode_active {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('a') {
            return KeyAction::CursorStart;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('e') {
            return KeyAction::CursorEnd;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('k') {
            return KeyAction::KillToEnd;
        }
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
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return KeyAction::CopyObjectId;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('e') {
        return KeyAction::OpenInEditor;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
        return KeyAction::OpenInAnytype;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
        return KeyAction::HalfPageDown;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('u') {
        return KeyAction::HalfPageUp;
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

    #[test]
    fn ctrl_e_maps_to_open_in_editor() {
        let key = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(map_key_with_input_mode(key, false), KeyAction::OpenInEditor);
    }

    #[test]
    fn input_mode_ctrl_shortcuts_map_to_line_editing() {
        let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        let e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(map_key_with_input_mode(a, true), KeyAction::CursorStart);
        assert_eq!(map_key_with_input_mode(e, true), KeyAction::CursorEnd);
        assert_eq!(map_key_with_input_mode(k, true), KeyAction::KillToEnd);
    }

    #[test]
    fn ctrl_shortcuts_map_in_normal_mode() {
        let c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        let o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(map_key_with_input_mode(c, false), KeyAction::CopyObjectId);
        assert_eq!(map_key_with_input_mode(d, false), KeyAction::HalfPageDown);
        assert_eq!(map_key_with_input_mode(u, false), KeyAction::HalfPageUp);
        assert_eq!(map_key_with_input_mode(o, false), KeyAction::OpenInAnytype);
    }
}
