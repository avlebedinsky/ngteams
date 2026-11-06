//! Calling operation names, methods and path segments.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CallOperation {
    /// `PUT …/conv/<id>/updateEndpointMetadata`: the caller's endpoint metadata.
    UpdateEndpointMetadata,
    /// `POST …/conv/<id>/add`: an invitation into the call (the answer's `addParticipantAndModality` link).
    Add,
    /// `POST …/conv/<id>/leave`: the caller leaves.
    Leave,
    /// `POST …/cc/v1/callAcceptance/<call>/…/acknowledge`: the acceptance acknowledged.
    Acknowledge,
    /// `POST …/cc/v1/mcProxy/<call>/…/applyChannelParameters`: a media line's send capabilities.
    ApplyChannelParameters,
    /// `POST …/cc/v1/forked/<call>/…/attach`: the callee's endpoint attached to its ringing call leg, with a
    /// roster-only subscription to the call's conversation (no media, no answer): sent by the callee's client while an
    /// offered invitation rings.
    Attach,
    /// `POST …/cc/v1/incoming/<call>/…/progress`: the callee's endpoint tells the call controller it rings (the link the
    /// permitted `attach`'s answer handed out): sent by the callee's client while an offered invitation rings.
    Progress,
    /// `POST …/conv/<id>/updateEndpointState`: the callee's endpoint state (muted or not) in the call's conversation, at
    /// the link the `attach`'s conversation subscription handed out: sent when the callee answers, and on its call.
    UpdateEndpointState,
    /// `POST …/cc/v1/incoming/<call>/…/accept`: the callee answers (its media answer and its call's callbacks), at the
    /// `acceptance` link the `attach`'s answer handed out.
    Accept,
    /// `DELETE …/cc/v1/incoming/<call>/…/reject`: the callee declines, at the `callLeg` link the `attach`'s answer
    /// handed out.
    Reject,
    /// `POST …/conv/<id>/updateParticipantProperties`: the callee's own participant properties (its AI voice consent)
    /// in the call's conversation, once it answered.
    UpdateParticipantProperties,
    /// `POST …/cc/v1/negotiations/<call>/…/answer`: the callee's answer to a media renegotiation of its answered call,
    /// at the `mediaAnswer` link the renegotiation delivered to its own `mediaRenegotiation` callback handed out.
    MediaAnswer,
    /// `POST …/cc/v1/active/<call>/…/renegotiate`: a media renegotiation of the call (its camera turned on, say), at the
    /// `mediaRenegotiation` link the call controller handed out (the caller's acceptance, the callee's acceptance's
    /// acknowledgement).
    Renegotiate,
    /// `POST …/cc/v1/mediaAnswers/<call>/…/acknowledge`, with no body: the answer to the sender's renegotiation
    /// acknowledged, at the `mediaAcknowledgement` link that answer, delivered to its own `mediaAnswer` callback, handed
    /// out.
    MediaAcknowledge,
    /// `POST …/cc/v1/active/<call>/…/updateMediaDescriptions`: which media lines the sender sends and receives (its
    /// camera on: its main video line `sendrecv`), at the `updateMediaDescriptions` link the call controller handed out.
    UpdateMediaDescriptions,
    /// `POST …/conv/<id>`: a join of a running call's conversation from the chat it belongs to: with a call invitation
    /// (media) the sender enters the call; without one it subscribes to the conversation's roster only (the client does
    /// so by itself once its own ring ended, to show the chat's join). Judged like a start in that chat.
    Join,
    /// `POST …/cc/v1/active/<call>/<n>/<leg>/<n>` with `{"callParticipantUpdate": {}}`: the call's keep-alive, at the
    /// call leg's own `callLeg` link (the caller's acceptance's, the callee's acceptance's acknowledgement's), sent
    /// every 90 % of the call's `callKeepAliveInterval` (seen 2026-10-25, `ab-call --call keep-alive`).
    KeepAlive,
    /// `POST …/conv/<id>/notificationLinks`: the sender's own callbacks for a conversation it subscribed to or entered
    /// (its roster, its updates, what admitting and adding report), as a meeting's pre-join hands them once its
    /// Trouter socket is up (seen 2026-11-02, `ab-meeting-join`). Names the sender, adds nobody.
    NotificationLinks,
    /// `POST …/conv/<id>/admit`: the sender lets the people of `to` in from the meeting's lobby, at the meeting
    /// conversation's `admit` link (seen 2026-11-03, `meeting-steps`: the organizer's Admit). Adds them.
    Admit,
    /// `POST …/conv/<id>/removeParticipant`: the sender takes the people of `to` out of the meeting's conversation, at
    /// its `removeParticipant` link: what the organizer's Deny sends for someone waiting in the lobby (seen 2026-11-03,
    /// `meeting-steps`). Names them.
    RemoveParticipant,
    /// `POST …/conv/<id>/publishState`: the sender's raised hand (`stateType` `raiseHands`) published in the call's
    /// conversation, at its `publishState` link (seen 2026-11-06, `meeting-steps`: Raise). Names nobody else.
    PublishState,
    /// `POST …/conv/<id>/removeState`: a state the sender published taken back by its id (its hand lowered), at the
    /// conversation's `removeState` link (seen 2026-11-06, `meeting-steps`: Lower hand). Names nobody else.
    RemoveState,
    /// `POST …/conv/<id>/sendMessage`: a live reaction (Like, Love, Applause, Laugh, Surprised) to everyone in the call,
    /// at the conversation's `sendMessage` link (seen 2026-11-06, `meeting-steps`: React). Reaches only who is in it.
    SendMessage,
    /// `POST …/cc/v1/active/<call>/…/hold`: the sender puts its 1:1 call on hold (`holdType` `musicOnHold`: the far
    /// end hears the service's music), at the call leg's `hold` link (seen 2026-11-06, `ab-call --call steps`: Hold).
    /// Names nobody else.
    Hold,
    /// `POST …/cc/v1/holdSession/<call>/…/resume`: the sender takes its call off hold, at the `resume` link the hold's
    /// answer handed out (seen 2026-11-06, `ab-call --call steps`: Resume). Names nobody else.
    Resume,
    /// `DELETE …/conv/<id>`: the organizer ends the meeting for everyone in it, at the conversation's own URL (seen
    /// 2026-11-06, `meeting-steps`: End meeting). Reaches only who is in it.
    EndConversation,
    /// `POST …/conv/<id>/updateMeetingStates`: a change of the meeting's shared states (its timer), at the
    /// conversation's `updateMeetingStates` link (seen 2026-11-06, `ab-call --call steps`: Timer). Names nobody else.
    UpdateMeetingStates,
    /// `PUT …/conv/<id>/updateMeetingSettings`: the organizer locks or unlocks the meeting, at the conversation's
    /// `updateMeetingSettings` link (seen 2026-11-06, `meeting-steps`: Lock the meeting). Names nobody else.
    UpdateMeetingSettings,
}

impl CallOperation {
    pub const ALL: [Self; 28] = [
        Self::UpdateEndpointMetadata,
        Self::Add,
        Self::Leave,
        Self::Acknowledge,
        Self::ApplyChannelParameters,
        Self::Attach,
        Self::Progress,
        Self::UpdateEndpointState,
        Self::Accept,
        Self::Reject,
        Self::UpdateParticipantProperties,
        Self::MediaAnswer,
        Self::Renegotiate,
        Self::MediaAcknowledge,
        Self::UpdateMediaDescriptions,
        Self::Join,
        Self::KeepAlive,
        Self::NotificationLinks,
        Self::Admit,
        Self::RemoveParticipant,
        Self::PublishState,
        Self::RemoveState,
        Self::SendMessage,
        Self::EndConversation,
        Self::Hold,
        Self::Resume,
        Self::UpdateMeetingStates,
        Self::UpdateMeetingSettings,
    ];

    /// The protocol operation name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata => "update_endpoint_metadata",
            Self::Add => "add",
            Self::Leave => "leave",
            Self::Acknowledge => "acknowledge",
            Self::ApplyChannelParameters => "apply_channel_parameters",
            Self::Attach => "attach",
            Self::Progress => "progress",
            Self::UpdateEndpointState => "update_endpoint_state",
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::UpdateParticipantProperties => "update_participant_properties",
            Self::MediaAnswer => "media_answer",
            Self::Renegotiate => "renegotiate",
            Self::MediaAcknowledge => "media_acknowledge",
            Self::UpdateMediaDescriptions => "update_media_descriptions",
            Self::Join => "join",
            Self::KeepAlive => "keep_alive",
            Self::NotificationLinks => "notification_links",
            Self::Admit => "admit",
            Self::RemoveParticipant => "remove_participant",
            Self::PublishState => "publish_state",
            Self::RemoveState => "remove_state",
            Self::SendMessage => "send_message",
            Self::EndConversation => "end_conversation",
            Self::Hold => "hold",
            Self::Resume => "resume",
            Self::UpdateMeetingStates => "update_meeting_states",
            Self::UpdateMeetingSettings => "update_meeting_settings",
        }
    }

    /// The method the capture sends it with.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata | Self::UpdateMeetingSettings => "PUT",
            Self::Reject | Self::EndConversation => "DELETE",
            Self::Add
            | Self::Leave
            | Self::Acknowledge
            | Self::ApplyChannelParameters
            | Self::Attach
            | Self::Progress
            | Self::UpdateEndpointState
            | Self::Accept
            | Self::UpdateParticipantProperties
            | Self::MediaAnswer
            | Self::Renegotiate
            | Self::MediaAcknowledge
            | Self::UpdateMediaDescriptions
            | Self::Join
            | Self::KeepAlive
            | Self::NotificationLinks
            | Self::Admit
            | Self::RemoveParticipant
            | Self::PublishState
            | Self::RemoveState
            | Self::SendMessage
            | Self::Hold
            | Self::Resume
            | Self::UpdateMeetingStates => "POST",
        }
    }

    /// The last path segment of its link.
    #[must_use]
    pub fn segment(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata => "updateEndpointMetadata",
            Self::Add => "add",
            Self::Leave => "leave",
            Self::Acknowledge | Self::MediaAcknowledge => "acknowledge",
            Self::ApplyChannelParameters => "applyChannelParameters",
            Self::Attach => "attach",
            Self::Progress => "progress",
            Self::UpdateEndpointState => "updateEndpointState",
            Self::Accept => "accept",
            Self::Reject => "reject",
            Self::UpdateParticipantProperties => "updateParticipantProperties",
            Self::MediaAnswer => "answer",
            Self::Renegotiate => "renegotiate",
            Self::UpdateMediaDescriptions => "updateMediaDescriptions",
            Self::NotificationLinks => "notificationLinks",
            Self::Admit => "admit",
            Self::RemoveParticipant => "removeParticipant",
            Self::PublishState => "publishState",
            Self::RemoveState => "removeState",
            Self::SendMessage => "sendMessage",
            Self::Hold => "hold",
            Self::Resume => "resume",
            Self::UpdateMeetingStates => "updateMeetingStates",
            Self::UpdateMeetingSettings => "updateMeetingSettings",
            // the conversation's own URL, the call leg's own: no segment after it
            Self::Join | Self::KeepAlive | Self::EndConversation => "",
        }
    }
}

impl std::fmt::Display for CallOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
