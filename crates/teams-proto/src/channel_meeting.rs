//! A channel's scheduled meeting as its post carries it (`docs/protocol/calling.md`, "Schedule a meeting"): once a
//! meeting is created on a team's group calendar, the service posts it to the channel, a `RichText/Html` message whose
//! text is "Scheduled a meeting" and whose `properties.meeting` (a JSON object as a string) is a
//! `http://schema.skype.com/ScheduledMeetingCreated` naming its title (`meetingtitle`), its times and location and its
//! calendar ids (`scheduledmeetinginfo`: `startTime`, `endTime`, `location`, `exchangeId`, `iCalUid`, `eventType`),
//! its organizer and its join link (`meetingJoinUrl`), seen 2026-11-03. The channel draws it as Teams' meeting card;
//! its cancellation names the meeting by its `iCalUid` and its post ([`crate::CancelChannelEvent`]).

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The `@type` of a scheduled meeting's post.
pub const SCHEDULED_MEETING_TYPE: &str = "http://schema.skype.com/ScheduledMeetingCreated";

/// The `@type` of the reply its cancellation posts under it (its text "The meeting "<title>" is cancelled", its
/// `scheduledmeetinginfo.isCancelled` `true`).
pub const CANCELLED_MEETING_TYPE: &str = "http://schema.skype.com/ScheduledMeetingCancelled";

/// A channel's scheduled meeting, as its post names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledMeeting {
    pub title: String,
    /// Its start and end, ms since the epoch.
    pub start_ms: i64,
    pub end_ms: i64,
    /// Its start as the post wrote it (what its cancellation names it by).
    pub start_text: String,
    /// Its location (`Microsoft Teams Meeting` for an online one), when it names one.
    pub location: Option<String>,
    /// Its id on the team's calendar (`iCalUid`): what its cancellation goes to.
    pub ical_uid: String,
    /// `Single`, or an occurrence's kind.
    pub event_type: String,
    /// The organizer's object id, when the post names one.
    pub organizer_id: Option<String>,
    /// Its join link, when the post names one.
    pub join_url: Option<String>,
    /// The post is its cancellation's reply ([`CANCELLED_MEETING_TYPE`]): the meeting with this `ical_uid` is
    /// cancelled.
    pub cancelled: bool,
}

impl ScheduledMeeting {
    /// The meeting a post's `properties` (as stored: the JSON object as text) names; `None` for a post that names none
    /// or names it without a title, its times or its calendar id.
    #[must_use]
    pub fn of_properties(properties: &str) -> Option<Self> {
        let properties: Value = serde_json::from_str(properties).ok()?;
        let meeting = match properties.get("meeting")? {
            Value::String(text) => serde_json::from_str::<Value>(text).ok()?,
            object @ Value::Object(_) => object.clone(),
            _ => return None,
        };
        let cancelled = match meeting.get("@type").and_then(Value::as_str) {
            Some(SCHEDULED_MEETING_TYPE) => false,
            Some(CANCELLED_MEETING_TYPE) => true,
            _ => return None,
        };
        let info = meeting.get("scheduledmeetinginfo")?;
        let text = |value: &Value, key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let start_text = text(info, "startTime")?;
        Some(Self {
            title: text(&meeting, "meetingtitle")?,
            start_ms: ms(&start_text)?,
            end_ms: ms(&text(info, "endTime")?)?,
            start_text,
            location: text(info, "location"),
            ical_uid: text(info, "iCalUid").filter(|id| id.chars().all(|c| c.is_ascii_hexdigit()))?,
            event_type: text(info, "eventType").unwrap_or_else(|| "Single".to_owned()),
            organizer_id: text(&meeting, "organizerId").map(|id| id.to_ascii_lowercase()),
            join_url: text(&meeting, "meetingJoinUrl").filter(|url| url.starts_with("https://")),
            cancelled: cancelled || info.get("isCancelled").and_then(Value::as_bool) == Some(true),
        })
    }

    /// The channel's meeting an event of a calendar is (its [`crate::ChannelEventRef`]), as its post would name it, with
    /// its post's id: what its cancellation takes when its details or its calendar card cancel it. `None` for an event
    /// that isn't a channel's meeting, names no post, or whose times have no instant.
    #[must_use]
    pub fn of_event(event: &crate::CalendarEvent) -> Option<(Self, String)> {
        let reference = event.channel_meeting.as_ref()?;
        let (crate::EventTime::Instant(start_ms), crate::EventTime::Instant(end_ms)) = (&event.start, &event.end)
        else {
            return None;
        };
        let event_type = match &event.kind {
            crate::EventKind::Single => "Single".to_owned(),
            crate::EventKind::Occurrence => "Occurrence".to_owned(),
            crate::EventKind::Exception => "Exception".to_owned(),
            crate::EventKind::RecurringMaster => "RecurringMaster".to_owned(),
            crate::EventKind::Other(kind) if kind.is_empty() => "Single".to_owned(),
            crate::EventKind::Other(kind) => kind.clone(),
        };
        let meeting = Self {
            title: event.subject.clone(),
            start_ms: *start_ms,
            end_ms: *end_ms,
            start_text: crate::calendar::iso_utc(*start_ms)?,
            location: event.location.clone(),
            ical_uid: reference.ical_uid.clone(),
            event_type,
            organizer_id: reference
                .organizer
                .as_deref()
                .map(|mri| mri.trim_start_matches("8:orgid:").to_ascii_lowercase()),
            join_url: event.join_url.clone(),
            cancelled: event.cancelled,
        };
        Some((meeting, reference.post.clone()?))
    }

    /// Whether `object_id` (an account's id without its `8:orgid:`) organizes it.
    #[must_use]
    pub fn organized_by(&self, object_id: &str) -> bool {
        self.organizer_id
            .as_deref()
            .is_some_and(|organizer| organizer.eq_ignore_ascii_case(object_id))
    }
}

/// Ms since the epoch of an RFC 3339 time.
fn ms(text: &str) -> Option<i64> {
    let time = OffsetDateTime::parse(text, &Rfc3339).ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(meeting: &Value) -> String {
        serde_json::json!({"meeting": meeting.to_string(), "languageStamp": "x"}).to_string()
    }

    fn made_up() -> Value {
        serde_json::json!({
            "@type": SCHEDULED_MEETING_TYPE,
            "meetingtitle": "lab channel meeting",
            "scheduledmeetinginfo": {
                "startTime": "2026-11-03T02:00:00+00:00",
                "endTime": "2026-11-03T02:30:00+00:00",
                "location": "Microsoft Teams Meeting",
                "exchangeId": "AQMkMadeUp",
                "iCalUid": "040000008200e00074c5b7101a82e008",
                "eventType": "Single",
                "meetingType": 1
            },
            "organizerId": "00000000-0000-4000-8000-00000000000A",
            "meetingJoinUrl": "https://teams.microsoft.com/l/meetup-join/x"
        })
    }

    #[test]
    fn a_scheduled_meeting_s_post_names_it() {
        let meeting = ScheduledMeeting::of_properties(&post(&made_up())).expect("a meeting");
        assert_eq!(meeting.title, "lab channel meeting");
        assert_eq!(meeting.end_ms - meeting.start_ms, 30 * 60_000);
        assert_eq!(meeting.start_text, "2026-11-03T02:00:00+00:00");
        assert_eq!(meeting.ical_uid, "040000008200e00074c5b7101a82e008");
        assert!(meeting.organized_by("00000000-0000-4000-8000-00000000000a"));
        // an object rather than its text reads the same
        let object = serde_json::json!({"meeting": made_up()}).to_string();
        assert_eq!(ScheduledMeeting::of_properties(&object), Some(meeting.clone()));
        assert!(!meeting.cancelled);
        // its cancellation's reply names the same meeting, cancelled
        let mut cancelled = made_up();
        cancelled["@type"] = serde_json::json!(CANCELLED_MEETING_TYPE);
        cancelled["scheduledmeetinginfo"]["isCancelled"] = serde_json::json!(true);
        let reply = ScheduledMeeting::of_properties(&post(&cancelled)).expect("a cancellation");
        assert!(reply.cancelled);
        assert_eq!(reply.ical_uid, meeting.ical_uid);
    }

    #[test]
    fn a_meet_now_s_post_names_no_scheduled_meeting() {
        // a channel's Meet now posts the same `@type` with no title and no times
        let mut meet_now = made_up();
        meet_now["meetingtitle"] = Value::Null;
        meet_now["scheduledmeetinginfo"] = serde_json::json!({"startTime": "0001-01-01T00:00:00",
            "endTime": "0001-01-01T00:00:00", "meetingType": 0});
        assert!(ScheduledMeeting::of_properties(&post(&meet_now)).is_none());
    }

    #[test]
    fn any_other_post_names_none() {
        for change in [
            ("@type", serde_json::json!("http://schema.skype.com/Other")),
            ("meetingtitle", Value::Null),
        ] {
            let mut meeting = made_up();
            meeting[change.0] = change.1;
            assert!(
                ScheduledMeeting::of_properties(&post(&meeting)).is_none(),
                "{}",
                change.0
            );
        }
        let mut odd_id = made_up();
        odd_id["scheduledmeetinginfo"]["iCalUid"] = serde_json::json!("../x");
        assert!(ScheduledMeeting::of_properties(&post(&odd_id)).is_none());
        assert!(ScheduledMeeting::of_properties("{}").is_none());
        assert!(ScheduledMeeting::of_properties("not json").is_none());
    }
}
