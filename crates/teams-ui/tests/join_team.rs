//! "Join a team" on GPUI's test context, over a fake core: the suggested teams as cards, Join (or Request to join) on
//! one and a code's Add team each asking the core for that one write, a failed join said on the page, the search box
//! reading the service's suggestions for its words after the pause, Back and Create team told to the shell, and the
//! read-only mode showing the cards without any way to join.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use common::Rig;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::TeamWritten;
use teams_proto::{JoinableTeam, TeamWrite};
use teams_ui::join_team::{JoinTeam, JoinTeamEvent};

fn joinable(group: &str, name: &str, public: bool) -> JoinableTeam {
    JoinableTeam {
        group: group.to_owned(),
        name: name.to_owned(),
        description: format!("All about {name}, made up"),
        public,
    }
}

/// The page in a window of its own over the rig's core, and what it told the shell.
fn page(
    rig: &Rig,
) -> (
    Entity<JoinTeam>,
    &'static mut VisualTestContext,
    Rc<RefCell<Vec<JoinTeamEvent>>>,
) {
    rig.cx.update(teams_ui::window::init);
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| JoinTeam::new(Arc::clone(&core), window, cx));
    let told = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &JoinTeamEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    cx.run_until_parked();
    (view, cx, told)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn writes(rig: &Rig) -> Vec<TeamWrite> {
    rig.core
        .team_writes
        .lock()
        .unwrap()
        .iter()
        .map(|(write, _)| write.clone())
        .collect()
}

#[test]
fn join_on_a_public_team_s_card_joins_it_and_tells_the_shell() {
    let rig = Rig::start();
    *rig.core.joinable.lock().unwrap() = vec![joinable("g-alpha", "Alpha", true), joinable("g-beta", "Beta", false)];
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (_view, cx, told) = page(&rig);
    assert_eq!(
        *rig.core.joinable_asked.lock().unwrap(),
        [None],
        "the suggestions are read at once"
    );
    assert!(bounds(cx, "join-card-0").is_some());
    assert!(bounds(cx, "join-card-1").is_some());
    assert!(bounds(cx, "join-code-card").is_some(), "a code's card comes first");

    click(cx, "join-card-join-0");
    assert_eq!(
        writes(&rig),
        [TeamWrite::Join {
            group: "g-alpha".to_owned()
        }]
    );
    assert_eq!(*told.borrow(), [JoinTeamEvent::Joined("You joined the team")]);
    assert!(bounds(cx, "join-problem").is_none());
}

#[test]
fn a_join_that_fails_is_said_on_the_page_and_tells_the_shell_nothing() {
    let rig = Rig::start();
    *rig.core.joinable.lock().unwrap() = vec![joinable("g-beta", "Beta", false)];
    let (_view, cx, told) = page(&rig);
    // a private team: its owners are asked
    click(cx, "join-card-join-0");
    assert_eq!(
        writes(&rig),
        [TeamWrite::Join {
            group: "g-beta".to_owned()
        }]
    );
    assert!(told.borrow().is_empty());
    assert!(bounds(cx, "join-problem").is_some(), "the page says it didn't work");
}

#[test]
fn add_team_joins_by_the_code_typed_once_there_is_one() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (view, cx, told) = page(&rig);
    // nothing typed: Add team asks nothing
    cx.update(|_, cx| view.update(cx, JoinTeam::join_by_code));
    assert!(writes(&rig).is_empty());

    click(cx, "join-code");
    cx.simulate_input("  made-up-code ");
    cx.run_until_parked();
    click(cx, "join-code-add");
    assert_eq!(
        writes(&rig),
        [TeamWrite::JoinByCode {
            code: "made-up-code".to_owned()
        }]
    );
    assert_eq!(*told.borrow(), [JoinTeamEvent::Joined("You joined the team")]);
}

#[test]
fn the_search_box_reads_the_suggestions_for_its_words_after_the_pause() {
    let rig = Rig::start();
    *rig.core.joinable.lock().unwrap() = vec![joinable("g-alpha", "Alpha", true), joinable("g-beta", "Beta", true)];
    let (_view, cx, _told) = page(&rig);
    click(cx, "join-search");
    cx.simulate_input("bet");
    cx.run_until_parked();
    assert_eq!(rig.core.joinable_asked.lock().unwrap().len(), 1, "not before the pause");
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(
        rig.core.joinable_asked.lock().unwrap().last(),
        Some(&Some("bet".to_owned()))
    );
    assert!(bounds(cx, "join-card-0").is_some(), "Beta's card");
    assert!(bounds(cx, "join-card-1").is_none(), "Alpha isn't found");
}

#[test]
fn back_and_create_team_are_told_to_the_shell() {
    let rig = Rig::start();
    let (_view, cx, told) = page(&rig);
    click(cx, "join-back");
    click(cx, "join-create");
    assert_eq!(*told.borrow(), [JoinTeamEvent::Back, JoinTeamEvent::CreateTeam]);
}

#[test]
fn the_read_only_mode_shows_the_cards_and_offers_no_joining() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    *rig.core.joinable.lock().unwrap() = vec![joinable("g-alpha", "Alpha", true)];
    let (_view, cx, _told) = page(&rig);
    assert!(bounds(cx, "join-card-0").is_some());
    assert!(bounds(cx, "join-card-join-0").is_none());
    assert!(bounds(cx, "join-code-card").is_none());
    assert!(bounds(cx, "join-create").is_none());
    assert!(bounds(cx, "join-back").is_some());
}

#[test]
fn the_read_only_mode_without_suggestions_says_there_are_none() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx, _told) = page(&rig);
    assert!(bounds(cx, "join-card-0").is_none());
    assert!(bounds(cx, "join-cards").is_some());
    // a join asked of the page itself still goes through the core: the core refuses it below the views
    cx.update(|_, cx| view.update(cx, |page, cx| page.join("g-x".to_owned(), cx)));
    cx.run_until_parked();
    assert_eq!(
        writes(&rig),
        [TeamWrite::Join {
            group: "g-x".to_owned()
        }]
    );
}
