//! What a write is ([`WriteKind`]) and where it goes ([`Target`]).

use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

/// Every kind of write: anything other people could notice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteKind {
    Message,
    Edit,
    Delete,
    Reaction,
    /// A typing signal: others see it, but it carries nothing to them, so the fence permits it on a chat without a
    /// member read.
    Typing,
    /// A read receipt or a consumption horizon update: like [`WriteKind::Typing`], permitted on a chat without a member
    /// read.
    ReadReceipt,
    /// Placing a call.
    Call,
    /// Answering an incoming call.
    CallAnswer,
    /// What the callee's client sends by itself while an invitation rings ([`Scope::CallOperation`]: permitted under
    /// the lab's call rule, judged on its offered invitation by the call registry's): never one that answers, declines,
    /// joins a call or adds anyone.
    CallRinging,
    /// Declining an offered invitation: the captured rejection of its call leg, so the ringing stops everywhere and the
    /// caller sees it declined; judged like an answer.
    CallDecline,
    /// A captured operation on a call ([`Scope::CallOperation`], other than an `add`, which is an
    /// [`WriteKind::Invite`]), never judged by a member read: permitted under the lab's call rule once the call was let
    /// in, resolved to a call this run's fence registered under the call registry's.
    CallFollowUp,
    Invite,
    /// A calendar event created, changed, cancelled or deleted ([`Scope::New`] for a new one, [`Scope::Event`] for one
    /// that exists): its invitation, update or cancellation reaches every attendee.
    Meeting,
    MemberChange,
    ChatCreate,
    ChannelPost,
    /// A team's channel created, or a channel shown to the whole team by default ([`Scope::Team`]): everyone on the team
    /// can see it.
    ChannelChange,
    /// A new private team ([`Scope::New`]): nobody but the account is in it until people are added.
    TeamCreate,
    /// A new team anyone in the organization can find and join, or that takes everyone in: it never names who it reaches,
    /// so it has no target to judge and the lab's fence denies it.
    OpenTeamCreate,
    /// A team changed for everyone on it ([`Scope::Team`]): its name, its settings, its tags, archived or deleted.
    TeamChange,
    BotReply,
    /// The account's own availability (UPS's presence endpoint `PUT` and the manual status): seen across the tenant,
    /// but it carries nothing to anyone. The fence permits it without a member read, only on [`Scope::Account`] naming
    /// the fence's own account, with nobody added, so that a lab account never sets another's presence.
    OwnAvailability,
}

impl WriteKind {
    pub const ALL: [Self; 22] = [
        Self::Message,
        Self::Edit,
        Self::Delete,
        Self::Reaction,
        Self::Typing,
        Self::ReadReceipt,
        Self::Call,
        Self::CallAnswer,
        Self::CallRinging,
        Self::CallDecline,
        Self::CallFollowUp,
        Self::Invite,
        Self::Meeting,
        Self::MemberChange,
        Self::ChatCreate,
        Self::ChannelPost,
        Self::ChannelChange,
        Self::TeamCreate,
        Self::OpenTeamCreate,
        Self::TeamChange,
        Self::BotReply,
        Self::OwnAvailability,
    ];

    /// The name used in `fence.log`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::Reaction => "reaction",
            Self::Typing => "typing",
            Self::ReadReceipt => "read_receipt",
            Self::Call => "call",
            Self::CallAnswer => "call_answer",
            Self::CallRinging => "call_ringing",
            Self::CallDecline => "call_decline",
            Self::CallFollowUp => "call_follow_up",
            Self::Invite => "invite",
            Self::Meeting => "meeting",
            Self::MemberChange => "member_change",
            Self::ChatCreate => "chat_create",
            Self::ChannelPost => "channel_post",
            Self::ChannelChange => "channel_change",
            Self::TeamCreate => "team_create",
            Self::OpenTeamCreate => "open_team_create",
            Self::TeamChange => "team_change",
            Self::BotReply => "bot_reply",
            Self::OwnAvailability => "own_availability",
        }
    }

    /// Whether this kind of write reaches other people, so its target's members are judged: every kind but typing,
    /// read receipts and the account's own availability, which others may see but which carry nothing to them.
    #[must_use]
    pub fn is_contacting(self) -> bool {
        !matches!(self, Self::Typing | Self::ReadReceipt | Self::OwnAvailability)
    }

    /// Whether this kind of write can create its own target ([`Scope::New`]): a new chat, a new call, a new meeting, a
    /// new private team.
    /// Every other kind lands on something that already exists, whose members the fence has to fetch.
    #[must_use]
    pub fn creates_target(self) -> bool {
        matches!(self, Self::ChatCreate | Self::Call | Self::Meeting | Self::TeamCreate)
    }
}

impl fmt::Display for WriteKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A participant's MRI (`8:orgid:<guid>`, `28:<bot>`, …). Compared exactly: a lookalike in another case or with extra
/// characters is a different participant, so a mismatch can only deny, never allow.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mri(String);

impl Mri {
    #[must_use]
    pub fn new(mri: impl Into<String>) -> Self {
        Self(mri.into())
    }

    /// A participant known by a mail address alone: a calendar event's attendee or organizer, whom Exchange reaches by
    /// address. Kept as `smtp:<address in lower case>`, a form no Teams MRI takes, so it matches only the roster's lab
    /// addresses ([`crate::Roster::admits`]), never an MRI.
    #[must_use]
    pub fn mail(address: &str) -> Self {
        Self(format!("{MAIL_PREFIX}{}", address.trim().to_lowercase()))
    }

    /// The address of a participant made by [`Mri::mail`].
    #[must_use]
    pub fn mail_address(&self) -> Option<&str> {
        self.0.strip_prefix(MAIL_PREFIX)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The prefix of a participant known by a mail address ([`Mri::mail`]).
const MAIL_PREFIX: &str = "smtp:";

impl fmt::Display for Mri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a write lands: the thing whose participants are fetched.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// A chat (1:1, group or meeting chat) by its conversation id.
    Conversation { id: String },
    /// A channel of a team.
    Channel { team: String, channel: String },
    /// A team as a whole (membership, settings).
    Team { id: String },
    /// A call, placed or incoming, by an id: judged by its fetched members, which no source reads yet.
    Call { id: String },
    /// A captured operation on a call: under the lab's call rule judged by its entry (a join by the chat `joins` names,
    /// an answer or a decline by the offered invitation its call leg names, an `add` by its invitees, anything else
    /// permitted); under the call registry's, on a call this run's fence registered (`crate::Fence::pend_start`). `url`
    /// is the exact URL the write goes to, one of the links the call's services handed out for that operation, and
    /// `from` the participant the body names as its sender (`participants.from.id`), for the operations that carry
    /// one, and `callbacks` the endpoint callback links the body hands the call's services (an acknowledgement's
    /// `callAcceptanceAcknowledgement.links`, an `add`'s `links`, an `attach`'s), which under the registry's rule must
    /// be on the call's own endpoint and call agent, and `joins` the conversation an `attach` subscribes to (its one
    /// `join` action's URL) or the group chat a join names. Nothing is fetched but the group chat a join names or an
    /// incoming group call names.
    CallOperation {
        operation: CallOperation,
        url: String,
        // never in `fence.log`, which holds no participant's MRI
        #[serde(skip)]
        from: Option<Mri>,
        // the endpoint's push URLs; `fence.log` keeps the operation and its URL
        #[serde(skip)]
        callbacks: Box<[String]>,
        #[serde(skip)]
        joins: Option<String>,
    },
    /// A calendar event of the account's calendar, by its id (`objectId`): a meeting whose attendees and organizer,
    /// read fresh from the event, are its members (each by address, [`Mri::mail`]).
    Event { id: String },
    /// Something the write creates: a new chat, call or meeting. It has no members to fetch yet: its participants are
    /// exactly the target's added participants. Only kinds that [create a target](WriteKind::creates_target) may use
    /// it; the fence denies any other kind on `New`.
    New,
    /// The account itself, by its MRI: where its own availability lands ([`WriteKind::OwnAvailability`]). It has no
    /// members to fetch: the fence permits a write here only when it is the account's own availability and `mri` is
    /// the fence's own account ([`crate::Fence::own_account`]); it denies any other kind here.
    Account { mri: String },
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversation { id } => write!(f, "conversation {id}"),
            Self::Channel { team, channel } => write!(f, "channel {channel} of team {team}"),
            Self::Team { id } => write!(f, "team {id}"),
            Self::Call { id } => write!(f, "call {id}"),
            Self::Event { id } => write!(f, "event {id}"),
            Self::CallOperation { operation, url, .. } => write!(f, "{operation} on the call at {url}"),
            Self::New => f.write_str("a new target"),
            Self::Account { mri } => write!(f, "account {mri}"),
        }
    }
}

/// A captured operation on an existing call or an offered invitation, each from a capture
/// (`docs/protocol/calling-echo.md`, `docs/protocol/calling.md`). Its method and the link it goes to are fixed: the
/// conversation service's links from the call start's answer, the call controller's from the acceptance delivered for
/// the call, an invitation's from the invitation itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
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
}

impl CallOperation {
    pub const ALL: [Self; 17] = [
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
    ];

    /// The name used in `fence.log`.
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
        }
    }

    /// The method the capture sends it with.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata => "PUT",
            Self::Reject => "DELETE",
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
            | Self::KeepAlive => "POST",
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
            // the conversation's own URL, the call leg's own: no segment after it
            Self::Join | Self::KeepAlive => "",
        }
    }

    /// The kind of write it is: an `add` invites, an `attach` and a `progress` are ring-time sends, an `accept` answers,
    /// a `reject` declines, the rest follow up.
    #[must_use]
    pub fn kind(self) -> WriteKind {
        match self {
            Self::Add => WriteKind::Invite,
            Self::Attach | Self::Progress => WriteKind::CallRinging,
            Self::Accept => WriteKind::CallAnswer,
            Self::Reject => WriteKind::CallDecline,
            Self::Join => WriteKind::Call,
            _ => WriteKind::CallFollowUp,
        }
    }

    /// Whether its body names its sender: the conversation service's operations do (`participants.from`, an endpoint
    /// state's `from`), an `attach` in its `join` action, a `progress` as its `callProgress.sender` and an `accept` as
    /// its `callAcceptance.acceptedBy`.
    #[must_use]
    pub fn names_sender(self) -> bool {
        !matches!(
            self,
            Self::Acknowledge
                | Self::ApplyChannelParameters
                | Self::Reject
                | Self::MediaAcknowledge
                | Self::UpdateMediaDescriptions
                | Self::KeepAlive
        )
    }

    /// Whether it acts on an offered invitation (the callee's side) rather than on a call this fence registered.
    #[must_use]
    pub fn on_invitation(self) -> bool {
        matches!(
            self,
            Self::Attach
                | Self::Progress
                | Self::UpdateEndpointState
                | Self::Accept
                | Self::Reject
                | Self::UpdateParticipantProperties
        )
    }
}

impl fmt::Display for CallOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One write: its kind, its scope, and the participants the write itself adds (invitees, the members of a new chat,
/// the callees of a new call). The fence judges the scope's current members together with the added ones.
///
/// `Debug` and `Display` show the added participants as a count only; [`Target::added`] returns their MRIs. Scope ids
/// are shown as they are, and a 1:1 chat id (`19:<oid>_<oid>@unq.gbl.spaces`) holds both members' object ids.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Target {
    kind: WriteKind,
    scope: Scope,
    added: BTreeSet<Mri>,
    sender: Option<Mri>,
}

impl Target {
    #[must_use]
    pub fn new(kind: WriteKind, scope: Scope) -> Self {
        Self {
            kind,
            scope,
            added: BTreeSet::new(),
            sender: None,
        }
    }

    /// Names who sends the write (a call start's caller, also among [`Target::added`]): the fence admits only a lab
    /// account or an approved service as a sender, never a call guest.
    #[must_use]
    pub fn with_sender(mut self, sender: Mri) -> Self {
        self.sender = Some(sender);
        self
    }

    /// Adds participants that the write brings in.
    #[must_use]
    pub fn with_added(mut self, added: impl IntoIterator<Item = Mri>) -> Self {
        self.added.extend(added);
        self
    }

    #[must_use]
    pub fn kind(&self) -> WriteKind {
        self.kind
    }

    #[must_use]
    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    #[must_use]
    pub fn added(&self) -> &BTreeSet<Mri> {
        &self.added
    }

    /// Who sends the write, when it names one ([`Target::with_sender`]).
    #[must_use]
    pub fn sender(&self) -> Option<&Mri> {
        self.sender.as_ref()
    }
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("kind", &self.kind)
            .field("scope", &self.scope)
            .field("added", &self.added.len())
            .field("sender", &self.sender.is_some())
            .finish()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} on {}", self.kind, self.scope)?;
        if !self.added.is_empty() {
            write!(f, " adding {} participant(s)", self.added.len())?;
        }
        Ok(())
    }
}
