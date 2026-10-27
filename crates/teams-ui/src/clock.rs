//! How the app shows a time: every time a view shows (a message's header, a chat list row's time, a search hit's, a
//! channel post's) is made here, on the clock the settings' "Time format" gives ([`TimeFormat`]).
//!
//! - **The clock**: 12-hour ("3:30 PM") or 24-hour ("15:30"). "Follow the system" takes the locale the environment
//!   names for times, as the C library reads it: the first of `LC_ALL`, `LC_TIME` and `LANG` that is set and not
//!   empty. Its territory decides ([`locale_cycle`]): the territories whose clock CLDR's `timeData` prefers 12-hour
//!   (the United States, Canada in English, Australia, India, the Arab countries, Korea, Taiwan, …) give 12-hour,
//!   every other one (and `C`, `POSIX`, no locale at all) 24-hour. Read from the environment alone: no locale
//!   database, no dependency.
//! - **The wording** is Teams', the same on either clock: the time alone today, "Yesterday" and the weekday within the
//!   week, else the day (and the year, if not this one) before it ([`message_time`]); a list row shows the time only
//!   today, else the short weekday or day ([`list_time`]).
//! - **Live**: [`follow_settings`] keeps the clock the settings give as a GPUI global and redraws every window when it
//!   changes; a view reads it with [`cycle`] as it draws.

use chrono::{DateTime, Datelike as _, TimeZone};
use gpui::{App, Entity, Global, Subscription};

use crate::settings::{SettingsModel, TimeFormat};

/// The clock a time of day is shown on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HourCycle {
    /// "3:30 PM".
    Twelve,
    /// "15:30".
    TwentyFour,
}

impl HourCycle {
    /// The time of day's `chrono` format.
    fn time_of_day(self) -> &'static str {
        match self {
            Self::Twelve => "%-I:%M %p",
            Self::TwentyFour => "%H:%M",
        }
    }
}

/// The territories (ISO 3166 codes, as a locale names them) whose clock is 12-hour: those CLDR's `timeData` gives a
/// preferred `h`. In order: the Americas and Oceania, South and South-East Asia, the Arab countries, East Asia.
const TWELVE_HOUR_TERRITORIES: &[&str] = &[
    "US", "CA", "PR", "GU", "VI", "AS", "MP", "UM", "AU", "NZ", "JM", "BS", "BB", "AG", "TT", "GY", "LR", "IN", "PK",
    "BD", "PH", "MY", "SG", "AE", "BH", "DZ", "EG", "IQ", "JO", "KW", "LB", "LY", "MR", "OM", "PS", "QA", "SA", "SD",
    "SY", "TN", "YE", "KR", "TW", "HK",
];

/// The locale the environment names for times: the first of `LC_ALL`, `LC_TIME` and `LANG` that `var` gives and isn't
/// empty, as the C library takes them.
#[must_use]
pub fn time_locale(var: impl Fn(&str) -> Option<String>) -> Option<String> {
    ["LC_ALL", "LC_TIME", "LANG"]
        .into_iter()
        .filter_map(var)
        .find(|locale| !locale.is_empty())
}

/// The clock `locale` (`language[_TERRITORY][.codeset][@modifier]`) uses: 12-hour in a territory of
/// [`TWELVE_HOUR_TERRITORIES`] (Canada only in English) and for English without one (CLDR's `en` is American),
/// 24-hour otherwise, `C` and `POSIX` included.
#[must_use]
pub fn locale_cycle(locale: &str) -> HourCycle {
    let name = locale.split(['.', '@']).next().unwrap_or_default();
    let (language, territory) = name.split_once('_').unwrap_or((name, ""));
    let twelve = match territory {
        "" | "CA" => language == "en",
        territory => TWELVE_HOUR_TERRITORIES.contains(&territory),
    };
    if twelve {
        HourCycle::Twelve
    } else {
        HourCycle::TwentyFour
    }
}

/// The clock the system's locale uses ([`time_locale`] from the process's environment); 24-hour without one.
#[must_use]
pub fn system_cycle() -> HourCycle {
    time_locale(|name| std::env::var(name).ok()).map_or(HourCycle::TwentyFour, |locale| locale_cycle(&locale))
}

/// The clock `format` gives, the system's for [`TimeFormat::System`].
#[must_use]
pub fn resolve(format: TimeFormat) -> HourCycle {
    match format {
        TimeFormat::System => system_cycle(),
        TimeFormat::Twelve => HourCycle::Twelve,
        TimeFormat::TwentyFour => HourCycle::TwentyFour,
    }
}

/// A message's time in its header (a chat's or a channel's), as Teams words it: the time of day today; "Yesterday",
/// the weekday within the week, else the day (and the year, if not this one) before it.
#[must_use]
pub fn message_time<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>, cycle: HourCycle) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let at = at.with_timezone(&now.timezone());
    let days = (now.date_naive() - at.date_naive()).num_days();
    let day = match days {
        0 => "",
        1 => "Yesterday ",
        2..7 => "%A ",
        _ if at.year() == now.year() => "%-d %B ",
        _ => "%-d %B %Y ",
    };
    at.format(&format!("{day}{}", cycle.time_of_day())).to_string()
}

/// A list row's time (a chat's, a search hit's): the time of day today, the short weekday within the week, else the
/// day (and the year, if not this one).
#[must_use]
pub fn list_time<Tz: TimeZone>(ms: i64, now: &DateTime<Tz>, cycle: HourCycle) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let Some(at) = DateTime::from_timestamp_millis(ms) else {
        return String::new();
    };
    let at = at.with_timezone(&now.timezone());
    let days = (now.date_naive() - at.date_naive()).num_days();
    let format = match days {
        0 => cycle.time_of_day(),
        1..7 => "%a",
        _ if at.year() == now.year() => "%-d %b",
        _ => "%-d %b %Y",
    };
    at.format(format).to_string()
}

/// The clock windows show times on now.
struct Current(HourCycle);

impl Global for Current {}

/// Shows times on the clock `format` gives from now on and redraws every window when that is another clock.
pub fn set(format: TimeFormat, cx: &mut App) {
    let cycle = resolve(format);
    if cx.try_global::<Current>().is_some_and(|current| current.0 == cycle) {
        return;
    }
    cx.set_global(Current(cycle));
    cx.refresh_windows();
}

/// The clock windows show times on now: the settings' once [`follow_settings`] runs, the system's before.
#[must_use]
pub fn cycle(cx: &App) -> HourCycle {
    cx.try_global::<Current>()
        .map_or_else(system_cycle, |current| current.0)
}

/// Applies the settings' [`TimeFormat`] now and again each time the settings change, for as long as the returned
/// subscription is kept.
pub fn follow_settings(settings: &Entity<SettingsModel>, cx: &mut App) -> Subscription {
    set(settings.read(cx).settings().time_format, cx);
    cx.observe(settings, |settings, cx| {
        set(settings.read(cx).settings().time_format, cx);
    })
}
