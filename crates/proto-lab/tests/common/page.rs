//! Opening a test page and reading what it reports, over the armed connection. Included with
//! `#[path = "common/page.rs"] mod page;` by the test crates that use it.
//!
//! `Page.navigate` answers once the new document commits, before its parser has run the page's scripts: a test that
//! reads `window.results` right after it can find the document without it (`undefined`, or `results is not defined`),
//! which a loaded machine makes likely. So [`navigate`] waits for the document it started to be loaded, and only then
//! does a test read the page; an exception in a read after that is a real error and fails the test.

use std::time::Duration;

use proto_lab::cdp::{Cdp, SessionId};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How often a wait looks again.
const POLL: Duration = Duration::from_millis(50);

/// Evaluates `expression` in the page (awaiting a promise it gives) and gives its value; an exception fails.
pub async fn evaluate(cdp: &Cdp, session: &SessionId, expression: &str) -> TestResult<Value> {
    let params = json!({"expression": expression, "returnByValue": true, "awaitPromise": true});
    let result = cdp.call(Some(session), "Runtime.evaluate", params).await?;
    if let Some(exception) = result.get("exceptionDetails") {
        return Err(format!("{expression} threw: {exception}").into());
    }
    Ok(result["result"]["value"].clone())
}

/// Polls `expression` until it gives something other than `null`, for up to `timeout`. An exception fails at once:
/// the page is read only once [`navigate`] has seen it loaded, so what its scripts define is there.
pub async fn evaluate_until(cdp: &Cdp, session: &SessionId, expression: &str, timeout: Duration) -> TestResult<Value> {
    let poll = async {
        loop {
            let value = evaluate(cdp, session, expression).await?;
            if !value.is_null() {
                return Ok(value);
            }
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::time::timeout(timeout, poll)
        .await
        .map_err(|_| format!("{expression} stayed null for {timeout:?}"))?
}

/// Navigates the page to `url` and waits for the document that navigation started to be loaded (`readyState`
/// `complete`: every script of the page's markup has run), all of it (the command too) within `timeout`. A navigation
/// the browser refuses (`errorText`) fails.
pub async fn navigate(cdp: &Cdp, session: &SessionId, url: &str, timeout: Duration) -> TestResult {
    let deadline = tokio::time::Instant::now() + timeout;
    let committed = async {
        let navigated = cdp.call(Some(session), "Page.navigate", json!({ "url": url })).await?;
        if let Some(error) = navigated.get("errorText") {
            return Err(format!("navigating to {url}: {error}").into());
        }
        // the frame shows the new document once it has committed; one within the same document has no loader of its own
        if let Some(loader) = navigated["loaderId"].as_str() {
            while current_loader(cdp, session).await? != loader {
                tokio::time::sleep(POLL).await;
            }
        }
        TestResult::Ok(())
    };
    tokio::time::timeout_at(deadline, committed)
        .await
        .map_err(|_| format!("{url} didn't commit within {timeout:?}"))??;
    // what is left of the same bound
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    evaluate_until(cdp, session, "document.readyState === 'complete' || null", left)
        .await
        .map_err(|error| format!("loading {url}: {error}"))?;
    Ok(())
}

/// The loader of the document the page's main frame shows now.
async fn current_loader(cdp: &Cdp, session: &SessionId) -> TestResult<String> {
    let tree = cdp.call(Some(session), "Page.getFrameTree", json!({})).await?;
    Ok(tree["frameTree"]["frame"]["loaderId"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}
