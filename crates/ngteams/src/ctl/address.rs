//! Which account and which chat a request names.
//!
//! - **An account** (`--account`): by its UPN (any case), its MRI or its index in the app's accounts (from 0, as
//!   `status` and `accounts` number them); without one, the account the window shows (the first one while it shows
//!   none). Anything else is [`ErrorCode::UnknownAccount`]; an account whose engine stopped (shut down, signed out) is
//!   [`ErrorCode::Stopped`].
//! - **A chat**: by its id (any stored conversation's), or by its title as the chat list names it
//!   ([`chat_list::chat_name`]: the topic, otherwise the other members' names; a 1:1 chat the other person's), in any
//!   case. A title several chats have is [`ErrorCode::AmbiguousChat`] with each one's id and title; none is
//!   [`ErrorCode::UnknownChat`].

use std::sync::Arc;

use gpui::{App, Entity};
use teams_ctl::protocol::{ChatRef, CtlError, ErrorCode};
use teams_store::Conversation;
use teams_ui::chat_list::{self, ALL_CHATS};
use teams_ui::{Account, Core, Engine, Tray};

use crate::app::Running;

/// The account a request addresses, as its reads need it.
pub struct Addressed {
    pub upn: String,
    pub mri: String,
    pub core: Arc<dyn Core>,
    /// The badge's unread counts of the account (none while the window holds no tile of it).
    pub tray: Option<Entity<Tray>>,
}

/// The account `wanted` names, or the one the window shows without it (see the module docs).
///
/// # Errors
/// [`ErrorCode::UnknownAccount`] when no account is the one named (or there is none), [`ErrorCode::Stopped`] when its
/// engine stopped.
pub fn account(running: &Running, wanted: Option<&str>, cx: &App) -> Result<Addressed, CtlError> {
    let (mri, upn, bridge) = known(running, wanted, cx)?;
    let bridge = bridge.read(cx);
    let status = bridge.models().status.read(cx);
    if !bridge.running() || status.engine == Engine::Stopped || matches!(status.account, Account::SignedOut { .. }) {
        return Err(stopped(upn));
    }
    let tray = running
        .desktop()
        .read(cx)
        .accounts()
        .read(cx)
        .tile(mri)
        .map(|tile| tile.tray.clone());
    Ok(Addressed {
        upn: upn.clone(),
        mri: mri.clone(),
        core: bridge.core(),
        tray,
    })
}

/// The account `wanted` names, or the one the window shows without it, as `(MRI, UPN, bridge)`, whether or not its
/// engine runs (the window shows a signed-out account too).
///
/// # Errors
/// [`ErrorCode::UnknownAccount`] when no account is the one named (or there is none).
pub fn known<'a>(
    running: &'a Running,
    wanted: Option<&str>,
    cx: &App,
) -> Result<(&'a String, &'a String, &'a Entity<teams_ui::Bridge>), CtlError> {
    let bridges = running.bridges();
    let index = if let Some(wanted) = wanted.map(str::trim) {
        named(bridges, wanted)
    } else {
        let rail = running.desktop().read(cx).accounts().read(cx);
        rail.active_key()
            .and_then(|shown| bridges.iter().position(|(mri, _, _)| mri == shown))
            .or_else(|| (!bridges.is_empty()).then_some(0))
    };
    let Some((mri, upn, bridge)) = index.and_then(|index| bridges.get(index)) else {
        return Err(CtlError::new(
            ErrorCode::UnknownAccount,
            match wanted {
                Some(wanted) => format!("no account is {wanted}: `ngteamsctl accounts` lists them"),
                None => "the app runs no account".to_owned(),
            },
        ));
    };
    Ok((mri, upn, bridge))
}

/// The index of the account `wanted` names: its MRI, its UPN in any case, or its index.
fn named(bridges: &[(String, String, Entity<teams_ui::Bridge>)], wanted: &str) -> Option<usize> {
    bridges
        .iter()
        .position(|(mri, upn, _)| mri == wanted || upn.eq_ignore_ascii_case(wanted))
        .or_else(|| wanted.parse::<usize>().ok().filter(|index| *index < bridges.len()))
}

/// The error of an account whose engine stopped.
fn stopped(upn: &str) -> CtlError {
    CtlError::new(
        ErrorCode::Stopped,
        format!("{upn}'s engine stopped: sign it in again or start the app again"),
    )
}

/// The error an engine's request failed with: [`ErrorCode::Stopped`] once it stopped, otherwise
/// [`ErrorCode::Engine`] with its message.
#[must_use]
pub fn engine_error(error: &teams_core::EngineError) -> CtlError {
    match error {
        teams_core::EngineError::Stopped => CtlError::new(ErrorCode::Stopped, "the account's engine stopped"),
        other => CtlError::new(ErrorCode::Engine, other.to_string()),
    }
}

/// The account's stored conversations the chat list lists, the latest activity first.
///
/// # Errors
/// The engine's ([`engine_error`]).
pub async fn listed(core: &dyn Core) -> Result<Vec<Conversation>, CtlError> {
    let conversations = core
        .conversations(ALL_CHATS)
        .await
        .map_err(|error| engine_error(&error))?;
    Ok(conversations.into_iter().filter(chat_list::listed).collect())
}

/// The chat `wanted` names (see the module docs).
///
/// # Errors
/// [`ErrorCode::UnknownChat`], [`ErrorCode::AmbiguousChat`] with the candidates, or the engine's.
pub async fn chat(core: &dyn Core, wanted: &str) -> Result<Conversation, CtlError> {
    let conversations = core
        .conversations(ALL_CHATS)
        .await
        .map_err(|error| engine_error(&error))?;
    let wanted = wanted.trim();
    if let Some(found) = conversations.iter().find(|conversation| conversation.id == wanted) {
        return Ok(found.clone());
    }
    let mut found: Vec<(Conversation, String)> = Vec::new();
    for conversation in conversations.into_iter().filter(chat_list::listed) {
        let title = chat_list::chat_name(core, &conversation).await.to_string();
        if title.trim().to_lowercase() == wanted.to_lowercase() {
            found.push((conversation, title));
        }
    }
    match found.len() {
        0 => Err(CtlError::new(
            ErrorCode::UnknownChat,
            format!("no chat is {wanted}: `ngteamsctl chats` lists them"),
        )),
        1 => Ok(found.swap_remove(0).0),
        many => Err(CtlError {
            code: ErrorCode::AmbiguousChat,
            message: format!("{many} chats are called {wanted}: name one by its id"),
            candidates: found
                .into_iter()
                .map(|(conversation, title)| ChatRef {
                    id: conversation.id,
                    title,
                })
                .collect(),
        }),
    }
}
