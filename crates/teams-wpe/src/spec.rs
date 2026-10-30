//! What a browser store ([`SessionSpec`]) and a page ([`ViewSpec`]) are built from: the `WebKit` surface the HTML
//! overlay uses (`NetworkSession`, `Settings`, `UserContentManager` with its scripts and message handlers).

use std::path::PathBuf;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// A browser store's id, chosen by [`crate::Host::create_session`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(pub(crate) u64);

/// A page's id, chosen by [`crate::Host::create_view`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ViewId(pub(crate) u64);

crate::raw_ids!(SessionId, ViewId);

/// A browser store (`WebKitNetworkSession`): cookies, local storage, caches. Downloads are refused in every one:
/// nothing a page shows is saved to disk ([`crate::Event::DownloadRefused`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSpec {
    /// Kept in memory and gone with the session.
    Ephemeral,
    /// Kept on disk, data and cache apart, so saved logins outlive the page, the session and the app.
    Persistent { data_dir: PathBuf, cache_dir: PathBuf },
    /// Kept in memory, Intelligent Tracking Prevention off, and every load (but those to `ignore_hosts`, as GIO's
    /// proxy resolver reads them) going through the HTTP proxy at `proxy`, with no direct fallback: a store whose
    /// way out the consumer holds. A store whose proxy can't be set is never made (the page fails,
    /// [`crate::Event::ViewFailed`]).
    Guarded { proxy: String, ignore_hosts: Vec<String> },
}

bitflags! {
    /// `WebKitSettings` a page is built with: each flag on sets its setting to true, off to false.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    pub struct Settings: u32 {
        /// `enable-javascript-markup`: the page's own scripts run (the consumer's user scripts run regardless).
        const JAVASCRIPT_MARKUP = 1;
        /// `javascript-can-open-windows-automatically`.
        const OPEN_WINDOWS_AUTOMATICALLY = 1 << 1;
        /// `allow-file-access-from-file-urls`.
        const FILE_ACCESS_FROM_FILE_URLS = 1 << 2;
        /// `allow-universal-access-from-file-urls`.
        const UNIVERSAL_ACCESS_FROM_FILE_URLS = 1 << 3;
        /// `enable-developer-extras`.
        const DEVELOPER_EXTRAS = 1 << 4;
        /// `enable-fullscreen`.
        const FULLSCREEN = 1 << 5;
        /// `media-playback-requires-user-gesture`.
        const MEDIA_REQUIRES_USER_GESTURE = 1 << 6;
        /// `enable-webrtc`.
        const WEBRTC = 1 << 7;
        /// `enable-page-cache`.
        const PAGE_CACHE = 1 << 8;
    }
}

impl Settings {
    /// The HTML overlay's settings (`agterm-gtk`'s `html_overlay/page.rs`): the page's own scripts only with
    /// `javascript`, and nothing that opens a window, reads other files, goes full screen or plays media unasked.
    #[must_use]
    pub fn overlay(javascript: bool) -> Self {
        let mut settings = Self::MEDIA_REQUIRES_USER_GESTURE;
        settings.set(Self::JAVASCRIPT_MARKUP, javascript);
        settings
    }
}

/// When a user script runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InjectionTime {
    /// Before the document's own scripts.
    Start,
    /// After the document has loaded.
    End,
}

/// A script `WebKit` injects into every top-frame load (`WebKitUserScript`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserScript {
    pub source: String,
    pub time: InjectionTime,
    /// The script world it runs in; `None` for the page's own.
    pub world: Option<String>,
}

/// A script message handler (`window.webkit.messageHandlers.<name>.postMessage(body)`, which returns a promise of
/// the reply) registered in `world` (`None`: the page's own).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageHandler {
    pub name: String,
    pub world: Option<String>,
}

/// A color, each channel in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Rgba {
    pub const TRANSPARENT: Self = Self {
        red: 0.0,
        green: 0.0,
        blue: 0.0,
        alpha: 0.0,
    };
    /// The engine's own canvas.
    pub const WHITE: Self = Self {
        red: 1.0,
        green: 1.0,
        blue: 1.0,
        alpha: 1.0,
    };
}

/// A page's size in logical pixels and its scale; buffers are `width * scale` by `height * scale`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

/// A page (`WebKitWebView` over a `WPEView`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewSpec {
    /// The browser store, created first ([`crate::Host::create_session`]).
    pub session: SessionId,
    pub settings: Settings,
    pub scripts: Vec<UserScript>,
    pub message_handlers: Vec<MessageHandler>,
    pub zoom: f64,
    /// Drawn where the page draws nothing; `None` keeps the engine's opaque white.
    pub background: Option<Rgba>,
    pub geometry: Geometry,
    /// `WebKit` features switched off for the page, by their identifiers (`WebKitFeature`); one this `WebKit` doesn't
    /// have fails the page ([`crate::Event::ViewFailed`]).
    pub features_off: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_settings_follow_the_gtk_overlay() {
        assert_eq!(
            Settings::overlay(false),
            Settings::MEDIA_REQUIRES_USER_GESTURE,
            "media needs a gesture, and nothing else is on"
        );
        assert_eq!(
            Settings::overlay(true),
            Settings::MEDIA_REQUIRES_USER_GESTURE | Settings::JAVASCRIPT_MARKUP
        );
    }
}
