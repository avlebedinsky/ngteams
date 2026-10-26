//! What the call's services send back, read as the captures show it (`docs/protocol/calling.md`,
//! `docs/protocol/calling-echo.md`; fixtures `tests/fixtures/calling/`): an incoming call's invitation, the
//! conversation a start or a join opens, an `attach`'s and an `accept`'s answers, the acceptance of a call the account
//! placed, and a call's end. Only the parts the machine acts on are read; a link is kept only when it reads as a
//! [`CallLink`] (exactly as handed out), and a value in a form no capture showed is left out, never guessed.

use base64::Engine as _;
use serde_json::Value;
use teams_fence::{INVITATION_EVENTS, Mri};
use teams_proto::{CallLink, MediaContent};

/// An incoming call's invitation (`{"evt": 107 | 109, "gp": <base64 JSON>}` at the calling endpoint's root), read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Invitation {
    pub(crate) caller: Mri,
    pub(crate) caller_name: Option<String>,
    pub(crate) callee: Mri,
    /// The group chat a group call (`evt` 109) is in.
    pub(crate) group_chat: Option<String>,
    /// `conversationInvitation.conversationController`: the call's conversation.
    pub(crate) conversation: CallLink,
    /// `callNotification.links.attach`.
    pub(crate) attach: CallLink,
    /// The caller's offer (a 1:1 call's) or the call's mixer's (a group call's).
    pub(crate) offer: Option<MediaContent>,
}

/// The invitation `body` is; `None` for any other body or one that doesn't read.
pub(crate) fn invitation(body: &[u8]) -> Option<Invitation> {
    let body: Value = serde_json::from_slice(body).ok()?;
    if !INVITATION_EVENTS.contains(&body.get("evt")?.as_u64()?) {
        return None;
    }
    let gp = base64::engine::general_purpose::STANDARD
        .decode(body.get("gp")?.as_str()?)
        .ok()?;
    let gp: Value = serde_json::from_slice(&gp).ok()?;
    let notification = gp.get("callNotification")?;
    let text = |at: &Value, pointer: &str| at.pointer(pointer).and_then(Value::as_str).map(str::to_owned);
    Some(Invitation {
        caller: Mri::new(text(notification, "/from/id")?),
        caller_name: text(notification, "/from/displayName"),
        callee: Mri::new(text(notification, "/to/id")?),
        group_chat: text(&gp, "/groupChat/threadId"),
        conversation: link(&gp, "/conversationInvitation/conversationController")?,
        attach: link(notification, "/links/attach")?,
        offer: notification.get("mediaContent").and_then(media),
    })
}

/// The link at `pointer`, if it reads as a [`CallLink`].
fn link(at: &Value, pointer: &str) -> Option<CallLink> {
    CallLink::parse(at.pointer(pointer)?.as_str()?)
}

/// An offer or an answer: its blob and content type, its media leg when given.
pub(crate) fn media(content: &Value) -> Option<MediaContent> {
    let blob = content.get("blob")?.as_str()?;
    let content_type = content.get("contentType")?.as_str()?;
    let mut media = MediaContent::new(blob, content_type);
    media.media_leg_id = content.get("mediaLegId").and_then(Value::as_str).map(str::to_owned);
    Some(media)
}

/// The links of a call's conversation the machine sends to, as an answer hands them out (each where it reads).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Conversation {
    pub(crate) leave: Option<CallLink>,
    pub(crate) endpoint_metadata: Option<CallLink>,
    pub(crate) endpoint_state: Option<CallLink>,
    /// `addParticipantAndModality`: the `…/add` an `add` posts to.
    pub(crate) add: Option<CallLink>,
}

impl Conversation {
    fn of(links: &Value) -> Self {
        let at = |name: &str| links.get(name).and_then(Value::as_str).and_then(CallLink::parse);
        Self {
            leave: at("leave"),
            endpoint_metadata: at("updateEndpointMetadata"),
            endpoint_state: at("updateEndpointState"),
            add: at("addParticipantAndModality"),
        }
    }
}

/// The conversation a start's or a join's answer is (`{"conversationController", "links": {…}}`); `None` unless it
/// names a conversation controller.
pub(crate) fn conversation(body: &[u8]) -> Option<Conversation> {
    let body: Value = serde_json::from_slice(body).ok()?;
    link(&body, "/conversationController")?;
    Some(Conversation::of(body.get("links")?))
}

/// What an `attach`'s answer hands out: the call leg's `progress`, `acceptance` and `callLeg` (the decline's) links,
/// and the links of the conversation its `join` subscribed to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Attached {
    pub(crate) progress: CallLink,
    pub(crate) acceptance: CallLink,
    pub(crate) call_leg: CallLink,
    pub(crate) conversation: Conversation,
}

/// The `attach` answer `body`, its `join` the subscription to `conversation`; `None` unless the leg's three links read.
pub(crate) fn attached(body: &[u8], conversation: &CallLink) -> Option<Attached> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let links = body.pointer("/callInvitation/links")?;
    let subscription = body
        .get("additionalActionResponses")?
        .as_array()?
        .iter()
        .find(|response| response.get("url").and_then(Value::as_str) == Some(conversation.as_str()))
        .and_then(|response| response.pointer("/output/links"))
        .map(Conversation::of)
        .unwrap_or_default();
    Some(Attached {
        progress: link(links, "/progress")?,
        acceptance: link(links, "/acceptance")?,
        call_leg: link(links, "/callLeg")?,
        conversation: subscription,
    })
}

/// How often the call's services want a keep-alive, in seconds (`callKeepAliveInterval`), from an acceptance (`{"callAcceptance":
/// {…}}`) or an `accept`'s answer (`{"callAcceptanceAcknowledgement": {…}}`); `None` when it isn't a positive count.
fn keep_alive(at: &Value) -> Option<u64> {
    at.get("callKeepAliveInterval")?.as_u64().filter(|seconds| *seconds > 0)
}

/// What an `accept`'s answer (`{"callAcceptanceAcknowledgement": {…}}`) hands out: the keep-alive interval and the
/// call controller's `mediaRenegotiation` link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AcceptAnswer {
    pub(crate) keep_alive: Option<u64>,
    pub(crate) renegotiation: Option<CallLink>,
    /// The call leg's own link: where the keep-alive goes.
    pub(crate) call_leg: Option<CallLink>,
    /// Where a group call's media lines are described again (`updateMediaDescriptions`) and its send capabilities
    /// given (`applyChannelParameters`): the camera turned on or off.
    pub(crate) group_media: GroupMediaLinks,
}

/// Where a group call's media lines are described (`updateMediaDescriptions`) and its send capabilities given
/// (`applyChannelParameters`), as an acceptance or an `accept`'s answer hands them out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GroupMediaLinks {
    pub(crate) descriptions: Option<CallLink>,
    pub(crate) channel_parameters: Option<CallLink>,
}

impl GroupMediaLinks {
    fn of(at: &Value) -> Self {
        Self {
            descriptions: link(at, "/links/updateMediaDescriptions"),
            channel_parameters: link(at, "/links/applyChannelParameters"),
        }
    }
}

/// An `accept`'s answer, as far as it reads.
pub(crate) fn accept_answer(body: &[u8]) -> AcceptAnswer {
    let Some(acknowledgement) = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.get("callAcceptanceAcknowledgement").cloned())
    else {
        return AcceptAnswer::default();
    };
    AcceptAnswer {
        keep_alive: keep_alive(&acknowledgement),
        renegotiation: link(&acknowledgement, "/links/mediaRenegotiation"),
        call_leg: link(&acknowledgement, "/links/callLeg"),
        group_media: GroupMediaLinks::of(&acknowledgement),
    }
}

/// The acceptance of a call the account placed or joined: its `acknowledgement` link (for an acknowledgement by
/// request), the answer to the account's offer, the keep-alive interval, the call controller's `mediaRenegotiation`
/// link, and whether the callee's voicemail took the call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Acceptance {
    pub(crate) acknowledgement: Option<CallLink>,
    pub(crate) answer: Option<MediaContent>,
    pub(crate) keep_alive: Option<u64>,
    pub(crate) renegotiation: Option<CallLink>,
    /// The call leg's own link: where the keep-alive goes.
    pub(crate) call_leg: Option<CallLink>,
    /// The callee's voicemail took the call (`acceptedBy.endpointType` `voicemail`, as captured for a 1:1 call the
    /// callee declined or didn't answer).
    pub(crate) voicemail: bool,
    /// A group call's media links.
    pub(crate) group_media: GroupMediaLinks,
}

/// The acceptance delivery `body`; `None` unless it is a `callAcceptance`.
pub(crate) fn acceptance(body: &[u8]) -> Option<Acceptance> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let acceptance = body.get("callAcceptance")?;
    Some(Acceptance {
        acknowledgement: link(acceptance, "/links/acknowledgement"),
        answer: acceptance.get("mediaContent").and_then(media),
        keep_alive: keep_alive(acceptance),
        renegotiation: link(acceptance, "/links/mediaRenegotiation"),
        call_leg: link(acceptance, "/links/callLeg"),
        voicemail: acceptance.pointer("/acceptedBy/endpointType").and_then(Value::as_str) == Some("voicemail"),
        group_media: GroupMediaLinks::of(acceptance),
    })
}

/// One participant of a call as its roster lists them (a `rosterUpdate` delivered at the conversation's callback:
/// `{"participants": {<MRI>: {"version", "state", "details": {"displayName"}, "endpoints": {<id>: {"call":
/// {"mediaStreams": [{"type", "label", "sourceId", "direction"}]}}}}}, "type": "Delta", "sequenceNumber"}`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RosterEntry {
    pub(crate) mri: Mri,
    /// Counts up per participant: an entry delivered again, or older, changes nothing.
    pub(crate) version: u64,
    /// `active` while in the call; `inactive` once gone (its endpoints empty).
    pub(crate) active: bool,
    pub(crate) name: Option<String>,
    /// The main audio's and the main video's source ids, and whether the main video goes out (`sendrecv` or
    /// `sendonly`): of the first endpoint in the call that lists its media.
    pub(crate) audio_source: Option<i64>,
    pub(crate) video_source: Option<i64>,
    pub(crate) video_on: bool,
}

/// The roster update `body`'s participants, each where it reads (an MRI and a version); `None` unless it is one.
pub(crate) fn roster(body: &[u8]) -> Option<Vec<RosterEntry>> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let participants = body.get("participants")?.as_object()?;
    Some(
        participants
            .iter()
            .filter_map(|(mri, entry)| roster_entry(mri, entry))
            .collect(),
    )
}

fn roster_entry(mri: &str, entry: &Value) -> Option<RosterEntry> {
    let version = entry.get("version")?.as_u64()?;
    let streams = entry.get("endpoints").and_then(Value::as_object).and_then(|endpoints| {
        endpoints
            .values()
            .find_map(|endpoint| endpoint.pointer("/call/mediaStreams")?.as_array())
    });
    let stream = |label: &str| {
        streams?
            .iter()
            .find(|stream| stream.get("label").and_then(Value::as_str) == Some(label))
    };
    let video = stream("main-video");
    Some(RosterEntry {
        mri: Mri::new(mri),
        version,
        active: entry.get("state").and_then(Value::as_str) == Some("active") && streams.is_some(),
        name: entry
            .pointer("/details/displayName")
            .and_then(Value::as_str)
            .map(str::to_owned),
        audio_source: stream("main-audio").and_then(|audio| audio.get("sourceId")?.as_i64()),
        video_source: video.and_then(|video| video.get("sourceId")?.as_i64()),
        video_on: video
            .and_then(|video| video.get("direction")?.as_str())
            .is_some_and(|direction| matches!(direction, "sendrecv" | "sendonly")),
    })
}

/// A renegotiation delivered at the account's `mediaRenegotiation` callback (`{"mediaNegotiation": {"mediaContent",
/// "links": {"mediaAnswer", …}}}`): its offer and the link the answer goes to; `None` unless both read.
pub(crate) fn negotiation(body: &[u8]) -> Option<(MediaContent, CallLink)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let negotiation = body.get("mediaNegotiation")?;
    Some((
        media(negotiation.get("mediaContent")?)?,
        link(negotiation, "/links/mediaAnswer")?,
    ))
}

/// The answer to the account's renegotiation, delivered at its `mediaAnswer` callback (`{"mediaAnswer":
/// {"mediaContent", "links": {"mediaAcknowledgement"}}}`): the answer, and the link its acknowledgement goes to where
/// it reads; `None` without a readable answer.
pub(crate) fn media_answer(body: &[u8]) -> Option<(MediaContent, Option<CallLink>)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let answer = body.get("mediaAnswer")?;
    Some((
        media(answer.get("mediaContent")?)?,
        link(answer, "/links/mediaAcknowledgement"),
    ))
}

/// Whether a rejection of the account's renegotiation (`{"mediaNegotiationFailure": {"code": 491, "phrase":
/// "Negotiation is in progress.", "resultCategories": ["ExpectedError"]}}`, `ab-call --call video`, when both pages
/// turned their cameras on together) says another negotiation of the call was in progress: the renegotiation may go
/// again once it is over.
pub(crate) fn negotiation_in_progress(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.pointer("/mediaNegotiationFailure/code").and_then(Value::as_i64))
        == Some(491)
}

/// How the call's services ended a call or a ring: the `callEnd` (or a conversation's end) as delivered. Each part only
/// where the delivery gives it; nothing is read into a code no capture named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteEnd {
    pub code: Option<i64>,
    pub sub_code: Option<i64>,
    pub phrase: Option<String>,
}

/// The end `body` delivers (`{"callEnd": {…}}`, or a conversation's end); every part `None` when it doesn't read.
pub(crate) fn remote_end(body: &[u8]) -> RemoteEnd {
    let Ok(body) = serde_json::from_slice::<Value>(body) else {
        return RemoteEnd::default();
    };
    // a conversation's end may also come bare (`{"code": 409, "subCode", "phrase", "conversationUrl", …}`, a start in a
    // chat whose call still runs: `docs/protocol/calling.md`, "Joining a running call")
    let end = body
        .get("callEnd")
        .or_else(|| body.get("conversationEnd"))
        .unwrap_or(&body);
    RemoteEnd {
        code: end.get("code").and_then(Value::as_i64),
        sub_code: end.get("subCode").and_then(Value::as_i64),
        phrase: end.get("phrase").and_then(Value::as_str).map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use serde_json::json;

    use super::{accept_answer, acceptance, attached, conversation, invitation, remote_end};

    const PROXY: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/ep";
    const CALLER: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
    const CALLEE: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";

    fn conv(rest: &str) -> String {
        format!("{PROXY}/conv-zz-01-prod-aks.conv.skype.com/conv/Conversation1{rest}?i=10-0-0-2&e=1")
    }

    fn cc(kind: &str, op: &str) -> String {
        format!("{PROXY}/cc-zz-01-prod-aks.cc.skype.com/cc/v1/{kind}/Call1/27/t/2/{op}?i=10-0-0-1")
    }

    fn invitation_body(evt: u64, gp: &serde_json::Value) -> Vec<u8> {
        let gp = base64::engine::general_purpose::STANDARD.encode(gp.to_string());
        json!({"evt": evt, "gp": gp}).to_string().into_bytes()
    }

    fn gp() -> serde_json::Value {
        json!({
            "callNotification": {
                "from": {"id": CALLER, "displayName": "Lab A"},
                "to": {"id": CALLEE},
                "links": {"attach": cc("forked", "attach")},
                "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0", "mediaLegId": "LEG"},
            },
            "conversationInvitation": {"conversationController": conv("")},
        })
    }

    #[test]
    fn an_invitation_reads_its_parties_links_and_offer() {
        let read = invitation(&invitation_body(107, &gp())).unwrap();
        assert_eq!(read.caller.as_str(), CALLER);
        assert_eq!(read.caller_name.as_deref(), Some("Lab A"));
        assert_eq!(read.callee.as_str(), CALLEE);
        assert_eq!(read.group_chat, None);
        assert_eq!(read.conversation.as_str(), conv(""));
        assert_eq!(read.attach.as_str(), cc("forked", "attach"));
        let offer = read.offer.unwrap();
        assert_eq!(
            (offer.blob.as_str(), offer.media_leg_id.as_deref()),
            ("v=0\r\n", Some("LEG"))
        );
        let mut group = gp();
        group["groupChat"] = json!({"threadId": "19:group@thread.v2", "messageId": "0"});
        let read = invitation(&invitation_body(109, &group)).unwrap();
        assert_eq!(read.group_chat.as_deref(), Some("19:group@thread.v2"));
    }

    #[test]
    fn a_body_that_is_no_readable_invitation_is_none() {
        assert!(
            invitation(&invitation_body(115, &gp())).is_none(),
            "a participant event"
        );
        assert!(invitation(b"{\"evt\":107,\"gp\":\"not base64!\"}").is_none());
        let mut elsewhere = gp();
        elsewhere["callNotification"]["links"]["attach"] = json!("https://cc.example.test/attach");
        assert!(
            invitation(&invitation_body(107, &elsewhere)).is_none(),
            "an attach off the flight proxy"
        );
        let mut nobody = gp();
        nobody["callNotification"]["from"] = json!({});
        assert!(invitation(&invitation_body(107, &nobody)).is_none());
    }

    #[test]
    fn answers_hand_out_the_links_the_machine_sends_to() {
        let answer = json!({
            "conversationController": conv(""),
            "links": {"leave": conv("/leave"), "updateEndpointMetadata": conv("/updateEndpointMetadata"),
                "addParticipantAndModality": conv("/add"), "getAllParticipants": conv("/getAllParticipants")},
        });
        let read = conversation(answer.to_string().as_bytes()).unwrap();
        assert_eq!(read.leave.unwrap().as_str(), conv("/leave"));
        assert_eq!(read.add.unwrap().as_str(), conv("/add"));
        assert!(conversation(b"{\"links\":{}}").is_none(), "no conversation");

        let attach = json!({
            "callInvitation": {"links": {"progress": cc("incoming", "progress"),
                "acceptance": cc("incoming", "accept"), "callLeg": cc("incoming", "reject")}},
            "additionalActionResponses": [{"url": conv(""), "output": {"links": {
                "leave": conv("/leave"), "updateEndpointState": conv("/updateEndpointState")}}}],
        });
        let controller = teams_proto::CallLink::parse(&conv("")).unwrap();
        let read = attached(attach.to_string().as_bytes(), &controller).unwrap();
        assert_eq!(read.acceptance.as_str(), cc("incoming", "accept"));
        assert_eq!(read.call_leg.as_str(), cc("incoming", "reject"));
        assert_eq!(
            read.conversation.endpoint_state.unwrap().as_str(),
            conv("/updateEndpointState")
        );

        let accept = json!({"callAcceptanceAcknowledgement": {
            "links": {"mediaRenegotiation": cc("active", "renegotiate")}, "callKeepAliveInterval": 2700}});
        let read = accept_answer(accept.to_string().as_bytes());
        assert_eq!(read.keep_alive, Some(2700));
        assert_eq!(read.renegotiation.unwrap().as_str(), cc("active", "renegotiate"));
        let none = json!({"callAcceptanceAcknowledgement": {"callKeepAliveInterval": null}});
        assert_eq!(
            accept_answer(none.to_string().as_bytes()),
            super::AcceptAnswer::default()
        );
        assert_eq!(accept_answer(b"not json"), super::AcceptAnswer::default());
    }

    #[test]
    fn a_renegotiation_and_the_answer_to_one_read_their_media_and_links() {
        let offer = json!({"mediaNegotiation": {
            "mediaContent": {"blob": "v=0", "contentType": "application/sdp-ngc-1.0"},
            "links": {"mediaAnswer": cc("negotiations", "answer"), "rejection": cc("negotiations", "reject")},
        }});
        let (media, answer) = super::negotiation(offer.to_string().as_bytes()).unwrap();
        assert_eq!(media.content_type, "application/sdp-ngc-1.0");
        assert_eq!(answer.as_str(), cc("negotiations", "answer"));
        let unlinked = json!({"mediaNegotiation": {"mediaContent": {"blob": "v=0", "contentType": "x"}}});
        assert!(super::negotiation(unlinked.to_string().as_bytes()).is_none());

        let answered = json!({"mediaAnswer": {
            "mediaContent": {"blob": "v=0", "contentType": "application/sdp-ngc-1.0"},
            "links": {"mediaAcknowledgement": cc("mediaAnswers", "acknowledge")},
        }});
        let (_, acknowledgement) = super::media_answer(answered.to_string().as_bytes()).unwrap();
        assert_eq!(acknowledgement.unwrap().as_str(), cc("mediaAnswers", "acknowledge"));
        let empty = json!({"mediaAnswer": {}});
        assert!(super::media_answer(empty.to_string().as_bytes()).is_none());
    }

    #[test]
    fn an_acceptance_reads_its_acknowledgement_answer_and_keep_alive() {
        let body = json!({"callAcceptance": {
            "links": {"acknowledgement": cc("callAcceptance", "acknowledge")},
            "mediaContent": {"blob": "v=0\r\n", "contentType": "application/sdp-ngc-1.0"},
            "callKeepAliveInterval": 2700,
        }});
        let read = acceptance(body.to_string().as_bytes()).unwrap();
        assert_eq!(
            read.acknowledgement.unwrap().as_str(),
            cc("callAcceptance", "acknowledge")
        );
        assert_eq!(read.keep_alive, Some(2700));
        assert!(read.answer.is_some());
        assert_eq!(read.renegotiation, None);
        assert!(!read.voicemail, "taken by the callee");
        assert!(acceptance(b"{}").is_none());
        let voicemail = json!({"callAcceptance": {"acceptedBy": {"endpointType": "voicemail"}}});
        assert!(acceptance(voicemail.to_string().as_bytes()).unwrap().voicemail);
    }

    #[test]
    fn an_end_keeps_what_it_delivers_and_nothing_else() {
        let end = remote_end(br#"{"callEnd":{"code":487,"subCode":0,"phrase":"CallEndReasonLocalUserInitiated"}}"#);
        assert_eq!(
            (end.code, end.sub_code, end.phrase.as_deref()),
            (Some(487), Some(0), Some("CallEndReasonLocalUserInitiated"))
        );
        assert_eq!(remote_end(b"not json"), super::RemoteEnd::default());
        assert_eq!(remote_end(b"{\"callEnd\":{\"code\":\"x\"}}").code, None);
        let bare = remote_end(br#"{"code":409,"subCode":5704,"phrase":"Conversation cannot be created"}"#);
        assert_eq!((bare.code, bare.sub_code), (Some(409), Some(5704)));
    }
}
