//! Channel management requests and account-owned notification preferences, as the Teams web client sends them.
//! The owning team and the channel's type stay together so private/shared changes use their provisioning route.

use serde_json::{Value, json};
use teams_fence::{Scope, Target, WriteKind};

use super::sealed::Sealed;
use super::{Mutating, Wire};
use crate::{Endpoint, Method, Read, Service};

/// A channel and the service identities needed to manage it. Display names never identify a request target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelTarget {
    pub team: String,
    pub team_group: String,
    pub channel: String,
    /// `OID:<substrate group>@<tenant>` for a private/shared channel; absent for a standard channel.
    pub channel_group: Option<String>,
    pub mailbox: Option<String>,
}

/// A channel change. Own notification preferences and list placement use separate account-owned requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelWrite {
    pub target: ChannelTarget,
    pub action: ChannelChange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelChange {
    Edit {
        name: String,
        description: String,
    },
    /// The complete captured settings object, preserving fields this build doesn't expose.
    Settings(Value),
    Archive {
        archived: bool,
        site_read_only: bool,
    },
    Delete,
    CreateEmail,
    UpdateEmail {
        allowed_sender_type: String,
        allowed_domains: Vec<String>,
    },
    RemoveEmail,
    AddMember {
        mri: String,
        owner: bool,
    },
    UpdateMember {
        mri: String,
        owner: bool,
    },
    RemoveMember {
        mri: String,
    },
    Moderator {
        mri: String,
        moderator: bool,
    },
}

impl Mutating for ChannelWrite {
    fn target(&self) -> Target {
        let kind = if matches!(
            self.action,
            ChannelChange::AddMember { .. }
                | ChannelChange::UpdateMember { .. }
                | ChannelChange::RemoveMember { .. }
                | ChannelChange::Moderator { .. }
        ) {
            WriteKind::MemberChange
        } else {
            WriteKind::ChannelChange
        };
        let target = Target::new(
            kind,
            Scope::Team {
                id: self.target.team.clone(),
            },
        );
        if let ChannelChange::AddMember { mri, .. } = &self.action {
            target.with_added([teams_fence::Mri::new(mri)])
        } else {
            target
        }
    }
}

impl ChannelWrite {
    fn email_wire(&self) -> Option<Wire> {
        let (method, body) = match &self.action {
            ChannelChange::CreateEmail => (
                Method::Post,
                Some(json!({"allowedDomains":null,"allowedSenderType":"members"})),
            ),
            ChannelChange::UpdateEmail {
                allowed_sender_type,
                allowed_domains,
            } => (
                Method::Put,
                Some(
                    json!({"allowedSenderType":allowed_sender_type,"allowedDomains":if allowed_domains.is_empty(){Value::Null}else{json!(allowed_domains)}}),
                ),
            ),
            ChannelChange::RemoveEmail => (Method::Delete, None),
            _ => return None,
        };
        Some(Wire {
            raw: None,
            service: Service::MiddleTier,
            method,
            endpoint: Endpoint::new(["beta", "channels", self.target.channel.as_str(), "email"]),
            headers: Vec::new(),
            body,
        })
    }

    fn member_wire(&self) -> Option<Wire> {
        let target = &self.target;
        let (mri, owner, remove, add) = match &self.action {
            ChannelChange::AddMember { mri, owner } => (mri, *owner, false, true),
            ChannelChange::UpdateMember { mri, owner } => (mri, *owner, false, false),
            ChannelChange::RemoveMember { mri } => (mri, false, true, false),
            ChannelChange::Moderator { mri, moderator } => {
                let object = crate::object_id(mri)?;
                return Some(Wire {
                    raw: None,
                    service: Service::Provisioning,
                    method: Method::Put,
                    endpoint: Endpoint::new(["channels", target.channel.as_str(), "members"]),
                    headers: Vec::new(),
                    body: Some(json!({ "users": [{ "id": object, "isModerator": moderator }] })),
                });
            }
            _ => return None,
        };
        let object = crate::object_id(mri)?;
        let group = target.channel_group.as_deref()?;
        let (_, tenant) = group.strip_prefix("OID:")?.split_once('@')?;
        if tenant.is_empty() {
            return None;
        }
        let user = format!("OID:{object}@{tenant}");
        let mut endpoint = Endpoint::new(["teams", target.team_group.as_str(), "channels", group, "users"]);
        if remove {
            endpoint = Endpoint::new([
                "teams",
                target.team_group.as_str(),
                "channels",
                group,
                "users",
                user.as_str(),
            ]);
        }
        Some(Wire {
            raw: None,
            service: Service::Provisioning,
            method: if remove {
                Method::Delete
            } else if add {
                Method::Post
            } else {
                Method::Patch
            },
            endpoint: endpoint.with_query("forceSync", "false"),
            headers: Vec::new(),
            body: (!remove).then(|| json!({ "users": [{ "id": user, "role": if owner { 2 } else { 1 } }] })),
        })
    }
}

impl Sealed for ChannelWrite {
    fn lab_channel(&self) -> Option<&ChannelTarget> {
        Some(&self.target)
    }

    fn wire(&self) -> Option<Wire> {
        let target = &self.target;
        let mut service = Service::MiddleTier;
        let mut endpoint = Endpoint::new([
            "beta",
            "teams",
            target.team.as_str(),
            "channels",
            target.channel.as_str(),
        ]);
        let mut headers = Vec::new();
        if let Some(group) = &target.channel_group {
            service = Service::Provisioning;
            endpoint = Endpoint::new(["teams", target.team_group.as_str(), "channels", group.as_str()]);
            headers.extend(target.mailbox.iter().map(|value| ("x-anchormailbox", value.clone())));
        }
        let (method, body) = match &self.action {
            ChannelChange::AddMember { .. }
            | ChannelChange::UpdateMember { .. }
            | ChannelChange::RemoveMember { .. }
            | ChannelChange::Moderator { .. } => return self.member_wire(),
            ChannelChange::Edit { name, description } => {
                if service == Service::Provisioning {
                    endpoint = endpoint.with_query("forceSync", "false");
                }
                (
                    Method::Patch,
                    Some(json!({ "displayName": name, "description": description })),
                )
            }
            ChannelChange::Settings(settings) => {
                // Channel moderation is a thread property for standard channels, never an account preference.
                service = Service::ChatService;
                endpoint = Endpoint::new(["v1", "threads", target.channel.as_str(), "properties"])
                    .with_query("name", "channelSettings");
                headers.clear();
                (Method::Put, Some(json!({ "channelSettings": settings.to_string() })))
            }
            ChannelChange::Archive {
                archived,
                site_read_only,
            } => {
                service = Service::Provisioning;
                endpoint = Endpoint::new([
                    "teams",
                    target.team_group.as_str(),
                    "channels",
                    target.channel.as_str(),
                    if *archived { "archive" } else { "unarchive" },
                ]);
                if *archived && *site_read_only {
                    endpoint = endpoint.with_query("setSpoSiteReadOnlyForMembers", "true");
                }
                (Method::Post, None)
            }
            ChannelChange::Delete => {
                if service == Service::Provisioning {
                    endpoint = endpoint.with_query("forceSync", "false");
                }
                (Method::Delete, None)
            }
            ChannelChange::CreateEmail | ChannelChange::UpdateEmail { .. } | ChannelChange::RemoveEmail => {
                return self.email_wire();
            }
        };
        Some(Wire {
            raw: None,
            service,
            method,
            endpoint,
            headers,
            body,
        })
    }
}

/// Reads used by the management dialogs. These never generate an email address or change a preference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelRead {
    Sections,
    Thread { channel: String },
    Preferences { channel: String },
    Email { channel: String },
}

impl ChannelRead {
    /// A thread response must describe the exact requested channel; a stale or redirected context isn't editable.
    #[must_use]
    pub fn read_answer(&self, body: &[u8]) -> Option<ChannelData> {
        let data = ChannelData::read(body)?;
        match self {
            Self::Thread { channel } | Self::Preferences { channel } => {
                if data.0.get("id").and_then(Value::as_str) != Some(channel.as_str())
                    || data.0.get("isDeleted").and_then(Value::as_bool) == Some(true)
                {
                    return None;
                }
            }
            Self::Sections => {
                crate::ChannelSections::read(&data.0)?;
            }
            Self::Email { .. } => {}
        }
        if matches!(self, Self::Preferences { .. }) && !valid_preferences(&data.0) {
            return None;
        }
        Some(data)
    }
}

fn valid_preferences(data: &Value) -> bool {
    let Some(properties) = data.get("properties") else {
        return true;
    };
    let Some(properties) = properties.as_object() else {
        return false;
    };
    if let Some(value) = properties.get("channelNotificationSettings") {
        let Some(text) = value.as_str() else {
            return false;
        };
        if !serde_json::from_str::<Value>(text).is_ok_and(|value| value.is_object()) {
            return false;
        }
    }
    ["alerts", "followAllRc", "notifyAllRc"].into_iter().all(|key| {
        properties
            .get(key)
            .is_none_or(|value| matches!(value.as_str(), Some("true" | "false")))
    })
}

impl Read for ChannelRead {
    fn service(&self) -> Service {
        match self {
            Self::Sections => Service::Csa,
            Self::Email { .. } => Service::MiddleTier,
            _ => Service::ChatService,
        }
    }

    fn endpoint(&self) -> Endpoint {
        match self {
            Self::Sections => crate::channel_sections::sections_endpoint(),
            Self::Thread { channel } => {
                Endpoint::new(["v1", "threads", channel.as_str()]).with_query("view", "msnp24Equivalent")
            }
            Self::Preferences { channel } => Endpoint::new(["v1", "users", "ME", "conversations", channel.as_str()])
                .with_query("view", "msnp24Equivalent"),
            Self::Email { channel } => Endpoint::new(["beta", "channels", channel.as_str(), "email"]),
        }
    }
}

/// The original shape is retained so a settings change can preserve unknown fields.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ChannelData(pub Value);

impl std::fmt::Debug for ChannelData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelData").finish_non_exhaustive()
    }
}

impl ChannelData {
    #[must_use]
    pub fn read(body: &[u8]) -> Option<Self> {
        let value: Value = serde_json::from_slice(body).ok()?;
        value.is_object().then_some(Self(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(private: bool) -> ChannelTarget {
        ChannelTarget {
            team: "19:owned@thread.tacv2".to_owned(),
            team_group: "team-group".to_owned(),
            channel: "19:channel@thread.tacv2".to_owned(),
            channel_group: private.then(|| "OID:channel-group@tenant".to_owned()),
            mailbox: None,
        }
    }
    #[test]
    fn private_edits_never_use_the_standard_channel_endpoint() {
        for private in [false, true] {
            let write = ChannelWrite {
                target: target(private),
                action: ChannelChange::Edit {
                    name: "Synthetic".to_owned(),
                    description: "Synthetic description".to_owned(),
                },
            };
            let wire = write.wire().expect("wire");
            assert_eq!(
                wire.service,
                if private {
                    Service::Provisioning
                } else {
                    Service::MiddleTier
                }
            );
            assert_eq!(wire.method, Method::Patch);
            assert_eq!(
                wire.endpoint.segments().last().map(String::as_str),
                Some(if private {
                    "OID:channel-group@tenant"
                } else {
                    "19:channel@thread.tacv2"
                })
            );
            assert_eq!(
                write.target().scope(),
                &Scope::Team {
                    id: "19:owned@thread.tacv2".to_owned()
                }
            );
        }
    }
    #[test]
    fn membership_writes_reject_nonwork_and_malformed_ids_before_wire_creation() {
        for mri in ["28:bot", "8:orgid:", "8:orgid:not-a-guid", "8:live:external"] {
            assert!(
                ChannelWrite {
                    target: target(true),
                    action: ChannelChange::AddMember {
                        mri: mri.to_owned(),
                        owner: false
                    }
                }
                .wire()
                .is_none()
            );
        }
        let good = "8:orgid:00000000-0000-4000-8000-000000000001";
        let add = ChannelWrite {
            target: target(true),
            action: ChannelChange::AddMember {
                mri: good.to_owned(),
                owner: false,
            },
        };
        let wire = add.wire().expect("wire");
        assert_eq!(wire.method, Method::Post);
        assert_eq!(
            wire.body.as_ref().and_then(|body| body.pointer("/users/0/role")),
            Some(&json!(1))
        );
        assert_eq!(
            wire.body.as_ref().and_then(|body| body.pointer("/users/0/id")),
            Some(&json!("OID:00000000-0000-4000-8000-000000000001@tenant"))
        );
        assert!(
            ChannelWrite {
                target: target(false),
                action: add.action
            }
            .wire()
            .is_none(),
            "private membership requires an exact channel group"
        );
    }
    #[test]
    fn moderation_round_trips_unknown_server_fields_and_has_a_channel_thread_target() {
        let settings = json!({"postPermissions":1,"futureSetting":{"keep":true}});
        let wire = ChannelWrite {
            target: target(false),
            action: ChannelChange::Settings(settings.clone()),
        }
        .wire()
        .expect("wire");
        assert_eq!(wire.service, Service::ChatService);
        assert_eq!(
            wire.endpoint.query(),
            &[("name".to_owned(), "channelSettings".to_owned())]
        );
        let stored = wire.body.expect("body")["channelSettings"]
            .as_str()
            .expect("string")
            .to_owned();
        assert_eq!(serde_json::from_str::<Value>(&stored).expect("JSON"), settings);
    }
    #[test]
    fn archive_and_restore_name_the_owning_group_and_exact_channel() {
        for archived in [true, false] {
            let wire = ChannelWrite {
                target: target(false),
                action: ChannelChange::Archive {
                    archived,
                    site_read_only: true,
                },
            }
            .wire()
            .expect("wire");
            assert_eq!(wire.service, Service::Provisioning);
            assert_eq!(
                wire.endpoint.segments(),
                [
                    "teams",
                    "team-group",
                    "channels",
                    "19:channel@thread.tacv2",
                    if archived { "archive" } else { "unarchive" }
                ]
            );
            assert_eq!(wire.endpoint.query().len(), usize::from(archived));
        }
    }
}

#[cfg(test)]
mod response_tests {
    use super::*;
    #[test]
    fn stale_thread_responses_and_partial_folders_never_become_editable_data() {
        let read = ChannelRead::Thread {
            channel: "expected".to_owned(),
        };
        assert!(read.read_answer(br#"{"id":"other","properties":{}}"#).is_none());
        assert!(read.read_answer(br#"{"id":"expected","isDeleted":true}"#).is_none());
        assert!(
            read.read_answer(br#"{"id":"expected","properties":{"channelSettings":"{}"}}"#)
                .is_some()
        );
        assert!(
            ChannelRead::Sections
                .read_answer(br#"{"isPartialResponse":true}"#)
                .is_none()
        );
    }
}
