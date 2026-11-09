//! A call's sounds, heard as it goes along ([`crate::sounds`]): what the account's calls do, read from the [`Calls`]
//! model each time it changes and told to the [`SoundPlayer`].
//!
//! The ringback plays while the account's call rings at its callees. The rest play once, each as its state changes:
//! the call connecting and ending, its connection lost and back, people coming and going (not in a call larger than
//! [`LARGE_CALL`], and never for the roster first told), someone waiting in the lobby, the far end holding the call, a
//! recording, a screen shared, a raised hand, a live reaction, the account's own microphone, and an incoming call that
//! nobody answered ([`Calls::missed`]). The ringtone of an incoming call is the [`CallAlert`](crate::call_alert)'s.

use std::collections::BTreeSet;
use std::sync::Arc;

use gpui::{App, Context, Entity, Subscription};
use teams_core::{CallId, CallStatus};

use crate::calls::{ActiveCall, Calls};
use crate::ringtone::Ringtone;
use crate::settings::SettingsModel;
use crate::sounds::{LARGE_CALL, SoundEvent, SoundPlayer};

/// What a call was when last looked at.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a state of the call of its own, whose change makes a sound of its own"
)]
struct Seen {
    id: CallId,
    connected: bool,
    reconnecting: bool,
    held: bool,
    muted: bool,
    /// Who is in it, from the first roster told.
    roster: Option<BTreeSet<String>>,
    hands: BTreeSet<String>,
    lobby: BTreeSet<String>,
    recording: bool,
    sharing: bool,
    /// When the newest live reaction seen was sent.
    reacted: Option<std::time::Instant>,
}

impl Seen {
    fn of(call: &ActiveCall) -> Self {
        Self {
            id: call.id,
            connected: false,
            reconnecting: false,
            held: call.held_by_far_end,
            muted: call.muted,
            roster: None,
            hands: raised(call),
            lobby: waiting(call),
            recording: call.meeting.recording.is_some(),
            sharing: call.remote_share,
            reacted: call.reactions.last().map(|reaction| reaction.at),
        }
    }
}

fn raised(call: &ActiveCall) -> BTreeSet<String> {
    call.view
        .participants
        .iter()
        .filter(|person| person.hand_raised)
        .map(|person| person.mri.clone())
        .collect()
}

fn waiting(call: &ActiveCall) -> BTreeSet<String> {
    call.lobby.iter().map(|person| person.mri.to_string()).collect()
}

/// A state that went from `before` to `now`: `on` as it comes, `off` as it goes.
fn edge(before: bool, now: bool, on: SoundEvent, off: SoundEvent) -> Option<SoundEvent> {
    match (before, now) {
        (false, true) => Some(on),
        (true, false) => Some(off),
        _ => None,
    }
}

/// The sounds of one account's calls (see the module docs). A GPUI entity the desktop holds.
pub struct CallSounds {
    account: String,
    calls: Entity<Calls>,
    settings: Entity<SettingsModel>,
    seen: Option<Seen>,
    /// The call whose invitation last rang unanswered, already told.
    missed: Option<CallId>,
    ringback: Option<Ringtone>,
    _observing: Subscription,
}

impl CallSounds {
    pub fn new(account: String, calls: Entity<Calls>, settings: Entity<SettingsModel>, cx: &mut Context<Self>) -> Self {
        let observing = cx.observe(&calls, |this: &mut Self, _, cx| this.follow(cx));
        let missed = calls.read(cx).missed(&account);
        Self {
            account,
            calls,
            settings,
            seen: None,
            missed,
            ringback: None,
            _observing: observing,
        }
    }

    fn player(&self, cx: &App) -> Option<Arc<dyn SoundPlayer>> {
        self.settings.read(cx).sound_player().cloned()
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let Some(player) = self.player(cx) else {
            self.ringback = None;
            return;
        };
        let calls = self.calls.read(cx);
        let in_call = calls.call().is_some();
        let call = calls
            .call()
            .filter(|call| !matches!(call.status, CallStatus::Ended(_)))
            .cloned();
        let missed = calls.missed(&self.account);
        if missed.is_some() && missed != self.missed {
            player.play(SoundEvent::MissedCall, &self.account, in_call);
        }
        self.missed = missed;
        let ringing = call.as_ref().is_some_and(|call| call.status == CallStatus::Ringing);
        if ringing && self.ringback.is_none() {
            self.ringback = Some(player.looped(SoundEvent::OutgoingCall));
        } else if !ringing {
            self.ringback = None;
        }
        let events = self.changes(call.as_ref());
        for event in events {
            player.play(event, &self.account, in_call);
        }
    }

    /// What changed since the call was last looked at, as the sounds to play.
    fn changes(&mut self, call: Option<&ActiveCall>) -> Vec<SoundEvent> {
        let Some(call) = call else {
            return self
                .seen
                .take()
                .filter(|seen| seen.connected)
                .map(|_| vec![SoundEvent::CallEnded])
                .unwrap_or_default();
        };
        let seen = match &mut self.seen {
            Some(seen) if seen.id == call.id => seen,
            slot => slot.insert(Seen::of(call)),
        };
        let mut events = Vec::new();
        let connected = matches!(call.status, CallStatus::Connected | CallStatus::Reconnecting);
        if connected && !seen.connected {
            seen.connected = true;
            events.push(SoundEvent::CallJoined);
        }
        let reconnecting = call.status == CallStatus::Reconnecting;
        events.extend(edge(
            seen.reconnecting,
            reconnecting,
            SoundEvent::Reconnecting,
            SoundEvent::Reconnected,
        ));
        seen.reconnecting = reconnecting;
        events.extend(edge(
            seen.held,
            call.held_by_far_end,
            SoundEvent::OnHold,
            SoundEvent::Resumed,
        ));
        seen.held = call.held_by_far_end;
        events.extend(edge(
            seen.muted,
            call.muted,
            SoundEvent::MicMuted,
            SoundEvent::MicUnmuted,
        ));
        seen.muted = call.muted;
        let recording = call.meeting.recording.is_some();
        events.extend(edge(
            seen.recording,
            recording,
            SoundEvent::RecordingStarted,
            SoundEvent::RecordingStopped,
        ));
        seen.recording = recording;
        events.extend(edge(
            seen.sharing,
            call.remote_share,
            SoundEvent::ScreenShareStarted,
            SoundEvent::ScreenShareEnded,
        ));
        seen.sharing = call.remote_share;
        Self::people(seen, call, &mut events);
        events
    }

    /// Who came, left, waits, raised a hand or reacted.
    fn people(seen: &mut Seen, call: &ActiveCall, events: &mut Vec<SoundEvent>) {
        let lobby = waiting(call);
        if !lobby.is_subset(&seen.lobby) {
            events.push(SoundEvent::LobbyRequest);
        }
        seen.lobby = lobby;
        let hands = raised(call);
        if !hands.is_subset(&seen.hands) {
            events.push(SoundEvent::HandRaised);
        }
        seen.hands = hands;
        let reacted = call.reactions.last().map(|reaction| reaction.at);
        if reacted.is_some() && reacted != seen.reacted {
            events.push(SoundEvent::LiveReaction);
        }
        seen.reacted = reacted;
        let roster: BTreeSet<String> = call.view.participants.iter().map(|person| person.mri.clone()).collect();
        if roster.is_empty() {
            return;
        }
        if let Some(before) = &seen.roster
            && roster.len().max(before.len()) <= LARGE_CALL
        {
            if !roster.is_subset(before) {
                events.push(SoundEvent::PersonJoined);
            }
            if !before.is_subset(&roster) {
                events.push(SoundEvent::PersonLeft);
            }
        }
        seen.roster = Some(roster);
    }
}
