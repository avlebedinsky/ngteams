//! Input in `WPEPlatform`'s own terms: what the shared types of `teams-wpe-client` can't name without linking it.

use crate::platform_sys as platform;
pub use teams_wpe_client::input::{Button, Input, Key, Modifiers, Pointer, PointerKind, Scroll};

/// The `WPEEventType` of a pointer event.
pub(crate) trait PointerEventType {
    fn event_type(self) -> platform::WPEEventType;
}

impl PointerEventType for PointerKind {
    fn event_type(self) -> platform::WPEEventType {
        match self {
            Self::Enter => platform::WPE_EVENT_POINTER_ENTER,
            Self::Leave => platform::WPE_EVENT_POINTER_LEAVE,
            Self::Move => platform::WPE_EVENT_POINTER_MOVE,
            Self::Down(_) => platform::WPE_EVENT_POINTER_DOWN,
            Self::Up(_) => platform::WPE_EVENT_POINTER_UP,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shared_modifier_bits_are_wpe_s_own() {
        for (ours, theirs) in [
            (Modifiers::CONTROL, platform::WPE_MODIFIER_KEYBOARD_CONTROL),
            (Modifiers::SHIFT, platform::WPE_MODIFIER_KEYBOARD_SHIFT),
            (Modifiers::ALT, platform::WPE_MODIFIER_KEYBOARD_ALT),
            (Modifiers::META, platform::WPE_MODIFIER_KEYBOARD_META),
            (Modifiers::CAPS_LOCK, platform::WPE_MODIFIER_KEYBOARD_CAPS_LOCK),
            (Modifiers::BUTTON1, platform::WPE_MODIFIER_POINTER_BUTTON1),
            (Modifiers::BUTTON2, platform::WPE_MODIFIER_POINTER_BUTTON2),
            (Modifiers::BUTTON3, platform::WPE_MODIFIER_POINTER_BUTTON3),
            (Modifiers::BUTTON4, platform::WPE_MODIFIER_POINTER_BUTTON4),
            (Modifiers::BUTTON5, platform::WPE_MODIFIER_POINTER_BUTTON5),
        ] {
            assert_eq!(ours.bits(), theirs);
        }
    }

    #[test]
    fn pointer_kinds_map_to_wpe_values() {
        assert_eq!(
            PointerKind::Down(Button::Primary).event_type(),
            platform::WPE_EVENT_POINTER_DOWN
        );
        assert_eq!(
            PointerKind::Up(Button::Primary).event_type(),
            platform::WPE_EVENT_POINTER_UP
        );
        assert_eq!(PointerKind::Move.event_type(), platform::WPE_EVENT_POINTER_MOVE);
        assert_eq!(PointerKind::Enter.event_type(), platform::WPE_EVENT_POINTER_ENTER);
        assert_eq!(PointerKind::Leave.event_type(), platform::WPE_EVENT_POINTER_LEAVE);
    }
}

#[cfg(test)]
#[path = "../tests/support/input.rs"]
mod history_tests;
