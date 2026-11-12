//! `open-link`: a Teams link opened as the app opens it, what a second launch hands over ([`crate::handoff`]) and what
//! the app opens itself when it starts with one.
//!
//! - **A meeting's join link** (`msteams:/l/meetup-join/…`, or `https://teams.microsoft.com/l/meetup-join/…`, the
//!   form a browser hands the desktop): its meeting's pre-join opened in the call window, as the calendar's Join opens
//!   it ([`calls::show_prejoin`]); nothing goes out until the user joins there. The meeting's chat is the link's; its
//!   organizer and title are read from that chat's thread ([`Core::meeting`]), the organizer otherwise taken from the
//!   link's `context`. A link that names neither is [`ErrorCode::NoMeeting`]. It answers `done`.
//! - **Any other link**: the window shown, and the link logged as unrecognised by its shape alone (its scheme), never
//!   its text. It answers with what the window shows, as `activate` does.
//!
//! The account is the one `--account` names, otherwise the one the window shows. A join while a call is up, rings or is
//! being set up is refused ([`ErrorCode::Busy`]), as the calendar's Join refuses it; the read-only mode joins nothing
//! ([`ErrorCode::RefusedReadOnly`]); an app that shows no calls answers [`ErrorCode::NoCalling`]. The link is never
//! opened in the browser: the desktop hands it here, and would hand it back.

use gpui::{App, AsyncApp, WeakEntity};
use teams_ctl::protocol::{CALENDAR_ANSWER, CtlError, ErrorCode, Request, Response};
use teams_proto::{MeetingInfo, join_url_chat, join_url_meeting};
use teams_ui::Desktop;
use teams_ui::calls::{self as ui_calls, MeetingToJoin};

use super::address::engine_error;
use super::calls::{self, Fallback, Target};
use super::dispatch::alive;
use super::ui;
use super::writes::within;
use crate::app::Running;

/// What a meeting's pre-join is titled when its chat's thread names no title (as the meeting chat's Join titles it).
const UNTITLED: &str = "Meeting";

/// What a link opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    /// A meeting's join link: its chat, and the organizer its `context` names, when it names one.
    Meeting { chat: String, meeting: Option<MeetingInfo> },
    /// Anything else.
    Other,
}

impl Link {
    /// `url` read (see the module docs).
    #[must_use]
    pub fn of(url: &str) -> Self {
        let url = url.trim();
        if !(has_prefix(url, "msteams:") || has_prefix(url, "https://teams.microsoft.com/")) {
            return Self::Other;
        }
        match join_url_chat(url) {
            Some(chat) => Self::Meeting {
                chat,
                meeting: join_url_meeting(url),
            },
            None => Self::Other,
        }
    }
}

/// Whether `text` starts with `prefix`, in any case.
fn has_prefix(text: &str, prefix: &str) -> bool {
    text.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
}

/// Whether `request` is `open-link`.
#[must_use]
pub fn serves(request: &Request) -> bool {
    matches!(request, Request::OpenLink { .. })
}

/// `request`, an `open-link`, answered for `account` (see the module docs).
///
/// # Errors
/// The account isn't one, the app shows no calls or is read-only, the link names no meeting the app joins, the chat's
/// thread wasn't read in time, a call is up or rings, or the app is quitting.
pub async fn answer(
    running: &WeakEntity<Running>,
    account: Option<&str>,
    request: &Request,
    cx: &mut AsyncApp,
) -> Result<Response, CtlError> {
    let Request::OpenLink { url } = request else {
        return Err(CtlError::new(ErrorCode::BadRequest, "not open-link"));
    };
    match Link::of(url) {
        Link::Meeting { chat, meeting } => {
            let target = cx.update(|cx| calls::target(&alive(running, cx)?, account, Fallback::Shown, cx))?;
            let executor = cx.background_executor().clone();
            let late = format!("the meeting wasn't read in {}s", CALENDAR_ANSWER.as_secs());
            let to_join = within(CALENDAR_ANSWER, &late, to_join(&target, chat, meeting), &executor).await?;
            cx.update(|cx| prejoin(&target, to_join, cx))?;
            Ok(Response::Done)
        }
        Link::Other => cx.update(|cx| {
            let running = alive(running, cx)?;
            log::info!("an unrecognised {} link was opened: the window is shown", scheme(url));
            let desktop = running.read(cx).desktop().clone();
            desktop.update(cx, Desktop::show);
            Ok(Response::UiState(ui::ui_state(running.read(cx), cx)))
        }),
    }
}

/// `url`'s shape as the log names it: its scheme when it has a usual one, never more of it.
fn scheme(url: &str) -> &'static str {
    let url = url.trim();
    ["msteams", "https", "http"]
        .into_iter()
        .find(|scheme| url.len() > scheme.len() && has_prefix(url, scheme) && url[scheme.len()..].starts_with(':'))
        .unwrap_or("other")
}

/// The meeting of `chat` as the pre-join needs it: its organizer and title read from the chat's thread, the organizer
/// otherwise the link's (`meeting`).
async fn to_join(target: &Target, chat: String, meeting: Option<MeetingInfo>) -> Result<MeetingToJoin, CtlError> {
    let core = &*target.account.core;
    if core.read_only() {
        return Err(CtlError::new(
            ErrorCode::RefusedReadOnly,
            "the read-only mode joins no meeting",
        ));
    }
    let (meeting, title) = match (core.meeting(chat.clone()).await, meeting) {
        (Ok(Some(details)), _) => (details.meeting, details.title),
        // the link's own organizer still opens it: a thread the account can't read is its meeting's to tell
        (Ok(None) | Err(_), Some(meeting)) => (meeting, None),
        (Ok(None), None) => {
            return Err(CtlError::new(
                ErrorCode::NoMeeting,
                "the link names no Teams meeting the app can join",
            ));
        }
        (Err(error), None) => return Err(engine_error(&error)),
    };
    Ok(MeetingToJoin::Chat {
        chat,
        meeting,
        title: title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| UNTITLED.to_owned()),
    })
}

/// `to_join`'s pre-join opened on `target`'s calls, as the calendar's Join opens it.
fn prejoin(target: &Target, to_join: MeetingToJoin, cx: &mut App) -> Result<(), CtlError> {
    let busy_elsewhere = target.accounts.read(cx).busy_elsewhere(&target.account.mri, cx);
    if !ui_calls::show_prejoin(&target.calls, &target.trace, to_join, busy_elsewhere, cx) {
        return Err(calls::refusal(target.calls.read(cx).refused()));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/support/ctl_links.rs"]
mod tests;
