//! The opt-in diagnostic log (`TEAMS_HEADLESS_LOG`): only our own crates' records, at the chosen level or above, each
//! line without terminal controls.

use log::{Level, Metadata, Record};
use teams_headless::debug_log::{line, shown};

fn metadata(level: Level, target: &str) -> Metadata<'_> {
    Metadata::builder().level(level).target(target).build()
}

#[test]
fn only_our_crates_records_at_the_level_or_above_are_shown() {
    assert!(shown(&metadata(Level::Debug, "teams_trouter::session"), Level::Debug));
    assert!(shown(&metadata(Level::Warn, "teams_core::engine"), Level::Debug));
    assert!(!shown(&metadata(Level::Debug, "teams_trouter::session"), Level::Info));
    for target in [
        "tungstenite::protocol",
        "reqwest::connect",
        "rustls::client",
        "hyper",
        "teams",
        // the app's full requests, answers and frames: this log stays by kind and size
        "teams_wire::http",
        "teams_wire::trouter",
    ] {
        assert!(!shown(&metadata(Level::Error, target), Level::Debug), "{target}");
    }
}

#[test]
fn a_line_has_the_time_level_target_and_message_without_controls() {
    let shown_line = line(
        1_700_000_000_000,
        &Record::builder()
            .level(Level::Debug)
            .target("teams_trouter::session")
            .args(format_args!("a delivery of kind {:?}\u{1b}[2J", "NewMessage"))
            .build(),
    );
    assert_eq!(
        shown_line,
        "1700000000000 DEBUG teams_trouter::session: a delivery of kind \"NewMessage\"\\u{1b}[2J"
    );
}

#[test]
fn environment_logger_is_opt_in_idempotent_and_never_prints_wire_records() {
    const CHILD: &str = "NGTEAMS_TEST_DEBUG_LOG_CHILD";
    if std::env::var_os(CHILD).is_some() {
        teams_headless::debug_log::init_from_env();
        teams_headless::debug_log::init_from_env();
        log::info!(target: "teams_headless::synthetic", "synthetic lifecycle\u{1b}[2J");
        log::debug!(target: "teams_headless::synthetic", "synthetic debug");
        log::error!(target: "teams_wire::http", "WIRE_PAYLOAD_MUST_NOT_PRINT");
        log::error!(target: "unrelated", "EXTERNAL_MUST_NOT_PRINT");
        log::logger().flush();
        return;
    }
    for setting in [None, Some("invalid"), Some("debug")] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "environment_logger_is_opt_in_idempotent_and_never_prints_wire_records",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env_remove(teams_headless::debug_log::VARIABLE);
        if let Some(setting) = setting {
            child.env(teams_headless::debug_log::VARIABLE, setting);
        }
        let output = child.output().unwrap();
        assert!(output.status.success());
        assert_child_log(setting, &String::from_utf8(output.stderr).unwrap());
    }
}

fn assert_child_log(setting: Option<&str>, stderr: &str) {
    assert!(!stderr.contains("WIRE_PAYLOAD_MUST_NOT_PRINT"));
    assert!(!stderr.contains("EXTERNAL_MUST_NOT_PRINT"));
    if setting == Some("debug") {
        assert!(stderr.contains(r"synthetic lifecycle\u{1b}[2J"), "{stderr}");
        assert!(stderr.contains("synthetic debug"), "{stderr}");
        assert_eq!(stderr.matches("synthetic lifecycle").count(), 1);
    } else {
        assert!(stderr.is_empty(), "{stderr}");
    }
}

#[test]
fn the_stderr_sink_redacts_credentials_and_raw_ice_fields_before_terminal_escaping() {
    let message = "SDP failed a=ice-pwd:SyntheticPwd4321 a=ice-ufrag:SyntheticUfrag5678 Bearer SyntheticBearer6543\r\n";
    let line = line(
        7,
        &Record::builder()
            .level(Level::Warn)
            .target("teams_core::calling")
            .args(format_args!("{message}"))
            .build(),
    );
    assert!(line.contains("SDP failed"));
    assert!(line.contains("[redacted]"));
    for secret in ["SyntheticPwd4321", "SyntheticUfrag5678", "SyntheticBearer6543"] {
        assert!(!line.contains(secret), "{line}");
    }
    assert!(!line.contains('\r') && !line.contains('\n'));
}
