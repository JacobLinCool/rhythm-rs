use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rhythm_mode_taiko::TaikoAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuIntent {
    Quit,
    Back,
    Confirm,
    Up,
    Down,
    Left,
    Right,
}

const DON_KEYS: [char; 10] = [' ', 'f', 'g', 'h', 'j', 'c', 'v', 'b', 'n', 'm'];
const KAT_KEYS_LEFT: [char; 10] = ['d', 's', 'a', 't', 'r', 'e', 'w', 'q', 'x', 'z'];
const KAT_KEYS_RIGHT: [char; 11] = ['k', 'l', ';', '\'', 'y', 'u', 'i', 'o', ',', '.', '/'];

fn is_don_key(c: char) -> bool {
    DON_KEYS.contains(&c)
}

fn is_kat_key(c: char) -> bool {
    KAT_KEYS_LEFT.contains(&c) || KAT_KEYS_RIGHT.contains(&c)
}

pub fn map_game_hit(key: KeyEvent) -> Option<TaikoAction> {
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }

    match key.code {
        KeyCode::Char(c) if is_don_key(c) => Some(TaikoAction::Don),
        KeyCode::Char(c) if is_kat_key(c) => Some(TaikoAction::Kat),
        _ => None,
    }
}

pub fn is_game_pause_toggle_key(key: KeyEvent) -> bool {
    matches!(
        key,
        KeyEvent {
            code: KeyCode::Char('p' | 'P'),
            modifiers,
            ..
        } if !modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
    )
}

pub fn map_menu_intent(key: KeyEvent) -> Option<MenuIntent> {
    match key {
        KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            ..
        } => Some(MenuIntent::Quit),
        KeyEvent {
            code: KeyCode::Esc, ..
        } => Some(MenuIntent::Back),
        KeyEvent {
            code: KeyCode::Enter,
            ..
        } => Some(MenuIntent::Confirm),
        KeyEvent {
            code: KeyCode::Up, ..
        } => Some(MenuIntent::Up),
        KeyEvent {
            code: KeyCode::Down,
            ..
        } => Some(MenuIntent::Down),
        KeyEvent {
            code: KeyCode::Left,
            ..
        } => Some(MenuIntent::Left),
        KeyEvent {
            code: KeyCode::Right,
            ..
        } => Some(MenuIntent::Right),
        KeyEvent {
            code: KeyCode::Char(c),
            ..
        } if is_don_key(c) => Some(MenuIntent::Confirm),
        KeyEvent {
            code: KeyCode::Char(c),
            ..
        } if KAT_KEYS_LEFT.contains(&c) => Some(MenuIntent::Up),
        KeyEvent {
            code: KeyCode::Char(c),
            ..
        } if KAT_KEYS_RIGHT.contains(&c) => Some(MenuIntent::Down),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_game_pause_toggle_key, map_game_hit};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rhythm_mode_taiko::TaikoAction;

    #[test]
    fn pause_toggle_uses_p_key_without_stealing_ctrl_combos() {
        assert!(is_game_pause_toggle_key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::NONE
        )));
        assert!(is_game_pause_toggle_key(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::SHIFT
        )));
        assert!(!is_game_pause_toggle_key(KeyEvent::new(
            KeyCode::Char('p'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn p_is_no_longer_a_kat_hit_key() {
        assert_eq!(
            map_game_hit(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            map_game_hit(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(TaikoAction::Kat)
        );
    }
}
