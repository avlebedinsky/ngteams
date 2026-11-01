//! Consumer DMA format matching before any render device or EGL display is opened.

use super::*;
use crate::{BufferFormat, MODIFIER_INVALID, MODIFIER_LINEAR, fourcc};

#[test]
fn cpu_dma_display_matches_only_offered_format_modifier_pairs() {
    let display: Display = glib::Object::new();
    assert!(!display.renders_dmabuf());
    assert!(!display.offers(fourcc(b"XR24"), MODIFIER_LINEAR));
    assert!(display.buffer_formats().is_null());
    display.imp().target.replace(Some(DmaBufTarget {
        render_node: "/dev/dri/synthetic-unopened".into(),
        formats: vec![
            BufferFormat {
                fourcc: fourcc(b"XR24"),
                modifiers: vec![MODIFIER_LINEAR, MODIFIER_INVALID],
            },
            BufferFormat {
                fourcc: fourcc(b"AR24"),
                modifiers: vec![7],
            },
        ],
    }));
    assert!(display.renders_dmabuf());
    assert!(display.offers(fourcc(b"XR24"), MODIFIER_LINEAR));
    assert!(display.offers(fourcc(b"XR24"), MODIFIER_INVALID));
    assert!(display.offers(fourcc(b"AR24"), 7));
    assert!(!display.offers(fourcc(b"AR24"), MODIFIER_LINEAR));
    assert!(!display.offers(fourcc(b"XR24"), 7));
    assert!(!display.offers(fourcc(b"NV12"), MODIFIER_LINEAR));
    assert!(
        display.buffer_formats().is_null(),
        "no connected delegate supplies buffer formats"
    );
}

#[test]
fn cpu_dma_display_rejects_nul_before_opening_a_render_device() {
    use std::os::unix::ffi::OsStringExt as _;

    let display: Display = glib::Object::new();
    display.imp().target.replace(Some(DmaBufTarget {
        render_node: std::ffi::OsString::from_vec(b"/dev/dri/renderD128\0synthetic".to_vec()).into(),
        formats: vec![BufferFormat {
            fourcc: fourcc(b"XR24"),
            modifiers: vec![MODIFIER_LINEAR],
        }],
    }));
    let error = display.open_delegate().unwrap_err();
    assert_eq!(error.message(), "the render node path holds a NUL byte");
    assert!(display.delegate_raw().is_none());
    display.imp().target.replace(None);
    assert!(display.open_delegate().is_ok());
    assert!(!display.renders_dmabuf());
    assert!(!display.offers(fourcc(b"XR24"), MODIFIER_LINEAR));
}
