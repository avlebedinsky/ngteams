//! A call's meeting options on the middle tier (`docs/protocol/calling.md`, "More of a running call": Controls), as
//! captured 2026-11-06: `GET /api/mt/part/<partition>/beta/meetings/v1/options/<tenant>/<organizer>/<chat>/0/0` lists
//! them, a `POST` of `{"options": [{"name", "type", "currentValue"}]}` changes one and answers it as it now is. Only the
//! organizer of the running call in that chat (its starter) reads or changes them; everyone in the call is told the
//! change at their `conversationUpdate` callbacks (`meetingDetails.meetingCapability`), and the roster again (while only
//! the organizer may present, everyone else is an attendee).
//!
//! **The mock's own, not captured**: only the four options Controls change are kept, each with the values the capture
//! listed.

use percent_encoding::percent_decode_str;
use serde_json::{Map, Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::calling::MockCall;
use crate::chatsvc::{json as json_answer, status};
use crate::world::State;

/// The options a call starts with, as the capture's read listed them.
pub(crate) fn defaults() -> Map<String, Value> {
    let mut options = Map::new();
    for (name, value) in [
        ("AutoAdmittedUsers", "EveryoneInCompany"),
        ("PresenterOption", "Everyone"),
        ("AllowMeetingChat", "Enabled"),
        ("AttendeeRestrictions", "UnRestricted"),
    ] {
        options.insert(name.to_owned(), json!(value));
    }
    options
}

/// Whether only the organizer may present in `call`.
pub(crate) fn presenters_only(call: &MockCall) -> bool {
    call.options.get("PresenterOption").and_then(Value::as_str) == Some("OrganizerOnly")
}

/// Each option's type and the values it takes, as the capture listed them.
fn kind(name: &str) -> Option<(&'static str, &'static [&'static str])> {
    Some(match name {
        "AutoAdmittedUsers" => (
            "List",
            &[
                "Everyone",
                "EveryoneInSameAndFederatedCompany",
                "EveryoneInCompany",
                "EveryoneInCompanyExcludingGuests",
                "OrganizerOnly",
            ],
        ),
        "PresenterOption" => (
            "List",
            &["Everyone", "EveryoneInCompany", "SpecifiedPeople", "OrganizerOnly"],
        ),
        "AllowMeetingChat" => ("List", &["Enabled", "Disabled", "InMeeting"]),
        "AttendeeRestrictions" => ("BitMask", &["UnRestricted", "DisableAudio", "DisableVideo"]),
        _ => return None,
    })
}

/// One option as a read or an answer lists it.
fn listed(name: &str, value: &Value) -> Value {
    let (kind, values) = kind(name).unwrap_or(("List", &[]));
    json!({"values": values, "currentValue": value, "name": name, "type": kind, "isDisabled": false})
}

/// The options request `request` is, answered; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let segments: Vec<String> = request
        .url
        .path_segments()?
        .map(|segment| percent_decode_str(segment).decode_utf8().ok().map(String::from))
        .collect::<Option<_>>()?;
    let (organizer, chat) = match segments.as_slice() {
        [
            api,
            mt,
            part,
            partition,
            beta,
            meetings,
            v1,
            options,
            _tenant,
            organizer,
            chat,
            zero,
            zero2,
        ] if api == "api"
            && mt == "mt"
            && part == "part"
            && partition == crate::calendar::PARTITION
            && (beta, meetings, v1, options)
                == (
                    &"beta".to_owned(),
                    &"meetings".to_owned(),
                    &"v1".to_owned(),
                    &"options".to_owned(),
                )
            && zero == "0"
            && zero2 == "0" =>
        {
            (organizer.clone(), chat.clone())
        }
        _ => return None,
    };
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.to_owned());
    let Some(account) = token.and_then(|token| state.account_by_token(&token).map(|account| account.mri.clone()))
    else {
        return Some(status(401));
    };
    let monarch = request
        .url
        .query_pairs()
        .any(|(name, value)| name == "monarch" && value == "true");
    let allowed = account == format!("8:orgid:{organizer}");
    let index = state
        .calling
        .calls
        .iter()
        .rposition(|call| call.group_chat.as_deref() == Some(chat.as_str()) && call.caller == account);
    let (Some(index), true, true) = (index, allowed, monarch) else {
        return Some(status(403));
    };
    Some(match request.method.as_str() {
        "GET" => {
            let options: Vec<Value> = state.calling.calls[index]
                .options
                .iter()
                .map(|(name, value)| listed(name, value))
                .collect();
            json_answer(200, &json!({"options": options}))
        }
        "POST" => change(state, index, &request.body),
        _ => status(405),
    })
}

/// A `POST` of one option to call `index`'s options: kept, answered as it now is, and told to everyone in the call.
fn change(state: &mut State, index: usize, body: &[u8]) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let Some([option]) = body.get("options").and_then(Value::as_array).map(Vec::as_slice) else {
        return status(400);
    };
    let (Some(name), Some(value)) = (
        option.get("name").and_then(Value::as_str),
        option.get("currentValue").and_then(Value::as_str),
    ) else {
        return status(400);
    };
    let Some((kind, values)) = kind(name) else {
        return status(400);
    };
    let fits = value.split(',').all(|part| values.contains(&part));
    if option.get("type").and_then(Value::as_str) != Some(kind) || !fits {
        return status(400);
    }
    state.calling.calls[index].options.insert(name.to_owned(), json!(value));
    let options = &state.calling.calls[index].options;
    let told = |name: &str| options.get(name).cloned().unwrap_or(Value::Null);
    // the restrictions are told as the service tells them: both as `DisableAv`
    let restrictions = match told("AttendeeRestrictions").as_str() {
        Some("DisableAudio,DisableVideo") => json!("DisableAv"),
        _ => told("AttendeeRestrictions"),
    };
    let capability = json!({
        "autoAdmittedUsers": told("AutoAdmittedUsers"),
        "presenterOption": told("PresenterOption"),
        "attendeeRestrictions": restrictions,
    });
    let answer = json!({"success": true, "options": [listed(name, &json!(value))]});
    crate::roster::options_changed(state, index, &capability);
    json_answer(200, &answer)
}
