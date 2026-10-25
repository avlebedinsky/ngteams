//! The personal app's sign-out on GPUI's test context: once the session can't renew its tokens the window shows
//! "signed out", the engine is stopped (nothing more is sent: every request answers that it stopped) and the sign-in
//! window is opened again, once; the settings ask for the first run's screen. And the personal build shows no call
//! (M3 Task 17, until its call rule is built in Task 22): its engine has no calling, and its models leave out whatever
//! of a call they would be told. The engine is the personal build's own
//! (`teams::engine::start`) over a `MemorySecretStore` holding no refresh token, so no request leaves the process.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::Cell;
use std::fs;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::TestAppContext;
use teams::app::{self, SIGNED_OUT_HINT, SignInAgain};
use teams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;
use teams_core::{CallAction, CallActionError, CallId, EngineError, Event, IncomingCall, RegistrationReport};
use teams_fence::Fence;
use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalPaths};
use teams_mock::CHAT_BASE;
use teams_ui::{Account, Engine, SettingsFile};

const WAIT: Duration = Duration::from_secs(30);

/// A personal build signed in once (its account and region saved) under `dir`, with no refresh token anywhere.
fn signed_in_setup(dir: &tempfile::TempDir) -> (Setup, PersonalAccount) {
    let lab = LabLocations {
        config_file: dir.path().join("config/teams-lab/lab.toml"),
        data_dir: dir.path().join("share/teams-lab"),
        personal_data_dir: dir.path().join("share/teams"),
    };
    let paths = PersonalPaths::new(dir.path().join("config/teams"), dir.path().join("share/teams"));
    let setup = Setup::new(paths, lab).unwrap();
    let account = PersonalAccount::new(
        "someone@example.test",
        "00000000-0000-4000-8000-0000000000ff",
        "00000000-0000-4000-8000-000000000001",
    )
    .unwrap();
    account.save(&setup.paths).unwrap();
    setup.paths.make_dirs().unwrap();
    fs::write(
        setup.paths.region_file(),
        format!(r#"{{"region": "amer", "partition": null, "regionGtms": {{"chatServiceAfd": "{CHAT_BASE}"}}}}"#),
    )
    .unwrap();
    (setup, account)
}

/// Steps GPUI's test context until `done` holds: the engine answers on a thread of its own.
fn settle(cx: &TestAppContext, mut done: impl FnMut(&TestAppContext) -> bool) {
    let start = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(start.elapsed() < WAIT, "not settled in time");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn signed_out_shows_it_stops_the_engine_and_opens_the_sign_in_window_again() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &setup.lab).unwrap());
    let engine = teams::engine::start(&setup.paths, &account, fence, MemorySecretStore::new(), false).unwrap();
    let core = engine.handle.requests();
    let cx = TestAppContext::single();
    let (models, bridge) = cx.update(|cx| app::bridge(engine.handle, cx));
    let asked = Rc::new(Cell::new(0));
    let counted = Rc::clone(&asked);
    let again: SignInAgain = Rc::new(move |_| counted.set(counted.get() + 1));
    cx.update(|cx| app::watch_sign_out(engine.session.watch_signed_out(), bridge.downgrade(), again, cx));

    // the first request that needs a token finds the account signed out
    assert!(core.sync().blocking_wait().is_err());
    settle(&cx, |_| asked.get() > 0);
    let status = cx.read(|cx| models.status.read(cx).clone());
    assert_eq!(
        status.account,
        Account::SignedOut {
            hint: SIGNED_OUT_HINT.into()
        }
    );
    assert_eq!(
        status.engine,
        Engine::Stopped,
        "the window opens again only once the engine is down"
    );
    assert!(!cx.read(|cx| bridge.read(cx).running()));
    // nothing more is sent: every request answers that the engine stopped
    let after = core.sync().blocking_wait();
    assert!(matches!(after, Err(EngineError::Stopped)), "{after:?}");
    cx.run_until_parked();
    assert_eq!(asked.get(), 1, "the sign-in window is opened again once");
}

#[test]
fn the_personal_build_shows_no_call_and_its_engine_places_none() {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &setup.lab).unwrap());
    let engine = teams::engine::start(&setup.paths, &account, fence, MemorySecretStore::new(), false).unwrap();
    let core = engine.handle.requests();
    let refused = core
        .call(CallAction::Call {
            chat: "19:00000000-0000-4000-8000-0000000000ff_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces"
                .to_owned(),
        })
        .blocking_wait();
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::NoCalling))),
        "no calling: {refused:?}"
    );
    let cx = TestAppContext::single();
    let (models, _bridge) = cx.update(|cx| app::bridge(engine.handle, cx));
    cx.update(|cx| {
        models.apply(Event::CallingRegistration(RegistrationReport::Registered), cx);
        models.apply(
            Event::IncomingCall(IncomingCall {
                call: CallId::for_tests(1),
                caller: teams_fence::Mri::new("8:orgid:00000000-0000-4000-8000-00000000000b"),
                caller_name: Some("Someone Else".to_owned()),
                group_chat: None,
            }),
            cx,
        );
    });
    let calls = cx.read(|cx| {
        let calls = models.calls.read(cx);
        (
            calls.shows(),
            calls.registered(),
            calls.ringing().is_some(),
            calls.idle(),
        )
    });
    assert_eq!(
        calls,
        (false, false, false, false),
        "nothing of a call is kept, shown or offered"
    );
}

#[test]
fn the_settings_ask_for_the_first_run_s_screen() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    let settings = app::settings(file, None);
    assert!(settings.awaiting_first_run());
    assert!(!settings.sends_typing() && !settings.sends_read_receipts() && !settings.sends_presence());
}
