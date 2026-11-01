//! Input methods are `GObjects`: these tests need no browser, compositor or desktop input method.

use std::cell::RefCell;
use std::rc::Rc;

use glib::translate::from_glib_full;

use super::*;
use crate::engine::platform::{Display, Link, View};
use crate::spec::ViewId;

fn method() -> (InputMethod, View) {
    let display: Display = glib::Object::new();
    let view: View = glib::Object::builder().property("display", &display).build();
    let raw = InputMethod::new_for(view.as_wpe());
    // SAFETY: new_for returns one newly owned reference to the context of the live view.
    let context: WpeInputMethodContext = unsafe { from_glib_full(raw) };
    (context.downcast().unwrap(), view)
}

fn raw(method: &InputMethod) -> *mut platform::WPEInputMethodContext {
    method.upcast_ref::<WpeInputMethodContext>().to_glib_none().0
}

fn preedit(method: &InputMethod) -> (String, u32) {
    let mut text = ptr::null_mut();
    let mut underlines = ptr::null_mut();
    let mut cursor = u32::MAX;
    // SAFETY: a live context and valid writable output pointers; the returned string transfers to us.
    unsafe { get_preedit_string(raw(method), &raw mut text, &raw mut underlines, &raw mut cursor) };
    assert!(underlines.is_null());
    // SAFETY: the getter returned a GLib-allocated, owned string.
    let text: String = unsafe { from_glib_full(text) };
    (text, cursor)
}

#[test]
fn composition_signals_bracket_updates_and_committing_ends_the_preedit_first() {
    let (method, _) = method();
    let signals = Rc::new(RefCell::new(Vec::<String>::new()));
    for name in ["preedit-started", "preedit-changed", "preedit-finished", "committed"] {
        let signals = Rc::clone(&signals);
        method.connect_local(name, false, move |values| {
            let note = if name == "committed" {
                format!("committed:{}", values[1].get::<String>().unwrap())
            } else {
                name.to_owned()
            };
            signals.borrow_mut().push(note);
            None
        });
    }
    assert_eq!(preedit(&method), (String::new(), 0));
    method.preedit("あß", 2);
    assert_eq!(preedit(&method), ("あß".to_owned(), 2));
    method.preedit("あß字", 1);
    assert_eq!(preedit(&method), ("あß字".to_owned(), 1));
    method.commit("完成");
    assert_eq!(preedit(&method), (String::new(), 0));
    method.commit("!");
    assert_eq!(
        *signals.borrow(),
        [
            "preedit-started",
            "preedit-changed",
            "preedit-changed",
            "preedit-changed",
            "preedit-finished",
            "committed:完成",
            "committed:!"
        ]
    );
}

#[test]
fn reset_clears_composition_and_optional_output_pointers_are_supported() {
    let (method, _) = method();
    method.preedit("synthetic", 4);
    // SAFETY: all calls receive a live context; null optional output/input pointers are permitted by these vfuncs.
    unsafe { get_preedit_string(raw(&method), ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
    // SAFETY: this pass-through vfunc never dereferences the event pointer.
    let filtered = unsafe { filter_key_event(raw(&method), ptr::null_mut()) };
    assert_eq!(filtered, glib::ffi::GFALSE);
    // SAFETY: the context remains live and this focus vfunc has no side effects.
    unsafe { focus(raw(&method)) };
    // SAFETY: empty surrounding text is permitted and the vfunc reads none of it.
    unsafe { set_surrounding(raw(&method), ptr::null(), 0, 0, 0) };
    // SAFETY: resetting a live context changes only its owned composition.
    unsafe { reset(raw(&method)) };
    assert_eq!(preedit(&method), (String::new(), 0));
    method.preedit("", 0);
    assert_eq!(preedit(&method), (String::new(), 0));
}

#[test]
fn caret_changes_report_the_views_id_and_stop_after_detach() {
    let (method, view) = method();
    let (events, queue) = async_channel::unbounded();
    let id = ViewId(32);
    view.attach(Link { id, events });
    // SAFETY: the context owns a reference to its live view.
    unsafe { set_cursor_area(raw(&method), 10, 20, 3, 18) };
    match queue.try_recv().unwrap() {
        Event::ImeCursorArea { view, area } => {
            assert_eq!(view, id);
            assert_eq!(
                area,
                Area {
                    x: 10,
                    y: 20,
                    width: 3,
                    height: 18
                }
            );
        }
        other => panic!("unexpected event {other:?}"),
    }
    view.detach();
    while queue.try_recv().is_ok() {}
    // SAFETY: the context and view are still live, but no longer linked to a page.
    unsafe { set_cursor_area(raw(&method), 50, 60, 2, 16) };
    assert!(queue.try_recv().is_err());
}
