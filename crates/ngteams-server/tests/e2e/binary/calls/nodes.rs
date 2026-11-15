//! Two nodes: a's client at one, b's at the other, and a node restarting while a call rings or is up.

use teams_core::{CallAction, CallStatus, End};

use super::support::{A_TONE, B_TONE, Line, Rig, TALK, Turn};
use super::{connect, hang_up, heard_each_other, pair, shows};
use crate::support::Backend;

/// The two-node rig, when the run has a Postgres and a coturn.
async fn two_nodes(flow: &str, ring_timeout: u64) -> Option<Rig> {
    if std::env::var_os(crate::support::PG_ENV).is_none() {
        eprintln!("skipped: two nodes need {}", crate::support::PG_ENV);
        return None;
    }
    let rig = Rig::start(Backend::Postgres, flow, ring_timeout, Turn::Coturn).await?;
    assert!(rig.two_nodes());
    Some(rig)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ring_outlives_a_restart_of_the_callees_node_and_is_answered() {
    let Some(mut rig) = two_nodes("ring-restart-answer", 40).await else {
        return;
    };
    let (mut a, mut b, chat) = pair(&rig).await;
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.clone() }).await;
    a.status(from, call, &CallStatus::Ringing).await;
    let rings = b.incoming(to).await;
    // the node b's client is at restarts: its sockets drop and come back
    let back = b.mark();
    rig.stop_node(false).await;
    rig.start_node(false).await;
    b.online(back).await;
    let answered = b.act(CallAction::Answer { call: rings.call }).await;
    b.status(to, answered, &CallStatus::Connected).await;
    a.status(from, call, &CallStatus::Connected).await;
    tokio::time::sleep(TALK).await;
    hang_up(&mut a, call, &mut b, answered).await;
    heard_each_other(&a, A_TONE, &b, B_TONE).await;
    rig.no_open_calls().await;
    a.close().await;
    b.close().await;
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_ring_nobody_answers_ends_at_both_ends_though_the_callers_node_restarted() {
    let Some(mut rig) = two_nodes("ring-restart-timeout", 12).await else {
        return;
    };
    let (mut a, mut b, chat) = pair(&rig).await;
    let (from, to) = (a.mark(), b.mark());
    let call = a.act(CallAction::Call { chat: chat.clone() }).await;
    a.status(from, call, &CallStatus::Ringing).await;
    let rings = b.incoming(to).await;
    // the node a's client is at restarts while it rings
    let back = a.mark();
    rig.stop_node(true).await;
    rig.start_node(true).await;
    a.online(back).await;
    let ring_end = b.ring_ended(to, rings.call).await;
    assert_eq!(ring_end.code, Some(487), "{ring_end:?}");
    let end = a.ended(from, call).await;
    assert!(matches!(end, End::Remote(_)), "{end:?}");
    rig.no_open_calls().await;
    let caller = a.mri.clone();
    b.shows_lines(
        &chat,
        &[&|line| *line == Line::Started, &|line| line.missed_from(&caller)],
    )
    .await;
    a.close().await;
    b.close().await;
    rig.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_call_goes_on_through_a_restart_of_each_node_and_is_hung_up_after() {
    let Some(mut rig) = two_nodes("call-restart", 30).await else {
        return;
    };
    let (mut a, mut b, chat) = pair(&rig).await;
    let talk = connect(&mut a, &mut b, &chat).await;
    for first in [true, false] {
        let (from, to) = (a.mark(), b.mark());
        rig.stop_node(first).await;
        rig.start_node(first).await;
        if first {
            a.online(from).await;
        } else {
            b.online(to).await;
        }
    }
    // the media never went through a node: both still hear each other, and nobody's call ended
    tokio::time::sleep(TALK + TALK).await;
    for phone in [&a, &b] {
        let over = phone.log.iter().any(|event| {
            matches!(
                event,
                teams_core::engine::Event::Call {
                    status: CallStatus::Ended(_),
                    ..
                }
            )
        });
        assert!(!over, "{}: the call ended over a node's restart", phone.upn);
    }
    // signaling works again: b's mute shows at a, and a's hang-up ends the call for b
    let (from, b_mri) = (a.mark(), b.mri.clone());
    let muted = CallAction::Mute {
        call: talk.answered,
        muted: true,
    };
    b.act(muted).await;
    a.view(from, talk.call, "b muted after the restarts", |view| {
        shows(&b_mri)(view) && view.participants.iter().any(|party| party.muted)
    })
    .await;
    hang_up(&mut a, talk.call, &mut b, talk.answered).await;
    assert!(b.heard(A_TONE, TALK) > super::support::HEARD, "b heard a to the end");
    rig.no_open_calls().await;
    let people = [a.mri.clone(), b.mri.clone()];
    b.shows_lines(
        &chat,
        &[&|line| *line == Line::Started, &|line| {
            line.ended_with([&people[0], &people[1]])
        }],
    )
    .await;
    a.close().await;
    b.close().await;
    rig.stop().await;
}
