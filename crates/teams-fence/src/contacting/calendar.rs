//! The middle tier's calendar rows (`docs/protocol/calendar.md`, "Writing an event"): an event of the account's own
//! calendar created, changed, cancelled or deleted, each a [`WriteKind::Meeting`] judged by the event's attendees, the
//! empty online meeting the web client makes before it creates a Teams meeting, and a meeting's options saved by its
//! organizer naming nobody (who bypasses the lobby, who presents: `docs/protocol/calling.md`, "The lobby"), which
//! reach nobody. A channel's meeting, created on its team's group calendar or cancelled there
//! (`docs/protocol/calling.md`, "Schedule a meeting"), reaches everyone on the team: a [`WriteKind::Meeting`] on
//! [`Scope::TeamCalendar`], judged by the team's members.
//!
//! Every row reads the middle tier's path after its partition (`/api/mt/part/<partition>/…`), the query keys exactly
//! and the body strictly ([`strict_json`]); anything else on the middle tier's calendar paths stays
//! [`Contact::Unreadable`]. An attendee is judged by its mail address ([`Mri::mail`]): Exchange sends the invitation to
//! the address, whatever MRI the web client put beside it.

use serde_json::{Map, Value};

use super::json::{optional, strict_json};
use super::{Contact, Parsed, decoded_segments};
use crate::{Mri, Scope, Target, WriteKind};

/// The keys of the empty online meeting's body (`schedulingService/create`), as the web bundle builds it.
const SCHEDULING_KEYS: [&str; 8] = [
    "isStreamEnabled",
    "meetingType",
    "subject",
    "startTime",
    "endTime",
    "threadId",
    "meetingTemplateId",
    "useLiveEventAuthorizationV2",
];

/// The scheduling form's reads of its attendees' free and busy times and working hours (`v2.0/me/calendars/<read>`),
/// which the web client sends as a `PUT` with the people and the window in the body.
const FREE_BUSY_READS: [&str; 3] = ["findMeetingTimes", "schedules", "attendeeWorkHours"];

/// The keys of a cancellation's body: the meeting's chat and the note sent with the cancellation.
const CANCEL_KEYS: [&str; 2] = ["conversationId", "cancellationMessage"];

/// An event's fields that would reach people other than its attendees (a team's members, a chat's, a broadcast's
/// audience): a body that sets any of them to more than nothing is unreadable.
const OTHER_AUDIENCES: [&str; 5] = [
    "inviteAllMembers",
    "parentConversationId",
    "originalGroupChatThreadId",
    "broadcastSettings",
    "conflictingMeetings",
];

/// The calendar row a mutating middle tier request matches, if any: `Some(contact)` decides it, `None` leaves it to
/// the middle tier's general rule.
pub(super) fn classify(request: &Parsed<'_>) -> Option<Contact> {
    let segments = decoded_segments(request.url)?;
    let rest = after_partition(&segments)?;
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    let contact = match (request.method.as_str(), rest.as_slice()) {
        ("POST", ["v2.0", "me", "calendars", "default", "events"]) => create(request),
        ("POST", ["v2.0", "groups", group, "calendars", "default", "events"]) => team_event(request, group),
        ("PUT", ["v2.0", "groups", group, "calendars", "events", id, "cancel"]) => team_cancel(request, group, id),
        ("PUT", ["v2.0", "me", "calendars", "events", id]) => update(request, id),
        ("PUT", ["v2.0", "me", "calendars", "events", id, "cancel"]) => cancel(request, id),
        ("DELETE", ["v2.0", "me", "calendars", "events", id]) => delete(request, id),
        (
            "POST",
            [
                "beta",
                "me",
                "calendarEvents",
                "privateMeeting",
                "schedulingService",
                "create",
            ],
        ) => empty_online_meeting(request),
        (
            "POST",
            [
                "beta",
                "meetings",
                "v1",
                "options",
                tenant,
                organizer,
                thread,
                message,
                rest @ ..,
            ],
        ) if rest.iter().all(|segment| segment.is_empty()) => {
            meeting_options(request, [tenant, organizer, thread, message])
        }
        // the scheduling form's free/busy reads, sent as a `PUT` naming the people asked about: reads, whoever they name
        ("PUT" | "POST", ["v2.0", "me", "calendars", read]) if FREE_BUSY_READS.contains(read) => Some(Contact::None),
        _ => return None,
    };
    Some(contact.unwrap_or(Contact::Unreadable))
}

/// `POST beta/meetings/v1/options/<tenant>/<organizer>/<meeting chat>/<message>/?monarch=true` with `{options: [...]}`:
/// the meeting options page's Save (seen 2026-11-02, `meeting-steps`), every option with its value. It reaches nobody
/// while it picks nobody (co-organizers, presenters, producers: [`names_nobody`]); one that picks people, names another
/// key, or names its meeting in any other form is unreadable.
fn meeting_options(request: &Parsed<'_>, [tenant, organizer, thread, message]: [&str; 4]) -> Option<Contact> {
    let query_is_monarch = request.only_query("monarch") == Some("true");
    let guid = |id: &str| id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    // the chat's id with its colon written as an underscore, as the page writes it
    let meeting_chat = thread.starts_with("19_meeting_") && thread.ends_with("@thread.v2");
    if !query_is_monarch || !guid(tenant) || !guid(organizer) || !meeting_chat || message != "0" {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &["options"])?;
    let options = optional(&body, "options").ok()??;
    (options.is_array() && names_nobody(Some(options))).then_some(Contact::None)
}

/// The segments after `api`, `mt`, `part` and the partition.
fn after_partition(segments: &[String]) -> Option<&[String]> {
    match segments {
        [api, mt, part, _partition, rest @ ..] if api == "api" && mt == "mt" && part == "part" => Some(rest),
        _ => None,
    }
}

/// Whether the query has exactly `keys`, once each, each a boolean.
fn boolean_query(request: &Parsed<'_>, keys: &[&str]) -> bool {
    request.query.len() == keys.len()
        && !request.has_duplicate_key()
        && keys.iter().all(|key| {
            request
                .query
                .iter()
                .any(|(name, value)| name == key && matches!(value.as_str(), "true" | "false"))
        })
}

/// `POST v2.0/me/calendars/default/events?isOnlineMeeting=<bool>&shouldDecryptData=<bool>` with `{calendarEvent,
/// schedulingServiceEvent?, meetingOptions?}`: a new event, inviting its attendees.
fn create(request: &Parsed<'_>) -> Option<Contact> {
    if !boolean_query(request, &["isOnlineMeeting", "shouldDecryptData"]) {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &["calendarEvent", "schedulingServiceEvent", "meetingOptions"])?;
    let event = optional(&body, "calendarEvent").ok()??.as_object()?;
    match optional(&body, "schedulingServiceEvent").ok()? {
        None | Some(Value::Null | Value::Object(_)) => {}
        Some(_) => return None,
    }
    if !names_nobody(optional(&body, "meetingOptions").ok()?) {
        return None;
    }
    let attendees = attendees(event)?;
    Some(Contact::Write(
        Target::new(WriteKind::Meeting, Scope::New).with_added(attendees),
    ))
}

/// `POST v2.0/groups/smtp:<team mailbox>/calendars/default/events?isOnlineMeeting=<bool>&shouldDecryptData=<bool>` with
/// the event's keys at the top level: a channel's meeting, as the channel's Schedule a meeting sends it (seen 2026-11-03,
/// `docs/protocol/calling.md`), posted to the team's group calendar, which reaches everyone on the team. A
/// [`WriteKind::Meeting`] on [`Scope::TeamCalendar`]: the channel its `skypeTeamsData` names (`cid`, a team's or
/// channel's thread) and the mailbox the path names, which `mailboxAddress` must name again; any attendee other than
/// the organizer and the team's mailbox is added. Its "Send personal invites" (`inviteAllMembers`, a boolean) puts the
/// meeting in the personal calendars of the channel's members too: the same people the team's calendar reaches, so the
/// same target. One naming its group by anything but its mail address, a channel the fence can't read as a team's
/// thread, or anyone else's audience ([`OTHER_AUDIENCES`]) is unreadable.
fn team_event(request: &Parsed<'_>, group: &str) -> Option<Contact> {
    let mailbox = group.strip_prefix("smtp:").filter(|address| is_address(address))?;
    if !boolean_query(request, &["isOnlineMeeting", "shouldDecryptData"]) {
        return None;
    }
    let mut event = object_body(request)?;
    match optional(&event, "inviteAllMembers").ok()? {
        None | Some(Value::Bool(_)) => {}
        Some(_) => return None,
    }
    // the channel's members are the team's: no audience beyond the target's
    event.remove("inviteAllMembers");
    // the personal calendar's wrappers have no place here: the event's keys are the body's own
    for wrapper in ["calendarEvent", "schedulingServiceEvent", "meetingOptions"] {
        if event.contains_key(wrapper) {
            return None;
        }
    }
    match optional(&event, "mailboxAddress").ok()? {
        Some(Value::String(named)) if named.eq_ignore_ascii_case(mailbox) => {}
        _ => return None,
    }
    let channel = team_channel(&event)?;
    let team = Mri::mail(mailbox);
    let added: Vec<Mri> = attendees(&event)?.into_iter().filter(|mri| *mri != team).collect();
    Some(Contact::Write(
        Target::new(
            WriteKind::Meeting,
            Scope::TeamCalendar {
                channel,
                mailbox: mailbox.to_ascii_lowercase(),
            },
        )
        .with_added(added),
    ))
}

/// The keys of a channel meeting's cancellation (seen 2026-11-03, held back): its channel and post, the line the channel
/// shows, how the event is found, and the note for everyone.
const TEAM_CANCEL_KEYS: [&str; 6] = [
    "conversationId",
    "messageId",
    "rootMessageId",
    "cancellationMessageFormat",
    "fetchEventContract",
    "cancellationMessage",
];

/// `PUT v2.0/groups/smtp:<team mailbox>/calendars/events/<id>/cancel` with `{conversationId, messageId, rootMessageId,
/// cancellationMessageFormat, fetchEventContract, cancellationMessage?}`: a channel's meeting cancelled by its organizer
/// (its event on the team's calendar, told to everyone on the team, its channel's post marked). A
/// [`WriteKind::Meeting`] on [`Scope::TeamCalendar`]: the channel `conversationId` names (a team's or channel's thread)
/// and the mailbox the path names. Any other key, query or form is unreadable.
fn team_cancel(request: &Parsed<'_>, group: &str, id: &str) -> Option<Contact> {
    let mailbox = group.strip_prefix("smtp:").filter(|address| is_address(address))?;
    if id.is_empty() || !request.query.is_empty() {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &TEAM_CANCEL_KEYS)?;
    let channel = match optional(&body, "conversationId").ok()? {
        Some(Value::String(channel)) if crate::membership::is_team_thread_id(channel) => channel.clone(),
        _ => return None,
    };
    for key in [
        "messageId",
        "rootMessageId",
        "cancellationMessageFormat",
        "cancellationMessage",
    ] {
        match optional(&body, key).ok()? {
            None | Some(Value::Null | Value::String(_)) => {}
            Some(_) => return None,
        }
    }
    match optional(&body, "fetchEventContract").ok()? {
        None | Some(Value::Null) => {}
        Some(Value::Object(contract)) => only_keys(contract, &["eventType", "eventTimeZone", "startTime"])?,
        Some(_) => return None,
    }
    Some(Contact::Write(Target::new(
        WriteKind::Meeting,
        Scope::TeamCalendar {
            channel,
            mailbox: mailbox.to_ascii_lowercase(),
        },
    )))
}

/// The channel a team event's `skypeTeamsData` names: a JSON text whose `cid` is a team's or channel's thread id.
fn team_channel(event: &Map<String, Value>) -> Option<String> {
    let Some(Value::String(data)) = optional(event, "skypeTeamsData").ok()? else {
        return None;
    };
    let Value::Object(data) = strict_json(data.as_bytes())? else {
        return None;
    };
    match optional(&data, "cid").ok()? {
        Some(Value::String(cid)) if crate::membership::is_team_thread_id(cid) => Some(cid.clone()),
        _ => None,
    }
}

/// `PUT v2.0/me/calendars/events/<id>?isOnlineMeeting=<bool>` with `{updatedCalendarEvent}`: the event changed, its
/// update sent to every attendee it names (and a cancellation to any it no longer names).
fn update(request: &Parsed<'_>, id: &str) -> Option<Contact> {
    if id.is_empty() || !boolean_query(request, &["isOnlineMeeting"]) {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &["updatedCalendarEvent"])?;
    let event = optional(&body, "updatedCalendarEvent").ok()??.as_object()?;
    // the event the body describes must be the one the path names
    match optional(event, "objectId").ok()? {
        None | Some(Value::Null) => {}
        Some(Value::String(object_id)) if object_id == id => {}
        Some(_) => return None,
    }
    let attendees = attendees(event)?;
    Some(Contact::Write(event_target(id).with_added(attendees)))
}

/// `PUT v2.0/me/calendars/events/<id>/cancel` with `{conversationId?, cancellationMessage?}`: the meeting cancelled,
/// its attendees told.
fn cancel(request: &Parsed<'_>, id: &str) -> Option<Contact> {
    if id.is_empty() || !request.query.is_empty() {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &CANCEL_KEYS)?;
    for key in CANCEL_KEYS {
        match optional(&body, key).ok()? {
            None | Some(Value::Null | Value::String(_)) => {}
            Some(_) => return None,
        }
    }
    Some(Contact::Write(event_target(id)))
}

/// `DELETE v2.0/me/calendars/events/<id>`, no query, no body (or an empty one): the event deleted; an organizer's
/// meeting is cancelled for its attendees.
fn delete(request: &Parsed<'_>, id: &str) -> Option<Contact> {
    let no_body = request.body.is_none_or(<[u8]>::is_empty);
    (!id.is_empty() && request.query.is_empty() && no_body).then(|| Contact::Write(event_target(id)))
}

/// `POST beta/me/calendarEvents/privateMeeting/schedulingService/create`, no query: the empty online meeting (its join
/// link and chat, the organizer alone in it) the web client makes before it creates a Teams meeting's event. It reaches
/// nobody, so it is let through; one that names a chat (`threadId`) would tie the meeting to that chat, and stays
/// unreadable.
fn empty_online_meeting(request: &Parsed<'_>) -> Option<Contact> {
    if !request.query.is_empty() {
        return None;
    }
    let body = object_body(request)?;
    only_keys(&body, &SCHEDULING_KEYS)?;
    match optional(&body, "threadId").ok()? {
        None | Some(Value::Null) => Some(Contact::None),
        Some(_) => None,
    }
}

/// The write's target on the existing event `id`.
fn event_target(id: &str) -> Target {
    Target::new(WriteKind::Meeting, Scope::Event { id: id.to_owned() })
}

/// The body as a JSON object, read strictly.
fn object_body(request: &Parsed<'_>) -> Option<Map<String, Value>> {
    match strict_json(request.body?)? {
        Value::Object(object) => Some(object),
        _ => None,
    }
}

/// `Some(())` when every key of `object` is one of `keys`.
fn only_keys(object: &Map<String, Value>, keys: &[&str]) -> Option<()> {
    object.keys().all(|key| keys.contains(&key.as_str())).then_some(())
}

/// Whether the meeting options name nobody: none, or a list none of whose options picks people (presenters, say, by
/// `selectedPeople` or an `optionPerson`).
fn names_nobody(options: Option<&Value>) -> bool {
    match options {
        None | Some(Value::Null) => true,
        Some(Value::Array(options)) => options.iter().all(|option| {
            option.as_object().is_some_and(|option| {
                [option.get("selectedPeople"), option.get("optionPerson")]
                    .into_iter()
                    .all(|people| match people {
                        None | Some(Value::Null) => true,
                        Some(Value::Array(people)) => people.is_empty(),
                        Some(_) => false,
                    })
            })
        }),
        Some(_) => false,
    }
}

/// The participants an event's body invites, each by its mail address ([`Mri::mail`]); `None` when the body reaches
/// anyone else ([`OTHER_AUDIENCES`]) or an attendee has no readable address.
fn attendees(event: &Map<String, Value>) -> Option<Vec<Mri>> {
    for key in OTHER_AUDIENCES {
        match optional(event, key).ok()? {
            None | Some(Value::Null | Value::Bool(false)) => {}
            Some(Value::Array(items)) if items.is_empty() => {}
            Some(_) => return None,
        }
    }
    let list = match optional(event, "attendees").ok()? {
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return None,
    };
    let mut invited = Vec::new();
    for attendee in list {
        let attendee = attendee.as_object()?;
        match optional(attendee, "address").ok()? {
            Some(Value::String(address)) if is_address(address) => invited.push(Mri::mail(address)),
            // the web client lists the organizer (the account itself) with no address
            None | Some(Value::Null) if is_organizer(attendee)? => {}
            _ => return None,
        }
    }
    Some(invited)
}

/// Whether an attendee entry is the organizer's (`"type": "Organizer"`); `None` when its type can't be read.
fn is_organizer(attendee: &Map<String, Value>) -> Option<bool> {
    Some(matches!(optional(attendee, "type").ok()?, Some(Value::String(kind)) if kind == "Organizer"))
}

/// A mail address: one `@` with something on both sides, nothing that could hide another recipient.
fn is_address(address: &str) -> bool {
    let address = address.trim();
    address
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.') && !domain.contains('@'))
        && !address.contains([',', ';', ' ', '<', '>', '"'])
}
