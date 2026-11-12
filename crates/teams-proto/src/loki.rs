//! Loki, Microsoft's persona card service (Delve's live persona card), which Teams' profile card reads
//! (`docs/protocol/loki.md`). Every request here is a [`Read`]: a `GET` with no body, with the account's token for
//! [`LOKI_AUDIENCE`] and the headers the service wants of a Teams client (the transport adds them).
//!
//! - [`LokiConfiguration`], on the fixed global base ([`Service::LokiGlobal`]), names the account's regional base
//!   (`LokiUrl`, `https://<region>.loki.delve.office.com/`), which the transport takes once at run time
//!   (`HttpTransport::set_loki_region`). The global host refuses every other path.
//! - On the regional base ([`Service::Loki`]), with the query
//!   `teamsMri=8:orgid:<oid>&aadObjectId=<oid>&personaType=User`: a person's names and addresses ([`LokiPerson`]), their organization ([`LokiOrganization`]), who they work with
//!   ([`LokiWorkingWith`]), their schedule ([`LokiSchedule`]) and whether their automatic replies are on
//!   ([`LokiOutOfOffice`]). Each is made only for an `8:orgid:<object id>` MRI ([`object_id`]).
//!
//! Only fields a capture showed are read; a value in a form no capture showed is kept as unknown, never guessed. The
//! organization's entries were never captured with content: each is read leniently with a working-with entry's keys
//! (unverified).

use std::fmt;

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use url::Url;

use crate::endpoint::{Endpoint, Service};
use crate::read::{Read, object_id};
use crate::recurrence::Weekday;

/// The Entra audience of the token Loki takes (the token's `aud` is Loki's application id).
pub const LOKI_AUDIENCE: &str = "https://loki.delve.office.com";

/// Loki's global host: its configuration read only.
pub const LOKI_GLOBAL_HOST: &str = "loki.delve.office.com";

/// A regional Loki host ends in this (`nam.loki.delve.office.com`).
pub const LOKI_HOST_SUFFIX: &str = ".loki.delve.office.com";

/// The configuration's path under the global base.
pub(crate) const CONFIGURATION_PATH: [&str; 4] = ["api", "v1", "livepersonacard", "configuration"];

/// How many people the working-with read asks for (`limit`), as Teams' card asks.
pub const WORKING_WITH_LIMIT: usize = 12;

/// The most managers or direct reports kept from one answer.
const MAX_ORGANIZATION: usize = 256;

/// The most addresses of one kind kept from one person's answer.
const MAX_ADDRESSES: usize = 16;

/// The regional base `base` names, if it is `https://<one label>.loki.delve.office.com/` exactly (the label lowercase
/// letters, digits and inner `-`; the trailing slash optional), with no port, user info, path, query or fragment.
pub(crate) fn parse_regional_base(base: &str) -> Option<Url> {
    if base.contains(['?', '#']) || base.trim() != base {
        return None;
    }
    let origin = base.strip_suffix('/').unwrap_or(base);
    let url = Url::parse(origin).ok()?;
    let label = url.host_str()?.strip_suffix(LOKI_HOST_SUFFIX)?;
    let labelled = !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    let plain = url.scheme() == "https"
        && labelled
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        // the base as it was given, not one the parser normalized
        && url.as_str() == format!("{origin}/");
    plain.then_some(url)
}

/// `GET https://loki.delve.office.com/api/v1/livepersonacard/configuration?cultureName=en-us`: the persona card's
/// configuration for the account, of which only `LokiUrl` (the account's regional base) is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LokiConfiguration;

impl LokiConfiguration {
    /// The regional base the answer names (`LokiUrl`), when it is one the transport takes
    /// (`https://<region>.loki.delve.office.com/`); `None` for any other answer.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<String> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let base = answer.get("LokiUrl")?.as_str()?;
        parse_regional_base(base).map(|_| base.to_owned())
    }
}

impl Read for LokiConfiguration {
    fn service(&self) -> Service {
        Service::LokiGlobal
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(CONFIGURATION_PATH).with_query("cultureName", "en-us")
    }
}

/// The query every person read carries, for the person whose object id is `id`.
fn persona_endpoint(path: &[&str], id: &str) -> Endpoint {
    Endpoint::new(path.iter().copied())
        .with_query("teamsMri", format!("8:orgid:{id}"))
        .with_query("aadObjectId", id)
        .with_query("personaType", "User")
}

/// A person read on the regional base: its struct, `of`, a `Debug` that leaves the object id out, and its [`Read`].
macro_rules! persona_read {
    ($(#[$doc:meta])* $name:ident, $path:expr $(, $key:literal = $value:expr)*) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name {
            id: String,
        }

        impl $name {
            /// The read of the person `mri` names; `None` unless it is `8:orgid:<object id>` ([`object_id`]).
            #[must_use]
            pub fn of(mri: &str) -> Option<Self> {
                object_id(mri).map(|id| Self { id: id.to_owned() })
            }
        }

        /// `Debug` leaves the object id out.
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }

        impl Read for $name {
            fn service(&self) -> Service {
                Service::Loki
            }

            fn endpoint(&self) -> Endpoint {
                persona_endpoint(&$path, &self.id)$(.with_query($key, $value))*
            }
        }
    };
}

persona_read!(
    /// `GET <regional base>/api/v2/person?<persona query>`: a person's names, addresses and directory type. Its answer
    /// is read with [`LokiPerson::read_answer`].
    LokiPerson,
    ["api", "v2", "person"]
);

persona_read!(
    /// `GET <regional base>/api/v1/organization?<persona query>`: a person's managers and direct reports. Its answer
    /// is read with [`LokiOrganization::read_answer`].
    LokiOrganization,
    ["api", "v1", "organization"]
);

persona_read!(
    /// `GET <regional base>/api/v1/workingwith?<persona query>&limit=12`: the people a person works with, as the
    /// card's Organization tab lists them. Its answer is read with [`LokiWorkingWith::read_answer`].
    LokiWorkingWith,
    ["api", "v1", "workingwith"],
    "limit" = WORKING_WITH_LIMIT.to_string()
);

persona_read!(
    /// `GET <regional base>/api/v1/schedule?<persona query>`: a person's free/busy status now and next, and their
    /// working hours. Its answer is read with [`LokiSchedule::read_answer`].
    LokiSchedule,
    ["api", "v1", "schedule"]
);

persona_read!(
    /// `GET <regional base>/api/v1/oofstatus?<persona query>`: whether a person's automatic replies are on. Its answer
    /// is read with [`LokiOutOfOffice::read_answer`].
    LokiOutOfOffice,
    ["api", "v1", "oofstatus"]
);

/// Why a Loki answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LokiAnswerError {
    /// Not the captured answer's shape.
    #[error("not the persona card service's answer")]
    Malformed,
    /// A person answer of someone other than the person asked about.
    #[error("the answer is of someone else")]
    OtherUser,
}

/// A trimmed, non-empty string value; `None` for an empty one or another kind of value.
fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The JSON object `body` is.
fn object(body: &[u8]) -> Result<serde_json::Map<String, Value>, LokiAnswerError> {
    match serde_json::from_slice(body) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(LokiAnswerError::Malformed),
    }
}

/// The items of the list `value` is: none when it is missing, `Malformed` when it is another kind of value.
fn list(value: Option<&Value>) -> Result<&[Value], LokiAnswerError> {
    match value {
        None | Some(Value::Null) => Ok(&[]),
        Some(Value::Array(items)) => Ok(items),
        Some(_) => Err(LokiAnswerError::Malformed),
    }
}

/// A person's names and addresses as Loki gave them. A field the answer left out (or gave empty) is `None` or empty.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PersonDetails {
    /// `names[0].value.displayName`.
    pub display_name: Option<String>,
    /// `names[0].value.givenName`.
    pub given_name: Option<String>,
    /// `names[0].value.surname`.
    pub surname: Option<String>,
    /// Each `emailAddresses[].value.address`, in order, each once.
    pub emails: Vec<String>,
    /// Each `imAddresses[].value.displayText` (the address Teams shows beside Chat), in order, each once.
    pub im_addresses: Vec<String>,
    /// `userPrincipalName`.
    pub user_principal_name: Option<String>,
    /// `userType`: `Member`, or another directory user type as given.
    pub user_type: Option<String>,
}

/// `Debug` leaves the names and addresses out.
impl fmt::Debug for PersonDetails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersonDetails")
            .field("emails", &self.emails.len())
            .field("im_addresses", &self.im_addresses.len())
            .field("user_type", &self.user_type)
            .finish_non_exhaustive()
    }
}

/// The `key` of each entry's `value` of the list `entries`, in order, each once.
fn addresses(entries: Option<&Value>, key: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for entry in entries.and_then(Value::as_array).map_or(&[][..], Vec::as_slice) {
        if let Some(address) = text(entry.get("value").and_then(|value| value.get(key)))
            && !found.iter().any(|known| known.eq_ignore_ascii_case(&address))
            && found.len() < MAX_ADDRESSES
        {
            found.push(address);
        }
    }
    found
}

impl LokiPerson {
    /// The person the answer `body` gives: `{"person": {"names", "emailAddresses", "imAddresses",
    /// "userPrincipalName", "aadObjectId", "userType", …}}`.
    ///
    /// # Errors
    /// [`LokiAnswerError::Malformed`] unless the body is an object with a `person` object;
    /// [`LokiAnswerError::OtherUser`] when its `aadObjectId` names someone other than the person asked about.
    pub fn read_answer(&self, body: &[u8]) -> Result<PersonDetails, LokiAnswerError> {
        let answer = object(body)?;
        let Some(person @ Value::Object(_)) = answer.get("person") else {
            return Err(LokiAnswerError::Malformed);
        };
        if text(person.get("aadObjectId")).is_some_and(|id| !id.eq_ignore_ascii_case(&self.id)) {
            return Err(LokiAnswerError::OtherUser);
        }
        let name = person
            .get("names")
            .and_then(Value::as_array)
            .and_then(|names| names.first())
            .and_then(|name| name.get("value"));
        let name_part = |key: &str| text(name.and_then(|name| name.get(key)));
        Ok(PersonDetails {
            display_name: name_part("displayName"),
            given_name: name_part("givenName"),
            surname: name_part("surname"),
            emails: addresses(person.get("emailAddresses"), "address"),
            im_addresses: addresses(person.get("imAddresses"), "displayText"),
            user_principal_name: text(person.get("userPrincipalName")),
            user_type: text(person.get("userType")),
        })
    }
}

/// Someone a person's card names (a manager, a direct report, someone they work with), as Loki gave them.
#[derive(Clone, PartialEq, Eq)]
pub struct PersonaRef {
    /// Their object id (`aadObjectId`), a GUID: their MRI is `8:orgid:<object id>`.
    pub object_id: String,
    /// `fullName`.
    pub name: Option<String>,
    /// `email`.
    pub email: Option<String>,
    /// `userPrincipalName`.
    pub user_principal_name: Option<String>,
}

impl PersonaRef {
    /// An entry with a working-with entry's keys (`aadObjectId`, `fullName`, `email`, `userPrincipalName`); `None`
    /// without an object id that is a GUID.
    fn read(entry: &Value) -> Option<Self> {
        let id = text(entry.get("aadObjectId")).filter(|id| crate::read::is_guid(id))?;
        Some(Self {
            object_id: id,
            name: text(entry.get("fullName")),
            email: text(entry.get("email")),
            user_principal_name: text(entry.get("userPrincipalName")),
        })
    }

    /// Their MRI, `8:orgid:<object id>`.
    #[must_use]
    pub fn mri(&self) -> String {
        format!("8:orgid:{}", self.object_id)
    }
}

/// `Debug` names them by their object id only.
impl fmt::Debug for PersonaRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersonaRef")
            .field("object_id", &self.object_id)
            .finish_non_exhaustive()
    }
}

/// The entries of `list` read as [`PersonaRef`]s (one without an object id left out), at most `max`.
fn people(items: &[Value], max: usize) -> Vec<PersonaRef> {
    items.iter().filter_map(PersonaRef::read).take(max).collect()
}

/// A person's managers and direct reports, as Loki gave them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Organization {
    /// `managers`, nearest first as given.
    pub managers: Vec<PersonaRef>,
    /// `directs`.
    pub directs: Vec<PersonaRef>,
}

impl LokiOrganization {
    /// The organization the answer `body` gives: `{"managers": […], "directs": […], "dottedLineManagers": […]}` (the
    /// last not read). No capture showed an entry: each is read with a working-with entry's keys (unverified), and one
    /// without an object id is left out.
    ///
    /// # Errors
    /// [`LokiAnswerError::Malformed`] unless the body is an object whose `managers` and `directs`, when there, are
    /// lists.
    pub fn read_answer(body: &[u8]) -> Result<Organization, LokiAnswerError> {
        let answer = object(body)?;
        Ok(Organization {
            managers: people(list(answer.get("managers"))?, MAX_ORGANIZATION),
            directs: people(list(answer.get("directs"))?, MAX_ORGANIZATION),
        })
    }
}

impl LokiWorkingWith {
    /// The people the answer `body` gives: `{"value": [{"aadObjectId", "fullName", "email", "userPrincipalName",
    /// "profilePictureUrl", "permanentEntryId"}, …]}` (the last two not read), at most [`WORKING_WITH_LIMIT`]; one
    /// without an object id is left out.
    ///
    /// # Errors
    /// [`LokiAnswerError::Malformed`] unless the body is an object with a `value` list.
    pub fn read_answer(body: &[u8]) -> Result<Vec<PersonaRef>, LokiAnswerError> {
        let answer = object(body)?;
        let Some(Value::Array(items)) = answer.get("value") else {
            return Err(LokiAnswerError::Malformed);
        };
        Ok(people(items, WORKING_WITH_LIMIT))
    }
}

/// A free/busy status, in Graph's documented words (`freeBusyStatus`; only `Free` was captured), read in any case.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FreeBusyStatus {
    Free,
    Tentative,
    Busy,
    /// `oof`: out of office.
    OutOfOffice,
    WorkingElsewhere,
    /// Graph's own `unknown`.
    Unknown,
    /// A word Graph doesn't document, as given.
    Other(String),
}

impl FreeBusyStatus {
    /// The status `word` names.
    #[must_use]
    pub fn of(word: &str) -> Self {
        match word.to_ascii_lowercase().as_str() {
            "free" => Self::Free,
            "tentative" => Self::Tentative,
            "busy" => Self::Busy,
            "oof" => Self::OutOfOffice,
            "workingelsewhere" => Self::WorkingElsewhere,
            "unknown" => Self::Unknown,
            _ => Self::Other(word.to_owned()),
        }
    }
}

/// A time of day, as a working day's start or end is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClockTime {
    pub hour: u8,
    pub minute: u8,
}

impl ClockTime {
    /// `HH:MM[:SS[.fraction]]` (`08:00:00.0000000`), the seconds dropped.
    fn read(text: &str) -> Option<Self> {
        let mut parts = text.trim().split(':');
        let hour: u8 = parts.next()?.parse().ok()?;
        let minute: u8 = parts.next()?.parse().ok()?;
        (hour < 24 && minute < 60).then_some(Self { hour, minute })
    }
}

/// A person's schedule as Loki gave it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    /// `nextAvailability.currentStatus`.
    pub current: Option<FreeBusyStatus>,
    /// `nextAvailability.nextStatus`.
    pub next: Option<FreeBusyStatus>,
    /// `nextAvailability.utcDateTime`, ms since the epoch: when `current` gives way to `next`; `None` for
    /// `0001-01-01T00:00:00` (no time) or a time in another form.
    pub until_ms: Option<i64>,
    /// `workingHoursCalendar.daysOfWeek`, in the order given.
    pub work_days: Vec<Weekday>,
    /// `workingHoursCalendar.startTime` and `endTime`.
    pub work_start: Option<ClockTime>,
    pub work_end: Option<ClockTime>,
    /// `workingHoursCalendar.timeZone.offset` (hours, fractional), in minutes east of UTC.
    pub utc_offset_minutes: Option<i32>,
    /// `workingHoursCalendar.timeZone.name` (`GMT+03:00`).
    pub zone_name: Option<String>,
}

/// An ISO time in UTC (`…Z`, an offset, or none: UTC) as ms since the epoch; `None` for `0001-01-01…` (no time) or
/// another form.
pub(crate) fn utc_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.starts_with("0001-01-01") {
        return None;
    }
    let zoned = text.ends_with(['Z', 'z'])
        || text
            .split_once(['T', 't'])
            .is_some_and(|(_, clock)| clock.contains(['+', '-']));
    let time = if zoned {
        OffsetDateTime::parse(text, &Rfc3339)
    } else {
        OffsetDateTime::parse(&format!("{text}Z"), &Rfc3339)
    }
    .ok()?;
    i64::try_from(time.unix_timestamp_nanos() / 1_000_000).ok()
}

/// A zone's offset in hours (`3.0`, `5.5`, `-4.0`) as whole minutes; `None` for anything beyond a day.
fn offset_minutes(value: Option<&Value>) -> Option<i32> {
    let hours = value?.as_f64()?;
    let minutes = (hours * 60.0).round();
    // a real zone is within ±14 h
    (minutes.abs() <= 24.0 * 60.0).then(|| {
        #[expect(clippy::cast_possible_truncation, reason = "checked to be within a day just above")]
        let minutes = minutes as i32;
        minutes
    })
}

impl LokiSchedule {
    /// The schedule the answer `body` gives: `{"nextAvailability": {"utcDateTime", "currentStatus", "nextStatus"},
    /// "workingHoursCalendar": {"daysOfWeek", "startTime", "endTime", "timeZone": {"offset", "name", …}}}`; either
    /// part may be missing, and a field of another form is none.
    ///
    /// # Errors
    /// [`LokiAnswerError::Malformed`] unless the body is an object.
    pub fn read_answer(body: &[u8]) -> Result<Schedule, LokiAnswerError> {
        let answer = object(body)?;
        let next = answer.get("nextAvailability");
        let field = |key: &str| text(next.and_then(|next| next.get(key)));
        let hours = answer.get("workingHoursCalendar");
        let hour_field = |key: &str| hours.and_then(|hours| hours.get(key));
        let zone = hour_field("timeZone");
        Ok(Schedule {
            current: field("currentStatus").as_deref().map(FreeBusyStatus::of),
            next: field("nextStatus").as_deref().map(FreeBusyStatus::of),
            until_ms: field("utcDateTime").as_deref().and_then(utc_ms),
            work_days: hour_field("daysOfWeek")
                .and_then(Value::as_array)
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .filter_map(Weekday::read)
                .collect(),
            work_start: text(hour_field("startTime")).as_deref().and_then(ClockTime::read),
            work_end: text(hour_field("endTime")).as_deref().and_then(ClockTime::read),
            utc_offset_minutes: offset_minutes(zone.and_then(|zone| zone.get("offset"))),
            zone_name: text(zone.and_then(|zone| zone.get("name"))),
        })
    }
}

/// Whether a person's automatic replies are on, as Loki gave it (`outOfOfficeState`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum OutOfOfficeState {
    /// `Disabled`: they are off (the only state captured).
    Disabled,
    /// Another word, as given: not captured, so not read as on or off.
    Other(String),
}

impl LokiOutOfOffice {
    /// The state the answer `body` gives: `{"outOfOfficeState", "externalAudience", "emailAddress"}` (only the first
    /// read).
    ///
    /// # Errors
    /// [`LokiAnswerError::Malformed`] unless the body is an object with an `outOfOfficeState` string.
    pub fn read_answer(body: &[u8]) -> Result<OutOfOfficeState, LokiAnswerError> {
        let answer = object(body)?;
        let state = text(answer.get("outOfOfficeState")).ok_or(LokiAnswerError::Malformed)?;
        Ok(if state.eq_ignore_ascii_case("disabled") {
            OutOfOfficeState::Disabled
        } else {
            OutOfOfficeState::Other(state)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_time_without_an_offset_is_utc_and_the_first_day_is_none() {
        assert_eq!(utc_ms("1970-01-01T00:00:01"), Some(1000));
        assert_eq!(utc_ms("1970-01-01T00:00:01.5000000"), Some(1500));
        assert_eq!(utc_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(utc_ms("1970-01-01T01:00:01+01:00"), Some(1000));
        assert_eq!(utc_ms("0001-01-01T00:00:00"), None);
        assert_eq!(utc_ms("0001-01-01T00:00:00Z"), None);
        assert_eq!(utc_ms("tomorrow"), None);
    }

    #[test]
    fn a_zone_offset_is_whole_minutes_within_a_day() {
        let minutes = |value: Value| offset_minutes(Some(&value));
        assert_eq!(minutes(serde_json::json!(3.0)), Some(180));
        assert_eq!(minutes(serde_json::json!(5.5)), Some(330));
        assert_eq!(minutes(serde_json::json!(-4)), Some(-240));
        assert_eq!(minutes(serde_json::json!(48)), None);
        assert_eq!(minutes(serde_json::json!("3")), None);
        assert_eq!(offset_minutes(None), None);
    }

    #[test]
    fn a_clock_time_is_hours_and_minutes() {
        assert_eq!(
            ClockTime::read("08:00:00.0000000"),
            Some(ClockTime { hour: 8, minute: 0 })
        );
        assert_eq!(ClockTime::read("17:30"), Some(ClockTime { hour: 17, minute: 30 }));
        assert_eq!(ClockTime::read("24:00"), None);
        assert_eq!(ClockTime::read("8"), None);
    }
}
