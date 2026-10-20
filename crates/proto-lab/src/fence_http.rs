//! The browser fence's HTTP layer: what happens to one request `Fetch` paused.
//!
//! [`HttpFence::decide`] classifies the request ([`crate::classify`]) and decides:
//! - a read passes, unlogged;
//! - an own-state request passes, and is logged to `fence.log` (one that can't be logged is denied);
//! - a mapped write passes only with a [`teams_fence::WritePermit`] from [`Fence::check`], which fetches the target's
//!   members fresh and logs its own decision;
//! - anything else is denied and logged.
//!
//! [`HttpFence::handle`] carries a decision out: `Fetch.continueRequest` on allow, `Fetch.failRequest` (`BlockedByClient`)
//! on deny, so a denied request never reaches the network and the page sees a network error. Every deny is also kept in
//! memory ([`HttpFence::denials`]), and any deny fails the run.
//!
//! A run can also force a deny ([`HttpFence::force_deny`], [`ForcedDeny`]): a request it matches is denied before it is
//! classified, whatever it is. That is how a scenario checks, live, that a denied write never leaves the browser.
//!
//! For a run that writes, [`HttpFence::with_credentials`] hands the live membership source the lab account's chat
//! service credential: [`HttpFence::handle`] offers every paused request's URL and headers, from the raw event, to
//! [`ChatCredential::from_request`] before it builds the [`PausedRequest`], which never holds a header. The raw event,
//! headers included, has been handed to the run's event watchers and offered to the capture recorder before that.

use std::sync::{Arc, Mutex, PoisonError};

use regex::Regex;
use serde_json::{Value, json};
use teams_fence::{
    Channel, ChatCredential, Fence, LiveMembershipSource, MembershipSource, RequestOutcome, RequestRecord,
};

use crate::cdp::{Cdp, CdpError, Event};
use crate::classify::{Classification, Classifier};

/// One request as `Fetch.requestPaused` shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausedRequest {
    /// The id to continue or fail it with, on the session it paused on.
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// The body as `postData` carries it. Missing when the request has none, or when Chrome didn't inline it.
    pub body: Option<Vec<u8>>,
    /// The request's `Network` id (`networkId`), the one the page's `Network` events know it by. Missing when the event
    /// doesn't give one.
    pub network_id: Option<String>,
}

impl PausedRequest {
    /// Reads a `Fetch.requestPaused` event's params.
    ///
    /// # Errors
    /// [`MalformedPause`] when the request id, method or URL is missing.
    pub fn from_params(params: &Value) -> Result<Self, MalformedPause> {
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let request = &params["request"];
        Ok(Self {
            request_id: text(&params["requestId"]).ok_or(MalformedPause("requestId"))?,
            method: text(&request["method"]).ok_or(MalformedPause("request.method"))?,
            url: text(&request["url"]).ok_or(MalformedPause("request.url"))?,
            body: request["postData"].as_str().map(|body| body.as_bytes().to_vec()),
            network_id: text(&params["networkId"]),
        })
    }
}

/// A `Fetch.requestPaused` event without a field the fence needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a paused request without {0}")]
pub struct MalformedPause(pub &'static str);

/// Why a request (or a WebSocket frame) was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyCause {
    /// Nothing classifies it.
    Unclassified,
    /// An own-state request whose allow couldn't be written to `fence.log`.
    LogUnavailable,
    /// The fence refused the write (its reason is in `fence.log`).
    Fenced(teams_fence::DenyReason),
    /// The paused event couldn't be read.
    Malformed,
    /// A WebSocket frame sent through another realm's `send` (not the one whose `WebSocket` made the socket); the guard
    /// dropped it unasked.
    ForeignSocket,
    /// The run forced a deny on it ([`ForcedDeny`]).
    Forced,
}

/// What happens to one request (or, in the WebSocket layer, one frame).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny(DenyCause),
}

/// A denied request, kept for the run's verdict. The URL has no query or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    pub method: String,
    pub url: String,
    pub cause: DenyCause,
    /// For a forced deny: the label of the rule that forced it ([`ForcedDeny::labelled`]), if it has one.
    pub label: Option<String>,
    /// For a forced deny: the request's `Network` id ([`PausedRequest::network_id`]), if the event gave one.
    pub network_id: Option<String>,
}

impl Denial {
    /// A deny of `method` `url` (its query and fragment dropped) for `cause`, with no label or `Network` id.
    #[must_use]
    pub fn new(method: &str, url: &str, cause: DenyCause) -> Self {
        Self {
            method: method.to_owned(),
            url: without_query(url).to_owned(),
            cause,
            label: None,
            network_id: None,
        }
    }
}

/// A deny a run asks for on purpose: every request with this method, a URL the pattern finds a match in, and (when
/// given) a body holding the text, is denied before it is classified. When several match a request, the first one the
/// run forced is the one its [`Denial`] names.
#[derive(Clone, Debug)]
pub struct ForcedDeny {
    method: String,
    url: Regex,
    body_contains: Option<String>,
    label: Option<String>,
}

impl ForcedDeny {
    /// Requests with `method` (any case) whose URL `url_pattern` (a regular expression) finds a match in.
    ///
    /// # Errors
    /// When the pattern doesn't compile.
    pub fn new(method: &str, url_pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            method: method.to_ascii_uppercase(),
            url: Regex::new(url_pattern)?,
            body_contains: None,
            label: None,
        })
    }

    /// Names the rule: a request it denies is kept with this label ([`Denial::label`]), so a run can tell its own
    /// forced denies from another rule's.
    #[must_use]
    pub fn labelled(mut self, label: &str) -> Self {
        self.label = Some(label.to_owned());
        self
    }

    /// Only requests whose body holds `text`.
    #[must_use]
    pub fn with_body_containing(mut self, text: &str) -> Self {
        self.body_contains = Some(text.to_owned());
        self
    }

    /// Whether it denies `request`. A body Chrome didn't inline doesn't hold the text.
    #[must_use]
    pub fn matches(&self, request: &PausedRequest) -> bool {
        let body_matches = self.body_contains.as_ref().is_none_or(|text| {
            request
                .body
                .as_deref()
                .is_some_and(|body| String::from_utf8_lossy(body).contains(text.as_str()))
        });
        self.method.eq_ignore_ascii_case(&request.method) && self.url.is_match(&request.url) && body_matches
    }
}

/// The HTTP layer of the browser fence.
pub struct HttpFence {
    classifier: Classifier,
    fence: Arc<Fence>,
    members: Arc<dyn MembershipSource + Send>,
    /// Where the lab account's chat service credential goes, for a run that writes.
    credentials: Option<Arc<LiveMembershipSource>>,
    forced: Mutex<Vec<ForcedDeny>>,
    denials: Mutex<Vec<Denial>>,
}

impl HttpFence {
    /// `members` answers the membership checks of mapped writes; it has to fetch fresh on every call.
    #[must_use]
    pub fn new(classifier: Classifier, fence: Arc<Fence>, members: Arc<dyn MembershipSource + Send>) -> Self {
        Self {
            classifier,
            fence,
            members,
            credentials: None,
            forced: Mutex::new(Vec::new()),
            denials: Mutex::new(Vec::new()),
        }
    }

    /// Hands `live` the chat service credential of every paused request that carries one (see the module docs). The
    /// live source is normally the fence's membership source too.
    #[must_use]
    pub fn with_credentials(mut self, live: Arc<LiveMembershipSource>) -> Self {
        self.credentials = Some(live);
        self
    }

    /// Denies every request `rule` matches from now on, before it is classified (see [`ForcedDeny`]).
    pub fn force_deny(&self, rule: ForcedDeny) {
        self.forced.lock().unwrap_or_else(PoisonError::into_inner).push(rule);
    }

    /// Stops every forced deny.
    pub fn clear_forced(&self) {
        self.forced.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// The first forced deny that matches `request`.
    fn forced(&self, request: &PausedRequest) -> Option<ForcedDeny> {
        self.forced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|rule| rule.matches(request))
            .cloned()
    }

    /// Offers the request of a `Fetch.requestPaused` event's `params` to the live source as its credential, when the run
    /// has one ([`Self::with_credentials`]). [`Self::handle`] calls it for every paused request.
    pub fn offer_credential(&self, params: &Value) {
        let Some(live) = &self.credentials else {
            return;
        };
        let request = &params["request"];
        let (Some(url), Some(headers)) = (request["url"].as_str(), request["headers"].as_object()) else {
            return;
        };
        let headers = headers
            .iter()
            .filter_map(|(name, value)| Some((name.as_str(), value.as_str()?)));
        if let Some(credential) = ChatCredential::from_request(url, headers) {
            live.set_credential(credential);
        }
    }

    /// Classifies a request and decides it. Every deny is logged and kept; a forced one comes first.
    pub async fn decide(&self, request: &PausedRequest) -> Verdict {
        if let Some(rule) = self.forced(request) {
            let record = RequestRecord {
                channel: Channel::Http,
                method: &request.method,
                url: &request.url,
                outcome: RequestOutcome::Forced,
            };
            // denied whether or not the log takes it
            let _ = self.fence.log().record_request(&record);
            self.keep(Denial {
                label: rule.label,
                network_id: request.network_id.clone(),
                ..Denial::new(&request.method, &request.url, DenyCause::Forced)
            });
            return Verdict::Deny(DenyCause::Forced);
        }
        let classification = self
            .classifier
            .classify(&request.method, &request.url, request.body.as_deref());
        self.judge(classification, &request.method, &request.url).await
    }

    /// Decides a request already classified. Every deny is logged and kept.
    pub async fn judge(&self, classification: Classification, method: &str, url: &str) -> Verdict {
        let request = RequestRecord {
            channel: Channel::Http,
            method,
            url,
            outcome: RequestOutcome::Unclassified,
        };
        let verdict = judge(&self.fence, self.members.as_ref(), classification, request).await;
        if let Verdict::Deny(cause) = verdict {
            self.keep(Denial::new(method, url, cause));
        }
        verdict
    }

    fn deny_malformed(&self) {
        let record = RequestRecord {
            channel: Channel::Http,
            method: "?",
            url: "?",
            outcome: RequestOutcome::Unclassified,
        };
        // denied whether or not the log takes it
        let _ = self.fence.log().record_request(&record);
        self.keep(Denial::new("?", "?", DenyCause::Malformed));
    }

    fn keep(&self, denial: Denial) {
        self.denials.lock().unwrap_or_else(PoisonError::into_inner).push(denial);
    }

    /// Every request denied so far, in order.
    #[must_use]
    pub fn denials(&self) -> Vec<Denial> {
        self.denials.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Decides one `Fetch.requestPaused` event and answers it on the session it paused on: continue on allow, fail with
    /// `BlockedByClient` on deny. A malformed event is denied (and failed, when it names a request id).
    ///
    /// # Errors
    /// When the browser refuses the answer; a request that is gone (its target closed) can't be answered either.
    pub async fn handle(&self, cdp: &Cdp, event: &Event) -> Result<Verdict, CdpError> {
        let session = event.session_id.as_ref();
        self.offer_credential(&event.params);
        let Ok(request) = PausedRequest::from_params(&event.params) else {
            self.deny_malformed();
            if let Some(id) = event.params["requestId"].as_str() {
                fail(cdp, session, id).await?;
            }
            return Ok(Verdict::Deny(DenyCause::Malformed));
        };
        let verdict = self.decide(&request).await;
        match verdict {
            Verdict::Allow => {
                let params = json!({"requestId": request.request_id});
                cdp.call(session, "Fetch.continueRequest", params).await?;
            }
            Verdict::Deny(_) => fail(cdp, session, &request.request_id).await?,
        }
        Ok(verdict)
    }
}

/// Decides one request or frame already classified, for either layer of the browser fence: a read passes unlogged, an
/// own-state one passes logged (denied if the log won't take it), a mapped write passes only with a permit from
/// [`Fence::check`] (which logs it), and anything else is denied and logged. `request` names the channel, method and
/// URL for the log; its outcome is set here.
pub(crate) async fn judge(
    fence: &Fence,
    members: &(dyn MembershipSource + Send),
    classification: Classification,
    request: RequestRecord<'_>,
) -> Verdict {
    let record = |outcome| RequestRecord { outcome, ..request };
    match classification {
        Classification::Read => Verdict::Allow,
        Classification::Own(kind) => match fence.log().record_request(&record(RequestOutcome::Own(kind.as_str()))) {
            Ok(()) => Verdict::Allow,
            Err(_) => Verdict::Deny(DenyCause::LogUnavailable),
        },
        Classification::Write(target) => match fence.check(target, members).await {
            Ok(_permit) => Verdict::Allow,
            Err(denied) => Verdict::Deny(DenyCause::Fenced(*denied.reason())),
        },
        Classification::Unknown => {
            // denied whether or not the log takes it; a log failure fails the run through the deny anyway
            let _ = fence.log().record_request(&record(RequestOutcome::Unclassified));
            Verdict::Deny(DenyCause::Unclassified)
        }
    }
}

/// A URL without its query and fragment, where tokens travel.
pub(crate) fn without_query(url: &str) -> &str {
    &url[..url.find(['?', '#']).unwrap_or(url.len())]
}

async fn fail(cdp: &Cdp, session: Option<&crate::cdp::SessionId>, request_id: &str) -> Result<(), CdpError> {
    let params = json!({"requestId": request_id, "errorReason": "BlockedByClient"});
    cdp.call(session, "Fetch.failRequest", params).await.map(drop)
}
