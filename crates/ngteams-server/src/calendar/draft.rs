//! An event as the scheduling form sends it, checked: what is kept of it is a fixed list of the form's own fields,
//! so nothing a request names can stand in for what the server decides (who organizes, who answered what, the
//! meeting's links).

use serde_json::{Map, Value};

use super::recur::{self, Series};
use super::zones::Zone;
use super::{Answer, Error};

const TEXT_LIMIT: usize = 255;
const MOST_CATEGORIES: usize = 25;
/// The longest single event or occurrence.
const LONGEST_MS: i64 = 366 * 24 * 3_600_000;

const SHOW_AS: [&str; 6] = ["Free", "Tentative", "Busy", "OOF", "WorkingElsewhere", "Unknown"];

/// An event's fields as kept, with what the server reads out of them.
pub(super) struct Draft {
    pub body: Map<String, Value>,
    pub start_ms: i64,
    pub end_ms: i64,
    /// When a series' last occurrence is over (`i64::MAX`: never); `None` for an event that doesn't repeat.
    pub series_until: Option<i64>,
}

impl Draft {
    /// The event `sent` describes; a series only where `may_repeat`.
    pub(super) fn read(sent: &Value, may_repeat: bool) -> Answer<Self> {
        let sent = sent.as_object().ok_or(Error::BAD_REQUEST)?;
        let mut body = Map::new();
        for key in ["subject", "location"] {
            let text = match sent.get(key) {
                None | Some(Value::Null) => "",
                Some(Value::String(text)) => text.trim(),
                Some(_) => return Err(Error::BAD_REQUEST),
            };
            if text.chars().count() > TEXT_LIMIT {
                return Err(Error::BAD_REQUEST);
            }
            body.insert(key.into(), text.into());
        }
        for key in [
            "isAllDayEvent",
            "isReminderSet",
            "isPrivate",
            "isResponseRequested",
            "doNotForward",
        ] {
            let flag = match sent.get(key) {
                None | Some(Value::Null) => false,
                Some(Value::Bool(flag)) => *flag,
                Some(_) => return Err(Error::BAD_REQUEST),
            };
            body.insert(key.into(), flag.into());
        }
        let show_as = sent.get("showAs").and_then(Value::as_str).unwrap_or("Busy");
        if !SHOW_AS.contains(&show_as) {
            return Err(Error::BAD_REQUEST);
        }
        body.insert("showAs".into(), show_as.into());
        let reminder = sent
            .get("reminderMinutesBeforeStart")
            .map_or(Some(0), Value::as_u64)
            .filter(|minutes| *minutes <= 40_320)
            .ok_or(Error::BAD_REQUEST)?;
        body.insert("reminderMinutesBeforeStart".into(), reminder.into());
        text_of(sent, &mut body)?;
        categories(sent, &mut body)?;
        let (start_ms, end_ms) = (time(sent.get("startTime"))?, time(sent.get("endTime"))?);
        if end_ms < start_ms || end_ms - start_ms > LONGEST_MS {
            return Err(Error::BAD_REQUEST);
        }
        let zone = zone_of(sent, &mut body)?;
        for key in ["eventRecurrencePattern", "eventRecurrenceRange"] {
            if let Some(value) = sent.get(key).filter(|value| !value.is_null()) {
                body.insert(key.into(), value.clone());
            }
        }
        let series = recur::series(&body, start_ms, end_ms, zone).map_err(|()| Error::BAD_REQUEST)?;
        if series.is_some() && !may_repeat {
            return Err(Error::BAD_REQUEST);
        }
        if series.is_none() {
            body.remove("eventRecurrencePattern");
            body.remove("eventRecurrenceRange");
        }
        Ok(Self {
            body,
            start_ms,
            end_ms,
            series_until: series.as_ref().map(|series| series.until_ms().unwrap_or(i64::MAX)),
        })
    }
}

/// The series a kept event is.
pub(super) fn series_of(body: &Map<String, Value>, start_ms: i64, end_ms: i64) -> Option<Series> {
    let offset = body.get("utcOffset").and_then(Value::as_f64).unwrap_or(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "an offset in minutes, clamped by the zone"
    )]
    let zone = Zone::of(body.get("eventTimeZone").and_then(Value::as_str), offset as i64);
    recur::series(body, start_ms, end_ms, zone).ok().flatten()
}

fn text_of(sent: &Map<String, Value>, body: &mut Map<String, Value>) -> Answer<()> {
    match sent.get("bodyContent") {
        None | Some(Value::Null) => {}
        Some(Value::String(text)) => {
            body.insert("bodyContentType".into(), "html".into());
            body.insert("bodyContent".into(), text.as_str().into());
        }
        Some(_) => return Err(Error::BAD_REQUEST),
    }
    Ok(())
}

fn categories(sent: &Map<String, Value>, body: &mut Map<String, Value>) -> Answer<()> {
    let Some(sent) = sent.get("categories").filter(|sent| !sent.is_null()) else {
        return Ok(());
    };
    let names: Vec<&str> = sent
        .as_array()
        .ok_or(Error::BAD_REQUEST)?
        .iter()
        .map(|name| name.as_str().filter(|name| name.chars().count() <= TEXT_LIMIT))
        .collect::<Option<_>>()
        .ok_or(Error::BAD_REQUEST)?;
    if names.len() > MOST_CATEGORIES {
        return Err(Error::BAD_REQUEST);
    }
    body.insert("categories".into(), names.into());
    Ok(())
}

/// The event's zone as it names it, kept with the offset it carries.
fn zone_of(sent: &Map<String, Value>, body: &mut Map<String, Value>) -> Answer<Zone> {
    let name = match sent.get("eventTimeZone") {
        None | Some(Value::Null) => "Utc",
        Some(Value::String(name)) if name.len() <= 64 => name.trim(),
        Some(_) => return Err(Error::BAD_REQUEST),
    };
    let offset = match sent.get("utcOffset") {
        None | Some(Value::Null) => 0.0,
        Some(offset) => offset
            .as_f64()
            .filter(|offset| offset.abs() <= 1080.0)
            .ok_or(Error::BAD_REQUEST)?,
    };
    body.insert("eventTimeZone".into(), name.into());
    body.insert("utcOffset".into(), offset.into());
    #[expect(
        clippy::cast_possible_truncation,
        reason = "checked to be within 18 hours of minutes"
    )]
    Ok(Zone::of(Some(name), offset as i64))
}

/// An event's time: an instant with its offset.
fn time(value: Option<&Value>) -> Answer<i64> {
    value
        .and_then(Value::as_str)
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|time| time.timestamp_millis())
        .filter(|ms| (0..=253_402_300_799_999).contains(ms))
        .ok_or(Error::BAD_REQUEST)
}
