//! The bounded queues between the loop thread and the devices.
//!
//! A full queue drops its oldest entries, so whoever reads always gets the newest media: a reader that falls behind
//! (or starts late) loses the backlog, not the present. The loop thread holds its end as [`Closing`], which closes
//! the queue when the stream ends (it errs, is unlinked, is closed, or the loop stops with the server's fatal
//! error); the device side then hears [`DeviceError::Closed`].

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::devices::DeviceError;

/// One bounded queue, shared by the loop thread and a device.
pub(super) struct Queue<T> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for Queue<T> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

struct Shared<T> {
    state: Mutex<State<T>>,
    arrived: Condvar,
    capacity: usize,
}

struct State<T> {
    items: VecDeque<T>,
    closed: bool,
}

impl<T> Queue<T> {
    /// An open queue keeping at most `capacity` entries.
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    items: VecDeque::with_capacity(capacity),
                    closed: false,
                }),
                arrived: Condvar::new(),
                capacity,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.shared.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Adds `items` at the back, dropping the oldest beyond the capacity.
    ///
    /// # Errors
    /// [`DeviceError::Closed`] once the queue is closed (nothing is added).
    pub(super) fn extend(&self, items: impl IntoIterator<Item = T>) -> Result<(), DeviceError> {
        let mut state = self.lock();
        if state.closed {
            return Err(DeviceError::Closed);
        }
        state.items.extend(items);
        let excess = state.items.len().saturating_sub(self.shared.capacity);
        state.items.drain(..excess);
        drop(state);
        self.shared.arrived.notify_one();
        Ok(())
    }

    /// Adds one entry at the back, dropping the oldest when full; nothing once closed.
    pub(super) fn push(&self, item: T) {
        let _ = self.extend(std::iter::once(item));
    }

    /// The oldest entry, waiting up to `timeout` for one; `None` when none came.
    ///
    /// # Errors
    /// [`DeviceError::Closed`] once the queue is closed and drained.
    pub(super) fn recv(&self, timeout: Duration) -> Result<Option<T>, DeviceError> {
        let (mut state, _) = self
            .shared
            .arrived
            .wait_timeout_while(self.lock(), timeout, |state| state.items.is_empty() && !state.closed)
            .unwrap_or_else(PoisonError::into_inner);
        match state.items.pop_front() {
            Some(item) => Ok(Some(item)),
            None if state.closed => Err(DeviceError::Closed),
            None => Ok(None),
        }
    }

    /// Runs `take` over the entries, oldest first (the speaker's playback takes what it plays from the front).
    pub(super) fn with_items<R>(&self, take: impl FnOnce(&mut VecDeque<T>) -> R) -> R {
        take(&mut self.lock().items)
    }

    /// Closes the queue: no entry is added any more, and a reader hears [`DeviceError::Closed`] once it is drained.
    pub(super) fn close(&self) {
        self.lock().closed = true;
        self.shared.arrived.notify_all();
    }
}

/// The loop thread's end of a queue: closes it when dropped (its stream gone).
pub(super) struct Closing<T>(Queue<T>);

impl<T> Closing<T> {
    pub(super) fn new(queue: Queue<T>) -> Self {
        Self(queue)
    }

    pub(super) fn queue(&self) -> &Queue<T> {
        &self.0
    }
}

impl<T> Drop for Closing<T> {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    const NOW: Duration = Duration::ZERO;

    #[test]
    fn a_source_left_unread_past_its_capacity_hands_over_the_newest_entries() {
        let queue = Queue::new(3);
        for frame in 0..10 {
            queue.push(frame);
        }
        assert_eq!(queue.recv(NOW).unwrap(), Some(7), "the first read is a recent entry");
        assert_eq!(queue.recv(NOW).unwrap(), Some(8));
        assert_eq!(queue.recv(NOW).unwrap(), Some(9));
        assert_eq!(queue.recv(NOW).unwrap(), None);
        // a reader that keeps up again gets each new entry as it comes, with no lasting lag
        queue.push(10);
        assert_eq!(queue.recv(NOW).unwrap(), Some(10));
    }

    #[test]
    fn audio_beyond_the_capacity_drops_the_oldest_samples() {
        let queue = Queue::new(4);
        queue.extend([1, 2, 3]).unwrap();
        queue.extend([4, 5, 6]).unwrap();
        let held: Vec<i32> = queue.with_items(|items| items.drain(..).collect());
        assert_eq!(held, [3, 4, 5, 6]);
    }

    #[test]
    fn a_closed_queue_is_drained_then_reports_closed_and_refuses_more() {
        let queue = Queue::new(3);
        queue.push(1);
        drop(Closing::new(queue.clone()));
        assert!(matches!(queue.extend([2]), Err(DeviceError::Closed)));
        assert_eq!(queue.recv(NOW).unwrap(), Some(1));
        assert!(matches!(queue.recv(NOW), Err(DeviceError::Closed)));
    }

    #[test]
    fn a_waiting_reader_wakes_for_an_entry_and_for_the_close() {
        let queue = Queue::new(3);
        let feeder = queue.clone();
        let feeding = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            feeder.push(1);
            thread::sleep(Duration::from_millis(20));
            feeder.close();
        });
        let long = Duration::from_secs(30);
        assert_eq!(queue.recv(long).unwrap(), Some(1));
        assert!(matches!(queue.recv(long), Err(DeviceError::Closed)));
        feeding.join().unwrap();
    }

    #[test]
    fn a_read_with_nothing_queued_times_out_empty() {
        let queue: Queue<u8> = Queue::new(3);
        assert_eq!(queue.recv(Duration::from_millis(10)).unwrap(), None);
    }
}
