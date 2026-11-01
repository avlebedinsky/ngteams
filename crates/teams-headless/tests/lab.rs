//! Lab configuration and private local files, loaded in a subprocess whose XDG roots are temporary. No engine,
//! Secret Service, tenant request or GUI is started.

#![cfg(test)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use teams_fence::lab::{Alias, LabConfig, record_abc_chat};
use teams_headless::lab::{Lab, LabError, call_movie};
use teams_mock::{ABC_CHAT, LAB_A, lab_toml};

const CHILD: &str = "NGTEAMS_PRIVATE_LAB_LOAD_CHILD";

#[test]
fn the_lab_loads_only_the_private_config_store_and_recorded_chats() {
    if std::env::var(CHILD).as_deref() == Ok("child") {
        check_loaded_lab();
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("teams-lab");
    std::fs::create_dir_all(&config_dir).unwrap();
    let text = lab_toml(&dir.path().join("data"));
    std::fs::write(config_dir.join("lab.toml"), &text).unwrap();
    let config = LabConfig::parse(&text, dir.path()).unwrap();
    std::fs::create_dir_all(config.paths().data_dir()).unwrap();
    record_abc_chat(config.paths(), ABC_CHAT).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_lab_loads_only_the_private_config_store_and_recorded_chats",
            "--nocapture",
        ])
        .env(CHILD, "child")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", config_home)
        .env("XDG_DATA_HOME", dir.path().join("unused-data"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

fn check_loaded_lab() {
    let lab = Lab::load(Alias::A).unwrap();
    assert_eq!(lab.alias, Alias::A);
    assert_eq!(lab.abc.as_deref(), Some(ABC_CHAT));
    assert_eq!(lab.names.mri(Alias::A), Some(LAB_A));
    assert!(matches!(lab.echo(), Err(LabError::NoEcho)));
    let store = lab.open_store().unwrap();
    assert_eq!(store.path(), lab.paths().store_file(Alias::A));
    drop(store);
    let recordings = lab.recordings_dir().unwrap();
    assert_eq!(
        std::fs::metadata(&recordings).unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::remove_dir(&recordings).unwrap();
    std::fs::write(&recordings, b"a file blocks the directory").unwrap();
    assert!(matches!(lab.recordings_dir(), Err(LabError::Recordings(_))));
}

#[test]
fn a_movie_offset_must_be_finite_nonnegative_and_inside_the_media() {
    let missing = std::path::PathBuf::from("/a/made-up/missing-movie.mp4");
    for offset in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(call_movie(missing.clone(), offset, false).is_err());
    }
    let fixture = std::path::PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../teams-media/tests/fixtures/media/made-up-test-pattern.mp4"
    ));
    assert!(call_movie(fixture, 1_000.0, false).is_err());
}
