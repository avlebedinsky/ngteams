//! What a topic's subscribers can read again: the fan-out is at most once, so a payload worth replaying is also kept
//! in the topic's journal for [`KEPT_FOR`], at most [`CAPACITY`] of them. A reader (a push endpoint) keeps a cursor,
//! the last entry it was given, and reads what came after it, on whichever node it reconnects to.
//!
//! [`LocalJournal`] is one process's (a single node, SQLite); [`RedisJournal`] keeps each topic in a Redis Stream
//! (`journal:<topic>`, capped with `MAXLEN`, expiring [`KEPT_FOR`] after its last entry) and a topic's cursors in a
//! hash (`cursors:<topic>`), at most [`CURSORS`] of them, so neither grows with the readers that come and go.
//! [`Keeping`] is the fan-out with a journal behind it: it keeps what its rule picks, then publishes, and says so on
//! the fan-out when it couldn't keep one.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bytes::Bytes;
use redis::aio::ConnectionManager;

use super::{Fanout, FanoutError, Subscription};

/// How long an entry is kept after it was appended (Teams' deliveries live 15 minutes, `ttl=900`).
pub const KEPT_FOR: Duration = Duration::from_mins(15);
/// The most entries a topic keeps.
pub const CAPACITY: usize = 512;
/// The most entries one [`Journal::after`] returns.
pub const BATCH: usize = 64;
/// How long a reader's cursor is kept after it last moved.
pub const CURSOR_KEPT_FOR: Duration = Duration::from_hours(24);
/// The most readers a topic keeps cursors of: one more pushes out the one that moved longest ago.
pub const CURSORS: usize = 64;

/// An entry's place in its topic: the time it was appended (Unix milliseconds) and its number within that
/// millisecond, a Redis Stream id. Later entries compare greater.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryId {
    pub ms: u64,
    pub seq: u64,
}

impl EntryId {
    /// `<ms>-<seq>`, as Redis writes it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let (ms, seq) = text.split_once('-')?;
        Some(Self {
            ms: ms.parse().ok()?,
            seq: seq.parse().ok()?,
        })
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.ms, self.seq)
    }
}

/// A kept payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub payload: Bytes,
}

/// What a topic still holds after a cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kept {
    /// The entries after the cursor, oldest first, at most [`BATCH`].
    pub entries: Vec<Entry>,
    /// Whether more follow these.
    pub more: bool,
    /// Whether these are all there ever were after the cursor: `false` when entries after it may have been dropped
    /// (the topic overflowed, or the cursor is older than the journal keeps).
    pub complete: bool,
    /// Whether the topic overflowed past the cursor: the one gap a reader that never fell behind can still have.
    pub overflowed: bool,
}

/// A journal every node holds one of.
#[async_trait]
pub trait Journal: Send + Sync + std::fmt::Debug {
    /// Keeps `payload` as `topic`'s newest entry.
    async fn append(&self, topic: &str, payload: Bytes) -> Result<EntryId, FanoutError>;

    /// The entries of `topic` after `cursor`.
    async fn after(&self, topic: &str, cursor: EntryId) -> Result<Kept, FanoutError>;

    /// A cursor after which only entries appended from now on come.
    async fn tail(&self, topic: &str) -> Result<EntryId, FanoutError>;

    /// The cursor `reader` last kept in `topic`, while it is kept.
    async fn cursor(&self, topic: &str, reader: &str) -> Result<Option<EntryId>, FanoutError>;

    /// Keeps `reader`'s cursor in `topic`, among the topic's [`CURSORS`] that moved last.
    async fn set_cursor(&self, topic: &str, reader: &str, cursor: EntryId) -> Result<(), FanoutError>;
}

/// Whether `topic` overflowed past `cursor`: it is full, with nothing at or before the cursor left.
fn overflowed(first: Option<EntryId>, len: usize, cursor: EntryId) -> bool {
    len >= CAPACITY && first.is_none_or(|first| first > cursor)
}

/// Whether entries after `cursor` may be gone: nothing at or before the cursor is left to prove the topic lived on
/// since, and it either overflowed or the cursor is older than entries are kept.
fn incomplete(first: Option<EntryId>, len: usize, cursor: EntryId, now_ms: u64) -> bool {
    let kept_ms = u64::try_from(KEPT_FOR.as_millis()).unwrap_or(u64::MAX);
    first.is_none_or(|first| first > cursor) && (len >= CAPACITY || cursor.ms.saturating_add(kept_ms) < now_ms)
}

/// Whether a cursor last moved at `set_ms` is still kept at `now_ms`.
fn cursor_kept(set_ms: u64, now_ms: u64) -> bool {
    let kept_ms = u64::try_from(CURSOR_KEPT_FOR.as_millis()).unwrap_or(u64::MAX);
    set_ms.saturating_add(kept_ms) >= now_ms
}

/// The cursor just before anything appended at `now_ms` or later.
fn before(now_ms: u64) -> EntryId {
    EntryId {
        ms: now_ms.saturating_sub(1),
        seq: u64::MAX,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
}

/// Topics and cursors one process sweeps for stale ones once it holds this many.
const SWEEP_AT: usize = 1024;

#[derive(Debug, Default)]
struct LocalState {
    topics: HashMap<String, VecDeque<Entry>>,
    /// Each topic's readers: the cursor and when it last moved.
    cursors: HashMap<String, HashMap<String, (EntryId, u64)>>,
    last: Option<EntryId>,
}

/// The in-process journal: one node.
#[derive(Debug, Default)]
pub struct LocalJournal {
    state: Mutex<LocalState>,
}

impl LocalJournal {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LocalState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn expired(entry: &Entry, now: u64) -> bool {
    let kept_ms = u64::try_from(KEPT_FOR.as_millis()).unwrap_or(u64::MAX);
    entry.id.ms.saturating_add(kept_ms) < now
}

#[async_trait]
impl Journal for LocalJournal {
    async fn append(&self, topic: &str, payload: Bytes) -> Result<EntryId, FanoutError> {
        let now = now_ms();
        let mut state = self.state();
        let id = match state.last {
            Some(last) if last.ms >= now => EntryId {
                ms: last.ms,
                seq: last.seq + 1,
            },
            _ => EntryId { ms: now, seq: 0 },
        };
        state.last = Some(id);
        if state.topics.len() >= SWEEP_AT {
            state
                .topics
                .retain(|_, entries| entries.back().is_some_and(|newest| !expired(newest, now)));
        }
        let entries = state.topics.entry(topic.to_owned()).or_default();
        // as the Redis key does: a topic nothing was appended to for the whole time starts again
        if entries.back().is_some_and(|newest| expired(newest, now)) {
            entries.clear();
        }
        entries.push_back(Entry { id, payload });
        while entries.len() > CAPACITY {
            entries.pop_front();
        }
        Ok(id)
    }

    async fn after(&self, topic: &str, cursor: EntryId) -> Result<Kept, FanoutError> {
        let now = now_ms();
        let state = self.state();
        let kept = state
            .topics
            .get(topic)
            .filter(|entries| entries.back().is_some_and(|newest| !expired(newest, now)));
        let Some(kept) = kept else {
            return Ok(Kept {
                complete: !incomplete(None, 0, cursor, now),
                ..Kept::default()
            });
        };
        let mut later = kept.iter().filter(|entry| entry.id > cursor);
        let entries: Vec<Entry> = later.by_ref().take(BATCH).cloned().collect();
        Ok(Kept {
            entries,
            more: later.next().is_some(),
            complete: !incomplete(kept.front().map(|entry| entry.id), kept.len(), cursor, now),
            overflowed: overflowed(kept.front().map(|entry| entry.id), kept.len(), cursor),
        })
    }

    async fn tail(&self, topic: &str) -> Result<EntryId, FanoutError> {
        let now = now_ms();
        let state = self.state();
        let newest = state
            .topics
            .get(topic)
            .and_then(|entries| entries.back())
            .filter(|newest| !expired(newest, now));
        Ok(newest.map_or_else(
            || state.last.map_or_else(|| before(now), |last| last.max(before(now))),
            |newest| newest.id,
        ))
    }

    async fn cursor(&self, topic: &str, reader: &str) -> Result<Option<EntryId>, FanoutError> {
        let now = now_ms();
        let state = self.state();
        let kept = state.cursors.get(topic).and_then(|readers| readers.get(reader));
        Ok(kept
            .filter(|(_, set)| cursor_kept(*set, now))
            .map(|(cursor, _)| *cursor))
    }

    async fn set_cursor(&self, topic: &str, reader: &str, cursor: EntryId) -> Result<(), FanoutError> {
        let now = now_ms();
        let mut state = self.state();
        if state.cursors.len() >= SWEEP_AT {
            state
                .cursors
                .retain(|_, readers| readers.values().any(|(_, set)| cursor_kept(*set, now)));
        }
        let readers = state.cursors.entry(topic.to_owned()).or_default();
        readers.insert(reader.to_owned(), (cursor, now));
        while readers.len() > CURSORS {
            let oldest = readers
                .iter()
                .filter(|(name, _)| name.as_str() != reader)
                .min_by_key(|(_, (_, set))| *set)
                .map(|(name, _)| name.clone());
            let Some(oldest) = oldest else { break };
            readers.remove(&oldest);
        }
        Ok(())
    }
}

/// The journal in Redis Streams, shared by every node.
#[derive(Clone)]
pub struct RedisJournal {
    commands: ConnectionManager,
}

impl std::fmt::Debug for RedisJournal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisJournal").finish_non_exhaustive()
    }
}

/// A stream's entries as `XRANGE` answers them: the id, then the fields and values.
type Range = Vec<(String, Vec<(String, Vec<u8>)>)>;

impl RedisJournal {
    /// A journal over a node's command connection ([`super::Redis::connection`]).
    #[must_use]
    pub fn new(commands: ConnectionManager) -> Self {
        Self { commands }
    }

    fn key(topic: &str) -> String {
        format!("journal:{topic}")
    }

    fn cursors_key(topic: &str) -> String {
        format!("cursors:{topic}")
    }
}

/// Keeps a reader's cursor (`ARGV[1]`, `ARGV[2]`) with Redis' time in the topic's hash, drops the readers beyond
/// `ARGV[3]` that moved longest ago, and lets the hash expire `ARGV[4]` seconds after its last move.
const SET_CURSOR: &str = r"
local time = redis.call('TIME')
local now = time[1] * 1000 + math.floor(time[2] / 1000)
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2] .. ' ' .. string.format('%.0f', now))
local over = redis.call('HLEN', KEYS[1]) - tonumber(ARGV[3])
if over > 0 then
  local all = redis.call('HGETALL', KEYS[1])
  local readers = {}
  for i = 1, #all, 2 do
    if all[i] ~= ARGV[1] then
      readers[#readers + 1] = {all[i], tonumber(string.match(all[i + 1], ' (%d+)$')) or 0}
    end
  end
  table.sort(readers, function(a, b) return a[2] < b[2] end)
  for i = 1, math.min(over, #readers) do
    redis.call('HDEL', KEYS[1], readers[i][1])
  end
end
redis.call('EXPIRE', KEYS[1], ARGV[4])
return 1
";

/// A kept cursor as the hash holds it, `<cursor> <moved, Unix milliseconds>`, while it is kept at `now_ms`.
fn kept_cursor(value: &str, now_ms: u64) -> Option<EntryId> {
    let (cursor, set) = value.split_once(' ')?;
    cursor_kept(set.parse().ok()?, now_ms)
        .then(|| EntryId::parse(cursor))
        .flatten()
}

fn entries(range: Range) -> Vec<Entry> {
    range
        .into_iter()
        .filter_map(|(id, fields)| {
            let payload = fields.into_iter().find(|(name, _)| name == "p")?.1;
            Some(Entry {
                id: EntryId::parse(&id)?,
                payload: Bytes::from(payload),
            })
        })
        .collect()
}

/// Redis' clock as `TIME` answers it, in milliseconds.
fn redis_ms((seconds, micros): (u64, u64)) -> u64 {
    seconds.saturating_mul(1000).saturating_add(micros / 1000)
}

#[async_trait]
impl Journal for RedisJournal {
    async fn append(&self, topic: &str, payload: Bytes) -> Result<EntryId, FanoutError> {
        let key = Self::key(topic);
        let mut commands = self.commands.clone();
        let (id,): (String,) = redis::pipe()
            .cmd("XADD")
            .arg(&key)
            .arg("MAXLEN")
            .arg(CAPACITY)
            .arg("*")
            .arg("p")
            .arg(payload.as_ref())
            .cmd("EXPIRE")
            .arg(&key)
            .arg(KEPT_FOR.as_secs())
            .ignore()
            .query_async(&mut commands)
            .await?;
        EntryId::parse(&id).ok_or_else(|| FanoutError::Unavailable("an unreadable stream id".to_owned()))
    }

    async fn after(&self, topic: &str, cursor: EntryId) -> Result<Kept, FanoutError> {
        let key = Self::key(topic);
        let mut commands = self.commands.clone();
        let (now, len, first, later): ((u64, u64), usize, Range, Range) = redis::pipe()
            .cmd("TIME")
            .cmd("XLEN")
            .arg(&key)
            .cmd("XRANGE")
            .arg(&key)
            .arg("-")
            .arg("+")
            .arg("COUNT")
            .arg(1)
            .cmd("XRANGE")
            .arg(&key)
            .arg(format!("({cursor}"))
            .arg("+")
            .arg("COUNT")
            .arg(BATCH + 1)
            .query_async(&mut commands)
            .await?;
        let first = first.first().and_then(|(id, _)| EntryId::parse(id));
        let mut later = entries(later);
        let more = later.len() > BATCH;
        later.truncate(BATCH);
        Ok(Kept {
            entries: later,
            more,
            complete: !incomplete(first, len, cursor, redis_ms(now)),
            overflowed: overflowed(first, len, cursor),
        })
    }

    async fn tail(&self, topic: &str) -> Result<EntryId, FanoutError> {
        let mut commands = self.commands.clone();
        let (now, newest): ((u64, u64), Range) = redis::pipe()
            .cmd("TIME")
            .cmd("XREVRANGE")
            .arg(Self::key(topic))
            .arg("+")
            .arg("-")
            .arg("COUNT")
            .arg(1)
            .query_async(&mut commands)
            .await?;
        let newest = newest.first().and_then(|(id, _)| EntryId::parse(id));
        Ok(newest.unwrap_or_else(|| before(redis_ms(now))))
    }

    async fn cursor(&self, topic: &str, reader: &str) -> Result<Option<EntryId>, FanoutError> {
        let mut commands = self.commands.clone();
        let (now, kept): ((u64, u64), Option<String>) = redis::pipe()
            .cmd("TIME")
            .cmd("HGET")
            .arg(Self::cursors_key(topic))
            .arg(reader)
            .query_async(&mut commands)
            .await?;
        Ok(kept.and_then(|value| kept_cursor(&value, redis_ms(now))))
    }

    async fn set_cursor(&self, topic: &str, reader: &str, cursor: EntryId) -> Result<(), FanoutError> {
        let mut commands = self.commands.clone();
        redis::cmd("EVAL")
            .arg(SET_CURSOR)
            .arg(1)
            .arg(Self::cursors_key(topic))
            .arg(reader)
            .arg(cursor.to_string())
            .arg(CURSORS)
            .arg(CURSOR_KEPT_FOR.as_secs())
            .query_async::<()>(&mut commands)
            .await?;
        Ok(())
    }
}

/// Which payloads of which topics [`Keeping`] keeps.
pub type Keep = fn(topic: &str, payload: &[u8]) -> bool;

/// What tells a topic's subscribers that a payload of it wasn't kept: the topic and payload [`Keeping`] publishes.
pub type Unkept = fn(topic: &str) -> Option<(String, Bytes)>;

/// A fan-out that keeps what `keep` picks in the journal before it publishes it, so a subscriber woken by the
/// payload finds it there. A subscriber reads kept payloads only from the journal, so one the journal refused would
/// pass it by unseen: `unkept`'s word goes out with it.
#[derive(Debug)]
pub struct Keeping {
    fanout: Arc<dyn Fanout>,
    journal: Arc<dyn Journal>,
    keep: Keep,
    unkept: Unkept,
}

impl Keeping {
    #[must_use]
    pub fn new(fanout: Arc<dyn Fanout>, journal: Arc<dyn Journal>, keep: Keep, unkept: Unkept) -> Self {
        Self {
            fanout,
            journal,
            keep,
            unkept,
        }
    }
}

#[async_trait]
impl Fanout for Keeping {
    async fn publish(&self, topic: &str, payload: Bytes) -> Result<(), FanoutError> {
        if (self.keep)(topic, &payload)
            && let Err(error) = self.journal.append(topic, payload.clone()).await
        {
            tracing::warn!(%error, "a payload wasn't kept for replay");
            if let Some((topic, word)) = (self.unkept)(topic) {
                self.fanout.publish(&topic, word).await?;
            }
        }
        self.fanout.publish(topic, payload).await
    }

    async fn subscribe(&self, topic: &str) -> Result<Subscription, FanoutError> {
        self.fanout.subscribe(topic).await
    }

    async fn ready(&self) -> Result<(), FanoutError> {
        self.fanout.ready().await
    }

    fn breaks(&self) -> u64 {
        self.fanout.breaks()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_ids_read_and_order_as_stream_ids() {
        let id = EntryId::parse("1700000000000-3").unwrap();
        assert_eq!(
            id,
            EntryId {
                ms: 1_700_000_000_000,
                seq: 3
            }
        );
        assert_eq!(id.to_string(), "1700000000000-3");
        assert!(EntryId::parse("1700000000000-4").unwrap() > id);
        assert!(EntryId::parse("1700000000001-0").unwrap() > id);
        for text in ["", "1", "1-", "-1", "a-1", "1-b"] {
            assert_eq!(EntryId::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_cursor_is_incomplete_only_when_entries_after_it_may_be_gone() {
        let kept = u64::try_from(KEPT_FOR.as_millis()).unwrap();
        let now = 10 * kept;
        let recent = EntryId { ms: now - 5, seq: 0 };
        let old = EntryId {
            ms: now - kept - 5,
            seq: 0,
        };
        let newer = |cursor: EntryId| {
            Some(EntryId {
                ms: cursor.ms + 1,
                seq: 0,
            })
        };
        assert!(!incomplete(None, 0, recent, now));
        assert!(!incomplete(newer(recent), 3, recent, now));
        assert!(incomplete(newer(recent), CAPACITY, recent, now));
        assert!(!incomplete(Some(recent), CAPACITY, recent, now));
        assert!(incomplete(None, 0, old, now));
        assert!(incomplete(newer(old), 1, old, now));
        assert!(!incomplete(Some(old), 1, old, now));
        // a reader that followed the topic can only have lost entries to an overflow, however old its cursor
        assert!(overflowed(newer(old), CAPACITY, old));
        assert!(!overflowed(newer(old), 1, old));
        assert!(!overflowed(Some(old), CAPACITY, old));
        assert!(!overflowed(None, 0, old));
    }

    #[test]
    fn a_kept_cursor_is_read_until_it_is_too_old() {
        let day = u64::try_from(CURSOR_KEPT_FOR.as_millis()).unwrap();
        let cursor = EntryId { ms: 7, seq: 1 };
        assert_eq!(kept_cursor("7-1 1000", 1000 + day), Some(cursor));
        assert_eq!(kept_cursor("7-1 1000", 1001 + day), None);
        for unreadable in ["", "7-1", "7-1 x", "x 1000"] {
            assert_eq!(kept_cursor(unreadable, 1000), None, "{unreadable}");
        }
    }

    /// A journal that keeps nothing.
    #[derive(Debug)]
    struct Refusing;

    #[async_trait]
    impl Journal for Refusing {
        async fn append(&self, _: &str, _: Bytes) -> Result<EntryId, FanoutError> {
            Err(FanoutError::Unavailable("full".to_owned()))
        }
        async fn after(&self, _: &str, _: EntryId) -> Result<Kept, FanoutError> {
            Ok(Kept::default())
        }
        async fn tail(&self, _: &str) -> Result<EntryId, FanoutError> {
            Ok(before(now_ms()))
        }
        async fn cursor(&self, _: &str, _: &str) -> Result<Option<EntryId>, FanoutError> {
            Ok(None)
        }
        async fn set_cursor(&self, _: &str, _: &str, _: EntryId) -> Result<(), FanoutError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_payload_the_journal_refuses_is_published_with_the_word_that_it_was_not_kept() {
        let fanout: Arc<dyn Fanout> = Arc::new(crate::fanout::Local::new());
        let keeping = Keeping::new(
            Arc::clone(&fanout),
            Arc::new(Refusing),
            |topic, _| topic.starts_with("kept:"),
            |topic| Some((format!("word:{topic}"), Bytes::from_static(b"unkept"))),
        );
        let mut word = fanout.subscribe("word:kept:a").await.unwrap();
        let mut payloads = keeping.subscribe("kept:a").await.unwrap();
        let mut others = keeping.subscribe("other:a").await.unwrap();
        keeping.publish("kept:a", Bytes::from_static(b"one")).await.unwrap();
        keeping.publish("other:a", Bytes::from_static(b"two")).await.unwrap();
        assert_eq!(word.recv().await.unwrap(), Bytes::from_static(b"unkept"));
        assert_eq!(payloads.recv().await.unwrap(), Bytes::from_static(b"one"));
        assert_eq!(others.recv().await.unwrap(), Bytes::from_static(b"two"));
        // nothing was said of the payload that was never to be kept
        keeping.publish("kept:a", Bytes::from_static(b"three")).await.unwrap();
        assert_eq!(word.recv().await.unwrap(), Bytes::from_static(b"unkept"));
        assert!(word.receiver.is_empty());
        keeping.ready().await.unwrap();
        assert_eq!(keeping.breaks(), 0);
        // the journal's other answers, which a fan-out never asks for
        let none = EntryId { ms: 0, seq: 0 };
        assert!(Refusing.after("kept:a", none).await.unwrap().entries.is_empty());
        assert!(Refusing.tail("kept:a").await.unwrap() > none);
        Refusing.set_cursor("kept:a", "reader", none).await.unwrap();
        assert_eq!(Refusing.cursor("kept:a", "reader").await.unwrap(), None);
    }
}
