//! A virtual keyboard and pointer in the nested niri: a Wayland client of its socket (never of `WAYLAND_DISPLAY`),
//! through `zwp_virtual_keyboard_manager_v1` and `zwlr_virtual_pointer_manager_v1`.
//!
//! The keyboard carries a US keymap compiled here (rules `evdev`, layout `us`, the environment's XKB names ignored);
//! text is typed by looking each character up in it, at level 1 or 2 (Shift). Key codes are Linux evdev codes
//! ([`keys`]). Events go to whichever window niri has focused; the pointer's absolute coordinates span the output.

use std::{
    collections::HashMap,
    io::Write as _,
    os::{fd::AsFd as _, unix::net::UnixStream},
    path::Path,
    thread,
    time::{Duration, Instant},
};

use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, delegate_noop,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{
        wl_pointer::ButtonState,
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
    },
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1, zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};
use xkbcommon::xkb;

use crate::SandboxResult;

/// Linux evdev key and button codes the tests use.
pub mod keys {
    pub const ESC: u32 = 1;
    pub const BACKSPACE: u32 = 14;
    pub const TAB: u32 = 15;
    pub const ENTER: u32 = 28;
    pub const LEFT_CTRL: u32 = 29;
    pub const LEFT_SHIFT: u32 = 42;
    pub const LEFT_ALT: u32 = 56;
    pub const UP: u32 = 103;
    pub const LEFT: u32 = 105;
    pub const RIGHT: u32 = 106;
    pub const DOWN: u32 = 108;
    pub const SPACE: u32 = 57;
    pub const HOME: u32 = 102;
    pub const PAGE_UP: u32 = 104;
    pub const END: u32 = 107;
    pub const PAGE_DOWN: u32 = 109;
    pub const DELETE: u32 = 111;
    pub const F1: u32 = 59;
    /// The left mouse button.
    pub const BTN_LEFT: u32 = 0x110;
    pub const BTN_RIGHT: u32 = 0x111;
}

/// A modifier held through a key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
}

impl Modifier {
    const fn key(self) -> u32 {
        match self {
            Self::Shift => keys::LEFT_SHIFT,
            Self::Ctrl => keys::LEFT_CTRL,
            Self::Alt => keys::LEFT_ALT,
        }
    }

    const fn xkb_name(self) -> &'static str {
        match self {
            Self::Shift => xkb::MOD_NAME_SHIFT,
            Self::Ctrl => xkb::MOD_NAME_CTRL,
            Self::Alt => xkb::MOD_NAME_ALT,
        }
    }
}

/// A key press as a name gives it (`ctrl+k`, `shift+Tab`, `Down`, `Enter`, `a`, `F6`): its modifiers (`ctrl`, `shift`,
/// `alt`, joined by `+`, case aside) and its key, a named one (`Esc`/`Escape`, `Tab`, `Enter`/`Return`, `Backspace`,
/// `Space`, `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Delete`, `F1`–`F10`) or one character
/// on `keymap` (Shift added where it needs it). `None` for anything else.
#[must_use]
pub fn parse_key(spec: &str, keymap: &Keymap) -> Option<(Vec<Modifier>, u32)> {
    let mut parts: Vec<&str> = spec.split('+').collect();
    let key = parts.pop().filter(|key| !key.is_empty())?;
    let mut modifiers = Vec::new();
    for part in parts {
        let modifier = match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Modifier::Ctrl,
            "shift" => Modifier::Shift,
            "alt" => Modifier::Alt,
            _ => return None,
        };
        if !modifiers.contains(&modifier) {
            modifiers.push(modifier);
        }
    }
    let named = match key.to_ascii_lowercase().as_str() {
        "esc" | "escape" => Some(keys::ESC),
        "tab" => Some(keys::TAB),
        "enter" | "return" => Some(keys::ENTER),
        "backspace" => Some(keys::BACKSPACE),
        "space" => Some(keys::SPACE),
        "up" => Some(keys::UP),
        "down" => Some(keys::DOWN),
        "left" => Some(keys::LEFT),
        "right" => Some(keys::RIGHT),
        "home" => Some(keys::HOME),
        "end" => Some(keys::END),
        "pageup" => Some(keys::PAGE_UP),
        "pagedown" => Some(keys::PAGE_DOWN),
        "delete" | "del" => Some(keys::DELETE),
        name => name
            .strip_prefix('f')
            .and_then(|number| number.parse::<u32>().ok())
            .filter(|number| (1..=10).contains(number))
            .map(|number| keys::F1 + number - 1),
    };
    if let Some(code) = named {
        return Some((modifiers, code));
    }
    let mut chars = key.chars();
    let (Some(typed), None) = (chars.next(), chars.next()) else {
        return None;
    };
    let (code, shift) = keymap.key_for(typed)?;
    if shift && !modifiers.contains(&Modifier::Shift) {
        modifiers.push(Modifier::Shift);
    }
    Some((modifiers, code))
}

/// `wl_keyboard`'s key states.
const RELEASED: u32 = 0;
const PRESSED: u32 = 1;
/// `wl_keyboard`'s keymap format `xkb_v1`.
const XKB_V1: u32 = 1;
/// xkb key codes are evdev codes plus 8.
const EVDEV_OFFSET: u32 = 8;
/// A pause between events, so a client sees each as its own.
const PACE: Duration = Duration::from_millis(15);

/// The keymap: its text for the compositor, the key (and whether Shift) for each character it types, and each
/// modifier's mask.
pub struct Keymap {
    text: String,
    chars: HashMap<char, (u32, bool)>,
    masks: HashMap<&'static str, u32>,
}

impl Keymap {
    /// A US keymap from the system's XKB data.
    ///
    /// # Errors
    /// The data is missing or doesn't compile.
    pub fn us() -> SandboxResult<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
        let keymap =
            xkb::Keymap::new_from_names(&context, "evdev", "pc105", "us", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
                .ok_or("no US keymap: the system's XKB data (xkeyboard-config) is missing")?;
        let mut chars = HashMap::new();
        for raw in keymap.min_keycode().raw()..=keymap.max_keycode().raw() {
            for (level, shift) in [(0, false), (1, true)] {
                for typed in typed_at(&keymap, raw, level) {
                    chars.entry(typed).or_insert((raw.saturating_sub(EVDEV_OFFSET), shift));
                }
            }
        }
        let masks = [Modifier::Shift, Modifier::Ctrl, Modifier::Alt]
            .into_iter()
            .map(|modifier| {
                let index = keymap.mod_get_index(modifier.xkb_name());
                (modifier.xkb_name(), 1_u32.checked_shl(index).unwrap_or(0))
            })
            .collect();
        Ok(Self {
            text: keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1),
            chars,
            masks,
        })
    }

    /// The evdev key and whether Shift for `typed`.
    #[must_use]
    pub fn key_for(&self, typed: char) -> Option<(u32, bool)> {
        self.chars.get(&typed).copied()
    }

    fn mask(&self, modifiers: &[Modifier]) -> u32 {
        modifiers
            .iter()
            .map(|modifier| self.masks.get(modifier.xkb_name()).copied().unwrap_or(0))
            .fold(0, |mask, bit| mask | bit)
    }
}

/// The printable characters key `raw` (an xkb key code) types at `level` of the first layout.
fn typed_at(keymap: &xkb::Keymap, raw: u32, level: u32) -> Vec<char> {
    keymap
        .key_get_syms_by_level(xkb::Keycode::new(raw), 0, level)
        .iter()
        .filter_map(|&sym| char::from_u32(xkb::keysym_to_utf32(sym)))
        .filter(|typed| !typed.is_control())
        .collect()
}

/// The client's dispatch state: nothing it gets matters.
struct State;

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);

/// The virtual devices of one connection to the nested niri.
pub struct VirtualInput {
    connection: Connection,
    queue: EventQueue<State>,
    keyboard: ZwpVirtualKeyboardV1,
    pointer: ZwlrVirtualPointerV1,
    keymap: Keymap,
    extent: (u32, u32),
    started: Instant,
}

impl VirtualInput {
    /// Connects to the compositor at `socket` and makes a keyboard (with [`Keymap::us`]) and a pointer whose absolute
    /// coordinates span `extent` (the output's size).
    ///
    /// # Errors
    /// The connection fails, a protocol is missing, or the keymap can't be made.
    pub fn connect(socket: &Path, extent: (u32, u32)) -> SandboxResult<Self> {
        let connection = Connection::from_socket(UnixStream::connect(socket)?)?;
        let (globals, mut queue) = registry_queue_init::<State>(&connection)?;
        let handle = queue.handle();
        let seat: WlSeat = globals.bind(&handle, 1..=1, ())?;
        let keyboards: ZwpVirtualKeyboardManagerV1 = globals.bind(&handle, 1..=1, ())?;
        let pointers: ZwlrVirtualPointerManagerV1 = globals.bind(&handle, 1..=1, ())?;
        let keymap = Keymap::us()?;
        let keyboard = keyboards.create_virtual_keyboard(&seat, &handle, ());
        let mut file = tempfile::tempfile()?;
        file.write_all(keymap.text.as_bytes())?;
        file.write_all(&[0])?;
        let size = u32::try_from(keymap.text.len() + 1)?;
        keyboard.keymap(XKB_V1, file.as_fd(), size);
        let pointer = pointers.create_virtual_pointer(Some(&seat), &handle, ());
        queue.roundtrip(&mut State)?;
        Ok(Self {
            connection,
            queue,
            keyboard,
            pointer,
            keymap,
            extent,
            started: Instant::now(),
        })
    }

    /// The keyboard's keymap.
    #[must_use]
    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Milliseconds since the devices were made, as event timestamps.
    fn time(&self) -> u32 {
        u32::try_from(self.started.elapsed().as_millis()).unwrap_or(u32::MAX)
    }

    /// Sends what was queued and waits until the compositor has handled it.
    fn sync(&mut self) -> SandboxResult {
        self.connection.flush()?;
        self.queue.roundtrip(&mut State)?;
        thread::sleep(PACE);
        Ok(())
    }

    fn set_modifiers(&self, modifiers: &[Modifier]) {
        self.keyboard.modifiers(self.keymap.mask(modifiers), 0, 0, 0);
    }

    /// Presses and releases `key` (an evdev code) with `modifiers` held.
    ///
    /// # Errors
    /// The connection fails.
    pub fn key_with(&mut self, modifiers: &[Modifier], key: u32) -> SandboxResult {
        for modifier in modifiers {
            self.keyboard.key(self.time(), modifier.key(), PRESSED);
        }
        self.set_modifiers(modifiers);
        self.keyboard.key(self.time(), key, PRESSED);
        self.sync()?;
        self.keyboard.key(self.time(), key, RELEASED);
        for modifier in modifiers.iter().rev() {
            self.keyboard.key(self.time(), modifier.key(), RELEASED);
        }
        self.set_modifiers(&[]);
        self.sync()
    }

    /// Presses and releases `key` (an evdev code).
    ///
    /// # Errors
    /// The connection fails.
    pub fn key(&mut self, key: u32) -> SandboxResult {
        self.key_with(&[], key)
    }

    /// Types `text`, a key per character (Shift where the keymap needs it).
    ///
    /// # Errors
    /// A character isn't on the US keymap, or the connection fails.
    pub fn type_text(&mut self, text: &str) -> SandboxResult {
        for typed in text.chars() {
            let (key, shift) = self
                .keymap
                .key_for(typed)
                .ok_or_else(|| format!("{typed:?} isn't on the US keymap"))?;
            let modifiers: &[Modifier] = if shift { &[Modifier::Shift] } else { &[] };
            self.key_with(modifiers, key)?;
        }
        Ok(())
    }

    /// Moves the pointer to (`x`, `y`) in output pixels.
    ///
    /// # Errors
    /// The connection fails.
    pub fn move_to(&mut self, x: u32, y: u32) -> SandboxResult {
        let (width, height) = self.extent;
        self.pointer.motion_absolute(self.time(), x, y, width, height);
        self.pointer.frame();
        self.sync()
    }

    /// Moves the pointer to (`x`, `y`) and clicks `button` (an evdev code, [`keys::BTN_LEFT`]).
    ///
    /// # Errors
    /// The connection fails.
    pub fn click(&mut self, x: u32, y: u32, button: u32) -> SandboxResult {
        self.move_to(x, y)?;
        self.pointer.button(self.time(), button, ButtonState::Pressed);
        self.pointer.frame();
        self.sync()?;
        self.pointer.button(self.time(), button, ButtonState::Released);
        self.pointer.frame();
        self.sync()
    }
}

impl Drop for VirtualInput {
    fn drop(&mut self) {
        self.keyboard.destroy();
        self.pointer.destroy();
        let _ = self.connection.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_read_from_its_name_with_its_modifiers() {
        let Ok(keymap) = Keymap::us() else {
            eprintln!("skipping: no XKB data");
            return;
        };
        assert_eq!(parse_key("Down", &keymap), Some((Vec::new(), keys::DOWN)));
        assert_eq!(parse_key("enter", &keymap), Some((Vec::new(), keys::ENTER)));
        assert_eq!(
            parse_key("ctrl+shift+Tab", &keymap),
            Some((vec![Modifier::Ctrl, Modifier::Shift], keys::TAB))
        );
        assert_eq!(parse_key("F6", &keymap), Some((Vec::new(), keys::F1 + 5)));
        let (modifiers, k) = parse_key("ctrl+k", &keymap).unwrap();
        assert_eq!(
            (modifiers, Some(k)),
            (vec![Modifier::Ctrl], keymap.key_for('k').map(|(key, _)| key))
        );
        // a character that needs Shift gets it
        let (modifiers, _) = parse_key("A", &keymap).unwrap();
        assert_eq!(modifiers, [Modifier::Shift]);
        for bad in ["", "ctrl+", "hyper+a", "F11", "ab", "ctrl+nothing"] {
            assert_eq!(parse_key(bad, &keymap), None, "{bad}");
        }
    }
}
