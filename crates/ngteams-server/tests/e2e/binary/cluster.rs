//! ngteams on its own server as a deployment runs it (`packaging/server/compose.yaml`): two API nodes
//! (`serve --role api`) and a media node (`serve --role media`) of the real binary on one Postgres, one Redis and one
//! S3 bucket (`NGTEAMS_SERVER_S3_URL`: `RustFS` in the runs), coturn as the calls' relay, and a front that sends each
//! connection to a node that takes it, as the reverse proxy does. The server is seeded first with hundreds of
//! synthetic accounts, their chats, a team, files and events ([`seed`]). The people `srv-a`, `srv-b` and `srv-c` run
//! the client as the app runs it (session, engine with its calling stack and push channels, media on the file
//! devices), each through the front of a different node.
//!
//! - chat both ways, each push in under two seconds across the nodes;
//! - reactions, edits, an account's read on its other device, presence;
//! - a picture and a file through the shared bucket;
//! - a 1:1 audio and video call, and a group call of three with video through the media node;
//! - a node killed: its clients reconnect to the other node and catch up;
//! - the single-node mode (SQLite, no Redis, `serve` with its own media node) runs the chat and call subset.
//!
//! The calls are skipped without `NGTS_TEST_COTURN`/`NGTS_TEST_COTURN_SECRET`, the two-node cases without
//! `NGTEAMS_SERVER_PG_URL` and `NGTEAMS_SERVER_REDIS_URL` (`docs/server.md`).

mod seed;
mod support;

use std::time::{Duration, Instant};

use teams_core::Availability;
use teams_core::engine::{Action, Event, MessageChange, OwnStatus};
use teams_proto::chat::MessageText;
use teams_proto::{Cloud, ManualAvailability};

use self::support::{Cluster, Shape, TONES, arrives, send, stored_text, times};
use super::calls::group_video;
use super::calls::support::TALK;
use super::calls::{camera_shows, connect, hang_up, heard_each_other};

/// The longest a message may take from its sender's send to the other client's push.
const PUSH: Duration = Duration::from_secs(2);

/// `from` sends `count` messages to `chat`; `to` gets each pushed. How long each took, from the send to the push.
async fn round(
    from: &mut super::calls::support::Phone,
    to: &mut super::calls::support::Phone,
    chat: &str,
    count: usize,
) -> Vec<Duration> {
    let mut taken = Vec::new();
    for n in 0..count {
        let mark = to.mark();
        let text = format!("{} → {} #{n} ✉️", from.upn, to.upn);
        let sent = Instant::now();
        let id = send(from, chat, &text).await;
        arrives(to, mark, chat, &id, MessageChange::New).await;
        taken.push(sent.elapsed());
        assert_eq!(stored_text(to, chat, &id).await, format!("<p>{text}</p>"), "{}", to.upn);
    }
    taken
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_on_two_nodes_chat_both_ways_and_each_push_comes_in_under_two_seconds() {
    let Some(cluster) = Cluster::start(Shape::Pair, "chat").await else {
        return;
    };
    let seeded = seed::plant(&cluster).await;
    let mut a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 1).await;
    // the seeded chats came with the first sync
    for (phone, chats) in [(&a, &seeded.chats[0]), (&b, &seeded.chats[1])] {
        phone.sync().await;
        for chat in chats {
            let page = phone.engine().messages(chat, None, 50).await.unwrap();
            assert!(!page.messages.is_empty(), "{}: seeded chat {chat} is empty", phone.upn);
        }
    }
    assert_eq!(seeded.accounts.len(), seed::ACCOUNTS);
    // the seeded team, with its general and two more channels, is in a's list once its read settles
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    loop {
        let teams = a.engine().teams().await.unwrap().unwrap_or_default();
        if teams
            .iter()
            .any(|team| team.id == seeded.team && team.channels.len() == 3)
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "a's teams: {teams:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    let there = round(&mut a, &mut b, &chat, 8).await;
    let back = round(&mut b, &mut a, &chat, 8).await;
    eprintln!("{}", times("a → b", there.clone()));
    eprintln!("{}", times("b → a", back.clone()));
    for taken in there.iter().chain(&back) {
        assert!(*taken < PUSH, "a push took {taken:?}");
    }
    assert_eq!(cluster.front(0).counts()[1], 0, "a's client reached b's node");
    assert_eq!(cluster.front(1).counts()[1], 0, "b's client reached a's node");
    a.close().await;
    b.close().await;
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reactions_edits_an_accounts_read_and_presence_cross_the_nodes() {
    let Some(cluster) = Cluster::start(Shape::Pair, "chat-changes").await else {
        return;
    };
    let mut a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 1).await;
    // a's second device, at b's node
    let mut a2 = cluster.person(0, 1).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    a2.sync().await;

    let (on_b, on_a2) = (b.mark(), a2.mark());
    let id = send(&a, &chat, "react to this, then I'll edit it").await;
    arrives(&mut b, on_b, &chat, &id, MessageChange::New).await;
    arrives(&mut a2, on_a2, &chat, &id, MessageChange::New).await;

    // b reacts: a and a2 see it, from b
    let mark = a.mark();
    support::write(
        &b,
        Action::React {
            conversation: chat.clone(),
            message_id: id.clone(),
            key: "like".into(),
        },
    )
    .await;
    arrives(&mut a, mark, &chat, &id, MessageChange::Reaction).await;
    let reactions = a.engine().reactions(&chat, &id).await.unwrap();
    assert_eq!(reactions.len(), 1, "{reactions:?}");
    assert_eq!(reactions[0].user, b.mri);

    // a edits: b sees the new text
    stored_text(&a, &chat, &id).await;
    let mark = b.mark();
    support::write(
        &a,
        Action::Edit {
            conversation: chat.clone(),
            message_id: id.clone(),
            text: MessageText::plain("edited ✏️ on node a").unwrap(),
        },
    )
    .await;
    arrives(&mut b, mark, &chat, &id, MessageChange::Edit).await;
    assert!(stored_text(&b, &chat, &id).await.contains("edited ✏️ on node a"));

    // b answers; a reads it on its first device, and its second device at the other node hears of the read
    let mark = a2.mark();
    let answer = send(&b, &chat, "an answer to read").await;
    arrives(&mut a2, mark, &chat, &answer, MessageChange::New).await;
    assert!(a2.engine().unread_count(&chat).await.unwrap() >= 1);
    let read = a2.mark();
    support::write(
        &a,
        Action::Read {
            conversation: chat.clone(),
            message_id: answer.clone(),
        },
    )
    .await;
    a2.since(
        read,
        "the account's read",
        |event| matches!(event, Event::ReadHorizon { conversation } if *conversation == chat),
    )
    .await;
    assert_eq!(a2.engine().unread_count(&chat).await.unwrap(), 0);

    presence_crosses(&a, &mut b).await;
    for phone in [&mut a, &mut a2, &mut b] {
        phone.close().await;
    }
    cluster.stop().await;
}

/// b watches a; a's window is in use (the app reports it), then a's statuses, set at a's node, reach b at its node.
async fn presence_crosses(a: &super::calls::support::Phone, b: &mut super::calls::support::Phone) {
    b.engine().presence(a.mri.clone()).await.unwrap();
    let mark = b.mark();
    a.engine().own_availability(OwnStatus::Available).await.unwrap();
    let of = a.mri.clone();
    b.since(mark, "a online", |event| {
        matches!(event, Event::Presence { mri, availability } if *mri == of && *availability == Availability::Available)
    })
    .await;
    for (status, shown) in [
        (ManualAvailability::Busy, Availability::Busy),
        (ManualAvailability::DoNotDisturb, Availability::DoNotDisturb),
        (ManualAvailability::Available, Availability::Available),
    ] {
        let mark = b.mark();
        a.engine().own_availability(OwnStatus::Manual(status)).await.unwrap();
        let of = a.mri.clone();
        b.since(
            mark,
            &format!("a as {shown:?}"),
            |event| matches!(event, Event::Presence { mri, availability } if *mri == of && *availability == shown),
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_picture_and_a_file_sent_at_one_node_are_read_at_the_other_from_the_shared_bucket() {
    let Some(cluster) = Cluster::start(Shape::Pair, "files").await else {
        return;
    };
    let cloud = Cloud::own(&cluster.origin).unwrap();
    let a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 1).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    let before = cluster.blobs.keys().await.len();

    let png = crate::media::png(120, 80);
    let mark = b.mark();
    let (src, id) = support::paste(&a, &cloud, &chat, &png, (120, 80)).await;
    arrives(&mut b, mark, &chat, &id, MessageChange::New).await;
    assert!(stored_text(&b, &chat, &id).await.contains(&src));
    assert_eq!(support::picture(&b, &src).await, png);

    let bytes: Vec<u8> = (0..300_000u32).map(|n| u8::try_from(n % 251).unwrap()).collect();
    let mark = b.mark();
    let id = support::send_file(&a, &cloud, &chat, "Quarterly plan (v2) — ünïcode.pdf", &bytes).await;
    arrives(&mut b, mark, &chat, &id, MessageChange::New).await;
    let file = support::file_of(&b, &cloud, &chat, &id).await;
    assert_eq!(file.name, "Quarterly plan (v2) — ünïcode.pdf");
    assert_eq!(support::download(&b, file.share_url.as_deref().unwrap()).await, bytes);
    assert!(
        cluster.blobs.keys().await.len() >= before + 2,
        "the picture and the file are objects of the bucket"
    );
    drop((a, b));
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_1_1_call_across_the_nodes_carries_audio_and_video_both_ways() {
    let Some(cluster) = Cluster::with_calls(Shape::Pair, "call").await else {
        return;
    };
    one_to_one(&cluster).await;
    cluster.stop().await;
}

/// srv-a calls srv-b in their chat, b answers: both hear and see each other, a hangs up.
async fn one_to_one(cluster: &Cluster) {
    let mut a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 1).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    let talk = connect(&mut a, &mut b, &chat).await;
    tokio::time::sleep(TALK).await;
    camera_shows(&a, talk.call, &b).await;
    camera_shows(&b, talk.answered, &a).await;
    hang_up(&mut a, talk.call, &mut b, talk.answered).await;
    heard_each_other(&a, TONES[0], &b, TONES[1]).await;
    no_open_calls(cluster).await;
    a.close().await;
    b.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_on_two_nodes_see_and_hear_each_other_in_a_group_call_through_the_media_node() {
    let Some(cluster) = Cluster::with_calls(Shape::Pair, "group").await else {
        return;
    };
    group_of_three(&cluster).await;
    cluster.stop().await;
}

/// srv-a calls the group chat of the three; b (another node) and c answer; each sees the other two's cameras and
/// hears them in the mix; everyone hangs up.
async fn group_of_three(cluster: &Cluster) {
    let mut phones = vec![
        cluster.person(0, 0).await,
        cluster.person(1, 1).await,
        cluster.person(2, 0).await,
    ];
    let chat = {
        let [a, b, c] = &phones[..] else { unreachable!() };
        group_video::group_chat(a, &[b, c], &[], "Group call 📞 across nodes").await
    };
    let calls = {
        let [a, b, c] = &mut phones[..] else { unreachable!() };
        group_video::start_and_answer(a, &mut [b, c], &chat).await
    };
    group_video::three_cameras_seen(&mut phones, &calls).await;
    for (phone, call) in phones.iter_mut().zip(&calls).rev() {
        group_video::hang_up(phone, *call).await;
    }
    no_open_calls(cluster).await;
    for phone in &mut phones {
        phone.close().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_killed_nodes_clients_reconnect_to_the_other_node_and_catch_up() {
    let Some(mut cluster) = Cluster::start(Shape::Pair, "failover").await else {
        return;
    };
    let mut a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 1).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    round(&mut a, &mut b, &chat, 2).await;

    // a's node dies; b writes meanwhile
    let reconnect = a.mark();
    cluster.kill_node(0);
    let mut missed = Vec::new();
    for n in 0..3 {
        missed.push(send(&b, &chat, &format!("while a's node is down #{n}")).await);
    }
    // a's client is back, at the other node, and holds what it missed
    a.online(reconnect).await;
    for id in &missed {
        stored_text(&a, &chat, id).await;
    }
    assert!(
        cluster.front(0).counts()[1] > 0,
        "a's client never reached the other node"
    );
    // and the push and a's own writes work again, both ways
    let there = round(&mut a, &mut b, &chat, 3).await;
    let back = round(&mut b, &mut a, &chat, 3).await;
    eprintln!("{}", times("after the kill, a → b", there));
    eprintln!("{}", times("after the kill, b → a", back));

    // the node comes back and takes new clients again
    cluster.start_node(0).await;
    let mut c = cluster.person(2, 0).await;
    let group = c.chat_with(&b.mri).await;
    b.sync().await;
    round(&mut c, &mut b, &group, 2).await;
    for phone in [&mut a, &mut b, &mut c] {
        phone.close().await;
    }
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_node_on_sqlite_chats_both_ways_with_reactions_and_edits() {
    let Some(cluster) = Cluster::start(Shape::Single, "single-chat").await else {
        return;
    };
    let mut a = cluster.person(0, 0).await;
    let mut b = cluster.person(1, 0).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    let taken: Vec<Duration> = round(&mut a, &mut b, &chat, 4)
        .await
        .into_iter()
        .chain(round(&mut b, &mut a, &chat, 4).await)
        .collect();
    eprintln!("{}", times("one node", taken.clone()));
    assert!(taken.iter().all(|taken| *taken < PUSH), "{taken:?}");
    let id = send(&a, &chat, "one node: react and edit").await;
    let mark = a.mark();
    support::write(
        &b,
        Action::React {
            conversation: chat.clone(),
            message_id: id.clone(),
            key: "laugh".into(),
        },
    )
    .await;
    arrives(&mut a, mark, &chat, &id, MessageChange::Reaction).await;
    stored_text(&a, &chat, &id).await;
    let mark = b.mark();
    support::write(
        &a,
        Action::Edit {
            conversation: chat.clone(),
            message_id: id.clone(),
            text: MessageText::plain("one node: edited").unwrap(),
        },
    )
    .await;
    arrives(&mut b, mark, &chat, &id, MessageChange::Edit).await;
    a.close().await;
    b.close().await;
    cluster.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_node_on_sqlite_runs_a_1_1_call_and_a_group_call_with_video() {
    let Some(cluster) = Cluster::with_calls(Shape::Single, "single-calls").await else {
        return;
    };
    one_to_one(&cluster).await;
    group_of_three(&cluster).await;
    cluster.stop().await;
}

/// Waits until the server holds no call that isn't over.
async fn no_open_calls(cluster: &Cluster) {
    let db = ngteams_server::db::Db::connect(&cluster.database, 1).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let open = ngteams_server::calling::open_calls(&db).await.unwrap();
        if open.is_empty() {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "calls left open: {open:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
