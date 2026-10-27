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
