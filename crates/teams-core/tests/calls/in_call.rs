//! What each participant of a group call tells the others while it runs, on the mock (its roster delivered to everyone
//! in the call, its live reactions to everyone's `receiveMessage` callback): a mute told to the call's conversation
//! shows b muted to a; b's raised hand shows to a and to b itself, and goes once b lowers it; c's live reaction reaches
//! everyone, c included.

use crate::support::{A_TONE, Account, B_TONE, Lab, PEER_TONE};
use teams_core::{CallAction, CallId, CallStatus, Event};
use teams_mock::{AB_CHAT, ABC_CHAT, LAB_A, LAB_B, LAB_C};

/// `account` answers `ringing` and is connected: its call.
async fn answer(account: &mut Account, ringing: CallId) -> CallId {
    let call = account
        .handle
        .requests()
        .call(CallAction::Answer { call: ringing })
        .await
        .unwrap();
    account.status(call, &CallStatus::Connected).await;
    call
}

/// `account` sees `who` in `call` as `wanted` takes them.
async fn sees(account: &mut Account, call: CallId, who: &str, wanted: impl Fn(&teams_core::CallParticipant) -> bool) {
    account
        .view(call, |view| {
            view.participants
                .iter()
                .any(|participant| participant.mri == who && wanted(participant))
        })
        .await;
}

/// The next live reaction `account` is told in `call`: who sent it and which.
async fn reaction(account: &mut Account, call: CallId) -> (String, String) {
    account
        .next(|event| match event {
            Event::CallReaction {
                call: of,
                from,
                reaction,
                ..
            } if *of == call => Some((from.as_str().to_owned(), reaction.clone())),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mute_a_raised_hand_and_a_reaction_reach_everyone_in_the_call() {
    let lab = Lab::start_group_video().await;
    let mut a = lab.account_without_video(LAB_A, A_TONE).await;
    let mut b = lab.account_without_video(LAB_B, B_TONE).await;
    let mut c = lab.account_without_video(LAB_C, PEER_TONE).await;
    let call_a = a
        .handle
        .requests()
        .call(CallAction::Call { chat: ABC_CHAT.into() })
        .await
        .unwrap();
    let (ringing_b, ringing_c) = (b.incoming().await, c.incoming().await);
    let call_b = answer(&mut b, ringing_b.call).await;
    let call_c = answer(&mut c, ringing_c.call).await;
    a.status(call_a, &CallStatus::Connected).await;
    sees(&mut a, call_a, LAB_B, |b| !b.muted && !b.hand_raised).await;

    b.handle
        .requests()
        .call(CallAction::Mute {
            call: call_b,
            muted: true,
        })
        .await
        .unwrap();
    sees(&mut a, call_a, LAB_B, |b| b.muted).await;

    b.handle
        .requests()
        .call(CallAction::RaiseHand {
            call: call_b,
            raised: true,
        })
        .await
        .unwrap();
    sees(&mut a, call_a, LAB_B, |b| b.hand_raised).await;
    b.next(|event| match event {
        Event::CallOwn {
            call,
            hand_raised: true,
            ..
        } if *call == call_b => Some(()),
        _ => None,
    })
    .await;
    b.handle
        .requests()
        .call(CallAction::RaiseHand {
            call: call_b,
            raised: false,
        })
        .await
        .unwrap();
    sees(&mut a, call_a, LAB_B, |b| !b.hand_raised).await;

    c.handle
        .requests()
        .call(CallAction::React {
            call: call_c,
            reaction: "heart".to_owned(),
        })
        .await
        .unwrap();
    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)] {
        assert_eq!(reaction(account, call).await, (LAB_C.to_owned(), "heart".to_owned()));
    }
    // nobody organizes a chat's call: none of them may end it for everyone
    let ended = a.handle.requests().call(CallAction::EndMeeting { call: call_a }).await;
    assert!(ended.is_err(), "{ended:?}");

    for (account, call) in [(&mut a, call_a), (&mut b, call_b), (&mut c, call_c)] {
        account
            .handle
            .requests()
            .call(CallAction::HangUp { call })
            .await
            .unwrap();
        account.ended(call).await;
    }
}

/// The next hold state `account` is told for `call`.
async fn held(account: &mut crate::support::Account, call: CallId) -> bool {
    account
        .next(|event| match event {
            Event::CallHeld { call: of, held } if *of == call => Some(*held),
            _ => None,
        })
        .await
}

/// The next hold by the far end `account` is told for `call`.
async fn held_by_far_end(account: &mut crate::support::Account, call: CallId) -> bool {
    account
        .next(|event| match event {
            Event::CallHeldByFarEnd { call: of, held } if *of == call => Some(*held),
            _ => None,
        })
        .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_one_to_one_call_goes_on_hold_and_comes_off_it_its_lines_as_they_were() {
    let lab = crate::support::Lab::start().await;
    let mut a = lab.account_without_video(LAB_A, A_TONE).await;
    let mut b = lab.account_without_video(LAB_B, B_TONE).await;
    let call = a
        .handle
        .requests()
        .call(CallAction::Call { chat: AB_CHAT.into() })
        .await
        .unwrap();
    let ringing = b.incoming().await;
    let call_b = answer(&mut b, ringing.call).await;
    a.status(call, &CallStatus::Connected).await;
    let hold = |held: bool| CallAction::Hold { call, held };
    a.handle.requests().call(hold(true)).await.unwrap();
    assert!(held(&mut a, call).await, "on hold once its hold was taken");
    // b, held, is told so by the music on hold in its roster
    assert!(held_by_far_end(&mut b, call_b).await, "b sees itself held");
    a.handle.requests().call(hold(false)).await.unwrap();
    assert!(!held(&mut a, call).await, "off hold once its resume was taken");
    assert!(!held_by_far_end(&mut b, call_b).await, "b sees the hold end");
    let sent = lab.sent_by(LAB_A);
    let at = |op: &str| sent.iter().position(|sent| sent == op);
    assert!(at("hold").is_some() && at("hold") < at("resume"), "{sent:?}");
    // the call's lines stay as they are: nothing is renegotiated
    assert!(at("renegotiate").is_none(), "{sent:?}");
}
