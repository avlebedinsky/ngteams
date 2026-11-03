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
                raw: None,
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
                raw: None,
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
            raw: None,
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
            raw: None,
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

#[cfg(test)]
mod tests {
    //! The ids, names and answers here are synthetic: the dialog's shapes with made-up values.

    use teams_fence::{Contact, Policy, Request as FenceRequest, contacting};

    use super::*;

    const TEAM: &str = "19:team0001@thread.tacv2";
    const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";
    const CHANNEL_GROUP: &str = "OID:00000000-0000-4000-8000-0000000000cc@00000000-0000-4000-8000-0000000000ee";

    /// What the fence makes of the bytes `wire` sends, on made-up bases.
    fn classified(wire: &Wire) -> Contact {
        let base = match wire.service() {
            Service::Provisioning => "https://teams.cloud.microsoft/fabric/amer/templates/api",
            _ => "https://teams.cloud.microsoft/api/mt/part/amer-03",
        };
        let mut url = url::Url::parse(&format!("{base}/")).expect("the base");
        url.path_segments_mut()
            .expect("a base")
            .pop_if_empty()
            .extend(wire.endpoint().segments());
        if !wire.endpoint().query().is_empty() {
            url.query_pairs_mut().extend_pairs(wire.endpoint().query());
        }
        let body = wire.body_bytes();
        contacting(
            &FenceRequest::new(wire.method().as_str(), url.as_str()).with_body(body.as_deref()),
            &Policy::new(),
        )
    }

    fn new_channel(kind: NewChannelKind, layout: ChannelLayout, mailbox: Option<&str>) -> NewChannel {
        NewChannel {
            team: TEAM.to_owned(),
            group: GROUP.to_owned(),
            mailbox: mailbox.map(str::to_owned),
            name: "Plans".to_owned(),
            description: "Where plans go".to_owned(),
            kind,
            layout,
        }
    }

    #[test]
    fn each_kind_and_layout_has_its_wire_name() {
        assert_eq!(NewChannelKind::Standard.wire(), "Standard");
        assert_eq!(NewChannelKind::Private.wire(), "Private");
        assert_eq!(ChannelLayout::default(), ChannelLayout::Threads);
        assert_eq!(ChannelLayout::Threads.wire(), "Conversational");
        assert_eq!(ChannelLayout::Posts.wire(), "PostReply");
    }

    #[test]
    fn a_standard_channel_is_made_on_the_middle_tier_for_the_team() {
        let create = CreateChannel::new(new_channel(NewChannelKind::Standard, ChannelLayout::Posts, None));
        let wire = create.wire().expect("a wire form");
        assert_eq!(wire.service(), Service::MiddleTier);
        assert_eq!(wire.method(), Method::Post);
        assert_eq!(wire.endpoint().segments(), ["beta", "teams", TEAM, "channels"]);
        assert!(wire.headers().is_empty());
        assert_eq!(
            wire.body(),
            Some(&json!({
                "displayName": "Plans",
                "description": "Where plans go",
                "groupId": GROUP,
                "channelType": "Standard",
                "chatModalityType": "PostReply",
            }))
        );
        assert_eq!(
            create.target(),
            Target::new(WriteKind::ChannelChange, Scope::Team { id: TEAM.to_owned() })
        );
        assert_eq!(classified(&wire), Contact::Write(create.target()));
    }

    #[test]
    fn a_private_channel_is_made_on_the_provisioning_service_routed_by_the_teams_mailbox() {
        let create = CreateChannel::new(new_channel(
            NewChannelKind::Private,
            ChannelLayout::Threads,
            Some("team@example.com"),
        ));
        let wire = create.wire().expect("a wire form");
        assert_eq!(wire.service(), Service::Provisioning);
        assert_eq!(wire.method(), Method::Post);
        assert_eq!(wire.endpoint().segments(), ["teams", GROUP, "channels"]);
        assert_eq!(wire.endpoint().query(), [("forceSync".to_owned(), "false".to_owned())]);
        assert_eq!(wire.headers(), [("x-anchormailbox", "team@example.com".to_owned())]);
        assert_eq!(
            wire.body(),
            Some(&json!({
                "spaceThreadId": TEAM,
                "displayName": "Plans",
                "description": "Where plans go",
                "channelType": "Private",
                "sharedWithParentTeam": true,
                "chatModalityType": "Conversational",
            }))
        );
        assert_eq!(classified(&wire), Contact::Write(create.target()));
        let unrouted = CreateChannel::new(new_channel(NewChannelKind::Private, ChannelLayout::Threads, None));
        assert!(unrouted.wire().expect("a wire form").headers().is_empty());
    }

    #[test]
    fn a_creations_answer_names_the_thread_or_the_group() {
        let read = |body: Value| CreateChannel::read_answer(body.to_string().as_bytes());
        assert_eq!(
            read(json!({"value": {"objectId": "19:a@thread.tacv2"}})),
            Some(ChannelCreated {
                thread: Some("19:a@thread.tacv2".to_owned()),
                group: None
            })
        );
        assert_eq!(
            read(json!({"groupId": CHANNEL_GROUP, "threadId": "19:b@thread.tacv2"})),
            Some(ChannelCreated {
                thread: Some("19:b@thread.tacv2".to_owned()),
                group: Some(CHANNEL_GROUP.to_owned())
            })
        );
        assert_eq!(
            read(json!({"groupId": CHANNEL_GROUP, "threadId": ""})),
            Some(ChannelCreated {
                thread: None,
                group: Some(CHANNEL_GROUP.to_owned())
            })
        );
        assert_eq!(
            read(json!({"id": "19:c@thread.tacv2"})).and_then(|created| created.thread),
            Some("19:c@thread.tacv2".to_owned())
        );
        assert_eq!(read(json!({"something": "else"})), None);
        assert_eq!(CreateChannel::read_answer(b"not json"), None);
    }

    #[test]
    fn a_private_channels_people_are_named_by_object_id_in_its_tenant() {
        let members = [Mri::new("8:orgid:aaaa"), Mri::new("8:orgid:bbbb")];
        let add = AddChannelMembers::new(GROUP.to_owned(), CHANNEL_GROUP.to_owned(), &members).expect("a request");
        let tenant = "00000000-0000-4000-8000-0000000000ee";
        let wire = add.wire().expect("a wire form");
        assert_eq!(wire.service(), Service::Provisioning);
        assert_eq!(wire.method(), Method::Post);
        assert_eq!(
            wire.endpoint().segments(),
            ["teams", GROUP, "channels", CHANNEL_GROUP, "users"]
        );
        assert_eq!(
            wire.body(),
            Some(&json!({"users": [
                {"id": format!("OID:aaaa@{tenant}"), "role": 1},
                {"id": format!("OID:bbbb@{tenant}"), "role": 1},
            ]}))
        );
        assert_eq!(
            add.target(),
            Target::new(
                WriteKind::MemberChange,
                Scope::Channel {
                    team: GROUP.to_owned(),
                    channel: CHANNEL_GROUP.to_owned(),
                },
            )
            .with_added(members.clone())
        );
        assert_eq!(classified(&wire), Contact::Write(add.target()));
    }

    #[test]
    fn a_private_channels_people_need_a_tenant_somebody_and_work_accounts() {
        let one = [Mri::new("8:orgid:aaaa")];
        assert!(AddChannelMembers::new(GROUP.to_owned(), "no-prefix@tenant".to_owned(), &one).is_none());
        assert!(AddChannelMembers::new(GROUP.to_owned(), "OID:no-tenant".to_owned(), &one).is_none());
        assert!(AddChannelMembers::new(GROUP.to_owned(), "OID:x@".to_owned(), &one).is_none());
        assert!(AddChannelMembers::new(GROUP.to_owned(), CHANNEL_GROUP.to_owned(), &[]).is_none());
        for other in ["28:bot", "8:orgid:", "8:live:someone"] {
            assert!(
                AddChannelMembers::new(GROUP.to_owned(), CHANNEL_GROUP.to_owned(), &[Mri::new(other)]).is_none(),
                "{other}"
            );
        }
    }

    #[test]
    fn the_people_not_added_are_read_by_either_name() {
        let members = [
            Mri::new("8:orgid:aaaa"),
            Mri::new("8:orgid:bbbb"),
            Mri::new("8:orgid:cccc"),
        ];
        let add = AddChannelMembers::new(GROUP.to_owned(), CHANNEL_GROUP.to_owned(), &members).expect("a request");
        let tenant = "00000000-0000-4000-8000-0000000000ee";
        let body = json!({"multipleStatus": [
            {"id": format!("OID:aaaa@{tenant}"), "code": "Failed"},
            {"id": "8:orgid:cccc"},
            {"id": "OID:stranger@elsewhere"},
            {"code": "no id"},
        ]});
        assert_eq!(
            add.failed(body.to_string().as_bytes()),
            [Mri::new("8:orgid:aaaa"), Mri::new("8:orgid:cccc")]
        );
        assert!(add.failed(b"{}").is_empty());
        assert!(add.failed(b"<html>").is_empty());
    }

    #[test]
    fn a_channel_shown_by_default_is_put_on_the_middle_tier() {
        let show = ShowChannelByDefault::new("19:channel@thread.tacv2".to_owned());
        let wire = show.wire().expect("a wire form");
        assert_eq!(wire.service(), Service::MiddleTier);
        assert_eq!(wire.method(), Method::Put);
        assert_eq!(
            wire.endpoint().segments(),
            ["beta", "teams", "19:channel@thread.tacv2", "setDefaultChannel"]
        );
        assert_eq!(wire.body(), None);
        assert_eq!(
            show.target(),
            Target::new(
                WriteKind::ChannelChange,
                Scope::Team {
                    id: "19:channel@thread.tacv2".to_owned()
                }
            )
        );
        assert_eq!(classified(&wire), Contact::Write(show.target()));
    }

    #[test]
    fn an_answers_error_code_is_read_from_each_place_the_bundle_reads() {
        let code = |body: Value| channel_error_code(body.to_string().as_bytes());
        assert_eq!(code(json!({"errorCode": "NameTaken"})).as_deref(), Some("NameTaken"));
        assert_eq!(
            code(json!({"errorCode": "", "errors": [{"errorCode": "TooLong"}]})).as_deref(),
            Some("TooLong")
        );
        assert_eq!(code(json!({"code": "Forbidden"})).as_deref(), Some("Forbidden"));
        assert_eq!(
            code(json!({"error": {"code": "Throttled"}})).as_deref(),
            Some("Throttled")
        );
        assert_eq!(code(json!({"message": "no code"})), None);
        assert_eq!(channel_error_code(b"]"), None);
    }
}
