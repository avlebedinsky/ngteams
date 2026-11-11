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

#[test]
fn a_render_node_that_does_not_exist_fails_the_connect() {
    let connected = Display::connect(Some(DmaBufTarget {
        render_node: "/nonexistent/teams-wpe-synthetic-render-node".into(),
        formats: vec![BufferFormat {
            fourcc: fourcc(b"XR24"),
            modifiers: vec![MODIFIER_LINEAR],
        }],
    }));
    let Err(failure) = connected else {
        panic!("a missing render node cannot be connected");
    };
    assert!(!failure.is_empty());
}

/// The acknowledgements `WebKit` is told of, in order: (rendered, the buffer's id).
type Acks = std::rc::Rc<std::cell::RefCell<Vec<(bool, BufferId)>>>;

fn listen(view: &View) -> Acks {
    let acks = Acks::default();
    for (signal, rendered) in [("buffer-rendered", true), ("buffer-released", false)] {
        let acks = acks.clone();
        view.connect_local(signal, false, move |values| {
            let buffer = values[1].get::<WpeBuffer>().unwrap();
            acks.borrow_mut().push((rendered, buffer_id(&buffer)));
            None
        });
    }
    acks
}

fn dmabuf(display: &Display, width: i32, height: i32, stride: u32) -> WpeBuffer {
    use std::os::fd::IntoRawFd as _;

    let mut fds = [tempfile::tempfile().unwrap().into_raw_fd()];
    let mut offsets = [0u32];
    let mut strides = [stride];
    // SAFETY: a live display and one plane's descriptor (which the buffer takes over), offset and stride.
    let raw = unsafe {
        platform::wpe_buffer_dma_buf_new(
            display.as_wpe(),
            width,
            height,
            fourcc(b"XR24"),
            1,
            fds.as_mut_ptr(),
            offsets.as_mut_ptr(),
            strides.as_mut_ptr(),
            MODIFIER_LINEAR,
        )
    };
    // SAFETY: a new buffer, which we take over.
    let buffer: WpeBufferDmaBuf = unsafe { glib::translate::from_glib_full(raw) };
    buffer.upcast()
}

fn render(view: &View, buffer: &WpeBuffer, damage: &[platform::WPERectangle]) -> bool {
    let mut error: *mut glib::ffi::GError = ptr::null_mut();
    // SAFETY: a live view and buffer, `damage.len()` rectangles and a valid error out-pointer.
    let rendered = unsafe {
        platform::wpe_view_render_buffer(
            view.as_wpe(),
            buffer.to_glib_none().0,
            damage.as_ptr(),
            u32::try_from(damage.len()).unwrap(),
            &raw mut error,
        )
    };
    assert!(error.is_null());
    rendered != glib::ffi::GFALSE
}

fn frame(events: &async_channel::Receiver<Event>) -> Frame {
    match events.try_recv() {
        Ok(Event::Frame(frame)) => frame,
        other => panic!("expected a frame, got {other:?}"),
    }
}

fn run_pending(context: &glib::MainContext) {
    while context.iteration(false) {}
}

/// A view on a display whose consumer imports another format than the buffers below have, its acknowledgements
/// recorded.
fn dmabuf_view() -> (Display, View, Acks) {
    let display: Display = glib::Object::new();
    display.imp().target.replace(Some(DmaBufTarget {
        render_node: "/dev/dri/synthetic-unopened".into(),
        formats: vec![BufferFormat {
            fourcc: fourcc(b"AR24"),
            modifiers: vec![MODIFIER_LINEAR],
        }],
    }));
    let view: View = glib::Object::builder().property("display", &display).build();
    let acks = listen(&view);
    (display, view, acks)
}

/// The acknowledgements told since the last call, once the engine's context ran them.
fn told(context: &glib::MainContext, acks: &Acks) -> Vec<(bool, BufferId)> {
    run_pending(context);
    acks.borrow_mut().drain(..).collect()
}

fn planes_of(frame: &Frame) -> Arc<DmaBuf> {
    let FrameContent::DmaBuf(planes) = &frame.content else {
        panic!("a dmabuf buffer makes a dmabuf frame");
    };
    planes.clone()
}

/// The first frame of `buffer`: its damage clamped to the buffer and empty rectangles dropped, its planes the
/// buffer's under descriptors of the host's own, handed over although the consumer offered another format (it checks
/// each buffer before importing).
fn first_frame(view: &View, events: &async_channel::Receiver<Event>, buffer: &WpeBuffer) -> Arc<DmaBuf> {
    use std::os::fd::AsRawFd as _;

    let damage = [
        platform::WPERectangle {
            x: -2,
            y: 1,
            width: 4,
            height: 3,
        },
        platform::WPERectangle {
            x: 0,
            y: 0,
            width: 0,
            height: 4,
        },
    ];
    assert!(render(view, buffer, &damage));
    let shown = frame(events);
    assert_eq!(
        (shown.view, shown.buffer, shown.width, shown.height),
        (ViewId::from_raw(5), buffer_id(buffer), 8, 4)
    );
    let clamped = Rect {
        x: 0,
        y: 1,
        width: 4,
        height: 3,
    };
    assert_eq!(shown.damage, [clamped]);
    let planes = planes_of(&shown);
    assert_eq!((planes.fourcc, planes.modifier), (fourcc(b"XR24"), MODIFIER_LINEAR));
    assert_eq!((planes.width, planes.height, planes.planes.len()), (8, 4, 1));
    assert_eq!((planes.planes[0].offset, planes.planes[0].stride), (0, 32));
    let raw = buffer.downcast_ref::<WpeBufferDmaBuf>().unwrap().to_glib_none().0;
    // SAFETY: a live dmabuf buffer with one plane.
    let engines = unsafe { platform::wpe_buffer_dma_buf_get_fd(raw, 0) };
    assert_ne!(planes.planes[0].fd.as_raw_fd(), engines);
    planes
}

#[test]
fn dmabuf_frames_carry_duplicated_planes_once_per_buffer_and_go_back_as_the_consumer_presents_them() {
    let context = glib::MainContext::new();
    context
        .with_thread_default(|| {
            let (display, view, acks) = dmabuf_view();
            let (first, second) = (dmabuf(&display, 8, 4, 32), dmabuf(&display, 8, 4, 32));
            let (first_id, second_id) = (buffer_id(&first), buffer_id(&second));
            // nobody shows a frame of a view no page owns: it goes straight back
            assert!(render(&view, &first, &[]));
            assert_eq!(told(&context, &acks), [(true, first_id), (false, first_id)]);
            let (sender, events) = async_channel::unbounded();
            view.attach(Link {
                id: ViewId::from_raw(5),
                events: sender,
            });
            let planes = first_frame(&view, &events, &first);
            // a newer frame supersedes the one never presented, which goes back whole
            assert!(render(&view, &second, &[]));
            assert_eq!(frame(&events).buffer, second_id);
            assert_eq!(told(&context, &acks), [(true, first_id), (false, first_id)]);
            view.presented(second_id);
            assert_eq!(told(&context, &acks), [(true, second_id)]);
            // a buffer seen before reuses its planes; presenting it releases the one shown before
            assert!(render(&view, &first, &[]));
            assert!(Arc::ptr_eq(&planes, &planes_of(&frame(&events))));
            view.presented(first_id);
            assert_eq!(told(&context, &acks), [(true, first_id), (false, second_id)]);
            view.detach();
            let mut expected = vec![first_id, second_id];
            expected.sort_unstable();
            assert!(
                matches!(events.try_recv(), Ok(Event::BuffersRemoved { view, buffers }) if view == ViewId::from_raw(5) && buffers == expected),
                "the buffers leave with the page"
            );
        })
        .unwrap();
}

#[test]
fn a_view_maps_at_its_toplevels_size_and_unmaps_once_it_has_none() {
    let display: Display = glib::Object::new();
    let view: View = glib::Object::builder().property("display", &display).build();
    let toplevel: Toplevel = glib::Object::builder()
        .property("display", &display)
        .property("max-views", 1u32)
        .build();
    let raw_view = view.as_wpe();
    // SAFETY: a live view and toplevel.
    unsafe { platform::wpe_view_set_toplevel(raw_view, toplevel.as_wpe()) };
    // SAFETY: a live view.
    assert_ne!(unsafe { platform::wpe_view_get_mapped(raw_view) }, glib::ffi::GFALSE);
    assert!(view.toplevel().is_some());
    toplevel.set_geometry(40, 30, 2.0);
    // SAFETY: a live view.
    let width = unsafe { platform::wpe_view_get_width(raw_view) };
    // SAFETY: a live view.
    let height = unsafe { platform::wpe_view_get_height(raw_view) };
    assert_eq!((width, height), (40, 30));
    // SAFETY: a live view; no toplevel.
    unsafe { platform::wpe_view_set_toplevel(raw_view, ptr::null_mut()) };
    // SAFETY: a live view.
    assert_eq!(unsafe { platform::wpe_view_get_mapped(raw_view) }, glib::ffi::GFALSE);
    assert!(view.toplevel().is_none());
}
