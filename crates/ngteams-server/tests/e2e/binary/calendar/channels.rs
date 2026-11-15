//! A meeting scheduled in a channel: posted there, its details opened from the post, changed and cancelled only by
//! who scheduled it, on every channel member's calendar with "Send personal invites", and nothing of it for someone
//! outside the team.

use teams_core::EventWrite;
use teams_core::engine::{Event, MessageChange};
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, EventResponse, MeetingChannel, NewTeam, ScheduledMeeting, TeamRole, TeamWrite};

use super::{Trio, User, refused, window};
use crate::calendar::{DAY, HOUR, MINUTE, MONDAY, draft};
use crate::support::backends;

const LAUNCH: i64 = MONDAY + 2 * DAY + 15 * HOUR;

/// A team of a's with b and c on it: its general channel's thread and its group.
async fn team(a: &User, people: &[&User]) -> (String, String) {
    let create = TeamWrite::Create(NewTeam {
        name: "Rocket".into(),
        description: "Rocket's people".into(),
        first_channel: "General".into(),
        public: false,
    });
    let (thread, group) = a.engine().team_write(create, None).await.unwrap().created.unwrap();
    let group = group.unwrap();
    let add = TeamWrite::AddMembers {
        team: thread.clone(),
        group: group.clone(),
        members: people.iter().map(|user| (user.mri.clone(), TeamRole::Member)).collect(),
    };
    let written = a.engine().team_write(add, None).await.unwrap();
    assert!(written.not_added.is_empty(), "{written:?}");
    (thread, group)
}

/// The meetings the channel's messages name, oldest first: each meeting, its message's id and its text.
async fn posts(user: &User, channel: &str) -> Vec<(ScheduledMeeting, String, String)> {
    let client = user.client();
    let page = ChatReads::new(&client).messages(channel, 50).await.unwrap();
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

struct Launch {
    trio: Trio,
    d: User,
    channel: MeetingChannel,
    created: CalendarEvent,
}

impl Launch {
    async fn scheduled(backend: crate::support::Backend) -> Self {
        let mut trio = Trio::start(backend).await;
        let d = trio.client("d@own.test", true).await;
        let (thread, group) = team(&trio.a, &[&trio.b, &trio.c]).await;
        let host = url::Url::parse(&trio.rig.origin)
            .unwrap()
            .host_str()
            .unwrap()
            .to_owned();
        let channel = MeetingChannel {
            channel: thread,
            mailbox: format!("{group}@{host}"),
        };
        let (on_b, on_c) = (trio.b.mark(), trio.c.mark());
        let write = EventWrite::CreateInChannel {
            draft: draft("Countdown", LAUNCH, 45),
            channel: channel.clone(),
            personal_invites: true,
        };
        let created = trio.a.engine().write_event(write).await.unwrap().created.unwrap();
        // the post reaches the running clients of the channel's people
        for (user, mark) in [(&mut trio.b, on_b), (&mut trio.c, on_c)] {
            user.since(mark, "the meeting's post", |event| {
                matches!(event, Event::Message { conversation, change: MessageChange::New, .. }
                    if *conversation == channel.channel)
            })
            .await;
        }
        Self {
            trio,
            d,
            channel,
            created,
        }
    }

    /// Posted in the channel; b opens its details from the post.
    async fn opened_from_its_post(&self) -> String {
        let named = self.created.channel_meeting.clone().unwrap();
        assert!(named.organized_by(self.trio.a.mri.as_str()));
        let listed = posts(&self.trio.b, &self.channel.channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        let (meeting, post, text) = &listed[0];
        assert_eq!(text, "Scheduled a meeting");
        assert_eq!(Some(post), named.post.as_ref());
        assert_eq!((meeting.title.as_str(), meeting.start_ms), ("Countdown", LAUNCH));
        assert_eq!(meeting.end_ms, LAUNCH + 45 * MINUTE);
        let details = self
            .trio
            .b
            .engine()
            .channel_event(self.channel.mailbox.clone(), meeting.ical_uid.clone())
            .await
            .unwrap();
        assert_eq!(details.event.subject, "Countdown");
        assert!(!details.event.is_organizer);
        assert_eq!(details.event.join_url, self.created.join_url);
        post.clone()
    }

    /// "Send personal invites": on b's and c's calendars, not answered yet; a's copy is accepted.
    async fn on_every_members_calendar(&self) {
        for user in [&self.trio.b, &self.trio.c] {
            let events = window(user, MONDAY, 7).await;
            assert_eq!(events.len(), 1, "{}: {events:?}", user.upn);
            assert_eq!(events[0].response, EventResponse::NotResponded);
            assert_eq!(events[0].organizer_name.as_deref(), Some("Rocket"));
            assert_eq!(
                events[0].channel_meeting.as_ref().unwrap().channel,
                self.channel.channel
            );
        }
        let own = window(&self.trio.a, MONDAY, 7).await;
        assert_eq!(own[0].response, EventResponse::Accepted);
    }

    /// d, outside the team: no copy, no details, no post.
    async fn nothing_for_an_outsider(&self) {
        let uid = self.created.channel_meeting.clone().unwrap().ical_uid;
        assert!(window(&self.d, MONDAY, 7).await.is_empty());
        let read = self.d.engine().channel_event(self.channel.mailbox.clone(), uid).await;
        assert!(read.is_err());
        let client = self.d.client();
        assert!(
            ChatReads::new(&client)
                .messages(&self.channel.channel, 10)
                .await
                .is_err()
        );
        let write = EventWrite::CreateInChannel {
            draft: draft("Sneak in", LAUNCH, 30),
            channel: self.channel.clone(),
            personal_invites: false,
        };
        refused(self.d.engine().write_event(write).await, "404");
    }

    /// b and c change and cancel nothing; a's change edits the post in place.
    async fn only_its_scheduler_changes_it(&self, post: &str) {
        let uid = self.created.channel_meeting.clone().unwrap().ical_uid;
        let renamed = draft("Countdown, final", LAUNCH + HOUR, 45);
        for user in [&self.trio.b, &self.trio.c] {
            let theirs = user
                .engine()
                .channel_event(self.channel.mailbox.clone(), uid.clone())
                .await
                .unwrap();
            let change = EventWrite::UpdateInChannel {
                event: theirs.raw,
                draft: renamed.clone(),
                channel: self.channel.clone(),
            };
            refused(user.engine().write_event(change).await, "403");
        }
        let details = self
            .trio
            .a
            .engine()
            .channel_event(self.channel.mailbox.clone(), uid)
            .await
            .unwrap();
        let change = EventWrite::UpdateInChannel {
            event: details.raw,
            draft: renamed,
            channel: self.channel.clone(),
        };
        self.trio.a.engine().write_event(change).await.unwrap();
        let listed = posts(&self.trio.c, &self.channel.channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(
            (listed[0].0.title.as_str(), listed[0].0.start_ms),
            ("Countdown, final", LAUNCH + HOUR)
        );
        assert_eq!(listed[0].1, post);
        let theirs = window(&self.trio.b, MONDAY, 7).await;
        assert_eq!(theirs[0].subject, "Countdown, final");
    }

    async fn cancelled_by_its_scheduler(&self) {
        let own = window(&self.trio.a, MONDAY, 7).await;
        let (meeting, of_post) = ScheduledMeeting::of_event(&own[0]).unwrap();
        let cancel = |note: &str| EventWrite::CancelInChannel {
            meeting: Box::new(meeting.clone()),
            channel: self.channel.clone(),
            post: of_post.clone(),
            message: Some(note.into()),
        };
        refused(self.trio.b.engine().write_event(cancel("No")).await, "403");
        refused(self.d.engine().write_event(cancel("No")).await, "404");
        self.trio.a.engine().write_event(cancel("Scrubbed")).await.unwrap();
        let listed = posts(&self.trio.b, &self.channel.channel).await;
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed[1].0.cancelled);
        assert_eq!(listed[1].2, "The meeting \"Countdown, final\" is cancelled: Scrubbed");
        assert!(window(&self.trio.a, MONDAY, 7).await.is_empty());
        for user in [&self.trio.b, &self.trio.c] {
            let events = window(user, MONDAY, 7).await;
            assert!(events.len() == 1 && events[0].cancelled, "{}: {events:?}", user.upn);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_meeting_is_posted_opened_from_its_post_on_every_members_calendar_and_changed_only_by_its_scheduler()
{
    for backend in backends() {
        let launch = Launch::scheduled(backend).await;
        let post = launch.opened_from_its_post().await;
        launch.on_every_members_calendar().await;
        launch.nothing_for_an_outsider().await;
        launch.only_its_scheduler_changes_it(&post).await;
        launch.cancelled_by_its_scheduler().await;
        let Launch { trio, d, .. } = launch;
        drop(d);
        trio.stop().await;
    }
}
