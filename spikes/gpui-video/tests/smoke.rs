//! M0 Task 15 smoke test: the `gpui-video` window opens on niri, renders text and an image, and closes, within
//! deadlines and without an error in GPUI's log, on every GPU of the machine.
//!
//! Nothing reaches the desktop the tests run in. Each sandbox starts a private headless weston (GL renderer,
//! `--fake-seat`: GPUI needs a `wl_seat`) and a niri nested in it, each with its own `XDG_RUNTIME_DIR` in a temp dir.
//! Every child's environment is built from scratch: no `WAYLAND_DISPLAY`, `DISPLAY`, `NIRI_SOCKET` or D-Bus address of
//! the session leaks in, the smoke binary gets the nested niri's socket as an absolute `WAYLAND_DISPLAY`, and
//! `niri msg` always gets the nested niri's `NIRI_SOCKET`.
//!
//! Without a weston that takes `--fake-seat` (15.0 or later) or a niri on `PATH`, the compositor tests say so on
//! stderr and pass, unless `GPUI_SMOKE_REQUIRE=1`, which makes them fail instead.

use std::{
    ffi::{OsStr, OsString},
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

/// Helpers return errors; the tests unwrap them.
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const SMOKE: &str = env!("CARGO_BIN_EXE_gpui-video");
/// The smoke window's Wayland app id.
const APP_ID: &str = "ms-teams-gpui-smoke";
/// From launch to the ready line (a cold dGPU wakes up within it).
const STARTUP: Duration = Duration::from_secs(30);
/// From the compositor's close request to the process exit.
const CLOSE: Duration = Duration::from_secs(5);
/// How long a compositor gets to create its sockets, and niri to write a screenshot.
const SOCKET_WAIT: Duration = Duration::from_secs(15);

const WESTON_CONFIG: &str =
    "[core]\nidle-time=0\n\n[shell]\npanel-position=none\nlocking=false\n\n[input-method]\npath=\n";
/// A private niri: no Xwayland, no hotkey overlay, no animations.
const NIRI_CONFIG: &str = "hotkey-overlay {\n    skip-at-startup\n}\nxwayland-satellite {\n    off\n}\n\
animations {\n    off\n}\nprefer-no-csd\n";

/// A command whose environment holds only `PATH`, `HOME` and `LANG` of this process.
fn clean_command(program: impl AsRef<OsStr>) -> Command {
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

fn wait_until<T>(deadline: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
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

fn private_dir(path: PathBuf) -> TestResult<PathBuf> {
    fs::create_dir_all(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// A private headless weston with a niri nested in it; both are killed on drop.
struct Sandbox {
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
    fn compositors() -> Result<(PathBuf, PathBuf), String> {
        let weston = seated_weston().ok_or("no weston with --fake-seat on PATH (weston 15.0 or later)")?;
        let niri = on_path("niri").ok_or("no niri on PATH")?;
        Ok((weston, niri))
    }

    fn start(weston: &Path, niri: &Path) -> TestResult<Self> {
        let dir = TempDir::new()?;
        let niri_run = private_dir(dir.path().join("niri-run"))?;
        let mut sandbox = Self {
            dir,
            helpers: Vec::new(),
            niri_run,
            wayland: PathBuf::new(),
            niri_socket: PathBuf::new(),
        };
        let parent = sandbox.start_weston(weston)?;
        sandbox.start_niri(niri, &parent)?;
        Ok(sandbox)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn with_mesa_egl(command: &mut Command) {
        if let Some(vendor) = mesa_egl_vendor() {
            command.env("__EGL_VENDOR_LIBRARY_FILENAMES", vendor);
        }
    }

    fn start_weston(&mut self, weston: &Path) -> TestResult<PathBuf> {
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
            .args(["--width=1280", "--height=800"])
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

    fn start_niri(&mut self, niri: &Path, parent: &Path) -> TestResult {
        let config = self.path("niri.kdl");
        fs::write(&config, NIRI_CONFIG)?;
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
    fn logs(&self) -> String {
        let mut logs = String::new();
        for name in ["weston.log", "weston.stderr", "niri.log"] {
            let text = fs::read_to_string(self.path(name)).unwrap_or_default();
            let _ = write!(logs, "\n--- {name}\n{text}");
        }
        logs
    }

    /// `niri msg` against the nested niri only; returns its stdout.
    fn niri_msg(&self, args: &[&str]) -> TestResult<String> {
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

    /// The smoke binary on the nested niri, with `extra_env` on top of its clean environment.
    fn spawn_smoke(&self, extra_env: &[(&str, &str)]) -> TestResult<Child> {
        let mut command = clean_command(SMOKE);
        command
            .env("XDG_RUNTIME_DIR", &self.niri_run)
            .env("WAYLAND_DISPLAY", &self.wayland)
            .envs(extra_env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Ok(command.spawn()?)
    }
}

/// The sandbox, or `None` (with a note) when the compositors can't run here and `GPUI_SMOKE_REQUIRE` isn't set.
fn sandbox_or_skip(test: &str) -> TestResult<Option<Sandbox>> {
    match Sandbox::compositors() {
        Ok((weston, niri)) => Ok(Some(Sandbox::start(&weston, &niri)?)),
        Err(reason) if std::env::var_os("GPUI_SMOKE_REQUIRE").is_some_and(|value| value == "1") => {
            Err(format!("{test}: {reason}, and GPUI_SMOKE_REQUIRE=1").into())
        }
        Err(reason) => {
            eprintln!("skipping {test}: {reason}");
            Ok(None)
        }
    }
}

/// What the ready line says.
#[derive(Debug)]
struct Ready {
    compositor: String,
    gpu: String,
    software: bool,
}

fn parse_ready(line: &str) -> Option<Ready> {
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

/// A running smoke binary: its stdout lines as they come, its stderr once it exits. Killed on drop.
struct Running {
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

impl Running {
    fn new(mut child: Child) -> TestResult<Self> {
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

    /// The ready line; an early exit is told apart from a timeout. Errors carry no stderr: see [`Running::fail`].
    fn ready(&self) -> TestResult<Ready> {
        let line = self.lines.recv_timeout(STARTUP).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => format!("no ready line within {STARTUP:?}"),
            mpsc::RecvTimeoutError::Disconnected => "stdout closed before the ready line".to_owned(),
        })?;
        Ok(parse_ready(&line).ok_or_else(|| format!("unexpected stdout line: {line}"))?)
    }

    /// Everything the binary wrote to stderr; it must have exited or been killed.
    fn take_stderr(&mut self) -> String {
        match self.stderr.take().map(thread::JoinHandle::join) {
            Some(Ok(text)) => text,
            Some(Err(_)) => "<stderr reader panicked>".to_owned(),
            None => "<stderr already taken>".to_owned(),
        }
    }

    /// Kills the binary and returns `error` with its exit status and stderr attached.
    fn fail(&mut self, error: &dyn std::fmt::Display) -> Box<dyn std::error::Error> {
        let _ = self.child.kill();
        let status = self
            .child
            .wait()
            .map_or_else(|wait| format!("wait failed: {wait}"), |status| status.to_string());
        let stderr = self.take_stderr();
        format!("{error} (gpui-video: {status})\n--- gpui-video stderr\n{stderr}").into()
    }

    /// Waits up to `deadline` for the exit; returns the status and stderr.
    fn finish(mut self, deadline: Duration) -> TestResult<(ExitStatus, String)> {
        let Some(status) = wait_until(deadline, || self.child.try_wait().ok().flatten()) else {
            return Err(self.fail(&format!("still running {deadline:?} after the close request")));
        };
        Ok((status, self.take_stderr()))
    }
}

/// Counts of the smoke window's colours in a screenshot: magenta and cyan tiles, and white text.
fn colour_counts(png: &Path) -> TestResult<[usize; 3]> {
    let shot = image::open(png)?.to_rgb8();
    let near = |pixel: &image::Rgb<u8>, target: [u8; 3]| {
        pixel
            .0
            .iter()
            .zip(target)
            .all(|(&channel, wanted)| channel.abs_diff(wanted) <= 8)
    };
    let mut counts = [0; 3];
    for pixel in shot.pixels() {
        for (count, target) in counts.iter_mut().zip([[255, 0, 255], [0, 255, 255], [255, 255, 255]]) {
            *count += usize::from(near(pixel, target));
        }
    }
    Ok(counts)
}

/// Screenshots the nested niri into `shot` and counts the smoke colours in it.
fn screenshot_colours(sandbox: &Sandbox, shot: &str) -> TestResult<[usize; 3]> {
    let png = sandbox.path(shot);
    let png_arg = png.to_string_lossy().into_owned();
    sandbox.niri_msg(&[
        "action",
        "screenshot-screen",
        "--show-pointer",
        "false",
        "--path",
        &png_arg,
    ])?;
    wait_until(SOCKET_WAIT, || image::open(&png).ok().map(|_| ()))
        .ok_or_else(|| format!("no screenshot at {}", png.display()))?;
    colour_counts(&png)
}

/// Waits for the ready line, screenshots the screen, lists the windows and asks niri to close the focused one.
fn show_and_close(sandbox: &Sandbox, running: &Running, shot: &str) -> TestResult<(Ready, [usize; 3], String)> {
    let ready = running.ready()?;
    let colours = screenshot_colours(sandbox, shot)?;
    let windows = sandbox.niri_msg(&["windows"])?;
    sandbox.niri_msg(&["action", "close-window"])?;
    Ok((ready, colours, windows))
}

/// Runs the smoke binary on the sandbox's niri: ready, screenshot with text and image, listed by niri, closed by
/// niri, clean exit without an error logged. Returns what the ready line said and the binary's stderr. A step that
/// fails kills the binary and reports its stderr.
fn smoke(sandbox: &Sandbox, extra_env: &[(&str, &str)], shot: &str) -> TestResult<(Ready, String)> {
    let mut running = Running::new(sandbox.spawn_smoke(extra_env)?)?;
    let (ready, [magenta, cyan, white], windows) =
        show_and_close(sandbox, &running, shot).map_err(|error| running.fail(&error))?;
    let (status, stderr) = running.finish(CLOSE)?;
    eprintln!("{ready:?}\n{stderr}");
    assert!(windows.contains(APP_ID), "niri does not list the window: {windows}");
    assert!(status.success(), "exit {status}: {stderr}");
    let errors: Vec<_> = stderr.lines().filter(|line| line.starts_with("[ERROR")).collect();
    assert!(errors.is_empty(), "errors logged: {errors:#?}");
    assert!(
        magenta > 2000 && cyan > 2000,
        "image not on screen: magenta {magenta}, cyan {cyan}"
    );
    assert!(white > 50, "text not on screen: {white} white pixels");
    Ok((ready, stderr))
}

#[test]
fn window_opens_renders_and_closes_on_niri() {
    let Some(sandbox) = sandbox_or_skip("window_opens_renders_and_closes_on_niri").unwrap() else {
        return;
    };
    let (ready, _) = smoke(&sandbox, &[], "default.png").unwrap();
    assert_eq!(ready.compositor, "Wayland");
    assert!(!ready.software, "rendered in software: {ready:?}");
}

/// A GPU of this machine: its PCI vendor and device ids from sysfs, as `0x`-prefixed hex.
struct Gpu {
    vendor: String,
    device: String,
}

fn render_node_gpus() -> Vec<Gpu> {
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

fn parse_hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// One entry of GPUI's `Found N GPU adapter(s):` listing (`gpui_wgpu`, logged at info).
#[derive(Debug, PartialEq)]
struct Adapter {
    name: String,
    vendor: u32,
    device: u32,
    backend: String,
}

/// `[INFO target]   - <name> (vendor=0x8086, device=0xa7a0, backend=Vulkan, type=IntegratedGpu)`.
fn parse_adapter(line: &str) -> Option<Adapter> {
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
fn parse_selected(line: &str) -> Option<(&str, &str)> {
    let (_, chosen) = line.split_once("] Selected GPU (passed configuration test): ")?;
    let (name, backend) = chosen.rsplit_once(" (")?;
    Some((name, backend.strip_suffix(')')?))
}

/// Checks from GPUI's stderr that the adapter it rendered on is the PCI device `gpu` asked for with
/// `ZED_DEVICE_ID`: an adapter with that vendor and device id is listed, the first adapter tried passed (no
/// `trying next`), the selected adapter is one of the listed ones with that id, and the ready line names it.
/// `ZED_DEVICE_ID` only moves the matching adapter to the front, so any of these failing is a silent fallback.
fn check_selected_gpu(gpu: &Gpu, ready: &Ready, stderr: &str) -> Result<(), String> {
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

/// Every GPU in turn (the iGPU and the dGPU on a hybrid laptop), selected with GPUI's `ZED_DEVICE_ID`, which puts
/// that adapter first; GPUI's log must show it rendered on that PCI device, so a silent fallback to another GPU, of
/// the same vendor or not, fails.
#[test]
fn window_renders_on_every_gpu() {
    let Some(sandbox) = sandbox_or_skip("window_renders_on_every_gpu").unwrap() else {
        return;
    };
    let gpus = render_node_gpus();
    assert!(!gpus.is_empty(), "no render nodes in /sys/class/drm");
    for gpu in gpus {
        let shot = format!("gpu-{}.png", gpu.device);
        let (ready, stderr) = smoke(&sandbox, &[("ZED_DEVICE_ID", gpu.device.as_str())], &shot).unwrap();
        assert!(!ready.software, "{}: rendered in software: {ready:?}", gpu.device);
        if let Err(error) = check_selected_gpu(&gpu, &ready, &stderr) {
            panic!("{}: {error}\n--- gpui-video stderr\n{stderr}", gpu.device);
        }
    }
}

/// A GPUI adapter log for `check_selected_gpu`: an Intel iGPU and an Intel dGPU, `selected` chosen.
fn adapter_log(selected: &str) -> String {
    format!(
        "[INFO gpui_wgpu::wgpu_context] ZED_DEVICE_ID filter: 0xa7a0\n\
         [INFO gpui_wgpu::wgpu_context] Found 2 GPU adapter(s):\n\
         [INFO gpui_wgpu::wgpu_context]   - Intel(R) Iris(R) Xe Graphics (RPL-P) (vendor=0x8086, device=0xa7a0, \
         backend=Vulkan, type=IntegratedGpu)\n\
         [INFO gpui_wgpu::wgpu_context]   - Intel(R) Arc(tm) A770 Graphics (DG2) (vendor=0x8086, device=0x56a0, \
         backend=Vulkan, type=DiscreteGpu)\n\
         [INFO gpui_wgpu::wgpu_context] Selected GPU (passed configuration test): {selected} (Vulkan)\n"
    )
}

#[test]
fn selected_gpu_is_checked_by_pci_id() {
    let igpu = Gpu {
        vendor: "0x8086".to_owned(),
        device: "0xa7a0".to_owned(),
    };
    let ready = |gpu: &str| Ready {
        compositor: "Wayland".to_owned(),
        gpu: gpu.to_owned(),
        software: false,
    };
    let iris = "Intel(R) Iris(R) Xe Graphics (RPL-P)";
    let arc = "Intel(R) Arc(tm) A770 Graphics (DG2)";
    check_selected_gpu(&igpu, &ready(iris), &adapter_log(iris)).unwrap();
    // The other GPU of the same vendor rendered.
    let error = check_selected_gpu(&igpu, &ready(arc), &adapter_log(arc)).unwrap_err();
    assert!(error.contains("not one of"), "{error}");
    let fallback = adapter_log(arc).replace(
        "[INFO gpui_wgpu::wgpu_context] Selected",
        "[INFO gpui_wgpu::wgpu_context]   Adapter x (Vulkan) failed: boom, trying next...\n\
         [INFO gpui_wgpu::wgpu_context] Selected",
    );
    let error = check_selected_gpu(&igpu, &ready(arc), &fallback).unwrap_err();
    assert!(error.contains("fell back"), "{error}");
    // The requested id matches no adapter (its driver is missing, or a vendor nobody mapped).
    let virtio = Gpu {
        vendor: "0x1af4".to_owned(),
        device: "0x1050".to_owned(),
    };
    let error = check_selected_gpu(&virtio, &ready(iris), &adapter_log(iris)).unwrap_err();
    assert!(error.contains("no adapter 0x1af4:0x1050"), "{error}");
    // The ready line names another adapter than the selected one.
    let error = check_selected_gpu(&igpu, &ready(arc), &adapter_log(iris)).unwrap_err();
    assert!(error.contains("ready line"), "{error}");
    let error = check_selected_gpu(&igpu, &ready(iris), "").unwrap_err();
    assert!(error.contains("no adapter"), "{error}");
}

/// With no Wayland compositor named (and an X11 display that isn't there), the binary refuses to start instead of
/// falling back to X11 or to running headless.
#[test]
fn refuses_to_run_without_a_wayland_display() {
    let dir = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let output = clean_command(SMOKE)
        .env("XDG_RUNTIME_DIR", &run)
        .env("DISPLAY", ":4242")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "started without a compositor: {stderr}");
    assert!(stderr.contains("no Wayland compositor"), "{stderr}");
    assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
}

/// A `WAYLAND_DISPLAY` naming no socket is fatal: no fallback to X11 or headless, and nothing on stdout.
#[test]
fn fails_on_a_wayland_display_with_no_compositor() {
    let dir = TempDir::new().unwrap();
    let run = private_dir(dir.path().join("run")).unwrap();
    let missing: OsString = run.join("wayland-missing").into();
    let mut child = clean_command(SMOKE)
        .env("XDG_RUNTIME_DIR", &run)
        .env("WAYLAND_DISPLAY", missing)
        .env("DISPLAY", ":4242")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait_until(STARTUP, || child.try_wait().ok().flatten());
    let output = if status.is_some() {
        child.wait_with_output().unwrap()
    } else {
        child.kill().unwrap();
        panic!("still running {STARTUP:?} after a start without a compositor");
    };
    assert!(!output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(output.stdout.is_empty(), "{}", String::from_utf8_lossy(&output.stdout));
}

/// A binary that dies before its ready line is reported at once as an early exit, with its stderr and status.
#[test]
fn early_exit_is_reported_with_stderr() {
    let child = clean_command("sh")
        .args(["-c", "echo 'adapter exploded' >&2; exit 3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut running = Running::new(child).unwrap();
    let start = Instant::now();
    let error = running.ready().unwrap_err();
    assert!(
        start.elapsed() < STARTUP / 2,
        "waited {:?} for a dead process",
        start.elapsed()
    );
    let error = running.fail(&error).to_string();
    assert!(error.contains("stdout closed before the ready line"), "{error}");
    assert!(error.contains("adapter exploded"), "{error}");
    assert!(error.contains("exit status: 3"), "{error}");
}

#[test]
fn ready_line_parses() {
    let line = "ready compositor=Wayland gpu=\"Intel(R) Graphics (RPL-P)\" driver=\"Intel open-source Mesa driver\" \
                software=false";
    let ready = parse_ready(line).unwrap();
    assert_eq!(ready.compositor, "Wayland");
    assert_eq!(ready.gpu, "Intel(R) Graphics (RPL-P)");
    assert!(!ready.software);
    assert!(
        parse_ready("ready compositor=Wayland gpu=\"x\" driver=\"y\" software=true")
            .unwrap()
            .software
    );
    assert!(parse_ready("hello").is_none());
}
