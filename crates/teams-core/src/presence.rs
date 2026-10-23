//! Others' presence (M2 Task 17): the availability the views show beside people, read from UPS in the background by
//! polling, never stored.
//!
//! - **What is read**: UPS's presence read ([`PresenceRead`], `v1/presence/getpresence/`), a read sent as a `POST`,
//!   which the transport sends whoever it names, once the reads' transport has the presence base (the GTM
//!   `presenceUPS`). No pubsub subscription: subscribing changes UPS's state, so it is no read (the web client's other way to hear of a
//!   change); the views' presence is as fresh as the last poll.
//! - **Who**: the people the views asked about ([`EngineRequests::presence`](crate::EngineRequests::presence)): a
//!   view asks for someone it shows, and from then on they are watched, at most [`MAX_WATCHED`] at once. Asking again
//!   about someone watched makes them the latest asked; a new one past the cap drops the one asked about longest ago,
//!   whose presence is then told as [`Availability::Unknown`] (when a view was told another), so no view keeps showing
//!   a value no read refreshes: a view asks again about the people it shows each time it shows them anew. Everyone
//!   watched is read in one read.
//! - **When**: every [`PRESENCE_POLL`] (the engine's
//!   [`EngineOptions::presence_poll`](crate::EngineOptions::presence_poll)), from the engine's tick, one read at a time.
//!   A view asking about someone not watched yet brings the next read forward to the next tick, but never sooner than
//!   [`PRESENCE_MIN_GAP`] after the last read started, so the reads never follow the views' rebuilds. A read the
//!   transport's one retry left throttled (429, 503) waits its `Retry-After`, counted from its answer: no read goes out before it is over, whoever is asked
//!   about meanwhile.
//! - **What is shown**: an availability as UPS gave it ([`Availability`]); [`Availability::Unknown`] before the first
//!   read, for someone the answer left out or gave no presence for, and for everyone a read asked about when it failed
//!   (the network, a status, an answer of another shape or naming anyone not asked for). Each change is told as
//!   [`Event::Presence`](crate::Event::Presence).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use teams_fence::Mri;
use teams_proto::{Client, Credentials, HttpError, HttpTransport, PresenceRead};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// How often the watched people's presence is read: every minute. The web client polls about every four minutes
/// beside its subscription (the captures of 2026-10-19 to 2026-10-21: `presence-sync-strategy:onInterval` 218–258 s
/// apart); the native client has no subscription, so it polls more often, for at most [`MAX_WATCHED`] people in one
/// read.
pub const PRESENCE_POLL: Duration = Duration::from_mins(1);

/// The most people watched at once (and so named by one read).
pub const MAX_WATCHED: usize = 20;

/// The soonest a read brought forward by someone newly watched follows the last read's start: however often the
/// views ask about someone new, at most one read in this time beyond the poll.
pub const PRESENCE_MIN_GAP: Duration = Duration::from_secs(10);

/// Someone's availability as UPS gave it. Only `Offline` was captured so far (ab-chat of 2026-10-22, every lab
/// account then); the other words are UPS's documented availability values. A word no capture or document showed is
/// kept as it came ([`Availability::Other`]), never read as one of these.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Availability {
    Available,
    AvailableIdle,
    Away,
    BeRightBack,
    Busy,
    BusyIdle,
    DoNotDisturb,
    Offline,
    /// A word this build doesn't name, as UPS gave it.
    Other(String),
    /// Not known: not read yet, not readable in this build, left out of the answer (or `PresenceUnknown`), or the read
    /// failed.
    Unknown,
}

impl Availability {
    /// The availability UPS's word names (`None`: it gave none).
    #[must_use]
    pub fn of(word: Option<&str>) -> Self {
        let Some(word) = word else {
            return Self::Unknown;
        };
        match word {
            "Available" => Self::Available,
            "AvailableIdle" => Self::AvailableIdle,
            "Away" => Self::Away,
            "BeRightBack" => Self::BeRightBack,
            "Busy" => Self::Busy,
            "BusyIdle" => Self::BusyIdle,
            "DoNotDisturb" => Self::DoNotDisturb,
            "Offline" => Self::Offline,
            "PresenceUnknown" => Self::Unknown,
            other => Self::Other(other.to_owned()),
        }
    }
}

/// A finished read: whom it asked about, and what it gave each (or `Err`: it failed, with how long UPS said to wait).
#[derive(Debug)]
pub(crate) struct Polled {
    asked: Vec<String>,
    read: Result<HashMap<String, Availability>, Option<Duration>>,
}

/// When the next read may go out (see the module docs): the poll, a throttled read's wait and the reads brought
/// forward, against the engine's clock (`now` given by the caller).
#[derive(Debug)]
struct Schedule {
    poll: Duration,
    /// The next read, at the soonest.
    next: Instant,
    /// When the last read started (`None`: none yet).
    started: Option<Instant>,
    /// No read before this: a throttled read's `Retry-After`, counted from its answer.
    throttled_until: Option<Instant>,
}

impl Schedule {
    fn new(poll: Duration, now: Instant) -> Self {
        Self {
            poll,
            next: now,
            started: None,
            throttled_until: None,
        }
    }

    /// Whether a read may start at `now`.
    fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// A read started at `now`: the next one a poll later.
    fn started(&mut self, now: Instant) {
        self.started = Some(now);
        self.next = now + self.poll;
    }

    /// No read could start at `now` (nobody watched, or nothing to read with): the next try a poll later.
    fn skipped(&mut self, now: Instant) {
        self.next = now + self.poll;
    }

    /// Someone was newly watched at `now`: the next read as soon as it may go, never before a throttled read's wait is
    /// over nor sooner than [`PRESENCE_MIN_GAP`] after the last read started.
    fn bring_forward(&mut self, now: Instant) {
        let mut soonest = now;
        if let Some(started) = self.started {
            soonest = soonest.max(started + PRESENCE_MIN_GAP);
        }
        if let Some(until) = self.throttled_until {
            soonest = soonest.max(until);
        }
        self.next = self.next.min(soonest);
    }

    /// A read was answered at `now` with a throttle (429, 503) asking to wait `wait`: no read before it is over.
    fn throttled(&mut self, now: Instant, wait: Duration) {
        let until = now + wait;
        self.throttled_until = Some(self.throttled_until.map_or(until, |earlier| earlier.max(until)));
        self.next = self.next.max(until);
    }
}

/// The presence polling: who is watched, what each was last read as, and the read under way.
pub(crate) struct Presences<C> {
    client: Arc<Client<HttpTransport<C>>>,
    /// Watched, the one asked about longest ago first.
    watched: VecDeque<String>,
    known: HashMap<String, Availability>,
    running: JoinSet<Polled>,
    /// Who the running read asks about.
    asking: Vec<String>,
    schedule: Schedule,
}

impl<C: Credentials + 'static> Presences<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, poll: Duration) -> Self {
        Self {
            client,
            watched: VecDeque::new(),
            known: HashMap::new(),
            running: JoinSet::new(),
            asking: Vec::new(),
            schedule: Schedule::new(poll, Instant::now()),
        }
    }

    /// `mri`'s availability as last read; from now on they are watched (see the module docs). Someone newly watched brings the next read forward. The second value is whom the watch
    /// dropped to make room, when a view was told a presence of theirs: now [`Availability::Unknown`], to be told.
    pub(crate) fn presence(&mut self, mri: &str) -> (Availability, Option<String>) {
        let mut dropped = None;
        if let Some(at) = self.watched.iter().position(|watched| watched == mri) {
            if let Some(moved) = self.watched.remove(at) {
                self.watched.push_back(moved);
            }
        } else {
            self.watched.push_back(mri.to_owned());
            if self.watched.len() > MAX_WATCHED
                && let Some(gone) = self.watched.pop_front()
                && self.known.remove(&gone).is_some_and(|was| was != Availability::Unknown)
            {
                dropped = Some(gone);
            }
            self.schedule.bring_forward(Instant::now());
        }
        let availability = self.known.get(mri).cloned().unwrap_or(Availability::Unknown);
        (availability, dropped)
    }

    /// Starts a read when one is due (from the engine's tick): none runs, the schedule allows it, and someone is
    /// watched.
    pub(crate) fn poll_due(&mut self) {
        let now = Instant::now();
        if !self.running.is_empty() || !self.schedule.due(now) {
            return;
        }
        // without the presence base the read can't go out
        if !self.client.transport().has_presence() || self.watched.is_empty() {
            self.schedule.skipped(now);
            return;
        }
        self.schedule.started(now);
        let asked: Vec<String> = self.watched.iter().cloned().collect();
        self.asking.clone_from(&asked);
        self.running.spawn(read(Arc::clone(&self.client), asked));
    }

    /// Whether a read runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The read's result once it is over; never while none runs ([`Self::busy`]). A read that panicked or was
    /// cancelled failed.
    pub(crate) async fn next(&mut self) -> Option<Polled> {
        let joined = self.running.join_next().await?;
        let asked = std::mem::take(&mut self.asking);
        Some(joined.unwrap_or(Polled { asked, read: Err(None) }))
    }

    /// Keeps what a read gave (see the module docs); who changed, with their availability now.
    pub(crate) fn settle(&mut self, polled: Polled) -> Vec<(String, Availability)> {
        let read = match polled.read {
            Ok(read) => Some(read),
            Err(wait) => {
                if let Some(wait) = wait {
                    self.schedule.throttled(Instant::now(), wait);
                }
                None
            }
        };
        let mut changed = Vec::new();
        for mri in polled.asked {
            // someone dropped from the watch while the read ran isn't kept
            if !self.watched.contains(&mri) {
                continue;
            }
            let now = read
                .as_ref()
                .and_then(|read| read.get(&mri).cloned())
                .unwrap_or(Availability::Unknown);
            // unknown before the first read: a read that leaves someone unknown changes nothing a view shows
            let before = self
                .known
                .insert(mri.clone(), now.clone())
                .unwrap_or(Availability::Unknown);
            if before != now {
                changed.push((mri, now));
            }
        }
        changed
    }
}

/// One read of `asked`'s presence.
async fn read<C: Credentials>(client: Arc<Client<HttpTransport<C>>>, asked: Vec<String>) -> Polled {
    let failed = |asked, wait| Polled { asked, read: Err(wait) };
    let Some(request) = PresenceRead::new(asked.iter().map(|mri| Mri::new(mri.as_str()))) else {
        return failed(asked, None);
    };
    let answer = match client.send(&request).await {
        Ok(answer) => answer,
        Err(HttpError::Status(answer)) => {
            log::debug!("the presence read was answered HTTP {}", answer.status());
            let wait = matches!(answer.status(), 429 | 503)
                .then(|| answer.retry_after())
                .flatten();
            return failed(asked, wait);
        }
        Err(error) => {
            // the error names no token and no body
            log::debug!("the presence read failed: {error}");
            return failed(asked, None);
        }
    };
    match request.read_answer(answer.body()) {
        Ok(presences) => {
            let read = presences
                .into_iter()
                .map(|presence| {
                    let availability = Availability::of(presence.availability.as_deref());
                    (presence.mri.as_str().to_owned(), availability)
                })
                .collect();
            Polled { asked, read: Ok(read) }
        }
        Err(error) => {
            log::debug!("the presence answer wasn't read: {error}");
            failed(asked, None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLL: Duration = Duration::from_mins(1);

    fn at(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn the_first_read_goes_at_once_and_then_every_poll() {
        let start = Instant::now();
        let mut schedule = Schedule::new(POLL, start);
        assert!(schedule.due(start));
        schedule.started(start);
        assert!(!schedule.due(at(start, 59)));
        assert!(schedule.due(at(start, 60)));
    }

    #[test]
    fn someone_newly_watched_brings_the_read_forward_but_not_within_the_gap_of_the_last() {
        let start = Instant::now();
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        // asked about 2 s after the read started: the next read waits for the gap, not the poll
        schedule.bring_forward(at(start, 2));
        assert!(!schedule.due(at(start, 9)));
        assert!(schedule.due(at(start, 10)));
        // asked about long after the gap: at once
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        schedule.bring_forward(at(start, 30));
        assert!(schedule.due(at(start, 30)));
        // a poll shorter than the gap isn't pushed back by it
        let mut schedule = Schedule::new(Duration::from_secs(3), start);
        schedule.started(start);
        schedule.bring_forward(at(start, 1));
        assert!(schedule.due(at(start, 3)));
    }

    #[test]
    fn a_throttled_read_waits_its_retry_after_whoever_is_asked_about_meanwhile() {
        let start = Instant::now();
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        // answered 429 at 1 s, Retry-After 300 s
        schedule.throttled(at(start, 1), Duration::from_secs(300));
        schedule.bring_forward(at(start, 20));
        schedule.bring_forward(at(start, 200));
        assert!(!schedule.due(at(start, 300)));
        assert!(schedule.due(at(start, 301)));
    }

    #[test]
    fn a_retry_after_shorter_than_the_poll_counts_from_the_answer() {
        let start = Instant::now();
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        // answered 429 at 20 s, Retry-After 50 s: not at the poll (60 s), at 70 s
        schedule.throttled(at(start, 20), Duration::from_secs(50));
        assert!(!schedule.due(at(start, 60)));
        assert!(!schedule.due(at(start, 69)));
        assert!(schedule.due(at(start, 70)));
    }

    #[test]
    fn a_retry_after_over_before_the_poll_leaves_the_poll_and_the_gap() {
        let start = Instant::now();
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        schedule.throttled(at(start, 1), Duration::from_secs(5));
        assert!(!schedule.due(at(start, 59)));
        assert!(schedule.due(at(start, 60)));
        // someone asked about once the wait is over: the gap still holds
        schedule.bring_forward(at(start, 7));
        assert!(!schedule.due(at(start, 9)));
        assert!(schedule.due(at(start, 10)));
    }
}
