//! The organizer's controls of a group call on the mock: Mute all mutes everyone else (each told the organizer muted
//! them, the organizer sees them muted), and Controls' options read and changed on the middle tier, everyone told
//! (who presents makes the others attendees). Nobody else may do either.

use crate::support::{A_TONE, Account, B_TONE, Lab};
use teams_core::{CallAction, CallId, CallStatus, Event, MeetingState};
use teams_mock::{ABC_CHAT, LAB_A, LAB_B, LAB_C};
use teams_proto::MeetingOption;

/// The next meeting state `account` is told for `call` that `wanted` takes.
async fn meeting(account: &mut Account, call: CallId, wanted: impl Fn(&MeetingState) -> bool) -> MeetingState {
    account
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call && wanted(state) => Some(state.clone()),
            _ => None,
        })
        .await
}

/// a's group call in the a-b-c chat, answered by b and c: a's call, b's and c's.
async fn group_call(lab: &Lab) -> ((Account, CallId), (Account, CallId), (Account, CallId)) {
    let mut a = lab.account_without_video(LAB_A, A_TONE).await;
    let mut b = lab.account_without_video(LAB_B, B_TONE).await;
    let mut c = lab.account_without_video(LAB_C, crate::support::PEER_TONE).await;
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
    // up, and a organizes it (it started it), as its roster says: told in either order
    let (mut up, mut organizer) = (false, false);
    a.next(|event| {
        match event {
            Event::Call { call, status } if *call == call_a && *status == CallStatus::Connected => up = true,
            Event::CallOwn { organizer: true, .. } => organizer = true,
            _ => {}
        }
        (up && organizer).then_some(())
    })
    .await;
    ((a, call_a), (b, answered[0]), (c, answered[1]))
}

async fn hang_up(accounts: [(&mut Account, CallId); 3]) {
    for (account, call) in accounts {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        account.ended(call).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mute_all_mutes_everyone_but_the_organizer() {
    let lab = Lab::start_group_video().await;
    let ((mut a, call_a), (mut b, call_b), (mut c, call_c)) = group_call(&lab).await;
    // only the organizer mutes everyone
    let refused = b.handle.requests().call(CallAction::MuteAll { call: call_b }).await;
    assert!(refused.is_err(), "{refused:?}");
    a.handle
        .requests()
        .call(CallAction::MuteAll { call: call_a })
        .await
        .unwrap();
    for (account, call) in [(&mut b, call_b), (&mut c, call_c)] {
        let state = meeting(account, call, |state| state.muted_by_organizer.is_some()).await;
        assert!(!state.attendee);
    }
    a.view(call_a, |view| {
        view.participants.len() == 2 && view.participants.iter().all(|person| person.muted)
    })
    .await;
    assert!(lab.sent_by(LAB_A).iter().any(|sent| sent == "mute"));
    hang_up([(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn controls_read_and_change_the_meeting_options_and_everyone_is_told() {
    let lab = Lab::start_group_video().await;
    let ((mut a, call_a), (mut b, call_b), (mut c, call_c)) = group_call(&lab).await;
    let read = a.handle.requests().call_meeting_options(call_a).await.unwrap().unwrap();
    assert_eq!(
        (read.lobby, read.present, read.chat, read.unmute, read.camera),
        (Some(true), Some(true), Some(true), Some(true), Some(true))
    );
    // nobody else reads or changes them
    assert_eq!(b.handle.requests().call_meeting_options(call_b).await.unwrap(), None);
    let refused = b
        .handle
        .requests()
        .call(CallAction::MeetingOption {
            call: call_b,
            option: MeetingOption::Present(false),
        })
        .await;
    assert!(refused.is_err(), "{refused:?}");
    for option in [
        MeetingOption::Present(false),
        MeetingOption::Attendees {
            unmute: false,
            camera: false,
        },
    ] {
        a.handle
            .requests()
            .call(CallAction::MeetingOption { call: call_a, option })
            .await
            .unwrap();
    }
    // b is an attendee now, kept from presenting, unmuting and its camera
    let state = meeting(&mut b, call_b, |state| {
        state.attendee && state.options.unmute == Some(false) && state.options.camera == Some(false)
    })
    .await;
    assert!(state.share_blocked() && state.unmute_blocked() && state.camera_blocked());
    let read = a.handle.requests().call_meeting_options(call_a).await.unwrap().unwrap();
    assert_eq!(
        (read.present, read.unmute, read.camera),
        (Some(false), Some(false), Some(false))
    );
    hang_up([(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)]).await;
}
