//! The faces a call shows of its people (Teams' camera-off tiles): each one's profile picture, else the initials of
//! their name; the account's own tile likewise, its mark while its profile names it nothing; a picture read later
//! (`Event::Profile`) shown once it came, even when it came while an earlier read was under way; the 1:1 call's
//! ringing, held and ended screens with the other one's picture; the People pane's rows, the lobby's card and the
//! share's strip. Every id and name is made up.

use gpui::{Bounds, Pixels, VisualTestContext};
use teams_core::{CallStatus, End, Event, LobbyPerson, NameFrom, Person, RemoteEnd};
use teams_proto::Mri;
use teams_store::StoredPicture;
use teams_ui::video::VideoFeed;

use super::common::{LAB_A, LAB_B, Rig};
use super::{
    AB, LAB_C, call, call_windows, calling, click, draw, drawn, group_call_up, in_meeting, one_to_one_video_call,
    registered, shell_window, stage, status, trace, view,
};

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

/// The engine's answers held back so far, now sent as they were made.
fn release_held(rig: &Rig) {
    let held = std::mem::take(&mut *rig.core.held.lock().unwrap());
    assert!(!held.is_empty(), "an answer was held");
    for (answer, person) in held {
        answer.send(Ok(person));
    }
    rig.cx.run_until_parked();
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

#[test]
fn a_profile_change_that_comes_while_a_face_is_read_is_not_lost_to_the_older_answer() {
    let rig = calling();
    // c's first read is held back, as an engine answering a batch of people one by one would be slow on it
    rig.core.hold_people.lock().unwrap().insert(LAB_C.to_owned());
    let stage = group_call_up(&rig);
    assert!(drawn(stage, "call-participant-1-initials"), "c's face while it is read");

    // c's picture read in the background meanwhile: c is asked again and that answer comes at once
    rig.core.hold_people.lock().unwrap().clear();
    profile(&rig, LAB_C, "Lab Sea", true);
    rig.send(Event::Profile { mri: LAB_C.to_owned() });
    assert!(drawn(stage, "call-participant-1-picture"));

    // the first read's answer, made before the change, comes last: it is dropped
    release_held(&rig);
    assert!(drawn(stage, "call-participant-1-picture"));
    assert!(!drawn(stage, "call-participant-1-initials"));
}

#[test]
fn the_account_s_own_row_shows_its_picture_while_its_profile_names_it_nothing() {
    let rig = calling();
    rig.core.people.lock().unwrap().insert(
        LAB_A.to_owned(),
        Person {
            mri: LAB_A.to_owned(),
            name: None,
            from: None,
            picture: Some(StoredPicture {
                media_type: "image/png".to_owned(),
                bytes: PICTURE.to_vec(),
            }),
            looking: false,
        },
    );
    let stage = group_call_up(&rig);
    click(stage, "call-roster");
    assert!(drawn(stage, "call-roster-self-picture"));
    assert!(drawn(stage, "call-participant-self-picture"), "and its self view");
}

#[test]
fn the_people_pane_s_rows_show_each_one_s_picture_else_their_initials() {
    let rig = calling();
    profile(&rig, LAB_A, "Lab Ay", false);
    profile(&rig, LAB_B, "Lab Bee", true);
    let stage = group_call_up(&rig);
    click(stage, "call-roster");
    for face in [
        "call-roster-self-initials",
        "call-roster-0-picture",
        "call-roster-1-initials",
    ] {
        assert!(drawn(stage, face), "{face}");
    }

    // c left: listed under "People added to the chat" with their initials, then their picture once it was read
    rig.send(Event::CallParticipants {
        call: call(1),
        view: view(&[(LAB_B, "Lab Bee", false, Some(1), false)], false),
    });
    rig.cx.run_until_parked();
    assert!(drawn(stage, "call-roster-added-0-initials"));
    profile(&rig, LAB_C, "Lab Sea", true);
    rig.send(Event::Profile { mri: LAB_C.to_owned() });
    assert!(drawn(stage, "call-roster-added-0-picture"));
}

#[test]
fn whoever_waits_in_the_lobby_shows_their_picture_on_its_card_and_in_people() {
    let rig = calling();
    profile(&rig, LAB_B, "Lab Bee", true);
    let (trace, _) = trace();
    let (_windows, stage) = in_meeting(&rig, &trace);
    rig.send(Event::Lobby {
        call: call(1),
        waiting: vec![
            LobbyPerson {
                mri: Mri::new(LAB_B),
                name: Some("Lab Bee".to_owned()),
            },
            LobbyPerson {
                mri: Mri::new(LAB_C),
                name: Some("Lab Sea".to_owned()),
            },
        ],
    });
    rig.cx.run_until_parked();
    draw(stage);
    assert!(drawn(stage, "call-lobby-person-0-picture"));
    assert!(drawn(stage, "call-lobby-person-1-initials"));
    click(stage, "call-roster");
    assert!(drawn(stage, "call-roster-waiting-0-picture"));
    assert!(drawn(stage, "call-roster-waiting-1-initials"));
}

#[test]
fn the_share_s_strip_shows_the_other_one_s_picture() {
    let feed = VideoFeed::new();
    let (rig, stage) = one_to_one_video_call(&feed);
    profile(&rig, LAB_B, "Lab Bee", true);
    rig.send(Event::Profile { mri: LAB_B.to_owned() });
    rig.send(Event::CallRemoteShare {
        call: call(1),
        on: true,
    });
    assert!(drawn(stage, "call-share-strip"));
    assert!(drawn(stage, "call-person-0-picture"));
}

#[test]
fn a_one_to_one_call_held_either_way_and_ended_shows_the_other_one_s_picture() {
    let rig = calling();
    profile(&rig, LAB_B, "Lab Bee", true);
    let (trace, _) = trace();
    let (shell, cx) = shell_window(&rig, Some(AB), std::sync::Arc::clone(&trace));
    let (windows, _) = call_windows(&rig, &shell, &trace);
    registered(&rig);
    cx.simulate_keystrokes("ctrl-shift-c");
    cx.run_until_parked();
    status(&rig, 1, CallStatus::Connected);
    let window = stage(&rig, &windows);

    for held_by_far_end in [false, true] {
        let event = |held| {
            if held_by_far_end {
                Event::CallHeldByFarEnd { call: call(1), held }
            } else {
                Event::CallHeld { call: call(1), held }
            }
        };
        rig.send(event(true));
        assert!(
            drawn(window, "call-calling-picture"),
            "held, by the far end: {held_by_far_end}"
        );
        rig.send(event(false));
    }

    // b hung up: the end shows who it was with
    status(&rig, 1, CallStatus::Ended(End::Remote(RemoteEnd::default())));
    assert!(drawn(window, "call-end"));
    assert!(drawn(window, "call-end-picture"));
}
