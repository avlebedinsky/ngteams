//! A team's new channel (`docs/protocol/channels.md`, "Creating a channel"), as the web client's "Create a channel"
//! dialog sends it (`proto-lab run channel-create`, held back): a standard channel on the middle tier
//! ([`Service::MiddleTier`]) and a private one on the teams' provisioning service ([`Service::Provisioning`]), the people
//! a private channel starts with ([`AddChannelMembers`]), and a standard channel shown to the whole team
//! ([`ShowChannelByDefault`]). Each is a sealed [`Mutating`] request whose target is the team or the channel:
//! `teams_fence::contacting` reads the same bytes to the same target.

use serde_json::{Value, json};
use teams_fence::{Mri, Scope, Target, WriteKind};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::endpoint::{Endpoint, Method, Service};

/// Who a new channel is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NewChannelKind {
    /// Everyone on the team.
    Standard,
    /// Specific people on the team.
    Private,
}

impl NewChannelKind {
    /// Its `channelType`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Private => "Private",
        }
    }
}

/// How a new channel's messages are laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// Looks like chat with replies on the side in threads.
    #[default]
    Threads,
    /// Posts reorder by most recent reply.
    Posts,
}

impl ChannelLayout {
    /// Its `chatModalityType`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Threads => "Conversational",
            Self::Posts => "PostReply",
        }
    }
}

/// A new channel, as the dialog makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChannel {
    /// The team's thread id (its general channel's: `19:…@thread.tacv2`).
    pub team: String,
    /// The team's Microsoft 365 group.
    pub group: String,
    /// The team's mail address (`smtpAddress`), when known: a private channel's request routes by it.
    pub mailbox: Option<String>,
    pub name: String,
    pub description: String,
    pub kind: NewChannelKind,
    pub layout: ChannelLayout,
}

/// The creation of a [`NewChannel`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreateChannel {
    channel: NewChannel,
}

/// What a creation's answer says: the channel's thread, which a private channel may not have yet, and a private
/// channel's group (`OID:<id>@<tenant>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelCreated {
    pub thread: Option<String>,
    pub group: Option<String>,
}

impl CreateChannel {
    #[must_use]
    pub fn new(channel: NewChannel) -> Self {
        Self { channel }
    }

    /// What its answer `body` says: the middle tier's `{"value": {"objectId": <thread>}}`, the provisioning service's
    /// `{"groupId", "threadId"}`, or `{"id": <thread>}` (the bundle's other form of a standard channel). `None` for any
    /// other answer: the channel may exist, and the Teams list tells when it comes.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<ChannelCreated> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let created = ChannelCreated {
            thread: text(answer.pointer("/value/objectId"))
                .or_else(|| text(answer.get("threadId")))
                .or_else(|| text(answer.get("id"))),
            group: text(answer.get("groupId")),
        };
        (created.thread.is_some() || created.group.is_some()).then_some(created)
    }
}

impl Mutating for CreateChannel {
    fn target(&self) -> Target {
        Target::new(
            WriteKind::ChannelChange,
            Scope::Team {
                id: self.channel.team.clone(),
            },
        )
    }
}

impl Sealed for CreateChannel {
    fn wire(&self) -> Option<Wire> {
        let channel = &self.channel;
        Some(match channel.kind {
            NewChannelKind::Standard => Wire {
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", channel.team.as_str(), "channels"]),
                headers: Vec::new(),
                body: Some(json!({
                    "displayName": channel.name,
                    "description": channel.description,
                    "groupId": channel.group,
                    "channelType": channel.kind.wire(),
                    "chatModalityType": channel.layout.wire(),
                })),
            },
            NewChannelKind::Private => Wire {
                service: Service::Provisioning,
                method: Method::Post,
                endpoint: Endpoint::new(["teams", channel.group.as_str(), "channels"]).with_query("forceSync", "false"),
                headers: channel
                    .mailbox
                    .iter()
                    .map(|mailbox| ("x-anchormailbox", mailbox.clone()))
                    .collect(),
                body: Some(json!({
                    "spaceThreadId": channel.team,
                    "displayName": channel.name,
                    "description": channel.description,
                    "channelType": channel.kind.wire(),
                    "sharedWithParentTeam": true,
                    "chatModalityType": channel.layout.wire(),
                })),
            },
        })
    }
}

/// The people a private channel starts with, as its members: `POST teams/<team group>/channels/<channel group>/users`
/// on the provisioning service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddChannelMembers {
    team_group: String,
    channel_group: String,
    members: Vec<(Mri, String)>,
}

/// A member's `role` in [`AddChannelMembers`]' body, as the bundle's add-people dialog sends a member (not an owner).
const MEMBER_ROLE: u64 = 1;

impl AddChannelMembers {
    /// The request adding `members` (work accounts' MRIs, `8:orgid:<object id>`) to the channel `channel_group`
    /// (`OID:<id>@<tenant>`) of the team's group `team_group`; `None` when the channel's id names no tenant, when there
    /// is nobody, or for anyone else's MRI.
    #[must_use]
    pub fn new(team_group: String, channel_group: String, members: &[Mri]) -> Option<Self> {
        let (_, tenant) = channel_group.strip_prefix("OID:")?.split_once('@')?;
        if tenant.is_empty() || members.is_empty() {
            return None;
        }
        let members = members
            .iter()
            .map(|member| {
                let oid = member.as_str().strip_prefix("8:orgid:").filter(|oid| !oid.is_empty())?;
                Some((member.clone(), format!("OID:{oid}@{tenant}")))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            team_group,
            channel_group,
            members,
        })
    }

    /// The MRIs its answer `body` says weren't added (`multipleStatus`, one entry per failure, by `id`: the people's
    /// object ids, as the request named them), each as the request named them.
    #[must_use]
    pub fn failed(&self, body: &[u8]) -> Vec<Mri> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let failures = answer.get("multipleStatus").and_then(Value::as_array);
        failures
            .into_iter()
            .flatten()
            .filter_map(|failure| failure.get("id").and_then(Value::as_str))
            .filter_map(|id| {
                self.members
                    .iter()
                    .find(|(mri, named)| named.as_str() == id || mri.as_str() == id)
                    .map(|(mri, _)| mri.clone())
            })
            .collect()
    }
}

impl Mutating for AddChannelMembers {
    fn target(&self) -> Target {
        let added: Vec<Mri> = self
            .members
            .iter()
            .filter_map(|(_, named)| {
                let (oid, _) = named.strip_prefix("OID:")?.split_once('@')?;
                Some(Mri::new(format!("8:orgid:{oid}")))
            })
            .collect();
        Target::new(
            WriteKind::MemberChange,
            Scope::Channel {
                team: self.team_group.clone(),
                channel: self.channel_group.clone(),
            },
        )
        .with_added(added)
    }
}

impl Sealed for AddChannelMembers {
    fn wire(&self) -> Option<Wire> {
        let users: Vec<Value> = self
            .members
            .iter()
            .map(|(_, named)| json!({ "id": named, "role": MEMBER_ROLE }))
            .collect();
        Some(Wire {
            service: Service::Provisioning,
            method: Method::Post,
            endpoint: Endpoint::new([
                "teams",
                self.team_group.as_str(),
                "channels",
                self.channel_group.as_str(),
                "users",
            ])
            .with_query("forceSync", "false"),
            headers: Vec::new(),
            body: Some(json!({ "users": users })),
        })
    }
}

/// A channel shown in everyone's channel list: `PUT beta/teams/<channel>/setDefaultChannel` on the middle tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShowChannelByDefault {
    channel: String,
}

impl ShowChannelByDefault {
    #[must_use]
    pub fn new(channel: String) -> Self {
        Self { channel }
    }
}

impl Mutating for ShowChannelByDefault {
    fn target(&self) -> Target {
        Target::new(
            WriteKind::ChannelChange,
            Scope::Team {
                id: self.channel.clone(),
            },
        )
    }
}

impl Sealed for ShowChannelByDefault {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            service: Service::MiddleTier,
            method: Method::Put,
            endpoint: Endpoint::new(["beta", "teams", self.channel.as_str(), "setDefaultChannel"]),
            headers: Vec::new(),
            body: None,
        })
    }
}

/// The error code an answer's `body` names: `errorCode`, or the first of `errors`' (the bundle's reading of the middle
/// tier's answers), or `code`, or `error.code`.
#[must_use]
pub fn channel_error_code(body: &[u8]) -> Option<String> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    text(answer.get("errorCode"))
        .or_else(|| text(answer.pointer("/errors/0/errorCode")))
        .or_else(|| text(answer.get("code")))
        .or_else(|| text(answer.pointer("/error/code")))
}
