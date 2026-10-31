//! The scheduling form's free and busy reads (`docs/protocol/calendar.md`, "The Scheduler"), as the web client sends
//! them while its form names attendees (`ab-meeting`, 2026-10-30): two reads sent as a `PUT` on the middle tier, the
//! people asked about and the window in the body. They reach nobody, whoever they name: plain requests.
//!
//! - [`MeetingTimes`]: `PUT v2.0/me/calendars/findMeetingTimes`, the times every attendee is free for the meeting's
//!   length (`requestType: "SuggestedTimesOnly"`, two days from the window's start), answered with
//!   `{"timeSuggestions": [{"meetingTime": <ISO>, "confidence": <0-100>}]}`.
//! - [`Schedules`]: `PUT v2.0/me/calendars/schedules`, each attendee's busy times (`requestType:
//!   "AttendeeAvailabilityOnly"`, five days), answered with `[{"userPrincipalName", "timeZone"?, "freeBusyEvents":
//!   [{"freeBusyStatus", "startTime", "endTime", "details": {"subject"?, "location"?, "isPrivate"?}}]}]`; the details
//!   only for the account's own events.
//!
//! The body (both): `{"attendees": [<attendee>…, <organizer>], "minimumSuggestionQuality": "Good", "numberOfDays",
//! "requestType", "startDate", "startTime", "startDateTimeZone": "Utc", "meetingDurationInMinutes", "endTime"}`, each
//! attendee as an event lists it (`{"type", "address", "name", "mri"?, "role": "Admin"}`), the organizer last, the
//! window's ends in UTC with milliseconds (the start the local midnight of its first day).

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::endpoint::{Endpoint, Method, Service};
use crate::request::Request;
use crate::write::{EventAttendee, Organizer};

const DAY_MS: i64 = 24 * 3_600_000;

/// How many days a suggestion read covers, as the web client asks.
pub const SUGGESTION_DAYS: u32 = 2;

/// How many days an availability read covers, as the web client asks.
pub const AVAILABILITY_DAYS: u32 = 5;

/// What both reads ask about: whom, from when, for how many days, for a meeting how long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreeBusyAsk {
    pub attendees: Vec<EventAttendee>,
    pub organizer: Organizer,
    /// The window's start, ms since the epoch (the local midnight of its first day).
    pub start_ms: i64,
    pub meeting_minutes: u32,
}

impl FreeBusyAsk {
    fn body(&self, request_type: &str, days: u32) -> Option<Value> {
        let mut attendees: Vec<Value> = self
            .attendees
            .iter()
            .map(|attendee| {
                let mut entry = json!({
                    "type": if attendee.required { "Required" } else { "Optional" },
                    "address": attendee.address,
                    "name": attendee.name.clone().unwrap_or_else(|| attendee.address.clone()),
                    "role": "Admin",
                });
                if let Some(mri) = &attendee.mri {
                    entry["mri"] = json!(mri);
                }
                entry
            })
            .collect();
        let mut organizer = json!({"type": "Organizer", "role": "Admin", "mri": self.organizer.mri});
        if let Some(address) = &self.organizer.address {
            organizer["address"] = json!(address);
        }
        if let Some(name) = &self.organizer.name {
            organizer["name"] = json!(name);
        }
        attendees.push(organizer);
        let start = utc(self.start_ms)?;
        let end = utc(self.start_ms.checked_add(i64::from(days) * DAY_MS)?)?;
        Some(json!({
            "attendees": attendees,
            "minimumSuggestionQuality": "Good",
            "numberOfDays": days,
            "requestType": request_type,
            "startDate": start,
            "startTime": start,
            "startDateTimeZone": "Utc",
            "meetingDurationInMinutes": self.meeting_minutes,
            "endTime": end,
        }))
    }
}

/// `PUT v2.0/me/calendars/findMeetingTimes` (see the module docs): the times every attendee is free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingTimes(pub FreeBusyAsk);

impl MeetingTimes {
    /// The suggested starts the answer `body` lists (ms since the epoch), in its order; `None` unless it is an object
    /// with a `timeSuggestions` array. A suggestion whose time doesn't read is left out.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Vec<i64>> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let suggestions = answer.get("timeSuggestions")?.as_array()?;
        Some(
            suggestions
                .iter()
                .filter_map(|suggestion| instant(suggestion.get("meetingTime")?.as_str()?))
                .collect(),
        )
    }
}

impl Request for MeetingTimes {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Put
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v2.0", "me", "calendars", "findMeetingTimes"])
    }

    fn body(&self) -> Option<Value> {
        self.0.body("SuggestedTimesOnly", SUGGESTION_DAYS)
    }
}

/// `PUT v2.0/me/calendars/schedules` (see the module docs): each attendee's busy times.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Schedules(pub FreeBusyAsk);

/// How a person's time shows in their calendar, a `freeBusyStatus`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FreeBusy {
    Free,
    Tentative,
    Busy,
    /// Out of office.
    Away,
    WorkingElsewhere,
    /// A status no capture showed, as it came.
    Other(String),
}

/// A stretch of someone's calendar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BusyTime {
    pub status: FreeBusy,
    pub start_ms: i64,
    pub end_ms: i64,
    /// Its subject, when the answer gives one (the account's own events).
    pub subject: Option<String>,
}

/// One person's calendar, as the answer gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonSchedule {
    /// Their address (`userPrincipalName`).
    pub address: String,
    pub busy: Vec<BusyTime>,
}

impl Schedules {
    /// The calendars the answer `body` lists; `None` unless it is an array. An entry without an address, or a stretch
    /// whose times don't read, is left out.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Vec<PersonSchedule>> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let entries = answer.as_array().or_else(|| answer.get("value")?.as_array())?;
        Some(
            entries
                .iter()
                .filter_map(|entry| {
                    let address = entry.get("userPrincipalName")?.as_str()?.trim().to_owned();
                    let busy = entry
                        .get("freeBusyEvents")
                        .and_then(Value::as_array)
                        .map(|events| events.iter().filter_map(busy_time).collect())
                        .unwrap_or_default();
                    (!address.is_empty()).then_some(PersonSchedule { address, busy })
                })
                .collect(),
        )
    }
}

impl Request for Schedules {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Put
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v2.0", "me", "calendars", "schedules"])
    }

    fn body(&self) -> Option<Value> {
        self.0.body("AttendeeAvailabilityOnly", AVAILABILITY_DAYS)
    }
}

fn busy_time(event: &Value) -> Option<BusyTime> {
    let status = match event.get("freeBusyStatus").and_then(Value::as_str).unwrap_or_default() {
        "Free" => FreeBusy::Free,
        "Tentative" => FreeBusy::Tentative,
        "Busy" => FreeBusy::Busy,
        "Oof" | "OOF" | "Away" => FreeBusy::Away,
        "WorkingElsewhere" => FreeBusy::WorkingElsewhere,
        other => FreeBusy::Other(other.to_owned()),
    };
    let start_ms = instant(event.get("startTime")?.as_str()?)?;
    let end_ms = instant(event.get("endTime")?.as_str()?)?;
    let subject = event
        .get("details")
        .and_then(|details| details.get("subject"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|subject| !subject.is_empty())
        .map(str::to_owned);
    (end_ms > start_ms).then_some(BusyTime {
        status,
        start_ms,
        end_ms,
        subject,
    })
}

/// An RFC 3339 instant as ms since the epoch.
fn instant(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text.trim(), &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

/// `ms` as the window's ends are written: UTC, milliseconds, `Z`.
fn utc(ms: i64) -> Option<String> {
    let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    Some(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
        time.millisecond()
    ))
}

#[cfg(test)]
mod tests {
    use teams_fence::{Contact, Policy, Request as FenceRequest, contacting};

    use super::*;

    fn ask() -> FreeBusyAsk {
        FreeBusyAsk {
            attendees: vec![EventAttendee {
                address: "bot-b@lab.example.com".to_owned(),
                name: Some("Bot B".to_owned()),
                mri: Some("8:orgid:00000000-0000-4000-8000-00000000000b".to_owned()),
                required: true,
            }],
            organizer: Organizer {
                mri: "8:orgid:00000000-0000-4000-8000-00000000000a".to_owned(),
                address: Some("bot-a@lab.example.com".to_owned()),
                name: Some("Bot A".to_owned()),
            },
            start_ms: 1_793_048_400_000,
            meeting_minutes: 30,
        }
    }

    #[test]
    fn the_reads_go_out_as_the_web_client_sent_them() {
        let body = MeetingTimes(ask()).body().expect("a body");
        assert_eq!(body["requestType"], "SuggestedTimesOnly");
        assert_eq!(body["numberOfDays"], 2);
        assert_eq!(body["startDate"], "2026-10-26T21:00:00.000Z");
        assert_eq!(body["startTime"], body["startDate"]);
        assert_eq!(body["endTime"], "2026-10-28T21:00:00.000Z");
        assert_eq!(body["attendees"][0]["type"], "Required");
        assert_eq!(body["attendees"][1]["type"], "Organizer");
        let body = Schedules(ask()).body().expect("a body");
        assert_eq!(body["requestType"], "AttendeeAvailabilityOnly");
        assert_eq!(body["endTime"], "2026-10-31T21:00:00.000Z");
    }

    #[test]
    fn the_fence_reads_them_as_reaching_nobody() {
        for (method, path, body) in [
            ("PUT", "findMeetingTimes", MeetingTimes(ask()).body()),
            ("PUT", "schedules", Schedules(ask()).body()),
        ] {
            let url = format!("https://teams.microsoft.com/api/mt/part/emea-02/v2.0/me/calendars/{path}");
            let body = body.map(|body| body.to_string().into_bytes());
            let contact = contacting(
                &FenceRequest::new(method, &url).with_body(body.as_deref()),
                &Policy::new(),
            );
            assert_eq!(contact, Contact::None, "{path}");
        }
    }

    #[test]
    fn the_answers_read_as_captured() {
        let suggestions = br#"{"timeSuggestions":[{"meetingTime":"2026-10-30T05:00:00+00:00","confidence":100.0},
            {"meetingTime":"later","confidence":100.0}]}"#;
        assert_eq!(MeetingTimes::read_answer(suggestions), Some(vec![1_793_336_400_000]));
        let schedules = br#"[{"userPrincipalName":"bot-b@lab.example.com","freeBusyEvents":[{"freeBusyStatus":
            "Tentative","startTime":"2026-10-30T07:00:00+00:00","endTime":"2026-10-30T07:30:00+00:00","details":{}}]},
            {"userPrincipalName":"bot-a@lab.example.com","timeZone":"RussianSt","freeBusyEvents":[{"freeBusyStatus":
            "Busy","startTime":"2026-10-30T07:00:00+00:00","endTime":"2026-10-30T07:30:00+00:00","details":
            {"subject":"Planning","isPrivate":false}}]}]"#;
        let read = Schedules::read_answer(schedules).expect("calendars");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].busy[0].status, FreeBusy::Tentative);
        assert_eq!(read[0].busy[0].end_ms - read[0].busy[0].start_ms, 1_800_000);
        assert_eq!(read[1].busy[0].subject.as_deref(), Some("Planning"));
        assert_eq!(Schedules::read_answer(b"{}"), None);
    }
}
