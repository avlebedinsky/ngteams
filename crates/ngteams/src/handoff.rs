//! A second launch of the personal desktop app, or a Teams link opened from the desktop (`ngteams %u`), handed to the
//! app already running over its control socket ([`crate::ctl`]) rather than starting a second one.
//!
//! `main` asks it after parsing the command line and before anything is opened or locked, on the personal desktop
//! branch only, with what [`crate::cli::Cli::hand_off`] says (`activate`, or `open-link <url>`; nothing for
//! `--forget` and `--account`), and again, for a few seconds, when the first account turns out to be held by another
//! app (one still starting, which locks its accounts before it serves its socket). The socket is the one the app
//! would serve on itself: `--ctl-socket`, else the personal one. Every wait is short ([`TIMEOUTS`]) but a link's
//! answer, which may wait for its meeting to be read and so is waited for past the app's own bound on that
//! ([`CALENDAR_ANSWER`], as `ngteamsctl` waits for a calendar read):
//!
//! - nobody behind the socket (no file, or a stale one left by an app that ended): [`HandOff::NotRunning`], and this
//!   launch starts the app (whose server replaces the stale file);
//! - an app that answers: [`HandOff::Done`], and this launch ends;
//! - an app that takes the connection but doesn't answer in time, or answers with an error: [`HandOff::Failed`],
//!   said, and this launch ends without starting a second app (its engine lock and socket are held anyway).

use std::path::Path;
use std::time::Duration;

use teams_ctl::protocol::{CALENDAR_ANSWER, Request, Response};
use teams_ctl::{Client, ClientError, Timeouts};

/// The hand-off's bounds: a second to connect, five for the app's answer; an `open-link`'s answer is waited for
/// [`CALENDAR_ANSWER`] longer ([`hand_off`]).
pub const TIMEOUTS: Timeouts = Timeouts {
    connect: Duration::from_secs(1),
    read: Duration::from_secs(5),
    write: Duration::from_secs(2),
};

/// How a hand-off went.
#[derive(Debug, PartialEq, Eq)]
pub enum HandOff {
    /// The running app took it; `note` is what to tell the user, if anything (a link it doesn't open, say).
    Done { note: Option<String> },
    /// No app runs on the socket: this launch starts one.
    NotRunning,
    /// An app runs there but didn't take it: why, to say.
    Failed(String),
}

/// `request` handed to the app serving `socket`, within `timeouts`, an `open-link`'s answer within
/// [`CALENDAR_ANSWER`] more (see the module docs).
#[must_use]
pub fn hand_off(socket: &Path, request: Request, timeouts: Timeouts) -> HandOff {
    let link = matches!(request, Request::OpenLink { .. });
    let answered = Client::connect(socket, timeouts).and_then(|mut client| {
        // the app reads a meeting link's meeting first, bounded by `CALENDAR_ANSWER`: its answer, an error too, comes
        // before this wait ends
        if link {
            client.set_read_timeout(CALENDAR_ANSWER + timeouts.read)?;
        }
        client.request(None, request)
    });
    match answered {
        Ok(Response::UiState(_)) if link => HandOff::Done {
            note: Some("that link isn't a Teams meeting link the app opens: its window is shown".to_owned()),
        },
        Ok(_) => HandOff::Done { note: None },
        Err(ClientError::NotRunning { .. }) => HandOff::NotRunning,
        Err(ClientError::Timeout) => HandOff::Failed(format!(
            "Teams is running (its control socket {} is taken) but didn't answer in time: quit it, or end it if it \
             hangs, and start Teams again",
            socket.display()
        )),
        Err(ClientError::Server(error)) => HandOff::Failed(format!("the running Teams: {}", error.message)),
        Err(error) => HandOff::Failed(format!("the running Teams couldn't be reached: {error}")),
    }
}
