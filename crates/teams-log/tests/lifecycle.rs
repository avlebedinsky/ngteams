//! Failed installation, the effective installed level and the diagnostics readers share the public logger API.

#![cfg(test)]

use std::error::Error as _;
use std::fs;

use teams_log::diagnostics::Source;
use teams_log::{Config, InstallError, Level, Rotation, apply_setting, install, installed};

fn source(log_file: Option<std::path::PathBuf>) -> Source {
    Source {
        app: "teams-app 0.1.0".to_owned(),
        log_file,
        log_problem: None,
    }
}

fn failed_installation_leaves_the_logger_available(config: &Config) {
    fs::write(&config.dir, b"a file cannot be the log directory").expect("block the directory");

    let error = install(config).expect_err("a regular file cannot hold the log");

    assert!(matches!(&error, InstallError::Open { dir, .. } if *dir == config.dir));
    assert!(
        error
            .source()
            .expect("the opening error is retained")
            .downcast_ref::<std::io::Error>()
            .is_some()
    );
    let message = error.to_string();
    assert!(message.contains(&config.dir.display().to_string()), "{message}");
    assert!(message.contains("the app runs without it"), "{message}");
    assert!(installed().is_none());
    fs::remove_file(&config.dir).expect("unblock the directory");
}

fn diagnostics_before_installation_use_the_setting() {
    assert!(installed().is_none());
    assert_eq!(apply_setting(Level::Debug), None);
    let before = source(None).collect(Level::Debug);
    assert!(before.text.contains("log level: debug\n"), "{}", before.text);
    assert!(before.text.contains("log: not written\n"), "{}", before.text);
    assert!(!before.has_debug);
}

fn diagnostics_after_installation_show_records_at_the_effective_level(logger: &teams_log::FileLog) {
    log::warn!(target: "teams_core::test", "synthetic warning before collection");
    log::debug!(target: "teams_core::test", "synthetic debug that stays filtered");

    // This checks the caller-visible report with a running writer. The writer may already have consumed the
    // warning, so it does not isolate collect's flush; file.rs tests acknowledgement with the writer held.
    let report = source(Some(logger.path().to_owned())).collect(Level::Debug);

    assert!(
        report.text.contains("log level: warn (set by TEAMS_APP_LOG)\n"),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("synthetic warning before collection"),
        "{}",
        report.text
    );
    assert!(!report.text.contains("synthetic debug that stays filtered"));
    assert!(!report.has_debug);
    assert_eq!(logger.dropped(), 0);
}

#[test]
fn diagnostics_use_the_setting_until_install_then_the_effective_level_and_current_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    diagnostics_before_installation_use_the_setting();
    let config = Config {
        dir: dir.path().join("logs"),
        setting: Level::Debug,
        env: Some(" WARN ".to_owned()),
        rotation: Rotation::default(),
    };
    failed_installation_leaves_the_logger_available(&config);
    let logger = install(&config).expect("a failed install did not claim the global logger");
    assert_eq!(logger.level(), Level::Warn);
    assert_eq!(apply_setting(Level::Debug), Some(Level::Warn));
    assert_eq!(log::max_level(), log::LevelFilter::Warn);
    diagnostics_after_installation_show_records_at_the_effective_level(logger);

    log::warn!(target: "teams_core::test", "synthetic warning before facade flush");
    log::logger().flush();
    // The facade's end-to-end result is checked here; the writer may have consumed the record before the call.
    let text = fs::read_to_string(logger.path()).expect("read the current records after the facade flush");
    assert!(text.contains("synthetic warning before facade flush"), "{text}");

    let error = install(&config).expect_err("the successful installation remains exclusive");
    assert!(matches!(error, InstallError::AlreadyInstalled));
    assert!(error.source().is_none());
    assert_eq!(
        error.to_string(),
        "a logger is installed already: the diagnostic log isn't"
    );
}
