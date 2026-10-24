//! The browser fence's HTTP layer: what happens to one request `Fetch` paused.
//!
//! [`HttpFence::decide`] classifies the request ([`crate::classify`], the lab rule's `teams_fence::contacting`) and
//! decides:
//! - a read passes, unlogged;
//! - a mutating request that reaches nobody passes, and is logged to `fence.log` as `allowed` by its method, host and
//!   path shape (one that can't be logged is denied);
//! - a mapped write passes only with a [`teams_fence::WritePermit`] from [`Fence::check`], which fetches the target's
//!   members fresh and logs its own decision;
//! - a mutating request on a contacting service that nothing reads is denied and logged.
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
//! one pauses no response of its own (any other paused response but the call start's answer, below, is failed, and fails
//! the run).
//!
//! **The call start's answer** feeds the fence's call registry (`teams_fence`'s `calls`: a call's follow-ups pass only
//! on a call this fence permitted, to the links its services handed out). A call start the fence permits is kept,
//! with its body, as a pending call ([`teams_fence::Fence::pend_start`]) under the request's `Network` id, which both
//! fetch layers see; the browser session also pauses the responses to [`CALL_START_URL`] at the response stage (of any
//! method: the start's CORS preflight `OPTIONS` pauses too, and is continued), and when the answer to a start's `POST`
//! kept so is a success, its body (`Fetch.getResponseBody`) registers the call ([`teams_fence::Fence::register_start`])
//! before the response is continued, so the page can't name the call before the fence knows it. A join of a running
//! call with media pends and registers the same way: the browser session also pauses the answers to a conversation's
//! requests ([`CONVERSATIONS`]). Any other answer registers nothing. Continuing a response sends nothing.
//!
//! **An `attach`'s and an `accept`'s answers** do the same on the callee's side: an `attach` or an `accept` the fence
//! permits on an offered invitation pends under its `Network` id ([`teams_fence::Fence::pend_answer`]), the browser
//! session pauses the responses to an invitation's `attach` and `accept` links ([`CALL_ANSWERS`]) at the response
//! stage, and a success's body binds the links it hands out (an `attach`'s: the call leg's and the conversation's; an
//! `accept`'s: the answered call's `mediaRenegotiation`, `updateMediaDescriptions` and `applyChannelParameters`) to
//! the invitation ([`teams_fence::Fence::bind_answer`]) before it is continued. The capture records whether such a
//! read answer registered or bound anything ([`Handled::CallAnswerRead`]).
//!
//! A run can also force a deny ([`HttpFence::force_deny`], [`ForcedDeny`]): a request it matches is denied before it is
//! classified, whatever it is. That is how a scenario checks, live, that a denied write never leaves the browser.
//!
//! For a run that writes, [`HttpFence::with_credentials`] hands the live membership source the lab account's chat
//! service credential: [`HttpFence::handle`] offers every paused request's URL and headers, from the raw event, to
//! [`ChatCredential::from_request`] before it builds the [`PausedRequest`], which holds every header's name but the
//! value only of the method overrides the classifier reads ([`OVERRIDE_HEADERS`]), never a token or a cookie. The raw
//! event, headers included, has been handed to the run's event watchers and offered to the capture recorder before
//! that.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use regex::Regex;
use serde_json::{Value, json};
use teams_auth::redirect::{RedirectError, RedirectUri};
use teams_auth::secrets::Secret;
use teams_fence::{
    AllowedRecord, CALL_START_URL, CallOperation, Channel, ChatCredential, Fence, LiveMembershipSource,
    MembershipSource, OVERRIDE_HEADERS, PendingAnswer, PendingStart, RequestOutcome, RequestRecord, Scope, WriteKind,
    WritePermit,
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
    /// The request's headers as the event lists them, each once: every header's name, with its value only when the
    /// classifier reads it (a method override, [`OVERRIDE_HEADERS`]) and it is a string; so no credential
    /// (`authorization`, `Cookie`) is held here, and `Debug` prints none.
    pub headers: Vec<(String, Option<String>)>,
    /// The body as `postData` carries it. Missing when the request has none, or when Chrome didn't inline it.
    pub body: Option<Vec<u8>>,
    /// Whether the event says something of a body that `body` doesn't hold: a `postData` that isn't a string, or
    /// `hasPostData` or `postDataEntries` (of any value) without a string `postData`. Only with it false does a missing
    /// `body` mean the request has none; the classifier reads such a request as one with an empty body
    /// ([`Self::classified_body`]).
    pub body_unread: bool,
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
            headers: request["headers"]
                .as_object()
                .map(|headers| {
                    headers
                        .iter()
                        .map(|(name, value)| {
                            let read = OVERRIDE_HEADERS.iter().any(|read| read.eq_ignore_ascii_case(name));
                            let value = value.as_str().filter(|_| read).map(str::to_owned);
                            (name.clone(), value)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            body: request["postData"].as_str().map(|body| body.as_bytes().to_vec()),
            body_unread: !request["postData"].is_string()
                && ["postData", "hasPostData", "postDataEntries"]
                    .iter()
                    .any(|key| request.get(key).is_some()),
            network_id: text(&params["networkId"]),
        })
    }

    /// The body the classifier reads: the inlined one, an empty one for a body Chrome didn't inline ([`Self::body_unread`]:
    /// the request is mutating, and a row that reads its body finds nothing it takes), or none.
    #[must_use]
    pub fn classified_body(&self) -> Option<&[u8]> {
        self.body.as_deref().or_else(|| self.body_unread.then_some(&[][..]))
    }
}

/// A `Fetch.requestPaused` event without a field the fence needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a paused request without {0}")]
pub struct MalformedPause(pub &'static str);

/// Why a request (or a WebSocket frame) was denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyCause {
    /// A mutating request (or a frame) that could reach people and that nothing reads: unknown to the classifier.
    Unclassified,
    /// A request (or a frame) let through as reaching nobody whose `allowed` entry, or an unclassified or forced one
    /// whose deny, couldn't be written to `fence.log`.
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
    /// A Trouter delivery response answering a pending call delivery, which the fence refused (its reason is in
    /// `fence.log`).
    DeliveryResponse(teams_fence::DeliveryDenyReason),
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
    except: Option<Regex>,
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
            except: None,
            body_contains: None,
            label: None,
        })
    }

    /// But not requests whose URL `url_pattern` finds a match in: those go to the fence's own decision, as any other
    /// request does.
    ///
    /// # Errors
    /// When the pattern doesn't compile.
    pub fn except(mut self, url_pattern: &str) -> Result<Self, regex::Error> {
        self.except = Some(Regex::new(url_pattern)?);
        Ok(self)
    }

    /// Names the rule: a request it denies is kept with this label ([`Denial::label`]), so a run can tell its own
    /// forced denies from another rule's.
    #[must_use]
    pub fn labelled(mut self, label: &str) -> Self {
        self.label = Some(label.to_owned());
        self
    }

    /// The rule's label ([`ForcedDeny::labelled`]), if it has one.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
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
        let excepted = self.except.as_ref().is_some_and(|except| except.is_match(&request.url));
        self.method.eq_ignore_ascii_case(&request.method)
            && self.url.is_match(&request.url)
            && !excepted
            && body_matches
    }
}

/// What [`HttpFence::handle`] did with one paused request or response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    /// A request, decided.
    Decided(Verdict),
    /// The authorization redirect: caught, handed to the catch's receiver, and failed.
    RedirectCaught,
    /// A paused response continued: the login origin's that isn't the redirect, or a call answer (a call start's or a
    /// join's, an `attach`'s or an `accept`'s) no permitted request of the fence's waited for.
    ResponseContinued,
    /// A paused call answer to a `POST` the fence permitted and waited for (a call start or a join with media, an
    /// `attach`, an `accept`), read and then continued.
    CallAnswerRead {
        /// Whether the answer registered its call or bound the links it hands out to its invitation.
        bound: bool,
    },
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
    /// The call starts this fence permitted, by the request's `Network` id, waiting for their answers (see the module
    /// docs).
    starts: Mutex<HashMap<String, PendingStart>>,
    /// The `attach`es and `accept`s this fence permitted, by the request's `Network` id, waiting for their answers.
    answers: Mutex<HashMap<String, PendingAnswer>>,
}

/// The `Fetch` pattern that pauses the call start's answer, on the browser session.
fn call_start_answer_pattern() -> Value {
    json!({"urlPattern": CALL_START_URL, "requestStage": "Response"})
}

/// An invitation's `attach` link and its leg's `accept` link, as a regular expression: where the answers
/// [`call_answer_patterns`] pause go to.
pub const CALL_ANSWERS: &str = concat!(
    r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/cc/v1/",
    r"(forked/[^?]+/attach|incoming/[^?]+/accept)(\?|$)"
);

/// The `Fetch` patterns that pause the answers to `attach` and `accept` links (every one on the flight proxy's call
/// controllers; only a permitted one's binds anything), on the browser session.
fn call_answer_patterns() -> [Value; 2] {
    ["forked/*/attach*", "incoming/*/accept*"].map(|link| {
        json!({
            "urlPattern": format!("https://api.flightproxy.teams.microsoft.com/api/v2/ep/*/cc/v1/{link}"),
            "requestStage": "Response",
        })
    })
}

/// Whether a paused response answers a request to an `attach` or an `accept` link ([`CALL_ANSWERS`]).
fn is_call_answer(params: &Value) -> bool {
    params["request"]["url"]
        .as_str()
        .is_some_and(|url| Regex::new(CALL_ANSWERS).is_ok_and(|answers| answers.is_match(url)))
}

/// Whether a paused response is one [`call_start_answer_pattern`] or [`conversation_answer_pattern`] pauses: a request
/// to [`CALL_START_URL`] exactly, or to a conversation on the flight proxy ([`CONVERSATIONS`]: a join goes to the
/// conversation itself), of any method (the patterns match every method, a CORS preflight `OPTIONS` too). Only a
/// `POST`'s answer to a start or join the fence pended can register a call ([`HttpFence::read_call_start_answer`]);
/// every one is continued.
fn is_call_start_answer(params: &Value) -> bool {
    params["request"]["url"].as_str().is_some_and(|url| {
        url == CALL_START_URL || Regex::new(CONVERSATIONS).is_ok_and(|conversations| conversations.is_match(url))
    })
}

/// Any URL of a conversation on the flight proxy (its own, or an operation on it), as a regular expression.
pub const CONVERSATIONS: &str = r"^https://api\.flightproxy\.teams\.microsoft\.com/api/v2/ep/[^/?]+/conv/";

/// The `Fetch` pattern that pauses the answers to a conversation's requests (a join's among them), on the browser
/// session.
fn conversation_answer_pattern() -> Value {
    json!({"urlPattern": "https://api.flightproxy.teams.microsoft.com/api/v2/ep/*/conv/*", "requestStage": "Response"})
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
            starts: Mutex::new(HashMap::new()),
            answers: Mutex::new(HashMap::new()),
        }
    }

    /// The same fence, catching the authorization redirect (see the module docs).
    #[must_use]
    pub fn with_redirect_catch(mut self, catch: RedirectCatch) -> Self {
        self.catch = Some(catch);
        self
    }

    /// The fence's `fence.log`, for what the browser fence records beside its decisions (a target it couldn't arm).
    pub(crate) fn log(&self) -> &teams_fence::FenceLog {
        self.fence.log()
    }

    /// The `Fetch.enable` params for the browser session: every request at the request stage, the call start's answer,
    /// a conversation's answers (a join's) and the `attach` and `accept` answers at the response stage, plus, with a
    /// redirect catch, the responses of the login origin.
    #[must_use]
    pub fn fetch_patterns(&self) -> Value {
        let mut patterns = vec![
            json!({"urlPattern": "*"}),
            call_start_answer_pattern(),
            conversation_answer_pattern(),
        ];
        patterns.extend(call_answer_patterns());
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

    /// Classifies a request and decides it. Every deny is logged and kept; a forced one comes first
    /// ([`DenyCause::Forced`]; [`DenyCause::LogUnavailable`] when the log won't take it).
    pub async fn decide(&self, request: &PausedRequest) -> Verdict {
        if let Some(rule) = self.forced(request) {
            let record = RequestRecord {
                channel: Channel::Http,
                method: &request.method,
                url: &request.url,
                outcome: RequestOutcome::Forced,
            };
            // denied either way; a deny the log won't take is `LogUnavailable`, so no scenario's review expects it
            let cause = match self.fence.log().record_request(&record) {
                Ok(()) => DenyCause::Forced,
                Err(_) => DenyCause::LogUnavailable,
            };
            self.keep(Denial {
                label: rule.label,
                network_id: request.network_id.clone(),
                ..Denial::new(&request.method, &request.url, cause)
            });
            return Verdict::Deny(cause);
        }
        let classification = self.classifier.classify(
            &request.method,
            &request.url,
            &request.headers,
            request.classified_body(),
        );
        let (verdict, permit) = self.judge_keeping(classification, &request.method, &request.url).await;
        if let (Some(permit), Some(network_id)) = (&permit, &request.network_id) {
            self.pend_answer(permit, network_id);
        }
        if let (Some(permit), Some(network_id), Some(body)) = (permit, &request.network_id, &request.body) {
            self.pend_start(&permit, network_id, body);
        }
        verdict
    }

    /// Keeps an `attach`'s or an `accept`'s permit as a pending answer to the request with this `Network` id; the
    /// first layer to permit the request pends it. A permit for anything else pends nothing.
    fn pend_answer(&self, permit: &WritePermit, network_id: &str) {
        let answered = matches!(
            permit.target().scope(),
            Scope::CallOperation {
                operation: CallOperation::Attach | CallOperation::Accept,
                ..
            }
        );
        if !answered {
            return;
        }
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        if answers.contains_key(network_id) {
            return;
        }
        if let Some(pending) = self.fence.pend_answer(permit) {
            answers.insert(network_id.to_owned(), pending);
        }
    }

    /// Keeps a call start's permit, with its body, as a pending call waiting for the answer to the request with this
    /// `Network` id; the browser's and the target's layers both permit the same request, and the first one pends it.
    /// A permit for anything but a call start pends nothing.
    fn pend_start(&self, permit: &WritePermit, network_id: &str, body: &[u8]) {
        if permit.target().kind() != WriteKind::Call {
            return;
        }
        let mut starts = self.starts.lock().unwrap_or_else(PoisonError::into_inner);
        if starts.contains_key(network_id) {
            return;
        }
        if let Some(pending) = self.fence.pend_start(permit, body) {
            starts.insert(network_id.to_owned(), pending);
        }
    }

    /// Decides a request already classified. Every deny is logged and kept.
    pub async fn judge(&self, classification: Classification, method: &str, url: &str) -> Verdict {
        self.judge_keeping(classification, method, url).await.0
    }

    /// [`Self::judge`], giving the permit of a write it allowed too.
    async fn judge_keeping(
        &self,
        classification: Classification,
        method: &str,
        url: &str,
    ) -> (Verdict, Option<WritePermit>) {
        let request = RequestRecord {
            channel: Channel::Http,
            method,
            url,
            outcome: RequestOutcome::Unclassified,
        };
        let (verdict, permit) = judge_keeping(&self.fence, self.members.as_ref(), classification, request).await;
        if let Verdict::Deny(cause) = verdict {
            self.keep(Denial::new(method, url, cause));
        }
        (verdict, permit)
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

    /// Reads a paused call start answer and registers its call, when it answers a start this fence permitted (by the
    /// request's `Network` id) with a success: the body as `Fetch.getResponseBody` gives it. Anything else (another
    /// method than `POST`, such as the start's preflight, which leaves a pending start as it is; no pending start, a
    /// failure status, a body that can't be had or read) registers nothing, and the call's follow-ups stay denied.
    /// Continuing the response afterwards sends nothing. Whether it registered the call; `None` when no start was
    /// pending for it (or it isn't a `POST`).
    async fn read_call_start_answer(&self, cdp: &Cdp, session: Option<&SessionId>, params: &Value) -> Option<bool> {
        if params["request"]["method"] != "POST" {
            return None;
        }
        let pending = params["networkId"].as_str().and_then(|network_id| {
            self.starts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(network_id)
        })?;
        let Some(body) = success_body(cdp, session, params).await else {
            return Some(false);
        };
        // an answer that doesn't read registers nothing; its call's follow-ups stay denied
        Some(self.fence.register_start(pending, &body).is_ok())
    }

    /// Reads a paused `attach` or `accept` answer and binds the links it hands out to its invitation, when it answers
    /// one this fence permitted (by the request's `Network` id) with a success. Anything else (the preflight, nothing
    /// pending, a failure, a body that can't be had or read) binds nothing, and what would go to those links stays denied.
    /// Whether it bound them; `None` when no answer was pending for it (or it isn't a `POST`).
    async fn read_call_answer(&self, cdp: &Cdp, session: Option<&SessionId>, params: &Value) -> Option<bool> {
        if params["request"]["method"] != "POST" {
            return None;
        }
        let pending = params["networkId"].as_str().and_then(|network_id| {
            self.answers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(network_id)
        })?;
        let Some(body) = success_body(cdp, session, params).await else {
            return Some(false);
        };
        // an answer that binds nothing leaves the leg's links unbound; its ring-time sends stay denied
        Some(self.fence.bind_answer(pending, &body).is_ok())
    }

    /// A response paused at the response stage. Every fence pauses the call answers: the responses to
    /// [`CALL_START_URL`], to a conversation's requests (a join's among them, [`CONVERSATIONS`]) and to `attach` and
    /// `accept` links ([`CALL_ANSWERS`]): each is continued, and the answer to a `POST` the fence permitted and waits for
    /// is read first ([`Handled::CallAnswerRead`], see the module docs). Only a fence with a catch also pauses the login
    /// origin's responses: the redirect is caught and failed, anything else continued. Any other response paused
    /// without a catch is failed and fails the run.
    async fn handle_response(
        &self,
        cdp: &Cdp,
        session: Option<&SessionId>,
        params: &Value,
    ) -> Result<Handled, CdpError> {
        let id = params["requestId"].as_str().unwrap_or_default();
        if is_call_start_answer(params) || is_call_answer(params) {
            let read = if is_call_start_answer(params) {
                self.read_call_start_answer(cdp, session, params).await
            } else {
                self.read_call_answer(cdp, session, params).await
            };
            continue_response(cdp, session, params).await?;
            return Ok(read.map_or(Handled::ResponseContinued, |bound| Handled::CallAnswerRead { bound }));
        }
        let Some(catch) = &self.catch else {
            self.deny_malformed();
            fail(cdp, session, id).await?;
            return Ok(Handled::Decided(Verdict::Deny(DenyCause::Malformed)));
        };
        if redirect_target(params).is_some_and(|target| catch.take(&target)) {
            fail(cdp, session, id).await?;
            return Ok(Handled::RedirectCaught);
        }
        continue_response(cdp, session, params).await?;
        Ok(Handled::ResponseContinued)
    }
}

/// Decides one request or frame already classified, for either layer of the browser fence: a read passes unlogged, one
/// that reaches nobody passes logged as `allowed` ([`AllowedRecord`]: its method, host and path shape; denied as
/// [`DenyCause::LogUnavailable`] if the log won't take it), a mapped write passes only with a permit from
/// [`Fence::check`] (which logs it), and anything else is denied and logged ([`DenyCause::LogUnavailable`] when the
/// log won't take it). `request` names the channel, method and URL for the log; its outcome is set here.
pub(crate) async fn judge(
    fence: &Fence,
    members: &(dyn MembershipSource + Send),
    classification: Classification,
    request: RequestRecord<'_>,
) -> Verdict {
    judge_keeping(fence, members, classification, request).await.0
}

/// [`judge`], giving the permit of a write it allowed too (the call start's, which the call registry takes).
async fn judge_keeping(
    fence: &Fence,
    members: &(dyn MembershipSource + Send),
    classification: Classification,
    request: RequestRecord<'_>,
) -> (Verdict, Option<WritePermit>) {
    let record = |outcome| RequestRecord { outcome, ..request };
    match classification {
        Classification::Read => (Verdict::Allow, None),
        Classification::Allow => {
            let allowed = AllowedRecord {
                method: request.method,
                url: request.url,
            };
            match fence.log().record_allowed(&allowed) {
                Ok(()) => (Verdict::Allow, None),
                Err(_) => (Verdict::Deny(DenyCause::LogUnavailable), None),
            }
        }
        Classification::Write(target) => match fence.check(target, members).await {
            Ok(permit) => (Verdict::Allow, Some(permit)),
            Err(denied) => (Verdict::Deny(DenyCause::Fenced(*denied.reason())), None),
        },
        // denied either way; a deny the log won't take is `LogUnavailable`, so no scenario's review expects it
        Classification::Unknown => match fence.log().record_request(&record(RequestOutcome::Unclassified)) {
            Ok(()) => (Verdict::Deny(DenyCause::Unclassified), None),
            Err(_) => (Verdict::Deny(DenyCause::LogUnavailable), None),
        },
    }
}

/// Continues a response paused at the response stage, which sends nothing: `Fetch.continueResponse` for one that has
/// a response, `Fetch.continueRequest` for one that failed instead (`responseErrorReason`, no status), which
/// `continueResponse` refuses, leaving the request paused until the page gives it up.
async fn continue_response(cdp: &Cdp, session: Option<&SessionId>, params: &Value) -> Result<(), CdpError> {
    let method = if params.get("responseStatusCode").is_some() {
        "Fetch.continueResponse"
    } else {
        "Fetch.continueRequest"
    };
    cdp.call(session, method, json!({"requestId": params["requestId"]}))
        .await
        .map(drop)
}

/// The body of a response paused at the response stage, when its status is a success (`Fetch.getResponseBody`).
async fn success_body(cdp: &Cdp, session: Option<&SessionId>, params: &Value) -> Option<Vec<u8>> {
    let success = params["responseStatusCode"]
        .as_u64()
        .is_some_and(|status| (200..300).contains(&status));
    if !success {
        return None;
    }
    let answer = cdp
        .call(
            session,
            "Fetch.getResponseBody",
            json!({"requestId": params["requestId"]}),
        )
        .await;
    answer.ok().as_ref().and_then(response_body)
}

/// The bytes of a `Fetch.getResponseBody` answer (`body`, base64 when `base64Encoded`).
fn response_body(answer: &Value) -> Option<Vec<u8>> {
    let body = answer["body"].as_str()?;
    if answer["base64Encoded"].as_bool() == Some(true) {
        base64::engine::general_purpose::STANDARD.decode(body).ok()
    } else {
        Some(body.as_bytes().to_vec())
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
