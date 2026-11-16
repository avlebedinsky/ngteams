//! What a signaling node reaches the media nodes by ([`MediaNodes`]): a new room goes to the least loaded live node
//! (the registry's, else this process's own), and every request of a call goes to the node its row names, over the
//! internal API.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::api::{EPOCH, token};
use super::registry::{Registry, least_loaded};
use crate::app::App;
use crate::calling::{MediaError, MediaHome, MediaNodes, Sdp, Sources};

/// How long a request to a media node may take to connect, and in all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How many heartbeats a node's entry must stay missing before the node counts as gone.
const MISSED_BEATS: u32 = 2;
/// How long a node's missing entry is kept in mind after it was first missed.
const FORGET_AFTER: Duration = Duration::from_secs(3600);

/// This process's own media node, as its signaling side knows it.
#[derive(Clone, Debug)]
pub(crate) struct Local {
    pub home: MediaHome,
    /// Cleared once the node stops.
    pub alive: Arc<AtomicBool>,
}

/// Since when each node life's registry entry has been missing. Only an entry missing for `grace` is evidence that
/// the node is gone: a Redis that lost its data has every live node's entry back within a heartbeat.
#[derive(Debug)]
struct Missing {
    grace: Duration,
    since: Mutex<BTreeMap<(String, u64), Instant>>,
}

impl Missing {
    fn new(grace: Duration) -> Self {
        Self {
            grace,
            since: Mutex::new(BTreeMap::new()),
        }
    }

    fn found(&self, home: &MediaHome) {
        if let Ok(mut since) = self.since.lock() {
            since.remove(&(home.node.clone(), home.epoch));
        }
    }

    /// The entry of `home` is missing at `now`: whether it has been missing for the grace.
    fn gone(&self, home: &MediaHome, now: Instant) -> bool {
        let Ok(mut since) = self.since.lock() else {
            return false;
        };
        since.retain(|_, first| now.duration_since(*first) < FORGET_AFTER);
        let first = since.entry((home.node.clone(), home.epoch)).or_insert(now);
        now.duration_since(*first) >= self.grace
    }
}

/// The media nodes over the internal API.
#[derive(Debug)]
pub struct HttpNodes {
    app: Weak<App>,
    http: reqwest::Client,
    registry: Option<Registry>,
    local: Option<Local>,
    missing: Missing,
}

impl HttpNodes {
    pub(crate) fn new(app: &Arc<App>, registry: Option<Registry>, local: Option<Local>) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| error.to_string())?;
        let heartbeat = Duration::from_secs(app.config.mixer.heartbeat);
        Ok(Self {
            app: Arc::downgrade(app),
            http,
            registry,
            local,
            missing: Missing::new(heartbeat * MISSED_BEATS),
        })
    }

    fn local_alive(&self) -> Option<&MediaHome> {
        self.local
            .as_ref()
            .filter(|local| local.alive.load(Ordering::SeqCst))
            .map(|local| &local.home)
    }

    /// `method` on the node `home` at `path` (under its rooms) with `body`: the answer's JSON, if it has one.
    async fn call(
        &self,
        method: Method,
        home: &MediaHome,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value, MediaError> {
        let app = self.app.upgrade().ok_or(MediaError::Unavailable)?;
        let token = token(&app).ok_or(MediaError::Unavailable)?;
        let url = format!("{}/internal/mixer/v1/rooms/{path}", home.url.trim_end_matches('/'));
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(token)
            .header(EPOCH, home.epoch.to_string());
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let response = request
            .send()
            .await
            .map_err(|error| MediaError::Failed(error.to_string()))?;
        match response.status() {
            status if status.is_success() => {
                let bytes = response
                    .bytes()
                    .await
                    .map_err(|error| MediaError::Failed(error.to_string()))?;
                if bytes.is_empty() {
                    return Ok(Value::Null);
                }
                serde_json::from_slice(&bytes)
                    .map_err(|error| MediaError::Failed(format!("the media node's answer isn't JSON: {error}")))
            }
            StatusCode::GONE | StatusCode::NOT_FOUND => Err(MediaError::Gone),
            StatusCode::SERVICE_UNAVAILABLE => Err(MediaError::Unavailable),
            status => Err(MediaError::Failed(format!("the media node answered {status}"))),
        }
    }
}

fn leg_path(call: &str, leg: &str) -> String {
    let encode =
        |part: &str| percent_encoding::utf8_percent_encode(part, percent_encoding::NON_ALPHANUMERIC).to_string();
    format!("{}/legs/{}", encode(call), encode(leg))
}

fn room_path(call: &str) -> String {
    percent_encoding::utf8_percent_encode(call, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn sdp_in(answer: &Value, field: &str) -> Result<Sdp, MediaError> {
    serde_json::from_value(answer.get(field).cloned().unwrap_or(Value::Null))
        .map_err(|error| MediaError::Failed(format!("the media node's answer had no {field}: {error}")))
}

#[async_trait]
impl MediaNodes for HttpNodes {
    async fn pick(&self) -> Result<MediaHome, MediaError> {
        if let Some(registry) = &self.registry {
            match registry.live().await {
                Ok(live) => {
                    if let Some(entry) = least_loaded(&live) {
                        return Ok(MediaHome {
                            node: entry.id.clone(),
                            url: entry.url.clone(),
                            epoch: entry.epoch,
                        });
                    }
                }
                Err(error) => tracing::warn!(%error, "the media nodes' registry can't be read"),
            }
        }
        self.local_alive().cloned().ok_or(MediaError::Unavailable)
    }

    async fn put_leg(
        &self,
        home: &MediaHome,
        call: &str,
        leg: &str,
        sources: Sources,
        offer: Option<&Sdp>,
    ) -> Result<Sdp, MediaError> {
        let body = json!({"sources": sources, "offer": offer});
        let answer = self.call(Method::PUT, home, &leg_path(call, leg), Some(body)).await?;
        sdp_in(&answer, if offer.is_some() { "answer" } else { "offer" })
    }

    async fn answer_leg(&self, home: &MediaHome, call: &str, leg: &str, answer: &Sdp) -> Result<(), MediaError> {
        let path = format!("{}/answer", leg_path(call, leg));
        self.call(Method::POST, home, &path, Some(json!({"answer": answer})))
            .await?;
        Ok(())
    }

    async fn renegotiate_leg(&self, home: &MediaHome, call: &str, leg: &str, offer: &Sdp) -> Result<Sdp, MediaError> {
        let path = format!("{}/offer", leg_path(call, leg));
        let answer = self
            .call(Method::POST, home, &path, Some(json!({"offer": offer})))
            .await?;
        sdp_in(&answer, "answer")
    }

    async fn seed(&self, home: &MediaHome, call: &str, leg: &str, seed: u64) -> Result<(), MediaError> {
        let path = format!("{}/parameters", leg_path(call, leg));
        self.call(Method::POST, home, &path, Some(json!({"seed": seed})))
            .await?;
        Ok(())
    }

    async fn delete_leg(&self, home: &MediaHome, call: &str, leg: &str) -> Result<(), MediaError> {
        self.call(Method::DELETE, home, &leg_path(call, leg), None).await?;
        Ok(())
    }

    async fn delete_room(&self, home: &MediaHome, call: &str) -> Result<(), MediaError> {
        self.call(Method::DELETE, home, &room_path(call), None).await?;
        Ok(())
    }

    async fn alive(&self, home: &MediaHome) -> Result<bool, MediaError> {
        if let Some(local) = &self.local
            && local.home.node == home.node
        {
            return Ok(local.alive.load(Ordering::SeqCst) && local.home.epoch == home.epoch);
        }
        let Some(registry) = &self.registry else {
            return Ok(false);
        };
        let entry = registry
            .entry(&home.node)
            .await
            .map_err(|error| MediaError::Failed(error.to_string()))?;
        match entry {
            Some(entry) => {
                self.missing.found(home);
                Ok(entry.epoch == home.epoch)
            }
            None if self.missing.gone(home, Instant::now()) => Ok(false),
            None => Err(MediaError::Failed(
                "the media node's entry is missing, not yet for long".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(epoch: u64) -> MediaHome {
        MediaHome {
            node: "node".into(),
            url: "http://node".into(),
            epoch,
        }
    }

    #[test]
    fn a_node_counts_as_gone_only_once_its_entry_stays_missing_for_the_grace() {
        let missing = Missing::new(Duration::from_secs(2));
        let now = Instant::now();
        assert!(!missing.gone(&home(1), now), "a first miss is no evidence");
        assert!(!missing.gone(&home(1), now + Duration::from_secs(1)));
        assert!(missing.gone(&home(1), now + Duration::from_secs(2)));
        assert!(
            !missing.gone(&home(2), now + Duration::from_secs(2)),
            "another life is missed on its own"
        );
        missing.found(&home(1));
        assert!(
            !missing.gone(&home(1), now + Duration::from_secs(3)),
            "found again: the grace starts over"
        );
        let much_later = now + FORGET_AFTER + Duration::from_secs(3);
        assert!(!missing.gone(&home(2), much_later), "an old miss is forgotten");
    }
}
