//! A 1:1 call's shared screen in the engine, as Teams' Share and Stop sharing and the far end's share
//! (`docs/protocol/calling.md`, "More of a running call"):
//!
//! - **Share** ([`crate::CallAction::Share`] on): the user picks the screen at the desktop's portal
//!   ([`media::pick_screen`], off the engine's thread, the call going on meanwhile); then, as the web client does, the
//!   account tells the call's conversation it shares (`screenSharerAttributes`) and renegotiates with `["Audio",
//!   "ScreenSharer"]`, its offer adding a sharing line that only sends ([`media::share`]). **Stop sharing**
//!   renegotiates `["Audio"]` with that line rejected ([`media::stop_share`]); so does the screen ending at the
//!   desktop. [`Event::CallSharing`] tells it as it stands.
//! - **The far end's share**: its renegotiation adding a sharing line it sends on is answered receiving, with
//!   `["Audio", "ScreenViewer"]`; its frames go to the video output's sharing sink. [`Event::CallRemoteShare`] tells it
//!   shown and, once an offer rejects that line, gone.
//!
//! - **Someone's share in a group call**: the mixer's sharing line shows whoever the roster says shares, asked for as a
//!   receiving line's video is (`super::gallery`); [`Event::CallRemoteShare`] tells it shown and gone.
//!
//! The account shares only in a 1:1 call's media: a group call's screen goes out on the mixer's own sharing line,
//! which libwebrtc's bindings can't turn to send without an offer of ours (`docs/protocol/calling.md`).

use std::sync::Arc;
use std::time::Instant;

use teams_call::{CallId, Screen};
use teams_media::devices::VideoSource;
use teams_media::sdp::{NgcSdp, SharingLine};
use teams_proto::{Credentials, HttpError, Transport};
use tokio::sync::oneshot;

use super::media;
use super::{CallActionError, Calling, Waiting};
use crate::{EngineError, Event};

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// The account's Share (`on`) or Stop sharing in `call`; `reply` is answered once its renegotiation's offer went
    /// (or why not). Only a 1:1 call's media that takes video shares.
    pub(super) fn share(&mut self, call: CallId, on: bool, reply: Waiting) {
        let Some(live) = self.live.as_ref().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        if live.camera.is_none() {
            return refuse(reply, CallActionError::NoVideo);
        }
        if live.is_group() {
            return refuse(reply, CallActionError::Call(teams_call::CallError::NotNow));
        }
        if live.shares.own == on {
            let _ = reply.send(Ok(call));
            return;
        }
        if on {
            // the portal asks the user and waits for their answer: the call's negotiations go on meanwhile
            let step = media::pick_screen(call, Arc::clone(&self.media.devices), reply);
            self.media_steps.spawn_blocking(step);
            return;
        }
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                let step = media::stop_share(live, reply);
                self.sdp_step(step);
            }
            Err(error) => refuse(reply, error.into()),
        }
    }

    /// The screen the user picked (or why there is none): shared with the renegotiation it takes, once nothing else
    /// renegotiates the call.
    pub(super) fn screen_picked(&mut self, call: CallId, result: Result<Box<dyn VideoSource>, String>, reply: Waiting) {
        let screen = match result {
            Ok(screen) => screen,
            Err(why) => {
                log::info!("call {}: no screen shared: {why}", call.get());
                return refuse(reply, CallActionError::Media(why));
            }
        };
        let Some(live) = self.live.as_ref().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                let step = media::share(live, screen, reply);
                self.sdp_step(step);
            }
            Err(error) => refuse(reply, error.into()),
        }
    }

    /// The account's screen shared (`on`) or no longer, with the offer that adds or rejects its line: the sharer's
    /// participant properties told (sharing), the modalities named, the renegotiation sent, the state told.
    pub(super) fn shared(
        &mut self,
        (call, on): (CallId, bool),
        result: Result<Option<NgcSdp>, String>,
        reply: Waiting,
        now: Instant,
    ) {
        let sent = match result {
            Ok(Some(offer)) => {
                self.calls.set_screen(call, on.then_some(Screen::Sharer));
                if on && let Err(error) = self.calls.tell_screen_shared(call) {
                    log::info!(
                        "call {}: its sharing isn't told to the conversation: {error}",
                        call.get()
                    );
                }
                let sent = self.calls.renegotiate(call, &media::content(&offer));
                if sent.is_err() {
                    // the session waits on an offer that won't go
                    self.media_failed(call, "its renegotiation's offer can't go", now);
                }
                sent.map_err(CallActionError::from)
            }
            Ok(None) => {
                self.calls.give_up_renegotiation(call);
                Ok(())
            }
            Err(why) => {
                self.calls.give_up_renegotiation(call);
                Err(CallActionError::Media(why))
            }
        };
        let sharing = sent.is_ok() && on;
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call)
            && live.shares.own != sharing
        {
            live.shares.own = sharing;
            log::info!("call {}: sharing its screen {sharing}", call.get());
            self.emit(Event::CallSharing { call, on: sharing });
        }
        let _ = reply.send(sent.map(|()| call).map_err(EngineError::from));
    }

    /// The screen the account shares ended at the desktop: stopped, as Stop sharing does (nobody waits for it).
    pub(super) fn screen_ended(&mut self, call: CallId) {
        log::info!("call {}: the shared screen ended at the desktop", call.get());
        let (reply, _) = oneshot::channel();
        self.share(call, false, reply);
    }

    /// Our answer to the far end's renegotiation of `call`, a 1:1 call's: its modalities as the sharing line goes (ours
    /// sending, the far end's shown), and the far end's share told when it came or went.
    pub(super) fn answered_share(&mut self, call: CallId, answer: &NgcSdp) {
        let line = answer.sharing_line();
        self.calls.set_screen(
            call,
            line.map(|line| match line {
                SharingLine::Sends => Screen::Sharer,
                SharingLine::Receives => Screen::Viewer,
            }),
        );
        let shown = line == Some(SharingLine::Receives);
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call)
            && live.shares.theirs != shown
        {
            live.shares.theirs = shown;
            log::info!("call {}: the far end's screen shown {shown}", call.get());
            self.emit(Event::CallRemoteShare { call, on: shown });
        }
    }
}

/// Answers `reply` with `error`: nothing was sent.
fn refuse(reply: Waiting, error: CallActionError) {
    let _ = reply.send(Err(EngineError::from(error)));
}
