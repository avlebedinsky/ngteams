//! The middle tier's calendar view (`docs/protocol/calendar.md`): `GET /api/mt/part/<partition>/v2.0/me/calendars/
//! default/calendarView?StartDate=<ISO>&EndDate=<ISO>&shouldDecryptData=true`, under the middle-tier base
//! [`crate::MIDDLE_TIER_BASE`], taking the account's token alone. Its answer is `{"type": …, "value": [<event>…]}`
//! with the same made-up working week for every account, repeated every week: each weekday's events at fixed UTC
//! times, a recurring series' occurrences among them, a cancelled one, two that overlap, an all-day one and a Teams
//! meeting with its join link; every event that starts inside the window asked for. A request without a token the mock
//! handed out is a 401; another path or query under the base, or a window that doesn't read, a 404.

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::{json as answer, status};
use crate::world::State;

/// The middle tier's partition in [`crate::MIDDLE_TIER_BASE`].
pub(crate) const PARTITION: &str = "amer-03";

const DAY_MS: i64 = 24 * 3_600_000;
const HOUR_MS: i64 = 3_600_000;

/// One event of the made-up week: its weekday (0 Monday), start and end in minutes of the UTC day, and what it is.
struct Planned {
    weekday: i64,
    start: i64,
    end: i64,
    subject: &'static str,
    organizer: &'static str,
    series: Option<&'static str>,
    cancelled: bool,
    response: &'static str,
    online: bool,
    all_day: bool,
}

const fn planned(weekday: i64, start: i64, end: i64, subject: &'static str, organizer: &'static str) -> Planned {
    Planned {
        weekday,
        start,
        end,
        subject,
        organizer,
        series: None,
        cancelled: false,
        response: "Accepted",
        online: true,
        all_day: false,
    }
}

const fn recurring(mut event: Planned, series: &'static str) -> Planned {
    event.series = Some(series);
    event
}

const fn cancelled(mut event: Planned) -> Planned {
    event.cancelled = true;
    event
}

const fn answered(mut event: Planned, response: &'static str) -> Planned {
    event.response = response;
    event
}

const fn all_day(mut event: Planned) -> Planned {
    event.all_day = true;
    event.online = false;
    event
}

/// The made-up week (organizers made up too).
const WEEK: [Planned; 15] = [
    answered(
        recurring(planned(0, 930, 960, "Weekly sync", "Avery Example"), "sync"),
        "Tentative",
    ),
    cancelled(recurring(
        planned(0, 1050, 1065, "Team standup", "Jordan Sample"),
        "standup",
    )),
    planned(0, 1200, 1245, "Product all hands", "Riley Placeholder"),
    recurring(
        planned(1, 1050, 1080, "App migration review", "Casey Madeup"),
        "migration",
    ),
    recurring(planned(1, 1050, 1065, "Team standup", "Jordan Sample"), "standup"),
    answered(
        recurring(planned(2, 840, 900, "Code review", "Lab Account A"), "review"),
        "Organizer",
    ),
    answered(
        recurring(planned(2, 930, 960, "Weekly sync", "Avery Example"), "sync"),
        "Tentative",
    ),
    recurring(planned(2, 1050, 1065, "Team standup", "Jordan Sample"), "standup"),
    all_day(planned(2, 0, 1440, "Release day", "Lab Account A")),
    recurring(
        planned(3, 1050, 1080, "App migration review", "Casey Madeup"),
        "migration",
    ),
    answered(
        recurring(planned(4, 930, 960, "Weekly sync", "Avery Example"), "sync"),
        "Tentative",
    ),
    cancelled(planned(4, 1020, 1040, "Engineering lookback", "Morgan Fictional")),
    recurring(planned(4, 1050, 1065, "Platform sync", "Morgan Fictional"), "platform"),
    planned(4, 1080, 1140, "Weekly roundup", "Avery Example"),
    answered(planned(3, 600, 630, "Design crit", "Riley Placeholder"), "NotResponded"),
];

/// The answer to `request` when it is under the middle-tier base; `None` for any other path.
pub(crate) fn calendar_view(state: &State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let rest = match segments.as_slice() {
        [api, mt, part, partition, rest @ ..]
            if api == "api" && mt == "mt" && part == "part" && partition == PARTITION =>
        {
            rest
        }
        _ => return None,
    };
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token);
    if token.and_then(|token| state.account_by_token(token)).is_none() {
        return Some(status(401));
    }
    let query: Vec<(String, String)> = request.url.query_pairs().into_owned().collect();
    let window = match query.as_slice() {
        [(start_name, start), (end_name, end), (decrypt, yes)]
            if start_name == "StartDate"
                && end_name == "EndDate"
                && decrypt == "shouldDecryptData"
                && yes == "true" =>
        {
            iso_ms(start).zip(iso_ms(end))
        }
        _ => None,
    };
    let the_read = request.method.as_str() == "GET" && rest == ["v2.0", "me", "calendars", "default", "calendarView"];
    Some(match window {
        Some((start, end)) if the_read && start < end => answer(
            200,
            &json!({
                "type": "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent",
                "value": events(start, end),
            }),
        ),
        _ => status(404),
    })
}

/// The made-up week's events that start in `[start, end)`.
fn events(start: i64, end: i64) -> Vec<Value> {
    let mut out = Vec::new();
    let mut day = start.div_euclid(DAY_MS) * DAY_MS;
    while day < end {
        let weekday = (day.div_euclid(DAY_MS) + 3).rem_euclid(7);
        for (index, event) in WEEK.iter().enumerate().filter(|(_, event)| event.weekday == weekday) {
            let begins = day + event.start * 60_000;
            if (start..end).contains(&begins) {
                out.push(event_json(event, index, day));
            }
        }
        day += DAY_MS;
    }
    out
}

fn event_json(event: &Planned, index: usize, day: i64) -> Value {
    let id = format!("AAMkMadeUpEvent{:04}{index:02}", day.div_euclid(DAY_MS) % 10_000);
    let thread = format!("19:meeting_MadeUpMeeting{index:02}@thread.v2");
    let link = format!(
        "https://teams.microsoft.com/l/meetup-join/{}/0?context=%7b%7d",
        thread.replace(':', "%3a").replace('@', "%40")
    );
    let subject = if event.cancelled {
        format!("Canceled: {}", event.subject)
    } else {
        event.subject.to_owned()
    };
    let (start, end) = if event.all_day {
        (day, day + DAY_MS)
    } else {
        (day + event.start * 60_000, day + event.end * 60_000)
    };
    let mut value = json!({
        "objectId": id,
        "subject": subject,
        "startTime": iso(start),
        "endTime": iso(end),
        "isAllDayEvent": event.all_day,
        "eventType": if event.series.is_some() { "Occurrence" } else { "Single" },
        "isCancelled": event.cancelled,
        "isOnlineMeeting": event.online,
        "isOrganizer": event.response == "Organizer",
        "myResponseType": event.response,
        "showAs": if event.response == "Tentative" { "Tentative" } else { "Busy" },
        "organizerName": event.organizer,
        "organizerAddress": "someone@example.com",
        "location": if event.online { "Microsoft Teams Meeting" } else { "" },
        "isPrivate": false,
        "attendees": [
            {"name": event.organizer, "address": "someone@example.com"},
            {"name": "Lab Account A", "address": "a@example.com"},
        ],
    });
    if event.online {
        value["skypeTeamsMeetingUrl"] = Value::String(link);
        value["skypeTeamsData"] = Value::String(json!({"cid": thread, "type": 0}).to_string());
    }
    value
}

/// `ms` as the service writes an event's time: UTC with seven fraction digits.
fn iso(ms: i64) -> String {
    let days = ms.div_euclid(DAY_MS);
    let rest = ms.rem_euclid(DAY_MS);
    let (year, month, day) = civil(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.0000000Z",
        rest / HOUR_MS,
        rest % HOUR_MS / 60_000,
        rest % 60_000 / 1000
    )
}

/// `2026-10-26T00:00:00.000Z` (the window's ends) in ms since the epoch.
fn iso_ms(text: &str) -> Option<i64> {
    let text = text.strip_suffix('Z')?;
    let (date, clock) = text.split_once('T')?;
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut clock = clock.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (clock.next()?.ok()?, clock.next()?.ok()?, clock.next()?.ok()?);
    Some(days_from_civil(year, month, day) * DAY_MS + hour * HOUR_MS + minute * 60_000 + second * 1000)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_era_days - 719_468
}

/// The date `days` after 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let of_era = days - era * 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * of_year + 2) / 153;
    let day = of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{events, iso, iso_ms};

    #[test]
    fn times_round_trip() {
        let ms = iso_ms("2026-10-26T00:00:00.000Z").unwrap();
        assert_eq!(iso(ms), "2026-10-26T00:00:00.0000000Z");
        assert_eq!(iso(ms + 90 * 60_000), "2026-10-26T01:30:00.0000000Z");
    }

    #[test]
    fn a_week_holds_the_whole_made_up_week() {
        let monday = iso_ms("2026-10-26T00:00:00.000Z").unwrap();
        assert_eq!(events(monday, monday + 7 * super::DAY_MS).len(), super::WEEK.len());
        assert!(events(monday + 5 * super::DAY_MS, monday + 7 * super::DAY_MS).is_empty());
    }
}
