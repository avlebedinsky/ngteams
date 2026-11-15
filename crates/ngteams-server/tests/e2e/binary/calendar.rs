//! The calendar and meetings as users of three ngteams clients meet them: the real server binary (one node on
//! SQLite; two on one Postgres and Redis, a at the first node, b and c at the second), accounts the operator added,
//! and each account's session, engine, store and push channel. All accounts are synthetic.

mod channels;
mod crowd;
mod events;
mod invites;
mod live;
mod meetings;
mod nodes;
mod options;
mod races;
mod repeats;
mod scheduler;

use reqwest::Method;
use serde_json::Value;
use teams_core::{EngineError, EventWrite};
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, EventAttendee, EventDraft, RsvpAnswer, ScheduledMeeting};

use super::push::support::{Rig, SPACES, User, token};
use super::support::Node;
use crate::calendar::meetings::invited;
use crate::calendar::{DAY, instant};
use crate::support::Backend;

/// The middle tier's base on the own server.
pub const MT: &str = "/api/mt/part/own-01";

/// Accounts a, b and c with their clients running: a at the first node, b and c at the other (the same one when
/// there is one node). d is an account of the server too, with no client running.
pub struct Trio {
    pub a: User,
    pub b: User,
    pub c: User,
    pub rig: Rig,
}

impl Trio {
    pub async fn start(backend: Backend) -> Self {
        let rig = Rig::start(backend).await;
        let a = rig.team_user(&rig.first, "a@own.test").await;
        let b = rig.team_user(rig.other(), "b@own.test").await;
        let c = rig.team_user(rig.other(), "c@own.test").await;
        Self { a, b, c, rig }
    }

    /// Another client of `upn`, at `node` (`true`: the first).
    pub async fn client(&self, upn: &str, first: bool) -> User {
        let node = if first { &self.rig.first } else { self.rig.other() };
        self.rig.team_user(node, upn).await
    }

    pub async fn stop(self) {
        let Self { a, b, c, rig } = self;
        drop((a, b, c));
        rig.stop().await;
    }
}

/// Someone invited, required, as the scheduling form names a picked account.
pub fn invite(user: &User) -> EventAttendee {
    invited(&user.upn, "QA Operator", Some(user.mri.as_str()), true)
}

/// The account's events of `days` days from `from`, read from the service now, by start.
pub async fn window(user: &User, from: i64, days: i64) -> Vec<CalendarEvent> {
    let mut events = user.engine().calendar(from, from + days * DAY).await.unwrap();
    events.sort_by_key(|event| instant(event.start));
    events
}

/// Each event as (subject, start).
pub fn shown(events: &[CalendarEvent]) -> Vec<(String, i64)> {
    events
        .iter()
        .map(|event| (event.subject.clone(), instant(event.start)))
        .collect()
}

pub async fn create(user: &User, draft: EventDraft) -> CalendarEvent {
    let written = user.engine().write_event(EventWrite::Create(draft)).await.unwrap();
    written.created.unwrap()
}

/// The event `id` changed to `draft`, from the event as the account reads it now.
pub async fn change(user: &User, id: &str, draft: EventDraft) -> Result<(), EngineError> {
    let whole = user.engine().calendar_event(id.to_owned()).await?;
    user.engine()
        .write_event(EventWrite::Update {
            id: id.to_owned(),
            event: whole.raw,
            draft,
        })
        .await
        .map(|_| ())
}

pub async fn respond(user: &User, id: &str, answer: RsvpAnswer) -> Result<(), EngineError> {
    user.engine()
        .write_event(EventWrite::Respond {
            id: id.into(),
            answer,
            send: true,
            comment: None,
        })
        .await
        .map(|_| ())
}

pub async fn cancel(user: &User, event: &CalendarEvent, note: Option<&str>) -> Result<(), EngineError> {
    user.engine()
        .write_event(EventWrite::Cancel {
            id: event.id.clone(),
            chat: event.meeting_chat.clone(),
            message: note.map(str::to_owned),
        })
        .await
        .map(|_| ())
}

pub async fn delete(user: &User, id: &str) -> Result<(), EngineError> {
    user.engine()
        .write_event(EventWrite::Delete { id: id.into() })
        .await
        .map(|_| ())
}

/// The write failed with `status`.
pub fn refused<T: std::fmt::Debug>(answer: Result<T, EngineError>, status: &str) {
    let error = answer.unwrap_err().to_string();
    assert!(error.contains(status), "wanted {status}: {error}");
}

/// The meetings a conversation's messages name, oldest first: (title, cancelled, the message's text), as `user`'s
/// client reads them from the chat service now.
pub async fn meeting_posts(user: &User, conversation: &str) -> Vec<(String, bool, String)> {
    let client = user.client();
    let page = ChatReads::new(&client).messages(conversation, 50).await.unwrap();
    let mut items: Vec<(String, bool, String)> = page
        .messages
        .iter()
        .filter_map(|message| {
            let properties = serde_json::to_string(&message.properties).unwrap();
            let meeting = ScheduledMeeting::of_properties(&properties)?;
            Some((meeting.title, meeting.cancelled, message.content.clone()))
        })
        .collect();
    items.reverse();
    items
}

/// `method path` on the middle tier at `node` as `upn`, with its own token: the status and the answer.
pub async fn raw(node: &Node, upn: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
    let token = token(node, upn, SPACES).await;
    let mut request = node
        .http
        .request(method, format!("{}{MT}{path}", node.endpoint))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or(Value::Null))
}
