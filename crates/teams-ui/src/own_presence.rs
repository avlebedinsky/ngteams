//! The account's own presence as the app reports it (M2 Task 20): `teams-core`'s policy
//! ([`OwnPresencePolicy`]) driven by the app, its writes handed to the engine
//! ([`Core::own_availability`](crate::Core::own_availability)), never sent by the UI itself.
//!
//! - **Its inputs**: the push channel registered or stopped (the [`Feed`](crate::Feed)'s
//!   [`FeedEvent::Registered`] and [`FeedEvent::PushStopped`]); the window used (the shell's mouse and key input,
//!   [`OwnPresence::active`]); a tick every [`POLICY_TICK`] (the refresh, and idle noticed); the settings' manual status
//!   (the account menu and the settings view change it) and idle time; the account menu's choice itself
//!   ([`OwnPresence::choose`]: the status chosen again, which leaves the setting as it was); the quit
//!   ([`OwnPresence::quit`]).
//! - **What it reports**: Available (the endpoint `PUT`) while the window was used within the idle time or a manual
//!   status is chosen, sent again every [`AVAILABLE_REFRESH`](teams_core::AVAILABLE_REFRESH); nothing once idle (the
//!   endpoint lapses on its own: no captured request reports Away for one endpoint); nothing on quit (no captured
//!   request reports Offline); the manual status when chosen, its reset when Automatic is chosen again. The policy's
//!   module docs say why.
//! - **Only while it may be sent** ([`SettingsModel::sends_presence`]: its switch on, and no first run's screen
//!   waiting in the personal build): until then a registration isn't told to the policy, so nothing is reported; once
//!   it may be, a registration already made is told then, and when it may no longer be, the policy is told the push
//!   channel stopped.
//! - **A failed report is shown** ([`OwnPresence::failure`], in the account menu) and never sent again on its own: the
//!   engine's transient rules are the only retries (one more attempt after a 401, one short `Retry-After` wait), and a
//!   later [`FeedEvent::OwnReport`] tells how a waiting one went. A manual status (or reset) that failed is shown as not
//!   set ([`Reporting::NotSet`]) and goes out again only when the user chooses it again in the account menu. A
//!   refusal before sending stops the reports for the run.
//!
//! Traced as `own-presence` `sent`, `failed` or `denied`, with the report's kind (`available`, `busy`,
//! `do-not-disturb`, `away`, `manual-available`, `reset`).

use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AsyncApp, Context, Entity, Subscription, Task, WeakEntity};
use teams_core::{Choice, EngineError, OwnPresencePolicy, OwnStatus, Reporting, TransientSent};
use teams_proto::ManualAvailability;

use crate::bridge::Core;
use crate::models::{FeedEvent, Models};
use crate::settings::{Settings, SettingsModel};
use crate::trace::{TraceLine, ViewTrace};

/// How often the policy is looked at between inputs: the refresh comes due, and idle is noticed, within this.
pub const POLICY_TICK: Duration = Duration::from_secs(5);

/// A report that failed, as the account menu shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// What it reported.
    pub status: OwnStatus,
    /// Why it failed, in a few words.
    pub why: String,
}

/// What a report sets, as the trace and the account menu name it.
#[must_use]
pub fn kind(status: OwnStatus) -> &'static str {
    match status {
        OwnStatus::Available => "available",
        OwnStatus::Manual(ManualAvailability::Busy) => "busy",
        OwnStatus::Manual(ManualAvailability::DoNotDisturb) => "do-not-disturb",
        OwnStatus::Manual(ManualAvailability::Away) => "away",
        OwnStatus::Manual(ManualAvailability::Available) => "manual-available",
        OwnStatus::ClearManual => "reset",
    }
}

/// Whether two reports are of the same kind: the endpoint's, or the manual status's (a set or the reset). A report
/// that went through clears a failure of its kind.
fn same_kind(one: OwnStatus, other: OwnStatus) -> bool {
    (one == OwnStatus::Available) == (other == OwnStatus::Available)
}

/// The account's own presence as the app reports it (see the module docs). A GPUI entity, held by the
/// [`Desktop`](crate::Desktop) for the app's life (the window may close while the app stays in the tray).
pub struct OwnPresence {
    core: Arc<dyn Core>,
    policy: OwnPresencePolicy,
    /// The settings' idle time, as the policy last took it.
    idle_minutes: u32,
    failure: Option<Failure>,
    /// What was reported when the observers were last told: going idle changes what is reported with no input, so the
    /// tick compares against this, not against a reading taken after the idle time already passed.
    shown: Reporting,
    trace: Arc<ViewTrace>,
    /// A registration of the push channel went through and it hasn't stopped since.
    registered: bool,
    /// The account's availability may be reported now ([`SettingsModel::sends_presence`]).
    allowed: bool,
    _subscriptions: [Subscription; 2],
    _ticking: Task<()>,
}

impl OwnPresence {
    /// The policy over `core`, from the settings' manual status and idle time, the app's start counting as use.
    pub fn new(
        core: Arc<dyn Core>,
        models: &Models,
        settings: &Entity<SettingsModel>,
        trace: Arc<ViewTrace>,
        cx: &mut Context<Self>,
    ) -> Self {
        let now = cx.background_executor().now();
        let saved = settings.read(cx).settings().clone();
        let allowed = settings.read(cx).sends_presence();
        let account = core.me().to_owned();
        let policy = OwnPresencePolicy::new(saved.manual_status_of(&account).choice(), idle(saved.idle_minutes), now);
        let feeding = cx.subscribe(&models.feed, |this: &mut Self, _, event, cx| this.fed(event, cx));
        let settled = cx.observe(settings, |this: &mut Self, settings, cx| {
            let allowed = settings.read(cx).sends_presence();
            let settings = settings.read(cx).settings().clone();
            this.allowed_changed(allowed, cx);
            this.settings_changed(&settings, cx);
        });
        let ticking = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(POLICY_TICK).await;
                if this.update(cx, Self::tick).is_err() {
                    break;
                }
            }
        });
        let shown = policy.reporting(now);
        Self {
            core,
            policy,
            idle_minutes: saved.idle_minutes,
            failure: None,
            shown,
            trace,
            registered: false,
            allowed,
            _subscriptions: [feeding, settled],
            _ticking: ticking,
        }
    }

    /// What is reported now.
    #[must_use]
    pub fn reporting(&self, cx: &App) -> Reporting {
        self.policy.reporting(cx.background_executor().now())
    }

    /// The last report that failed, until one of its kind goes through.
    #[must_use]
    pub fn failure(&self) -> Option<&Failure> {
        self.failure.as_ref()
    }

    /// The window was used: Available at once if it had stopped for idle.
    pub fn active(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let send = self.policy.active(now);
        self.send(send, cx);
        self.publish(cx);
    }

    /// The user chose `choice` in the account menu: sent as the policy says, the choice already made sent again only
    /// when its report failed (the user chose it again: a new write of the user's, through the engine's outbox).
    /// The menu keeps it in the settings too; that change, if any, finds the policy already holding it.
    pub fn choose(&mut self, choice: Choice, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let send = self.policy.choose(choice, now);
        self.send(send, cx);
        self.changed(cx);
    }

    /// The app quits: nothing is sent, and nothing more is reported.
    pub fn quit(&mut self, cx: &mut Context<Self>) {
        let send = self.policy.quit();
        self.send(send, cx);
        self.changed(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let send = self.policy.tick(now);
        self.send(send, cx);
        self.publish(cx);
    }

    /// Tells the observers when what is reported differs from what they were last told.
    fn publish(&mut self, cx: &mut Context<Self>) {
        if self.reporting(cx) != self.shown {
            self.changed(cx);
        }
    }

    /// Tells the observers, and keeps what is reported as what they were told.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.shown = self.reporting(cx);
        cx.notify();
    }

    fn fed(&mut self, event: &FeedEvent, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        match event {
            FeedEvent::Registered => {
                self.registered = true;
                if self.allowed {
                    let send = self.policy.registered(now);
                    self.send(send, cx);
                }
            }
            FeedEvent::PushStopped => {
                self.registered = false;
                self.policy.stopped();
            }
            FeedEvent::OwnReport { status, sent } => self.answered(*status, Ok(sent), cx),
            FeedEvent::Arrived { .. } | FeedEvent::Read { .. } | FeedEvent::Changed { .. } | FeedEvent::Down { .. } => {
                return;
            }
        }
        self.changed(cx);
    }

    /// Whether the account's availability may be reported changed (its switch, or the first run's screen confirmed): a
    /// registration already made is told to the policy once it may, and the policy is told the push channel stopped
    /// once it may not.
    fn allowed_changed(&mut self, allowed: bool, cx: &mut Context<Self>) {
        if allowed == self.allowed {
            return;
        }
        self.allowed = allowed;
        if !allowed {
            self.policy.stopped();
        } else if self.registered {
            let now = cx.background_executor().now();
            let send = self.policy.registered(now);
            self.send(send, cx);
        }
        self.changed(cx);
    }

    /// The settings changed: a manual status that differs from the policy's choice is chosen (the same one isn't: a
    /// change of another setting never sends a failed one again), and the idle time taken.
    fn settings_changed(&mut self, settings: &Settings, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let choice = settings.manual_status_of(self.core.me()).choice();
        let mut send = if choice == self.policy.choice() {
            Vec::new()
        } else {
            self.policy.choose(choice, now)
        };
        if settings.idle_minutes != self.idle_minutes {
            self.idle_minutes = settings.idle_minutes;
            send.extend(self.policy.set_idle_after(idle(settings.idle_minutes), now));
        }
        self.send(send, cx);
        self.changed(cx);
    }

    /// Hands each report to the engine, in order (the engine takes its requests in the order they are made), and takes
    /// each answer as it comes.
    fn send(&mut self, statuses: Vec<OwnStatus>, cx: &mut Context<Self>) {
        for status in statuses {
            let reply = self.core.own_availability(status);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let answer = reply.await;
                // gone with the app: nobody is left to show it to
                let _ = this.update(cx, |this, cx| this.answered(status, answer.as_ref(), cx));
            })
            .detach();
        }
    }

    /// How a report went: told to the policy (which never sends it again), shown when it failed, traced.
    fn answered(&mut self, status: OwnStatus, answer: Result<&TransientSent, &EngineError>, cx: &mut Context<Self>) {
        self.policy.reported(status, answer);
        let (change, why) = match answer {
            Ok(TransientSent::Sent) => ("sent", None),
            // it goes out from the engine's tick; its outcome comes as `FeedEvent::OwnReport`
            Ok(TransientSent::Waiting { .. }) | Err(EngineError::NoEndpoint | EngineError::Stopped) => return,
            Ok(TransientSent::Denied { reason }) => ("denied", Some(format!("refused before sending: {reason}"))),
            Ok(TransientSent::Dropped { reason }) => ("failed", Some(reason.clone())),
            Err(error) => ("failed", Some(error.to_string())),
        };
        self.trace
            .record(&TraceLine::new("own-presence", change).kind(kind(status)));
        match why {
            Some(why) => {
                log::warn!(
                    "the account's own availability ({}) wasn't reported: {why}",
                    kind(status)
                );
                self.failure = Some(Failure { status, why });
            }
            None => {
                if self
                    .failure
                    .as_ref()
                    .is_some_and(|failed| same_kind(failed.status, status))
                {
                    self.failure = None;
                }
            }
        }
        self.changed(cx);
    }
}

/// The idle time of `minutes`.
fn idle(minutes: u32) -> Duration {
    Duration::from_secs(u64::from(minutes) * 60)
}

/// What the account menu says is reported.
#[must_use]
pub fn reporting_label(reporting: Reporting) -> &'static str {
    match reporting {
        Reporting::Connecting => "Not reported yet",
        Reporting::Available => "Available",
        Reporting::Manual(ManualAvailability::Busy) => "Busy",
        Reporting::Manual(ManualAvailability::DoNotDisturb) => "Do not disturb",
        Reporting::Manual(ManualAvailability::Away) => "Appear away",
        Reporting::Manual(ManualAvailability::Available) => "Available (set)",
        Reporting::NotSet(Choice::Manual(ManualAvailability::Busy)) => "Busy not set: choose it again",
        Reporting::NotSet(Choice::Manual(ManualAvailability::DoNotDisturb)) => {
            "Do not disturb not set: choose it again"
        }
        Reporting::NotSet(Choice::Manual(ManualAvailability::Away)) => "Appear away not set: choose it again",
        Reporting::NotSet(Choice::Manual(ManualAvailability::Available)) => "Available not set: choose it again",
        Reporting::NotSet(Choice::Automatic) => "Status not reset: choose Automatic again",
        Reporting::Inactive => "Inactive: not reported",
        Reporting::Halted => "Not reported: denied",
        Reporting::Quit => "Not reported",
    }
}
