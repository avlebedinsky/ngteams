//! Message extensions in a chat with the bots shown (`NGTEAMS_BOTS=1`, `teams_ui::apps_flyout`,
//! `teams_ui::app_dialog`), on GPUI's test context over the fake engine and the synthetic apps of
//! `tests/fixtures/bots/extension-apps.json`: the compose box's Actions and apps (its list, its search, its keys, "Get
//! more apps"), an app's search and a result's card in the box, the message sent with the card, an action command's
//! dialog asked for, a message's "More actions" with the apps' commands, and none of it without the flag or in
//! read-only mode. Every app, bot and id here is synthetic.

use gpui::{Entity, Modifiers, VisualTestContext};
use serde_json::json;
use teams_core::Action;
use teams_proto::{InvokeAnswer, InvokeKind};
use teams_ui::bots_flag::BotsFlag;
use teams_ui::chat_view::{ChatView, ChatViewEvent};

use super::common::{LAB_B, Rig};
use super::{AB, NOON, chat_window, draw, message, rig_with, trace};

const FINDER: &str = "00000000-0000-4000-8000-00000000e001";
const ID: &str = "701";

fn apps_rig(flag: bool) -> Rig {
    let rig = rig_with(vec![message(ID, LAB_B, "<p>A made-up order</p>", NOON)]);
    rig.cx.update(|cx| cx.set_global(BotsFlag(flag)));
    *rig.core.extension_apps.lock().unwrap() =
        teams_proto::extension_apps(teams_mock::extension_apps_answer().to_string().as_bytes());
    rig
}

fn window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let (written, _) = trace();
    chat_window(rig, written)
}

fn found(cx: &mut VisualTestContext, selector: &str) -> bool {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .is_some()
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    draw(cx);
    let at = cx
        .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    cx.run_until_parked();
}

fn told(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> std::rc::Rc<std::cell::RefCell<Vec<ChatViewEvent>>> {
    let told: std::rc::Rc<std::cell::RefCell<Vec<ChatViewEvent>>> = std::rc::Rc::default();
    let kept = std::rc::Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    told
}

fn results() -> InvokeAnswer {
    InvokeAnswer::Results {
        grid: false,
        attachments: vec![
            json!({"contentType": "application/vnd.microsoft.card.adaptive",
                   "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 1"}]},
                   "preview": {"contentType": "application/vnd.microsoft.card.thumbnail",
                               "content": {"title": "Record 1", "text": "Made up",
                                           "tap": {"type": "invoke", "value": {"id": 1}}}}}),
            json!({"contentType": "application/vnd.microsoft.card.adaptive",
                   "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 2"}]}}),
        ],
    }
}

#[test]
fn the_plus_lists_the_apps_for_the_box_and_its_search_and_keys_work() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    click(cx, "compose-apps");
    // the apps whose commands run in the compose box, the account's only
    assert!(found(cx, &format!("apps-flyout-app-{FINDER}")));
    assert!(found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002"));
    assert!(
        !found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e003"),
        "a message action only"
    );
    assert!(
        !found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e004"),
        "the store's offer"
    );
    cx.simulate_input("praise");
    assert!(!found(cx, &format!("apps-flyout-app-{FINDER}")));
    assert!(found(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002"));
    cx.simulate_input("zzz");
    assert!(found(cx, "apps-flyout-no-match"));
    click(cx, "apps-flyout-more");
    assert!(matches!(told.borrow().last(), Some(ChatViewEvent::ShowStore)));
    assert!(!found(cx, "apps-flyout"), "it closes");
    // Enter chooses the row Up and Down stand on; Escape goes back, then closes
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    cx.simulate_keystrokes("down up enter");
    cx.run_until_parked();
    assert!(found(cx, "apps-flyout-pane-name"));
    assert!(found(cx, "apps-flyout-result-1"));
    cx.simulate_keystrokes("escape");
    assert!(!found(cx, "apps-flyout-pane-name"));
    cx.simulate_keystrokes("escape");
    assert!(!found(cx, "apps-flyout"));
}

/// The search ran at once, with no words, as its command says.
fn the_search_ran_at_once(rig: &Rig) {
    let asks = rig.core.bot_asks.lock().unwrap();
    let [ask] = asks.as_slice() else { panic!("{asks:?}") };
    assert!(matches!(&ask.kind, InvokeKind::Query { command, parameters, .. }
        if command == "findRecords" && parameters == &[("keyword".to_owned(), String::new())]));
    assert_eq!(ask.app_id, FINDER);
    assert_eq!(ask.conversation.as_deref(), Some(AB));
}

/// The text the chat's send carried.
fn sent(rig: &Rig) -> teams_proto::chat::MessageText {
    let actions = rig.core.actions.lock().unwrap().clone();
    actions
        .iter()
        .find_map(|action| match action {
            Action::Send { conversation, text } if conversation == AB => Some(text.clone()),
            _ => None,
        })
        .expect("a send")
}

#[test]
fn a_result_goes_into_the_box_and_out_with_the_message() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let chosen = InvokeAnswer::Results {
        grid: false,
        attachments: vec![json!({"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Record 1, chosen"}]}})],
    };
    rig.core.bot_answers.lock().unwrap().extend([Ok(results()), Ok(chosen)]);
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    the_search_ran_at_once(&rig);
    // a result whose preview names an invoke asks the app for its card
    click(cx, "apps-flyout-result-0");
    assert!(matches!(&rig.core.bot_asks.lock().unwrap()[1].kind,
        InvokeKind::SelectItem { value } if value == &json!({"id": 1})));
    assert!(!found(cx, "apps-flyout"));
    assert!(found(cx, "compose-app-card-0"));
    // the card is sent with the message, with no text of its own
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.send(window, cx)));
    cx.run_until_parked();
    let sent = sent(&rig);
    assert_eq!(sent.cards().len(), 1);
    assert_eq!(sent.cards()[0].app_id, FINDER);
    assert_eq!(sent.cards()[0].content["body"][0]["text"], "Record 1, chosen");
    assert!(sent.html().contains("InputExtension"));
    assert!(!found(cx, "compose-app-card-0"), "the box is empty again");
}

#[test]
fn a_cards_close_mark_takes_it_out_of_the_box() {
    let rig = apps_rig(true);
    let (_view, cx) = window(&rig);
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    // a result with no invoke goes in as it is
    click(cx, "apps-flyout-result-1");
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 1);
    assert!(found(cx, "compose-app-card-0"));
    click(cx, "compose-app-card-remove-0");
    assert!(!found(cx, "compose-app-card-0"));
}

#[test]
fn an_action_command_asks_for_its_dialog_and_a_web_page_asks_nothing() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    rig.core.bot_answers.lock().unwrap().push_back(Ok(results()));
    click(cx, "compose-apps");
    click(cx, &format!("apps-flyout-app-{FINDER}"));
    click(cx, "apps-flyout-command-1");
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    assert_eq!(seed.app.name, "Made-up Finder");
    assert!(matches!(&seed.opened, teams_ui::app_dialog::Opened::Command { command, .. } if command.id == "newRecord"));
    // the one action of an app opens its dialog at once
    click(cx, "compose-apps");
    click(cx, "apps-flyout-app-00000000-0000-4000-8000-00000000e002");
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    assert!(
        matches!(&seed.opened, teams_ui::app_dialog::Opened::Command { command, .. } if command.id == "sendPraise")
    );
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 1, "only the search asked");
}

#[test]
fn more_actions_lists_the_apps_commands_and_an_offer_opens_its_details() {
    let rig = apps_rig(true);
    let (view, cx) = window(&rig);
    let told = told(&view, cx);
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    click(cx, "message-more-actions");
    let task = "message-app-action-00000000-0000-4000-8000-00000000e003-taskFromMessage";
    let ticket = "message-app-action-00000000-0000-4000-8000-00000000e004-fileTicket";
    assert!(found(cx, task));
    assert!(found(cx, ticket));
    assert!(found(cx, "message-create-workflows"), "Create workflows stays");
    click(cx, ticket);
    assert!(
        matches!(told.borrow().last(), Some(ChatViewEvent::AppDetails(store)) if store.app.name == "Made-up Tickets")
    );
    // the account's app opens its dialog on the message, described as Teams describes it
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    click(cx, "message-more-actions");
    click(cx, task);
    let Some(ChatViewEvent::AppDialog(seed)) = told.borrow().last().cloned() else {
        panic!("a dialog");
    };
    let teams_ui::app_dialog::Opened::Command { message, context, .. } = &seed.opened else {
        panic!("a command");
    };
    assert_eq!(*context, teams_proto::CommandContext::Message);
    let payload = message.as_ref().expect("the message");
    assert_eq!(payload.id, ID);
    assert_eq!(payload.html, "<p>A made-up order</p>");
}

#[test]
fn without_the_flag_or_in_read_only_mode_there_is_no_app() {
    let rig = apps_rig(false);
    let (view, cx) = window(&rig);
    click(cx, "compose-apps");
    assert!(!found(cx, "apps-flyout"));
    cx.update(|_, cx| view.update(cx, |view, cx| view.toggle_options(ID, cx)));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).message_app_actions().is_empty()));
    let rig = apps_rig(true);
    *rig.core.read_only.lock().unwrap() = true;
    let (view, cx) = window(&rig);
    let compose = cx.update(|_, cx| view.read(cx).compose().clone());
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.toggle_apps(window, cx)));
    assert!(!found(cx, "apps-flyout"));
    assert!(cx.update(|_, cx| view.read(cx).message_app_actions().is_empty()));
}
