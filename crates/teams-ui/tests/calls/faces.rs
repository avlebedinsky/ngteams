//! The faces a call shows of its people (Teams' camera-off tiles): each one's profile picture, else the initials of
//! their name; the account's own tile likewise, its mark while its profile names it nothing; a picture read later
//! (`Event::Profile`) shown once it came; the 1:1 call's ringing screen with the other one's picture. Every id and
//! name is made up.

use gpui::{Bounds, Pixels, VisualTestContext};
use teams_core::{CallStatus, Event, NameFrom, Person};
use teams_store::StoredPicture;

use super::common::{LAB_A, LAB_B, Rig};
use super::{AB, LAB_C, call_windows, calling, drawn, group_call_up, registered, shell_window, stage, status, trace};

/// A made-up picture's bytes (a PNG's signature: GPUI is handed it, never decodes it in a test).
const PICTURE: [u8; 4] = [0x89, b'P', b'N', b'G'];

/// `mri`, named `name`, with a picture when `pictured`, as the engine gives them.
fn profile(rig: &Rig, mri: &str, name: &str, pictured: bool) {
    let picture = pictured.then(|| StoredPicture {
        media_type: "image/png".to_owned(),
        bytes: PICTURE.to_vec(),
    });
    rig.core.people.lock().unwrap().insert(
        mri.to_owned(),
        Person {
            mri: mri.to_owned(),
            name: Some(name.to_owned()),
            from: Some(NameFrom::Graph),
            picture,
            looking: false,
        },
    );
}

/// The faces take their tiles' sides, measured as they were laid out: the frame after it is drawn.
fn measured(stage: &mut VisualTestContext) {
    assert!(drawn(stage, "call-stage"));
    stage.update(|window, cx| {
        window.simulate_next_frame(cx);
    });
    assert!(drawn(stage, "call-stage"));
}

/// Whether `inner` sits centred in `outer`, within a pixel.
fn centred(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    let (a, b) = (inner.center(), outer.center());
    (a.x - b.x).abs() <= gpui::px(1.0) && (a.y - b.y).abs() <= gpui::px(1.0)
}

#[test]
fn a_group_call_s_tiles_show_each_one_s_picture_else_their_initials_and_the_account_s_own() {
    let rig = calling();
    profile(&rig, LAB_A, "Lab Ay", true);
    profile(&rig, LAB_B, "Lab Bee", true);
    let stage = group_call_up(&rig);

    // b's picture, c's initials (no picture), the account's own picture in its self view
    for face in [
        "call-participant-0-picture",
        "call-participant-1-initials",
        "call-participant-self-picture",
    ] {
        assert!(drawn(stage, face), "{face}");
    }
    assert!(!drawn(stage, "call-participant-1-picture"));

    // each face round in the middle of its tile, sized to it as Teams sizes it once its tile was laid out
    measured(stage);
    for (face, tile) in [
        ("call-participant-0-picture", "call-participant-0"),
        ("call-participant-self-picture", "call-participant-self"),
    ] {
        let (face, tile) = (stage.debug_bounds(face).unwrap(), stage.debug_bounds(tile).unwrap());
        assert_eq!(face.size.width, face.size.height, "{face:?} is round");
        assert!(centred(face, tile), "{face:?} centred in {tile:?}");
        let shorter = tile.size.width.min(tile.size.height);
        assert!(
            face.size.width >= shorter * 0.2 && face.size.width <= shorter * 0.6,
            "{face:?} sized to {tile:?}"
        );
    }

    // c's picture read later in the background: their tile shows it
    profile(&rig, LAB_C, "Lab Sea", true);
    rig.send(Event::Profile { mri: LAB_C.to_owned() });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-participant-1-picture"));
    assert!(!drawn(stage, "call-participant-1-initials"));
}

#[test]
fn a_one_to_one_call_rings_with_the_other_one_s_picture_and_shows_it_once_connected() {
    let rig = calling();
    profile(&rig, LAB_B, "Lab Bee", true);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), std::sync::Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Ringing);
    let window = stage(&rig, &windows);
    assert!(drawn(window, "call-calling-picture"), "b's picture while it rings");

    // connected, before the roster tells anyone: b's tile with their picture, the account's own with its mark (its
    // profile names it nothing)
    status(&rig, 1, CallStatus::Connected);
    assert!(drawn(window, "call-person-0-picture"));
    assert!(drawn(window, "call-participant-self-mark"));
}

#[test]
fn a_one_to_one_call_without_a_picture_rings_with_initials() {
    let rig = calling();
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), std::sync::Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Ringing);
    let window = stage(&rig, &windows);
    assert!(drawn(window, "call-calling-initials"));
    status(&rig, 1, CallStatus::Connected);
    assert!(drawn(window, "call-person-0-initials"));
}
