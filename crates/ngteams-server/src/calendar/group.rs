//! A team's group calendar: a meeting scheduled in one of its channels (`docs/protocol/calling.md`, "Schedule a
//! meeting"). The calendar is addressed by the team's mailbox (`smtp:<group id>@<host>`) and a meeting by its
//! `iCalUID`. The meeting is the team's: it is posted to its channel, anyone on the team reads it there, and only
//! who scheduled it changes or cancels it. That account has a copy on its own calendar, as has anyone it invites
//! by name and, with personal invites, everyone in the channel.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::Value;

use super::activity::{self, Change};
use super::draft::Draft;
use super::repo::{self, Attendee, Event};
use super::wire::{self, Shown};
use super::{Answer, Error, Found, Me, body, book, invite, meeting, people, write};
use crate::app::App;
use crate::chatsvc::service;
use crate::db::users::{self, User};
use crate::teams::repo::{self as teams, Channel, Team};

/// The team whose mailbox a path names, when the caller is on it; anyone else learns only that it isn't there.
async fn team_of(app: &App, me: &Me, group: &str) -> Answer<Team> {
    let host = app.config.public_origin.host_str().unwrap_or("localhost");
    let id = group
        .strip_prefix("smtp:")
        .and_then(|mailbox| mailbox.rsplit_once('@'))
        .filter(|(_, domain)| domain.eq_ignore_ascii_case(host))
        .map(|(id, _)| id)
        .ok_or(Error::NOT_FOUND)?;
    let team = teams::team_by_group(&app.db, id).await?.ok_or(Error::NOT_FOUND)?;
    teams::membership(&app.db, &team.id, &me.id)
        .await?
        .ok_or(Error::NOT_FOUND)?;
    Ok(team)
}

/// The channel of `team` a sent event names (`skypeTeamsData`'s `cid`), when the caller is in it.
async fn channel_of(app: &App, me: &Me, team: &Team, sent: &Value) -> Answer<Channel> {
    let data: Value = sent["skypeTeamsData"]
        .as_str()
        .and_then(|data| serde_json::from_str(data).ok())
        .ok_or(Error::BAD_REQUEST)?;
    let id = data["cid"].as_str().ok_or(Error::BAD_REQUEST)?;
    let channel = teams::channel(&app.db, id)
        .await?
        .filter(|channel| channel.team_id == team.id)
        .ok_or(Error::NOT_FOUND)?;
    if teams::joined(&app.db, &me.id, std::slice::from_ref(&channel.id))
        .await?
        .is_empty()
    {
        return Err(Error::NOT_FOUND);
    }
    if team.archived || channel.archived {
        return Err(Error::FORBIDDEN);
    }
    Ok(channel)
}

/// The team's meeting `uid`, while it isn't cancelled, for someone in its channel: a private channel's meeting is
/// its people's alone.
async fn meeting_of(app: &App, me: &Me, team: &Team, uid: &str) -> Answer<Event> {
    let found = repo::event(&app.db, uid, "").await?;
    let event = found
        .map(|(event, _)| event)
        .filter(|event| event.team_id.as_deref() == Some(team.id.as_str()) && !event.cancelled)
        .ok_or(Error::NOT_FOUND)?;
    let channel = event.channel_id.clone().unwrap_or_default();
    if teams::joined(&app.db, &me.id, &[channel]).await?.is_empty() {
        return Err(Error::NOT_FOUND);
    }
    Ok(event)
}

/// Whom the meeting has on their own calendars: who scheduled it, whom it names, and with `inviteAllMembers`
/// everyone in its channel now.
async fn wanted(app: &App, me: &str, sent: &Value, channel: &str, mailbox: &str) -> Answer<Vec<(String, bool)>> {
    let named = people::invitees(app, me, &sent["attendees"], Some(mailbox)).await?;
    let mut wanted = vec![(me.to_owned(), false)];
    wanted.extend(named.into_iter().map(|invitee| (invitee.user.id, invitee.optional)));
    if sent["inviteAllMembers"].as_bool() == Some(true) {
        for member in service::members(&app.db, channel).await? {
            if !wanted.iter().any(|(known, _)| *known == member) {
                wanted.push((member, false));
            }
        }
    }
    Ok(wanted)
}

fn mailbox(app: &App, team: &Team) -> String {
    let host = app.config.public_origin.host_str().unwrap_or("localhost");
    format!("{}@{host}", team.group_id)
}

async fn account(app: &App, id: &str) -> Answer<User> {
    users::by_id(&app.db, id).await?.ok_or(Error::FORBIDDEN)
}

/// The join link of a channel's meeting: the channel and the meeting's own post.
fn link(app: &App, event: &Event) -> String {
    let channel = event.channel_id.as_deref().unwrap_or_default();
    let post = event.post_id.unwrap_or(0).to_string();
    meeting::join_link(app, channel, &post, &event.owner_id)
}

/// `POST v2.0/groups/smtp:<mailbox>/calendars/default/events` with the event itself: a meeting in a channel of the
/// team, posted there.
pub(super) async fn create(
    State(app): State<Arc<App>>,
    me: Me,
    Path(group): Path<String>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer {
    let sent = body(input)?;
    let team = team_of(&app, &me, &group).await?;
    let channel = channel_of(&app, &me, &team, &sent).await?;
    let draft = Draft::read(&sent, true)?;
    let wanted = wanted(&app, &me.id, &sent, &channel.id, &mailbox(&app, &team)).await?;
    let organizer = account(&app, &me.id).await?;
    let mut fields = draft.body;
    fields.insert(
        "inviteAllMembers".into(),
        (sent["inviteAllMembers"].as_bool() == Some(true)).into(),
    );
    let mut event = Event {
        id: uuid::Uuid::new_v4().simple().to_string(),
        owner_id: me.id.clone(),
        team_id: Some(team.id.clone()),
        channel_id: Some(channel.id.clone()),
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
        body: Value::Object(fields).to_string(),
        version: crate::chatsvc::now(),
    };
    let rows: Vec<Attendee> = write::invitations(&event.id, &wanted)
        .into_iter()
        .map(|row| accepted_by(row, &me.id))
        .collect();
    repo::insert(&app.db, &event, &rows).await?;
    invite::posted(&app, &mut event, &organizer, |event| link(&app, event)).await?;
    activity::tell(&app, &write::ids(&wanted), Change::Created, &event, &organizer).await;
    let book = book(&app, &[&event]).await?;
    let shown = Shown {
        reader: &me.id,
        event: &event,
        occurrence: None,
        mine: rows.first(),
        book: &book,
        team_view: false,
    };
    Ok((StatusCode::CREATED, Json(wire::events(vec![shown.entry()]))).into_response())
}

/// Who scheduled a meeting has accepted it.
fn accepted_by(mut row: Attendee, scheduler: &str) -> Attendee {
    if row.user_id == scheduler {
        row.response = "Accepted".into();
    }
    row
}

/// `GET v2.0/groups/smtp:<mailbox>/calendars/events/iCalUId/<uid>`: the meeting as the team's calendar holds it,
/// the same for whoever on the team reads it.
pub(super) async fn read(
    State(app): State<Arc<App>>,
    me: Me,
    Path((group, uid)): Path<(String, String)>,
) -> Answer<Json<Value>> {
    let team = team_of(&app, &me, &group).await?;
    let event = meeting_of(&app, &me, &team, &uid).await?;
    let book = book(&app, &[&event]).await?;
    let attendees = repo::attendees(&app.db, &event.id).await?;
    let shown = Shown {
        reader: &me.id,
        event: &event,
        occurrence: None,
        mine: None,
        book: &book,
        team_view: true,
    };
    Ok(Json(shown.full(&attendees)))
}

/// The meeting a change or a cancellation names, for who scheduled it: the whole of it, or the one occurrence its
/// `fetchEventContract` names by its kind and start.
async fn own(app: &App, me: &Me, group: &str, uid: &str, contract: &Value) -> Answer<(Team, Found)> {
    let team = team_of(app, me, group).await?;
    let event = meeting_of(app, me, &team, uid).await?;
    if event.owner_id != me.id {
        return Err(Error::FORBIDDEN);
    }
    let one = matches!(contract["eventType"].as_str(), Some("Occurrence" | "Exception")) && event.recurring;
    if !one {
        let found = Found {
            event,
            mine: None,
            occurrence: None,
        };
        return Ok((team, found));
    }
    let start = contract["startTime"]
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp_millis())
        .ok_or(Error::BAD_REQUEST)?;
    let found = Found::of(&app.db, &me.id, &wire::occurrence_id(&event.id, start)).await?;
    Ok((team, found))
}

/// `PUT v2.0/groups/smtp:<mailbox>/calendars/events/<uid>` with `{"updatedCalendarEvent", "fetchEventContract"}`:
/// the change of who scheduled it; the channel's post is edited in place.
pub(super) async fn update(
    State(app): State<Arc<App>>,
    me: Me,
    Path((group, uid)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<StatusCode> {
    let input = body(input)?;
    let (team, found) = own(&app, &me, &group, &uid, &input["fetchEventContract"]).await?;
    let one = found.one_of_a_series();
    let mut sent = input["updatedCalendarEvent"].clone();
    if let (true, Some(sent)) = (one, sent.as_object_mut()) {
        sent.remove("eventRecurrencePattern");
        sent.remove("eventRecurrenceRange");
    }
    let mut draft = Draft::read(&sent, !one)?;
    let before: Value = serde_json::from_str(&found.event.body).unwrap_or_default();
    let all = sent["inviteAllMembers"]
        .as_bool()
        .or(before["inviteAllMembers"].as_bool())
        == Some(true);
    draft.body.insert("inviteAllMembers".into(), all.into());
    sent["inviteAllMembers"] = all.into();
    let channel = found.event.channel_id.clone().unwrap_or_default();
    let wanted = wanted(&app, &me.id, &sent, &channel, &mailbox(&app, &team)).await?;
    let (row, reshaped) = if one {
        (write::occurrence_changed(&app, &found, draft).await?, false)
    } else {
        write::whole_changed(&app, &found, draft).await?
    };
    let moved = (found.event.start_ms, found.event.end_ms) != (row.start_ms, row.end_ms) && found.occurrence.is_none();
    let mut rows = vec![row.id.clone()];
    if found.event.recurring && !reshaped {
        rows.extend(repo::own_row_ids(&app.db, &row.id).await?);
    }
    repo::sync_attendees(&app.db, &rows, &wanted, moved).await?;
    repo::set_response(&app.db, &rows, &me.id, "Accepted", row.version).await?;
    let organizer = account(&app, &me.id).await?;
    if !one {
        invite::changed(&app, &channel, &row, &link(&app, &row), &organizer).await?;
    }
    activity::tell(&app, &write::ids(&wanted), Change::Updated, &row, &organizer).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT v2.0/groups/smtp:<mailbox>/calendars/events/<uid>/cancel`: the cancellation of who scheduled it, told
/// under the channel's post; the copies on people's calendars are cancelled, the scheduler's own taken off.
pub(super) async fn cancel(
    State(app): State<Arc<App>>,
    me: Me,
    Path((group, uid)): Path<(String, String)>,
    input: Result<Json<Value>, JsonRejection>,
) -> Answer<StatusCode> {
    let input = body(input)?;
    let (_, found) = own(&app, &me, &group, &uid, &input["fetchEventContract"]).await?;
    let note = match &input["cancellationMessage"] {
        Value::Null => None,
        Value::String(note) if note.chars().count() <= 1000 => Some(note.as_str()),
        _ => return Err(Error::BAD_REQUEST),
    };
    let at = crate::chatsvc::now();
    let mut row = found.own_row();
    row.cancelled = true;
    row.version = at;
    let rows = if found.one_of_a_series() {
        repo::put_own_row(&app.db, &row).await?;
        vec![row.id.clone()]
    } else {
        repo::cancel(&app.db, &row.id, at).await?;
        let mut rows = vec![row.id.clone()];
        rows.extend(repo::own_row_ids(&app.db, &row.id).await?);
        rows
    };
    let attendees = repo::attendees(&app.db, &found.event.id).await?;
    repo::set_removed(&app.db, &rows, &me.id).await?;
    let organizer = account(&app, &me.id).await?;
    let channel = row.channel_id.clone().unwrap_or_default();
    invite::cancelled(&app, &channel, &row, &link(&app, &row), &organizer, note).await?;
    let told: Vec<String> = attendees.into_iter().map(|(attendee, _)| attendee.user_id).collect();
    activity::tell(&app, &told, Change::Canceled, &row, &organizer).await;
    Ok(StatusCode::NO_CONTENT)
}
