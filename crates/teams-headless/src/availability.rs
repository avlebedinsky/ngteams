//! `teams-headless <alias> availability`: the account's own availability set from its push channel's endpoint (M2 Task
//! 19), for the live check of what another account sees (`docs/protocol/trouter.md`, "Own presence").
//!
//! Once the push channel's registration went through, the endpoint's presence `PUT` (available), then, if asked, a
//! manual status and its reset, each at its time counted from the `PUT`; the push channel stays up until the hold is
//! over (or a signal), and the command ends, which takes the endpoint down. Every step goes out through the engine
//! ([`teams_core::EngineRequests::own_availability`]): a transient write of the outbox under the fence's permit for the
//! account's own fence, logged to `fence.log`. Each step is printed with its time: `availability <step> <outcome>
//! at_ms=<ms>`.

use std::future::Future;
use std::io::Write;
use std::time::Duration;

use teams_core::{EngineHandle, Event, OwnStatus, TransientSent};
use teams_proto::ManualAvailability;
use teams_trouter::{RegistrationReport, Stop};
use tokio::time::Instant;

use crate::commands::CommandError;
use crate::print::{now_ms, visible};

/// What the command sets when, counted from the endpoint's `PUT` (sent once the registration went through).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// A manual status to set, and how long after the `PUT`.
    pub manual: Option<(ManualAvailability, Duration)>,
    /// How long after the `PUT` the manual status is cleared, if it is.
    pub reset_after: Option<Duration>,
    /// How long after the `PUT` the command ends.
    pub hold: Duration,
    /// How long it waits for the registration.
    pub register_wait: Duration,
}

/// How the command ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Every step went out; `all_sent` when the service took each.
    Done { all_sent: bool },
    /// Its owner stopped it (a signal).
    Stopped,
    /// No registration went through within the wait: nothing was set.
    NotRegistered,
    /// The push channel stopped for good, and why.
    PushStopped(Stop),
    /// The engine stopped.
    EngineStopped,
}

/// Runs `plan` on `handle` (an engine with its push channel), printing each step to `out`, until it is over, `stop`
/// is ready, the push channel stops or the engine does.
///
/// # Errors
/// When the output can't be written.
pub async fn run(
    handle: &mut EngineHandle,
    plan: Plan,
    out: &mut impl Write,
    stop: impl Future<Output = ()>,
) -> Result<End, CommandError> {
    tokio::pin!(stop);
    if let Some(end) = registered(handle, plan.register_wait, out, &mut stop).await? {
        return Ok(end);
    }
    let start = Instant::now();
    let mut steps = vec![(Duration::ZERO, OwnStatus::Available)];
    if let Some((status, after)) = plan.manual {
        steps.push((after, OwnStatus::Manual(status)));
    }
    if let Some(after) = plan.reset_after {
        steps.push((after, OwnStatus::ClearManual));
    }
    steps.sort_by_key(|(after, _)| *after);
    let mut all_sent = true;
    for (after, status) in steps {
        if let Some(end) = drain_until(handle, start + after, out, &mut stop).await? {
            return Ok(end);
        }
        let sent = handle.own_availability(status).await;
        all_sent &= matches!(sent, Ok(TransientSent::Sent));
        let outcome = match sent {
            Ok(sent) => sent_word(&sent),
            Err(error) => format!("refused: {}", visible(&error.to_string())),
        };
        writeln!(out, "availability {} {outcome} at_ms={}", step_word(status), now_ms())?;
        out.flush()?;
    }
    if let Some(end) = drain_until(handle, start + plan.hold, out, &mut stop).await? {
        return Ok(end);
    }
    Ok(End::Done { all_sent })
}

/// Waits for the push channel's registration to go through, printing it; `Some` when the command ends first.
async fn registered(
    handle: &mut EngineHandle,
    wait: Duration,
    out: &mut impl Write,
    stop: &mut (impl Future<Output = ()> + Unpin),
) -> Result<Option<End>, CommandError> {
    let deadline = Instant::now() + wait;
    loop {
        let event = tokio::select! {
            () = &mut *stop => return Ok(Some(End::Stopped)),
            () = tokio::time::sleep_until(deadline) => return Ok(Some(End::NotRegistered)),
            event = handle.next_event() => event,
        };
        match event {
            None => return Ok(Some(End::EngineStopped)),
            Some(Event::PushStopped(why)) => return Ok(Some(End::PushStopped(why))),
            Some(Event::Registration(RegistrationReport::Registered)) => {
                writeln!(out, "availability registered at_ms={}", now_ms())?;
                return Ok(None);
            }
            Some(Event::Registration(RegistrationReport::Failed(why))) => {
                writeln!(
                    out,
                    "availability registration_failed {} at_ms={}",
                    visible(&why),
                    now_ms()
                )?;
            }
            Some(_) => {}
        }
    }
}

/// Takes the engine's events until `deadline`, printing how a step that waited out a `Retry-After` went; `Some` when
/// the command ends first.
async fn drain_until(
    handle: &mut EngineHandle,
    deadline: Instant,
    out: &mut impl Write,
    stop: &mut (impl Future<Output = ()> + Unpin),
) -> Result<Option<End>, CommandError> {
    loop {
        let event = tokio::select! {
            () = &mut *stop => return Ok(Some(End::Stopped)),
            () = tokio::time::sleep_until(deadline) => return Ok(None),
            event = handle.next_event() => event,
        };
        match event {
            None => return Ok(Some(End::EngineStopped)),
            Some(Event::PushStopped(why)) => return Ok(Some(End::PushStopped(why))),
            Some(Event::Transient(transient, sent)) => {
                let (kind, sent) = (transient.kind(), sent_word(&sent));
                writeln!(out, "availability {kind} {sent} after_its_wait at_ms={}", now_ms())?;
            }
            Some(_) => {}
        }
    }
}

/// A step as printed: `available`, `manual <status>` or `reset`.
fn step_word(status: OwnStatus) -> String {
    match status {
        OwnStatus::Available => "available".to_owned(),
        OwnStatus::Manual(status) => format!("manual {}", status.as_str()),
        OwnStatus::ClearManual => "reset".to_owned(),
    }
}

/// How a step's send went, as printed.
fn sent_word(sent: &TransientSent) -> String {
    match sent {
        TransientSent::Sent => "sent".to_owned(),
        TransientSent::Waiting { not_before } => format!("waiting not_before_ms={not_before}"),
        TransientSent::Denied { reason } => format!("denied: {}", visible(reason)),
        TransientSent::Dropped { reason } => format!("dropped: {}", visible(reason)),
    }
}
