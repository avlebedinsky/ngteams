//! Synthetic CDP construction with optional disposable process ownership.
use super::*;
impl ArmedBrowser {
    pub(crate) fn synthetic_managed(
        cdp: Cdp,
        events: Events,
        http: CaptureHold,
        capture: Option<Capture>,
        browser: Browser,
    ) -> Self {
        Self::managed(cdp, events, http, capture, Some(browser))
    }
    pub(crate) fn synthetic(cdp: Cdp, events: Events, http: CaptureHold, capture: Option<Capture>) -> Self {
        Self::managed(cdp, events, http, capture, None)
    }
}
