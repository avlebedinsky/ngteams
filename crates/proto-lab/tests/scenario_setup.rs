//! Public scenario admission in environment-cleared subprocesses. Every path is synthetic and temporary;
//! the pinned browser is absent and the native-path placeholder is deliberately not executable.

#![cfg(test)]

use std::fmt::Write as _;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::ValueEnum as _;
use proto_lab::browser::Display;
use proto_lab::config::{Alias, LabLocations, LabPaths};
use proto_lab::scenario::ab_call::CallStep;
use proto_lab::scenario::{self, RunOptions, ScenarioError, ScenarioName, ScenarioReport};
use proto_lab::session::SessionError;

const CASE: &str = "PROTO_LAB_SCENARIO_SETUP_CASE";
const ROOT: &str = "PROTO_LAB_SCENARIO_SETUP_ROOT";
const GROUP: &str = "19:0000000000000000000000000000abc1@thread.v2";

#[test]
fn public_scenario_entry_points_refuse_incomplete_setup_without_opening_personal_resources() {
    for case in [
        "missing-config",
        "malformed-config",
        "missing-chats",
        "malformed-chats",
        "missing-mris",
        "create-missing-mri",
        "no-chrome",
        "no-echo",
        "tone-obstructed",
        "native-unset",
        "native-missing",
        "invalid-step",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "scenario_setup_child", "--nocapture", "--test-threads", "1"])
            .env_clear()
            .env(CASE, case)
            .env(ROOT, root)
            .env("PATH", "")
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/absent-bus", root.display()),
            );
        if case != "native-unset" {
            child.env("TEAMS_HEADLESS_BIN", root.join("native.disabled"));
        }
        for name in ["LD_LIBRARY_PATH", "LANG", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(name) {
                child.env(name, value);
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

fn options() -> RunOptions {
    RunOptions {
        display: Display::Headless,
        load_timeout: Duration::from_millis(10),
        call: CallStep::Answer,
    }
}

fn fixture_config(locations: &LabLocations, mris: bool, echo: bool) {
    fs::create_dir_all(locations.config_file.parent().unwrap()).unwrap();
    let mut text = String::new();
    for alias in Alias::ALL {
        writeln!(
            text,
            "[[accounts]]\nalias = {:?}\nupn = \"synthetic-{}@lab.example.test\"",
            alias.as_str(),
            alias.as_str()
        )
        .unwrap();
        if mris {
            writeln!(text, "mri = \"8:orgid:00000000-0000-4000-8000-00000000000{}\"", alias.as_str()).unwrap();
        }
    }
    if echo {
        text.push_str("\n[[service_participants]]\nname = \"echo\"\nmri = \"28:synthetic-echo\"\n");
    }
    fs::write(&locations.config_file, text).unwrap();
}

fn fixture(root: &Path, case: &str) -> (LabLocations, LabPaths) {
    let locations = LabLocations::from_env().unwrap();
    let paths = LabPaths::new(locations.data_dir.clone());
    assert!(locations.config_file.starts_with(root));
    assert!(paths.data_dir().starts_with(root));
    fs::create_dir_all(paths.data_dir()).unwrap();
    if !matches!(
        case,
        "missing-config" | "native-unset" | "native-missing" | "invalid-step"
    ) {
        fixture_config(
            &locations,
            !matches!(case, "missing-mris" | "create-missing-mri"),
            case != "no-echo",
        );
    }
    if case == "malformed-config" {
        fs::write(&locations.config_file, "[[accounts\nsynthetic malformed config").unwrap();
    }
    if !matches!(
        case,
        "missing-chats" | "create-missing-mri" | "native-unset" | "native-missing" | "invalid-step"
    ) {
        fs::write(paths.chats_file(), format!("abc = {GROUP:?}\n")).unwrap();
    }
    if case == "malformed-chats" {
        fs::write(paths.chats_file(), "abc = [synthetic malformed chats").unwrap();
    }
    if case == "tone-obstructed" {
        fs::write(paths.data_dir().join("media"), "synthetic obstruction").unwrap();
    }
    if !matches!(case, "native-unset" | "native-missing") {
        let disabled = root.join("native.disabled");
        fs::write(&disabled, "synthetic non-executable native-path placeholder\n").unwrap();
        fs::set_permissions(&disabled, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(fs::metadata(disabled).unwrap().permissions().mode() & 0o111, 0);
    }
    (locations, paths)
}

async fn invoke(name: ScenarioName, paths: &LabPaths) -> Result<ScenarioReport, ScenarioError> {
    scenario::run(name, paths, options()).await
}

fn no_chrome(name: ScenarioName, result: Result<ScenarioReport, ScenarioError>, paths: &LabPaths) {
    let error = result.unwrap_err();
    if scenario::require_lab_scenario(name).is_err() {
        assert!(
            error.to_string().contains("unavailable for implementation runs"),
            "{name}: {error}"
        );
        return;
    }
    let ScenarioError::Session(SessionError::NoChrome(executable)) = error else {
        panic!("{name}: {error}")
    };
    assert!(executable.starts_with(paths.chrome_dir()));
    assert!(!executable.exists());
}

async fn missing_config(paths: &LabPaths) {
    for &name in ScenarioName::value_variants() {
        let result = invoke(name, paths).await;
        if scenario::require_lab_scenario(name).is_err() {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("unavailable for implementation runs")
            );
            continue;
        }
        if matches!(
            name,
            ScenarioName::Load | ScenarioName::Search | ScenarioName::ChannelCreate | ScenarioName::TeamManage
        ) {
            no_chrome(name, result, paths);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(error.contains("lab.toml"), "{name}: {error}");
        }
    }
}

async fn missing_chats(paths: &LabPaths) {
    for name in [ScenarioName::AbcCall, ScenarioName::AbcCallMixed] {
        let error = invoke(name, paths).await.unwrap_err().to_string();
        assert!(error.contains("records no a-b-c chat"), "{name}: {error}");
    }
    let report = invoke(ScenarioName::AbcEveryone, paths).await.unwrap();
    assert!(report.accounts.is_empty());
    assert_eq!(report.failures.len(), 1);
    assert!(report.failures[0].contains("no group chat recorded"));
    no_chrome(ScenarioName::AbcChat, invoke(ScenarioName::AbcChat, paths).await, paths);
}

async fn malformed_chats(paths: &LabPaths) {
    for name in [
        ScenarioName::AbcChat,
        ScenarioName::AbcEveryone,
        ScenarioName::AbcCall,
        ScenarioName::AbcCallMixed,
        ScenarioName::ChatShapes,
    ] {
        let error = invoke(name, paths).await.unwrap_err().to_string();
        assert!(error.contains("chats.toml"), "{name}: {error}");
    }
}

async fn missing_mris(paths: &LabPaths) {
    for name in [
        ScenarioName::AbCall,
        ScenarioName::AbcCall,
        ScenarioName::AbCallNative,
        ScenarioName::AbcCallMixed,
        ScenarioName::AbCallApp,
    ] {
        let error = invoke(name, paths).await.unwrap_err().to_string();
        assert!(error.contains("MRI"), "{name}: {error}");
    }
}

async fn no_browser(paths: &LabPaths) {
    for &name in ScenarioName::value_variants() {
        no_chrome(name, invoke(name, paths).await, paths);
    }
}

async fn obstructed_tone(paths: &LabPaths) {
    for name in [
        ScenarioName::EchoCall,
        ScenarioName::AbCall,
        ScenarioName::AbcCall,
        ScenarioName::AbCallNative,
        ScenarioName::AbcCallMixed,
        ScenarioName::AbCallApp,
    ] {
        let error = invoke(name, paths).await.unwrap_err().to_string();
        if scenario::require_lab_scenario(name).is_err() {
            assert!(error.contains("unavailable for implementation runs"));
            continue;
        }
        assert!(error.contains("can't write the tone"), "{name}: {error}");
    }
}

async fn absent_native(paths: &LabPaths, unset: bool) {
    for name in [ScenarioName::AbCallNative, ScenarioName::AbcCallMixed] {
        let error = invoke(name, paths).await.unwrap_err().to_string();
        assert!(error.contains("TEAMS_HEADLESS_BIN"), "{name}: {error}");
        assert!(
            error.contains(if unset { "is unset" } else { "is not a file" }),
            "{error}"
        );
    }
}

async fn invalid_step(paths: &LabPaths) {
    let mut options = options();
    options.call = CallStep::Decline;
    let error = scenario::run(ScenarioName::AbCallApp, paths, options)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("takes --call answer or --call place"));
    let error = scenario::own_presence::run_named(paths, options, ScenarioName::Load)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("is no own-presence scenario"));
}

fn assert_no_sessions_or_credentials(paths: &LabPaths, root: &Path) {
    for path in [
        paths.chrome_dir(),
        paths.profiles_dir(),
        paths.captures_dir(),
        paths.data_dir().join("auth"),
        root.join("data/keyrings"),
        root.join("data/ngteams"),
        root.join("config/ngteams"),
        root.join("home/.local/share/keyrings"),
    ] {
        assert!(!path.exists(), "refusal created {}", path.display());
    }
    for alias in Alias::ALL {
        assert!(!paths.store_file(alias).exists());
    }
    if paths.fence_log().exists() {
        assert_eq!(
            fs::metadata(paths.fence_log()).unwrap().len(),
            0,
            "no fence decision was requested"
        );
    }
}

#[tokio::test]
async fn scenario_setup_child() {
    let Ok(case) = std::env::var(CASE) else { return };
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    assert_eq!(std::env::var("PATH").unwrap(), "");
    for key in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "PROTO_LAB_LIVE",
        "TEAMS_MEDIA_TURN_SERVER",
        "TURN_URL",
    ] {
        assert!(std::env::var_os(key).is_none());
    }
    let (locations, paths) = fixture(&root, &case);
    match case.as_str() {
        "missing-config" | "malformed-config" => missing_config(&paths).await,
        "missing-chats" => missing_chats(&paths).await,
        "malformed-chats" => malformed_chats(&paths).await,
        "missing-mris" => missing_mris(&paths).await,
        "create-missing-mri" => {
            let error = invoke(ScenarioName::AbcChat, &paths).await.unwrap_err().to_string();
            assert!(error.contains("no MRI for b"), "{error}");
        }
        "no-chrome" => no_browser(&paths).await,
        "no-echo" => no_chrome(
            ScenarioName::EchoCall,
            invoke(ScenarioName::EchoCall, &paths).await,
            &paths,
        ),
        "tone-obstructed" => obstructed_tone(&paths).await,
        "native-unset" | "native-missing" => absent_native(&paths, case == "native-unset").await,
        "invalid-step" => invalid_step(&paths).await,
        _ => panic!("unknown synthetic case {case}"),
    }
    assert_no_sessions_or_credentials(&paths, &root);
    if matches!(
        case.as_str(),
        "missing-config" | "native-unset" | "native-missing" | "invalid-step"
    ) {
        assert!(!locations.config_file.exists());
        assert!(!paths.fence_log().exists());
        assert!(!paths.data_dir().join("media").exists());
    }
}
