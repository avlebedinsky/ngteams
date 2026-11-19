//! A 1:1 call's shared screen, on the mock and its loopback peer (`docs/protocol/calling.md`, "More of a running
//! call"): the far end's share answered receiving with `["Audio", "ScreenViewer"]` and shown on the sharing line's sink
//! until its offer rejects that line; the account's own Share telling its conversation it shares and renegotiating
//! with `["Audio", "ScreenSharer"]`, the far end shown its screen, and Stop sharing renegotiating it away. A group
//! call's share describes its sharing line instead, renegotiating nothing.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use serde_json::Value;

use crate::support::{
    A_TONE, Account, B_TONE, HeldCamera, Lab, PEER_TONE, actual_callback, renegotiated_one_at_a_time, wait_until,
};
use teams_core::{CallAction, CallId, CallStatus, End, Event};
use teams_media::sdp::NgcSdp;
use teams_mock::mixer::MixerPeer;
use teams_mock::{AB_CHAT, ABC_CHAT, LAB_A, LAB_B, LAB_C};

/// Frames of a shared screen a side must have shown.
const FRAMES: u64 = 15;

/// The next share state `account` is told for `call`: its own (`own`), or the far end's.
async fn share_told(account: &mut Account, call: CallId, own: bool) -> bool {
    account
        .next(|event| match event {
            Event::CallSharing { call: of, on } if own && *of == call => Some(*on),
            Event::CallRemoteShare { call: of, on } if !own && *of == call => Some(*on),
            _ => None,
        })
        .await
}

/// The `callModalities` of every renegotiation and renegotiation answer `mri` sent, in order.
async fn modalities(lab: &Lab, mri: &str) -> Vec<Vec<String>> {
    lab.mock
        .received_requests()
        .await
        .into_iter()
        .filter_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            let sent = body
                .pointer("/mediaNegotiation")
                .or_else(|| body.pointer("/mediaAnswer"))?;
            (sent.pointer("/sender/id")?.as_str()? == mri).then(|| {
                sent["callModalities"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|modality| modality.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .collect()
}

/// Waits until the last renegotiation or answer `mri` sent named `wanted` (its write may go a moment after the state is
/// told); fails after five seconds.
async fn last_modalities(lab: &Lab, mri: &str, wanted: &[&str]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let last = modalities(lab, mri).await.pop().unwrap_or_default();
        if last == wanted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{mri} last named {last:?}, not {wanted:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_s_screen_is_shown_on_its_sharing_line_until_it_stops() {
    let lab = Lab::start().await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;

    // the peer shares: b answers receiving, tells the share shown, and shows its frames apart from any camera's
    assert!(lab.mock.renegotiate_share(&id, LAB_B, true));
    assert!(share_told(&mut b, call, false).await, "the far end's share shown");
    wait_until(|| b.video.share.counts().indexed >= FRAMES).await;
    assert_eq!(
        b.video.remote.counts().frames,
        0,
        "no camera's frames: the far end's camera is off"
    );
    last_modalities(&lab, LAB_B, &["Audio", "ScreenViewer"]).await;

    // and stops: the line goes, so does the share
    assert!(lab.mock.renegotiate_share(&id, LAB_B, false));
    assert!(!share_told(&mut b, call, false).await, "the far end's share gone");
    last_modalities(&lab, LAB_B, &["Audio"]).await;

    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_end_shares_its_screen_and_the_other_is_shown_it() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let incoming = b.incoming().await;
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    b.status(answered, &CallStatus::Connected).await;

    // a shares: its conversation told first, then its renegotiation adds the sharing line, which b is shown
    let share = |on| CallAction::Share { call, on };
    assert_eq!(a.handle.requests().call(share(true)).await.unwrap(), call);
    assert!(share_told(&mut a, call, true).await, "a shares");
    assert!(share_told(&mut b, answered, false).await, "b is shown a's screen");
    wait_until(|| b.video.share.counts().indexed >= FRAMES).await;
    assert_eq!(b.video.remote.counts().frames, 0, "a's camera stays off");
    let sent = lab.sent_by(LAB_A);
    let at = |op: &str| sent.iter().position(|sent| sent == op);
    assert!(
        at("updateParticipantProperties").is_some() && at("updateParticipantProperties") < at("renegotiate"),
        "{sent:?}"
    );
    last_modalities(&lab, LAB_A, &["Audio", "ScreenSharer"]).await;
    last_modalities(&lab, LAB_B, &["Audio", "ScreenViewer"]).await;

    // and stops: the sharing line rejected, b's share gone
    assert_eq!(a.handle.requests().call(share(false)).await.unwrap(), call);
    assert!(!share_told(&mut a, call, true).await, "a no longer shares");
    assert!(!share_told(&mut b, answered, false).await, "b's share gone");
    last_modalities(&lab, LAB_A, &["Audio"]).await;

    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

/// A 1:1 call's native end shares, stops and shares again, as a user does: the far end is shown the second share's
/// frames too, not only told that it shares.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_native_end_shares_again_after_stopping_and_the_other_is_shown_it_again() {
    let lab = Lab::start().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let incoming = b.incoming().await;
    let answered = b
        .handle
        .requests()
        .call(CallAction::Answer { call: incoming.call })
        .await
        .unwrap();
    a.status(call, &CallStatus::Connected).await;
    b.status(answered, &CallStatus::Connected).await;

    let share = |on| CallAction::Share { call, on };
    let mut shown = 0;
    for round in 1..=3 {
        // taken at once: while the stop's renegotiation is still open, the share waits for it
        assert_eq!(a.handle.requests().call(share(true)).await.unwrap(), call);
        assert!(share_told(&mut a, call, true).await, "a shares ({round})");
        assert!(
            share_told(&mut b, answered, false).await,
            "b is told a's share ({round})"
        );
        let wanted = shown + FRAMES;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while b.video.share.counts().indexed < wanted {
            assert!(
                std::time::Instant::now() < deadline,
                "share {round}: b was shown {:?} of a's frames, not {wanted} indexed",
                b.video.share.counts()
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        shown = b.video.share.counts().indexed;
        last_modalities(&lab, LAB_A, &["Audio", "ScreenSharer"]).await;

        assert_eq!(a.handle.requests().call(share(false)).await.unwrap(), call);
        assert!(!share_told(&mut a, call, true).await, "a no longer shares ({round})");
        assert!(!share_told(&mut b, answered, false).await, "b's share gone ({round})");
        last_modalities(&lab, LAB_A, &["Audio"]).await;
    }

    a.handle.requests().call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(a.ended(call).await, End::HungUp);
}

/// The bodies of every `updateMediaDescriptions` the mock got, in order.
async fn descriptions(lab: &Lab) -> Vec<Value> {
    lab.mock
        .received_requests()
        .await
        .into_iter()
        .filter(|request| request.url.path().ends_with("/updateMediaDescriptions"))
        .filter_map(|request| serde_json::from_slice::<Value>(&request.body).ok())
        .map(|body| body["UpdateMediaDescriptions"]["mediaDescriptions"].clone())
        .collect()
}

/// The last `updateMediaDescriptions` describes the sharing line going `direction` (labelled while it sends), tagged
/// as the account's first share; waits for it up to five seconds.
async fn shares_described(lab: &Lab, direction: &str) -> Value {
    shares_described_as(lab, direction, 1).await
}

/// [`shares_described`], tagged as the account's share number `share`.
async fn shares_described_as(lab: &Lab, direction: &str, share: u32) -> Value {
    let tag = format!(";ss_{share}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let last = descriptions(lab).await.pop().unwrap_or_default();
        let sharing = last["descriptions"]
            .as_array()
            .and_then(|lines| lines.last())
            .cloned()
            .unwrap_or_default();
        let labelled = sharing.get("label").and_then(Value::as_str);
        let tagged = last["negotiationTag"]
            .as_str()
            .is_some_and(|written| written.ends_with(&tag));
        let wanted_label = (direction == "sendonly").then_some("applicationsharing-video");
        if sharing["direction"] == direction && labelled == wanted_label && tagged {
            return last;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "not described {direction}: {last}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// A group call's share (`docs/protocol/calling.md`, "The native client's share, group call or meeting"): a calls the
/// a–b–c chat (its own group offer), b and c answer the mixer's offers. b shares: its conversation told, then its
/// lines described with the sharing line `sendonly`, labelled and tagged, nothing renegotiated; Stop sharing describes
/// it `recvonly` with the same tag. a, whose sharing line its own offer made, shares the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_group_call_shares_on_its_sharing_line_by_describing_it_without_a_renegotiation() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let mut c = lab.account(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let (ringing_b, ringing_c) = (b.incoming().await, c.incoming().await);
    let mut answered = Vec::new();
    for (account, ringing) in [(&mut b, ringing_b.call), (&mut c, ringing_c.call)] {
        let call = account
            .handle
            .requests()
            .call(CallAction::Answer { call: ringing })
            .await
            .unwrap();
        account.status(call, &CallStatus::Connected).await;
        answered.push(call);
    }
    a.status(call_a, &CallStatus::Connected).await;
    let call_b = answered[0];

    let share = |call, on| CallAction::Share { call, on };
    assert_eq!(b.handle.requests().call(share(call_b, true)).await.unwrap(), call_b);
    assert!(share_told(&mut b, call_b, true).await, "b shares");
    shares_described(&lab, "sendonly").await;
    let sent = lab.sent_by(LAB_B);
    let at = |op: &str| sent.iter().position(|sent| sent == op);
    assert!(
        at("updateParticipantProperties").is_some()
            && at("updateParticipantProperties") < at("updateMediaDescriptions"),
        "{sent:?}"
    );
    assert!(!sent.contains(&"renegotiate".to_owned()), "{sent:?}");

    assert_eq!(b.handle.requests().call(share(call_b, false)).await.unwrap(), call_b);
    assert!(!share_told(&mut b, call_b, true).await, "b no longer shares");
    shares_described(&lab, "recvonly").await;

    assert_eq!(a.handle.requests().call(share(call_a, true)).await.unwrap(), call_a);
    assert!(
        share_told(&mut a, call_a, true).await,
        "a shares on its own offer's line"
    );
    shares_described(&lab, "sendonly").await;
    assert!(!lab.sent_by(LAB_A).contains(&"renegotiate".to_owned()));

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, answered[1])] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
    }
}

/// The mixer asks a sharer for its screen with a `controlVideoStreaming` notice, as a share starts and as each viewer
/// joins (`docs/protocol/calling.md`, "Keyframes of a share"): the sharer answers each at once, as Teams for the web
/// does, and its session makes a keyframe of the screen. Unanswered, the live mixer sent each notice again five times
/// and forwarded no picture to a viewer who joined.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sharer_answers_the_mixer_s_video_control_at_once() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let ringing = b.incoming().await;
    let call_b = b
        .handle
        .requests()
        .call(CallAction::Answer { call: ringing.call })
        .await
        .unwrap();
    b.status(call_b, &CallStatus::Connected).await;
    a.status(call_a, &CallStatus::Connected).await;
    let share = CallAction::Share { call: call_b, on: true };
    assert_eq!(b.handle.requests().call(share).await.unwrap(), call_b);
    assert!(share_told(&mut b, call_b, true).await, "b shares");

    let callback = actual_callback(
        &lab,
        "/callAcceptance/clientContentForMediaController/controlVideoStreaming",
    )
    .await;
    let notice = serde_json::json!({"controlVideoStreaming": {"sequenceNumber": 7,
        "globalTimeStamp": "11/19/2026 09:07:46", "controlInfo": [{"control": 0, "sourceId": 1003,
        "fmtParams": "max-fps=1500;max-br=955;max-mbps=135000;profile-level-id=42C02A;packetization-mode=1;max-fs=8160"}]},
        "debugContent": null});
    let id = lab.trouter.deliver_call(LAB_B, &callback, &notice);
    let answered = |id: i64| {
        lab.trouter
            .sockets()
            .iter()
            .filter(|socket| socket.account.as_deref() == Some(LAB_B))
            .flat_map(|socket| socket.frames.clone())
            .filter_map(|frame| serde_json::from_str::<Value>(frame.strip_prefix("3:::")?).ok())
            .any(|frame| frame["id"] == id && frame["status"] == 200 && frame["body"] == "")
    };
    wait_until(|| answered(id)).await;

    for (account, call) in [(&mut a, call_a), (&mut b, call_b)] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
    }
}

/// As in Teams, a share that starts while the account shares in a group call takes over: b shares, then the roster
/// lists c sharing; b's share stops, told and described `recvonly` again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_another_starts_in_a_group_call_takes_over_the_account_s() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account(LAB_A, A_TONE).await;
    let mut b = lab.account(LAB_B, B_TONE).await;
    let mut c = lab.account(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let (ringing_b, ringing_c) = (b.incoming().await, c.incoming().await);
    let mut answered = Vec::new();
    for (account, ringing) in [(&mut b, ringing_b.call), (&mut c, ringing_c.call)] {
        let call = account
            .handle
            .requests()
            .call(CallAction::Answer { call: ringing })
            .await
            .unwrap();
        account.status(call, &CallStatus::Connected).await;
        answered.push(call);
    }
    a.status(call_a, &CallStatus::Connected).await;
    let (call_b, call_c) = (answered[0], answered[1]);
    let share = CallAction::Share { call: call_b, on: true };
    assert_eq!(b.handle.requests().call(share).await.unwrap(), call_b);
    assert!(share_told(&mut b, call_b, true).await, "b shares");
    shares_described(&lab, "sendonly").await;

    let id = lab.mock.calls().last().unwrap().id.clone();
    assert!(lab.mock.share_made_up(&id, LAB_C, true));
    assert!(!share_told(&mut b, call_b, true).await, "c's share took over b's");
    shares_described(&lab, "recvonly").await;

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        assert_eq!(account.ended(call).await, End::HungUp);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_without_video_shares_nothing() {
    let lab = Lab::start().await;
    let mut b = lab.account_without_video(LAB_B, B_TONE).await;
    let _id = lab.mock.ring(LAB_C, LAB_B);
    let incoming = b.incoming().await;
    let requests = b.handle.requests();
    let call = requests.call(CallAction::Answer { call: incoming.call }).await.unwrap();
    b.status(call, &CallStatus::Connected).await;
    assert!(requests.call(CallAction::Share { call, on: true }).await.is_err());
    assert!(!lab.sent_by(LAB_B).contains(&"renegotiate".to_owned()));
    requests.call(CallAction::HangUp { call }).await.unwrap();
    assert_eq!(b.ended(call).await, End::HungUp);
}

/// A 1:1 call a → b, both native and connected; a's first camera opens only once the test lets it (`release`, told at
/// `asked` when it is asked): a camera switch's renegotiation of a's held open.
struct HeldCall {
    lab: Lab,
    a: Account,
    b: Account,
    /// a's call.
    call: CallId,
    /// b's call.
    answered: CallId,
    asked: Option<Receiver<()>>,
    release: Sender<()>,
}

impl HeldCall {
    /// a calls b, b answers.
    async fn start() -> Self {
        Self::placed(true).await
    }

    /// b calls a, a answers: a's answer hands out the callback where a's call takes the service's offers.
    async fn answered_by_a() -> Self {
        Self::placed(false).await
    }

    /// The 1:1 call between a, whose camera is held, and b: placed by a (`by_a`) or by b.
    async fn placed(by_a: bool) -> Self {
        let lab = Lab::start().await;
        let (devices, asked, release) = HeldCamera::new();
        let mut a = lab.account_with_devices(devices).await;
        let mut b = lab.account(LAB_B, B_TONE).await;
        let (placing, answering) = if by_a { (&mut a, &mut b) } else { (&mut b, &mut a) };
        let placed = placing
            .handle
            .requests()
            .call(CallAction::Call { chat: AB_CHAT.into() })
            .await
            .unwrap();
        let incoming = answering.incoming().await;
        let answered = answering
            .handle
            .requests()
            .call(CallAction::Answer { call: incoming.call })
            .await
            .unwrap();
        placing.status(placed, &CallStatus::Connected).await;
        answering.status(answered, &CallStatus::Connected).await;
        let (call, answered) = if by_a { (placed, answered) } else { (answered, placed) };
        Self {
            lab,
            a,
            b,
            call,
            answered,
            asked: Some(asked),
            release,
        }
    }

    /// a's own action on the call.
    async fn act(&self, action: CallAction) -> Result<CallId, teams_core::EngineError> {
        self.a.handle.requests().call(action).await
    }

    /// a turns its camera on, which stays open until [`Self::release`]: its renegotiation is open meanwhile.
    async fn camera_held_on(&mut self) -> tokio::task::JoinHandle<Result<CallId, teams_core::EngineError>> {
        let (requests, call) = (self.a.handle.requests(), self.call);
        let on = tokio::spawn(async move { requests.call(CallAction::Camera { call, on: true }).await });
        let asked = self.asked.take().expect("the camera is held once");
        tokio::task::spawn_blocking(move || asked.recv_timeout(Duration::from_secs(30)))
            .await
            .unwrap()
            .expect("the camera was asked to open");
        on
    }

    /// The held camera opens: its renegotiation goes and settles.
    async fn release(&self, on: tokio::task::JoinHandle<Result<CallId, teams_core::EngineError>>) {
        self.release.send(()).unwrap();
        assert_eq!(on.await.unwrap().unwrap(), self.call);
    }

    /// Waits until a sent `count` renegotiations, each one acknowledged before the next went, and the last one too.
    async fn renegotiated(&self, count: usize) {
        wait_until(|| {
            renegotiated_one_at_a_time(&self.lab, LAB_A) == count
                && self.lab.sent_by(LAB_A).last().map(String::as_str) == Some("acknowledge")
        })
        .await;
    }

    async fn hang_up(mut self) {
        let call = self.call;
        self.act(CallAction::HangUp { call }).await.unwrap();
        assert_eq!(self.a.ended(call).await, End::HungUp);
    }
}

/// Share picked while the call's own camera renegotiation is open: as in Teams the share is taken, not refused, and
/// its offer goes once that one settled, never two offers at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_started_while_its_own_renegotiation_is_open_goes_once_that_settled() {
    let mut held = HeldCall::start().await;
    let call = held.call;
    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    held.release(on).await;

    assert!(share_told(&mut held.a, call, true).await, "a shares");
    assert!(
        share_told(&mut held.b, held.answered, false).await,
        "b is shown a's screen"
    );
    wait_until(|| held.b.video.share.counts().indexed >= FRAMES).await;
    held.renegotiated(2).await;
    held.hang_up().await;
}

/// Stop sharing while the call's own camera renegotiation is open: taken, not refused, and the share stops with a
/// renegotiation of its own once that one settled.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_stopped_while_its_own_renegotiation_is_open_stops_once_that_settled() {
    let mut held = HeldCall::start().await;
    let call = held.call;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(share_told(&mut held.a, call, true).await, "a shares");
    assert!(share_told(&mut held.b, held.answered, false).await, "b is shown it");
    held.renegotiated(1).await;

    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Share { call, on: false }).await.unwrap(), call);
    held.release(on).await;

    assert!(!share_told(&mut held.a, call, true).await, "a no longer shares");
    assert!(!share_told(&mut held.b, held.answered, false).await, "b's share gone");
    held.renegotiated(3).await;
    held.hang_up().await;
}

/// A camera switch and a share both taken while the call's own camera renegotiation is open: they go in turn once it
/// settled, one offer at a time, and the call ends as the later switches say (the camera off, the screen shared).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_camera_switch_and_a_share_both_waiting_go_in_turn_and_end_as_asked() {
    let mut held = HeldCall::start().await;
    let call = held.call;
    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Camera { call, on: false }).await.unwrap(), call);
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    held.release(on).await;

    assert!(share_told(&mut held.a, call, true).await, "a shares");
    assert!(
        share_told(&mut held.b, held.answered, false).await,
        "b is shown a's screen"
    );
    held.renegotiated(3).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (camera, screen) = (held.b.video.remote.counts().frames, held.b.video.share.counts().indexed);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(held.b.video.remote.counts().frames, camera, "a's camera ended off");
    assert!(
        held.b.video.share.counts().indexed > screen,
        "a's screen goes on: {:?}",
        held.b.video.share.counts()
    );
    held.hang_up().await;
}

/// Share, then Stop sharing, both while the call's own camera renegotiation is open: the later switch decides, so
/// nothing is shared and no renegotiation goes for either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_started_and_stopped_while_a_renegotiation_is_open_shares_nothing() {
    let mut held = HeldCall::start().await;
    let call = held.call;
    let on = held.camera_held_on().await;
    for on in [true, false] {
        assert_eq!(held.act(CallAction::Share { call, on }).await.unwrap(), call);
    }
    held.release(on).await;

    held.renegotiated(1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(renegotiated_one_at_a_time(&held.lab, LAB_A), 1);
    assert_eq!(held.b.video.share.counts().indexed, 0, "b was shown no screen");
    held.hang_up().await;
}

/// A share switch that waits for the call's own camera renegotiation is told at once as the share will go, as Teams'
/// toolbar shows it: Stop sharing in Share's place while the camera still opens, and Share again once a stop drops the
/// waiting start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_switch_that_waits_is_told_at_once_as_the_share_will_go() {
    let mut held = HeldCall::start().await;
    let call = held.call;
    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(
        share_told(&mut held.a, call, true).await,
        "told sharing while the camera still opens"
    );
    assert_eq!(held.act(CallAction::Share { call, on: false }).await.unwrap(), call);
    assert!(
        !share_told(&mut held.a, call, true).await,
        "told not sharing: the waiting start goes no more"
    );
    held.release(on).await;

    held.renegotiated(1).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(renegotiated_one_at_a_time(&held.lab, LAB_A), 1);
    held.hang_up().await;
}

/// The service's new mixer session for a's call, a mixer of its own offering it (`newOffer`, `fromMixer`) while a's
/// held camera keeps a renegotiation open: the offer waits until the camera opens (`on`) and its renegotiation
/// settled, then a's call sets its media up again and answers it, and the mixer takes that answer. The mixer, kept for
/// the rest of the test, and the `callModalities` of a's answer.
async fn new_session(
    held: &HeldCall,
    on: tokio::task::JoinHandle<Result<CallId, teams_core::EngineError>>,
) -> (MixerPeer, Vec<String>) {
    let mixer = MixerPeer::start_at(held.lab.relay, 80_000, PEER_TONE).unwrap();
    let offer = mixer.offer().unwrap();
    let callback = actual_callback(&held.lab, "/callAcceptance/links/mediaRenegotiation").await;
    let call_id = held.lab.last_call();
    let answer_path = format!("/api/v2/ep/cc-zz-01-prod-aks.cc.skype.com/cc/v1/negotiations/{call_id}/1/t/9/answer");
    held.lab
        .mock
        .answer_next("POST", &answer_path, 200, &serde_json::json!({}))
        .await;
    let negotiation = serde_json::json!({"mediaNegotiation":{
        "mediaContent":{"blob":offer.as_str(),"contentType":offer.content_type(),"newOffer":true,"fromMixer":true},
        "links":{"mediaAnswer":format!("https://api.flightproxy.teams.microsoft.com{answer_path}?i=10-0-0-1")}
    }});
    let _ = held.lab.trouter.deliver_call(LAB_A, &callback, &negotiation);
    held.release(on).await;
    held.lab.wait_attempts(&answer_path, 1).await;
    let answer = held
        .lab
        .mock
        .received_requests()
        .await
        .into_iter()
        .find_map(|request| {
            if request.url.path() != answer_path {
                return None;
            }
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            body.get("mediaAnswer").cloned()
        })
        .expect("a answered the new session");
    let content = &answer["mediaContent"];
    let sdp = NgcSdp::new(
        content["contentType"].as_str().unwrap(),
        content["blob"].as_str().unwrap(),
    )
    .unwrap();
    mixer.accept_answer(&sdp).unwrap();
    let modalities = answer["callModalities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|modality| modality.as_str().map(str::to_owned))
        .collect();
    (mixer, modalities)
}

/// Share picked while the call's camera renegotiation is open, and the service sets the call's media up again
/// meanwhile: the share that waited is not lost with the old session, it goes out on the new session's sharing line,
/// described `sendonly` as a group call's share is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_waiting_while_the_media_is_set_up_again_goes_on_the_new_session() {
    let mut held = HeldCall::answered_by_a().await;
    let call = held.call;
    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    let (_mixer, answered) = new_session(&held, on).await;
    assert!(
        !answered.iter().any(|modality| modality == "ScreenSharer"),
        "{answered:?}"
    );

    assert!(
        share_told(&mut held.a, call, true).await,
        "told sharing while the camera still opens"
    );
    shares_described(&held.lab, "sendonly").await;
    let flipped: Vec<_> = held
        .a
        .events_within(Duration::from_millis(500))
        .await
        .into_iter()
        .filter(|event| matches!(event, Event::CallSharing { call: of, on: false } if *of == call))
        .collect();
    assert!(flipped.is_empty(), "the share stays told as on: {flipped:?}");
    held.hang_up().await;
}

/// a shares in the 1:1 call, and the service sets the call's media up again (as it does when someone is added): as in
/// Teams the share goes on, its screen out on the new session's sharing line, described `sendonly` as a group call's
/// share is (told to the conversation again, as share 2), and a is never told it stopped. Before, the share ended with
/// the old session, and the one added saw none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_share_going_while_the_media_is_set_up_again_goes_on_the_new_session() {
    let mut held = HeldCall::answered_by_a().await;
    let call = held.call;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(share_told(&mut held.a, call, true).await, "a shares");
    held.renegotiated(1).await;

    let on = held.camera_held_on().await;
    let (_mixer, _) = new_session(&held, on).await;
    shares_described_as(&held.lab, "sendonly", 2).await;
    let flipped: Vec<_> = held
        .a
        .events_within(Duration::from_millis(500))
        .await
        .into_iter()
        .filter(|event| matches!(event, Event::CallSharing { call: of, on: false } if *of == call))
        .collect();
    assert!(flipped.is_empty(), "the share stays told as on: {flipped:?}");

    // Stop sharing ends it on the new session
    assert_eq!(held.act(CallAction::Share { call, on: false }).await.unwrap(), call);
    assert!(!share_told(&mut held.a, call, true).await, "a no longer shares");
    shares_described_as(&held.lab, "recvonly", 2).await;
    held.hang_up().await;
}

/// a shares in the 1:1 call, the service sets the call's media up again, and a stops sharing at once, while the kept
/// screen comes or starts to go out on the new session: the share ends, and the new session's sharing line does not
/// stay described `sendonly`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_the_kept_screen_comes_ends_the_share() {
    let mut held = HeldCall::answered_by_a().await;
    let call = held.call;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(share_told(&mut held.a, call, true).await, "a shares");
    held.renegotiated(1).await;

    let on = held.camera_held_on().await;
    let (_mixer, _) = new_session(&held, on).await;
    assert_eq!(held.act(CallAction::Share { call, on: false }).await.unwrap(), call);
    assert!(!share_told(&mut held.a, call, true).await, "a no longer shares");
    tokio::time::sleep(Duration::from_secs(1)).await;
    let last = descriptions(&held.lab).await.pop().unwrap_or_default();
    let sharing = last["descriptions"].as_array().and_then(|lines| lines.last());
    assert!(
        sharing.is_none_or(|line| line["direction"] != "sendonly"),
        "the stopped share is still described: {last}"
    );
    held.hang_up().await;
}

/// Stop sharing while the call's camera renegotiation is open, and the service sets the call's media up again
/// meanwhile: the new session shares nothing, so a is told it no longer shares and its answer names no
/// `ScreenSharer`. Share then goes again, on the new session's sharing line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_waiting_while_the_media_is_set_up_again_ends_the_share() {
    let mut held = HeldCall::answered_by_a().await;
    let call = held.call;
    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(share_told(&mut held.a, call, true).await, "a shares");
    held.renegotiated(1).await;

    let on = held.camera_held_on().await;
    assert_eq!(held.act(CallAction::Share { call, on: false }).await.unwrap(), call);
    let (_mixer, answered) = new_session(&held, on).await;
    assert!(!share_told(&mut held.a, call, true).await, "a no longer shares");
    assert!(
        !answered.iter().any(|modality| modality == "ScreenSharer"),
        "{answered:?}"
    );

    assert_eq!(held.act(CallAction::Share { call, on: true }).await.unwrap(), call);
    assert!(share_told(&mut held.a, call, true).await, "a shares again");
    held.hang_up().await;
}

/// The far end shares in the 1:1 call, and the service sets the call's media up again: the new session shows no share
/// yet, so a is told the far end's share gone and its answer names no `ScreenViewer`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_far_end_s_share_shown_when_the_media_is_set_up_again_is_told_gone() {
    let mut held = HeldCall::answered_by_a().await;
    let (call, answered) = (held.call, held.answered);
    let share = CallAction::Share {
        call: answered,
        on: true,
    };
    assert_eq!(held.b.handle.requests().call(share).await.unwrap(), answered);
    assert!(share_told(&mut held.b, answered, true).await, "b shares");
    assert!(share_told(&mut held.a, call, false).await, "a is shown b's screen");

    let on = held.camera_held_on().await;
    let (_mixer, modalities) = new_session(&held, on).await;
    assert!(!share_told(&mut held.a, call, false).await, "b's share gone for a");
    assert!(
        !modalities.iter().any(|modality| modality == "ScreenViewer"),
        "{modalities:?}"
    );
    held.hang_up().await;
}
