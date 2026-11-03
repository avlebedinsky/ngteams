//! Lab startup controllers use only disposable files, socket pairs and the existing mock GPUI/account backend.

use std::fs;
use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode};

use clap::Parser as _;
use ngteams::accounts_file::PersonalAccount;
use ngteams::cli::Cli;

use super::{Outcome, lab_broker, run_lab};

const CASE: &str = "NGTEAMS_LAB_STARTUP_CASE";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn lab_startup_and_broker_use_alias_files_and_preserve_restart_descriptors() {
    for case in ["ready", "pending", "wrong-account", "no-sandbox", "broker"] {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lab_startup_tests::lab_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env_clear()
            .env(CASE, case)
            .env(
                "NGTEAMS_STARTUP_TEST_CASE",
                if case == "pending" {
                    "first-success"
                } else {
                    "lab-ready"
                },
            )
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-user-bus", root.path().display()),
            )
            .env("UI_SANDBOX_DIR", root.path().join("sandbox"))
            .env("WAYLAND_DISPLAY", root.path().join("sandbox/synthetic-wayland"))
            .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !root.path().join("config/ngteams").exists(),
            "{case}: personal config untouched"
        );
        assert!(
            !root.path().join("data/ngteams").exists(),
            "{case}: personal data untouched"
        );
    }
}

#[test]
fn lab_child() {
    let Ok(case) = std::env::var(CASE) else { return };
    fs::create_dir_all(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).unwrap();
    let paths = ngteams::lab::paths_from_env("a").unwrap();
    let pending = PersonalAccount::pending("pending@example.test").unwrap();
    let account = if case == "pending" {
        pending.clone()
    } else {
        PersonalAccount::new("ready@example.test", OID, TID).unwrap()
    };
    account.save(&paths).unwrap();
    if let Some(region) = paths.region_file(&account) {
        fs::create_dir_all(region.parent().unwrap()).unwrap();
        fs::write(
            region,
            format!(
                r#"{{"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{}"}}}}"#,
                teams_mock::CHAT_BASE
            ),
        )
        .unwrap();
    }
    if case == "broker" {
        broker_cases(&paths, &pending);
        return;
    }
    let (peer, socket) = UnixStream::pair().unwrap();
    let mut args = vec![
        "ngteams".to_owned(),
        "--lab".into(),
        "a".into(),
        "--secrets-fd".into(),
        socket.into_raw_fd().to_string(),
    ];
    if case == "wrong-account" {
        args.extend(["--account".into(), "other@example.test".into()]);
    }
    let cli = Cli::try_parse_from(args).unwrap();
    let mode = cli.lab_mode().unwrap().unwrap();
    if case == "no-sandbox" {
        // SAFETY: this isolated subprocess has not started any runtime/thread or GUI backend yet.
        unsafe { std::env::set_var("DISPLAY", ":synthetic-forbidden") };
    }
    match (case.as_str(), run_lab(&cli, &mode)) {
        ("ready", Ok(Outcome::Quit)) => assert!(paths.store_file(&account).unwrap().exists()),
        ("pending", Ok(Outcome::LabRestart(descriptors))) => assert!(descriptors.socket().is_ok()),
        ("wrong-account", Err(error)) => assert!(error.contains("configured for")),
        ("no-sandbox", Err(error)) => assert!(error.contains("DISPLAY")),
        (_, Err(error)) => panic!("{case}: {error}"),
        _ => panic!("{case}: unexpected startup outcome"),
    }
    drop(peer);
}

fn broker_cases(paths: &ngteams::accounts_file::PersonalPaths, pending: &PersonalAccount) {
    let closed_broker = || {
        let (peer, socket) = UnixStream::pair().unwrap();
        drop(peer);
        lab_broker("a", socket.into_raw_fd())
    };
    assert_eq!(lab_broker("a", -1), ExitCode::FAILURE);
    assert_eq!(closed_broker(), ExitCode::SUCCESS, "EOF opens no lazy keyring");
    pending.save(paths).unwrap();
    assert_eq!(
        closed_broker(),
        ExitCode::FAILURE,
        "two accounts cannot share a scoped alias"
    );
    PersonalAccount::remove(paths, "ready@example.test").unwrap();
    assert_eq!(
        closed_broker(),
        ExitCode::SUCCESS,
        "pending account can sign in through the broker"
    );
}
