//! Meetings on the group call controller: the client's engines (`teams-core`, `teams-media` on the file device
//! backend) join a meeting scheduled on the server's calendar, wait in its lobby until someone lets them in, raise
//! hands, react, are muted by the organizer and see the meeting ended for all; a channel's meeting is posted to its
//! channel, titled and joined the same way. Every account and meeting is synthetic.

use reqwest::Method;
use serde_json::json;
use teams_core::engine::Event;
use teams_core::{CallAction, CallId, CallStatus, End, EventWrite};
use teams_media::tone::Tone;
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, MIDDLE_TIER_AUDIENCE, MeetingInfo, Mri};

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

/// The meeting's lobby option (`AutoAdmittedUsers`) saved as `value` by its organizer, as Teams' meeting options page
/// saves it (the native client opens that page in a browser).
pub(super) async fn admit_automatically(mixed: &Mixed, organizer: &Person, event: &CalendarEvent, value: &str) {
    let tenant = &event.meeting.as_ref().unwrap().tenant_id;
    let chat = event.meeting_chat.as_ref().unwrap();
    let path = format!(
        "/api/mt/part/own-01/beta/meetings/v1/options/{tenant}/{}/{chat}/0/0",
        organizer.id()
    );
    let token = crate::teams::security::token(mixed.server(), &organizer.upn, MIDDLE_TIER_AUDIENCE).await;
    let options = json!({"options": [{"name": "AutoAdmittedUsers", "currentValue": value}]});
    let (status, answer) =
        crate::teams::security::call(mixed.server(), &token, Method::POST, &path, Some(options)).await;
    assert_eq!(status, 200, "{answer}");
}

/// Until `person`'s lobby of `call` is told as `waiting` (by MRI).
pub(super) async fn lobby_is(person: &mut Person, call: CallId, waiting: &[&str]) {
    person
        .engine
        .next(|event| match event {
            Event::Lobby {
                call: of,
                waiting: told,
            } if *of == call => {
                let told: Vec<&str> = told.iter().map(|person| person.mri.as_str()).collect();
                (told == waiting).then_some(())
            }
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outsider_waits_in_the_lobby_until_the_organizer_admits_it_and_one_turned_away_never_comes_in() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut dan = Person::new(&mixed, "Dan", C_TONE).await;
        let mut eve = Person::new(&mixed, "Eve", C_TONE).await;
        let event = schedule(&ada, &[&bert]).await;
        admit_automatically(&mixed, &ada, &event, "InvitedUsers").await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        // an invited guest comes straight in
        let b_call = joined(&mut bert, &join).await;
        // someone the meeting doesn't invite waits, with no media, until let in
        let d_call = dan.engine.call(join.clone()).await;
        dan.engine.status(d_call, &CallStatus::Lobby).await;
        let waiting = dan.engine.mri.clone();
        lobby_is(&mut ada, a_call, &[&waiting]).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        let admit = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(waiting.clone()),
            admit: true,
        };
        ada.engine.call(admit).await;
        dan.engine.status(d_call, &CallStatus::Connected).await;
        lobby_is(&mut ada, a_call, &[]).await;
        mixed.connected(&id, 3).await;
        bert.engine
            .view(b_call, |view| {
                view.participants.iter().any(|party| party.mri == waiting)
            })
            .await;
        // one turned away ends its call and never comes in
        let e_call = eve.engine.call(join.clone()).await;
        eve.engine.status(e_call, &CallStatus::Lobby).await;
        let turned_away = eve.engine.mri.clone();
        lobby_is(&mut ada, a_call, &[&turned_away]).await;
        let deny = CallAction::Lobby {
            call: a_call,
            waiting: Mri::new(turned_away),
            admit: false,
        };
        ada.engine.call(deny).await;
        let end = eve.engine.ended(e_call).await;
        assert!(matches!(end, End::Remote(_)), "{end:?}");
        lobby_is(&mut ada, a_call, &[]).await;
        mixed.connected(&id, 3).await;
        leave_all(&mut [&mut dan, &mut bert, &mut ada], &[d_call, b_call, a_call]).await;
        shut_down(vec![ada, bert, dan, eve], mixed).await;
    }
}
