//! A private compositor for the GPUI tests and the benchmark: a headless weston with a niri nested in it, so nothing
//! reaches the desktop the tests run in.
//!
//! weston runs headless with the GL renderer and `--fake-seat` (GPUI needs a `wl_seat`); niri runs nested in it.
//! Each gets its own `XDG_RUNTIME_DIR` in a temp dir, and every child's environment is built from scratch: no
//! `WAYLAND_DISPLAY`, `DISPLAY`, `NIRI_SOCKET` or D-Bus address of the session leaks in. A client gets the nested
//! niri's socket as an absolute `WAYLAND_DISPLAY`, and `niri msg` always gets the nested niri's `NIRI_SOCKET`.
//!
//! Frame pacing: weston's headless output repaints on a timer at its refresh rate (60 Hz unless set), the nested niri
//! draws when weston's frame callback arrives, and GPUI draws when niri's frame callback arrives. Nothing scans out
//! to a display, so there's no vblank: the cadence is weston's timer.

use std::{
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::{BufRead as _, BufReader, Read as _},
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;

/// Errors of the harness: anything goes, with a readable message.
pub type SandboxResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How long a compositor gets to create its sockets, and niri to write a screenshot.
pub const SOCKET_WAIT: Duration = Duration::from_secs(15);

const WESTON_CONFIG: &str =
    "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n";
/// A private niri: no Xwayland, no hotkey overlay, no animations.
const NIRI_CONFIG: &str = "hotkey-overlay {\n    skip-at-startup\n}\nxwayland-satellite {\n    off\n}\n\
animations {\n    off\n}\nprefer-no-csd\n";
/// A niri layout where a window takes the whole output: no gaps, no focus ring, no border, full-width column.
pub const NIRI_FULL_WINDOW: &str = "layout {\n    gaps 0\n    default-column-width { proportion 1.0; }\n\
    focus-ring {\n        off\n    }\n    border {\n        off\n    }\n}\n";

/// The weston output and the niri config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxOptions {
    pub width: u32,
    pub height: u32,
    /// weston's headless refresh rate in mHz; `None` keeps weston's default (60 Hz).
    pub refresh_mhz: Option<u32>,
    /// Appended to the niri config.
    pub niri_extra: &'static str,
}

impl Default for SandboxOptions {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
            refresh_mhz: None,
            niri_extra: "",
        }
    }
}

/// A command whose environment holds only `PATH`, `HOME` and `LANG` of this process.
pub fn clean_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    for name in ["PATH", "HOME", "LANG"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
}

/// The first weston on `PATH` that takes `--fake-seat` (weston 15.0 and later).
fn seated_weston() -> Option<PathBuf> {
    let weston = on_path("weston")?;
    let help = clean_command(&weston).arg("--help").output().ok()?;
    String::from_utf8_lossy(&help.stdout)
        .contains("--fake-seat")
        .then_some(weston)
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name).extension().is_some_and(|actual| actual == extension)
}

fn file_name_matches(path: &Path, wanted: impl Fn(&str) -> bool) -> bool {
    path.file_name().is_some_and(|name| wanted(&name.to_string_lossy()))
}

/// Mesa's EGL vendor file, so weston and niri render through Mesa rather than another vendor's EGL.
fn mesa_egl_vendor() -> Option<PathBuf> {
    ["/usr/share/glvnd/egl_vendor.d", "/etc/glvnd/egl_vendor.d"]
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| file_name_matches(path, |name| name.contains("mesa") && has_extension(name, "json")))
}

/// Polls `probe` every 50 ms until it gives a value or `deadline` passes.
pub fn wait_until<T>(deadline: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if start.elapsed() > deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The first entry of `dir` whose name `wanted` accepts, once there is one.
fn wait_for_entry(dir: &Path, wanted: fn(&str) -> bool) -> Option<PathBuf> {
    wait_until(SOCKET_WAIT, || {
        fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| file_name_matches(path, wanted))
    })
}

/// Creates `path` (mode 0700) and returns it.
///
/// # Errors
/// The directory can't be made.
pub fn private_dir(path: PathBuf) -> SandboxResult<PathBuf> {
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// A private headless weston with a niri nested in it; both are killed on drop.
pub struct Sandbox {
    dir: TempDir,
    helpers: Vec<Child>,
    /// The nested niri's runtime dir, its Wayland socket and its IPC socket.
    niri_run: PathBuf,
    wayland: PathBuf,
    niri_socket: PathBuf,
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for child in self.helpers.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Sandbox {
    /// The compositors to run, or why they can't run here.
    ///
    /// # Errors
    /// No weston 15 or later, or no niri, on `PATH`.
    pub fn compositors() -> Result<(PathBuf, PathBuf), String> {
        let weston = seated_weston().ok_or("no weston with --fake-seat on PATH (weston 15.0 or later)")?;
        let niri = on_path("niri").ok_or("no niri on PATH")?;
        Ok((weston, niri))
    }

    /// Starts weston, then niri nested in it.
    ///
    /// # Errors
    /// A compositor fails to start or make its sockets in time.
    pub fn start(weston: &Path, niri: &Path, options: &SandboxOptions) -> SandboxResult<Self> {
        let dir = TempDir::new()?;
        let niri_run = private_dir(dir.path().join("niri-run"))?;
        let mut sandbox = Self {
            dir,
            helpers: Vec::new(),
            niri_run,
            wayland: PathBuf::new(),
            niri_socket: PathBuf::new(),
        };
        let parent = sandbox.start_weston(weston, options)?;
        sandbox.start_niri(niri, &parent, options)?;
        Ok(sandbox)
    }

    /// A path in the sandbox's temp dir.
    #[must_use]
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn with_mesa_egl(command: &mut Command) {
        if let Some(vendor) = mesa_egl_vendor() {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
        }
    }

    fn start_weston(&mut self, weston: &Path, options: &SandboxOptions) -> SandboxResult<PathBuf> {
        let run = private_dir(self.path("weston-run"))?;
        let config = self.path("weston.ini");
        fs::write(&config, WESTON_CONFIG)?;
        let mut command = clean_command(weston);
        command
            .args([
                "--backend=headless",
                "--renderer=gl",
                "--socket=wayland-0",
                "--fake-seat",
            ])
            .arg(format!("--width={}", options.width))
            .arg(format!("--height={}", options.height))
            .args(options.refresh_mhz.map(|rate| format!("--refresh-rate={rate}")))
            .arg(format!("--config={}", config.display()))
            .arg(format!("--log={}", self.path("weston.log").display()))
            .env("XDG_RUNTIME_DIR", &run)
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.path("weston.stderr"))?);
        Self::with_mesa_egl(&mut command);
        self.helpers.push(command.spawn()?);
        let socket = wait_for_entry(&run, |name| name == "wayland-0");
        Ok(socket.ok_or_else(|| format!("weston made no socket: {}", self.logs()))?)
    }

    fn start_niri(&mut self, niri: &Path, parent: &Path, options: &SandboxOptions) -> SandboxResult {
        let config = self.path("niri.kdl");
        fs::write(&config, format!("{NIRI_CONFIG}{}", options.niri_extra))?;
        let mut command = clean_command(niri);
        command
            .arg("-c")
            .arg(&config)
            .env("WAYLAND_DISPLAY", parent)
            .env("XDG_RUNTIME_DIR", &self.niri_run)
            .env("NIRI_DISABLE_SYSTEM_MANAGER_NOTIFY", "1")
            .stdout(Stdio::null())
            .stderr(fs::File::create(self.path("niri.log"))?);
        Self::with_mesa_egl(&mut command);
        self.helpers.push(command.spawn()?);
        let wayland = wait_for_entry(&self.niri_run, |name| {
            name.starts_with("wayland-") && !has_extension(name, "lock")
        });
        let ipc = wait_for_entry(&self.niri_run, |name| {
            name.starts_with("niri.") && has_extension(name, "sock")
        });
        let (Some(wayland), Some(ipc)) = (wayland, ipc) else {
            return Err(format!("nested niri made no sockets: {}", self.logs()).into());
        };
        self.wayland = wayland;
        self.niri_socket = ipc;
        Ok(())
    }

    /// The compositors' logs, for a failure message.
    #[must_use]
    pub fn logs(&self) -> String {
        let mut logs = String::new();
        for name in ["weston.log", "weston.stderr", "niri.log"] {
            let text = fs::read_to_string(self.path(name)).unwrap_or_default();
            let _ = write!(logs, "\n--- {name}\n{text}");
        }
        logs
    }

    /// `niri msg` against the nested niri only; returns its stdout.
    ///
    /// # Errors
    /// `niri msg` can't run or fails.
    pub fn niri_msg(&self, args: &[&str]) -> SandboxResult<String> {
        let output = clean_command("niri")
            .arg("msg")
            .args(args)
            .env("NIRI_SOCKET", &self.niri_socket)
            .env("XDG_RUNTIME_DIR", &self.niri_run)
            .output()?;
        if !output.status.success() {
            return Err(format!("niri msg {args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Screenshots the focused window (`window: true`) or the whole screen through the nested niri into `name` in
    /// the sandbox dir, and returns its path once the file is readable.
    ///
    /// # Errors
    /// niri refuses, or no image appears in time.
    pub fn screenshot(&self, name: &str, window: bool) -> SandboxResult<PathBuf> {
        let png = self.path(name);
        let png_arg = png.to_string_lossy().into_owned();
        let action = if window {
            "screenshot-window"
        } else {
            "screenshot-screen"
        };
        self.niri_msg(&["action", action, "--show-pointer", "false", "--path", &png_arg])?;
        wait_until(SOCKET_WAIT, || image::open(&png).ok().map(|_| ()))
            .ok_or_else(|| format!("no screenshot at {}", png.display()))?;
        Ok(png)
    }

    /// `program` with `args` as a client of the nested niri, with `extra_env` on top of its clean environment;
    /// stdout and stderr piped.
    ///
    /// # Errors
    /// The program can't be started.
    pub fn spawn(&self, program: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> SandboxResult<Child> {
        let mut command = clean_command(program);
        command
            .args(args)
            .env("XDG_RUNTIME_DIR", &self.niri_run)
            .env("WAYLAND_DISPLAY", &self.wayland)
            .envs(extra_env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command.spawn()?)
    }
}

/// The sandbox, or `None` (with a note) when the compositors can't run here and `GPUI_SMOKE_REQUIRE` isn't `1`.
///
/// # Errors
/// The compositors are missing and `GPUI_SMOKE_REQUIRE=1`, or they fail to start.
pub fn sandbox_or_skip(test: &str, options: &SandboxOptions) -> SandboxResult<Option<Sandbox>> {
    match Sandbox::compositors() {
        Ok((weston, niri)) => Ok(Some(Sandbox::start(&weston, &niri, options)?)),
        Err(reason) if std::env::var_os("GPUI_SMOKE_REQUIRE").is_some_and(|value| value == "1") => {
            Err(format!("{test}: {reason}, and GPUI_SMOKE_REQUIRE=1").into())
        }
        Err(reason) => {
            eprintln!("skipping {test}: {reason}");
            Ok(None)
        }
    }
}

/// A running client: its stdout lines as they come, its stderr once it exits. Killed on drop.
pub struct Running {
    child: Child,
    lines: mpsc::Receiver<String>,
    stderr: Option<thread::JoinHandle<String>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Why no stdout line came.
#[derive(Debug, PartialEq, Eq)]
pub enum LineError {
    Timeout,
    /// stdout closed: the process exited (or closed it).
    Closed,
}

impl Running {
    /// Starts reading `child`'s piped stdout and stderr.
    ///
    /// # Errors
    /// The child's stdout or stderr isn't piped.
    pub fn new(mut child: Child) -> SandboxResult<Self> {
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let mut stderr = child.stderr.take().ok_or("no stderr")?;
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let stderr = thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });
        Ok(Self {
            child,
            lines,
            stderr: Some(stderr),
        })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The next stdout line within `timeout`.
    ///
    /// # Errors
    /// None came in time, or stdout closed.
    pub fn line(&self, timeout: Duration) -> Result<String, LineError> {
        self.lines.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => LineError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => LineError::Closed,
        })
    }

    /// Everything the client wrote to stderr; it must have exited or been killed.
    pub fn take_stderr(&mut self) -> String {
        match self.stderr.take().map(thread::JoinHandle::join) {
            Some(Ok(text)) => text,
            Some(Err(_)) => "<stderr reader panicked>".to_owned(),
            None => "<stderr already taken>".to_owned(),
        }
    }

    /// Kills the client and returns `error` with its exit status and stderr attached.
    pub fn fail(&mut self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_or_else(|wait| format!("wait failed: {wait}"), |status| status.to_string());
        let stderr = self.take_stderr();
        format!("{error} (gpui-video: {status})\n--- gpui-video stderr\n{stderr}").into()
    }

    /// Waits up to `deadline` for the exit; returns the status and stderr.
    ///
    /// # Errors
    /// Still running at the deadline (it's killed, and the error carries its stderr).
    pub fn finish(mut self, deadline: Duration) -> SandboxResult<(ExitStatus, String)> {
        let Some(status) = wait_until(deadline, || self.child.try_wait().ok().flatten()) else {
            return Err(self.fail(&format!("still running {deadline:?} after the close request")));
        };
        Ok((status, self.take_stderr()))
    }
}

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
