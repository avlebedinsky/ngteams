//! What the engine reports ([`Event`]), and the requests among them the consumer answers by token.
//!
//! `WebKit` asks some questions synchronously (may this navigation go on, what does this URI serve, what does this
//! script message get back); the engine holds each one open, reports it with a token and answers it when the consumer
//! does ([`crate::Host::decide_policy`], [`crate::Host::reply_script_message`],
//! [`crate::Host::finish_scheme_request`]). Closing a page answers what it left open: navigations are ignored,
//! script messages get "page closed", scheme requests fail.

use std::path::PathBuf;

use crate::frames::{BufferId, Frame};
use crate::spec::{SessionId, ViewId};
use serde::{Deserialize, Serialize};

/// A policy question's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PolicyToken(pub(crate) u64);

/// A script message's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReplyToken(pub(crate) u64);

/// A scheme request's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SchemeToken(pub(crate) u64);

crate::raw_ids!(PolicyToken, ReplyToken, SchemeToken);

/// A main-frame load's progress (`WebKitLoadEvent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadEvent {
    Started,
    Redirected,
    Committed,
    Finished,
}

/// How a load failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadErrorKind {
    /// Cancelled, typically superseded by the next load (`WEBKIT_NETWORK_ERROR_CANCELLED`).
    Cancelled,
    /// Stopped by a policy decision or an unshowable response (`WEBKIT_POLICY_ERROR_*`).
    Interrupted,
    /// Anything else: the network, the server, the scheme handler.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadError {
    pub kind: LoadErrorKind,
    pub message: String,
}

/// Why a page's web process ended (`WebKitWebProcessTerminationReason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Termination {
    Crashed,
    ExceededMemoryLimit,
    TerminatedByApi,
}

/// What a page shows and where its history can go, reported whenever any of it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageState {
    pub uri: Option<String>,
    pub title: Option<String>,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// Why a navigation started (`WebKitNavigationType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NavigationType {
    LinkClicked,
    FormSubmitted,
    BackForward,
    Reload,
    FormResubmitted,
    Other,
}

/// A policy question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyKind {
    /// A navigation in the page's frame, or one that would open a new window.
    Navigation {
        uri: String,
        /// The request's HTTP method; `None` when `WebKit` left it out (a `GET`).
        method: Option<String>,
        navigation: NavigationType,
        new_window: bool,
    },
    /// A response about to be shown; nothing is ever saved to disk, so an unshowable one can only be ignored.
    Response {
        uri: String,
        main_frame: bool,
        mime_supported: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRequest {
    pub token: PolicyToken,
    pub view: ViewId,
    pub kind: PolicyKind,
}

/// A message a page's script sent to a [`crate::MessageHandler`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptMessage {
    pub token: ReplyToken,
    pub view: ViewId,
    pub handler: String,
    /// The body as JSON when it is an object, `None` for anything else.
    pub body: Option<String>,
}

/// A read of one of the consumer's URI schemes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemeRequest {
    pub token: SchemeToken,
    /// The page that reads it, when it is one of the host's.
    pub view: Option<ViewId>,
    pub uri: String,
}

/// Why a scheme request failed, as the page's load error reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemeError {
    NotFound,
    PermissionDenied,
    IsDirectory,
    Failed,
}

/// The answer to a [`SchemeRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SchemeResponse {
    /// A file, streamed from disk with the type its name suggests.
    File(PathBuf),
    Bytes {
        data: Vec<u8>,
        mime: String,
    },
    Error {
        kind: SchemeError,
        message: String,
    },
}

/// Something the engine reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// A frame to show; answer with [`crate::Host::frame_presented`] once it is on screen. Never serialized: a worker
    /// sends its content beside the message (crate `worker`).
    #[serde(skip)]
    Frame(Frame),
    /// Buffers that left a page's set: forget what was imported from them.
    BuffersRemoved {
        view: ViewId,
        buffers: Vec<BufferId>,
    },
    Load {
        view: ViewId,
        event: LoadEvent,
    },
    /// A failed load; the engine shows no error page of its own.
    LoadFailed {
        view: ViewId,
        uri: String,
        error: LoadError,
    },
    WebProcessTerminated {
        view: ViewId,
        reason: Termination,
    },
    Page {
        view: ViewId,
        page: PageState,
    },
    /// Answer with [`crate::Host::decide_policy`].
    Policy(PolicyRequest),
    /// Answer with [`crate::Host::reply_script_message`].
    ScriptMessage(ScriptMessage),
    /// Answer with [`crate::Host::finish_scheme_request`].
    SchemeRequest(SchemeRequest),
    /// The page's script called `window.close()`.
    CloseRequested {
        view: ViewId,
    },
    /// The pointer over the page wants this cursor (a CSS cursor name: `default`, `pointer`, `text`, ...).
    Cursor {
        view: ViewId,
        name: String,
    },
    /// A download started and was cancelled: nothing a page shows is saved to disk.
    DownloadRefused {
        session: SessionId,
        view: Option<ViewId>,
        uri: String,
    },
    /// A page was built ([`crate::Host::create_view`]): `page_id` is its page's id as a web process extension's
    /// `webkit_web_page_get_id` names it.
    ViewCreated {
        view: ViewId,
        page_id: u64,
    },
    /// A page could not be built ([`crate::Host::create_view`]): its store does not exist, or its spec is invalid
    /// (a message handler twice in one world, a feature to switch off that `WebKit` lacks). The id takes no commands.
    ViewFailed {
        view: ViewId,
        message: String,
    },
    /// A store's data was cleared ([`crate::Host::clear_session`]), or why it was not.
    SessionCleared {
        session: SessionId,
        result: Result<(), String>,
    },
    /// A page put text on the clipboard (a copy, a cut, Copy Link Address): `WPEPlatform`'s clipboard is the
    /// engine's own, so the consumer puts it on the desktop's. The consumer hands the desktop's text to the page
    /// before a paste ([`crate::Host::set_clipboard`]).
    Clipboard {
        text: String,
        /// The HTML the page copied with it, if any.
        html: Option<String>,
    },
    /// A `<select>` popup to show; answer with [`crate::Host::choose_option`].
    OptionMenu(OptionMenu),
    /// The page closed the popup of `token` itself (the select went away, lost focus).
    OptionMenuClosed {
        token: MenuToken,
    },
    /// A right-click asked for the page's context menu, which the consumer draws: what lies under the pointer.
    ContextMenu(ContextMenu),
    /// Where the page's caret is, for an input method's candidate window (logical pixels in the page).
    ImeCursorArea {
        view: ViewId,
        area: Area,
    },
}

/// A rectangle in a page's logical pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// An option popup's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MenuToken(pub(crate) u64);

crate::raw_ids!(MenuToken);

/// What a row of a `<select>` popup is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OptionKind {
    /// An option outside any group.
    #[default]
    Option,
    /// An `<optgroup>`'s label, not choosable.
    GroupLabel,
    /// An option inside an `<optgroup>`.
    GroupChild,
}

/// One row of a `<select>` popup (`WebKitOptionMenuItem`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionItem {
    pub label: String,
    pub tooltip: Option<String>,
    pub kind: OptionKind,
    pub enabled: bool,
    pub selected: bool,
}

/// A `<select>` popup: its rows, and the select's area to place it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OptionMenu {
    pub token: MenuToken,
    pub view: ViewId,
    pub area: Area,
    pub items: Vec<OptionItem>,
}

/// What lies under a right-click (`WebKitHitTestResult`), from which the consumer builds its menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextMenu {
    pub view: ViewId,
    /// Where the menu was asked for, in the page's logical pixels.
    pub x: i32,
    pub y: i32,
    pub link: Option<String>,
    pub image: Option<String>,
    pub media: Option<String>,
    /// An editable field.
    pub editable: bool,
    /// Over selected text.
    pub selection: bool,
}

/// An editing command a page runs on its focused content ([`crate::Host::editing_command`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditingCommand {
    Copy,
    Cut,
    Paste,
    PasteAsPlainText,
    SelectAll,
    Undo,
    Redo,
}
