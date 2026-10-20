//! Call signaling, shaped from the web client's bundle (version 26260.1717.5229.7049) only.
//!
//! **Unconfirmed — needs the Task 18 capture.** Nothing here has been checked against live traffic: the lab accounts
//! have no Teams license (`docs/backlog/lab-accounts-have-no-teams-license.md`), so no call was ever captured. The
//! shapes below are what the bundle builds and reads; the endpoint the call start is posted to, its headers, the
//! callback base URL (our Trouter endpoint) and the way an incoming call is delivered are not known yet, and nothing
//! in this module sends anything. `docs/spikes/echo-call.md` lists what Task 18 has to confirm.
//!
//! What the bundle shows, in our words:
//! - A call starts with one POST whose JSON payload has `participants` (`from`: our id, display name, endpoint id,
//!   participant id; `to`: the callees), a `conversationRequest` with callback links for the conversation, and a
//!   `callInvitation` with the modalities, callback links for the call (`progress`, `mediaAnswer`, `acceptance`,
//!   `redirection`, `end`) and `mediaContent`: our SDP offer as `blob`, typed `application/sdp-ngc-1.0`.
//! - The service then calls us back on those links (through Trouter) at paths such as `/call/progress/`,
//!   `/call/mediaAnswer/`, `/call/acceptance/` and `/call/end/`; the media answer carries the remote SDP in
//!   `mediaAnswer.mediaContent.blob` and a `mediaAcknowledgement` link we post to after applying it.
//! - An incoming call arrives as a payload with a `callNotification` object. **A lab client never answers one**: it
//!   is ignored (no acceptance, no rejection, nothing sent), see [`OutgoingCall::on_inbound`].
//! - TURN: a relay descriptor has `addresses`, `fqdns`, `udpPort` and `tcpPort`; a relay token has `username`,
//!   `password`, `realm` and `expires`. The tokens come from a relay token service (the bundle names
//!   `edge.skype.com/trap/tokens`), whose request is not known yet.

use std::net::IpAddr;

use serde_json::{Map, Value, json};
use teams_proto::{Authorized, StartCall};

use crate::relay::{RelayServer, fallback_order};
use crate::turn::Credentials;

/// The media content type the web client offers (it also accepts `application/sdp-ngc-0.5`).
pub const SDP_NGC: &str = "application/sdp-ngc-1.0";

/// The call callbacks the bundle registers, by the path the service calls back on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callback {
    Progress,
    MediaAnswer,
    Acceptance,
    Redirection,
    End,
}

impl Callback {
    pub const ALL: [Self; 5] = [
        Self::Progress,
        Self::MediaAnswer,
        Self::Acceptance,
        Self::Redirection,
        Self::End,
    ];

    /// The path under our callback base.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Progress => "/call/progress/",
            Self::MediaAnswer => "/call/mediaAnswer/",
            Self::Acceptance => "/call/acceptance/",
            Self::Redirection => "/call/redirection/",
            Self::End => "/call/end/",
        }
    }

    /// The key of the link in `callInvitation.links`.
    #[must_use]
    pub fn link_name(self) -> &'static str {
        match self {
            Self::Progress => "progress",
            Self::MediaAnswer => "mediaAnswer",
            Self::Acceptance => "acceptance",
            Self::Redirection => "redirection",
            Self::End => "end",
        }
    }

    fn from_path(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|callback| path.ends_with(callback.path()))
    }
}

/// Who we are in the call: what goes into `participants.from`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub display_name: String,
    /// Our registered endpoint id (a GUID from the endpoint registration).
    pub endpoint_id: String,
    /// A fresh id for our leg of this call.
    pub participant_id: String,
}

/// The JSON payload that starts a call: built only from a call the fence permitted, so its participants are exactly
/// the permitted ones. `callback_base` is where the service calls us back (our Trouter URL). Unconfirmed.
#[must_use]
pub fn call_start_body(call: &Authorized<StartCall>, me: &Endpoint, offer_sdp: &str, callback_base: &str) -> Value {
    let request = call.request();
    let links: Map<String, Value> = Callback::ALL
        .into_iter()
        .map(|callback| {
            (
                callback.link_name().to_owned(),
                Value::String(format!("{}{}", callback_base.trim_end_matches('/'), callback.path())),
            )
        })
        .collect();
    let to: Vec<Value> = request
        .callees()
        .iter()
        .map(|callee| json!({ "id": callee.as_str() }))
        .collect();
    json!({
        "participants": {
            "from": {
                "id": request.caller().as_str(),
                "displayName": me.display_name,
                "endpointId": me.endpoint_id,
                "participantId": me.participant_id,
            },
            "to": to,
        },
        "conversationRequest": {
            "links": {
                "conversationEnd": format!("{}/conversation/conversationEnd/", callback_base.trim_end_matches('/')),
                "conversationUpdate": format!("{}/conversation/conversationUpdate/", callback_base.trim_end_matches('/')),
            },
        },
        "callInvitation": {
            "callModalities": ["Audio"],
            "links": links,
            "mediaContent": { "contentType": SDP_NGC, "blob": offer_sdp },
        },
    })
}

/// Something the service delivered to us (through Trouter), read by its path and payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inbound {
    /// Someone calls us. Never answered.
    IncomingCall,
    /// Our call is ringing or progressing.
    Progress,
    /// The remote media answer, and where to acknowledge it.
    MediaAnswer {
        sdp: String,
        content_type: String,
        acknowledgement: Option<String>,
    },
    /// The callee accepted our call.
    Accepted,
    /// The call is redirected elsewhere.
    Redirected,
    /// The call ended, with the service's code when it gave one.
    Ended { code: Option<i64> },
    /// Anything else.
    Unknown,
}

impl Inbound {
    /// Reads one delivery. A `callNotification` payload is an incoming call whatever its path; a media answer without
    /// an SDP blob is [`Inbound::Unknown`].
    #[must_use]
    pub fn parse(path: &str, body: &Value) -> Self {
        if body.get("callNotification").is_some() {
            return Self::IncomingCall;
        }
        match Callback::from_path(path) {
            Some(Callback::Progress) => Self::Progress,
            Some(Callback::MediaAnswer) => media_answer(body).unwrap_or(Self::Unknown),
            Some(Callback::Acceptance) => Self::Accepted,
            Some(Callback::Redirection) => Self::Redirected,
            Some(Callback::End) => Self::Ended {
                code: body.pointer("/callEnd/code").and_then(Value::as_i64),
            },
            None => Self::Unknown,
        }
    }
}

fn media_answer(body: &Value) -> Option<Inbound> {
    let answer = body.get("mediaAnswer")?;
    let content = answer.get("mediaContent")?;
    Some(Inbound::MediaAnswer {
        sdp: content.get("blob")?.as_str()?.to_owned(),
        content_type: content.get("contentType")?.as_str()?.to_owned(),
        acknowledgement: answer
            .pointer("/links/mediaAcknowledgement")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Where our outgoing call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallState {
    Placing,
    Ringing,
    /// The remote SDP arrived and was handed to the peer.
    MediaAnswered,
    Connected,
    Ended,
}

/// What the caller has to do after a delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing at all: no request goes out.
    Nothing,
    /// Apply the remote SDP; acknowledge it at the link, if there is one.
    ApplyAnswer {
        sdp: String,
        acknowledgement: Option<String>,
    },
    /// The call is over: stop the media.
    Hangup,
}

/// Our one outgoing call. It reacts to deliveries about it and to nothing else.
#[derive(Debug)]
pub struct OutgoingCall {
    state: CallState,
    ignored_incoming: u32,
}

impl Default for OutgoingCall {
    fn default() -> Self {
        Self::new()
    }
}

impl OutgoingCall {
    #[must_use]
    pub fn new() -> Self {
        Self {
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
    pub fn on_inbound(&mut self, inbound: Inbound) -> Action {
        if self.state == CallState::Ended {
            return Action::Nothing;
        }
        match inbound {
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
            Inbound::MediaAnswer {
                sdp, acknowledgement, ..
            } => {
                self.state = CallState::MediaAnswered;
                Action::ApplyAnswer { sdp, acknowledgement }
            }
            Inbound::Accepted => {
                self.state = CallState::Connected;
                Action::Nothing
            }
            Inbound::Ended { .. } => {
                self.state = CallState::Ended;
                Action::Hangup
            }
            Inbound::Redirected | Inbound::Unknown => Action::Nothing,
        }
    }
}

/// A relay as the bundle describes one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayDescriptor {
    pub addresses: Vec<IpAddr>,
    pub fqdns: Vec<String>,
    pub udp_port: u16,
    pub tcp_port: u16,
}

impl RelayDescriptor {
    /// Reads `{addresses, fqdns, udpPort, tcpPort}`; `None` unless all four are there (as the bundle checks). An
    /// address that isn't an IP is skipped.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let port = |key: &str| value.get(key)?.as_u64().and_then(|p| u16::try_from(p).ok());
        let strings =
            |key: &str| -> Option<Vec<&str>> { value.get(key)?.as_array()?.iter().map(Value::as_str).collect() };
        Some(Self {
            addresses: strings("addresses")?.iter().filter_map(|a| a.parse().ok()).collect(),
            fqdns: strings("fqdns")?.into_iter().map(str::to_owned).collect(),
            udp_port: port("udpPort")?,
            tcp_port: port("tcpPort")?,
        })
    }

    /// The servers to try: UDP on every address, then TCP.
    #[must_use]
    pub fn servers(&self) -> Vec<RelayServer> {
        fallback_order(&self.addresses, self.udp_port, self.tcp_port)
    }
}

/// A relay token: long-term TURN credentials. `Debug` shows no password.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayToken {
    pub credentials: Credentials,
    /// The realm the server is expected to name.
    pub realm: Option<String>,
    /// As the service gives it (its unit is not confirmed).
    pub expires: Option<i64>,
}

impl RelayToken {
    /// Reads `{username, password, realm, expires}`; `None` without a username and password.
    #[must_use]
    pub fn from_json(value: &Value) -> Option<Self> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            credentials: Credentials {
                username: text("username")?,
                password: text("password")?,
            },
            realm: text("realm"),
            expires: value.get("expires").and_then(Value::as_i64),
        })
    }
}
