//! The calendar, as the client's calendar and scheduling form ask the middle tier for it
//! (`docs/protocol/calendar.md`): an account's events and the series among them, read by window and whole, and
//! their writes.
//!
//! Whose an event is comes from the database on every request: its organizer changes and cancels it, an account
//! it invites reads it, answers it and takes it off its own calendar, and nobody else learns that it exists.

mod draft;
mod recur;
pub(crate) mod repo;
pub(crate) mod schema;
mod wire;
mod zones;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::app::App;
use crate::auth::extract::Spaces;
use crate::db::{Db, DbError};
use crate::teams::Actor;
use draft::Draft;
use repo::{Event, Seen};
use wire::{People, Shown};

type Me = Actor<Spaces>;
type Params = HashMap<String, String>;

/// The widest window one calendar read takes (the client asks for 62 days at most).
const WIDEST_WINDOW_MS: i64 = 100 * 24 * 3_600_000;

/// The calendar's routes, under the middle tier's base.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/v2.0/me/calendars/default/calendarView", get(view))
        .route("/v2.0/me/calendars/default/events", post(create))
        .route("/v2.0/me/calendars/events/{id}", get(read))
        .route("/beta/me/calendarEvents/timeZoneSettingsWithOffset", get(time_zone))
}

/// A refusal, with the code the client's log names.
#[derive(Debug)]
pub(crate) enum Error {
    Code(StatusCode, &'static str),
    Database(DbError),
}

impl Error {
    const BAD_REQUEST: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    const NOT_FOUND: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Database(error)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Code(status, code) => (status, Json(json!({"errorCode": code, "message": code}))).into_response(),
            Self::Database(error) => {
                tracing::error!(%error, "calendar database failure");
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        }
    }
}

type Answer<T = Response> = Result<T, Error>;

fn body(body: Result<Json<Value>, JsonRejection>) -> Answer<Value> {
    body.map(|Json(body)| body).map_err(|_| Error::BAD_REQUEST)
}

/// `GET beta/me/calendarEvents/timeZoneSettingsWithOffset?timezone=<IANA zone>`: the zone as an event names it.
async fn time_zone(_: Me, Query(query): Query<Params>) -> Answer<Json<Value>> {
    let zone = query.get("timezone").ok_or(Error::BAD_REQUEST)?;
    let (name, offset) = zones::exchange(zone, chrono::Utc::now()).ok_or(Error::NOT_FOUND)?;
    let named = json!({"timeZone": name, "utcOffsetInMinutes": offset});
    Ok(Json(json!({
        "timeZone": name, "utcOffsetInMinutes": offset, "local": named, "remote": named,
    })))
}

/// The accounts `events` name.
async fn people(db: &Db, events: &[&Event]) -> Result<People, DbError> {
    let mut ids: Vec<String> = events.iter().map(|event| event.owner_id.clone()).collect();
    ids.sort();
    ids.dedup();
    let users = repo::users_by_id(db, &ids).await?;
    Ok(People(users.into_iter().map(|user| (user.id.clone(), user)).collect()))
}

/// `GET v2.0/me/calendars/default/calendarView?StartDate&EndDate`: the account's events of the window, a series
/// as its occurrences.
async fn view(State(app): State<Arc<App>>, me: Me, Query(query): Query<Params>) -> Answer<Json<Value>> {
    let end_of = |name: &str| {
        query
            .get(name)
            .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
            .map(|time| time.timestamp_millis())
            .ok_or(Error::BAD_REQUEST)
    };
    let (from, to) = (end_of("StartDate")?, end_of("EndDate")?);
    if to <= from || to - from > WIDEST_WINDOW_MS {
        return Err(Error::BAD_REQUEST);
    }
    let seen = repo::window(&app.db, &me.id, from, to).await?;
    let series: Vec<String> = seen
        .iter()
        .filter(|(event, _)| event.recurring)
        .map(|(event, _)| event.id.clone())
        .collect();
    let own_rows = if series.is_empty() {
        Vec::new()
    } else {
        repo::own_rows(&app.db, &series).await?
    };
    let events: Vec<&Event> = seen.iter().map(|(event, _)| event).collect();
    let people = people(&app.db, &events).await?;
    let mut listed: Vec<(i64, Value)> = Vec::new();
    for (event, mine) in &seen {
        let shown = |occurrence| Shown {
            reader: &me.id,
            event,
            occurrence,
            mine: mine.as_ref(),
            people: &people,
        };
        if !event.recurring {
            listed.push((event.start_ms, shown(None).entry()));
            continue;
        }
        let fields = serde_json::from_str(&event.body).unwrap_or_default();
        let Some(rule) = draft::series_of(&fields, event.start_ms, event.end_ms) else {
            continue;
        };
        for start in rule.starts(from, to) {
            if !own_rows.iter().any(|(id, own)| *id == event.id && *own == start) {
                listed.push((start, shown(Some(start)).entry()));
            }
        }
    }
    listed.sort_by_key(|(start, _)| *start);
    Ok(Json(wire::events(listed.into_iter().map(|(_, entry)| entry).collect())))
}

/// An event `user` may read: the row, or the occurrence of a series that has no row of its own.
struct Found {
    event: Event,
    mine: Option<repo::Attendee>,
    occurrence: Option<i64>,
}

impl Found {
    /// The event `id` when it is on `user`'s calendar; anyone else learns only that it isn't there.
    async fn of(db: &Db, user: &str, id: &str) -> Answer<Self> {
        let (seen, occurrence) = match repo::event(db, id, user).await? {
            Some(seen) => (seen, None),
            None => {
                let (series, start) = id.rsplit_once('.').ok_or(Error::NOT_FOUND)?;
                let start: i64 = start.parse().map_err(|_| Error::NOT_FOUND)?;
                let seen = repo::event(db, series, user).await?.ok_or(Error::NOT_FOUND)?;
                (seen, Some(start))
            }
        };
        let (event, mine) = seen;
        if !on_calendar(&(event.clone(), mine.clone()), user) {
            return Err(Error::NOT_FOUND);
        }
        if let Some(start) = occurrence {
            let fields = serde_json::from_str(&event.body).unwrap_or_default();
            let is_one = event.recurring
                && draft::series_of(&fields, event.start_ms, event.end_ms).is_some_and(|rule| rule.has(start));
            if !is_one {
                return Err(Error::NOT_FOUND);
            }
        }
        Ok(Self {
            event,
            mine,
            occurrence,
        })
    }
}

/// Whether the event is on `user`'s calendar: its own while it isn't cancelled, or one it is invited to and didn't
/// take off.
fn on_calendar((event, mine): &Seen, user: &str) -> bool {
    let own = event.team_id.is_none() && event.owner_id == user && !event.cancelled;
    own || mine.as_ref().is_some_and(|mine| !mine.removed)
}

/// `GET v2.0/me/calendars/events/<id>`: one event whole.
async fn read(State(app): State<Arc<App>>, me: Me, Path(id): Path<String>) -> Answer<Json<Value>> {
    let found = Found::of(&app.db, &me.id, &id).await?;
    let people = people(&app.db, &[&found.event]).await?;
    let attendees = repo::attendees(&app.db, &found.event.id).await?;
    let shown = Shown {
        reader: &me.id,
        event: &found.event,
        occurrence: found.occurrence,
        mine: found.mine.as_ref(),
        people: &people,
    };
    Ok(Json(shown.full(&attendees)))
}

/// `POST v2.0/me/calendars/default/events` with `{"calendarEvent"}`: a new event of the account's own.
async fn create(State(app): State<Arc<App>>, me: Me, input: Result<Json<Value>, JsonRejection>) -> Answer {
    let input = body(input)?;
    let draft = Draft::read(&input["calendarEvent"], true)?;
    let at = crate::chatsvc::now();
    let event = Event {
        id: uuid::Uuid::new_v4().simple().to_string(),
        owner_id: me.id.clone(),
        team_id: None,
        channel_id: None,
        post_id: None,
        series_id: None,
        original_start: None,
        start_ms: draft.start_ms,
        end_ms: draft.end_ms,
        until_ms: draft.series_until.unwrap_or(draft.end_ms),
        recurring: draft.series_until.is_some(),
        changed: false,
        cancelled: false,
        meeting_chat: None,
        body: Value::Object(draft.body).to_string(),
        version: at,
    };
    repo::insert(&app.db, &event, &[]).await?;
    let people = people(&app.db, &[&event]).await?;
    let shown = Shown {
        reader: &me.id,
        event: &event,
        occurrence: None,
        mine: None,
        people: &people,
    };
    Ok((StatusCode::CREATED, Json(wire::events(vec![shown.entry()]))).into_response())
}
