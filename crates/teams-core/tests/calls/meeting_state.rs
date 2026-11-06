//! A meeting's timer set by one participant reaches everyone in the call, on the mock (its `updateMeetingStates`
//! answered and the conversation's update delivered to everyone's `conversationUpdate` callback).

use crate::support::{A_TONE, Account, B_TONE, Lab};
use teams_core::{CallAction, CallId, CallStatus, Event};
use teams_mock::{ABC_CHAT, LAB_A, LAB_B, LAB_C};
use teams_proto::{MeetingTimer, TimerState};

/// The next timer `account` is told for `call` (`None`: no timer shows).
async fn timer(account: &mut Account, call: CallId) -> Option<MeetingTimer> {
    account
        .next(|event| match event {
            Event::CallMeeting { call: of, state } if *of == call => Some(state.timer.clone()),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timer_set_by_one_participant_shows_to_everyone_in_the_call() {
    let lab = Lab::start_group_video().await;
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
    a.status(call_a, &CallStatus::Connected).await;
    let started = MeetingTimer {
        state: TimerState::InProgress,
        remaining_ms: 180_000,
        original_ms: 180_000,
        edited_ms: None,
        previous: None,
    };
    a.handle
        .requests()
        .call(CallAction::Timer {
            call: call_a,
            timer: started,
        })
        .await
        .unwrap();
    for (account, call) in [(&mut a, call_a), (&mut b, answered[0])] {
        let told = timer(account, call).await.expect("a timer shows");
        assert_eq!(told.state, TimerState::InProgress);
        assert_eq!(told.original_ms, 180_000);
        assert!(told.edited_ms.is_some(), "the service's edit time came with it");
    }
    assert!(lab.sent_by(LAB_A).iter().any(|sent| sent == "updateMeetingStates"));
    // a chat's call is organized by whoever started it: b may not lock it
    let locked = b
        .handle
        .requests()
        .call(CallAction::LockMeeting {
            call: answered[0],
            locked: true,
        })
        .await;
    assert!(locked.is_err(), "{locked:?}");
    for (account, call) in [(&mut a, call_a), (&mut b, answered[0]), (&mut c, answered[1])] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        account.ended(call).await;
    }
}
