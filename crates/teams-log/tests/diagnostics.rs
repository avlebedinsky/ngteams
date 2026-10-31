//! "Copy diagnostics": the version and build profile, the level (and whether `TEAMS_APP_LOG` set it), the current
//! log's last lines (capped), the recent `fence.log` lines; when a `debug` line is among them the text says so first.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::fmt::Write as _;
use std::fs;

use teams_log::Level;
use teams_log::diagnostics::{
    LevelState, MAX_FENCE_LINES, MAX_LOG_BYTES, MAX_LOG_LINES, Source, WARNING, build_profile, is_debug,
};

fn source(dir: &tempfile::TempDir, log: &str, fence: &str) -> Source {
    let log_file = dir.path().join("teams-app.log");
    let fence_log = dir.path().join("fence.log");
    fs::write(&log_file, log).unwrap();
    fs::write(&fence_log, fence).unwrap();
    Source {
        app: "teams-app 0.1.0".to_owned(),
        log_file: Some(log_file),
        log_problem: None,
        fence_log: Some(fence_log),
    }
}

fn lines(count: usize, level: &str, what: &str) -> String {
    let mut text = String::new();
    for number in 0..count {
        let _ = writeln!(
            text,
            "2026-10-22T08:00:00.000Z {level} teams_core::engine: {what} {number}"
        );
    }
    text
}

fn fence_lines(count: usize) -> String {
    let mut text = String::new();
    for number in 0..count {
        let _ = writeln!(text, r#"{{"ts_ms":{number},"verdict":"allow","kind":"message_post"}}"#);
    }
    text
}

#[test]
fn the_text_holds_the_version_the_level_and_the_last_lines_capped() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(&dir, &lines(300, "INFO ", "row"), &fence_lines(60));
    let report = source.collect_with(LevelState {
        level: Level::Info,
        overridden: false,
    });
    assert!(!report.has_debug);
    let text = &report.text;
    assert!(!text.contains(WARNING), "{text}");
    assert!(
        text.starts_with(&format!(
            "teams-app 0.1.0 ({} build)\nlog level: info\n",
            build_profile()
        )),
        "{text}"
    );
    assert!(text.contains(&format!("log file: {}", dir.path().join("teams-app.log").display())));
    assert!(
        text.contains(&format!("== the log: the last {MAX_LOG_LINES} line(s) ==")),
        "{text}"
    );
    assert!(
        text.contains("row 299") && text.contains("row 100") && !text.contains("row 99\n"),
        "{text}"
    );
    assert!(text.contains(&format!("== fence.log: the last {MAX_FENCE_LINES} line(s) ==")));
    assert!(
        text.contains(r#""ts_ms":59,"#) && !text.contains(r#""ts_ms":19,"#),
        "{text}"
    );
}

#[test]
fn a_debug_line_makes_the_text_say_first_that_it_isn_t_safe_to_paste() {
    let dir = tempfile::tempdir().unwrap();
    let log = format!(
        "{}{}",
        lines(3, "INFO ", "row"),
        lines(1, "DEBUG", "response GET https://example.invalid/ -> 200")
    );
    let source = source(&dir, &log, "");
    let report = source.collect_with(LevelState {
        level: Level::Debug,
        overridden: true,
    });
    assert!(report.has_debug);
    assert!(
        report
            .text
            .starts_with("WARNING: contains full requests and responses: not safe to paste\n"),
        "{}",
        report.text
    );
    assert!(report.text.contains("log level: debug (set by TEAMS_APP_LOG)"));
    assert!(is_debug("2026-10-22T08:00:00.000Z DEBUG teams_proto::http: x"));
    assert!(!is_debug("2026-10-22T08:00:00.000Z INFO  teams_proto::http: DEBUG"));
}

#[test]
fn a_newest_line_longer_than_the_cap_is_kept_cut_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    // words, not one opaque run the last pass would take as a token
    let body = "word ".repeat(MAX_LOG_BYTES / 2);
    let log = format!(
        "{}2026-10-22T08:00:00.000Z DEBUG teams_wire::http: response GET https://example.invalid/ -> 200 {body}\n",
        lines(3, "INFO ", "row")
    );
    let source = source(&dir, &log, "");
    let report = source.collect_with(LevelState {
        level: Level::Debug,
        overridden: false,
    });
    let text = &report.text;
    assert!(report.has_debug, "the line's level is still read");
    assert!(text.starts_with("WARNING: "), "{}", &text[..text.len().min(400)]);
    assert!(
        text.contains("== the log: the last 1 line(s) =="),
        "{}",
        &text[..text.len().min(400)]
    );
    assert!(
        text.contains("DEBUG teams_wire::http: response GET https://example.invalid/ -> 200 word word"),
        "{}",
        &text[..text.len().min(400)]
    );
    assert!(text.contains("more bytes cut]\n"));
    assert!(!text.contains("row 2"), "the older lines don't fit");
    assert!(text.len() < MAX_LOG_BYTES + 4096, "{}", text.len());

    // one longer than the part of the file read is said, never written from its middle
    let huge = format!(
        "2026-10-22T08:00:00.000Z DEBUG teams_wire::http: {}\n",
        "y".repeat(1_100_000)
    );
    let source = self::source(&dir, &format!("{}{huge}", lines(3, "INFO ", "row")), "");
    let report = source.collect_with(LevelState {
        level: Level::Debug,
        overridden: false,
    });
    assert!(
        report
            .text
            .contains("== the log: the last 1 line(s) ==\n[the newest line is longer than 1024 KiB: left out]\n"),
        "{}",
        report.text
    );
    assert!(!report.text.contains("yyy"));
}

#[test]
fn a_log_not_written_is_said_and_a_missing_file_has_no_lines() {
    let dir = tempfile::tempdir().unwrap();
    let source = Source {
        app: "teams-app 0.1.0".to_owned(),
        log_file: None,
        log_problem: Some("the diagnostic log can't be opened".to_owned()),
        fence_log: Some(dir.path().join("missing.log")),
    };
    let report = source.collect_with(LevelState {
        level: Level::Info,
        overridden: false,
    });
    assert!(
        report
            .text
            .contains("log: not written (the diagnostic log can't be opened)")
    );
    assert!(report.text.contains("== fence.log: the last 0 line(s) =="));
}

#[test]
fn an_unreadable_log_is_reported_without_losing_the_other_section() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = source(&dir, "", &fence_lines(1));
    // Opening a directory succeeds on Unix, but reading it as a file fails independently of permission or uid.
    source.log_file = Some(dir.path().to_owned());

    let report = source.collect_with(LevelState {
        level: Level::Info,
        overridden: false,
    });

    assert!(!report.has_debug);
    assert!(report.text.contains("== the log: can't be read ("), "{}", report.text);
    assert!(
        report.text.contains("== fence.log: the last 1 line(s) =="),
        "{}",
        report.text
    );
    assert!(report.text.contains(r#""ts_ms":0,"#), "{}", report.text);
}
