//! The personal build's first sign-in screen (`teams_ui::account_prompt`) on GPUI's test context: the account typed
//! and Enter (or the button) ask for its sign-in once; what isn't an account is said under the field and asks nothing;
//! a sign-in that stored nothing opens the field again with its reason; an earlier attempt's account fills the field.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use teams_ui::account_prompt::{NOT_AN_ACCOUNT, SIGNING_IN};
use teams_ui::{AccountPrompt, AccountPromptEvent, theme};

const UPN: &str = "someone@example.test";

/// The screen open and focused, its field holding `account`, with every event it emitted.
fn prompt(
    account: Option<&'static str>,
    cx: &mut TestAppContext,
) -> (
    Entity<AccountPrompt>,
    Rc<RefCell<Vec<AccountPromptEvent>>>,
    VisualTestContext,
) {
    cx.update(theme::init);
    let window = cx.add_window(|window, cx| AccountPrompt::new(account, window, cx));
    let view = window.root(cx).unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &AccountPromptEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    (view, events, cx)
}

fn problem(view: &Entity<AccountPrompt>, cx: &mut VisualTestContext) -> Option<String> {
    cx.read(|cx| view.read(cx).problem().map(ToString::to_string))
}

#[gpui::test]
fn the_account_typed_is_asked_for_once(cx: &mut TestAppContext) {
    let (view, events, mut cx) = prompt(None, cx);
    cx.simulate_input("  someone@example.test ");
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), vec![AccountPromptEvent::SignIn(UPN.to_owned())]);
    assert!(cx.read(|cx| view.read(cx).is_signing_in()));
    // while Microsoft's window is open, another Enter asks nothing more
    cx.simulate_keystrokes("enter");
    view.update(&mut cx, AccountPrompt::submit);
    assert_eq!(events.borrow().len(), 1);
    assert!(cx.debug_bounds("account-prompt-status").is_some(), "says {SIGNING_IN}");
}

#[gpui::test]
fn what_isnt_an_account_is_said_and_asks_nothing(cx: &mut TestAppContext) {
    let (view, events, mut cx) = prompt(None, cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NOT_AN_ACCOUNT));
    cx.simulate_input("someone");
    assert_eq!(problem(&view, &mut cx), None, "typing clears it");
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NOT_AN_ACCOUNT));
    assert!(events.borrow().is_empty());
    assert!(!cx.read(|cx| view.read(cx).is_signing_in()));
}

#[gpui::test]
fn a_failed_sign_in_opens_the_field_again_with_its_reason(cx: &mut TestAppContext) {
    let (view, events, mut cx) = prompt(Some(UPN), cx);
    assert_eq!(
        cx.read(|cx| view.read(cx).account(cx)),
        UPN,
        "an earlier attempt's account fills the field"
    );
    cx.simulate_keystrokes("enter");
    view.update(&mut cx, |view, cx| view.failed("the window was closed", cx));
    assert!(!cx.read(|cx| view.read(cx).is_signing_in()));
    assert_eq!(problem(&view, &mut cx).as_deref(), Some("the window was closed"));
    cx.simulate_keystrokes("enter");
    assert_eq!(
        *events.borrow(),
        vec![
            AccountPromptEvent::SignIn(UPN.to_owned()),
            AccountPromptEvent::SignIn(UPN.to_owned())
        ]
    );
}

// --- an own server ---

use teams_ui::account_prompt::{NO_PASSWORD, NOT_A_SERVER, NOT_A_USER, SIGNING_IN_OWN};
use teams_ui::{OwnSignIn, SignInKind};

const SERVER: &str = "https://chat.example.test:8443";
const PASSWORD: &str = "a made-up password";

fn own(events: &Rc<RefCell<Vec<AccountPromptEvent>>>) -> Vec<OwnSignIn> {
    events
        .borrow()
        .iter()
        .filter_map(|event| match event {
            AccountPromptEvent::SignInOwn(own) => Some(own.clone()),
            AccountPromptEvent::SignIn(_) => None,
        })
        .collect()
}

/// Clicks into the input of the field `selector` (its name is above the input, so near its bottom edge).
fn click_into(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds(selector).unwrap();
    let at = gpui::point(bounds.center().x, bounds.bottom() - gpui::px(10.0));
    cx.simulate_click(at, gpui::Modifiers::none());
}

#[gpui::test]
fn the_choice_shows_only_when_own_servers_are_offered(cx: &mut TestAppContext) {
    let (view, events, mut cx) = prompt(None, cx);
    assert!(
        cx.debug_bounds("account-prompt-kind-own").is_none(),
        "Microsoft 365 alone"
    );
    // choosing a place that isn't offered changes nothing
    view.update_in(&mut cx, |view, window, cx| {
        view.choose(SignInKind::OwnServer, window, cx);
    });
    assert_eq!(cx.read(|cx| view.read(cx).kind()), SignInKind::Microsoft);

    view.update(&mut cx, AccountPrompt::offer_own_server);
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-prompt-kind-microsoft").is_some());
    assert!(
        cx.debug_bounds("account-prompt-field").is_some(),
        "Microsoft 365 is chosen first"
    );
    assert!(cx.debug_bounds("account-prompt-server").is_none());
    // the Microsoft flow is as it was with the choice showing
    cx.simulate_input(UPN);
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), vec![AccountPromptEvent::SignIn(UPN.to_owned())]);
}

/// The screen with own servers offered and "Own server" clicked.
fn own_prompt(
    cx: &mut TestAppContext,
) -> (
    Entity<AccountPrompt>,
    Rc<RefCell<Vec<AccountPromptEvent>>>,
    VisualTestContext,
) {
    let (view, events, mut cx) = prompt(None, cx);
    view.update(&mut cx, AccountPrompt::offer_own_server);
    cx.run_until_parked();
    let segment = cx.debug_bounds("account-prompt-kind-own").unwrap();
    cx.simulate_click(segment.center(), gpui::Modifiers::none());
    assert_eq!(cx.read(|cx| view.read(cx).kind()), SignInKind::OwnServer);
    (view, events, cx)
}

#[gpui::test]
fn an_own_servers_fields_say_what_they_lack_and_ask_nothing(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    for field in [
        "account-prompt-server",
        "account-prompt-user",
        "account-prompt-password",
    ] {
        assert!(cx.debug_bounds(field).is_some(), "{field}");
    }
    assert!(cx.debug_bounds("account-prompt-field").is_none());

    // each field says what it lacks, in order, and asks nothing
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NOT_A_SERVER));
    cx.simulate_input(" chat.example.test:8443 ");
    assert_eq!(problem(&view, &mut cx), None, "typing clears it");
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NOT_A_USER));
    click_into("account-prompt-user", &mut cx);
    cx.simulate_input(UPN);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NO_PASSWORD));
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn an_own_servers_account_is_asked_for_with_its_password_once(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    cx.simulate_input(" chat.example.test:8443 ");
    click_into("account-prompt-user", &mut cx);
    cx.simulate_input(UPN);
    click_into("account-prompt-password", &mut cx);
    cx.simulate_input(PASSWORD);
    cx.simulate_keystrokes("enter");
    let asked = own(&events);
    assert_eq!(asked.len(), 1);
    assert_eq!(
        (
            asked[0].server.as_str(),
            asked[0].user.as_str(),
            asked[0].password.as_str()
        ),
        ("chat.example.test:8443", UPN, PASSWORD)
    );
    assert!(
        !format!("{:?}", asked[0]).contains(PASSWORD),
        "the password is never printed"
    );
    assert!(cx.read(|cx| view.read(cx).is_signing_in()), "says {SIGNING_IN_OWN}");
    // while it signs in nothing more is asked, and the place can't be changed
    cx.simulate_keystrokes("enter");
    view.update_in(&mut cx, |view, window, cx| {
        view.choose(SignInKind::Microsoft, window, cx);
    });
    assert_eq!(own(&events).len(), 1);
    assert_eq!(cx.read(|cx| view.read(cx).kind()), SignInKind::OwnServer);

    // a sign-in that stored nothing: its reason, the password cleared, the server and the user name kept
    view.update(&mut cx, |view, cx| {
        view.failed("The user name or password isn't right.", cx);
    });
    cx.run_until_parked();
    assert_eq!(
        cx.read(|cx| view.read(cx).own_fields(cx)),
        ("chat.example.test:8443".to_owned(), UPN.to_owned())
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(NO_PASSWORD));
    assert_eq!(own(&events).len(), 1);
}

#[gpui::test]
fn an_own_servers_account_signed_out_is_asked_for_its_password_alone(cx: &mut TestAppContext) {
    let (view, events, mut cx) = prompt(None, cx);
    view.update_in(&mut cx, |view, window, cx| view.ask_own_again(SERVER, UPN, window, cx));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("account-prompt-kind-own").is_none(),
        "no other place is offered"
    );
    assert_eq!(
        cx.read(|cx| view.read(cx).own_fields(cx)),
        (SERVER.to_owned(), UPN.to_owned())
    );
    // the password's field has the focus: typing and Enter sign in
    cx.simulate_input(PASSWORD);
    cx.simulate_keystrokes("enter");
    let asked = own(&events);
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].server.as_str(), asked[0].user.as_str()), (SERVER, UPN));
}
