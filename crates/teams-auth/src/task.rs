//! A request's network work on a task of its own.
//!
//! An engine awaits a request deep inside its own futures, and in a debug build each of their poll frames holds its
//! child future by value. A connection's TLS handshake (the certificate parse recurses) on top of that once overflowed
//! the engine thread's stack. On its own task, the connect and the handshake run from the scheduler, at a shallow depth,
//! whoever waits for them.

use std::future::Future;

use tokio::task::JoinHandle;

/// `future`'s output, `future` run as a task of its own on the current tokio runtime. Dropping the returned future
/// aborts the task, as dropping `future` itself would end it; a panic in it goes on in the caller.
///
/// # Panics
///
/// Outside a tokio runtime, as every reqwest request does.
pub async fn own_task<F>(future: F) -> F::Output
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let mut task = AbortOnDrop(tokio::spawn(future));
    match (&mut task.0).await {
        Ok(output) => output,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        // only a runtime that shuts down cancels it, and that drops this caller too
        Err(_) => std::future::pending().await,
    }
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use super::own_task;

    #[tokio::test]
    async fn the_output_comes_back() {
        assert_eq!(own_task(async { 7 }).await, 7);
    }

    #[tokio::test]
    async fn a_dropped_caller_ends_the_task() {
        let finished = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&finished);
        let waiting = own_task(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            flag.store(true, Ordering::SeqCst);
        });
        assert!(tokio::time::timeout(Duration::from_millis(5), waiting).await.is_err());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!finished.load(Ordering::SeqCst));
    }

    #[tokio::test]
    #[should_panic(expected = "synthetic task failure")]
    async fn a_panic_goes_on_in_the_caller() {
        own_task(async {
            std::panic::panic_any("synthetic task failure");
        })
        .await;
    }
}
