//! The activity feed and a group chat's people on the push channel: a mention's item reaches the mentioned account's
//! clients live, and someone added to or taken out of a group chat hears exactly what is theirs to hear.

use teams_core::activity::{ActivityKind, Mention};
use teams_core::engine::{Action, ChatHistory, Event, MessageChange};

use super::support::{Rig, User};
use crate::chatsvc::activity::{FEED, mentioning};
use crate::chatsvc::members::RemoveMember;
use crate::support::backends;

pub(super) fn in_chat(event: &Event, chat: &str) -> bool {
    matches!(event, Event::Message { conversation, .. } if conversation == chat)
}

/// Waits until the push channel brought the chat's own event of `kind`.
async fn roster_event(user: &mut User, chat: &str, kind: &str) {
    user.since(0, kind, |event| {
        matches!(event, Event::Message { conversation, message: Some(message), .. }
            if conversation == chat && message.message_type == kind)
    })
    .await;
}

/// Waits until the client knows the chat's people: its own writes to the roster need them.
pub(super) async fn knows_people(user: &mut User, chat: &str, people: usize) {
    for _ in 0..30 {
        if user.engine().members(chat).await.unwrap().len() == people {
            return;
        }
        user.settle().await;
    }
    panic!("{}: never knew the {people} people of {chat}", user.upn);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mentions_feed_item_and_a_group_chats_people_changes_reach_the_clients_they_are_for_live() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(&rig.first, "c@own.test").await;
        let mut d = rig.user(rig.other(), "d@own.test").await;
        let group = a.create(&[&b, &c], Some("Launch")).await;
        let first = a.send(&group, "before dan").await;
        for user in [&mut b, &mut c] {
            user.pushed(0, &group, &first, MessageChange::New).await;
        }

        // a mention's item is in the mentioned account's feed without a sync, and in nobody else's
        let on_c = c.mark();
        let mention = Action::Send {
            conversation: group.clone(),
            text: mentioning(b.mri.as_str(), "Bert", "look at this"),
        };
        let mention = a.write(mention).await.message_id.unwrap();
        b.since(0, "the mention's feed item", |event| in_chat(event, FEED))
            .await;
        let feed = b.engine().activity().await.unwrap();
        assert_eq!(feed.items.len(), 1, "{feed:?}");
        assert_eq!(feed.items[0].kind, ActivityKind::Mention(Mention::You));
        assert_eq!(feed.items[0].message.as_deref(), Some(mention.as_str()));
        assert_eq!(feed.unseen(), 1);
        c.pushed(on_c, &group, &mention, MessageChange::New).await;
        c.settle().await;
        assert_eq!(c.count(on_c, |event| in_chat(event, FEED)), 0, "{}", c.upn);
        assert!(c.engine().activity().await.unwrap().items.is_empty());

        // someone added hears of it and of what follows, and nothing of what was before
        knows_people(&mut a, &group, 3).await;
        a.write(Action::AddMembers {
            conversation: group.clone(),
            members: vec![d.mri.clone()],
            history: ChatHistory::None,
        })
        .await;
        for user in [&mut b, &mut c, &mut d] {
            roster_event(user, &group, "ThreadActivity/AddMember").await;
        }
        let welcome = a.send(&group, "welcome dan").await;
        d.pushed(0, &group, &welcome, MessageChange::New).await;
        d.shows(&group, &welcome, "welcome dan").await;
        assert!(
            d.engine()
                .message(group.as_str(), first.as_str())
                .await
                .unwrap()
                .is_none()
        );

        // someone taken out hears that, and nothing after it
        let remove = RemoveMember {
            chat: group.clone(),
            member: c.mri.as_str().to_owned(),
        };
        assert_eq!(a.client().write(remove).await.unwrap().status(), 200);
        for user in [&mut b, &mut c, &mut d] {
            roster_event(user, &group, "ThreadActivity/DeleteMember").await;
        }
        c.settle().await;
        let on_c = c.mark();
        let later = a.send(&group, "without cora").await;
        for user in [&mut b, &mut d] {
            user.pushed(0, &group, &later, MessageChange::New).await;
        }
        c.settle().await;
        assert_eq!(c.count(on_c, |event| in_chat(event, &group)), 0, "{}", c.upn);
        assert!(
            c.engine()
                .message(group.as_str(), later.as_str())
                .await
                .unwrap()
                .is_none()
        );
        rig.stop().await;
    }
}
