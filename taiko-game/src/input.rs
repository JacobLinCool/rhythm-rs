use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rhythm_chart::Tick;
use rhythm_core::TimedInput;
use rhythm_mode_taiko::TaikoAction;

use crate::preferences::{BindingSlot, DrumBindings};

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

const DON_MENU_KEYS: [char; 10] = [' ', 'f', 'g', 'h', 'j', 'c', 'v', 'b', 'n', 'm'];
const MENU_UP_KEYS: [char; 10] = ['d', 's', 'a', 't', 'r', 'e', 'w', 'q', 'x', 'z'];
const MENU_DOWN_KEYS: [char; 11] = ['k', 'l', ';', '\'', 'y', 'u', 'i', 'o', ',', '.', '/'];

pub(crate) const MAX_OFFLINE_PENDING_INPUTS: usize = 512;

/// Inserts one offline input in stable timestamp order.
///
/// Equal-tick strikes retain arrival order. Once the fixed window is full the
/// newest strike is rejected, so a terminal paste/event burst cannot grow the
/// single-player or local multiplayer queue without bound.
#[must_use]
pub(crate) fn enqueue_offline_input(
    pending: &mut Vec<TimedInput<TaikoAction>>,
    input: TimedInput<TaikoAction>,
) -> bool {
    if pending.len() >= MAX_OFFLINE_PENDING_INPUTS {
        return false;
    }
    let insert_at = pending.partition_point(|queued| queued.tick <= input.tick);
    pending.insert(insert_at, input);
    true
}

pub(crate) fn collect_due_offline_inputs(
    pending: &mut Vec<TimedInput<TaikoAction>>,
    now_tick: Tick,
) -> Vec<TimedInput<TaikoAction>> {
    let split_at = pending.partition_point(|input| input.tick <= now_tick);
    pending.drain(..split_at).collect()
}

fn is_don_key(c: char) -> bool {
    DON_MENU_KEYS.contains(&c)
}

pub fn map_bound_game_hit(key: KeyEvent, bindings: DrumBindings) -> Option<TaikoAction> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    let KeyCode::Char(character) = key.code else {
        return None;
    };
    match bindings.slot_for_key(character)? {
        BindingSlot::LeftKat => Some(TaikoAction::LEFT_KAT),
        BindingSlot::LeftDon => Some(TaikoAction::LEFT_DON),
        BindingSlot::RightDon => Some(TaikoAction::RIGHT_DON),
        BindingSlot::RightKat => Some(TaikoAction::RIGHT_KAT),
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
        } if MENU_UP_KEYS.contains(&c) => Some(MenuIntent::Up),
        KeyEvent {
            code: KeyCode::Char(c),
            ..
        } if MENU_DOWN_KEYS.contains(&c) => Some(MenuIntent::Down),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        collect_due_offline_inputs, enqueue_offline_input, is_game_pause_toggle_key,
        map_bound_game_hit, MAX_OFFLINE_PENDING_INPUTS,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use rhythm_chart::Tick;
    use rhythm_core::TimedInput;
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
    fn configured_gameplay_keys_preserve_side_and_zone() {
        let bindings = crate::preferences::DrumBindings::player_one_default();
        for (key, action) in [
            ('a', TaikoAction::LEFT_KAT),
            ('s', TaikoAction::LEFT_DON),
            ('d', TaikoAction::RIGHT_DON),
            ('f', TaikoAction::RIGHT_KAT),
        ] {
            assert_eq!(
                map_bound_game_hit(
                    KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE),
                    bindings,
                ),
                Some(action)
            );
        }
        assert_eq!(
            map_bound_game_hit(
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                bindings
            ),
            None
        );
        assert_eq!(
            map_bound_game_hit(
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                bindings,
            ),
            None,
            "space has no physical side and remains menu-only"
        );
    }

    #[test]
    fn offline_input_queue_is_bounded_and_stable_under_bursts() {
        let mut pending = Vec::new();
        let stable_actions = [
            TaikoAction::LEFT_DON,
            TaikoAction::RIGHT_DON,
            TaikoAction::LEFT_KAT,
            TaikoAction::RIGHT_KAT,
        ];
        for action in stable_actions {
            assert!(enqueue_offline_input(
                &mut pending,
                TimedInput { tick: 10, action }
            ));
        }
        let same_tick = collect_due_offline_inputs(&mut pending, 10);
        assert_eq!(
            same_tick
                .iter()
                .map(|input| input.action)
                .collect::<Vec<_>>(),
            stable_actions,
            "equal-tick inputs must retain terminal arrival order"
        );

        for index in 0..MAX_OFFLINE_PENDING_INPUTS {
            assert!(enqueue_offline_input(
                &mut pending,
                TimedInput {
                    tick: Tick::try_from(MAX_OFFLINE_PENDING_INPUTS - index)
                        .expect("test tick fits"),
                    action: TaikoAction::LEFT_DON,
                }
            ));
        }
        for _ in 0..64 {
            assert!(!enqueue_offline_input(
                &mut pending,
                TimedInput {
                    tick: 0,
                    action: TaikoAction::RIGHT_KAT,
                }
            ));
        }
        assert_eq!(pending.len(), MAX_OFFLINE_PENDING_INPUTS);
        assert!(
            pending.windows(2).all(|pair| pair[0].tick <= pair[1].tick),
            "accepted burst must remain sorted for deterministic runtime input"
        );
        assert!(
            pending.iter().all(|input| input.tick > 0),
            "overflow policy must reject the newest input without evicting accepted strikes"
        );
    }
}
