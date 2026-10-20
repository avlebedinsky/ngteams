//! External surfaces on Linux (an agterm-rs patch): draws `gpui::ExternalSurface` frames — dmabufs a producer
//! rendered on this GPU, and CPU pixels uploaded by damaged rectangle into a texture kept per surface — and opens the
//! Vulkan device so that it can import dmabufs.
//!
//! - Device ([`device`]): wgpu enables `VK_KHR_external_memory_fd` and `VK_EXT_external_memory_dma_buf` where
//!   supported but never `VK_EXT_image_drm_format_modifier` (needed to describe any GPU-rendered buffer) nor
//!   `VK_EXT_queue_family_foreign` (needed to take a buffer over from its producer), so [`open_device`] opens the
//!   adapter through wgpu-hal's `open_with_callback` adding them. [`record_caps`] publishes what the device imports
//!   (`gpui::dmabuf_import_caps`): its DRM render node (`VK_EXT_physical_device_drm`) and the formats and modifiers it
//!   copies from an imported dmabuf.
//! - Import ([`import`]): one raw `VkImage` per producer buffer with its modifier and every memory plane's layout (a
//!   non-disjoint image: Intel's CCS modifiers keep three planes in one object), checked against the capabilities
//!   first, bound to a dedicated allocation importing the dmabuf, never handed to wgpu. Each new frame is acquired from
//!   the foreign queue family in `GENERAL`, copied on the GPU into a wgpu texture (the buffer's snapshot, which the
//!   renderer samples) and released back; redraws of the same frame sample the snapshot alone. wgpu cannot hold the
//!   imported image itself: its first use would be an `UNDEFINED` layout transition, which lets the driver discard
//!   the producer's pixels, and it never transfers queue-family ownership. Imports are cached per `gpui::DmaBuffer`
//!   and destroyed once the producer dropped the buffer and the GPU finished the last copy from it.
//! - Completion ([`completion`]): a frame reports `Presented` once the GPU finished the submission that copied or
//!   first drew it — from then on the renderer never reads the producer's buffer for it — `Lost` when that cannot be
//!   known (device lost), `Discarded` when it was never submitted, `Failed` when it does not import.
//! - Acquire: the producer's rendering must be finished before a frame is handed over (the caller waits on the
//!   producer's fence); the renderer adds no wait of its own.

mod completion;
mod device;
mod import;

pub(crate) use device::{open_device, record_caps};

use anyhow::{Result, anyhow, bail, ensure};
use ash::vk;
use collections::FxHashMap;
use completion::Timeline;
use gpui::external_surface_counters as counters;
use gpui::{DmaBufFrame, DmaBuffer, ExternalSurface, FrameOutcome, PaintSurface, PixelFrame, PixelSurface};
use import::{CopyJob, RawImage};
use std::sync::{Arc, Weak};
use std::time::Duration;
use wgpu::hal::vulkan;

/// `fourcc_code` of `drm_fourcc.h`.
const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*code)
}

/// A DRM format the renderer draws: its Vulkan and wgpu formats, and whether its fourth channel is padding.
struct Format {
    fourcc: u32,
    vk: vk::Format,
    wgpu: wgpu::TextureFormat,
    opaque: bool,
}

/// The formats an external surface may have: 8-bit RGB with alpha or padding, in either byte order. DRM names
/// channels from the most significant bit of a little-endian word, so `AR24` (ARGB8888) is B, G, R, A in memory.
const FORMATS: [Format; 4] = [
    Format {
        fourcc: fourcc(b"AR24"),
        vk: vk::Format::B8G8R8A8_UNORM,
        wgpu: wgpu::TextureFormat::Bgra8Unorm,
        opaque: false,
    },
    Format {
        fourcc: fourcc(b"XR24"),
        vk: vk::Format::B8G8R8A8_UNORM,
        wgpu: wgpu::TextureFormat::Bgra8Unorm,
        opaque: true,
    },
    Format {
        fourcc: fourcc(b"AB24"),
        vk: vk::Format::R8G8B8A8_UNORM,
        wgpu: wgpu::TextureFormat::Rgba8Unorm,
        opaque: false,
    },
    Format {
        fourcc: fourcc(b"XB24"),
        vk: vk::Format::R8G8B8A8_UNORM,
        wgpu: wgpu::TextureFormat::Rgba8Unorm,
        opaque: true,
    },
];

fn format_of(fourcc: u32) -> Option<&'static Format> {
    FORMATS.iter().find(|format| format.fourcc == fourcc)
}

fn fourcc_name(fourcc: u32) -> String {
    fourcc
        .to_le_bytes()
        .iter()
        .map(|&byte| if byte.is_ascii_graphic() { char::from(byte) } else { '?' })
        .collect()
}


// MARK: - the renderer's side

/// A producer's buffer as the renderer holds it: the imported image and the snapshot the renderer samples.
struct Import {
    buffer: Weak<DmaBuffer>,
    source: RawImage,
    snapshot: wgpu::Texture,
    view: wgpu::TextureView,
    opaque: bool,
    /// The frame whose pixels the snapshot holds (its copy, and before the first one the snapshot's initializing
    /// clear, was submitted); weak, so its address stays its own.
    copied: Option<Weak<DmaBufFrame>>,
    /// The timeline value of the last submission that copied from `source` (0: none, or known finished).
    last_use: u64,
}

/// The texture of a CPU-pixel surface.
struct PixelTexture {
    surface: Weak<PixelSurface>,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    /// The serial of the frame uploaded last.
    serial: u64,
}

type Callback = Box<dyn FnOnce(FrameOutcome) + Send>;

/// What `prepare` recorded for the frame about to be submitted; the cache learns of it only once it was submitted.
#[derive(Default)]
struct Pending {
    /// The copies recorded: (import key, frame).
    copies: Vec<(usize, Weak<DmaBufFrame>)>,
    /// Their command buffers, submitted before the frame's own.
    command_buffers: Vec<wgpu::CommandBuffer>,
    /// The completions of frames this frame copies or draws for the first time.
    completing: Vec<Callback>,
}

/// The renderer's external surfaces: imports, pixel textures, this frame's draws and pending work.
#[derive(Default)]
pub(crate) struct ExternalSurfaces {
    /// By `Arc::as_ptr` of the buffer: the weak reference keeps the allocation, so the address is not reused while
    /// the entry exists.
    imports: FxHashMap<usize, Import>,
    /// Buffers that did not import, keyed as `imports`, with why: a frame shown again is not imported again.
    failed: FxHashMap<usize, (Weak<DmaBuffer>, String)>,
    pixels: FxHashMap<usize, PixelTexture>,
    /// Imported images whose buffer is gone, with the last timeline value that used them.
    retired: Vec<(RawImage, u64)>,
    /// One bind group per `scene.surfaces` entry, in scene order; `None` when that frame is not drawn.
    draws: Vec<Option<wgpu::BindGroup>>,
    pending: Pending,
    timeline: Option<Timeline>,
    timeline_failed: bool,
    /// The device the raw images belong to, kept alive while any exists.
    device: Option<(wgpu::Device, ash::Device)>,
}

/// Uniforms of one external surface draw (`ExternalSurfaceParams` in shaders.wgsl).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ExternalSurfaceParams {
    pub(crate) bounds: [f32; 4],
    pub(crate) content_mask: [f32; 4],
    pub(crate) opaque: u32,
    pub(crate) pad: [u32; 3],
}

/// What [`ExternalSurfaces::prepare`] needs from the renderer.
pub(crate) struct Resources<'a> {
    pub(crate) device: &'a wgpu::Device,
    pub(crate) queue: &'a wgpu::Queue,
    pub(crate) layout: &'a wgpu::BindGroupLayout,
    pub(crate) sampler: &'a wgpu::Sampler,
}

impl ExternalSurfaces {
    /// The bind group drawing `scene.surfaces[index]`, if that frame is drawn.
    pub(crate) fn draw(&self, index: usize) -> Option<&wgpu::BindGroup> {
        self.draws.get(index).and_then(Option::as_ref)
    }

    /// Imports, copies, uploads and binds this frame's surfaces, and forgets what their producers dropped.
    pub(crate) fn prepare(&mut self, resources: &Resources<'_>, surfaces: &[PaintSurface]) {
        self.retire();
        self.draws.clear();
        let mut jobs = Vec::new();
        for surface in surfaces {
            let drawn = match &surface.source {
                ExternalSurface::DmaBuf(frame) => self.dmabuf_view(resources.device, frame, &mut jobs),
                ExternalSurface::Pixels(frame) => self.pixel_view(resources, frame).map(|view| (view, false)),
            };
            match drawn {
                Ok((view, opaque)) => {
                    let params = ExternalSurfaceParams {
                        bounds: bounds(surface.bounds),
                        content_mask: bounds(surface.content_mask.bounds),
                        opaque: u32::from(opaque),
                        pad: [0; 3],
                    };
                    self.draws.push(Some(bind_group(resources, &view, &params)));
                    if let Some(callback) = surface.source.completion().take() {
                        self.pending.completing.push(callback);
                    }
                }
                Err(error) => {
                    self.draws.push(None);
                    surface.source.completion().complete(FrameOutcome::Failed(format!("{error:#}")));
                }
            }
        }
        if jobs.is_empty() {
            return;
        }
        match import::record(resources.device, &jobs) {
            Ok(command_buffers) => {
                counters::add(&counters::COPIES, jobs.len() as u64);
                self.pending.command_buffers.extend(command_buffers);
                self.pending
                    .copies
                    .extend(jobs.into_iter().map(|job| (job.key, job.frame)));
            }
            Err(error) => log::error!("external surfaces: recording the dmabuf copies failed: {error:#}"),
        }
    }

    /// The command buffers to submit before the frame's own (the copies), taken.
    pub(crate) fn command_buffers(&mut self) -> Vec<wgpu::CommandBuffer> {
        std::mem::take(&mut self.pending.command_buffers)
    }

    /// Before the frame's submission: has it signal the timeline when it copies or first draws a frame.
    pub(crate) fn before_submit(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<u64> {
        if self.pending.completing.is_empty() && self.pending.copies.is_empty() {
            return None;
        }
        if self.timeline.is_none() && !self.timeline_failed {
            // SAFETY: the guard is only checked and dropped at once.
            let vulkan = unsafe { device.as_hal::<vulkan::Api>() }.is_some();
            if vulkan {
                match Timeline::new(device) {
                    Ok(timeline) => self.timeline = Some(timeline),
                    Err(error) => {
                        log::error!("external surfaces: no completion timeline ({error:#}); waiting for each frame");
                        self.timeline_failed = true;
                    }
                }
            } else {
                // Not Vulkan (GL): no dmabuf imports here, and CPU pixels are copied at submission.
                self.timeline_failed = true;
            }
        }
        self.timeline.as_mut()?.signal(queue)
    }

    /// After the frame's `submission` (`signal` from [`Self::before_submit`]): the copies it recorded join the cache,
    /// and its frames complete once the GPU finished it.
    pub(crate) fn after_submit(
        &mut self,
        device: &wgpu::Device,
        submission: wgpu::SubmissionIndex,
        signal: Option<u64>,
    ) {
        let copies = std::mem::take(&mut self.pending.copies);
        let callbacks = std::mem::take(&mut self.pending.completing);
        let (last_use, outcome) = match (signal, &self.timeline) {
            (Some(value), Some(_)) => (value, None),
            _ if copies.is_empty() => (0, Some(FrameOutcome::Presented)),
            _ => match device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(10)),
            }) {
                Ok(_) => (0, Some(FrameOutcome::Presented)),
                Err(error) => (u64::MAX, Some(FrameOutcome::Lost(format!("waiting for the frame: {error}")))),
            },
        };
        match (outcome, &self.timeline) {
            (Some(outcome), _) => {
                for callback in callbacks {
                    callback(outcome.clone());
                }
            }
            (None, Some(timeline)) => timeline.complete_at(last_use, callbacks),
            (None, None) => {}
        }
        for (key, frame) in copies {
            if let Some(import) = self.imports.get_mut(&key) {
                import.copied = Some(frame);
                import.last_use = last_use;
            }
        }
    }

    /// The frame was not submitted (recording it failed): its copies are forgotten. Its frames are still shown and the
    /// next frame copies them again, so their completions stay pending and that frame's submission reports them: the
    /// producer gets its buffer back only after a copy the GPU finished (or once nothing reads it at all).
    pub(crate) fn abort_frame(&mut self) {
        self.pending.copies.clear();
        self.pending.command_buffers.clear();
    }

    /// Forgets what producers dropped; destroys imported images the GPU is done with.
    fn retire(&mut self) {
        let mut released = 0;
        let mut kept = FxHashMap::default();
        for (key, import) in self.imports.drain() {
            if import.buffer.strong_count() > 0 {
                kept.insert(key, import);
            } else {
                released += 1;
                self.retired.push((import.source, import.last_use));
            }
        }
        self.imports = kept;
        counters::add(&counters::IMPORTS_RELEASED, released);
        self.failed.retain(|_, (buffer, _)| buffer.strong_count() > 0);
        self.pixels.retain(|_, texture| texture.surface.strong_count() > 0);
        if self.retired.is_empty() {
            return;
        }
        let reached = self.timeline.as_ref().map_or(0, Timeline::reached);
        let Some((_, raw)) = &self.device else {
            return;
        };
        let (done, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.retired)
            .into_iter()
            .partition(|(_, last_use)| *last_use <= reached);
        self.retired = waiting;
        for (image, _) in done {
            // SAFETY: the producer dropped the buffer, no frame of it is pending, and the GPU reached the last
            // submission that copied from the image; it was created on this device.
            unsafe { image.destroy(raw) };
        }
    }

    fn dmabuf_view(
        &mut self,
        device: &wgpu::Device,
        frame: &Arc<DmaBufFrame>,
        jobs: &mut Vec<CopyJob>,
    ) -> Result<(wgpu::TextureView, bool)> {
        let buffer = &frame.buffer;
        let key = Arc::as_ptr(buffer) as usize;
        if let Some((_, reason)) = self.failed.get(&key) {
            bail!("{reason}");
        }
        if !self.imports.contains_key(&key) {
            self.import(device, buffer, key)?;
        }
        let import = self.imports.get(&key).ok_or_else(|| anyhow!("import missing"))?;
        let frame_ptr = Arc::as_ptr(frame);
        let copied = import.copied.as_ref().is_some_and(|copied| copied.as_ptr() == frame_ptr);
        let queued = jobs
            .iter()
            .any(|job| job.key == key && job.frame.as_ptr() == frame_ptr);
        if !copied && !queued {
            let clear = import.copied.is_none() && !jobs.iter().any(|job| job.key == key);
            jobs.push(CopyJob {
                key,
                frame: Arc::downgrade(frame),
                source: import.source.image,
                snapshot: import.snapshot.clone(),
                width: buffer.width,
                height: buffer.height,
                clear,
            });
        }
        Ok((import.view.clone(), import.opaque))
    }

    fn import(&mut self, device: &wgpu::Device, buffer: &Arc<DmaBuffer>, key: usize) -> Result<()> {
        let caps = gpui::dmabuf_import_caps().ok_or_else(|| anyhow!("the renderer has no device yet"))?;
        let imported = import::check_buffer(&caps, buffer).and_then(|format| {
            let source = RawImage::import(device, buffer, format)?;
            Ok((source, format))
        });
        let (source, format) = match imported {
            Ok(imported) => imported,
            Err(error) => {
                counters::add(&counters::IMPORT_FAILURES, 1);
                let error = error.context(format!(
                    "dmabuf {}x{} {} modifier {:#018x} ({} planes) does not import",
                    buffer.width,
                    buffer.height,
                    fourcc_name(buffer.fourcc),
                    buffer.modifier,
                    buffer.planes.len()
                ));
                log::warn!("external surfaces: {error:#}");
                self.failed.insert(key, (Arc::downgrade(buffer), format!("{error:#}")));
                return Err(error);
            }
        };
        if self.device.is_none() {
            // SAFETY: the guard is only read and dropped at once; the ash device stays valid while the wgpu device
            // kept beside it lives.
            let raw = unsafe { device.as_hal::<vulkan::Api>() }.map(|hal| hal.raw_device().clone());
            self.device = raw.map(|raw| (device.clone(), raw));
        }
        counters::add(&counters::IMPORTS, 1);
        let snapshot = import::create_snapshot(device, buffer, format);
        let view = snapshot.create_view(&wgpu::TextureViewDescriptor::default());
        self.imports.insert(
            key,
            Import {
                buffer: Arc::downgrade(buffer),
                source,
                snapshot,
                view,
                opaque: format.opaque,
                copied: None,
                last_use: 0,
            },
        );
        Ok(())
    }

    fn pixel_view(&mut self, resources: &Resources<'_>, frame: &PixelFrame) -> Result<wgpu::TextureView> {
        let expected = u64::from(frame.width) * u64::from(frame.height) * 4;
        ensure!(
            frame.width > 0 && frame.height > 0 && frame.pixels.len() as u64 == expected,
            "pixel frame {}x{} with {} bytes",
            frame.width,
            frame.height,
            frame.pixels.len()
        );
        let key = Arc::as_ptr(&frame.surface) as usize;
        let reusable = self
            .pixels
            .get(&key)
            .is_some_and(|texture| texture.width == frame.width && texture.height == frame.height);
        if !reusable {
            let texture = resources.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("external_pixels"),
                size: wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Bgra8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.pixels.insert(
                key,
                PixelTexture {
                    surface: Arc::downgrade(&frame.surface),
                    texture,
                    view,
                    width: frame.width,
                    height: frame.height,
                    serial: 0,
                },
            );
        }
        let texture = self
            .pixels
            .get_mut(&key)
            .ok_or_else(|| anyhow!("pixel texture missing"))?;
        if !reusable || texture.serial != frame.serial {
            let incremental = reusable && texture.serial.checked_add(1) == Some(frame.serial);
            upload(resources.queue, texture, frame, incremental);
            texture.serial = frame.serial;
        }
        Ok(texture.view.clone())
    }
}

impl Drop for ExternalSurfaces {
    fn drop(&mut self) {
        // Frames bound for a submission that will never happen: nothing reads their buffers any more.
        self.abort_frame();
        for callback in self.pending.completing.drain(..) {
            callback(FrameOutcome::Discarded);
        }
        counters::add(&counters::IMPORTS_RELEASED, self.imports.len() as u64);
        // Every submission that copied from an imported image must be finished before the image goes.
        let settled = self.timeline.as_ref().is_none_or(Timeline::settle);
        self.timeline = None;
        let images: Vec<(RawImage, u64)> = self
            .imports
            .drain()
            .map(|(_, import)| (import.source, import.last_use))
            .chain(std::mem::take(&mut self.retired))
            .collect();
        let Some((_, raw)) = &self.device else {
            return;
        };
        for (image, last_use) in images {
            if settled && last_use != u64::MAX {
                // SAFETY: the GPU finished every submission that used the image (the timeline settled, or each copy
                // was waited for), and nothing records commands any more; it was created on this device.
                unsafe { image.destroy(raw) };
            } else {
                log::warn!("external surfaces: a dmabuf copy never finished; its imported image is leaked");
            }
        }
    }
}

fn bounds(bounds: gpui::Bounds<gpui::ScaledPixels>) -> [f32; 4] {
    [
        bounds.origin.x.0,
        bounds.origin.y.0,
        bounds.size.width.0,
        bounds.size.height.0,
    ]
}

fn bind_group(resources: &Resources<'_>, view: &wgpu::TextureView, params: &ExternalSurfaceParams) -> wgpu::BindGroup {
    use wgpu::util::DeviceExt as _;
    let uniform = resources.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("external_surface_params"),
        contents: bytemuck::bytes_of(params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    resources.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("external_surface"),
        layout: resources.layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 4,
                resource: uniform.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::Sampler(resources.sampler),
            },
        ],
    })
}

/// The bind group layout of external surface draws (group 1 of `vs_external_surface`/`fs_external_surface`).
pub(crate) fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("external_surfaces_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<ExternalSurfaceParams>() as u64),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 5,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 6,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// Uploads `frame` into `texture`: its damaged rectangles when the texture holds the frame before it, else all of it.
fn upload(queue: &wgpu::Queue, texture: &PixelTexture, frame: &PixelFrame, incremental: bool) {
    let whole = [gpui::PixelRect {
        x: 0,
        y: 0,
        width: frame.width,
        height: frame.height,
    }];
    let rects: &[gpui::PixelRect] = if incremental { &frame.damage } else { &whole };
    let stride = frame.width * 4;
    for rect in rects {
        let x = rect.x.min(frame.width);
        let y = rect.y.min(frame.height);
        let width = rect.width.min(frame.width - x);
        let height = rect.height.min(frame.height - y);
        if width == 0 || height == 0 {
            continue;
        }
        let start = y as usize * stride as usize + x as usize * 4;
        let end = (y + height - 1) as usize * stride as usize + (x + width) as usize * 4;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &frame.pixels[start..end],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        counters::add(&counters::PIXEL_BYTES_UPLOADED, u64::from(width) * u64::from(height) * 4);
    }
}

