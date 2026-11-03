//! Synthetic channel menu/management interactions. No tenant or network writes occur in this test process.
#![cfg(test)]
#[expect(dead_code, reason = "shared view-test rig")]
mod common;
use common::Rig;
use gpui::{AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use teams_proto::{ChannelChange, OwnTeamView, TeamWrite};
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
    rig.cx.update(teams_ui::window::init);
    *rig.core.teams.lock().unwrap() = Some(vec![team()]);
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
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.channel_chosen(team, channel, choice, window, cx)));
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
