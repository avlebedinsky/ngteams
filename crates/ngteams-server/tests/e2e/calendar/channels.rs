use teams_core::{EngineError, EventWrite};
use teams_proto::chat::ChatReads;
use teams_proto::{CalendarEvent, EventKind, EventResponse, MeetingChannel, ScheduledMeeting};

use super::meetings::invited;
use super::{DAY, HOUR, MINUTE, MONDAY, Seat, draft, server, week};
use crate::support::backends;
use crate::teams::ORIGIN;

fn mailbox(group: &str) -> String {
    format!("{group}@own.example.test")
}

fn place(channel: &str, group: &str) -> MeetingChannel {
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

async fn schedule(seat: &Seat, draft: teams_proto::EventDraft, channel: MeetingChannel, all: bool) -> CalendarEvent {
    let write = EventWrite::CreateInChannel {
        draft,
        channel,
        personal_invites: all,
    };
    seat.engine.write_event(write).await.unwrap().created.unwrap()
}

fn refused(answer: Result<teams_core::EventWritten, EngineError>, status: &str) {
    let error = answer.unwrap_err().to_string();
    assert!(error.contains(status), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_meeting_scheduled_in_a_channel_is_posted_there_and_changed_and_cancelled_by_who_scheduled_it() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let cora = Seat::new(&server, "cora@own.test", "Cora").await;
        let (team, group) = ada.make_team("Rocket", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let start = MONDAY + 2 * DAY + 15 * HOUR;

        let created = schedule(&ada, draft("Countdown", start, 45), place(&channel, &group), false).await;
        let named = created.channel_meeting.clone().unwrap();
        assert_eq!(named.channel, channel);
        assert!(named.organized_by(&ada.mri));
        assert!(created.online);
        let link = created.join_url.clone().unwrap();
        assert!(link.starts_with(&format!("{ORIGIN}/l/meetup-join/19%3A")), "{link}");

        // the service posts it to the channel, for everyone in it
        let listed = posts(&bert, &channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        let (posted, post, text) = &listed[0];
        assert_eq!(text, "Scheduled a meeting");
        assert_eq!(Some(post), named.post.as_ref());
        assert_eq!(posted.title, "Countdown");
        assert_eq!((posted.start_ms, posted.end_ms), (start, start + 45 * MINUTE));
        assert_eq!(posted.ical_uid, named.ical_uid);
        assert!(posted.organized_by(&ada.id) && !posted.cancelled);
        assert_eq!(posted.join_url.as_deref(), Some(link.as_str()));
        assert!(
            link.contains(&format!("/{post}?")),
            "the link names the meeting's own post: {link}"
        );

        // who scheduled it has it as an accepted copy, the team its organizer; nobody else's calendar has it
        let own = week(&ada).await;
        assert_eq!(own.len(), 1, "{own:?}");
        assert_eq!(own[0].channel_meeting, created.channel_meeting);
        assert!(!own[0].is_organizer);
        assert_eq!(own[0].response, EventResponse::Accepted);
        assert_eq!(own[0].organizer_name.as_deref(), Some("Rocket"));
        assert_eq!(own[0].organizer_address, Some(mailbox(&group)));
        assert!(week(&bert).await.is_empty());

        // its details, from the team's calendar: for the team's people, the organizer's for who scheduled it
        let details = ada
            .engine
            .channel_event(mailbox(&group), named.ical_uid.clone())
            .await
            .unwrap();
        assert!(details.event.is_organizer);
        assert_eq!(details.event.subject, "Countdown");
        assert_eq!(details.raw["myResponseType"], "Organizer");
        assert_eq!(details.raw["organizerName"], "Rocket");
        let theirs = bert
            .engine
            .channel_event(mailbox(&group), named.ical_uid.clone())
            .await
            .unwrap();
        assert!(!theirs.event.is_organizer);
        assert!(
            cora.engine
                .channel_event(mailbox(&group), named.ical_uid.clone())
                .await
                .is_err()
        );

        // its change edits the post in place; only who scheduled it may
        let renamed = draft("Countdown, final", start + HOUR, 45);
        let change = |raw: &teams_proto::FullEvent| EventWrite::UpdateInChannel {
            event: raw.raw.clone(),
            draft: renamed.clone(),
            channel: place(&channel, &group),
        };
        refused(bert.engine.write_event(change(&theirs)).await, "403");
        ada.engine.write_event(change(&details)).await.unwrap();
        let listed = posts(&bert, &channel).await;
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].0.title, "Countdown, final");
        assert_eq!(listed[0].0.start_ms, start + HOUR);
        assert_eq!(&listed[0].1, post);
        let own = week(&ada).await;
        assert_eq!(own[0].subject, "Countdown, final");

        // its cancellation is a reply under the post, and the copies are cancelled
        let (meeting, of_post) = ScheduledMeeting::of_event(&own[0]).unwrap();
        let cancel = |message: &str| EventWrite::CancelInChannel {
            meeting: Box::new(meeting.clone()),
            channel: place(&channel, &group),
            post: of_post.clone(),
            message: Some(message.into()),
        };
        refused(bert.engine.write_event(cancel("No")).await, "403");
        refused(cora.engine.write_event(cancel("No")).await, "404");
        ada.engine.write_event(cancel("Scrubbed")).await.unwrap();
        let listed = posts(&bert, &channel).await;
        assert_eq!(listed.len(), 2, "{listed:?}");
        assert!(listed[1].0.cancelled);
        assert_eq!(listed[1].0.ical_uid, named.ical_uid);
        assert_eq!(listed[1].2, "The meeting \"Countdown, final\" is cancelled: Scrubbed");
        let roots = bert.engine.sync_conversation(channel.as_str()).await.unwrap();
        assert_eq!(roots.inserted, 2);
        let roots = bert.engine.reply_roots(channel.as_str()).await.unwrap();
        assert!(
            roots
                .iter()
                .any(|(message, root)| *message == listed[1].1 && root == post),
            "the cancellation is under the meeting's post: {roots:?}"
        );
        assert!(week(&ada).await.is_empty(), "who cancelled it no longer has it");
        refused(ada.engine.write_event(cancel("Again")).await, "404");
        for seat in [ada, bert, cora] {
            seat.stop();
        }
        server.stop().await;
    }
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
        let start = MONDAY + 9 * HOUR;

        // in the general channel, everyone invited, and someone from outside the team by name
        let mut all_hands = draft("All hands", start, 30);
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
        // an invited account answers its copy and takes it off as any invitation
        let own = week(&bert).await.remove(0);
        bert.engine
            .write_event(EventWrite::Respond {
                id: own.id.clone(),
                answer: teams_proto::RsvpAnswer::Accept,
                send: true,
                comment: None,
            })
            .await
            .unwrap();
        assert_eq!(week(&bert).await[0].response, EventResponse::Accepted);
        // but it is the team's: no copy changes or cancels it from a calendar
        let raw = ada.engine.calendar_event(own.id.clone()).await.unwrap().raw;
        let change = EventWrite::Update {
            id: own.id.clone(),
            event: raw,
            draft: draft("Mine now", start, 30),
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
        assert!(week(&bert).await.is_empty());
        assert_eq!(week(&cora).await.len(), 1);

        // a channel its people alone are in takes a meeting from them alone; an archived one takes none
        let private = ada
            .engine
            .create_channel(teams_core::ChannelDraft {
                channel: crate::teams::new_channel(&team, &group, "Flight crew", teams_proto::NewChannelKind::Private),
                show_by_default: false,
            })
            .await
            .unwrap()
            .thread
            .unwrap();
        let outside = EventWrite::CreateInChannel {
            draft: draft("Sneak in", start, 30),
            channel: place(&private, &group),
            personal_invites: false,
        };
        refused(bert.engine.write_event(outside.clone()).await, "404");
        refused(dan.engine.write_event(outside).await, "404");
        let wrong_team = EventWrite::CreateInChannel {
            draft: draft("Elsewhere", start, 30),
            channel: MeetingChannel {
                channel: team.clone(),
                mailbox: mailbox("00000000-0000-4000-8000-000000000000"),
            },
            personal_invites: false,
        };
        refused(ada.engine.write_event(wrong_team).await, "404");
        ada.engine
            .team_write(
                teams_proto::TeamWrite::Archive {
                    group: group.clone(),
                    site_read_only: false,
                },
                None,
            )
            .await
            .unwrap();
        let late = EventWrite::CreateInChannel {
            draft: draft("Too late", start, 30),
            channel: place(&team, &group),
            personal_invites: false,
        };
        refused(ada.engine.write_event(late).await, "403");
        for seat in [ada, bert, cora, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}
