//! An event as the middle tier answers it (`docs/protocol/calendar.md`, "An event"), for the account that reads it.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::meeting::Meeting;
use super::repo::{Attendee, Event};
use crate::app::App;
use crate::db::users::User;
use crate::teams::repo::Team;

pub(super) const EVENT_TYPE: &str = "Microsoft.SkypeSpaces.MiddleTier.Models.CalendarEvent";

/// The longest `bodyPreview`, in characters.
const PREVIEW: usize = 255;

/// What the events of one answer name beyond their own rows: their organizers, their meetings and, for a
/// channel's meetings, their teams.
pub(super) struct Book<'a> {
    pub app: &'a App,
    pub teams: HashMap<String, Team>,
    pub users: HashMap<String, User>,
    pub meetings: HashMap<String, Meeting>,
}

/// One event for one reader: the row, the occurrence of it when the row is a series read by its occurrences, and
/// the reader's own invitation.
pub(super) struct Shown<'a> {
    pub reader: &'a str,
    pub event: &'a Event,
    pub occurrence: Option<i64>,
    pub mine: Option<&'a Attendee>,
    pub book: &'a Book<'a>,
    /// Read from the team's calendar: the team's own view of its meeting, the same for whoever reads it.
    pub team_view: bool,
}

impl Shown<'_> {
    fn fields(&self) -> Map<String, Value> {
        serde_json::from_str(&self.event.body).unwrap_or_default()
    }

    fn times(&self) -> (i64, i64) {
        match self.occurrence {
            Some(start) => (start, start + (self.event.end_ms - self.event.start_ms)),
            None => (self.event.start_ms, self.event.end_ms),
        }
    }

    fn kind(&self) -> &'static str {
        if self.occurrence.is_some() {
            "Occurrence"
        } else if self.event.recurring {
            "RecurringMaster"
        } else if self.event.series_id.is_none() {
            "Single"
        } else if self.event.changed {
            "Exception"
        } else {
            "Occurrence"
        }
    }

    fn organizes(&self) -> bool {
        self.team_view || (self.event.team_id.is_none() && self.event.owner_id == self.reader)
    }

    fn team(&self) -> Option<&Team> {
        self.event.team_id.as_ref().and_then(|id| self.book.teams.get(id))
    }

    /// A channel's meeting: the team organizes it, its channel is its chat, and its link names its post there.
    fn of_channel(&self, team: &Team, fields: &mut Map<String, Value>) {
        let app = self.book.app;
        let channel = self.event.channel_id.as_deref().unwrap_or_default();
        let post = self.event.post_id.unwrap_or(0);
        let link = super::meeting::join_link(app, channel, &post.to_string(), &self.event.owner_id);
        let data = json!({"cid": channel, "uid": mri(&self.event.owner_id), "rid": post, "mid": post, "type": 0});
        fields.insert("organizerName".into(), json!(team.name));
        fields.insert("organizerAddress".into(), json!(mailbox(app, team)));
        fields.insert("isOnlineMeeting".into(), json!(true));
        fields.insert("skypeTeamsMeetingUrl".into(), json!(link));
        fields.insert("skypeTeamsData".into(), json!(data.to_string()));
        if fields.get("location").and_then(Value::as_str) == Some("") {
            fields.insert("location".into(), json!("Microsoft Teams Meeting"));
        }
    }

    fn response(&self) -> &'static str {
        if self.organizes() {
            return "Organizer";
        }
        match self.mine.map(|mine| mine.response.as_str()) {
            Some("Accepted") => "Accepted",
            Some("TentativelyAccepted") => "Tentative",
            Some("Declined") => "Declined",
            _ => "NotResponded",
        }
    }

    /// The event as the calendar view lists it.
    pub(super) fn entry(&self) -> Value {
        let mut fields = self.fields();
        let (start, end) = self.times();
        let id = match self.occurrence {
            Some(start) => occurrence_id(&self.event.id, start),
            None => self.event.id.clone(),
        };
        let subject = fields.get("subject").and_then(Value::as_str).unwrap_or_default();
        let subject = if self.event.cancelled {
            format!("Canceled: {subject}")
        } else {
            subject.to_owned()
        };
        let preview = fields
            .get("bodyContent")
            .and_then(Value::as_str)
            .map(preview)
            .unwrap_or_default();
        let owner = self.book.users.get(&self.event.owner_id);
        let meeting = self
            .event
            .meeting_chat
            .as_ref()
            .and_then(|chat| self.book.meetings.get(chat));
        let series = self
            .occurrence
            .map(|_| self.event.id.clone())
            .or_else(|| self.event.series_id.clone());
        let ical_uid = self.event.series_id.as_deref().unwrap_or(&self.event.id);
        let computed = json!({
            "objectId": id, "iCalUID": ical_uid, "cleanGlobalObjectId": ical_uid,
            "subject": subject, "startTime": iso(start), "endTime": iso(end),
            "eventType": self.kind(), "isCancelled": self.event.cancelled,
            "isOnlineMeeting": meeting.is_some(), "location": location(&Value::Object(fields.clone()), meeting.is_some()),
            "showAs": self.show_as(&fields),
            "isOrganizer": self.organizes(), "myResponseType": self.response(),
            "organizerName": owner.map(|owner| owner.display_name.as_str()),
            "organizerAddress": owner.map(|owner| owner.upn.as_str()),
            "bodyPreview": preview, "lastModifiedTime": iso(self.event.version),
        });
        if let Value::Object(computed) = computed {
            fields.extend(computed);
        }
        if let Some(series) = series {
            fields.insert("seriesMasterId".into(), json!(series));
        }
        if let Some(meeting) = meeting {
            let link = meeting.join_link(self.book.app);
            let data = json!({"cid": meeting.chat_id, "uid": mri(&meeting.organizer_id), "type": 0, "private": true});
            fields.insert("skypeTeamsMeetingUrl".into(), json!(link));
            fields.insert(
                "shortOnlineMeetingJoinUrl".into(),
                json!(meeting.short_link(self.book.app)),
            );
            fields.insert("onlineMeetingConferenceId".into(), json!(meeting.code));
            fields.insert("skypeTeamsData".into(), json!(data.to_string()));
        }
        if let Some(team) = self.team() {
            self.of_channel(team, &mut fields);
        }
        if self.kind() != "RecurringMaster" {
            fields.remove("eventRecurrencePattern");
            fields.remove("eventRecurrenceRange");
        }
        fields.remove("bodyContent");
        fields.remove("bodyContentType");
        Value::Object(fields)
    }

    /// The event read whole: its text and, for a meeting, whom it invites with their answers.
    pub(super) fn full(&self, attendees: &[(Attendee, User)]) -> Value {
        let mut entry = self.entry();
        let stored = self.fields();
        for key in ["bodyContent", "bodyContentType"] {
            if let Some(value) = stored.get(key) {
                entry[key] = value.clone();
            }
        }
        entry["isAppointment"] = json!(attendees.is_empty());
        if attendees.is_empty() {
            return entry;
        }
        let mut listed = Vec::new();
        if let Some(team) = self.team() {
            listed.push(
                json!({"address": mailbox(self.book.app, team), "name": team.name, "role": "User",
                "type": "Organizer", "status": {"response": "Organizer", "time": iso(self.event.version)}}),
            );
        } else if let Some(owner) = self.book.users.get(&self.event.owner_id) {
            listed.push(attendee(owner, "Organizer", "Organizer", self.event.version));
        }
        for (invited, user) in attendees {
            let kind = if invited.optional { "Optional" } else { "Required" };
            let time = invited.responded_at.unwrap_or(self.event.version);
            listed.push(attendee(user, kind, &invited.response, time));
        }
        entry["attendees"] = Value::Array(listed);
        entry
    }

    /// How the event shows the reader's time: its organizer's as set; an invited account's by its answer.
    fn show_as(&self, fields: &Map<String, Value>) -> Value {
        let set = fields.get("showAs").cloned().unwrap_or_else(|| json!("Busy"));
        match self.response() {
            "Organizer" | "Accepted" => set,
            "Declined" => json!("Free"),
            _ => json!("Tentative"),
        }
    }
}

fn attendee(user: &User, kind: &str, response: &str, time: i64) -> Value {
    json!({
        "address": user.upn, "name": user.display_name, "mri": mri(&user.id), "role": "Admin", "type": kind,
        "status": {"response": response, "time": iso(time)},
    })
}

/// A team's mailbox: what its group calendar is addressed by.
fn mailbox(app: &App, team: &Team) -> String {
    let host = app.config.public_origin.host_str().unwrap_or("localhost");
    format!("{}@{host}", team.group_id)
}

fn mri(id: &str) -> String {
    format!("8:orgid:{id}")
}

/// Where the event is: as set, else Teams' own words for an online meeting.
pub(super) fn location(fields: &Value, online: bool) -> &str {
    match fields["location"].as_str() {
        Some(place) if !place.is_empty() => place,
        _ if online => "Microsoft Teams Meeting",
        _ => "",
    }
}

/// The id of the occurrence of the series `series` that starts at `start`.
pub(super) fn occurrence_id(series: &str, start: i64) -> String {
    format!("{series}.{start}")
}

/// The answer's envelope around its events.
pub(super) fn events(events: Vec<Value>) -> Value {
    json!({"type": EVENT_TYPE, "value": events})
}

/// A time as the service writes an event's: UTC, seven fraction digits.
pub(super) fn iso(ms: i64) -> String {
    let time = chrono::DateTime::from_timestamp_millis(ms).unwrap_or(chrono::DateTime::UNIX_EPOCH);
    format!(
        "{}.{:03}0000Z",
        time.format("%Y-%m-%dT%H:%M:%S"),
        time.timestamp_subsec_millis()
    )
}

/// The start of an event's text, its markup taken out and its spaces folded.
fn preview(html: &str) -> String {
    let text: Vec<String> = scraper::Html::parse_fragment(html)
        .root_element()
        .text()
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect();
    text.join(" ").chars().take(PREVIEW).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preview_is_the_texts_start_without_markup() {
        assert_eq!(
            preview("<p>Bring the <b>x-rays</b></p>\n<p>&amp; a  pen</p>"),
            "Bring the x-rays & a pen"
        );
        assert_eq!(preview(""), "");
        assert_eq!(preview(&"word ".repeat(100)).chars().count(), PREVIEW);
    }

    #[test]
    fn a_time_is_written_in_utc_with_seven_fraction_digits() {
        assert_eq!(iso(1_799_020_800_123), "2027-01-04T00:00:00.1230000Z");
    }
}
