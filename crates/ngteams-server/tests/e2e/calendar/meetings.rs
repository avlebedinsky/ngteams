use teams_core::{EngineError, EventWrite};
use teams_proto::chat::ChatReads;
use teams_proto::{
    CalendarEvent, EventAttendee, EventKind, EventResponse, EventTime, FullEvent, MeetingCode, RsvpAnswer,
    ScheduledMeeting, ShowAs, Weekday,
};

use super::events::weekly;
use super::{DAY, HOUR, MINUTE, MONDAY, Seat, draft, server, week};
use crate::support::backends;
use crate::teams::ORIGIN;

pub(crate) fn invited(upn: &str, name: &str, mri: Option<&str>, required: bool) -> EventAttendee {
    EventAttendee {
        address: upn.into(),
        name: Some(name.into()),
        mri: mri.map(str::to_owned),
        required,
    }
}

pub(crate) async fn create(seat: &Seat, draft: teams_proto::EventDraft) -> CalendarEvent {
    let written = seat.engine.write_event(EventWrite::Create(draft)).await.unwrap();
    written.created.unwrap()
}

/// What an account's answer to the event `id` gives.
pub(crate) async fn respond(seat: &Seat, id: &str, answer: RsvpAnswer) -> Result<(), EngineError> {
    seat.engine
        .write_event(EventWrite::Respond {
            id: id.into(),
            answer,
            send: true,
            comment: Some("See you".into()),
        })
        .await
        .map(|_| ())
}

async fn delete(seat: &Seat, id: &str) -> Result<(), EngineError> {
    seat.engine
        .write_event(EventWrite::Delete { id: id.into() })
        .await
        .map(|_| ())
}

async fn cancel(seat: &Seat, event: &CalendarEvent, message: &str) -> Result<(), EngineError> {
    seat.engine
        .write_event(EventWrite::Cancel {
            id: event.id.clone(),
            chat: event.meeting_chat.clone(),
            message: Some(message.into()),
        })
        .await
        .map(|_| ())
}

fn refused(answer: Result<(), EngineError>, status: &str) {
    let error = answer.unwrap_err().to_string();
    assert!(error.contains(status), "{error}");
}

/// Whom an event read whole names: (name, address, answer, optional).
fn roster(full: &FullEvent) -> Vec<(String, String, Option<EventResponse>, bool)> {
    full.event
        .attendees
        .iter()
        .map(|attendee| {
            (
                attendee.name.clone().unwrap_or_default(),
                attendee.address.clone().unwrap_or_default(),
                attendee.response.clone(),
                attendee.optional,
            )
        })
        .collect()
}

/// The meetings the messages of a chat name, oldest first: (title, cancelled, the message's text).
async fn chat_items(seat: &Seat, chat: &str) -> Vec<(String, bool, String)> {
    let page = ChatReads::new(&*seat.client).messages(chat, 50).await.unwrap();
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

/// Ada's meeting with Bert and (optional) Cora; Dan is not invited.
struct Planning {
    ada: Seat,
    bert: Seat,
    cora: Seat,
    dan: Seat,
    draft: teams_proto::EventDraft,
    created: CalendarEvent,
    chat: String,
    link: String,
}

const PLANNING: i64 = MONDAY + 14 * HOUR;

impl Planning {
    async fn made(server: &crate::support::TestServer) -> Self {
        let ada = Seat::new(server, "ada@own.test", "Ada").await;
        let bert = Seat::new(server, "bert@own.test", "Bert").await;
        let cora = Seat::new(server, "cora@own.test", "Cora").await;
        let dan = Seat::new(server, "dan@own.test", "Dan").await;
        let mut draft = draft("Planning", PLANNING, 30);
        draft.online = true;
        draft.attendees = vec![
            invited("Bert@own.test", "Bert", Some(&bert.mri), true),
            invited("cora@own.test", "Cora", None, false),
        ];
        let created = create(&ada, draft.clone()).await;
        Self {
            chat: created.meeting_chat.clone().unwrap(),
            link: created.join_url.clone().unwrap(),
            ada,
            bert,
            cora,
            dan,
            draft,
            created,
        }
    }

    /// The meeting names its chat, its link, its organizer and tenant, and its short link's id.
    async fn names_its_meeting(&self, tenant: &str) {
        let created = &self.created;
        assert!(created.online && created.is_organizer);
        let prefix = format!("{ORIGIN}/l/meetup-join/19%3Ameeting_");
        assert!(self.link.starts_with(&prefix), "{}", self.link);
        assert!(teams_proto::is_meeting_chat_id(&self.chat), "{}", self.chat);
        let meeting = created.meeting.clone().unwrap();
        assert_eq!(
            (meeting.organizer_id.as_str(), meeting.tenant_id.as_str()),
            (self.ada.id.as_str(), tenant)
        );
        let code = created.meeting_code.clone().unwrap();
        assert!(code.id.len() >= 9 && !code.passcode.is_empty(), "{code:?}");
        assert_eq!(created.location.as_deref(), Some("Microsoft Teams Meeting"));
        self.chat_names_it(&meeting).await;
    }

    async fn chat_names_it(&self, meeting: &teams_proto::MeetingInfo) {
        // the meeting's chat names the meeting to the people in it, and holds the invitation
        for seat in [&self.ada, &self.bert, &self.cora] {
            let details = seat.engine.meeting(self.chat.clone()).await.unwrap().unwrap();
            assert_eq!(&details.meeting, meeting);
            assert_eq!(details.title.as_deref(), Some("Planning"));
            assert_eq!(details.join_url.as_deref(), Some(self.link.as_str()));
        }
        assert!(self.dan.engine.meeting(self.chat.clone()).await.is_err());
        let listed = ChatReads::new(&*self.bert.client).conversations().await.unwrap();
        assert!(listed.conversations.iter().any(|entry| entry.id == self.chat));
        assert_eq!(
            chat_items(&self.bert, &self.chat).await,
            [("Planning".to_owned(), false, "Scheduled a meeting".to_owned())]
        );
    }

    /// Its organizer reads whom it invites; each of them has it, not answered yet; nobody else has it.
    async fn is_on_its_peoples_calendars(&self) {
        let whole = self.ada.engine.calendar_event(self.created.id.clone()).await.unwrap();
        let none = Some(EventResponse::NoResponse);
        assert_eq!(
            roster(&whole),
            [
                (
                    "Ada".into(),
                    "ada@own.test".into(),
                    Some(EventResponse::Organizer),
                    false
                ),
                ("Bert".into(), "bert@own.test".into(), none.clone(), false),
                ("Cora".into(), "cora@own.test".into(), none, true),
            ]
        );
        assert_eq!(whole.optional, ["cora@own.test"]);
        self.invited_have_it().await;
    }

    async fn invited_have_it(&self) {
        for seat in [&self.bert, &self.cora] {
            let events = week(seat).await;
            assert_eq!(events.len(), 1, "{events:?}");
            let event = &events[0];
            assert_eq!(event.id, self.created.id);
            assert!(!event.is_organizer && event.online);
            assert_eq!(event.response, EventResponse::NotResponded);
            assert_eq!(event.show_as, ShowAs::Tentative);
            assert_eq!(event.organizer_name.as_deref(), Some("Ada"));
            assert_eq!(event.join_url.as_deref(), Some(self.link.as_str()));
            assert_eq!(event.meeting_chat.as_deref(), Some(self.chat.as_str()));
        }
        self.nobody_else_has_it().await;
    }

    async fn nobody_else_has_it(&self) {
        let theirs = self.bert.engine.calendar_event(self.created.id.clone()).await.unwrap();
        assert_eq!(roster(&theirs).len(), 3);
        assert!(week(&self.dan).await.is_empty());
        assert!(self.dan.engine.calendar_event(self.created.id.clone()).await.is_err());
    }

    /// Answers: each account's own, read back by the organizer.
    async fn is_answered(&self) {
        let id = &self.created.id;
        respond(&self.bert, id, RsvpAnswer::Accept).await.unwrap();
        respond(&self.cora, id, RsvpAnswer::Decline).await.unwrap();
        refused(respond(&self.dan, id, RsvpAnswer::Accept).await, "404");
        refused(respond(&self.ada, id, RsvpAnswer::Accept).await, "403");
        let events = week(&self.bert).await;
        assert_eq!(
            (&events[0].response, &events[0].show_as),
            (&EventResponse::Accepted, &ShowAs::Busy)
        );
        assert_eq!(week(&self.cora).await[0].response, EventResponse::Declined);
        let whole = self.ada.engine.calendar_event(id.clone()).await.unwrap();
        let answers: Vec<Option<EventResponse>> = roster(&whole).into_iter().map(|row| row.2).collect();
        assert_eq!(
            answers,
            [
                Some(EventResponse::Organizer),
                Some(EventResponse::Accepted),
                Some(EventResponse::Declined)
            ]
        );
    }

    /// Only its organizer changes it; the change reaches the people it now invites: a new time asks for a new
    /// answer, who was taken off no longer has it, who was added has it and is in its chat.
    async fn is_changed(&self) {
        let id = &self.created.id;
        let mut later = self.draft.clone();
        later.subject = "Planning, later".into();
        later.start.ms += HOUR;
        later.end.ms += HOUR;
        later.attendees = vec![
            invited("bert@own.test", "Bert", Some(&self.bert.mri), true),
            invited("dan@own.test", "Dan", Some(&self.dan.mri), true),
        ];
        let change = |raw: &FullEvent| EventWrite::Update {
            id: id.clone(),
            event: raw.raw.clone(),
            draft: later.clone(),
        };
        let theirs = self.bert.engine.calendar_event(id.clone()).await.unwrap();
        refused(self.bert.engine.write_event(change(&theirs)).await.map(|_| ()), "403");
        refused(cancel(&self.bert, &self.created, "No").await, "403");
        let whole = self.ada.engine.calendar_event(id.clone()).await.unwrap();
        self.ada.engine.write_event(change(&whole)).await.unwrap();

        self.change_reached_them().await;
    }

    async fn change_reached_them(&self) {
        let events = week(&self.bert).await;
        assert_eq!(events[0].subject, "Planning, later");
        assert_eq!(events[0].start, EventTime::Instant(PLANNING + HOUR));
        assert_eq!(events[0].response, EventResponse::NotResponded);
        assert!(week(&self.cora).await.is_empty());
        assert_eq!(week(&self.dan).await.len(), 1);
        assert_eq!(week(&self.ada).await[0].join_url.as_deref(), Some(self.link.as_str()));
        self.chat_follows_the_change().await;
    }

    async fn chat_follows_the_change(&self) {
        let details = self.dan.engine.meeting(self.chat.clone()).await.unwrap().unwrap();
        assert_eq!(details.title.as_deref(), Some("Planning, later"));
        assert!(self.cora.engine.meeting(self.chat.clone()).await.is_err());
        assert_eq!(
            chat_items(&self.dan, &self.chat).await,
            [("Planning, later".to_owned(), false, "Scheduled a meeting".to_owned())]
        );
    }

    /// Cancelled: gone for its organizer, cancelled for the others until they take it off.
    async fn is_cancelled(&self) {
        let id = &self.created.id;
        let current = week(&self.ada).await.remove(0);
        cancel(&self.ada, &current, "Postponed").await.unwrap();
        assert!(week(&self.ada).await.is_empty());
        let events = week(&self.bert).await;
        assert!(events[0].cancelled);
        assert_eq!(events[0].subject, "Canceled: Planning, later");
        refused(respond(&self.bert, id, RsvpAnswer::Accept).await, "400");
        self.cancellation_reached_them().await;
    }

    async fn cancellation_reached_them(&self) {
        let id = &self.created.id;
        let items = chat_items(&self.bert, &self.chat).await;
        assert_eq!(items.len(), 2, "{items:?}");
        let text = "The meeting \"Planning, later\" is cancelled: Postponed";
        assert_eq!(items[1], ("Planning, later".to_owned(), true, text.to_owned()));
        delete(&self.bert, id).await.unwrap();
        assert!(week(&self.bert).await.is_empty());
        refused(delete(&self.bert, id).await, "404");
        assert_eq!(week(&self.dan).await.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_teams_meeting_reaches_whom_it_invites_with_its_chat_its_link_and_their_answers() {
    for backend in backends() {
        let server = server(backend).await;
        let tenant = ngteams_server::db::meta::tenant(&server.app.db).await.unwrap();
        let planning = Planning::made(&server).await;
        planning.names_its_meeting(&tenant).await;
        planning.is_on_its_peoples_calendars().await;
        planning.is_answered().await;
        planning.is_changed().await;
        planning.is_cancelled().await;
        for seat in [planning.ada, planning.bert, planning.cora, planning.dan] {
            seat.stop();
        }
        server.stop().await;
    }
}

pub(crate) fn answers(events: &[CalendarEvent]) -> Vec<EventResponse> {
    events.iter().map(|event| event.response.clone()).collect()
}

/// One occurrence answered is still an occurrence; the series answered is every occurrence answered.
async fn answered_one_then_all(ada: &Seat, bert: &Seat, master: &CalendarEvent, events: &[CalendarEvent]) {
    let (wednesday, friday) = (&events[1], &events[2]);
    respond(bert, &wednesday.id, RsvpAnswer::Accept).await.unwrap();
    let after = week(bert).await;
    assert_eq!(
        answers(&after),
        [
            EventResponse::NotResponded,
            EventResponse::Accepted,
            EventResponse::NotResponded
        ]
    );
    assert_eq!(after[1].kind, EventKind::Occurrence);
    assert_eq!(after[1].id, wednesday.id);
    let whole = ada.engine.calendar_event(wednesday.id.clone()).await.unwrap();
    assert_eq!(roster(&whole)[1].2, Some(EventResponse::Accepted));
    let whole = ada.engine.calendar_event(friday.id.clone()).await.unwrap();
    assert_eq!(roster(&whole)[1].2, Some(EventResponse::NoResponse));
    respond(bert, &master.id, RsvpAnswer::Tentative).await.unwrap();
    assert_eq!(answers(&week(bert).await), vec![EventResponse::Tentative; 3]);
    let next = bert.engine.calendar(MONDAY + 7 * DAY, MONDAY + 14 * DAY).await.unwrap();
    assert_eq!(answers(&next), vec![EventResponse::Tentative; 3]);
}

/// One occurrence cancelled: its organizer no longer has it, the invited account has it cancelled until it takes
/// it off; taken off one calendar, an occurrence stays on the other's. Then the whole series.
async fn cancelled_one_then_all(ada: &Seat, bert: &Seat, master: &CalendarEvent, events: &[CalendarEvent]) {
    let (monday, wednesday, friday) = (&events[0], &events[1], &events[2]);
    cancel(ada, monday, "Holiday").await.unwrap();
    let own = week(ada).await;
    assert_eq!(own.len(), 2, "{own:?}");
    let after = week(bert).await;
    assert_eq!(after.len(), 3);
    assert!(after[0].cancelled && !after[1].cancelled);
    assert_eq!(after[0].subject, "Canceled: Sync");
    delete(bert, &after[0].id).await.unwrap();
    delete(bert, &friday.id).await.unwrap();
    one_left(ada, bert, wednesday).await;
    all_cancelled(ada, bert, master).await;
}

async fn one_left(ada: &Seat, bert: &Seat, wednesday: &CalendarEvent) {
    let after = week(bert).await;
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].id, wednesday.id);
    assert_eq!(week(ada).await.len(), 2);
}

async fn all_cancelled(ada: &Seat, bert: &Seat, master: &CalendarEvent) {
    cancel(ada, master, "Done").await.unwrap();
    assert!(ada.engine.calendar(MONDAY, MONDAY + 28 * DAY).await.unwrap().is_empty());
    let after = bert.engine.calendar(MONDAY, MONDAY + 14 * DAY).await.unwrap();
    assert_eq!(after.len(), 4, "{after:?}");
    assert!(after.iter().all(|event| event.cancelled));
    delete(bert, &master.id).await.unwrap();
    assert!(
        bert.engine
            .calendar(MONDAY, MONDAY + 28 * DAY)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_series_of_meetings_is_answered_and_cancelled_whole_or_one_at_a_time() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let mut sync = draft("Sync", MONDAY + 10 * HOUR, 30);
        sync.online = true;
        sync.attendees = vec![invited("bert@own.test", "Bert", Some(&bert.mri), true)];
        sync.recurrence = Some(weekly(&[Weekday::Monday, Weekday::Wednesday, Weekday::Friday], None));
        let master = create(&ada, sync).await;
        assert_eq!(master.kind, EventKind::RecurringMaster);
        let chat = master.meeting_chat.clone().unwrap();
        let events = week(&bert).await;
        assert_eq!(events.len(), 3, "{events:?}");
        for event in &events {
            assert_eq!(event.response, EventResponse::NotResponded);
            assert_eq!(
                event.meeting_chat.as_deref(),
                Some(chat.as_str()),
                "one chat for the series"
            );
        }
        answered_one_then_all(&ada, &bert, &master, &events).await;
        cancelled_one_then_all(&ada, &bert, &master, &events).await;
        let items = chat_items(&bert, &chat).await;
        let texts: Vec<&str> = items.iter().map(|item| item.2.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Scheduled a meeting",
                "The meeting \"Sync\" is cancelled: Holiday",
                "The meeting \"Sync\" is cancelled: Done"
            ]
        );
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_event_invites_only_accounts_of_the_server_and_becomes_a_teams_meeting_when_changed_to_one() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let start = MONDAY + 3 * DAY + 8 * HOUR;
        let mut lunch = draft("Lunch", start, 60);
        lunch.attendees = vec![invited("stranger@elsewhere.test", "Stranger", None, true)];
        let refused = ada.engine.write_event(EventWrite::Create(lunch.clone())).await;
        assert!(refused.unwrap_err().to_string().contains("400"));
        assert!(week(&ada).await.is_empty());

        // a meeting in a room, not online: the invited account has it, there is no chat
        lunch.attendees = vec![
            invited("bert@own.test", "Bert", None, true),
            invited("ada@own.test", "Ada", Some(&ada.mri), true),
            invited("BERT@own.test", "Bert again", Some(&bert.mri), false),
        ];
        lunch.location = "Canteen".into();
        let created = create(&ada, lunch.clone()).await;
        assert!(!created.online && created.join_url.is_none() && created.meeting_chat.is_none());
        let whole = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        assert_eq!(roster(&whole).len(), 2, "the organizer and Bert, once");
        let events = week(&bert).await;
        assert_eq!(events[0].location.as_deref(), Some("Canteen"));
        assert_eq!(events[0].end, EventTime::Instant(start + 60 * MINUTE));

        // changed to a Teams meeting, it gets its link and chat
        lunch.online = true;
        ada.engine
            .write_event(EventWrite::Update {
                id: created.id.clone(),
                event: whole.raw,
                draft: lunch,
            })
            .await
            .unwrap();
        let events = week(&bert).await;
        assert!(events[0].online);
        let chat = events[0].meeting_chat.clone().unwrap();
        let link = events[0].join_url.clone().unwrap();
        assert!(MeetingCode::of_link_in(&teams_proto::Cloud::own(ORIGIN).unwrap(), &link).is_none());
        assert!(events[0].meeting_code.is_some());
        assert_eq!(events[0].location.as_deref(), Some("Canteen"));
        let details = bert.engine.meeting(chat).await.unwrap().unwrap();
        assert_eq!(details.title.as_deref(), Some("Lunch"));
        // an answer kept through a change that moves nothing
        respond(&bert, &created.id, RsvpAnswer::Accept).await.unwrap();
        let whole = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        let mut renamed = draft("Lunch!", start, 60);
        renamed.online = true;
        renamed.attendees = vec![invited("bert@own.test", "Bert", None, true)];
        ada.engine
            .write_event(EventWrite::Update {
                id: created.id.clone(),
                event: whole.raw,
                draft: renamed,
            })
            .await
            .unwrap();
        assert_eq!(week(&bert).await[0].response, EventResponse::Accepted);
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn meet_now_makes_a_meeting_with_its_chat_and_links_and_no_event() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cloud = teams_proto::Cloud::own(ORIGIN).unwrap();
        let made = ada.engine.meet_now("Meeting with Ada").await.unwrap();
        assert!(teams_proto::is_meeting_chat_id(&made.thread), "{made:?}");
        assert_eq!(
            teams_proto::join_url_chat_in(&cloud, &made.join).as_deref(),
            Some(made.thread.as_str())
        );
        let organizer = teams_proto::join_url_meeting_in(&cloud, &made.join).unwrap();
        assert_eq!(organizer.organizer_id, ada.id);
        let code = MeetingCode::of_link_in(&cloud, &made.share).unwrap();
        assert!((9..=20).contains(&code.id.len()), "{code:?}");
        let details = ada.engine.meeting(made.thread.clone()).await.unwrap().unwrap();
        assert_eq!(details.title.as_deref(), Some("Meeting with Ada"));
        assert_eq!(details.join_url.as_deref(), Some(made.join.as_str()));
        assert!(week(&ada).await.is_empty());
        assert!(bert.engine.meeting(made.thread.clone()).await.is_err());
        // each one is a meeting of its own
        let other = ada.engine.meet_now("Meeting with Ada").await.unwrap();
        assert_ne!((other.thread, other.share), (made.thread, made.share));
        ada.stop();
        bert.stop();
        server.stop().await;
    }
}
