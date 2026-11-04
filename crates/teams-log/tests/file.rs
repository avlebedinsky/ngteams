//! The log's file: `0600` in a `0700` directory (a file found open to others tightened), rotated by size with the
//! older files kept `0600`, written from a thread of its own; lines that find the writer's channel full are dropped,
//! never waited for, and their count is written; and the logger itself: only our crates' records, at the level that
//! applies, `TEAMS_APP_LOG` overriding the settings, `trace` taken as `debug`, one record per line.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fs::{self, OpenOptions, Permissions};
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use log::{Level as LogLevel, Log as _, Metadata, Record};
use teams_log::file::{DIR_MODE, FILE_MODE, Sink};
use teams_log::{Config, FileLog, Level, Rotation, effective, format_line, ours};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn config(dir: &Path, setting: Level, env: Option<&str>) -> Config {
    Config {
        dir: dir.join("logs").join("a"),
        setting,
        env: env.map(str::to_owned),
        rotation: Rotation::default(),
    }
}

fn record(log: &FileLog, level: LogLevel, target: &str, message: &str) {
    log.log(
        &Record::builder()
            .level(level)
            .target(target)
            .args(format_args!("{message}"))
            .build(),
    );
}

fn read(log: &FileLog) -> String {
    assert!(log.flush_written());
    fs::read_to_string(log.path()).unwrap()
}

#[test]
fn the_file_is_private_in_a_private_directory() {
    let dir = tempfile::tempdir().unwrap();
    let log = FileLog::open(&config(dir.path(), Level::Info, None)).unwrap();
    record(&log, LogLevel::Info, "teams_core::engine", "started");
    read(&log);
    assert_eq!(log.path(), dir.path().join("logs/a/teams-app.log"));
    assert_eq!(mode(log.path()), FILE_MODE);
    assert_eq!(mode(&dir.path().join("logs/a")), DIR_MODE);
}

#[test]
fn a_file_open_to_others_is_tightened_and_appended_to() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("logs/a");
    fs::create_dir_all(&logs).unwrap();
    fs::set_permissions(&logs, Permissions::from_mode(0o755)).unwrap();
    let path = logs.join("teams-app.log");
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o644)
        .open(&path)
        .unwrap();
    fs::write(&path, "an earlier line\n").unwrap();
    let log = FileLog::open(&config(dir.path(), Level::Info, None)).unwrap();
    record(&log, LogLevel::Info, "teams_core::engine", "a later line");
    let text = read(&log);
    assert!(
        text.starts_with("an earlier line\n") && text.contains("a later line"),
        "{text}"
    );
    assert_eq!(mode(&path), FILE_MODE);
    assert_eq!(mode(&logs), DIR_MODE);
}

#[test]
fn the_file_is_rotated_by_size_and_the_oldest_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let sink = Sink::open(
        dir.path(),
        "teams-app.log",
        Rotation {
            max_bytes: 100,
            keep: 2,
        },
    )
    .unwrap();
    for number in 0..10 {
        // 50 bytes a line with its newline: two lines a file
        sink.push(format!("{number:0>49}"));
    }
    assert!(sink.flush());
    let file = |name: &str| dir.path().join(name);
    assert_eq!(
        fs::read_to_string(file("teams-app.log")).unwrap(),
        format!("{:0>49}\n{:0>49}\n", 8, 9)
    );
    assert_eq!(
        fs::read_to_string(file("teams-app.log.1")).unwrap(),
        format!("{:0>49}\n{:0>49}\n", 6, 7)
    );
    assert_eq!(
        fs::read_to_string(file("teams-app.log.2")).unwrap(),
        format!("{:0>49}\n{:0>49}\n", 4, 5)
    );
    assert!(!file("teams-app.log.3").exists(), "two older files kept");
    for name in ["teams-app.log", "teams-app.log.1", "teams-app.log.2"] {
        assert_eq!(mode(&file(name)), FILE_MODE, "{name}");
    }
}

/// A sink rotating every two 50-byte lines, two older files kept, with lines `0..4` written: `.1` holds 0 and 1, the
/// file 2 and 3.
fn rotated_sink(dir: &Path) -> Sink {
    let sink = Sink::open(
        dir,
        "teams-app.log",
        Rotation {
            max_bytes: 100,
            keep: 2,
        },
    )
    .unwrap();
    for number in 0..4 {
        sink.push(format!("{number:0>49}"));
    }
    assert!(sink.flush());
    sink
}

fn lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| line.trim_start_matches('0').to_owned())
        .collect()
}

#[test]
fn a_rotation_that_can_t_make_its_new_file_moves_nothing_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let sink = rotated_sink(dir.path());
    let file = |name: &str| dir.path().join(name);
    // the new file can't be made (a directory where it goes, as a full disk or no descriptors would fail it)
    fs::create_dir(file("teams-app.log.new")).unwrap();
    fs::write(file("teams-app.log.new/keep"), "").unwrap();
    for number in 4..10 {
        sink.push(format!("{number:0>49}"));
    }
    assert!(sink.flush());
    assert_eq!(lines(&file("teams-app.log.1")), ["", "1"], "the archive stays");
    assert!(!file("teams-app.log.2").exists(), "nothing shifted over it");
    assert_eq!(
        lines(&file("teams-app.log")),
        ["2", "3", "4", "5", "6", "7", "8", "9"],
        "the writer kept its file under its name"
    );
    // once it can be made again, the rotation goes on as before
    fs::remove_dir_all(file("teams-app.log.new")).unwrap();
    sink.push(format!("{:0>49}", "a"));
    assert!(sink.flush());
    assert_eq!(lines(&file("teams-app.log")), ["a"]);
    assert_eq!(lines(&file("teams-app.log.1")).len(), 8);
    assert_eq!(lines(&file("teams-app.log.2")), ["", "1"]);
    assert!(!file("teams-app.log.new").exists());
    assert_eq!(mode(&file("teams-app.log")), FILE_MODE);
}

#[test]
fn a_file_taken_away_is_made_again_without_moving_the_older_ones() {
    let dir = tempfile::tempdir().unwrap();
    let sink = rotated_sink(dir.path());
    let file = |name: &str| dir.path().join(name);
    fs::remove_file(file("teams-app.log")).unwrap();
    sink.push(format!("{:0>49}", 4));
    assert!(sink.flush());
    assert_eq!(lines(&file("teams-app.log.1")), ["", "1"], "the archive stays");
    assert!(!file("teams-app.log.2").exists(), "nothing shifted over it");
    assert_eq!(lines(&file("teams-app.log")), ["4"], "made again");
    assert_eq!(mode(&file("teams-app.log")), FILE_MODE);
    // and rotated as before
    for number in 5..7 {
        sink.push(format!("{number:0>49}"));
    }
    assert!(sink.flush());
    assert_eq!(lines(&file("teams-app.log")), ["6"]);
    assert_eq!(lines(&file("teams-app.log.1")), ["4", "5"]);
    assert_eq!(lines(&file("teams-app.log.2")), ["", "1"]);
}

#[test]
fn the_default_rotation_is_8_mib_and_4_older_files() {
    assert_eq!(
        Rotation::default(),
        Rotation {
            max_bytes: 8 * 1024 * 1024,
            keep: 4
        }
    );
}

#[test]
fn a_full_channel_drops_lines_without_waiting_and_counts_them() {
    let dir = tempfile::tempdir().unwrap();
    let (sink, paused) = Sink::open_paused(dir.path(), "teams-app.log", Rotation::default(), 3).unwrap();
    let started = std::time::Instant::now();
    for number in 0..10 {
        sink.push(format!("line {number}"));
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "no push waited for the writer"
    );
    assert_eq!(sink.dropped(), 7);
    paused.resume();
    assert!(sink.flush());
    let text = fs::read_to_string(dir.path().join("teams-app.log")).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(&lines[..3], ["line 0", "line 1", "line 2"]);
    assert_eq!(lines.len(), 4, "{text}");
    assert!(
        lines[3].contains("WARN  teams_log: 7 log line(s) dropped: the writer fell behind"),
        "{text}"
    );
}

#[test]
fn only_our_crates_records_at_the_level_that_applies_reach_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let log = FileLog::open(&config(dir.path(), Level::Info, None)).unwrap();
    record(&log, LogLevel::Info, "teams_proto::http", "ours, info");
    record(&log, LogLevel::Debug, "teams_proto::http", "ours, debug");
    record(&log, LogLevel::Error, "tungstenite::protocol", "theirs, error");
    record(&log, LogLevel::Warn, "gpui_component::theme", "the kit's, warn");
    record(&log, LogLevel::Error, "teams", "not a crate of ours");
    let text = read(&log);
    assert!(text.contains("ours, info"), "{text}");
    for absent in ["ours, debug", "theirs", "the kit's", "not a crate of ours"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    assert!(ours(&Metadata::builder().target("teams_ui::bridge").build()));
    assert!(ours(&Metadata::builder().target("ngteams::app").build()));
    assert!(!ours(&Metadata::builder().target("ngteamsish").build()));
    assert!(!ours(&Metadata::builder().target("reqwest::connect").build()));
}

#[test]
fn a_flush_requires_the_writers_acknowledgement_and_recovers_after_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), Level::Info, None);
    let (sink, paused) = Sink::open_paused(&config.dir, "teams-app.log", config.rotation, 2).unwrap();
    let log = FileLog::with_sink(sink, &config);
    record(&log, LogLevel::Warn, "teams_core::test", "synthetic queued warning");

    // The writer cannot consume either the record or the flush request until it is explicitly resumed.
    assert!(
        !log.flush_written(),
        "a held writer cannot acknowledge the queued record"
    );
    assert_eq!(fs::read_to_string(log.path()).unwrap(), "");
    assert_eq!(log.dropped(), 0);

    paused.resume();
    assert!(
        log.flush_written(),
        "the writer recovers after the earlier request timed out"
    );
    let text = fs::read_to_string(log.path()).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(text.contains("synthetic queued warning"), "{text}");
    assert_eq!(log.dropped(), 0);
}

#[test]
fn the_settings_level_changes_live_unless_teams_app_log_overrides_it() {
    let dir = tempfile::tempdir().unwrap();
    let log = FileLog::open(&config(dir.path(), Level::Warn, None)).unwrap();
    assert_eq!(log.level(), Level::Warn);
    record(&log, LogLevel::Info, "teams_core::engine", "info at warn");
    assert_eq!(log.apply_setting(Level::Debug), Level::Debug);
    record(&log, LogLevel::Debug, "teams_core::engine", "debug at debug");
    let text = read(&log);
    assert!(
        !text.contains("info at warn") && text.contains("debug at debug"),
        "{text}"
    );

    let overridden = tempfile::tempdir().unwrap();
    let log = FileLog::open(&config(overridden.path(), Level::Info, Some("trace"))).unwrap();
    assert_eq!(log.overridden(), Some(Level::Debug), "trace is debug");
    assert_eq!(log.apply_setting(Level::Error), Level::Debug, "the variable wins");
    assert_eq!(log.level(), Level::Debug);
}

#[test]
fn levels_parse_from_the_settings_and_the_variable() {
    assert_eq!(Level::default(), Level::Info);
    for level in Level::ALL {
        assert_eq!(Level::parse(level.key()), Some(level));
    }
    assert_eq!(Level::parse(" DEBUG "), Some(Level::Debug));
    assert_eq!(Level::parse("trace"), Some(Level::Debug));
    assert_eq!(Level::parse("loud"), None);
    assert_eq!(effective(Level::Info, Some("warn")), Level::Warn);
    assert_eq!(effective(Level::Info, Some("nonsense")), Level::Info);
    assert_eq!(effective(Level::Error, None), Level::Error);
    assert_eq!(teams_log::ENV, "TEAMS_APP_LOG");
}

#[test]
fn a_line_is_one_line_with_its_time_level_and_target() {
    let at = UNIX_EPOCH + Duration::from_millis(1_792_663_200_123);
    let line = format_line(at, LogLevel::Info, "teams_core::engine", "two\nlines\u{1b}[2J");
    assert_eq!(
        line,
        "2026-10-22T10:00:00.123Z INFO  teams_core::engine: two\\nlines\\u{1b}[2J"
    );
    assert_eq!(
        format_line(UNIX_EPOCH, LogLevel::Debug, "teams_log", "x"),
        "1970-01-01T00:00:00.000Z DEBUG teams_log: x"
    );
}

#[test]
fn log_is_capped_at_debug_at_compile_time() {
    assert_eq!(log::STATIC_MAX_LEVEL, log::LevelFilter::Debug);
}
