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

fn managed_window() -> (common::Rig, Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    let (rig, view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    (rig, view, cx)
}
fn choose_connection(cx: &mut VisualTestContext, index: usize) {
    click(cx, &format!("workflow-connection-{index}"));
    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
}

#[test]
fn connector_accounts_must_be_chosen_and_are_bound_to_the_actual_saved_definition() {
    let (rig, _view, cx) = managed_window();
    click(cx, "workflow-template-synthetic-template");
    click(cx, "workflow-save");
    assert!(bounds(cx, "workflow-error").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-connections-refresh");
    click(cx, "workflow-save");
    let requests = rig.core.workflow_requests.lock().unwrap();
    let properties = requests
        .iter()
        .find_map(|request| match request {
            WorkflowRequest::Create { properties, .. } => Some(properties),
            _ => None,
        })
        .unwrap();
    for key in ["legacy", "teams"] {
        assert_eq!(
            properties["connectionReferences"][key]["connectionName"],
            "synthetic-connection"
        );
        assert_eq!(
            properties["connectionReferences"][key]["id"],
            "/providers/Microsoft.PowerApps/apis/shared_teams"
        );
    }
}

#[test]
fn connection_sign_in_accepts_only_the_service_issued_microsoft_consent_host() {
    fn unsafe_consent(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if matches!(request, WorkflowRequest::ConnectionConsent { .. }) {
            Ok(serde_json::json!({"consentLink":"https://outside.example.test/collect"}))
        } else {
            managed_answer(request)
        }
    }
    let (rig, _view, cx) = managed_window();
    click(cx, "workflow-template-synthetic-template");
    *rig.core.workflow_handler.lock().unwrap() = Some(unsafe_consent);
    click(cx, "workflow-connect-0");
    assert!(bounds(cx, "workflow-error").is_some());
    let before = rig.core.workflow_requests.lock().unwrap().len();
    cx.run_until_parked();
    assert_eq!(
        rig.core.workflow_requests.lock().unwrap().len(),
        before,
        "consent errors do not replay creation"
    );
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    click(cx, "workflow-connect-0");
    assert!(rig.core.workflow_requests.lock().unwrap().iter().any(|request| matches!(request, WorkflowRequest::ConnectionConsent { environment, connector, .. } if environment == "Default-synthetic" && connector == "shared_teams")));
    assert_eq!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::CreateConnection { .. }))
            .count(),
        2
    );
}

#[test]
fn failed_connections_can_be_refreshed_and_a_disconnected_saved_reference_cannot_be_reused() {
    fn unavailable(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        if matches!(request, WorkflowRequest::Connections { .. }) {
            return Err(teams_proto::WorkflowFailure::Forbidden);
        }
        let mut value = managed_answer(request)?;
        if matches!(request, WorkflowRequest::Template { .. }) {
            value["properties"]["connectionReferences"]["legacy"]["connectionName"] = serde_json::json!("disconnected");
        }
        Ok(value)
    }
    let (rig, view, cx) = managed_window();
    *rig.core.workflow_handler.lock().unwrap() = Some(unavailable);
    view.update(cx, WorkflowDialog::refresh);
    cx.run_until_parked();
    click(cx, "workflow-template-synthetic-template");
    click(cx, "workflow-save");
    assert!(bounds(cx, "workflow-error").is_some());
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
    *rig.core.workflow_handler.lock().unwrap() = Some(managed_answer);
    click(cx, "workflow-connections-refresh");
    choose_connection(cx, 0);
    choose_connection(cx, 1);
    click(cx, "workflow-save");
    assert!(
        rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Create { .. }))
    );
}

fn catalog_page(cx: &mut VisualTestContext, next: bool) {
    // The kit icon Buttons have no selector wrapper; their centers follow the visible templates button.
    let heading = bounds(cx, "workflow-all-templates").unwrap();
    cx.simulate_click(
        gpui::point(
            heading.origin.x + heading.size.width + gpui::px(if next { 64.0 } else { 24.0 }),
            heading.center().y,
        ),
        Modifiers::none(),
    );
    cx.run_until_parked();
}

#[test]
fn discovery_paging_search_home_and_environment_changes_keep_the_selected_actor_scope() {
    let (rig, _view, cx) = managed_window();
    catalog_page(cx, true);
    assert!(bounds(cx, "workflow-template-synthetic-extra").is_some());
    catalog_page(cx, false);
    assert!(bounds(cx, "workflow-template-synthetic-template").is_some());
    click(cx, "workflow-all-templates");
    click(cx, "workflow-search");
    cx.simulate_input("Additional");
    assert!(bounds(cx, "workflow-template-synthetic-extra").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-template").is_none());
    click(cx, "workflow-tab-ai");
    assert!(
        bounds(cx, "workflow-open-synthetic-flow").is_none(),
        "no license is presented as no matching AI workflow"
    );
    click(cx, "workflow-tab-all");
    click(cx, "workflow-home");
    click(cx, "workflow-environment");
    cx.simulate_keystrokes("down down enter");
    cx.run_until_parked();
    let requests = rig.core.workflow_requests.lock().unwrap();
    assert!(
        requests.iter().any(
            |request| matches!(request, WorkflowRequest::List { environment } if environment == "Other-synthetic")
        )
    );
    assert!(requests.iter().any(
        |request| matches!(request, WorkflowRequest::Connections { environment } if environment == "Other-synthetic")
    ));
}

#[test]
fn each_native_builder_uses_its_template_and_advanced_definition_edit_remains_optional() {
    for (selector, expected) in [
        ("workflow-build-manual", "805739fdad754e799502b6379900922e"),
        ("workflow-build-scheduled", "805739fdad754e799502b6379900922e"),
        ("workflow-build-webhook", "d271a6f01c2545a28348d8f2cddf4c8f"),
    ] {
        let (rig, _view, cx) = window();
        click(cx, "workflow-build");
        click(cx, selector);
        assert!(
            rig.core
                .workflow_requests
                .lock()
                .unwrap()
                .iter()
                .any(|request| matches!(request, WorkflowRequest::Template { id } if id == expected))
        );
        click(cx, "workflow-definition-toggle");
        assert!(bounds(cx, "workflow-definition").is_some());
        click(cx, "workflow-definition-toggle");
        assert!(bounds(cx, "workflow-definition").is_none());
        click(cx, "workflow-edit-cancel");
        assert!(bounds(cx, "workflow-build").is_some());
        assert!(
            !rig.core
                .workflow_requests
                .lock()
                .unwrap()
                .iter()
                .any(WorkflowRequest::writes)
        );
    }
}

#[test]
fn copying_a_webhook_reads_only_its_address_and_never_invokes_or_runs_it() {
    fn webhook(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
        let mut value = answer(request)?;
        match request {
            WorkflowRequest::Get { .. } => {
                value["properties"]["definition"]["triggers"] =
                    serde_json::json!({"manual":{"type":"Request","kind":"TeamsWebhookV2"}});
            }
            WorkflowRequest::WebhookAddress { .. } => {
                value =
                    serde_json::json!({"response":{"value":"https://synthetic.example.test/service-issued-webhook"}});
            }
            _ => {}
        }
        Ok(value)
    }
    let (rig, _view, cx) = window();
    *rig.core.workflow_handler.lock().unwrap() = Some(webhook);
    click(cx, "workflow-open-synthetic-flow");
    click(cx, "workflow-copy-webhook");
    assert_eq!(
        rig.cx.read(|cx| cx.read_from_clipboard().and_then(|item| item.text())),
        Some("https://synthetic.example.test/service-issued-webhook".into())
    );
    assert!(
        !rig.core
            .workflow_requests
            .lock()
            .unwrap()
            .iter()
            .any(WorkflowRequest::writes)
    );
}

/// Discovery and connector answers used to exercise native account binding and environment navigation.
fn managed_answer(request: &WorkflowRequest) -> teams_proto::WorkflowResponse {
    let mut value = answer(request)?;
    match request {
        WorkflowRequest::Catalog => {
            value["value"].as_array_mut().expect("the synthetic catalog is an array").push(serde_json::json!({"id":"synthetic-extra","name":"Additional synthetic template","description":"A seventh native template"}));
        }
        WorkflowRequest::Environments => {
            value = serde_json::json!({"value":[
                {"name":"Default-synthetic","properties":{"displayName":"Synthetic environment"}},
                {"name":"Other-synthetic","properties":{"displayName":"Other synthetic environment"}}
            ]});
        }
        WorkflowRequest::Connections { .. } => {
            value = serde_json::json!({"value":[
                {"name":"synthetic-connection","properties":{"displayName":"Synthetic connected account","apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}},
                {"name":"disconnected","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Disconnected"}]}},
                {"properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams","statuses":[{"status":"Connected"}]}},
                {"name":"unrelated","properties":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_other","statuses":[{"status":"Connected"}]}}
            ]});
        }
        WorkflowRequest::Template { .. } | WorkflowRequest::Get { .. } => {
            value["properties"]["connectionReferences"] = serde_json::json!({
                "legacy":{"id":"/providers/Microsoft.PowerApps/apis/shared_teams"},
                "teams":{"api":{"name":"shared_teams","properties":{"displayName":"Microsoft Teams"}}}
            });
            value["properties"]["lastModifiedTime"] = serde_json::json!("synthetic-version");
            if let WorkflowRequest::Get { flow, .. } = request {
                value["name"] = serde_json::json!(flow);
            }
        }
        WorkflowRequest::CreateConnection { .. } => value = serde_json::json!({}),
        WorkflowRequest::ConnectionConsent { .. } => {
            value = serde_json::json!({"consentLink":"https://global.consent.azure-apim.net/redirect/teams?state=synthetic"});
        }
        _ => {}
    }
    Ok(value)
}
