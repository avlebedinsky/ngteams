//! Which PipeWire server a build may open, fixed when it is built: `test-support` always wins (this test build has
//! both `test-support` and `personal` on, and still opens only the UI sandbox's server); the lab (no feature) opens
//! only the sandbox's; only `personal` alone opens the desktop's. The sandbox's server is accepted only at its fixed
//! place inside the sandbox dir, never through a link out of it, never the desktop's PipeWire or Pulse sockets, never
//! with the session overriding the sandbox's client config, and nothing here connects to any socket.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

use teams_media::devices::DeviceError;
use teams_media::devices::pipewire::{
    CONFIG_OVERRIDES, Endpoint, PipeWireDevices, Reach, SANDBOX_CLIENT_CONF, SANDBOX_ENV, SANDBOX_SOCKET, SessionEnv,
    THIS_BUILD, endpoint, reach_for,
};

/// This process's user id, as the files it makes carry it.
fn uid() -> u32 {
    tempfile::tempfile().unwrap().metadata().unwrap().uid()
}

/// A listening socket nobody should connect to, and whether anybody did.
struct Canary(UnixListener);

impl Canary {
    fn plant(path: &Path) -> Self {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        Self(listener)
    }

    fn untouched(&self) -> bool {
        self.0.accept().is_err()
    }
}

/// A sandbox dir holding a socket at the sandbox's place and its client config.
fn sandbox() -> (tempfile::TempDir, Canary) {
    let dir = tempfile::tempdir().unwrap();
    let canary = Canary::plant(&dir.path().join(SANDBOX_SOCKET));
    let conf = dir.path().join(SANDBOX_CLIENT_CONF);
    fs::create_dir_all(conf.parent().unwrap()).unwrap();
    fs::write(&conf, "context.modules = [ ]\n").unwrap();
    (dir, canary)
}

fn sandbox_env(dir: &Path) -> SessionEnv {
    SessionEnv::from_vars([(SANDBOX_ENV, dir.as_os_str())], uid())
}

fn refusal(result: Result<Endpoint, DeviceError>) -> String {
    match result {
        Err(DeviceError::Refused(reason)) => reason,
        other => panic!("not refused: {other:?}"),
    }
}

#[test]
fn test_support_always_wins_and_only_personal_alone_reaches_the_desktop() {
    assert_eq!(reach_for(false, false), Reach::Sandbox, "the lab");
    assert_eq!(reach_for(true, false), Reach::Sandbox);
    assert_eq!(reach_for(true, true), Reach::Sandbox, "test-support wins");
    assert_eq!(reach_for(false, true), Reach::Desktop, "the personal build");
    // this test build has both on
    assert_eq!(THIS_BUILD, Reach::Sandbox);
}

#[test]
fn a_build_with_both_features_refuses_the_session_s_pipewire() {
    if std::env::var_os(SANDBOX_ENV).is_some() {
        eprintln!("skipping: this test process runs inside a UI sandbox");
        return;
    }
    // this process's own session: no sandbox, so nothing is opened (the refusal comes before any connect)
    let error = PipeWireDevices::connect().unwrap_err();
    assert!(
        matches!(&error, DeviceError::Refused(reason) if reason.contains(SANDBOX_ENV)),
        "{error}"
    );
}

#[test]
fn the_sandbox_s_server_is_taken_at_its_place_with_its_client_config() {
    let (dir, canary) = sandbox();
    let found = endpoint(Reach::Sandbox, &sandbox_env(dir.path())).unwrap();
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(found.socket, root.join(SANDBOX_SOCKET));
    assert_eq!(found.client_conf, Some(root.join(SANDBOX_CLIENT_CONF)));
    assert!(canary.untouched());
}

#[test]
fn without_a_sandbox_the_desktop_s_sockets_are_refused() {
    let desktop = tempfile::tempdir().unwrap();
    let canaries = [
        Canary::plant(&desktop.path().join("pipewire-0")),
        Canary::plant(&desktop.path().join("pulse/native")),
    ];
    let env = SessionEnv::from_vars(
        [
            ("XDG_RUNTIME_DIR", desktop.path().as_os_str()),
            ("PIPEWIRE_RUNTIME_DIR", desktop.path().as_os_str()),
        ],
        uid(),
    );
    assert!(refusal(endpoint(Reach::Sandbox, &env)).contains(SANDBOX_ENV));
    for relative in ["relative/dir", "/tmp/../tmp"] {
        let env = SessionEnv::from_vars([(SANDBOX_ENV, relative)], uid());
        assert!(refusal(endpoint(Reach::Sandbox, &env)).contains("absolute"));
    }
    assert!(canaries.iter().all(Canary::untouched));
}

#[test]
fn a_sandbox_dir_holding_the_desktop_s_runtime_dirs_is_refused() {
    let mut dirs = vec![PathBuf::from("/"), PathBuf::from("/run")];
    let session = PathBuf::from(format!("/run/user/{}", uid()));
    if session.is_dir() {
        dirs.push(session);
    }
    for dir in dirs {
        let reason = refusal(endpoint(Reach::Sandbox, &sandbox_env(&dir)));
        assert!(reason.contains("desktop's runtime dir"), "{}: {reason}", dir.display());
    }
}

#[test]
fn a_socket_leading_out_of_the_sandbox_is_refused() {
    // the desktop's runtime dir, stood in for by another dir with a server in it
    let desktop = tempfile::tempdir().unwrap();
    let desktop_pipewire = Canary::plant(&desktop.path().join("pipewire-0"));
    let desktop_pulse = Canary::plant(&desktop.path().join("pulse/native"));

    // the sandbox's PipeWire dir is a link to the desktop's runtime dir
    let (linked_dir, _) = sandbox();
    let pipewire_dir = linked_dir.path().join("pipewire");
    fs::remove_dir_all(&pipewire_dir).unwrap();
    symlink(desktop.path(), &pipewire_dir).unwrap();
    assert!(refusal(endpoint(Reach::Sandbox, &sandbox_env(linked_dir.path()))).contains("leads out"));

    // the socket itself links to the desktop's Pulse socket
    let (linked_socket, _) = sandbox();
    let socket = linked_socket.path().join(SANDBOX_SOCKET);
    fs::remove_file(&socket).unwrap();
    symlink(desktop.path().join("pulse/native"), &socket).unwrap();
    assert!(refusal(endpoint(Reach::Sandbox, &sandbox_env(linked_socket.path()))).contains("leads out"));

    assert!(desktop_pipewire.untouched() && desktop_pulse.untouched());
}

#[test]
fn a_session_overriding_the_client_config_is_refused_in_the_sandbox() {
    let (dir, canary) = sandbox();
    for name in CONFIG_OVERRIDES {
        for value in ["client.conf", ""] {
            let env = SessionEnv::from_vars([(SANDBOX_ENV, dir.path().as_os_str()), (name, value.as_ref())], uid());
            let reason = refusal(endpoint(Reach::Sandbox, &env));
            assert!(reason.contains(name), "{name}={value:?}: {reason}");
        }
    }
    assert!(canary.untouched());
}

#[test]
fn a_sandbox_without_its_server_or_its_client_config_is_refused() {
    let empty = tempfile::tempdir().unwrap();
    assert!(refusal(endpoint(Reach::Sandbox, &sandbox_env(empty.path()))).contains("no PipeWire"));

    let not_a_socket = tempfile::tempdir().unwrap();
    let socket = not_a_socket.path().join(SANDBOX_SOCKET);
    fs::create_dir_all(socket.parent().unwrap()).unwrap();
    fs::write(&socket, "").unwrap();
    assert!(refusal(endpoint(Reach::Sandbox, &sandbox_env(not_a_socket.path()))).contains("isn't a socket"));

    let (no_conf, _) = sandbox();
    fs::remove_file(no_conf.path().join(SANDBOX_CLIENT_CONF)).unwrap();
    assert!(refusal(endpoint(Reach::Sandbox, &sandbox_env(no_conf.path()))).contains("client config"));
}

#[test]
fn the_personal_reach_takes_the_desktop_session_s_server_only() {
    let desktop = tempfile::tempdir().unwrap();
    let canary = Canary::plant(&desktop.path().join("pipewire-0"));
    let other = tempfile::tempdir().unwrap();
    let env = SessionEnv::from_vars(
        [
            ("PIPEWIRE_RUNTIME_DIR", desktop.path().as_os_str()),
            ("XDG_RUNTIME_DIR", other.path().as_os_str()),
        ],
        uid(),
    );
    let found = endpoint(Reach::Desktop, &env).unwrap();
    assert_eq!(found.socket, desktop.path().join("pipewire-0"));
    assert_eq!(found.client_conf, None, "the user's own config");

    let env = SessionEnv::from_vars([("XDG_RUNTIME_DIR", desktop.path().as_os_str())], uid());
    assert_eq!(
        endpoint(Reach::Desktop, &env).unwrap().socket,
        desktop.path().join("pipewire-0")
    );

    assert!(
        refusal(endpoint(
            Reach::Desktop,
            &SessionEnv::from_vars::<&str, &str>([], uid())
        ))
        .contains("no desktop")
    );
    let env = SessionEnv::from_vars([("XDG_RUNTIME_DIR", other.path().as_os_str())], uid());
    assert!(matches!(endpoint(Reach::Desktop, &env), Err(DeviceError::Io(_))));
    assert!(canary.untouched());
}
