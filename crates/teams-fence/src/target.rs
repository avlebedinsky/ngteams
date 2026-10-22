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
    Typing,
    /// A read receipt or a consumption horizon update.
    ReadReceipt,
    /// Placing a call.
    Call,
    /// Answering an incoming call.
    CallAnswer,
    Invite,
    Meeting,
    MemberChange,
    ChatCreate,
    ChannelPost,
    BotReply,
    /// Creating a media service (AMS) object a conversation may read: an image to be sent in it. Its scope is that
    /// conversation, so its readers are judged as the conversation's members.
    MediaObject,
}

impl WriteKind {
    pub const ALL: [Self; 15] = [
        Self::Message,
        Self::Edit,
        Self::Delete,
        Self::Reaction,
        Self::Typing,
        Self::ReadReceipt,
        Self::Call,
        Self::CallAnswer,
        Self::Invite,
        Self::Meeting,
        Self::MemberChange,
        Self::ChatCreate,
        Self::ChannelPost,
        Self::BotReply,
        Self::MediaObject,
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
            Self::Invite => "invite",
            Self::Meeting => "meeting",
            Self::MemberChange => "member_change",
            Self::ChatCreate => "chat_create",
            Self::ChannelPost => "channel_post",
            Self::BotReply => "bot_reply",
            Self::MediaObject => "media_object",
        }
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
    /// A call, placed or incoming.
    Call { id: String },
    /// Something the write creates: a new chat, call or meeting. It has no members to fetch yet: its participants are
    /// exactly the target's added participants. Only kinds that [create a target](WriteKind::creates_target) may use
    /// it; the fence denies any other kind on `New`.
    New,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversation { id } => write!(f, "conversation {id}"),
            Self::Channel { team, channel } => write!(f, "channel {channel} of team {team}"),
            Self::Team { id } => write!(f, "team {id}"),
            Self::Call { id } => write!(f, "call {id}"),
            Self::New => f.write_str("a new target"),
        }
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
