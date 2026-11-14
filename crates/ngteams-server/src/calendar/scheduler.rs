//! The scheduling form's Scheduler (`docs/protocol/calendar.md`, "The Scheduler"): the busy times of the people a
//! meeting would invite, and the starts all of them are free at. Both are reads sent as a `PUT`. Anyone signed in
//! may ask for any account's busy times, as in an organization's directory; what an event is about is told only
//! of the caller's own.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use chrono::SecondsFormat;
use serde_json::{Value, json};

use super::{Answer, Error, Me, body, placed};
use crate::app::App;
use crate::db::users;

const DAY_MS: i64 = 24 * 3_600_000;
const HALF_HOUR_MS: i64 = 30 * 60_000;
/// The most people one read asks about, the most days, and the most suggestions answered.
const MOST_PEOPLE: usize = 100;
const MOST_DAYS: i64 = 62;
const MOST_SUGGESTIONS: usize = 96;
/// The working day suggestions are in, in hours after the midnight the read names.
const WORKING_HOURS: std::ops::Range<i64> = 8..18;

/// One busy time: its ends, how it shows, and for the caller's own what it is about.
struct Busy {
    start: i64,
    end: i64,
    status: String,
    details: Value,
}

/// Whom a read asks about and when.
struct Ask {
    from: i64,
    to: i64,
    minutes: i64,
    /// Each person as the read named them, with their account when they have one here.
    people: Vec<(String, Option<String>)>,
}

impl Ask {
    async fn read(app: &App, me: &Me, sent: &Value) -> Answer<Self> {
        let time = |key: &str| {
            sent[key]
                .as_str()
                .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
                .map(|time| time.timestamp_millis())
        };
        let from = time("startTime").ok_or(Error::BAD_REQUEST)?;
        let days = sent["numberOfDays"].as_i64().unwrap_or(1);
        let to = time("endTime").unwrap_or(from + days * DAY_MS);
        let minutes = sent["meetingDurationInMinutes"].as_i64().unwrap_or(30);
        let entries = sent["attendees"].as_array().ok_or(Error::BAD_REQUEST)?;
        let sound = to > from && to - from <= MOST_DAYS * DAY_MS && (1..=1440).contains(&minutes);
        if !sound || entries.len() > MOST_PEOPLE {
            return Err(Error::BAD_REQUEST);
        }
        let own = users::by_id(&app.db, &me.id).await?.ok_or(Error::FORBIDDEN)?;
        let mut people: Vec<(String, Option<String>)> = Vec::new();
        for entry in entries {
            let address = entry["address"].as_str().map(str::trim).unwrap_or_default();
            let (address, account) = if entry["type"].as_str() == Some("Organizer") {
                (own.upn.clone(), Some(me.id.clone()))
            } else {
                let known = users::by_upn(&app.db, address).await?;
                let account = known.filter(|user| user.deleted_at.is_none()).map(|user| user.id);
                (address.to_owned(), account)
            };
            if !address.is_empty() && !people.iter().any(|(known, _)| known.eq_ignore_ascii_case(&address)) {
                people.push((address, account));
            }
        }
        Ok(Self {
            from,
            to,
            minutes,
            people,
        })
    }

    /// When the account `user` is busy in the window, by start.
    async fn busy(&self, app: &App, me: &Me, user: &str) -> Answer<Vec<Busy>> {
        let mut busy = Vec::new();
        for slot in placed(app, user, self.from, self.to).await? {
            let (event, mine) = &*slot.seen;
            let fields: Value = serde_json::from_str(&event.body).unwrap_or_default();
            let set = fields["showAs"].as_str().unwrap_or("Busy");
            let organizes = event.team_id.is_none() && event.owner_id == user;
            let status = match mine.as_ref().map(|mine| mine.response.as_str()) {
                _ if event.cancelled => continue,
                _ if organizes => set,
                Some("Accepted") => set,
                Some("Declined") => continue,
                _ => "Tentative",
            };
            if status == "Free" {
                continue;
            }
            let details = if user == me.id {
                json!({"subject": fields["subject"], "location": fields["location"], "isPrivate": fields["isPrivate"]})
            } else {
                json!({})
            };
            busy.push(Busy {
                start: slot.start,
                end: slot.end,
                status: status.to_owned(),
                details,
            });
        }
        Ok(busy)
    }
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

/// `PUT v2.0/me/calendars/schedules`: each person's busy times in the window.
pub(super) async fn schedules(
    State(app): State<Arc<App>>,
    me: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let ask = Ask::read(&app, &me, &body(input)?).await?;
    let mut answer = Vec::new();
    for (address, account) in &ask.people {
        let busy = match account {
            Some(account) => ask.busy(&app, &me, account).await?,
            None => Vec::new(),
        };
        let events: Vec<Value> = busy
            .iter()
            .map(|busy| {
                json!({"freeBusyStatus": busy.status, "startTime": iso(busy.start), "endTime": iso(busy.end),
                    "details": busy.details})
            })
            .collect();
        answer.push(json!({"userPrincipalName": address, "freeBusyEvents": events}));
    }
    Ok(Json(Value::Array(answer)))
}

/// `PUT v2.0/me/calendars/findMeetingTimes`: the half hours of each working day (counted from the midnight the
/// read starts at) at which everyone asked about is free for the meeting's length, none of them past.
pub(super) async fn meeting_times(
    State(app): State<Arc<App>>,
    me: Me,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<Json<Value>> {
    let ask = Ask::read(&app, &me, &body(input)?).await?;
    let mut taken: Vec<(i64, i64)> = Vec::new();
    for (_, account) in &ask.people {
        if let Some(account) = account {
            taken.extend(
                ask.busy(&app, &me, account)
                    .await?
                    .iter()
                    .map(|busy| (busy.start, busy.end)),
            );
        }
    }
    let length = ask.minutes * 60_000;
    let now = crate::chatsvc::now();
    let mut suggestions = Vec::new();
    let mut day = ask.from;
    while day < ask.to && suggestions.len() < MOST_SUGGESTIONS {
        let mut at = day + WORKING_HOURS.start * 2 * HALF_HOUR_MS;
        while at + length <= day + WORKING_HOURS.end * 2 * HALF_HOUR_MS && suggestions.len() < MOST_SUGGESTIONS {
            let free = taken.iter().all(|(start, end)| at + length <= *start || at >= *end);
            if free && at >= now && at + length <= ask.to {
                suggestions.push(json!({"meetingTime": iso(at), "confidence": 100.0}));
            }
            at += HALF_HOUR_MS;
        }
        day += DAY_MS;
    }
    Ok(Json(json!({"timeSuggestions": suggestions})))
}
