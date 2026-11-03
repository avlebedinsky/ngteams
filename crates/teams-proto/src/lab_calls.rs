//! Account-local operational call bindings. These do not consult network authorization policy.
//! A call's participants are proved before entry; only its own callbacks and service-issued links can extend it.
//! Leaving a proved call remains possible when a later lookup fails or an outside participant appears.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde_json::Value;
use teams_lab_targets::{LabRoster, TargetError};

mod delivery;
mod wire;

pub(crate) use delivery::DeliveryPlan;
pub(crate) use wire::CallPlan;

/// A calling delivery, decoded on its own socket. Debug never exposes its content.
pub struct CallDeliveryData {
    pub connection: String,
    pub id: i64,
    pub path: String,
    pub body: Vec<u8>,
    pub headers: Vec<(&'static str, Option<String>)>,
}

impl std::fmt::Debug for CallDeliveryData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallDeliveryData")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// Whether a delivery belongs to a proved call, or requires that call to leave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallObservation {
    Allowed,
    Ignored,
    TargetChanged,
}

/// A calling socket's independent account/recipient verifier, shared with its HTTP write transport.
pub trait CallObserver: Send + Sync {
    fn call_bindings(&self) -> Option<Arc<LabCallBindings>>;
    fn observe_call(
        self: Arc<Self>,
        delivery: CallDeliveryData,
    ) -> Pin<Box<dyn Future<Output = CallObservation> + Send>>;
}

#[derive(Clone)]
struct Binding {
    key: u64,
    operation: String,
}

struct Call {
    chat: String,
    members: BTreeSet<String>,
    callbacks: BTreeSet<String>,
    proven: bool,
    unsafe_target: bool,
    ended: bool,
    controller: Option<String>,
    inviter: Option<String>,
}

struct PendingResponse {
    key: u64,
    acceptance: bool,
    headers: Vec<(&'static str, Option<String>)>,
}

#[derive(Default)]
struct State {
    next: u64,
    roots: BTreeMap<String, String>,
    calls: BTreeMap<u64, Call>,
    links: BTreeMap<String, Binding>,
    callbacks: BTreeMap<String, u64>,
    responses: BTreeMap<(String, i64), PendingResponse>,
}

/// Per-account call identity evidence. It contains no credential and never relies on a transport fence.
pub struct LabCallBindings {
    own: String,
    roster: Arc<LabRoster>,
    state: Mutex<State>,
}

impl std::fmt::Debug for LabCallBindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LabCallBindings").finish_non_exhaustive()
    }
}

impl LabCallBindings {
    pub(crate) fn new(own: String, roster: Arc<LabRoster>) -> Self {
        Self {
            own,
            roster,
            state: Mutex::new(State::default()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Binds callbacks to the exact endpoint this account's calling socket registered.
    ///
    /// # Errors
    /// Rejects an unknown socket or a URL outside the first-party calling endpoint format.
    pub fn connected(&self, connection: &str, root: &str) -> Result<(), TargetError> {
        let url = url::Url::parse(root).map_err(|_| TargetError::Conversation)?;
        let valid = url.scheme() == "https"
            && url.username().is_empty()
            && url.password().is_none()
            && url
                .host_str()
                .is_some_and(|host| host.ends_with(".trouter.teams.microsoft.com"))
            && url.port().is_none_or(|port| matches!(port, 443 | 3443))
            && url.query().is_none()
            && url.fragment().is_none()
            && url
                .path()
                .strip_prefix("/v4/f/")
                .and_then(|tail| tail.strip_suffix('/'))
                .is_some_and(|endpoint| {
                    !endpoint.is_empty()
                        && endpoint
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                });
        if !valid || connection.is_empty() {
            return Err(TargetError::Conversation);
        }
        self.state().roots.insert(connection.to_owned(), url.to_string());
        Ok(())
    }

    /// Responses belong to one connection; closing it never removes an active call's cleanup links.
    pub fn disconnected(&self, connection: &str) {
        let mut state = self.state();
        state.roots.remove(connection);
        state.responses.retain(|(socket, _), _| socket != connection);
    }

    /// The final send checks the call again after any asynchronous work performed by the socket.
    #[must_use]
    pub fn response_still_valid(&self, connection: &str, key: u64) -> bool {
        let state = self.state();
        state.roots.contains_key(connection)
            && state
                .calls
                .get(&key)
                .is_some_and(|call| call.proven && !call.unsafe_target && !call.ended)
    }
}

fn calling_url(value: &str) -> Result<String, TargetError> {
    crate::CallLink::parse(value)
        .map(|link| link.as_str().to_owned())
        .ok_or(TargetError::Conversation)
}

fn text<'a>(body: &'a Value, at: &str) -> Result<&'a str, TargetError> {
    body.pointer(at)
        .and_then(Value::as_str)
        .ok_or(TargetError::Conversation)
}

fn bind_link(state: &mut State, key: u64, operation: &str, link: &str) -> Result<(), TargetError> {
    let link = calling_url(link)?;
    if state
        .links
        .get(&link)
        .is_some_and(|bound| bound.key != key && state.calls.get(&bound.key).is_some_and(|call| !call.ended))
    {
        return Err(TargetError::Conversation);
    }
    state.links.insert(
        link,
        Binding {
            key,
            operation: operation.to_owned(),
        },
    );
    Ok(())
}

fn bind_links(state: &mut State, key: u64, links: &Value) -> Result<(), TargetError> {
    let Some(links) = links.as_object() else { return Ok(()) };
    if let Some(leave) = links.get("leave") {
        bind_link(state, key, "leave", leave.as_str().ok_or(TargetError::Conversation)?)?;
    }
    for (operation, value) in links {
        if !matches!(
            operation.as_str(),
            "updateEndpointState"
                | "updateEndpointMetadata"
                | "addParticipantAndModality"
                | "admit"
                | "removeParticipant"
                | "attach"
                | "progress"
                | "acceptance"
                | "callLeg"
                | "acknowledgement"
                | "mediaRenegotiation"
                | "updateMediaDescriptions"
                | "applyChannelParameters"
                | "mediaAnswer"
                | "mediaAcknowledgement"
                | "reject"
        ) {
            continue;
        }
        let link = value.as_str().ok_or(TargetError::Conversation)?;
        if link.starts_with("cc://") {
            continue;
        }
        bind_link(state, key, operation, link)?;
    }
    Ok(())
}

fn callbacks(state: &State, body: &Value) -> Result<BTreeSet<String>, TargetError> {
    let mut urls = Vec::new();
    for pointer in [
        "/conversationRequest/links",
        "/callInvitation/links",
        "/callInvitation/clientContentForMediaController",
        "/attach/links",
        "/callAcceptance/links",
        "/callAcceptance/clientContentForMediaController",
        "/mediaNegotiation/links",
        "/mediaNegotiation/clientContentForMediaController",
        "/mediaAnswer/links",
        "/mediaAnswer/clientContentForMediaController",
        "/callAcceptanceAcknowledgement/links",
        "/links",
    ] {
        if let Some(links) = body.pointer(pointer).and_then(Value::as_object) {
            for value in links.values() {
                urls.push(value.as_str().ok_or(TargetError::Conversation)?);
            }
        }
    }
    if let Some(value) = body.pointer("/conversationRequest/roster/rosterUpdate") {
        urls.push(value.as_str().ok_or(TargetError::Conversation)?);
    }
    let mut found = BTreeSet::new();
    for value in urls {
        let url = url::Url::parse(value).map_err(|_| TargetError::Conversation)?;
        if !state.roots.values().any(|root| {
            value
                .strip_prefix(root)
                .is_some_and(|tail| tail.starts_with("callAgent/"))
        }) || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(TargetError::Conversation);
        }
        found.insert(url.path().to_owned());
    }
    Ok(found)
}

fn add_callbacks(state: &mut State, key: u64, found: BTreeSet<String>) -> Result<(), TargetError> {
    if found
        .iter()
        .any(|path| state.callbacks.get(path).is_some_and(|bound| *bound != key))
    {
        return Err(TargetError::Conversation);
    }
    for path in &found {
        state.callbacks.insert(path.clone(), key);
    }
    state
        .calls
        .get_mut(&key)
        .ok_or(TargetError::Conversation)?
        .callbacks
        .extend(found);
    Ok(())
}

#[cfg(test)]
mod tests;
