//! Restricted native CLI parsing and data-free operator failures. No browser, credentials or tenant access.
use super::*;
use proto_lab::commands::login::{LoginError, SignInStop};
use proto_lab::session::{SessionError, SessionReport};
use teams_auth::token::{EntraError, TokenError};

const PRIVATE: &str = "synthetic-sensitive-value-never-display";

fn command(args: &[&str]) -> Command {
    Cli::try_parse_from(std::iter::once("proto-lab").chain(args.iter().copied()))
        .unwrap()
        .command
}

#[test]
fn only_native_provisioning_and_read_only_evidence_commands_are_exposed() {
    assert!(matches!(command(&["browser"]), Command::Browser));
    assert!(matches!(
        command(&["login", "a"]),
        Command::Login {
            alias: Alias::A,
            native_alias: None,
            timeout: 300
        }
    ));
    assert!(matches!(
        command(&["auth", "b"]),
        Command::Auth {
            alias: Alias::B,
            native_alias: None,
            timeout: 120
        }
    ));
}

#[test]
fn aliases_timeouts_and_selected_targets_reach_the_typed_options() {
    assert!(
        matches!(command(&["refresh", "c", "--native-alias", "synthetic"]), Command::Refresh { alias: Alias::C, native_alias: Some(value) } if value == "synthetic")
    );
    assert!(
        matches!(command(&["login", "b", "--native-alias", "synthetic", "--timeout", "7"]), Command::Login { alias: Alias::B, native_alias: Some(value), timeout: 7 } if value == "synthetic")
    );
    assert!(
        matches!(command(&["reach", "a", "--chat", "synthetic-ab", "--chat", "synthetic-abc"]), Command::Reach { alias: Alias::A, chats, .. } if chats == ["synthetic-ab", "synthetic-abc"])
    );
}

#[test]
fn unsupported_commands_and_out_of_range_timeouts_are_usage_errors() {
    for args in [
        vec!["run", "ab-call"],
        vec!["probe", "a"],
        vec!["login", "outsider"],
        vec!["auth", "a", "--timeout", "0"],
        vec!["login", "a", "--timeout", "901"],
        vec!["refresh", "a", "--timeout", "10"],
    ] {
        assert!(Cli::try_parse_from(std::iter::once("proto-lab").chain(args)).is_err());
    }
}

fn entra(code: u32) -> EntraError {
    EntraError {
        error: PRIVATE.into(),
        codes: vec![code],
        description: PRIVATE.into(),
        correlation_id: Some(PRIVATE.into()),
        suberror: Some(PRIVATE.into()),
    }
}

#[test]
fn diagnostic_failures_never_echo_browser_service_or_private_input() {
    let failures: Vec<Box<dyn std::error::Error>> = vec![
        Box::new(std::io::Error::other(PRIVATE)),
        Box::new(auth::AuthError::Login(LoginError::UnexpectedHost(PRIVATE.into()))),
        Box::new(auth::AuthError::Login(LoginError::Timeout {
            timeout: Duration::from_secs(1),
            authz: None,
        })),
        Box::new(auth::AuthError::Login(LoginError::SignIn(SignInStop::MfaRegistration))),
        Box::new(auth::AuthError::Session(SessionError::ActionTargets(PRIVATE.into()))),
        Box::new(auth::AuthError::Provision(std::io::Error::other(PRIVATE))),
        Box::new(auth::AuthError::Token(TokenError::NotSignedIn {
            account: PRIVATE.into(),
        })),
        Box::new(auth::AuthError::Token(TokenError::ConditionalAccess(
            entra(53003).into(),
        ))),
        Box::new(auth::AuthError::Token(TokenError::InteractionRequired(
            entra(50076).into(),
        ))),
        Box::new(auth::AuthError::Redirect(
            teams_auth::redirect::RedirectError::InvalidRedirectUri,
        )),
        Box::new(auth::AuthError::InteractionNeeded),
        Box::new(auth::AuthError::Elsewhere(PRIVATE.into())),
    ];
    for error in failures {
        let message = failure(error.as_ref());
        assert!(!message.is_empty());
        assert!(!message.contains(PRIVATE), "{message}");
    }
    assert!(
        failure(&auth::AuthError::Token(TokenError::ConditionalAccess(
            entra(53003).into()
        )))
        .contains("53003")
    );
    assert!(
        failure(&auth::AuthError::Token(TokenError::InteractionRequired(
            entra(50076).into()
        )))
        .contains("50076")
    );
}

#[test]
fn a_report_without_native_tokens_and_a_test_support_entry_point_refuse_success() {
    assert!(
        show_report(&auth::AuthReport {
            browser: SessionReport::default(),
            signed_in: None,
            refreshed: None,
            audiences: Vec::new(),
            skypetoken: None,
        })
        .is_err()
    );
    assert_eq!(main(), ExitCode::FAILURE);
}

#[tokio::test]
async fn native_commands_run_only_with_private_inputs() {
    const CHILD: &str = "PROTO_LAB_SYNTHETIC_NATIVE_COMMANDS";
    if std::env::var_os(CHILD).is_some() {
        native_commands().await;
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child.env_clear();
    for name in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
        "TEMP",
        "TMP",
        "LLVM_PROFILE_FILE",
        "NGTEAMS_WINDOWS_VM_SANDBOX",
        "UI_SANDBOX_DIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            child.env(name, value);
        }
    }
    child
        .env(CHILD, "1")
        .env("LOCALAPPDATA", root.path())
        .kill_on_drop(true);
    child.args([
        "--exact",
        "main_windows::tests::native_commands_run_only_with_private_inputs",
        "--nocapture",
    ]);
    let output = tokio::time::timeout(Duration::from_secs(30), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn native_commands() {
    let locations = LabLocations::from_env().unwrap();
    let target = auth::NativeTarget::windows_alias("synthetic-cli").unwrap();
    assert_eq!(
        auth::NativeTarget::from_env(Alias::A).unwrap().root(),
        locations.data_dir.parent().unwrap().join("ngteams-lab/a")
    );
    run(Command::Browser).await.unwrap();
    let text = ["a", "b", "c"]
        .map(|alias| format!("[[accounts]]\nalias='{alias}'\nupn='synthetic-{alias}@example.test'\n"))
        .join("\n");
    std::fs::write(&locations.config_file, text).unwrap();
    target
        .save(
            &teams_auth::id_token::Account {
                username: "synthetic-another@example.test".into(),
                object_id: "00000000-0000-4000-8000-00000000000a".into(),
                tenant: "00000000-0000-4000-8000-000000000001".into(),
                display_name: None,
            },
            None,
        )
        .unwrap();
    proto_lab::private::atomic(
        &accounts_file(&locations),
        br#"[{"profile":"a","email":"synthetic-a@example.test","password":"synthetic-only"}]"#,
    )
    .unwrap();
    public_entry_refusals(&locations, &target).await;
    for command in [
        Command::Auth {
            alias: Alias::A,
            native_alias: Some("synthetic-cli".into()),
            timeout: 1,
        },
        Command::Login {
            alias: Alias::A,
            native_alias: Some("synthetic-cli".into()),
            timeout: 1,
        },
        Command::Refresh {
            alias: Alias::A,
            native_alias: Some("synthetic-cli".into()),
        },
        Command::Reach {
            alias: Alias::A,
            native_alias: Some("synthetic-cli".into()),
            chats: vec!["synthetic-ab".into()],
        },
    ] {
        assert!(run(command).await.is_err());
    }
    // All entry paths released their live lock after refusal; no token read or browser navigation occurred.
    drop(proto_lab::private::exclusive(&locations.data_dir.join("live.lock")).unwrap());
}

async fn public_entry_refusals(locations: &LabLocations, target: &auth::NativeTarget) {
    let config = LabConfig::load(locations).unwrap();
    let wrong = target.account("synthetic-another@example.test").unwrap();
    auth::NativeTarget::from_env(Alias::A)
        .unwrap()
        .save(&wrong, None)
        .unwrap();
    let options = auth::AuthOptions {
        timeout: Duration::from_secs(1),
        skypetoken: true,
        other_audiences: false,
    };
    assert!(matches!(
        auth::authorize(
            &config,
            Alias::A,
            &teams_auth::secrets::MemorySecretStore::new(),
            &options
        )
        .await,
        Err(auth::AuthError::Provision(_))
    ));
    // The public login entry refuses a missing synthetic roster credential before opening any profile.
    assert!(
        proto_lab::commands::login::login(
            Alias::B,
            proto_lab::commands::login::LoginOptions {
                timeout: Duration::from_secs(1)
            }
        )
        .await
        .is_err()
    );
    assert!(!config.paths().profile_dir(Alias::B).exists());
}
