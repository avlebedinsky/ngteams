//! Clipboard transfers use an isolated WPE display object, never the desktop clipboard.

use super::*;

#[test]
fn consumer_seeding_is_quiet_and_a_page_copy_reports_text_and_optional_html() {
    let display: Display = glib::Object::new();
    let (events, queue) = async_channel::unbounded();
    watch_clipboard(&display, events);
    set_clipboard(&display, "synthetic consumer text");
    let clipboard = clipboard(&display).unwrap();
    assert_eq!(
        read_text(&clipboard, TEXT_FORMAT).as_deref(),
        Some("synthetic consumer text")
    );
    assert!(queue.try_recv().is_err(), "seeding is not a page copy");
    let raw: *mut platform::WPEClipboard = clipboard.to_glib_none().0;
    let text = c_string("synthetic page copy");
    let html_format = c_string(HTML_FORMAT);
    let html = glib::Bytes::from_static(b"<b>synthetic page copy</b>");
    // SAFETY: a fresh owned content object, live clipboard and bytes; setters take their own references/copies.
    let content = unsafe { platform::wpe_clipboard_content_new() };
    // SAFETY: the live content copies the NUL-terminated text.
    unsafe { platform::wpe_clipboard_content_set_text(content, text.as_ptr()) };
    // SAFETY: the live content takes a reference to the bytes for the NUL-terminated MIME type.
    unsafe { platform::wpe_clipboard_content_set_bytes(content, html_format.as_ptr(), html.to_glib_none().0) };
    // SAFETY: the live clipboard takes its own reference to the content.
    unsafe { platform::wpe_clipboard_set_content(raw, content) };
    // SAFETY: drops our reference after the clipboard took its own.
    unsafe { platform::wpe_clipboard_content_unref(content) };
    match queue.try_recv().unwrap() {
        Event::Clipboard { text, html } => {
            assert_eq!(text, "synthetic page copy");
            assert_eq!(html.as_deref(), Some("<b>synthetic page copy</b>"));
        }
        other => panic!("unexpected event {other:?}"),
    }
    assert!(queue.try_recv().is_err());
}

#[test]
fn an_unsupported_clipboard_format_and_an_empty_content_have_no_text() {
    let display: Display = glib::Object::new();
    let clipboard = clipboard(&display).unwrap();
    assert_eq!(read_text(&clipboard, "application/x-synthetic-unknown"), None);
    let raw: *mut platform::WPEClipboard = clipboard.to_glib_none().0;
    let (events, queue) = async_channel::unbounded();
    watch_clipboard(&display, events);
    // SAFETY: the clipboard takes a reference to a fresh empty content object before our reference is dropped.
    let content = unsafe { platform::wpe_clipboard_content_new() };
    // SAFETY: the live clipboard takes its own reference to the content.
    unsafe { platform::wpe_clipboard_set_content(raw, content) };
    // SAFETY: drops our reference after the clipboard took its own.
    unsafe { platform::wpe_clipboard_content_unref(content) };
    assert!(queue.try_recv().is_err(), "non-text copies are not emitted as text");
}

#[test]
fn popup_rectangles_and_nullable_labels_are_copied_without_borrowing() {
    assert_eq!(rectangle(std::ptr::null()), Area::default());
    let rect = webkit::WebKitRectangle {
        x: -4,
        y: 12,
        width: 80,
        height: 24,
    };
    assert_eq!(
        rectangle(&raw const rect),
        Area {
            x: -4,
            y: 12,
            width: 80,
            height: 24
        }
    );
    assert_eq!(copied(std::ptr::null()), None);
    assert_eq!(copied(c_string("").as_ptr()), None);
    assert_eq!(
        copied(c_string("synthetic label").as_ptr()),
        Some("synthetic label".to_owned())
    );
    assert!(!flag(glib::ffi::GFALSE));
    assert!(flag(glib::ffi::GTRUE));
    assert!(flag(-1));
}
