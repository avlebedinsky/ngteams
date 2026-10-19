//! The Vulkan device that imports dmabufs, and what it imports (`gpui::dmabuf_import_caps`).

use super::{FORMATS, fourcc_name};
use anyhow::{Context as _, Result, ensure};
use ash::vk;
use gpui::{DmaBufFormat, DmaBufImportCaps, DmaBufModifier};
use std::ffi::CStr;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use wgpu::hal::vulkan;

/// The device extensions dmabuf import needs beyond what wgpu enables by itself: the external memory pair (wgpu adds
/// them where supported), DRM format modifiers (wgpu never does) and the foreign queue family a dmabuf is acquired
/// from and released to.
pub(super) const EXTENSIONS: [&CStr; 4] = [
    ash::khr::external_memory_fd::NAME,
    ash::ext::external_memory_dma_buf::NAME,
    ash::ext::image_drm_format_modifier::NAME,
    ash::ext::queue_family_foreign::NAME,
];

/// Opens `adapter` as a Vulkan device that imports dmabufs; `None` when it is not a Vulkan adapter or cannot import
/// them (the caller then opens it as wgpu would, and [`record_caps`] says why).
pub(crate) fn open_device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let open = {
        // SAFETY: the guard is only read here and dropped before the adapter is used again.
        let hal_adapter = unsafe { adapter.as_hal::<vulkan::Api>() }?;
        if let Err(reason) = missing_requirements(&hal_adapter) {
            log::info!("dmabuf import unavailable on {}: {reason}", adapter.get_info().name);
            return None;
        }
        let callback: Box<vulkan::CreateDeviceCallback<'_>> = Box::new(|args| {
            for extension in EXTENSIONS {
                if !args.extensions.contains(&extension) {
                    args.extensions.push(extension);
                }
            }
        });
        // SAFETY: the callback only adds extensions the physical device supports (`missing_requirements` checked
        // each), which wgpu-hal allows; features, limits and hints are the descriptor's, as `request_device` passes.
        let opened = unsafe {
            hal_adapter.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(callback),
            )
        };
        match opened {
            Ok(open) => open,
            Err(error) => {
                log::warn!("opening the Vulkan device with dmabuf import failed: {error}");
                return None;
            }
        }
    };
    // SAFETY: `open` was opened from this adapter's own physical device with the descriptor's features and limits.
    match unsafe { adapter.create_device_from_hal::<vulkan::Api>(open, desc) } {
        Ok(pair) => Some(pair),
        Err(error) => {
            log::warn!("wrapping the Vulkan device with dmabuf import failed: {error}");
            None
        }
    }
}

/// Why `adapter` cannot import dmabufs, if it cannot.
fn missing_requirements(adapter: &vulkan::Adapter) -> Result<(), String> {
    let capabilities = adapter.physical_device_capabilities();
    let missing: Vec<String> = EXTENSIONS
        .iter()
        .filter(|extension| !capabilities.supports_extension(extension))
        .map(|extension| extension.to_string_lossy().into_owned())
        .collect();
    if !missing.is_empty() {
        return Err(format!("missing {}", missing.join(", ")));
    }
    // The completion thread waits with core 1.2's vkWaitSemaphores on a timeline semaphore; wgpu enables the
    // timeline feature whenever the physical device has it.
    let api = capabilities
        .properties()
        .api_version
        .min(adapter.shared_instance().instance_api_version());
    if api < vk::API_VERSION_1_2 {
        return Err(format!(
            "Vulkan {}.{} (timeline semaphores need 1.2)",
            vk::api_version_major(api),
            vk::api_version_minor(api)
        ));
    }
    let mut features12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut features = vk::PhysicalDeviceFeatures2::default().push_next(&mut features12);
    // SAFETY: the physical device is the adapter's and the instance supports Vulkan 1.2 (checked above), so
    // vkGetPhysicalDeviceFeatures2 exists and accepts the 1.2 feature struct.
    unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_features2(adapter.raw_physical_device(), &mut features);
    }
    if features12.timeline_semaphore != vk::TRUE {
        return Err("no timeline semaphores".to_owned());
    }
    Ok(())
}

/// Publishes what `device` (opened from `adapter`) imports, as `gpui::dmabuf_import_caps`.
pub(crate) fn record_caps(adapter: &wgpu::Adapter, device: &wgpu::Device) {
    let name = adapter.get_info().name;
    let caps = match import_caps(adapter, device) {
        Ok(caps) => caps,
        Err(reason) => DmaBufImportCaps {
            adapter: name,
            render_node: None,
            formats: Vec::new(),
            unsupported: Some(format!("{reason:#}")),
        },
    };
    match &caps.unsupported {
        None => log::info!(
            "dmabuf import on {} ({}): {}",
            caps.adapter,
            caps.render_node
                .as_ref()
                .map_or_else(|| "no render node".to_owned(), |node| node.display().to_string()),
            caps.formats
                .iter()
                .map(|format| format!("{} x{}", fourcc_name(format.fourcc), format.modifiers.len()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Some(reason) => log::info!("no dmabuf import on {}: {reason}", caps.adapter),
    }
    gpui::set_dmabuf_import_caps(caps);
}

fn import_caps(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Result<DmaBufImportCaps> {
    let info = adapter.get_info();
    ensure!(
        info.backend == wgpu::Backend::Vulkan,
        "the GPU renders through {:?}, not Vulkan",
        info.backend
    );
    {
        // SAFETY: the guard is only read and dropped at the end of this block.
        let hal_device = unsafe { device.as_hal::<vulkan::Api>() }.context("not a Vulkan device")?;
        let enabled = hal_device.enabled_device_extensions();
        let missing: Vec<_> = EXTENSIONS.iter().filter(|extension| !enabled.contains(extension)).collect();
        ensure!(
            missing.is_empty(),
            "the device was opened without {}",
            missing
                .iter()
                .map(|extension| extension.to_string_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // SAFETY: the guard is only read and dropped at the end of this function.
    let hal_adapter = unsafe { adapter.as_hal::<vulkan::Api>() }.context("not a Vulkan adapter")?;
    let formats: Vec<DmaBufFormat> = FORMATS
        .iter()
        .filter_map(|format| {
            let modifiers = importable_modifiers(&hal_adapter, format.vk);
            (!modifiers.is_empty()).then(|| DmaBufFormat {
                fourcc: format.fourcc,
                modifiers,
            })
        })
        .collect();
    let unsupported = formats
        .is_empty()
        .then(|| "the device imports none of AR24, XR24, AB24, XB24 as a copy source".to_owned());
    Ok(DmaBufImportCaps {
        adapter: info.name,
        render_node: render_node(&hal_adapter),
        formats,
        unsupported,
    })
}

/// The modifiers of `format` the adapter copies from an imported dmabuf, in the driver's order.
fn importable_modifiers(adapter: &vulkan::Adapter, format: vk::Format) -> Vec<DmaBufModifier> {
    let instance = adapter.shared_instance().raw_instance();
    let physical = adapter.raw_physical_device();
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default();
    // SAFETY: Vulkan 1.2 and VK_EXT_image_drm_format_modifier are present (`missing_requirements`); the first call
    // only fills the count, the list pointing nowhere.
    unsafe {
        instance.get_physical_device_format_properties2(
            physical,
            format,
            &mut vk::FormatProperties2::default().push_next(&mut list),
        );
    }
    let mut properties = vec![vk::DrmFormatModifierPropertiesEXT::default(); list.drm_format_modifier_count as usize];
    let mut list = vk::DrmFormatModifierPropertiesListEXT::default().drm_format_modifier_properties(&mut properties);
    // SAFETY: as above; the list now points at `properties`, which holds the count the first call returned.
    unsafe {
        instance.get_physical_device_format_properties2(
            physical,
            format,
            &mut vk::FormatProperties2::default().push_next(&mut list),
        );
    }
    let count = list.drm_format_modifier_count as usize;
    properties.truncate(count);
    properties
        .iter()
        .filter(|properties| {
            properties
                .drm_format_modifier_tiling_features
                .contains(vk::FormatFeatureFlags::TRANSFER_SRC)
                && imports(adapter, format, properties.drm_format_modifier)
        })
        .map(|properties| DmaBufModifier {
            modifier: properties.drm_format_modifier,
            plane_count: properties.drm_format_modifier_plane_count,
        })
        .collect()
}

/// Whether a dmabuf of `format` with `modifier` imports as a 2D image to copy from.
fn imports(adapter: &vulkan::Adapter, format: vk::Format, modifier: u64) -> bool {
    let mut external_info =
        vk::PhysicalDeviceExternalImageFormatInfo::default().handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
    let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
        .drm_format_modifier(modifier)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    let info = vk::PhysicalDeviceImageFormatInfo2::default()
        .format(format)
        .ty(vk::ImageType::TYPE_2D)
        .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
        .usage(vk::ImageUsageFlags::TRANSFER_SRC)
        .push_next(&mut external_info)
        .push_next(&mut modifier_info);
    let mut external = vk::ExternalImageFormatProperties::default();
    let mut properties = vk::ImageFormatProperties2::default().push_next(&mut external);
    // SAFETY: Vulkan 1.2 and the external memory and modifier extensions are present (`missing_requirements`); every
    // struct of both chains lives until the call returns.
    let result = unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_image_format_properties2(adapter.raw_physical_device(), &info, &mut properties)
    };
    result.is_ok()
        && external
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
}

/// The adapter's DRM render node, from `VK_EXT_physical_device_drm`.
fn render_node(adapter: &vulkan::Adapter) -> Option<PathBuf> {
    if !adapter
        .physical_device_capabilities()
        .supports_extension(ash::ext::physical_device_drm::NAME)
    {
        return None;
    }
    let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
    let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
    // SAFETY: the physical device supports VK_EXT_physical_device_drm (checked above), so the instance (Vulkan 1.2)
    // fills its properties struct.
    unsafe {
        adapter
            .shared_instance()
            .raw_instance()
            .get_physical_device_properties2(adapter.raw_physical_device(), &mut properties);
    }
    if drm.has_render != vk::TRUE {
        return None;
    }
    find_render_node(drm.render_major, drm.render_minor)
}

/// The `/dev/dri` node with device number `major:minor`.
fn find_render_node(major: i64, minor: i64) -> Option<PathBuf> {
    let wanted = makedev(u64::try_from(major).ok()?, u64::try_from(minor).ok()?);
    std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.rdev() == wanted))
}

/// glibc's `makedev`.
fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xffff_f000) << 32) | ((major & 0x0000_0fff) << 8) | ((minor & 0xffff_ff00) << 12) | (minor & 0x0000_00ff)
}
