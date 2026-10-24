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
    /// A captured operation on a call this run's fence registered ([`Scope::CallOperation`], other than an `add`, which
    /// is an [`WriteKind::Invite`]): judged by the call registry, never by a member read.
    CallFollowUp,
    Invite,
    Meeting,
    MemberChange,
    ChatCreate,
    ChannelPost,
    BotReply,
    /// The account's own availability (UPS's presence endpoint `PUT` and the manual status): seen across the tenant,
    /// but it carries nothing to anyone. The fence permits it without a member read, only on [`Scope::Account`] naming
    /// the fence's own account, with nobody added, so that a lab account never sets another's presence.
    OwnAvailability,
}

impl WriteKind {
    pub const ALL: [Self; 16] = [
        Self::Message,
        Self::Edit,
        Self::Delete,
        Self::Reaction,
        Self::Typing,
        Self::ReadReceipt,
        Self::Call,
        Self::CallAnswer,
        Self::CallFollowUp,
        Self::Invite,
        Self::Meeting,
        Self::MemberChange,
        Self::ChatCreate,
        Self::ChannelPost,
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
            Self::CallFollowUp => "call_follow_up",
            Self::Invite => "invite",
            Self::Meeting => "meeting",
            Self::MemberChange => "member_change",
            Self::ChatCreate => "chat_create",
            Self::ChannelPost => "channel_post",
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

    /// Whether this kind of write can create its own target ([`Scope::New`]): a new chat, a new call, a new meeting.
    /// Every other kind lands on something that already exists, whose members the fence has to fetch.
    #[must_use]
    pub fn creates_target(self) -> bool {
        matches!(self, Self::ChatCreate | Self::Call | Self::Meeting)
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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

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
    /// A captured operation on a call this run's fence registered (`crate::Fence::pend_start`): `url` is the exact URL
    /// the write goes to, one of the links the call's services handed out for that operation, and `from` the
    /// participant the body names as its sender (`participants.from.id`), for the operations that carry one, and
    /// `callbacks` the endpoint callback links the body hands the call's services (an acknowledgement's
    /// `callAcceptanceAcknowledgement.links`, an `add`'s `links`), each of which must be on the call start's own
    /// endpoint and call agent. The call registry resolves it; nothing else is fetched.
    CallOperation {
        operation: CallOperation,
        url: String,
        // never in `fence.log`, which holds no participant's MRI
        #[serde(skip)]
        from: Option<Mri>,
        // the endpoint's push URLs; `fence.log` keeps the operation and its URL
        #[serde(skip)]
        callbacks: Box<[String]>,
    },
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
            Self::CallOperation { operation, url, .. } => write!(f, "{operation} on the call at {url}"),
            Self::New => f.write_str("a new target"),
            Self::Account { mri } => write!(f, "account {mri}"),
        }
    }
}

/// A captured operation on an existing call, each from the echo call's capture (`docs/protocol/calling-echo.md`). Its
/// method and the link it goes to are fixed: the conversation service's links from the call start's answer, the call
/// controller's from the acceptance delivered for the call.
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
}

impl CallOperation {
    pub const ALL: [Self; 5] = [
        Self::UpdateEndpointMetadata,
        Self::Add,
        Self::Leave,
        Self::Acknowledge,
        Self::ApplyChannelParameters,
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
        }
    }

    /// The method the capture sends it with.
    #[must_use]
    pub fn method(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata => "PUT",
            Self::Add | Self::Leave | Self::Acknowledge | Self::ApplyChannelParameters => "POST",
        }
    }

    /// The last path segment of its link.
    #[must_use]
    pub fn segment(self) -> &'static str {
        match self {
            Self::UpdateEndpointMetadata => "updateEndpointMetadata",
            Self::Add => "add",
            Self::Leave => "leave",
            Self::Acknowledge => "acknowledge",
            Self::ApplyChannelParameters => "applyChannelParameters",
        }
    }

    /// The kind of write it is: an `add` invites, the rest follow up.
    #[must_use]
    pub fn kind(self) -> WriteKind {
        match self {
            Self::Add => WriteKind::Invite,
            _ => WriteKind::CallFollowUp,
        }
    }

    /// Whether its body names its sender (`participants.from`): the conversation service's operations do.
    #[must_use]
    pub fn names_sender(self) -> bool {
        matches!(self, Self::UpdateEndpointMetadata | Self::Add | Self::Leave)
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
}

impl Target {
    #[must_use]
    pub fn new(kind: WriteKind, scope: Scope) -> Self {
        Self {
            kind,
            scope,
            added: BTreeSet::new(),
        }
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
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Target")
            .field("kind", &self.kind)
            .field("scope", &self.scope)
            .field("added", &self.added.len())
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
