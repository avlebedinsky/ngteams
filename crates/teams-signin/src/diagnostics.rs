//! Payload-free sign-in decisions in the app's diagnostic log.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use percent_encoding::percent_decode_str;
use serde_json::json;
use teams_log::redact::{OPAQUE_RUN, REDACTED, secret_param};

#[derive(Clone, Copy)]
pub(crate) enum Outcome {
    Read,
    Own(&'static str),
    Unclassified,
    SignInRedirect,
    Tunnel,
}

pub(crate) fn record(method: &str, url: &str, outcome: Outcome) {
    let mut entry = json!({
        "ts_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
        "channel": "http",
        "method": method,
        "url": endpoint(url),
        "verdict": "allow",
    });
    match outcome {
        Outcome::Read => entry["read"] = true.into(),
        Outcome::Own(kind) => entry["own"] = kind.into(),
        Outcome::Tunnel => entry["tunnel"] = true.into(),
        Outcome::Unclassified => {
            entry["verdict"] = "deny".into();
            entry["reason"] = "unclassified".into();
        }
        Outcome::SignInRedirect => {
            entry["verdict"] = "deny".into();
            entry["reason"] = "sign_in_redirect".into();
        }
    }
    log::info!(target: "teams_signin::guard", "{entry}");
}

/// Keeps endpoint names and query keys, never user info, query values or fragments. Path credentials are redacted.
fn endpoint(text: &str) -> String {
    if authority(text) {
        return text.to_owned();
    }
    let Ok(url) = url::Url::parse(text) else {
        return "[unreadable url]".into();
    };
    let Some(segments) = url.path_segments() else {
        return "[unreadable url]".into();
    };
    let mut out = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        let _ = write!(out, ":{port}");
    }
    let mut credential = false;
    for segment in segments {
        out.push('/');
        if credential || opaque(segment) {
            out.push_str(REDACTED);
        } else if let Some((name, _)) = segment.split_once('=') {
            out.push_str(name);
            out.push('=');
            out.push_str(REDACTED);
        } else {
            out.push_str(segment);
        }
        credential = secret_param(&percent_decode_str(segment).decode_utf8_lossy());
    }
    if let Some(query) = url.query() {
        out.push('?');
        out.push_str(
            &query
                .split('&')
                .filter(|pair| !pair.is_empty())
                .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
                .map(|key| if opaque(key) { REDACTED } else { key })
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    out
}

fn opaque(text: &str) -> bool {
    let text = percent_decode_str(text).decode_utf8_lossy();
    text.starts_with("eyJ")
        || (text.len() >= OPAQUE_RUN
            && text.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
            }))
}

fn authority(text: &str) -> bool {
    !text.contains("://")
        && text.contains(':')
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']'))
        && text.rsplit_once(':').is_some_and(|(_, port)| {
            !port.is_empty() && port.len() <= 5 && port.bytes().all(|byte| byte.is_ascii_digit())
        })
}
