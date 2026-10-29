//! A file's bytes as the player reads them: blocks of [`BLOCK`] bytes read from a [`ByteSource`] (the engine's ranged
//! reads, in the app) by a few threads of the cache's own, kept up to [`MAX_BLOCKS`] of them (the least recently used
//! let go first), and the [`AHEAD`] blocks after each one asked for read ahead, so playing rarely waits on the
//! network.
//!
//! A block whose read failed on the way is tried again ([`TRIES`] times, a growing pause between); a file that can't
//! be had, or a block that failed every try, fails every read after it.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// The bytes a block holds (one ranged read).
pub const BLOCK: u32 = 1024 * 1024;
/// The blocks read ahead of each block asked for.
pub const AHEAD: u64 = 8;
/// The blocks kept.
pub const MAX_BLOCKS: usize = 48;
/// The threads that read blocks.
const READERS: usize = 3;
/// How many times a block is read before the file fails.
const TRIES: u32 = 4;

/// Why a part of a file can't be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    /// The file can't be had (no link, the account can't read it).
    #[error("the file can't be read")]
    Unavailable,
    /// A read failed on the way (the network): the cache tries again.
    #[error("a read failed on the way")]
    Failed,
    /// The player stopped.
    #[error("stopped")]
    Stopped,
}

/// Where a file's bytes come from: `len` bytes (at most [`BLOCK`]) from `offset`, and the file's whole size. Called on
/// the cache's own threads, and may block.
pub trait ByteSource: Send + Sync {
    /// # Errors
    /// [`SourceError::Unavailable`] when the file can't be had, [`SourceError::Failed`] when the read failed on the way.
    fn read(&self, offset: u64, len: u32) -> Result<(Vec<u8>, u64), SourceError>;
}

/// A block: read, being read, or wanted.
enum Block {
    Wanted,
    Reading,
    Read(Arc<Vec<u8>>, Instant),
}

struct State {
    blocks: HashMap<u64, Block>,
    /// The blocks to read, the most urgent first.
    queue: VecDeque<u64>,
    total: Option<u64>,
    failed: Option<SourceError>,
    closed: bool,
}

struct Shared {
    source: Arc<dyn ByteSource>,
    state: Mutex<State>,
    changed: Condvar,
}

/// A file's bytes, read in blocks (see the module docs). Dropped, its threads end.
pub struct Cache {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache").finish_non_exhaustive()
    }
}

impl Cache {
    /// A cache over `source`, its threads started.
    #[must_use]
    pub fn new(source: Arc<dyn ByteSource>) -> Self {
        let shared = Arc::new(Shared {
            source,
            state: Mutex::new(State {
                blocks: HashMap::new(),
                queue: VecDeque::new(),
                total: None,
                failed: None,
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let readers = (0..READERS)
            .filter(|_| {
                let shared = Arc::clone(&shared);
                thread::Builder::new()
                    .name("video-file".into())
                    .spawn(move || read_blocks(&shared))
                    .is_ok()
            })
            .count();
        if readers == 0 {
            // nothing would ever read a block: every read fails rather than waits
            shared.state.lock().unwrap_or_else(PoisonError::into_inner).failed = Some(SourceError::Failed);
        }
        Self { shared }
    }

    /// The file's size, once a block was read; waits for the first block.
    ///
    /// # Errors
    /// As [`Cache::read_at`].
    pub fn total(&self) -> Result<u64, SourceError> {
        let mut state = self.lock();
        loop {
            if let Some(total) = state.total {
                return Ok(total);
            }
            if let Some(failed) = state.failed {
                return Err(failed);
            }
            if state.closed {
                return Err(SourceError::Stopped);
            }
            want(&mut state, 0, true);
            self.shared.changed.notify_all();
            state = self.wait(state);
        }
    }

    /// `len` bytes from `offset`: at once when every block they are in was read; else, when `wait`, once they are,
    /// and `Ok(None)` without waiting. Asks for the blocks missing first and for the ones after them.
    ///
    /// # Errors
    /// The file failed ([`SourceError::Unavailable`], or [`SourceError::Failed`] after every try), or the cache was
    /// closed ([`SourceError::Stopped`]).
    pub fn read_at(&self, offset: u64, len: usize, wait: bool) -> Result<Option<Vec<u8>>, SourceError> {
        let block = u64::from(BLOCK);
        let end = offset.saturating_add(len as u64);
        let (first, last) = (offset / block, end.saturating_sub(1) / block);
        let mut state = self.lock();
        loop {
            if let Some(failed) = state.failed {
                return Err(failed);
            }
            if state.closed {
                return Err(SourceError::Stopped);
            }
            if len == 0 {
                return Ok(Some(Vec::new()));
            }
            let mut out = Vec::with_capacity(len);
            let mut missing = false;
            for index in first..=last {
                match state.blocks.get_mut(&index) {
                    Some(Block::Read(bytes, used)) => {
                        *used = Instant::now();
                        let start = usize::try_from(offset.saturating_sub(index * block)).unwrap_or(usize::MAX);
                        let stop = usize::try_from(end - index * block)
                            .unwrap_or(usize::MAX)
                            .min(bytes.len());
                        out.extend_from_slice(bytes.get(start.min(stop)..stop).unwrap_or_default());
                    }
                    _ => missing = true,
                }
            }
            for index in (first..=last).chain(last + 1..=last + AHEAD) {
                want(&mut state, index, index <= last);
            }
            self.shared.changed.notify_all();
            if !missing {
                // a read past the file's end holds what there is
                return Ok(Some(out));
            }
            if !wait {
                return Ok(None);
            }
            state = self.wait(state);
        }
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

impl Cache {
    /// Closes the cache: its threads end, and every read, a waiting one too, answers [`SourceError::Stopped`].
    pub fn close(&self) {
        self.lock().closed = true;
        self.shared.changed.notify_all();
    }
}

impl Drop for Cache {
    fn drop(&mut self) {
        self.close();
    }
}

/// Asks for block `index` (at the queue's front when `urgent`), unless it is past the file's end, read or being read.
fn want(state: &mut State, index: u64, urgent: bool) {
    if state
        .total
        .is_some_and(|total| index.saturating_mul(u64::from(BLOCK)) >= total.max(1))
    {
        return;
    }
    match state.blocks.get(&index) {
        Some(Block::Read(..) | Block::Reading) => return,
        Some(Block::Wanted) if !urgent => return,
        Some(Block::Wanted) | None => {}
    }
    state.blocks.insert(index, Block::Wanted);
    state.queue.retain(|queued| *queued != index);
    if urgent {
        state.queue.push_front(index);
    } else {
        state.queue.push_back(index);
    }
}

/// One of the cache's threads: reads the most urgent wanted block, keeps it, and lets the oldest go past
/// [`MAX_BLOCKS`].
fn read_blocks(shared: &Shared) {
    loop {
        let index = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if state.closed || state.failed.is_some() {
                    return;
                }
                if let Some(index) = state.queue.pop_front() {
                    if matches!(state.blocks.get(&index), Some(Block::Wanted)) {
                        state.blocks.insert(index, Block::Reading);
                        break index;
                    }
                    continue;
                }
                state = shared.changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let read = read_block(shared, index);
        let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
        match read {
            Ok((bytes, total)) => {
                state.total = Some(total);
                state.blocks.insert(index, Block::Read(Arc::new(bytes), Instant::now()));
                let excess = state.blocks.len().saturating_sub(MAX_BLOCKS);
                forget_oldest(&mut state, excess);
            }
            Err(error) => {
                state.failed = Some(error);
            }
        }
        shared.changed.notify_all();
    }
}

/// Block `index`, tried up to [`TRIES`] times while its reads fail on the way.
fn read_block(shared: &Shared, index: u64) -> Result<(Vec<u8>, u64), SourceError> {
    let mut pause = Duration::from_millis(250);
    for _ in 1..TRIES {
        if shared.state.lock().unwrap_or_else(PoisonError::into_inner).closed {
            return Err(SourceError::Stopped);
        }
        match shared.source.read(index * u64::from(BLOCK), BLOCK) {
            Err(SourceError::Failed) => {
                thread::sleep(pause);
                pause *= 2;
            }
            other => return other,
        }
    }
    shared.source.read(index * u64::from(BLOCK), BLOCK)
}

/// Lets go of the `count` least recently used blocks read.
fn forget_oldest(state: &mut State, count: usize) {
    for _ in 0..count {
        let oldest = state
            .blocks
            .iter()
            .filter_map(|(index, block)| match block {
                Block::Read(_, used) => Some((*index, *used)),
                Block::Wanted | Block::Reading => None,
            })
            .min_by_key(|(_, used)| *used)
            .map(|(index, _)| index);
        let Some(oldest) = oldest else { return };
        state.blocks.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Bytes {
        data: Vec<u8>,
        reads: AtomicUsize,
        fail_first: AtomicUsize,
    }

    impl ByteSource for Bytes {
        fn read(&self, offset: u64, len: u32) -> Result<(Vec<u8>, u64), SourceError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            if self.fail_first.load(Ordering::Relaxed) > 0 {
                self.fail_first.fetch_sub(1, Ordering::Relaxed);
                return Err(SourceError::Failed);
            }
            let start = usize::try_from(offset).unwrap().min(self.data.len());
            let end = (start + len as usize).min(self.data.len());
            Ok((self.data[start..end].to_vec(), self.data.len() as u64))
        }
    }

    fn data(len: usize) -> Vec<u8> {
        (0..len).map(|at| (at % 251) as u8).collect()
    }

    #[test]
    fn reads_across_blocks_and_past_the_end() {
        let bytes = data(BLOCK as usize * 2 + 100);
        let cache = Cache::new(Arc::new(Bytes {
            data: bytes.clone(),
            reads: AtomicUsize::new(0),
            fail_first: AtomicUsize::new(1),
        }));
        assert_eq!(cache.total(), Ok(bytes.len() as u64));
        let at = u64::from(BLOCK) - 10;
        let read = cache.read_at(at, 30, true).unwrap().unwrap();
        assert_eq!(read, bytes[at as usize..at as usize + 30]);
        let tail = cache.read_at(bytes.len() as u64 - 20, 50, true).unwrap().unwrap();
        assert_eq!(tail, bytes[bytes.len() - 20..]);
    }

    #[test]
    fn a_file_that_cant_be_had_fails_every_read() {
        struct Never;
        impl ByteSource for Never {
            fn read(&self, _: u64, _: u32) -> Result<(Vec<u8>, u64), SourceError> {
                Err(SourceError::Unavailable)
            }
        }
        let cache = Cache::new(Arc::new(Never));
        assert_eq!(cache.total(), Err(SourceError::Unavailable));
        assert_eq!(cache.read_at(0, 4, true), Err(SourceError::Unavailable));
    }
}
