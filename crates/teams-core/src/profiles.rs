//! People's names and pictures (M2 Task 12): what the views call someone and show beside their name, read in the
//! background and kept in the store.
//!
//! - **Where a name comes from**, first that gives one: Microsoft Graph's user record (`users/<object id>`, a read,
//!   [`GraphUser`]); the middle tier's profile lookup (`users/fetchShortProfile`, a read sent as a `POST`,
//!   [`ShortProfileLookup`], sent by [`Client::lookup`], whoever it names), for a name Graph doesn't give; the name
//!   the person's newest stored message carried (`imdisplayname`);
//!   a name the binary knows already ([`Parts::known_names`](crate::Parts::known_names): a lab binary's lab accounts,
//!   from `lab.toml`). Else none, and a view shows the MRI. A **picture** is Graph's 96×96 one ([`GraphPicture`]);
//!   without one a view draws initials.
//! - **Kept in the store** ([`teams_store::Profile`]) and read again once older than [`PROFILE_MAX_AGE`]. A read that
//!   failed on the way (the network, a 5xx, a throttled or refused token) keeps what the last read gave for that part
//!   and is tried again [`PROFILE_RETRY`] later (the engine's
//!   [`EngineOptions::profile_retry`](crate::EngineOptions::profile_retry)), never sooner, from the engine's tick
//!   whether or not a view asks for them again; an answer that settles it (a 404 for a picture, an account Graph
//!   doesn't know, a lookup the transport refuses) is kept as "none" until the next read by age.
//! - **One lookup per person at a time**: a person asked for while their lookup is queued or running isn't looked up
//!   again; at most [`MAX_LOOKUPS`] run at once, on the engine's runtime, so the engine's loop never waits for one.
//! - **The bytes of a picture reach a view only from the engine** ([`Person::picture`]): nothing else reads them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use teams_fence::Mri;
use teams_proto::{Client, Credentials, GraphPicture, GraphUser, HttpError, HttpTransport, ShortProfileLookup};
use teams_store::{NameSource, PictureState, Profile, Store, StoreError, StoredPicture};
use tokio::task::{Id, JoinSet};
use tokio::time::Instant;

/// How old a stored profile may get before it is read again: a day.
pub const PROFILE_MAX_AGE: Duration = Duration::from_hours(24);

/// How long after a read that failed on the way the person is read again: five minutes.
pub const PROFILE_RETRY: Duration = Duration::from_mins(5);

/// The most lookups that run at once.
pub const MAX_LOOKUPS: usize = 4;

/// Where the name a view shows came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameFrom {
    /// Microsoft Graph's user record.
    Graph,
    /// The middle tier's profile lookup.
    ShortProfile,
    /// The person's newest stored message (`imdisplayname`).
    Message,
    /// A name the binary knew already (a lab binary's lab accounts).
    Known,
}

/// A person as the views show them: their name, where it came from, and their picture. `Debug` leaves the name out.
#[derive(Clone, PartialEq, Eq)]
pub struct Person {
    pub mri: String,
    /// The name to show; `None` when nothing gave one.
    pub name: Option<String>,
    pub from: Option<NameFrom>,
    /// Their picture's bytes, undecoded; `None` when they have none or it wasn't read yet.
    pub picture: Option<StoredPicture>,
    /// Whether a lookup of them is queued or running (this ask's or an earlier one's): its change, when it brings one,
    /// comes as [`Event::Profile`](crate::Event::Profile). `false` when their stored profile is fresh or a failed
    /// lookup's retry wait runs: no lookup is under way, and none is told.
    pub looking: bool,
}

impl fmt::Debug for Person {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Person")
            .field("mri", &self.mri)
            .field("from", &self.from)
            .field("picture", &self.picture)
            .field("looking", &self.looking)
            .finish_non_exhaustive()
    }
}

/// How one part of a lookup went.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part<T> {
    Found(T),
    /// An answer that settles it: there is none (for this build).
    Absent,
    /// Failed on the way: what the last read gave is kept, and it is tried again later.
    Failed,
}

impl<T> Part<T> {
    fn failed(&self) -> bool {
        matches!(self, Self::Failed)
    }
}

/// A finished lookup of one person.
#[derive(Debug)]
pub(crate) struct Looked {
    mri: String,
    name: Part<(String, NameSource)>,
    picture: Part<StoredPicture>,
}

/// The lookups: who is queued, who runs, who waits after a failure.
pub(crate) struct Profiles<C> {
    client: Arc<Client<HttpTransport<C>>>,
    known: HashMap<String, String>,
    running: JoinSet<Looked>,
    /// Who each running task looks up.
    tasks: HashMap<Id, String>,
    /// Queued or running.
    under_way: HashSet<String>,
    queued: VecDeque<String>,
    /// After a failed lookup: not before then, and then again.
    retry_at: HashMap<String, Instant>,
    /// How long after a failed lookup it is tried again.
    retry: Duration,
}

impl<C: Credentials + 'static> Profiles<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, known: Vec<(String, String)>, retry: Duration) -> Self {
        Self {
            client,
            known: known.into_iter().collect(),
            running: JoinSet::new(),
            tasks: HashMap::new(),
            under_way: HashSet::new(),
            queued: VecDeque::new(),
            retry_at: HashMap::new(),
            retry,
        }
    }

    /// `mri` as the views show them now (see the module docs), from the store and what the binary knows; a lookup is
    /// started (or queued) when their stored profile is missing or old.
    pub(crate) fn person(&mut self, store: &Store, mri: &str) -> Result<Person, StoreError> {
        if due(store.profile(mri)?.as_ref()) {
            self.queue(mri);
        }
        stored_person(store, mri, &self.known, self.under_way.contains(mri))
    }

    /// Whether a lookup runs or waits to.
    pub(crate) fn busy(&self) -> bool {
        !self.running.is_empty()
    }

    /// The next lookup to finish; never while none runs ([`Self::busy`]).
    pub(crate) async fn next(&mut self) -> Option<Looked> {
        loop {
            let joined = self.running.join_next_with_id().await?;
            let (id, looked) = match joined {
                Ok((id, looked)) => (id, Some(looked)),
                // a lookup that panicked or was cancelled: tried again after the wait
                Err(error) => (error.id(), None),
            };
            let Some(mri) = self.tasks.remove(&id) else {
                continue;
            };
            self.under_way.remove(&mri);
            self.start_queued();
            match looked {
                Some(looked) => return Some(looked),
                None => {
                    self.retry_at.insert(mri, Instant::now() + self.retry);
                }
            }
        }
    }

    /// Stores what a lookup found over what the store held (see the module docs); the profile when it changed.
    pub(crate) fn store(&mut self, store: &mut Store, looked: Looked) -> Result<Option<Profile>, StoreError> {
        let old = store.profile(&looked.mri)?;
        if looked.name.failed() || looked.picture.failed() {
            self.retry_at.insert(looked.mri.clone(), Instant::now() + self.retry);
        } else {
            self.retry_at.remove(&looked.mri);
        }
        let profile = merged(old.as_ref(), looked, now_ms());
        if old.as_ref() == Some(&profile) {
            return Ok(None);
        }
        store.put_profile(&profile)?;
        Ok(Some(profile))
    }

    /// Queues the lookups that failed and whose wait is over (from the engine's tick): a person isn't left on what
    /// a failed lookup gave until a view happens to ask for them again. Its change, when it brings one, is told as
    /// any lookup's.
    pub(crate) fn retry_due(&mut self) {
        let now = Instant::now();
        let mut due: Vec<String> = Vec::new();
        self.retry_at.retain(|mri, at| {
            let waits = now < *at;
            if !waits {
                due.push(mri.clone());
            }
            waits
        });
        for mri in due {
            self.queue(&mri);
        }
    }

    /// Queues a lookup of `mri`, unless one is under way or one failed within the retry wait.
    fn queue(&mut self, mri: &str) {
        if self.under_way.contains(mri) || self.retry_at.get(mri).is_some_and(|at| Instant::now() < *at) {
            return;
        }
        self.under_way.insert(mri.to_owned());
        self.queued.push_back(mri.to_owned());
        self.start_queued();
    }

    fn start_queued(&mut self) {
        while self.running.len() < MAX_LOOKUPS {
            let Some(mri) = self.queued.pop_front() else {
                return;
            };
            let handle = self.running.spawn(look_up(Arc::clone(&self.client), mri.clone()));
            self.tasks.insert(handle.id(), mri);
        }
    }
}

/// `mri` as the views show them, from the store and the names the binary knows (`known`), `looking` told as given;
/// nothing is looked up.
pub(crate) fn stored_person(
    store: &Store,
    mri: &str,
    known: &HashMap<String, String>,
    looking: bool,
) -> Result<Person, StoreError> {
    let stored = store.profile(mri)?;
    let mut person = Person {
        mri: mri.to_owned(),
        name: None,
        from: None,
        picture: None,
        looking,
    };
    if let Some(profile) = stored {
        if let PictureState::Some(picture) = profile.picture {
            person.picture = Some(picture);
        }
        person.from = profile.name_source.map(|source| match source {
            NameSource::Graph => NameFrom::Graph,
            NameSource::ShortProfile => NameFrom::ShortProfile,
        });
        person.name = profile.name.filter(|_| person.from.is_some());
    }
    if person.name.is_none() {
        let (name, from) = match store.display_name(mri)? {
            Some(name) => (Some(name), Some(NameFrom::Message)),
            None => match known.get(mri) {
                Some(name) => (Some(name.clone()), Some(NameFrom::Known)),
                None => (None, None),
            },
        };
        person.name = name;
        person.from = from;
    }
    Ok(person)
}

/// Whether `stored` is due a read: none, never settled, or older than [`PROFILE_MAX_AGE`].
pub(crate) fn due(stored: Option<&Profile>) -> bool {
    let Some(profile) = stored else {
        return true;
    };
    let age = u64::try_from(now_ms().saturating_sub(profile.read_at)).unwrap_or(0);
    profile.read_at == 0 || Duration::from_millis(age) > PROFILE_MAX_AGE
}

/// The profile a lookup leaves: each part as found, none when it settled there is none, the last one's when it failed;
/// `read_at` moves only when nothing failed.
fn merged(old: Option<&Profile>, looked: Looked, now: i64) -> Profile {
    let settled = !looked.name.failed() && !looked.picture.failed();
    let (name, name_source) = match looked.name {
        Part::Found((name, source)) => (Some(name), Some(source)),
        Part::Absent => (None, None),
        Part::Failed => old.map_or((None, None), |old| (old.name.clone(), old.name_source)),
    };
    let picture = match looked.picture {
        Part::Found(picture) => PictureState::Some(picture),
        Part::Absent => PictureState::None,
        Part::Failed => old.map_or(PictureState::Unread, |old| old.picture.clone()),
    };
    Profile {
        mri: looked.mri,
        name,
        name_source,
        picture,
        read_at: if settled { now } else { old.map_or(0, |old| old.read_at) },
    }
}

/// One person's lookup (see the module docs).
async fn look_up<C: Credentials>(client: Arc<Client<HttpTransport<C>>>, mri: String) -> Looked {
    let graph = graph_name(&client, &mri).await;
    let name = match graph {
        Part::Found(name) => Part::Found((name, NameSource::Graph)),
        graph => match short_profile_name(&client, &mri).await {
            Part::Found(name) => Part::Found((name, NameSource::ShortProfile)),
            // the lookup settled there is none: still a failure if Graph's read failed on the way
            Part::Absent if graph.failed() => Part::Failed,
            Part::Absent => Part::Absent,
            Part::Failed => Part::Failed,
        },
    };
    let picture = graph_picture(&client, &mri).await;
    Looked { mri, name, picture }
}

async fn graph_name<C: Credentials>(client: &Client<HttpTransport<C>>, mri: &str) -> Part<String> {
    let Some(user) = GraphUser::of(mri) else {
        return Part::Absent;
    };
    match client.read(&user).await {
        // an answer of another form or another account gives no name: never guessed
        Ok(answer) => user
            .read_answer(answer.body())
            .ok()
            .flatten()
            .map_or(Part::Absent, Part::Found),
        Err(error) => part_of(&error),
    }
}

async fn graph_picture<C: Credentials>(client: &Client<HttpTransport<C>>, mri: &str) -> Part<StoredPicture> {
    let Some(read) = GraphPicture::of(mri) else {
        return Part::Absent;
    };
    match client.read(&read).await {
        // a picture of a type not kept, or too large, is none
        Ok(answer) => read.read_answer(&answer).map_or(Part::Absent, |picture| {
            Part::Found(StoredPicture {
                media_type: picture.format.media_type().to_owned(),
                bytes: picture.bytes,
            })
        }),
        Err(error) => part_of(&error),
    }
}

/// The middle tier's name for `mri`.
async fn short_profile_name<C: Credentials>(client: &Client<HttpTransport<C>>, mri: &str) -> Part<String> {
    // without the middle tier's base the lookup can't go out
    if !client.transport().has_middle_tier() {
        return Part::Absent;
    }
    let Some(lookup) = ShortProfileLookup::new([Mri::new(mri)]) else {
        return Part::Absent;
    };
    match client.lookup(lookup.clone()).await {
        Ok(answer) => lookup
            .read_answer(answer.body())
            .ok()
            .and_then(|profiles| profiles.into_iter().find(|profile| profile.mri.as_str() == mri))
            .and_then(|profile| profile.display_name)
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .map_or(Part::Absent, Part::Found),
        Err(error) => part_of(&error),
    }
}

/// What a read's error says about the part it read: settled when the service answered that there is none (a 4xx
/// other than a refused token, a timeout or throttling) or the transport can't send it (no route, refused); failed on
/// the way otherwise.
fn part_of<T>(error: &HttpError) -> Part<T> {
    match error {
        HttpError::Status(answer) if (400..500).contains(&answer.status()) => match answer.status() {
            401 | 408 | 429 => Part::Failed,
            _ => Part::Absent,
        },
        HttpError::Refused(_) | HttpError::NoRoute | HttpError::Unknown => Part::Absent,
        _ => Part::Failed,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}
