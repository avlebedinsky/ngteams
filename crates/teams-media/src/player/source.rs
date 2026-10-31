//! A file's bytes as the player reads them: streamed from a [`ByteSource`] (the engine's streamed reads, in the app) as
//! they arrive, as a browser plays a video file from the network, and kept in runs of contiguous bytes, up to
//! [`MAX_HELD`] of them (the least recently read let go first).
//!
//! - **A read** is answered as soon as every byte it wants arrived, not when a block around it did. A read whose bytes
//!   are missing waits for a stream that will bring them within [`NEAR`] bytes, or starts one at its first missing byte,
//!   at most [`STREAM_LEN`] long (it stops where bytes already held begin, and a stream that would run into it stops
//!   there).
//! - **At most [`MAX_STREAMS`] streams** run at once (a demuxer may read two places of a file in turns, while a type
//!   finder probes another), and a stream no read wanted for [`LIVE`] gives way to a new one: what nobody reads doesn't
//!   share the network with what is waited for. A seek ([`Cache::interrupt`]) lets a read waiting on a stream go, and every stream gives way to the first one
//!   the new place needs (one that brings what it wants keeps running), so the old place never holds up the new one.
//! - **Reading ahead**: once a read was answered and no stream runs, a stream starts at the first byte missing within
//!   [`AHEAD`] bytes after it, so playing rarely waits on the network.
//! - A stream that fails on the way is started again where it stopped ([`TRIES`] times in a row, a growing pause
//!   between); a file that can't be had, or a stream that failed every try, fails every read after it.

use std::collections::BTreeMap;
use std::future::Future;
use std::ops::Bound;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Poll, Waker};
use std::thread;
use std::time::{Duration, Instant};

/// The most one stream asks for.
pub const STREAM_LEN: u64 = 16 * 1024 * 1024;
/// How far after what was read the bytes are read ahead.
pub const AHEAD: u64 = 8 * 1024 * 1024;
/// The bytes kept.
pub const MAX_HELD: u64 = 128 * 1024 * 1024;
/// A read whose first missing byte is at most this far past a stream's front waits for that stream.
pub const NEAR: u64 = 256 * 1024;
/// The streams that run at once: a demuxer may read two places of a file in turns, and a type finder probes a third.
pub const MAX_STREAMS: usize = 3;
/// How long a stream no read wanted keeps its place when another one is needed.
pub const LIVE: Duration = Duration::from_millis(500);
/// How long a stream started ahead of any read ([`Cache::prefetch`]) is taken as wanted.
pub const PREFETCHED: Duration = Duration::from_secs(5);
/// How many times in a row a stream is started before the file fails.
pub const TRIES: u32 = 4;

/// Why a part of a file can't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The file can't be had (no link, the account can't read it).
    #[error("the file can't be read")]
    Unavailable,
    /// A read failed on the way (the network): the cache tries again.
    #[error("a read failed on the way")]
    Failed,
    /// The read was let go (its stream stopped, or the player did).
    #[error("stopped")]
    Stopped,
}

/// Where a file's bytes come from: streams of them. Called on the cache's own threads, and may block; a call waiting
/// returns [`SourceError::Stopped`] at once when its [`Abandon`] is set ([`Abandon::wait_for`]).
pub trait ByteSource: Send + Sync {
    /// A stream of `len` bytes of the file from `offset` (fewer where the file ends), once it started.
    ///
    /// # Errors
    /// [`SourceError::Unavailable`] when the file can't be had, [`SourceError::Failed`] when the read failed on the way,
    /// [`SourceError::Stopped`] when abandoned.
    fn open(&self, offset: u64, len: u64, abandon: &Abandon) -> Result<Box<dyn ByteStream>, SourceError>;
}

/// A stream a [`ByteSource`] opened: its bytes in order, as they arrive. Dropped, it stops.
pub trait ByteStream: Send {
    /// The file's whole size.
    fn total(&self) -> u64;

    /// The next bytes; `None` once all of them came.
    ///
    /// # Errors
    /// As [`ByteSource::open`].
    fn next(&mut self, abandon: &Abandon) -> Result<Option<Vec<u8>>, SourceError>;
}

/// A stream of bytes already at hand, in pieces: what a source that holds the file (a test's) hands out.
#[derive(Debug)]
pub struct Pieces {
    total: u64,
    pieces: std::collections::VecDeque<Vec<u8>>,
}

impl Pieces {
    /// `bytes` in pieces of `piece` bytes, of a file of `total` bytes.
    #[must_use]
    pub fn new(bytes: &[u8], total: u64, piece: usize) -> Self {
        Self {
            total,
            pieces: bytes.chunks(piece.max(1)).map(<[u8]>::to_vec).collect(),
        }
    }
}

impl ByteStream for Pieces {
    fn total(&self) -> u64 {
        self.total
    }

    fn next(&mut self, abandon: &Abandon) -> Result<Option<Vec<u8>>, SourceError> {
        if abandon.is_set() {
            return Err(SourceError::Stopped);
        }
        Ok(self.pieces.pop_front())
    }
}

/// Set when a stream is let go: a source's call waiting for it ([`Abandon::wait_for`]) returns at once.
#[derive(Clone, Debug, Default)]
pub struct Abandon(Arc<Signal>);

#[derive(Debug, Default)]
struct Signal {
    set: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Abandon {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether it was set.
    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.set.load(Ordering::SeqCst)
    }

    /// Sets it, waking the call waiting.
    pub fn set(&self) {
        self.0.set.store(true, Ordering::SeqCst);
        let waker = self.0.waker.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    /// Waits for `future` on this thread: its output, or `None` once this is set (`future` dropped unfinished).
    pub fn wait_for<F: Future>(&self, future: F) -> Option<F::Output> {
        let mut future = pin!(future);
        futures_executor::block_on(std::future::poll_fn(|cx| {
            if self.is_set() {
                return Poll::Ready(None);
            }
            if let Poll::Ready(output) = future.as_mut().poll(cx) {
                return Poll::Ready(Some(output));
            }
            *self.0.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
            // set while the waker was being kept: nothing would wake it
            if self.is_set() {
                Poll::Ready(None)
            } else {
                Poll::Pending
            }
        }))
    }
}

/// Bytes held from where a run starts, and when a read last wanted them.
struct Run {
    bytes: Vec<u8>,
    used: Instant,
}

/// A stream running into the run that starts at `start`, until `end`.
struct Stream {
    id: u64,
    start: u64,
    end: u64,
    /// When a read last wanted its bytes (or it started).
    used: Instant,
    abandon: Abandon,
}

struct State {
    runs: BTreeMap<u64, Run>,
    held: u64,
    streams: Vec<Stream>,
    next_id: u64,
    total: Option<u64>,
    failed: Option<SourceError>,
    closed: bool,
    /// Where the last read answered ended.
    reader: u64,
    /// How many times reads were interrupted: a read waiting when it changes gives up.
    interrupts: u64,
}

struct Shared {
    source: Arc<dyn ByteSource>,
    state: Mutex<State>,
    changed: Condvar,
}

/// A file's bytes, streamed (see the module docs). Dropped, its streams stop.
pub struct Cache {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache").finish_non_exhaustive()
    }
}

impl Cache {
    /// A cache over `source`; nothing is read until asked.
    #[must_use]
    pub fn new(source: Arc<dyn ByteSource>) -> Self {
        Self {
            shared: Arc::new(Shared {
                source,
                state: Mutex::new(State {
                    runs: BTreeMap::new(),
                    held: 0,
                    streams: Vec::new(),
                    next_id: 0,
                    total: None,
                    failed: None,
                    closed: false,
                    reader: 0,
                    interrupts: 0,
                }),
                changed: Condvar::new(),
            }),
        }
    }

    /// Starts reading the file from its start (what every player reads first), without waiting.
    pub fn begin(&self) {
        let mut state = self.lock();
        if state.streams.is_empty() && state.missing(0, 1).is_some() {
            want(&self.shared, &mut state, 0);
        }
    }

    /// Starts reading at `at` (what the reads will soon want), without waiting.
    /// No read wants its bytes yet, so its stream is taken as wanted for [`PREFETCHED`] (it doesn't give way before).
    pub fn prefetch(&self, at: u64) {
        let mut state = self.lock();
        if state.missing(at, at.saturating_add(1)).is_none() {
            return;
        }
        want(&self.shared, &mut state, at);
        let until = Instant::now().checked_add(PREFETCHED);
        if let Some(stream) = state.streams.iter_mut().find(|stream| stream.start == at) {
            stream.used = until.unwrap_or(stream.used);
        }
    }

    /// The file's size, once a stream told it; waits for the first.
    ///
    /// # Errors
    /// As [`Cache::read_at`].
    pub fn total(&self) -> Result<u64, SourceError> {
        let mut state = self.lock();
        loop {
            if let Some(total) = state.total {
                return Ok(total);
            }
            state.check()?;
            if state.streams.is_empty() {
                want(&self.shared, &mut state, 0);
            }
            state = self.wait(state);
        }
    }

    /// `len` bytes from `offset` (what there is, past the file's end): at once when all of them arrived; else, when
    /// `wait`, once they did, and `Ok(None)` without waiting. A read waiting when the reads are interrupted
    /// ([`Cache::interrupt`]) gives up with `Ok(None)` too.
    ///
    /// # Errors
    /// The file failed ([`SourceError::Unavailable`], or [`SourceError::Failed`] after every try), or the cache was
    /// closed ([`SourceError::Stopped`]).
    pub fn read_at(&self, offset: u64, len: usize, wait: bool) -> Result<Option<Vec<u8>>, SourceError> {
        let mut state = self.lock();
        let interrupts = state.interrupts;
        loop {
            state.check()?;
            if state.interrupts != interrupts {
                return Ok(None);
            }
            let end = offset.saturating_add(len as u64);
            let end = state.total.map_or(end, |total| end.min(total));
            if end <= offset {
                return Ok(Some(Vec::new()));
            }
            match state.missing(offset, end) {
                None => {
                    let bytes = state.copy(offset, end);
                    state.reader = end;
                    read_ahead(&self.shared, &mut state);
                    return Ok(Some(bytes));
                }
                Some(need) => want(&self.shared, &mut state, need),
            }
            if !wait {
                return Ok(None);
            }
            state = self.wait(state);
        }
    }

    /// Interrupts the reads (a seek): a read waiting gives up (`Ok(None)`), and every stream gives way to the first
    /// one the reads after it need (one bringing what they want keeps running); what arrived is kept.
    pub fn interrupt(&self) {
        let mut state = self.lock();
        state.interrupts += 1;
        let long_ago = Instant::now().checked_sub(LIVE);
        for stream in &mut state.streams {
            stream.used = long_ago.unwrap_or(stream.used);
        }
        self.shared.changed.notify_all();
    }

    /// Closes the cache: its streams stop, and every read, a waiting one too, answers [`SourceError::Stopped`].
    pub fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        for stream in &state.streams {
            stream.abandon.set();
        }
        self.shared.changed.notify_all();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>) -> MutexGuard<'a, State> {
        self.shared
            .changed
            .wait_timeout(state, Duration::from_millis(200))
            .unwrap_or_else(PoisonError::into_inner)
            .0
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.close();
    }
}

impl State {
    /// The file failed, or the cache was closed.
    fn check(&self) -> Result<(), SourceError> {
        if let Some(failed) = self.failed {
            return Err(failed);
        }
        if self.closed {
            return Err(SourceError::Stopped);
        }
        Ok(())
    }

    /// The run holding the byte at `at`, by where it starts.
    fn run_at(&self, at: u64) -> Option<(u64, &Run)> {
        self.runs
            .range(..=at)
            .next_back()
            .filter(|(start, run)| *start + run.bytes.len() as u64 > at)
            .map(|(start, run)| (*start, run))
    }

    /// The first byte from `from` to `to` not held.
    fn missing(&self, from: u64, to: u64) -> Option<u64> {
        let mut at = from;
        while at < to {
            let Some((start, run)) = self.run_at(at) else {
                return Some(at);
            };
            at = start + run.bytes.len() as u64;
        }
        None
    }

    /// The bytes from `from` to `to`, all held.
    fn copy(&mut self, from: u64, to: u64) -> Vec<u8> {
        let now = Instant::now();
        let mut out = Vec::with_capacity(usize::try_from(to - from).unwrap_or(0));
        let mut at = from;
        while at < to {
            let Some((start, _)) = self.run_at(at) else { break };
            let Some(run) = self.runs.get_mut(&start) else { break };
            run.used = now;
            let first = usize::try_from(at - start).unwrap_or(usize::MAX);
            let last = usize::try_from(to - start).unwrap_or(usize::MAX).min(run.bytes.len());
            out.extend_from_slice(run.bytes.get(first..last).unwrap_or_default());
            at = start + last as u64;
        }
        for stream in &mut self.streams {
            if stream.start < to && from < stream.end {
                stream.used = now;
            }
        }
        out
    }

    /// Where `stream`'s next byte goes.
    fn front(&self, stream: &Stream) -> u64 {
        stream.start + self.runs.get(&stream.start).map_or(0, |run| run.bytes.len() as u64)
    }

    /// Stops `stream` (taken out of the streams); its run stays, unless it holds nothing.
    fn let_go(&mut self, stream: &Stream) {
        stream.abandon.set();
        if self.runs.get(&stream.start).is_some_and(|run| run.bytes.is_empty()) {
            self.runs.remove(&stream.start);
        }
    }

    /// Stops the streams no read wanted for [`LIVE`]: what nobody waits for doesn't share the network with what is.
    fn prune(&mut self, now: Instant) {
        let streams = std::mem::take(&mut self.streams);
        let (kept, stale): (Vec<Stream>, Vec<Stream>) = streams
            .into_iter()
            .partition(|stream| now.duration_since(stream.used) < LIVE);
        self.streams = kept;
        for stream in stale {
            self.let_go(&stream);
        }
    }

    /// Lets go of the least recently read runs no stream runs into, past [`MAX_HELD`].
    fn forget_oldest(&mut self) {
        while self.held > MAX_HELD {
            let oldest = self
                .runs
                .iter()
                .filter(|(start, _)| !self.streams.iter().any(|stream| stream.start == **start))
                .min_by_key(|(_, run)| run.used)
                .map(|(start, _)| *start);
            let Some(oldest) = oldest else { return };
            if let Some(run) = self.runs.remove(&oldest) {
                self.held -= run.bytes.len() as u64;
            }
        }
    }
}

/// A read wants the byte at `need`: a stream that brings it soon is waited for, else one starts there; either way the
/// streams no read wanted lately stop.
fn want(shared: &Arc<Shared>, state: &mut State, need: u64) {
    let now = Instant::now();
    let fronts: Vec<u64> = state.streams.iter().map(|stream| state.front(stream)).collect();
    let coming = state
        .streams
        .iter_mut()
        .zip(fronts)
        .find(|(stream, front)| *front <= need && need < stream.end && need - front <= NEAR);
    if let Some((stream, _)) = coming {
        stream.used = now;
        state.prune(now);
        return;
    }
    start_stream(shared, state, need);
}

/// Once a read was answered: the streams no read wanted lately stop, and when none runs, a stream starts at the first
/// byte missing within [`AHEAD`] after it.
fn read_ahead(shared: &Arc<Shared>, state: &mut State) {
    let (from, to) = (state.reader, state.reader.saturating_add(AHEAD));
    let to = state.total.map_or(to, |total| to.min(total));
    let now = Instant::now();
    let mut ahead = false;
    for stream in &mut state.streams {
        if stream.start < to && from < stream.end {
            // what is read next: it keeps its place
            stream.used = now;
            ahead = true;
        }
    }
    state.prune(now);
    if ahead || !state.streams.is_empty() {
        return;
    }
    if let Some(at) = state.missing(from, to) {
        start_stream(shared, state, at);
    }
}

/// Starts a stream at `at` (see the module docs): the streams no read wanted lately give way, and so does the least
/// recently wanted past [`MAX_STREAMS`].
fn start_stream(shared: &Arc<Shared>, state: &mut State, at: u64) {
    if state.closed || state.failed.is_some() || state.total.is_some_and(|total| at >= total) {
        return;
    }
    let now = Instant::now();
    state.prune(now);
    while state.streams.len() >= MAX_STREAMS {
        let Some(oldest) = (0..state.streams.len()).min_by_key(|index| state.streams[*index].used) else {
            break;
        };
        let stream = state.streams.remove(oldest);
        state.let_go(&stream);
    }
    for stream in &mut state.streams {
        if stream.start < at && at < stream.end {
            stream.end = at;
        }
    }
    let next = state
        .runs
        .range((Bound::Excluded(at), Bound::Unbounded))
        .next()
        .map_or(u64::MAX, |(start, _)| *start);
    let end = at
        .saturating_add(STREAM_LEN)
        .min(next)
        .min(state.total.unwrap_or(u64::MAX));
    if end <= at {
        return;
    }
    state.runs.entry(at).or_insert(Run {
        bytes: Vec::new(),
        used: now,
    });
    let (id, abandon) = (state.next_id, Abandon::new());
    state.next_id += 1;
    state.streams.push(Stream {
        id,
        start: at,
        end,
        used: now,
        abandon: abandon.clone(),
    });
    let theirs = Arc::clone(shared);
    let spawned = thread::Builder::new()
        .name("video-file".into())
        .spawn(move || stream_bytes(&theirs, id, &abandon));
    if spawned.is_err() {
        // nothing would ever bring its bytes: every read fails rather than waits
        state.failed = Some(SourceError::Failed);
    }
    shared.changed.notify_all();
}

/// What a stream's thread does next.
enum Step {
    /// Open a stream of this many bytes from there.
    Open(u64, u64),
    /// It was let go, or it is done.
    Stop,
}

/// A stream's thread: streams its bytes into its run until its end, starting again where it stopped when it failed on
/// the way, until it was let go.
fn stream_bytes(shared: &Arc<Shared>, id: u64, abandon: &Abandon) {
    let mut tries = 0;
    let mut pause = Duration::from_millis(250);
    loop {
        let Step::Open(front, len) = next_step(shared, id) else {
            return;
        };
        let failed = match shared.source.open(front, len, abandon) {
            Ok(mut stream) => match drain(shared, id, stream.as_mut(), abandon, &mut tries) {
                Some(error) => error,
                None => return,
            },
            Err(error) => error,
        };
        match failed {
            SourceError::Stopped => return,
            SourceError::Unavailable => return fail(shared, SourceError::Unavailable),
            SourceError::Failed => {
                // a stream that brought bytes since its last failure starts its pauses over too
                if tries == 0 {
                    pause = Duration::from_millis(250);
                }
                tries += 1;
                if tries >= TRIES {
                    return fail(shared, SourceError::Failed);
                }
                thread::sleep(pause);
                pause *= 2;
            }
        }
    }
}

/// Takes what `stream` brings into stream `id`'s run (each piece resets `tries`): `None` once it was let go or reached
/// its end, else how it failed (one that ended before its end failed on the way).
fn drain(
    shared: &Arc<Shared>,
    id: u64,
    stream: &mut dyn ByteStream,
    abandon: &Abandon,
    tries: &mut u32,
) -> Option<SourceError> {
    taken_total(shared, id, stream.total());
    loop {
        match stream.next(abandon) {
            Ok(Some(bytes)) => {
                *tries = 0;
                if !keep(shared, id, &bytes) {
                    return None;
                }
            }
            Ok(None) => return Some(SourceError::Failed),
            Err(error) => return Some(error),
        }
    }
}

/// Where stream `id` goes on, or that it stops (let go, or done: then taken out and the reads ahead looked at).
fn next_step(shared: &Arc<Shared>, id: u64) -> Step {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(index) = state.streams.iter().position(|stream| stream.id == id) else {
        return Step::Stop;
    };
    let front = state.front(&state.streams[index]);
    let end = state.streams[index].end;
    if front < end && !state.closed {
        return Step::Open(front, end - front);
    }
    let stream = state.streams.remove(index);
    state.let_go(&stream);
    read_ahead(shared, &mut state);
    shared.changed.notify_all();
    Step::Stop
}

/// The file's size, as stream `id` was told it: its end held within it.
fn taken_total(shared: &Shared, id: u64, total: u64) {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    state.total = Some(total);
    if let Some(stream) = state.streams.iter_mut().find(|stream| stream.id == id) {
        stream.end = stream.end.min(total);
    }
    shared.changed.notify_all();
}

/// Keeps `bytes` stream `id` brought (up to its end); `false` once it was let go or reached its end.
fn keep(shared: &Arc<Shared>, id: u64, bytes: &[u8]) -> bool {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(index) = state.streams.iter().position(|stream| stream.id == id) else {
        return false;
    };
    let (start, end) = (state.streams[index].start, state.streams[index].end);
    let front = state.front(&state.streams[index]);
    let take = usize::try_from(end.saturating_sub(front))
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    if let Some(run) = state.runs.get_mut(&start) {
        run.bytes.extend_from_slice(&bytes[..take]);
    }
    state.held += take as u64;
    state.forget_oldest();
    shared.changed.notify_all();
    let done = front + take as u64 >= end;
    drop(state);
    if done {
        // taken out, and the reads ahead looked at
        let _ = next_step(shared, id);
    }
    !done
}

/// The file failed: every read after this answers `error`, and every stream stops.
fn fail(shared: &Shared, error: SourceError) {
    let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
    state.failed = Some(error);
    let streams = std::mem::take(&mut state.streams);
    for stream in streams {
        state.let_go(&stream);
    }
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The file at hand, in small pieces; counts the streams opened and fails the first `fail_first`.
    struct Bytes {
        data: Vec<u8>,
        opened: Mutex<Vec<(u64, u64)>>,
        fail_first: AtomicUsize,
    }

    impl Bytes {
        fn new(data: Vec<u8>, fail_first: usize) -> Arc<Self> {
            Arc::new(Self {
                data,
                opened: Mutex::new(Vec::new()),
                fail_first: AtomicUsize::new(fail_first),
            })
        }
    }

    impl ByteSource for Bytes {
        fn open(&self, offset: u64, len: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
            self.opened.lock().unwrap().push((offset, len));
            if self.fail_first.load(Ordering::Relaxed) > 0 {
                self.fail_first.fetch_sub(1, Ordering::Relaxed);
                return Err(SourceError::Failed);
            }
            let start = usize::try_from(offset).unwrap().min(self.data.len());
            let end = (start + usize::try_from(len).unwrap()).min(self.data.len());
            Ok(Box::new(Pieces::new(
                &self.data[start..end],
                self.data.len() as u64,
                1000,
            )))
        }
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|at| u8::try_from(at % 251).unwrap()).collect()
    }

    #[test]
    fn reads_across_streams_and_past_the_end() {
        let bytes = data(300_000);
        let source = Bytes::new(bytes.clone(), 1);
        let cache = Cache::new(source.clone());
        assert_eq!(cache.total(), Ok(bytes.len() as u64));
        let read = cache.read_at(100_000, 30, true).unwrap().unwrap();
        assert_eq!(read, bytes[100_000..100_030]);
        let tail = cache.read_at(bytes.len() as u64 - 20, 50, true).unwrap().unwrap();
        assert_eq!(tail, bytes[bytes.len() - 20..]);
        assert_eq!(cache.read_at(bytes.len() as u64, 10, true), Ok(Some(Vec::new())));
        // the failed first start was tried again from the same place
        let opened = source.opened.lock().unwrap().clone();
        assert_eq!(opened[0].0, 0);
        assert_eq!(opened[1].0, 0);
    }

    #[test]
    fn a_read_far_ahead_starts_a_stream_where_it_wants() {
        let bytes = data(usize::try_from(STREAM_LEN).unwrap() * 2);
        let source = Bytes::new(bytes.clone(), 0);
        let cache = Cache::new(source.clone());
        let at = STREAM_LEN + 12_345;
        let read = cache.read_at(at, 100, true).unwrap().unwrap();
        let start = usize::try_from(at).unwrap();
        assert_eq!(read, bytes[start..start + 100]);
        assert!(source.opened.lock().unwrap().iter().any(|(offset, _)| *offset == at));
    }

    #[test]
    fn an_interrupt_lets_a_waiting_read_go() {
        struct Never;
        impl ByteSource for Never {
            fn open(&self, _: u64, _: u64, abandon: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
                abandon.wait_for(std::future::pending::<()>());
                Err(SourceError::Stopped)
            }
        }
        let cache = Arc::new(Cache::new(Arc::new(Never)));
        let reader = {
            let cache = Arc::clone(&cache);
            thread::spawn(move || cache.read_at(0, 10, true))
        };
        thread::sleep(Duration::from_millis(100));
        cache.interrupt();
        assert_eq!(reader.join().unwrap(), Ok(None));
    }

    #[test]
    fn a_file_that_cant_be_had_fails_every_read() {
        struct Gone;
        impl ByteSource for Gone {
            fn open(&self, _: u64, _: u64, _: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
                Err(SourceError::Unavailable)
            }
        }
        let cache = Cache::new(Arc::new(Gone));
        assert_eq!(cache.total(), Err(SourceError::Unavailable));
        assert_eq!(cache.read_at(0, 4, true), Err(SourceError::Unavailable));
    }
}
