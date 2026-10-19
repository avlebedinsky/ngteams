//! External surfaces on Linux (an agterm-rs patch): frames produced outside GPUI that the renderer draws through
//! [`crate::Window::paint_external_surface`], the Linux counterpart of macOS's `paint_surface`.
//!
//! A frame is either a dmabuf ([`DmaBufFrame`]), which the renderer imports into its own device (no CPU copy: the GPU
//! takes the buffer over from the producer's foreign queue family and copies the frame into a texture of its own), or
//! CPU pixels ([`PixelFrame`]), uploaded by damaged rectangle into a texture the renderer keeps per [`PixelSurface`].
//! Every frame reports once how it ended ([`FrameOutcome`]): `Presented` only after the GPU finished the submission
//! that copied or first drew it, so a producer that reuses a buffer on that report never races the renderer's reads.
//!
//! A dmabuf is imported once per [`DmaBuffer`] and kept while the producer keeps the buffer: the renderer holds only a
//! weak reference, so the import goes once the producer dropped its last `Arc` (when the buffer leaves its set) and
//! the GPU is done with it. [`dmabuf_import_caps`] says what the renderer's device imports, for a producer to allocate
//! on that device in formats it can import.

use std::fmt;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};

/// One memory plane of a dmabuf.
#[derive(Debug)]
pub struct DmaBufPlane {
    /// The plane's dmabuf; the renderer duplicates it to import, so the producer keeps this one.
    pub fd: OwnedFd,
    /// Byte offset of the plane in its buffer.
    pub offset: u32,
    /// Row pitch in bytes.
    pub stride: u32,
}

/// A producer's GPU buffer: a DRM fourcc and format modifier with every memory plane (a compressed buffer keeps several
/// in one object, e.g. Intel CCS: main, aux and clear color). The renderer imports it once and keeps the import while
/// this value lives.
#[derive(Debug)]
pub struct DmaBuffer {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The DRM fourcc (`XR24` is XRGB8888).
    pub fourcc: u32,
    /// The DRM format modifier.
    pub modifier: u64,
    /// The memory planes, in plane order.
    pub planes: Vec<DmaBufPlane>,
}

/// How a frame ended, reported once per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameOutcome {
    /// The GPU finished the submission that copied or first drew the frame: the renderer reads its buffer no more.
    Presented,
    /// The frame was dropped without being drawn (replaced before a draw, its window closed, its frame not submitted).
    Discarded,
    /// The renderer cannot draw it: the reason (a dmabuf that does not import, a device without dmabuf import).
    Failed(String),
    /// The frame was submitted but the renderer cannot tell when the GPU finished it (the device was lost, or the
    /// renderer shut down first): its buffer may still be read, so the producer must not render into it again.
    Lost(String),
}

/// The callback a frame reports its [`FrameOutcome`] to, called at most once, on any thread; a frame dropped before it
/// ended reports [`FrameOutcome::Discarded`].
pub struct FrameCompletion(Mutex<Option<Box<dyn FnOnce(FrameOutcome) + Send>>>);

impl FrameCompletion {
    /// A completion calling `callback`.
    pub fn new(callback: impl FnOnce(FrameOutcome) + Send + 'static) -> Self {
        Self(Mutex::new(Some(Box::new(callback))))
    }

    /// A completion nobody listens to.
    pub fn none() -> Self {
        Self(Mutex::new(None))
    }

    /// Takes the callback out, for a renderer that reports the outcome later (`None` once taken).
    pub fn take(&self) -> Option<Box<dyn FnOnce(FrameOutcome) + Send>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    /// Reports `outcome` now, unless the outcome was already reported.
    pub fn complete(&self, outcome: FrameOutcome) {
        if let Some(callback) = self.take() {
            callback(outcome);
        }
    }

    /// Whether the outcome is still to be reported.
    pub fn is_pending(&self) -> bool {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).is_some()
    }
}

impl Drop for FrameCompletion {
    fn drop(&mut self) {
        self.complete(FrameOutcome::Discarded);
    }
}

impl fmt::Debug for FrameCompletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameCompletion")
            .field("pending", &self.is_pending())
            .finish()
    }
}

/// A frame shown from a dmabuf.
#[derive(Debug)]
pub struct DmaBufFrame {
    /// The buffer the frame was rendered into.
    pub buffer: Arc<DmaBuffer>,
    /// Where the frame's outcome goes.
    pub completion: FrameCompletion,
}

/// The identity of a CPU-pixel surface: the renderer keeps one texture per surface while it lives.
#[derive(Debug, Default)]
pub struct PixelSurface {
    _private: (),
}

impl PixelSurface {
    /// A new surface.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

/// A rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

/// A frame of CPU pixels: premultiplied BGRA, B, G, R, A in memory (`ARGB8888` on a little-endian machine), rows
/// `width * 4` bytes apart.
#[derive(Debug)]
pub struct PixelFrame {
    /// The surface whose texture the pixels update.
    pub surface: Arc<PixelSurface>,
    /// Numbers the surface's frames: the renderer uploads only `damage` when the frame before (`serial - 1`) is
    /// the last one it uploaded for this surface at this size, and the whole frame otherwise.
    pub serial: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The whole frame.
    pub pixels: Arc<Vec<u8>>,
    /// What changed since frame `serial - 1`.
    pub damage: Vec<PixelRect>,
    /// Where the frame's outcome goes.
    pub completion: FrameCompletion,
}

/// A frame produced outside GPUI.
#[derive(Debug, Clone)]
pub enum ExternalSurface {
    /// A dmabuf, imported and copied on the GPU.
    DmaBuf(Arc<DmaBufFrame>),
    /// CPU pixels, uploaded by damaged rectangle.
    Pixels(Arc<PixelFrame>),
}

impl ExternalSurface {
    /// The frame's completion.
    pub fn completion(&self) -> &FrameCompletion {
        match self {
            Self::DmaBuf(frame) => &frame.completion,
            Self::Pixels(frame) => &frame.completion,
        }
    }
}

/// A format modifier a device imports, with the memory planes a buffer of it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaBufModifier {
    /// The DRM format modifier.
    pub modifier: u64,
    /// Its memory plane count.
    pub plane_count: u32,
}

/// A DRM format a device imports, with every modifier it imports it with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufFormat {
    /// The DRM fourcc.
    pub fourcc: u32,
    /// The modifiers, in the driver's order.
    pub modifiers: Vec<DmaBufModifier>,
}

/// What the device GPUI renders with imports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmaBufImportCaps {
    /// The adapter's name.
    pub adapter: String,
    /// Its DRM render node (`VK_EXT_physical_device_drm`), when the driver names one: a producer must allocate there,
    /// since a dmabuf made on another GPU does not import.
    pub render_node: Option<PathBuf>,
    /// The formats it imports; empty when it imports none.
    pub formats: Vec<DmaBufFormat>,
    /// Why it imports none (not a Vulkan device, an extension or feature missing), when `formats` is empty.
    pub unsupported: Option<String>,
}

static DMABUF_IMPORT_CAPS: RwLock<Option<Arc<DmaBufImportCaps>>> = RwLock::new(None);

/// What the device GPUI renders with imports: `None` until a renderer opened its device (the first window, or a
/// headless renderer). On Linux every window shares one device.
pub fn dmabuf_import_caps() -> Option<Arc<DmaBufImportCaps>> {
    DMABUF_IMPORT_CAPS
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Records what a renderer's newly opened device imports (renderers only).
#[doc(hidden)]
pub fn set_dmabuf_import_caps(caps: DmaBufImportCaps) {
    *DMABUF_IMPORT_CAPS
        .write()
        .unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(caps));
}

/// Counters over every renderer of the process, for diagnostics and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExternalSurfaceStats {
    /// Dmabufs imported.
    pub imports: u64,
    /// Dmabuf imports that failed.
    pub import_failures: u64,
    /// Imports dropped (their buffer gone, or their renderer).
    pub imports_released: u64,
    /// Dmabuf frames copied into the renderer's own textures (one per new frame, none for a redraw).
    pub copies: u64,
    /// Bytes of CPU pixels uploaded.
    pub pixel_bytes_uploaded: u64,
}

impl ExternalSurfaceStats {
    /// Imports alive now.
    pub fn live_imports(&self) -> u64 {
        self.imports - self.imports_released
    }
}

/// Where renderers count into [`external_surface_stats`] (renderers only).
#[doc(hidden)]
pub mod external_surface_counters {
    use super::{AtomicU64, Ordering};

    /// Dmabufs imported.
    pub static IMPORTS: AtomicU64 = AtomicU64::new(0);
    /// Failed imports.
    pub static IMPORT_FAILURES: AtomicU64 = AtomicU64::new(0);
    /// Imports dropped.
    pub static IMPORTS_RELEASED: AtomicU64 = AtomicU64::new(0);
    /// Dmabuf frames copied.
    pub static COPIES: AtomicU64 = AtomicU64::new(0);
    /// Pixel bytes uploaded.
    pub static PIXEL_BYTES_UPLOADED: AtomicU64 = AtomicU64::new(0);

    /// Adds `count` to `counter`.
    pub fn add(counter: &AtomicU64, count: u64) {
        counter.fetch_add(count, Ordering::Relaxed);
    }
}

/// The counters so far.
pub fn external_surface_stats() -> ExternalSurfaceStats {
    use external_surface_counters::*;
    ExternalSurfaceStats {
        imports: IMPORTS.load(Ordering::Relaxed),
        import_failures: IMPORT_FAILURES.load(Ordering::Relaxed),
        imports_released: IMPORTS_RELEASED.load(Ordering::Relaxed),
        copies: COPIES.load(Ordering::Relaxed),
        pixel_bytes_uploaded: PIXEL_BYTES_UPLOADED.load(Ordering::Relaxed),
    }
}
