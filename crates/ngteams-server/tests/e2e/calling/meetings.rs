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
        // each view read is told after the wait for it began: bert was in before ada and cora came, ada before cora
        let (organizer, guest, last) = (ada.engine.mri.clone(), bert.engine.mri.clone(), cora.engine.mri.clone());
        bert.engine
            .view(b_call, |view| {
                view.participants
                    .iter()
                    .any(|party| party.mri == organizer && party.organizer)
            })
            .await;
        ada.engine
            .view(a_call, |view| {
                let shown = |mri: &str| view.participants.iter().find(|party| party.mri == mri);
                shown(&last).is_some() && shown(&guest).is_some_and(|party| !party.organizer)
            })
            .await;
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

/// Until `person`'s view of `call` shows `mri` with its hand raised (`raised`) or not.
async fn hand_shown(person: &mut Person, call: CallId, mri: &str, raised: bool) {
    person
        .engine
        .view(call, |view| {
            view.participants
                .iter()
                .any(|party| party.mri == mri && party.hand_raised == raised)
        })
        .await;
}

/// Until `person` is told `from` reacted with `reaction` in `call`.
async fn reaction_told(person: &mut Person, call: CallId, from: &str, reaction: &str) {
    person
        .engine
        .next(|event| match event {
            Event::CallReaction {
                call: of,
                from: by,
                reaction: told,
                ..
            } if *of == call && by.as_str() == from && told == reaction => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_hand_and_a_reaction_reach_everyone_in_the_meeting() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let b = bert.engine.mri.clone();
        bert.engine
            .call(CallAction::RaiseHand {
                call: b_call,
                raised: true,
            })
            .await;
        hand_shown(&mut ada, a_call, &b, true).await;
        hand_shown(&mut cora, c_call, &b, true).await;
        bert.engine
            .next(|event| match event {
                Event::CallOwn {
                    call,
                    hand_raised: true,
                    ..
                } if *call == b_call => Some(()),
                _ => None,
            })
            .await;
        let c = cora.engine.mri.clone();
        let react = CallAction::React {
            call: c_call,
            reaction: "applause".to_owned(),
        };
        cora.engine.call(react).await;
        reaction_told(&mut ada, a_call, &c, "applause").await;
        reaction_told(&mut bert, b_call, &c, "applause").await;
        reaction_told(&mut cora, c_call, &c, "applause").await;
        bert.engine
            .call(CallAction::RaiseHand {
                call: b_call,
                raised: false,
            })
            .await;
        hand_shown(&mut ada, a_call, &b, false).await;
        hand_shown(&mut cora, c_call, &b, false).await;
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// Until `person`'s view of `call` shows `mri` muted (`muted`) or not.
async fn muted_shown(person: &mut Person, call: CallId, mri: &str, muted: bool) {
    person
        .engine
        .view(call, |view| {
            view.participants
                .iter()
                .any(|party| party.mri == mri && party.muted == muted)
        })
        .await;
}

/// Until `person` is told the organizer muted it in `call`.
async fn muted_by_organizer(person: &mut Person, call: CallId) {
    person
        .engine
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call && state.muted_by_organizer.is_some() => Some(()),
            _ => None,
        })
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizer_mutes_everyone_else_who_see_it_and_may_unmute_themselves() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let b_call = joined(&mut bert, &join).await;
        let a_call = joined(&mut ada, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let (b, c) = (bert.engine.mri.clone(), cora.engine.mri.clone());
        muted_shown(&mut ada, a_call, &c, false).await;
        ada.engine.call(CallAction::MuteAll { call: a_call }).await;
        muted_by_organizer(&mut bert, b_call).await;
        muted_by_organizer(&mut cora, c_call).await;
        muted_shown(&mut ada, a_call, &b, true).await;
        muted_shown(&mut ada, a_call, &c, true).await;
        let a = ada.engine.mri.clone();
        muted_shown(&mut bert, b_call, &a, false).await;
        // a muted participant unmutes itself, as Teams' "You've been muted" card lets it
        bert.engine
            .call(CallAction::Mute {
                call: b_call,
                muted: false,
            })
            .await;
        muted_shown(&mut ada, a_call, &b, false).await;
        muted_shown(&mut cora, c_call, &b, false).await;
        leave_all(&mut [&mut ada, &mut bert, &mut cora], &[a_call, b_call, c_call]).await;
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_organizer_ends_the_meeting_for_everyone_in_it_and_its_chat_tells_the_end() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let mut cora = Person::new(&mixed, "Cora", C_TONE).await;
        let event = schedule(&ada, &[&bert, &cora]).await;
        let join = join_of(&event);
        let a_call = joined(&mut ada, &join).await;
        let b_call = joined(&mut bert, &join).await;
        let c_call = joined(&mut cora, &join).await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 3).await;
        ada.engine.call(CallAction::EndMeeting { call: a_call }).await;
        for (person, call) in [(&mut bert, b_call), (&mut cora, c_call)] {
            let end = person.engine.ended(call).await;
            assert!(matches!(end, End::Remote(_)), "{end:?}");
        }
        ada.engine.ended(a_call).await;
        let calls = ngteams_server::calling::open_calls(&mixed.server().app.db)
            .await
            .unwrap();
        assert!(calls.is_empty(), "{calls:?}");
        let chat = event.meeting_chat.clone().unwrap();
        let events = call_events(&mixed, &bert, &chat).await;
        assert!(
            events[0].contains("callEnded") && events[0].contains("count=\"3\""),
            "{events:?}"
        );
        shut_down(vec![ada, bert, cora], mixed).await;
    }
}

/// A team `organizer` makes with `others` in it, and a channel of it, once each person's engine has them: the channel.
async fn channel_of(mixed: &Mixed, organizer: &Person, others: &[&Person]) -> String {
    let id = organizer.id().to_owned();
    let maker = crate::teams::Seat::of(mixed.server(), &organizer.upn, id).await;
    let mut members = Vec::new();
    for other in others {
        members.push(crate::teams::Seat::of(mixed.server(), &other.upn, other.id().to_owned()).await);
    }
    let (team, group) = maker.make_team("Rocket", false).await;
    maker.add(&team, &group, &members.iter().collect::<Vec<_>>()).await;
    let channel = maker.make_channel(&team, &group, "Launch", true).await;
    for person in std::iter::once(organizer).chain(others.iter().copied()) {
        assert!(person.engine.handle.sync().await.unwrap().failed.is_empty());
    }
    maker.stop();
    for member in members {
        member.stop();
    }
    channel
}

/// The channel's posts `reader` reads, newest first: (its kind, its text, its meeting's title).
async fn posts(mixed: &Mixed, reader: &Person, channel: &str) -> Vec<(String, String, Option<String>)> {
    let client = reader.engine.account.client(mixed.server());
    let page = ChatReads::new(&client).messages(channel, 20).await.unwrap();
    page.messages
        .iter()
        .map(|message| {
            let meeting = match message.properties.get("meeting") {
                Some(serde_json::Value::String(text)) => serde_json::from_str(text).ok(),
                other => other.cloned(),
            };
            let title = meeting
                .as_ref()
                .and_then(|meeting: &serde_json::Value| meeting.get("meetingtitle")?.as_str())
                .map(str::to_owned);
            (message.message_type.as_str().to_owned(), message.content.clone(), title)
        })
        .collect()
}

/// The `<callId>` an `Event/Call` post names.
fn call_id(text: &str) -> Option<String> {
    let rest = text.split("<callId>").nth(1)?;
    Some(rest.split('<').next()?.to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_channels_meet_now_is_posted_titled_joined_from_its_post_and_told_ended() {
    for backend in backends() {
        let mixed = Mixed::start(backend).await;
        let mut ada = Person::new(&mixed, "Ada", A_TONE).await;
        let mut bert = Person::new(&mixed, "Bert", B_TONE).await;
        let channel = channel_of(&mixed, &ada, &[&bert]).await;
        let a_call = ada
            .engine
            .call(CallAction::MeetNow {
                channel: channel.clone(),
            })
            .await;
        ada.engine.status(a_call, &CallStatus::Connected).await;
        // the pre-join's title goes to the post the service makes, as Teams' web client titles it
        ada.engine
            .handle
            .requests()
            .title_meet_now(channel.clone(), "Sprint review")
            .await
            .unwrap();
        let listed = posts(&mixed, &bert, &channel).await;
        let started = listed
            .iter()
            .find(|(kind, text, _)| kind == "Event/Call" && text.contains("callStarted"))
            .unwrap_or_else(|| panic!("{listed:?}"));
        assert_eq!(started.2.as_deref(), Some("Sprint review"), "{listed:?}");
        let b_call = joined(
            &mut bert,
            &CallAction::JoinChannel {
                channel: channel.clone(),
            },
        )
        .await;
        let id = mixed.open_call().await;
        mixed.connected(&id, 2).await;
        let organizer = ada.engine.mri.clone();
        bert.engine
            .view(b_call, |view| {
                view.participants
                    .iter()
                    .any(|party| party.mri == organizer && party.organizer)
            })
            .await;
        leave_all(&mut [&mut bert, &mut ada], &[b_call, a_call]).await;
        let listed = posts(&mixed, &bert, &channel).await;
        let (_, ended, _) = &listed[0];
        assert!(ended.contains("callEnded"), "{listed:?}");
        assert_eq!(call_id(ended), call_id(&started.1), "{listed:?}");
        shut_down(vec![ada, bert], mixed).await;
    }
}
