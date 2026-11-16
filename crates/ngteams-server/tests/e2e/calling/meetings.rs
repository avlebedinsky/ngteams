//! Meetings on the group call controller: the client's engines (`teams-core`, `teams-media` on the file device
//! backend) join a meeting scheduled on the server's calendar, wait in its lobby until someone lets them in, raise
//! hands, react, are muted by the organizer and see the meeting ended for all; a channel's meeting is posted to its
//! channel, titled and joined the same way. Every account and meeting is synthetic.

use teams_core::{CallAction, CallId, CallStatus, EventWrite};
use teams_media::tone::Tone;
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, MeetingInfo};

use super::engines::{A_TONE, B_TONE, Engine};
use super::group_media::{C_TONE, Mixed};
use crate::calendar::meetings::invited;
use crate::calendar::{MONDAY, draft};
use crate::support::backends;
use crate::trouter::Account;

/// An account of the server with its engine: what an invitation names it by.
pub(super) struct Person {
    pub(super) engine: Engine,
    pub(super) upn: String,
    pub(super) name: String,
}

impl Person {
    pub(super) async fn new(mixed: &Mixed, name: &str, tone: Tone) -> Self {
        let tag = uuid::Uuid::new_v4().simple().to_string();
        let upn = format!("{}-{tag}@own.test", name.to_lowercase());
        let id = mixed.server().add_user(&upn, name).await;
        let account = Account::sign_in(mixed.server(), &upn, &id).await;
        let nodes = (mixed.server(), mixed.server());
        let engine = Engine::start_served(&account, name, nodes, (tone, mixed.dir())).await;
        Self {
            engine,
            upn,
            name: name.to_owned(),
        }
    }

    /// The account's own id (its MRI's object id).
    pub(super) fn id(&self) -> &str {
        self.engine.mri.trim_start_matches("8:orgid:")
    }
}

/// A Teams meeting `organizer` schedules on its calendar, inviting `guests`: its event (its chat and meeting).
pub(super) async fn schedule(organizer: &Person, guests: &[&Person]) -> CalendarEvent {
    let mut meeting = draft("Planning", MONDAY + 9 * crate::calendar::HOUR, 30);
    meeting.online = true;
    meeting.attendees = guests
        .iter()
        .map(|guest| invited(&guest.upn, &guest.name, None, true))
        .collect();
    let written = organizer
        .engine
        .handle
        .write_event(EventWrite::Create(meeting))
        .await
        .unwrap();
    let event = written.created.unwrap();
    for person in std::iter::once(organizer).chain(guests.iter().copied()) {
        assert!(person.engine.handle.sync().await.unwrap().failed.is_empty());
    }
    event
}

/// What joining the meeting `event` takes: its chat and its organizer.
pub(super) fn join_of(event: &CalendarEvent) -> CallAction {
    CallAction::JoinMeeting {
        chat: event.meeting_chat.clone().expect("a Teams meeting has a chat"),
        meeting: event.meeting.clone().expect("a Teams meeting names its organizer"),
    }
}

/// `person` joins the meeting by `join` and is in it with its media up.
pub(super) async fn joined(person: &mut Person, join: &CallAction) -> CallId {
    let call = person.engine.call(join.clone()).await;
    person.engine.status(call, &CallStatus::Connected).await;
    call
}

/// The `Event/Call` messages of `chat`, newest first, as `reader` reads them.
pub(super) async fn call_events(mixed: &Mixed, reader: &Person, chat: &str) -> Vec<String> {
    let client = reader.engine.account.client(mixed.server());
    let page = ChatReads::new(&client).messages(chat, 20).await.unwrap();
    page.messages
        .iter()
        .filter(|message| message.message_type.as_str() == "Event/Call")
        .map(|message| message.content.clone())
        .collect()
}

/// Each person hangs up its call and waits for its end.
pub(super) async fn leave_all(people: &mut [&mut Person], calls: &[CallId]) {
    for (person, call) in people.iter_mut().zip(calls) {
        person.engine.call(CallAction::HangUp { call: *call }).await;
        person.engine.ended(*call).await;
    }
}

pub(super) async fn shut_down(people: Vec<Person>, mixed: Mixed) {
    for person in people {
        person.engine.shut_down().await;
    }
    mixed.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_join_a_scheduled_meeting_nobody_rings_and_its_chat_tells_its_start_and_end() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        assert_eq!(
            event.meeting,
            Some(MeetingInfo {
                organizer_id: ada.id().to_owned(),
                tenant_id: event.meeting.clone().unwrap().tenant_id,
            })
        );
        // a guest may start the meeting's call before its organizer comes
        let b_call = joined(&mut bert, &join).await;
        let a_call = joined(&mut ada, &join).await;
        let c_call = joined(&mut cora, &join).await;
        mixed.connected(&mixed.open_call().await, 3).await;
        let organizer = ada.engine.mri.clone();
        let view = cora
            .engine
            .view(c_call, |view| {
                view.participants.len() == 2 && view.participants.iter().any(|party| party.mri == organizer)
            })
            .await;
        let shown = |mri: &str| view.participants.iter().find(|party| party.mri == mri).unwrap();
        assert!(shown(&organizer).organizer, "{view:?}");
        assert!(!shown(&bert.engine.mri).organizer, "{view:?}");
        let chat = event.meeting_chat.clone().unwrap();
        leave_all(&mut [&mut bert, &mut ada, &mut cora], &[b_call, a_call, c_call]).await;
        let events = call_events(&mixed, &cora, &chat).await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events[1].contains("callStarted"), "{events:?}");
        assert!(
            events[0].contains("<ended/>") && events[0].contains("count=\"3\""),
            "{events:?}"
        );
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}
