//! Reply ownership for callers on asynchronous or blocking threads: a late answer, a cancelled answer, and a
//! receiver released before its answer must all finish without hanging or retaining an engine.

#![cfg(test)]

use teams_core::{EngineError, Reply};

#[test]
fn ready_and_later_replies_keep_their_result_across_threads() {
    assert_eq!(Reply::ready(Ok(7)).blocking_wait().unwrap(), 7);
    let (answer, reply) = Reply::later();
    let writer = std::thread::spawn(move || answer.send(Ok(11)));
    assert_eq!(reply.blocking_wait().unwrap(), 11);
    writer.join().unwrap();
    let (answer, reply) = Reply::<u8>::later();
    drop(answer);
    assert!(matches!(reply.blocking_wait(), Err(EngineError::Stopped)));
    let (answer, reply) = Reply::<u8>::later();
    drop(reply);
    answer.send(Ok(12));
}

#[tokio::test]
async fn asynchronous_ready_failed_and_cancelled_replies_preserve_their_error() {
    assert_eq!(Reply::ready(Ok(9)).await.unwrap(), 9);
    assert!(matches!(
        Reply::<u8>::ready(Err(EngineError::CalendarUnread)).await,
        Err(EngineError::CalendarUnread)
    ));
    let (answer, reply) = Reply::<u8>::later();
    drop(answer);
    assert!(matches!(reply.await, Err(EngineError::Stopped)));
}
