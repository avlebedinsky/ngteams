//! The push channel as users of two or three ngteams clients meet it: real server processes, operator-created users,
//! and each client's session, engine, store and Trouter channel. Nothing here asks an engine to sync: what a client
//! shows came over its push channel or the catch-up the channel itself starts.

mod support;

use std::time::Duration;

use ngteams_server::db::Db;
use teams_core::engine::{Action, Event, MessageChange};
use teams_proto::chat::MessageText;
use teams_store::Store;
use teams_trouter::{Down, Options, RegistrationReport, RegistrationTimes};

use self::support::{IC3, Link, QUIET, Raw, Rig, SPACES, UNAUTHORIZED, User, brief, options, register, token};
use crate::support::{Backend, PASSWORD, backends};

/// A 1:1 chat of `a` and `b` whose first message `b`'s client shows already.
async fn chat(a: &User, b: &mut User, text: &str) -> (String, String) {
    let chat = a.create(&[&*b], None).await;
    let first = a.send(&chat, text).await;
    b.eventually_stored(&chat, &first).await;
    (chat, first)
}

fn edit(chat: &str, id: &str, text: &str) -> Action {
    Action::Edit {
        conversation: chat.into(),
        message_id: id.into(),
        text: MessageText::plain(text).unwrap(),
    }
}

/// `b` answers; `a` reads it on one client, and the account's other client `a2` hears of the read.
async fn the_accounts_read_reaches_its_other_client(a: &User, a2: &mut User, b: &User, chat: &str) {
    let answer = b.send(chat, "an answer to read").await;
    a2.pushed(0, chat, &answer, MessageChange::New).await;
    assert!(a2.engine().unread_count(chat).await.unwrap() >= 1);
    let read = a2.mark();
    a.write(Action::Read {
        conversation: chat.into(),
        message_id: answer.clone(),
    })
    .await;
    a2.since(
        read,
        "the account's read horizon",
        |event| matches!(event, Event::ReadHorizon { conversation } if conversation == chat),
    )
    .await;
    assert_eq!(a2.engine().unread_count(chat).await.unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_its_edit_reaction_delete_typing_and_a_read_reach_the_other_clients_live() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.user(&rig.first, "a@own.test").await;
        let mut a2 = rig.user(rig.other(), "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        assert_ne!(a.epid, a2.epid);
        let (chat, first) = chat(&a, &mut b, "the chat's first message").await;
        a2.eventually_stored(&chat, &first).await;

        let (on_a2, on_b) = (a2.mark(), b.mark());
        let id = a.send(&chat, "hello, live").await;
        for user in [&mut a2, &mut b] {
            user.pushed(0, &chat, &id, MessageChange::New).await;
            user.shows(&chat, &id, "hello, live").await;
        }

        a.write(edit(&chat, &id, "edited, live")).await;
        for user in [&mut a2, &mut b] {
            user.pushed(0, &chat, &id, MessageChange::Edit).await;
            user.shows(&chat, &id, "edited, live").await;
        }

        a.write(Action::React {
            conversation: chat.clone(),
            message_id: id.clone(),
            key: "like".into(),
        })
        .await;
        for user in [&mut a2, &mut b] {
            user.pushed(0, &chat, &id, MessageChange::Reaction).await;
            let reactions = user.engine().reactions(&chat, &id).await.unwrap();
            assert_eq!(reactions.len(), 1, "{}", user.upn);
            assert_eq!(reactions[0].user, a.mri.as_str());
        }

        let typing = b.mark();
        a.typing(&chat).await;
        b.since(typing, "typing", |event| {
            matches!(event, Event::Typing { conversation, from, typing: true }
                if *conversation == chat && from.as_deref() == Some(a.mri.as_str()))
        })
        .await;

        a.write(Action::Delete {
            conversation: chat.clone(),
            message_id: id.clone(),
        })
        .await;
        for user in [&mut a2, &mut b] {
            user.pushed(0, &chat, &id, MessageChange::Delete).await;
            let deleted = user.engine().message(&chat, &id).await.unwrap().unwrap();
            assert!(deleted.delete_time.is_some() && deleted.content.is_empty());
        }

        the_accounts_read_reaches_its_other_client(&a, &mut a2, &b, &chat).await;

        // each delivered once
        a2.settle().await;
        b.settle().await;
        for (user, mark) in [(&a2, on_a2), (&b, on_b)] {
            for change in [
                MessageChange::New,
                MessageChange::Edit,
                MessageChange::Reaction,
                MessageChange::Delete,
            ] {
                assert_eq!(
                    user.pushes(mark, &chat, &id, change),
                    1,
                    "{} {change:?}: {:?}",
                    user.upn,
                    user.log[mark..].iter().map(brief).collect::<Vec<_>>()
                );
            }
        }
        drop((a, a2, b));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_member_of_a_group_chat_gets_a_push_once_and_a_non_member_gets_nothing() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let mut c = rig.user(&rig.first, "c@own.test").await;
        let mut d = rig.user(rig.other(), "d@own.test").await;
        let group = a.create(&[&b, &c], Some("QA push room")).await;
        let first = a.send(&group, "group, first").await;
        b.eventually_stored(&group, &first).await;
        c.eventually_stored(&group, &first).await;
        let stored = c.engine().conversation(group.clone()).await.unwrap().unwrap();
        assert_eq!(stored.topic.as_deref(), Some("QA push room"));

        let marks = [a.mark(), b.mark(), c.mark()];
        let from_a = a.send(&group, "group, from a").await;
        let from_b = b.send(&group, "group, from b").await;
        a.pushed(0, &group, &from_b, MessageChange::New).await;
        b.pushed(0, &group, &from_a, MessageChange::New).await;
        for id in [&from_a, &from_b] {
            c.pushed(0, &group, id, MessageChange::New).await;
        }
        for user in [&mut a, &mut b, &mut c, &mut d] {
            user.settle().await;
        }
        for (user, mark) in [&a, &b, &c].into_iter().zip(marks) {
            for (id, sender) in [(&from_a, "a@own.test"), (&from_b, "b@own.test")] {
                let pushes = user.pushes(mark, &group, id, MessageChange::New);
                // a member's own post comes back to the client that made it at most once
                assert!(
                    pushes == 1 || (user.upn == sender && pushes == 0),
                    "{} got {id} {pushes} times",
                    user.upn
                );
            }
            assert_eq!(
                user.stored(&group, "group, ").await,
                [first.clone(), from_a.clone(), from_b.clone()]
            );
        }
        // the non-member: no event names the chat, and its store never met it
        let about_the_group = d.count(0, |event| {
            matches!(event, Event::Message { conversation, .. } | Event::ConversationChanged { conversation }
                | Event::Typing { conversation, .. } if *conversation == group)
        });
        assert_eq!(about_the_group, 0, "{:?}", d.log.iter().map(brief).collect::<Vec<_>>());
        assert!(d.engine().conversation(group.clone()).await.unwrap().is_none());
        drop((a, b, c, d));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_post_through_one_node_reaches_a_client_connected_to_the_other_node() {
    for backend in backends() {
        if backend != Backend::Postgres {
            continue;
        }
        let rig = Rig::start(backend).await;
        assert!(rig.two_nodes());
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let (chat, _) = chat(&a, &mut b, "across the nodes, first").await;
        let (on_a, on_b) = (a.mark(), b.mark());
        let to_b = a.send(&chat, "from node A to node B").await;
        b.pushed(on_b, &chat, &to_b, MessageChange::New).await;
        b.shows(&chat, &to_b, "from node A to node B").await;
        let to_a = b.send(&chat, "from node B to node A").await;
        a.pushed(on_a, &chat, &to_a, MessageChange::New).await;
        let typing = b.mark();
        a.typing(&chat).await;
        b.since(
            typing,
            "typing across the nodes",
            |event| matches!(event, Event::Typing { conversation, typing: true, .. } if *conversation == chat),
        )
        .await;
        a.settle().await;
        b.settle().await;
        assert_eq!(b.pushes(on_b, &chat, &to_b, MessageChange::New), 1);
        assert_eq!(a.pushes(on_a, &chat, &to_a, MessageChange::New), 1);
        drop((a, b));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_lost_its_socket_and_then_restarted_ends_with_every_message_once_in_order() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let a = rig.user(&rig.first, "a@own.test").await;
        let link = Link::to(rig.other()).await;
        let mut b = User::signed_in(rig.other(), &rig.origin, "b@own.test", options()).await;
        b.through(&link);
        b.start();
        b.online(0).await;
        let (chat, first) = chat(&a, &mut b, "drop, first").await;
        let live = a.send(&chat, "drop, live").await;
        b.pushed(0, &chat, &live, MessageChange::New).await;
        let mut expected = vec![first, live];

        // the network drops; a goes on posting; the network is back
        let dropped = b.mark();
        link.cut();
        b.since(dropped, "disconnect", |event| {
            matches!(event, Event::Disconnected { .. })
        })
        .await;
        let mut missed = Vec::new();
        for n in 1..=5 {
            missed.push(a.send(&chat, &format!("drop, missed {n}")).await);
        }
        let back = b.mark();
        link.restore();
        b.online(back).await;
        for id in &missed {
            b.eventually_stored(&chat, id).await;
        }
        b.settle().await;
        expected.extend(missed.iter().cloned());
        assert_eq!(b.stored(&chat, "drop, ").await, expected);
        for id in &missed {
            assert!(
                b.pushes(dropped, &chat, id, MessageChange::New) <= 1,
                "{id} was delivered twice: {:?}",
                b.log[dropped..].iter().map(brief).collect::<Vec<_>>()
            );
        }
        let after = a.send(&chat, "drop, live again").await;
        b.pushed(back, &chat, &after, MessageChange::New).await;
        expected.push(after);

        // the client is closed; a posts; the client starts again on its store
        b.close();
        for n in 1..=3 {
            expected.push(a.send(&chat, &format!("drop, while closed {n}")).await);
        }
        let restarted = b.mark();
        b.start();
        b.online(restarted).await;
        for id in &expected {
            b.eventually_stored(&chat, id).await;
        }
        let last = a.send(&chat, "drop, after the restart").await;
        b.pushed(restarted, &chat, &last, MessageChange::New).await;
        expected.push(last);
        b.settle().await;
        assert_eq!(b.stored(&chat, "drop, ").await, expected);
        for id in &expected {
            assert!(b.pushes(restarted, &chat, id, MessageChange::New) <= 1, "{id}");
        }
        drop((a, b));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn clients_reconnect_after_their_node_restarts_and_the_push_works_again() {
    for backend in backends() {
        let mut rig = Rig::start(backend).await;
        let mut a = rig.user(&rig.first, "a@own.test").await;
        let mut b = rig.user(rig.other(), "b@own.test").await;
        let (chat, first) = chat(&a, &mut b, "restart, first").await;
        let mut expected = vec![first];

        // b's node goes down and comes back (with two nodes a's stays up, and a posts meanwhile)
        let (down_a, down_b) = (a.mark(), b.mark());
        if let Some(node) = &mut rig.second {
            node.stop().await;
            expected.push(a.send(&chat, "restart, while b's node is down").await);
            node.start().await;
        } else {
            rig.first.stop().await;
            rig.first.start().await;
            a.since(down_a, "a's disconnect", |event| {
                matches!(event, Event::Disconnected { .. })
            })
            .await;
            a.online(down_a).await;
        }
        b.since(down_b, "b's disconnect", |event| {
            matches!(event, Event::Disconnected { .. })
        })
        .await;
        b.online(down_b).await;
        for id in &expected {
            b.eventually_stored(&chat, id).await;
        }
        let back = b.mark();
        let live = a.send(&chat, "restart, live after b's node").await;
        b.pushed(back, &chat, &live, MessageChange::New).await;
        expected.push(live);

        // a's node too
        let (down_a, back_b) = (a.mark(), b.mark());
        rig.first.stop().await;
        rig.first.start().await;
        a.since(down_a, "a's disconnect", |event| {
            matches!(event, Event::Disconnected { .. })
        })
        .await;
        a.online(down_a).await;
        if !rig.two_nodes() {
            b.online(back_b).await;
        }
        let back = b.mark();
        let again = a.send(&chat, "restart, live after a's node").await;
        b.pushed(back, &chat, &again, MessageChange::New).await;
        expected.push(again);
        let back = a.mark();
        let answer = b.send(&chat, "restart, an answer").await;
        a.pushed(back, &chat, &answer, MessageChange::New).await;
        expected.push(answer);
        a.settle().await;
        b.settle().await;
        assert_eq!(b.stored(&chat, "restart, ").await, expected);
        assert_eq!(a.stored(&chat, "restart, ").await, expected);
        drop((a, b));
        rig.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_an_expired_and_a_late_token_and_a_disabled_account_are_refused() {
    for backend in backends() {
        let rig = Rig::start_with(backend, "[trouter]\nauth_timeout = 1\n[tokens]\naccess_lifetime = 6\n").await;
        let node = rig.other();
        let epid = "00000000-0000-4000-8000-0000000000a1";
        let issued = tokio::time::Instant::now();
        let old = token(&rig.first, "a@own.test", IC3).await;
        let spaces = token(node, "a@own.test", SPACES).await;

        // late: nothing sent within the time to authenticate
        let mut silent = Raw::open(node, &rig.origin, epid).await;
        assert_eq!(silent.next().await.as_deref(), Some("1::"));
        assert!(silent.ends_with(r#"0:::{"reason":"timeout"}"#).await);

        // bad: no token of this server, and a token for another service
        for bad in ["made-up", spaces.as_str()] {
            let mut socket = Raw::open(node, &rig.origin, epid).await;
            socket.authenticate(bad).await;
            assert!(socket.ends_with(UNAUTHORIZED).await);
            assert_eq!(register(node, bad, epid, "https://x/").await, 401);
        }

        // good: the endpoint, registered; and the socket is closed when its token runs out
        let fresh = token(node, "a@own.test", IC3).await;
        let mut socket = Raw::open(node, &rig.origin, epid).await;
        let info = socket.connect(&fresh).await;
        let surl = info["surl"].as_str().unwrap().to_owned();
        assert!(surl.starts_with(&format!("{}/trouter/v4/f/", rig.origin)));
        assert_eq!(register(node, &fresh, epid, &surl).await, 202);
        assert_eq!(
            socket.next_within(Duration::from_secs(12)).await.unwrap().as_deref(),
            Some(UNAUTHORIZED)
        );

        // expired, within the minute of clock skew the server allows: refused as past it, never connected
        let mut socket = Raw::open(node, &rig.origin, epid).await;
        socket.authenticate(&old).await;
        assert!(socket.ends_with(UNAUTHORIZED).await);
        // expired, past the skew
        tokio::time::sleep_until(issued + Duration::from_secs(6 + 60 + 2)).await;
        let mut socket = Raw::open(node, &rig.origin, epid).await;
        socket.authenticate(&old).await;
        assert!(socket.ends_with(UNAUTHORIZED).await);
        assert_eq!(register(node, &old, epid, &surl).await, 401);

        // disabled by the operator: its valid token opens no socket and registers nothing
        let valid = token(node, "b@own.test", IC3).await;
        let disabled = rig.first.cli(&["user", "disable", "b@own.test"], "");
        assert!(!disabled.contains(PASSWORD));
        let mut socket = Raw::open(node, &rig.origin, epid).await;
        socket.authenticate(&valid).await;
        assert!(socket.ends_with(UNAUTHORIZED).await);
        assert_ne!(register(node, &valid, epid, &surl).await, 202);
        assert_ne!(node.password("b@own.test", PASSWORD).await.status(), 200);
        // enabled again, it is taken
        rig.first.cli(&["user", "enable", "b@own.test"], "");
        let again = token(node, "b@own.test", IC3).await;
        let mut socket = Raw::open(node, &rig.origin, epid).await;
        socket.connect(&again).await;
        rig.stop().await;
    }
}

/// The client's own renewal times, with the token's expiry looked at every second instead of every thirty.
fn renewing() -> Options {
    Options {
        renew_check: Duration::from_secs(1),
        ..options()
    }
}

/// Access tokens that live 20 seconds longer than the client's refresh margin: a client renews each after 20 s.
const SHORT_TOKENS: &str = "[tokens]\naccess_lifetime = 320\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_refresh_on_a_long_connection_keeps_the_push() {
    for backend in backends() {
        let rig = Rig::start_with(backend, SHORT_TOKENS).await;
        let a = User::sign_in(&rig.first, &rig.origin, "a@own.test", renewing()).await;
        let mut b = User::sign_in(rig.other(), &rig.origin, "b@own.test", renewing()).await;
        let (chat, first) = chat(&a, &mut b, "refresh, first").await;
        let mark = b.mark();
        let mut expected = vec![first];
        // well past two renewals of the socket's token
        for n in 1..=12 {
            let id = a.send(&chat, &format!("refresh, {n}")).await;
            b.pushed(mark, &chat, &id, MessageChange::New).await;
            expected.push(id);
            tokio::time::sleep(Duration::from_secs(4)).await;
        }
        b.settle().await;
        let renewals = b.count(0, |event| {
            matches!(
                event,
                Event::Disconnected {
                    down: Down::Renewal,
                    ..
                }
            )
        });
        let log: Vec<String> = b.log.iter().map(brief).collect();
        assert!(renewals >= 2, "{log:?}");
        assert_eq!(b.count(0, |event| matches!(event, Event::PushStopped(_))), 0, "{log:?}");
        assert_eq!(b.stored(&chat, "refresh, ").await, expected);
        for id in &expected[1..] {
            assert_eq!(b.pushes(mark, &chat, id, MessageChange::New), 1, "{id}: {log:?}");
        }
        drop((a, b));
        rig.stop().await;
    }
}

/// Since `mark` the client was pushed none of `ids`, shows none of them and never connected again.
async fn nothing_reached(b: &User, mark: usize, chat: &str, ids: &[String]) {
    let log: Vec<String> = b.log[mark..].iter().map(brief).collect();
    for id in ids {
        assert_eq!(b.pushes(mark, chat, id, MessageChange::New), 0, "{log:?}");
        assert!(b.engine().message(chat, id).await.unwrap().is_none(), "{log:?}");
    }
    assert_eq!(
        b.count(mark, |event| matches!(event, Event::Connected { .. })),
        0,
        "{log:?}"
    );
}

/// The operator's `command` on b's account while b's client is connected: the service ends b's push at once, and
/// nothing posted after the command returned is pushed to b.
async fn the_operators_command_ends_the_clients_push(backend: Backend, command: &[&str], input: &str) {
    let rig = Rig::start_with(backend, SHORT_TOKENS).await;
    let a = User::sign_in(&rig.first, &rig.origin, "a@own.test", renewing()).await;
    let mut b = User::sign_in(rig.other(), &rig.origin, "b@own.test", renewing()).await;
    let (chat, _) = chat(&a, &mut b, "revoked, first").await;
    let mark = b.mark();
    rig.first.cli(command, input);
    let returned = tokio::time::Instant::now();
    let just_after = a.send(&chat, "revoked, posted just after").await;
    // long before its token's renewal (20 s) would have ended the socket
    b.since(mark, "the service's disconnect", |event| {
        matches!(event, Event::Disconnected { down: Down::ServiceDisconnect(Some(reason)), .. }
            if reason == "unauthorized")
    })
    .await;
    assert!(returned.elapsed() < Duration::from_secs(5), "{:?}", returned.elapsed());
    // its tokens can't be renewed: the client's session is signed out, as the app then shows it
    b.until_signed_out().await;
    let unseen = a.send(&chat, "revoked, never shown").await;
    b.settle().await;
    nothing_reached(&b, mark, &chat, &[just_after, unseen]).await;
    drop((a, b));
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_an_account_the_operator_disabled_loses_its_push_at_once() {
    for backend in backends() {
        the_operators_command_ends_the_clients_push(backend, &["user", "disable", "b@own.test"], "").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_of_an_account_whose_password_the_operator_reset_loses_its_push_at_once() {
    for backend in backends() {
        let command = ["user", "reset-password", "b@own.test"];
        the_operators_command_ends_the_clients_push(backend, &command, "another horse battery\n").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_clients_registration_is_taken_and_renewed_and_no_push_comes_after_its_sign_out() {
    for backend in backends() {
        let rig = Rig::start(backend).await;
        let db = Db::connect(&rig.database, 1).await.unwrap();
        let a = rig.user(&rig.first, "a@own.test").await;
        // a client that renews its registration every two seconds instead of every 3570
        let often = Options {
            registration: RegistrationTimes {
                ttl: Duration::from_secs(3),
                renew_before: Duration::from_secs(1),
                ..RegistrationTimes::default()
            },
            ..options()
        };
        let mut b = User::sign_in(rig.other(), &rig.origin, "b@own.test", often).await;
        let oid = b.oid().to_owned();
        let until = async |epid: String| {
            ngteams_server::registrar::until(&db, &oid, &epid, "TeamsCDLWebWorker")
                .await
                .unwrap()
        };
        let taken = until(b.epid.clone()).await.expect("the registration is kept");
        let ttl = taken - ngteams_server::db::now();
        assert!((3540..=3600).contains(&ttl), "{ttl}");
        assert_eq!(
            ngteams_server::registrar::until(&db, a.oid(), &b.epid, "TeamsCDLWebWorker")
                .await
                .unwrap(),
            None,
            "nobody else's"
        );

        let (chat, _) = chat(&a, &mut b, "registered, first").await;
        // renewed: the same registration, with a later end
        let mut mark = b.mark();
        for _ in 0..2 {
            mark = 1 + b
                .since(mark, "a renewal", |event| {
                    matches!(event, Event::Registration(RegistrationReport::Registered))
                })
                .await;
        }
        let renewed = until(b.epid.clone()).await.unwrap();
        assert!(renewed > taken, "{taken} then {renewed}");
        let live = a.send(&chat, "registered, live after the renewals").await;
        b.pushed(mark, &chat, &live, MessageChange::New).await;

        // the sign-out: the client's engine is shut down, its socket says goodbye
        let signed_out = b.epid.clone();
        let store = b.store_path();
        b.close();
        let unseen = a.send(&chat, "registered, posted after the sign-out").await;
        tokio::time::sleep(QUIET).await;
        let kept = Store::open(&store).unwrap().message(&chat, &unseen).unwrap();
        assert!(kept.is_none(), "a signed-out client was pushed a message");
        // the registration outlives the sign-out and runs out by its TTL: the client sends no removal (no capture
        // shows Teams' one), and its goodbye is the same on a stop that Teams leaves the registration through
        let left = until(signed_out.clone()).await.expect("the registration is kept");
        assert!(left > ngteams_server::db::now() + 3000, "{left}");

        // signed in again: a new endpoint, registered; what it missed is there, and the push is live
        let again = b.mark();
        b.start();
        b.online(again).await;
        assert_ne!(b.epid, signed_out);
        assert!(until(b.epid.clone()).await.is_some());
        b.eventually_stored(&chat, &unseen).await;
        let live = a.send(&chat, "registered, live after the second sign-in").await;
        b.pushed(again, &chat, &live, MessageChange::New).await;
        b.settle().await;
        assert_eq!(b.pushes(again, &chat, &live, MessageChange::New), 1);
        drop((a, b));
        rig.stop().await;
    }
}
