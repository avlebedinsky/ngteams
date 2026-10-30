//! WPE `WebKit` host, taken from ngterm's `ngterm-wpe` (MIT, the user's own project), where it runs the GPUI
//! frontend's HTML overlays; here it runs the sign-in window's page (`teams-signin`). Pages are rendered offscreen by
//! WPE `WebKit` (the `WPEPlatform` API) on an engine thread of their own, frames handed to the consumer as dmabufs on
//! its GPU, or as SHM pixels (the sign-in window's choice: [`FrameMode::Shm`]).
//!
//! What this copy adds to ngterm's: a store whose every load goes through a proxy the consumer holds
//! ([`SessionSpec::Guarded`]), web process extensions loaded into every web process ([`HostConfig::extensions`]),
//! `WebKit` features switched off per page ([`ViewSpec::features_off`]), WebRTC and the page cache as settings, a
//! page's id as its extension sees it ([`Event::ViewCreated`]), a navigation's method, and worker engines started as
//! often as asked (each a process of its own).
//!
//! - [`Host::start`] spawns the engine thread with its own `GMainContext` (thread-default before any `WebKit` object
//!   exists) and a display that renders on the consumer's render node with the formats it imports ([`HostConfig`],
//!   [`FrameMode`]). One in-process engine per process: `WebKit` binds its main run loop to the first thread that uses
//!   it, and that thread never ends (a shutdown closes everything and leaves it idle; the process then ends through
//!   [`exit_process`], since `WebKit`'s exit handlers must not run off its main thread). A worker engine
//!   ([`Host::start_worker`]) is a process of its own, so a process may start one after another.
//! - Commands go in through [`Host`]'s methods; everything else comes out as [`Event`]s on one channel: frames, load
//!   progress, what a page shows, and the questions `WebKit` asks (navigation and response policy, script messages,
//!   reads of the consumer's URI schemes), each held open until the consumer answers it by token.
//! - Frames follow `WebKit`'s buffer protocol (module `frames`): a frame counts as rendered once the consumer presented
//!   it, and a dmabuf goes back one frame later; acknowledgements run from a source of their own on the engine's
//!   context, never inside `WebKit`'s own hand-over. A page whose dmabufs the consumer cannot import falls back to
//!   SHM frames while it runs ([`Host::fall_back_to_shm`]).
//! - The `WebKit` surface the overlay uses: browser stores ([`SessionSpec`]: in memory, or on disk for saved logins;
//!   downloads refused), [`Settings`], user scripts and script message handlers, navigation and response policy, URI
//!   schemes, and what a page may not do (new windows, dialogs, file choosers, permissions, full screen) refused.
//! - [`worker`]: the engine in a process of its own, which the vendored WPE `WebKit` (its helpers at a compiled-in
//!   path, reachable only inside an overlay of `/usr/lib`, [`launcher`]) always gets, so the consumer itself never runs
//!   in that overlay's user namespace; [`Host::start_worker`] asks for one anywhere. Every program that may start an
//!   engine calls [`worker::serve_if_asked`] first thing in its `main`.
//!
//! - What `WebKitGTK` draws itself and an offscreen page cannot (module `engine::interaction`): the clipboard
//!   (`WPEPlatform`'s is the engine's own: a page's copy is reported, the desktop's text is handed in before a paste),
//!   `<select>` popups and the context menu (reported for the consumer to draw, answered by token or by editing
//!   command), and input methods (an `WPEInputMethodContext` of the engine's that takes the consumer's composition and
//!   commits and reports the caret's area).
//!
//! Not here: accessibility (an offscreen page has no AT-SPI bridge into the consumer's tree).

/// `from_raw` and `raw` on the engine's id types: a consumer's stand-in for the engine in its own tests makes ids
/// with them, and logs read them; the engine itself never takes an id it did not hand out.
macro_rules! raw_ids {
    ($($id:ident),+) => {
        $(
            impl $id {
                /// The id numbered `raw` (crate docs: for a consumer's stand-in engine).
                #[must_use]
                pub const fn from_raw(raw: u64) -> Self {
                    Self(raw)
                }

                /// The id's number.
                #[must_use]
                pub const fn raw(self) -> u64 {
                    self.0
                }
            }
        )+
    };
}
pub(crate) use raw_ids;

mod config;
mod engine;
mod event;
mod frames;
mod host;
mod input;
pub mod launcher;
mod spec;
pub mod worker;

use wpe_javascriptcore_sys as jsc_sys;
use wpe_platform_sys as platform_sys;
use wpe_webkit_sys as webkit_sys;

pub use config::{
    BufferFormat, ConfigError, DmaBufTarget, FrameMode, HostConfig, MODIFIER_INVALID, MODIFIER_LINEAR,
    WebProcessExtensions, fourcc,
};
pub use event::{
    Area, ContextMenu, EditingCommand, Event, LoadError, LoadErrorKind, LoadEvent, MenuToken, NavigationType,
    OptionItem, OptionKind, OptionMenu, PageState, PolicyKind, PolicyRequest, PolicyToken, ReplyToken, SchemeError,
    SchemeRequest, SchemeResponse, SchemeToken, ScriptMessage, Termination,
};
pub use frames::{BufferId, DmaBuf, Frame, FrameContent, Plane, Rect, ShmRegion};
pub use host::{Backend, Evaluation, Host, Load, StartError, exit_process};
pub use input::{Button, Input, Key, Modifiers, Pointer, PointerKind, Scroll};
pub use spec::{
    Geometry, InjectionTime, MessageHandler, Rgba, SessionId, SessionSpec, Settings, UserScript, ViewId, ViewSpec,
};
