//! The display's input method: an `WPEInputMethodContext` per view (the display's `create_input_method_context`)
//! that takes the consumer's input method instead of a system one. The consumer's composition arrives as a preedit
//! ([`InputMethod::preedit`]: `preedit-started`, `preedit-changed` and `preedit-finished` for `WebKit`, which reads it
//! back through `get_preedit_string`) and its committed text as `committed` ([`InputMethod::commit`]); the caret's area
//! `WebKit` reports goes to the consumer as [`crate::Event::ImeCursorArea`], where it places its candidate window.
//! Key events pass through unfiltered: the consumer's own input method saw them first.

use std::ffi::c_int;
use std::ptr;

use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::{IntoGlibPtr, ToGlibPtr, from_glib_borrow};

use crate::event::{Area, Event};
use crate::platform_sys as platform;

glib::wrapper! {
    /// `WPEInputMethodContext`.
    pub(crate) struct WpeInputMethodContext(
        Object<platform::WPEInputMethodContext, platform::WPEInputMethodContextClass>
    );
    match fn { type_ => || platform::wpe_input_method_context_get_type(), }
}

// SAFETY: a GObject class whose class struct starts with `GObjectClass` (the -sys crate's layout tests check it against
// the header); the subclass adds nothing to the instance or class struct the parent sees.
unsafe impl<T: ObjectImpl> IsSubclassable<T> for WpeInputMethodContext {}

mod imp {
    use std::cell::{Cell, RefCell};

    use glib::subclass::prelude::*;

    use super::{
        WpeInputMethodContext, filter_key_event, focus, get_preedit_string, reset, set_cursor_area, set_surrounding,
    };

    #[derive(Default)]
    pub(crate) struct InputMethod {
        pub(super) preedit: RefCell<String>,
        /// The caret, in characters into the preedit.
        pub(super) cursor: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for InputMethod {
        const NAME: &'static str = "TeamsWpeInputMethod";
        type Type = super::InputMethod;
        type ParentType = WpeInputMethodContext;

        fn class_init(klass: &mut Self::Class) {
            let class = klass.upcast_ref_mut::<WpeInputMethodContext>().as_mut();
            class.get_preedit_string = Some(get_preedit_string);
            class.filter_key_event = Some(filter_key_event);
            class.focus_in = Some(focus);
            class.focus_out = Some(focus);
            class.set_cursor_area = Some(set_cursor_area);
            class.set_surrounding = Some(set_surrounding);
            class.reset = Some(reset);
        }
    }

    impl ObjectImpl for InputMethod {}
}

glib::wrapper! {
    /// A view's input method (module docs).
    pub(crate) struct InputMethod(ObjectSubclass<imp::InputMethod>) @extends WpeInputMethodContext;
}

impl InputMethod {
    /// A context for `view`, a new reference the display's vfunc hands to `WebKit`.
    pub(crate) fn new_for(view: *mut platform::WPEView) -> *mut platform::WPEInputMethodContext {
        // SAFETY: WPE passes a live view; borrowing takes no reference.
        let view: glib::translate::Borrowed<super::platform::WpeView> = unsafe { from_glib_borrow(view) };
        let context: Self = glib::Object::builder().property("view", &*view).build();
        context.upcast::<WpeInputMethodContext>().into_glib_ptr()
    }

    fn view(&self) -> Option<super::platform::View> {
        let raw: *mut platform::WPEInputMethodContext = self.upcast_ref::<WpeInputMethodContext>().to_glib_none().0;
        // SAFETY: a live context; the view it returns is owned by it (transfer none).
        let view = unsafe { platform::wpe_input_method_context_get_view(raw) };
        if view.is_null() {
            return None;
        }
        // SAFETY: a live view; `from_glib_none` takes a reference.
        let view: glib::Object = unsafe { glib::translate::from_glib_none(view.cast::<glib::gobject_ffi::GObject>()) };
        view.downcast().ok()
    }

    /// The composition is now `text` with the caret `cursor` characters in; an empty one ends it.
    pub(crate) fn preedit(&self, text: &str, cursor: u32) {
        let was = !self.imp().preedit.borrow().is_empty();
        self.imp().preedit.replace(text.to_owned());
        self.imp().cursor.set(cursor);
        if !was && !text.is_empty() {
            self.emit_by_name::<()>("preedit-started", &[]);
        }
        self.emit_by_name::<()>("preedit-changed", &[]);
        if was && text.is_empty() {
            self.emit_by_name::<()>("preedit-finished", &[]);
        }
    }

    /// `text` was committed: the composition ends and the text goes into the field.
    pub(crate) fn commit(&self, text: &str) {
        if !self.imp().preedit.borrow().is_empty() {
            self.preedit("", 0);
        }
        self.emit_by_name::<()>("committed", &[&text]);
    }
}

/// The input method behind a vfunc's instance pointer.
///
/// # Safety
///
/// `context` points to a live `WPEInputMethodContext`.
unsafe fn input_method(context: *mut platform::WPEInputMethodContext) -> Option<InputMethod> {
    // SAFETY: the caller guarantees a live GObject; borrowing takes no reference.
    let object: glib::translate::Borrowed<glib::Object> =
        unsafe { from_glib_borrow(context.cast::<glib::gobject_ffi::GObject>()) };
    object.downcast_ref::<InputMethod>().cloned()
}

unsafe extern "C" fn get_preedit_string(
    context: *mut platform::WPEInputMethodContext,
    text: *mut *mut std::ffi::c_char,
    underlines: *mut *mut glib::ffi::GList,
    cursor: *mut u32,
) {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let method = unsafe { input_method(context) };
    let (preedit, at) = method.map_or_else(
        || (String::new(), 0),
        |method| (method.imp().preedit.borrow().clone(), method.imp().cursor.get()),
    );
    if !text.is_null() {
        // SAFETY: `text` is the caller's out-pointer, which takes a string GLib allocated (transfer full).
        unsafe { *text = preedit.to_glib_full() };
    }
    if !underlines.is_null() {
        // SAFETY: as above; no underlines.
        unsafe { *underlines = ptr::null_mut() };
    }
    if !cursor.is_null() {
        // SAFETY: as above.
        unsafe { *cursor = at };
    }
}

unsafe extern "C" fn filter_key_event(
    _: *mut platform::WPEInputMethodContext,
    _: *mut platform::WPEEvent,
) -> glib::ffi::gboolean {
    glib::ffi::GFALSE
}

unsafe extern "C" fn focus(_: *mut platform::WPEInputMethodContext) {}

unsafe extern "C" fn reset(context: *mut platform::WPEInputMethodContext) {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    if let Some(method) = unsafe { input_method(context) } {
        method.imp().preedit.borrow_mut().clear();
        method.imp().cursor.set(0);
    }
}

unsafe extern "C" fn set_surrounding(
    _: *mut platform::WPEInputMethodContext,
    _: *const std::ffi::c_char,
    _: u32,
    _: u32,
    _: u32,
) {
}

unsafe extern "C" fn set_cursor_area(
    context: *mut platform::WPEInputMethodContext,
    x: c_int,
    y: c_int,
    width: c_int,
    height: c_int,
) {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(view) = (unsafe { input_method(context) }).and_then(|method| method.view()) else {
        return;
    };
    let area = Area { x, y, width, height };
    view.report(|id| Event::ImeCursorArea { view: id, area });
}

#[cfg(test)]
#[path = "../../tests/support/ime.rs"]
mod tests;
