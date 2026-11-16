//! Raised hands on the stage, as Teams shows them: each tile of a person with a hand up (the account's own too) shows
//! the hand with its place in the line of hands, the same number as the People pane; when a hand goes down, the hands
//! after it move up. Every id and name is made up.

use teams_core::Event;

use super::common::{LAB_B, Rig};
use super::{LAB_C, call, click, draw, drawn, group_call_up, view};

/// The trio's call told with b's and c's hands as `(b, c)`.
fn hands_up(rig: &Rig, (b, c): (bool, bool)) {
    let mut told = view(
        &[
            (LAB_B, "Lab Bee", false, Some(1), false),
            (LAB_C, "Lab Sea", false, Some(2), false),
        ],
        false,
    );
    told.participants[0].hand_raised = b;
    told.participants[1].hand_raised = c;
    rig.send(Event::CallParticipants {
        call: call(1),
        view: told,
    });
    rig.cx.run_until_parked();
}

/// The account's own hand, as the roster tells it.
fn own_hand(rig: &Rig, raised: bool) {
    rig.send(Event::CallOwn {
        call: call(1),
        hand_raised: raised,
        organizer: false,
    });
    rig.cx.run_until_parked();
}

/// The places of the hands shown on `tile` (a stage tile or a People row), from 1 to 9; 0 is a hand shown without
/// its place.
fn shown_on(stage: &mut gpui::VisualTestContext, tile: &str) -> Vec<usize> {
    (0..=9)
        .filter(|rank| drawn(stage, &format!("{tile}-hand-{rank}")))
        .collect()
}

#[test]
fn a_tile_shows_its_raised_hand_with_its_place_in_the_line_of_hands() {
    let rig = super::calling();
    let stage = group_call_up(&rig);
    hands_up(&rig, (false, true));
    assert_eq!(shown_on(stage, "call-participant-1"), [1], "c's hand, the first up");
    assert!(shown_on(stage, "call-participant-0").is_empty(), "b's hand is down");
    hands_up(&rig, (true, true));
    assert_eq!(shown_on(stage, "call-participant-0"), [2], "b's hand, up after c's");
    assert_eq!(shown_on(stage, "call-participant-1"), [1]);
    own_hand(&rig, true);
    assert_eq!(
        shown_on(stage, "call-participant-self"),
        [3],
        "the account's own hand, the last up"
    );
    // the People pane numbers each person the same
    click(stage, "call-roster");
    assert_eq!(shown_on(stage, "call-roster-1"), [1], "c's row");
    assert_eq!(shown_on(stage, "call-roster-0"), [2], "b's row");
    assert_eq!(shown_on(stage, "call-roster-self"), [3], "the account's row");
}

#[test]
fn when_a_hand_goes_down_the_hands_after_it_move_up() {
    let rig = super::calling();
    let stage = group_call_up(&rig);
    hands_up(&rig, (true, false));
    hands_up(&rig, (true, true));
    own_hand(&rig, true);
    assert_eq!(shown_on(stage, "call-participant-0"), [1]);
    assert_eq!(shown_on(stage, "call-participant-1"), [2]);
    assert_eq!(shown_on(stage, "call-participant-self"), [3]);
    hands_up(&rig, (false, true));
    draw(stage);
    assert!(shown_on(stage, "call-participant-0").is_empty(), "b lowered its hand");
    assert_eq!(shown_on(stage, "call-participant-1"), [1], "c's hand moved up");
    assert_eq!(
        shown_on(stage, "call-participant-self"),
        [2],
        "the account's hand moved up"
    );
    own_hand(&rig, false);
    assert!(shown_on(stage, "call-participant-self").is_empty());
    assert_eq!(shown_on(stage, "call-participant-1"), [1]);
    // raised again, b's hand goes to the end of the line
    hands_up(&rig, (true, true));
    assert_eq!(shown_on(stage, "call-participant-0"), [2]);
}

#[test]
fn the_speaker_views_strip_shows_the_hands_too() {
    let rig = super::calling();
    let stage = group_call_up(&rig);
    hands_up(&rig, (true, true));
    click(stage, "call-view");
    click(stage, "call-view-speaker");
    assert!(drawn(stage, "call-speaker-strip"));
    assert_eq!(shown_on(stage, "call-participant-0"), [1], "the large tile");
    assert_eq!(shown_on(stage, "call-participant-1"), [2], "a tile of the strip");
}
