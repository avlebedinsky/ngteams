//! Evidence for explicitly declared browser call scenarios. Fresh GETs compare observed participants to that
//! scenario's declared target; this module has no global account allowlist or application write policy.
//! Credentials stay in memory and are sent only to the exact validated chat service base where they were observed.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use reqwest::{Client, redirect};
use serde_json::Value;
use teams_proto::chat_members;
use url::Url;
use zeroize::Zeroizing;

use crate::cdp::Event;
use crate::config::{Alias, LabConfig};
use crate::session::identity_in;

const DEADLINE: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 1024 * 1024;

#[derive(Clone)]
struct Credential {
    base: Url,
    header: &'static str,
    value: Zeroizing<String>,
}

#[derive(Default)]
struct Observations {
    identity: bool,
    mismatch: bool,
    credential: Option<Credential>,
}

/// Per-profile authenticated identity and the targets declared by one explicit browser scenario.
pub(crate) struct ActionTargets {
    own: String,
    expected_upn: String,
    declared: Mutex<BTreeMap<String, Vec<String>>>,
    client: Client,
    observations: Mutex<Observations>,
    #[cfg(test)]
    synthetic_members: Option<std::collections::BTreeMap<String, Value>>,
}

impl std::fmt::Debug for ActionTargets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ActionTargets").finish_non_exhaustive()
    }
}

impl ActionTargets {
    pub(crate) fn new(config: &LabConfig, alias: Alias) -> Result<Self, String> {
        let account = config.account(alias);
        let own = account
            .mri
            .clone()
            .filter(|mri| !mri.is_empty())
            .ok_or("the scenario account has no configured identity")?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(DEADLINE)
            .build()
            .map_err(|_| "could not initialize the lab recipient reader".to_owned())?;
        Ok(Self {
            own,
            expected_upn: account.upn.clone(),
            declared: Mutex::new(BTreeMap::new()),
            client,
            observations: Mutex::new(Observations::default()),
            #[cfg(test)]
            synthetic_members: None,
        })
    }

    pub(crate) fn observe(&self, event: &Event) {
        if event.method == "Page.frameNavigated" && event.params["frame"].get("parentId").is_none() {
            self.reset_document();
            return;
        }
        if !matches!(
            event.method.as_str(),
            "Network.requestWillBeSent" | "Fetch.requestPaused"
        ) {
            return;
        }
        let request = &event.params["request"];
        let mut observations = self.observations.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(identity) = identity_in(&request["headers"]) {
            let matches = identity.mri().is_ok_and(|mri| mri == self.own)
                && identity.upn.eq_ignore_ascii_case(&self.expected_upn);
            observations.identity |= matches;
            observations.mismatch |= !matches;
        }
        if let Some(credential) = credential(request) {
            observations.credential = Some(credential);
        }
    }

    fn reset_document(&self) {
        #[cfg(test)]
        if self.synthetic_members.is_some() {
            return; // The offline source explicitly supplies this fixture page's identity on every read.
        }
        *self.observations.lock().unwrap_or_else(PoisonError::into_inner) = Observations::default();
    }

    /// Declares an operation's exact intended conversation and people, including the actor.
    pub(crate) fn declare(&self, chat: &str, members: &[String]) -> Result<(), String> {
        let unique: BTreeSet<_> = members.iter().collect();
        if chat.is_empty()
            || members.len() < 2
            || unique.len() != members.len()
            || !members.contains(&self.own)
            || members.iter().any(String::is_empty)
        {
            return Err("the scenario needs an exact conversation and distinct intended participants".to_owned());
        }
        self.declared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(chat.to_owned(), members.to_vec());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn synthetic(
        own: &str,
        declared: BTreeMap<String, Vec<String>>,
        replies: BTreeMap<String, Value>,
    ) -> Result<Self, String> {
        let source = Self {
            own: own.to_owned(),
            expected_upn: String::new(),
            declared: Mutex::new(BTreeMap::new()),
            client: Client::new(),
            observations: Mutex::new(Observations {
                identity: true,
                ..Observations::default()
            }),
            synthetic_members: Some(replies),
        };
        for (chat, members) in declared {
            source.declare(&chat, &members)?;
        }
        Ok(source)
    }

    pub(crate) async fn recipients(&self, expected: &str) -> Result<(), String> {
        #[cfg(test)]
        if let Some(replies) = &self.synthetic_members {
            let observations = self.observations.lock().unwrap_or_else(PoisonError::into_inner);
            if !observations.identity || observations.mismatch {
                return Err("the active account is not the expected scenario identity".to_owned());
            }
            let answer = replies
                .get(expected)
                .ok_or("synthetic full recipient response missing")?;
            return self.judge_recipients(expected, answer);
        }
        let credential = {
            let observations = self.observations.lock().unwrap_or_else(PoisonError::into_inner);
            if !observations.identity || observations.mismatch {
                return Err("the active account is not the expected scenario identity".to_owned());
            }
            observations
                .credential
                .clone()
                .ok_or("no own-account chat read credential")?
        };
        let members_endpoint = expected.ends_with("@thread.v2");
        if !members_endpoint && !expected.ends_with("@unq.gbl.spaces") {
            return Err("the scenario conversation is not a supported chat".to_owned());
        }
        let mut url = credential.base;
        url.path_segments_mut()
            .map_err(|()| "the scenario conversation is not a supported chat".to_owned())?
            .pop_if_empty()
            .extend(["v1", "threads", expected]);
        if members_endpoint {
            url.path_segments_mut()
                .map_err(|()| "the scenario conversation is not a supported chat".to_owned())?
                .push("members");
        }
        url.query_pairs_mut()
            .append_pair("view", "msnp24Equivalent")
            .append_pair("pageSize", "100");
        let answer = self.read(url, credential.header, &credential.value).await?;
        self.judge_recipients(expected, &answer)
    }

    fn judge_recipients(&self, expected: &str, answer: &Value) -> Result<(), String> {
        let members = chat_members(expected, answer, expected.ends_with("@thread.v2"))
            .ok_or("the complete exact-target membership was not returned")?;
        let actual: BTreeSet<_> = members.iter().map(teams_proto::Mri::as_str).collect();
        let declared = self.declared.lock().unwrap_or_else(PoisonError::into_inner);
        let intended = declared.get(expected).ok_or("no declared scenario conversation")?;
        let intended: BTreeSet<_> = intended.iter().map(String::as_str).collect();
        if actual != intended {
            return Err("observed participants differ from the scenario's intended target".to_owned());
        }
        Ok(())
    }

    async fn read(&self, url: Url, header: &str, value: &str) -> Result<Value, String> {
        let read = async {
            let mut response = self
                .client
                .get(url)
                .header(header, value)
                .header("Cache-Control", "no-cache")
                .send()
                .await
                .map_err(|_| "the full recipient read failed")?;
            if !response.status().is_success() {
                return Err("the full recipient read was refused");
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "the full recipient read failed")? {
                if body.len().saturating_add(chunk.len()) > MAX_BODY {
                    return Err("the full recipient list exceeded its limit");
                }
                body.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&body).map_err(|_| "the full recipient read was malformed")
        };
        tokio::time::timeout(DEADLINE, read)
            .await
            .map_err(|_| "the full recipient read timed out".to_owned())?
            .map_err(str::to_owned)
    }
}

fn credential(request: &Value) -> Option<Credential> {
    let mut base = Url::parse(request["url"].as_str()?).ok()?;
    if base.scheme() != "https" || !base.username().is_empty() || base.password().is_some() || base.port().is_some() {
        return None;
    }
    let host = base.host_str()?;
    let path = if matches!(host, "teams.microsoft.com" | "teams.cloud.microsoft") {
        let parts: Vec<_> = base.path_segments()?.collect();
        if parts.len() < 4
            || parts[0..2] != ["api", "chatsvc"]
            || parts[3] != "v1"
            || parts[2].is_empty()
            || !parts[2]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return None;
        }
        format!("/api/chatsvc/{}/", parts[2])
    } else {
        let prefix = host.strip_suffix(".msg.teams.microsoft.com")?;
        if prefix.is_empty() || !base.path().starts_with("/v1/") {
            return None;
        }
        "/".to_owned()
    };
    base.set_path(&path);
    base.set_query(None);
    base.set_fragment(None);
    let (header, value) = request["headers"].as_object()?.iter().find_map(|(name, value)| {
        let value = value.as_str()?;
        let header = match name.to_ascii_lowercase().as_str() {
            "authorization"
                if value
                    .get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Bearer ")) =>
            {
                "Authorization"
            }
            "authentication" if value.starts_with("skypetoken=") => "Authentication",
            "x-skypetoken" => "X-Skypetoken",
            _ => return None,
        };
        (!value.is_empty() && !value.contains(['\r', '\n'])).then_some((header, Zeroizing::new(value.to_owned())))
    })?;
    Some(Credential { base, header, value })
}

#[cfg(test)]
mod tests;
