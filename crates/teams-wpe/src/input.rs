//! Input the consumer forwards to a page, in `WPEPlatform`'s terms: positions in the view's logical pixels, XKB
//! keyvals, `WPEModifiers` bits.

use bitflags::bitflags;

use crate::platform_sys as platform;
use serde::{Deserialize, Serialize};

bitflags! {
    /// Keyboard modifiers and pressed pointer buttons (`WPEModifiers`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Modifiers: u32 {
        const CONTROL = platform::WPE_MODIFIER_KEYBOARD_CONTROL;
        const SHIFT = platform::WPE_MODIFIER_KEYBOARD_SHIFT;
        const ALT = platform::WPE_MODIFIER_KEYBOARD_ALT;
        const META = platform::WPE_MODIFIER_KEYBOARD_META;
        const CAPS_LOCK = platform::WPE_MODIFIER_KEYBOARD_CAPS_LOCK;
        const BUTTON1 = platform::WPE_MODIFIER_POINTER_BUTTON1;
        const BUTTON2 = platform::WPE_MODIFIER_POINTER_BUTTON2;
        const BUTTON3 = platform::WPE_MODIFIER_POINTER_BUTTON3;
        const BUTTON4 = platform::WPE_MODIFIER_POINTER_BUTTON4;
        const BUTTON5 = platform::WPE_MODIFIER_POINTER_BUTTON5;
    }
}

/// A pointer button, numbered as `WPEPlatform` (and X11/evdev order) numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Button {
    Primary,
    Middle,
    Secondary,
    Back,
    Forward,
}

impl Button {
    pub(crate) const fn number(self) -> u32 {
        match self {
            Self::Primary => 1,
            Self::Middle => 2,
            Self::Secondary => 3,
            Self::Back => 8,
            Self::Forward => 9,
        }
    }
}

/// A pointer event at (`x`, `y`) in the view's logical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pointer {
    pub kind: PointerKind,
    pub x: f64,
    pub y: f64,
    pub modifiers: Modifiers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PointerKind {
    /// The pointer entered the view.
    Enter,
    /// The pointer left the view.
    Leave,
    Move,
    /// A press; the engine counts double and triple clicks itself.
    Down(Button),
    Up(Button),
}

/// A wheel or touchpad scroll at (`x`, `y`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scroll {
    pub x: f64,
    pub y: f64,
    /// Pixels for a precise (touchpad) scroll, wheel steps otherwise; positive scrolls content up and left.
    pub delta_x: f64,
    pub delta_y: f64,
    /// A touchpad's pixel deltas rather than a wheel's steps.
    pub precise: bool,
    /// The end of a kinetic scroll.
    pub stop: bool,
    pub modifiers: Modifiers,
}

/// A key press or release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub pressed: bool,
    /// The XKB keyval (`XKB_KEY_*`).
    pub keyval: u32,
    /// The hardware keycode, when the consumer has it; otherwise the engine's keymap finds one for the keyval.
    pub keycode: Option<u32>,
    pub modifiers: Modifiers,
}

/// Input for a page.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Input {
    Pointer(Pointer),
    Scroll(Scroll),
    Key(Key),
}

impl PointerKind {
    /// The `WPEEventType` of this event.
    pub(crate) const fn event_type(self) -> platform::WPEEventType {
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
    fn modifiers_are_wpe_s_bits() {
        assert_eq!(Modifiers::CONTROL.bits(), 1);
        assert_eq!(Modifiers::SHIFT.bits(), 2);
        assert_eq!(Modifiers::ALT.bits(), 4);
        assert_eq!(Modifiers::META.bits(), 8);
        assert_eq!(Modifiers::BUTTON1.bits(), 256);
        assert_eq!((Modifiers::CONTROL | Modifiers::BUTTON3).bits(), 1 | 1024);
    }

    #[test]
    fn buttons_and_pointer_kinds_map_to_wpe_values() {
        assert_eq!(Button::Primary.number(), 1);
        assert_eq!(Button::Middle.number(), 2);
        assert_eq!(Button::Secondary.number(), 3);
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
