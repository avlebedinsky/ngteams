//! Frames a page hands to the consumer, and the acknowledgement protocol that gives its buffers back.
//!
//! `WebKit` renders a frame into one of its buffers, hands it over ([`Frame`]) and renders the next one only after
//! the consumer says the frame was shown ([`crate::Host::frame_presented`]), which marks the buffer rendered. A
//! dmabuf the consumer shows stays in use until a newer one replaces it on screen, so a buffer is released one frame
//! after it was presented; an SHM frame is copied out when it arrives, so its buffer goes back at once. `WebKit`
//! replaces its buffer set under the host (2 buffers, recreated on load, grown to 3): an acknowledgement for a buffer
//! that left the set is dropped, and the consumer forgets an imported buffer only when it leaves the set
//! ([`crate::Event::BuffersRemoved`]). [`FrameFlow`] is that protocol without the engine.

use std::os::fd::OwnedFd;
use std::sync::Arc;

use crate::spec::ViewId;
use serde::{Deserialize, Serialize};

/// A buffer of a page's current set; ids are never reused, so an id names one buffer for good.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BufferId(pub(crate) u64);

crate::raw_ids!(BufferId);

/// A rectangle in buffer pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One memory plane of a dmabuf.
#[derive(Debug)]
pub struct Plane {
    /// The host's own duplicate of the buffer's fd, valid while this value lives.
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// A GPU buffer to import (`WPEBufferDMABuf`): every memory plane, since a compressed buffer keeps several in one
/// object (Intel CCS: main, aux and clear color, a non-disjoint image).
#[derive(Debug)]
pub struct DmaBuf {
    pub width: u32,
    pub height: u32,
    /// The DRM fourcc ([`crate::fourcc`]).
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<Plane>,
}

/// Damaged pixels of an SHM frame, copied out of the engine's buffer: premultiplied ARGB, 32 bits per pixel in native
/// byte order (`WPE_PIXEL_FORMAT_ARGB8888`, B G R A in memory on a little-endian machine), rows `rect.width * 4`
/// bytes apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShmRegion {
    pub rect: Rect,
    pub pixels: Vec<u8>,
}

/// What a frame carries.
#[derive(Debug, Clone)]
pub enum FrameContent {
    /// The whole buffer, shared by every frame drawn into it; the consumer imports it once per [`BufferId`].
    DmaBuf(Arc<DmaBuf>),
    /// The damaged regions only: the consumer keeps the rest from earlier frames.
    Shm(Vec<ShmRegion>),
}

/// A frame of page `view`.
#[derive(Debug, Clone)]
pub struct Frame {
    pub view: ViewId,
    pub buffer: BufferId,
    /// The buffer's size in pixels.
    pub width: u32,
    pub height: u32,
    /// What changed since the previous frame; the whole buffer when the engine names nothing.
    pub damage: Vec<Rect>,
    pub content: FrameContent,
}

/// What the engine tells `WebKit` about a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ack {
    /// The frame was shown: `WebKit` may render the next one.
    Rendered(BufferId),
    /// The consumer no longer reads the buffer: `WebKit` may render into it again.
    Released(BufferId),
}

/// What a frame's buffer is, which decides when it goes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Read by the consumer until a newer frame replaces it on screen.
    DmaBuf,
    /// Copied out on arrival, so given back at once.
    Shm,
}

/// One page's buffer set and the frame in flight. A page may change kinds while it runs (a fall-back to SHM), so the
/// kind is the buffer's, not the page's.
#[derive(Debug, Default)]
pub(crate) struct FrameFlow {
    set: Vec<BufferId>,
    /// Handed over, not presented yet.
    pending: Option<(BufferId, Kind)>,
    /// The dmabuf presented last, still read by the consumer.
    shown: Option<BufferId>,
}

impl FrameFlow {
    /// Whether `buffer` is in the current set.
    pub(crate) fn contains(&self, buffer: BufferId) -> bool {
        self.set.contains(&buffer)
    }

    /// `WebKit` replaced its buffer set with `set`; returns the buffers that left it, which nothing acknowledges any
    /// more.
    pub(crate) fn buffers_changed(&mut self, set: Vec<BufferId>) -> Vec<BufferId> {
        let removed: Vec<BufferId> = self.set.iter().copied().filter(|id| !set.contains(id)).collect();
        self.set = set;
        if self.pending.is_some_and(|(id, _)| !self.contains(id)) {
            self.pending = None;
        }
        if self.shown.is_some_and(|id| !self.contains(id)) {
            self.shown = None;
        }
        removed
    }

    /// `WebKit` handed over a frame in `buffer` of `kind`. A buffer not announced yet joins the set; a frame still
    /// pending is superseded and given back, since it will never be presented; an SHM buffer goes back at once.
    pub(crate) fn rendered(&mut self, buffer: BufferId, kind: Kind) -> Vec<Ack> {
        if !self.contains(buffer) {
            self.set.push(buffer);
        }
        let mut acks = Vec::new();
        if let Some((superseded, was)) = self.pending.replace((buffer, kind)).filter(|&(id, _)| id != buffer) {
            acks.push(Ack::Rendered(superseded));
            if was == Kind::DmaBuf {
                acks.push(Ack::Released(superseded));
            }
        }
        if kind == Kind::Shm {
            acks.push(Ack::Released(buffer));
        }
        acks
    }

    /// The consumer presented the frame in `buffer`: it counts as rendered, and the dmabuf shown before it is
    /// released. A frame that is not the pending one (superseded, or from a buffer that left the set) is ignored.
    pub(crate) fn presented(&mut self, buffer: BufferId) -> Vec<Ack> {
        let Some((pending, kind)) = self.pending.filter(|&(id, _)| id == buffer) else {
            return Vec::new();
        };
        self.pending = None;
        let mut acks = vec![Ack::Rendered(pending)];
        let shown = match kind {
            Kind::DmaBuf => Some(pending),
            Kind::Shm => None,
        };
        if let Some(previous) = std::mem::replace(&mut self.shown, shown).filter(|&id| id != pending) {
            acks.push(Ack::Released(previous));
        }
        acks
    }
}

/// Copies `rects` (the whole buffer when empty) out of SHM `data`, `stride` bytes per row, clipped to the buffer.
pub(crate) fn copy_regions(data: &[u8], stride: usize, width: u32, height: u32, rects: &[Rect]) -> Vec<ShmRegion> {
    let whole = [Rect {
        x: 0,
        y: 0,
        width,
        height,
    }];
    let rects = if rects.is_empty() { &whole[..] } else { rects };
    rects
        .iter()
        .filter_map(|rect| clip(*rect, width, height))
        .filter_map(|rect| copy_region(data, stride, rect))
        .collect()
}

/// `rect` clipped to a `width` x `height` buffer; `None` when nothing of it is inside.
pub(crate) fn clip(rect: Rect, width: u32, height: u32) -> Option<Rect> {
    let right = rect.x.saturating_add(rect.width).min(width);
    let bottom = rect.y.saturating_add(rect.height).min(height);
    (rect.x < right && rect.y < bottom).then(|| Rect {
        x: rect.x,
        y: rect.y,
        width: right - rect.x,
        height: bottom - rect.y,
    })
}

/// One clipped rectangle's rows, or `None` when `data` is shorter than the buffer it claims to be.
fn copy_region(data: &[u8], stride: usize, rect: Rect) -> Option<ShmRegion> {
    let row_bytes = usize::try_from(rect.width).ok()? * 4;
    let left = usize::try_from(rect.x).ok()? * 4;
    let top = usize::try_from(rect.y).ok()?;
    let rows = usize::try_from(rect.height).ok()?;
    let mut pixels = Vec::with_capacity(row_bytes * rows);
    for row in top..top + rows {
        let start = row * stride + left;
        pixels.extend_from_slice(data.get(start..start + row_bytes)?);
    }
    Some(ShmRegion { rect, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: BufferId = BufferId(1);
    const B: BufferId = BufferId(2);
    const C: BufferId = BufferId(3);

    const DMABUF: Kind = Kind::DmaBuf;
    const SHM: Kind = Kind::Shm;

    #[test]
    fn a_dmabuf_is_released_one_frame_after_it_was_presented() {
        let mut flow = FrameFlow::default();
        assert!(flow.buffers_changed(vec![A, B]).is_empty());
        assert_eq!(flow.rendered(A, DMABUF), []);
        assert_eq!(
            flow.presented(A),
            [Ack::Rendered(A)],
            "nothing shown before: nothing to release"
        );
        assert_eq!(flow.rendered(B, DMABUF), []);
        assert_eq!(flow.presented(B), [Ack::Rendered(B), Ack::Released(A)]);
        assert_eq!(flow.rendered(A, DMABUF), []);
        assert_eq!(flow.presented(A), [Ack::Rendered(A), Ack::Released(B)]);
    }

    #[test]
    fn an_shm_buffer_goes_back_on_arrival_and_counts_as_rendered_when_presented() {
        let mut flow = FrameFlow::default();
        flow.buffers_changed(vec![A, B]);
        assert_eq!(flow.rendered(A, SHM), [Ack::Released(A)]);
        assert_eq!(flow.presented(A), [Ack::Rendered(A)]);
        assert_eq!(flow.rendered(B, SHM), [Ack::Released(B)]);
        assert_eq!(flow.presented(B), [Ack::Rendered(B)], "nothing held over");
    }

    #[test]
    fn falling_back_to_shm_releases_the_last_dmabuf_once_an_shm_frame_replaces_it() {
        let mut flow = FrameFlow::default();
        flow.buffers_changed(vec![A, B]);
        flow.rendered(A, DMABUF);
        assert_eq!(flow.presented(A), [Ack::Rendered(A)]);
        assert_eq!(flow.buffers_changed(vec![A, C]), [B], "WebKit's new SHM set");
        assert_eq!(flow.rendered(C, SHM), [Ack::Released(C)]);
        assert_eq!(flow.presented(C), [Ack::Rendered(C), Ack::Released(A)]);
        assert_eq!(flow.rendered(C, SHM), [Ack::Released(C)]);
        assert_eq!(flow.presented(C), [Ack::Rendered(C)], "no dmabuf held any more");
    }

    #[test]
    fn stale_and_repeated_presentations_are_dropped() {
        let mut flow = FrameFlow::default();
        flow.buffers_changed(vec![A, B]);
        assert_eq!(flow.presented(A), [], "never handed over");
        flow.rendered(A, DMABUF);
        assert_eq!(flow.presented(A), [Ack::Rendered(A)]);
        assert_eq!(flow.presented(A), [], "presented twice");
        flow.rendered(B, DMABUF);
        assert_eq!(flow.buffers_changed(vec![C]), [A, B], "recreated on load");
        assert_eq!(flow.presented(B), [], "the pending frame's buffer left the set");
        assert!(!flow.contains(A));
        assert_eq!(flow.rendered(C, DMABUF), []);
        assert_eq!(
            flow.presented(C),
            [Ack::Rendered(C)],
            "the old shown buffer is gone: nothing to release"
        );
    }

    #[test]
    fn the_set_grows_with_buffers_rendered_before_they_were_announced() {
        let mut flow = FrameFlow::default();
        flow.buffers_changed(vec![A, B]);
        assert!(!flow.contains(C));
        flow.rendered(C, DMABUF);
        assert!(flow.contains(C));
        assert_eq!(flow.buffers_changed(vec![A, B, C]), []);
    }

    #[test]
    fn a_superseded_frame_is_given_back() {
        let mut flow = FrameFlow::default();
        flow.buffers_changed(vec![A, B]);
        flow.rendered(A, DMABUF);
        assert_eq!(flow.rendered(B, DMABUF), [Ack::Rendered(A), Ack::Released(A)]);
        assert_eq!(flow.presented(A), []);
        assert_eq!(flow.presented(B), [Ack::Rendered(B)]);
        let mut shm = FrameFlow::default();
        shm.rendered(A, SHM);
        assert_eq!(
            shm.rendered(B, SHM),
            [Ack::Rendered(A), Ack::Released(B)],
            "A went back when it arrived"
        );
    }

    #[test]
    fn shm_regions_are_copied_row_by_row_and_clipped() {
        // 4x3 pixels, stride 20 (4 bytes of padding per row); pixel (x, y) holds bytes [y, x, 0, 0xff]
        let (width, height, stride) = (4_u32, 3_u32, 20_usize);
        let mut data = vec![0xee; stride * 3];
        for y in 0..3_u8 {
            for x in 0..4_u8 {
                let at = usize::from(y) * stride + usize::from(x) * 4;
                data[at..at + 4].copy_from_slice(&[y, x, 0, 0xff]);
            }
        }
        let rect = |x, y, width, height| Rect { x, y, width, height };
        let regions = copy_regions(
            &data,
            stride,
            width,
            height,
            &[rect(1, 1, 2, 2), rect(3, 2, 5, 5), rect(9, 9, 1, 1)],
        );
        assert_eq!(regions.len(), 2, "the rect outside the buffer is dropped");
        assert_eq!(regions[0].rect, rect(1, 1, 2, 2));
        assert_eq!(
            regions[0].pixels,
            [1, 1, 0, 0xff, 1, 2, 0, 0xff, 2, 1, 0, 0xff, 2, 2, 0, 0xff]
        );
        assert_eq!(regions[1].rect, rect(3, 2, 1, 1), "clipped to the buffer");
        assert_eq!(regions[1].pixels, [2, 3, 0, 0xff]);
        let whole = copy_regions(&data, stride, width, height, &[]);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].rect, rect(0, 0, 4, 3));
        assert_eq!(whole[0].pixels.len(), 4 * 4 * 3, "padding left out");
        assert!(
            copy_regions(&data[..30], stride, width, height, &[]).is_empty(),
            "short data copies nothing"
        );
    }
}
