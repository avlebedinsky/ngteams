//! Two things the call window got wrong live (2026-11-06), on the fake engine: a recording's notice whose line, with a
//! long name in it, ran past its card (lab b's window), and Controls' Present flipping back unchecked right after it was
//! turned back on, when the re-read of the options still had the old value.

use std::time::Duration;

use gpui::{Bounds, Pixels, VisualTestContext, px, size};
use teams_core::{CallAction, CallActionError, EngineError, Event};
use teams_proto::{MeetingOption, MeetingOptions};

use super::{LAB_B, LAB_C, actions, call, calling, click, drawn, group_call_up, meeting, organized_group_call, view};

/// A made-up participant's name long enough that no notice line holds it on one line.
const LONG_NAME: &str = "Bartholomew Alexander Montgomery-Featherstonehaugh of the Quality Assurance Laboratory";

fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

#[test]
fn a_recording_s_notice_wraps_a_long_name_inside_its_card_at_every_width() {
    let rig = calling();
    let stage = group_call_up(&rig);
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(
            &[
                (LAB_B, LONG_NAME, false, Some(1), false),
                (LAB_C, "Lab Sea", false, Some(2), false),
            ],
            false,
        ),
    });
    rig.cx.run_until_parked();
    meeting(&rig, super::recording_by(LAB_B));
    for width in [1280.0, 800.0, 480.0, 360.0] {
        stage.simulate_resize(size(px(width), px(720.0)));
        assert!(drawn(stage, "call-recording-notice"), "{width}");
        let card = stage.debug_bounds("call-recording-notice").unwrap();
        let line = stage.debug_bounds("call-recording-policy").unwrap();
        let close = stage.debug_bounds("call-recording-notice-close").unwrap();
        assert!(
            within(line, card),
            "at {width}: the line {line:?} runs past its card {card:?}"
        );
        assert!(
            within(close, card),
            "at {width}: Close {close:?} is pushed out of {card:?}"
        );
        assert!(
            card.right() <= px(width),
            "at {width}: the card {card:?} runs past the window"
        );
        assert!(
            // one line of small text is ~23 px high
            line.size.height > px(30.0),
            "at {width}: the long line wraps ({line:?})"
        );
    }
}

/// What the middle tier's read answers from now on.
fn server(rig: &super::Rig, options: MeetingOptions) {
    *rig.core.meeting_options.lock().unwrap() = Some(options);
}

/// The options are read again, as they are shortly after a change.
fn read_again(stage: &mut VisualTestContext) {
    stage.executor().advance_clock(Duration::from_secs(4));
    stage.run_until_parked();
}

fn on(stage: &mut VisualTestContext, key: &str) -> bool {
    drawn(stage, &format!("call-controls-{key}-on"))
}

/// Turns `key` off and the read agrees; turns it back on while the read still says off: it stays on.
fn kept_over_a_lagging_read(
    rig: &super::Rig,
    stage: &mut VisualTestContext,
    (key, off): (&str, fn(&mut MeetingOptions)),
    had: &mut MeetingOptions,
) {
    assert!(on(stage, key), "{key} starts on");
    click(stage, &format!("call-controls-{key}"));
    off(had);
    server(rig, *had);
    read_again(stage);
    assert!(!on(stage, key), "{key}: off, and read back off");
    click(stage, &format!("call-controls-{key}"));
    assert!(on(stage, key), "{key}: on again at once");
    // the read lags the change: it still says off
    read_again(stage);
    assert!(
        on(stage, key),
        "{key}: a read that lags the change doesn't flip it back"
    );
}

#[test]
fn each_controls_check_the_user_set_outlasts_a_read_that_lags_it() {
    let rig = calling();
    let mut had = MeetingOptions {
        lobby: Some(true),
        present: Some(true),
        chat: Some(true),
        unmute: Some(true),
        camera: Some(true),
    };
    server(&rig, had);
    let stage = organized_group_call(&rig);
    click(stage, "call-controls");
    kept_over_a_lagging_read(&rig, stage, ("lobby", |o| o.lobby = Some(false)), &mut had);
    kept_over_a_lagging_read(&rig, stage, ("chat", |o| o.chat = Some(false)), &mut had);
    kept_over_a_lagging_read(&rig, stage, ("present", |o| o.present = Some(false)), &mut had);
    // the restrictions take changes once not everyone presents; the service has present off by then
    click(stage, "call-controls-present");
    had.present = Some(false);
    server(&rig, had);
    read_again(stage);
    assert!(!on(stage, "present"));
    kept_over_a_lagging_read(&rig, stage, ("unmute", |o| o.unmute = Some(false)), &mut had);
    kept_over_a_lagging_read(&rig, stage, ("cameras", |o| o.camera = Some(false)), &mut had);
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::MeetingOption {
            call: call(1),
            option: MeetingOption::Attendees {
                unmute: true,
                camera: true,
            },
        })
    );
    // the service catches up: a read that shows what was set confirms it, and a later one that differs (changed
    // elsewhere) shows
    had.lobby = Some(true);
    server(&rig, had);
    click(stage, "call-controls-chat");
    read_again(stage);
    assert!(on(stage, "lobby"));
    had.lobby = Some(false);
    server(&rig, had);
    click(stage, "call-controls-chat");
    read_again(stage);
    assert!(
        !on(stage, "lobby"),
        "confirmed, the user's value gives way to a newer read"
    );
}

#[test]
fn the_conversation_s_newer_option_and_a_change_not_taken_replace_what_the_user_set() {
    let rig = calling();
    let had = MeetingOptions {
        present: Some(false),
        ..MeetingOptions::default()
    };
    server(&rig, had);
    let stage = organized_group_call(&rig);
    click(stage, "call-controls");
    // Unmute turned off; the conversation then tells it on, after the change: newer, it stands
    click(stage, "call-controls-unmute");
    assert!(!on(stage, "unmute"));
    meeting(
        &rig,
        teams_core::MeetingState {
            options: MeetingOptions {
                unmute: Some(true),
                ..MeetingOptions::default()
            },
            ..teams_core::MeetingState::default()
        },
    );
    assert!(on(stage, "unmute"), "the conversation told it after the change");
    // Chat turned off but the change isn't taken: it shows on again, read or not
    rig.core
        .call_answers
        .lock()
        .unwrap()
        .push_back(Err(EngineError::Call(CallActionError::NotChanged)));
    click(stage, "call-controls-chat");
    assert_eq!(
        actions(&rig).last(),
        Some(&CallAction::MeetingOption {
            call: call(1),
            option: MeetingOption::Chat(false),
        })
    );
    assert!(on(stage, "chat"), "a change not taken goes back at once");
}
