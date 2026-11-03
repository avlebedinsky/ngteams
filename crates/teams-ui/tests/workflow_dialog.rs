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
        about: teams_store::ChannelAbout::default(),
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

#[test]
fn an_old_catalog_reply_cannot_replace_a_new_refresh() {
    let (rig, view, cx) = window();
    *rig.core.hold_workflow_catalog.lock().unwrap() = true;
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    let old = rig.core.held_workflow_catalog.lock().unwrap().pop().unwrap();
    *rig.core.hold_workflow_catalog.lock().unwrap() = false;
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    old.send(Ok(Ok(
        serde_json::json!({"value":[{"id":"stale","name":"Stale","description":"Synthetic"}]}),
    )));
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    assert!(bounds(cx, "workflow-template-stale").is_none());
}

#[test]
fn unknown_write_outcomes_are_checked_without_automatic_replay() {
    fn uncertain(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if request.writes() {
            Err(teams_proto::WorkflowFailure::Connection)
        } else {
            answer(request)
        }
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(uncertain);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-toggle");
    click(cx, "workflow-toggle");
    assert!(bounds(cx, "workflow-reconcile").is_some());
    click(cx, "workflow-reconcile");
    assert_eq!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.writes())
            .count(),
        1
    );
    assert!(bounds(cx, "workflow-reconcile").is_none());
}

#[test]
fn read_only_managers_offer_history_without_mutating_actions() {
    let (rig, view, cx) = window();
    *rig.core.read_only.lock().unwrap() = true;
    view.update(cx, |_, cx| cx.notify());
    assert!(bounds(cx, "workflow-build").is_none());
    click(cx, "workflow-open-synthetic-flow");
    assert!(bounds(cx, "workflow-toggle").is_none());
    assert!(bounds(cx, "workflow-delete").is_none());
    click(cx, "workflow-history");
    assert!(bounds(cx, "workflow-run-synthetic-run").is_some());
    click(cx, "workflow-run-synthetic-run");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request, WorkflowRequest::RunActions{flow,run,..} if flow=="synthetic-flow" && run=="synthetic-run")));
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

#[test]
fn existing_edits_preserve_original_destinations_and_unknown_definition_nodes() {
    fn original_destination(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        let mut result = answer(request)?;
        if matches!(request, WorkflowRequest::Get { .. }) {
            result["properties"]["definition"]["parameters"]["teams.channel"]["defaultValue"] =
                serde_json::json!("another-synthetic-channel");
            result["properties"]["definition"]["metadata"] =
                serde_json::json!({"synthetic-unknown-node":{"preserved":true}});
        }
        Ok(result)
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(original_destination);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-edit");
    click(cx, "workflow-save");
    let requests = rig.core.workflow_requests.lock().unwrap();
    let properties = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Update { properties, .. } => Some(properties),
            _ => None,
        })
        .expect("the edit uses the native update operation");
    assert_eq!(
        properties
            .pointer("/definition/parameters/teams.channel/defaultValue")
            .unwrap(),
        "another-synthetic-channel"
    );
    assert_eq!(
        properties
            .pointer("/definition/metadata/synthetic-unknown-node/preserved")
            .unwrap(),
        true
    );
}

#[test]
fn required_trigger_inputs_are_native_and_missing_inputs_never_start_a_run() {
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::input_answer);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-run");
    assert!(bounds(cx, "workflow-run-input-text").is_some());
    click(cx, "workflow-run-submit");
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    click(cx, "workflow-run-input-text");
    cx.simulate_input("Synthetic reminder");
    click(cx, "workflow-run-submit");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request|matches!(request,WorkflowRequest::RunWithInputs {environment,flow,trigger,inputs} if environment=="Default-synthetic" && flow=="synthetic-flow" && trigger=="manual" && inputs["text"]=="Synthetic reminder")));
}
