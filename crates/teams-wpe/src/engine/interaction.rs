//! What `WebKitGTK` draws or connects itself and an offscreen page cannot: the clipboard, `<select>` popups and the
//! context menu. Input methods are the display's (`super::ime`).
//!
//! - The clipboard: `WPEPlatform`'s display keeps a clipboard of its own (`wpe_display_get_clipboard`, a local
//!   `WPEClipboard` for a display that names none), which every page copies to and pastes from. A change `WebKit` made
//!   (a copy, a cut) is reported as [`Event::Clipboard`]; the consumer puts the desktop's text there before a paste
//!   ([`set_clipboard`]). Text only: a page never gets files from the clipboard, so a paste can carry none.
//! - `<select>` popups: `show-option-menu` hands over a `WebKitOptionMenu`, held by token until the consumer picks a
//!   row or closes it ([`choose_option`]); a popup `WebKit` closes itself is reported.
//! - The context menu: the WPE port has no menu of its own to show (`WebContextMenuProxyWPE` draws nothing and its
//!   items' actions are connected to nothing), so `context-menu` reports what lies under the pointer
//!   ([`ContextMenu`]) and the consumer draws a menu whose items run editing commands, history steps or its own
//!   clipboard writes.

use std::cell::Cell;
use std::ffi::c_char;

use async_channel::Sender;
use glib::prelude::*;
use glib::translate::{ToGlibPtr, from_glib_none};

use super::platform::Display;
use super::webkit::{WebView, c_string};
use super::{send, token, with};
use crate::event::{Area, ContextMenu, EditingCommand, Event, MenuToken, OptionItem, OptionKind, OptionMenu};
use crate::platform_sys as platform;
use crate::spec::ViewId;
use crate::webkit_sys as webkit;

/// The plain-text format a page copies as, and pastes from.
const TEXT_FORMAT: &str = "text/plain;charset=utf-8";
const HTML_FORMAT: &str = "text/html";

thread_local! {
    /// The engine is putting the consumer's text on the clipboard: that change is not a page's copy.
    static SEEDING: Cell<bool> = const { Cell::new(false) };
}

glib::wrapper! {
    /// `WPEClipboard`.
    pub(crate) struct Clipboard(Object<platform::WPEClipboard, platform::WPEClipboardClass>);
    match fn { type_ => || platform::wpe_clipboard_get_type(), }
}

glib::wrapper! {
    /// `WebKitOptionMenu`.
    pub(crate) struct OptionMenuObject(Object<webkit::WebKitOptionMenu, webkit::WebKitOptionMenuClass>);
    match fn { type_ => || webkit::webkit_option_menu_get_type(), }
}

glib::wrapper! {
    /// `WebKitHitTestResult`.
    pub(crate) struct HitTestResult(Object<webkit::WebKitHitTestResult, webkit::WebKitHitTestResultClass>);
    match fn { type_ => || webkit::webkit_hit_test_result_get_type(), }
}

glib::wrapper! {
    /// `WebKitContextMenu`.
    pub(crate) struct ContextMenuObject(Object<webkit::WebKitContextMenu, webkit::WebKitContextMenuClass>);
    match fn { type_ => || webkit::webkit_context_menu_get_type(), }
}

// MARK: - the clipboard

/// The display's clipboard.
fn clipboard(display: &Display) -> Option<Clipboard> {
    // SAFETY: a live display; the clipboard is owned by it (transfer none) and `from_glib_none` takes a reference.
    let raw = unsafe { platform::wpe_display_get_clipboard(display.as_wpe()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a live WPEClipboard.
    Some(unsafe { from_glib_none(raw) })
}

/// Reports every change a page makes to the display's clipboard on `events`.
pub(super) fn watch_clipboard(display: &Display, events: Sender<Event>) {
    let Some(clipboard) = clipboard(display) else {
        log::warn!("the WPE display has no clipboard: a page's copy stays in the page");
        return;
    };
    clipboard.connect_notify_local(Some("change-count"), move |clipboard, _| {
        if SEEDING.get() {
            return;
        }
        let Some(text) = read_text(clipboard, TEXT_FORMAT) else {
            return;
        };
        let html = read_text(clipboard, HTML_FORMAT);
        send(&events, Event::Clipboard { text, html });
    });
}

/// The clipboard's content in `format`, as text.
fn read_text(clipboard: &Clipboard, format: &str) -> Option<String> {
    let format = c_string(format);
    let mut size = 0;
    let raw: *mut platform::WPEClipboard = clipboard.to_glib_none().0;
    // SAFETY: a live clipboard, a NUL-terminated format and a valid out-pointer; the text is ours (transfer full).
    let text = unsafe { platform::wpe_clipboard_read_text(raw, format.as_ptr(), &raw mut size) };
    if text.is_null() {
        return None;
    }
    // SAFETY: the call returned `size` bytes at `text`, which we free after copying them.
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), size) }.to_vec();
    // SAFETY: `text` came from GLib's allocator.
    unsafe { glib::ffi::g_free(text.cast()) };
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Puts `text` on the display's clipboard, which the next paste reads.
pub(super) fn set_clipboard(display: &Display, text: &str) {
    let Some(clipboard) = clipboard(display) else {
        return;
    };
    let text = c_string(text);
    // SAFETY: a fresh content of ours, which `set_text` copies the NUL-terminated text into.
    let content = unsafe { platform::wpe_clipboard_content_new() };
    // SAFETY: as above.
    unsafe { platform::wpe_clipboard_content_set_text(content, text.as_ptr()) };
    SEEDING.set(true);
    let raw: *mut platform::WPEClipboard = clipboard.to_glib_none().0;
    // SAFETY: a live clipboard and content; the clipboard takes its own reference.
    unsafe { platform::wpe_clipboard_set_content(raw, content) };
    SEEDING.set(false);
    // SAFETY: drops our reference.
    unsafe { platform::wpe_clipboard_content_unref(content) };
}

// MARK: - option popups

/// `show-option-menu` on page `view`'s web view: the popup is held by token and reported.
pub(super) fn connect_option_menu(web_view: &WebView, view: ViewId) {
    web_view.connect_local("show-option-menu", false, move |values| {
        let menu = super::webkit::object_arg::<OptionMenuObject>(values, 1);
        let area = rectangle(super::webkit::pointer_arg(values, 2).cast());
        if let Some(menu) = menu {
            show_option_menu(view, &menu, area);
        }
        super::webkit::handled(true)
    });
}

/// A `WebKitRectangle` the signal passes, or an empty area for none.
fn rectangle(rect: *const webkit::WebKitRectangle) -> Area {
    if rect.is_null() {
        return Area::default();
    }
    // SAFETY: the signal passes a live rectangle for the call.
    let rect = unsafe { *rect };
    Area {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

fn show_option_menu(view: ViewId, menu: &OptionMenuObject, area: Area) {
    let items = option_items(menu);
    let Some((token, events)) = with(|engine| {
        let token = MenuToken(token(engine));
        engine.menus.insert(token, menu.clone());
        (token, engine.events.clone())
    }) else {
        return;
    };
    menu.connect_local("close", false, move |_| {
        if with(|engine| engine.menus.remove(&token)).flatten().is_some() {
            send(&events, Event::OptionMenuClosed { token });
        }
        None
    });
    with(|engine| {
        send(
            &engine.events,
            Event::OptionMenu(OptionMenu {
                token,
                view,
                area,
                items,
            }),
        );
    });
}

/// The popup's rows.
fn option_items(menu: &OptionMenuObject) -> Vec<OptionItem> {
    let raw: *mut webkit::WebKitOptionMenu = menu.to_glib_none().0;
    // SAFETY: a live menu.
    let count = unsafe { webkit::webkit_option_menu_get_n_items(raw) };
    (0..count)
        .filter_map(|index| {
            // SAFETY: a live menu and an index below its count; the item is owned by the menu.
            let item = unsafe { webkit::webkit_option_menu_get_item(raw, index) };
            (!item.is_null()).then(|| option_item(item))
        })
        .collect()
}

/// A string owned by `WebKit`, or null, copied.
fn copied(raw: *const c_char) -> Option<String> {
    // SAFETY: the getters this reads hand out a NUL-terminated string owned by their object, or null.
    let text: Option<String> = unsafe { from_glib_none(raw) };
    text.filter(|text| !text.is_empty())
}

fn flag(value: glib::ffi::gboolean) -> bool {
    value != glib::ffi::GFALSE
}

/// One row of a popup, from a live item the popup owns.
fn option_item(item: *mut webkit::WebKitOptionMenuItem) -> OptionItem {
    // SAFETY: each getter below reads the live item.
    let label = copied(unsafe { webkit::webkit_option_menu_item_get_label(item) });
    // SAFETY: as above.
    let tooltip = copied(unsafe { webkit::webkit_option_menu_item_get_tooltip(item) });
    // SAFETY: as above.
    let enabled = flag(unsafe { webkit::webkit_option_menu_item_is_enabled(item) });
    // SAFETY: as above.
    let selected = flag(unsafe { webkit::webkit_option_menu_item_is_selected(item) });
    // SAFETY: as above.
    let group_label = flag(unsafe { webkit::webkit_option_menu_item_is_group_label(item) });
    // SAFETY: as above.
    let group_child = flag(unsafe { webkit::webkit_option_menu_item_is_group_child(item) });
    let kind = match (group_label, group_child) {
        (true, _) => OptionKind::GroupLabel,
        (false, true) => OptionKind::GroupChild,
        (false, false) => OptionKind::Option,
    };
    OptionItem {
        label: label.unwrap_or_default(),
        tooltip,
        kind,
        enabled,
        selected,
    }
}

/// The consumer's answer to popup `token`: the row at `index` (the select changes and the popup closes), or none.
pub(super) fn choose_option(token: MenuToken, index: Option<u32>) {
    let Some(Some(menu)) = with(|engine| engine.menus.remove(&token)) else {
        return;
    };
    let raw: *mut webkit::WebKitOptionMenu = menu.to_glib_none().0;
    match index {
        // SAFETY: a live menu; an index past its rows is ignored by WebKit.
        Some(index) => unsafe { webkit::webkit_option_menu_activate_item(raw, index) },
        // SAFETY: a live menu.
        None => unsafe { webkit::webkit_option_menu_close(raw) },
    }
}

// MARK: - the context menu

/// `context-menu` on page `view`'s web view: what lies under the pointer is reported, `WebKit` shows nothing; `at` is
/// where the last press was, should the menu not say.
pub(super) fn connect_context_menu(
    web_view: &WebView,
    view: ViewId,
    events: Sender<Event>,
    at: impl Fn() -> (i32, i32) + 'static,
) {
    web_view.connect_local("context-menu", false, move |values| {
        let menu = super::webkit::object_arg::<ContextMenuObject>(values, 1);
        let hit = super::webkit::object_arg::<HitTestResult>(values, 2);
        let (x, y) = menu.as_ref().and_then(menu_position).unwrap_or_else(&at);
        if let Some(hit) = hit {
            send(&events, Event::ContextMenu(context_of(view, x, y, &hit)));
        }
        super::webkit::handled(true)
    });
}

fn menu_position(menu: &ContextMenuObject) -> Option<(i32, i32)> {
    let (mut x, mut y) = (0, 0);
    let raw: *mut webkit::WebKitContextMenu = menu.to_glib_none().0;
    // SAFETY: a live menu and valid out-pointers.
    let known = unsafe { webkit::webkit_context_menu_get_position(raw, &raw mut x, &raw mut y) };
    (known != glib::ffi::GFALSE).then_some((x, y))
}

fn context_of(view: ViewId, x: i32, y: i32, hit: &HitTestResult) -> ContextMenu {
    let raw: *mut webkit::WebKitHitTestResult = hit.to_glib_none().0;
    ContextMenu {
        view,
        x,
        y,
        // SAFETY: each getter here reads the live hit test result.
        link: copied(unsafe { webkit::webkit_hit_test_result_get_link_uri(raw) }),
        // SAFETY: as above.
        image: copied(unsafe { webkit::webkit_hit_test_result_get_image_uri(raw) }),
        // SAFETY: as above.
        media: copied(unsafe { webkit::webkit_hit_test_result_get_media_uri(raw) }),
        // SAFETY: as above.
        editable: flag(unsafe { webkit::webkit_hit_test_result_context_is_editable(raw) }),
        // SAFETY: as above.
        selection: flag(unsafe { webkit::webkit_hit_test_result_context_is_selection(raw) }),
    }
}

/// Runs `command` on `web_view`'s focused content.
pub(super) fn editing_command(web_view: &WebView, command: EditingCommand) {
    let name: &[u8] = match command {
        EditingCommand::Copy => webkit::WEBKIT_EDITING_COMMAND_COPY,
        EditingCommand::Cut => webkit::WEBKIT_EDITING_COMMAND_CUT,
        EditingCommand::Paste => webkit::WEBKIT_EDITING_COMMAND_PASTE,
        EditingCommand::PasteAsPlainText => webkit::WEBKIT_EDITING_COMMAND_PASTE_AS_PLAIN_TEXT,
        EditingCommand::SelectAll => webkit::WEBKIT_EDITING_COMMAND_SELECT_ALL,
        EditingCommand::Undo => webkit::WEBKIT_EDITING_COMMAND_UNDO,
        EditingCommand::Redo => webkit::WEBKIT_EDITING_COMMAND_REDO,
    };
    // SAFETY: a live web view and one of WebKit's NUL-terminated command names.
    unsafe { webkit::webkit_web_view_execute_editing_command(web_view.raw(), name.as_ptr().cast()) };
}
