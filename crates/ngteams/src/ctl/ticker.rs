//! The `meeting_starting` events: a ticker over each account's calendar, running only while a subscriber wants the kind.
//!
//! Every [`TICK`] it reads each account's calendar as the store kept it ([`Core::stored_calendar`], the window from
//! the longest lead a subscriber wants up to now: a query, which the engine answers even while it catches up). Beside
//! it, each account's calendar is read again from the service ([`Core::calendar`], which keeps what it read in the
//! store; one that fails leaves what was kept) every [`REFRESH`], and at once when a subscriber looks further ahead
//! than the last read reached. That read runs on its own, one at a time per account: the engine may hold it through a
//! catch-up, and no tick waits for it, so a slow account holds up neither its own stored meetings nor another
//! account's. What it read is told as it comes, as the store may keep it only after the engine answered. A cancelled
//! or all-day event is no meeting that starts. The hub tells each subscriber a meeting once per lead it wants (its
//! `leads`, else [`DEFAULT_LEADS`]), when that lead's moment came within the last [`GRACE_MS`]: a subscriber that comes
//! after a moment passed isn't told it late.
//!
//! The time is GPUI's wall clock ([`wall_ms`]), the system's in the app and the test clock in a test. The ticker starts
//! with the first subscriber that wants the kind and stops once none does (the quit closes every stream).
//!
//! [`Core::stored_calendar`]: teams_ui::Core::stored_calendar
//! [`Core::calendar`]: teams_ui::Core::calendar

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, AsyncApp};
use teams_proto::CalendarEvent;

use super::events::{Accounts, Hub, Meeting, Origin};

/// The event kind the ticker makes.
pub const KIND: &str = "meeting_starting";
/// How often the calendar is looked at.
pub const TICK: Duration = Duration::from_secs(15);
/// How often each account's calendar is read again from the service.
pub const REFRESH: Duration = Duration::from_mins(5);
/// How long after a lead's moment it is still told.
pub const GRACE_MS: i64 = 60_000;
/// The leads, in minutes before the start, a subscriber that names none is told at.
pub const DEFAULT_LEADS: &[u32] = &[5, 0];

const MINUTE_MS: i64 = 60_000;

/// Now on GPUI's wall clock, in ms since the epoch: the system's, or a test's clock, which its timers follow.
#[must_use]
pub fn wall_ms(cx: &App) -> i64 {
    cx.background_executor()
        .scheduler_executor()
        .scheduler()
        .clock()
        .utc_now()
        .timestamp_millis()
}

/// The ticker over `accounts`, publishing on `hub`, unless it runs already (see the module docs).
pub fn start(hub: &Rc<Hub>, accounts: Accounts, cx: &mut App) {
    if !hub.start_ticking() {
        return;
    }
    let hub = Rc::clone(hub);
    cx.spawn(async move |cx: &mut AsyncApp| run(&hub, &accounts, cx).await)
        .detach();
}

/// One look at the calendars: what it reads, as of when.
struct Look {
    accounts: Vec<Origin>,
    now: i64,
    /// How far ahead a subscriber looks, in ms.
    ahead: i64,
    at: Instant,
}

/// An account's last read of its calendar from the service: when it started, up to when it reads (ms since the
/// epoch), and whether it still runs.
struct Refresh {
    at: Instant,
    until: i64,
    running: Rc<Cell<bool>>,
}

impl Refresh {
    /// Whether another read is due at `look`: none runs, and the last one is [`REFRESH`] old or reached short of where
    /// a subscriber looks now.
    fn due(&self, look: &Look) -> bool {
        !self.running.get() && (look.at.duration_since(self.at) >= REFRESH || look.now + look.ahead > self.until)
    }
}

async fn run(hub: &Rc<Hub>, accounts: &Accounts, cx: &mut AsyncApp) {
    // each account's last read from the service, by its MRI
    let mut refreshes: HashMap<String, Refresh> = HashMap::new();
    loop {
        let look = cx.update(|cx| {
            if !hub.wants(KIND) {
                hub.stop_ticking();
                return None;
            }
            Some(Look {
                accounts: accounts(cx),
                now: wall_ms(cx),
                ahead: i64::from(hub.longest_lead()) * MINUTE_MS + GRACE_MS,
                at: cx.background_executor().now(),
            })
        });
        let Some(look) = look else { return };
        for account in &look.accounts {
            if refreshes.get(&account.mri).is_none_or(|refresh| refresh.due(&look)) {
                let refresh = refresh(hub, account, &look, cx);
                refreshes.insert(account.mri.clone(), refresh);
            }
            let Ok(events) = account
                .core
                .stored_calendar(look.now - GRACE_MS, look.now + look.ahead)
                .await
            else {
                continue;
            };
            let meetings = meetings(&events);
            cx.update(|cx| hub.meetings(account, &meetings, wall_ms(cx)));
        }
        cx.background_executor().timer(TICK).await;
    }
}

/// `account`'s calendar read from the service as of `look`, on its own task; what it read is told on `hub` once it
/// comes.
fn refresh(hub: &Rc<Hub>, account: &Origin, look: &Look, cx: &mut AsyncApp) -> Refresh {
    let until = look.now + look.ahead + i64::try_from(REFRESH.as_millis()).unwrap_or(0);
    let running = Rc::new(Cell::new(true));
    let read = account.core.calendar(look.now, until);
    let (hub, account, ended) = (Rc::clone(hub), account.clone(), Rc::clone(&running));
    cx.spawn(async move |cx: &mut AsyncApp| {
        // a read that fails leaves what the store kept
        let read = read.await;
        ended.set(false);
        if let Ok(events) = read {
            let meetings = meetings(&events);
            cx.update(|cx| hub.meetings(&account, &meetings, wall_ms(cx)));
        }
    })
    .detach();
    Refresh {
        at: look.at,
        until,
        running,
    }
}

/// The meetings that start among `events`: neither cancelled nor all day.
#[must_use]
pub fn meetings(events: &[CalendarEvent]) -> Vec<Meeting> {
    events
        .iter()
        .filter(|event| !event.cancelled && !event.all_day)
        .map(|event| Meeting {
            id: event.id.clone(),
            subject: event.subject.clone(),
            start: teams_core::calendar::instant(event.start),
            joinable: event.online,
        })
        .collect()
}
