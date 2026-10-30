//! Written by hand, included by gir's generated `lib.rs` (every `src/*.rs` beside it becomes a module re-exported at
//! the crate root). `WPEKeymapXKB` hands out libxkbcommon's `struct xkb_keymap` and `struct xkb_state`, which no
//! .gir describes: they stay opaque here, as libxkbcommon declares them.

/// libxkbcommon's opaque `struct xkb_keymap`.
#[repr(C)]
pub struct xkb_keymap {
    _private: [u8; 0],
}

/// libxkbcommon's opaque `struct xkb_state`.
#[repr(C)]
pub struct xkb_state {
    _private: [u8; 0],
}
