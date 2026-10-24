//! The spike's sandbox: the UI sandbox ([`ui_sandbox`]: a headless weston with a niri nested in it, a private bus,
//! virtual input and a no-op URI opener), so nothing reaches the desktop the tests run in, plus what only this spike
//! reads: the GPUs of the machine, the binary's ready line and GPUI's adapter log.

use std::{fs, path::PathBuf, time::Duration};

pub use ui_sandbox::{
    LineError, NIRI_FULL_WINDOW, Running, SANDBOX_ENV, SOCKET_WAIT, Sandbox, SandboxOptions, SandboxResult,
    clean_command, private_dir, sandbox_or_skip, wait_until,
};

/// A GPU of this machine: its PCI vendor and device ids from sysfs, as `0x`-prefixed hex.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpu {
    pub vendor: String,
    pub device: String,
}

/// The GPUs behind `/sys/class/drm/renderD*`, one per device id.
#[must_use]
pub fn render_node_gpus() -> Vec<Gpu> {
    let read = |path: PathBuf| fs::read_to_string(path).ok().map(|text| text.trim().to_owned());
    let mut gpus: Vec<Gpu> = fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD"))
        .filter_map(|entry| {
            let device = entry.path().join("device");
            Some(Gpu {
                vendor: read(device.join("vendor"))?,
                device: read(device.join("device"))?,
            })
        })
        .collect();
    gpus.sort_by(|a, b| a.device.cmp(&b.device));
    gpus.dedup_by(|a, b| a.device == b.device);
    gpus
}

/// `0x`-prefixed hex.
#[must_use]
pub fn parse_hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// What the binary's ready line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub compositor: String,
    pub gpu: String,
    pub software: bool,
}

/// Parses `ready compositor=<name> gpu="<device>" driver="<driver>" software=<bool>`.
#[must_use]
pub fn parse_ready(line: &str) -> Option<Ready> {
    let rest = line.strip_prefix("ready compositor=")?;
    let (compositor, rest) = rest.split_once(" gpu=")?;
    let (gpu, rest) = rest.split_once(" driver=")?;
    let software = rest.rsplit_once(" software=")?.1 == "true";
    Some(Ready {
        compositor: compositor.to_owned(),
        gpu: gpu.trim_matches('"').to_owned(),
        software,
    })
}

/// The ready line within `timeout`; an early exit is told apart from a timeout. Errors carry no stderr: see
/// [`Running::fail`].
///
/// # Errors
/// No line in time, stdout closed first, or the first line isn't a ready line.
pub fn wait_ready(running: &Running, timeout: Duration) -> SandboxResult<Ready> {
    let line = running.line(timeout).map_err(|error| match error {
        LineError::Timeout => format!("no ready line within {timeout:?}"),
        LineError::Closed => "stdout closed before the ready line".to_owned(),
    })?;
    Ok(parse_ready(&line).ok_or_else(|| format!("unexpected stdout line: {line}"))?)
}

/// One entry of GPUI's `Found N GPU adapter(s):` listing (`gpui_wgpu`, logged at info).
#[derive(Debug, PartialEq, Eq)]
pub struct Adapter {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub backend: String,
}

/// `[INFO target]   - <name> (vendor=0x8086, device=0xa7a0, backend=Vulkan, type=IntegratedGpu)`.
#[must_use]
pub fn parse_adapter(line: &str) -> Option<Adapter> {
    let (_, entry) = line.split_once("]   - ")?;
    let (name, fields) = entry.rsplit_once(" (vendor=")?;
    let mut fields = fields.strip_suffix(')')?.split(", ");
    let vendor = parse_hex(fields.next()?)?;
    let device = parse_hex(fields.next()?.strip_prefix("device=")?)?;
    let backend = fields.next()?.strip_prefix("backend=")?;
    Some(Adapter {
        name: name.to_owned(),
        vendor,
        device,
        backend: backend.to_owned(),
    })
}

/// `[INFO target] Selected GPU (passed configuration test): <name> (<backend>)`, as (name, backend).
#[must_use]
pub fn parse_selected(line: &str) -> Option<(&str, &str)> {
    let (_, chosen) = line.split_once("] Selected GPU (passed configuration test): ")?;
    let (name, backend) = chosen.rsplit_once(" (")?;
    Some((name, backend.strip_suffix(')')?))
}

/// Checks from GPUI's stderr that the adapter it rendered on is the PCI device `gpu` asked for with
/// `ZED_DEVICE_ID`: an adapter with that vendor and device id is listed, the first adapter tried passed (no
/// `trying next`), the selected adapter is one of the listed ones with that id, and the ready line names it.
/// `ZED_DEVICE_ID` only moves the matching adapter to the front, so any of these failing is a silent fallback.
///
/// # Errors
/// The rendering adapter isn't the requested one; the message says why.
pub fn check_selected_gpu(gpu: &Gpu, ready: &Ready, stderr: &str) -> Result<(), String> {
    let (vendor, device) = parse_hex(&gpu.vendor)
        .zip(parse_hex(&gpu.device))
        .ok_or_else(|| format!("unreadable PCI ids {} {}", gpu.vendor, gpu.device))?;
    let adapters: Vec<_> = stderr.lines().filter_map(parse_adapter).collect();
    let wanted: Vec<_> = adapters
        .iter()
        .filter(|adapter| adapter.vendor == vendor && adapter.device == device)
        .collect();
    if wanted.is_empty() {
        return Err(format!(
            "no adapter {vendor:#06x}:{device:#06x} listed by GPUI: {adapters:#?}"
        ));
    }
    if let Some(line) = stderr.lines().find(|line| line.ends_with(", trying next...")) {
        return Err(format!("GPUI fell back from the requested adapter: {line}"));
    }
    let (name, backend) = stderr
        .lines()
        .find_map(parse_selected)
        .ok_or("GPUI logged no selected adapter")?;
    if !wanted
        .iter()
        .any(|adapter| adapter.name == name && adapter.backend == backend)
    {
        return Err(format!("selected {name} ({backend}), not one of {wanted:#?}"));
    }
    if ready.gpu != name {
        return Err(format!("selected {name}, but the ready line says {ready:?}"));
    }
    Ok(())
}
