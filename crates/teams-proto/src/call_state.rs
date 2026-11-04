//! Call lifecycle hooks implemented by the account's canonical teams-call tracker.
use crate::captions::CaptionWrite;

/// Connection and current-call correlation, independent of any participant allowlist.
pub trait CallState: Send + Sync {
    /// Whether a normal call write still names this account and its current operation links.
    fn call_allowed(&self, url: &url::Url, body: Option<&serde_json::Value>) -> bool;
    /// Whether this exact service link and actor still belong to the active call.
    fn caption_allowed(&self, write: &CaptionWrite) -> bool;
    /// Binds the registered first-party callback endpoint to its delivery connection.
    fn connected(&self, connection: &str, root: &str) -> bool;
    /// Invalidates replies from a closed delivery connection.
    fn disconnected(&self, connection: &str);
    /// Checks a typed reply immediately before sending on its delivery connection.
    fn response_allowed(&self, connection: &str, id: i64, envelope: &str) -> bool;
}
