//! Protocol shape helpers used by browser captures and read probes, independent of native write policy.
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use std::io::Read as _;
use url::Url;
pub const AFD_HOSTS: [&str; 2] = ["teams.microsoft.com", "teams.cloud.microsoft"];
const REGIONAL_SUFFIX: &str = ".msg.teams.microsoft.com";
pub const CALL_START_URL: &str = "https://api.flightproxy.teams.microsoft.com/api/v2/epconv";
const REDACTED: &str = "[redacted]";
const UNREADABLE_URL: &str = "[unreadable url]";
const MAX_DELIVERY: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct Delivery {
    pub connection: String,
    pub id: i64,
    pub path: String,
    pub trouter_request: Option<String>,
    pub chain_id: Option<String>,
    pub message_id: Option<String>,
    pub body: Vec<u8>,
}
/// The path segments after v1 on a recognized commercial chat-service origin.
#[must_use]
pub fn chat_service_path(url: &Url) -> Option<Vec<String>> {
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let mut segments = url.path_segments()?;
    if AFD_HOSTS.contains(&host.as_str()) {
        if segments.next()? != "api" || segments.next()? != "chatsvc" || !is_region(segments.next()?) {
            return None;
        }
    } else {
        let name = host.strip_suffix(REGIONAL_SUFFIX)?;
        if name.is_empty()
            || !name
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        {
            return None;
        }
    }
    if segments.next()? != "v1" {
        return None;
    }
    segments
        .map(|part| percent_decode_str(part).decode_utf8().ok().map(String::from))
        .collect()
}

fn is_region(region: &str) -> bool {
    (1..=32).contains(&region.len())
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
#[must_use]
pub fn one_to_one_id(mri_a: &str, mri_b: &str) -> Option<String> {
    let oid = |mri: &str| {
        let oid = mri.strip_prefix("8:orgid:")?;
        is_guid(oid).then(|| oid.to_ascii_lowercase())
    };
    let mut oids = [oid(mri_a)?, oid(mri_b)?];
    oids.sort();
    let [first, second] = oids;
    (first != second).then(|| format!("19:{first}_{second}@unq.gbl.spaces"))
}
fn is_guid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
#[must_use]
pub fn is_group_chat_id(id: &str) -> bool {
    id.strip_prefix("19:")
        .and_then(|id| id.strip_suffix("@thread.v2"))
        .is_some_and(|inner| {
            !inner.is_empty()
                && inner.len() <= 128
                && !inner.to_ascii_lowercase().starts_with("meeting_")
                && inner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
}
#[must_use]
pub fn log_url(url: &str) -> String {
    if url.starts_with(['?', '#']) {
        // nothing before the query or fragment: no URL to name (an old capture's unknown socket, `?`)
        return String::new();
    }
    if url.is_empty() || authority_form(url) {
        return url.to_owned();
    }
    let Ok(parsed) = Url::parse(url) else {
        return UNREADABLE_URL.to_owned();
    };
    let Some(segments) = parsed.path_segments() else {
        return UNREADABLE_URL.to_owned();
    };
    let mut out = format!("{}://{}", parsed.scheme(), parsed.host_str().unwrap_or_default());
    if let Some(port) = parsed.port() {
        out = format!("{out}:{port}");
    }
    let mut after_credential_word = false;
    for segment in segments {
        out.push('/');
        out.push_str(&log_segment(segment, after_credential_word));
        after_credential_word = credential_name(segment);
    }
    if let Some(query) = parsed.query() {
        out.push('?');
        out.push_str(&query_keys(query));
    }
    out
}
fn authority_form(target: &str) -> bool {
    !target.contains("://")
        && target.contains(':')
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']'))
        && target.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
        })
}
fn log_segment(segment: &str, after_credential_name: bool) -> String {
    if after_credential_name || opaque(segment) {
        return REDACTED.to_owned();
    }
    match segment.split_once('=') {
        Some((name, _)) => format!("{name}={REDACTED}"),
        None => segment.to_owned(),
    }
}
fn query_keys(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .map(|key| if opaque(key) { REDACTED } else { key })
        .collect::<Vec<_>>()
        .join("&")
}
fn opaque(text: &str) -> bool {
    let decoded = percent_decode_str(text).decode_utf8_lossy();
    decoded.starts_with("eyJ")
        || (decoded.len() >= OPAQUE_RUN
            && decoded.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
            }))
}
fn credential_name(segment: &str) -> bool {
    let name = percent_decode_str(segment).decode_utf8_lossy().to_ascii_lowercase();
    CREDENTIAL_NAMES.contains(&name.as_str())
}
pub fn delivery_of(connection: &str, text: &str) -> Option<Delivery> {
    let message = crate::classify::strict_json(text.strip_prefix("3:::")?.as_bytes())?;
    let headers = message.get("headers")?.as_object()?;
    let header = |wanted: &str| {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .and_then(|(_, value)| value.as_str())
    };
    let body = message.get("body")?.as_str()?;
    let body = if header("X-Microsoft-Skype-Content-Encoding").is_some_and(|value| value.eq_ignore_ascii_case("gzip")) {
        let compressed = base64::engine::general_purpose::STANDARD.decode(body).ok()?;
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(compressed.as_slice())
            .take(MAX_DELIVERY + 1)
            .read_to_end(&mut decoded)
            .ok()?;
        (decoded.len() as u64 <= MAX_DELIVERY).then_some(decoded)?
    } else {
        body.as_bytes().to_vec()
    };
    Some(Delivery {
        connection: connection.to_owned(),
        id: message.get("id")?.as_i64()?,
        path: message.get("url")?.as_str()?.to_owned(),
        trouter_request: header("trouter-request").map(str::to_owned),
        chain_id: header("X-Microsoft-Skype-Chain-ID").map(str::to_owned),
        message_id: header("X-Microsoft-Skype-Message-ID").map(str::to_owned),
        body,
    })
}
const OPAQUE_RUN: usize = 100;

/// Path segments (lowercase) after which [`log_url`] takes the next segment as a credential (`/skypetoken/<token>`).
const CREDENTIAL_NAMES: [&str; 14] = [
    "token",
    "skypetoken",
    "access_token",
    "accesstoken",
    "sig",
    "signature",
    "key",
    "apikey",
    "api_key",
    "secret",
    "password",
    "tempauth",
    "authorization",
    "authentication",
];

/// A request recorded by the browser; held requests never reached the service.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    pub method: String,
    pub url: String,
    pub held: bool,
    pub id: String,
}
/// Reads current Network/capture-hold events and older captures' HTTP verdict records.
#[must_use]
pub fn http_requests(text: &str) -> Vec<CapturedRequest> {
    let mut requests = std::collections::BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (request, id, held) = if entry["type"] == "cdp" && entry["method"] == "Network.requestWillBeSent" {
            (&entry["params"]["request"], &entry["params"]["requestId"], false)
        } else if entry["type"] == "held_request" {
            (&entry, &entry["network_id"], true)
        } else if entry["type"] == "verdict" && entry["channel"] == "http" {
            (&entry, &entry["network_id"], entry["verdict"] == "deny")
        } else {
            continue;
        };
        let (Some(method), Some(url)) = (request["method"].as_str(), request["url"].as_str()) else {
            continue;
        };
        let id = id.as_str().map_or_else(|| format!("entry-{index}"), str::to_owned);
        let captured = CapturedRequest {
            method: method.into(),
            url: url.into(),
            held,
            id: id.clone(),
        };
        if held || !requests.contains_key(&id) {
            requests.insert(id, captured);
        }
    }
    requests.into_values().collect()
}
/// Sent socket text, with its URL, from Network events (or older captures' socket verdicts).
#[must_use]
pub fn sent_frames(text: &str) -> Vec<(String, String)> {
    let mut sockets = std::collections::BTreeMap::new();
    let mut frames = Vec::new();
    let mut legacy = Vec::new();
    let mut network = false;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let key = (entry["session"].to_string(), entry["params"]["requestId"].to_string());
        network |= entry["type"] == "cdp"
            && entry["method"]
                .as_str()
                .is_some_and(|method| method.starts_with("Network.webSocket"));
        if entry["type"] == "cdp" && entry["method"] == "Network.webSocketCreated" {
            sockets.insert(key, entry["params"]["url"].as_str().unwrap_or_default().to_owned());
        } else if entry["type"] == "cdp"
            && entry["method"] == "Network.webSocketFrameSent"
            && entry["params"]["response"]["opcode"] == 1
        {
            if let Some(data) = entry["params"]["response"]["payloadData"].as_str() {
                frames.push((sockets.get(&key).cloned().unwrap_or_default(), data.into()));
            }
        } else if entry["type"] == "verdict"
            && entry["channel"] == "web_socket"
            && entry["verdict"] == "allow"
            && let Some(data) = entry["frame"]["data"].as_str()
        {
            legacy.push((entry["url"].as_str().unwrap_or_default().into(), data.into()));
        }
    }
    if network { frames } else { legacy }
}
