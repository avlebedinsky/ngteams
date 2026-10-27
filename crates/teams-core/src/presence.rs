//! Others' presence (M2 Task 17): the availability the views show beside people, pushed by UPS as it changes once the
//! account's endpoint subscribed to it, read from UPS in the background by polling besides, never stored.
//!
//! - **What is read**: UPS's presence read ([`PresenceRead`], `v1/presence/getpresence/`), a read sent as a `POST`
//!   by [`Client::lookup`], whoever it names, once the reads' transport has the presence base (the GTM
//!   `presenceUPS`), [`MAX_NAMED`] people per read.
//! - **What is pushed**: the account's own Trouter endpoint subscribed to the presence of the same people
//!   ([`subscription`]: UPS's `v1/pubsub/subscriptions/<endpoint>`, sent as the read is, filtered as it is), the way
//!   the web client hears of a change; each change then comes as a delivery on the messaging socket
//!   ([`teams_trouter::PresenceDelivery`], never answered) and is applied as a read's would be
//!   ([`Presences::delivered`]); a stored one, replayed on a connect, is older than what the connect's subscription
//!   and read give, and is left alone.
//! - **Who**: the people the views asked about ([`EngineRequests::presence`](crate::EngineRequests::presence)), never
//!   a bot or a service ([`has_presence`]: answered unknown, never watched): a
//!   view asks for someone it shows, and from then on they are watched, at most [`MAX_WATCHED`] at once. Asking again
//!   about someone watched makes them the latest asked; a new one past the cap drops the one asked about longest ago,
//!   whose presence is then told as [`Availability::Unknown`] (when a view was told another), so no view keeps showing
//!   a value no read refreshes: a view asks again about the people it shows each time it shows them anew. Everyone
//!   watched is read in each read ([`MAX_NAMED`] per request).
//! - **When**: every [`PRESENCE_POLL`] (the engine's
//!   [`EngineOptions::presence_poll`](crate::EngineOptions::presence_poll)), from the engine's tick, one read at a time;
//!   while the subscription covers everyone it may name, every [`PRESENCE_POLL_SUBSCRIBED`]
//!   ([`EngineOptions::presence_poll_subscribed`](crate::EngineOptions::presence_poll_subscribed)), the web client's
//!   pace, a fallback for a delivery missed: back to the minute as soon as it doesn't (the socket down, someone UPS
//!   didn't take, a subscription that failed). A message-loss report brings the next read forward.
//!   A view asking about someone not watched yet brings the next read forward to the next tick, but never sooner than
//!   [`PRESENCE_MIN_GAP`] after the last read started, so the reads never follow the views' rebuilds. A read the
//!   transport's one retry left throttled (429, 503) waits its `Retry-After`, counted from its answer: no read goes out before it is over, whoever is asked
//!   about meanwhile.
//! - **What is shown**: an availability as UPS gave it ([`Availability`]); [`Availability::Unknown`] before the first
//!   read, for someone the answer left out or gave no presence for, and for everyone a read asked about when it failed
//!   (the network, a status, an answer of another shape or naming anyone not asked for). A push is newer than any
//!   read that started before it: what such a read gives someone pushed since it started is dropped, and a failed
//!   read leaves a pushed value alone while the subscription still covers that person (its pushes keep it current).
//!   Each change is told as [`Event::Presence`](crate::Event::Presence).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use teams_fence::Mri;
use teams_proto::{Client, Credentials, HttpError, HttpTransport, PresenceRead, RegisteredEndpoint};
use teams_trouter::PresenceDelivery;
use tokio::task::JoinSet;
use tokio::time::Instant;

pub(crate) mod subscription;

use subscription::{Subscribed, Subscription};

/// How often the watched people's presence is read while the subscription doesn't cover them all: every minute.
pub const PRESENCE_POLL: Duration = Duration::from_mins(1);

/// How often it is read while the subscription covers everyone it may name: every four minutes, the web client's
/// interval (`presence-sync-strategy:onInterval`, 218–258 s apart in the captures of 2026-10-19 to 2026-10-21, while
/// its subscriptions were denied; once they went through, it read only whom its subscription didn't cover).
pub const PRESENCE_POLL_SUBSCRIBED: Duration = Duration::from_mins(4);

/// The most people watched at once.
pub const MAX_WATCHED: usize = 20;

/// The most people one read or one subscription request names: 8, the most any of the web client's presence reads
/// named in the lab's captures (its subscriptions named 3 at most, every lab account); more are read, or
/// subscribed, [`MAX_NAMED`] at a time.
pub const MAX_NAMED: usize = 8;

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

/// A finished read: each request's names, and what it gave each (or `Err`: it failed, with how long UPS said to
/// wait); after a throttled request the rest aren't sent, and aren't in it.
#[derive(Debug)]
pub(crate) struct Polled {
    /// The serial of the latest push when the read started ([`Known::serial`]).
    since: u64,
    reads: Vec<(Vec<String>, Read)>,
}

/// What one request of a read gave each person it asked about, or `Err`: it failed, with how long UPS said to wait.
type Read = Result<HashMap<String, Availability>, Option<Duration>>;

/// What finished in the background: a read, or a subscription (`None`: one that panicked).
#[derive(Debug)]
pub(crate) enum PresenceDone {
    Polled(Polled),
    Subscribed(Option<Subscribed>),
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

    /// The poll is now `poll` (a subscription covering everyone, or no longer): the next read moves with it, unless
    /// it was brought forward or a throttled read's wait holds it.
    fn set_poll(&mut self, poll: Duration) {
        if poll == self.poll {
            return;
        }
        if let Some(started) = self.started
            && self.next == started + self.poll
        {
            self.next = started + poll;
            if let Some(until) = self.throttled_until {
                self.next = self.next.max(until);
            }
        }
        self.poll = poll;
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

/// What each watched person was last read or pushed as, and which of those values a push gave (see the module docs:
/// a push is newer than a read that started before it).
#[derive(Debug, Default)]
struct Known {
    availability: HashMap<String, Availability>,
    /// Whose value kept came from a push, by that push's serial; a read's value takes it out.
    pushed: HashMap<String, u64>,
    /// The serial of the latest push applied.
    pushes: u64,
}

impl Known {
    /// `mri`'s availability as last read or pushed ([`Availability::Unknown`] before either).
    fn get(&self, mri: &str) -> Availability {
        self.availability.get(mri).cloned().unwrap_or(Availability::Unknown)
    }

    /// `mri` is no longer watched: what was kept for them, if anything.
    fn forget(&mut self, mri: &str) -> Option<Availability> {
        self.pushed.remove(mri);
        self.availability.remove(mri)
    }

    /// The serial of the latest push: a read started now is older than every push after it.
    fn serial(&self) -> u64 {
        self.pushes
    }

    /// A push gave `mri` as `now`: `Some(now)` when that changed what was kept.
    fn pushed(&mut self, mri: &str, now: Availability) -> Option<Availability> {
        self.pushes += 1;
        self.pushed.insert(mri.to_owned(), self.pushes);
        self.keep(mri, now)
    }

    /// A read started when the latest push was `since` gave `mri` as `read` (`None`: it failed); `live`: the
    /// subscription covers `mri` now. `Some` when that changed what was kept. A push since the read started wins; a
    /// failed read leaves a pushed value alone while the subscription covers them.
    fn read(&mut self, mri: &str, read: Option<Availability>, since: u64, live: bool) -> Option<Availability> {
        let pushed = self.pushed.get(mri).copied();
        if pushed.is_some_and(|serial| serial > since) || (read.is_none() && live && pushed.is_some()) {
            return None;
        }
        self.pushed.remove(mri);
        self.keep(mri, read.unwrap_or(Availability::Unknown))
    }

    /// Keeps `now` for `mri`: `Some(now)` when it differs from what was kept (unknown before the first value, so a
    /// value left unknown changes nothing a view shows).
    fn keep(&mut self, mri: &str, now: Availability) -> Option<Availability> {
        let before = self
            .availability
            .insert(mri.to_owned(), now.clone())
            .unwrap_or(Availability::Unknown);
        (before != now).then_some(now)
    }
}

/// Others' presence: who is watched, what each was last read or pushed as, the read under way and the subscription.
pub(crate) struct Presences<C> {
    client: Arc<Client<HttpTransport<C>>>,
    /// Watched, the one asked about longest ago first.
    watched: VecDeque<String>,
    known: Known,
    running: JoinSet<Polled>,
    /// Who the running read asks about, and the serial of the latest push when it started.
    asking: (Vec<String>, u64),
    schedule: Schedule,
    /// The poll without a subscription covering everyone, and with one.
    polls: (Duration, Duration),
    subscription: Subscription,
}

impl<C: Credentials + 'static> Presences<C> {
    /// `polls`: how often the watched are read without a subscription covering them all, and with one.
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, polls: (Duration, Duration)) -> Self {
        Self {
            client,
            watched: VecDeque::new(),
            known: Known::default(),
            running: JoinSet::new(),
            asking: (Vec::new(), 0),
            schedule: Schedule::new(polls.0, Instant::now()),
            polls,
            subscription: Subscription::default(),
        }
    }

    /// The push channel connected, its push URL `push_url`: the subscription waits for its registration.
    pub(crate) fn connected(&mut self, push_url: String) {
        self.subscription.connected(push_url);
    }

    /// The push channel registered `endpoint`: the subscription is made in full.
    pub(crate) fn registered(&mut self, endpoint: RegisteredEndpoint) {
        self.subscription.registered(endpoint);
    }

    /// The push channel is down or stopped: no subscription is live, and the reads go back to the minute.
    pub(crate) fn down(&mut self) {
        self.subscription.down();
    }

    /// The push channel reported pushes missed: the next read as soon as it may go.
    pub(crate) fn missed(&mut self) {
        self.schedule.bring_forward(Instant::now());
    }

    /// Applies a presence delivery: each watched person's availability as it gives it; who changed, with their
    /// availability now. A stored one (replayed on a connect) is left alone (see the module docs), and so is anyone not
    /// watched.
    pub(crate) fn delivered(&mut self, delivery: &PresenceDelivery) -> Vec<(String, Availability)> {
        if delivery.stored {
            return Vec::new();
        }
        let mut changed = Vec::new();
        for presence in &delivery.presences {
            let mri = presence.mri.as_str();
            if !self.watched.iter().any(|watched| watched == mri) {
                continue;
            }
            let now = Availability::of(presence.availability.as_deref());
            if let Some(now) = self.known.pushed(mri, now) {
                changed.push((mri.to_owned(), now));
            }
        }
        changed
    }

    /// `mri`'s availability as last read; from now on they are watched (see the module docs). Someone newly watched brings the next read forward. The second value is whom the watch
    /// dropped to make room, when a view was told a presence of theirs: now [`Availability::Unknown`], to be told.
    pub(crate) fn presence(&mut self, mri: &str) -> (Availability, Option<String>) {
        if !has_presence(mri) {
            return (Availability::Unknown, None);
        }
        let mut dropped = None;
        if let Some(at) = self.watched.iter().position(|watched| watched == mri) {
            if let Some(moved) = self.watched.remove(at) {
                self.watched.push_back(moved);
            }
        } else {
            self.watched.push_back(mri.to_owned());
            if self.watched.len() > MAX_WATCHED
                && let Some(gone) = self.watched.pop_front()
                && self.known.forget(&gone).is_some_and(|was| was != Availability::Unknown)
            {
                dropped = Some(gone);
            }
            self.schedule.bring_forward(Instant::now());
        }
        (self.known.get(mri), dropped)
    }

    /// From the engine's tick: starts a subscription when one is due ([`subscription`]), and a read when one is due
    /// (none runs, the schedule allows it at the poll the subscription now calls for, and someone watched may be named,
    /// [`Self::readable`]).
    pub(crate) fn poll_due(&mut self) {
        let now = Instant::now();
        // without the presence base nothing can go out
        let readable = if self.client.transport().has_presence() {
            self.readable()
        } else {
            Vec::new()
        };
        self.subscription.start_due(&self.client, &readable, now);
        let poll = if self.subscription.covers(&readable) {
            self.polls.1
        } else {
            self.polls.0
        };
        self.schedule.set_poll(poll);
        if !self.running.is_empty() || !self.schedule.due(now) {
            return;
        }
        let asked = readable;
        if asked.is_empty() {
            self.schedule.skipped(now);
            return;
        }
        self.schedule.started(now);
        let since = self.known.serial();
        self.asking = (asked.clone(), since);
        self.running.spawn(read(Arc::clone(&self.client), asked, since));
    }

    /// The watched a read names, in the watch's order: everyone watched (a bot or a service is never watched,
    /// [`has_presence`]).
    fn readable(&self) -> Vec<String> {
        self.watched.iter().cloned().collect()
    }

    /// Whether a read or a subscription runs.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty() || self.subscription.busy()
    }

    /// The next read's or subscription's result once it is over; never while none runs ([`Self::busy`]). A read that
    /// panicked or was cancelled failed.
    pub(crate) async fn next(&mut self) -> Option<PresenceDone> {
        tokio::select! {
            joined = self.running.join_next(), if !self.running.is_empty() => {
                let joined = joined?;
                let (asked, since) = std::mem::take(&mut self.asking);
                Some(PresenceDone::Polled(joined.unwrap_or(Polled { since, reads: vec![(asked, Err(None))] })))
            }
            subscribed = self.subscription.next(), if self.subscription.busy() => {
                Some(PresenceDone::Subscribed(subscribed))
            }
            else => None,
        }
    }

    /// Keeps what a read or a subscription gave (see the module docs); who changed, with their availability now.
    pub(crate) fn settle(&mut self, done: PresenceDone) -> Vec<(String, Availability)> {
        match done {
            PresenceDone::Polled(Polled { since, reads }) => reads
                .into_iter()
                .flat_map(|(asked, read)| self.settle_read(asked, read, since))
                .collect(),
            PresenceDone::Subscribed(subscribed) => {
                if let Some(subscribed) = subscribed {
                    self.subscription.settle(subscribed);
                }
                Vec::new()
            }
        }
    }

    /// Keeps what one read of `asked`, started when the latest push was `since`, gave; who changed, with their
    /// availability now.
    fn settle_read(&mut self, asked: Vec<String>, read: Read, since: u64) -> Vec<(String, Availability)> {
        let read = match read {
            Ok(read) => Some(read),
            Err(wait) => {
                if let Some(wait) = wait {
                    self.schedule.throttled(Instant::now(), wait);
                }
                None
            }
        };
        let mut changed = Vec::new();
        for mri in asked {
            // someone dropped from the watch while the read ran isn't kept
            if !self.watched.contains(&mri) {
                continue;
            }
            let given = read
                .as_ref()
                .map(|read| read.get(&mri).cloned().unwrap_or(Availability::Unknown));
            let live = self.subscription.covers(std::slice::from_ref(&mri));
            if let Some(now) = self.known.read(&mri, given, since, live) {
                changed.push((mri, now));
            }
        }
        changed
    }
}

/// Whether `mri` is someone UPS gives a presence for, so a read may name them: not a bot's or a service's `28:` id
/// (the echo service of the test call, an app), nor a `48:` stream's (the account's notes, its call log): UPS has no
/// presence for one and Teams shows none, so naming one would only make the read and the subscription longer.
#[must_use]
pub fn has_presence(mri: &str) -> bool {
    !mri.starts_with("28:") && !mri.starts_with("48:")
}

/// A read of `asked`'s presence: [`MAX_NAMED`] per request, one after the other; after a throttled one, the rest
/// aren't sent.
async fn read<C: Credentials>(client: Arc<Client<HttpTransport<C>>>, asked: Vec<String>, since: u64) -> Polled {
    let mut reads = Vec::new();
    for batch in asked.chunks(MAX_NAMED) {
        let read = read_one(&client, batch).await;
        let throttled = matches!(read, Err(Some(_)));
        reads.push((batch.to_vec(), read));
        if throttled {
            break;
        }
    }
    Polled { since, reads }
}

/// One request of a read, of `asked`'s presence: what it gave each, or how long UPS said to wait when it failed.
async fn read_one<C: Credentials>(client: &Client<HttpTransport<C>>, asked: &[String]) -> Read {
    let Some(request) = PresenceRead::new(asked.iter().map(|mri| Mri::new(mri.as_str()))) else {
        return Err(None);
    };
    let answer = match client.lookup(request.clone()).await {
        Ok(answer) => answer,
        Err(HttpError::Status(answer)) => {
            log::debug!("the presence read was answered HTTP {}", answer.status());
            let wait = matches!(answer.status(), 429 | 503)
                .then(|| answer.retry_after())
                .flatten();
            return Err(wait);
        }
        Err(error) => {
            // the error names no token and no body
            log::debug!("the presence read failed: {error}");
            return Err(None);
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
            Ok(read)
        }
        Err(error) => {
            log::debug!("the presence answer wasn't read: {error}");
            Err(None)
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
    fn a_new_poll_moves_the_next_read_but_not_one_brought_forward_or_held_by_a_throttle() {
        let start = Instant::now();
        let subscribed = Duration::from_mins(4);
        // a subscription covering everyone: the next read four minutes after the last, not one
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        schedule.set_poll(subscribed);
        assert!(!schedule.due(at(start, 60)));
        assert!(schedule.due(at(start, 240)));
        // and back: the minute again, from the last read
        schedule.set_poll(POLL);
        assert!(schedule.due(at(start, 60)));
        // a read brought forward stays forward
        let mut schedule = Schedule::new(POLL, start);
        schedule.started(start);
        schedule.bring_forward(at(start, 20));
        schedule.set_poll(subscribed);
        assert!(schedule.due(at(start, 20)));
        // a throttled read's wait still holds
        let mut schedule = Schedule::new(subscribed, start);
        schedule.started(start);
        schedule.throttled(at(start, 1), Duration::from_secs(300));
        schedule.set_poll(POLL);
        assert!(!schedule.due(at(start, 60)));
        assert!(schedule.due(at(start, 301)));
    }

    const A: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";

    #[test]
    fn a_push_while_a_read_runs_wins_over_its_answer_and_its_failure() {
        for answer in [Some(Availability::Offline), Some(Availability::Unknown), None] {
            for live in [true, false] {
                let mut known = Known::default();
                assert_eq!(
                    known.read(A, Some(Availability::Away), known.serial(), live),
                    Some(Availability::Away)
                );
                // a read starts, a push comes before its answer
                let since = known.serial();
                assert_eq!(known.pushed(A, Availability::Busy), Some(Availability::Busy));
                // the read's older answer, or its failure, changes nothing
                assert_eq!(
                    known.read(A, answer.clone(), since, live),
                    None,
                    "{answer:?} live {live}"
                );
                assert_eq!(known.get(A), Availability::Busy);
                // the next read, started after the push, is taken
                assert_eq!(
                    known.read(A, Some(Availability::Available), known.serial(), live),
                    Some(Availability::Available)
                );
            }
        }
    }

    #[test]
    fn a_read_started_after_a_push_replaces_it_and_a_failed_one_keeps_it_only_while_subscribed() {
        // a push before the read started: a successful read is newer
        let mut known = Known::default();
        known.pushed(A, Availability::Busy);
        assert_eq!(
            known.read(A, Some(Availability::Away), known.serial(), true),
            Some(Availability::Away)
        );
        // the value is now the read's: a failed read makes it unknown even while subscribed
        assert_eq!(known.read(A, None, known.serial(), true), Some(Availability::Unknown));
        // a push before a failed read: kept while the subscription covers them, its pushes keep it current
        let mut known = Known::default();
        known.pushed(A, Availability::Busy);
        assert_eq!(known.read(A, None, known.serial(), true), None);
        assert_eq!(known.get(A), Availability::Busy);
        // and unknown once it doesn't: nothing refreshes it any more
        assert_eq!(known.read(A, None, known.serial(), false), Some(Availability::Unknown));
    }

    #[test]
    fn a_push_after_a_read_changes_what_it_gave_and_someone_dropped_starts_unknown() {
        let mut known = Known::default();
        assert_eq!(
            known.read(A, Some(Availability::Away), known.serial(), true),
            Some(Availability::Away)
        );
        assert_eq!(known.pushed(A, Availability::Away), None);
        assert_eq!(
            known.pushed(A, Availability::DoNotDisturb),
            Some(Availability::DoNotDisturb)
        );
        assert_eq!(known.forget(A), Some(Availability::DoNotDisturb));
        assert_eq!(known.get(A), Availability::Unknown);
        // watched again: a read started before the push they were dropped with is no longer held back by it
        assert_eq!(
            known.read(A, Some(Availability::Offline), 0, true),
            Some(Availability::Offline)
        );
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
