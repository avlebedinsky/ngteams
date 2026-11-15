//! Online meetings (`docs/protocol/calendar.md`, "A Teams meeting's online meeting", "Meet now"): a meeting is a
//! chat-service thread of its own (`19:meeting_…@thread.v2`) with a join link that names it, its organizer and the
//! tenant, and a short link made of its numeric id and passcode. An event takes the meeting its form made; Meet
//! now makes one with no event.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use diesel::prelude::*;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

pub(super) use super::meeting_repo::{of_chat, of_chats};
use super::schema::meetings;
use super::{Answer, Error, Me, body, meeting_repo, wire};
use crate::app::App;
use crate::chatsvc::service;
use crate::db::{Db, DbError};

/// What a link's path segment or query value keeps as it is: letters, digits and `-._~`.
const KEPT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// How long a meeting's links are said to hold.
const HELD_MS: i64 = 60 * 24 * 3_600_000;

#[derive(Clone, Debug, Queryable, Selectable, Insertable)]
#[diesel(table_name = meetings)]
pub(crate) struct Meeting {
    pub chat_id: String,
    pub organizer_id: String,
    /// Its numeric id, as "Join with an ID" takes it.
    pub code: String,
    pub passcode: String,
    pub subject: Option<String>,
    /// `Scheduled` (an event's) or `MeetNow`.
    pub kind: String,
    pub created_at: i64,
}

impl Meeting {
    /// `<origin>/l/meetup-join/<chat>/0?context={"Tid","Oid"}`: what the client's join reads its chat, tenant and
    /// organizer from.
    pub(super) fn join_link(&self, app: &App) -> String {
        join_link(app, &self.chat_id, "0", &self.organizer_id)
    }

    /// `<origin>/meet/<id>?p=<passcode>`.
    pub(super) fn short_link(&self, app: &App) -> String {
        format!("{}/meet/{}?p={}", app.config.origin(), self.code, self.passcode)
    }

    /// The meeting as the scheduling service answers its creation.
    fn answer(&self, app: &App) -> Value {
        let join = self.join_link(app);
        json!({
            "expiryTime": wire::iso(self.created_at + HELD_MS),
            "groupContext": {"threadId": self.chat_id},
            "etag": self.created_at.to_string(),
            "subject": self.subject,
            "meetingUrl": join,
            "links": {"join": join, "shortJoinUrl": self.short_link(app)},
            "onlineMeetingConferenceId": self.code,
        })
    }
}

/// A join link of the thread `thread` (a meeting's chat, or a channel with the meeting's post).
pub(super) fn join_link(app: &App, thread: &str, message: &str, organizer: &str) -> String {
    let context = json!({"Tid": app.issuer.tenant(), "Oid": organizer}).to_string();
    format!(
        "{}/l/meetup-join/{}/{message}?context={}",
        app.config.origin(),
        utf8_percent_encode(thread, KEPT),
        utf8_percent_encode(&context, KEPT)
    )
}

/// Makes a meeting `organizer` organizes, with its chat.
pub(super) async fn make(app: &App, organizer: &str, kind: &str, subject: Option<&str>) -> Result<Meeting, DbError> {
    let mut tries = 0;
    loop {
        let random = uuid::Uuid::new_v4().as_u128();
        let meeting = Meeting {
            chat_id: format!("19:meeting_{}@thread.v2", uuid::Uuid::new_v4().simple()),
            organizer_id: organizer.to_owned(),
            code: (1_000_000_000_000 + random % 9_000_000_000_000).to_string(),
            passcode: format!("{:012x}", (random >> 64) & 0xffff_ffff_ffff),
            subject: subject.map(str::to_owned),
            kind: kind.to_owned(),
            created_at: crate::chatsvc::now(),
        };
        service::open_thread(&app.db, &meeting.chat_id, organizer, subject).await?;
        tries += 1;
        match meeting_repo::insert(&app.db, &meeting).await {
            Ok(()) => return Ok(meeting),
            Err(DbError::Conflict) if tries < 3 => {}
            Err(error) => return Err(error),
        }
    }
}

/// Names the meeting and its chat after its event.
pub(super) async fn rename(db: &Db, chat: &str, subject: &str) -> Result<(), DbError> {
    meeting_repo::set_subject(db, chat, subject).await?;
    service::set_topic(db, chat, subject).await
}

/// `POST beta/me/calendarEvents/privateMeeting/schedulingService/create`: a meeting for the event the form is
/// about to save (`Scheduled`), or one to start now (`MeetNow`, named).
pub(super) async fn create(State(app): State<Arc<App>>, me: Me, input: Result<Json<Value>, JsonRejection>) -> Answer {
    let input = body(input)?;
    let kind = match input["meetingType"].as_str() {
        Some("Scheduled") => "Scheduled",
        Some("MeetNow") => "MeetNow",
        _ => return Err(Error::BAD_REQUEST),
    };
    let subject = match &input["subject"] {
        Value::Null => None,
        Value::String(subject) if subject.chars().count() <= 255 => Some(subject.trim()).filter(|s| !s.is_empty()),
        _ => return Err(Error::BAD_REQUEST),
    };
    let meeting = make(&app, &me.id, kind, subject).await?;
    Ok((StatusCode::CREATED, Json(json!({"value": meeting.answer(&app)}))).into_response())
}

/// What a meeting's chat says of its meeting, in the thread's properties (`properties.meeting`, JSON as text: the
/// organizer, the tenant, the title, the join link), as the client reads a meeting it joins by its chat.
pub(crate) async fn describe_thread(app: &App, thread: &str, properties: &mut Value) -> Result<(), DbError> {
    if !thread.starts_with("19:meeting_") {
        return Ok(());
    }
    let Some(meeting) = of_chat(&app.db, thread).await? else {
        return Ok(());
    };
    properties["threadType"] = json!("meeting");
    properties["meeting"] = json!(
        json!({
            "organizerId": meeting.organizer_id, "tenantId": app.issuer.tenant(),
            "subject": meeting.subject, "meetingJoinUrl": meeting.join_link(app),
            "meetingType": meeting.kind,
        })
        .to_string()
    );
    Ok(())
}
