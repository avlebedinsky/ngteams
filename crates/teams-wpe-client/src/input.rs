//! Input the consumer forwards to a page, in `WPEPlatform`'s terms: positions in the view's logical pixels, XKB
//! keyvals, `WPEModifiers` bits.

use bitflags::bitflags;

use serde::{Deserialize, Serialize};

bitflags! {
    /// Keyboard modifiers and pressed pointer buttons (`WPEModifiers`).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
    pub struct Modifiers: u32 {
        const CONTROL = 1;
        const SHIFT = 2;
        const ALT = 4;
        const META = 8;
        const CAPS_LOCK = 16;
        const BUTTON1 = 256;
        const BUTTON2 = 512;
        const BUTTON3 = 1024;
        const BUTTON4 = 2048;
        const BUTTON5 = 4096;
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
    #[must_use]
    pub const fn number(self) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_are_wpe_s_bits() {
        assert_eq!(Modifiers::CONTROL.bits(), 1);
        assert_eq!(Modifiers::BUTTON1.bits(), 256);
        assert_eq!((Modifiers::CONTROL | Modifiers::BUTTON3).bits(), 1 | 1024);
    }

    #[test]
    fn buttons_are_numbered_as_wpe_numbers_them() {
        assert_eq!(Button::Primary.number(), 1);
        assert_eq!(Button::Middle.number(), 2);
        assert_eq!(Button::Secondary.number(), 3);
        assert_eq!(Button::Back.number(), 8);
        assert_eq!(Button::Forward.number(), 9);
    }
}
