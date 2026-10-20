//! A producer's dmabuf as a raw `VkImage` wgpu never tracks, and the GPU copy that acquires it from the foreign queue
//! family, copies it into a wgpu texture (the snapshot the renderer samples) and releases it back.
//!
//! wgpu cannot hold the imported image itself: every texture from hal enters wgpu-core's tracker uninitialized, so its
//! first use is an `UNDEFINED` layout transition, which lets the driver discard the producer's pixels (Mesa ANV
//! re-initialises fast-clear state on it, and CC modifiers keep the clear colour in the dmabuf itself), and wgpu
//! never acquires an image from `VK_QUEUE_FAMILY_FOREIGN_EXT`. So the image stays raw: each new frame is acquired
//! (`GENERAL`, foreign → this queue family), copied, and released (this queue family → foreign, back to `GENERAL`) in a
//! raw-only command buffer, the snapshot handed to and from wgpu's tracker as `COPY_DST` through
//! `CommandEncoder::transition_resources`, wgpu's documented native-interop path.

use super::{Format, fourcc_name, format_of};
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use ash::vk;
use gpui::{DmaBufFrame, DmaBufImportCaps, DmaBuffer};
use std::os::fd::{AsRawFd as _, IntoRawFd as _, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::sync::Weak;
use wgpu::hal::vulkan;

/// The format `buffer` is copied as, if the device imports it as it is: its format and modifier among the device's,
/// with the modifier's plane count, every plane in one buffer object, a size the device takes.
pub(super) fn check_buffer(caps: &DmaBufImportCaps, buffer: &DmaBuffer) -> Result<&'static Format> {
    if let Some(reason) = &caps.unsupported {
        bail!("the renderer's device imports no dmabufs: {reason}");
    }
    let format = format_of(buffer.fourcc).ok_or_else(|| anyhow!("unsupported format {}", fourcc_name(buffer.fourcc)))?;
    let modifiers = caps
        .formats
        .iter()
        .find(|candidate| candidate.fourcc == buffer.fourcc)
        .map(|candidate| candidate.modifiers.as_slice())
        .unwrap_or_default();
    let modifier = modifiers
        .iter()
        .find(|candidate| candidate.modifier == buffer.modifier)
        .ok_or_else(|| anyhow!("the device does not import {} with this modifier", fourcc_name(buffer.fourcc)))?;
    ensure!(
        buffer.planes.len() == modifier.plane_count as usize,
        "{} planes, the modifier has {}",
        buffer.planes.len(),
        modifier.plane_count
    );
    ensure!(buffer.width > 0 && buffer.height > 0, "empty buffer");
    ensure!(
        planes_share_one_object(buffer)?,
        "planes in separate buffer objects (a disjoint image is not supported)"
    );
    Ok(format)
}

/// Whether every plane's fd is the same dmabuf (one inode per exported buffer object).
fn planes_share_one_object(buffer: &DmaBuffer) -> Result<bool> {
    let identity = |fd: &OwnedFd| -> Result<(u64, u64)> {
        let file = std::fs::File::from(fd.try_clone().context("dup a plane fd")?);
        let metadata = file.metadata().context("fstat a plane fd")?;
        Ok((metadata.dev(), metadata.ino()))
    };
    let mut planes = buffer.planes.iter();
    let Some(first) = planes.next() else {
        return Ok(false);
    };
    let first = identity(&first.fd)?;
    for plane in planes {
        if identity(&plane.fd)? != first {
            return Ok(false);
        }
    }
    Ok(true)
}

/// An imported dmabuf: a `VkImage` with the buffer's modifier and plane layouts, bound to a dedicated allocation
/// importing its fd. wgpu never sees it; [`RawImage::destroy`] frees it once no submission uses it any more.
pub(super) struct RawImage {
    pub(super) image: vk::Image,
    memory: vk::DeviceMemory,
}

impl RawImage {
    /// Imports `buffer` (checked by [`check_buffer`]) on `device` as a copy source.
    pub(super) fn import(device: &wgpu::Device, buffer: &DmaBuffer, format: &Format) -> Result<Self> {
        // SAFETY: the guard is dropped at the end of this function, before the device is used through wgpu again.
        let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("the device is not Vulkan")?;
        let (image, memory) = create_image(&hal_device, buffer, format)?;
        Ok(Self { image, memory })
    }

    /// Destroys the image and frees its memory.
    ///
    /// # Safety
    ///
    /// `raw` is the device the image was created on, and no submitted or recorded command uses the image any more.
    pub(super) unsafe fn destroy(self, raw: &ash::Device) {
        // SAFETY: the caller guarantees nothing uses the image; it and its memory were created on `raw`.
        unsafe { raw.destroy_image(self.image, None) };
        // SAFETY: as above; the image bound to the memory is gone.
        unsafe { raw.free_memory(self.memory, None) };
    }
}

/// The wgpu texture a buffer's frames are copied into and sampled from.
pub(super) fn create_snapshot(device: &wgpu::Device, buffer: &DmaBuffer, format: &Format) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dmabuf_snapshot"),
        size: wgpu::Extent3d {
            width: buffer.width,
            height: buffer.height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: format.wgpu,
        // RENDER_ATTACHMENT for the one clear that makes wgpu count the snapshot initialized (it would otherwise clear
        // it lazily at the first sampling, over the raw copy).
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

/// One frame to copy: from the buffer's imported image into its snapshot.
pub(super) struct CopyJob {
    /// The import's key.
    pub(super) key: usize,
    /// The frame copied, kept weakly so its address names it until the cache moves on.
    pub(super) frame: Weak<DmaBufFrame>,
    pub(super) source: vk::Image,
    pub(super) snapshot: wgpu::Texture,
    pub(super) width: u32,
    pub(super) height: u32,
    /// The snapshot needs its initializing clear first.
    pub(super) clear: bool,
}

/// The two command buffers of a batch of copies, to submit in this order before the frame's own: wgpu's (the
/// snapshots' first clear and their transition to `COPY_DST`) and the raw one (acquire, copy, release).
pub(super) fn record(device: &wgpu::Device, jobs: &[CopyJob]) -> Result<[wgpu::CommandBuffer; 2]> {
    let mut prepare = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("external_surfaces_prepare"),
    });
    for job in jobs.iter().filter(|job| job.clear) {
        let view = job.snapshot.create_view(&wgpu::TextureViewDescriptor::default());
        drop(prepare.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dmabuf_snapshot_clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        }));
    }
    prepare.transition_resources(
        std::iter::empty(),
        jobs.iter().map(|job| wgpu::TextureTransition {
            texture: &job.snapshot,
            selector: None,
            state: wgpu::TextureUses::COPY_DST,
        }),
    );
    let mut copy = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("external_surfaces_copy"),
    });
    record_raw(device, &mut copy, jobs)?;
    Ok([prepare.finish(), copy.finish()])
}

/// Records, raw, each job's acquire from the foreign queue family, copy into its snapshot (which wgpu left in
/// `TRANSFER_DST_OPTIMAL` and finds there again) and release to the foreign queue family.
fn record_raw(device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder, jobs: &[CopyJob]) -> Result<()> {
    let (raw, family) = {
        // SAFETY: the guard is only read and dropped at the end of this block.
        let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("the device is not Vulkan")?;
        (hal_device.raw_device().clone(), hal_device.queue_family_index())
    };
    let snapshots = jobs
        .iter()
        .map(|job| {
            // SAFETY: the guard is only read and dropped at once; the handle stays valid while `job.snapshot` (held
            // by the caller until the submission is recorded and by the cache after) lives.
            let texture = unsafe { job.snapshot.as_hal::<vulkan::Api>() }?;
            // SAFETY: the handle is only recorded into commands, never destroyed here.
            Some(unsafe { texture.raw_handle() })
        })
        .collect::<Option<Vec<vk::Image>>>()
        .context("a snapshot is not a Vulkan texture")?;
    let color = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .level_count(1)
        .layer_count(1);
    let acquire: Vec<vk::ImageMemoryBarrier<'_>> = jobs
        .iter()
        .map(|job| {
            vk::ImageMemoryBarrier::default()
                .image(job.source)
                .subresource_range(color)
                .src_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .dst_queue_family_index(family)
                .old_layout(vk::ImageLayout::GENERAL)
                .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
        })
        .collect();
    let release: Vec<vk::ImageMemoryBarrier<'_>> = jobs
        .iter()
        .map(|job| {
            vk::ImageMemoryBarrier::default()
                .image(job.source)
                .subresource_range(color)
                .src_queue_family_index(family)
                .dst_queue_family_index(vk::QUEUE_FAMILY_FOREIGN_EXT)
                .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_READ)
                .dst_access_mask(vk::AccessFlags::empty())
        })
        .collect();
    let record = |encoder: Option<&mut vulkan::CommandEncoder>| {
        let encoder = encoder?;
        // SAFETY: the command buffer is open (wgpu-core opened it for this call) and is only recorded into.
        let commands = unsafe { encoder.raw_handle() };
        // SAFETY: every image is alive (the sources are the cache's, the snapshots the jobs'); each source is
        // released by the producer in GENERAL and owned by the foreign queue family between frames
        // (VK_EXT_queue_family_foreign is enabled), each snapshot is in TRANSFER_DST_OPTIMAL (the prepare buffer put
        // it there through wgpu's tracker); the copies are whole 2D images of one format and size.
        unsafe { record_commands(&raw, commands, jobs, &snapshots, &acquire, &release) };
        Some(())
    };
    // SAFETY: a raw-only encoder (wgpu forbids mixing it with wgpu commands), so wgpu records nothing into it.
    let recorded = unsafe { encoder.as_hal_mut::<vulkan::Api, _, _>(record) };
    recorded.context("the copy encoder is not a Vulkan encoder")
}

/// # Safety
///
/// As at the call in [`record_raw`].
unsafe fn record_commands(
    raw: &ash::Device,
    commands: vk::CommandBuffer,
    jobs: &[CopyJob],
    snapshots: &[vk::Image],
    acquire: &[vk::ImageMemoryBarrier<'_>],
    release: &[vk::ImageMemoryBarrier<'_>],
) {
    // SAFETY: see the caller.
    unsafe {
        raw.cmd_pipeline_barrier(
            commands,
            vk::PipelineStageFlags::TOP_OF_PIPE,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            acquire,
        );
    }
    let layers = vk::ImageSubresourceLayers::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .layer_count(1);
    for (job, &snapshot) in jobs.iter().zip(snapshots) {
        let region = vk::ImageCopy::default()
            .src_subresource(layers)
            .dst_subresource(layers)
            .extent(vk::Extent3D {
                width: job.width,
                height: job.height,
                depth: 1,
            });
        // SAFETY: see the caller.
        unsafe {
            raw.cmd_copy_image(
                commands,
                job.source,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                snapshot,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        }
    }
    // SAFETY: see the caller.
    unsafe {
        raw.cmd_pipeline_barrier(
            commands,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            release,
        );
    }
}

/// A `VkImage` over `buffer`'s memory: the modifier's tiling with every plane's layout, bound to a dedicated
/// allocation importing the buffer's dmabuf.
fn create_image(device: &vulkan::Device, buffer: &DmaBuffer, format: &Format) -> Result<(vk::Image, vk::DeviceMemory)> {
    let raw = device.raw_device();
    let plane_layouts: Vec<vk::SubresourceLayout> = buffer
        .planes
        .iter()
        .map(|plane| vk::SubresourceLayout {
            offset: u64::from(plane.offset),
            size: 0,
            row_pitch: u64::from(plane.stride),
            array_pitch: 0,
            depth_pitch: 0,
        })
        .collect();
    let mut modifier_info = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
        .drm_format_modifier(buffer.modifier)
        .plane_layouts(&plane_layouts);
    let mut external_info =
        vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    // `initial_layout` UNDEFINED is what an external-memory image must be created with; no barrier ever transitions
    // from it (every acquire names GENERAL, the layout the producer hands buffers over in).
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format.vk)
        .extent(vk::Extent3D {
            width: buffer.width,
            height: buffer.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .push_next(&mut external_info)
        .push_next(&mut modifier_info);
    // SAFETY: the device has VK_EXT_image_drm_format_modifier and the external memory extensions (`open_device`), and
    // `check_buffer` matched the format, the modifier and its plane count against what the device reports it imports
    // as a copy source; every chained struct lives until the call returns.
    let image = unsafe { raw.create_image(&image_info, None) }.context("vkCreateImage")?;
    match bind_imported_memory(device, image, buffer) {
        Ok(memory) => Ok((image, memory)),
        Err(error) => {
            // SAFETY: the image was created above, bound to nothing and used by nothing.
            unsafe { raw.destroy_image(image, None) };
            Err(error)
        }
    }
}

/// Allocates memory importing `buffer`'s dmabuf for `image` and binds it.
fn bind_imported_memory(device: &vulkan::Device, image: vk::Image, buffer: &DmaBuffer) -> Result<vk::DeviceMemory> {
    let raw = device.raw_device();
    let instance = device.shared_instance().raw_instance();
    let first = buffer.planes.first().ok_or_else(|| anyhow!("no planes"))?;
    let fd = first.fd.try_clone().context("dup the dmabuf fd")?;
    let fd_device = ash::khr::external_memory_fd::Device::new(instance, raw);
    let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: VK_KHR_external_memory_fd is enabled on this device; `fd` is an open dmabuf this function owns.
    unsafe {
        fd_device.get_memory_fd_properties(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT, fd.as_raw_fd(), &mut fd_properties)
    }
    .context("vkGetMemoryFdPropertiesKHR")?;
    // SAFETY: `image` was created on this device and is not bound yet.
    let requirements = unsafe { raw.get_image_memory_requirements(image) };
    let type_bits = requirements.memory_type_bits & fd_properties.memory_type_bits;
    ensure!(type_bits != 0, "no memory type fits both the image and the dmabuf");
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
        .fd(fd.as_raw_fd());
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let allocate = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(type_bits.trailing_zeros())
        .push_next(&mut import)
        .push_next(&mut dedicated);
    // SAFETY: the memory type is one both the image and the dmabuf accept, the allocation is dedicated to `image`,
    // and `fd` is open. On success Vulkan owns the fd (released from `OwnedFd` below without closing it); on failure
    // it stays ours and `fd` closes it as it drops.
    let memory = unsafe { raw.allocate_memory(&allocate, None) }.context("vkAllocateMemory (dmabuf import)")?;
    let _owned_by_vulkan = fd.into_raw_fd();
    // SAFETY: `memory` was allocated for `image` (dedicated, of its size) and neither is bound or in use.
    if let Err(error) = unsafe { raw.bind_image_memory(image, memory, 0) } {
        // SAFETY: `memory` was allocated above and is bound to nothing.
        unsafe { raw.free_memory(memory, None) };
        bail!("vkBindImageMemory: {error}");
    }
    Ok(memory)
}
