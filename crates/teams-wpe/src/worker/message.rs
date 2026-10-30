//! What the app and the worker say to each other, and how each message becomes a [`Packet`]: the body as JSON, with
//! the bytes it describes in the blob (an SHM frame's pixels, a scheme answer's data) and a dmabuf's planes as the
//! packet's descriptors.
//!
//! The app does not trust the worker (a page's content runs in it): what it reads back is checked before anything of
//! the app sees it, and a message that breaks a bound is a broken transport. A frame is at most [`MAX_FRAME_SIDE`]
//! pixels a side and [`MAX_FRAME_PIXELS`] in all; an SHM frame's regions are non-empty, lie inside it and add up to its
//! pixels exactly; a dmabuf frame has the frame's size, one to `MAX_BODY_FDS` planes, each a dmabuf whose object holds
//! the plane's offset (and the first plane's rows); damage is clipped to the frame. Page coordinates a popup or the
//! input method is placed by are clamped to [`MAX_COORDINATE`], sizes to non-negative ones. Strings and lists are
//! bounded by the body's size (`wire::Limits::FROM_WORKER`), enums by serde, which refuses an unknown variant; ids
//! naming no page or buffer of the app are dropped or ignored where they arrive (crate `worker::client`, the consumer).

use std::os::fd::OwnedFd;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::wire::{MAX_BODY_FDS, Packet, WireError};
use crate::config::HostConfig;
use crate::event::{Area, Event, SchemeResponse, SchemeToken};
use crate::frames::{BufferId, DmaBuf, Frame, FrameContent, Plane, Rect, ShmRegion, clip};
use crate::host::{Command, Evaluation, StartError};
use crate::spec::ViewId;

/// The protocol's version: the app and its worker are one executable, so a mismatch means the file on disk was
/// replaced by another build while the app ran.
pub(crate) const PROTOCOL: u32 = 1;

/// The longest side of a frame the app takes from a worker, in pixels: the largest 2D image today's desktop GPUs make
/// (Vulkan's `maxImageDimension2D`).
pub(crate) const MAX_FRAME_SIDE: u32 = 16_384;
/// The most pixels a frame the app takes from a worker has: 8192x8192, past two 4K displays side by side at scale 2
/// (15360x4320).
pub(crate) const MAX_FRAME_PIXELS: u64 = 1 << 26;
/// The most bytes an SHM frame's pixels take, 4 a pixel.
pub(crate) const MAX_FRAME_BYTES: u64 = MAX_FRAME_PIXELS * 4;
/// How far from a page's origin a coordinate the worker reports may lie, in logical pixels, and the largest size.
pub(crate) const MAX_COORDINATE: i32 = 1 << 24;
/// `DMA_BUF_MAGIC` of `linux/magic.h`: the file system every dmabuf's descriptor names.
const DMA_BUF_MAGIC: u64 = 0x444d_4142;

/// From the app to the worker.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum ToWorker {
    /// The engine's configuration, once, after the worker's [`FromWorker::Hello`].
    Start(HostConfig),
    /// Any command but an evaluation and a scheme's bytes (`Command::Quit` included).
    Command(Command),
    /// [`crate::Host::evaluate`], answered by [`FromWorker::Evaluation`] with its `id`.
    Evaluate {
        id: u64,
        view: ViewId,
        script: String,
        world: Option<String>,
    },
    /// [`SchemeResponse::Bytes`], its data in the blob.
    SchemeBytes { token: SchemeToken, mime: String },
}

/// From the worker to the app.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum FromWorker {
    /// The worker is up: its protocol, its build and its pid (in the app's pid namespace).
    Hello {
        protocol: u32,
        version: String,
        pid: u32,
    },
    /// How the engine's start went.
    Started(Result<(), StartFailure>),
    /// Any event but a frame.
    Event(Event),
    /// A dmabuf frame; the planes are the packet's descriptors, in order.
    DmaBufFrame {
        frame: FrameHeader,
        dmabuf: DmaBufHeader,
    },
    /// An SHM frame; the regions' pixels follow each other in the blob.
    ShmFrame {
        frame: FrameHeader,
        regions: Vec<Rect>,
    },
    Evaluation {
        id: u64,
        result: Evaluation,
    },
    /// The engine closed everything after `Command::Quit`; the worker ends.
    Stopped,
}

/// Why the worker's engine did not start, as [`StartError`] tells it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum StartFailure {
    Display(String),
    Ended,
    Other(String),
}

impl From<&StartError> for StartFailure {
    fn from(error: &StartError) -> Self {
        match error {
            StartError::Display(message) => Self::Display(message.clone()),
            StartError::Ended => Self::Ended,
            other => Self::Other(other.to_string()),
        }
    }
}

impl From<StartFailure> for StartError {
    fn from(failure: StartFailure) -> Self {
        match failure {
            StartFailure::Display(message) => Self::Display(message),
            StartFailure::Ended => Self::Ended,
            StartFailure::Other(message) => Self::Worker(message),
        }
    }
}

/// A frame without its content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FrameHeader {
    view: ViewId,
    buffer: BufferId,
    width: u32,
    height: u32,
    damage: Vec<Rect>,
}

/// A dmabuf without its descriptors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct DmaBufHeader {
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    /// Each plane's offset and stride.
    planes: Vec<(u32, u32)>,
}

fn json(body: &impl Serialize) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(body).map_err(|err| WireError::Broken(format!("not serializable: {err}")))
}

/// A body read back from its packet's JSON.
pub(crate) fn body<T: for<'de> Deserialize<'de>>(packet: &Packet) -> Result<T, WireError> {
    serde_json::from_slice(&packet.body).map_err(|err| WireError::Broken(format!("an unreadable body: {err}")))
}

impl ToWorker {
    /// `command` as the message that carries it, and the bytes that go with it.
    pub(crate) fn command(command: Command) -> (Self, Option<Vec<u8>>) {
        match command {
            Command::Scheme(token, SchemeResponse::Bytes { data, mime }) => {
                (Self::SchemeBytes { token, mime }, Some(data))
            }
            command => (Self::Command(command), None),
        }
    }

    pub(crate) fn packet(&self, blob: Option<Vec<u8>>) -> Result<Packet, WireError> {
        Ok(Packet {
            body: json(self)?,
            blob,
            fds: Vec::new(),
        })
    }
}

impl FromWorker {
    pub(crate) fn packet(&self) -> Result<Packet, WireError> {
        Ok(Packet {
            body: json(self)?,
            blob: None,
            fds: Vec::new(),
        })
    }

    /// An event of the engine as its packet: a frame's content travels beside its body.
    pub(crate) fn event_packet(event: Event) -> Result<Packet, WireError> {
        let Event::Frame(frame) = event else {
            return Self::Event(event).packet();
        };
        let header = FrameHeader {
            view: frame.view,
            buffer: frame.buffer,
            width: frame.width,
            height: frame.height,
            damage: frame.damage,
        };
        match frame.content {
            FrameContent::DmaBuf(dmabuf) => {
                let fds = dmabuf
                    .planes
                    .iter()
                    .map(|plane| plane.fd.try_clone())
                    .collect::<std::io::Result<Vec<OwnedFd>>>()?;
                let body = Self::DmaBufFrame {
                    frame: header,
                    dmabuf: DmaBufHeader {
                        width: dmabuf.width,
                        height: dmabuf.height,
                        fourcc: dmabuf.fourcc,
                        modifier: dmabuf.modifier,
                        planes: dmabuf.planes.iter().map(|plane| (plane.offset, plane.stride)).collect(),
                    },
                };
                Ok(Packet {
                    body: json(&body)?,
                    blob: None,
                    fds,
                })
            }
            FrameContent::Shm(regions) => {
                let mut pixels = Vec::with_capacity(regions.iter().map(|region| region.pixels.len()).sum());
                for region in &regions {
                    pixels.extend_from_slice(&region.pixels);
                }
                let body = Self::ShmFrame {
                    frame: header,
                    regions: regions.iter().map(|region| region.rect).collect(),
                };
                Ok(Packet {
                    body: json(&body)?,
                    blob: Some(pixels),
                    fds: Vec::new(),
                })
            }
        }
    }
}

fn broken(what: impl Into<String>) -> WireError {
    WireError::Broken(what.into())
}

/// The frame a [`FromWorker::DmaBufFrame`] packet carries, checked (module docs); `known` is the buffer's dmabuf as an
/// earlier frame brought it (one `Arc` per buffer, as the engine hands them out), and the planes this packet brought
/// again are closed.
pub(crate) fn dmabuf_frame(
    frame: FrameHeader,
    dmabuf: DmaBufHeader,
    fds: Vec<OwnedFd>,
    known: Option<Arc<DmaBuf>>,
) -> Result<Frame, WireError> {
    let frame = frame.checked()?;
    if (dmabuf.width, dmabuf.height) != (frame.width, frame.height) {
        return Err(broken(format!(
            "a {}x{} dmabuf in a {}x{} frame",
            dmabuf.width, dmabuf.height, frame.width, frame.height
        )));
    }
    if dmabuf.planes.is_empty() || dmabuf.planes.len() > MAX_BODY_FDS {
        return Err(broken(format!("a dmabuf of {} planes", dmabuf.planes.len())));
    }
    if fds.len() != dmabuf.planes.len() {
        return Err(broken(format!(
            "{} plane descriptors for {} planes",
            fds.len(),
            dmabuf.planes.len()
        )));
    }
    for (index, (fd, &(offset, stride))) in fds.iter().zip(&dmabuf.planes).enumerate() {
        check_plane(fd, index, offset, stride, frame.height)?;
    }
    let dmabuf = known.unwrap_or_else(|| {
        Arc::new(DmaBuf {
            width: dmabuf.width,
            height: dmabuf.height,
            fourcc: dmabuf.fourcc,
            modifier: dmabuf.modifier,
            planes: fds
                .into_iter()
                .zip(dmabuf.planes)
                .map(|(fd, (offset, stride))| Plane { fd, offset, stride })
                .collect(),
        })
    });
    Ok(Frame {
        view: frame.view,
        buffer: frame.buffer,
        width: frame.width,
        height: frame.height,
        damage: frame.damage,
        content: FrameContent::DmaBuf(dmabuf),
    })
}

/// Checks plane `index` of a dmabuf frame `height` rows high: its descriptor names a dmabuf, whose object holds the
/// plane's offset, and for the first (main) plane its rows too (`offset + stride * height`; the other planes' strides
/// mean what their modifier says).
fn check_plane(fd: &OwnedFd, index: usize, offset: u32, stride: u32, height: u32) -> Result<(), WireError> {
    let not_a_dmabuf = || broken(format!("plane {index} is not a dmabuf"));
    let fs = rustix::fs::fstatfs(fd).map_err(|_| not_a_dmabuf())?;
    // the word's type is the architecture's (`__fsword_t`)
    if u64::try_from(fs.f_type).ok() != Some(DMA_BUF_MAGIC) {
        return Err(not_a_dmabuf());
    }
    let size = u64::try_from(rustix::fs::fstat(fd)?.st_size).unwrap_or(0);
    let end = if index == 0 {
        if stride == 0 {
            return Err(broken("a dmabuf's main plane with no stride"));
        }
        u64::from(offset) + u64::from(stride) * u64::from(height)
    } else {
        u64::from(offset)
    };
    if end > size {
        return Err(broken(format!(
            "plane {index} reaches byte {end} of a {size}-byte dmabuf"
        )));
    }
    Ok(())
}

/// The frame a [`FromWorker::ShmFrame`] packet carries, checked (module docs), its regions cut from the blob (one
/// region takes the blob as it is).
pub(crate) fn shm_frame(frame: FrameHeader, regions: &[Rect], blob: Option<Vec<u8>>) -> Result<Frame, WireError> {
    let frame = frame.checked()?;
    let mut pixels = blob.unwrap_or_default();
    let mut lens = Vec::with_capacity(regions.len());
    let mut total = 0u64;
    for rect in regions {
        if !inside(*rect, frame.width, frame.height) {
            return Err(broken(format!(
                "an SHM region {rect:?} outside its {}x{} frame",
                frame.width, frame.height
            )));
        }
        // at most the frame's bytes, which are bounded
        let len = u64::from(rect.width) * u64::from(rect.height) * 4;
        total = total.saturating_add(len);
        lens.push(usize::try_from(len).map_err(|_| broken("a region too large"))?);
    }
    if total != pixels.len() as u64 {
        return Err(broken(format!(
            "an SHM frame's {} bytes of pixels for regions of {total}",
            pixels.len()
        )));
    }
    let cut = if let [rect] = regions {
        vec![ShmRegion {
            rect: *rect,
            pixels: std::mem::take(&mut pixels),
        }]
    } else {
        let mut at = 0usize;
        regions
            .iter()
            .zip(lens)
            .map(|(rect, len)| {
                let region = ShmRegion {
                    rect: *rect,
                    pixels: pixels[at..at + len].to_vec(),
                };
                at += len;
                region
            })
            .collect()
    };
    Ok(Frame {
        view: frame.view,
        buffer: frame.buffer,
        width: frame.width,
        height: frame.height,
        damage: frame.damage,
        content: FrameContent::Shm(cut),
    })
}

/// Whether `rect` is non-empty and lies inside a `width` x `height` frame.
fn inside(rect: Rect, width: u32, height: u32) -> bool {
    let right = rect.x.checked_add(rect.width);
    let bottom = rect.y.checked_add(rect.height);
    rect.width > 0
        && rect.height > 0
        && right.is_some_and(|right| right <= width)
        && bottom.is_some_and(|bottom| bottom <= height)
}

impl FrameHeader {
    pub(crate) fn view(&self) -> ViewId {
        self.view
    }

    pub(crate) fn buffer(&self) -> BufferId {
        self.buffer
    }

    /// The header once its size keeps to a worker frame's bounds (module docs), its damage clipped to it.
    fn checked(mut self) -> Result<Self, WireError> {
        let (width, height) = (self.width, self.height);
        // u32 x u32 fits a u64
        let pixels = u64::from(width) * u64::from(height);
        if width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE || pixels > MAX_FRAME_PIXELS {
            return Err(broken(format!(
                "a {width}x{height} frame, larger than any the app takes"
            )));
        }
        self.damage = self
            .damage
            .into_iter()
            .filter_map(|rect| clip(rect, width, height))
            .collect();
        Ok(self)
    }
}

/// `event` from the worker with the page coordinates it carries kept within [`MAX_COORDINATE`] and its sizes
/// non-negative (module docs).
pub(crate) fn checked_event(event: Event) -> Event {
    match event {
        Event::ImeCursorArea { view, area } => Event::ImeCursorArea {
            view,
            area: bounded_area(area),
        },
        Event::OptionMenu(mut menu) => {
            menu.area = bounded_area(menu.area);
            Event::OptionMenu(menu)
        }
        Event::ContextMenu(mut menu) => {
            menu.x = bounded(menu.x);
            menu.y = bounded(menu.y);
            Event::ContextMenu(menu)
        }
        other => other,
    }
}

fn bounded(value: i32) -> i32 {
    value.clamp(-MAX_COORDINATE, MAX_COORDINATE)
}

fn bounded_area(area: Area) -> Area {
    Area {
        x: bounded(area.x),
        y: bounded(area.y),
        width: area.width.clamp(0, MAX_COORDINATE),
        height: area.height.clamp(0, MAX_COORDINATE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(width: u32, height: u32) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    #[test]
    fn an_shm_frame_comes_back_region_by_region() {
        let event = Event::Frame(Frame {
            view: ViewId(3),
            buffer: BufferId(9),
            width: 4,
            height: 4,
            damage: vec![rect(4, 4)],
            content: FrameContent::Shm(vec![
                ShmRegion {
                    rect: rect(1, 1),
                    pixels: vec![1, 2, 3, 4],
                },
                ShmRegion {
                    rect: rect(2, 1),
                    pixels: vec![5, 6, 7, 8, 9, 10, 11, 12],
                },
            ]),
        });
        let packet = FromWorker::event_packet(event).unwrap();
        let FromWorker::ShmFrame { frame, regions } = body(&packet).unwrap() else {
            panic!("not an SHM frame");
        };
        let frame = shm_frame(frame, &regions, packet.blob).unwrap();
        let FrameContent::Shm(regions) = frame.content else {
            panic!("not SHM content");
        };
        assert_eq!(regions[0].pixels, [1, 2, 3, 4]);
        assert_eq!(regions[1].pixels, [5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!((frame.view, frame.buffer), (ViewId(3), BufferId(9)));
        // pixels that do not add up to the regions are a broken packet
        assert!(
            shm_frame(
                FrameHeader {
                    view: ViewId(3),
                    buffer: BufferId(9),
                    width: 4,
                    height: 4,
                    damage: Vec::new(),
                },
                &[rect(2, 2)],
                Some(vec![0; 15]),
            )
            .is_err()
        );
    }

    fn header(width: u32, height: u32, damage: Vec<Rect>) -> FrameHeader {
        FrameHeader {
            view: ViewId(3),
            buffer: BufferId(9),
            width,
            height,
            damage,
        }
    }

    fn at(x: u32, y: u32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }

    fn refused(result: Result<Frame, WireError>) -> String {
        match result {
            Err(WireError::Broken(message)) => message,
            Err(other) => panic!("refused as {other}, not as a broken packet"),
            Ok(frame) => panic!("a {}x{} frame was taken", frame.width, frame.height),
        }
    }

    #[test]
    fn an_shm_frame_beyond_the_app_s_bounds_is_refused_before_anything_is_allocated() {
        // the review's packet: a 2^31 x 2^31 frame with one 1x1 region and its four bytes
        let huge = 1 << 31;
        let message = refused(shm_frame(
            header(huge, huge, Vec::new()),
            &[rect(1, 1)],
            Some(vec![0; 4]),
        ));
        assert!(message.contains("larger than any"), "{message}");
        // a side past the largest image, and an area past the largest frame
        refused(shm_frame(
            header(MAX_FRAME_SIDE + 1, 1, Vec::new()),
            &[rect(1, 1)],
            Some(vec![0; 4]),
        ));
        refused(shm_frame(
            header(MAX_FRAME_SIDE, 8193, Vec::new()),
            &[rect(1, 1)],
            Some(vec![0; 4]),
        ));
        // the largest frame there is goes through
        let frame = shm_frame(
            header(MAX_FRAME_SIDE, 4096, Vec::new()),
            &[rect(1, 1)],
            Some(vec![0; 4]),
        );
        assert!(frame.is_ok());
    }

    #[test]
    fn an_shm_region_outside_its_frame_or_not_matching_its_pixels_is_refused() {
        let frame = || header(4, 4, Vec::new());
        refused(shm_frame(frame(), &[at(3, 0, 2, 1)], Some(vec![0; 8])));
        refused(shm_frame(frame(), &[at(0, 3, 1, 2)], Some(vec![0; 8])));
        // a rectangle whose right edge wraps around
        refused(shm_frame(frame(), &[at(u32::MAX, 0, 2, 1)], Some(vec![0; 8])));
        // an empty one, in a frame whose pixels would add up without it
        refused(shm_frame(frame(), &[at(0, 0, 0, 1), rect(1, 1)], Some(vec![0; 4])));
        refused(shm_frame(frame(), &[at(0, 0, 1, 0)], None));
        // pixels short of the regions, and past them
        refused(shm_frame(frame(), &[rect(2, 2)], Some(vec![0; 15])));
        refused(shm_frame(frame(), &[rect(1, 1)], Some(vec![0; 5])));
        refused(shm_frame(frame(), &[rect(1, 1)], None));
        // a frame without regions carries no pixels
        assert!(shm_frame(frame(), &[], None).is_ok());
        refused(shm_frame(frame(), &[], Some(vec![0; 4])));
    }

    #[test]
    fn a_frame_s_damage_is_clipped_to_it() {
        let damage = vec![at(2, 2, 10, 10), at(9, 9, 1, 1), at(u32::MAX, 0, 5, 5)];
        let frame = shm_frame(header(4, 4, damage), &[rect(1, 1)], Some(vec![0; 4])).unwrap();
        assert_eq!(frame.damage, [at(2, 2, 2, 2)]);
    }

    fn dmabuf(width: u32, height: u32, planes: usize) -> DmaBufHeader {
        DmaBufHeader {
            width,
            height,
            fourcc: crate::fourcc(b"XR24"),
            modifier: 0,
            planes: vec![(0, 16); planes],
        }
    }

    fn files(count: usize) -> Vec<OwnedFd> {
        (0..count)
            .map(|_| OwnedFd::from(tempfile::tempfile().unwrap()))
            .collect()
    }

    #[test]
    fn a_dmabuf_frame_s_size_planes_and_descriptors_are_checked() {
        let huge = 1 << 31;
        refused(dmabuf_frame(
            header(huge, huge, Vec::new()),
            dmabuf(huge, huge, 1),
            files(1),
            None,
        ));
        let message = refused(dmabuf_frame(header(4, 4, Vec::new()), dmabuf(8, 4, 1), files(1), None));
        assert!(message.contains("8x4 dmabuf in a 4x4 frame"), "{message}");
        refused(dmabuf_frame(
            header(4, 4, Vec::new()),
            dmabuf(4, 4, 0),
            Vec::new(),
            None,
        ));
        refused(dmabuf_frame(header(4, 4, Vec::new()), dmabuf(4, 4, 5), files(5), None));
        refused(dmabuf_frame(header(4, 4, Vec::new()), dmabuf(4, 4, 2), files(1), None));
        // a descriptor that is no dmabuf (a plain file here)
        let message = refused(dmabuf_frame(header(4, 4, Vec::new()), dmabuf(4, 4, 1), files(1), None));
        assert!(message.contains("not a dmabuf"), "{message}");
    }

    #[test]
    fn page_coordinates_from_the_worker_are_clamped() {
        let far = Area {
            x: i32::MIN,
            y: i32::MAX,
            width: i32::MAX,
            height: -5,
        };
        let Event::ImeCursorArea { area, .. } = checked_event(Event::ImeCursorArea {
            view: ViewId(1),
            area: far,
        }) else {
            panic!("not the caret area");
        };
        assert_eq!(
            area,
            Area {
                x: -MAX_COORDINATE,
                y: MAX_COORDINATE,
                width: MAX_COORDINATE,
                height: 0
            }
        );
        // the popup's bottom edge, which its placement adds up, stays far inside an i32
        assert!(area.y.checked_add(area.height).is_some());
        let Event::OptionMenu(menu) = checked_event(Event::OptionMenu(crate::event::OptionMenu {
            token: crate::event::MenuToken(1),
            view: ViewId(1),
            area: far,
            items: Vec::new(),
        })) else {
            panic!("not the popup");
        };
        assert!(menu.area.y.checked_add(menu.area.height).is_some());
        let Event::ContextMenu(menu) = checked_event(Event::ContextMenu(crate::event::ContextMenu {
            view: ViewId(1),
            x: i32::MAX,
            y: i32::MIN,
            link: None,
            image: None,
            media: None,
            editable: false,
            selection: false,
        })) else {
            panic!("not the context menu");
        };
        assert_eq!((menu.x, menu.y), (MAX_COORDINATE, -MAX_COORDINATE));
    }

    #[test]
    fn a_scheme_answer_s_bytes_travel_in_the_blob() {
        let (message, blob) = ToWorker::command(Command::Scheme(
            SchemeToken(4),
            SchemeResponse::Bytes {
                data: vec![0; 70_000],
                mime: "image/png".to_owned(),
            },
        ));
        assert!(matches!(message, ToWorker::SchemeBytes { token: SchemeToken(4), ref mime } if mime == "image/png"));
        assert_eq!(blob.map(|blob| blob.len()), Some(70_000));
        let (message, blob) = ToWorker::command(Command::Clipboard("x".to_owned()));
        assert!(matches!(message, ToWorker::Command(Command::Clipboard(_))) && blob.is_none());
    }

    #[test]
    fn a_frame_never_goes_through_the_event_s_json() {
        let frame = Event::Frame(Frame {
            view: ViewId(1),
            buffer: BufferId(1),
            width: 1,
            height: 1,
            damage: Vec::new(),
            content: FrameContent::Shm(Vec::new()),
        });
        assert!(serde_json::to_vec(&FromWorker::Event(frame)).is_err());
    }
}
