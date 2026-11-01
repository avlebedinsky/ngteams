//! What the scheduling form edits, apart from its views: the event's time slot and how it moves, the choices of its
//! drop-downs, how a typed address becomes an attendee, and the draft a Save sends.

use chrono::{
    Datelike as _, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset as _, TimeZone as _, Timelike as _,
};
use teams_core::{EventAttendee, EventDraft, EventShowAs, Organizer, ZonedTime};
use teams_proto::Recurrence;

use crate::clock::HourCycle;

/// How long a new event lasts unless its slot says otherwise.
pub const DEFAULT_LENGTH: Duration = Duration::minutes(30);

/// The step the time drop-downs list and a drag snaps to.
pub const STEP_MINUTES: i64 = 15;

/// An event's time: its start and end in local time, and whether it lasts all day (then both are midnights).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub all_day: bool,
}

impl Slot {
    /// `start` to `end`, an end not after the start made the start plus [`DEFAULT_LENGTH`].
    #[must_use]
    pub fn new(start: NaiveDateTime, end: NaiveDateTime) -> Self {
        let end = if end > start { end } else { start + DEFAULT_LENGTH };
        Self {
            start,
            end,
            all_day: false,
        }
    }

    /// The slot New opens with at `now`: the next half hour, for [`DEFAULT_LENGTH`].
    #[must_use]
    pub fn next_half_hour(now: NaiveDateTime) -> Self {
        let minutes = i64::from(now.hour()) * 60 + i64::from(now.minute());
        let next = (minutes / 30 + 1) * 30;
        let start = now.date().and_time(NaiveTime::MIN) + Duration::minutes(next);
        Self::new(start, start + DEFAULT_LENGTH)
    }

    /// How long it lasts.
    #[must_use]
    pub fn length(self) -> Duration {
        self.end - self.start
    }

    /// The same length, starting at `start`.
    #[must_use]
    pub fn starting(self, start: NaiveDateTime) -> Self {
        Self {
            start,
            end: start + self.length(),
            all_day: self.all_day,
        }
    }

    /// Ending at `end`: one not after the start keeps the start and lasts [`DEFAULT_LENGTH`].
    #[must_use]
    pub fn ending(self, end: NaiveDateTime) -> Self {
        Self {
            all_day: self.all_day,
            ..Self::new(self.start, end)
        }
    }

    /// On `date`, at the same times of day (an all-day slot as many days long).
    #[must_use]
    pub fn on(self, date: NaiveDate) -> Self {
        self.starting(date.and_time(self.start.time()))
    }

    /// All day (`true`: the start's day to the midnight after the end's, a day at least) or at times again (the
    /// start's day at the next half hour of the working day).
    #[must_use]
    pub fn with_all_day(self, all_day: bool) -> Self {
        if all_day == self.all_day {
            return self;
        }
        let day = self.start.date();
        if all_day {
            let last = if self.end.time() == NaiveTime::MIN && self.end.date() > day {
                self.end.date()
            } else {
                self.end.date() + Duration::days(1)
            };
            Self {
                start: day.and_time(NaiveTime::MIN),
                end: last.max(day + Duration::days(1)).and_time(NaiveTime::MIN),
                all_day: true,
            }
        } else {
            let start = day.and_hms_opt(9, 0, 0).unwrap_or(self.start);
            Self::new(start, start + DEFAULT_LENGTH)
        }
    }

    /// Moved by `minutes`, snapped to [`STEP_MINUTES`] (a drag of the event's block).
    #[must_use]
    pub fn moved(self, minutes: i64) -> Self {
        self.starting(snap(self.start + Duration::minutes(minutes)))
    }

    /// Its start moved by `minutes`, snapped, kept a step before the end (a drag of the block's top).
    #[must_use]
    pub fn start_moved(self, minutes: i64) -> Self {
        let start = snap(self.start + Duration::minutes(minutes)).min(self.end - Duration::minutes(STEP_MINUTES));
        Self { start, ..self }
    }

    /// Its end moved by `minutes`, snapped, kept a step after the start (a drag of the block's bottom).
    #[must_use]
    pub fn end_moved(self, minutes: i64) -> Self {
        let end = snap(self.end + Duration::minutes(minutes)).max(self.start + Duration::minutes(STEP_MINUTES));
        Self { end, ..self }
    }

    /// What the form's time line says: `Tue 10/27/2026 3:00 PM - 3:25 PM`, an all-day slot `Tue 10/27/2026 (all
    /// day)`, one over several days both dates.
    #[must_use]
    pub fn text(self, cycle: HourCycle) -> String {
        let date = |time: NaiveDateTime| time.format("%a %-m/%-d/%Y").to_string();
        let time = |time: NaiveDateTime| time.format(cycle.time_of_day()).to_string();
        if self.all_day {
            let last = self.end.date() - Duration::days(1);
            if last <= self.start.date() {
                format!("{} (all day)", date(self.start))
            } else {
                format!("{} - {} (all day)", date(self.start), last.format("%a %-m/%-d/%Y"))
            }
        } else if self.start.date() == self.end.date() {
            format!("{} {} - {}", date(self.start), time(self.start), time(self.end))
        } else {
            format!(
                "{} {} - {} {}",
                date(self.start),
                time(self.start),
                date(self.end),
                time(self.end)
            )
        }
    }

    /// The draft's times: each instant with the offset of the local zone then.
    #[must_use]
    pub fn zoned(self) -> (ZonedTime, ZonedTime) {
        (zoned(self.start), zoned(self.end))
    }
}

/// `time`, rounded to the nearest [`STEP_MINUTES`].
#[must_use]
pub fn snap(time: NaiveDateTime) -> NaiveDateTime {
    let minutes = i64::from(time.hour()) * 60 + i64::from(time.minute());
    let snapped = (minutes + STEP_MINUTES / 2) / STEP_MINUTES * STEP_MINUTES;
    time.date().and_time(NaiveTime::MIN) + Duration::minutes(snapped)
}

/// A local time as an instant with its offset (the earlier of two when the clock goes back, the moment after a gap
/// when it skips).
#[must_use]
pub fn zoned(time: NaiveDateTime) -> ZonedTime {
    let local = Local
        .from_local_datetime(&time)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(time + Duration::hours(1))).earliest());
    match local {
        Some(local) => ZonedTime {
            ms: local.timestamp_millis(),
            offset_minutes: local.offset().fix().local_minus_utc() / 60,
        },
        None => ZonedTime {
            ms: time.and_utc().timestamp_millis(),
            offset_minutes: 0,
        },
    }
}

/// The times of `date` a start drop-down lists: every half hour, as Teams lists them.
#[must_use]
pub fn day_times(date: NaiveDate) -> Vec<NaiveDateTime> {
    (0..48)
        .map(|half| date.and_time(NaiveTime::MIN) + Duration::minutes(half * 30))
        .collect()
}

/// The end times an end drop-down lists after `start`: every half hour for a day, each with how long it would make
/// the event (`30 min`, `1 hr`, `1.5 hrs`).
#[must_use]
pub fn end_times(start: NaiveDateTime) -> Vec<(NaiveDateTime, String)> {
    (1..=48)
        .map(|half| {
            let minutes = half * 30;
            let length = if minutes < 60 {
                format!("{minutes} min")
            } else if minutes == 60 {
                "1 hr".to_owned()
            } else if minutes % 60 == 0 {
                format!("{} hrs", minutes / 60)
            } else {
                format!("{}.5 hrs", minutes / 60)
            };
            (start + Duration::minutes(minutes), length)
        })
        .collect()
}

/// A reminder the form offers: minutes before the start (`None`: no reminder), with its label.
pub const REMINDERS: [(Option<u32>, &str); 10] = [
    (None, "Don't remind me"),
    (Some(0), "At time of event"),
    (Some(5), "5 minutes before"),
    (Some(15), "15 minutes before"),
    (Some(30), "30 minutes before"),
    (Some(60), "1 hour before"),
    (Some(120), "2 hours before"),
    (Some(720), "12 hours before"),
    (Some(1440), "1 day before"),
    (Some(10_080), "1 week before"),
];

/// A reminder's label (one the list doesn't have, in minutes).
#[must_use]
pub fn reminder_label(minutes: Option<u32>) -> String {
    REMINDERS.iter().find(|(choice, _)| *choice == minutes).map_or_else(
        || format!("{} minutes before", minutes.unwrap_or_default()),
        |(_, label)| (*label).to_owned(),
    )
}

/// Show as's label.
#[must_use]
pub fn show_as_label(show_as: EventShowAs) -> &'static str {
    match show_as {
        EventShowAs::Free => "Free",
        EventShowAs::WorkingElsewhere => "Working elsewhere",
        EventShowAs::Tentative => "Tentative",
        EventShowAs::Busy => "Busy",
        EventShowAs::Away => "Away",
    }
}

/// The categories Outlook gives every mailbox, which the category drop-down offers beside the event's own.
pub const CATEGORIES: [&str; 6] = [
    "Red category",
    "Orange category",
    "Yellow category",
    "Green category",
    "Blue category",
    "Purple category",
];

/// A typed address as an attendee, when it is one: `name@domain.tld`, nothing that could hide another recipient.
#[must_use]
pub fn typed_attendee(text: &str, required: bool) -> Option<EventAttendee> {
    let address = text.trim().trim_end_matches([';', ',']).trim();
    let (local, domain) = address.split_once('@')?;
    let valid = !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && !address.contains([' ', ',', ';', '<', '>', '"']);
    valid.then(|| EventAttendee {
        address: address.to_owned(),
        name: None,
        mri: None,
        required,
    })
}

/// What the form holds, as the draft a Save sends. The organizer and the time zone are the engine's to fill in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fields {
    pub subject: String,
    pub slot: Slot,
    pub attendees: Vec<EventAttendee>,
    pub location: String,
    /// The text as HTML: the editor's, or the event's own while it is unchanged (empty for none).
    pub body_html: String,
    pub online: bool,
    pub show_as: EventShowAs,
    pub reminder: Option<u32>,
    pub private: bool,
    pub categories: Vec<String>,
    /// How a series repeats (`None` for a single event, or a change of one occurrence).
    pub recurrence: Option<Recurrence>,
}

impl Fields {
    /// The draft of these fields.
    #[must_use]
    pub fn draft(&self) -> EventDraft {
        let (start, end) = self.slot.zoned();
        EventDraft {
            subject: self.subject.trim().to_owned(),
            start,
            end,
            time_zone: None,
            all_day: self.slot.all_day,
            organizer: Organizer {
                mri: String::new(),
                address: None,
                name: None,
            },
            attendees: self.attendees.clone(),
            location: self.location.trim().to_owned(),
            body_html: self.body_html.clone(),
            online: self.online,
            show_as: self.show_as,
            reminder_minutes: self.reminder,
            private: self.private,
            categories: self.categories.clone(),
            recurrence: self.recurrence.clone(),
        }
    }
}

/// Plain text as the HTML an event's body carries: each line a `<div>`, escaped; none for empty text.
#[must_use]
pub fn text_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    text.lines()
        .map(|line| {
            let escaped = line
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            if escaped.is_empty() {
                "<div><br></div>".to_owned()
            } else {
                format!("<div>{escaped}</div>")
            }
        })
        .collect()
}

/// An event's HTML body as the plain text the form edits: tags dropped, block ends as line breaks, the common entities
/// read.
#[must_use]
pub fn html_text(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        let tag = rest[open + 1..open + close].trim().to_ascii_lowercase();
        let name = tag.trim_start_matches('/').split([' ', '/']).next().unwrap_or_default();
        if matches!(name, "br" | "p" | "div" | "tr" | "li") && (tag.starts_with('/') || name == "br") {
            text.push('\n');
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let text = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim().to_owned()
}

/// The day pane's title: `Tue, Oct 27, 2026`.
#[must_use]
pub fn day_title(date: NaiveDate) -> String {
    format!(
        "{}, {} {}, {}",
        date.format("%a"),
        date.format("%b"),
        date.day(),
        date.year()
    )
}

#[cfg(test)]
#[path = "../../tests/support/event_form_model_units.rs"]
mod tests;
