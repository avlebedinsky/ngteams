//! Independent action preflights. This reader issues only fresh GETs and has no transport fence/permit dependency.
//! Credentials stay in memory and are sent only to the exact validated chat service base where they were observed.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use reqwest::{Client, redirect};
use serde_json::Value;
use teams_lab_targets::{LabAccount, LabRoster, TargetError, full_members};
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

/// Per-profile evidence for explicit lab actions, independent of the network authorization policy.
pub(crate) struct ActionTargets {
    roster: LabRoster,
    own: String,
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
        let accounts = config.accounts().each_ref().map(|account| LabAccount {
            mri: account.mri.clone().unwrap_or_default(),
            upn: account.upn.clone(),
        });
        let roster = LabRoster::new(accounts).map_err(|error| error.to_string())?;
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .redirect(redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(DEADLINE)
            .build()
            .map_err(|_| "could not initialize the lab recipient reader".to_owned())?;
        Ok(Self {
            roster,
            own: config.account(alias).mri.clone().unwrap_or_default(),
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
            let matches = identity
                .mri()
                .is_ok_and(|mri| self.roster.verify_identity(&self.own, &mri, &identity.upn).is_ok());
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

    #[cfg(test)]
    pub(crate) fn synthetic(
        own: &str,
        roster: LabRoster,
        replies: std::collections::BTreeMap<String, Value>,
    ) -> Result<Self, String> {
        if roster.account(own).is_none() {
            return Err(TargetError::Identity.to_string());
        }
        Ok(Self {
            own: own.to_owned(),
            roster,
            client: Client::new(),
            observations: Mutex::new(Observations {
                identity: true,
                ..Observations::default()
            }),
            synthetic_members: Some(replies),
        })
    }

    pub(crate) async fn recipients(&self, expected: &str) -> Result<(), String> {
        #[cfg(test)]
        if let Some(replies) = &self.synthetic_members {
            let observations = self.observations.lock().unwrap_or_else(PoisonError::into_inner);
            if !observations.identity || observations.mismatch {
                return Err(TargetError::Identity.to_string());
            }
            let answer = replies
                .get(expected)
                .ok_or("synthetic full recipient response missing")?;
            return self.judge_recipients(expected, answer);
        }
        let credential = {
            let observations = self.observations.lock().unwrap_or_else(PoisonError::into_inner);
            if !observations.identity || observations.mismatch {
                return Err(TargetError::Identity.to_string());
            }
            observations
                .credential
                .clone()
                .ok_or("no own-account chat read credential")?
        };
        let members_endpoint = expected.ends_with("@thread.v2");
        if !members_endpoint && !expected.ends_with("@unq.gbl.spaces") {
            return Err(TargetError::Conversation.to_string());
        }
        let mut url = credential.base;
        url.path_segments_mut()
            .map_err(|()| TargetError::Conversation.to_string())?
            .pop_if_empty()
            .extend(["v1", "threads", expected]);
        if members_endpoint {
            url.path_segments_mut()
                .map_err(|()| TargetError::Conversation.to_string())?
                .push("members");
        }
        url.query_pairs_mut()
            .append_pair("view", "msnp24Equivalent")
            .append_pair("pageSize", "100");
        let answer = self.read(url, credential.header, &credential.value).await?;
        self.judge_recipients(expected, &answer)
    }

    fn judge_recipients(&self, expected: &str, answer: &Value) -> Result<(), String> {
        let members =
            full_members(expected, answer, expected.ends_with("@thread.v2")).map_err(|error| error.to_string())?;
        self.roster
            .verify_recipients(&self.own, &members)
            .map_err(|error| error.to_string())
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
