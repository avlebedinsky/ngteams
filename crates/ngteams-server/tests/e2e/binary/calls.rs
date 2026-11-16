//! 1:1 calls as two or three ngteams clients make them: real server processes (one node on SQLite, two on Postgres
//! and Redis), a real coturn the nodes hold the secret of, accounts the operator added, and each account's client as
//! the app runs it (session, engine with its calling stack and push channels, media on the file devices). Skipped
//! without `NGTS_TEST_COTURN` and `NGTS_TEST_COTURN_SECRET` (`docs/server.md`).

mod access;
mod accounts;
mod call_log;
mod coturn_log;
mod flows;
mod group;
mod group_video;
mod nodes;
mod own_relay;
mod qa_net;
mod relay;
mod support;

use teams_core::{CallAction, CallId, CallStatus, CallView, End};

use self::support::{A_TONE, B_TONE, FRAMES, HEARD, Phone, Rig, TALK, wait_until};

/// a's client at the first node and b's at the other one, with their chat (a made it, b has it).
async fn pair(rig: &Rig) -> (Phone, Phone, String) {
    let a = rig.phone(&rig.first, "a@own.test", "Ada", A_TONE).await;
    let b = rig.phone(rig.other(), "b@own.test", "Bert", B_TONE).await;
    let chat = a.chat_with(&b.mri).await;
    b.sync().await;
    (a, b, chat)
}

/// A call that is up: the caller's id of it and the callee's.
#[derive(Clone, Copy)]
struct Talk {
    call: CallId,
    answered: CallId,
}

fn shows(who: &str) -> impl Fn(&CallView) -> bool + '_ {
    move |view| view.participants.iter().any(|party| party.mri == who)
}

/// `caller` calls in `chat`, `answerer` answers: both are connected, with media, and see each other.
async fn connect(caller: &mut Phone, answerer: &mut Phone, chat: &str) -> Talk {
    let (placed, rang) = (caller.mark(), answerer.mark());
    let call = caller.act(CallAction::Call { chat: chat.to_owned() }).await;
    caller.status(placed, call, &CallStatus::Ringing).await;
    let rings = answerer.incoming(rang).await;
    assert_eq!(rings.caller.as_str(), caller.mri);
    let taken = answerer.act(CallAction::Answer { call: rings.call }).await;
    answerer.status(rang, taken, &CallStatus::Connected).await;
    caller.status(placed, call, &CallStatus::Connected).await;
    let (caller_mri, answerer_mri) = (caller.mri.clone(), answerer.mri.clone());
    caller
        .view(placed, call, "the answerer in the call", shows(&answerer_mri))
        .await;
    answerer
        .view(rang, taken, "the caller in the call", shows(&caller_mri))
        .await;
    Talk { call, answered: taken }
}

/// `leaver` hangs up its `left` call: it ends there as hung up, and at `other` as ended by the far end.
async fn hang_up(leaver: &mut Phone, left: CallId, other: &mut Phone, others: CallId) {
    let (leaver_seen, other_seen) = (leaver.mark(), other.mark());
    leaver.act(CallAction::HangUp { call: left }).await;
    assert_eq!(leaver.ended(leaver_seen, left).await, End::HungUp);
    let end = other.ended(other_seen, others).await;
    assert!(matches!(end, End::Remote(_)), "{end:?}");
}

/// Windows of [`TALK`] a check of the far tone waits on after the first, while a listener hears too little.
const DIP_WINDOWS: u32 = 3;

/// How loud each `(listener, far tone)` heard that tone in the last [`TALK`]. While one heard it at [`HEARD`] or less,
/// measured again over the next [`TALK`], up to [`DIP_WINDOWS`] times: a heavily loaded host starves the media for
/// seconds, which passes; media that isn't there stays away.
async fn loudness(sides: &[(&Phone, teams_media::tone::Tone)]) -> Vec<f32> {
    let measure = || {
        sides
            .iter()
            .map(|(listener, tone)| listener.heard(*tone, TALK))
            .collect::<Vec<_>>()
    };
    let mut heard = measure();
    for _ in 0..DIP_WINDOWS {
        if heard.iter().all(|level| *level > HEARD) {
            break;
        }
        eprintln!("a dip ({heard:.3?}): measured again over the next {TALK:?}");
        tokio::time::sleep(TALK).await;
        heard = measure();
    }
    heard
}

/// Each side's speaker played the other's tone in the last [`TALK`] of the call, or in a later one after a loaded
/// host's dip ([`loudness`]).
async fn heard_each_other(a: &Phone, a_tone: teams_media::tone::Tone, b: &Phone, b_tone: teams_media::tone::Tone) {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let heard = loudness(&[(a, b_tone), (b, a_tone)]).await;
    let (a_heard, b_heard) = (heard[0], heard[1]);
    eprintln!(
        "{} heard {} at {a_heard:.3}, {} heard {} at {b_heard:.3}",
        a.upn, b.upn, b.upn, a.upn
    );
    assert!(
        a_heard > HEARD && b_heard > HEARD,
        "by half seconds, {} heard {:.2?} and {} heard {:.2?}",
        a.upn,
        a.levels(b_tone),
        b.upn,
        b.levels(a_tone)
    );
}

/// The camera of `sender`'s call goes on: `viewer` sees its picture.
async fn camera_shows(sender: &Phone, call: CallId, viewer: &Phone) {
    let before = viewer.video.remote.counts().frames;
    sender.act(CallAction::Camera { call, on: true }).await;
    wait_until("the far end's video", || {
        viewer.video.remote.counts().frames >= before + FRAMES
    })
    .await;
}
