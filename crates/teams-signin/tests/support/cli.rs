//! Constructing a standalone page configuration does not start a page; every directory and descriptor is synthetic.

use std::process::Command;

use super::*;

const CASE: &str = "TEAMS_SIGNIN_CONFIG_TEST_CASE";

#[test]
fn standalone_configuration_checks_its_bound_and_environment_before_any_page_starts() {
    for case in [
        "good",
        "remote",
        "display",
        "missing-home",
        "missing-extension",
        "executable-error",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "tests::configuration_child", "--nocapture"])
            .env_clear()
            .env(CASE, case)
            .env("TEAMS_SIGNIN_CONFIG_TEST_ROOT", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("UI_SANDBOX_DIR", root.path().join("sandbox"))
            .env("WAYLAND_DISPLAY", root.path().join("sandbox/synthetic-wayland"));
        if case == "missing-home" {
            child.env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME");
        }
        if case != "missing-home" {
            child.env("HOME", root.path().join("home"));
        }
        if case == "display" {
            child.env("DISPLAY", ":synthetic");
        }
        for key in ["PATH", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn configuration_child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let root = std::path::PathBuf::from(std::env::var_os("TEAMS_SIGNIN_CONFIG_TEST_ROOT").unwrap());
    std::fs::create_dir_all(root.join("data/teams-lab")).unwrap();
    let bin = root.join("synthetic-bin");
    std::fs::create_dir(&bin).unwrap();
    if case != "missing-extension" {
        std::fs::write(bin.join(window::EXTENSION_FILE), b"_synthetic; not a loadable module").unwrap();
    }
    let request = WindowRequest {
        authorize: if case == "remote" {
            "https://example.test/authorize"
        } else {
            "http://127.0.0.1/synthetic"
        }
        .into(),
        redirect_uri: teams_auth::token::NATIVE_CLIENT_REDIRECT.into(),
    };
    let configured = request_config(request, || {
        if case == "executable-error" {
            Err(io::Error::other("synthetic executable failure"))
        } else {
            Ok(bin.join("synthetic-signin"))
        }
    });
    if case == "good" {
        let config = configured.unwrap();
        assert_eq!(config.request.authorize, "http://127.0.0.1/synthetic");
        assert_eq!(config.extension, bin.join(window::EXTENSION_FILE));
        assert!(matches!(config.bound, Bound::Loopback));
        return;
    }
    let error = configured.err().unwrap();
    let wanted = match case.as_str() {
        "remote" => NOT_LIVE,
        "display" => "DISPLAY is set",
        "missing-home" => "the lab's directories",
        "missing-extension" => "no libteams_signin_extension.so",
        "executable-error" => "where am I: synthetic executable failure",
        _ => panic!("unknown case {case}"),
    };
    assert!(error.contains(wanted), "{case}: {error}");
}
