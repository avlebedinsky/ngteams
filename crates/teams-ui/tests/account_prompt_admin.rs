//! The sign-in screen's own-server forms beyond the sign-in (`teams_ui::account_prompt`) on GPUI's test context: the
//! certificate authority file's field, the question about a server once its address is entered, the form that creates
//! a server's first administrator, and the form of a password that must change.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};
use teams_ui::account_prompt::{
    MUST_CHANGE, NO_ADMIN_YET, NO_DISPLAY_NAME, NO_SETUP_CODE, NOT_A_USER, PASSWORDS_DIFFER, SAME_PASSWORD,
    SHORT_PASSWORD, Stage,
};
use teams_ui::{AccountPrompt, AccountPromptEvent, SignInKind, theme};

const SERVER: &str = "chat.example.test:8443";
const UPN: &str = "someone@example.test";
const PASSWORD: &str = "a made-up password";
const CA: &str = "/made/up/ca.pem";

type Events = Rc<RefCell<Vec<AccountPromptEvent>>>;

/// The screen with own servers offered and "Own server" chosen, and every event it emits.
fn own_prompt(cx: &mut TestAppContext) -> (Entity<AccountPrompt>, Events, VisualTestContext) {
    cx.update(theme::init);
    let window = cx.add_window(|window, cx| AccountPrompt::new(None, window, cx));
    let view = window.root(cx).unwrap();
    let events = Events::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &AccountPromptEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    // a field hears that it lost the keyboard only in the active window
    cx.update(|window, _| window.activate_window());
    view.update(&mut cx, AccountPrompt::offer_own_server);
    cx.run_until_parked();
    let segment = cx.debug_bounds("account-prompt-kind-own").unwrap();
    cx.simulate_click(segment.center(), gpui::Modifiers::none());
    assert_eq!(cx.read(|cx| view.read(cx).kind()), SignInKind::OwnServer);
    (view, events, cx)
}

/// Types `text` into the input of the field `selector` (its name is above the input, so near its bottom edge).
fn type_into(selector: &'static str, text: &str, cx: &mut VisualTestContext) {
    let bounds = cx.debug_bounds(selector).unwrap_or_else(|| panic!("no {selector}"));
    let at = gpui::point(bounds.center().x, bounds.bottom() - gpui::px(10.0));
    cx.simulate_click(at, gpui::Modifiers::none());
    cx.simulate_input(text);
    // a field hears that it lost the keyboard when the next frame is drawn
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn problem(view: &Entity<AccountPrompt>, cx: &mut VisualTestContext) -> Option<String> {
    cx.read(|cx| view.read(cx).problem().map(ToString::to_string))
}

fn stage(view: &Entity<AccountPrompt>, cx: &mut VisualTestContext) -> Stage {
    cx.read(|cx| view.read(cx).stage())
}

/// The sign-in form filled in and sent.
fn signed_in(view: &Entity<AccountPrompt>, cx: &mut VisualTestContext) {
    type_into("account-prompt-server", SERVER, cx);
    type_into("account-prompt-user", UPN, cx);
    type_into("account-prompt-password", PASSWORD, cx);
    cx.simulate_keystrokes("enter");
    assert!(cx.read(|cx| view.read(cx).is_signing_in()));
}

#[gpui::test]
fn the_servers_address_is_asked_about_once_and_its_ca_file_rides_the_sign_in(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    assert!(cx.debug_bounds("account-prompt-ca-file").is_some());
    type_into("account-prompt-server", SERVER, &mut cx);
    assert!(
        events.borrow().is_empty(),
        "nothing is asked while the address is typed"
    );
    type_into("account-prompt-user", UPN, &mut cx);
    let entered = |ca_file: &str| AccountPromptEvent::ServerEntered {
        server: SERVER.to_owned(),
        ca_file: ca_file.to_owned(),
    };
    assert_eq!(*events.borrow(), [entered("")]);
    // the same address again asks nothing; with a certificate authority file it is another question
    type_into("account-prompt-server", "", &mut cx);
    type_into("account-prompt-user", "", &mut cx);
    assert_eq!(events.borrow().len(), 1);
    type_into("account-prompt-ca-file", &format!(" {CA} "), &mut cx);
    type_into("account-prompt-password", PASSWORD, &mut cx);
    assert_eq!(*events.borrow(), [entered(""), entered(CA)]);
    assert_eq!(cx.read(|cx| view.read(cx).ca_file(cx)), CA);

    cx.simulate_keystrokes("enter");
    let events = events.borrow();
    let Some(AccountPromptEvent::SignInOwn(own)) = events.last() else {
        panic!("no sign-in: {events:?}");
    };
    assert_eq!(
        (
            own.server.as_str(),
            own.user.as_str(),
            own.password.as_str(),
            own.ca_file.as_str()
        ),
        (SERVER, UPN, PASSWORD, CA)
    );
}

#[gpui::test]
fn what_isnt_an_address_is_not_asked_about(cx: &mut TestAppContext) {
    let (_, events, mut cx) = own_prompt(cx);
    type_into("account-prompt-server", "http://chat.example.test", &mut cx);
    type_into("account-prompt-user", UPN, &mut cx);
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn a_server_without_an_administrator_shows_the_form_that_creates_one(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    type_into("account-prompt-server", SERVER, &mut cx);
    // another server's answer is not this one's
    view.update(&mut cx, |view, cx| view.needs_admin("other.example.test", true, cx));
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    view.update(&mut cx, |view, cx| view.needs_admin(SERVER, true, cx));
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::Setup { code_required: true });
    assert_eq!(stage(&view, &mut cx).key(), "setup");
    assert_eq!(cx.read(|cx| view.read(cx).asks()), NO_ADMIN_YET);
    for shown in [
        "account-prompt-server-line",
        "account-prompt-code",
        "account-prompt-user",
        "account-prompt-name",
        "account-prompt-new-password",
        "account-prompt-confirm",
        "account-prompt-back",
    ] {
        assert!(cx.debug_bounds(shown).is_some(), "{shown}");
    }
    for hidden in ["account-prompt-kind", "account-prompt-logo", "account-prompt-ca-file"] {
        assert!(cx.debug_bounds(hidden).is_none(), "{hidden}");
    }

    the_form_says_what_it_lacks_then_asks(&view, &events, &mut cx);
    refusals_are_said_on_the_form(&view, &events, &mut cx);
}

/// Each field of the first administrator's form says what it lacks, in order; filled in, it asks once.
fn the_form_says_what_it_lacks_then_asks(view: &Entity<AccountPrompt>, events: &Events, cx: &mut VisualTestContext) {
    // each field says what it lacks, in order; the code's field has the keyboard
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(view, cx).as_deref(), Some(NO_SETUP_CODE));
    cx.simulate_input("aaaaa-bbbbb");
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(view, cx).as_deref(), Some(NOT_A_USER));
    type_into("account-prompt-user", UPN, cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(view, cx).as_deref(), Some(NO_DISPLAY_NAME));
    type_into("account-prompt-name", " Ada Admin ", cx);
    assert_eq!(cx.read(|cx| view.read(cx).display_name(cx)), "Ada Admin");
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(view, cx).as_deref(), Some(SHORT_PASSWORD));
    type_into("account-prompt-new-password", PASSWORD, cx);
    type_into("account-prompt-confirm", "a made-up", cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(view, cx).as_deref(), Some(PASSWORDS_DIFFER));
    assert!(
        events
            .borrow()
            .iter()
            .all(|event| !matches!(event, AccountPromptEvent::CreateAdmin(_)))
    );
    // the rest of it, typed at the end of what the field holds
    type_into("account-prompt-confirm", " password", cx);
    cx.simulate_keystrokes("enter");

    let created = events.borrow().last().cloned();
    let Some(AccountPromptEvent::CreateAdmin(new)) = created else {
        panic!("no administrator asked for: {created:?}");
    };
    assert_eq!(
        (
            new.server.as_str(),
            new.user.as_str(),
            new.name.as_str(),
            new.password.as_str()
        ),
        (SERVER, UPN, "Ada Admin", PASSWORD)
    );
    assert_eq!(new.code.as_ref().map(|code| code.as_str()), Some("aaaaa-bbbbb"));
    assert!(!format!("{new:?}").contains(PASSWORD) && !format!("{new:?}").contains("aaaaa"));
    assert!(cx.read(|cx| view.read(cx).is_signing_in()));
    // nothing more is asked while it is being created
    let asked = |events: &Events| {
        events
            .borrow()
            .iter()
            .filter(|event| matches!(event, AccountPromptEvent::CreateAdmin(_)))
            .count()
    };
    assert_eq!(asked(events), 1);
    cx.simulate_keystrokes("enter");
    view.update(cx, AccountPrompt::back);
    assert_eq!(stage(view, cx), Stage::Setup { code_required: true });
    assert_eq!(asked(events), 1);
}

/// A wrong setup code, and a server that got its administrator meanwhile.
fn refusals_are_said_on_the_form(view: &Entity<AccountPrompt>, events: &Events, cx: &mut VisualTestContext) {
    // a wrong code: said, the code's field emptied with the keyboard on it
    view.update(cx, |view, cx| {
        view.setup_failed("The setup code isn't right.", true, false, cx);
    });
    cx.run_until_parked();
    assert_eq!(problem(view, cx).as_deref(), Some("The setup code isn't right."));
    cx.simulate_input("ccccc");
    cx.simulate_keystrokes("enter");
    let again = events.borrow().last().cloned();
    let Some(AccountPromptEvent::CreateAdmin(new)) = again else {
        panic!("no second attempt: {again:?}");
    };
    assert_eq!(new.code.as_ref().map(|code| code.as_str()), Some("ccccc"));

    // the server got its administrator meanwhile: the sign-in form is back, saying so
    view.update(cx, |view, cx| {
        view.setup_failed("This server has an administrator now.", false, true, cx);
    });
    cx.run_until_parked();
    assert_eq!(stage(view, cx), Stage::SignIn);
    assert_eq!(
        problem(view, cx).as_deref(),
        Some("This server has an administrator now.")
    );
}

#[gpui::test]
fn a_server_that_asks_no_setup_code_shows_no_field_for_one(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    type_into("account-prompt-server", SERVER, &mut cx);
    view.update(&mut cx, |view, cx| view.needs_admin(SERVER, false, cx));
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-prompt-code").is_none());
    // the user name's field has the keyboard
    cx.simulate_input(UPN);
    type_into("account-prompt-name", "Ada Admin", &mut cx);
    type_into("account-prompt-new-password", PASSWORD, &mut cx);
    type_into("account-prompt-confirm", PASSWORD, &mut cx);
    let send = cx.debug_bounds("account-prompt-sign-in").unwrap();
    cx.simulate_click(send.center(), gpui::Modifiers::none());
    let created = events.borrow().last().cloned();
    let Some(AccountPromptEvent::CreateAdmin(new)) = created else {
        panic!("no administrator asked for: {created:?}");
    };
    assert_eq!((new.user.as_str(), new.code), (UPN, None));
}

#[gpui::test]
fn back_returns_to_the_sign_in_and_the_server_is_asked_about_again(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    type_into("account-prompt-server", SERVER, &mut cx);
    type_into("account-prompt-user", UPN, &mut cx);
    assert_eq!(events.borrow().len(), 1);
    view.update(&mut cx, |view, cx| view.needs_admin(SERVER, true, cx));
    cx.run_until_parked();
    let back = cx.debug_bounds("account-prompt-back").unwrap();
    cx.simulate_click(back.center(), gpui::Modifiers::none());
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    assert_eq!(stage(&view, &mut cx).key(), "sign-in");
    assert!(cx.debug_bounds("account-prompt-kind").is_some());
    // the keyboard is on the address, and leaving it asks again
    type_into("account-prompt-user", "", &mut cx);
    assert_eq!(events.borrow().len(), 2);
    // back does nothing on the sign-in form
    view.update(&mut cx, AccountPrompt::back);
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
}

#[gpui::test]
fn a_password_that_must_change_is_asked_for_twice(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    // no sign-in is under way: nothing to change
    view.update(&mut cx, AccountPrompt::must_change_password);
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);

    signed_in(&view, &mut cx);
    view.update(&mut cx, AccountPrompt::must_change_password);
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::NewPassword);
    assert_eq!(stage(&view, &mut cx).key(), "new-password");
    assert_eq!(cx.read(|cx| view.read(cx).asks()), MUST_CHANGE);
    assert!(cx.read(|cx| !view.read(cx).is_signing_in()));
    for shown in [
        "account-prompt-server-line",
        "account-prompt-user-line",
        "account-prompt-new-password",
        "account-prompt-confirm",
    ] {
        assert!(cx.debug_bounds(shown).is_some(), "{shown}");
    }
    assert!(cx.debug_bounds("account-prompt-password").is_none());

    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(SHORT_PASSWORD));
    // the new password's field has the keyboard; the current password again is refused
    cx.simulate_input(PASSWORD);
    type_into("account-prompt-confirm", PASSWORD, &mut cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(SAME_PASSWORD));
    type_into("account-prompt-new-password", " 2", &mut cx);
    cx.simulate_keystrokes("enter");
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(PASSWORDS_DIFFER));
    type_into("account-prompt-confirm", " 2", &mut cx);
    cx.simulate_keystrokes("enter");

    let changed = events.borrow().last().cloned();
    let Some(AccountPromptEvent::ChangePassword { own, new }) = changed else {
        panic!("no change asked for: {changed:?}");
    };
    assert_eq!(
        (
            own.server.as_str(),
            own.user.as_str(),
            own.password.as_str(),
            new.as_str()
        ),
        (SERVER, UPN, PASSWORD, "a made-up password 2")
    );
    // a refusal is said on the same form
    view.update(&mut cx, |view, cx| view.failed("Use a longer password.", cx));
    assert_eq!(stage(&view, &mut cx), Stage::NewPassword);
    assert_eq!(problem(&view, &mut cx).as_deref(), Some("Use a longer password."));
}

#[gpui::test]
fn an_account_asked_again_changes_its_password_on_its_own_server(cx: &mut TestAppContext) {
    cx.update(theme::init);
    let window = cx.add_window(|window, cx| AccountPrompt::new(None, window, cx));
    let view = window.root(cx).unwrap();
    let events = Events::default();
    let seen = Rc::clone(&events);
    cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &AccountPromptEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.ask_own_again("https://chat.example.test", UPN, window, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("account-prompt-ca-file").is_none());
    cx.simulate_input(PASSWORD);
    cx.simulate_keystrokes("enter");
    // an account asked again is never asked about its server
    assert_eq!(events.borrow().len(), 1);
    view.update(&mut cx, |view, cx| {
        view.needs_admin("https://chat.example.test", true, cx);
    });
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    view.update(&mut cx, AccountPrompt::must_change_password);
    cx.run_until_parked();
    cx.simulate_input("another made-up one");
    type_into("account-prompt-confirm", "another made-up one", &mut cx);
    cx.simulate_keystrokes("enter");
    let changed = events.borrow().last().cloned();
    let Some(AccountPromptEvent::ChangePassword { own, new }) = changed else {
        panic!("no change asked for: {changed:?}");
    };
    assert_eq!(
        (own.server.as_str(), own.ca_file.as_str(), new.as_str()),
        ("https://chat.example.test", "", "another made-up one")
    );
    // back from here asks the password again
    view.update(&mut cx, |view, cx| view.failed("no", cx));
    view.update(&mut cx, AccountPrompt::back);
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    assert!(cx.debug_bounds("account-prompt-password").is_some());
}

#[gpui::test]
fn a_sign_in_that_fails_after_the_change_asks_for_the_new_password(cx: &mut TestAppContext) {
    let (view, events, mut cx) = own_prompt(cx);
    signed_in(&view, &mut cx);
    view.update(&mut cx, AccountPrompt::must_change_password);
    cx.run_until_parked();
    let new = "a made-up password 2";
    cx.simulate_input(new);
    type_into("account-prompt-confirm", new, &mut cx);
    let changes = |events: &Events| {
        events
            .borrow()
            .iter()
            .filter(|event| matches!(event, AccountPromptEvent::ChangePassword { .. }))
            .count()
    };
    cx.simulate_keystrokes("enter");
    assert_eq!(changes(&events), 1);

    // a change the server refused has nothing to go back to: its form and what was said stay
    let refused = "The server said no.";
    view.update(&mut cx, |view, cx| view.failed(refused, cx));
    view.update(&mut cx, AccountPrompt::password_changed);
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::NewPassword);
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(refused));
    // and the next try is still a change of the refused sign-in's password
    cx.simulate_keystrokes("enter");
    assert_eq!(changes(&events), 2);

    // the change went through: the sign-in form is back, the sign-in with the new password under way
    view.update(&mut cx, AccountPrompt::password_changed);
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    assert!(cx.read(|cx| view.read(cx).is_signing_in()));
    // the sign-in after it fails: said there, and the next one sends what is typed, never the old password again
    let said = "Too many attempts: wait a few minutes, then sign in again. Your new password is set: sign in with it.";
    view.update(&mut cx, |view, cx| view.failed(said, cx));
    cx.run_until_parked();
    assert_eq!(stage(&view, &mut cx), Stage::SignIn);
    assert_eq!(problem(&view, &mut cx).as_deref(), Some(said));
    type_into("account-prompt-password", new, &mut cx);
    cx.simulate_keystrokes("enter");
    let again = events.borrow().last().cloned();
    let Some(AccountPromptEvent::SignInOwn(own)) = again else {
        panic!("no sign-in asked for: {again:?}");
    };
    assert_eq!((own.user.as_str(), own.password.as_str()), (UPN, new));
}
