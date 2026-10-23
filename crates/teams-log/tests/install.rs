//! The log installed for a process, as the app installs it: `log`'s logger, its level live, only our crates' records
//! (a third-party `log` line and a `tracing` event forwarded to `log` with no subscriber, as the UI kit's are, never
//! reach the file), installed once, and the panic hook: a panic written redacted with its location, its thread and a
//! backtrace, flushed before the previous hook runs. One test: the logger and the hook are the process's.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs;

use teams_log::{Config, InstallError, Level, Rotation, apply_setting, install, installed};

/// A JWT-shaped token, made up.
fn jwt() -> String {
    let body = "PaNiCtOkEnAbCdEfGh0123456789-_".repeat(4);
    format!("eyJ0eXAiOiJKV1QiLCJhbGciOiJSUzI1NiJ9.eyJ{body}.{body}")
}

#[test]
fn the_installed_log_takes_only_our_records_and_writes_a_panic_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        dir: dir.path().join("logs").join("a"),
        setting: Level::Info,
        env: None,
        rotation: Rotation::default(),
    };
    let log = install(&config).unwrap();
    assert!(std::ptr::eq(log, installed().unwrap()));
    assert!(matches!(install(&config), Err(InstallError::AlreadyInstalled)));
    assert_eq!(log::max_level(), log::LevelFilter::Info);
    log_at_info();
    assert_eq!(apply_setting(Level::Debug), Some(Level::Debug));
    assert_eq!(log::max_level(), log::LevelFilter::Debug);
    log::debug!(target: "teams_core::engine", "ours at debug, after the change");
    panic_on_a_thread();

    assert!(log.flush_written());
    let text = fs::read_to_string(log.path()).unwrap();
    check_records(&text);
    check_panic(&text);
}

/// Our records at `info` and `debug`, a third party's, and `tracing` events (the kit's), while the level is `info`.
fn log_at_info() {
    log::info!(target: "teams_core::engine", "ours at info");
    log::debug!(target: "teams_core::engine", "ours at debug, before the change");
    log::error!(target: "tungstenite::protocol", "a third-party error");
    kit_error();
    own_tracing_warning();
}

/// A `tracing` event as the UI kit makes one (forwarded to `log`: no subscriber is set).
fn kit_error() {
    tracing::error!(target: "gpui_component::theme", "the kit's tracing error");
}

/// A `tracing` event of this test's own module.
fn own_tracing_warning() {
    tracing::warn!("a tracing warning of this test's own module");
}

/// A panic on a thread named `panicker`, its message carrying a token.
fn panic_on_a_thread() {
    let token = jwt();
    let panicked = std::thread::Builder::new()
        .name("panicker".to_owned())
        .spawn(move || {
            let message = format!("a panic carrying {token}");
            std::panic::panic_any(message);
        })
        .unwrap()
        .join();
    assert!(panicked.is_err());
}

/// Our records at the level that applied when they were made, nobody else's.
fn check_records(text: &str) {
    assert!(
        text.contains("teams_log: log opened at level info (the settings)"),
        "{text}"
    );
    assert!(text.contains("ours at info"), "{text}");
    assert!(text.contains("ours at debug, after the change"), "{text}");
    assert!(text.contains("teams_log: log level now debug"), "{text}");
    for absent in [
        "before the change",
        "a third-party error",
        "the kit's tracing error",
        "a tracing warning",
    ] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
}

/// The panic, redacted, with its location, its thread and a backtrace.
fn check_panic(text: &str) {
    let panic = text
        .lines()
        .find(|line| line.contains("panic on thread panicker"))
        .unwrap_or_else(|| panic!("the panic is written: {text}"));
    assert!(panic.contains("ERROR teams_log:"), "{panic}");
    assert!(panic.contains("tests/install.rs:"), "its location: {panic}");
    assert!(panic.contains("a panic carrying [redacted]"), "{panic}");
    assert!(panic.contains("backtrace:"), "{panic}");
    assert!(!text.contains(&jwt()), "the token never reaches the file");
}
