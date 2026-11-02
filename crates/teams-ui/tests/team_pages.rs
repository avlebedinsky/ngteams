//! The team pages on GPUI's test context, over a fake core: "See all your teams" and the page's rows and menu, Create team
//! and its Add members step, Delete team asking first, Hide all channels, and Manage team's settings writing the whole of
//! them.

#![cfg(test)]

#[expect(
    dead_code,
    reason = "the helpers every test file shares; these tests use some of them"
)]
mod common;

use std::sync::Arc;

use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_core::TeamWritten;
use teams_proto::{OwnTeamView, TeamWrite};
use teams_store::{ChannelKind, Team, TeamAbout, TeamChannel};
use teams_ui::ViewTrace;
use teams_ui::manage_team::ManageTab;
use teams_ui::settings::{Settings, SettingsModel};
use teams_ui::shell::{Main, Shell, ShellParts};
use teams_ui::team_dialogs::{AddMembersDialog, ConfirmDialog, CreateTeamDialog};

const OWNED: &str = "19:team0001@thread.tacv2";
const MEMBER_OF: &str = "19:team0002@thread.tacv2";
const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

fn team(id: &str, name: &str, owner: bool) -> Team {
    Team {
        id: id.to_owned(),
        name: name.to_owned(),
        group_id: Some(GROUP.to_owned()),
        owner,
        can_add_channel: true,
        about: TeamAbout {
            description: "Made up".to_owned(),
            people: 3,
            ..TeamAbout::default()
        },
        channels: vec![TeamChannel {
            id: id.to_owned(),
            name: "General".to_owned(),
            general: true,
            kind: ChannelKind::Standard,
            shown: true,
        }],
        ..Team::default()
    }
}

fn shell_window(rig: &Rig, teams: Vec<Team>) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    *rig.core.teams.lock().unwrap() = Some(teams);
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

fn open_page(shell: &Entity<Shell>, cx: &mut VisualTestContext) {
    click(cx, "see-all-teams");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::TeamsPage);
}

fn open_row_menu(cx: &mut VisualTestContext, row: usize) {
    let found = bounds(cx, &format!("teams-row-{row}")).expect("the team's row");
    cx.simulate_mouse_move(found.center(), None, Modifiers::none());
    click(cx, &format!("teams-row-more-{row}"));
}

#[test]
fn see_all_your_teams_lists_them_as_teams_does_and_a_row_opens_manage_team() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true), team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    assert!(bounds(cx, "teams-page-title").is_some());
    assert!(bounds(cx, "teams-row-0").is_some());
    assert!(bounds(cx, "teams-row-1").is_some());
    click(cx, "teams-filter-owned");
    assert!(bounds(cx, "teams-row-1").is_none(), "only the team the account owns");
    click(cx, "teams-filter-all");
    click(cx, "teams-row-1");
    assert_eq!(
        cx.update(|_, cx| shell.read(cx).main().clone()),
        Main::ManageTeam(MEMBER_OF.to_owned())
    );
    assert!(bounds(cx, "manage-tab-settings").is_none(), "a member has no settings");
    assert!(bounds(cx, "manage-tab-members").is_some());
}

#[test]
fn a_member_isnt_offered_what_only_an_owner_can_do() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    assert!(bounds(cx, "team-manage-0").is_some());
    assert!(bounds(cx, "team-leave-0").is_some());
    assert!(bounds(cx, "team-delete-0").is_none());
    assert!(bounds(cx, "team-archive-0").is_none());
}

#[test]
fn delete_team_asks_first_and_goes_out_only_after_its_tick() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten::default());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    click(cx, "team-delete-0");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<ConfirmDialog>().ok())
        .expect("the dialog");
    assert!(!cx.update(|_, cx| dialog.read(cx).can_confirm()));
    cx.update(|_, cx| dialog.update(cx, teams_ui::team_dialogs::ConfirmDialog::confirm));
    cx.run_until_parked();
    assert!(rig.core.team_writes.lock().unwrap().is_empty(), "nothing goes unticked");
    cx.update(|_, cx| dialog.update(cx, |dialog, cx| dialog.set_ticked(true, cx)));
    click(cx, "team-confirm-ok");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(matches!(writes.as_slice(), [(TeamWrite::Delete { team }, _)] if team == OWNED));
    drop(writes);
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

#[test]
fn hide_all_channels_is_the_accounts_own_view() {
    let rig = Rig::start();
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    open_page(&shell, cx);
    open_row_menu(cx, 0);
    click(cx, "team-hide-all-0");
    click(cx, "team-confirm-ok");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    assert_eq!(
        *rig.core.team_views.lock().unwrap(),
        [OwnTeamView::HideAll { team: OWNED.to_owned() }]
    );
}

#[test]
fn create_team_makes_a_private_team_by_default_and_goes_on_to_add_members() {
    let rig = Rig::start();
    *rig.core.team_written.lock().unwrap() = Some(TeamWritten {
        created: Some((OWNED.to_owned(), Some(GROUP.to_owned()))),
        not_added: Vec::new(),
    });
    let (shell, cx) = shell_window(&rig, vec![team(MEMBER_OF, "Beta", false)]);
    open_page(&shell, cx);
    click(cx, "teams-create");
    let dialog = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<CreateTeamDialog>().ok())
        .expect("the dialog");
    assert!(!cx.update(|_, cx| dialog.read(cx).can_create(cx)));
    cx.update(|window, cx| {
        dialog.update(cx, |dialog, cx| {
            dialog.fill("New team", "About it", "Planning", window, cx);
        });
    });
    click(cx, "create-team-create");
    {
        let writes = rig.core.team_writes.lock().unwrap();
        let [(TeamWrite::Create(new), _)] = writes.as_slice() else {
            panic!("one new team");
        };
        assert_eq!(new.name, "New team");
        assert!(!new.public);
    }
    let next = cx
        .update(|_, cx| shell.read(cx).team_dialog().cloned())
        .and_then(|dialog| dialog.downcast::<AddMembersDialog>().ok());
    assert!(next.is_some(), "its Add members step comes next");
    assert!(bounds(cx, "add-members-cancel").is_some());
}

#[test]
fn a_settings_box_writes_the_whole_of_the_teams_settings() {
    let rig = Rig::start();
    *rig.core.team_settings.lock().unwrap() =
        Some(r#"{"userSettings":{"createTopic":true,"updateTopic":true,"other":7}}"#.to_owned());
    let (shell, cx) = shell_window(&rig, vec![team(OWNED, "Alpha", true)]);
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.open_manage_team(OWNED.to_owned(), ManageTab::Settings, window, cx);
        });
    });
    click(cx, "manage-section-members");
    click(cx, "create-update-channel");
    let writes = rig.core.team_writes.lock().unwrap();
    let [(TeamWrite::Settings { team, settings }, _)] = writes.as_slice() else {
        panic!("one settings write: {writes:?}");
    };
    assert_eq!(team, OWNED);
    let written: serde_json::Value = serde_json::from_str(settings).expect("JSON");
    assert_eq!(written["userSettings"]["createTopic"], false);
    assert_eq!(written["userSettings"]["updateTopic"], false);
    assert_eq!(written["userSettings"]["other"], 7);
}
