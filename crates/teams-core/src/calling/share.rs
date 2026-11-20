//! A 1:1 call's shared screen in the engine, as Teams' Share and Stop sharing and the far end's share
//! (`docs/protocol/calling.md`, "More of a running call"):
//!
//! - **Share** ([`crate::CallAction::Share`] on): the user picks the screen at the desktop's portal
//!   ([`media::pick_screen`], off the engine's thread, the call going on meanwhile); then, as the web client does, the
//!   account tells the call's conversation it shares (`screenSharerAttributes`) and renegotiates with `["Audio",
//!   "ScreenSharer"]`, its offer adding a sharing line that only sends ([`media::share`]). **Stop sharing**
//!   renegotiates `["Audio"]` with that line rejected ([`media::stop_share`]); so does the screen ending at the
//!   desktop. [`Event::CallSharing`] tells how the share goes once the switches made are done, at once, as Teams'
//!   toolbar shows it ([`Calling::tell_sharing`]). As in Teams, a share or a stop while a renegotiation is open either
//!   way is not refused: it waits ([`media::ShareWait`]) and goes once that one settled ([`Calling::share_again`]),
//!   never two offers at once; the later switch says how the share ends.
//! - **The far end's share**: its renegotiation adding a sharing line it sends on is answered receiving, with
//!   `["Audio", "ScreenViewer"]`; its frames go to the video output's sharing sink. [`Event::CallRemoteShare`] tells it
//!   shown and, once an offer rejects that line, gone.
//!
//! - **Someone's share in a group call**: the mixer's sharing line shows whoever the roster says shares, asked for as a
//!   receiving line's video is (`super::gallery`); [`Event::CallRemoteShare`] tells it shown and gone.
//! - **Share in a group call or meeting** (`docs/protocol/calling.md`, "Share in a group call"): the screen is picked
//!   the same way, then goes out on the mixer's sharing line, which the call negotiated both ways from its start
//!   ([`media::share_group`]); as the web client does, the account tells the conversation it shares and describes its
//!   lines again with the sharing line `sendonly` (`Calls::describe_share`), renegotiating nothing. Stop sharing
//!   stops the frames and describes that line `recvonly` again.
//! - **The media set up again** (a new mixer session, as when someone is added to a 1:1 call): as in Teams, a share
//!   goes on. The old session gives its screen back ([`MediaDone::ScreenKept`]), and the screen goes out on the new
//!   session's sharing line as a group call's share does ([`Calling::screen_kept`]). The share stays told as on
//!   meanwhile. A Stop sharing before the screen came drops it, and one while it starts to go out stops it once it
//!   went ([`Calling::group_shared`]).

use std::sync::Arc;
use std::time::Instant;

use teams_call::{CallId, Screen};
use teams_media::devices::VideoSource;
use teams_media::sdp::{NgcSdp, SharingLine};
use teams_media::session::MediaSession;
use teams_proto::{Credentials, HttpError, Transport};
use tokio::sync::oneshot;

use super::media::{self, LiveMedia, MediaDone};
use super::{CallActionError, Calling, Waiting};
use crate::{EngineError, Event};

/// The shares of the session that the call's media replaces ([`Calling::rebuilt`]).
#[derive(Default)]
pub(super) struct OldShares {
    /// A share switch that waited for the old session's renegotiation: it goes on the new session.
    pub(super) waits: Option<media::ShareWait>,
    shared: bool,
    /// The old session's share step not done yet: a Stop sharing made keeps nothing.
    turning: Option<bool>,
    told: bool,
    shown: bool,
}

impl OldShares {
    /// The shares of `old`, its waiting switch taken.
    pub(super) fn of(old: &mut LiveMedia) -> Self {
        Self {
            waits: old.share_waits.take(),
            shared: old.shares.own,
            turning: old.shares.turning,
            told: old.shares.told,
            shown: old.shares.theirs,
        }
    }

    /// As in Teams, a screen the old session shared goes on in the new one, unless a switch waits.
    fn kept(&self) -> bool {
        self.shared && self.turning != Some(false) && self.waits.is_none()
    }

    /// The new session `live` starts as the share was told; a kept screen stays told as on while it comes.
    pub(super) fn carry(&self, live: &mut LiveMedia) {
        live.shares.told = self.told;
        if self.kept() {
            live.shares.turning = Some(true);
            live.screen_coming = true;
        }
    }
}

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// The account's Share (`on`) or Stop sharing in `call`; `reply` is answered once its renegotiation's offer went,
    /// or a group call's lines were described, or the switch waits for an open renegotiation (or why not). Only a
    /// call's media that takes video shares.
    pub(super) fn share(&mut self, call: CallId, on: bool, reply: Waiting) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        if live.camera.is_none() {
            return refuse(reply, CallActionError::NoVideo);
        }
        if live.share_goes() == on {
            let _ = reply.send(Ok(call));
            return;
        }
        if live.share_waits.take().is_some() {
            // the later switch goes back to how the share goes without the waiting one: that one goes no more
            log::info!("call {}: its waiting share switch goes no more", call.get());
            self.tell_sharing(call);
            let _ = reply.send(Ok(call));
            return;
        }
        if !on && std::mem::take(&mut live.screen_coming) {
            // the old session's screen has not come yet: it goes no more, and nothing shares on this session
            log::info!("call {}: its kept screen goes no more", call.get());
            live.shares.turning = None;
            self.tell_sharing(call);
            let _ = reply.send(Ok(call));
            return;
        }
        if on {
            // the portal asks the user and waits for their answer: the call's negotiations go on meanwhile
            let step = media::pick_screen(call, Arc::clone(&self.media.devices), reply);
            self.media_steps.spawn_blocking(step);
            return;
        }
        if live.is_group() {
            return self.stop_group_share(call, reply);
        }
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                live.shares.turning = Some(false);
                let step = media::stop_share(live, reply);
                self.sdp_step(step);
                self.tell_sharing(call);
            }
            Err(_) if self.calls.renegotiating(call) => {
                self.share_waits(call, media::ShareWait::Stop);
                let _ = reply.send(Ok(call));
            }
            Err(error) => refuse(reply, error.into()),
        }
    }

    /// Stop sharing in the group call `call`. A screen that still starts to go out stops once it went
    /// ([`Self::group_shared`]), as the two steps run in no fixed order.
    fn stop_group_share(&mut self, call: CallId, reply: Waiting) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        let starting = live.shares.turning == Some(true);
        live.shares.turning = Some(false);
        if starting {
            log::info!("call {}: its screen stops once it went out", call.get());
            self.tell_sharing(call);
            let _ = reply.send(Ok(call));
            return;
        }
        let step = media::share_group(live, None, reply);
        self.media_steps.spawn_blocking(step);
    }

    /// A share's media step done: the screen picked, or shared (or no longer) on a line of its own or a group call's.
    pub(super) fn share_step_done(&mut self, done: MediaDone, now: Instant) {
        match done {
            MediaDone::ScreenPicked { call, result, reply } => self.screen_picked(call, result, reply),
            MediaDone::ScreenKept { call, screen } => self.screen_kept(call, screen),
            MediaDone::GroupShared {
                call,
                on,
                result,
                reply,
            } => self.group_shared((call, on), result, reply),
            MediaDone::Shared {
                call,
                on,
                result,
                reply,
            } => self.shared((call, on), result, reply, now),
            _ => {}
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
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        if live.is_group() {
            // nothing renegotiates: the screen goes out on the sharing line, then the lines are described
            if let Err(error) = self.calls.describable(call) {
                return refuse(reply, error.into());
            }
            let step = media::share_group(live, Some(screen), reply);
            self.media_steps.spawn_blocking(step);
            return;
        }
        if live.share_goes() {
            // another share was taken while the user picked this screen
            let _ = reply.send(Ok(call));
            return;
        }
        match self.calls.begin_renegotiation(call) {
            Ok(()) => {
                live.shares.turning = Some(true);
                let step = media::share(live, screen, reply);
                self.sdp_step(step);
                self.tell_sharing(call);
            }
            Err(_) if self.calls.renegotiating(call) => {
                self.share_waits(call, media::ShareWait::Start(screen));
                let _ = reply.send(Ok(call));
            }
            Err(error) => refuse(reply, error.into()),
        }
    }

    /// The account's share switch in `call`, a 1:1 call's, while a renegotiation of it is open (either way): as in
    /// Teams it is taken, and it goes once that one settled ([`Self::share_again`]), never two offers at once.
    fn share_waits(&mut self, call: CallId, wait: media::ShareWait) {
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            let on = matches!(wait, media::ShareWait::Start(_));
            live.share_waits = Some(wait);
            log::info!(
                "call {}: sharing its screen {on} once its open renegotiation settled",
                call.get()
            );
        }
        self.tell_sharing(call);
    }

    /// How the account's share in `call` goes once the switches made are done ([`media::LiveMedia::share_goes`]),
    /// told when it changed: Teams' toolbar shows Stop sharing (or Share) at once, a switch that waits too.
    pub(super) fn tell_sharing(&mut self, call: CallId) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return;
        };
        let on = live.share_goes();
        if live.shares.told != on {
            live.shares.told = on;
            self.emit(Event::CallSharing { call, on });
        }
    }

    /// A share switch kept while a renegotiation was open goes once that one settled, with a renegotiation of its own
    /// (nobody waits for its answer). Tried again after the next one settled when another is under way.
    pub(super) fn share_again(&mut self, call: CallId) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return;
        };
        let Some(wait) = live.share_waits.take() else {
            return;
        };
        if let Err(error) = self.calls.begin_renegotiation(call) {
            log::info!("call {}: its share switch waits to go: {error}", call.get());
            live.share_waits = Some(wait);
            return;
        }
        let (reply, _) = oneshot::channel();
        match wait {
            media::ShareWait::Start(screen) => {
                live.shares.turning = Some(true);
                let step = media::share(live, screen, reply);
                self.sdp_step(step);
            }
            media::ShareWait::Stop => {
                live.shares.turning = Some(false);
                let step = media::stop_share(live, reply);
                self.sdp_step(step);
            }
        }
    }

    /// A share switch that waited while the media of `call` was set up again ([`Self::share_waits`]), once the new
    /// session answered: a screen goes out on the new session's sharing line as a group call's does, or with a
    /// renegotiation of its own on a 1:1 session. A stop has nothing to do: the new session shares nothing, and
    /// [`Self::rebuilt`] told the screen the old session shared as stopped.
    pub(super) fn share_after_rebuild(&mut self, call: CallId, wait: media::ShareWait) {
        let media::ShareWait::Start(screen) = wait else {
            return;
        };
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return;
        };
        if !live.is_group() {
            live.share_waits = Some(media::ShareWait::Start(screen));
            return self.share_again(call);
        }
        if let Err(error) = self.calls.describable(call) {
            log::info!("call {}: its waiting screen goes no more: {error}", call.get());
            return;
        }
        // the share stays told as on while the screen starts to go out (`group_shared` ends the step)
        live.shares.turning = Some(true);
        let (reply, _) = oneshot::channel();
        let step = media::share_group(live, Some(screen), reply);
        self.media_steps.spawn_blocking(step);
    }

    /// Lets go of the old `session` of `call`: it closes, once it gave back the screen it shared when that goes on.
    pub(super) fn let_go_of(&mut self, call: CallId, session: Arc<MediaSession>, old: &OldShares) {
        if old.kept() {
            let from = Arc::clone(&session);
            self.media_steps.spawn_blocking(move || MediaDone::ScreenKept {
                call,
                screen: from.take_screen(),
            });
        }
        tokio::task::spawn_blocking(move || super::close(call, session));
    }

    /// The shares of `call` once its new session replaced the old one: the new session shows no share yet, so a
    /// screen the old one showed is told gone (its sharing line tells it again, `show_participants`). A screen the old
    /// one shared goes on ([`Self::screen_kept`]), unless a switch waits.
    pub(super) fn shares_replaced(&mut self, call: CallId, old: &OldShares) {
        if old.shared || old.shown {
            self.calls.set_screen(call, None);
        }
        if old.kept() {
            log::info!(
                "call {}: its shared screen goes on once its media is set up again",
                call.get()
            );
        } else if old.shared {
            log::info!(
                "call {}: sharing its screen false: its media was set up again",
                call.get()
            );
        }
        if old.shown {
            log::info!(
                "call {}: a shared screen shown false: its media was set up again",
                call.get()
            );
            self.emit(Event::CallRemoteShare { call, on: false });
        }
    }

    /// The screen the old session shared, given back once the media of `call` was set up again ([`Self::rebuilt`]): it
    /// goes on in the new session as a screen that waited does ([`Self::share_after_rebuild`]), unless Stop sharing
    /// came meanwhile. A screen whose stream ended meanwhile ends the share.
    pub(super) fn screen_kept(&mut self, call: CallId, screen: Option<Box<dyn VideoSource>>) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return;
        };
        if !std::mem::take(&mut live.screen_coming) {
            return;
        }
        live.shares.turning = None;
        match screen {
            Some(screen) => self.share_after_rebuild(call, media::ShareWait::Start(screen)),
            None => log::info!(
                "call {}: its shared screen ended while its media was set up again",
                call.get()
            ),
        }
        self.tell_sharing(call);
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
        let offered = matches!(result, Ok(Some(_)));
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
                // a switch that waited has nobody to tell why it failed: the log tells it
                log::info!("call {}: its share switch {on} didn't go: {why}", call.get());
                self.calls.give_up_renegotiation(call);
                Err(CallActionError::Media(why))
            }
        };
        let sharing = sent.is_ok() && on;
        if let Some(live) = self.live.as_mut().filter(|live| live.call == call) {
            live.shares.turning = None;
            if live.shares.own != sharing {
                live.shares.own = sharing;
                log::info!("call {}: sharing its screen {sharing}", call.get());
            }
        }
        self.tell_sharing(call);
        if !offered {
            // no renegotiation went: a switch kept while it was open goes now
            self.offer_owed(call);
        }
        let _ = reply.send(sent.map(|()| call).map_err(EngineError::from));
    }

    /// The account's screen shared (`on`) or no longer on a group call's sharing line (or why not): as the web client
    /// does, the conversation told it shares, then the lines described again with the sharing line going as the
    /// screen does, and the state told. A screen whose lines can't be described stops going out again.
    pub(super) fn group_shared(&mut self, (call, on): (CallId, bool), result: Result<(), String>, reply: Waiting) {
        let Some(live) = self.live.as_mut().filter(|live| live.call == call) else {
            return refuse(reply, CallActionError::NoMedia);
        };
        // the share step is done; a Stop sharing that came while it started the screen stops it now
        let stopped = live.shares.turning.take() == Some(false) && on;
        if let Err(why) = result {
            log::info!("call {}: its screen isn't shared: {why}", call.get());
            self.tell_sharing(call);
            return refuse(reply, CallActionError::Media(why));
        }
        if stopped {
            // nothing was told or described yet
            log::info!("call {}: its screen stopped as it went out", call.get());
            let session = Arc::clone(&live.session);
            tokio::task::spawn_blocking(move || session.stop_group_screen());
            self.tell_sharing(call);
            let _ = reply.send(Ok(call));
            return;
        }
        if on && let Err(error) = self.calls.tell_screen_shared(call) {
            log::info!(
                "call {}: its sharing isn't told to the conversation: {error}",
                call.get()
            );
        }
        let described = live.describe_share(on);
        let told = self.calls.describe_share(call, &described);
        let sharing = match told {
            Ok(()) => on,
            Err(error) if on => {
                log::warn!(
                    "call {}: its screen's line can't be described ({error}): stopped",
                    call.get()
                );
                let session = Arc::clone(&live.session);
                tokio::task::spawn_blocking(move || session.stop_group_screen());
                self.tell_sharing(call);
                return refuse(reply, error.into());
            }
            Err(error) => {
                // the frames stopped: the line sends nothing whatever the mixer last heard
                log::info!("call {}: its stopped share isn't described: {error}", call.get());
                false
            }
        };
        if live.shares.own != sharing {
            live.shares.own = sharing;
            log::info!("call {}: sharing its screen {sharing}", call.get());
        }
        self.tell_sharing(call);
        let _ = reply.send(Ok(call));
    }

    /// The call's media controller asked for the account's video again (a viewer starts to watch it): a keyframe of
    /// the shared screen, as Teams for the web makes. The camera's keyframes are libwebrtc's, at the mixer's FIR.
    pub(super) fn video_asked(&self, call: CallId, camera: bool) {
        if camera {
            return;
        }
        if let Some(live) = self.live.as_ref().filter(|live| live.call == call) {
            live.session.ask_screen_keyframe();
        }
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
