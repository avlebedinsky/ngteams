//! "More create team options" on GPUI's test context, over a fake core: its column's sources, From group saying how
//! many groups the account owns (or Teams' words for none), From another team with no team owned, a picked team's form
//! with its privacy chosen and closed again, Create refused said on the form, and Back and From scratch told to the
//! shell.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use common::Rig;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_proto::TeamWrite;
use teams_store::{Team, TeamAbout};
use teams_ui::create_team_more::{CreateTeamMore, CreateTeamMoreEvent, MoreChoice, MoreSource};

const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

fn owned(name: &str) -> Team {
    Team {
        id: format!("19:{name}@thread.tacv2"),
        name: name.to_owned(),
        group_id: Some(GROUP.to_owned()),
        owner: true,
        about: TeamAbout {
            description: format!("About {name}"),
            public: true,
            ..TeamAbout::default()
        },
        ..Team::default()
    }
}

type Told = Rc<RefCell<Vec<CreateTeamMoreEvent>>>;

/// The page in a window of its own over the rig's core, the account owning `teams`, and what it told the shell.
fn page(rig: &Rig, teams: Vec<Team>) -> (Entity<CreateTeamMore>, &'static mut VisualTestContext, Told) {
    rig.cx.update(teams_ui::window::init);
    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| CreateTeamMore::new(Arc::clone(&core), teams, window, cx));
    let told = Told::default();
    let seen = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &CreateTeamMoreEvent, _| {
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

#[test]
fn from_group_says_how_many_groups_the_account_owns() {
    let rig = Rig::start();
    let (view, cx, _told) = page(&rig, Vec::new());
    assert_eq!(cx.update(|_, cx| view.read(cx).source()), MoreSource::Template);
    // the group read answers none: Teams' words for owning no group
    click(cx, "create-more-group");
    assert_eq!(cx.update(|_, cx| view.read(cx).source()), MoreSource::Group);
    assert!(bounds(cx, "create-more-no-group").is_some());

    for (count, shown) in [
        (1, "create-more-groups"),
        (3, "create-more-groups"),
        (0, "create-more-no-group"),
    ] {
        let rig = Rig::start();
        *rig.core.owned_groups.lock().unwrap() = Some(count);
        let (_view, cx, _told) = page(&rig, Vec::new());
        click(cx, "create-more-group");
        assert!(bounds(cx, shown).is_some(), "{count} groups: {shown}");
    }
}

#[test]
fn from_another_team_without_a_team_owned_says_so() {
    let rig = Rig::start();
    let (view, cx, _told) = page(&rig, Vec::new());
    click(cx, "create-more-another-team");
    assert!(cx.update(|_, cx| view.read(cx).owned().is_empty()));
    assert!(bounds(cx, "create-more-no-team").is_some());
    assert!(bounds(cx, "create-more-team-0").is_none());
}

#[test]
fn a_picked_team_s_form_takes_its_privacy_and_closes_without_sending() {
    let rig = Rig::start();
    let (view, cx, _told) = page(&rig, vec![owned("Alpha")]);
    assert!(
        cx.update(|_, cx| view.read(cx).templates().is_empty()),
        "no templates read"
    );
    click(cx, "create-more-another-team");
    click(cx, "create-more-team-0");
    assert!(cx.update(|_, cx| view.read(cx).form_shown()));
    assert!(matches!(
        cx.update(|_, cx| view.read(cx).choice().cloned()),
        Some(MoreChoice::Team(team)) if team.name == "Alpha"
    ));
    // the team is public: its copy starts public, and Private is picked
    click(cx, "create-more-private");
    let private = cx.update(|_, cx| view.read(cx).write(cx));
    assert!(matches!(private, Some(TeamWrite::Copy(copy)) if !copy.public && copy.name == "Alpha [copy]"));
    click(cx, "create-more-public");
    let public = cx.update(|_, cx| view.read(cx).write(cx));
    assert!(matches!(public, Some(TeamWrite::Copy(copy)) if copy.public));
    click(cx, "create-more-cancel");
    assert!(cx.update(|_, cx| view.read(cx).choice().is_none()));
    assert!(!cx.update(|_, cx| view.read(cx).form_shown()));
    assert!(rig.core.team_writes.lock().unwrap().is_empty(), "nothing sent");
}

#[test]
fn a_create_the_service_refuses_is_said_on_the_form() {
    let rig = Rig::start();
    let (_view, cx, told) = page(&rig, vec![owned("Alpha")]);
    click(cx, "create-more-another-team");
    click(cx, "create-more-team-0");
    click(cx, "create-more-create");
    assert_eq!(rig.core.team_writes.lock().unwrap().len(), 1);
    assert!(bounds(cx, "create-more-problem").is_some());
    assert!(told.borrow().is_empty());
}

#[test]
fn back_and_from_scratch_are_told_to_the_shell() {
    let rig = Rig::start();
    let (_view, cx, told) = page(&rig, Vec::new());
    click(cx, "create-more-scratch");
    click(cx, "create-more-back");
    assert_eq!(
        *told.borrow(),
        [CreateTeamMoreEvent::FromScratch, CreateTeamMoreEvent::Back]
    );
}
