//! The `load` scenario: account a opens the Teams web client under the **fail-closed** fence (the seeded own-state list
//! only, no write mapped), waits for it to load, keeps recording a while, and closes. The run is recorded
//! (`captures/load/<ts>/`); every request the fence denies is expected and is in the capture as data for mapping writes
//! (`docs/protocol/fence-targets.md`).
//!
//! It fails when the web client doesn't load (Teams refusing the account, a sign-in page, no answer in time) and when
//! the fence is breached. Denies don't fail it ([`DenyPolicy::AllExpected`]).

use std::time::Duration;

use crate::classify::Classifier;
use crate::commands::status::account_status;
use crate::config::{Alias, LabPaths};
use crate::session::{LabSession, SessionOptions, SessionState, TEAMS_URL};

use super::{AccountRun, DenyPolicy, RunOptions, ScenarioError, ScenarioName, ScenarioReport};

/// How long the run keeps recording after the web client has loaded: its first syncs, the event channel coming up.
pub const LINGER: Duration = Duration::from_secs(20);

/// The account that loads.
pub const ACCOUNT: Alias = Alias::A;

/// Runs `load` (see the module docs).
///
/// # Errors
/// When the run can't start: no Chrome, no fence, no capture, a browser that can't be armed.
pub async fn run(paths: &LabPaths, options: RunOptions) -> Result<ScenarioReport, ScenarioError> {
    let session_options = SessionOptions {
        capture: Some(ScenarioName::Load.as_str().to_owned()),
        display: options.display,
        ..SessionOptions::fail_closed(Classifier::seeded())
    };
    let mut session = LabSession::open_with(paths, ACCOUNT, session_options).await?;
    let state = match session.navigate(TEAMS_URL).await {
        Ok(()) => {
            let state = session.settle(options.load_timeout).await;
            if state == SessionState::Loaded {
                linger(&mut session, LINGER).await;
            }
            Ok(state)
        }
        Err(error) => Err(error),
    };
    let last_authz = session.watch().last_authz_status();
    let denials = session.denials();
    let frame_denials = session.frame_denials();
    // the browser goes down, and the capture is finished, whatever happened
    let (fence, capture) = session.close_recorded().await?;
    let state = state?;
    let mut report = ScenarioReport::new(ScenarioName::Load);
    let status = account_status(&state, options.load_timeout, last_authz);
    report.notes.push(format!("{ACCOUNT}: {status}"));
    report
        .notes
        .push(format!("{ACCOUNT}: {} (fail-closed: expected)", fence.summary()));
    if let Some(dir) = &capture {
        report.notes.push(format!("recorded into {}", dir.display()));
    }
    if state != SessionState::Loaded {
        report
            .failures
            .push(format!("{ACCOUNT}: the web client didn't load: {status}"));
    }
    report.add_account(
        AccountRun {
            alias: ACCOUNT,
            fence,
            denials,
            frame_denials,
            capture,
        },
        &DenyPolicy::AllExpected,
    );
    Ok(report)
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
