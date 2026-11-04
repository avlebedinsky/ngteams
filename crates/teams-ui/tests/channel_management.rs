//! Synthetic channel menu/management interactions. No tenant or network writes occur in this test process.
#![cfg(test)]
#[expect(dead_code, reason = "shared view-test rig")]
mod common;
use common::{LAB_A, LAB_B, Rig};
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use teams_proto::{ChannelChange, ChannelData, ChannelSectionAction, OwnTeamView, TeamWrite};
use teams_store::{ChannelKind, Team, TeamChannel};
use teams_ui::{
    ViewTrace,
    channel_menu::ChannelChoice,
    settings::{Settings, SettingsModel},
    shell::{Shell, ShellParts},
};
const TEAM: &str = "19:synthetic-team@thread.tacv2";
const CHANNEL: &str = "19:synthetic-channel@thread.tacv2";
fn team() -> Team {
    Team {
        id: TEAM.to_owned(),
        name: "Synthetic team".to_owned(),
        owner: true,
        group_id: Some("synthetic-group".to_owned()),
        channels: vec![
            TeamChannel {
                id: TEAM.to_owned(),
                name: "General".to_owned(),
                general: true,
                kind: ChannelKind::Standard,
                shown: true,
                about: teams_store::ChannelAbout::default(),
            },
            TeamChannel {
                id: CHANNEL.to_owned(),
                name: "Synthetic channel".to_owned(),
                general: false,
                kind: ChannelKind::Standard,
                shown: true,
                about: teams_store::ChannelAbout::default(),
            },
        ],
        ..Team::default()
    }
}
fn shell(rig: &Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    with_team(rig, team())
}
fn with_team(rig: &Rig, team: Team) -> (Entity<Shell>, &'static mut VisualTestContext) {
    rig.cx.update(teams_ui::window::init);
    *rig.core.teams.lock().unwrap() = Some(vec![team]);
    *rig.core.team_written.lock().unwrap() = Some(teams_core::TeamWritten::default());
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::in_memory(Settings::default()))),
        trace: Arc::new(ViewTrace::off()),
    };
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (view, cx)
}
fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<gpui::Bounds<gpui::Pixels>> {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}
fn click(cx: &mut VisualTestContext, selector: &str) {
    let b = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} drawn"));
    cx.simulate_click(b.center(), Modifiers::none());
    cx.run_until_parked();
}
fn open(shell: &Entity<Shell>, choice: ChannelChoice, cx: &mut VisualTestContext) {
    let team = team();
    let channel = team.channels[1].clone();
    cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.channel_chosen(&team, &channel, choice, window, cx);
        });
    });
    cx.run_until_parked();
}
#[test]
fn ellipsis_offers_reference_actions_and_does_not_open_the_channel() {
    let rig = Rig::start();
    let (shell, cx) = shell(&rig);
    click(cx, "channel-more-1");
    for action in [
        "notifications",
        "hide",
        "edit",
        "manage",
        "copy-link",
        "email",
        "workflows",
        "archive",
        "delete",
    ] {
        assert!(bounds(cx, &format!("channel-{action}-1")).is_some(), "{action} shown");
    }
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    assert!(rig.core.synced.lock().unwrap().is_empty());
    click(cx, "channel-edit-1");
    assert!(bounds(cx, "channel-management-name").is_some());
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_some()));
}
#[test]
fn archive_and_delete_are_confirmed_and_cancel_writes_nothing() {
    let rig = Rig::start();
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Archive, cx);
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    click(cx, "channel-management-cancel");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    open(&shell, ChannelChoice::Delete, cx);
    click(cx, "channel-management-save");
    assert!(
        rig.core.team_writes.lock().unwrap().is_empty(),
        "delete requires acknowledgement"
    );
    click(cx, "channel-delete-confirm");
    click(cx, "channel-management-save");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0,TeamWrite::Channel(write)if write.target.channel==CHANNEL&&write.action==ChannelChange::Delete)
    );
}
#[test]
fn notification_choices_reach_own_properties_and_failed_reads_cannot_be_saved() {
    let rig = Rig::start();
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Notifications, cx);
    click(cx, "channel-notification-muted");
    click(cx, "channel-management-save");
    let views = rig.core.team_views.lock().unwrap();
    assert!(views.iter().any(|request|matches!(request,OwnTeamView::ChannelProperty{channel,property:teams_proto::ChannelOwnProperty::Alerts,value}if channel==CHANNEL&&value=="false")));
    drop(views);
    rig.core
        .channel_data_errors
        .lock()
        .unwrap()
        .insert(format!("preferences:{CHANNEL}"));
    open(&shell, ChannelChoice::Notifications, cx);
    assert!(bounds(cx, "channel-management-error").is_some());
    let before = rig.core.team_views.lock().unwrap().len();
    click(cx, "channel-management-save");
    assert_eq!(rig.core.team_views.lock().unwrap().len(), before);
}
#[test]
fn read_only_menu_and_dialog_never_offer_writes() {
    let rig = Rig::start();
    *rig.core.read_only.lock().unwrap() = true;
    let (shell, cx) = shell(&rig);
    click(cx, "channel-more-1");
    assert!(bounds(cx, "channel-copy-link-1").is_some());
    assert!(bounds(cx, "channel-delete-1").is_none());
    click(cx, "channel-manage-1");
    assert!(bounds(cx, "channel-management-save").is_none());
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    cx.simulate_keystrokes("escape");
    assert!(cx.update(|_, cx| shell.read(cx).team_dialog().is_none()));
}

fn type_in(cx: &mut VisualTestContext, selector: &str, text: &str) {
    click(cx, selector);
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input(text);
    cx.run_until_parked();
}

fn member(mri: &str, owner: bool) -> teams_core::TeamMember {
    teams_core::TeamMember {
        mri: mri.into(),
        name: if owner { "Synthetic owner" } else { "Synthetic member" }.into(),
        email: None,
        title: None,
        location: None,
        owner,
        guest: false,
        moderator: false,
    }
}
fn private_team() -> Team {
    let mut team = team();
    team.about.tenant = "synthetic-tenant".into();
    team.channels[1].kind = ChannelKind::Private;
    team.channels[1].about.owner = true;
    team.channels[1].about.group = Some("synthetic-private-group".into());
    team
}

#[test]
fn moderation_toggles_and_notification_options_reach_their_domain_operations() {
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![member(LAB_A, true), member(LAB_B, false)];
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Manage, cx);
    for selector in [
        "channel-moderation-on",
        "channel-moderation-replies",
        "channel-moderation-pins",
        "channel-moderation-bots",
        "channel-moderation-connectors",
    ] {
        click(cx, selector);
    }
    click(cx, "channel-management-save");
    let writes = rig.core.team_writes.lock().unwrap();
    let TeamWrite::Channel(write) = &writes[0].0 else {
        panic!("channel settings")
    };
    let ChannelChange::Settings(settings) = &write.action else {
        panic!("moderation settings")
    };
    assert_eq!(settings["postPermissions"], 1);
    assert_eq!(settings["allowReplies"], 1);
    assert_eq!(settings["allowPinPosts"], 1);
    assert_eq!(settings["allowBotsPost"], 0);
    assert_eq!(settings["allowConnectorsPost"], 0);
    drop(writes);
    open(&shell, ChannelChoice::Notifications, cx);
    click(cx, "channel-notification-all");
    for selector in [
        "channel-notification-follow",
        "channel-notification-mentions",
        "channel-notification-tags",
        "channel-notification-desktop",
    ] {
        click(cx, selector);
    }
    click(cx, "channel-management-save");
    assert!(!rig.core.team_views.lock().unwrap().is_empty());
}

#[test]
fn editing_a_private_channel_keeps_its_provisioning_identity_and_validates_user_input() {
    let rig = Rig::start();
    let (shell, cx) = with_team(&rig, private_team());
    open(&shell, ChannelChoice::Edit, cx);
    type_in(cx, "channel-management-name", "General");
    assert!(bounds(cx, "channel-management-validation").is_some());
    click(cx, "channel-management-save");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    type_in(cx, "channel-management-name", "  Release planning  ");
    type_in(cx, "channel-management-description", "A synthetic description");
    click(cx, "channel-management-save");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::Channel(write) if write.target.channel_group.as_deref() == Some("OID:synthetic-private-group@synthetic-tenant") && matches!(&write.action, ChannelChange::Edit { name, description } if name == "Release planning" && description == "A synthetic description"))
    );
}

#[test]
fn email_domains_are_validated_then_saved_without_sending_mail_and_address_can_be_copied() {
    let rig = Rig::start();
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Email, cx);
    click(cx, "channel-email-copy");
    assert_eq!(
        rig.cx.read(|cx| cx.read_from_clipboard().unwrap().text().unwrap()),
        "synthetic-channel@example.invalid"
    );
    click(cx, "channel-email-settings");
    click(cx, "channel-email-allowed-anyone");
    click(cx, "channel-email-allowed-members");
    click(cx, "channel-email-allowed-domains");
    type_in(cx, "channel-email-domains", "https://invalid.example");
    click(cx, "channel-management-save");
    assert!(rig.core.team_writes.lock().unwrap().is_empty());
    assert!(bounds(cx, "channel-management-validation").is_some());
    type_in(cx, "channel-email-domains", "EXAMPLE.TEST; another.example");
    click(cx, "channel-management-save");
    let writes = rig.core.team_writes.lock().unwrap();
    assert!(
        matches!(&writes[0].0, TeamWrite::Channel(write) if matches!(&write.action, ChannelChange::UpdateEmail { allowed_sender_type, allowed_domains } if allowed_sender_type == "domains" && allowed_domains == &["example.test", "another.example"]))
    );
}

#[test]
fn email_create_refresh_remove_and_service_refusal_are_explicit_user_actions() {
    let rig = Rig::start();
    rig.core
        .channel_data
        .lock()
        .unwrap()
        .insert(format!("email:{CHANNEL}"), ChannelData(serde_json::json!({})));
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Email, cx);
    assert!(bounds(cx, "channel-email-create").is_some());
    rig.core
        .channel_data
        .lock()
        .unwrap()
        .remove(&format!("email:{CHANNEL}"));
    click(cx, "channel-email-create");
    assert!(
        bounds(cx, "channel-email-copy").is_some(),
        "creation refreshes the returned address"
    );
    click(cx, "channel-email-remove");
    assert_eq!(rig.core.team_writes.lock().unwrap().len(), 2);
    assert!(
        matches!(&rig.core.team_writes.lock().unwrap()[1].0, TeamWrite::Channel(write) if write.action == ChannelChange::RemoveEmail)
    );
    *rig.core.team_written.lock().unwrap() = None;
    rig.core.channel_data.lock().unwrap().insert(
        format!("email:{CHANNEL}"),
        ChannelData(serde_json::json!({"errorCode":"NotFound"})),
    );
    open(&shell, ChannelChoice::Email, cx);
    assert!(bounds(cx, "channel-management-error").is_some());
    let writes = rig.core.team_writes.lock().unwrap().len();
    cx.run_until_parked();
    assert_eq!(
        rig.core.team_writes.lock().unwrap().len(),
        writes,
        "failed creation is not automatically retried"
    );
    rig.core
        .channel_data
        .lock()
        .unwrap()
        .remove(&format!("email:{CHANNEL}"));
    click(cx, "channel-management-reload");
    assert!(bounds(cx, "channel-email-copy").is_some());
    assert_eq!(rig.core.team_writes.lock().unwrap().len(), writes);
}

#[test]
fn private_members_search_add_roles_and_confirmed_removal_use_the_channel_group() {
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![member(LAB_A, true), member(LAB_B, false)];
    *rig.core.team_found.lock().unwrap() = vec![teams_proto::TeamPerson {
        mri: "8:orgid:00000000-0000-4000-8000-00000000000c".into(),
        name: "Synthetic new person".into(),
        email: None,
        job_title: None,
    }];
    let (shell, cx) = with_team(&rig, private_team());
    open(&shell, ChannelChoice::Manage, cx);
    assert!(
        bounds(cx, "channel-member-remove-0").is_none(),
        "last owner is protected"
    );
    type_in(cx, "channel-member-search", "Synthetic");
    click(cx, "channel-member-find");
    click(cx, "channel-member-add-0");
    click(cx, "channel-member-role-1");
    click(cx, "channel-member-remove-1");
    click(cx, "channel-member-remove-cancel");
    let before = rig.core.team_writes.lock().unwrap().len();
    click(cx, "channel-member-remove-1");
    click(cx, "channel-member-remove-confirm");
    let writes = rig.core.team_writes.lock().unwrap();
    assert_eq!(writes.len(), before + 1);
    assert!(writes.iter().all(|(write,_)| matches!(write, TeamWrite::Channel(write) if write.target.channel_group.as_deref() == Some("OID:synthetic-private-group@synthetic-tenant"))));
    assert!(writes.iter().any(|(write,_)| matches!(write, TeamWrite::Channel(write) if matches!(&write.action, ChannelChange::UpdateMember { mri, owner:true } if mri == LAB_B))));
}

#[test]
fn failed_member_changes_remain_visible_and_refresh_never_repeats_the_write() {
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![member(LAB_A, true), member(LAB_B, false)];
    let (shell, cx) = with_team(&rig, private_team());
    *rig.core.team_written.lock().unwrap() = None;
    open(&shell, ChannelChoice::Manage, cx);
    click(cx, "channel-member-role-1");
    assert!(bounds(cx, "channel-management-error").is_some());
    click(cx, "channel-management-reload");
    assert_eq!(rig.core.team_writes.lock().unwrap().len(), 1);
    click(cx, "channel-management-close");
    *rig.core.read_only.lock().unwrap() = true;
    open(&shell, ChannelChoice::Manage, cx);
    assert!(bounds(cx, "channel-member-role-1").is_none());
    assert!(bounds(cx, "channel-member-remove-1").is_none());
}

#[test]
fn standard_channel_moderators_and_custom_sections_are_account_scoped() {
    let rig = Rig::start();
    *rig.core.team_roster.lock().unwrap() = vec![member(LAB_A, true), member(LAB_B, false)];
    let (shell, cx) = shell(&rig);
    open(&shell, ChannelChoice::Manage, cx);
    click(cx, "channel-manage-members");
    click(cx, "channel-member-moderator-1");
    assert!(
        matches!(&rig.core.team_writes.lock().unwrap()[0].0, TeamWrite::Channel(write) if matches!(&write.action, ChannelChange::Moderator { mri, moderator:true } if mri == LAB_B))
    );
    click(cx, "channel-manage-settings");
    click(cx, "channel-management-close");
    open(&shell, ChannelChoice::NewSection, cx);
    type_in(cx, "channel-management-name", "  Release work  ");
    click(cx, "channel-management-save");
    assert!(
        matches!(&rig.core.section_actions.lock().unwrap()[0], ChannelSectionAction::Create { channel, name } if channel == CHANNEL && name == "Release work")
    );
}

fn section_rig() -> Rig {
    let rig = Rig::start();
    rig.core.channel_data.lock().unwrap().insert("sections".into(), ChannelData(serde_json::json!({
        "isPartialResponse":false,"folderHierarchyVersion":7,"conversationFolderOrder":["favorites","release"],
        "conversationFolders":[
            {"id":"favorites","name":"Favorites","folderType":"Favorites","conversationFolderItems":[]},
            {"id":"release","name":"Release work","folderType":"UserDefined","isExpanded":true,"conversationFolderItems":[{"conversationId":CHANNEL}]}
        ]
    })));
    rig
}

#[test]
fn custom_section_pointer_and_keyboard_folding_persist_the_choice_and_failed_writes_roll_back() {
    let rig = section_rig();
    let (shell, cx) = shell(&rig);
    assert!(bounds(cx, "channel-section-release").is_some());
    assert!(bounds(cx, "channel-row-1").is_some());
    click(cx, "channel-section-release");
    assert!(bounds(cx, "channel-row-1").is_none());
    click(cx, "channel-section-release");
    assert!(bounds(cx, "channel-row-1").is_some());
    assert!(
        matches!(rig.core.section_actions.lock().unwrap().last(), Some(ChannelSectionAction::Expanded { section, expanded:true }) if section == "release")
    );
    *rig.core.stopped.lock().unwrap() = true;
    click(cx, "channel-section-release");
    assert!(
        bounds(cx, "channel-row-1").is_some(),
        "a refused preference restores the displayed expansion"
    );
    *rig.core.stopped.lock().unwrap() = false;
    let sidebar = cx.update(|_, cx| shell.read(cx).sidebar().clone());
    cx.update(|window, cx| sidebar.update(cx, |sidebar, cx| sidebar.focus_list(window, cx)));
    cx.simulate_keystrokes("home left");
    assert!(bounds(cx, "channel-row-1").is_none());
    cx.simulate_keystrokes("right");
    assert!(bounds(cx, "channel-row-1").is_some());
    let writes = rig.core.section_actions.lock().unwrap().len();
    *rig.core.read_only.lock().unwrap() = true;
    click(cx, "channel-section-release");
    assert!(bounds(cx, "channel-row-1").is_none());
    assert_eq!(
        rig.core.section_actions.lock().unwrap().len(),
        writes,
        "read-only folding is local"
    );
}

fn open_move_menu(cx: &mut VisualTestContext) {
    click(cx, "channel-more-1");
    cx.simulate_keystrokes("down down right");
    if bounds(cx, "channel-section-1-0").is_none() {
        cx.simulate_keystrokes("left");
    }
    assert!(bounds(cx, "channel-section-1-0").is_some());
}
#[test]
fn channel_move_submenu_preserves_current_section_and_moves_only_on_an_explicit_other_choice() {
    let rig = section_rig();
    let (_shell, cx) = shell(&rig);
    open_move_menu(cx);
    click(cx, "channel-section-1-1");
    assert!(
        rig.core.section_actions.lock().unwrap().is_empty(),
        "current section is no write"
    );
    open_move_menu(cx);
    click(cx, "channel-section-1-0");
    assert!(
        matches!(rig.core.section_actions.lock().unwrap().last(), Some(ChannelSectionAction::Move { channel, section }) if channel == CHANNEL && section == "favorites")
    );
    assert!(
        rig.core.team_writes.lock().unwrap().is_empty(),
        "a section move is an own preference"
    );
}
