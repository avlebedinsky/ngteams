//! Records the web client loading and settling. Session readiness, capture setup and browser shutdown determine the result.

use std::time::Duration;

use crate::commands::status::account_status;
use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use super::{AccountRun, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// How long the run keeps recording after the web client has loaded: its first syncs, the event channel coming up.
pub const LINGER: Duration = Duration::from_secs(20);

/// The account that loads.
pub const ACCOUNT: Alias = Alias::A;

/// Runs `load` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, no capture, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let session_options = SessionOptions {
        capture: Some(ScenarioName::Load.as_str().to_owned()),
        display: options.display,
        ..SessionOptions::default()
    };
    let session = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    finish(session, options, LINGER).await
}

async fn finish(
    mut session: LabSession,
    options: RunOptions,
    duration: Duration,
) -> Result<ScenarioReport, ScenarioError> {
    let state = match session.navigate(TEAMS_URL).await {
        Ok(()) => {
            let state = session.settle(options.load_timeout).await;
            if state == SessionState::Loaded {
                linger(&mut session, duration).await;
            }
            Ok(state)
        }
        Err(error) => Err(error),
    };
    let last_authz = session.watch().last_authz_status();
    let held_requests = session.held_requests();
    // the browser goes down, and the capture is finished, whatever happened
    let (browser, capture) = session.close_recorded().await?;
    let state = state?;
    Ok(report(
        &state,
        options,
        last_authz,
        AccountRun {
            alias: ACCOUNT,
            browser,
            held_requests,
            capture,
        },
    ))
}

fn report(state: &SessionState, options: RunOptions, last_authz: Option<u16>, run: AccountRun) -> ScenarioReport {
    let mut report = ScenarioReport::new(ScenarioName::Load);
    let status = account_status(state, options.load_timeout, last_authz);
    report.notes.push(format!("{ACCOUNT}: {status}"));
    report.notes.push(format!("{ACCOUNT}: {}", run.browser.summary()));
    if let Some(dir) = &run.capture {
        report.notes.push(format!("recorded into {}", dir.display()));
    }
    if state != &SessionState::Loaded {
        report
            .failures
            .push(format!("{ACCOUNT}: the web client didn't load: {status}"));
    }
    report.add_account(run);
    report
}

/// Keeps the session's events flowing (and recorded) for `duration`, or until the connection goes.
async fn linger(session: &mut LabSession, duration: Duration) {
    let end = tokio::time::Instant::now() + duration;
    while let Some(left) = end.checked_duration_since(tokio::time::Instant::now()) {
        if left.is_zero() || !session.pump(left.min(Duration::from_millis(500))).await {
            return;
        }
    }
}

#[cfg(test)]
#[path = "../../tests/support/scenario/load_tests.rs"]
mod tests;
