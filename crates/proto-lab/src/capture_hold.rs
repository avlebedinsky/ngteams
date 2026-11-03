//! Selective capture holds: fail only requests a scenario explicitly matches; continue every other request.
//! This module neither classifies writes nor reads memberships or creates a fence log.
use crate::cdp::{Cdp, CdpError, Event};
use crate::redirect_catch::{RedirectCatch, at_response_stage, redirect_target};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::{Mutex, PoisonError};

/// Text marking a deliberately unsent message, retained across browser-profile reloads.
pub const HELD_MARKER: &str = "lab held capture ";
const LEGACY_HELD_MARKER: &str = "lab fence probe ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PausedRequest {
    pub request_id: String,
    pub method: String,
    pub url: String,
    pub body: Option<Vec<u8>>,
    pub network_id: Option<String>,
}
impl PausedRequest {
    #[must_use]
    pub fn from_params(params: &Value) -> Option<Self> {
        let request = &params["request"];
        Some(Self {
            request_id: params["requestId"].as_str()?.into(),
            method: request["method"].as_str()?.into(),
            url: request["url"].as_str()?.into(),
            body: request["postData"].as_str().map(|v| v.as_bytes().to_vec()),
            network_id: params["networkId"].as_str().map(str::to_owned),
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRequest {
    pub method: String,
    pub url: String,
    pub label: Option<String>,
    pub network_id: Option<String>,
}
impl HeldRequest {
    #[must_use]
    /// Records the URL without its query or fragment.
    pub fn new(method: &str, url: &str) -> Self {
        Self {
            method: method.into(),
            url: without_query(url).into(),
            label: None,
            network_id: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Hold {
    method: String,
    url: Regex,
    except: Option<Regex>,
    body_contains: Option<String>,
    body_lacks: Option<String>,
    label: Option<String>,
}
impl Hold {
    /// # Errors
    /// When the URL pattern is not a valid regular expression.
    pub fn new(method: &str, url_pattern: &str) -> Result<Self, regex::Error> {
        Ok(Self {
            method: method.to_ascii_uppercase(),
            url: Regex::new(url_pattern)?,
            except: None,
            body_contains: None,
            body_lacks: None,
            label: None,
        })
    }

    /// # Errors
    /// When the exclusion pattern is not a valid regular expression.
    pub fn except(mut self, url_pattern: &str) -> Result<Self, regex::Error> {
        self.except = Some(Regex::new(url_pattern)?);
        Ok(self)
    }

    #[must_use]
    pub fn labelled(mut self, label: &str) -> Self {
        self.label = Some(label.to_owned());
        self
    }

    #[must_use]
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    #[must_use]
    pub fn with_body_containing(mut self, text: &str) -> Self {
        self.body_contains = Some(text.to_owned());
        self
    }

    #[must_use]
    pub fn with_body_lacking(mut self, text: &str) -> Self {
        self.body_lacks = Some(text.to_owned());
        self
    }

    #[must_use]
    pub fn matches(&self, request: &PausedRequest) -> bool {
        let body_matches = self.body_contains.as_ref().is_none_or(|text| {
            request
                .body
                .as_deref()
                .is_some_and(|body| String::from_utf8_lossy(body).contains(text.as_str()))
        });
        let holds = |text: &str| {
            request
                .body
                .as_deref()
                .is_some_and(|body| String::from_utf8_lossy(body).contains(text))
        };
        let lacks = self.body_lacks.as_deref().is_none_or(|text| !holds(text));
        let excepted = self.except.as_ref().is_some_and(|except| except.is_match(&request.url));
        self.method.eq_ignore_ascii_case(&request.method)
            && self.url.is_match(&request.url)
            && !excepted
            && body_matches
            && lacks
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handled {
    Continued,
    Held,
    RedirectCaught,
    ResponseContinued,
}
#[derive(Default)]
pub struct CaptureHold {
    rules: Mutex<Vec<Hold>>,
    held: Mutex<Vec<HeldRequest>>,
    catch: Option<RedirectCatch>,
    pending_messages: bool,
}
impl CaptureHold {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Keeps deliberately held messages unsent if the web client retries them on a later run.
    /// These capture markers survive clearing the current scenario's holds.
    #[must_use]
    pub(crate) fn with_pending_messages(mut self) -> Self {
        self.pending_messages = true;
        self
    }
    #[must_use]
    pub fn with_redirect_catch(mut self, catch: RedirectCatch) -> Self {
        self.catch = Some(catch);
        self
    }
    #[must_use]
    pub fn fetch_patterns(&self) -> Value {
        let mut patterns = vec![json!({"urlPattern":"*"})];
        patterns.extend(self.catch.as_ref().map(RedirectCatch::response_pattern));
        json!({"patterns":patterns})
    }
    pub fn hold(&self, rule: Hold) {
        self.rules.lock().unwrap_or_else(PoisonError::into_inner).push(rule);
    }
    pub fn clear(&self) {
        self.rules.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
    #[must_use]
    pub fn held_requests(&self) -> Vec<HeldRequest> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
    /// Answers a paused request or response on the same CDP session that delivered it.
    /// # Errors
    /// When Chrome refuses the answer or the connection closes.
    pub async fn handle(&self, cdp: &Cdp, event: &Event) -> Result<Handled, CdpError> {
        let params = &event.params;
        let Some(id) = params["requestId"].as_str() else {
            return Ok(Handled::Continued);
        };
        let response = at_response_stage(params);
        let caught = self.catch.as_ref().is_some_and(|catch| {
            if response {
                redirect_target(params).is_some_and(|url| catch.take(&url))
            } else {
                params["request"]["method"] == "GET"
                    && params["request"]["url"].as_str().is_some_and(|url| catch.take(url))
            }
        });
        let rule = if response || caught {
            None
        } else {
            PausedRequest::from_params(params).and_then(|request| {
                self.rules
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .iter()
                    .find(|rule| rule.matches(&request))
                    .map(|rule| (rule.label.clone(), request.clone()))
                    .or_else(|| {
                        (self.pending_messages && pending_message(&request))
                            .then(|| (Some("an earlier run's held capture".into()), request))
                    })
            })
        };
        let method = if caught || rule.is_some() {
            "Fetch.failRequest"
        } else if response && params.get("responseStatusCode").is_some() {
            "Fetch.continueResponse"
        } else {
            "Fetch.continueRequest"
        };
        let mut answer = json!({"requestId":id});
        if method == "Fetch.failRequest" {
            answer["errorReason"] = json!("BlockedByClient");
        }
        cdp.call(event.session_id.as_ref(), method, answer).await?;
        if let Some((label, request)) = rule {
            self.held
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(HeldRequest {
                    method: request.method,
                    url: without_query(&request.url).into(),
                    label,
                    network_id: request.network_id,
                });
            Ok(Handled::Held)
        } else if caught {
            Ok(Handled::RedirectCaught)
        } else if response {
            Ok(Handled::ResponseContinued)
        } else {
            Ok(Handled::Continued)
        }
    }
}
fn pending_message(request: &PausedRequest) -> bool {
    matches!(request.method.to_ascii_uppercase().as_str(), "POST" | "PUT")
        && request.body.as_deref().is_some_and(|body| {
            let body = String::from_utf8_lossy(body);
            [HELD_MARKER, LEGACY_HELD_MARKER]
                .iter()
                .any(|marker| body.contains(marker))
        })
}
#[must_use]
pub fn without_query(url: &str) -> &str {
    &url[..url.find(['?', '#']).unwrap_or(url.len())]
}

#[cfg(test)]
#[path = "../tests/support/capture_hold.rs"]
mod tests;
