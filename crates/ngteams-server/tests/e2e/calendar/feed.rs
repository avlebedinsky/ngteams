//! A meeting in the activity feed: the invitation, a change and the cancellation are items the invited account's
//! feed keeps, as a mention's are, so a client that was away lists them and marks them read.

use teams_core::EventWrite;
use teams_core::activity::{ActivityKind, MeetingChange};
use teams_core::engine::Action;
use teams_proto::ChannelOwnProperty;

use super::meetings::{create, invited};
use super::{HOUR, MINUTE, MONDAY, Seat, draft, server};
use crate::chatsvc::activity::{feed, sent};
use crate::support::backends;
use crate::teams::activity::set;

#[tokio::test(flavor = "multi_thread")]
async fn a_meetings_invitation_change_and_cancellation_are_kept_in_the_invited_accounts_feed() {
    for backend in backends() {
        let server = server(backend).await;
        let ada = Seat::new(&server, "ada@own.test", "Ada").await;
        let bert = Seat::new(&server, "bert@own.test", "Bert").await;
        let dan = Seat::new(&server, "dan@own.test", "Dan").await;
        let start = MONDAY + 14 * HOUR;
        let mut planning = draft("Planning", start, 30);
        planning.online = true;
        planning.attendees = vec![
            invited("bert@own.test", "Bert", Some(&bert.mri), true),
            invited("dan@own.test", "Dan", Some(&dan.mri), true),
        ];
        let created = create(&ada, planning.clone()).await;
        let chat = created.meeting_chat.clone().unwrap();

        // kept, not only pushed: an engine that was not connected when it was written lists it
        let theirs = feed(&bert.engine).await;
        assert_eq!(theirs.items.len(), 1, "{theirs:?}");
        let item = &theirs.items[0];
        assert_eq!(item.kind, ActivityKind::Meeting(MeetingChange::Invited));
        assert_eq!(item.title(), "Ada invited you");
        assert_eq!(item.actor.as_deref(), Some(ada.mri.as_str()));
        assert_eq!(item.preview, "Planning");
        assert_eq!(item.meeting_event.as_deref(), Some(created.id.as_str()));
        assert_eq!(item.meeting_time, Some((start, start + 30 * MINUTE)));
        assert!(!theirs.is_read(item));
        assert_eq!(theirs.unseen(), 1);
        assert_eq!(feed(&dan.engine).await.items.len(), 1);
        assert!(
            feed(&ada.engine).await.items.is_empty(),
            "its organizer is told of nothing"
        );

        // the account's mark is kept as any feed item's is: its other device reads it back
        let invitation = item.id.clone();
        sent(
            &bert.engine,
            Action::ActivityRead {
                item: invitation.clone(),
                read: true,
            },
        )
        .await;
        let other_device = Seat::of(&server, "bert@own.test", bert.id.clone()).await;
        let kept = feed(&other_device.engine).await;
        assert!(kept.is_read(kept.item(&invitation).unwrap()));
        assert_eq!(kept.unseen(), 0);

        // a change is a new item, for whoever has not muted the meeting's chat
        assert!(dan.engine.sync().await.unwrap().failed.is_empty());
        set(&dan, &chat, ChannelOwnProperty::Alerts, "false").await;
        let mut later = planning;
        later.start.ms += HOUR;
        later.end.ms += HOUR;
        let whole = ada.engine.calendar_event(created.id.clone()).await.unwrap();
        ada.engine
            .write_event(EventWrite::Update {
                id: created.id.clone(),
                event: whole.raw,
                draft: later,
            })
            .await
            .unwrap();
        let theirs = feed(&other_device.engine).await;
        assert_eq!(theirs.items.len(), 2, "{theirs:?}");
        assert_eq!(theirs.items[0].kind, ActivityKind::Meeting(MeetingChange::Updated));
        assert_eq!(theirs.items[0].title(), "Ada updated");
        assert_eq!(theirs.items[0].meeting_time, Some((start + HOUR, start + 90 * MINUTE)));
        assert!(!theirs.is_read(&theirs.items[0]));
        assert_eq!(feed(&dan.engine).await.items.len(), 1, "muted: the change is no item");

        ada.engine
            .write_event(EventWrite::Cancel {
                id: created.id.clone(),
                chat: Some(chat),
                message: None,
            })
            .await
            .unwrap();
        let theirs = feed(&bert.engine).await;
        assert_eq!(theirs.items.len(), 3, "{theirs:?}");
        assert_eq!(theirs.items[0].kind, ActivityKind::Meeting(MeetingChange::Canceled));
        assert_eq!(
            feed(&dan.engine).await.items.len(),
            1,
            "muted: the cancellation is no item"
        );

        other_device.stop();
        for seat in [ada, bert, dan] {
            seat.stop();
        }
        server.stop().await;
    }
}
