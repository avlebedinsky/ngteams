//! Synthetic native workflow interactions. No real identities, credentials, external connections or tenant writes.
#![cfg(test)]
#[expect(dead_code, reason = "shared fake engine helpers used by other view tests")]
mod common;
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use std::sync::Arc;
use teams_proto::WorkflowRequest;
use teams_store::{Team, TeamChannel};
use teams_ui::workflow_dialog::WorkflowDialog;

#[path = "support/workflow_fixtures.rs"]
mod fixtures;
use fixtures::answer;
fn window() -> (common::Rig, Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    let rig = common::Rig::start();
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let team = Team {
        id: "synthetic-team".into(),
        group_id: Some("synthetic-group".into()),
        name: "Synthetic team".into(),
        ..Team::default()
    };
    let channel = TeamChannel {
        id: "synthetic-channel".into(),
        name: "General".into(),
        general: false,
        kind: teams_store::ChannelKind::Standard,
        shown: true,
    };
    let (view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new(core, team, channel, window, cx));
    cx.run_until_parked();
    (rig, view, cx)
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

#[test]
fn templates_are_real_cards_and_configuration_is_native() {
    let (_rig, _view, cx) = window();
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    click(cx, "workflow-template-synthetic-template");
    assert!(bounds(cx, "workflow-name").is_some());
    assert!(bounds(cx, "workflow-save").is_some());
    assert!(
        bounds(cx, "workflow-definition").is_none(),
        "definition is an optional advanced view"
    );
}
#[test]
fn lifecycle_actions_keep_the_opened_account_environment_and_workflow() {
    let (rig, _view, cx) = window();
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-toggle");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request,WorkflowRequest::SetEnabled { environment,flow,enabled:false } if environment=="Default-synthetic" && flow=="synthetic-flow")));
}
#[test]
fn delete_requires_its_own_confirmation_and_history_is_a_read() {
    let (rig, _view, cx) = window();
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-delete");
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    click(cx, "workflow-delete-confirm");
    assert!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request,WorkflowRequest::Delete { flow,.. } if flow=="synthetic-flow"))
    );
}
