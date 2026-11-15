use teams_core::{EngineError, EventWrite};
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, EventKind, EventResponse, FullEvent, MeetingChannel, ScheduledMeeting};

use super::meetings::invited;
use super::{DAY, HOUR, MINUTE, MONDAY, Seat, draft, server, week};
use crate::support::backends;
use crate::teams::ORIGIN;

fn mailbox(group: &str) -> String {
    format!("{group}@own.example.test")
}

pub(crate) fn place(channel: &str, group: &str) -> MeetingChannel {
    MeetingChannel {
        channel: channel.into(),
        mailbox: mailbox(group),
    }
}

/// The meetings a channel's messages name, oldest first, each with its message's id and text.
async fn posts(seat: &Seat, channel: &str) -> Vec<(ScheduledMeeting, String, String)> {
    let page = ChatReads::new(&*seat.client).messages(channel, 50).await.unwrap();
    let mut posts: Vec<_> = page
        .messages
        .iter()
        .filter_map(|message| {
            let properties = serde_json::to_string(&message.properties).unwrap();
            let meeting = ScheduledMeeting::of_properties(&properties)?;
            Some((meeting, message.id.clone(), message.content.clone()))
        })
        .collect();
    posts.reverse();
    posts
}

pub(crate) async fn schedule(
    seat: &Seat,
    draft: teams_proto::EventDraft,
    channel: MeetingChannel,
    all: bool,
) -> CalendarEvent {
    let write = EventWrite::CreateInChannel {
        draft,
        channel,
        personal_invites: all,
    };
    seat.engine.write_event(write).await.unwrap().created.unwrap()
}

pub(crate) fn refused(answer: Result<teams_core::EventWritten, EngineError>, status: &str) {
    let error = answer.unwrap_err().to_string();
    assert!(error.contains(status), "{error}");
}

const COUNTDOWN: i64 = MONDAY + 2 * DAY + 15 * HOUR;

/// A team of Ada's with Bert on it and a channel, a meeting Ada scheduled there, and Cora outside the team.
struct Launch {
    ada: Seat,
    bert: Seat,
    cora: Seat,
    group: String,
    channel: String,
    created: CalendarEvent,
    named: teams_proto::ChannelEventRef,
}

impl Launch {
    async fn scheduled(server: &crate::support::TestServer) -> Self {
        let ada = Seat::new(server, "ada@own.test", "Ada").await;
        let bert = Seat::new(server, "bert@own.test", "Bert").await;
        let cora = Seat::new(server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let created = schedule(&ada, draft("Countdown", COUNTDOWN, 45), place(&channel, &group), false).await;
        Self {
            named: created.channel_meeting.clone().unwrap(),
            ada,
            bert,
            cora,
            group,
            channel,
            created,
        }
    }

    fn place(&self) -> MeetingChannel {
        place(&self.channel, &self.group)
    }

    /// The service posts it to the channel, for everyone in it; its link names the meeting's own post.
    async fn is_posted(&self) -> String {
        assert_eq!(self.named.channel, self.channel);
        assert!(self.named.organized_by(&self.ada.mri));
        assert!(self.created.online);
        let link = self.created.join_url.clone().unwrap();
        assert!(link.starts_with(&format!("{ORIGIN}/l/meetup-join/19%3A")), "{link}");
        let listed = posts(&self.bert, &self.channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        let (posted, post, text) = &listed[0];
        assert_eq!(text, "Scheduled a meeting");
        assert_eq!(Some(post), self.named.post.as_ref());
        assert_eq!(posted.title, "Countdown");
        assert_eq!((posted.start_ms, posted.end_ms), (COUNTDOWN, COUNTDOWN + 45 * MINUTE));
        assert_eq!(posted.ical_uid, self.named.ical_uid);
        assert!(posted.organized_by(&self.ada.id) && !posted.cancelled);
        assert_eq!(posted.join_url.as_deref(), Some(link.as_str()));
        assert!(link.contains(&format!("/{post}?")), "{link}");
        post.clone()
    }

    /// Who scheduled it has it as an accepted copy, the team its organizer; nobody else's calendar has it. Its
    /// details come from the team's calendar: for the team's people, the organizer's for who scheduled it.
    async fn is_read(&self) -> (FullEvent, FullEvent) {
        let own = week(&self.ada).await;
        assert_eq!(own.len(), 1, "{own:?}");
        assert_eq!(own[0].channel_meeting, self.created.channel_meeting);
        assert!(!own[0].is_organizer);
        assert_eq!(own[0].response, EventResponse::Accepted);
        assert_eq!(own[0].organizer_name.as_deref(), Some("Rocket"));
        assert_eq!(own[0].organizer_address, Some(mailbox(&self.group)));
        assert!(week(&self.bert).await.is_empty());
        self.details().await
    }

    /// Its details, from the team's calendar: for the team's people, the organizer's for who scheduled it.
    async fn details(&self) -> (FullEvent, FullEvent) {
        let read = |seat: &Seat| {
            seat.engine
                .channel_event(mailbox(&self.group), self.named.ical_uid.clone())
        };
        let details = read(&self.ada).await.unwrap();
        assert!(details.event.is_organizer);
        assert_eq!(details.event.subject, "Countdown");
        assert_eq!(details.raw["myResponseType"], "Organizer");
        assert_eq!(details.raw["organizerName"], "Rocket");
        let theirs = read(&self.bert).await.unwrap();
        assert!(!theirs.event.is_organizer);
        assert!(read(&self.cora).await.is_err());
        (details, theirs)
    }

    /// Its change edits the post in place; only who scheduled it may.
    async fn is_changed(&self, details: &FullEvent, theirs: &FullEvent, post: &str) {
        let renamed = draft("Countdown, final", COUNTDOWN + HOUR, 45);
        let change = |raw: &FullEvent| EventWrite::UpdateInChannel {
            event: raw.raw.clone(),
            draft: renamed.clone(),
            channel: self.place(),
        };
        refused(self.bert.engine.write_event(change(theirs)).await, "403");
        self.ada.engine.write_event(change(details)).await.unwrap();
        let listed = posts(&self.bert, &self.channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].0.title, "Countdown, final");
        assert_eq!(listed[0].0.start_ms, COUNTDOWN + HOUR);
        assert_eq!(listed[0].1, post);
        assert_eq!(week(&self.ada).await[0].subject, "Countdown, final");
    }

    /// Its cancellation is a reply under the post; who cancelled it no longer has it.
    async fn is_cancelled(&self, post: &str) {
        let own = week(&self.ada).await;
        let (meeting, of_post) = ScheduledMeeting::of_event(&own[0]).unwrap();
        let cancel = |message: &str| EventWrite::CancelInChannel {
            meeting: Box::new(meeting.clone()),
            channel: self.place(),
            post: of_post.clone(),
            message: Some(message.into()),
        };
        refused(self.bert.engine.write_event(cancel("No")).await, "403");
        refused(self.cora.engine.write_event(cancel("No")).await, "404");
        self.ada.engine.write_event(cancel("Scrubbed")).await.unwrap();
        self.cancellation_is_posted(post).await;
        assert!(week(&self.ada).await.is_empty());
        refused(self.ada.engine.write_event(cancel("Again")).await, "404");
    }

    async fn cancellation_is_posted(&self, post: &str) {
        let listed = posts(&self.bert, &self.channel).await;
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed[1].0.cancelled);
        assert_eq!(listed[1].0.ical_uid, self.named.ical_uid);
        assert_eq!(listed[1].2, "The meeting \"Countdown, final\" is cancelled: Scrubbed");
        let synced = self.bert.engine.sync_conversation(self.channel.as_str()).await.unwrap();
        assert_eq!(synced.inserted, 2);
        let roots = self.bert.engine.reply_roots(self.channel.as_str()).await.unwrap();
        let under = roots
            .iter()
            .any(|(message, root)| *message == listed[1].1 && root == post);
        assert!(under, "the cancellation is under the meeting's post: {roots:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_scheduled_in_a_channel_is_posted_there_and_changed_and_cancelled_by_who_scheduled_it() {
    for backend in backends() {
        let server = server(backend).await;
        let launch = Launch::scheduled(&server).await;
        let post = launch.is_posted().await;
        let (details, theirs) = launch.is_read().await;
        launch.is_changed(&details, &theirs, &post).await;
        launch.is_cancelled(&post).await;
        for seat in [launch.ada, launch.bert, launch.cora] {
            seat.stop();
        }
        server.stop().await;
    }
}

/// An invited account answers its copy and takes it off as any invitation; but the meeting is the team's: no copy
/// changes or cancels it from a calendar.
async fn copies_are_copies(ada: &Seat, bert: &Seat, cora: &Seat, created: &CalendarEvent) {
    let own = week(bert).await.remove(0);
    bert.engine
        .write_event(EventWrite::Respond {
            id: own.id.clone(),
            answer: teams_proto::RsvpAnswer::Accept,
            send: true,
            comment: None,
        })
        .await
        .unwrap();
    assert_eq!(week(bert).await[0].response, EventResponse::Accepted);
    let raw = ada.engine.calendar_event(own.id.clone()).await.unwrap().raw;
    let change = EventWrite::Update {
        id: own.id.clone(),
        event: raw,
        draft: draft("Mine now", MONDAY + 9 * HOUR, 30),
    };
    refused(ada.engine.write_event(change).await, "403");
    let cancel = EventWrite::Cancel {
        id: own.id.clone(),
        chat: None,
        message: None,
    };
    refused(ada.engine.write_event(cancel).await, "403");
    bert.engine
        .write_event(EventWrite::Delete { id: created.id.clone() })
        .await
        .unwrap();
    assert!(week(bert).await.is_empty());
    assert_eq!(week(cora).await.len(), 1);
}

/// A channel its people alone are in takes a meeting from them alone; another team's mailbox names nothing; an
/// archived team takes none.
async fn only_where_allowed(ada: &Seat, bert: &Seat, dan: &Seat, team: &str, group: &str) {
    let start = MONDAY + 9 * HOUR;
    let private = teams_proto::NewChannelKind::Private;
    let made = ada
        .engine
        .create_channel(teams_core::ChannelDraft {
            channel: crate::teams::new_channel(team, group, "Flight crew", private),
            show_by_default: false,
        })
        .await
        .unwrap();
    let private_thread = made.thread.unwrap();
    let outside = EventWrite::CreateInChannel {
        draft: draft("Sneak in", start, 30),
        channel: place(&private_thread, group),
        personal_invites: false,
    };
    refused(bert.engine.write_event(outside.clone()).await, "404");
    refused(dan.engine.write_event(outside).await, "404");
    // and its meeting is read from the team's calendar by its people alone
    let crew = schedule(ada, draft("Crew only", start, 30), place(&private_thread, group), false).await;
    let uid = crew.channel_meeting.unwrap().ical_uid;
    assert!(ada.engine.channel_event(mailbox(group), uid.clone()).await.is_ok());
    assert!(bert.engine.channel_event(mailbox(group), uid).await.is_err());
    let wrong_team = EventWrite::CreateInChannel {
        draft: draft("Elsewhere", start, 30),
        channel: MeetingChannel {
            channel: team.to_owned(),
            mailbox: mailbox("00000000-0000-4000-8000-000000000000"),
        },
        personal_invites: false,
    };
    refused(ada.engine.write_event(wrong_team).await, "404");
    let archive = teams_proto::TeamWrite::Archive {
        group: group.to_owned(),
        site_read_only: false,
    };
    ada.engine.team_write(archive, None).await.unwrap();
    let late = EventWrite::CreateInChannel {
        draft: draft("Too late", start, 30),
        channel: place(team, group),
        personal_invites: false,
    };
    refused(ada.engine.write_event(late).await, "403");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_channel_meeting_with_personal_invites_is_on_the_calendar_of_everyone_in_the_channel() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert, &cora]).await;

        // in the general channel, everyone invited, and someone from outside the team by name
        let mut all_hands = draft("All hands", MONDAY + 9 * HOUR, 30);
        all_hands.attendees = vec![invited("dan@own.test", "Dan", Some(&dan.mri), false)];
        all_hands.recurrence = Some(super::events::weekly(&[teams_proto::Weekday::Monday], None));
        let created = schedule(&ada, all_hands, place(&team, &group), true).await;
        assert_eq!(created.kind, EventKind::RecurringMaster);
        for seat in [&bert, &cora, &dan] {
            let events = seat.engine.calendar(MONDAY, MONDAY + 14 * DAY).await.unwrap();
            assert_eq!(events.len(), 2, "{events:?}");
            assert_eq!(events[0].response, EventResponse::NotResponded);
            assert_eq!(events[0].kind, EventKind::Occurrence);
            assert_eq!(events[0].organizer_name.as_deref(), Some("Rocket"));
            assert_eq!(events[0].channel_meeting.as_ref().unwrap().channel, team);
        }
        assert_eq!(week(&ada).await[0].response, EventResponse::Accepted);
        copies_are_copies(&ada, &bert, &cora, &created).await;
        only_where_allowed(&ada, &bert, &dan, &team, &group).await;
        for seat in [ada, bert, cora, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}
