//! What a meeting's call shares with everyone in it while it runs (`docs/protocol/calling.md`, "More of a running
//! call"), as its conversation and its roster tell it: the timer (the conversation's `meetingStates.timer`), the lock
//! (`meetingDetails.meetingCapability.lockMeeting`), how to join it (`meetingDetails.invitation`), and a recording
//! someone runs (Microsoft's recording bot in the roster, its endpoint's `processingModes.recording`). The account
//! changes the timer and the lock ([`Calls::set_timer`], [`Calls::lock_meeting`]) and starts or stops a recording
//! through the recording bot's command link ([`Calls::record_command`]). Each change is told as
//! [`CallEvent::Meeting`].

use serde_json::Value;
use teams_proto::{MeetingJoinInfo, MeetingTimer, Mri, TimerUpdate, UpdateMeetingSettings, UpdateMeetingStates};

use super::{CallError, CallEvent, CallId, Calls, up};
use crate::agent::Kind;
use crate::call::Step;
use crate::{CallWrite, Delivery};

/// Microsoft's recording bot: in the call's roster while a recording or a transcription runs (seen 2026-11-06,
/// `ab-call --call steps`); Teams hides it from the people in the call.
pub const RECORDER_MRI: &str = "28:bdd75849-e0a6-4cce-8fc1-d7c0d4da43e5";

/// A meeting's shared state as last told ([`CallEvent::Meeting`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeetingState {
    /// The timer, while one shows (none once cancelled, or before anyone set one).
    pub timer: Option<MeetingTimer>,
    /// Nobody else can join (the organizer locked it).
    pub locked: bool,
    /// How to join it, as its invitation says.
    pub join: Option<MeetingJoinInfo>,
    /// A recording runs.
    pub recording: Option<Recording>,
}

/// A recording someone runs in the call, as the recording bot's roster entry tells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    /// Who started it, when the entry names them (`userId`).
    pub by: Option<Mri>,
}

/// The meeting-state part of a call the machine keeps: the state told, the timer as last delivered (its sequence and
/// its JSON text, which the next change names and sends back), the settings changes the account made, and the
/// recording bot's roster version.
#[derive(Clone, Debug, Default)]
pub(crate) struct Meeting {
    pub(crate) state: MeetingState,
    timer: Option<TimerUpdate>,
    settings: u64,
    recorder_version: Option<u64>,
}

/// What a `conversationUpdate` (or a start's answer) tells of the meeting, each part only where it says it.
fn conversation_update(body: &[u8]) -> Option<(Option<TimerUpdate>, Option<bool>, Option<MeetingJoinInfo>)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let timer = body.pointer("/meetingStates/timer").and_then(MeetingTimer::read);
    Some((timer, teams_proto::meeting_locked(&body), MeetingJoinInfo::read(&body)))
}

/// The recording bot's entry in a roster update (`body`): its version, and whether it records now, with who started
/// it (`userId`, empty once it stopped); `None` when the update doesn't list it.
fn recorder(body: &[u8]) -> Option<(u64, Option<Recording>)> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let entry = body.get("participants")?.get(RECORDER_MRI)?;
    let version = entry.get("version")?.as_u64()?;
    let active = entry.get("state").and_then(Value::as_str) == Some("active");
    let recording = entry
        .get("endpoints")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|endpoints| endpoints.values())
        .filter_map(|endpoint| endpoint.pointer("/endpointMetadata/processingModes/recording"))
        .find(|mode| mode.get("state").and_then(Value::as_str) == Some("Active"))
        .filter(|_| active)
        .map(|mode| Recording {
            by: mode
                .get("userId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(Mri::new),
        });
    Some((version, recording))
}

impl Meeting {
    /// Takes what a `conversationUpdate` (or a start's answer) `body` tells; whether the state changed.
    pub(crate) fn conversation(&mut self, body: &[u8]) -> bool {
        let Some((timer, locked, join)) = conversation_update(body) else {
            return false;
        };
        let before = self.state.clone();
        if let Some(update) = timer {
            let newer = self.timer.as_ref().is_none_or(|told| told.sequence <= update.sequence);
            if newer {
                self.state.timer =
                    (update.timer.state != teams_proto::TimerState::Cancelled).then(|| update.timer.clone());
                self.timer = Some(update);
            }
        }
        if let Some(locked) = locked {
            self.state.locked = locked;
        }
        if join.is_some() {
            self.state.join = join;
        }
        self.state != before
    }

    /// Takes what a roster update `body` tells of the recording bot; whether the state changed.
    pub(crate) fn roster(&mut self, body: &[u8]) -> bool {
        let Some((version, recording)) = recorder(body) else {
            return false;
        };
        if self.recorder_version.is_some_and(|told| told >= version) {
            return false;
        }
        self.recorder_version = Some(version);
        let changed = self.state.recording != recording;
        self.state.recording = recording;
        changed
    }
}

impl Calls {
    /// The account sets the meeting's timer to `timer` (Start, Pause, Play, Reset, Cancel timer) at the conversation's
    /// `updateMeetingStates` link, as the change of the timer last told, as the web client sends it; everyone in the
    /// call is told with the conversation's next update ([`CallEvent::Meeting`]).
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up or its conversation
    /// named no such link; nothing was sent.
    pub fn set_timer(&mut self, call: CallId, timer: &MeetingTimer) -> Result<(), CallError> {
        let current = up(&mut self.call, call)?;
        let link = current.conversation.meeting_states.clone().ok_or(CallError::NotNow)?;
        let status = current.callback(Kind::Conversation, "updateMeetingStatesStatus")?;
        let told = current.meeting.timer.as_ref();
        let sequence = told.map_or(0, |told| told.sequence);
        let data = told.map(|told| told.data.clone());
        let write = UpdateMeetingStates::timer(
            &current.context,
            &link,
            &current.me,
            (timer, sequence, data.as_deref()),
            &status,
        )?;
        self.out
            .send(current, Step::MeetingStates, CallWrite::MeetingStates(write));
        Ok(())
    }

    /// The account, the meeting's organizer, locks it (`locked`: nobody else can join) or unlocks it at the
    /// conversation's `updateMeetingSettings` link, as Teams' Lock the meeting and Unlock the meeting send it.
    ///
    /// # Errors
    /// [`CallError::NoSuchCall`] for another call, [`CallError::NotNow`] while the call isn't up, the account doesn't
    /// organize it or its conversation named no such link; nothing was sent.
    pub fn lock_meeting(&mut self, call: CallId, locked: bool) -> Result<(), CallError> {
        let me = self.identity.mri.clone();
        let current = up(&mut self.call, call)?;
        if !current.roster.own(&me).organizer {
            return Err(CallError::NotNow);
        }
        let link = current.conversation.meeting_settings.clone().ok_or(CallError::NotNow)?;
        let sequence = current.meeting.settings + 1;
        let write = UpdateMeetingSettings::lock(&current.context, &link, &current.me, locked, sequence)?;
        current.meeting.settings = sequence;
        self.out
            .send(current, Step::MeetingSettings, CallWrite::MeetingSettings(write));
        Ok(())
    }

    /// A `conversationUpdate` at the call's own callback: the meeting's state told when it changed.
    pub(crate) fn conversation_updated(&mut self, delivery: &Delivery) {
        self.meeting_told(|meeting| meeting.conversation(&delivery.body));
    }

    /// A roster update's word on the recording bot: the meeting's state told when it changed.
    pub(crate) fn recorder_told(&mut self, body: &[u8]) {
        self.meeting_told(|meeting| meeting.roster(body));
    }

    /// A start's or a join's answer, as the conversation it is: what it tells of the meeting.
    pub(crate) fn meeting_answered(&mut self, body: &[u8]) {
        self.meeting_told(|meeting| meeting.conversation(body) | meeting.roster_of_answer(body));
    }

    fn meeting_told(&mut self, apply: impl FnOnce(&mut Meeting) -> bool) {
        let Some(call) = self.call.as_mut() else { return };
        if apply(&mut call.meeting) {
            let (id, state) = (call.id, call.meeting.state.clone());
            self.out.event(CallEvent::Meeting { call: id, state });
        }
    }
}

impl Meeting {
    /// A start's answer's `roster` (as an update's): the recording bot in it.
    fn roster_of_answer(&mut self, body: &[u8]) -> bool {
        let Some(roster) = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|body| body.get("roster").cloned())
        else {
            return false;
        };
        self.roster(roster.to_string().as_bytes())
    }
}
