//! A team's "..." menu and the "Create a channel" dialog on GPUI's test context, over a fake core: what the menu offers,
//! the form's rules, what Create asks of the engine for each type and layout, Teams' words for what goes wrong, and a
//! private channel's people step, up to the channel opening.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::{ChannelMade, ChannelProblem, Event};
use teams_proto::{ChannelLayout, NewChannelKind, TeamPerson};
use teams_store::{ChannelKind, Team, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::channel_dialog::{ChannelDialog, Step};
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};

const TEAM: &str = "19:channel0001@thread.tacv2";
const NEW: &str = "19:new0001@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";
const CHANNEL_GROUP: &str = "OID:00000000-0000-4000-8000-0000000000bb@00000000-0000-4000-8000-0000000000cc";
const LAB_B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

fn team(can_add: bool, can_add_private: bool, owner: bool) -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "Made-up Team".to_owned(),
        archived: false,
        group_id: Some(GROUP.to_owned()),
        mailbox: Some("made-up-team@lab.example.com".to_owned()),
        owner,
        can_add_channel: can_add,
        can_add_private_channel: can_add_private,
        about: teams_store::TeamAbout::default(),
        channels: vec![
            TeamChannel {
                id: TEAM.to_owned(),
                name: "General".to_owned(),
                general: true,
                kind: ChannelKind::Standard,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
            TeamChannel {
                id: "19:channel0002@thread.tacv2".to_owned(),
                name: "Made-up Channel".to_owned(),
                general: false,
                kind: ChannelKind::Private,
                about: teams_store::ChannelAbout::default(),
                shown: true,
            },
        ],
    }
}

fn rig_with(teams: Vec<Team>) -> Rig {
    let rig = Rig::start();
    rig.cx.update(teams_ui::window::init);
    *rig.core.teams.lock().unwrap() = Some(teams);
    rig
}

fn shell_window(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (shell, cx)
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
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

/// The team's "...", as the pointer reaches it.
fn open_menu(cx: &mut VisualTestContext) {
    let line = bounds(cx, "team-0").expect("the team's line");
    cx.simulate_mouse_move(line.center(), None, Modifiers::none());
    click(cx, "team-more-0");
}

fn open_dialog(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> Entity<ChannelDialog> {
    open_menu(cx);
    click(cx, "team-add-channel-0");
    cx.update(|_, cx| shell.read(cx).channel_dialog().cloned())
        .expect("the dialog opened")
}

fn name_it(dialog: &Entity<ChannelDialog>, name: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.set_name(name, window, cx)));
    cx.run_until_parked();
}

fn pick(cx: &mut VisualTestContext, kind: &str) {
    click(cx, "channel-type");
    click(cx, &format!("channel-type-{kind}"));
}

#[test]
fn a_team_s_menu_offers_add_channel_and_opens_the_dialog_on_the_name() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (shell, cx) = shell_window(&rig);
    open_menu(cx);
    assert!(bounds(cx, "team-add-channel-0").is_some());
    assert!(
        cx.update(|_, cx| shell.read(cx).sidebar().read(cx).team_open(TEAM)),
        "the click stayed on the \"...\": the team didn't fold"
    );
    click(cx, "team-add-channel-0");
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_some()));
    for selector in [
        "channel-dialog",
        "channel-name",
        "channel-description",
        "channel-type",
        "channel-layout-threads",
        "channel-layout-posts",
        "channel-cancel",
        "channel-create",
    ] {
        assert!(bounds(cx, selector).is_some(), "{selector} is drawn");
    }
    assert!(bounds(cx, "team-add-channel-0").is_none(), "the menu closed");
    click(cx, "channel-cancel");
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()));
    assert!(rig.core.channels_asked.lock().unwrap().is_empty(), "nothing asked");
}

#[test]
fn a_team_that_doesn_t_let_the_account_add_a_channel_offers_no_menu() {
    let rig = rig_with(vec![team(false, false, false)]);
    let (_shell, cx) = shell_window(&rig);
    assert!(bounds(cx, "team-0").is_some());
    assert!(bounds(cx, "team-more-0").is_none());
}

#[test]
fn a_private_channel_s_row_carries_its_lock() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (_shell, cx) = shell_window(&rig);
    assert!(
        bounds(cx, "channel-private-0").is_none(),
        "the general channel isn't private"
    );
    assert!(bounds(cx, "channel-private-1").is_some());
}

#[test]
fn create_stays_off_until_the_name_reads_and_a_type_is_chosen() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    let can_create = |cx: &mut VisualTestContext| cx.update(|_, cx| dialog.read(cx).can_create(cx));
    assert!(!can_create(cx), "nothing typed");
    assert!(bounds(cx, "channel-name-problem").is_none(), "no problem shown yet");
    name_it(&dialog, "New channel", cx);
    assert!(!can_create(cx), "no type yet");
    pick(cx, "standard");
    assert!(can_create(cx));
    for (name, problem) in [
        ("", true),
        ("a#b", true),
        ("general", true),
        ("made-up channel", true),
        ("Fine", false),
    ] {
        name_it(&dialog, name, cx);
        assert_eq!(can_create(cx), !problem, "{name}");
        assert_eq!(bounds(cx, "channel-name-problem").is_some(), problem, "{name}");
    }
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.set_description("see https://example.com", window, cx);
        });
    });
    assert!(!can_create(cx), "a link in the description");
    assert!(bounds(cx, "channel-description-problem").is_some());
}

#[test]
fn a_team_that_doesn_t_let_the_account_make_private_channels_offers_standard_only() {
    let rig = rig_with(vec![team(true, false, false)]);
    let (shell, cx) = shell_window(&rig);
    open_dialog(&shell, cx);
    click(cx, "channel-type");
    assert!(bounds(cx, "channel-type-standard").is_some());
    assert!(bounds(cx, "channel-type-private").is_none());
}

#[test]
fn a_standard_channel_goes_with_its_layout_and_opens() {
    let rig = rig_with(vec![team(true, true, false)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Ok(ChannelMade {
        thread: Some(NEW.to_owned()),
        group: None,
        not_shown_by_default: false,
    }));
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "  New channel  ", cx);
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| dialog.set_description("what for", window, cx));
    });
    pick(cx, "standard");
    click(cx, "channel-layout-posts");
    click(cx, "channel-create");
    let drafts = rig.core.channels_asked.lock().unwrap().clone();
    let [draft] = drafts.as_slice() else {
        panic!("one creation: {drafts:?}");
    };
    assert_eq!(draft.channel.team, TEAM);
    assert_eq!(draft.channel.group, GROUP);
    assert_eq!(draft.channel.name, "New channel", "trimmed");
    assert_eq!(draft.channel.description, "what for");
    assert_eq!(draft.channel.kind, NewChannelKind::Standard);
    assert_eq!(draft.channel.layout, ChannelLayout::Posts);
    assert!(!draft.show_by_default);
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()), "closed");
    assert!(
        cx.update(|_, cx| matches!(shell.read(cx).main(), Main::Channel(_))),
        "the channel opens"
    );
}

#[test]
fn threads_is_chosen_at_first() {
    let rig = rig_with(vec![team(true, true, false)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Err(ChannelProblem::Other));
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "New channel", cx);
    pick(cx, "private");
    click(cx, "channel-create");
    let drafts = rig.core.channels_asked.lock().unwrap().clone();
    assert_eq!(drafts[0].channel.layout, ChannelLayout::Threads);
    assert_eq!(drafts[0].channel.kind, NewChannelKind::Private);
}

#[test]
fn an_owner_may_show_a_standard_channel_to_everyone() {
    let rig = rig_with(vec![team(true, true, true)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Ok(ChannelMade {
        thread: Some(NEW.to_owned()),
        group: None,
        not_shown_by_default: true,
    }));
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "New channel", cx);
    assert!(bounds(cx, "channel-show-by-default").is_none(), "no type yet");
    pick(cx, "private");
    assert!(bounds(cx, "channel-show-by-default").is_none(), "not a private one");
    pick(cx, "standard");
    click(cx, "channel-show-by-default");
    click(cx, "channel-create");
    assert!(rig.core.channels_asked.lock().unwrap()[0].show_by_default);
    // made, but not shown to everyone: said, and Close goes on to the channel
    assert!(bounds(cx, "channel-failure").is_some());
    click(cx, "channel-failure-close");
    assert!(cx.update(|_, cx| matches!(shell.read(cx).main(), Main::Channel(_))));
}

#[test]
fn what_goes_wrong_is_said_in_teams_words_and_try_again_goes_again() {
    let rig = rig_with(vec![team(true, true, false)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Err(ChannelProblem::Other));
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "New channel", cx);
    pick(cx, "standard");
    click(cx, "channel-create");
    assert!(bounds(cx, "channel-failure-words").is_some());
    click(cx, "channel-try-again");
    assert_eq!(rig.core.channels_asked.lock().unwrap().len(), 2, "went again");
    let drafts = rig.core.channels_asked.lock().unwrap().clone();
    assert_eq!(drafts[0], drafts[1], "with what was typed");

    *rig.core.channel_answer.lock().unwrap() = Some(Err(ChannelProblem::NameTaken));
    click(cx, "channel-try-again");
    assert!(bounds(cx, "channel-try-again").is_none(), "trying again wouldn't help");
    click(cx, "channel-failure-close");
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()));
}

#[test]
fn a_private_channel_asks_for_its_people_then_opens() {
    let rig = rig_with(vec![team(true, true, false)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Ok(ChannelMade {
        thread: None,
        group: Some(CHANNEL_GROUP.to_owned()),
        not_shown_by_default: false,
    }));
    *rig.core.team_found.lock().unwrap() = vec![TeamPerson {
        mri: LAB_B.to_owned(),
        name: "Lab B".to_owned(),
        email: Some("lab-b@lab.example.com".to_owned()),
        job_title: None,
    }];
    *rig.core.channel_thread_answer.lock().unwrap() = Some(NEW.to_owned());
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "Hush", cx);
    pick(cx, "private");
    click(cx, "channel-create");
    assert!(
        cx.update(|_, cx| matches!(dialog.read(cx).step(), Step::People { .. })),
        "the form gave way to the people step"
    );
    assert!(bounds(cx, "channel-people").is_some());
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.search_people("lab".to_owned(), cx)));
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    click(cx, "channel-person-0");
    assert!(bounds(cx, "channel-picked-0").is_some());
    click(cx, "channel-people-add");
    assert_eq!(
        *rig.core.channel_members_added.lock().unwrap(),
        vec![(GROUP.to_owned(), CHANNEL_GROUP.to_owned(), vec![LAB_B.to_owned()])]
    );
    assert!(cx.update(|_, cx| shell.read(cx).channel_dialog().is_none()), "closed");
    assert!(cx.update(|_, cx| matches!(shell.read(cx).main(), Main::Channel(_))));
}

#[test]
fn skipping_the_people_step_still_opens_the_channel() {
    let rig = rig_with(vec![team(true, true, false)]);
    *rig.core.channel_answer.lock().unwrap() = Some(Ok(ChannelMade {
        thread: Some(NEW.to_owned()),
        group: Some(CHANNEL_GROUP.to_owned()),
        not_shown_by_default: false,
    }));
    let (shell, cx) = shell_window(&rig);
    let dialog = open_dialog(&shell, cx);
    name_it(&dialog, "Hush", cx);
    pick(cx, "private");
    click(cx, "channel-create");
    click(cx, "channel-people-skip");
    assert!(rig.core.channel_members_added.lock().unwrap().is_empty());
    assert!(cx.update(|_, cx| matches!(shell.read(cx).main(), Main::Channel(_))));
}

#[test]
fn a_new_channel_the_engine_stored_is_in_the_sidebar_when_it_opens() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (shell, cx) = shell_window(&rig);
    let mut listed = team(true, true, false);
    listed.channels.push(TeamChannel {
        id: NEW.to_owned(),
        name: "Brand new".to_owned(),
        general: false,
        kind: ChannelKind::Standard,
        about: teams_store::ChannelAbout::default(),
        shown: true,
    });
    *rig.core.teams.lock().unwrap() = Some(vec![listed]);
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert!(bounds(cx, "channel-row-2").is_some());
    assert!(cx.update(|_, cx| shell.read(cx).teams().read(cx).chosen(NEW).is_some()));
}

#[test]
fn only_a_refreshed_team_list_closes_a_previously_listed_channel_that_was_removed() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (shell, cx) = shell_window(&rig);
    let channel = "19:channel0002@thread.tacv2";
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_channel_id(channel.into(), window, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(channel.into())
    );

    let teams = cx.update(|_, cx| shell.read(cx).teams().clone());
    cx.update(|_, cx| teams.update(cx, |teams, cx| teams.select(None, cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(channel.into()),
        "a local selection notification carries no deletion evidence"
    );

    let mut listed = team(true, true, false);
    listed.channels.retain(|listed| listed.id != channel);
    *rig.core.teams.lock().unwrap() = Some(vec![listed]);
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
    assert!(cx.update(|_, cx| shell.read(cx).channel().is_none()));
}

#[test]
fn an_unlisted_channel_survives_missing_snapshots_until_a_listed_channel_is_actually_removed() {
    let rig = rig_with(vec![team(true, true, false)]);
    let (shell, cx) = shell_window(&rig);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_channel_id(NEW.into(), window, cx)));
    cx.run_until_parked();
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(NEW.into()),
        "a previously started list read cannot prove that a newly opened channel was removed"
    );
    let mut listed = team(true, true, false);
    let mut made = listed.channels[1].clone();
    made.id = NEW.into();
    made.name = "Brand new".into();
    listed.channels.push(made);
    *rig.core.teams.lock().unwrap() = Some(vec![listed]);
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::Channel(NEW.into())
    );
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).teams().read(cx).chosen(NEW).unwrap().name.to_string()),
        "Brand new"
    );
    *rig.core.teams.lock().unwrap() = Some(vec![team(true, true, false)]);
    rig.send(Event::Teams);
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Nothing);
}
