//! The calendar writes (`docs/protocol/calendar.md`, "Writing an event"): an event of the account's own calendar
//! created ([`CreateEvent`]), changed ([`UpdateEvent`]), cancelled ([`CancelEvent`]) or deleted ([`DeleteEvent`]), each a
//! sealed [`Mutating`] request on the middle tier in the shape the web client sent it (`ab-meeting`). Each is a
//! [`WriteKind::Meeting`]: a new event on [`Scope::New`] inviting its attendees, every other on [`Scope::Event`] of the
//! event it names, adding the attendees a change names; each attendee by address ([`Mri::mail`]), as
//! `teams_fence::contacting` reads the same bytes.
//!
//! An event's fields come from an [`EventDraft`] (what the scheduling form edits); a change sends the event as it was
//! read with the draft's fields over it ([`EventDraft::over`]), so a field the form doesn't edit goes back as it came.
//! A Teams meeting's event carries the online meeting [`CreateOnlineMeeting`] made just before (its
//! `schedulingServiceEvent`), as the web client's does.

use serde_json::{Map, Value, json};
use teams_fence::{Mri, Scope, Target, WriteKind};
use time::{OffsetDateTime, UtcOffset};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::endpoint::{Endpoint, Method, Service};
use crate::recurrence::Recurrence;
use crate::request::Request;

/// Someone an event invites: their address, their name, and whether they are required.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EventAttendee {
    pub address: String,
    pub name: Option<String>,
    /// Their MRI, when the picker knew it.
    pub mri: Option<String>,
    /// Required (`true`) or optional.
    pub required: bool,
}

impl EventAttendee {
    /// Its entry in an event's `attendees` (and in the free/busy reads' list of people).
    pub(crate) fn entry(&self) -> Value {
        let mut entry = json!({
            "address": self.address,
            "name": self.name.clone().unwrap_or_else(|| self.address.clone()),
            "role": "Admin",
            "type": if self.required { "Required" } else { "Optional" },
        });
        if let Some(mri) = &self.mri {
            entry["mri"] = json!(mri);
        }
        entry
    }
}

/// How the event shows the account's time (`showAs`), as the form's Show as offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EventShowAs {
    Free,
    WorkingElsewhere,
    Tentative,
    #[default]
    Busy,
    /// Out of office.
    Away,
}

impl EventShowAs {
    /// Every choice, in the order the form lists them.
    pub const ALL: [Self; 5] = [
        Self::Free,
        Self::WorkingElsewhere,
        Self::Tentative,
        Self::Busy,
        Self::Away,
    ];

    /// Its `showAs` value.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::WorkingElsewhere => "WorkingElsewhere",
            Self::Tentative => "Tentative",
            Self::Busy => "Busy",
            Self::Away => "OOF",
        }
    }

    /// The choice a `showAs` value names; `None` for one the form doesn't offer.
    #[must_use]
    pub fn of(wire: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|show| show.wire() == wire)
    }
}

/// An instant with the offset of the time zone it is shown in: how the event's times are written
/// (`2026-10-31T01:30:00+03:00`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ZonedTime {
    /// Ms since the epoch.
    pub ms: i64,
    /// Minutes east of UTC.
    pub offset_minutes: i32,
}

impl ZonedTime {
    /// `2026-10-31T01:30:00+03:00`; `None` out of range.
    #[must_use]
    pub fn iso(self) -> Option<String> {
        let offset = UtcOffset::from_whole_seconds(self.offset_minutes.checked_mul(60)?).ok()?;
        let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(self.ms) * 1_000_000)
            .ok()?
            .to_offset(offset);
        let sign = if self.offset_minutes < 0 { '-' } else { '+' };
        let minutes = self.offset_minutes.unsigned_abs();
        Some(format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
            time.year(),
            u8::from(time.month()),
            time.day(),
            time.hour(),
            time.minute(),
            time.second(),
            minutes / 60,
            minutes % 60
        ))
    }
}

/// The account as the organizer of what it creates: its MRI, and its address and name when known.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Organizer {
    pub mri: String,
    pub address: Option<String>,
    pub name: Option<String>,
}

/// An event as the scheduling form makes it: what a new event's body, or a change's, carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDraft {
    pub subject: String,
    pub start: ZonedTime,
    pub end: ZonedTime,
    /// The account's time zone as Exchange names it (`TurkeySt`), when known; else UTC's.
    pub time_zone: Option<String>,
    pub all_day: bool,
    pub organizer: Organizer,
    pub attendees: Vec<EventAttendee>,
    pub location: String,
    /// Its text, as HTML.
    pub body_html: String,
    /// A Teams meeting.
    pub online: bool,
    pub show_as: EventShowAs,
    /// Minutes before its start the reminder goes off; `None` for no reminder.
    pub reminder_minutes: Option<u32>,
    pub private: bool,
    /// Its categories, by name.
    pub categories: Vec<String>,
    /// How it repeats: a series (its creation makes the series' master), else a single event. A change of an
    /// occurrence leaves it `None` and keeps what the event read has.
    pub recurrence: Option<Recurrence>,
}

/// The label the web client gives a Teams meeting's join.
const JOIN_LABEL: &str = "Join Microsoft Teams Online Meeting";

impl EventDraft {
    /// The event's fields as the body's event carries them.
    fn fields(&self) -> Option<Map<String, Value>> {
        let mut organizer = Map::new();
        if let Some(address) = &self.organizer.address {
            organizer.insert("address".to_owned(), json!(address));
            organizer.insert("mri".to_owned(), json!(self.organizer.mri));
        }
        if let Some(name) = &self.organizer.name {
            organizer.insert("name".to_owned(), json!(name));
        }
        organizer.insert("role".to_owned(), json!("Admin"));
        organizer.insert("type".to_owned(), json!("Organizer"));
        let mut attendees = vec![Value::Object(organizer)];
        attendees.extend(self.attendees.iter().map(EventAttendee::entry));
        let mut fields = json!({
            "attendees": attendees,
            "doNotForward": false,
            "endTime": self.end.iso()?,
            "eventTimeZone": self.time_zone.clone().unwrap_or_else(|| "Utc".to_owned()),
            // what the form's creation names it: a series' master when it repeats (a change keeps the event's own)
            "eventType": if self.recurrence.is_some() { "RecurringMaster" } else { "Single" },
            "isAllDayEvent": self.all_day,
            "isAppointment": self.attendees.is_empty(),
            "isOrganizer": true,
            "isPrivate": self.private,
            "isReminderSet": self.reminder_minutes.is_some(),
            "isResponseRequested": true,
            "location": self.location,
            "primarySpokenLanguage": "en-us",
            "reminderMinutesBeforeStart": self.reminder_minutes.unwrap_or(0),
            "showAs": self.show_as.wire(),
            "startTime": self.start.iso()?,
            "subject": self.subject,
            "utcOffset": if self.time_zone.is_some() { self.start.offset_minutes } else { 0 },
        });
        let object = fields.as_object_mut()?;
        if !self.categories.is_empty() {
            object.insert("categories".to_owned(), json!(self.categories));
        }
        if !self.body_html.trim().is_empty() {
            object.insert("bodyContentType".to_owned(), json!("html"));
            object.insert("bodyContent".to_owned(), json!(self.body_html));
        }
        if let Some(address) = &self.organizer.address {
            object.insert("organizerAddress".to_owned(), json!(address));
        }
        if let Some(name) = &self.organizer.name {
            object.insert("organizerName".to_owned(), json!(name));
        }
        if let Some(recurrence) = &self.recurrence {
            object.insert("eventRecurrencePattern".to_owned(), recurrence.pattern.wire());
            object.insert("eventRecurrenceRange".to_owned(), recurrence.range_wire()?);
        }
        if self.online {
            object.insert("joinMeetingLabel".to_owned(), json!(JOIN_LABEL));
            object.insert("meetingTemplateId".to_owned(), json!("default"));
            let data = json!({"uid": self.organizer.mri, "type": 0, "private": true});
            object.insert("skypeTeamsData".to_owned(), json!(data.to_string()));
        }
        Some(fields.as_object()?.clone())
    }

    /// `event` (an event as it was read) with this draft's fields over it: what a change sends. The meeting's own
    /// fields (its join links, its chat) and the event's kind (`eventType`: an occurrence stays one, a series' master
    /// one) stay as they were read, and so does its recurrence unless the draft sets one (a series changed whole). A
    /// body or categories the draft cleared are written out empty, so the event's own don't stay; without a zone of its
    /// own the draft keeps the event's.
    #[must_use]
    pub fn over(&self, event: &Map<String, Value>) -> Option<Map<String, Value>> {
        let mut merged = event.clone();
        let mut fields = self.fields()?;
        for kept in ["skypeTeamsData", "eventType"] {
            if event.contains_key(kept) {
                fields.remove(kept);
            }
        }
        // without the account's zone the event keeps the one it was read with, rather than going to UTC
        if self.time_zone.is_none() && event.contains_key("eventTimeZone") {
            fields.remove("eventTimeZone");
            fields.remove("utcOffset");
        }
        if event.contains_key("categories") && !fields.contains_key("categories") {
            fields.insert("categories".to_owned(), json!([]));
        }
        if event.contains_key("bodyContent") && !fields.contains_key("bodyContent") {
            fields.insert("bodyContentType".to_owned(), json!("html"));
            fields.insert("bodyContent".to_owned(), json!(""));
        }
        merged.extend(fields);
        Some(merged)
    }

    /// The participants it invites, by address.
    fn invited(&self) -> impl Iterator<Item = Mri> + '_ {
        self.organizer
            .address
            .iter()
            .map(|address| Mri::mail(address))
            .chain(self.attendees.iter().map(|attendee| Mri::mail(&attendee.address)))
    }
}

/// The middle tier's path to the account's events.
const EVENTS: [&str; 4] = ["v2.0", "me", "calendars", "events"];

/// `POST beta/me/calendarEvents/privateMeeting/schedulingService/create` with `{"isStreamEnabled": false,
/// "meetingType": "Scheduled"}`: an online meeting of the account's own (its join link and its chat, the organizer
/// alone in it), made before a Teams meeting's event, which then carries it. It reaches nobody: a plain request. The
/// service answers `201` with `{"value": <the meeting>}` ([`CreateOnlineMeeting::read_answer`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CreateOnlineMeeting;

impl CreateOnlineMeeting {
    /// The meeting the answer `body` gives, as the event's `schedulingServiceEvent` carries it: `None` unless it is an
    /// object with a join link.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Value> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let meeting = answer.get("value")?;
        meeting
            .get("links")
            .and_then(|links| links.get("join"))
            .and_then(Value::as_str)
            .is_some()
            .then(|| meeting.clone())
    }
}

impl Request for CreateOnlineMeeting {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "beta",
            "me",
            "calendarEvents",
            "privateMeeting",
            "schedulingService",
            "create",
        ])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({"isStreamEnabled": false, "meetingType": "Scheduled"}))
    }
}

/// `POST v2.0/me/calendars/default/events?isOnlineMeeting=<bool>&shouldDecryptData=true` with `{"calendarEvent":
/// <event>, "schedulingServiceEvent"?: <online meeting>}`: a new event, its invitation sent to every attendee. The
/// service answers `201` with `{"value": [<event>]}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateEvent {
    draft: EventDraft,
    meeting: Option<Value>,
    transaction: String,
}

impl CreateEvent {
    /// The creation of `draft`, carrying `meeting` ([`CreateOnlineMeeting`]'s) for a Teams meeting.
    #[must_use]
    pub fn new(draft: EventDraft, meeting: Option<Value>) -> Self {
        Self {
            draft,
            meeting,
            transaction: transaction_id(),
        }
    }

    #[must_use]
    pub fn draft(&self) -> &EventDraft {
        &self.draft
    }
}

/// The web client's id of an event it makes (`clientOptimisticEventId`, `TransactionId`): five random 32-bit numbers
/// joined by `-`.
fn transaction_id() -> String {
    let mut parts = Vec::with_capacity(5);
    for _ in 0..5 {
        parts.push(getrandom::u32().unwrap_or_default().to_string());
    }
    parts.join("-")
}

impl Mutating for CreateEvent {
    fn target(&self) -> Target {
        Target::new(WriteKind::Meeting, Scope::New).with_added(self.draft.invited())
    }
}

impl Sealed for CreateEvent {
    fn wire(&self) -> Option<Wire> {
        let mut event = self.draft.fields()?;
        event.insert("clientOptimisticEventId".to_owned(), json!(self.transaction));
        event.insert("TransactionId".to_owned(), json!(self.transaction));
        let mut body = json!({ "calendarEvent": Value::Object(event) });
        if let Some(meeting) = self.meeting.as_ref().filter(|_| self.draft.online) {
            body["schedulingServiceEvent"] = meeting.clone();
        }
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Post,
            endpoint: Endpoint::new(["v2.0", "me", "calendars", "default", "events"])
                .with_query(
                    "isOnlineMeeting",
                    bool_text(self.draft.online && self.meeting.is_some()),
                )
                .with_query("shouldDecryptData", "true"),
            headers: Vec::new(),
            body: Some(body),
        })
    }
}

/// `PUT v2.0/me/calendars/events/<id>?isOnlineMeeting=<bool>` with `{"updatedCalendarEvent": <event>}`: the event
/// changed, its update sent to every attendee. The event is the one read with the draft over it
/// ([`EventDraft::over`]); the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateEvent {
    id: String,
    draft: EventDraft,
    event: Map<String, Value>,
}

impl UpdateEvent {
    /// The change of the event `id`, read as `event`, to `draft`; `None` when `event` names another id.
    #[must_use]
    pub fn new(id: String, event: Map<String, Value>, draft: EventDraft) -> Option<Self> {
        match event.get("objectId") {
            None | Some(Value::Null) => {}
            Some(Value::String(object_id)) if *object_id == id => {}
            Some(_) => return None,
        }
        (!id.is_empty()).then_some(Self { id, draft, event })
    }

    /// Whether the event as read was a Teams meeting: a change keeps it one.
    fn online(&self) -> bool {
        self.event
            .get("isOnlineMeeting")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || self.draft.online
    }
}

impl Mutating for UpdateEvent {
    fn target(&self) -> Target {
        Target::new(WriteKind::Meeting, Scope::Event { id: self.id.clone() }).with_added(self.draft.invited())
    }
}

impl Sealed for UpdateEvent {
    fn wire(&self) -> Option<Wire> {
        let event = self.draft.over(&self.event)?;
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str()]))
                .with_query("isOnlineMeeting", bool_text(self.online())),
            headers: Vec::new(),
            body: Some(json!({ "updatedCalendarEvent": Value::Object(event) })),
        })
    }
}

/// `PUT v2.0/me/calendars/events/<id>/cancel` with `{"conversationId"?: <meeting chat>, "cancellationMessage"?:
/// <text>}`: an organizer's meeting cancelled, its attendees told; the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelEvent {
    id: String,
    chat: Option<String>,
    message: Option<String>,
}

impl CancelEvent {
    /// The cancellation of the event `id`, naming its meeting chat when it has one, with `message` for its attendees.
    #[must_use]
    pub fn new(id: String, chat: Option<String>, message: Option<String>) -> Option<Self> {
        (!id.is_empty()).then_some(Self { id, chat, message })
    }
}

impl Mutating for CancelEvent {
    fn target(&self) -> Target {
        Target::new(WriteKind::Meeting, Scope::Event { id: self.id.clone() })
    }
}

impl Sealed for CancelEvent {
    fn wire(&self) -> Option<Wire> {
        let mut body = Map::new();
        if let Some(chat) = &self.chat {
            body.insert("conversationId".to_owned(), Value::String(chat.clone()));
        }
        if let Some(message) = self.message.as_ref().filter(|message| !message.trim().is_empty()) {
            body.insert("cancellationMessage".to_owned(), Value::String(message.clone()));
        }
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str(), "cancel"])),
            headers: Vec::new(),
            body: Some(Value::Object(body)),
        })
    }
}

/// `DELETE v2.0/me/calendars/events/<id>`: the event deleted from the account's calendar (an appointment, or a meeting
/// the account doesn't organize); the service answers `204`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeleteEvent {
    id: String,
}

impl DeleteEvent {
    #[must_use]
    pub fn new(id: String) -> Option<Self> {
        (!id.is_empty()).then_some(Self { id })
    }
}

impl Mutating for DeleteEvent {
    fn target(&self) -> Target {
        Target::new(WriteKind::Meeting, Scope::Event { id: self.id.clone() })
    }
}

impl Sealed for DeleteEvent {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method: Method::Delete,
            endpoint: Endpoint::new(EVENTS.into_iter().chain([self.id.as_str()])),
            headers: Vec::new(),
            body: None,
        })
    }
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use teams_fence::{Contact, Policy, Request as FenceRequest, contacting};

    use super::*;

    const BASE: &str = "https://teams.microsoft.com/api/mt/part/emea-02";

    fn draft() -> EventDraft {
        EventDraft {
            subject: "Planning".to_owned(),
            start: ZonedTime {
                ms: 1_793_372_400_000,
                offset_minutes: 180,
            },
            end: ZonedTime {
                ms: 1_793_374_200_000,
                offset_minutes: 180,
            },
            time_zone: Some("TurkeySt".to_owned()),
            all_day: false,
            organizer: Organizer {
                mri: "8:orgid:00000000-0000-4000-8000-00000000000a".to_owned(),
                address: Some("bot-a@lab.example.com".to_owned()),
                name: Some("Bot A".to_owned()),
            },
            attendees: vec![EventAttendee {
                address: "bot-b@lab.example.com".to_owned(),
                name: Some("Bot B".to_owned()),
                mri: None,
                required: true,
            }],
            location: String::new(),
            body_html: String::new(),
            online: true,
            show_as: EventShowAs::Busy,
            reminder_minutes: Some(15),
            private: false,
            categories: Vec::new(),
            recurrence: None,
        }
    }

    /// What the fence makes of the bytes `request` sends, on a made-up middle-tier base.
    fn classified<R: Mutating>(request: &R) -> Contact {
        let wire = request.wire().expect("a wire form");
        let mut url = url::Url::parse(&format!("{BASE}/")).expect("the base");
        url.path_segments_mut()
            .expect("a base")
            .pop_if_empty()
            .extend(wire.endpoint().segments());
        if !wire.endpoint().query().is_empty() {
            url.query_pairs_mut().extend_pairs(wire.endpoint().query());
        }
        let body = wire.body().map(|body| body.to_string().into_bytes());
        contacting(
            &FenceRequest::new(wire.method().as_str(), url.as_str()).with_body(body.as_deref()),
            &Policy::new(),
        )
    }

    fn object(value: &Value) -> Map<String, Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn each_write_s_target_is_what_the_fence_reads_from_its_bytes() {
        let meeting = json!({"links": {"join": "https://teams.microsoft.com/l/meetup-join/x"}});
        let create = CreateEvent::new(draft(), Some(meeting));
        assert_eq!(classified(&create), Contact::Write(create.target()));
        let event = object(&json!({"objectId": "AAMkEvent", "subject": "Old", "iCalUId": "kept"}));
        let update = UpdateEvent::new("AAMkEvent".to_owned(), event, draft()).expect("an update");
        assert_eq!(classified(&update), Contact::Write(update.target()));
        let cancel = CancelEvent::new("AAMkEvent".to_owned(), Some("19:meeting_x@thread.v2".to_owned()), None)
            .expect("a cancellation");
        assert_eq!(classified(&cancel), Contact::Write(cancel.target()));
        let delete = DeleteEvent::new("AAMkEvent".to_owned()).expect("a delete");
        assert_eq!(classified(&delete), Contact::Write(delete.target()));
    }

    #[test]
    fn a_new_event_is_written_as_the_web_client_writes_it() {
        let create = CreateEvent::new(draft(), None);
        let wire = create.wire().expect("a wire form");
        let event = &wire.body().expect("a body")["calendarEvent"];
        assert_eq!(event["startTime"], "2026-10-30T18:00:00+03:00");
        assert_eq!(event["eventTimeZone"], "TurkeySt");
        assert_eq!(event["utcOffset"], 180);
        assert_eq!(event["attendees"][0]["type"], "Organizer");
        assert_eq!(event["attendees"][1]["type"], "Required");
        assert_eq!(event["isAppointment"], false);
        assert_eq!(event["clientOptimisticEventId"], event["TransactionId"]);
        // a Teams meeting without its online meeting goes as an event alone
        assert_eq!(
            wire.endpoint().query()[0],
            ("isOnlineMeeting".to_owned(), "false".to_owned())
        );
    }

    #[test]
    fn a_series_is_created_as_its_master_with_its_recurrence() {
        use crate::recurrence::{Pattern, Weekday};
        let mut series = draft();
        series.recurrence = Some(Recurrence {
            pattern: Pattern::Weekly {
                interval: 1,
                days: vec![Weekday::Friday],
            },
            start: ZonedTime {
                ms: 1_793_307_600_000,
                offset_minutes: 180,
            },
            end: None,
        });
        let create = CreateEvent::new(series.clone(), None);
        let wire = create.wire().expect("a wire form");
        let event = &wire.body().expect("a body")["calendarEvent"];
        assert_eq!(event["eventType"], "RecurringMaster");
        assert_eq!(event["eventRecurrencePattern"]["patternType"], 2);
        assert_eq!(event["eventRecurrenceRange"]["startDate"], "2026-10-30T00:00:00+03:00");
        assert_eq!(classified(&create), Contact::Write(create.target()));
        // a change of one occurrence keeps its kind and the series' recurrence as read
        let occurrence = object(&json!({"objectId": "AAMkOne", "eventType": "Occurrence",
            "eventRecurrencePattern": {"patternType": 1, "daily": {"interval": 1}}}));
        let merged = draft().over(&occurrence).expect("merged");
        assert_eq!(merged["eventType"], "Occurrence");
        assert_eq!(merged["eventRecurrencePattern"]["patternType"], 1);
        let master = object(&json!({"objectId": "AAMkSeries", "eventType": "RecurringMaster"}));
        let merged = series.over(&master).expect("merged");
        assert_eq!(merged["eventType"], "RecurringMaster");
        assert_eq!(merged["eventRecurrencePattern"]["weekly"]["daysOfTheWeek"], json!([5]));
    }

    #[test]
    fn a_change_keeps_what_the_form_doesn_t_edit() {
        let event = object(&json!({"objectId": "AAMkEvent", "subject": "Old", "iCalUId": "kept",
            "skypeTeamsData": "{\"cid\":\"19:meeting_x@thread.v2\"}"}));
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["iCalUId"], "kept");
        assert_eq!(merged["subject"], "Planning");
        assert_eq!(merged["skypeTeamsData"], "{\"cid\":\"19:meeting_x@thread.v2\"}");
        let other = object(&json!({"objectId": "Another"}));
        assert!(UpdateEvent::new("AAMkEvent".to_owned(), other, draft()).is_none());
    }

    #[test]
    fn a_change_clears_a_body_and_categories_the_form_emptied() {
        let event = object(&json!({"objectId": "AAMkEvent", "bodyContentType": "html",
            "bodyContent": "<p>Old notes</p>", "categories": ["Red category"]}));
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["bodyContent"], "");
        assert_eq!(merged["categories"], json!([]));
        // kept as the form has them when it has them
        let mut filled = draft();
        filled.body_html = "<p>New notes</p>".to_owned();
        filled.categories = vec!["Blue category".to_owned()];
        let merged = filled.over(&event).expect("merged");
        assert_eq!(merged["bodyContent"], "<p>New notes</p>");
        assert_eq!(merged["categories"], json!(["Blue category"]));
        // an event without them gets none
        let bare = object(&json!({"objectId": "AAMkEvent"}));
        let merged = draft().over(&bare).expect("merged");
        assert!(!merged.contains_key("bodyContent"));
        assert!(!merged.contains_key("categories"));
    }

    #[test]
    fn a_change_without_the_account_s_zone_keeps_the_event_s() {
        let event = object(&json!({"objectId": "AAMkEvent", "eventTimeZone": "TurkeySt", "utcOffset": 180}));
        let mut unzoned = draft();
        unzoned.time_zone = None;
        let merged = unzoned.over(&event).expect("merged");
        assert_eq!(merged["eventTimeZone"], "TurkeySt");
        assert_eq!(merged["utcOffset"], 180);
        let merged = draft().over(&event).expect("merged");
        assert_eq!(merged["eventTimeZone"], "TurkeySt");
    }

    #[test]
    fn the_online_meeting_s_answer_is_its_value() {
        let body = br#"{"value": {"links": {"join": "https://teams.microsoft.com/l/meetup-join/x"}, "etag": "e"}}"#;
        assert_eq!(
            CreateOnlineMeeting::read_answer(body).map(|meeting| meeting["etag"].clone()),
            Some(json!("e"))
        );
        assert!(CreateOnlineMeeting::read_answer(br#"{"value": {}}"#).is_none());
    }
}
