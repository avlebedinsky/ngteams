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
