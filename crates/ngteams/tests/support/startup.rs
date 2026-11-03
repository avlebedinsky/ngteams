//! The actual startup controllers on a mock GPUI platform and empty in-memory credentials.
//! Each case owns HOME/XDG in a subprocess; no user's files, keyring, display or service is reached.

use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use clap::Parser as _;
use ngteams::setup::Setup;
use teams_auth::secrets::MemorySecretStore;
use teams_fence::personal::PersonalAccount;
use teams_fence::{CallFloor, Fence};

use super::{Outcome, run};

mod startup_controls;

const CASE: &str = "NGTEAMS_STARTUP_TEST_CASE";
const TID: &str = "00000000-0000-4000-8000-000000000001";

fn calls() -> &'static Mutex<Vec<String>> {
    static CALLS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    CALLS.get_or_init(Mutex::default)
}

fn record(kind: &str, upn: &str) {
    calls().lock().unwrap().push(format!("{kind}:{upn}"));
}

pub(super) fn window_runner(runner: accounts::Runner) -> ngteams::accounts::Runner {
    ngteams::accounts::Runner {
        setup: runner.setup,
        floor: runner.floor,
        devices: runner.devices,
    }
}

pub(super) mod accounts {
    use super::*;
    use ngteams::calling::CallDevices;
    use teams_signin::window::Windows;

    pub type Started<S = MemorySecretStore> = ngteams::accounts::Started<S>;

    pub struct Runner {
        pub setup: Setup,
        pub floor: CallFloor,
        pub devices: Option<CallDevices>,
    }

    impl Runner {
        pub fn start(&self, account: &PersonalAccount) -> Result<Started, String> {
            record("start", &account.upn);
            let fence = Fence::open_personal_with(&self.setup.paths, &account.upn, &self.setup.lab)
                .map_err(|error| error.to_string())?
                .sharing_calls(&self.floor);
            let engine = ngteams::engine::start(
                &self.setup.paths,
                account,
                Arc::new(fence),
                MemorySecretStore::new(),
                false,
                None,
            )
            .map_err(|error| error.to_string())?;
            Ok(Started {
                account: account.clone(),
                engine,
                calls: None,
            })
        }
    }

    pub fn ready(_: &Setup, account: &PersonalAccount) -> Result<bool, String> {
        record("ready", &account.upn);
        if account.upn.starts_with("broken-ready") {
            return Err("synthetic readiness failure".to_owned());
        }
        Ok(account.upn.starts_with("ready"))
    }

    pub fn add(setup: &Setup, upn: &str, _: &Windows) -> Result<PersonalAccount, String> {
        record("add", upn);
        if std::env::var(CASE).unwrap().ends_with("error") {
            return Err("synthetic sign-in failure".to_owned());
        }
        setup.ask_for(upn).map_err(|error| error.to_string())
    }

    pub fn sign_in(_: &Setup, account: &PersonalAccount, _: &Windows) -> Result<PersonalAccount, String> {
        record("sign-in", &account.upn);
        if std::env::var(CASE).unwrap().ends_with("success") {
            Ok(account.clone())
        } else {
            Err("synthetic reauthentication failure".to_owned())
        }
    }

    pub fn forget(_: &Setup, upn: &str) -> Result<ngteams::forget::Forgot, String> {
        record("forget", upn);
        match upn {
            "missing@example.test" => Ok(ngteams::forget::Forgot::Nothing),
            "pending@example.test" => Ok(ngteams::forget::Forgot::Account {
                upn: upn.to_owned(),
                store: None,
            }),
            "stored@example.test" => Ok(ngteams::forget::Forgot::Account {
                upn: upn.to_owned(),
                store: Some("synthetic-store.sqlite".into()),
            }),
            _ => Err("synthetic forget failure".to_owned()),
        }
    }
}

pub(super) mod app {
    use gpui::AppContext as _;
    pub(crate) use ngteams::app::{TITLE, install_log, quit, quit_after, settings};

    pub(crate) fn launch<S: teams_auth::secrets::SecretStore + 'static>(
        cx: &mut gpui::App,
        launch: ngteams::app::Launch<S>,
    ) -> Result<ngteams::app::Launched, ngteams::app::LaunchFailed> {
        if std::env::var(super::CASE).unwrap().ends_with("window-error") {
            let stopped = cx.background_spawn(async move {
                for account in std::iter::once(launch.first).chain(launch.others) {
                    account.engine.handle.shutdown();
                }
            });
            return Err(ngteams::app::LaunchFailed {
                error: "synthetic window failure".to_owned(),
                stopped,
            });
        }
        ngteams::app::launch(cx, launch)
    }
}

pub(super) mod prompt {
    pub(crate) fn open(
        cx: &mut gpui::App,
        app_id: &str,
        title: gpui::SharedString,
        held: Option<&str>,
    ) -> gpui::Result<(
        gpui::WindowHandle<teams_ui::window::KitRoot>,
        gpui::Entity<teams_ui::AccountPrompt>,
    )> {
        if std::env::var(super::CASE).unwrap() == "prompt-window-error" {
            return Err(std::io::Error::other("synthetic prompt window failure").into());
        }
        teams_ui::account_prompt::open(cx, app_id, title, held)
    }
}

pub(super) mod platform {
    use std::time::{Duration, Instant};

    use gpui::{App, TestAppContext, WindowingModes};
    use teams_ui::account_prompt::{AccountPrompt, AccountPromptEvent};
    use teams_ui::window::KitRoot;

    pub struct Platform {
        cx: TestAppContext,
    }

    pub fn linux(_: WindowingModes) -> Platform {
        Platform {
            cx: TestAppContext::single(),
        }
    }

    impl Platform {
        pub fn run(self, launch: impl FnOnce(&mut App) + 'static) {
            let cx = self.cx;
            cx.update(launch);
            cx.run_until_parked();
            let case = std::env::var(super::CASE).unwrap();
            if matches!(case.as_str(), "prompt-success" | "prompt-error") {
                submit_prompt(&cx);
            }
            if matches!(case.as_str(), "prompt-success" | "prompt-error")
                || case.starts_with("first-")
                || case.starts_with("again-")
            {
                wait_for_backend(&cx);
            }
            cx.update(ngteams::app::quit);
            cx.run_until_parked();
        }
    }

    fn submit_prompt(cx: &TestAppContext) {
        let prompt = cx.update(|cx| {
            cx.windows()
                .into_iter()
                .filter_map(|window| window.downcast::<KitRoot>())
                .find_map(|window| window.read(cx).ok()?.view().clone().downcast::<AccountPrompt>().ok())
                .expect("the account prompt is open")
        });
        assert_eq!(cx.read(|cx| prompt.read(cx).account(cx)), "pending@example.test");
        cx.update(|cx| {
            prompt.update(cx, |_, cx| {
                cx.emit(AccountPromptEvent::SignIn("pending@example.test".into()));
            });
        });
    }

    fn wait_for_backend(cx: &TestAppContext) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            let complete = super::calls()
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.starts_with("add:") || call.starts_with("sign-in:"));
            if complete {
                // Flush the foreground continuation after the synchronous fake backend returns.
                for _ in 0..4 {
                    std::thread::sleep(Duration::from_millis(5));
                    cx.run_until_parked();
                }
                return;
            }
            assert!(Instant::now() < deadline, "startup did not call the synthetic backend");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn signed(setup: &Setup, upn: &str, suffix: u8, region: bool) -> PersonalAccount {
    let account = PersonalAccount::new(upn, &format!("00000000-0000-4000-8000-{suffix:012x}"), TID).unwrap();
    account.save(&setup.paths).unwrap();
    if region {
        let path = setup.paths.region_file(&account).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                r#"{{"_synthetic":true,"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{}"}}}}"#,
                teams_mock::CHAT_BASE
            ),
        )
        .unwrap();
    }
    account
}

#[test]
fn startup_controllers_use_only_the_fixture_backend_and_close_their_engines() {
    for case in [
        "command-invalid-env",
        "command-read-setup-error",
        "command-read-fence-error",
        "command-invalid-account",
        "command-error-files",
        "command-no-ready",
        "command-read-engine-error",
        "command-read-bad-accounts",
        "command-read-window-error",
        "window-error",
        "prompt-window-error",
        "command-broker",
        "command-snapshot",
        "command-read-refusal",
        "command-read-missing",
        "command-read-window",
        "empty",
        "pending",
        "prompt-success",
        "prompt-error",
        "first-success",
        "first-error",
        "ready",
        "ready-shortcut",
        "first-unready-success",
        "many",
        "first-start-fails",
        "broken-ready",
        "again-success",
        "again-error",
        "forget-missing",
        "forget-pending",
        "forget-stored",
        "forget-error",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "startup_tests::child", "--nocapture", "--test-threads", "1"])
            .env_clear()
            .env(CASE, case)
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_CACHE_HOME", root.path().join("cache"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-user-bus.sock", root.path().display()),
            );
        if (case.starts_with("command-read-") && case != "command-read-refusal") || case == "command-invalid-env" {
            child
                .env("UI_SANDBOX_DIR", root.path().join("sandbox"))
                .env("WAYLAND_DISPLAY", root.path().join("sandbox/synthetic-wayland"));
        }
        if case == "command-invalid-env" {
            child
                .env("HOME", "relative")
                .env("XDG_CONFIG_HOME", "relative")
                .env("XDG_DATA_HOME", "relative");
        }
        for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
            if let Some(value) = std::env::var_os(key) {
                child.env(key, value);
            }
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    if case == "command-invalid-env" {
        startup_controls::invalid_environment();
        return;
    }
    let setup = Setup::from_env().unwrap();
    std::fs::create_dir_all(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).unwrap();
    if case.starts_with("command-") {
        startup_controls::run(&case, &setup);
        return;
    }
    let mut args = vec!["ngteams".to_owned()];
    match case.as_str() {
        "pending" | "prompt-success" | "prompt-error" => {
            setup.ask_for("pending@example.test").unwrap();
        }
        "first-success" | "first-error" => args.extend(["--account".into(), "pending@example.test".into()]),
        "ready" | "ready-shortcut" | "window-error" => {
            signed(&setup, "ready@example.test", 2, true);
            if case == "ready-shortcut" {
                args.extend(["--account".into(), "ready@example.test".into()]);
            }
        }
        "first-unready-success" => {
            signed(&setup, "unready@example.test", 2, true);
            args.extend(["--account".into(), "unready@example.test".into()]);
        }
        "many" => {
            signed(&setup, "ready@example.test", 2, true);
            signed(&setup, "ready-failed@example.test", 3, false);
            signed(&setup, "ready-other@example.test", 4, true);
        }
        "first-start-fails" => {
            signed(&setup, "ready@example.test", 2, false);
        }
        "broken-ready" => {
            signed(&setup, "broken-ready@example.test", 2, true);
        }
        "again-success" | "again-error" => {
            signed(&setup, "ready@example.test", 2, true);
            signed(&setup, "unready@example.test", 3, true);
        }
        value if value.starts_with("forget-") => {
            args.extend([
                "--forget".into(),
                format!("{}@example.test", value.trim_start_matches("forget-")),
            ]);
        }
        "empty" | "prompt-window-error" => {}
        other => panic!("unknown synthetic case {other}"),
    }
    let outcome = run(
        &ngteams::cli::Cli::try_parse_from(args).unwrap(),
        teams_media::devices::pipewire::Reach::Sandbox,
    );
    if let Err(error) = &outcome {
        eprintln!("synthetic startup {case}: {error}");
    }
    match case.as_str() {
        "prompt-success" | "first-success" | "first-unready-success" | "again-success" => {
            assert!(matches!(outcome, Ok(Outcome::Restart)));
        }
        "first-error"
        | "first-start-fails"
        | "broken-ready"
        | "forget-error"
        | "window-error"
        | "prompt-window-error" => assert!(outcome.is_err()),
        _ => assert!(matches!(outcome, Ok(Outcome::Quit))),
    }
    let seen = calls().lock().unwrap();
    if case == "many" {
        assert!(
            seen.iter().any(|call| call == "start:ready-other@example.test"),
            "a later account still starts"
        );
    }
}
