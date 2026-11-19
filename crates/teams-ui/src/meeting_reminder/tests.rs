use super::*;

const START: i64 = 1_800_000_000_000;
const END: i64 = START + 30 * MINUTE_MS;

#[test]
fn a_meeting_is_reminded_from_its_reminder_time_and_again_as_it_starts() {
    let at = |minutes: i64, shown| stage_at(START, END, 15, START + minutes * MINUTE_MS, shown);
    assert_eq!(at(-16, false), None);
    assert_eq!(at(-15, false), Some(Stage::Before));
    // a run started after the reminder time but before the start reminds at once
    assert_eq!(at(-3, false), Some(Stage::Before));
    assert_eq!(at(0, true), Some(Stage::Start));
    assert_eq!(at(9, false), Some(Stage::Start));
    // not reminded yet and well past its start: none; reminded, its start's notice lasts until it ends
    assert_eq!(at(10, false), None);
    assert_eq!(at(20, true), Some(Stage::Start));
    assert_eq!(at(30, true), None);
}

#[test]
fn a_reminder_at_the_start_shows_only_the_start() {
    assert_eq!(stage_at(START, END, 0, START - MINUTE_MS, false), None);
    assert_eq!(stage_at(START, END, 0, START, false), Some(Stage::Start));
}

#[test]
fn an_unknown_reminder_is_outlook_s_default_and_an_off_one_none() {
    assert_eq!(minutes_of(EventReminder::Before(5)), Some(5));
    assert_eq!(minutes_of(EventReminder::Unknown), Some(DEFAULT_MINUTES));
    assert_eq!(minutes_of(EventReminder::Off), None);
}

#[test]
fn the_notice_says_how_long_until_it_starts() {
    assert_eq!(when_text(START, START - 15 * MINUTE_MS), "Starts in 15 minutes");
    assert_eq!(when_text(START, START - 14 * MINUTE_MS - 1), "Starts in 15 minutes");
    assert_eq!(when_text(START, START - 30_000), "Starts in 1 minute");
    assert_eq!(when_text(START, START), "Starting now");
    assert_eq!(when_text(START, START + 5 * MINUTE_MS), "Starting now");
    assert_eq!(snooze_label(0), "Until it starts");
    assert_eq!(snooze_label(1), "1 minute");
    assert_eq!(snooze_label(10), "10 minutes");
}
