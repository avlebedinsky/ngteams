//! Native profile paths, without reading any accounts or credentials.
#![cfg(test)]
#![cfg(windows)]

use std::path::{Path, PathBuf};
use teams_ctl::path::{Env, Mode, PathError, prepare_dir};
use teams_ctl::transport::pipe_name;

fn env() -> Env {
    Env {
        socket: None,
        runtime_dir: Some(r"C:\Users\u\AppData\Local\ngteams\runtime".into()),
        state_home: Some(r"C:\Users\u\AppData\Local".into()),
        home: Some(r"C:\Users\u".into()),
    }
}

#[test]
fn modes_and_user_profile_paths_stay_separate() {
    let env = env();
    let modes = [
        Mode::Personal,
        Mode::Lab("a".into()),
        Mode::Lab("b".into()),
        Mode::ReadOnly,
    ];
    let names: std::collections::HashSet<_> = modes
        .iter()
        .map(|mode| pipe_name(&env.mode_path(mode).unwrap()).unwrap())
        .collect();
    assert_eq!(names.len(), modes.len());
    assert_eq!(
        env.mode_path(&Mode::Lab("a".into())).unwrap(),
        PathBuf::from(r"C:\Users\u\AppData\Local\ngteams-lab\a\ctl.sock")
    );
    assert_eq!(
        Env {
            home: None,
            state_home: None,
            ..env.clone()
        }
        .mode_path(&Mode::ReadOnly),
        Err(PathError::NoStateHome)
    );
    assert_eq!(
        Env {
            runtime_dir: None,
            ..env.clone()
        }
        .mode_path(&Mode::Personal),
        Err(PathError::NoRuntimeDir)
    );
    let override_env = Env {
        socket: Some(r"C:\lab\override".into()),
        ..env.clone()
    };
    assert_eq!(
        override_env.socket_path(&Mode::ReadOnly).unwrap(),
        Path::new(r"C:\lab\override")
    );
    assert_eq!(override_env.mode_path(&Mode::ReadOnly), env.mode_path(&Mode::ReadOnly));
    for bad in ["", "..", "a/b", "a b", "ä", "a.b"] {
        assert_eq!(
            env.mode_path(&Mode::Lab(bad.into())),
            Err(PathError::BadAlias(bad.into()))
        );
    }
    assert_eq!(
        Env {
            state_home: None,
            home: None,
            ..env.clone()
        }
        .mode_path(&Mode::Lab("a".into())),
        Err(PathError::NoStateHome)
    );
    let fallback = Env {
        state_home: Some("relative".into()),
        ..env
    };
    assert_eq!(
        fallback.mode_path(&Mode::ReadOnly).unwrap(),
        Path::new(r"C:\Users\u\.local\state\ngteams-read-only\run\ctl.sock")
    );
}

#[test]
fn process_paths_use_local_app_data_and_the_users_profile() {
    let env = Env::from_process();
    assert_eq!(env.state_home, std::env::var_os("LOCALAPPDATA"));
    assert_eq!(env.home, std::env::var_os("USERPROFILE"));
    assert_eq!(
        env.runtime_dir,
        std::env::var_os("LOCALAPPDATA").map(|local| PathBuf::from(local).join("ngteams/runtime").into_os_string())
    );
    assert_eq!(env.socket, std::env::var_os(teams_ctl::path::ENV_SOCKET));
    assert_eq!(
        teams_ctl::socket_path(&Mode::ReadOnly),
        env.socket_path(&Mode::ReadOnly)
    );
}

#[test]
fn endpoint_names_are_local_bounded_and_preserve_unicode_units() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    let path = Path::new(r"C:\state\a\ctl.sock");
    let name = pipe_name(path).unwrap();
    assert!(name.starts_with(r"\\.\pipe\ngteams-ctl-"));
    assert!(name.len() < 100);
    assert_eq!(name, pipe_name(path).unwrap());
    let first = PathBuf::from(OsString::from_wide(&[67, 58, 92, 0xd800]));
    let second = PathBuf::from(OsString::from_wide(&[67, 58, 92, 0xd801]));
    assert_ne!(pipe_name(&first).unwrap(), pipe_name(&second).unwrap());
    assert!(pipe_name(Path::new("relative")).is_err());
    assert_eq!(
        teams_ctl::transport::Listener::bind(Path::new("relative"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    assert!(matches!(
        teams_ctl::Client::connect(Path::new("relative"), teams_ctl::Timeouts::default()),
        Err(teams_ctl::ClientError::NotRunning { .. })
    ));
    assert!(pipe_name(Path::new("C:\\bad\0name")).is_err());
}

#[test]
fn endpoint_directories_use_native_private_creation() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("first/second/ctl.sock");
    prepare_dir(&socket).unwrap();
    prepare_dir(&socket).unwrap();
    std::fs::write(socket.parent().unwrap().join("file"), b"").unwrap();
    assert!(prepare_dir(&socket.parent().unwrap().join("file/ctl.sock")).is_err());
    prepare_dir(Path::new("ctl.sock")).unwrap();
}
