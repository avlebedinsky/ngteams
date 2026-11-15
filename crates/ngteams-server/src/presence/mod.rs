//! Presence, as Teams' unified presence service (UPS) serves it (`docs/protocol/trouter.md`, "Others' presence" and
//! "Own presence"): an account shows as its live endpoints say, a manual status over them, and offline without one.
//!
//! - **State** is on the [`board`]: per account a hash (its manual status, when it was last active, each endpoint
//!   with the time it lapses), and the sorted set of the accounts with an endpoint, by the next lapse. In Redis every
//!   node reads and writes it; without Redis it is the node's own.
//! - **An endpoint lapses** when it isn't reported again within `presence.endpoint_ttl`. Each node sweeps the due
//!   accounts every second; the node that takes an account off the set tells its watchers.
//! - **Subscriptions**: an endpoint watches people (`presence.max_subscriptions` at most) for
//!   `presence.subscription_ttl`. A change of someone is published on each watcher's `push:<mri>`, naming the
//!   watching endpoints, and the push socket of each delivers it at its `unifiedPresenceService` path
//!   (`crate::trouter`). Nothing is kept for replay: the client's subscription after a reconnect gets a snapshot.

mod board;
mod routes;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use board::BoardError;
pub(crate) use routes::router;

use crate::config::PresenceConfig;
use crate::db::schema::users;
use crate::db::{Db, DbError, with_conn};
use crate::fanout::Fanout;
use board::{Board, Memory, Shared, key};

/// How long an account's hash lives after its last write: a manual status nobody reported under for this long is
/// forgotten.
const ACCOUNT_TTL: u64 = 30 * 86_400;
/// How often a node looks for lapsed endpoints.
const SWEEP: Duration = Duration::from_secs(1);
/// The accounts one sweep takes.
const SWEPT: usize = 256;
/// The watchers one change is told to.
const TOLD: usize = 10_000;
/// The resource type of a presence delivery's envelope on `push:<mri>`.
pub(crate) const RESOURCE_TYPE: &str = "Presence";
/// `lastActiveTime` of an account that never was active.
const NEVER: &str = "0001-01-01T00:00:00Z";
/// The source a subscription names.
const SOURCE: &str = "ups";

/// What an endpoint may report, the one shown first: of an account's endpoints the first of these wins.
const AVAILABILITIES: [&str; 8] = [
    "DoNotDisturb",
    "Busy",
    "BusyIdle",
    "Available",
    "AvailableIdle",
    "BeRightBack",
    "Away",
    "Offline",
];
/// The manual statuses.
const MANUAL: [&str; 6] = ["Available", "Busy", "DoNotDisturb", "Away", "BeRightBack", "Offline"];

/// Why a presence request isn't served.
#[derive(Debug, thiserror::Error)]
pub enum PresenceError {
    #[error(transparent)]
    Board(#[from] BoardError),
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("the account reports too many endpoints")]
    TooManyEndpoints,
}

/// An endpoint as its account reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Endpoint {
    availability: String,
    activity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    /// When it lapses (Unix milliseconds).
    until: i64,
}

/// An account's state as the board holds it.
#[derive(Debug, Default)]
struct State {
    manual: Option<String>,
    last_active: Option<i64>,
    endpoints: BTreeMap<String, Endpoint>,
}

impl State {
    fn read(fields: Vec<(String, String)>) -> Self {
        let mut state = Self::default();
        for (field, value) in fields {
            if field == "manual" {
                state.manual = Some(value);
            } else if field == "last" {
                state.last_active = value.parse().ok();
            } else if let Some(epid) = field.strip_prefix("ep:")
                && let Ok(endpoint) = serde_json::from_str(&value)
            {
                state.endpoints.insert(epid.to_owned(), endpoint);
            }
        }
        state
    }

    /// What the account shows at `now`, with the endpoints that lapsed by then left out.
    fn shown(&self, now: i64) -> Shown {
        let live = self.endpoints.values().filter(|endpoint| endpoint.until > now);
        let first = live.min_by_key(|endpoint| rank(&endpoint.availability));
        let (availability, activity, device) = match (first, &self.manual) {
            (None, _) => ("Offline".to_owned(), "Offline".to_owned(), None),
            (Some(endpoint), Some(manual)) => (manual.clone(), manual.clone(), endpoint.device.clone()),
            (Some(endpoint), None) => (
                endpoint.availability.clone(),
                endpoint.activity.clone(),
                endpoint.device.clone(),
            ),
        };
        Shown {
            availability,
            activity,
            device,
            last_active: self.last_active,
        }
    }
}

fn rank(availability: &str) -> usize {
    AVAILABILITIES
        .iter()
        .position(|known| *known == availability)
        .unwrap_or(AVAILABILITIES.len())
}

/// What an account shows.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Shown {
    availability: String,
    activity: String,
    device: Option<String>,
    last_active: Option<i64>,
}

impl Shown {
    /// Whether a watcher sees the same.
    fn same(&self, other: &Self) -> bool {
        self.availability == other.availability && self.activity == other.activity
    }

    /// The `presence` object of an item, as `own` (the account itself) or a colleague reads it.
    fn presence(&self, own: bool, out_of_office: bool) -> Value {
        let last = self
            .last_active
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map_or_else(
                || NEVER.to_owned(),
                |time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            );
        let mut presence = json!({
            "sourceNetwork": if own { "Self" } else { "SameEnterprise" },
            "calendarData": {"isOutOfOffice": out_of_office},
            "capabilities": [],
            "availability": self.availability,
            "activity": self.activity,
            "lastActiveTime": last,
        });
        if let Some(device) = &self.device {
            presence["deviceType"] = json!(device);
        }
        presence
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn millis(seconds: u64) -> i64 {
    i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX)
}

/// An item's `etag`: it grows with time.
fn etag(now: i64) -> String {
    format!("A{now:016X}")
}

fn mri(oid: &str) -> String {
    format!("8:orgid:{oid}")
}

/// The object id an MRI of a work account names.
fn object_id(mri: &str) -> Option<&str> {
    mri.strip_prefix("8:orgid:")
        .filter(|id| uuid::Uuid::parse_str(id).is_ok())
}

/// The topic an account's events are published on (`crate::fanout`).
fn push_topic(oid: &str) -> String {
    format!("push:{}", mri(oid))
}

/// A watcher as a watched account's set names it.
fn watcher(oid: &str, endpoint: &str) -> String {
    format!("{oid}|{endpoint}")
}

/// What a subscription made of one MRI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Taken {
    Subscribed,
    NotFound,
    TooMany,
}

impl Taken {
    /// UPS's status for it.
    fn status(self) -> i64 {
        match self {
            Self::Subscribed => 20200,
            Self::NotFound => 40020,
            Self::TooMany => 40040,
        }
    }
}

/// A subscription request of an endpoint.
pub(crate) struct Subscription<'a> {
    /// The account's object id.
    pub oid: &'a str,
    /// Its Trouter endpoint (`crate::trouter::endpoint_id`).
    pub endpoint: &'a str,
    pub purge: bool,
    pub add: &'a [String],
    pub remove: &'a [String],
}

struct Inner {
    board: Box<dyn Board>,
    fanout: Arc<dyn Fanout>,
    db: Db,
    limits: PresenceConfig,
}

/// The presence service of a node.
pub struct Presence(Arc<Inner>);

impl std::fmt::Debug for Presence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Presence").field("board", &self.0.board).finish()
    }
}

impl Presence {
    /// The service on Redis (`Some`: the state every node shares) or on the node's own state, sweeping lapsed
    /// endpoints until it is dropped.
    #[must_use]
    pub fn start(redis: Option<ConnectionManager>, fanout: Arc<dyn Fanout>, db: Db, limits: PresenceConfig) -> Self {
        let board: Box<dyn Board> = match redis {
            Some(redis) => Box::new(Shared(redis)),
            None => Box::new(Memory::default()),
        };
        let inner = Arc::new(Inner {
            board,
            fanout,
            db,
            limits,
        });
        tokio::spawn(sweeping(Arc::downgrade(&inner)));
        Self(inner)
    }

    /// How many endpoints watch `mri` now.
    ///
    /// # Errors
    /// The state can't be reached.
    pub async fn watchers(&self, mri: &str) -> Result<usize, BoardError> {
        let watchers = self
            .0
            .board
            .zrange(&key("watchers", mri), now_ms() + 1, i64::MAX, TOLD)
            .await?;
        Ok(watchers.len())
    }

    /// The items a presence read of `asked` by the account `viewer` answers: one per MRI, in order.
    pub(crate) async fn read(&self, viewer: &str, asked: &[(String, bool)]) -> Result<Vec<Value>, PresenceError> {
        let names: Vec<String> = asked.iter().map(|(mri, _)| mri.clone()).collect();
        let people = self.0.people(&names).await?;
        let now = now_ms();
        let mut items = Vec::with_capacity(asked.len());
        for (mri, sourced) in asked {
            let Some((oid, out_of_office)) = person(&people, mri) else {
                items.push(json!({"mri": mri, "status": 40400}));
                continue;
            };
            let shown = self.0.state(oid).await?.shown(now);
            let mut item = json!({
                "mri": mri,
                "presence": shown.presence(oid == viewer, out_of_office),
                "etagMatch": false,
                "etag": etag(now),
                "status": 20000,
            });
            if *sourced {
                item["source"] = json!(SOURCE);
            }
            items.push(item);
        }
        Ok(items)
    }

    /// Keeps the account's endpoint `epid` as reported, for the endpoint lifetime, and tells its watchers when what
    /// it shows changed.
    pub(crate) async fn report(
        &self,
        oid: &str,
        epid: &str,
        availability: &str,
        activity: &str,
        device: Option<&str>,
    ) -> Result<(), PresenceError> {
        let inner = &self.0;
        let account = key("account", oid);
        let now = now_ms();
        let mut state = inner.state(oid).await?;
        let before = state.shown(now);
        inner.drop_lapsed(&account, &mut state, now).await?;
        if !state.endpoints.contains_key(epid) && state.endpoints.len() >= inner.limits.max_endpoints {
            return Err(PresenceError::TooManyEndpoints);
        }
        let endpoint = Endpoint {
            availability: availability.to_owned(),
            activity: activity.to_owned(),
            device: device.map(str::to_owned),
            until: now.saturating_add(millis(inner.limits.endpoint_ttl)),
        };
        let value = serde_json::to_string(&endpoint).unwrap_or_default();
        inner
            .board
            .hset(&account, &format!("ep:{epid}"), &value, ACCOUNT_TTL)
            .await?;
        inner
            .board
            .hset(&account, "last", &now.to_string(), ACCOUNT_TTL)
            .await?;
        state.endpoints.insert(epid.to_owned(), endpoint);
        state.last_active = Some(now);
        inner.schedule(oid, &state).await?;
        inner.told_if_changed(oid, &before, &state.shown(now)).await
    }

    /// Sets the account's manual status, or clears it, and tells its watchers when what it shows changed.
    pub(crate) async fn force(&self, oid: &str, manual: Option<&str>) -> Result<(), PresenceError> {
        let inner = &self.0;
        let account = key("account", oid);
        let now = now_ms();
        let mut state = inner.state(oid).await?;
        let before = state.shown(now);
        match manual {
            Some(manual) => inner.board.hset(&account, "manual", manual, ACCOUNT_TTL).await?,
            None => inner.board.hdel(&account, &["manual".to_owned()]).await?,
        }
        state.manual = manual.map(str::to_owned);
        inner.told_if_changed(oid, &before, &state.shown(now)).await
    }

    /// Changes whom an endpoint watches; what became of each MRI it adds, in order. The endpoint is then sent a
    /// snapshot of those it now watches anew.
    pub(crate) async fn subscribe(&self, request: &Subscription<'_>) -> Result<Vec<Taken>, PresenceError> {
        let inner = &self.0;
        let me = watcher(request.oid, request.endpoint);
        let watching = key("watching", &me);
        let now = now_ms();
        if request.purge {
            inner.unwatch_all(&watching, &me).await?;
        }
        for mri in request.remove {
            inner.board.zrem(&key("watchers", mri), &me).await?;
            inner.board.zrem(&watching, mri).await?;
        }
        inner.board.zprune(&watching, now).await?;
        let people = inner.people(request.add).await?;
        let mut held = inner
            .board
            .zrange(&watching, now + 1, i64::MAX, usize::MAX)
            .await?
            .len();
        let mut taken = Vec::with_capacity(request.add.len());
        let mut snapshot = Vec::new();
        for mri in request.add {
            let Some((oid, out_of_office)) = person(&people, mri) else {
                taken.push(Taken::NotFound);
                continue;
            };
            if !inner.watch(&watching, &me, mri, &mut held, now).await? {
                taken.push(Taken::TooMany);
                continue;
            }
            let shown = inner.state(oid).await?.shown(now);
            snapshot.push(pushed_item(mri, &shown, oid == request.oid, out_of_office, now));
            taken.push(Taken::Subscribed);
        }
        if !snapshot.is_empty() {
            let resource = json!({"presence": snapshot, "isSnapshot": true});
            inner.push(request.oid, &[request.endpoint.to_owned()], &resource).await;
        }
        Ok(taken)
    }
}

/// The object id `mri` names and whether that account is out of office, when it is an account that can sign in.
fn person<'a>(people: &HashMap<String, bool>, mri: &'a str) -> Option<(&'a str, bool)> {
    let oid = object_id(mri)?;
    people.get(oid).map(|out_of_office| (oid, *out_of_office))
}

/// An item of a presence delivery.
fn pushed_item(mri: &str, shown: &Shown, own: bool, out_of_office: bool, now: i64) -> Value {
    json!({"mri": mri, "etag": etag(now), "source": SOURCE, "presence": shown.presence(own, out_of_office)})
}

impl Inner {
    /// Ends everything the watcher `me` watches.
    async fn unwatch_all(&self, watching: &str, me: &str) -> Result<(), BoardError> {
        for mri in self.board.zrange(watching, i64::MIN, i64::MAX, usize::MAX).await? {
            self.board.zrem(&key("watchers", &mri), me).await?;
        }
        self.board.del(watching).await
    }

    /// Makes the watcher `me` watch `mri` for the subscription lifetime; `false`, and nothing kept, when it watches
    /// as many as it may (`held`) and `mri` isn't among them.
    async fn watch(&self, watching: &str, me: &str, mri: &str, held: &mut usize, now: i64) -> Result<bool, BoardError> {
        let known = self.board.zscore(watching, mri).await?.is_some();
        if !known && *held >= self.limits.max_subscriptions {
            return Ok(false);
        }
        *held += usize::from(!known);
        let until = now.saturating_add(millis(self.limits.subscription_ttl));
        self.board.zadd(watching, mri, until).await?;
        self.board.zadd(&key("watchers", mri), me, until).await?;
        Ok(true)
    }

    async fn state(&self, oid: &str) -> Result<State, BoardError> {
        Ok(State::read(self.board.hgetall(&key("account", oid)).await?))
    }

    /// Of `mris`, the accounts that can sign in, by object id: whether each is out of office.
    async fn people(&self, mris: &[String]) -> Result<HashMap<String, bool>, DbError> {
        let ids: Vec<&str> = mris.iter().filter_map(|mri| object_id(mri)).collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let db = &self.db;
        let found: Vec<String> = with_conn!(db, |conn| users::table
            .filter(users::id.eq_any(&ids))
            .filter(users::disabled.eq(false))
            .select(users::id)
            .load(conn)
            .await?);
        let away = crate::people::out_of_office(db, &found).await?;
        Ok(found
            .into_iter()
            .map(|id| {
                let out = away.contains(&id);
                (id, out)
            })
            .collect())
    }

    /// Forgets the endpoints of `state` that lapsed by `now`.
    async fn drop_lapsed(&self, account: &str, state: &mut State, now: i64) -> Result<(), BoardError> {
        let lapsed: Vec<String> = state
            .endpoints
            .iter()
            .filter(|(_, endpoint)| endpoint.until <= now)
            .map(|(epid, _)| format!("ep:{epid}"))
            .collect();
        state.endpoints.retain(|_, endpoint| endpoint.until > now);
        self.board.hdel(account, &lapsed).await
    }

    /// Puts the account on the set of those with an endpoint, at its next lapse.
    async fn schedule(&self, oid: &str, state: &State) -> Result<(), BoardError> {
        match state.endpoints.values().map(|endpoint| endpoint.until).min() {
            Some(next) => self.board.zadd(&key("due", "accounts"), oid, next).await,
            None => Ok(()),
        }
    }

    async fn told_if_changed(&self, oid: &str, before: &Shown, after: &Shown) -> Result<(), PresenceError> {
        if before.same(after) {
            return Ok(());
        }
        self.tell(oid, after).await
    }

    /// Tells the endpoints that watch the account what it shows now.
    async fn tell(&self, oid: &str, shown: &Shown) -> Result<(), PresenceError> {
        let mri = mri(oid);
        let watched = key("watchers", &mri);
        let now = now_ms();
        self.board.zprune(&watched, now).await?;
        let watchers = self.board.zrange(&watched, now + 1, i64::MAX, TOLD).await?;
        if watchers.is_empty() {
            return Ok(());
        }
        let out_of_office = self.people(std::slice::from_ref(&mri)).await?.get(oid) == Some(&true);
        let mut endpoints: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for watcher in &watchers {
            if let Some((account, endpoint)) = watcher.split_once('|') {
                endpoints.entry(account).or_default().push(endpoint.to_owned());
            }
        }
        for (account, endpoints) in endpoints {
            let item = pushed_item(&mri, shown, account == oid, out_of_office, now);
            self.push(account, &endpoints, &json!({"presence": [item]})).await;
        }
        Ok(())
    }

    /// Publishes a presence delivery for `endpoints` of the account `oid`.
    async fn push(&self, oid: &str, endpoints: &[String], resource: &Value) {
        let envelope = json!({"resourceType": RESOURCE_TYPE, "endpoints": endpoints, "resource": resource});
        let published = self.fanout.publish(&push_topic(oid), envelope.to_string().into()).await;
        if let Err(error) = published {
            tracing::warn!(%error, "a presence change wasn't pushed; the client's next read shows it");
        }
    }

    /// Takes the accounts whose next lapse is due off the set, forgets their lapsed endpoints and tells their
    /// watchers. An account another node took at the same moment is that node's.
    async fn sweep(&self) -> Result<(), PresenceError> {
        let due = key("due", "accounts");
        let now = now_ms();
        for oid in self.board.zrange(&due, i64::MIN, now, SWEPT).await? {
            if !self.board.zrem(&due, &oid).await? {
                continue;
            }
            let mut state = self.state(&oid).await?;
            // as it showed just before its first endpoint lapsed
            let before = state.shown(i64::MIN);
            self.drop_lapsed(&key("account", &oid), &mut state, now).await?;
            self.schedule(&oid, &state).await?;
            self.told_if_changed(&oid, &before, &state.shown(now)).await?;
        }
        Ok(())
    }
}

/// Sweeps until the service is dropped. A state that can't be reached is said once, not every second.
async fn sweeping(presence: Weak<Inner>) {
    let mut every = tokio::time::interval(SWEEP);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut failing = false;
    loop {
        every.tick().await;
        let Some(inner) = presence.upgrade() else { return };
        failing = swept(inner.sweep().await, failing);
    }
}

/// Whether the sweeps fail now, said when that changes.
fn swept(sweep: Result<(), PresenceError>, failing: bool) -> bool {
    match &sweep {
        Ok(()) if failing => tracing::info!("lapsed presence endpoints are swept again"),
        Ok(()) => {}
        Err(error) if !failing => tracing::warn!(%error, "lapsed presence endpoints aren't swept"),
        Err(_) => {}
    }
    sweep.is_err()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(availability: &str, activity: &str, until: i64) -> String {
        json!({"availability": availability, "activity": activity, "until": until}).to_string()
    }

    #[test]
    fn an_account_shows_as_its_busiest_live_endpoint_under_its_manual_status() {
        let fields = |manual: Option<&str>| {
            let mut fields = vec![
                ("ep:one".to_owned(), endpoint("Away", "Away", 100)),
                ("ep:two".to_owned(), endpoint("Busy", "InACall", 50)),
                ("ep:three".to_owned(), endpoint("Dancing", "Dancing", 100)),
                ("ep:broken".to_owned(), "not json".to_owned()),
                ("last".to_owned(), "7".to_owned()),
                ("other".to_owned(), "x".to_owned()),
            ];
            fields.extend(manual.map(|manual| ("manual".to_owned(), manual.to_owned())));
            fields
        };
        let state = State::read(fields(None));
        let shown = state.shown(10);
        assert_eq!(
            (shown.availability.as_str(), shown.activity.as_str()),
            ("Busy", "InACall")
        );
        assert_eq!(shown.last_active, Some(7));
        assert_eq!(state.shown(60).availability, "Away");
        assert_eq!(state.shown(100).availability, "Offline");
        let manual = State::read(fields(Some("DoNotDisturb")));
        assert_eq!(manual.shown(10).activity, "DoNotDisturb");
        assert_eq!(manual.shown(100).availability, "Offline");
        assert!(shown.same(&state.shown(11)));
        assert!(!shown.same(&state.shown(60)));
    }

    #[test]
    fn a_presence_names_its_network_last_activity_and_device() {
        let shown = Shown {
            availability: "Available".into(),
            activity: "Available".into(),
            device: Some("Web".into()),
            last_active: Some(1_700_000_000_123),
        };
        let own = shown.presence(true, true);
        assert_eq!(own["sourceNetwork"], "Self");
        assert_eq!(own["calendarData"]["isOutOfOffice"], true);
        assert_eq!(own["lastActiveTime"], "2023-11-14T22:13:20.123Z");
        assert_eq!(own["deviceType"], "Web");
        let never = Shown {
            device: None,
            last_active: None,
            ..shown
        };
        let other = never.presence(false, false);
        assert_eq!(other["sourceNetwork"], "SameEnterprise");
        assert_eq!(other["lastActiveTime"], NEVER);
        assert_eq!(other.get("deviceType"), None);
        assert!(etag(2) > etag(1));
        assert_eq!(Taken::TooMany.status(), 40040);
        assert_eq!(object_id("8:orgid:nobody"), None);
        assert_eq!(millis(u64::MAX), i64::MAX);
    }

    #[test]
    fn a_failing_sweep_is_said_when_it_starts_and_when_it_ends() {
        let failed = || Err(PresenceError::TooManyEndpoints);
        assert!(swept(failed(), false));
        assert!(swept(failed(), true));
        assert!(!swept(Ok(()), true));
        assert!(!swept(Ok(()), false));
    }
}
