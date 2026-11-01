//! Synthetic CDP construction, with optional actual stand-in process ownership.
use super::*;

impl ArmedBrowser {
    /// A synthetic peer whose stand-in process is genuinely owned, killed and reaped by the ordinary close policy.
    #[cfg(test)]
    pub(crate) fn synthetic_managed(
        cdp: Cdp,
        events: Events,
        http: HttpFence,
        ws: WsFence,
        capture: Option<Capture>,
        browser: Browser,
    ) -> Self {
        let armed = Self::synthetic(cdp, events, http, ws, capture);
        *armed.shared.browser.lock().unwrap_or_else(PoisonError::into_inner) = Some(browser);
        armed
    }

    /// An offline CDP peer with the ordinary event/decision handling and no browser process to own.
    #[cfg(test)]
    pub(crate) fn synthetic(cdp: Cdp, events: Events, http: HttpFence, ws: WsFence, capture: Option<Capture>) -> Self {
        let shared = Arc::new(Shared {
            http,
            ws,
            capture,
            targets: Mutex::new(Targets::default()),
            changed: Notify::new(),
            browser: Mutex::new(None),
            lost: AtomicBool::new(false),
            broken: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            contained: AtomicBool::new(false),
            containment: tokio::sync::OnceCell::new(),
            deciding: AtomicUsize::new(0),
            settled: Notify::new(),
            taps: Mutex::new(Vec::new()),
        });
        let task = tokio::spawn(handle_events(cdp.clone(), events, Arc::clone(&shared)));
        Self { cdp, shared, task }
    }
}
