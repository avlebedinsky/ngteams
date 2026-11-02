//! A team managed (`docs/protocol/teams.md`), as the web client's team pages send it (`proto-lab run team-manage`): a
//! new team on the middle tier, its name and description, its settings on the chat service's thread, its tags on CSA,
//! its code, its people, archived and restored on the provisioning service, deleted, joined and left. Each is a sealed
//! [`Mutating`] request whose target is the team: `teams_fence::contacting` reads the same bytes to the same target.
//! What changes only the account's own view of a team (its channels hidden or shown) reaches nobody and is a plain
//! [`crate::Request`] (`team_read`).

use serde_json::{Value, json};
use teams_fence::{Mri, Scope, Target, WriteKind};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::endpoint::{Endpoint, Method, Service};

/// A new team, as the "Create a team" dialog makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTeam {
    pub name: String,
    pub description: String,
    /// The name of its first channel.
    pub first_channel: String,
    /// Anyone in the organization can join it; a private team's people need permission.
    pub public: bool,
}

/// `accessType` of a private team, as the web client's creation sends it.
pub const TEAM_PRIVATE: u64 = 1;

/// `accessType` of a public team (the bundle's list of access types: `None, Private, Secret, Public, …`).
pub const TEAM_PUBLIC: u64 = 3;

/// A person's role in a team.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TeamRole {
    #[default]
    Member,
    Owner,
}

impl TeamRole {
    /// Its `role` in a body adding people.
    #[must_use]
    pub fn wire(self) -> u64 {
        match self {
            Self::Member => 0,
            Self::Owner => 1,
        }
    }
}

/// What happens to someone already on a team: a body's `updateType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MemberUpdate {
    /// A member made an owner.
    Promoted,
    /// An owner made a member.
    Demoted,
    /// Someone taken off the team by an owner.
    Removed,
    /// The account leaving, as a member.
    Left,
    /// The account leaving, as an owner.
    DemotedAndLeft,
}

impl MemberUpdate {
    /// Its `updateType`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::Promoted => "Promoted",
            Self::Demoted => "Demoted",
            Self::Removed => "Removed",
            Self::Left => "Left",
            Self::DemotedAndLeft => "DemotedAndLeft",
        }
    }
}

/// A write that manages a team (see the module docs). `team` is its thread (its general channel's id), `group` its
/// Microsoft 365 group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TeamWrite {
    /// A new team.
    Create(NewTeam),
    /// A team's name, description and privacy; a name or a privacy left out stays as it is.
    Edit {
        team: String,
        name: Option<String>,
        description: String,
        public: Option<bool>,
    },
    /// A team's settings, the whole `spaceAdminSettings` thread property as text.
    Settings { team: String, settings: String },
    /// Who may change a team's tags, and whether apps apply them.
    TagSettings {
        team: String,
        owners_only: bool,
        automatic: bool,
    },
    /// A new tag, with the people who have it.
    CreateTag {
        team: String,
        name: String,
        description: String,
        members: Vec<Mri>,
    },
    /// A tag's name and description; `version` is the version it was read at.
    EditTag {
        team: String,
        tag: String,
        version: u64,
        name: String,
        description: String,
    },
    /// A tag deleted.
    DeleteTag { team: String, tag: String },
    /// A team archived, its site read-only for its members when asked.
    Archive { group: String, site_read_only: bool },
    /// An archived team restored.
    Restore { group: String },
    /// A team deleted.
    Delete { team: String },
    /// People added to a team, each with a role.
    AddMembers {
        team: String,
        group: String,
        members: Vec<(Mri, TeamRole)>,
    },
    /// A member of a private team asking its owners to let people in.
    RequestMembers { group: String, members: Vec<Mri> },
    /// Someone on a team changed, or gone.
    UpdateMember {
        team: String,
        group: String,
        member: Mri,
        update: MemberUpdate,
    },
    /// A team's code made, or made anew.
    ResetCode { team: String },
    /// A team's code taken away.
    RemoveCode { team: String },
    /// A public team joined, or a private one asked to join.
    Join { group: String },
    /// A team joined by its code.
    JoinByCode { code: String },
    /// Someone's request to join let in or turned away.
    AnswerJoinRequest { group: String, member: Mri, accept: bool },
}

impl TeamWrite {
    fn thread(team: &str) -> Scope {
        Scope::Team { id: team.to_owned() }
    }
}

impl Mutating for TeamWrite {
    fn target(&self) -> Target {
        match self {
            Self::Create(team) if team.public => Target::new(WriteKind::OpenTeamCreate, Scope::New),
            Self::Create(_) => Target::new(WriteKind::TeamCreate, Scope::New),
            Self::Edit { team, .. }
            | Self::Settings { team, .. }
            | Self::TagSettings { team, .. }
            | Self::EditTag { team, .. }
            | Self::DeleteTag { team, .. }
            | Self::Delete { team }
            | Self::ResetCode { team }
            | Self::RemoveCode { team } => Target::new(WriteKind::TeamChange, Self::thread(team)),
            Self::CreateTag { team, members, .. } => {
                Target::new(WriteKind::TeamChange, Self::thread(team)).with_added(members.iter().cloned())
            }
            Self::Archive { group, .. } | Self::Restore { group } => {
                Target::new(WriteKind::TeamChange, Self::thread(group))
            }
            Self::AddMembers { team, members, .. } => Target::new(WriteKind::MemberChange, Self::thread(team))
                .with_added(members.iter().map(|(member, _)| member.clone())),
            Self::RequestMembers { group, members } => {
                Target::new(WriteKind::MemberChange, Self::thread(group)).with_added(members.iter().cloned())
            }
            Self::UpdateMember { team, .. } => Target::new(WriteKind::MemberChange, Self::thread(team)),
            Self::Join { group } => Target::new(WriteKind::MemberChange, Self::thread(group)),
            Self::JoinByCode { code } => Target::new(WriteKind::MemberChange, Self::thread(code)),
            Self::AnswerJoinRequest { group, member, .. } => {
                Target::new(WriteKind::MemberChange, Self::thread(group)).with_added([member.clone()])
            }
        }
    }
}

impl TeamWrite {
    /// The wire of a write on the team itself.
    fn details_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::Create(team) => create_wire(team),
            Self::Edit {
                team,
                name,
                description,
                public,
            } => {
                let mut body = json!({
                    "description": description,
                    "displayNamePrefix": "",
                    "displayNameSuffix": "",
                    "validationRequired": false,
                });
                if let Some(name) = name {
                    body["displayName"] = json!(name);
                }
                if let Some(public) = public {
                    body["accessType"] = json!(access_type(*public));
                }
                middle_tier(Method::Patch, ["beta", "teams", team.as_str()], Some(body))
            }
            Self::Settings { team, settings } => Wire {
                service: Service::ChatService,
                method: Method::Put,
                endpoint: Endpoint::new(["v1", "threads", team.as_str(), "properties"])
                    .with_query("name", "spaceAdminSettings"),
                headers: Vec::new(),
                body: Some(json!({ "spaceAdminSettings": settings })),
            },
            Self::Archive { group, site_read_only } => {
                let endpoint = Endpoint::new(["groups", group.as_str(), "archive"]);
                let endpoint = if *site_read_only {
                    endpoint.with_query("setSpoSiteReadOnlyForMembers", "true")
                } else {
                    endpoint
                };
                provisioning(endpoint)
            }
            Self::Restore { group } => provisioning(Endpoint::new(["groups", group.as_str(), "unarchive"])),
            Self::Delete { team } => middle_tier(Method::Delete, ["beta", "teams", team.as_str(), "delete"], None),
            Self::ResetCode { team } => middle_tier(Method::Put, ["beta", "teams", team.as_str(), "joinCode"], None),
            Self::RemoveCode { team } => {
                middle_tier(Method::Delete, ["beta", "teams", team.as_str(), "joinCode"], None)
            }
            _ => return None,
        })
    }

    /// The wire of a write on its tags.
    fn tags_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::TagSettings {
                team,
                owners_only,
                automatic,
            } => csa(
                Method::Put,
                [team.as_str(), "tagSettings"].to_vec(),
                Some(json!({
                    "onlyOwnersCanUpdate": owners_only,
                    "disallowedSources": [],
                    "enableAutomaticTags": automatic,
                })),
                None,
            ),
            Self::CreateTag {
                team,
                name,
                description,
                members,
            } => csa(
                Method::Post,
                [team.as_str(), "tags"].to_vec(),
                Some(json!({
                    "tagName": name,
                    "tagDescription": description,
                    "memberIds": members.iter().map(Mri::as_str).collect::<Vec<_>>(),
                })),
                None,
            ),
            Self::EditTag {
                team,
                tag,
                version,
                name,
                description,
            } => csa(
                Method::Patch,
                [team.as_str(), "tags", tag.as_str()].to_vec(),
                Some(json!({ "tagName": name, "tagDescription": description })),
                Some(*version),
            ),
            Self::DeleteTag { team, tag } => csa(
                Method::Delete,
                [team.as_str(), "tags", tag.as_str()].to_vec(),
                None,
                None,
            ),
            _ => return None,
        })
    }

    /// The wire of a write on its people.
    fn people_wire(&self) -> Option<Wire> {
        Some(match self {
            Self::AddMembers { team, group, members } => {
                let users: Vec<Value> = members
                    .iter()
                    .map(|(member, role)| json!({ "mri": member.as_str(), "role": role.wire() }))
                    .collect();
                Wire {
                    service: Service::MiddleTier,
                    method: Method::Put,
                    endpoint: Endpoint::new(["beta", "teams", team.as_str(), "bulkUpdateRoledMembers"])
                        .with_query("allowAsyncAddition", "true"),
                    headers: Vec::new(),
                    body: Some(json!({ "users": users, "groupId": group })),
                }
            }
            Self::RequestMembers { group, members } => middle_tier(
                Method::Post,
                ["beta", "teams", "sponsorJoinRequest", group.as_str()],
                Some(json!(members.iter().map(Mri::as_str).collect::<Vec<_>>())),
            ),
            Self::UpdateMember {
                team,
                group,
                member,
                update,
            } => middle_tier(
                Method::Put,
                ["beta", "teams", team.as_str(), "members"],
                Some(json!({ "userMri": member.as_str(), "updateType": update.wire(), "groupId": group })),
            ),
            Self::Join { group } => middle_tier(Method::Post, ["beta", "teams", "joinRequest", group.as_str()], None),
            Self::JoinByCode { code } => Wire {
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", "joinByCode"])
                    .with_query("restrictJoinToPublicTeam", "false"),
                headers: Vec::new(),
                body: Some(json!(code)),
            },
            Self::AnswerJoinRequest { group, member, accept } => Wire {
                service: Service::MiddleTier,
                method: Method::Post,
                endpoint: Endpoint::new(["beta", "teams", "joinRequestAdminAction"])
                    .with_query("silentAction", "false"),
                headers: Vec::new(),
                body: Some(json!({
                    "GroupId": group,
                    "UserMri": member.as_str(),
                    "ActionType": if *accept { "Accept" } else { "Reject" },
                })),
            },
            _ => return None,
        })
    }
}

impl Sealed for TeamWrite {
    fn wire(&self) -> Option<Wire> {
        self.details_wire()
            .or_else(|| self.tags_wire())
            .or_else(|| self.people_wire())
    }
}

fn access_type(public: bool) -> u64 {
    if public { TEAM_PUBLIC } else { TEAM_PRIVATE }
}

fn create_wire(team: &NewTeam) -> Wire {
    Wire {
        service: Service::MiddleTier,
        method: Method::Post,
        endpoint: Endpoint::new(["beta", "teams", "create"]),
        headers: Vec::new(),
        body: Some(json!({
            "accessType": access_type(team.public),
            "description": team.description,
            "displayName": team.name,
            "displayNamePrefix": "",
            "displayNameSuffix": "",
            "isTenantWide": false,
            "sensitivityLabelId": null,
            "topicThreadTopic": team.first_channel,
            "validationRequired": false,
        })),
    }
}

fn middle_tier<const N: usize>(method: Method, path: [&str; N], body: Option<Value>) -> Wire {
    Wire {
        service: Service::MiddleTier,
        method,
        endpoint: Endpoint::new(path),
        headers: Vec::new(),
        body,
    }
}

fn provisioning(endpoint: Endpoint) -> Wire {
    Wire {
        service: Service::Provisioning,
        method: Method::Post,
        endpoint,
        headers: Vec::new(),
        body: None,
    }
}

/// On CSA, under `api/v1/teams/`; `version` goes as the `if-match` a tag's change needs.
fn csa(method: Method, path: Vec<&str>, body: Option<Value>, version: Option<u64>) -> Wire {
    let mut segments = vec!["api", "v1", "teams"];
    segments.extend(path);
    Wire {
        service: Service::Csa,
        method,
        endpoint: Endpoint::new(segments),
        headers: version
            .into_iter()
            .map(|version| ("if-match", version.to_string()))
            .collect(),
        body,
    }
}

/// What a new team's answer says: its thread and its group, `{"value": {"objectId", "siteInfo": {"groupId"}}}`.
#[must_use]
pub fn team_created(body: &[u8]) -> Option<(String, Option<String>)> {
    let answer: Value = serde_json::from_slice(body).ok()?;
    let value = answer.get("value")?;
    let thread = value
        .get("objectId")
        .or_else(|| value.get("skypeThreadId"))?
        .as_str()
        .filter(|thread| !thread.is_empty())?
        .to_owned();
    let group = value
        .pointer("/siteInfo/groupId")
        .and_then(Value::as_str)
        .filter(|group| !group.is_empty())
        .map(str::to_owned);
    Some((thread, group))
}

/// The error code a team's write answer names, as [`crate::channel_error_code`] reads a channel's.
#[must_use]
pub fn team_error_code(body: &[u8]) -> Option<String> {
    super::channel::channel_error_code(body)
}
