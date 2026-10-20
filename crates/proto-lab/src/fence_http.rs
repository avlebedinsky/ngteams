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
//! **The authorization redirect** (`proto-lab auth` only, [`RedirectCatch`]). The native sign-in takes its code from
//! Entra's redirect to the registered redirect URI, which must never load: the `nativeclient` page needn't, and Chrome
//! won't navigate to the `msauth.com.microsoft.teams2://auth` custom scheme at all. A fence given a catch also pauses
//! the responses of Entra's login origin, on the browser session only ([`HttpFence::fetch_patterns`]). A 3xx whose
//! `Location` is the registered redirect URI is caught there: the URL goes to the catch's receiver and the response is
//! failed, so the browser never follows it. A `GET` to the redirect URI that reaches the request stage anyway (a page
//! that sends the browser there by script) is caught and failed the same way. Every other paused response is continued:
//! its request was already let through at the request stage, and continuing a response sends nothing. So the catch
//! only ever fails what would otherwise go on; it lets nothing through that the fence would deny, and a fence without
//! one pauses no response at all (one paused anyway is failed, and fails the run).
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
use teams_auth::redirect::{RedirectError, RedirectUri};
use teams_auth::secrets::Secret;
use teams_fence::{
    Channel, ChatCredential, Fence, LiveMembershipSource, MembershipSource, RequestOutcome, RequestRecord,
};
use tokio::sync::mpsc;
use url::Url;

use crate::cdp::{Cdp, CdpError, Event, SessionId};
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

/// What [`HttpFence::handle`] did with one paused request or response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// A request, decided.
    Decided(Verdict),
    /// The authorization redirect: caught, handed to the catch's receiver, and failed.
    RedirectCaught,
    /// A paused response of the login origin that isn't the redirect: continued.
    ResponseContinued,
}

/// Why a [`RedirectCatch`] can't be made.
#[derive(Debug, thiserror::Error)]
pub enum CatchError {
    /// The redirect URI isn't usable.
    #[error(transparent)]
    RedirectUri(#[from] RedirectError),
    /// The authorize endpoint has no HTTP(S) origin whose responses could be paused.
    #[error("the authorize endpoint must be an absolute http(s) URL")]
    AuthorizeEndpoint,
}

/// Catches the authorization redirect to one registered redirect URI (see the module docs). Made for one
/// `proto-lab auth` run; the URL it catches carries the authorization code, so it is handed over as a [`Secret`].
#[derive(Debug)]
pub struct RedirectCatch {
    /// The origin whose responses pause at the response stage: the authorize endpoint's.
    login_origin: String,
    redirect: RedirectUri,
    caught: mpsc::UnboundedSender<Secret>,
}

impl RedirectCatch {
    /// A catch for redirects to `redirect_uri` from the origin of `authorize_endpoint`, and the receiver of the URLs it
    /// catches.
    ///
    /// # Errors
    /// [`CatchError::RedirectUri`] when `redirect_uri` isn't a bare absolute URL ([`RedirectUri::parse`]);
    /// [`CatchError::AuthorizeEndpoint`] when `authorize_endpoint` has no HTTP(S) origin.
    pub fn new(
        authorize_endpoint: &str,
        redirect_uri: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Secret>), CatchError> {
        let redirect = RedirectUri::parse(redirect_uri)?;
        let login_origin = Url::parse(authorize_endpoint)
            .ok()
            .filter(|url| matches!(url.scheme(), "https" | "http"))
            .map(|url| url.origin().ascii_serialization())
            .ok_or(CatchError::AuthorizeEndpoint)?;
        let (caught, receiver) = mpsc::unbounded_channel();
        let catch = Self {
            login_origin,
            redirect,
            caught,
        };
        Ok((catch, receiver))
    }

    /// The `Fetch` pattern that pauses the login origin's responses.
    fn response_pattern(&self) -> Value {
        json!({"urlPattern": format!("{}/*", self.login_origin), "requestStage": "Response"})
    }

    /// Whether `url` is the redirect; if it is, it goes to the receiver.
    fn take(&self, url: &str) -> bool {
        let caught = self.redirect.matches(url);
        if caught {
            // a receiver that is gone has stopped waiting; the redirect is failed all the same
            let _ = self.caught.send(Secret::new(url));
        }
        caught
    }
}

/// The URL a paused 3xx response redirects to (its `Location`, resolved against the request's URL).
fn redirect_target(params: &Value) -> Option<String> {
    let status = params["responseStatusCode"].as_u64()?;
    if !(300..400).contains(&status) {
        return None;
    }
    let location = params["responseHeaders"].as_array()?.iter().find(|header| {
        header["name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("location"))
    })?["value"]
        .as_str()?;
    let base = Url::parse(params["request"]["url"].as_str()?).ok()?;
    base.join(location).ok().map(String::from)
}

/// Whether a `Fetch.requestPaused` event is at the response stage.
fn at_response_stage(params: &Value) -> bool {
    params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some()
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
    catch: Option<RedirectCatch>,
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
            catch: None,
        }
    }

    /// The same fence, catching the authorization redirect (see the module docs).
    #[must_use]
    pub fn with_redirect_catch(mut self, catch: RedirectCatch) -> Self {
        self.catch = Some(catch);
        self
    }

    /// The `Fetch.enable` params for the browser session: every request at the request stage, plus, with a redirect
    /// catch, the responses of the login origin.
    #[must_use]
    pub fn fetch_patterns(&self) -> Value {
        let mut patterns = vec![json!({"urlPattern": "*"})];
        patterns.extend(self.catch.as_ref().map(RedirectCatch::response_pattern));
        json!({ "patterns": patterns })
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
    /// `BlockedByClient` on deny. A malformed event is denied (and failed, when it names a request id). With a redirect
    /// catch, a `GET` to the redirect URI is caught and failed, and a paused response is handled as the module docs say.
    ///
    /// # Errors
    /// When the browser refuses the answer; a request that is gone (its target closed) can't be answered either.
    pub async fn handle(&self, cdp: &Cdp, event: &Event) -> Result<Handled, CdpError> {
        let session = event.session_id.as_ref();
        if at_response_stage(&event.params) {
            return self.handle_response(cdp, session, &event.params).await;
        }
        self.offer_credential(&event.params);
        let Ok(request) = PausedRequest::from_params(&event.params) else {
            self.deny_malformed();
            if let Some(id) = event.params["requestId"].as_str() {
                fail(cdp, session, id).await?;
            }
            return Ok(Handled::Decided(Verdict::Deny(DenyCause::Malformed)));
        };
        let get = request.method.eq_ignore_ascii_case("GET");
        if get && self.catch.as_ref().is_some_and(|catch| catch.take(&request.url)) {
            fail(cdp, session, &request.request_id).await?;
            return Ok(Handled::RedirectCaught);
        }
        let verdict = self.decide(&request).await;
        match verdict {
            Verdict::Allow => {
                let params = json!({"requestId": request.request_id});
                cdp.call(session, "Fetch.continueRequest", params).await?;
            }
            Verdict::Deny(_) => fail(cdp, session, &request.request_id).await?,
        }
        Ok(Handled::Decided(verdict))
    }

    /// A response paused at the response stage: the redirect is caught and failed, anything else continued. Only a
    /// fence with a catch asks for responses; one paused without a catch is failed and fails the run.
    async fn handle_response(
        &self,
        cdp: &Cdp,
        session: Option<&SessionId>,
        params: &Value,
    ) -> Result<Handled, CdpError> {
        let id = params["requestId"].as_str().unwrap_or_default();
        let Some(catch) = &self.catch else {
            self.deny_malformed();
            fail(cdp, session, id).await?;
            return Ok(Handled::Decided(Verdict::Deny(DenyCause::Malformed)));
        };
        if redirect_target(params).is_some_and(|target| catch.take(&target)) {
            fail(cdp, session, id).await?;
            return Ok(Handled::RedirectCaught);
        }
        cdp.call(session, "Fetch.continueResponse", json!({"requestId": id}))
            .await?;
        Ok(Handled::ResponseContinued)
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

async fn fail(cdp: &Cdp, session: Option<&SessionId>, request_id: &str) -> Result<(), CdpError> {
    let params = json!({"requestId": request_id, "errorReason": "BlockedByClient"});
    cdp.call(session, "Fetch.failRequest", params).await.map(drop)
}
