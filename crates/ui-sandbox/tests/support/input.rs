//! Named-key and modifier parsing against a synthetic US keymap.

use super::*;

#[test]
fn named_keys_and_aliases_use_their_evdev_codes() {
    let keymap = Keymap::us().unwrap();
    for (names, code) in [
        (vec!["Esc", "Escape"], keys::ESC),
        (vec!["Tab"], keys::TAB),
        (vec!["Enter", "Return"], keys::ENTER),
        (vec!["Backspace"], keys::BACKSPACE),
        (vec!["Space"], keys::SPACE),
        (vec!["Up"], keys::UP),
        (vec!["Down"], keys::DOWN),
        (vec!["Left"], keys::LEFT),
        (vec!["Right"], keys::RIGHT),
        (vec!["Home"], keys::HOME),
        (vec!["End"], keys::END),
        (vec!["PageUp"], keys::PAGE_UP),
        (vec!["PageDown"], keys::PAGE_DOWN),
        (vec!["Delete", "Del"], keys::DELETE),
    ] {
        for name in names {
            assert_eq!(parse_key(name, &keymap), Some((Vec::new(), code)), "{name}");
            assert_eq!(
                parse_key(&name.to_ascii_uppercase(), &keymap),
                Some((Vec::new(), code)),
                "{name} in uppercase"
            );
        }
    }
    for number in 1..=10 {
        let name = format!("F{number}");
        assert_eq!(
            parse_key(&name, &keymap),
            Some((Vec::new(), keys::F1 + number - 1)),
            "{name}"
        );
    }
    for name in ["F0", "F11", "F4294967296", "F-1", "☃"] {
        assert_eq!(parse_key(name, &keymap), None, "{name}");
    }
    assert_eq!(
        parse_key("F", &keymap),
        keymap.key_for('F').map(|(code, _)| (vec![Modifier::Shift], code))
    );
}

#[test]
fn modifiers_are_case_insensitive_deduplicated_and_keep_their_order() {
    let keymap = Keymap::us().unwrap();
    assert_eq!(
        parse_key("CONTROL+Ctrl+ALT+alt+Shift+SHIFT+Tab", &keymap),
        Some((vec![Modifier::Ctrl, Modifier::Alt, Modifier::Shift], keys::TAB))
    );
    let (code, needs_shift) = keymap.key_for('A').unwrap();
    assert!(needs_shift);
    assert_eq!(
        parse_key("shift+ctrl+A", &keymap),
        Some((vec![Modifier::Shift, Modifier::Ctrl], code))
    );
    assert_eq!(
        parse_key("alt+A", &keymap),
        Some((vec![Modifier::Alt, Modifier::Shift], code))
    );
    for invalid in ["+a", "ctrl++a", "ctrl+alt+", "ctrl+meta+a", "ctrl+ab"] {
        assert_eq!(parse_key(invalid, &keymap), None, "{invalid}");
    }
}
