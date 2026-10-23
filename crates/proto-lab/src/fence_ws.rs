//! The browser fence's WebSocket layer: the guard in every scope, the decision on each frame, and the audit of what was
//! sent.
//!
//! **The guard** (`assets/ws_guard.js`, rendered per run by [`GuardScript`]) replaces `WebSocket.prototype.send` in
//! every JavaScript context of every lab target before the target's code runs (arming installs it:
//! [`crate::arming`]). It sends nothing synchronously: each frame is queued on its socket and the fence is asked for a
//! verdict through the `Runtime.addBinding` binding [`BINDING`]. [`WsFence::handle_binding`] decides the frame and
//! answers by calling the resolver [`RESOLVER`] in the frame's context with the run's token; the guard replays the
//! original `send` only on an allow, and drops a denied frame. A socket's frames leave in the order they were sent.
//!
//! A guard queues only the sockets its own realm made: a `send` called through another realm's prototype (a same-origin
//! iframe's, a popup's, an opener's) is dropped by that realm's guard and reported as [`Payload::Foreign`], which is
//! denied ([`DenyCause::ForeignSocket`]) without being classified. So each socket has one queue, and the context that
//! asks for a frame's verdict is always the socket's own: an allow is recorded for the audit on the session the browser
//! reports the frame on.
//!
//! The token goes both ways: the guard puts it in every request, and a binding call without it is denied and never
//! answered. The binding is in every context of a target, isolated worlds the guard isn't in included, and answering a
//! call from one would hand the token to whatever that world defined as the resolver.
//!
//! **The decision** classifies a frame by the frame rule ([`classify_frame`]): on a Trouter socket the socket.io 0.9
//! type decides (the session's disconnect, connect, heartbeat and events pass; a message, which is how a delivery is
//! answered, a JSON message, an acknowledgement, a binary frame and anything that doesn't parse are denied); every frame
//! on a socket of a contacting service (`teams_fence::on_contacting_service`: calling, the chat service, …) is denied;
//! on any other socket a frame passes. A frame let through is logged to `fence.log` as `allowed` (method `SEND`, the
//! socket's host and path shape), a denied one with the socket's URL without its query (channel `web_socket`, method
//! `SEND`), never a frame's content. A run can force a deny on frames too ([`ForcedFrameDeny`], before the rule), as it
//! can on requests.
//!
//! **The audit** compares what the browser reports as sent (`Network.webSocketFrameSent`, on the session of the target
//! that sent it) with the guard's allows: each allow covers one frame with the same opcode and content on the same
//! session, and a frame no allow covers is a breach — logged (`breach`, `sent_without_allow`) and kept, and it fails
//! the run. It only observes: by the time it sees a frame, the frame is out. It catches whatever gets past the guard (a
//! context the guard isn't in, such as an isolated world). Matching is by content, not by socket: a frame that got past
//! the guard with exactly the content of an allowed frame of the same target, sent before that one, isn't told apart.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, Read as _};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use regex::Regex;
use serde::Deserialize;
use serde_json::json;
use teams_fence::{Channel, Fence, MembershipSource, RequestOutcome, RequestRecord, on_contacting_service};
use url::{Host, Url};

use crate::cdp::{Cdp, CdpError, Event, SessionId};
use crate::classify::Classification;
use crate::fence_http::{DenyCause, Verdict, judge, without_query};

/// The binding the guard asks for verdicts through. The guard takes it off the global before any page code runs.
pub const BINDING: &str = "__labFenceWs";

/// The global function the fence answers through: `RESOLVER(token, socket, seq, allow)`.
pub const RESOLVER: &str = "__labFenceWsVerdict";

/// The method `fence.log` names for a frame.
const SEND: &str = "SEND";

const TEMPLATE: &str = include_str!("../assets/ws_guard.js");

/// The guard script of one run, with the run's token in it.
#[derive(Clone)]
pub struct GuardScript {
    source: String,
    token: String,
}

impl fmt::Debug for GuardScript {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GuardScript")
    }
}

/// A token that isn't lowercase hex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a guard token must be lowercase hex")]
pub struct BadToken;

impl GuardScript {
    /// The guard with a fresh token: 16 random bytes from the kernel, as hex.
    ///
    /// # Errors
    /// When `/dev/urandom` can't be read.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0_u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let token: String = bytes
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0xf])
            .map(hex_digit)
            .collect();
        Self::with_token(&token).map_err(io::Error::other)
    }

    /// The guard with a given token (tests use a known one).
    ///
    /// # Errors
    /// [`BadToken`] unless the token is non-empty lowercase hex, which is all the script can hold as written.
    pub fn with_token(token: &str) -> Result<Self, BadToken> {
        if token.is_empty() || !token.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(BadToken);
        }
        let source = TEMPLATE
            .replace("{{BINDING}}", BINDING)
            .replace("{{RESOLVER}}", RESOLVER)
            .replace("{{TOKEN}}", token);
        Ok(Self {
            source,
            token: token.to_owned(),
        })
    }

    /// The script: an expression that installs the guard in the context it runs in and gives `"armed"` (or
    /// `"no binding"`, when the binding is missing and the guard can only refuse every `send`).
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether a request carries this run's token.
    fn authenticates(&self, token: &str) -> bool {
        self.token == token
    }

    /// The expression that hands a verdict to the guard in one context.
    #[must_use]
    pub fn verdict_expression(&self, socket: u64, seq: u64, allow: bool) -> String {
        format!("{RESOLVER}(\"{}\", {socket}, {seq}, {allow})", self.token)
    }
}

/// The lowercase hex digit for a nibble.
fn hex_digit(nibble: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(nibble & 0xf)])
}

/// A frame's content, as the guard hands it over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    Text(String),
    /// The bytes, in standard base64 (as `Network.webSocketFrameSent` reports them).
    Binary(String),
    /// A `Blob` the guard couldn't read; it is dropped whatever the verdict.
    Unreadable,
    /// A frame on a socket of another realm, which the guard dropped without reading it; denied unclassified, and not
    /// answered (nothing waits for it).
    Foreign,
}

impl Payload {
    /// The WebSocket opcode `Network.webSocketFrameSent` reports for it, and the content it reports.
    fn sent_as(&self) -> Option<(u64, &str)> {
        match self {
            Self::Text(text) => Some((1, text)),
            Self::Binary(base64) => Some((2, base64)),
            Self::Unreadable | Self::Foreign => None,
        }
    }
}

/// One frame: the socket's URL and what is sent on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub url: String,
    pub payload: Payload,
}

/// The Trouter service's domain: the push channel's socket opens on `go.<domain>` and moves to a regional host under it.
pub const TROUTER_DOMAIN: &str = "trouter.teams.microsoft.com";

/// The socket.io 0.9 frame types that pass on a Trouter socket: the disconnect (`0`), the connect (`1`), the heartbeat
/// (`2`) and an event (`5`: the session's own, such as its authentication, keep-alive, activity and loss
/// acknowledgement). A message (`3`) is how a delivery is answered, which goes back to whoever sent it; a JSON message
/// (`4`) and an acknowledgement (`6`) are denied with it, and so is every other type.
const TROUTER_TYPES: [u8; 4] = *b"0125";

/// What the fence makes of one frame (see the module docs): on a Trouter socket ([`is_trouter_socket`]) a text frame
/// passes ([`Classification::Allow`]) only when it parses as a socket.io 0.9 frame of a type that passes; on a socket of
/// a contacting service, or one whose URL doesn't parse, every frame is [`Classification::Unknown`]; on any other
/// socket a frame passes. A frame the guard couldn't read, or one on another realm's socket, is unknown everywhere.
#[must_use]
pub fn classify_frame(frame: &Frame) -> Classification {
    let Ok(url) = Url::parse(&frame.url) else {
        return Classification::Unknown;
    };
    let allowed = match &frame.payload {
        Payload::Text(text) if is_trouter_socket(&url) => {
            trouter_type(text).is_some_and(|kind| TROUTER_TYPES.contains(&kind))
        }
        Payload::Binary(_) if is_trouter_socket(&url) => false,
        Payload::Text(_) | Payload::Binary(_) => !on_contacting_service(&url),
        Payload::Unreadable | Payload::Foreign => false,
    };
    if allowed {
        Classification::Allow
    } else {
        Classification::Unknown
    }
}

/// Whether `url` is a socket of the Trouter service: any host under [`TROUTER_DOMAIN`] (in any case, a trailing dot
/// dropped), whatever its scheme, port or path.
#[must_use]
pub fn is_trouter_socket(url: &Url) -> bool {
    let Some(Host::Domain(host)) = url.host() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    host.strip_suffix(TROUTER_DOMAIN)
        .and_then(|label| label.strip_suffix('.'))
        .is_some_and(|label| !label.is_empty())
}

/// The type of a socket.io 0.9 frame (`<type>:<id>[+]:<endpoint>[:<data>]`): its one-digit type, when the frame starts
/// with that digit, a `:`, an id of digits (or none) with an optional `+`, and another `:`. `None` for anything else,
/// a batch of frames (`\u{fffd}<length>\u{fffd}…`) among it.
#[must_use]
pub fn trouter_type(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    let (&kind, rest) = bytes.split_first()?;
    let rest = rest.strip_prefix(b":")?;
    let id_len = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
    let rest = &rest[id_len..];
    let rest = rest.strip_prefix(b"+").unwrap_or(rest);
    (kind.is_ascii_digit() && rest.starts_with(b":")).then_some(kind)
}

/// A deny a run asks for on purpose on WebSocket frames: every frame on a socket whose URL the pattern finds a match in
/// and whose content the rule takes (any, a text frame holding a text, or any but a text frame starting with a prefix)
/// is denied before it is classified ([`WsFence::force_deny`]). That
/// is how a scenario holds back one kind of frame (the client's own `user.activity`, say) to see what the service does
/// without it, and how a test stands a local socket in for one whose frames the fence denies.
#[derive(Clone, Debug)]
pub struct ForcedFrameDeny {
    url: Regex,
    text: FrameText,
    label: Option<String>,
}

/// Which frames on its sockets a [`ForcedFrameDeny`] denies.
#[derive(Clone, Debug, PartialEq, Eq)]
enum FrameText {
    /// Every frame, of any kind.
    Any,
    /// Text frames holding this text.
    Containing(String),
    /// Every frame but a text frame starting with this text.
    NotStartingWith(String),
}

impl ForcedFrameDeny {
    /// Frames on a socket whose URL `url_pattern` (a regular expression) finds a match in.
    ///
    /// # Errors
    /// When the pattern doesn't compile.
    pub fn new(url_pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            url: Regex::new(url_pattern)?,
            text: FrameText::Any,
            label: None,
        })
    }

    /// Only text frames holding `text` (a binary frame never matches then).
    #[must_use]
    pub fn with_text_containing(mut self, text: &str) -> Self {
        self.text = FrameText::Containing(text.to_owned());
        self
    }

    /// Every frame but a text frame starting with `prefix` (a binary frame always matches then).
    #[must_use]
    pub fn except_text_starting_with(mut self, prefix: &str) -> Self {
        self.text = FrameText::NotStartingWith(prefix.to_owned());
        self
    }

    /// Names the rule: a frame it denies is kept with this label ([`FrameDenial::label`]).
    #[must_use]
    pub fn labelled(mut self, label: &str) -> Self {
        self.label = Some(label.to_owned());
        self
    }

    /// Whether it denies `frame`.
    #[must_use]
    pub fn matches(&self, frame: &Frame) -> bool {
        let text_matches = match (&self.text, &frame.payload) {
            (FrameText::Containing(wanted), Payload::Text(text)) => text.contains(wanted.as_str()),
            (FrameText::Containing(_), _) => false,
            (FrameText::NotStartingWith(prefix), Payload::Text(text)) => !text.starts_with(prefix.as_str()),
            (FrameText::Any | FrameText::NotStartingWith(_), _) => true,
        };
        self.url.is_match(&frame.url) && text_matches
    }
}

/// What the guard asks: a verdict on one frame of one of its sockets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardRequest {
    /// The socket's number within its context.
    pub socket: u64,
    /// The frame's number within its socket.
    pub seq: u64,
    pub frame: Frame,
}

/// A binding call that isn't a guard request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a malformed guard request: {0}")]
pub struct MalformedRequest(String);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    token: String,
    socket: u64,
    seq: u64,
    url: String,
    kind: String,
    data: String,
}

impl GuardRequest {
    /// Reads the payload of a binding call, made by the guard of `script`.
    ///
    /// # Errors
    /// [`MalformedRequest`] when it isn't the JSON the guard sends, or doesn't carry the script's token (the error
    /// doesn't show either token).
    pub fn from_payload(payload: &str, script: &GuardScript) -> Result<Self, MalformedRequest> {
        let raw: RawRequest = serde_json::from_str(payload).map_err(|error| MalformedRequest(error.to_string()))?;
        if !script.authenticates(&raw.token) {
            return Err(MalformedRequest("not the run's token".to_owned()));
        }
        let payload = match raw.kind.as_str() {
            "text" => Payload::Text(raw.data),
            "binary" => Payload::Binary(raw.data),
            "unreadable" => Payload::Unreadable,
            "foreign" => Payload::Foreign,
            other => return Err(MalformedRequest(format!("frame kind {other:?}"))),
        };
        Ok(Self {
            socket: raw.socket,
            seq: raw.seq,
            frame: Frame { url: raw.url, payload },
        })
    }
}

/// A denied frame, kept for the run's verdict. The URL has no query or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameDenial {
    pub url: String,
    pub cause: DenyCause,
    /// For a forced deny: the label of the rule that forced it ([`ForcedFrameDeny::labelled`]), if it has one.
    pub label: Option<String>,
}

impl FrameDenial {
    /// A deny of a frame on the socket at `url` (its query and fragment dropped) for `cause`, with no label.
    #[must_use]
    pub fn new(url: &str, cause: DenyCause) -> Self {
        Self {
            url: without_query(url).to_owned(),
            cause,
            label: None,
        }
    }
}

/// A frame the browser sent without an allow covering it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsanctionedFrame {
    /// The session it was reported on.
    pub session: Option<SessionId>,
    /// The socket's URL without its query, when its creation was seen.
    pub url: Option<String>,
    pub opcode: u64,
    /// The length of the content as reported (base64 for a binary frame).
    pub len: usize,
}

/// One allowed frame, waiting to be seen sent.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Sent {
    session: Option<SessionId>,
    opcode: u64,
    content: String,
}

#[derive(Default)]
struct Audit {
    /// How many allowed frames of each kind haven't been seen sent yet.
    allowed: HashMap<Sent, usize>,
    /// The URL of each socket by (session, `requestId`), for the breach report.
    sockets: HashMap<(Option<SessionId>, String), String>,
    unsanctioned: Vec<UnsanctionedFrame>,
}

/// The WebSocket layer of the browser fence.
pub struct WsFence {
    fence: Arc<Fence>,
    members: Arc<dyn MembershipSource + Send>,
    script: GuardScript,
    forced: Mutex<Vec<ForcedFrameDeny>>,
    denials: Mutex<Vec<FrameDenial>>,
    audit: Mutex<Audit>,
}

impl WsFence {
    /// `members` answers the membership checks of mapped writes; it has to fetch fresh on every call.
    #[must_use]
    pub fn new(fence: Arc<Fence>, members: Arc<dyn MembershipSource + Send>, script: GuardScript) -> Self {
        Self {
            fence,
            members,
            script,
            forced: Mutex::new(Vec::new()),
            denials: Mutex::new(Vec::new()),
            audit: Mutex::new(Audit::default()),
        }
    }

    /// The guard script arming installs.
    #[must_use]
    pub fn script(&self) -> &GuardScript {
        &self.script
    }

    fn audit(&self) -> MutexGuard<'_, Audit> {
        self.audit.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Denies every frame `rule` matches from now on, before it is classified (see [`ForcedFrameDeny`]).
    pub fn force_deny(&self, rule: ForcedFrameDeny) {
        self.forced.lock().unwrap_or_else(PoisonError::into_inner).push(rule);
    }

    /// Stops every forced deny of frames.
    pub fn clear_forced(&self) {
        self.forced.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// Classifies a frame and decides it. Every deny is logged and kept. A [`Payload::Foreign`] frame is denied
    /// unclassified ([`DenyCause::ForeignSocket`]; `fence.log` has it as `unclassified`); a forced one comes next
    /// ([`DenyCause::Forced`], logged as `forced`).
    pub async fn decide(&self, frame: &Frame) -> Verdict {
        if frame.payload == Payload::Foreign {
            let record = RequestRecord {
                channel: Channel::WebSocket,
                method: SEND,
                url: &frame.url,
                outcome: RequestOutcome::Unclassified,
            };
            // denied whether or not the log takes it
            let _ = self.fence.log().record_request(&record);
            self.keep(FrameDenial::new(&frame.url, DenyCause::ForeignSocket));
            return Verdict::Deny(DenyCause::ForeignSocket);
        }
        let forced = self
            .forced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|rule| rule.matches(frame))
            .cloned();
        if let Some(rule) = forced {
            let record = RequestRecord {
                channel: Channel::WebSocket,
                method: SEND,
                url: &frame.url,
                outcome: RequestOutcome::Forced,
            };
            // denied whether or not the log takes it
            let _ = self.fence.log().record_request(&record);
            self.keep(FrameDenial {
                label: rule.label,
                ..FrameDenial::new(&frame.url, DenyCause::Forced)
            });
            return Verdict::Deny(DenyCause::Forced);
        }
        self.judge(classify_frame(frame), &frame.url).await
    }

    /// Decides a frame already classified, sent on the socket at `url`. Every deny is logged and kept.
    pub async fn judge(&self, classification: Classification, url: &str) -> Verdict {
        let request = RequestRecord {
            channel: Channel::WebSocket,
            method: SEND,
            url,
            outcome: RequestOutcome::Unclassified,
        };
        let verdict = judge(&self.fence, self.members.as_ref(), classification, request).await;
        if let Verdict::Deny(cause) = verdict {
            self.keep(FrameDenial::new(url, cause));
        }
        verdict
    }

    fn keep(&self, denial: FrameDenial) {
        self.denials.lock().unwrap_or_else(PoisonError::into_inner).push(denial);
    }

    fn deny_malformed(&self) {
        let record = RequestRecord {
            channel: Channel::WebSocket,
            method: SEND,
            url: "?",
            outcome: RequestOutcome::Unclassified,
        };
        // denied whether or not the log takes it
        let _ = self.fence.log().record_request(&record);
        self.keep(FrameDenial::new("?", DenyCause::Malformed));
    }

    /// Every frame denied so far, in order.
    #[must_use]
    pub fn denials(&self) -> Vec<FrameDenial> {
        self.denials.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Every frame the browser sent without an allow, in order.
    #[must_use]
    pub fn unsanctioned(&self) -> Vec<UnsanctionedFrame> {
        self.audit().unsanctioned.clone()
    }

    /// How many allowed frames the browser hasn't reported sent yet. Once every allowed frame is out, any left over
    /// means the audit doesn't see the frames of the target that sent them.
    #[must_use]
    pub fn allows_not_seen_sent(&self) -> usize {
        self.audit().allowed.values().sum()
    }

    /// Decides one `Runtime.bindingCalled` event of the guard's binding and answers the guard in the context that asked.
    /// An allow is recorded for the audit, on the session that asked, before the guard hears of it: the guard asks only
    /// for its own realm's sockets, so that is the session the browser reports the frame on. A [`Payload::Foreign`]
    /// frame is denied and not answered. Calls of other bindings are ignored (`None`);
    /// a malformed request, or one without the run's token (not the guard's: see the module docs), is denied and left
    /// unanswered, so its frame waits forever and is never sent, and the token goes to no context that didn't show it.
    ///
    /// # Errors
    /// When the answer can't be delivered (the context is gone: its frame went with it).
    pub async fn handle_binding(&self, cdp: &Cdp, event: &Event) -> Result<Option<Verdict>, CdpError> {
        if event.params["name"] != BINDING {
            return Ok(None);
        }
        let request = event.params["payload"]
            .as_str()
            .ok_or_else(|| MalformedRequest("no payload".to_owned()))
            .and_then(|payload| GuardRequest::from_payload(payload, &self.script));
        let (Ok(request), Some(context)) = (request, event.params["executionContextId"].as_i64()) else {
            self.deny_malformed();
            return Ok(Some(Verdict::Deny(DenyCause::Malformed)));
        };
        let session = event.session_id.as_ref();
        let verdict = self.decide(&request.frame).await;
        if request.frame.payload == Payload::Foreign {
            return Ok(Some(verdict));
        }
        let allow = verdict == Verdict::Allow;
        if let (true, Some((opcode, content))) = (allow, request.frame.payload.sent_as()) {
            let sent = Sent {
                session: session.cloned(),
                opcode,
                content: content.to_owned(),
            };
            *self.audit().allowed.entry(sent).or_default() += 1;
        }
        let expression = self.script.verdict_expression(request.socket, request.seq, allow);
        let params = json!({"expression": expression, "contextId": context, "returnByValue": true});
        cdp.call(session, "Runtime.evaluate", params).await?;
        Ok(Some(verdict))
    }

    /// Feeds the audit one event: `Network.webSocketCreated` and `Network.webSocketClosed` track the sockets' URLs,
    /// and each `Network.webSocketFrameSent` uses up one allow, or is a breach (logged and kept). Other events are
    /// ignored.
    pub fn observe(&self, event: &Event) {
        let params = &event.params;
        let socket = || {
            let id = params["requestId"].as_str().unwrap_or("?").to_owned();
            (event.session_id.clone(), id)
        };
        match event.method.as_str() {
            "Network.webSocketCreated" => {
                let url = params["url"].as_str().unwrap_or("?").to_owned();
                self.audit().sockets.insert(socket(), url);
            }
            "Network.webSocketClosed" => {
                self.audit().sockets.remove(&socket());
            }
            "Network.webSocketFrameSent" => {
                let response = &params["response"];
                let sent = Sent {
                    session: event.session_id.clone(),
                    opcode: response["opcode"].as_u64().unwrap_or(u64::MAX),
                    content: response["payloadData"].as_str().unwrap_or_default().to_owned(),
                };
                self.audit_sent(sent, &socket());
            }
            _ => {}
        }
    }

    fn audit_sent(&self, sent: Sent, socket: &(Option<SessionId>, String)) {
        let mut audit = self.audit();
        if let Some(count) = audit.allowed.get_mut(&sent).filter(|count| **count > 0) {
            *count -= 1;
            if *count == 0 {
                audit.allowed.remove(&sent);
            }
            return;
        }
        let url = audit.sockets.get(socket).cloned();
        let record = RequestRecord {
            channel: Channel::WebSocket,
            method: SEND,
            url: url.as_deref().unwrap_or("?"),
            outcome: RequestOutcome::SentWithoutAllow,
        };
        // kept whether or not the log takes it: the breach fails the run either way
        let _ = self.fence.log().record_request(&record);
        audit.unsanctioned.push(UnsanctionedFrame {
            session: sent.session,
            url: url.as_deref().map(|url| without_query(url).to_owned()),
            opcode: sent.opcode,
            len: sent.content.len(),
        });
    }

    /// Drops what the audit holds for a session that detached: its sockets and its allows not yet seen sent.
    pub fn forget(&self, session: &SessionId) {
        let mut audit = self.audit();
        audit.allowed.retain(|sent, _| sent.session.as_ref() != Some(session));
        audit.sockets.retain(|(owner, _), _| owner.as_ref() != Some(session));
    }
}
