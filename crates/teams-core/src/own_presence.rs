//! The app's policy for the account's own presence (M2 Task 20): when to send which of the engine's own-availability
//! writes ([`EngineRequests::own_availability`](crate::EngineRequests::own_availability)). It sends nothing itself:
//! each input (the push channel registered or stopped, the window used, a tick, the user's choice, a report's answer)
//! returns the [`OwnStatus`]es to send now, in order, and the app hands them to the engine. Time is the caller's, so
//! a test moves it.
//!
//! Only the requests Tasks 18–19 captured exist: the endpoint `PUT` (`Available`), the manual status (`Busy`,
//! `DoNotDisturb`, `Away`, `Available`) and its reset (`docs/protocol/trouter.md`, "Own presence"). Settled with Codex
//! (2026-10-23, `discussions/task-20-away-offline`):
//!
//! - **Available**: the endpoint `PUT` once a registration of the push channel went through, if the user used the
//!   window within the idle time (the app's start counts as use) or a manual status is chosen; then again every
//!   [`AVAILABLE_REFRESH`] while that holds. One `PUT` was read `Offline` 5 min 20 s after it (Task 19, run 2), so it
//!   has to be sent again; the interval is this policy's, not a captured one. A registration while idle sends nothing;
//!   the window used again sends it at once.
//! - **Idle** (Automatic, no use for the idle time): nothing is sent; the refresh stops and the endpoint lapses on its
//!   own. No captured request reports Away for one endpoint: the manual `Away` is account-wide server state that would
//!   override the account's other devices and outlive the app (a suspend, a crash), so it is never sent on the user's
//!   behalf. What others then see is the endpoint's lapse (`Offline` once, about 5 min after the last `PUT`), not
//!   Away.
//! - **Quit**: nothing is sent (no captured request reports Offline); the refresh stops, and the engine's shutdown
//!   closes the push channel, after which the endpoint goes. A manual status is left as the user set it.
//! - **A manual status** (the user's choice, [`Choice::Manual`]) is sent once when chosen and overrides Available
//!   until cleared ([`Choice::Automatic`]: the reset, sent only when a manual status was chosen before); the endpoint is
//!   kept reported meanwhile, idle or not (a manual status with no live endpoint was read `Offline`). The saved choice
//!   is applied once, at the run's first registration (the local choice wins over one made elsewhere since); Automatic
//!   at the start sends no reset. A choice made before a registration takes the saved one's place.
//! - **A failed report** is never sent again on its own: a manual status (or reset) that failed waits for the user to
//!   choose it again, which sends it again as a new choice ([`Reporting::NotSet`] meanwhile: what the account shows
//!   is not known), and a failed `PUT` doesn't bring the refresh forward (the refresh is a schedule, not a retry). A
//!   refusal before sending stops every report for the run. A report the engine had no endpoint for (its push channel stopped
//!   for good) ends the reports until a registration, and the choice it carried, if it still stands, goes out with it.

use std::time::{Duration, Instant};

use teams_proto::ManualAvailability;

use crate::engine::{EngineError, OwnStatus};
use crate::outbox::TransientSent;

/// How often the endpoint `PUT` is sent again while the account should show available: 2 minutes, so one refresh
/// that is dropped still leaves the next well before the 5 min 20 s after which one `PUT` was read `Offline`.
pub const AVAILABLE_REFRESH: Duration = Duration::from_secs(120);

/// The user's choice of status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Choice {
    /// Available while the window is used; nothing reported once idle.
    Automatic,
    /// A manual status, until cleared.
    Manual(ManualAvailability),
}

/// What the policy reports now, as the account menu shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reporting {
    /// No registration of the push channel went through yet (or it stopped): nothing can be reported.
    Connecting,
    /// Available, kept up by the refresh.
    Available,
    /// A manual status, the endpoint kept up by the refresh.
    Manual(ManualAvailability),
    /// The choice's manual status (or, for Automatic, its reset) failed: what the account shows is not known until the
    /// user chooses it again, which sends it again. The endpoint is still kept up by the refresh when it should be.
    NotSet(Choice),
    /// Idle: nothing is reported, and the endpoint lapses on its own.
    Inactive,
    /// A report was refused before sending: nothing more is reported in this run.
    Halted,
    /// The app is quitting, or the engine stopped: nothing more is reported.
    Quit,
}

/// Why the reports ended for the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ended {
    Denied,
    Quit,
}

/// The policy (see the module docs).
#[derive(Clone, Debug)]
pub struct OwnPresencePolicy {
    choice: Choice,
    idle_after: Duration,
    last_used: Instant,
    /// A registration went through and the push channel hasn't stopped since.
    live: bool,
    /// The refresh runs: the endpoint `PUT` was sent at `sent_at` for this registration and the account should still
    /// show available.
    reporting: bool,
    sent_at: Option<Instant>,
    /// The manual status (or its reset) waiting for a registration.
    pending: Option<OwnStatus>,
    /// The choice's manual status (or its reset) failed: choosing it again sends it again.
    failed: bool,
    ended: Option<Ended>,
}

impl OwnPresencePolicy {
    /// The policy at the app's start (`now`, which counts as use), with the saved `choice` and idle time.
    #[must_use]
    pub fn new(choice: Choice, idle_after: Duration, now: Instant) -> Self {
        Self {
            choice,
            idle_after,
            last_used: now,
            live: false,
            reporting: false,
            sent_at: None,
            pending: manual(choice),
            failed: false,
            ended: None,
        }
    }

    /// The user's choice now.
    #[must_use]
    pub fn choice(&self) -> Choice {
        self.choice
    }

    /// What is reported at `now`.
    #[must_use]
    pub fn reporting(&self, now: Instant) -> Reporting {
        match self.ended {
            Some(Ended::Denied) => return Reporting::Halted,
            Some(Ended::Quit) => return Reporting::Quit,
            None => {}
        }
        if !self.live {
            return Reporting::Connecting;
        }
        if self.failed {
            return Reporting::NotSet(self.choice);
        }
        match self.choice {
            Choice::Manual(status) => Reporting::Manual(status),
            Choice::Automatic if self.used_within(now) => Reporting::Available,
            Choice::Automatic => Reporting::Inactive,
        }
    }

    /// A registration of the push channel went through: the endpoint `PUT` for it if the account should show available,
    /// then the manual status waiting for it.
    pub fn registered(&mut self, now: Instant) -> Vec<OwnStatus> {
        if self.ended.is_some() {
            return Vec::new();
        }
        self.live = true;
        // each registration is reported anew
        self.reporting = false;
        let mut send = self.evaluate(now);
        send.extend(self.pending.take());
        send
    }

    /// The push channel stopped: nothing can be reported until a registration.
    pub fn stopped(&mut self) {
        self.live = false;
        self.reporting = false;
    }

    /// The window was used at `now`: the endpoint `PUT` at once if it had stopped for idle.
    pub fn active(&mut self, now: Instant) -> Vec<OwnStatus> {
        self.last_used = self.last_used.max(now);
        self.evaluate(now)
    }

    /// Time passed: the refresh when it is due; nothing once idle.
    pub fn tick(&mut self, now: Instant) -> Vec<OwnStatus> {
        self.evaluate(now)
    }

    /// The idle time changed: taken at once.
    pub fn set_idle_after(&mut self, idle_after: Duration, now: Instant) -> Vec<OwnStatus> {
        self.idle_after = idle_after;
        self.evaluate(now)
    }

    /// The user chose `choice`: the endpoint `PUT` if it now has to be reported and wasn't, then the manual status (or
    /// the reset, when a manual status was chosen before); kept for the next registration when none is live. The
    /// choice already made is sent again only when its own report failed: the user chose it again, a new choice.
    pub fn choose(&mut self, choice: Choice, now: Instant) -> Vec<OwnStatus> {
        if self.ended.is_some() || (choice == self.choice && !self.failed) {
            return Vec::new();
        }
        self.choice = choice;
        self.failed = false;
        // a change to Automatic comes from a manual status (or is Automatic's failed reset chosen again): the reset
        let status = manual_or_reset(choice);
        if !self.live {
            self.pending = Some(status);
            return Vec::new();
        }
        let mut send = self.evaluate(now);
        send.push(status);
        send
    }

    /// How a report went (the engine's reply, or its later [`Event::Transient`](crate::Event::Transient)). Nothing is
    /// ever sent again for it: a refusal before sending ends the reports for the run, a stopped engine ends them too, and a report
    /// the engine had no endpoint for (its push channel stopped for good) ends them until a registration, the manual
    /// status it carried kept for that one if it is still the choice. A manual status (or reset) of the choice that
    /// failed otherwise makes it [`Reporting::NotSet`] until the user chooses again; one that went through clears that.
    pub fn reported(&mut self, status: OwnStatus, outcome: Result<&TransientSent, &EngineError>) {
        let the_choice_s = status != OwnStatus::Available && manual_or_reset(self.choice) == status;
        match outcome {
            Ok(TransientSent::Denied { .. }) => self.end(Ended::Denied),
            Err(EngineError::Stopped) => self.end(Ended::Quit),
            Err(EngineError::NoEndpoint) => {
                self.stopped();
                if the_choice_s && self.pending.is_none() {
                    self.pending = Some(status);
                }
            }
            Ok(TransientSent::Sent) => {
                if the_choice_s {
                    self.failed = false;
                }
            }
            Ok(TransientSent::Dropped { .. }) | Err(_) => {
                if the_choice_s {
                    self.failed = true;
                }
            }
            // its outcome comes later, as another answer
            Ok(TransientSent::Waiting { .. }) => {}
        }
    }

    /// The app quits: nothing is sent (no captured request reports Offline), and nothing more is reported.
    pub fn quit(&mut self) -> Vec<OwnStatus> {
        self.end(Ended::Quit);
        Vec::new()
    }

    fn end(&mut self, why: Ended) {
        // a denial stays a denial when the app quits after it
        self.ended.get_or_insert(why);
        self.reporting = false;
        self.pending = None;
    }

    fn used_within(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_used) < self.idle_after
    }

    /// Whether the endpoint `PUT` is due at `now`: the account should show available and the refresh isn't running or
    /// is due. Idle stops it.
    fn evaluate(&mut self, now: Instant) -> Vec<OwnStatus> {
        let wanted =
            self.ended.is_none() && self.live && (matches!(self.choice, Choice::Manual(_)) || self.used_within(now));
        if !wanted {
            self.reporting = false;
            return Vec::new();
        }
        let due = !self.reporting
            || self
                .sent_at
                .is_none_or(|sent| now.saturating_duration_since(sent) >= AVAILABLE_REFRESH);
        if !due {
            return Vec::new();
        }
        self.reporting = true;
        self.sent_at = Some(now);
        vec![OwnStatus::Available]
    }
}

/// The manual status a saved choice applies at the run's first registration: none for Automatic (no reset at start).
fn manual(choice: Choice) -> Option<OwnStatus> {
    match choice {
        Choice::Automatic => None,
        Choice::Manual(status) => Some(OwnStatus::Manual(status)),
    }
}

/// The write a choice was sent as: its manual status, or the reset for Automatic.
fn manual_or_reset(choice: Choice) -> OwnStatus {
    match choice {
        Choice::Automatic => OwnStatus::ClearManual,
        Choice::Manual(status) => OwnStatus::Manual(status),
    }
}
