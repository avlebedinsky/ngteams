//! WPE input conversion and load classifications without launching `WebKit` or opening a display.

use super::*;
use crate::input::{Button, Modifiers};

struct OwnedEvent(*mut platform::WPEEvent);

impl Drop for OwnedEvent {
    fn drop(&mut self) {
        // SAFETY: each test owns the new event returned by a WPE event constructor.
        unsafe { platform::wpe_event_unref(self.0) };
    }
}

fn view() -> View {
    let display: Display = glib::Object::new();
    glib::Object::builder().property("display", &display).build()
}

fn check_position(event: &OwnedEvent, x: f64, y: f64) {
    let (mut actual_x, mut actual_y) = (0.0, 0.0);
    // SAFETY: a live event and valid writable coordinates.
    let known = unsafe { platform::wpe_event_get_position(event.0, &raw mut actual_x, &raw mut actual_y) };
    assert_ne!(known, 0);
    assert_eq!((actual_x, actual_y), (x, y));
}

#[test]
fn pointer_events_preserve_positions_modifiers_button_numbers_and_click_counts() {
    let view = view();
    for kind in [PointerKind::Enter, PointerKind::Move, PointerKind::Leave] {
        let event = OwnedEvent(pointer_event(
            view.as_wpe(),
            123,
            Pointer {
                kind,
                x: 4.5,
                y: 6.25,
                modifiers: Modifiers::CONTROL,
            },
        ));
        // SAFETY: the event is a live event owned until the end of this iteration.
        let actual_kind = unsafe { platform::wpe_event_get_event_type(event.0) };
        // SAFETY: the event is still live.
        let source = unsafe { platform::wpe_event_get_input_source(event.0) };
        // SAFETY: the event is still live.
        let time = unsafe { platform::wpe_event_get_time(event.0) };
        // SAFETY: the event is still live.
        let modifiers = unsafe { platform::wpe_event_get_modifiers(event.0) };
        // SAFETY: the event is still live, as is its view.
        let actual_view = unsafe { platform::wpe_event_get_view(event.0) };
        assert_eq!(actual_kind, kind.event_type());
        assert_eq!(source, platform::WPE_INPUT_SOURCE_MOUSE);
        assert_eq!(time, 123);
        assert_eq!(modifiers, Modifiers::CONTROL.bits());
        assert_eq!(actual_view, view.as_wpe());
        check_position(&event, 4.5, 6.25);
    }
    for (kind, count) in [(PointerKind::Down(Button::Back), 1), (PointerKind::Up(Button::Back), 0)] {
        let event = OwnedEvent(pointer_event(
            view.as_wpe(),
            500,
            Pointer {
                kind,
                x: 10.0,
                y: 20.0,
                modifiers: Modifiers::BUTTON4,
            },
        ));
        // SAFETY: a live pointer-button event.
        let actual_kind = unsafe { platform::wpe_event_get_event_type(event.0) };
        // SAFETY: a live pointer-button event.
        let button = unsafe { platform::wpe_event_pointer_button_get_button(event.0) };
        assert_eq!(actual_kind, kind.event_type());
        assert_eq!(button, 8);
        if count > 0 {
            // SAFETY: the event is a live press, the only type that exposes a press count.
            let presses = unsafe { platform::wpe_event_pointer_button_get_press_count(event.0) };
            assert_eq!(presses, count);
        }
        check_position(&event, 10.0, 20.0);
    }
}

#[test]
fn scrolling_preserves_precision_stop_flags_and_both_axes() {
    let view = view();
    for (precise, stop) in [(false, false), (true, false), (true, true)] {
        let event = OwnedEvent(scroll_event(
            view.as_wpe(),
            77,
            Scroll {
                x: 11.0,
                y: 22.0,
                delta_x: -2.0,
                delta_y: 3.5,
                precise,
                stop,
                modifiers: Modifiers::SHIFT,
            },
        ));
        let (mut delta_x, mut delta_y) = (0.0, 0.0);
        // SAFETY: a live scroll event and valid writable delta outputs.
        unsafe { platform::wpe_event_scroll_get_deltas(event.0, &raw mut delta_x, &raw mut delta_y) };
        // SAFETY: a live scroll event.
        let actual_precise = unsafe { platform::wpe_event_scroll_has_precise_deltas(event.0) };
        // SAFETY: a live scroll event.
        let actual_stop = unsafe { platform::wpe_event_scroll_is_stop(event.0) };
        // SAFETY: a live scroll event.
        let modifiers = unsafe { platform::wpe_event_get_modifiers(event.0) };
        assert_eq!(actual_precise != 0, precise);
        assert_eq!(actual_stop != 0, stop);
        assert_eq!(modifiers, Modifiers::SHIFT.bits());
        assert_eq!((delta_x, delta_y), (-2.0, 3.5));
        check_position(&event, 11.0, 22.0);
    }
}

#[test]
fn key_events_preserve_hardware_codes_and_resolve_a_missing_code_through_the_keymap() {
    let view = view();
    for pressed in [true, false] {
        let event = OwnedEvent(key_event(
            view.as_wpe(),
            81,
            Key {
                pressed,
                keyval: 0x61,
                keycode: Some(38),
                modifiers: Modifiers::ALT,
            },
        ));
        // SAFETY: a live keyboard event.
        let keycode = unsafe { platform::wpe_event_keyboard_get_keycode(event.0) };
        // SAFETY: a live keyboard event.
        let keyval = unsafe { platform::wpe_event_keyboard_get_keyval(event.0) };
        // SAFETY: a live keyboard event.
        let source = unsafe { platform::wpe_event_get_input_source(event.0) };
        // SAFETY: a live keyboard event.
        let kind = unsafe { platform::wpe_event_get_event_type(event.0) };
        assert_eq!(keycode, 38);
        assert_eq!(keyval, 0x61);
        assert_eq!(source, platform::WPE_INPUT_SOURCE_KEYBOARD);
        assert_eq!(
            kind,
            if pressed {
                platform::WPE_EVENT_KEYBOARD_KEY_DOWN
            } else {
                platform::WPE_EVENT_KEYBOARD_KEY_UP
            }
        );
    }
    let unknown = u32::MAX;
    let event = OwnedEvent(key_event(
        view.as_wpe(),
        82,
        Key {
            pressed: true,
            keyval: unknown,
            keycode: None,
            modifiers: Modifiers::empty(),
        },
    ));
    // SAFETY: a live keyboard event; no keymap entry can name an invalid keysym.
    assert_eq!(unsafe { platform::wpe_event_keyboard_get_keycode(event.0) }, 0);
    let _ = event_time();
}

#[test]
fn cancelled_policy_interrupted_and_other_load_errors_keep_their_messages() {
    // SAFETY: these functions return the static registered error-domain identifiers.
    let network = unsafe { webkit::webkit_network_error_quark() };
    // SAFETY: returns a static registered error-domain identifier.
    let policy = unsafe { webkit::webkit_policy_error_quark() };
    for (domain, code, expected) in [
        (
            network,
            webkit::WEBKIT_NETWORK_ERROR_CANCELLED,
            LoadErrorKind::Cancelled,
        ),
        (
            policy,
            webkit::WEBKIT_POLICY_ERROR_FRAME_LOAD_INTERRUPTED_BY_POLICY_CHANGE,
            LoadErrorKind::Interrupted,
        ),
        (
            policy,
            webkit::WEBKIT_POLICY_ERROR_CANNOT_SHOW_MIME_TYPE,
            LoadErrorKind::Interrupted,
        ),
        (network, webkit::WEBKIT_NETWORK_ERROR_FAILED, LoadErrorKind::Failed),
    ] {
        // SAFETY: the quarks above are registered GLib domain identifiers.
        let domain = unsafe { glib::translate::from_glib(domain) };
        let failure = load_error(&glib::Error::with_domain(domain, code, "synthetic load error"));
        assert_eq!(failure.kind, expected);
        assert_eq!(failure.message, "synthetic load error");
    }
}

#[test]
fn navigation_types_keep_unknown_webkit_values_as_other() {
    for (raw, expected) in [
        (webkit::WEBKIT_NAVIGATION_TYPE_LINK_CLICKED, NavigationType::LinkClicked),
        (
            webkit::WEBKIT_NAVIGATION_TYPE_FORM_SUBMITTED,
            NavigationType::FormSubmitted,
        ),
        (webkit::WEBKIT_NAVIGATION_TYPE_BACK_FORWARD, NavigationType::BackForward),
        (webkit::WEBKIT_NAVIGATION_TYPE_RELOAD, NavigationType::Reload),
        (
            webkit::WEBKIT_NAVIGATION_TYPE_FORM_RESUBMITTED,
            NavigationType::FormResubmitted,
        ),
        (i32::MAX, NavigationType::Other),
    ] {
        assert_eq!(navigation_type_of(raw), expected);
    }
}
