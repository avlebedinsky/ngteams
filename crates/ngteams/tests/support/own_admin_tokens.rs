//! What the Manage users page says when the account's token wasn't had (`own_admin::token_problem`): a lasting answer
//! ends the app's admin check, a passing one is asked again, so each token failure must land on the right side.

use std::time::Duration;

use teams_auth::token::{EntraError, TokenError};
use teams_ui::user_admin::AdminProblem;

use super::token_problem;

fn grant_refused() -> EntraError {
    EntraError {
        error: "invalid_grant".to_owned(),
        codes: Vec::new(),
        description: "the refresh token was revoked".to_owned(),
        correlation_id: None,
        suberror: None,
    }
}

#[test]
fn a_server_that_cant_be_reached_or_asks_to_wait_is_said_so() {
    // a request that never left: its error is the one a refused connection gives (`TokenError::Http`)
    let unsent = reqwest::Client::new().get("not an address").build().unwrap_err();
    assert_eq!(token_problem(&TokenError::Http(unsent)), AdminProblem::Unreachable);
    let throttled = TokenError::Throttled {
        retry_after: Some(Duration::from_secs(30)),
        error: None,
    };
    assert_eq!(token_problem(&throttled), AdminProblem::Wait);
}

#[test]
fn an_account_whose_sign_in_is_gone_is_signed_out() {
    let none = TokenError::NotSignedIn {
        account: "dana@own.example.test".to_owned(),
    };
    assert_eq!(token_problem(&none), AdminProblem::SignedOut);
    assert_eq!(
        token_problem(&TokenError::InvalidGrant(Box::new(grant_refused()))),
        AdminProblem::SignedOut
    );
}

#[test]
fn anything_else_is_named() {
    let AdminProblem::Other(said) = token_problem(&TokenError::StaleEntraToken) else {
        panic!("not named");
    };
    assert!(said.starts_with("Your sign-in couldn't be read: "), "{said}");
}
