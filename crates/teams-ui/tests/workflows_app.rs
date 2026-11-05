//! The Workflows app in the main window over synthetic data, as browser Teams has it: Home's featured templates eight
//! at a time and its suggestions, Templates by kind and app, "Your workflows" by state, a template opened with no team
//! chosen, and the shell's ways in (Workflows Home, the rail's More apps) and its Chat tab, the Workflows bot's chat. No
//! real identities, credentials or tenant writes.
#![cfg(test)]
#[expect(dead_code, reason = "shared fake engine helpers used by other view tests")]
mod common;
#[path = "support/workflow_fixtures.rs"]
#[expect(dead_code, reason = "fixtures shared with the other workflow view tests")]
mod fixtures;
#[path = "common/parts.rs"]
mod parts;

use std::sync::Arc;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};
use teams_proto::{WorkflowRequest, WorkflowResponse};
use teams_ui::ViewTrace;
use teams_ui::settings::Settings;
use teams_ui::shell::{Main, Shell};
use teams_ui::workflow_dialog::{AppTab, WorkflowDialog};
use teams_ui::workflows_app::bot_chat;

fn app_window(
    rig: &common::Rig,
    answer: fn(&WorkflowRequest) -> WorkflowResponse,
) -> (Entity<WorkflowDialog>, &'static mut VisualTestContext) {
    *rig.core.workflow_handler.lock().unwrap() = Some(answer);
    rig.cx.update(teams_ui::window::init);
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let core: Arc<dyn teams_ui::Core> = rig.core.clone();
    let (view, cx) = app.add_window_view(|window, cx| WorkflowDialog::new_app(core, window, cx));
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
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    cx.run_until_parked();
}

fn asked(rig: &common::Rig) -> Vec<WorkflowRequest> {
    rig.core.workflow_requests.lock().unwrap().clone()
}

/// The webhook template and an inactive workflow next to an active one.
fn listed_answer(request: &WorkflowRequest) -> WorkflowResponse {
    match request {
        WorkflowRequest::List { .. } => Ok(serde_json::json!({"value": [
            fixtures::webhook_flow("Started"),
            fixtures::stopped_flow()
        ]})),
        WorkflowRequest::Get { flow, .. } if flow == "synthetic-off" => Ok(fixtures::stopped_flow()),
        other => fixtures::webhook_answer(other),
    }
}

#[test]
fn home_reads_the_apps_gallery_and_pages_through_eight_templates_at_a_time() {
    let rig = common::Rig::start();
    let (_view, cx) = app_window(&rig, fixtures::answer);
    let requests = asked(&rig);
    for category in ["neptune_teams_pa_home", "home_featured"] {
        assert!(
            requests.iter().any(|request| matches!(
                request,
                WorkflowRequest::Gallery { category: asked, search: None, .. } if asked == category
            )),
            "{category} is read"
        );
    }
    assert!(
        !requests
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Catalog)),
        "the app reads its own gallery, not the channel's catalog"
    );
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_some());
    assert!(
        bounds(cx, "workflow-template-synthetic-email").is_none(),
        "the ninth waits"
    );
    click(cx, "workflow-next");
    assert!(bounds(cx, "workflow-template-synthetic-email").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_none());
    click(cx, "workflow-all-templates");
    assert!(
        bounds(cx, "workflow-kind-all").is_some(),
        "All templates leads to Templates"
    );
    assert!(!requests.iter().any(WorkflowRequest::writes));
}

#[test]
fn find_templates_suggests_what_the_gallery_finds_and_explores_the_rest() {
    let rig = common::Rig::start();
    let (view, cx) = app_window(&rig, fixtures::answer);
    view.update_in(cx, |view, window, cx| view.find_template("planner", window, cx));
    assert!(bounds(cx, "workflow-suggestions").is_some());
    assert!(asked(&rig).iter().any(|request| matches!(
        request,
        WorkflowRequest::Gallery { category, search: Some(text), .. } if category == "home_featured" && text == "planner"
    )));
    assert!(bounds(cx, "workflow-suggestion-synthetic-planner").is_some());
    assert!(bounds(cx, "workflow-suggestion-synthetic-webhook").is_none());
    click(cx, "workflow-explore-templates");
    assert!(bounds(cx, "workflow-suggestions").is_none());
    assert_eq!(cx.update(|_, cx| view.read(cx).app_tab()), Some(AppTab::Templates));
}

#[test]
fn templates_filter_by_kind_and_by_app() {
    let rig = common::Rig::start();
    let (view, cx) = app_window(&rig, fixtures::answer);
    view.update(cx, |view, cx| view.show_tab(AppTab::Templates, cx));
    for every in ["synthetic-webhook", "synthetic-reminder", "synthetic-note"] {
        assert!(
            bounds(cx, &format!("workflow-template-{every}")).is_some(),
            "{every} is listed"
        );
    }
    click(cx, "workflow-kind-schedule");
    assert!(bounds(cx, "workflow-template-synthetic-reminder").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_none());
    click(cx, "workflow-kind-action");
    assert!(bounds(cx, "workflow-template-synthetic-note").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-reminder").is_none());
    click(cx, "workflow-kind-all");
    click(cx, "workflow-filter");
    click(cx, "workflow-filter-shared_rss");
    assert!(bounds(cx, "workflow-template-synthetic-rss").is_some());
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_none());
}

#[test]
fn your_workflows_lists_every_state_and_see_all_filters_them() {
    let rig = common::Rig::start();
    let (_view, cx) = app_window(&rig, listed_answer);
    assert!(bounds(cx, "workflow-open-synthetic-flow").is_some());
    assert!(
        bounds(cx, "workflow-open-synthetic-off").is_some(),
        "the app lists workflows that are off too"
    );
    click(cx, "workflow-row-more-synthetic-flow");
    assert!(bounds(cx, "workflow-row-edit").is_some());
    assert!(
        bounds(cx, "workflow-row-manage").is_none(),
        "the app's menu has no Manage"
    );
    cx.simulate_keystrokes("escape");
    click(cx, "workflow-see-all");
    assert!(bounds(cx, "workflow-find-flows").is_some());
    click(cx, "workflow-flows-inactive");
    assert!(bounds(cx, "workflow-open-synthetic-off").is_some());
    assert!(bounds(cx, "workflow-open-synthetic-flow").is_none());
    click(cx, "workflow-flows-active");
    assert!(bounds(cx, "workflow-open-synthetic-flow").is_some());
    assert!(bounds(cx, "workflow-open-synthetic-off").is_none());
    click(cx, "workflow-all-back");
    assert!(bounds(cx, "workflow-template-synthetic-webhook").is_some());
}

#[test]
fn your_workflows_loads_the_lists_later_pages() {
    fn paged(request: &WorkflowRequest) -> WorkflowResponse {
        match request {
            WorkflowRequest::List { .. } => Ok(serde_json::json!({
                "value": [fixtures::webhook_flow("Started")],
                "nextLink": "https://api.flow.microsoft.com/providers/Microsoft.ProcessSimple/environments/Default-synthetic/flows?api-version=2016-11-01&$skiptoken=synthetic"
            })),
            WorkflowRequest::Page { .. } => Ok(serde_json::json!({"value": [fixtures::stopped_flow()]})),
            other => listed_answer(other),
        }
    }
    let rig = common::Rig::start();
    let (_view, cx) = app_window(&rig, paged);
    click(cx, "workflow-see-all");
    assert!(bounds(cx, "workflow-open-synthetic-off").is_none());
    click(cx, "workflow-load-more");
    assert!(
        bounds(cx, "workflow-open-synthetic-off").is_some(),
        "the next page's workflows are listed too"
    );
    assert!(bounds(cx, "workflow-open-synthetic-flow").is_some());
    assert!(bounds(cx, "workflow-load-more").is_none(), "no more pages");
}

#[test]
fn a_template_from_the_app_opens_with_no_team_chosen_and_saves_in_the_page() {
    let rig = common::Rig::start();
    let (_view, cx) = app_window(&rig, fixtures::webhook_answer);
    click(cx, "workflow-template-synthetic-webhook");
    assert!(bounds(cx, "workflow-simple-card").is_some());
    assert!(
        bounds(cx, "workflow-save").is_some(),
        "the page's Save is at the top, with no footer"
    );
    assert!(bounds(cx, "workflow-edit-cancel").is_none());
    click(cx, "workflow-save");
    assert!(
        !asked(&rig)
            .iter()
            .any(|request| matches!(request, WorkflowRequest::Create { .. })),
        "Save stays off until a team and a channel are chosen"
    );
}

fn shell_window(rig: &common::Rig) -> (Entity<Shell>, &'static mut VisualTestContext) {
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::answer);
    rig.cx.update(teams_ui::window::init);
    let parts = parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.run_until_parked();
    (shell, cx)
}

#[test]
fn more_apps_opens_workflows_across_both_panes_and_its_chat_is_the_bots() {
    let rig = common::Rig::start();
    let (shell, cx) = shell_window(&rig);
    assert!(bounds(cx, "rail-workflows").is_none());
    click(cx, "rail-more-apps");
    click(cx, "more-apps-workflows");
    assert_eq!(cx.update(|_, cx| shell.read(cx).main().clone()), Main::Workflows);
    assert!(bounds(cx, "rail-workflows").is_some(), "the open app shows on the rail");
    assert!(bounds(cx, "workflows-header").is_some());
    assert!(bounds(cx, "list-pane").is_none(), "the app takes both panes");
    click(cx, "workflows-tab-chat");
    let me = teams_ui::Core::me(rig.core.as_ref()).to_owned();
    let chat = cx.update(|_, cx| {
        shell
            .read(cx)
            .workflows()
            .and_then(|app| app.read(cx).chat().map(|chat| chat.read(cx).conversation().to_owned()))
    });
    assert_eq!(chat, bot_chat(&me));
    click(cx, "rail-chats");
    assert!(
        bounds(cx, "rail-workflows").is_none(),
        "leaving it takes it off the rail"
    );
}

/// Whether the app's bot chat is shown (its messages read as they come).
fn bot_chat_shown(shell: &Entity<Shell>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| {
        shell
            .read(cx)
            .workflows()
            .and_then(|app| app.read(cx).chat().map(|chat| chat.read(cx).is_shown()))
            .unwrap_or(false)
    })
}

#[test]
fn every_way_out_of_the_app_hides_the_bots_chat_and_coming_back_shows_it_again() {
    type Leave = fn(&mut Shell, &mut gpui::Window, &mut gpui::Context<Shell>);
    let rig = common::Rig::start();
    let (shell, cx) = shell_window(&rig);
    click(cx, "rail-more-apps");
    click(cx, "more-apps-workflows");
    click(cx, "workflows-tab-chat");
    assert!(bot_chat_shown(&shell, cx));
    let leaves: [(&str, Leave); 3] = [
        ("calendar", Shell::show_calendar),
        ("settings", |shell, _, cx| shell.show_settings(cx)),
        ("chats", Shell::show_chats),
    ];
    for (place, leave) in leaves {
        shell.update_in(cx, leave);
        assert!(
            !bot_chat_shown(&shell, cx),
            "the {place} hides the bot's chat: no read receipt goes"
        );
        click(cx, "rail-more-apps");
        click(cx, "more-apps-workflows");
        assert!(
            bot_chat_shown(&shell, cx),
            "back from the {place}, the app is on its Chat again"
        );
    }
}

#[test]
fn coming_back_to_the_app_keeps_an_open_card_editor() {
    let rig = common::Rig::start();
    let (shell, cx) = shell_window(&rig);
    *rig.core.workflow_handler.lock().unwrap() = Some(fixtures::webhook_answer);
    click(cx, "rail-more-apps");
    click(cx, "more-apps-workflows");
    click(cx, "workflow-template-synthetic-webhook");
    assert!(bounds(cx, "workflow-simple-card").is_some());
    click(cx, "workflows-tab-chat");
    click(cx, "workflows-tab-home");
    assert!(
        bounds(cx, "workflow-simple-card").is_some(),
        "a look at the Chat tab keeps the editor"
    );
    shell.update_in(cx, Shell::show_calendar);
    click(cx, "rail-more-apps");
    click(cx, "more-apps-workflows");
    assert!(
        bounds(cx, "workflow-simple-card").is_some(),
        "the app comes back as it was left"
    );
    shell.update_in(cx, |shell, window, cx| shell.open_workflows(false, window, cx));
    assert!(bounds(cx, "workflow-simple-card").is_none(), "Workflows Home is Home");
}

#[test]
fn the_channels_workflows_reads_the_apps_list_again() {
    let rig = common::Rig::start();
    let (shell, cx) = shell_window(&rig);
    let lists = |rig: &common::Rig| {
        asked(rig)
            .iter()
            .filter(|request| matches!(request, WorkflowRequest::List { .. }))
            .count()
    };
    shell.update_in(cx, |shell, window, cx| shell.open_workflows(false, window, cx));
    cx.run_until_parked();
    let before = lists(&rig);
    shell.update_in(cx, Shell::show_calendar);
    shell.update_in(cx, |shell, window, cx| shell.open_workflows(true, window, cx));
    cx.run_until_parked();
    assert!(
        lists(&rig) > before,
        "a change made in the channel's dialog shows in the app"
    );
}

#[test]
fn workflows_home_from_the_channel_opens_the_app() {
    let rig = common::Rig::start();
    let (shell, cx) = shell_window(&rig);
    shell.update_in(cx, |shell, window, cx| shell.open_workflows(true, window, cx));
    assert!(
        bounds(cx, "workflow-find-flows").is_some(),
        "See all opens Your workflows"
    );
    shell.update_in(cx, |shell, window, cx| shell.open_workflows(false, window, cx));
    assert!(bounds(cx, "workflow-build").is_some(), "Workflows Home opens Home");
    assert!(bounds(cx, "workflow-find-flows").is_none());
}

#[test]
fn the_bot_chat_is_the_accounts_one_to_one_with_the_workflows_bot() {
    assert_eq!(
        bot_chat("8:orgid:00000000-0000-4000-8000-00000000000a").as_deref(),
        Some("19:00000000-0000-4000-8000-00000000000a_358f0194-6b0e-4dd3-af35-c24fe8a9ec87@unq.gbl.spaces")
    );
    assert_eq!(bot_chat("28:not-a-person"), None);
}
