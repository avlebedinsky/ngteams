//! The account's calendar: read when a view shows a window of it (the calendar's day, week or month), and written
//! when the scheduling form saves an event or its card cancels or deletes one (`docs/protocol/calendar.md`).
//!
//! - **Reads.** The middle tier's calendar view ([`CalendarView`]), one read per ask, answered with the window's
//!   events; nothing is kept: the view asks again as it moves and refreshes. A window that is empty, reversed or wider
//!   than [`teams_proto::MAX_CALENDAR_WINDOW_MS`] is answered with no events and nothing read. A read that failed, or an
//!   answer that isn't in the shape the web client reads, is [`EngineError::CalendarUnread`]. One event is read whole
//!   ([`EventRead`]) before the form opens on it (a series' master too, when the form changes a whole series). The
//!   form's Scheduler reads the attendees' busy times and the suggested times ([`MeetingAvailability`]): two reads sent as a
//!   `PUT`, which reach nobody.
//! - **Writes** ([`EventWrite`]): an event created, changed, cancelled or deleted, each a sealed `teams-proto` write
//!   through the outbox's calendar kind ([`Outbox::try_calendar`]): every attempt under a fresh `Fence::check` with its
//!   `fence.log` line, no row, nothing sent again by a later run; one more attempt only after a 401 or a short
//!   throttle ([`CallRetries`]), never after an unknown outcome (an invitation sent twice is worse than a failed save).
//!   A write that wasn't answered with a 2xx is [`EngineError::EventNotSaved`], saying why.

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Map, Value};
use teams_fence::Mri;
use teams_proto::{
    Author, CalendarEvent, CalendarView, CancelEvent, Client, CreateEvent, CreateOnlineMeeting, Credentials,
    DeleteEvent, EventAttendee, EventDraft, EventRead, FreeBusyAsk, FullEvent, HttpError, HttpTransport, MeetingTimes,
    Mutating, Organizer, PersonSchedule, Schedules, ShortProfileLookup, TimeZoneSettings, Transport, UpdateEvent,
};

use crate::engine::{EngineError, Responder};
use crate::outbox::{CallRetries, CallTried, Outbox};

/// What the user asks of an event of the account's calendar.
#[derive(Clone, Debug, PartialEq)]
pub enum EventWrite {
    /// A new event: its invitation goes to every attendee.
    Create(EventDraft),
    /// The event `id`, read whole as `event` ([`FullEvent::raw`]), changed to `draft`: its update goes to every
    /// attendee.
    Update {
        id: String,
        event: Map<String, Value>,
        draft: EventDraft,
    },
    /// The organizer's meeting `id` cancelled (its chat named when it has one), `message` sent with the cancellation.
    Cancel {
        id: String,
        chat: Option<String>,
        message: Option<String>,
    },
    /// The event `id` deleted from the account's calendar.
    Delete { id: String },
}

impl EventWrite {
    /// What it is, for the log and the errors.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Create(_) => "creation",
            Self::Update { .. } => "change",
            Self::Cancel { .. } => "cancellation",
            Self::Delete { .. } => "delete",
        }
    }
}

/// Someone the attendee picker suggests: a name the account knows and the address their profile gives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitable {
    pub mri: String,
    pub name: String,
    pub address: String,
}

/// What the scheduling form's Scheduler shows ([`Calendar::availability`]): each person's busy times (the account's
/// own among them) and the starts every attendee is free for the meeting's length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingAvailability {
    pub people: Vec<PersonSchedule>,
    /// Suggested starts, ms since the epoch, in the service's order.
    pub suggestions: Vec<i64>,
}

/// What a write gave: the new event, as the service answered its creation (`None` for any other write, or an answer
/// that didn't read).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventWritten {
    pub created: Option<CalendarEvent>,
}

pub(crate) struct Calendar<C, T> {
    client: Arc<Client<HttpTransport<C>>>,
    outbox: Arc<Outbox<T>>,
    /// The account as an event's organizer, its address and its time zone's name once read ([`Whom`]).
    whom: Arc<Mutex<Option<Whom>>>,
}

/// What an event the account writes says of the account: its address (its profile's) and its time zone as Exchange
/// names it, each read once a run.
#[derive(Clone, Debug, Default)]
struct Whom {
    address: Option<String>,
    time_zone: Option<String>,
}

impl<C, T> Calendar<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>, outbox: Arc<Outbox<T>>) -> Self {
        Self {
            client,
            outbox,
            whom: Arc::new(Mutex::new(None)),
        }
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

    /// Answers `reply` with the event `id`, read whole.
    pub(crate) fn event(&self, id: &str, reply: Responder<FullEvent>) {
        let Some(read) = EventRead::new(id) else {
            let _ = reply.send(Err(EngineError::CalendarUnread));
            return;
        };
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let answer = match client.read(&read).await {
                Ok(answer) => read.read_answer(answer.body()).map_err(|error| {
                    log::info!("the event's answer wasn't taken: {error}");
                    EngineError::CalendarUnread
                }),
                Err(error) => {
                    log::info!("the event couldn't be read: {error}");
                    Err(EngineError::CalendarUnread)
                }
            };
            let _ = reply.send(answer);
        });
    }

    /// Answers `reply` with the addresses of `named` (stored people as (MRI, name)), from their profiles read now
    /// ([`ShortProfileLookup`], a read by `POST`); one without an address is left out.
    pub(crate) fn invitable(&self, named: Vec<(String, String)>, reply: Responder<Vec<Invitable>>) {
        let client = Arc::clone(&self.client);
        tokio::task::spawn_local(async move {
            let lookup = ShortProfileLookup::new(named.iter().map(|(mri, _)| Mri::new(mri.as_str())));
            let profiles = match lookup {
                Some(lookup) if client.transport().has_middle_tier() => match client.lookup(lookup.clone()).await {
                    Ok(answer) => lookup.read_answer(answer.body()).unwrap_or_default(),
                    Err(error) => {
                        log::info!("the attendee picker's profiles couldn't be read: {error}");
                        Vec::new()
                    }
                },
                _ => Vec::new(),
            };
            let found = named
                .into_iter()
                .filter_map(|(mri, name)| {
                    let profile = profiles.iter().find(|profile| profile.mri.as_str() == mri)?;
                    let address = profile
                        .email
                        .clone()
                        .or_else(|| profile.user_principal_name.clone())
                        .filter(|address| address.contains('@'))?;
                    Some(Invitable { mri, name, address })
                })
                .collect();
            let _ = reply.send(Ok(found));
        });
    }

    /// Answers `reply` with the busy times of `attendees` and the account from `start_ms` (the local midnight of the
    /// day the form shows) and the starts all of them are free for `meeting_minutes`: the scheduling form's two
    /// free/busy reads ([`Schedules`], [`MeetingTimes`]), plain requests that reach nobody, sent together. One that
    /// failed leaves its part empty; both failing is [`EngineError::CalendarUnread`].
    pub(crate) fn availability(
        &self,
        attendees: Vec<EventAttendee>,
        start_ms: i64,
        meeting_minutes: u32,
        reply: Responder<MeetingAvailability>,
    ) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        tokio::task::spawn_local(async move {
            let me = outbox.me().clone();
            let known = known(&client, &whom, &me).await;
            let ask = FreeBusyAsk {
                attendees,
                organizer: organizer(&me, &known),
                start_ms,
                meeting_minutes: meeting_minutes.max(1),
            };
            let schedules = Schedules(ask.clone());
            let times = MeetingTimes(ask);
            let (schedules, times) = tokio::join!(client.send(&schedules), client.send(&times));
            let people = match schedules {
                Ok(answer) => Schedules::read_answer(answer.body()),
                Err(error) => {
                    log::info!("the attendees' busy times couldn't be read: {error}");
                    None
                }
            };
            let suggestions = match times {
                Ok(answer) => MeetingTimes::read_answer(answer.body()),
                Err(error) => {
                    log::info!("the suggested times couldn't be read: {error}");
                    None
                }
            };
            let answer = if people.is_none() && suggestions.is_none() {
                Err(EngineError::CalendarUnread)
            } else {
                Ok(MeetingAvailability {
                    people: people.unwrap_or_default(),
                    suggestions: suggestions.unwrap_or_default(),
                })
            };
            let _ = reply.send(answer);
        });
    }

    /// Sends `write` (see the module docs) and answers `reply` with what it gave.
    pub(crate) fn write(&self, write: EventWrite, reply: Responder<EventWritten>) {
        let outbox = Arc::clone(&self.outbox);
        let client = Arc::clone(&self.client);
        let whom = Arc::clone(&self.whom);
        tokio::task::spawn_local(async move {
            let kind = write.kind();
            let me = outbox.me().clone();
            let write = match write {
                EventWrite::Create(draft) => EventWrite::Create(complete(&client, &whom, &me, draft).await),
                EventWrite::Update { id, event, draft } => EventWrite::Update {
                    id,
                    event,
                    draft: complete(&client, &whom, &me, draft).await,
                },
                other => other,
            };
            let answer = match write {
                EventWrite::Create(draft) => {
                    let meeting = if draft.online {
                        online_meeting(&client).await
                    } else {
                        None
                    };
                    if draft.online && meeting.is_none() {
                        Err(not_saved(kind, "the Teams meeting couldn't be made"))
                    } else {
                        send(&outbox, &CreateEvent::new(draft, meeting), kind).await
                    }
                }
                EventWrite::Update { id, event, draft } => match UpdateEvent::new(id, event, draft) {
                    Some(update) => send(&outbox, &update, kind).await,
                    None => Err(not_saved(kind, "the event read isn't the one changed")),
                },
                EventWrite::Cancel { id, chat, message } => match CancelEvent::new(id, chat, message) {
                    Some(cancel) => send(&outbox, &cancel, kind).await,
                    None => Err(not_saved(kind, "no event")),
                },
                EventWrite::Delete { id } => match DeleteEvent::new(id) {
                    Some(delete) => send(&outbox, &delete, kind).await,
                    None => Err(not_saved(kind, "no event")),
                },
            };
            let _ = reply.send(answer.map(|body| EventWritten {
                created: (kind == "creation").then(|| CalendarEvent::created(&body)).flatten(),
            }));
        });
    }
}

/// `draft` with the account as its organizer and its time zone, as read once a run ([`Whom`]).
async fn complete<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    whom: &Mutex<Option<Whom>>,
    me: &Author,
    mut draft: EventDraft,
) -> EventDraft {
    let known = known(client, whom, me).await;
    draft.organizer = organizer(me, &known);
    draft.time_zone = known.time_zone;
    draft
}

/// What the account's events say of it ([`Whom`]), read the first time it is asked for.
async fn known<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    whom: &Mutex<Option<Whom>>,
    me: &Author,
) -> Whom {
    let known = whom.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(known) = known {
        return known;
    }
    let read = Whom {
        address: own_address(client, me.mri()).await,
        time_zone: time_zone(client).await,
    };
    *whom.lock().unwrap_or_else(PoisonError::into_inner) = Some(read.clone());
    read
}

/// The account as the organizer: its MRI, its address and its name.
fn organizer(me: &Author, known: &Whom) -> Organizer {
    Organizer {
        mri: me.mri().as_str().to_owned(),
        address: known.address.clone(),
        name: Some(me.display_name().to_owned()).filter(|name| !name.trim().is_empty()),
    }
}

/// The account's address, as its profile gives it.
async fn own_address<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, me: &Mri) -> Option<String> {
    if !client.transport().has_middle_tier() {
        return None;
    }
    let lookup = ShortProfileLookup::new([me.clone()])?;
    let answer = client.lookup(lookup.clone()).await.ok()?;
    let profile = lookup
        .read_answer(answer.body())
        .ok()?
        .into_iter()
        .find(|profile| profile.mri == *me)?;
    profile
        .email
        .or(profile.user_principal_name)
        .filter(|address| address.contains('@'))
}

/// The account's time zone as Exchange names it (`TurkeySt`), from the middle tier's reading of the desktop's
/// ([`TimeZoneSettings`]).
async fn time_zone<C: Credentials + 'static>(client: &Client<HttpTransport<C>>) -> Option<String> {
    let read = TimeZoneSettings::new(&local_zone()?);
    let answer = client.read(&read).await.ok()?;
    TimeZoneSettings::read_answer(answer.body())
}

/// The desktop's time zone by its IANA name: `TZ`, else where `/etc/localtime` links to.
fn local_zone() -> Option<String> {
    if let Ok(zone) = std::env::var("TZ") {
        let zone = zone.trim_start_matches(':').trim();
        if !zone.is_empty() && !zone.starts_with('/') {
            return Some(zone.to_owned());
        }
    }
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_string_lossy();
    target.split_once("zoneinfo/").map(|(_, zone)| zone.to_owned())
}

/// A Teams meeting's online meeting, made now ([`CreateOnlineMeeting`]): what its event carries.
async fn online_meeting<C: Credentials + 'static>(client: &Client<HttpTransport<C>>) -> Option<Value> {
    let answer = match client.transport().send(&CreateOnlineMeeting).await {
        Ok(answer) => answer,
        Err(error) => {
            log::info!("the Teams meeting couldn't be made: {error}");
            return None;
        }
    };
    CreateOnlineMeeting::read_answer(answer.body())
}

/// Sends `write` until it is answered or no further attempt may follow ([`CallRetries`]); the answer's body on a 2xx.
async fn send<T, R>(outbox: &Outbox<T>, write: &R, kind: &'static str) -> Result<Vec<u8>, EngineError>
where
    T: Transport<Error = HttpError>,
    R: Mutating + Clone + Send,
{
    let mut retries = CallRetries::default();
    loop {
        let tried = outbox.try_calendar(write.clone()).await;
        if let Some(wait) = retries.again(&tried) {
            tokio::time::sleep(wait).await;
            continue;
        }
        return match tried {
            CallTried::Answered(answer) if (200..300).contains(&answer.status()) => Ok(answer.body().to_vec()),
            CallTried::Answered(answer) => Err(not_saved(kind, &format!("the service answered {}", answer.status()))),
            CallTried::Denied(reason) => Err(not_saved(kind, &format!("refused: {reason}"))),
            CallTried::NotSent(why) | CallTried::Uncertain(why) => Err(not_saved(kind, &why)),
            CallTried::Throttled(_) => Err(not_saved(kind, "the service is busy")),
        };
    }
}

fn not_saved(kind: &'static str, why: &str) -> EngineError {
    log::info!("the event's {kind} wasn't saved: {why}");
    EngineError::EventNotSaved {
        kind,
        why: why.to_owned(),
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
