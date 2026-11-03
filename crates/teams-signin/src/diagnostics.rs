//! Payload-free sign-in decisions in the app's diagnostic log.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;
use teams_proto::log_url;

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
        "url": log_url(url),
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
