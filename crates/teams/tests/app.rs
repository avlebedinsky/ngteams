//! The personal app's sign-out on GPUI's test context: once the session can't renew its tokens the window shows
//! "signed out", the engine is stopped (nothing more is sent: every request answers that it stopped) and the sign-in
//! window is opened again, once; the settings ask for the first run's screen. The personal build's window shows calls
//! when its devices opened (its test call to the web bundle's echo service, an invitation ringing by the name it
//! gives), and none without them; a test build opens no PipeWire outside the UI sandbox. The engine is the personal
//! build's own (`teams::engine::start`) over a `MemorySecretStore` holding no refresh token, so no request leaves the
//! process; the calls themselves run in `tests/calls.rs`.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::cell::Cell;
use std::fs;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::TestAppContext;
use teams::app::{self, SIGNED_OUT_HINT, SignInAgain};
use teams::calling::PersonalCalls;
use teams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;
use teams_core::{CallAction, CallActionError, CallId, EngineError, Event, IncomingCall, RegistrationReport};
use teams_fence::Fence;
use teams_fence::lab::LabLocations;
use teams_fence::personal::{PersonalAccount, PersonalPaths};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
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
    let region = setup.paths.region_file(&account).unwrap();
    fs::create_dir_all(region.parent().unwrap()).unwrap();
    fs::write(
        region,
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
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &account.upn, &setup.lab).unwrap());
    let engine = teams::engine::start(&setup.paths, &account, fence, MemorySecretStore::new(), false, None).unwrap();
    let core = engine.handle.requests();
    let cx = TestAppContext::single();
    let (models, bridge) = cx.update(|cx| app::bridge(engine.handle, None, cx));
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

/// What the models show of a call once the engine registered the calling endpoint and an invitation rang: whether
/// calls are shown, registered, ringing, idle, and the test call can be asked for.
fn shown_after_a_ring(calls: Option<&PersonalCalls>) -> (bool, bool, bool, bool, bool) {
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &account.upn, &setup.lab).unwrap());
    // the engine without calling: the models' side alone is under test here (tests/calls.rs runs the calls)
    let engine = teams::engine::start(&setup.paths, &account, fence, MemorySecretStore::new(), false, None).unwrap();
    let cx = TestAppContext::single();
    let (models, _bridge) = cx.update(|cx| app::bridge(engine.handle, calls, cx));
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
    cx.read(|cx| {
        let calls = models.calls.read(cx);
        (
            calls.shows(),
            calls.registered(),
            calls.ringing().is_some(),
            calls.idle(),
            calls.has_test_call(),
        )
    })
}

#[test]
fn the_personal_build_shows_its_calls_and_an_invitation_rings_by_the_name_it_gives() {
    // file devices: no PipeWire, no portal
    let devices = Arc::new(FileDevices {
        input: AudioInput::Silence,
        recording: None,
        camera: PatternCamera::default(),
        pacing: Pacing::RealTime,
    });
    let (engine_calls, calls) = teams::calling::on_devices(devices, None);
    assert_eq!(
        calls.echo.as_str(),
        teams_proto::TEAMS_ECHO_BOT_MRI,
        "the web bundle's echo service"
    );
    assert!(calls.ringer.is_some() && calls.video.is_none());
    assert!(engine_calls.media.video.is_none(), "no codec, no video");
    assert_eq!(
        shown_after_a_ring(Some(&calls)),
        (true, true, true, false, true),
        "shown, registered, ringing (so not idle), with the test call"
    );
}

#[test]
fn without_its_devices_the_personal_build_shows_no_call() {
    assert_eq!(
        shown_after_a_ring(None),
        (false, false, false, false, false),
        "nothing of a call is kept, shown or offered"
    );
}

#[test]
fn a_test_build_s_devices_are_the_sandbox_s_and_its_engine_without_them_places_no_call() {
    // `test-support` wins over `personal`: outside the UI sandbox the PipeWire is refused, so no call opens the desktop's
    let dir = tempfile::tempdir().unwrap();
    let (setup, account) = signed_in_setup(&dir);
    assert!(teams::calling::open(&setup.paths).is_err() || std::env::var_os("UI_SANDBOX_DIR").is_some());
    let fence = Arc::new(Fence::open_personal_with(&setup.paths, &account.upn, &setup.lab).unwrap());
    let engine = teams::engine::start(&setup.paths, &account, fence, MemorySecretStore::new(), false, None).unwrap();
    let refused = engine
        .handle
        .requests()
        .call(CallAction::TestCall {
            echo: teams::calling::echo(),
        })
        .blocking_wait();
    assert!(
        matches!(refused, Err(EngineError::Call(CallActionError::NoCalling))),
        "no calling: {refused:?}"
    );
    engine.handle.shutdown();
}

#[test]
fn the_settings_ask_for_the_first_run_s_screen() {
    let dir = tempfile::tempdir().unwrap();
    let file = SettingsFile::new(dir.path().join("settings.toml"));
    let settings = app::settings(file, None);
    assert!(settings.awaiting_first_run());
    assert!(!settings.sends_typing() && !settings.sends_read_receipts() && !settings.sends_presence());
}
