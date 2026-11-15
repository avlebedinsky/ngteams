//! The calls two users make: one answered, with its audio, video, mute and hold; the ones nobody took; the ones to
//! someone who can't be reached. What their chat says of each, live and after a restart of the client.

use std::time::Duration;

use teams_core::{CallAction, CallStatus, End};

use super::support::{A_TONE, B_TONE, Line, Phone, Rig, TALK, Turn};
use super::{Talk, camera_shows, connect, hang_up, heard_each_other, pair};
use crate::support::backends;

fn started(line: &Line) -> bool {
    *line == Line::Started
}

/// b's mute shows at a, and its end; a's hold shows at b, and its end.
async fn mute_and_hold(a: &mut Phone, b: &mut Phone, talk: Talk) {
    let (b_mri, from) = (b.mri.clone(), a.mark());
    let muted = |muted| CallAction::Mute {
        call: talk.answered,
        muted,
    };
    b.act(muted(true)).await;
    a.view(from, talk.call, "b muted", |view| {
        view.participants.iter().any(|party| party.mri == b_mri && party.muted)
    })
    .await;
    let from = a.mark();
    b.act(muted(false)).await;
    a.view(from, talk.call, "b unmuted", |view| {
        view.participants.iter().any(|party| party.mri == b_mri && !party.muted)
    })
    .await;
    let to = b.mark();
    let held = |held| CallAction::Hold { call: talk.call, held };
    a.act(held(true)).await;
    assert!(b.held_by_far_end(to, talk.answered).await, "b sees itself on hold");
    let to = b.mark();
    a.act(held(false)).await;
    assert!(!b.held_by_far_end(to, talk.answered).await, "b sees the hold end");
}

/// a calls b, whose two clients ring; one answers and the other stops ringing.
async fn answered_on_one_of_two_clients(a: &mut Phone, b: &mut Phone, idle: &mut Phone, chat: &str) -> Talk {
    let (from, to, also) = (a.mark(), b.mark(), idle.mark());
    let call = a.act(CallAction::Call { chat: chat.to_owned() }).await;
    a.status(from, call, &CallStatus::Ringing).await;
    let rings = b.incoming(to).await;
    let rings_too = idle.incoming(also).await;
    for ring in [&rings, &rings_too] {
        assert_eq!(ring.caller.as_str(), a.mri);
        assert_eq!(ring.caller_name.as_deref(), Some("Ada"));
        assert_eq!(ring.group_chat, None);
    }
    let answered = b.act(CallAction::Answer { call: rings.call }).await;
    b.status(to, answered, &CallStatus::Connected).await;
    a.status(from, call, &CallStatus::Connected).await;
    let end = idle.ring_ended(also, rings_too.call).await;
    assert_eq!(end.code, Some(487), "the other client stops ringing: {end:?}");
    Talk { call, answered }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_is_answered_carries_audio_and_video_and_either_side_hangs_up() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "answered", 30, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        // b's second client, at the node b's first one isn't at
        let mut idle = rig.phone(&rig.first, "b@own.test", "Bert", B_TONE).await;
        idle.sync().await;
        let seen = idle.mark();

        let talk = answered_on_one_of_two_clients(&mut a, &mut b, &mut idle, &chat).await;
        tokio::time::sleep(TALK).await;
        camera_shows(&a, talk.call, &b).await;
        camera_shows(&b, talk.answered, &a).await;
        assert!(a.video.preview.counts().frames > 0, "a's self view");
        mute_and_hold(&mut a, &mut b, talk).await;
        tokio::time::sleep(TALK).await;
        hang_up(&mut a, talk.call, &mut b, talk.answered).await;
        heard_each_other(&a, A_TONE, &b, B_TONE).await;

        // the chat tells it, live at a client that only watched, and after that client's restart
        let people = [a.mri.clone(), b.mri.clone()];
        let both = |line: &Line| line.ended_with([&people[0], &people[1]]);
        idle.pushed_line(seen, &chat, "Call started", started).await;
        idle.pushed_line(seen, &chat, "Call ended", both).await;
        idle.restart().await;
        for phone in [&mut a, &mut b, &mut idle] {
            phone.shows_lines(&chat, &[&started, &both]).await;
        }
        rig.no_open_calls().await;

        // the other way round, and the callee hangs up
        let back = connect(&mut b, &mut a, &chat).await;
        tokio::time::sleep(TALK).await;
        hang_up(&mut a, back.answered, &mut b, back.call).await;
        heard_each_other(&a, A_TONE, &b, B_TONE).await;
        a.shows_lines(&chat, &[&started, &both, &started, &both]).await;
        rig.no_open_calls().await;
        for phone in [&mut a, &mut b, &mut idle] {
            phone.close().await;
        }
        rig.stop().await;
    }
}

/// b declines a's call: a's call ends as declined.
async fn declined(a: &mut Phone, b: &mut Phone, chat: &str) {
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.to_owned() }).await;
    let rings = b.incoming(to).await;
    b.act(CallAction::Decline { call: rings.call }).await;
    let End::Remote(end) = a.ended(from, call).await else {
        panic!("a declined call ends as the far end's");
    };
    assert_eq!(end.code, Some(603), "{end:?}");
}

/// a cancels its call while it rings at b: b stops ringing.
async fn cancelled(a: &mut Phone, b: &mut Phone, chat: &str) {
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.to_owned() }).await;
    let rings = b.incoming(to).await;
    a.act(CallAction::HangUp { call }).await;
    assert_eq!(a.ended(from, call).await, End::HungUp);
    let end = b.ring_ended(to, rings.call).await;
    assert_eq!(end.code, Some(487), "{end:?}");
}

/// Nobody answers a's call: it ends at both ends once the ring timed out.
async fn unanswered(a: &mut Phone, b: &mut Phone, chat: &str) {
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.to_owned() }).await;
    let rings = b.incoming(to).await;
    let ring = b.ring_ended(to, rings.call).await;
    assert_eq!(ring.code, Some(487), "{ring:?}");
    assert_eq!(ring.phrase.as_deref(), Some("Call cancelled on timeout."));
    let End::Remote(end) = a.ended(from, call).await else {
        panic!("an unanswered call ends as the service's");
    };
    assert_eq!(end.code, Some(480), "{end:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declined_a_cancelled_and_an_unanswered_call_are_missed_calls_in_the_chat() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "missed", 5, Turn::Coturn).await else {
            return;
        };
        let (mut a, mut b, chat) = pair(&rig).await;
        let caller = a.mri.clone();
        let missed = |line: &Line| line.missed_from(&caller);
        let mut wanted: Vec<&dyn Fn(&Line) -> bool> = Vec::new();
        for case in 0..3 {
            let seen = b.mark();
            match case {
                0 => declined(&mut a, &mut b, &chat).await,
                1 => cancelled(&mut a, &mut b, &chat).await,
                _ => unanswered(&mut a, &mut b, &chat).await,
            }
            // live at the callee: the call's start, then its end with the caller alone ("Call missed")
            b.pushed_line(seen, &chat, "Call started", started).await;
            b.pushed_line(seen, &chat, "Call missed", missed).await;
            wanted.extend([&started as &dyn Fn(&Line) -> bool, &missed]);
            b.shows_lines(&chat, &wanted).await;
            rig.no_open_calls().await;
        }
        b.restart().await;
        b.shows_lines(&chat, &wanted).await;
        a.shows_lines(&chat, &wanted).await;
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}

/// a presses Call in `chat` and nobody can take it: how its call ended, or why the client didn't place it.
async fn call_nobody_takes(a: &mut Phone, chat: &str) -> Result<End, String> {
    let from = a.mark();
    let call = a.press(CallAction::Call { chat: chat.to_owned() }).await?;
    Ok(a.ended(from, call).await)
}

/// The server refused the call's start: the client ends it at its start, or doesn't place it.
fn refused(end: &Result<End, String>) -> bool {
    matches!(end, Ok(End::Failed("start")) | Err(_))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_to_someone_offline_disabled_or_deleted_ends_cleanly() {
    for backend in backends() {
        let Some(rig) = Rig::start(backend, "unreachable", 3, Turn::Coturn).await else {
            return;
        };
        let mut a = rig.phone(&rig.first, "a@own.test", "Ada", A_TONE).await;
        // b and c are signed in, with no client running
        let mut b = Phone::signed_in(rig.other(), &rig.origin, ("b@own.test", "Bert"), B_TONE).await;
        let c = Phone::signed_in(&rig.first, &rig.origin, ("c@own.test", "Cora"), B_TONE).await;
        let chat = a.chat_with(&b.mri).await;
        let gone = a.chat_with(&c.mri).await;

        let end = call_nobody_takes(&mut a, &chat).await;
        eprintln!("a call to an account with no client online: {end:?}");
        assert!(matches!(end, Ok(End::Remote(_))), "it rings out: {end:?}");
        rig.no_open_calls().await;
        // b's client, started later, shows the call it missed
        b.start();
        b.online(0).await;
        b.sync().await;
        let caller = a.mri.clone();
        b.shows_lines(&chat, &[&started, &|line| line.missed_from(&caller)])
            .await;
        b.close().await;

        rig.first.cli(&["user", "disable", "b@own.test"], "");
        let end = call_nobody_takes(&mut a, &chat).await;
        eprintln!("a call to a disabled account: {end:?}");
        assert!(refused(&end), "the server refuses it, it doesn't ring out: {end:?}");
        rig.first.cli(&["user", "delete", "c@own.test"], "");
        let end = call_nobody_takes(&mut a, &gone).await;
        eprintln!("a call to a deleted account: {end:?}");
        assert!(refused(&end), "the server refuses it, it doesn't ring out: {end:?}");
        rig.no_open_calls().await;

        // enabled again and signed in again, b takes a's next call
        rig.first.cli(&["user", "enable", "b@own.test"], "");
        let mut b = rig.phone(rig.other(), "b@own.test", "Bert", B_TONE).await;
        b.sync().await;
        let talk = connect(&mut a, &mut b, &chat).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        hang_up(&mut b, talk.answered, &mut a, talk.call).await;
        rig.no_open_calls().await;
        a.close().await;
        b.close().await;
        rig.stop().await;
    }
}
