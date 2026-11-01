//! Private startup controllers on synthetic lab files, empty broker credentials and GPUI's mock platform.
//! Each scenario owns HOME/XDG in a subprocess and cannot reach the user's display, bus, keyring or data.

use std::cell::RefCell;
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Entity, TestAppContext, WindowingModes};
use teams_auth::broker::{BrokerScope, serve};
use teams_auth::secrets::MemorySecretStore;
use teams_fence::lab::{Alias, LabConfig, LabLocations, LabPaths};
use teams_store::EngineLock;

use super::{broker, prepare, run_app, run_window};

const CASE: &str = "TEAMS_APP_STARTUP_TEST_CASE";

thread_local! {
    static LAUNCHED: RefCell<Option<teams_app::app::Launched>> = const { RefCell::new(None) };
}

pub(super) mod app {
    use super::*;
    pub(crate) use teams_app::app::{install_log, quit_after, start_failure};

    pub(crate) fn launch(
        cx: &mut App,
        launch: teams_app::app::Launch,
    ) -> Result<teams_app::app::Launched, teams_app::app::LaunchFailed> {
        if std::env::var(CASE).unwrap() == "window-error" {
            let stopped = cx.background_spawn(async move {
                for account in std::iter::once(launch.first).chain(launch.others) {
                    account.engine.shutdown();
                }
            });
            return Err(teams_app::app::LaunchFailed {
                error: "synthetic window failure".to_owned(),
                stopped,
            });
        }
        let launched = teams_app::app::launch(cx, launch)?;
        LAUNCHED.with(|held| {
            *held.borrow_mut() = Some(teams_app::app::Launched {
                window: launched.window,
                bridge: launched.bridge.clone(),
                bridges: launched.bridges.clone(),
                desktop: launched.desktop.clone(),
            });
        });
        Ok(launched)
    }
}

pub(super) mod platform {
    use super::*;

    pub(crate) struct Platform(TestAppContext);

    pub(crate) fn linux(_: WindowingModes) -> Platform {
        Platform(TestAppContext::single())
    }

    impl Platform {
        pub(crate) fn run(self, launch: impl FnOnce(&mut App) + 'static) {
            let cx = self.0;
            cx.update(launch);
            cx.run_until_parked();
            let launched = LAUNCHED.with(|held| held.borrow_mut().take());
            if let Some(launched) = launched {
                quit(&cx, &launched);
                wait(&cx, &launched.bridges);
            }
            cx.run_until_parked();
        }
    }

    fn quit(cx: &TestAppContext, launched: &teams_app::app::Launched) {
        cx.update(|cx| {
            launched.desktop.update(cx, |desktop, cx| {
                desktop.tray_asked(teams_ui::tray::TrayEvent::Quit, cx);
            });
        });
    }

    fn wait(cx: &TestAppContext, bridges: &[Entity<teams_ui::Bridge>]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            cx.run_until_parked();
            if cx.read(|cx| bridges.iter().all(|bridge| !bridge.read(cx).running())) {
                return;
            }
            assert!(Instant::now() < deadline, "synthetic engines did not shut down");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn startup_cases_adopt_descriptors_start_only_fenced_engines_and_never_use_personal_resources() {
    for case in [
        "refusal",
        "bad-socket",
        "bad-trace",
        "window-denied",
        "no-config",
        "empty",
        "first-fails",
        "later-fails",
        "single",
        "traced",
        "window",
        "window-error",
        "broker-closed",
        "broker-malformed",
        "broker-bad-fd",
        "broker-no-config",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "startup_tests::child", "--nocapture", "--test-threads", "1"])
            .env_clear()
            .env(CASE, case)
            .env("HOME", directory.path().join("home"))
            .env("XDG_CONFIG_HOME", directory.path().join("config"))
            .env("XDG_DATA_HOME", directory.path().join("data"))
            .env("XDG_CACHE_HOME", directory.path().join("cache"))
            .env("XDG_RUNTIME_DIR", directory.path().join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-user-bus.sock", directory.path().display()),
            );
        if case != "window-denied" {
            child
                .env("UI_SANDBOX_DIR", directory.path().join("sandbox"))
                .env("WAYLAND_DISPLAY", directory.path().join("sandbox/synthetic-wayland"));
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

fn fixture() -> LabConfig {
    let locations = LabLocations::from_env().unwrap();
    std::fs::create_dir_all(locations.config_file.parent().unwrap()).unwrap();
    std::fs::create_dir_all(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).unwrap();
    let text = teams_mock::lab_toml(&locations.data_dir);
    std::fs::write(&locations.config_file, &text).unwrap();
    let config = LabConfig::load(&locations).unwrap();
    for alias in Alias::ALL {
        let region = config.paths().region_file(alias);
        std::fs::create_dir_all(region.parent().unwrap()).unwrap();
        std::fs::write(
            region,
            serde_json::json!({ "_synthetic": true, "region": "amer", "partition": null,
            "regionGtms": { "chatServiceAfd": teams_mock::CHAT_BASE } })
            .to_string(),
        )
        .unwrap();
    }
    config
}

fn socket(config: &LabConfig, alias: Alias) -> (i32, JoinHandle<()>) {
    let (server, client) = UnixStream::pair().unwrap();
    let upn = config.account(alias).upn.clone();
    let serving = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        server.set_nonblocking(true).unwrap();
        runtime.block_on(async move {
            let server = tokio::net::UnixStream::from_std(server).unwrap();
            let _ = serve(
                server,
                &MemorySecretStore::new(),
                &BrokerScope::new(LabPaths::KEYRING_APPLICATION, upn),
                |_| {},
            )
            .await;
        });
    });
    (client.into_raw_fd(), serving)
}

fn finish(launch: teams_app::app::Launch) {
    for account in std::iter::once(launch.first).chain(launch.others) {
        account.engine.shutdown();
    }
}

#[test]
fn child() {
    let Ok(case) = std::env::var(CASE) else {
        return;
    };
    let config = fixture();
    match case.as_str() {
        "refusal" => {
            let (socket, _peer) = UnixStream::pair().unwrap();
            assert_eq!(run_app(&[(Alias::A, socket.as_raw_fd())], None), ExitCode::FAILURE);
            assert!(
                socket.peer_addr().is_ok(),
                "the hard refusal runs before descriptor ownership changes"
            );
        }
        "bad-socket" => assert!(prepare(&[(Alias::A, 0)], None).is_err()),
        "bad-trace" => {
            let (fd, serving) = socket(&config, Alias::A);
            let trace = std::fs::File::open(LabLocations::from_env().unwrap().config_file)
                .unwrap()
                .into_raw_fd();
            assert!(prepare(&[(Alias::A, fd)], Some(trace)).is_err());
            serving.join().unwrap();
        }
        "window-denied" | "no-config" | "first-fails" => {
            if case == "no-config" {
                std::fs::remove_file(LabLocations::from_env().unwrap().config_file).unwrap();
            }
            if case == "first-fails" {
                std::fs::remove_file(config.paths().region_file(Alias::A)).unwrap();
            }
            let (fd, serving) = socket(&config, Alias::A);
            assert!(prepare(&[(Alias::A, fd)], None).is_err());
            serving.join().unwrap();
        }
        "empty" => assert!(prepare(&[], None).is_err()),
        "single" | "traced" | "later-fails" | "window" | "window-error" => running(&case, &config),
        "broker-bad-fd" => assert_eq!(broker(Alias::A, 0), ExitCode::FAILURE),
        "broker-closed" | "broker-malformed" | "broker-no-config" => {
            use std::io::Write as _;
            let (socket, mut peer) = UnixStream::pair().unwrap();
            if case == "broker-no-config" {
                std::fs::remove_file(LabLocations::from_env().unwrap().config_file).unwrap();
            }
            if case == "broker-malformed" {
                peer.write_all(b"synthetic malformed frame\n").unwrap();
            }
            drop(peer);
            let status = broker(Alias::A, socket.into_raw_fd());
            assert_eq!(
                status,
                if case == "broker-closed" {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            );
        }
        _ => panic!("unknown synthetic startup case {case}"),
    }
}

fn running(case: &str, config: &LabConfig) {
    let (first, first_server) = socket(config, Alias::A);
    let mut serving = vec![first_server];
    let mut accounts = vec![(Alias::A, first)];
    if case == "later-fails" {
        std::fs::remove_file(config.paths().region_file(Alias::B)).unwrap();
        for alias in [Alias::B, Alias::C] {
            let (fd, server) = socket(config, alias);
            accounts.push((alias, fd));
            serving.push(server);
        }
    }
    let trace = (case == "traced").then(|| {
        std::fs::File::create(config.paths().data_dir().join("synthetic-trace.jsonl"))
            .unwrap()
            .into_raw_fd()
    });
    let launch = prepare(&accounts, trace).unwrap();
    assert_eq!(launch.first.alias, Alias::A);
    assert_eq!(
        launch.others.iter().map(|account| account.alias).collect::<Vec<_>>(),
        if case == "later-fails" { vec![Alias::C] } else { vec![] }
    );
    assert!(launch.first.broker.is_some() && launch.first.signed_out.is_some());
    assert!(
        launch.first.calls.is_none(),
        "this process has no sandbox PipeWire socket"
    );
    if matches!(case, "window" | "window-error") {
        assert_eq!(
            run_window(launch),
            if case == "window" {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        );
    } else {
        finish(launch);
    }
    for server in serving {
        server.join().unwrap();
    }
    for alias in [Alias::A, Alias::C] {
        if config.paths().store_file(alias).exists() {
            drop(EngineLock::hold(&config.paths().store_file(alias)).unwrap());
        }
    }
}
