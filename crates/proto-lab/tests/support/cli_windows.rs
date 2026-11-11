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
