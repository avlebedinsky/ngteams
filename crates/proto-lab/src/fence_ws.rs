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
//! **The decision** classifies a frame ([`FrameClassifier`]): an own-state frame on the reviewed list passes (logged),
//! a mapped write goes through [`Fence::check`] (from M0 Task 11), anything else is denied and logged. Nothing is on
//! the seeded list, so until writes are mapped every frame is denied, and every deny fails the run. `fence.log` gets the
//! socket's URL without its query (channel `web_socket`, method `SEND`), never a frame's content.
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

use serde::Deserialize;
use serde_json::json;
use teams_fence::{Channel, Fence, MembershipSource, RequestOutcome, RequestRecord};
use url::Url;

use crate::cdp::{Cdp, CdpError, Event, SessionId};
use crate::classify::{Classification, OwnKind, UrlPattern};
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

/// One entry of the own-state list for frames: a text frame starting with `prefix`, on a socket whose URL matches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRule {
    kind: OwnKind,
    url: UrlPattern,
    prefix: String,
}

impl FrameRule {
    /// `origin` (`ws://` or `wss://`) and `path` as [`UrlPattern::new`] takes them; `prefix` is the start of the
    /// frame's text. A binary frame matches no rule.
    #[must_use]
    pub fn new(kind: OwnKind, origin: &str, path: &str, prefix: &str) -> Self {
        Self {
            kind,
            url: UrlPattern::new(origin, path),
            prefix: prefix.to_owned(),
        }
    }

    fn matches(&self, frame: &Frame) -> bool {
        let Payload::Text(text) = &frame.payload else {
            return false;
        };
        text.starts_with(&self.prefix) && Url::parse(&frame.url).is_ok_and(|url| self.url.matches(&url))
    }
}

/// The classifier for frames: the own-state list (and, from M0 Task 11, the write mapping).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameClassifier {
    own: Vec<FrameRule>,
    /// Whether the calling stack's own-state frames pass ([`FrameClassifier::calling`]).
    calling: bool,
}

impl FrameClassifier {
    /// The seeded list, which is empty: no frame has been reviewed as own-state yet, so every frame is
    /// [`Classification::Unknown`].
    #[must_use]
    pub fn seeded() -> Self {
        Self::default()
    }

    /// The seeded list plus the calling stack's own-state frames ([`crate::calling::trouter_frame`]): what a run that
    /// calls (`echo-call`) needs for the account's own push channel.
    #[must_use]
    pub fn calling() -> Self {
        Self {
            calling: true,
            ..Self::seeded()
        }
    }

    /// The same list plus one more own-state rule. Tests use it to stand a local socket in for a Microsoft one.
    #[must_use]
    pub fn with_own(mut self, rule: FrameRule) -> Self {
        self.own.push(rule);
        self
    }

    /// Classifies one frame: [`Classification::Own`] or [`Classification::Unknown`], never a read (any frame can be
    /// noticed by whoever is on the other end).
    #[must_use]
    pub fn classify(&self, frame: &Frame) -> Classification {
        if let Some(rule) = self.own.iter().find(|rule| rule.matches(frame)) {
            return Classification::Own(rule.kind);
        }
        match &frame.payload {
            Payload::Text(text) if self.calling => {
                crate::calling::trouter_frame(&frame.url, text).map_or(Classification::Unknown, Classification::Own)
            }
            _ => Classification::Unknown,
        }
    }
}

/// Classifies one frame with the seeded classifier.
#[must_use]
pub fn classify_frame(frame: &Frame) -> Classification {
    FrameClassifier::seeded().classify(frame)
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
    classifier: FrameClassifier,
    fence: Arc<Fence>,
    members: Arc<dyn MembershipSource + Send>,
    script: GuardScript,
    denials: Mutex<Vec<FrameDenial>>,
    audit: Mutex<Audit>,
}

impl WsFence {
    /// `members` answers the membership checks of mapped writes; it has to fetch fresh on every call.
    #[must_use]
    pub fn new(
        classifier: FrameClassifier,
        fence: Arc<Fence>,
        members: Arc<dyn MembershipSource + Send>,
        script: GuardScript,
    ) -> Self {
        Self {
            classifier,
            fence,
            members,
            script,
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

    /// Classifies a frame and decides it. Every deny is logged and kept. A [`Payload::Foreign`] frame is denied
    /// unclassified ([`DenyCause::ForeignSocket`]; `fence.log` has it as `unclassified`).
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
            self.keep(&frame.url, DenyCause::ForeignSocket);
            return Verdict::Deny(DenyCause::ForeignSocket);
        }
        let classification = self.classifier.classify(frame);
        self.judge(classification, &frame.url).await
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
            self.keep(url, cause);
        }
        verdict
    }

    fn keep(&self, url: &str, cause: DenyCause) {
        self.denials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(FrameDenial {
                url: without_query(url).to_owned(),
                cause,
            });
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
        self.keep("?", DenyCause::Malformed);
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
