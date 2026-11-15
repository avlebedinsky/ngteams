//! An event's writes: its creation, its organizer's change, cancellation and delete, and an invited account's
//! answer and delete (`docs/protocol/calendar.md`, "Writing an event"). A write names one event, a whole series by
//! its own id, or one occurrence by the occurrence's.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value;

use super::activity::{self, Change};
use super::draft::Draft;
use super::people::{self, Invitee};
use super::repo::{self, Attendee, Event, Kept};
use super::wire::{self, Shown};
use super::{Answer, Error, Found, Me, Params, body, book, invite, meeting};
use crate::app::App;
use crate::chatsvc::service;
use crate::db::users::{self, User};

/// What a creation or a change asks for, checked.
struct Asked {
    draft: Draft,
    invitees: Vec<Invitee>,
    online: bool,
}

impl Asked {
    async fn read(app: &App, me: &str, sent: &Value, query: &Params, may_repeat: bool) -> Answer<Self> {
        Ok(Self {
            draft: Draft::read(sent, may_repeat)?,
            invitees: people::invitees(app, me, &sent["attendees"], None).await?,
            online: query.get("isOnlineMeeting").map(String::as_str) == Some("true"),
        })
    }

    fn wanted(&self) -> Vec<(String, bool)> {
        self.invitees
            .iter()
            .map(|invitee| (invitee.user.id.clone(), invitee.optional))
            .collect()
    }
}

/// The account behind a request, as the chat service names it on what the calendar posts for it.
pub(super) async fn account(app: &App, id: &str) -> Answer<User> {
    users::by_id(&app.db, id).await?.ok_or(Error::FORBIDDEN)
}

/// The meeting the form made for the event it now saves: the caller's own, of no event yet.
async fn claimed(app: &App, me: &str, made: &Value) -> Answer<String> {
    let chat = made["groupContext"]["threadId"].as_str().ok_or(Error::BAD_REQUEST)?;
    let own = meeting::of_chat(&app.db, chat)
        .await?
        .is_some_and(|meeting| meeting.organizer_id == me && meeting.kind == "Scheduled");
    if !own || repo::uses_chat(&app.db, chat).await? {
        return Err(Error::BAD_REQUEST);
    }
    Ok(chat.to_owned())
}

/// Tells the people of a meeting's chat about the event as it now is: the chat is named after it, who is invited is
/// in it (and whoever is `gone` no longer), and its message there names the meeting.
async fn tell(
    app: &App,
    event: &mut Event,
    organizer: &User,
    wanted: &[(String, bool)],
    gone: &[String],
) -> Answer<()> {
    let Some(chat) = event.meeting_chat.clone() else {
        return Ok(());
    };
    let Some(meeting) = meeting::of_chat(&app.db, &chat).await? else {
        return Ok(());
    };
    let fields: Value = serde_json::from_str(&event.body).unwrap_or_default();
    let subject = fields["subject"].as_str().unwrap_or_default();
    let renamed = meeting.subject.as_deref() != Some(subject);
    if renamed {
        meeting::rename(&app.db, &chat, subject).await?;
    }
    if repo::settle_chat(&app.db, &event.id, &chat, &ids(wanted), gone).await? || renamed {
        service::announce(app, &chat, organizer).await?;
    }
    let link = meeting.join_link(app);
    if event.post_id.is_some() {
        invite::changed(app, &chat, event, &link, organizer).await?;
    } else if let Some(post) = invite::scheduled(app, &chat, event, &link, organizer, "Scheduled a meeting").await? {
        repo::set_post(&app.db, &event.id, post).await?;
        event.post_id = Some(post);
    }
    Ok(())
}

/// `POST v2.0/me/calendars/default/events` with `{"calendarEvent", "schedulingServiceEvent"?}`: a new event of the
/// account's own, on the calendar of everyone it invites.
pub(super) async fn create(
    State(app): State<Arc<App>>,
    me: Me,
    Query(query): Query<Params>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer {
    let input = body(input)?;
    let asked = Asked::read(&app, &me.id, &input["calendarEvent"], &query, true).await?;
    let meeting_chat = if asked.online {
        Some(claimed(&app, &me.id, &input["schedulingServiceEvent"]).await?)
    } else {
        None
    };
    let organizer = account(&app, &me.id).await?;
    let wanted = asked.wanted();
    let at = crate::chatsvc::now();
    let mut event = Event {
        id: uuid::Uuid::new_v4().simple().to_string(),
        owner_id: me.id.clone(),
        team_id: None,
        channel_id: None,
        post_id: None,
        series_id: None,
        original_start: None,
        start_ms: asked.draft.start_ms,
        end_ms: asked.draft.end_ms,
        until_ms: asked.draft.series_until.unwrap_or(asked.draft.end_ms),
        recurring: asked.draft.series_until.is_some(),
        changed: false,
        cancelled: false,
        meeting_chat,
        body: Value::Object(asked.draft.body).to_string(),
        version: at,
    };
    repo::insert(&app.db, &event, &invitations(&event.id, &wanted)).await?;
    tell(&app, &mut event, &organizer, &wanted, &[]).await?;
    activity::tell(&app, &ids(&wanted), Change::Created, &event, &organizer).await;
    let book = book(&app, &[&event]).await?;
    let shown = Shown {
        reader: &me.id,
        event: &event,
        occurrence: None,
        mine: None,
        book: &book,
        team_view: false,
    };
    Ok((StatusCode::CREATED, Json(wire::events(vec![shown.entry()]))).into_response())
}

pub(super) fn ids(wanted: &[(String, bool)]) -> Vec<String> {
    wanted.iter().map(|(id, _)| id.clone()).collect()
}

pub(super) fn invitations(event: &str, wanted: &[(String, bool)]) -> Vec<Attendee> {
    wanted
        .iter()
        .enumerate()
        .map(|(position, (user, optional))| Attendee {
            event_id: event.to_owned(),
            user_id: user.clone(),
            position: i32::try_from(position).unwrap_or(i32::MAX),
            optional: *optional,
            response: "None".into(),
            responded_at: None,
            removed: false,
        })
        .collect()
}

/// `PUT v2.0/me/calendars/events/<id>` with `{"updatedCalendarEvent"}`: the organizer's change, to everyone the
/// event now invites. A new time asks each of them again.
pub(super) async fn update(
    State(app): State<Arc<App>>,
    me: Me,
    Path(id): Path<String>,
    Query(query): Query<Params>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<StatusCode> {
    let input = body(input)?;
    let found = Found::of(&app.db, &me.id, &id).await?;
    if !found.organized_by(&me.id) {
        return Err(Error::FORBIDDEN);
    }
    let one = found.one_of_a_series();
    let mut sent = input["updatedCalendarEvent"].clone();
    if let (true, Some(sent)) = (one, sent.as_object_mut()) {
        sent.remove("eventRecurrencePattern");
        sent.remove("eventRecurrenceRange");
    }
    let asked = Asked::read(&app, &me.id, &sent, &query, !one).await?;
    let organizer = account(&app, &me.id).await?;
    if one {
        one_changed(&app, &found, asked, &organizer).await?;
    } else {
        all_changed(&app, &found, asked, &organizer).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// One occurrence changed: a new time asks its people again; whoever it adds joins the series' chat, and whoever
/// it was the last invitation of leaves it; the series' own message stays the series'.
async fn one_changed(app: &App, found: &Found, asked: Asked, organizer: &User) -> Answer<()> {
    let wanted = asked.wanted();
    let (row, moved) = occurrence_changed(app, found, asked.draft).await?;
    let series = row.series_id.clone().unwrap_or_else(|| row.id.clone());
    let gone = repo::sync_attendees(&app.db, &series, &row.id, &wanted, moved).await?;
    if let Some(chat) = &row.meeting_chat
        && repo::settle_chat(&app.db, &series, chat, &ids(&wanted), &gone).await?
    {
        service::announce(app, chat, organizer).await?;
    }
    activity::tell(app, &ids(&wanted), Change::Updated, &row, organizer).await;
    Ok(())
}

/// An event or a whole series changed: its invitations follow, a new time asks everyone again, a change to a
/// Teams meeting makes its meeting, and everyone it invited, invites or no longer invites is told.
async fn all_changed(app: &App, found: &Found, asked: Asked, organizer: &User) -> Answer<()> {
    let wanted = asked.wanted();
    let online = asked.online;
    let before = repo::attendees(&app.db, &found.event.id).await?;
    let changed = whole_changed(app, found, asked.draft, &wanted).await?;
    let (mut row, gone) = (changed.row, changed.gone);
    if online && row.meeting_chat.is_none() {
        let fields: Value = serde_json::from_str(&row.body).unwrap_or_default();
        let made = meeting::make(app, &organizer.id, "Scheduled", fields["subject"].as_str()).await?;
        repo::set_meeting(&app.db, &row.id, &made.chat_id).await?;
        row.meeting_chat = Some(made.chat_id);
    }
    let before: Vec<String> = before.into_iter().map(|(attendee, _)| attendee.user_id).collect();
    let (kept, new): (Vec<String>, Vec<String>) = ids(&wanted).into_iter().partition(|id| before.contains(id));
    tell(app, &mut row, organizer, &wanted, &gone).await?;
    activity::tell(app, &kept, Change::Updated, &row, organizer).await;
    activity::tell(app, &new, Change::Created, &row, organizer).await;
    activity::tell(app, &gone, Change::Canceled, &row, organizer).await;
    Ok(())
}

/// One occurrence given fields of its own: kept as a row under its series. Whether its time changed from the one
/// it had, its own or the series'.
pub(super) async fn occurrence_changed(app: &App, found: &Found, draft: Draft) -> Answer<(Event, bool)> {
    let (row, moved) = occurrence_row(found, draft);
    repo::put_own_row(&app.db, &row).await?;
    Ok((row, moved))
}

/// The row an occurrence's change makes of it, not yet written, and whether its time changed.
pub(super) fn occurrence_row(found: &Found, draft: Draft) -> (Event, bool) {
    let had = found.own_row();
    let moved = (had.start_ms, had.end_ms) != (draft.start_ms, draft.end_ms);
    let row = Event {
        start_ms: draft.start_ms,
        end_ms: draft.end_ms,
        until_ms: draft.end_ms,
        changed: true,
        body: Value::Object(draft.body).to_string(),
        version: crate::chatsvc::now(),
        ..had
    };
    (row, moved)
}

/// An event or a whole series as its change left it.
pub(super) struct Whole {
    pub row: Event,
    /// A series' rule or times changed, which forgot what its occurrences were given on their own.
    pub reshaped: bool,
    /// Who it no longer invites anywhere.
    pub gone: Vec<String>,
}

/// An event or a whole series changed, with its invitations: `wanted` are who it now invites, asked again when
/// its time changed.
pub(super) async fn whole_changed(app: &App, found: &Found, draft: Draft, wanted: &[(String, bool)]) -> Answer<Whole> {
    let at = crate::chatsvc::now();
    let before: Value = serde_json::from_str(&found.event.body).unwrap_or_default();
    let rule = |fields: &Value| {
        (
            fields["eventRecurrencePattern"].clone(),
            fields["eventRecurrenceRange"].clone(),
        )
    };
    let mut after = Value::Object(draft.body);
    let moved = (found.event.start_ms, found.event.end_ms) != (draft.start_ms, draft.end_ms);
    let reshaped = moved || rule(&before) != rule(&after);
    let row = Event {
        start_ms: draft.start_ms,
        end_ms: draft.end_ms,
        until_ms: draft.series_until.unwrap_or(draft.end_ms),
        recurring: draft.series_until.is_some(),
        body: after.to_string(),
        version: at,
        ..found.event.clone()
    };
    if let Some(fields) = after.as_object_mut() {
        fields.remove("eventRecurrencePattern");
        fields.remove("eventRecurrenceRange");
    }
    let given = after.to_string();
    let kept = match (found.event.recurring, reshaped) {
        (false, _) => None,
        (true, true) => Some(Kept::Forgotten),
        (true, false) => Some(Kept::Given(&given)),
    };
    let gone = repo::save_whole(&app.db, &row, kept, wanted, moved).await?;
    Ok(Whole { row, reshaped, gone })
}

/// Ends the organizer's event: an event nobody else has is deleted; one that invites people stays, cancelled, on
/// their calendars, and its chat is told; one occurrence is kept as cancelled under its series.
async fn end(app: &App, me: &Me, found: &Found, note: Option<&str>) -> Answer<()> {
    let at = crate::chatsvc::now();
    let attendees = repo::attendees(&app.db, &found.event.id).await?;
    let invited = !attendees.is_empty();
    let mut row = found.own_row();
    row.cancelled = true;
    row.version = at;
    if found.one_of_a_series() {
        repo::put_own_row(&app.db, &row).await?;
    } else if invited {
        repo::cancel(&app.db, &row.id, at).await?;
    } else {
        repo::delete(&app.db, &row.id).await?;
    }
    if !invited {
        return Ok(());
    }
    let organizer = account(app, &me.id).await?;
    if let Some(chat) = &row.meeting_chat
        && let Some(meeting) = meeting::of_chat(&app.db, chat).await?
    {
        invite::cancelled(app, chat, &row, &meeting.join_link(app), &organizer, note).await?;
    }
    let told: Vec<String> = attendees.into_iter().map(|(attendee, _)| attendee.user_id).collect();
    activity::tell(app, &told, Change::Canceled, &row, &organizer).await;
    Ok(())
}

/// The note a cancellation is sent with (`cancellationMessage`).
pub(super) fn note(input: &Value) -> Answer<Option<&str>> {
    match &input["cancellationMessage"] {
        Value::Null => Ok(None),
        Value::String(note) if note.chars().count() <= 1000 => Ok(Some(note.as_str())),
        _ => Err(Error::BAD_REQUEST),
    }
}

/// `PUT v2.0/me/calendars/events/<id>/cancel` with `{"cancellationMessage"?}`: the organizer's cancellation.
pub(super) async fn cancel(
    State(app): State<Arc<App>>,
    me: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<StatusCode> {
    let input = body(input)?;
    let found = Found::of(&app.db, &me.id, &id).await?;
    if !found.organized_by(&me.id) {
        return Err(Error::FORBIDDEN);
    }
    end(&app, &me, &found, note(&input)?).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE v2.0/me/calendars/events/<id>`: the event taken off the account's calendar. Its organizer's delete ends
/// it for everyone; an invited account's only takes its own copy.
pub(super) async fn delete(State(app): State<Arc<App>>, me: Me, Path(id): Path<String>) -> Answer<StatusCode> {
    let found = Found::of(&app.db, &me.id, &id).await?;
    if found.organized_by(&me.id) {
        end(&app, &me, &found, None).await?;
    } else {
        let ((), rows) = invited_rows(&app, &me, &id, |_| Ok(())).await?;
        repo::set_removed(&app.db, &rows, &me.id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The rows an invited account's answer or removal of the event `id` touches, with what `may` let through. A
/// series that changed under the read of one of its occurrences is read, and asked about, again.
async fn invited_rows<T>(app: &App, me: &Me, id: &str, may: impl Fn(&Found) -> Answer<T>) -> Answer<(T, Vec<String>)> {
    for _ in 0..3 {
        let found = Found::of(&app.db, &me.id, id).await?;
        let let_through = may(&found)?;
        if let Some(rows) = found.rows_for(&app.db).await? {
            return Ok((let_through, rows));
        }
    }
    Err(Error::CONFLICT)
}

/// `PUT v2.0/me/calendars/events/<id>/respond` with `{"responseType"}`: an invited account's answer, which the
/// organizer reads on the event.
pub(super) async fn respond(
    State(app): State<Arc<App>>,
    me: Me,
    Path(id): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<StatusCode> {
    let input = body(input)?;
    let response = match input["responseType"].as_str() {
        Some("Accept") => Some("Accepted"),
        Some("TentativelyAccept") => Some("TentativelyAccepted"),
        Some("Decline") => Some("Declined"),
        _ => None,
    };
    let may = |found: &Found| {
        if found.organized_by(&me.id) || found.mine.is_none() {
            return Err(Error::FORBIDDEN);
        }
        response.filter(|_| !found.event.cancelled).ok_or(Error::BAD_REQUEST)
    };
    let (response, rows) = invited_rows(&app, &me, &id, may).await?;
    repo::set_response(&app.db, &rows, &me.id, response, crate::chatsvc::now()).await?;
    Ok(StatusCode::NO_CONTENT)
}
