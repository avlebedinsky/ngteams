//! Call signaling for the echo call, as the Task 18 capture of the web client's test call shows it (2026-10-20,
//! `docs/protocol/calling-echo.md`; fixtures `tests/fixtures/calling/echo/*.json`). Nothing in this module sends
//! anything: it builds requests and reads what comes back.
//!
//! The test call, step by step:
//! 1. **TURN credentials**: `GET` [`TRAP_TOKENS_URL`] answers `{"tokens":[{realm, username, password}], "expires"}`
//!    ([`TrapTokens`]); the relay is one fixed host ([`crate::turn::TEAMS_RELAY_HOST`]), no relay descriptor.
//! 2. **The call start**: `POST` [`CALL_START_URL`] with [`SKYPETOKEN_HEADER`] and [`call_start_body`]. It names no
//!    callee (`participants.to` is empty) and opens a conversation of the scenario
//!    [`TEST_CALL_SCENARIO`](teams_proto::TEST_CALL_SCENARIO); the offer goes in `callInvitation.mediaContent` in the
//!    Teams dialect [`SDP_NGC`] ([`NgcSdp`]), not as the browser's SDP. Every callback link is the endpoint's Trouter
//!    push URL, `callAgent/<agent id>/`, a random segment of its own, and `call/<name>/` or `conversation/<name>/`
//!    ([`CallbackLinks`]).
//! 3. **The answer** is the conversation ([`Conversation`]): its controller URL ([`ConversationUrl`], what the fence
//!    knows the call by) and operation links.
//! 4. **The acceptance**: the call controller calls back at the `acceptance` link with the media controller's answer
//!    (the remote SDP, Teams dialect) and the call leg's links ([`Acceptance`]); the client acknowledges it with a
//!    `POST` to its `acknowledgement` link. No `mediaAnswer` delivery was seen.
//! 5. **The echo joins** when the caller invites it: `POST <conversation>/add` ([`add_request`]: the URL and the body,
//!    both from a permitted [`AddToCall`]). That, the acknowledgement and the `leave` are writes on a call that exists, which the fence can't
//!    permit yet (`docs/backlog/fence-call-follow-ups.md`), so [`crate::call::start`] places no call.
//!
//! **Incoming calls are never answered**: a payload with a `callNotification` is an incoming call whatever its path
//! (its shape is still from the bundle: the capture had none), and [`OutgoingCall::on_delivery`] does nothing about
//! it.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use teams_fence::Mri;
use teams_proto::{AddToCall, Authorized, ConversationUrl, StartCall, TEST_CALL_SCENARIO};

use crate::turn::Credentials;

/// Where a call starts (through the flight proxy), with the account's skypetoken in [`SKYPETOKEN_HEADER`].
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";

/// The header every call-control request carries the skypetoken in (not the bearer token the chat service takes).
pub const SKYPETOKEN_HEADER: &str = "X-Skypetoken";

/// Where the TURN credentials come from (a read, with the account's token).
pub const TRAP_TOKENS_URL: &str = "https://teams.microsoft.com/trap/tokens";

/// The media content type of the offer and the answer: Teams' SDP dialect.
pub const SDP_NGC: &str = "application/sdp-ngc-1.0";

/// The callbacks a call registers, by name: the key of its link and the last segment of its path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Callback {
    // the call's (`callInvitation.links`)
    Progress,
    MediaAnswer,
    Acceptance,
    Redirection,
    End,
    // the conversation's (`conversationRequest.roster` and `.links`)
    RosterUpdate,
    ConversationEnd,
    ConversationUpdate,
    // the `add`'s
    AddParticipantSuccess,
    AddParticipantFailure,
}

impl Callback {
    /// The call's callbacks, as the call start registers them.
    pub const CALL: [Self; 5] = [
        Self::Progress,
        Self::MediaAnswer,
        Self::Acceptance,
        Self::Redirection,
        Self::End,
    ];

    pub const ALL: [Self; 10] = [
        Self::Progress,
        Self::MediaAnswer,
        Self::Acceptance,
        Self::Redirection,
        Self::End,
        Self::RosterUpdate,
        Self::ConversationEnd,
        Self::ConversationUpdate,
        Self::AddParticipantSuccess,
        Self::AddParticipantFailure,
    ];

    /// The link's key and the last segment of its path.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::MediaAnswer => "mediaAnswer",
            Self::Acceptance => "acceptance",
            Self::Redirection => "redirection",
            Self::End => "end",
            Self::RosterUpdate => "rosterUpdate",
            Self::ConversationEnd => "conversationEnd",
            Self::ConversationUpdate => "conversationUpdate",
            Self::AddParticipantSuccess => "addParticipantSuccess",
            Self::AddParticipantFailure => "addParticipantFailure",
        }
    }

    /// `call` or `conversation`: the segment before the name.
    #[must_use]
    pub fn kind(self) -> &'static str {
        if Self::CALL.contains(&self) {
            "call"
        } else {
            "conversation"
        }
    }
}

/// The callback links of one call. Each is `<push url>callAgent/<agent id>/<segment>/<kind>/<name>/`, with a segment
/// of its own, so a delivery at a path that is none of these is another call's (or nobody's) and not taken as ours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallbackLinks {
    /// `<push url>callAgent/<agent id>/`.
    prefix: String,
    /// Where the path part of the prefix starts.
    path_start: usize,
    segments: BTreeMap<Callback, String>,
}

impl CallbackLinks {
    /// The links under `push_url` (the endpoint's Trouter push URL) for `agent_id`, `segment` giving each link its own
    /// path segment.
    pub fn new(push_url: &str, agent_id: &str, mut segment: impl FnMut(Callback) -> String) -> Self {
        let prefix = format!("{}/callAgent/{agent_id}/", push_url.trim_end_matches('/'));
        let path_start = prefix
            .find("://")
            .and_then(|scheme| prefix[scheme + 3..].find('/').map(|slash| scheme + 3 + slash))
            .unwrap_or(0);
        Self {
            prefix,
            path_start,
            segments: Callback::ALL
                .into_iter()
                .map(|callback| (callback, segment(callback)))
                .collect(),
        }
    }

    /// The links with a random segment each (8 hex digits, as the web client's).
    ///
    /// # Errors
    /// When the system's random source fails.
    pub fn random(push_url: &str, agent_id: &str) -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; 4 * Callback::ALL.len()];
        getrandom::fill(&mut bytes)?;
        let (chunks, _) = bytes.as_chunks::<4>();
        let mut chunks = chunks.iter();
        Ok(Self::new(push_url, agent_id, |_| {
            format!("{:08x}", u32::from_be_bytes(chunks.next().copied().unwrap_or_default()))
        }))
    }

    /// The link the service calls back on for `callback`.
    #[must_use]
    pub fn url(&self, callback: Callback) -> String {
        format!(
            "{}{}/{}/{}/",
            self.prefix,
            self.segments.get(&callback).map_or("", String::as_str),
            callback.kind(),
            callback.name()
        )
    }

    /// The path a delivery for `callback` comes at (the link without its scheme and host).
    #[must_use]
    pub fn path(&self, callback: Callback) -> String {
        self.url(callback)[self.path_start..].to_owned()
    }

    /// The callback a delivery at `path` is for, if it is one of these links.
    #[must_use]
    pub fn callback_at(&self, path: &str) -> Option<Callback> {
        Callback::ALL.into_iter().find(|callback| self.path(*callback) == path)
    }
}

/// Who we are in the call: `participants.from`, besides our MRI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub display_name: String,
    /// Our registered endpoint id (a GUID from the endpoint registration).
    pub endpoint_id: String,
    /// A fresh id for our leg of this call.
    pub participant_id: String,
    pub language_id: String,
}

impl Endpoint {
    fn participant(&self, mri: &Mri) -> Value {
        json!({
            "id": mri.as_str(),
            "displayName": self.display_name,
            "endpointId": self.endpoint_id,
            "participantId": self.participant_id,
            "languageId": self.language_id,
        })
    }
}

/// An SDP in Teams' dialect ([`SDP_NGC`]): every media line on `RTP/SAVP` with a real port, not the browser's
/// `UDP/TLS/RTP/SAVPF` on the placeholder port 9. Writing str0m's offer in this dialect, and reading the media
/// controller's answer back into one str0m takes, is M3's work (`docs/spikes/echo-call.md`); this only tells the two
/// apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NgcSdp(String);

/// An SDP that isn't in Teams' dialect.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("not an SDP in Teams' dialect ({SDP_NGC}): {0}")]
pub struct NotNgc(&'static str);

impl NgcSdp {
    /// # Errors
    /// [`NotNgc`] when `sdp` doesn't start with `v=0`, has no media line, or has one that isn't `RTP/SAVP` on a real
    /// port.
    pub fn new(sdp: &str) -> Result<Self, NotNgc> {
        if !sdp.starts_with("v=0\r\n") {
            return Err(NotNgc("no session part"));
        }
        let mut media = sdp.split("\r\n").filter_map(|line| line.strip_prefix("m=")).peekable();
        if media.peek().is_none() {
            return Err(NotNgc("no media line"));
        }
        for line in media {
            let mut fields = line.split(' ').skip(1);
            let port = fields.next().and_then(|port| port.parse::<u16>().ok());
            if !matches!(port, Some(port) if port != 0 && port != 9) || fields.next() != Some("RTP/SAVP") {
                return Err(NotNgc("a media line isn't RTP/SAVP on a real port"));
            }
        }
        Ok(Self(sdp.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The JSON payload that starts the test call, from a call the fence permitted: our endpoint as `from`, nobody in
/// `to`, the scenario [`TEST_CALL_SCENARIO`], the call's and the conversation's callback links, the offer. Its keys are
/// a subset of the web client's (the rest of what it sends, capabilities and client details, is left out: whether the
/// service needs any of it is unconfirmed). The browser fence maps exactly this shape to the [`StartCall`]'s target
/// (`teams_fence::contacting`, checked in `tests/signaling.rs`).
#[must_use]
pub fn call_start_body(call: &Authorized<StartCall>, me: &Endpoint, offer: &NgcSdp, links: &CallbackLinks) -> Value {
    let link_map = |callbacks: &[Callback]| -> Map<String, Value> {
        callbacks
            .iter()
            .map(|callback| (callback.name().to_owned(), Value::String(links.url(*callback))))
            .collect()
    };
    json!({
        "conversationRequest": {
            "roster": { "type": "Delta", "rosterUpdate": links.url(Callback::RosterUpdate) },
            "links": link_map(&[Callback::ConversationEnd, Callback::ConversationUpdate]),
            "scenario": TEST_CALL_SCENARIO,
        },
        "participants": {
            "from": me.participant(call.request().caller()),
            "to": [],
        },
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": link_map(&Callback::CALL),
            "mediaContent": { "blob": offer.as_str(), "contentType": SDP_NGC },
        },
    })
}

/// The call start's answer: the conversation the call opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    /// `conversationController`: its URL on the conversation service, behind the flight proxy. The call's id for the
    /// fence ([`teams_fence::Scope::Call`]) and where an [`AddToCall`] on it is posted.
    pub controller: ConversationUrl,
    pub scenario: Option<String>,
    /// Its operations by name (`leave`, `addParticipant`, `getAllParticipants`, …).
    pub links: BTreeMap<String, String>,
}

impl Conversation {
    /// Reads the answer; `None` unless its `conversationController` is a [`ConversationUrl`] (`https`, a host, no user
    /// info or fragment, and a non-empty last path segment: the conversation id).
    #[must_use]
    pub fn from_answer(answer: &Value) -> Option<Self> {
        let controller = ConversationUrl::parse(answer.get("conversationController")?.as_str()?)?;
        let links = answer
            .get("links")
            .and_then(Value::as_object)
            .map(|links| {
                links
                    .iter()
                    .filter_map(|(name, link)| Some((name.clone(), link.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            controller,
            scenario: answer.get("scenario").and_then(Value::as_str).map(str::to_owned),
            links,
        })
    }

    /// The link of the operation `name`.
    #[must_use]
    pub fn link(&self, name: &str) -> Option<&str> {
        self.links.get(name).map(String::as_str)
    }
}

/// What an `add` says about the invitation besides who is invited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invitation {
    /// A fresh participant id for the invitee.
    pub participant_id: String,
    /// The 1:1 chat of the caller and the invitee the test call starts from.
    pub chat_thread: String,
    pub tenant_id: String,
}

/// A request ready for a transport: where it is posted and its body.
#[derive(Clone, Debug, PartialEq)]
pub struct WireRequest {
    pub url: String,
    pub body: Value,
}

/// The `add` that invites the echo into the call, from an invitation the fence permitted (today none: see
/// [`AddToCall`]). Its URL is [`AddToCall::url`] of the permitted request and its invitee that request's, so it lands
/// on the call the permit was issued for. `caller` is our own MRI (`8:orgid:<object id>`, the object id naming the
/// organizer); `None` for any other kind of MRI.
#[must_use]
pub fn add_request(
    add: &Authorized<AddToCall>,
    caller: &Mri,
    me: &Endpoint,
    invitation: &Invitation,
    links: &CallbackLinks,
) -> Option<WireRequest> {
    let organizer = caller.as_str().strip_prefix("8:orgid:")?;
    let body = json!({
        "disableUnmute": false,
        "participants": {
            "from": me.participant(caller),
            "to": [{ "id": add.request().invitee().as_str(), "participantId": invitation.participant_id }],
        },
        "participantInvitationData": {
            "invitationData": { "meetingInfo": { "organizerId": organizer, "tenantId": invitation.tenant_id } },
        },
        "replacementDetails": null,
        "groupContext": null,
        "groupChat": { "threadId": invitation.chat_thread, "messageId": null },
        "links": {
            "addParticipantSuccess": links.url(Callback::AddParticipantSuccess),
            "addParticipantFailure": links.url(Callback::AddParticipantFailure),
        },
    });
    Some(WireRequest {
        url: add.request().url().into(),
        body,
    })
}

/// The call controller's acceptance of our call: the remote SDP and the call leg's links.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acceptance {
    /// The media controller's answer, in Teams' dialect.
    pub answer: NgcSdp,
    /// Where the acceptance is acknowledged (a `POST`: a write on the call).
    pub acknowledgement: Option<String>,
    /// The call leg on the call controller.
    pub call_leg: Option<String>,
}

impl Acceptance {
    /// Reads `callAcceptance`; `None` without a media answer in Teams' dialect.
    fn from_body(body: &Value) -> Option<Self> {
        let acceptance = body.get("callAcceptance")?;
        let media = acceptance.get("mediaContent")?;
        if media.get("contentType")?.as_str()? != SDP_NGC {
            return None;
        }
        let link = |name: &str| {
            acceptance
                .pointer(&format!("/links/{name}"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Some(Self {
            answer: NgcSdp::new(media.get("blob")?.as_str()?).ok()?,
            acknowledgement: link("acknowledgement"),
            call_leg: link("callLeg"),
        })
    }
}

/// Something the service delivered to us (through Trouter), read by its path and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// Someone calls us. Never answered.
    IncomingCall,
    /// At none of our call's links.
    NotOurs,
    /// Our call is ringing or progressing.
    Progress,
    /// Our call is accepted, with the remote SDP.
    Accepted(Acceptance),
    /// The call or its conversation ended, with the service's code when it gave one.
    Ended { code: Option<i64> },
    /// One of our links, nothing to act on (an update, a redirection, a media answer, an `add`'s outcome) or a body
    /// that doesn't read.
    Unknown,
}

impl Inbound {
    /// Reads one delivery at `path`. A `callNotification` payload is an incoming call whatever its path; anything else
    /// counts only at one of `links`.
    #[must_use]
    pub fn parse(links: &CallbackLinks, path: &str, body: &Value) -> Self {
        if body.get("callNotification").is_some() {
            return Self::IncomingCall;
        }
        match links.callback_at(path) {
            None => Self::NotOurs,
            Some(Callback::Progress) => Self::Progress,
            Some(Callback::Acceptance) => Acceptance::from_body(body).map_or(Self::Unknown, Self::Accepted),
            Some(Callback::End) => Self::Ended {
                code: body.pointer("/callEnd/code").and_then(Value::as_i64),
            },
            Some(Callback::ConversationEnd) => Self::Ended { code: None },
            Some(_) => Self::Unknown,
        }
    }
}

/// Where our outgoing call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    Placing,
    Ringing,
    /// The remote SDP arrived and is to be applied.
    Accepted,
    Ended,
}

/// What the caller has to do after a delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing at all: no request goes out.
    Nothing,
    /// Apply the remote SDP and acknowledge the acceptance (a write on the call: through the fence, which can't permit
    /// it yet).
    Accept(Acceptance),
    /// The call is over: stop the media.
    Hangup,
}

/// Our one outgoing call, driven by the deliveries at its own callback links; a delivery at any other path is not ours
/// and changes nothing. Incoming calls are recognized and never acted on.
#[derive(Debug)]
pub struct OutgoingCall {
    links: CallbackLinks,
    state: CallState,
    ignored_incoming: u32,
}

impl OutgoingCall {
    /// The call whose start registered `links`.
    #[must_use]
    pub fn new(links: CallbackLinks) -> Self {
        Self {
            links,
            state: CallState::Placing,
            ignored_incoming: 0,
        }
    }

    #[must_use]
    pub fn state(&self) -> CallState {
        self.state
    }

    /// Incoming calls seen and ignored.
    #[must_use]
    pub fn ignored_incoming(&self) -> u32 {
        self.ignored_incoming
    }

    /// Reacts to one delivery. An incoming call is always [`Action::Nothing`]: answering is a write the lab never
    /// makes, and rejecting one would show the caller a decline, so neither happens; there is no action that could.
    /// The acceptance is acted on once: the service delivers it again while it isn't acknowledged.
    pub fn on_delivery(&mut self, path: &str, body: &Value) -> Action {
        if self.state == CallState::Ended {
            return Action::Nothing;
        }
        match Inbound::parse(&self.links, path, body) {
            Inbound::IncomingCall => {
                self.ignored_incoming += 1;
                Action::Nothing
            }
            Inbound::Progress => {
                if self.state == CallState::Placing {
                    self.state = CallState::Ringing;
                }
                Action::Nothing
            }
            Inbound::Accepted(acceptance) if self.state != CallState::Accepted => {
                self.state = CallState::Accepted;
                Action::Accept(acceptance)
            }
            Inbound::Ended { .. } => {
                self.state = CallState::Ended;
                Action::Hangup
            }
            Inbound::Accepted(_) | Inbound::NotOurs | Inbound::Unknown => Action::Nothing,
        }
    }
}

/// One TURN credential from `trap/tokens`. `Debug` shows no password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayToken {
    pub credentials: Credentials,
    /// The realm, without the quotes the service puts inside the string (`"rtcmedia"` → `rtcmedia`). The TURN client
    /// takes the realm the server names in its 401; this is what to expect there.
    pub realm: String,
}

/// The answer of `GET` [`TRAP_TOKENS_URL`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrapTokens {
    pub tokens: Vec<RelayToken>,
    /// How long the credentials hold, in seconds (604800, 7 days, in the capture).
    pub expires: u64,
}

impl TrapTokens {
    /// Reads `{"tokens":[{realm, username, password}], "expires"}`; `None` unless every token has all three and
    /// there is at least one, and `expires` is there.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let tokens = value
            .get("tokens")?
            .as_array()?
            .iter()
            .map(|token| {
                let text = |key: &str| token.get(key).and_then(Value::as_str).map(str::to_owned);
                let realm = text("realm")?;
                Some(RelayToken {
                    credentials: Credentials {
                        username: text("username")?,
                        password: text("password")?,
                    },
                    realm: realm
                        .strip_prefix('"')
                        .and_then(|inner| inner.strip_suffix('"'))
                        .unwrap_or(&realm)
                        .to_owned(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        if tokens.is_empty() {
            return None;
        }
        Some(Self {
            tokens,
            expires: value.get("expires")?.as_u64()?,
        })
    }
}
