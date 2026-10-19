//! `proto-lab status`: whether each lab account's session still works, without signing anything in.
//!
//! For each account its persistent profile opens Teams, headless, under the armed fence (fail-closed, the seeded
//! own-state list only: no sign-in form gets through). The web client loading means `ok`; Microsoft's sign-in page
//! standing in its place means `login needed`. A signed-in account Teams refuses (its authz exchange refused: no
//! Teams license) is reported as such, and anything else (another host, no answer in time) as it is.

use std::fmt;
use std::time::Duration;

use crate::classify::Classifier;
use crate::config::{Alias, LabPaths};
use crate::session::{FenceReport, LabSession, SessionError, SessionState, TEAMS_URL};

/// One account's session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountStatus {
    /// The web client loaded with a working session.
    Ok,
    /// Microsoft asks to sign in: run `proto-lab login`.
    LoginNeeded,
    /// Signed in, but Teams refused the account (its authz exchange got this status): it can't use Teams, typically
    /// for want of a Teams license. Signing in again doesn't change that.
    Refused(u16),
    /// Neither, for this reason.
    Unknown(String),
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::LoginNeeded => f.write_str("login needed"),
            Self::Refused(status) => write!(f, "signed in, but Teams refused the account (authz answered {status})"),
            Self::Unknown(reason) => write!(f, "unknown ({reason})"),
        }
    }
}

/// What a session state says about an account. `timeout` names how long it was given, and `last_authz` what the
/// authz exchange last answered, for a session that is still loading.
#[must_use]
pub fn account_status(state: &SessionState, timeout: Duration, last_authz: Option<u16>) -> AccountStatus {
    match state {
        SessionState::Loaded => AccountStatus::Ok,
        SessionState::SignInShown => AccountStatus::LoginNeeded,
        SessionState::Refused(status) => AccountStatus::Refused(*status),
        SessionState::Elsewhere(host) => AccountStatus::Unknown(format!("the page stopped on {host}")),
        SessionState::Loading => {
            let authz = last_authz.map_or_else(
                || "no authz answer".to_owned(),
                |status| format!("authz answered {status}"),
            );
            AccountStatus::Unknown(format!("the web client didn't load within {timeout:?}; {authz}"))
        }
    }
}

/// Opens Teams in `alias`'s profile under the fence and tells what became of the session, with what the fence did.
///
/// # Errors
/// When the browser can't be run under the fence, or had to be killed on closing.
pub async fn check(
    paths: &LabPaths,
    alias: Alias,
    timeout: Duration,
) -> Result<(AccountStatus, FenceReport), SessionError> {
    let mut session = LabSession::open(paths, alias, Classifier::seeded()).await?;
    let state = match session.navigate(TEAMS_URL).await {
        Ok(()) => Ok(session.settle(timeout).await),
        Err(error) => Err(error),
    };
    let last_authz = session.watch().last_authz_status();
    // the browser goes down whatever happened
    let fence = session.close().await?;
    Ok((account_status(&state?, timeout, last_authz), fence))
}
