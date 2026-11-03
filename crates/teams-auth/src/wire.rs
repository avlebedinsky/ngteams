//! The auth clients' single HTTP exchange: the request and complete answer reach the diagnostic wire log at
//! `debug`. The logger owns all credential redaction, just as it does for the protocol transport. Known sent
//! credentials are exchange-scoped formatting context so an error's free-text echo is redacted too; the original
//! response stays intact for the client's existing parsing and error classification.

use std::time::Instant;

use reqwest::header::HeaderMap;
use reqwest::{Client, RequestBuilder, StatusCode};

pub(crate) struct Answer {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

/// Sends exactly once, retaining every header and body byte needed by the client and its debug record. Build,
/// network and body-read errors remain reqwest errors for each client's existing error classification. `secrets`
/// are supplied only to the logger, never substituted in the request or the returned answer.
pub(crate) async fn exchange(
    client: &Client,
    builder: RequestBuilder,
    secrets: &[&str],
) -> Result<Answer, reqwest::Error> {
    let request = builder.build()?;
    let full = log::log_enabled!(log::Level::Debug);
    let method = request.method().as_str().to_owned();
    let url = request.url().as_str().to_owned();
    if full {
        let headers = header_pairs(request.headers());
        let record = teams_log::wire::Request {
            method: &method,
            url: &url,
            headers: &headers,
            body: request.body().and_then(reqwest::Body::as_bytes),
        };
        log::debug!(target: teams_log::wire::HTTP_TARGET, "{}", record.with_secrets(secrets));
    }
    let started = Instant::now();
    let response = client.execute(request).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?.to_vec();
    if full {
        let pairs = header_pairs(&headers);
        let record = teams_log::wire::Response {
            method: &method,
            url: &url,
            status: status.as_u16(),
            headers: &pairs,
            body: &body,
            elapsed: started.elapsed(),
        };
        log::debug!(target: teams_log::wire::HTTP_TARGET, "{}", record.with_secrets(secrets));
    }
    Ok(Answer { status, headers, body })
}

/// Preserve all response/request headers in the wire record, taking non-text values lossily as the transport does.
fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect()
}
