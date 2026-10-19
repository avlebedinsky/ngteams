//! The scenario steps: what a scenario does to an armed tab and waits for, each step named so that a failure says
//! which one.
//!
//! A [`Driver`] drives one tab over the armed connection and reads a copy of the connection's events
//! ([`crate::arming::ArmedBrowser::watch`]): every request any target of the browser sends (`Network.requestWillBeSent`)
//! and how it ended. It only drives and observes; the fence decides every request as always.
//!
//! - [`Driver::wait_for`]: an element is in the page, found by its `data-tid` (the attribute the Teams web client marks
//!   its controls with, [`Selector::Tid`]) or a CSS selector ([`Selector::Css`]);
//! - [`Driver::wait_for_request`]: a request matching a [`RequestPattern`] was sent, since the driver began or since
//!   the request the last wait matched (so a request sent just before the wait isn't missed, and none is matched twice);
//! - [`Driver::wait_for_failure`]: that request failed, and how (a fenced one with `net::ERR_BLOCKED_BY_CLIENT`);
//! - [`Driver::wait_for_text`]: the page shows a text;
//! - [`Driver::navigate`], [`Driver::click`], [`Driver::type_into`], [`Driver::press_enter`].
//!
//! A wait that runs out fails with [`StepError::Timeout`], naming the step, how long it waited and what for.

use std::fmt;
use std::time::Duration;

use regex::Regex;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::cdp::{Cdp, CdpError, Event, SessionId};

/// How often a wait on the page looks again.
pub const POLL: Duration = Duration::from_millis(200);

/// Why a step failed.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    #[error("step `{step}`: timed out after {waited:?} waiting for {waiting_for}")]
    Timeout {
        step: String,
        waited: Duration,
        waiting_for: String,
    },
    #[error("step `{step}`: {source}")]
    Cdp {
        step: String,
        #[source]
        source: CdpError,
    },
    #[error("step `{step}`: the browser connection is gone")]
    Closed { step: String },
    #[error("step `{step}`: {reason}")]
    Failed { step: String, reason: String },
}

impl StepError {
    /// The step that failed.
    #[must_use]
    pub fn step(&self) -> &str {
        match self {
            Self::Timeout { step, .. } | Self::Cdp { step, .. } | Self::Closed { step } | Self::Failed { step, .. } => {
                step
            }
        }
    }
}

/// How a step finds an element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selector<'a> {
    /// The element with this `data-tid`.
    Tid(&'a str),
    /// The first element this CSS selector matches.
    Css(&'a str),
}

impl Selector<'_> {
    /// The page expression that finds the element (or `null`).
    fn query(self) -> String {
        match self {
            Self::Tid(tid) => format!(
                "document.querySelector('[data-tid=\"' + CSS.escape({}) + '\"]')",
                js_string(tid)
            ),
            Self::Css(css) => format!("document.querySelector({})", js_string(css)),
        }
    }
}

impl fmt::Display for Selector<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tid(tid) => write!(f, "an element with data-tid {tid:?}"),
            Self::Css(css) => write!(f, "an element matching {css:?}"),
        }
    }
}

/// Which requests a wait is for: a method (any if `None`) and a regular expression found in the URL.
#[derive(Clone, Debug)]
pub struct RequestPattern {
    method: Option<String>,
    url: Regex,
}

impl RequestPattern {
    /// # Errors
    /// When `url` doesn't compile.
    pub fn new(method: Option<&str>, url: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            method: method.map(str::to_ascii_uppercase),
            url: Regex::new(url)?,
        })
    }

    #[must_use]
    pub fn matches(&self, method: &str, url: &str) -> bool {
        self.method
            .as_deref()
            .is_none_or(|wanted| wanted.eq_ignore_ascii_case(method))
            && self.url.is_match(url)
    }
}

impl fmt::Display for RequestPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a {} request to /{}/",
            self.method.as_deref().unwrap_or("any"),
            self.url
        )
    }
}

/// A request a target sent, as the `Network` events tell it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenRequest {
    /// The session it was seen on (`None`: the browser's).
    pub session: Option<SessionId>,
    /// The `Network` request id, on that session.
    pub request_id: String,
    pub method: String,
    pub url: String,
    /// The response status, once one came.
    pub status: Option<u16>,
    /// Why it failed (`Network.loadingFailed`'s `errorText`), if it did.
    pub failed: Option<String>,
}

/// Drives one armed tab (see the module docs).
pub struct Driver {
    cdp: Cdp,
    page: SessionId,
    events: mpsc::UnboundedReceiver<Event>,
    requests: Vec<SeenRequest>,
    /// The first request a request wait may match.
    cursor: usize,
    closed: bool,
}

impl Driver {
    /// A driver of the tab `page`, reading `events` (a watch of the armed connection).
    #[must_use]
    pub fn new(cdp: Cdp, page: SessionId, events: mpsc::UnboundedReceiver<Event>) -> Self {
        Self {
            cdp,
            page,
            events,
            requests: Vec::new(),
            cursor: 0,
            closed: false,
        }
    }

    /// Every request seen so far.
    #[must_use]
    pub fn requests(&self) -> &[SeenRequest] {
        &self.requests
    }

    /// Navigates the tab.
    ///
    /// # Errors
    /// When the browser refuses the navigation.
    pub async fn navigate(&mut self, step: &str, url: &str) -> Result<(), StepError> {
        self.call(step, "Page.navigate", json!({"url": url})).await.map(drop)
    }

    /// Waits up to `timeout` for the element `selector` finds to be in the page.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none shows up in time.
    pub async fn wait_for(&mut self, step: &str, selector: Selector<'_>, timeout: Duration) -> Result<(), StepError> {
        let expression = format!("{} !== null", selector.query());
        self.wait_until_true(step, &expression, &selector.to_string(), timeout)
            .await
    }

    /// [`Self::wait_for`] the element with `data-tid` `tid`.
    ///
    /// # Errors
    /// As [`Self::wait_for`].
    pub async fn wait_for_tid(&mut self, step: &str, tid: &str, timeout: Duration) -> Result<(), StepError> {
        self.wait_for(step, Selector::Tid(tid), timeout).await
    }

    /// Waits up to `timeout` for the page's text to hold `text`.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it doesn't in time.
    pub async fn wait_for_text(&mut self, step: &str, text: &str, timeout: Duration) -> Result<(), StepError> {
        let expression = format!("(document.body?.innerText ?? '').includes({})", js_string(text));
        self.wait_until_true(step, &expression, &format!("the text {text:?}"), timeout)
            .await
    }

    /// Whether the page's text holds `text` now.
    ///
    /// # Errors
    /// When the page can't be evaluated.
    pub async fn shows_text(&mut self, step: &str, text: &str) -> Result<bool, StepError> {
        let expression = format!("(document.body?.innerText ?? '').includes({})", js_string(text));
        Ok(self.evaluate(step, &expression).await? == Value::Bool(true))
    }

    /// Clicks the element `selector` finds.
    ///
    /// # Errors
    /// When there is no such element.
    pub async fn click(&mut self, step: &str, selector: Selector<'_>) -> Result<(), StepError> {
        let expression = format!(
            "(() => {{ const e = {}; if (!e) return false; e.click(); return true; }})()",
            selector.query()
        );
        self.expect_true(step, &expression, &format!("no {selector} to click"))
            .await
    }

    /// Focuses the element `selector` finds and types `text` into it, as a person typing would (`Input.insertText`).
    ///
    /// # Errors
    /// When there is no such element, or the browser refuses the input.
    pub async fn type_into(&mut self, step: &str, selector: Selector<'_>, text: &str) -> Result<(), StepError> {
        let expression = format!(
            "(() => {{ const e = {}; if (!e) return false; e.focus(); return true; }})()",
            selector.query()
        );
        self.expect_true(step, &expression, &format!("no {selector} to type into"))
            .await?;
        self.call(step, "Input.insertText", json!({"text": text}))
            .await
            .map(drop)
    }

    /// Presses Enter in the focused element.
    ///
    /// # Errors
    /// When the browser refuses the input.
    pub async fn press_enter(&mut self, step: &str) -> Result<(), StepError> {
        for kind in ["keyDown", "keyUp"] {
            let mut params = json!({"type": kind, "key": "Enter", "code": "Enter", "windowsVirtualKeyCode": 13});
            if kind == "keyDown" {
                params["text"] = json!("\r");
            }
            self.call(step, "Input.dispatchKeyEvent", params).await?;
        }
        Ok(())
    }

    /// Waits up to `timeout` for a request matching `pattern`, sent since the driver began or since the request the last
    /// such wait returned.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none comes in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_request(
        &mut self,
        step: &str,
        pattern: &RequestPattern,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            self.drain();
            let found = self.requests[self.cursor..]
                .iter()
                .position(|request| pattern.matches(&request.method, &request.url));
            if let Some(offset) = found {
                let index = self.cursor + offset;
                self.cursor = index + 1;
                return Ok(self.requests[index].clone());
            }
            self.wait_for_events(step, deadline, timeout, &pattern.to_string())
                .await?;
        }
    }

    /// Waits up to `timeout` for a request matching `pattern` that failed, sent since the driver began or since the
    /// request the last request wait returned; requests that match but didn't fail are passed over.
    ///
    /// # Errors
    /// [`StepError::Timeout`] when none fails in time, [`StepError::Closed`] when the connection goes.
    pub async fn wait_for_failed_request(
        &mut self,
        step: &str,
        pattern: &RequestPattern,
        timeout: Duration,
    ) -> Result<SeenRequest, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting_for = format!("{pattern} that fails");
        loop {
            self.drain();
            let found = self.requests[self.cursor..]
                .iter()
                .position(|request| request.failed.is_some() && pattern.matches(&request.method, &request.url));
            if let Some(offset) = found {
                let index = self.cursor + offset;
                self.cursor = index + 1;
                return Ok(self.requests[index].clone());
            }
            self.wait_for_events(step, deadline, timeout, &waiting_for).await?;
        }
    }

    /// Waits up to `timeout` for `request` to fail, and says why (`net::ERR_BLOCKED_BY_CLIENT` for one the fence
    /// denied).
    ///
    /// # Errors
    /// [`StepError::Timeout`] when it hasn't failed in time, [`StepError::Failed`] when it got a response instead.
    pub async fn wait_for_failure(
        &mut self,
        step: &str,
        request: &SeenRequest,
        timeout: Duration,
    ) -> Result<String, StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let waiting_for = format!("{} {} to fail", request.method, request.url);
        loop {
            self.drain();
            let seen = self
                .requests
                .iter()
                .find(|seen| seen.session == request.session && seen.request_id == request.request_id);
            if let Some(seen) = seen {
                if let Some(error) = &seen.failed {
                    return Ok(error.clone());
                }
                if let Some(status) = seen.status {
                    return Err(StepError::Failed {
                        step: step.to_owned(),
                        reason: format!(
                            "{} {} got a response ({status}) instead of failing",
                            seen.method, seen.url
                        ),
                    });
                }
            }
            self.wait_for_events(step, deadline, timeout, &waiting_for).await?;
        }
    }

    /// Reads every event that has come in.
    fn drain(&mut self) {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.observe(&event),
                Err(mpsc::error::TryRecvError::Empty) => return,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    self.closed = true;
                    return;
                }
            }
        }
    }

    /// Waits for the next event, or the deadline.
    async fn wait_for_events(
        &mut self,
        step: &str,
        deadline: tokio::time::Instant,
        timeout: Duration,
        waiting_for: &str,
    ) -> Result<(), StepError> {
        if self.closed {
            return Err(StepError::Closed { step: step.to_owned() });
        }
        match tokio::time::timeout_at(deadline, self.events.recv()).await {
            Ok(Some(event)) => {
                self.observe(&event);
                Ok(())
            }
            Ok(None) => {
                self.closed = true;
                Err(StepError::Closed { step: step.to_owned() })
            }
            Err(_) => Err(StepError::Timeout {
                step: step.to_owned(),
                waited: timeout,
                waiting_for: waiting_for.to_owned(),
            }),
        }
    }

    fn observe(&mut self, event: &Event) {
        let params = &event.params;
        let Some(id) = params["requestId"].as_str() else {
            return;
        };
        let session = event.session_id.clone();
        match event.method.as_str() {
            "Network.requestWillBeSent" => {
                let request = &params["request"];
                self.requests.push(SeenRequest {
                    session: session.clone(),
                    request_id: id.to_owned(),
                    method: request["method"].as_str().unwrap_or_default().to_owned(),
                    url: request["url"].as_str().unwrap_or_default().to_owned(),
                    status: None,
                    failed: None,
                });
            }
            "Network.responseReceived" => {
                let status = params["response"]["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok());
                if let Some(seen) = self.seen_mut(session.as_ref(), id) {
                    seen.status = status;
                }
            }
            "Network.loadingFailed" => {
                let error = params["errorText"].as_str().unwrap_or("failed").to_owned();
                if let Some(seen) = self.seen_mut(session.as_ref(), id) {
                    seen.failed = Some(error);
                }
            }
            _ => {}
        }
    }

    fn seen_mut(&mut self, session: Option<&SessionId>, id: &str) -> Option<&mut SeenRequest> {
        self.requests
            .iter_mut()
            .rev()
            .find(|seen| seen.session.as_ref() == session && seen.request_id == id)
    }

    async fn wait_until_true(
        &mut self,
        step: &str,
        expression: &str,
        waiting_for: &str,
        timeout: Duration,
    ) -> Result<(), StepError> {
        let deadline = tokio::time::Instant::now() + timeout;
        let timed_out = || StepError::Timeout {
            step: step.to_owned(),
            waited: timeout,
            waiting_for: waiting_for.to_owned(),
        };
        loop {
            self.drain();
            // a page that doesn't answer (a busy renderer, a modal dialog) runs out the wait too
            let value = tokio::time::timeout_at(deadline, self.evaluate(step, expression))
                .await
                .map_err(|_| timed_out())??;
            if value == Value::Bool(true) {
                return Ok(());
            }
            if tokio::time::Instant::now() + POLL > deadline {
                return Err(timed_out());
            }
            tokio::time::sleep(POLL).await;
        }
    }

    async fn expect_true(&mut self, step: &str, expression: &str, otherwise: &str) -> Result<(), StepError> {
        if self.evaluate(step, expression).await? == Value::Bool(true) {
            Ok(())
        } else {
            Err(StepError::Failed {
                step: step.to_owned(),
                reason: otherwise.to_owned(),
            })
        }
    }

    /// Evaluates `expression` in the tab's page and gives its value. An exception in the page reads as `null`.
    async fn evaluate(&self, step: &str, expression: &str) -> Result<Value, StepError> {
        let result = self
            .call(
                step,
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true}),
            )
            .await?;
        Ok(result["result"]["value"].clone())
    }

    async fn call(&self, step: &str, method: &str, params: Value) -> Result<Value, StepError> {
        self.cdp
            .call(Some(&self.page), method, params)
            .await
            .map_err(|source| StepError::Cdp {
                step: step.to_owned(),
                source,
            })
    }
}

/// A JavaScript string literal of `text`.
fn js_string(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}
