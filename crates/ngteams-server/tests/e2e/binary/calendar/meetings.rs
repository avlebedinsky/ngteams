//! A Teams meeting: its join link, its short link with the meeting's id and passcode, and a chat of everyone it
//! invites that shows its invitation and its cancellation; and Meet now, which makes the links and a chat and no
//! event.

use teams_core::engine::{Event, MessageChange};
use teams_proto::chat::ChatReads;
use teams_proto::{Cloud, MeetingCode};

use super::{Trio, User, cancel, create, invite, meeting_posts, window};
use crate::calendar::{DAY, HOUR, MONDAY, draft};
use crate::support::backends;

const PLANNING: i64 = MONDAY + 4 * DAY + 11 * HOUR;

fn in_chat(event: &Event, chat: &str, change: MessageChange) -> bool {
    matches!(event, Event::Message { conversation, change: of, .. } if conversation == chat && *of == change)
}

/// The meeting's chat lists `user` among its people and names the meeting.
async fn in_its_chat(user: &User, chat: &str, link: &str) {
    let client = user.client();
    let listed = ChatReads::new(&client).conversations().await.unwrap();
    assert!(
        listed.conversations.iter().any(|entry| entry.id == chat),
        "{}",
        user.upn
    );
    let details = user.engine().meeting(chat.to_owned()).await.unwrap().unwrap();
    assert_eq!(details.title.as_deref(), Some("Planning"));
    assert_eq!(details.join_url.as_deref(), Some(link));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_teams_meeting_has_its_links_and_a_chat_of_everyone_invited_with_its_invitation_and_cancellation() {
    for backend in backends() {
        let mut trio = Trio::start(backend).await;
        let cloud = Cloud::own(&trio.rig.origin).unwrap();
        let (on_b, on_c) = (trio.b.mark(), trio.c.mark());
        let mut planning = draft("Planning", PLANNING, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b), invite(&trio.c)];
        let created = create(&trio.a, planning).await;

        // the join link names the meeting's chat and its organizer
        let link = created.join_url.clone().unwrap();
        let chat = created.meeting_chat.clone().unwrap();
        assert!(teams_proto::is_meeting_chat_id(&chat), "{chat}");
        assert!(
            link.starts_with(&format!("{}/l/meetup-join/19%3Ameeting_", trio.rig.origin)),
            "{link}"
        );
        assert_eq!(
            teams_proto::join_url_chat_in(&cloud, &link).as_deref(),
            Some(chat.as_str())
        );
        let organizer = teams_proto::join_url_meeting_in(&cloud, &link).unwrap();
        assert_eq!(organizer.organizer_id, trio.a.oid());
        assert_eq!(created.meeting.as_ref().unwrap().organizer_id, trio.a.oid());
        // the short link carries the meeting's id and passcode
        let code = created.meeting_code.clone().unwrap();
        assert!(
            code.id.len() >= 9 && code.id.chars().all(|c| c.is_ascii_digit()),
            "{code:?}"
        );
        assert!(!code.passcode.is_empty(), "{code:?}");
        let whole = trio.a.engine().calendar_event(created.id.clone()).await.unwrap();
        let short = whole.raw["shortOnlineMeetingJoinUrl"].as_str().unwrap().to_owned();
        assert!(short.starts_with(&format!("{}/meet/", trio.rig.origin)), "{short}");
        assert_eq!(MeetingCode::of_link_in(&cloud, &short), Some(code.clone()));
        // b and c, at the other node, have the same links on their copies
        for user in [&trio.b, &trio.c] {
            let theirs = window(user, MONDAY, 7).await;
            assert_eq!(theirs[0].join_url.as_deref(), Some(link.as_str()), "{}", user.upn);
            assert_eq!(theirs[0].meeting_code.as_ref(), Some(&code), "{}", user.upn);
        }

        // the chat has everyone invited, and its invitation reached b's and c's running clients
        for user in [&trio.a, &trio.b, &trio.c] {
            in_its_chat(user, &chat, &link).await;
        }
        for (user, mark) in [(&mut trio.b, on_b), (&mut trio.c, on_c)] {
            user.since(mark, "the invitation in the meeting's chat", |event| {
                in_chat(event, &chat, MessageChange::New)
            })
            .await;
        }
        let invitation = ("Planning".to_owned(), false, "Scheduled a meeting".to_owned());
        assert_eq!(meeting_posts(&trio.c, &chat).await, std::slice::from_ref(&invitation));

        // its cancellation, with its note, is the chat's next message, live too
        let (on_b, on_c) = (trio.b.mark(), trio.c.mark());
        cancel(&trio.a, &created, Some("Room flooded")).await.unwrap();
        for (user, mark) in [(&mut trio.b, on_b), (&mut trio.c, on_c)] {
            user.since(mark, "the cancellation in the meeting's chat", |event| {
                in_chat(event, &chat, MessageChange::New)
            })
            .await;
        }
        let cancelled = (
            "Planning".to_owned(),
            true,
            "The meeting \"Planning\" is cancelled: Room flooded".to_owned(),
        );
        for user in [&trio.a, &trio.b, &trio.c] {
            assert_eq!(
                meeting_posts(user, &chat).await,
                [invitation.clone(), cancelled.clone()],
                "{}",
                user.upn
            );
        }
        trio.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meet_now_makes_a_meeting_with_its_links_and_chat_and_no_event() {
    for backend in backends() {
        let trio = Trio::start(backend).await;
        let cloud = Cloud::own(&trio.rig.origin).unwrap();
        let now = i64::try_from(std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap().as_millis()).unwrap();
        // b, at the other node, starts one
        let made = trio.b.engine().meet_now("Meeting with b").await.unwrap();
        assert!(teams_proto::is_meeting_chat_id(&made.thread), "{made:?}");
        assert_eq!(
            teams_proto::join_url_chat_in(&cloud, &made.join).as_deref(),
            Some(made.thread.as_str())
        );
        assert_eq!(
            teams_proto::join_url_meeting_in(&cloud, &made.join)
                .unwrap()
                .organizer_id,
            trio.b.oid()
        );
        let code = MeetingCode::of_link_in(&cloud, &made.share).unwrap();
        assert!(
            (9..=20).contains(&code.id.len()) && !code.passcode.is_empty(),
            "{code:?}"
        );
        let details = trio.b.engine().meeting(made.thread.clone()).await.unwrap().unwrap();
        assert_eq!(details.title.as_deref(), Some("Meeting with b"));
        assert_eq!(details.join_url.as_deref(), Some(made.join.as_str()));
        let client = trio.b.client();
        let listed = ChatReads::new(&client).conversations().await.unwrap();
        assert!(listed.conversations.iter().any(|entry| entry.id == made.thread));
        // no event on anyone's calendar; a meeting of b's alone
        for user in [&trio.a, &trio.b] {
            assert!(window(user, now - DAY, 2).await.is_empty(), "{}", user.upn);
        }
        assert!(trio.a.engine().meeting(made.thread.clone()).await.is_err());
        let again = trio.b.engine().meet_now("Meeting with b").await.unwrap();
        assert_ne!((&again.thread, &again.share), (&made.thread, &made.share));
        trio.stop().await;
    }
}
