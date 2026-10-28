//! How the app shows a time: Teams' wording on either clock, across the day's boundaries; the system's clock read
//! from the locale the environment names; the settings' choice applied live.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use chrono::{DateTime, FixedOffset, TimeZone as _};
use gpui::{AppContext as _, TestAppContext};
use teams_ui::clock::{self, HourCycle, list_time, locale_cycle, message_time, time_locale};
use teams_ui::settings::{Settings, SettingsModel, TimeFormat};

/// Wednesday 21 October 2026, 15:00 at UTC+3.
fn now() -> DateTime<FixedOffset> {
    zone().with_ymd_and_hms(2026, 10, 21, 15, 0, 0).single().unwrap()
}

fn zone() -> FixedOffset {
    FixedOffset::east_opt(3 * 3600).unwrap()
}

/// The time in milliseconds of a local time at UTC+3.
fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
    zone()
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .unwrap()
        .timestamp_millis()
}

#[test]
fn a_message_s_time_keeps_teams_wording_on_either_clock() {
    let now = now();
    let cases = [
        // today, from its first minute
        (at(2026, 10, 21, 0, 0), "12:00 AM", "00:00"),
        (at(2026, 10, 21, 9, 5), "9:05 AM", "09:05"),
        (at(2026, 10, 21, 12, 30), "12:30 PM", "12:30"),
        // yesterday, to its last minute
        (at(2026, 10, 20, 23, 59), "Yesterday 11:59 PM", "Yesterday 23:59"),
        (at(2026, 10, 20, 0, 0), "Yesterday 12:00 AM", "Yesterday 00:00"),
        // within the week: the weekday, up to six days back
        (at(2026, 10, 19, 15, 30), "Monday 3:30 PM", "Monday 15:30"),
        (at(2026, 10, 15, 8, 0), "Thursday 8:00 AM", "Thursday 08:00"),
        // a week back and more: the day, the year only when not this one
        (at(2026, 10, 14, 15, 30), "14 October 3:30 PM", "14 October 15:30"),
        (
            at(2025, 12, 31, 23, 0),
            "31 December 2025 11:00 PM",
            "31 December 2025 23:00",
        ),
    ];
    for (ms, twelve, twenty_four) in cases {
        assert_eq!(message_time(ms, &now, HourCycle::Twelve), twelve);
        assert_eq!(message_time(ms, &now, HourCycle::TwentyFour), twenty_four);
    }
}

#[test]
fn the_day_is_the_viewer_s_own() {
    // 22:30 UTC on the 20th is already the 21st at UTC+3: today, not yesterday
    let late = chrono::Utc
        .with_ymd_and_hms(2026, 10, 20, 22, 30, 0)
        .single()
        .unwrap()
        .timestamp_millis();
    assert_eq!(message_time(late, &now(), HourCycle::Twelve), "1:30 AM");
    let utc = now().with_timezone(&chrono::Utc);
    assert_eq!(message_time(late, &utc, HourCycle::Twelve), "Yesterday 10:30 PM");
}

#[test]
fn a_list_row_shows_the_time_only_today() {
    let now = now();
    let cases = [
        (at(2026, 10, 21, 9, 5), "9:05 AM", "09:05"),
        (at(2026, 10, 21, 23, 59), "11:59 PM", "23:59"),
        (at(2026, 10, 20, 23, 59), "Tue", "Tue"),
        (at(2026, 10, 19, 9, 5), "Mon", "Mon"),
        (at(2026, 10, 1, 9, 5), "1 Oct", "1 Oct"),
        (at(2025, 12, 31, 9, 5), "31 Dec 2025", "31 Dec 2025"),
    ];
    for (ms, twelve, twenty_four) in cases {
        assert_eq!(list_time(ms, &now, HourCycle::Twelve), twelve);
        assert_eq!(list_time(ms, &now, HourCycle::TwentyFour), twenty_four);
    }
}

#[test]
fn a_time_that_can_t_be_read_shows_nothing() {
    assert_eq!(message_time(i64::MAX, &now(), HourCycle::Twelve), "");
    assert_eq!(list_time(i64::MAX, &now(), HourCycle::TwentyFour), "");
    assert_eq!(clock::full_time(i64::MAX, &zone(), HourCycle::Twelve), "");
}

#[test]
fn a_forwarded_message_s_original_time_is_shown_in_full_as_teams_does() {
    let morning = at(2026, 10, 28, 2, 49);
    assert_eq!(
        clock::full_time(morning, &zone(), HourCycle::Twelve),
        "10/28/2026 2:49 AM"
    );
    assert_eq!(
        clock::full_time(morning, &zone(), HourCycle::TwentyFour),
        "10/28/2026 02:49"
    );
    // in the viewer's own zone, whatever day it is now
    let utc = FixedOffset::east_opt(0).unwrap();
    assert_eq!(
        clock::full_time(morning, &utc, HourCycle::Twelve),
        "10/27/2026 11:49 PM"
    );
    assert_eq!(
        clock::full_time(at(2025, 1, 5, 17, 36), &zone(), HourCycle::Twelve),
        "1/5/2025 5:36 PM"
    );
}

#[test]
fn the_system_s_clock_comes_from_the_locale_s_territory() {
    for twelve in [
        "en_US.UTF-8",
        "en_US",
        "en_CA.UTF-8",
        "en_AU.utf8",
        "en_IN",
        "hi_IN.UTF-8",
        "ar_EG.UTF-8",
        "ko_KR.UTF-8",
        "zh_TW.UTF-8",
        "es_US.UTF-8",
        "en",
    ] {
        assert_eq!(locale_cycle(twelve), HourCycle::Twelve, "{twelve}");
    }
    for twenty_four in [
        "en_GB.UTF-8",
        "en_IE.UTF-8",
        "de_DE.UTF-8",
        "de_DE@euro",
        "fr_CA.UTF-8",
        "ru_RU.UTF-8",
        "ja_JP.UTF-8",
        "zh_CN.UTF-8",
        "C",
        "C.UTF-8",
        "POSIX",
        "de",
        "",
    ] {
        assert_eq!(locale_cycle(twenty_four), HourCycle::TwentyFour, "{twenty_four}");
    }
}

#[test]
fn the_locale_for_times_is_lc_all_then_lc_time_then_lang() {
    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name: &str| {
            vars.iter()
                .find(|(var, _)| *var == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }
    let all = [
        ("LC_ALL", "de_DE.UTF-8"),
        ("LC_TIME", "en_US.UTF-8"),
        ("LANG", "ru_RU.UTF-8"),
    ];
    assert_eq!(time_locale(env(&all)).as_deref(), Some("de_DE.UTF-8"));
    let time = [("LC_ALL", ""), ("LC_TIME", "en_US.UTF-8"), ("LANG", "ru_RU.UTF-8")];
    assert_eq!(
        time_locale(env(&time)).as_deref(),
        Some("en_US.UTF-8"),
        "an empty one is skipped"
    );
    let lang = [("LANG", "en_GB.UTF-8")];
    assert_eq!(time_locale(env(&lang)).as_deref(), Some("en_GB.UTF-8"));
    assert_eq!(time_locale(env(&[])), None);
}

#[gpui::test]
fn the_settings_choice_is_applied_at_once(cx: &mut TestAppContext) {
    let model = cx.update(|cx| {
        cx.new(|_| {
            SettingsModel::in_memory(Settings {
                time_format: TimeFormat::Twelve,
                ..Settings::default()
            })
        })
    });
    let _following = cx.update(|cx| clock::follow_settings(&model, cx));
    assert_eq!(cx.read(clock::cycle), HourCycle::Twelve);
    let choose = |format, cx: &mut TestAppContext| {
        cx.update(|cx| model.update(cx, |model, cx| model.change(|s| s.time_format = format, cx)));
    };
    choose(TimeFormat::TwentyFour, cx);
    assert_eq!(cx.read(clock::cycle), HourCycle::TwentyFour);
    choose(TimeFormat::System, cx);
    assert_eq!(cx.read(clock::cycle), clock::system_cycle());
}
