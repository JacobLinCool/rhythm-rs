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
const KAT_KEYS_RIGHT: [char; 12] = ['k', 'l', ';', '\'', 'y', 'u', 'i', 'o', 'p', ',', '.', '/'];

fn is_don_key(c: char) -> bool {
    DON_KEYS.contains(&c)
}

fn is_kat_key(c: char) -> bool {
    KAT_KEYS_LEFT.contains(&c) || KAT_KEYS_RIGHT.contains(&c)
}

pub fn map_game_hit(key: KeyEvent) -> Option<TaikoAction> {
    match key.code {
        KeyCode::Char(c) if is_don_key(c) => Some(TaikoAction::Don),
        KeyCode::Char(c) if is_kat_key(c) => Some(TaikoAction::Kat),
        _ => None,
    }
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
