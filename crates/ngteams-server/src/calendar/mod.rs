//! The calendar, as the client's calendar and scheduling form ask the middle tier for it
//! (`docs/protocol/calendar.md`): an account's events and the series among them, read by window and whole, and
//! their writes.
//!
//! Whose an event is comes from the database on every request: its organizer changes and cancels it, an account
//! it invites reads it, answers it and takes it off its own calendar, and nobody else learns that it exists.

mod activity;
mod draft;
mod group;
mod invite;
mod meeting;
mod meeting_repo;
mod options;
mod people;
mod places;
mod recur;
pub(crate) mod repo;
mod scheduler;
pub(crate) mod schema;
mod wire;
mod write;
mod zones;

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::app::App;
use crate::auth::extract::Spaces;
use crate::db::{Db, DbError};
use crate::teams::Actor;
pub(crate) use meeting::{Meeting, describe_thread};
pub(crate) use options::option;
use repo::{Event, Seen};
use wire::{Book, Shown};

type Me = Actor<Spaces>;
type Params = HashMap<String, String>;

/// The meeting whose chat is `chat`, if it is a meeting's.
///
/// # Errors
/// A database failure.
pub(crate) async fn meeting_of(db: &Db, chat: &str) -> Result<Option<Meeting>, DbError> {
    meeting::of_chat(db, chat).await
}

/// The largest JSON body a calendar request takes: an event's text can be long.
const BODY_LIMIT: usize = 1024 * 1024;

/// The widest window one calendar read takes (the client asks for 62 days at most).
const WIDEST_WINDOW_MS: i64 = 100 * 24 * 3_600_000;

/// The calendar's routes, under the middle tier's base.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/v2.0/me/calendars/default/calendarView", get(view))
        .route("/v2.0/me/calendars/default/events", post(write::create))
        .route(
            "/v2.0/me/calendars/events/{id}",
            get(read).put(write::update).delete(write::delete),
        )
        .route("/v2.0/me/calendars/events/{id}/cancel", put(write::cancel))
        .route("/v2.0/me/calendars/events/{id}/respond", put(write::respond))
        .route("/v2.0/me/calendars/findMeetingTimes", put(scheduler::meeting_times))
        .route("/v2.0/me/calendars/schedules", put(scheduler::schedules))
        .route(
            "/beta/meetings/v1/options/{tenant}/{organizer}/{chat}/0/0",
            get(options::read).post(options::save),
        )
        .route("/v2.0/places/roomLists", get(places::lists))
        .route("/v2.0/places/rooms", get(places::rooms))
        .route("/v2.0/places/{list}/roomList/rooms", get(places::rooms_of))
        .route("/v2.0/groups/{group}/calendars/default/events", post(group::create))
        .route("/v2.0/groups/{group}/calendars/events/iCalUId/{uid}", get(group::read))
        .route("/v2.0/groups/{group}/calendars/events/{uid}", put(group::update))
        .route("/v2.0/groups/{group}/calendars/events/{uid}/cancel", put(group::cancel))
        .route(
            "/beta/me/calendarEvents/privateMeeting/schedulingService/create",
            post(meeting::create),
        )
        .route("/beta/me/calendarEvents/timeZoneSettingsWithOffset", get(time_zone))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// A refusal, with the code the client's log names.
#[derive(Debug)]
pub(crate) enum Error {
    Code(StatusCode, &'static str),
    Database(DbError),
}

impl Error {
    const BAD_REQUEST: Self = Self::Code(StatusCode::BAD_REQUEST, "BadRequest");
    /// The event kept changing under the request.
    const CONFLICT: Self = Self::Code(StatusCode::CONFLICT, "Conflict");
    const FORBIDDEN: Self = Self::Code(StatusCode::FORBIDDEN, "Forbidden");
    const NOT_FOUND: Self = Self::Code(StatusCode::NOT_FOUND, "NotFound");
    /// An attendee that isn't an account of this server: it takes no mail.
    const UNKNOWN_ATTENDEE: Self = Self::Code(StatusCode::BAD_REQUEST, "UnknownAttendee");
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

/// What `events` name beyond their rows: their organizers and their meetings.
async fn book<'a>(app: &'a App, events: &[&Event]) -> Result<Book<'a>, DbError> {
    let mut ids: Vec<String> = events.iter().map(|event| event.owner_id.clone()).collect();
    ids.sort();
    ids.dedup();
    let users = repo::users_by_id(&app.db, &ids).await?;
    let mut chats: Vec<String> = events.iter().filter_map(|event| event.meeting_chat.clone()).collect();
    chats.sort();
    chats.dedup();
    let meetings = if chats.is_empty() {
        Vec::new()
    } else {
        meeting::of_chats(&app.db, &chats).await?
    };
    let mut teams = HashMap::new();
    for id in events.iter().filter_map(|event| event.team_id.as_ref()) {
        if !teams.contains_key(id)
            && let Some(team) = crate::teams::repo::team(&app.db, id).await?
        {
            teams.insert(id.clone(), team);
        }
    }
    Ok(Book {
        app,
        teams,
        users: users.into_iter().map(|user| (user.id.clone(), user)).collect(),
        meetings: meetings
            .into_iter()
            .map(|meeting| (meeting.chat_id.clone(), meeting))
            .collect(),
    })
}

/// One event, or one occurrence of a series, where it is on an account's calendar.
pub(super) struct Placed {
    pub seen: Arc<Seen>,
    /// The occurrence's start, for an occurrence of a series that has no row of its own.
    pub occurrence: Option<i64>,
    pub start: i64,
    pub end: i64,
}

/// What is on `user`'s calendar in `[from, to)`, by start: a series as its occurrences, one kept as a row of its
/// own in place of the series' at that time.
pub(super) async fn placed(app: &App, user: &str, from: i64, to: i64) -> Result<Vec<Placed>, DbError> {
    let seen = repo::window(&app.db, user, from, to).await?;
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
    let mut placed = Vec::new();
    for seen in seen.into_iter().map(Arc::new) {
        let event = &seen.0;
        if !event.recurring {
            placed.push(Placed {
                occurrence: None,
                start: event.start_ms,
                end: event.end_ms,
                seen: Arc::clone(&seen),
            });
            continue;
        }
        let fields = serde_json::from_str(&event.body).unwrap_or_default();
        let Some(rule) = draft::series_of(&fields, event.start_ms, event.end_ms) else {
            continue;
        };
        let length = event.end_ms - event.start_ms;
        for start in rule.starts(from, to) {
            if !own_rows.iter().any(|(id, own)| *id == event.id && *own == start) {
                placed.push(Placed {
                    occurrence: Some(start),
                    start,
                    end: start + length,
                    seen: Arc::clone(&seen),
                });
            }
        }
    }
    placed.sort_by_key(|placed| placed.start);
    Ok(placed)
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
    let placed = placed(&app, &me.id, from, to).await?;
    let events: Vec<&Event> = placed.iter().map(|placed| &placed.seen.0).collect();
    let book = book(&app, &events).await?;
    let listed = placed
        .iter()
        .map(|placed| {
            let shown = Shown {
                reader: &me.id,
                event: &placed.seen.0,
                occurrence: placed.occurrence,
                mine: placed.seen.1.as_ref(),
                book: &book,
                team_view: false,
            };
            shown.entry()
        })
        .collect();
    Ok(Json(wire::events(listed)))
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
        let (seen, occurrence) = if let Some(seen) = repo::event(db, id, user).await? {
            (seen, None)
        } else {
            let (series, start) = id.rsplit_once('.').ok_or(Error::NOT_FOUND)?;
            let start: i64 = start.parse().map_err(|_| Error::NOT_FOUND)?;
            let seen = repo::event(db, series, user).await?.ok_or(Error::NOT_FOUND)?;
            (seen, Some(start))
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

impl Found {
    /// Whether the event is `user`'s own to change and cancel.
    fn organized_by(&self, user: &str) -> bool {
        self.event.team_id.is_none() && self.event.owner_id == user
    }

    /// Whether it is one occurrence of a series.
    fn one_of_a_series(&self) -> bool {
        self.occurrence.is_some() || self.event.series_id.is_some()
    }

    /// The occurrence as a row of its own under its series: the row it has, or the series' fields at its time.
    fn own_row(&self) -> Event {
        let Some(start) = self.occurrence else {
            return self.event.clone();
        };
        let mut fields: serde_json::Map<String, Value> = serde_json::from_str(&self.event.body).unwrap_or_default();
        fields.remove("eventRecurrencePattern");
        fields.remove("eventRecurrenceRange");
        let end = start + (self.event.end_ms - self.event.start_ms);
        Event {
            id: wire::occurrence_id(&self.event.id, start),
            series_id: Some(self.event.id.clone()),
            original_start: Some(start),
            start_ms: start,
            end_ms: end,
            until_ms: end,
            recurring: false,
            changed: false,
            body: Value::Object(fields).to_string(),
            ..self.event.clone()
        }
    }
}

impl Found {
    /// The rows `user`'s answer or removal of the event touches: the row itself; a series' and every occurrence
    /// kept under it; for an occurrence without a row, a row made for it. `None` when the series changed since it
    /// was read here: the occurrence has to be read again.
    async fn rows_for(&self, db: &Db) -> Result<Option<Vec<String>>, DbError> {
        if self.occurrence.is_some() {
            let row = self.own_row();
            let kept = repo::keep_own_row(db, &row).await?;
            return Ok(kept.then(|| vec![row.id]));
        }
        let mut rows = vec![self.event.id.clone()];
        if self.event.recurring {
            rows.extend(repo::own_row_ids(db, &self.event.id).await?);
        }
        Ok(Some(rows))
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
    let book = book(&app, &[&found.event]).await?;
    let attendees = repo::attendees(&app.db, &found.event.id).await?;
    let shown = Shown {
        reader: &me.id,
        event: &found.event,
        occurrence: found.occurrence,
        mine: found.mine.as_ref(),
        book: &book,
        team_view: false,
    };
    Ok(Json(shown.full(&attendees)))
}
