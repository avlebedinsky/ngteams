//! A meeting's start as the calling service pushes it (`docs/protocol/calling.md`, "When a meeting starts"): a delivery
//! at the calling endpoint's root, `{"evt": 128, "gp": <base64 JSON>}` or `{"evt": 128, "cp": <base64 of the JSON,
//! deflated as zlib or gzip>}`, read as the web client's bundle reads it. Only the fields the banner uses are kept; a
//! value in a form the bundle doesn't read is left out.

use std::io::Read as _;

use base64::Engine as _;
use serde_json::Value;

/// The calling push's event number of a meeting's start (the bundle's `MEETING_STARTED_NOTIFICATION`).
pub const MEETING_STARTED_EVENT: u64 = 128;

/// The largest payload a deflated one is inflated to.
const MAX_PAYLOAD: u64 = 256 * 1024;

/// The first bytes of a gzip stream.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// What started, as its `notificationType` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartKind {
    /// `MeetingStart`: someone started the meeting.
    Meeting,
    /// `MeetingStartFromLobby`: someone waits in the meeting's lobby, and nobody who admits is in it.
    Lobby,
    /// `HuddleMeetingStart`: a group chat's Meet now.
    Huddle,
}

/// A meeting's start, pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingStartPush {
    pub kind: StartKind,
    /// The call (`callId`): each call's start shows once.
    pub call_id: String,
    /// The meeting's chat, or a channel (`threadId`).
    pub thread: String,
    /// A channel's meeting's post (`messageId`; `"0"` for a meeting with its own chat, read as `None`).
    pub message: Option<String>,
    /// The meeting's title (`subject`).
    pub subject: Option<String>,
    /// Who started it, by MRI: `groupCallInitiator` when the push names their name, else `firstUserToJoinModality`.
    pub starter: Option<String>,
    /// The starter's name as the push gives it (`groupCallInitiatorDisplayName`).
    pub starter_name: Option<String>,
    /// The meeting's organizer and tenant (`meetingInfo`).
    pub organizer: Option<String>,
    pub tenant: Option<String>,
}

/// The meeting start `body` (a calling delivery's) pushes; `None` for any other delivery or one that doesn't read.
#[must_use]
pub fn read(body: &[u8]) -> Option<MeetingStartPush> {
    let body: Value = serde_json::from_slice(body).ok()?;
    if body.get("evt")?.as_u64()? != MEETING_STARTED_EVENT {
        return None;
    }
    let payload = payload(&body)?;
    let text = |pointer: &str| {
        payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let kind = match text("/notificationType")?.as_str() {
        "MeetingStart" => StartKind::Meeting,
        "MeetingStartFromLobby" => StartKind::Lobby,
        "HuddleMeetingStart" => StartKind::Huddle,
        _ => return None,
    };
    let starter_name = text("/groupCallInitiatorDisplayName");
    let starter = if starter_name.is_some() {
        text("/groupCallInitiator")
    } else {
        text("/firstUserToJoinModality")
    };
    Some(MeetingStartPush {
        kind,
        call_id: text("/callId")?,
        thread: text("/threadId")?,
        message: text("/messageId").filter(|message| message != "0"),
        subject: text("/subject"),
        starter,
        starter_name,
        organizer: text("/meetingInfo/organizerId"),
        tenant: text("/meetingInfo/tenantId"),
    })
}

/// The payload: `gp` as base64 JSON (or as the JSON object itself), else `cp` inflated (gzip by its magic, else zlib,
/// as the bundle's inflate tells them apart).
fn payload(body: &Value) -> Option<Value> {
    let base64 = base64::engine::general_purpose::STANDARD;
    let bytes = match (body.get("gp"), body.get("cp")) {
        (Some(Value::Object(gp)), _) => return Some(Value::Object(gp.clone())),
        (Some(Value::String(gp)), _) => base64.decode(gp).ok()?,
        (None, Some(Value::String(cp))) => {
            let deflated = base64.decode(cp).ok()?;
            let reader: Box<dyn std::io::Read + '_> = if deflated.starts_with(&GZIP_MAGIC) {
                Box::new(flate2::read::GzDecoder::new(deflated.as_slice()))
            } else {
                Box::new(flate2::read::ZlibDecoder::new(deflated.as_slice()))
            };
            let mut inflated = Vec::new();
            reader.take(MAX_PAYLOAD).read_to_end(&mut inflated).ok()?;
            inflated
        }
        _ => return None,
    };
    serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use serde_json::json;

    use super::*;

    fn gp(payload: &Value) -> Vec<u8> {
        let gp = base64::engine::general_purpose::STANDARD.encode(payload.to_string());
        json!({"evt": 128, "gp": gp}).to_string().into_bytes()
    }

    fn started() -> Value {
        json!({
            "_synthetic": true,
            "notificationType": "MeetingStart",
            "callId": "made-up-call",
            "threadId": "19:meeting_MadeUp@thread.v2",
            "messageId": "0",
            "subject": "Weekly sync",
            "groupCallInitiator": "8:orgid:00000000-0000-4000-8000-00000000000a",
            "groupCallInitiatorDisplayName": "Ada",
            "firstUserToJoinModality": "8:orgid:00000000-0000-4000-8000-00000000000c",
            "meetingInfo": {"tenantId": "made-up-tenant", "organizerId": "made-up-organizer"}
        })
    }

    #[test]
    fn a_meeting_s_start_reads_from_gp() {
        let read = read(&gp(&started())).unwrap();
        assert_eq!(
            read,
            MeetingStartPush {
                kind: StartKind::Meeting,
                call_id: "made-up-call".into(),
                thread: "19:meeting_MadeUp@thread.v2".into(),
                message: None,
                subject: Some("Weekly sync".into()),
                starter: Some("8:orgid:00000000-0000-4000-8000-00000000000a".into()),
                starter_name: Some("Ada".into()),
                organizer: Some("made-up-organizer".into()),
                tenant: Some("made-up-tenant".into()),
            }
        );
    }

    #[test]
    fn without_the_starter_s_name_the_first_to_join_started_it() {
        let mut payload = started();
        payload.as_object_mut().unwrap().remove("groupCallInitiatorDisplayName");
        payload["messageId"] = json!("1700000000000");
        payload["notificationType"] = json!("MeetingStartFromLobby");
        let read = read(&gp(&payload)).unwrap();
        assert_eq!(read.kind, StartKind::Lobby);
        assert_eq!(read.message.as_deref(), Some("1700000000000"));
        assert_eq!(
            read.starter.as_deref(),
            Some("8:orgid:00000000-0000-4000-8000-00000000000c")
        );
        assert_eq!(read.starter_name, None);
    }

    #[test]
    fn a_deflated_or_inline_payload_reads_too() {
        let mut deflater = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        deflater.write_all(started().to_string().as_bytes()).unwrap();
        let cp = base64::engine::general_purpose::STANDARD.encode(deflater.finish().unwrap());
        let body = json!({"evt": 128, "cp": cp}).to_string();
        assert_eq!(read(body.as_bytes()).unwrap().call_id, "made-up-call");
        let mut gzipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzipped.write_all(started().to_string().as_bytes()).unwrap();
        let cp = base64::engine::general_purpose::STANDARD.encode(gzipped.finish().unwrap());
        let body = json!({"evt": 128, "cp": cp}).to_string();
        assert_eq!(read(body.as_bytes()).unwrap().call_id, "made-up-call");
        let inline = json!({"evt": 128, "gp": started()}).to_string();
        assert_eq!(read(inline.as_bytes()).unwrap().kind, StartKind::Meeting);
    }

    /// A `cp` whose JSON inflates past [`MAX_PAYLOAD`]: it is cut there, so it doesn't read.
    fn bomb() -> Vec<u8> {
        let mut payload = started();
        payload["subject"] = json!(" ".repeat(usize::try_from(MAX_PAYLOAD).unwrap()));
        let mut gzipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzipped.write_all(payload.to_string().as_bytes()).unwrap();
        let cp = base64::engine::general_purpose::STANDARD.encode(gzipped.finish().unwrap());
        json!({"evt": 128, "cp": cp}).to_string().into_bytes()
    }

    #[test]
    fn other_events_and_unreadable_payloads_read_as_none() {
        let mut huddle = started();
        huddle["notificationType"] = json!("HuddleMeetingStart");
        assert_eq!(read(&gp(&huddle)).unwrap().kind, StartKind::Huddle);
        let mut other = started();
        other["notificationType"] = json!("SomethingElse");
        let mut no_call = started();
        no_call.as_object_mut().unwrap().remove("callId");
        for body in [
            gp(&other),
            gp(&no_call),
            json!({"evt": 107, "gp": "e30="}).to_string().into_bytes(),
            json!({"evt": 128, "gp": "not base64!"}).to_string().into_bytes(),
            json!({"evt": 128, "cp": "bm90IGRlZmxhdGVk"}).to_string().into_bytes(),
            json!({"evt": 128}).to_string().into_bytes(),
            bomb(),
            b"not json".to_vec(),
        ] {
            assert!(read(&body).is_none(), "{}", String::from_utf8_lossy(&body));
        }
    }
}
