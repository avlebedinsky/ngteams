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

use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};
use teams_fence::{Channel, Fence, MembershipSource, RequestOutcome, RequestRecord};

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
        })
    }
}

/// A `Fetch.requestPaused` event without a field the fence needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a paused request without {0}")]
pub struct MalformedPause(pub &'static str);

/// Why a request was denied.
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
}

/// What happens to one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpVerdict {
    Allow,
    Deny(DenyCause),
}

/// A denied request, kept for the run's verdict. The URL has no query or fragment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Denial {
    pub method: String,
    pub url: String,
    pub cause: DenyCause,
}

/// The HTTP layer of the browser fence.
pub struct HttpFence {
    classifier: Classifier,
    fence: Arc<Fence>,
    members: Arc<dyn MembershipSource + Send>,
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
            denials: Mutex::new(Vec::new()),
        }
    }

    /// Classifies a request and decides it. Every deny is logged and kept.
    pub async fn decide(&self, request: &PausedRequest) -> HttpVerdict {
        let classification = self
            .classifier
            .classify(&request.method, &request.url, request.body.as_deref());
        self.judge(classification, &request.method, &request.url).await
    }

    /// Decides a request already classified. Every deny is logged and kept.
    pub async fn judge(&self, classification: Classification, method: &str, url: &str) -> HttpVerdict {
        let record = |outcome| RequestRecord {
            channel: Channel::Http,
            method,
            url,
            outcome,
        };
        let verdict = match classification {
            Classification::Read => HttpVerdict::Allow,
            Classification::Own(kind) => match self
                .fence
                .log()
                .record_request(&record(RequestOutcome::Own(kind.as_str())))
            {
                Ok(()) => HttpVerdict::Allow,
                Err(_) => HttpVerdict::Deny(DenyCause::LogUnavailable),
            },
            Classification::Write(target) => match self.fence.check(target, self.members.as_ref()).await {
                Ok(_permit) => HttpVerdict::Allow,
                Err(denied) => HttpVerdict::Deny(DenyCause::Fenced(*denied.reason())),
            },
            Classification::Unknown => {
                // denied whether or not the log takes it; a log failure fails the run through the deny anyway
                let _ = self.fence.log().record_request(&record(RequestOutcome::Unclassified));
                HttpVerdict::Deny(DenyCause::Unclassified)
            }
        };
        if let HttpVerdict::Deny(cause) = verdict {
            self.keep(method, url, cause);
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
        self.keep("?", "?", DenyCause::Malformed);
    }

    fn keep(&self, method: &str, url: &str, cause: DenyCause) {
        let end = url.find(['?', '#']).unwrap_or(url.len());
        self.denials
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Denial {
                method: method.to_owned(),
                url: url[..end].to_owned(),
                cause,
            });
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
    pub async fn handle(&self, cdp: &Cdp, event: &Event) -> Result<HttpVerdict, CdpError> {
        let session = event.session_id.as_ref();
        let Ok(request) = PausedRequest::from_params(&event.params) else {
            self.deny_malformed();
            if let Some(id) = event.params["requestId"].as_str() {
                fail(cdp, session, id).await?;
            }
            return Ok(HttpVerdict::Deny(DenyCause::Malformed));
        };
        let verdict = self.decide(&request).await;
        match verdict {
            HttpVerdict::Allow => {
                let params = json!({"requestId": request.request_id});
                cdp.call(session, "Fetch.continueRequest", params).await?;
            }
            HttpVerdict::Deny(_) => fail(cdp, session, &request.request_id).await?,
        }
        Ok(verdict)
    }
}

async fn fail(cdp: &Cdp, session: Option<&crate::cdp::SessionId>, request_id: &str) -> Result<(), CdpError> {
    let params = json!({"requestId": request_id, "errorReason": "BlockedByClient"});
    cdp.call(session, "Fetch.failRequest", params).await.map(drop)
}
