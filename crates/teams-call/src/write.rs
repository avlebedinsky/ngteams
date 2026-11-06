//! Typed call writes and their outcome IDs. The engine sends them through its outbox and returns each attempt's outcome.

use teams_proto::{
    AcceptCall, AcknowledgeAcceptance, AcknowledgeMediaAnswer, AddToCall, AnswerMedia, ApplyChannelParameters,
    AttachCall, DecideLobby, DeclineCall, JoinCall, KeepAliveCall, LeaveCall, RenegotiateMedia, ReportRinging, Request,
    StartCall, UpdateEndpointMetadata, UpdateEndpointState, UpdateMediaDescriptions, UpdateParticipantProperties,
};
use teams_proto::{EndConversation, HoldCall, PublishState, RemoveState, ResumeCall, SendCallReaction};
use teams_proto::{UpdateMeetingSettings, UpdateMeetingStates};

/// The id a write's outcome comes back under ([`crate::Calls::on_outcome`]). Unique for the machine's life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WriteId(pub(crate) u64);

impl WriteId {
    /// The number, for a log line.
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Where the engine takes a call's writes: each tracked and sent in the order submitted, its outcome
/// reported back under its id. Every submission must get its outcome (a denial, a refusal or a stop included), or the
/// call waits on it. A write comes as its own sealed type, cloneable: one the service refused without acting on it
/// may go again (a fresh check, a fresh permit) while the machine still wants it ([`crate::Calls::may_send_again`]).
pub trait WriteSink {
    /// Takes `write`, to be sent and its outcome reported under `id`.
    fn submit<R: Request + Clone + Send + 'static>(&mut self, id: WriteId, write: R);
}

/// One call write, as the machine built it: each a sealed `teams-proto` type, built whole and classified when made.
#[derive(Clone, Debug)]
pub enum CallWrite {
    Start(StartCall),
    Join(JoinCall),
    Add(AddToCall),
    Attach(AttachCall),
    Ringing(ReportRinging),
    EndpointState(UpdateEndpointState),
    Accept(AcceptCall),
    Decline(DeclineCall),
    Acknowledge(AcknowledgeAcceptance),
    ChannelParameters(ApplyChannelParameters),
    EndpointMetadata(UpdateEndpointMetadata),
    ParticipantProperties(UpdateParticipantProperties),
    Leave(LeaveCall),
    Renegotiate(RenegotiateMedia),
    AnswerMedia(AnswerMedia),
    AcknowledgeMediaAnswer(AcknowledgeMediaAnswer),
    MediaDescriptions(UpdateMediaDescriptions),
    KeepAlive(KeepAliveCall),
    Lobby(DecideLobby),
    PublishState(PublishState),
    RemoveState(RemoveState),
    Reaction(SendCallReaction),
    EndMeeting(EndConversation),
    Hold(HoldCall),
    Resume(ResumeCall),
    MeetingStates(UpdateMeetingStates),
    MeetingSettings(UpdateMeetingSettings),
}

impl CallWrite {
    /// Hands the write, as its own sealed type, to `sink` under `id`.
    pub fn submit_to(self, id: WriteId, sink: &mut impl WriteSink) {
        match self {
            Self::Start(write) => sink.submit(id, write),
            Self::Join(write) => sink.submit(id, write),
            Self::Add(write) => sink.submit(id, write),
            Self::Attach(write) => sink.submit(id, write),
            Self::Ringing(write) => sink.submit(id, write),
            Self::EndpointState(write) => sink.submit(id, write),
            Self::Accept(write) => sink.submit(id, write),
            Self::Decline(write) => sink.submit(id, write),
            Self::Acknowledge(write) => sink.submit(id, write),
            Self::ChannelParameters(write) => sink.submit(id, write),
            Self::EndpointMetadata(write) => sink.submit(id, write),
            Self::ParticipantProperties(write) => sink.submit(id, write),
            Self::Leave(write) => sink.submit(id, write),
            Self::Renegotiate(write) => sink.submit(id, write),
            Self::AnswerMedia(write) => sink.submit(id, write),
            Self::AcknowledgeMediaAnswer(write) => sink.submit(id, write),
            Self::MediaDescriptions(write) => sink.submit(id, write),
            Self::KeepAlive(write) => sink.submit(id, write),
            Self::Lobby(write) => sink.submit(id, write),
            Self::PublishState(write) => sink.submit(id, write),
            Self::RemoveState(write) => sink.submit(id, write),
            Self::Reaction(write) => sink.submit(id, write),
            Self::EndMeeting(write) => sink.submit(id, write),
            Self::Hold(write) => sink.submit(id, write),
            Self::Resume(write) => sink.submit(id, write),
            Self::MeetingStates(write) => sink.submit(id, write),
            Self::MeetingSettings(write) => sink.submit(id, write),
        }
    }

    /// The write's name, for a log line and a failed step ([`crate::End::Failed`]).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Start(_) => "start",
            Self::Join(_) => "join",
            Self::Add(_) => "add",
            Self::Attach(_) => "attach",
            Self::Ringing(_) => "progress",
            Self::EndpointState(_) => "update_endpoint_state",
            Self::Accept(_) => "accept",
            Self::Decline(_) => "reject",
            Self::Acknowledge(_) => "acknowledge",
            Self::ChannelParameters(_) => "apply_channel_parameters",
            Self::EndpointMetadata(_) => "update_endpoint_metadata",
            Self::ParticipantProperties(_) => "update_participant_properties",
            Self::Leave(_) => "leave",
            Self::Renegotiate(_) => "renegotiate",
            Self::AnswerMedia(_) => "media_answer",
            Self::AcknowledgeMediaAnswer(_) => "media_acknowledge",
            Self::MediaDescriptions(_) => "update_media_descriptions",
            Self::KeepAlive(_) => "keep_alive",
            Self::Lobby(_) => "lobby_decision",
            Self::PublishState(_) => "publish_state",
            Self::RemoveState(_) => "remove_state",
            Self::Reaction(_) => "send_message",
            Self::EndMeeting(_) => "end_conversation",
            Self::Hold(_) => "hold",
            Self::Resume(_) => "resume",
            Self::MeetingStates(_) => "update_meeting_states",
            Self::MeetingSettings(_) => "update_meeting_settings",
        }
    }
}
