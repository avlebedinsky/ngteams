//! Lab startup controllers use only disposable files, socket pairs and the existing mock GPUI/account backend.

use std::fs;
use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode};

use clap::Parser as _;
use ngteams::accounts_file::PersonalAccount;
use ngteams::cli::Cli;

use super::{Outcome, lab_broker, run_lab};

const CASE: &str = "NGTEAMS_LAB_STARTUP_CASE";
const OID: &str = "00000000-0000-4000-8000-0000000000ff";
const TID: &str = "00000000-0000-4000-8000-000000000001";

#[test]
fn lab_startup_and_broker_use_alias_files_and_preserve_restart_descriptors() {
    for case in [
        "ready",
        "ready-ctl",
        "pending",
        "pending-closed",
        "pending-failed",
        "pending-shortcut-closed",
        "pending-shortcut-failed",
        "wrong-account",
        "no-sandbox",
        "broker",
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lab_startup_tests::lab_child",
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env_clear()
            .env(CASE, case)
            .env(
                "NGTEAMS_STARTUP_TEST_CASE",
                match case {
                    "pending" => "first-success",
                    "pending-closed" | "pending-shortcut-closed" => "first-lab-closed-error",
                    "pending-failed" | "pending-shortcut-failed" => "first-lab-failed-error",
                    _ => "lab-ready",
                },
            )
            .env("HOME", root.path().join("home"))
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("XDG_DATA_HOME", root.path().join("data"))
            .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-user-bus", root.path().display()),
            )
            .env("UI_SANDBOX_DIR", root.path().join("sandbox"))
            .env("WAYLAND_DISPLAY", root.path().join("sandbox/synthetic-wayland"))
            .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|value| ("LLVM_PROFILE_FILE", value)))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !root.path().join("config/ngteams").exists(),
            "{case}: personal config untouched"
        );
        assert!(
            !root.path().join("data/ngteams").exists(),
            "{case}: personal data untouched"
        );
    }
}

#[test]
fn lab_child() {
    let Ok(case) = std::env::var(CASE) else { return };
    fs::create_dir_all(std::env::var_os("XDG_RUNTIME_DIR").unwrap()).unwrap();
    let paths = ngteams::lab::paths_from_env("a").unwrap();
    let pending = PersonalAccount::pending("pending@example.test").unwrap();
    let account = if case.starts_with("pending") {
        pending.clone()
    } else {
        PersonalAccount::new("ready@example.test", OID, TID).unwrap()
    };
    account.save(&paths).unwrap();
    if let Some(region) = paths.region_file(&account) {
        fs::create_dir_all(region.parent().unwrap()).unwrap();
        fs::write(
            region,
            format!(
                r#"{{"region":"amer","partition":null,"regionGtms":{{"chatServiceAfd":"{}"}}}}"#,
                teams_mock::CHAT_BASE
            ),
        )
        .unwrap();
    }
    if case == "broker" {
        broker_cases(&paths, &pending);
        return;
    }
    if case.starts_with("pending-") {
        pending_failure(&case, &paths, &account);
        return;
    }
    let (peer, socket) = UnixStream::pair().unwrap();
    let mut args = vec![
        "ngteams".to_owned(),
        "--lab".into(),
        "a".into(),
        "--secrets-fd".into(),
        socket.into_raw_fd().to_string(),
    ];
    if case == "wrong-account" {
        args.extend(["--account".into(), "other@example.test".into()]);
    }
    // where its runner puts it: the alias's state dir
    let ctl = std::path::Path::new(&std::env::var_os("HOME").unwrap()).join(".local/state/ngteams-lab/a/ctl.sock");
    if case == "ready-ctl" {
        args.extend(["--ctl-socket".into(), ctl.display().to_string()]);
    }
    let cli = Cli::try_parse_from(args).unwrap();
    let mode = cli.lab_mode().unwrap().unwrap();
    if case == "no-sandbox" {
        // SAFETY: this isolated subprocess has not started any runtime/thread or GUI backend yet.
        unsafe { std::env::set_var("DISPLAY", ":synthetic-forbidden") };
    }
    let outcome = run_lab(&cli, &mode);
    let served: Vec<String> = crate::startup_tests::calls()
        .lock()
        .unwrap()
        .iter()
        .filter(|call| call.starts_with("ctl-"))
        .cloned()
        .collect();
    // only where the runner said; none (and never the personal one) without it
    let expected = if case == "ready-ctl" {
        vec![format!("ctl-bound:{}", ctl.display())]
    } else {
        Vec::new()
    };
    assert_eq!(served, expected, "{case}");
    assert!(!ctl.exists(), "{case}: the quit removes the socket");
    match (case.as_str(), outcome) {
        ("ready" | "ready-ctl", Ok(Outcome::Quit)) => assert!(paths.store_file(&account).unwrap().exists()),
        ("pending", Ok(Outcome::LabRestart(descriptors))) => assert!(descriptors.socket().is_ok()),
        ("wrong-account", Err(error)) => assert!(error.contains("configured for")),
        ("no-sandbox", Err(error)) => assert!(error.contains("DISPLAY")),
        (_, Err(error)) => panic!("{case}: {error}"),
        _ => panic!("{case}: unexpected startup outcome"),
    }
    drop(peer);
}

/// Exercise the actual add/sign-in failure distinction, not merely a fake backend returning an error. Neither
/// answer gives an authorization code, so the loopback-only clients make no HTTP calls and the broker is unused.
pub(super) fn failed_sign_in(
    setup: &ngteams::setup::Setup,
    account: &PersonalAccount,
    adding: bool,
) -> Result<PersonalAccount, String> {
    use ngteams::signin::{self, SignInClients};
    use teams_auth::authz::AuthzClient;
    use teams_auth::signin::SignInAnswer;
    use teams_auth::token::{EntraConfig, TokenClient};

    let entra = EntraConfig::teams_native().with_authority("http://127.0.0.1:1/organizations/");
    let clients = SignInClients {
        token: TokenClient::new(entra.clone()).unwrap(),
        authz: AuthzClient::with_endpoint("http://127.0.0.1:1/api/authsvc/v1.0/authz").unwrap(),
        entra,
    };
    let window = || -> signin::Window {
        Box::new(|_| {
            if std::env::var(CASE).unwrap().ends_with("closed") {
                SignInAnswer::Closed
            } else {
                SignInAnswer::Failed("synthetic sign-in window failure".into())
            }
        })
    };
    let store = setup.secrets();
    ngteams::accounts::runtime().unwrap().block_on(async {
        if adding {
            signin::add(setup, &account.upn, &clients, &store, |_| Ok(window())).await
        } else {
            signin::sign_in(setup, account, &clients, &store, window())
                .await
                .map_err(|error| signin::failure(setup, account, &error))
        }
    })
}

fn pending_failure(case: &str, paths: &ngteams::accounts_file::PersonalPaths, account: &PersonalAccount) {
    let before = fs::read(paths.accounts_file()).unwrap();
    for _ in 0..2 {
        let (peer, socket) = UnixStream::pair().unwrap();
        let mut args = vec![
            "ngteams".to_owned(),
            "--lab".into(),
            "a".into(),
            "--secrets-fd".into(),
            socket.into_raw_fd().to_string(),
        ];
        if case.contains("shortcut") {
            args.extend(["--account".into(), account.upn.clone()]);
        }
        let cli = Cli::try_parse_from(args).unwrap();
        let mode = cli.lab_mode().unwrap().unwrap();
        let error = run_lab(&cli, &mode).err().expect("synthetic sign-in must fail");
        assert!(
            error.contains(if case.ends_with("closed") {
                "closed before the sign-in finished"
            } else {
                "synthetic sign-in window failure"
            }),
            "retry must reach sign-in, not reject the alias: {error}"
        );
        assert_eq!(fs::read(paths.accounts_file()).unwrap(), before);
        assert_eq!(ngteams::lab::account(paths).unwrap(), *account);
        drop(peer);
        let (peer, socket) = UnixStream::pair().unwrap();
        drop(peer);
        assert_eq!(lab_broker("a", socket.into_raw_fd()), ExitCode::SUCCESS);
    }
}

fn broker_cases(paths: &ngteams::accounts_file::PersonalPaths, pending: &PersonalAccount) {
    let closed_broker = || {
        let (peer, socket) = UnixStream::pair().unwrap();
        drop(peer);
        lab_broker("a", socket.into_raw_fd())
    };
    assert_eq!(lab_broker("a", -1), ExitCode::FAILURE);
    assert_eq!(closed_broker(), ExitCode::SUCCESS, "EOF opens no lazy keyring");
    pending.save(paths).unwrap();
    assert_eq!(
        closed_broker(),
        ExitCode::FAILURE,
        "two accounts cannot share a scoped alias"
    );
    PersonalAccount::remove(paths, "ready@example.test").unwrap();
    assert_eq!(
        closed_broker(),
        ExitCode::SUCCESS,
        "pending account can sign in through the broker"
    );
}
