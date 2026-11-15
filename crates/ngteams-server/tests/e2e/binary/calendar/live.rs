//! A meeting live on the push channel: b's running client gets its invitation, change and cancellation, each with
//! its activity item, which b marks read and b's second client shows the same; a muted meeting chat brings no item;
//! and c, whose client was closed meanwhile, finds the event, the chat's messages and the feed's items when it
//! starts.

use std::time::Duration;

use teams_core::activity::{ActivityFeed, ActivityKind, FEED, MeetingChange};
use teams_core::engine::{Action, Event, MessageChange};
use teams_proto::{ChannelOwnProperty, OwnTeamView};

use super::{Trio, User, cancel, change, create, invite, window};
use crate::calendar::{HOUR, MINUTE, MONDAY, draft};
use crate::support::backends;

const PLANNING: i64 = MONDAY + 14 * HOUR;

fn told(event: &Event, conversation: &str) -> bool {
    matches!(event, Event::Message { conversation: of, change: MessageChange::New, .. } if of == conversation)
}

fn kinds(feed: &ActivityFeed) -> Vec<ActivityKind> {
    feed.items.iter().map(|item| item.kind.clone()).collect()
}

/// The chat's messages the client's store holds, once one holds `marker`: their texts, oldest first.
async fn stored_once(user: &mut User, chat: &str, marker: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let page = user.engine().messages(chat, None, 100).await.unwrap();
        let mut texts: Vec<String> = page.messages.into_iter().map(|message| message.content).collect();
        if texts.iter().any(|text| text.contains(marker)) {
            texts.reverse();
            return texts;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{}: {chat} never held {marker:?}: {texts:?}",
            user.upn
        );
        user.settle().await;
    }
}

/// The invitation reaches b's client live, as its chat's message and a feed item b's other client shows too;
/// b's read mark follows to its other client.
async fn invitation_live(trio: &mut Trio, b2: &mut User, chat: &str, event: &str) {
    let b = &mut trio.b;
    b.since(0, "the invitation's feed item", |seen| told(seen, FEED)).await;
    b.since(0, "the invitation in the chat", |seen| told(seen, chat)).await;
    let feed = b.feed("the invitation", |feed| feed.items.len() == 1).await;
    let item = &feed.items[0];
    assert_eq!(item.kind, ActivityKind::Meeting(MeetingChange::Invited));
    assert_eq!(item.title(), "QA Operator invited you");
    assert_eq!(item.actor.as_deref(), Some(trio.a.mri.as_str()));
    assert_eq!(item.preview, "Planning");
    assert_eq!(item.meeting_event.as_deref(), Some(event));
    assert_eq!(item.meeting_time, Some((PLANNING, PLANNING + 30 * MINUTE)));
    assert!(!feed.is_read(item));
    let id = item.id.clone();
    read_follows(b, b2, &id).await;
}

/// b's other client shows the item unread; b marks it read and the other client follows.
async fn read_follows(b: &mut User, b2: &mut User, id: &str) {
    let other = b2.feed("the invitation", |feed| feed.items.len() == 1).await;
    assert_eq!(other.items[0].id, id);
    assert!(!other.is_read(&other.items[0]));
    b.write(Action::ActivityRead {
        item: id.to_owned(),
        read: true,
    })
    .await;
    let read = b2
        .feed("the invitation read", |feed| {
            feed.item(id).is_some_and(|item| feed.is_read(item))
        })
        .await;
    assert_eq!(read.unseen(), 0);
}

/// The change reaches both of b's clients as a new item; b's calendar has the new time.
async fn change_live(trio: &mut Trio, b2: &mut User, id: &str, mut planning: teams_proto::EventDraft) {
    planning.start.ms += HOUR;
    planning.end.ms += HOUR;
    change(&trio.a, id, planning).await.unwrap();
    for user in [&mut trio.b, b2] {
        let feed = user.feed("the change", |feed| feed.items.len() == 2).await;
        assert_eq!(feed.items[0].kind, ActivityKind::Meeting(MeetingChange::Updated));
        assert_eq!(
            feed.items[0].meeting_time,
            Some((PLANNING + HOUR, PLANNING + HOUR + 30 * MINUTE))
        );
        assert!(!feed.is_read(&feed.items[0]) && feed.is_read(&feed.items[1]));
    }
    let events = window(&trio.b, MONDAY, 7).await;
    assert_eq!(super::shown(&events), [("Planning".to_owned(), PLANNING + HOUR)]);
}

/// b mutes the meeting's chat: the cancellation reaches the chat, and brings b no item.
async fn muted_cancellation(trio: &mut Trio, chat: &str) {
    let mute = OwnTeamView::ChannelProperty {
        channel: chat.into(),
        property: ChannelOwnProperty::Alerts,
        value: "false".into(),
    };
    trio.b.engine().own_team_view(vec![mute], None).await.unwrap();
    trio.b.settle().await;
    let mark = trio.b.mark();
    let current = window(&trio.a, MONDAY, 7).await.remove(0);
    cancel(&trio.a, &current, Some("Postponed")).await.unwrap();
    trio.b
        .since(mark, "the cancellation in the chat", |seen| told(seen, chat))
        .await;
    trio.b.settle().await;
    assert_eq!(trio.b.count(mark, |seen| told(seen, FEED)), 0, "muted: no item");
    assert_eq!(trio.b.engine().activity().await.unwrap().items.len(), 2);
    let events = window(&trio.b, MONDAY, 7).await;
    assert!(events[0].cancelled, "{events:?}");
}

/// c's client, closed meanwhile, starts: the cancelled event, the chat's two messages and three items.
async fn offline_account_catches_up(c: &mut User, chat: &str, event: &str) {
    let mark = c.mark();
    c.start();
    c.online(mark).await;
    c.caught_up(mark).await;
    let feed = c.feed("the three items", |feed| feed.items.len() == 3).await;
    let changes = [MeetingChange::Canceled, MeetingChange::Updated, MeetingChange::Invited];
    assert_eq!(kinds(&feed), changes.map(ActivityKind::Meeting).to_vec());
    assert!(
        feed.items
            .iter()
            .all(|item| item.meeting_event.as_deref() == Some(event))
    );
    assert_eq!(feed.unseen(), 3);
    let texts = stored_once(c, chat, "Postponed").await;
    let meeting: Vec<&String> = texts
        .iter()
        .filter(|text| text.contains("Scheduled a meeting") || text.contains("is cancelled"))
        .collect();
    assert_eq!(meeting.len(), 2, "{texts:?}");
    assert_eq!(meeting[1], "The meeting \"Planning\" is cancelled: Postponed");
    let events = window(c, MONDAY, 7).await;
    assert!(events.len() == 1 && events[0].cancelled, "{events:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_meeting_reaches_bs_clients_live_with_feed_items_a_muted_chat_brings_none_and_c_finds_all_after() {
    for backend in backends() {
        let mut trio = Trio::start(backend).await;
        trio.c.close();
        let mut b2 = trio.client("b@own.test", true).await;
        let mut planning = draft("Planning", PLANNING, 30);
        planning.online = true;
        planning.attendees = vec![invite(&trio.b), invite(&trio.c)];
        let created = create(&trio.a, planning.clone()).await;
        let chat = created.meeting_chat.clone().unwrap();

        invitation_live(&mut trio, &mut b2, &chat, &created.id).await;
        change_live(&mut trio, &mut b2, &created.id, planning).await;
        muted_cancellation(&mut trio, &chat).await;
        offline_account_catches_up(&mut trio.c, &chat, &created.id).await;
        // the organizer is told of nothing
        assert!(trio.a.engine().activity().await.unwrap().items.is_empty());
        drop(b2);
        trio.stop().await;
    }
}
