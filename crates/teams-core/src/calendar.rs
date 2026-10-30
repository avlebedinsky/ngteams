//! The account's calendar, read when a view shows a window of it (the calendar's day, week or month): the middle
//! tier's calendar view ([`CalendarView`], `docs/protocol/calendar.md`), one read per ask, answered with the window's
//! events. Nothing is kept: the view asks again as it moves and refreshes.
//!
//! - A window that is empty, reversed or wider than [`teams_proto::MAX_CALENDAR_WINDOW_MS`] is answered with no events
//!   and nothing read.
//! - A read that failed, or an answer that isn't in the shape the web client reads, is [`EngineError::CalendarUnread`]:
//!   the view says the calendar can't be read and asks again later.
//! - Nothing is sent but the read, in either build.

use std::sync::Arc;

use teams_proto::{CalendarEvent, CalendarView, Client, Credentials, HttpTransport};

use crate::engine::{EngineError, Responder};

pub(crate) struct Calendar<C> {
    client: Arc<Client<HttpTransport<C>>>,
}

impl<C: Credentials + 'static> Calendar<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self { client }
    }

    /// Answers `reply` with the account's events between `start_ms` and `end_ms`, once the read is over.
    pub(crate) fn view(&self, start_ms: i64, end_ms: i64, reply: Responder<Vec<CalendarEvent>>) {
        let Some(view) = CalendarView::between(start_ms, end_ms) else {
            let _ = reply.send(Ok(Vec::new()));
            return;
        };
        let client = Arc::clone(&self.client);
        // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
        tokio::task::spawn_local(async move {
            let _ = reply.send(read(&client, &view).await);
        });
    }
}

async fn read<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    view: &CalendarView,
) -> Result<Vec<CalendarEvent>, EngineError> {
    let answer = client.read(view).await.map_err(|error| {
        log::info!("the calendar couldn't be read: {error}");
        EngineError::CalendarUnread
    })?;
    let read = CalendarView::read_answer(answer.body()).map_err(|error| {
        log::info!("the calendar's answer wasn't taken: {error}");
        EngineError::CalendarUnread
    })?;
    if read.skipped > 0 {
        log::info!("the calendar's answer had {} events that don't read", read.skipped);
    }
    log::debug!("the calendar read {} events", read.events.len());
    Ok(read.events)
}
