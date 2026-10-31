//! The engine's `WPEPlatform` classes: a display that renders on the consumer's GPU with the formats it imports, a
//! view that hands its frames to the consumer, and a toplevel that carries the page's size and scale.
//!
//! These are subclasses of the abstract `WPEDisplay`, `WPEView` and `WPEToplevel` rather than the headless platform's
//! classes with patched vfuncs (ngterm's Task 2 spike): the frames, the buffer set and the acknowledgements are this crate's
//! (crate `frames`), and the device and formats are the consumer's ([`crate::DmaBufTarget`]). Two things are borrowed
//! from a private headless display on the same device: the DRM device, which it resolves through libdrm (the primary
//! node beside the render node), and the EGL display it opens on that device through GBM, which saves linking GBM and
//! EGL here.

use std::ffi::{CString, c_char, c_int};
use std::os::fd::BorrowedFd;
use std::ptr;
use std::sync::Arc;

use async_channel::Sender;
use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::{Borrowed, IntoGlibPtr, ToGlibPtr, from_glib_borrow, from_glib_none};

use super::send;
use crate::config::DmaBufTarget;
use crate::event::Event;
use crate::frames::{Ack, BufferId, DmaBuf, Frame, FrameContent, FrameFlow, Kind, Plane, Rect, copy_regions};
use crate::platform_sys as platform;
use crate::spec::ViewId;

// WPEPlatformHeadless lives in libWPEWebKit-2.0 with the rest of WPEPlatform; the -sys crates bind no Headless
// namespace (ngterm's Task 30), so the constructor used here is declared here.
unsafe extern "C" {
    fn wpe_display_headless_new_for_device(
        name: *const c_char,
        error: *mut *mut glib::ffi::GError,
    ) -> *mut platform::WPEDisplay;
}

glib::wrapper! {
    /// `WPEDisplay`.
    pub(crate) struct WpeDisplay(Object<platform::WPEDisplay, platform::WPEDisplayClass>);
    match fn { type_ => || platform::wpe_display_get_type(), }
}

glib::wrapper! {
    /// `WPEView`.
    pub(crate) struct WpeView(Object<platform::WPEView, platform::WPEViewClass>);
    match fn { type_ => || platform::wpe_view_get_type(), }
}

glib::wrapper! {
    /// `WPEToplevel`.
    pub(crate) struct WpeToplevel(Object<platform::WPEToplevel, platform::WPEToplevelClass>);
    match fn { type_ => || platform::wpe_toplevel_get_type(), }
}

glib::wrapper! {
    /// `WPEBuffer`.
    pub(crate) struct WpeBuffer(Object<platform::WPEBuffer, platform::WPEBufferClass>);
    match fn { type_ => || platform::wpe_buffer_get_type(), }
}

glib::wrapper! {
    /// `WPEBufferDMABuf`.
    pub(crate) struct WpeBufferDmaBuf(Object<platform::WPEBufferDMABuf, platform::WPEBufferDMABufClass>)
        @extends WpeBuffer;
    match fn { type_ => || platform::wpe_buffer_dma_buf_get_type(), }
}

glib::wrapper! {
    /// `WPEBufferSHM`.
    pub(crate) struct WpeBufferShm(Object<platform::WPEBufferSHM, platform::WPEBufferSHMClass>) @extends WpeBuffer;
    match fn { type_ => || platform::wpe_buffer_shm_get_type(), }
}

// SAFETY: the three are GObject classes whose class structs start with `GObjectClass` (the -sys crates' layout tests
// check them against the headers); a subclass adds nothing to the instance or class struct the parent sees.
unsafe impl<T: ObjectImpl> IsSubclassable<T> for WpeDisplay {}
// SAFETY: as above.
unsafe impl<T: ObjectImpl> IsSubclassable<T> for WpeView {}
// SAFETY: as above.
unsafe impl<T: ObjectImpl> IsSubclassable<T> for WpeToplevel {}

/// The object behind a vfunc's instance pointer, borrowed for the call.
///
/// # Safety
///
/// `instance` points to a live `GObject` for the duration of the borrow.
unsafe fn borrow_instance<P>(instance: *mut P) -> Borrowed<glib::Object> {
    // SAFETY: the caller guarantees a live GObject; borrowing takes no reference.
    unsafe { from_glib_borrow(instance.cast::<glib::gobject_ffi::GObject>()) }
}

// MARK: - display

mod display_imp {
    use std::cell::RefCell;

    use glib::subclass::prelude::*;

    use super::{
        DmaBufTarget, WpeDisplay, display_buffer_formats, display_connect, display_create_input_method,
        display_create_toplevel, display_create_view, display_drm_device, display_egl_display,
    };

    /// The frame target (`None`: SHM) and the headless display the device and EGL display come from.
    #[derive(Default)]
    pub(crate) struct Display {
        pub(super) target: RefCell<Option<DmaBufTarget>>,
        pub(super) delegate: RefCell<Option<WpeDisplay>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Display {
        const NAME: &'static str = "TeamsWpeDisplay";
        type Type = super::Display;
        type ParentType = WpeDisplay;

        fn class_init(klass: &mut Self::Class) {
            let class = klass.upcast_ref_mut::<WpeDisplay>().as_mut();
            class.connect = Some(display_connect);
            class.create_view = Some(display_create_view);
            class.create_toplevel = Some(display_create_toplevel);
            class.get_egl_display = Some(display_egl_display);
            class.get_drm_device = Some(display_drm_device);
            class.get_preferred_buffer_formats = Some(display_buffer_formats);
            class.create_input_method_context = Some(display_create_input_method);
        }
    }

    impl ObjectImpl for Display {}
}

glib::wrapper! {
    /// The engine's display ([module docs](self)).
    pub(crate) struct Display(ObjectSubclass<display_imp::Display>) @extends WpeDisplay;
}

impl Display {
    /// A display for dmabuf frames on `target`'s device, or SHM frames (`None`), connected and made the primary one.
    pub(crate) fn connect(target: Option<DmaBufTarget>) -> Result<Self, String> {
        let display: Self = glib::Object::new();
        display.imp().target.replace(target);
        let mut error: *mut glib::ffi::GError = ptr::null_mut();
        // SAFETY: `display` is a live WPEDisplay and `error` a valid out-pointer.
        let connected = unsafe { platform::wpe_display_connect(display.as_wpe(), &raw mut error) };
        if connected == glib::ffi::GFALSE {
            // SAFETY: on failure the call set `error` (or left it null), which we now own.
            let error: Option<glib::Error> = unsafe { glib::translate::from_glib_full(error) };
            return Err(error.map_or_else(|| "unknown error".to_owned(), |error| error.message().to_owned()));
        }
        // SAFETY: `display` is a live WPEDisplay.
        unsafe { platform::wpe_display_set_primary(display.as_wpe()) };
        Ok(display)
    }

    /// Whether frames are dmabufs (else SHM, from the start).
    pub(crate) fn renders_dmabuf(&self) -> bool {
        self.imp().target.borrow().is_some()
    }

    /// Whether the consumer imports `fourcc` with `modifier` (it offered them).
    fn offers(&self, fourcc: u32, modifier: u64) -> bool {
        self.imp().target.borrow().as_ref().is_some_and(|target| {
            target
                .formats
                .iter()
                .any(|format| format.fourcc == fourcc && format.modifiers.contains(&modifier))
        })
    }

    pub(crate) fn as_wpe(&self) -> *mut platform::WPEDisplay {
        self.upcast_ref::<WpeDisplay>().to_glib_none().0
    }

    /// Opens the headless display that resolves the device and opens EGL on it.
    fn open_delegate(&self) -> Result<(), glib::Error> {
        let target = self.imp().target.borrow().clone();
        let Some(target) = target else {
            return Ok(());
        };
        let node = CString::new(target.render_node.as_os_str().as_encoded_bytes())
            .map_err(|_| display_error("the render node path holds a NUL byte"))?;
        let mut error: *mut glib::ffi::GError = ptr::null_mut();
        // SAFETY: `node` is a NUL-terminated path and `error` a valid out-pointer.
        let raw = unsafe { wpe_display_headless_new_for_device(node.as_ptr(), &raw mut error) };
        if raw.is_null() {
            // SAFETY: on failure the call set `error`, which we now own.
            let error: Option<glib::Error> = unsafe { glib::translate::from_glib_full(error) };
            return Err(error.unwrap_or_else(|| display_error("the DRM device was not found")));
        }
        // SAFETY: the constructor returned a new reference to a WPEDisplay, which we take over.
        let delegate: WpeDisplay = unsafe { glib::translate::from_glib_full(raw) };
        // SAFETY: `delegate` is a live WPEDisplay; the device it returns is owned by it.
        let device = unsafe { platform::wpe_display_get_drm_device(delegate.to_glib_none().0) };
        if device.is_null() {
            return Err(display_error("the DRM device has no node WebKit can render on"));
        }
        self.imp().delegate.replace(Some(delegate));
        Ok(())
    }

    fn delegate_raw(&self) -> Option<*mut platform::WPEDisplay> {
        self.imp()
            .delegate
            .borrow()
            .as_ref()
            .map(|delegate| delegate.to_glib_none().0)
    }

    /// The formats the consumer imports, on the delegate's device.
    fn buffer_formats(&self) -> *mut platform::WPEBufferFormats {
        let (Some(target), Some(delegate)) = (self.imp().target.borrow().clone(), self.delegate_raw()) else {
            return ptr::null_mut();
        };
        // SAFETY: `delegate` is a live WPEDisplay owned by `self`; the device stays owned by it.
        let device = unsafe { platform::wpe_display_get_drm_device(delegate) };
        // SAFETY: a nullable device; the builder takes its own reference.
        let builder = unsafe { platform::wpe_buffer_formats_builder_new(device) };
        // SAFETY: a fresh builder; a null device means the builder's main device.
        unsafe {
            platform::wpe_buffer_formats_builder_append_group(
                builder,
                ptr::null_mut(),
                platform::WPE_BUFFER_FORMAT_USAGE_RENDERING,
            );
        };
        for format in &target.formats {
            for &modifier in &format.modifiers {
                // SAFETY: the builder is live and has a group to append to.
                unsafe { platform::wpe_buffer_formats_builder_append_format(builder, format.fourcc, modifier) };
            }
        }
        // SAFETY: ends the builder, consuming our reference; the formats are returned with full transfer.
        unsafe { platform::wpe_buffer_formats_builder_end(builder) }
    }
}

fn display_error(message: &str) -> glib::Error {
    // SAFETY: a plain quark lookup.
    let domain = unsafe { platform::wpe_display_error_quark() };
    // SAFETY: `domain` is WPE's display error quark, a registered domain.
    let domain = unsafe { glib::translate::from_glib(domain) };
    glib::Error::with_domain(domain, platform::WPE_DISPLAY_ERROR_NOT_SUPPORTED, message)
}

/// The engine's display behind a vfunc's instance pointer.
///
/// # Safety
///
/// `display` points to a live `WPEDisplay`.
unsafe fn display_of(display: *mut platform::WPEDisplay) -> Option<Display> {
    // SAFETY: the caller guarantees a live GObject.
    let object = unsafe { borrow_instance(display) };
    object.downcast_ref::<Display>().cloned()
}

unsafe extern "C" fn display_connect(
    display: *mut platform::WPEDisplay,
    error: *mut *mut glib::ffi::GError,
) -> glib::ffi::gboolean {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(display) = (unsafe { display_of(display) }) else {
        return glib::ffi::GFALSE;
    };
    match display.open_delegate() {
        Ok(()) => glib::ffi::GTRUE,
        Err(err) => {
            if !error.is_null() {
                // SAFETY: `error` is the caller's out-pointer, which takes over the error.
                unsafe { *error = err.into_glib_ptr() };
            }
            glib::ffi::GFALSE
        }
    }
}

unsafe extern "C" fn display_create_view(display: *mut platform::WPEDisplay) -> *mut platform::WPEView {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(display) = (unsafe { display_of(display) }) else {
        return ptr::null_mut();
    };
    let view: View = glib::Object::builder().property("display", &display).build();
    // `create_view` returns a new reference, which this one becomes
    view.upcast::<WpeView>().into_glib_ptr()
}

unsafe extern "C" fn display_create_toplevel(
    display: *mut platform::WPEDisplay,
    max_views: u32,
) -> *mut platform::WPEToplevel {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(display) = (unsafe { display_of(display) }) else {
        return ptr::null_mut();
    };
    let toplevel: Toplevel = glib::Object::builder()
        .property("display", &display)
        .property("max-views", max_views)
        .build();
    // `create_toplevel` returns a new reference, which this one becomes
    toplevel.upcast::<WpeToplevel>().into_glib_ptr()
}

unsafe extern "C" fn display_egl_display(
    display: *mut platform::WPEDisplay,
    error: *mut *mut glib::ffi::GError,
) -> glib::ffi::gpointer {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let delegate = unsafe { display_of(display) }.and_then(|display| display.delegate_raw());
    if let Some(delegate) = delegate {
        // SAFETY: `delegate` is a live WPEDisplay owned by the engine's display, which owns its EGL display too.
        return unsafe { platform::wpe_display_get_egl_display(delegate, error) };
    }
    if !error.is_null() {
        let err = display_error("SHM frames: the display renders without EGL");
        // SAFETY: `error` is the caller's out-pointer, which takes over the error.
        unsafe { *error = err.into_glib_ptr() };
    }
    ptr::null_mut()
}

unsafe extern "C" fn display_drm_device(display: *mut platform::WPEDisplay) -> *mut platform::WPEDRMDevice {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let delegate = unsafe { display_of(display) }.and_then(|display| display.delegate_raw());
    match delegate {
        // SAFETY: `delegate` is a live WPEDisplay; the device is owned by it (transfer none, as the vfunc returns).
        Some(delegate) => unsafe { platform::wpe_display_get_drm_device(delegate) },
        None => ptr::null_mut(),
    }
}

unsafe extern "C" fn display_buffer_formats(display: *mut platform::WPEDisplay) -> *mut platform::WPEBufferFormats {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    unsafe { display_of(display) }.map_or(ptr::null_mut(), |display| display.buffer_formats())
}

/// A view's input method: the engine's own, which takes the consumer's ([`super::ime`]); the view keeps a weak
/// reference to reach it.
unsafe extern "C" fn display_create_input_method(
    _: *mut platform::WPEDisplay,
    view: *mut platform::WPEView,
) -> *mut platform::WPEInputMethodContext {
    let context = super::ime::InputMethod::new_for(view);
    // SAFETY: WPE passes one of its live views.
    if let Some(view) = unsafe { view_of(view) } {
        // SAFETY: `context` is the live context just made, a reference of which the caller takes over.
        let object: glib::translate::Borrowed<glib::Object> =
            unsafe { from_glib_borrow(context.cast::<glib::gobject_ffi::GObject>()) };
        if let Some(method) = object.downcast_ref::<super::ime::InputMethod>() {
            view.imp().ime.replace(Some(method.downgrade()));
        }
    }
    context
}

// MARK: - toplevel

mod toplevel_imp {
    use glib::subclass::prelude::*;

    use super::{WpeToplevel, platform, toplevel_resize};

    #[derive(Default)]
    pub(crate) struct Toplevel;

    #[glib::object_subclass]
    impl ObjectSubclass for Toplevel {
        const NAME: &'static str = "TeamsWpeToplevel";
        type Type = super::Toplevel;
        type ParentType = WpeToplevel;

        fn class_init(klass: &mut Self::Class) {
            let class = klass.upcast_ref_mut::<WpeToplevel>().as_mut();
            class.resize = Some(toplevel_resize);
        }
    }

    impl ObjectImpl for Toplevel {
        fn constructed(&self) {
            self.parent_constructed();
            // SAFETY: the instance is a live WPEToplevel; an offscreen page's toplevel is always the active one.
            unsafe {
                platform::wpe_toplevel_state_changed(self.obj().as_wpe(), platform::WPE_TOPLEVEL_STATE_ACTIVE);
            };
        }
    }
}

glib::wrapper! {
    /// A page's toplevel: its logical size and its scale.
    pub(crate) struct Toplevel(ObjectSubclass<toplevel_imp::Toplevel>) @extends WpeToplevel;
}

impl Toplevel {
    pub(crate) fn as_wpe(&self) -> *mut platform::WPEToplevel {
        self.upcast_ref::<WpeToplevel>().to_glib_none().0
    }

    /// Resizes the page to `width` x `height` logical pixels at `scale`.
    pub(crate) fn set_geometry(&self, width: c_int, height: c_int, scale: f64) {
        // SAFETY: a live toplevel; the scale is positive (checked by the caller).
        unsafe { platform::wpe_toplevel_scale_changed(self.as_wpe(), scale) };
        // SAFETY: a live toplevel; WPE calls our `resize`, which resizes its views.
        unsafe { platform::wpe_toplevel_resize(self.as_wpe(), width, height) };
    }
}

unsafe extern "C" fn toplevel_resize(
    toplevel: *mut platform::WPEToplevel,
    width: c_int,
    height: c_int,
) -> glib::ffi::gboolean {
    // SAFETY: WPE calls `resize` on a live toplevel.
    unsafe { platform::wpe_toplevel_resized(toplevel, width, height) };
    // SAFETY: a live toplevel and a callback that resizes each of its views.
    unsafe { platform::wpe_toplevel_foreach_view(toplevel, Some(resize_view), ptr::null_mut()) };
    glib::ffi::GTRUE
}

unsafe extern "C" fn resize_view(
    toplevel: *mut platform::WPEToplevel,
    view: *mut platform::WPEView,
    _: glib::ffi::gpointer,
) -> glib::ffi::gboolean {
    let (mut width, mut height) = (0, 0);
    // SAFETY: WPE passes its live toplevel; the out-pointers are valid.
    unsafe { platform::wpe_toplevel_get_size(toplevel, &raw mut width, &raw mut height) };
    // SAFETY: WPE passes one of the toplevel's live views.
    unsafe { platform::wpe_view_resized(view, width, height) };
    glib::ffi::GFALSE
}

// MARK: - view

/// Where a view's frames go, set once its page exists.
pub(crate) struct Link {
    pub(crate) id: ViewId,
    pub(crate) events: Sender<Event>,
}

/// A buffer of the current set: the engine's reference and, for a dmabuf, the host's duplicate of its planes.
struct Held {
    buffer: WpeBuffer,
    dmabuf: Option<Arc<DmaBuf>>,
}

mod view_imp {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use glib::prelude::*;
    use glib::subclass::prelude::*;

    use super::{BufferId, FrameFlow, Held, Link, WpeView, view_buffers_changed, view_render_buffer, view_set_cursor};

    #[derive(Default)]
    pub(crate) struct View {
        pub(super) link: RefCell<Option<Link>>,
        pub(super) flow: RefCell<FrameFlow>,
        pub(super) held: RefCell<HashMap<BufferId, Held>>,
        /// The view's input method, once `WebKit` asked the display for it.
        pub(crate) ime: RefCell<Option<glib::WeakRef<crate::engine::ime::InputMethod>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for View {
        const NAME: &'static str = "TeamsWpeView";
        type Type = super::View;
        type ParentType = WpeView;

        fn class_init(klass: &mut Self::Class) {
            let class = klass.upcast_ref_mut::<WpeView>().as_mut();
            class.render_buffer = Some(view_render_buffer);
            class.buffers_changed = Some(view_buffers_changed);
            class.set_cursor_from_name = Some(view_set_cursor);
        }
    }

    impl ObjectImpl for View {
        fn constructed(&self) {
            self.parent_constructed();
            let view = self.obj();
            // the toplevel carries the size: a view that gets one takes its size and maps, one that loses it unmaps
            view.connect_notify_local(Some("toplevel"), |view, _| view.toplevel_changed());
        }
    }
}

glib::wrapper! {
    /// A page's view: frames out to the consumer, acknowledgements back ([`crate::frames`]).
    pub(crate) struct View(ObjectSubclass<view_imp::View>) @extends WpeView;
}

impl View {
    pub(crate) fn as_wpe(&self) -> *mut platform::WPEView {
        self.upcast_ref::<WpeView>().to_glib_none().0
    }

    /// Sends this view's frames to `link`.
    pub(crate) fn attach(&self, link: Link) {
        self.imp().link.replace(Some(link));
    }

    /// Stops sending frames: the page is closing. Its buffers leave with it, reported as [`Event::BuffersRemoved`] so
    /// a consumer that kept them (a worker's client) lets them go.
    pub(crate) fn detach(&self) {
        let link = self.imp().link.replace(None);
        let mut buffers: Vec<BufferId> = self.imp().held.borrow_mut().drain().map(|(id, _)| id).collect();
        if let Some(link) = link
            && !buffers.is_empty()
        {
            buffers.sort_unstable();
            send(&link.events, Event::BuffersRemoved { view: link.id, buffers });
        }
    }

    fn display(&self) -> Option<Display> {
        // SAFETY: a live view; the display is owned by it (transfer none).
        let raw = unsafe { platform::wpe_view_get_display(self.as_wpe()) };
        // SAFETY: `raw` is null or a live display; `from_glib_none` takes a reference.
        let display: Option<glib::Object> = unsafe { from_glib_none(raw.cast::<glib::gobject_ffi::GObject>()) };
        display.and_then(|display| display.downcast::<Display>().ok())
    }

    pub(crate) fn toplevel(&self) -> Option<Toplevel> {
        // SAFETY: a live view; the toplevel is owned by it (transfer none).
        let raw = unsafe { platform::wpe_view_get_toplevel(self.as_wpe()) };
        // SAFETY: `raw` is null or a live toplevel; `from_glib_none` takes a reference.
        let toplevel: Option<glib::Object> = unsafe { from_glib_none(raw.cast::<glib::gobject_ffi::GObject>()) };
        toplevel.and_then(|toplevel| toplevel.downcast::<Toplevel>().ok())
    }

    fn toplevel_changed(&self) {
        let view = self.as_wpe();
        let Some(toplevel) = self.toplevel() else {
            // SAFETY: a live view.
            unsafe { platform::wpe_view_unmap(view) };
            return;
        };
        let (mut width, mut height) = (0, 0);
        // SAFETY: a live toplevel; the out-pointers are valid.
        unsafe { platform::wpe_toplevel_get_size(toplevel.as_wpe(), &raw mut width, &raw mut height) };
        if width > 0 && height > 0 {
            // SAFETY: a live view.
            unsafe { platform::wpe_view_resized(view, width, height) };
        }
        // SAFETY: a live view.
        unsafe { platform::wpe_view_map(view) };
    }

    /// The consumer presented the frame in `buffer`.
    pub(crate) fn presented(&self, buffer: BufferId) {
        let acks = self.imp().flow.borrow_mut().presented(buffer);
        self.schedule(acks);
    }

    /// Runs `acks` from a source of their own on the engine's context (the thread-default one; glib's
    /// `idle_add_local` would attach to the global default context, which nothing runs here), never in place:
    /// acknowledging while `WebKit` hands over a frame re-enters it before it recorded the frame as pending (ngterm's
    /// Task 2, quirk 6).
    fn schedule(&self, acks: Vec<Ack>) {
        if acks.is_empty() {
            return;
        }
        let view = self.downgrade();
        glib::MainContext::ref_thread_default().spawn_local_with_priority(glib::Priority::DEFAULT, async move {
            if let Some(view) = view.upgrade() {
                view.acknowledge(&acks);
            }
        });
    }

    /// Tells `WebKit` about `acks`, dropping those whose buffer left the set since.
    fn acknowledge(&self, acks: &[Ack]) {
        for ack in acks {
            let (Ack::Rendered(id) | Ack::Released(id)) = *ack;
            let buffer = self.imp().held.borrow().get(&id).map(|held| held.buffer.clone());
            let Some(buffer) = buffer else { continue };
            let raw: *mut platform::WPEBuffer = buffer.to_glib_none().0;
            match ack {
                // SAFETY: a live view and a buffer of its current set, referenced by `buffer`.
                Ack::Rendered(_) => unsafe { platform::wpe_view_buffer_rendered(self.as_wpe(), raw) },
                // SAFETY: as above.
                Ack::Released(_) => unsafe { platform::wpe_view_buffer_released(self.as_wpe(), raw) },
            }
        }
    }

    /// `WebKit` replaced the buffer set.
    fn buffers_changed(&self, buffers: &[WpeBuffer]) {
        let ids: Vec<BufferId> = buffers.iter().map(buffer_id).collect();
        let removed = self.imp().flow.borrow_mut().buffers_changed(ids.clone());
        {
            let mut held = self.imp().held.borrow_mut();
            held.retain(|id, _| ids.contains(id));
            for (id, buffer) in ids.iter().zip(buffers) {
                held.entry(*id).or_insert_with(|| Held {
                    buffer: buffer.clone(),
                    dmabuf: None,
                });
            }
        }
        if removed.is_empty() {
            return;
        }
        if let Some(link) = self.imp().link.borrow().as_ref() {
            send(
                &link.events,
                Event::BuffersRemoved {
                    view: link.id,
                    buffers: removed,
                },
            );
        }
    }

    /// `WebKit` handed over a frame in `buffer` with `damage`.
    fn render(&self, buffer: &WpeBuffer, damage: &[Rect]) -> Result<(), glib::Error> {
        let id = buffer_id(buffer);
        let Some((view, events)) = self
            .imp()
            .link
            .borrow()
            .as_ref()
            .map(|link| (link.id, link.events.clone()))
        else {
            // not attached (yet, or any more): nobody shows it, so it goes straight back
            let acks = vec![Ack::Rendered(id), Ack::Released(id)];
            self.remember(id, buffer);
            self.schedule(acks);
            return Ok(());
        };
        self.remember(id, buffer);
        let (width, height) = buffer_size(buffer);
        let content = self.content(id, buffer, width, height, damage)?;
        let kind = match content {
            FrameContent::DmaBuf(_) => Kind::DmaBuf,
            FrameContent::Shm(_) => Kind::Shm,
        };
        let acks = self.imp().flow.borrow_mut().rendered(id, kind);
        send(
            &events,
            Event::Frame(Frame {
                view,
                buffer: id,
                width,
                height,
                damage: damage.to_vec(),
                content,
            }),
        );
        self.schedule(acks);
        Ok(())
    }

    fn remember(&self, id: BufferId, buffer: &WpeBuffer) {
        self.imp().held.borrow_mut().entry(id).or_insert_with(|| Held {
            buffer: buffer.clone(),
            dmabuf: None,
        });
    }

    fn content(
        &self,
        id: BufferId,
        buffer: &WpeBuffer,
        width: u32,
        height: u32,
        damage: &[Rect],
    ) -> Result<FrameContent, glib::Error> {
        if let Some(shm) = buffer.downcast_ref::<WpeBufferShm>() {
            return Ok(FrameContent::Shm(shm_regions(shm, width, height, damage)));
        }
        let Some(dmabuf) = buffer.downcast_ref::<WpeBufferDmaBuf>() else {
            return Err(view_error("a buffer of an unknown kind"));
        };
        let cached = self.imp().held.borrow().get(&id).and_then(|held| held.dmabuf.clone());
        if let Some(cached) = cached {
            return Ok(FrameContent::DmaBuf(cached));
        }
        let planes = dmabuf_of(dmabuf, width, height).map_err(|err| view_error(&err.to_string()))?;
        if self
            .display()
            .is_some_and(|display| !display.offers(planes.fourcc, planes.modifier))
        {
            // the driver could not allocate an offered format and WebKit fell back to its own choice (NVIDIA: no
            // linear render target, so its block-linear modifier); the consumer checks each buffer before importing
            log::warn!(
                "WebKit handed over a buffer in a format the consumer does not import (fourcc {:#010x}, modifier {:#x})",
                planes.fourcc,
                planes.modifier
            );
        }
        let planes = Arc::new(planes);
        if let Some(held) = self.imp().held.borrow_mut().get_mut(&id) {
            held.dmabuf = Some(planes.clone());
        }
        Ok(FrameContent::DmaBuf(planes))
    }

    fn set_cursor(&self, name: String) {
        self.report(|view| Event::Cursor { view, name });
    }

    /// Sends the event `event` makes of the page's id, while the view belongs to a page.
    pub(crate) fn report(&self, event: impl FnOnce(ViewId) -> Event) {
        if let Some(link) = self.imp().link.borrow().as_ref() {
            send(&link.events, event(link.id));
        }
    }

    /// The view's input method, once `WebKit` asked for one.
    pub(crate) fn input_method(&self) -> Option<crate::engine::ime::InputMethod> {
        self.imp().ime.borrow().as_ref().and_then(glib::WeakRef::upgrade)
    }
}

fn view_error(message: &str) -> glib::Error {
    // SAFETY: a plain quark lookup.
    let domain = unsafe { platform::wpe_view_error_quark() };
    // SAFETY: `domain` is WPE's view error quark, a registered domain.
    let domain = unsafe { glib::translate::from_glib(domain) };
    glib::Error::with_domain(domain, platform::WPE_VIEW_ERROR_RENDER_FAILED, message)
}

/// The id a buffer object carries for its life, given on first sight.
fn buffer_id(buffer: &WpeBuffer) -> BufferId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let quark = glib::Quark::from_str("teams-wpe-buffer-id");
    // SAFETY: this quark only ever holds a `BufferId`, set below.
    if let Some(id) = unsafe { buffer.qdata::<BufferId>(quark) } {
        // SAFETY: the qdata points to a `BufferId` the buffer owns for its life.
        return unsafe { *id.as_ptr() };
    }
    let id = BufferId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    // SAFETY: the quark's only type is `BufferId`.
    unsafe { buffer.set_qdata(quark, id) };
    id
}

fn buffer_size(buffer: &WpeBuffer) -> (u32, u32) {
    let raw: *mut platform::WPEBuffer = buffer.to_glib_none().0;
    // SAFETY: a live buffer.
    let width = unsafe { platform::wpe_buffer_get_width(raw) };
    // SAFETY: a live buffer.
    let height = unsafe { platform::wpe_buffer_get_height(raw) };
    (u32::try_from(width).unwrap_or(0), u32::try_from(height).unwrap_or(0))
}

/// The buffer's planes with fds of the host's own, so the consumer may keep them past the engine's buffer.
fn dmabuf_of(buffer: &WpeBufferDmaBuf, width: u32, height: u32) -> std::io::Result<DmaBuf> {
    let raw: *mut platform::WPEBufferDMABuf = buffer.to_glib_none().0;
    // SAFETY: a live dmabuf buffer.
    let count = unsafe { platform::wpe_buffer_dma_buf_get_n_planes(raw) };
    let mut planes = Vec::new();
    for plane in 0..count {
        // SAFETY: a live dmabuf buffer and a plane index below its count.
        let fd = unsafe { platform::wpe_buffer_dma_buf_get_fd(raw, plane) };
        // SAFETY: the buffer owns `fd` and keeps it open while it lives, which spans this call.
        let fd = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        // SAFETY: as above.
        let offset = unsafe { platform::wpe_buffer_dma_buf_get_offset(raw, plane) };
        // SAFETY: as above.
        let stride = unsafe { platform::wpe_buffer_dma_buf_get_stride(raw, plane) };
        planes.push(Plane { fd, offset, stride });
    }
    Ok(DmaBuf {
        width,
        height,
        // SAFETY: a live dmabuf buffer.
        fourcc: unsafe { platform::wpe_buffer_dma_buf_get_format(raw) },
        // SAFETY: a live dmabuf buffer.
        modifier: unsafe { platform::wpe_buffer_dma_buf_get_modifier(raw) },
        planes,
    })
}

/// The damaged pixels of an SHM buffer, copied out.
fn shm_regions(buffer: &WpeBufferShm, width: u32, height: u32, damage: &[Rect]) -> Vec<crate::frames::ShmRegion> {
    let raw: *mut platform::WPEBufferSHM = buffer.to_glib_none().0;
    // SAFETY: a live SHM buffer.
    let stride = unsafe { platform::wpe_buffer_shm_get_stride(raw) };
    // SAFETY: a live SHM buffer; the bytes are owned by it (transfer none), and `from_glib_none` takes a reference.
    let raw = unsafe { platform::wpe_buffer_shm_get_data(raw) };
    // SAFETY: `raw` comes straight from that call, owned as its documentation says.
    let bytes: Option<glib::Bytes> = unsafe { from_glib_none(raw) };
    let Some(bytes) = bytes else { return Vec::new() };
    copy_regions(&bytes, stride as usize, width, height, damage)
}

/// The engine's view behind a vfunc's instance pointer.
///
/// # Safety
///
/// `view` points to a live `WPEView`.
unsafe fn view_of(view: *mut platform::WPEView) -> Option<View> {
    // SAFETY: the caller guarantees a live GObject.
    let object = unsafe { borrow_instance(view) };
    object.downcast_ref::<View>().cloned()
}

unsafe extern "C" fn view_render_buffer(
    view: *mut platform::WPEView,
    buffer: *mut platform::WPEBuffer,
    damage: *const platform::WPERectangle,
    n_damage: u32,
    error: *mut *mut glib::ffi::GError,
) -> glib::ffi::gboolean {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(view) = (unsafe { view_of(view) }) else {
        return glib::ffi::GFALSE;
    };
    // SAFETY: WPE passes a live buffer for the call; `from_glib_none` takes a reference.
    let buffer: WpeBuffer = unsafe { from_glib_none(buffer) };
    let damage = if damage.is_null() || n_damage == 0 {
        &[][..]
    } else {
        // SAFETY: WPE passes `n_damage` rectangles at `damage`.
        unsafe { std::slice::from_raw_parts(damage, n_damage as usize) }
    };
    let damage: Vec<Rect> = damage.iter().filter_map(rect_of).collect();
    match view.render(&buffer, &damage) {
        Ok(()) => glib::ffi::GTRUE,
        Err(err) => {
            log::warn!("a WPE frame could not be handed over: {err}");
            if !error.is_null() {
                // SAFETY: `error` is the caller's out-pointer, which takes over the error.
                unsafe { *error = err.into_glib_ptr() };
            }
            glib::ffi::GFALSE
        }
    }
}

fn rect_of(rect: &platform::WPERectangle) -> Option<Rect> {
    let x = u32::try_from(rect.x.max(0)).ok()?;
    let y = u32::try_from(rect.y.max(0)).ok()?;
    let width = u32::try_from(rect.width).ok().filter(|width| *width > 0)?;
    let height = u32::try_from(rect.height).ok().filter(|height| *height > 0)?;
    Some(Rect { x, y, width, height })
}

unsafe extern "C" fn view_buffers_changed(
    view: *mut platform::WPEView,
    buffers: *mut *mut platform::WPEBuffer,
    n_buffers: u32,
) {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(view) = (unsafe { view_of(view) }) else { return };
    let buffers = if buffers.is_null() || n_buffers == 0 {
        &[][..]
    } else {
        // SAFETY: WPE passes `n_buffers` live buffers at `buffers`.
        unsafe { std::slice::from_raw_parts(buffers, n_buffers as usize) }
    };
    // SAFETY: each is a live buffer; `from_glib_none` takes a reference.
    let buffers: Vec<WpeBuffer> = buffers
        .iter()
        .map(|&buffer| unsafe { from_glib_none(buffer) })
        .collect();
    view.buffers_changed(&buffers);
}

unsafe extern "C" fn view_set_cursor(view: *mut platform::WPEView, name: *const c_char) {
    // SAFETY: WPE calls a class's vfuncs with instances of that class.
    let Some(view) = (unsafe { view_of(view) }) else { return };
    // SAFETY: WPE passes a NUL-terminated name (or null).
    let name: Option<String> = unsafe { from_glib_none(name) };
    if let Some(name) = name {
        view.set_cursor(name);
    }
}
