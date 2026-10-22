//! Others' presence (M2 Task 17): the availability the views show beside people, read from UPS in the background by
//! polling, never stored.
//!
//! - **What is read**: UPS's presence read ([`PresenceRead`], `v1/presence/getpresence/`), an entry of the sealed
//!   `PostRead` list, sent only with the permit `Fence::check_post_read` gives for its exact MRIs (in a lab build:
//!   every one a lab account) and only once the reads' transport has the presence base (the GTM `presenceUPS`). No
//!   pubsub subscription: subscribing changes UPS's state, so it is no read (the web client's other way to hear of a
//!   change); the views' presence is as fresh as the last poll.
//! - **Who**: the people the views asked about ([`EngineRequests::presence`](crate::EngineRequests::presence)): a view
//!   asks for someone it shows, and from then on they are watched for the run; at most [`MAX_WATCHED`], the one asked
//!   about longest ago dropped first. Of them, only those the fence would let a read name are read (a lab build: the
//!   lab accounts, [`teams_fence::Fence::lab_accounts_only`]), in one read; anyone else stays
//!   [`Availability::Unknown`], and the fence is never asked about them.
//! - **When**: every [`PRESENCE_POLL`] (the engine's
//!   [`EngineOptions::presence_poll`](crate::EngineOptions::presence_poll)), from the engine's tick, one read at a time;
//!   at once (on the next tick) when a view asks about someone not watched yet. A throttled read (429, 503) waits its
//!   `Retry-After` when that is longer than the poll.
//! - **What is shown**: an availability as UPS gave it ([`Availability`]); [`Availability::Unknown`] before the first
//!   read, for someone the answer left out or gave no presence for, and for everyone a read asked about when it failed
//!   (the network, a status, an answer of another shape or naming anyone not asked for). Each change is told as
//!   [`Event::Presence`](crate::Event::Presence).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use teams_fence::{Fence, Mri};
use teams_proto::{Client, Credentials, HttpError, HttpTransport, PostRead as _, PostReadError, PresenceRead};
use tokio::task::JoinSet;
use tokio::time::Instant;

/// How often the watched people's presence is read: every minute. The web client polls about every four minutes
/// beside its subscription (the captures of 2026-10-19 to 2026-10-21: `presence-sync-strategy:onInterval` 218–258 s
/// apart); the native client has no subscription, so it polls more often, for at most [`MAX_WATCHED`] people in one
/// read.
pub const PRESENCE_POLL: Duration = Duration::from_mins(1);

/// The most people watched at once (and so named by one read).
pub const MAX_WATCHED: usize = 20;

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

/// The presence polling: who is watched, what each was last read as, and the read under way.
pub(crate) struct Presences<C> {
    client: Arc<Client<HttpTransport<C>>>,
    fence: Arc<Fence>,
    /// Watched, the one asked about longest ago first.
    watched: VecDeque<String>,
    known: HashMap<String, Availability>,
    running: JoinSet<Polled>,
    /// Who the running read asks about.
    asking: Vec<String>,
    /// The next read, at the soonest.
    next_poll: Instant,
    poll: Duration,
}

impl<C: Credentials + 'static> Presences<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, fence: Arc<Fence>, poll: Duration) -> Self {
        Self {
            client,
            fence,
            watched: VecDeque::new(),
            known: HashMap::new(),
            running: JoinSet::new(),
            asking: Vec::new(),
            next_poll: Instant::now(),
            poll,
        }
    }

    /// `mri`'s availability as last read; from now on they are watched (see the module docs). Someone not watched
    /// before is read on the next tick.
    pub(crate) fn presence(&mut self, mri: &str) -> Availability {
        if let Some(at) = self.watched.iter().position(|watched| watched == mri) {
            if let Some(moved) = self.watched.remove(at) {
                self.watched.push_back(moved);
            }
        } else {
            self.watched.push_back(mri.to_owned());
            if self.watched.len() > MAX_WATCHED
                && let Some(dropped) = self.watched.pop_front()
            {
                self.known.remove(&dropped);
            }
            self.next_poll = Instant::now();
        }
        self.known.get(mri).cloned().unwrap_or(Availability::Unknown)
    }

    /// Starts a read when one is due (from the engine's tick): none runs, the poll's wait is over, and someone watched
    /// may be named by one.
    pub(crate) fn poll_due(&mut self) {
        if !self.running.is_empty() || Instant::now() < self.next_poll {
            return;
        }
        self.next_poll = Instant::now() + self.poll;
        // without the presence base the read can't go out: the fence isn't asked for a permit nobody would use
        if !self.client.transport().has_presence() {
            return;
        }
        let asked: Vec<String> = self
            .watched
            .iter()
            .filter(|mri| self.fence.lab_accounts_only(&[Mri::new(mri.as_str())]))
            .cloned()
            .collect();
        if asked.is_empty() {
            return;
        }
        let (client, fence) = (Arc::clone(&self.client), Arc::clone(&self.fence));
        self.asking.clone_from(&asked);
        self.running.spawn(read(client, fence, asked));
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
                if let Some(wait) = wait.filter(|wait| *wait > self.poll) {
                    self.next_poll = Instant::now() + wait;
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

/// One read of `asked`'s presence, under the fence's permit.
async fn read<C: Credentials>(client: Arc<Client<HttpTransport<C>>>, fence: Arc<Fence>, asked: Vec<String>) -> Polled {
    let failed = |asked, wait| Polled { asked, read: Err(wait) };
    let Some(request) = PresenceRead::new(asked.iter().map(|mri| Mri::new(mri.as_str()))) else {
        return failed(asked, None);
    };
    let Ok(permit) = fence.check_post_read(PresenceRead::KIND, request.names()) else {
        return failed(asked, None);
    };
    let answer = match client.post_read(request.clone(), permit).await {
        Ok(answer) => answer,
        Err(PostReadError::Transport(HttpError::Status(answer))) => {
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
