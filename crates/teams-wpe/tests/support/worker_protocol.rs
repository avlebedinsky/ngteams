//! The worker protocol driven by hand, as the app drives it, off the user's session: the greeting, the start and the
//! ways a worker refuses to serve or ends.

use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::path::Path;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use rustix::net::{AddressFamily, SocketFlags, SocketType};
use teams_wpe::worker::message::{self, FromWorker, PROTOCOL, ToWorker};
use teams_wpe::worker::wire::{self, Limits, Packet};
use teams_wpe::worker::{WORKER_FD_ENV, WORKER_IMAGE_ENV};
use teams_wpe::{
    BufferFormat, DmaBufTarget, FrameMode, HostConfig, MODIFIER_LINEAR, SchemeToken, StartError, ViewId, fourcc,
};
use teams_wpe_client::host::Command as EngineCommand;

use super::WORKER;

/// The worker started as the app starts it, off the user's session: its end of a socket pair inherited and named in
/// `WORKER_FD_ENV`, a disposable home, and `extra` in its environment. Returns the app's end.
fn spawn(home: &Path, extra: &[(&str, &str)]) -> (Child, OwnedFd) {
    let (app, worker) =
        rustix::net::socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, SocketFlags::CLOEXEC, None).unwrap();
    let mut command = ui_sandbox::clean_command(WORKER);
    command
        .env(WORKER_FD_ENV, worker.as_raw_fd().to_string())
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home)
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .envs(extra.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    ui_sandbox::spawn::pass_only(&mut command, &[worker.as_fd()]);
    let child = command.spawn().unwrap();
    (child, app)
}

/// The worker's exit code and what it wrote to stderr, killing it if it does not end within the wait.
fn finish(mut child: Child) -> (Option<i32>, String) {
    let end = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= end {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("the worker did not end: {}", String::from_utf8_lossy(&output.stderr));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn receive(app: &OwnedFd) -> Option<FromWorker> {
    wire::recv(app.as_fd(), Limits::FROM_WORKER)
        .unwrap()
        .map(|packet| message::body(&packet).unwrap())
}

fn send(app: &OwnedFd, message: &ToWorker, blob: Option<Vec<u8>>) {
    wire::send(app.as_fd(), &message.packet(blob).unwrap()).unwrap();
}

fn broken(app: &OwnedFd) {
    let packet = Packet {
        body: b"synthetic broken message".to_vec(),
        ..Packet::default()
    };
    wire::send(app.as_fd(), &packet).unwrap();
}

fn greeted(child: &Child, app: &OwnedFd) {
    match receive(app) {
        Some(FromWorker::Hello { protocol, version, pid }) => {
            assert_eq!(protocol, PROTOCOL);
            assert_eq!(version, teams_wpe_client::worker::build_identity());
            assert_eq!(pid, child.id());
        }
        other => panic!("the worker must greet first, got {other:?}"),
    }
}

fn shm() -> HostConfig {
    HostConfig {
        frames: FrameMode::Shm,
        schemes: Vec::new(),
        extensions: None,
    }
}

/// Starts the engine in a spawned worker and waits for it to say it started.
fn started(home: &Path) -> (Child, OwnedFd) {
    let (child, app) = spawn(home, &[]);
    greeted(&child, &app);
    send(&app, &ToWorker::Start(shm()), None);
    match receive(&app) {
        Some(FromWorker::Started(Ok(()))) => {}
        other => panic!("the engine must start, got {other:?}"),
    }
    (child, app)
}

/// The answer to evaluation `id`, skipping the engine's events.
fn evaluation(app: &OwnedFd, id: u64) -> teams_wpe::Evaluation {
    loop {
        match receive(app) {
            Some(FromWorker::Evaluation { id: answered, result }) if answered == id => return result,
            Some(_) => {}
            None => panic!("the worker went away before answering evaluation {id}"),
        }
    }
}

#[test]
fn a_worker_descriptor_that_is_no_number_or_names_no_socket_is_refused() {
    let output = ui_sandbox::clean_command(WORKER)
        .env(WORKER_FD_ENV, "synthetic")
        .env(WORKER_IMAGE_ENV, "synthetic")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not a descriptor number"));
    // stdin is no socket; the image's descriptor is closed whether or not it is open
    let output = ui_sandbox::clean_command(WORKER)
        .env(WORKER_FD_ENV, "0")
        .env(WORKER_IMAGE_ENV, "987")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("names no socket"));
}

#[test]
fn an_inherited_runtime_lock_that_is_no_descriptor_is_refused_before_the_worker_speaks() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = spawn(home.path(), &[(teams_wpe_client::RUNTIME_LOCK_ENV, "synthetic")]);
    let (code, stderr) = finish(child);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("invalid inherited web engine runtime lock"), "{stderr}");
    assert!(receive(&app).is_none());
}

#[test]
fn a_worker_greets_with_its_protocol_build_and_pid_and_ends_quietly_once_its_app_goes() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = spawn(home.path(), &[]);
    greeted(&child, &app);
    drop(app);
    assert_eq!(finish(child).0, Some(0));
}

#[test]
fn a_worker_whose_app_left_before_the_greeting_ends_quietly() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = spawn(home.path(), &[]);
    drop(app);
    assert_eq!(finish(child).0, Some(0));
}

#[test]
fn a_worker_given_anything_but_its_configuration_first_refuses_to_serve() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = spawn(home.path(), &[]);
    greeted(&child, &app);
    send(&app, &ToWorker::Command(EngineCommand::Quit), None);
    let (code, stderr) = finish(child);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("not given its configuration first"), "{stderr}");
    let (child, app) = spawn(home.path(), &[]);
    greeted(&child, &app);
    broken(&app);
    let (code, stderr) = finish(child);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("not given its configuration first"), "{stderr}");
}

/// The real worker pinned, as the app pins it at its start; once for this test program.
pub(super) fn pinned() {
    static PINNED: std::sync::Once = std::sync::Once::new();
    PINNED.call_once(|| teams_wpe_client::worker::pin_worker_path(Path::new(WORKER)).unwrap());
}

#[test]
fn a_render_node_that_is_missing_fails_the_workers_start_as_a_display_failure() {
    pinned();
    let config = HostConfig {
        frames: FrameMode::DmaBuf(DmaBufTarget {
            render_node: "/dev/dri/renderD9999".into(),
            formats: vec![BufferFormat {
                fourcc: fourcc(b"XR24"),
                modifiers: vec![MODIFIER_LINEAR],
            }],
        }),
        ..shm()
    };
    match teams_wpe::Host::start_worker(config) {
        Err(StartError::Display(message)) => assert!(!message.is_empty()),
        other => panic!("a missing render node is a display failure, got {other:?}"),
    }
}

#[test]
fn a_configuration_naming_an_invalid_scheme_is_refused_before_any_engine_starts() {
    let config = HostConfig {
        schemes: vec!["not a scheme".into()],
        ..shm()
    };
    assert!(matches!(
        teams_wpe::Host::start(config.clone()),
        Err(StartError::Config(_))
    ));
    assert!(matches!(
        teams_wpe::Host::start_worker(config),
        Err(StartError::Config(_))
    ));
}

#[test]
fn a_started_worker_answers_for_pages_it_lacks_ignores_a_second_start_and_ends_on_a_broken_message() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = started(home.path());
    let evaluate = |id| ToWorker::Evaluate {
        id,
        view: ViewId::from_raw(99),
        script: "1 + 1".into(),
        world: None,
    };
    send(&app, &evaluate(7), None);
    assert_eq!(evaluation(&app, 7), Err("no such page".to_owned()));
    send(&app, &ToWorker::Start(shm()), None);
    let bytes = ToWorker::SchemeBytes {
        token: SchemeToken(41),
        mime: "text/plain".into(),
    };
    send(&app, &bytes, Some(b"synthetic bytes for no request".to_vec()));
    send(&app, &evaluate(8), None);
    assert_eq!(evaluation(&app, 8), Err("no such page".to_owned()));
    broken(&app);
    let (code, stderr) = finish(child);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("got a broken message"), "{stderr}");
}

#[test]
fn a_started_worker_stops_its_engine_and_ends_quietly_once_its_app_goes() {
    let home = tempfile::tempdir().unwrap();
    let (child, app) = started(home.path());
    drop(app);
    assert_eq!(finish(child).0, Some(0));
}
