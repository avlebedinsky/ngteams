//! Fresh channel context before shared changes. A stale menu cannot substitute a different team, group, privacy type,
//! or role. Idempotent desired states are reconciled after uncertain outcomes instead of blindly resending.
use crate::EngineError;
use teams_proto::{ChannelChange, ChannelType, ChannelWrite, Client, Credentials, CsaTeams, HttpTransport};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    Send,
    AlreadyApplied,
}

pub(crate) async fn context<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    write: &ChannelWrite,
) -> Result<Decision, EngineError> {
    let answer = client
        .read(&CsaTeams)
        .await
        .map_err(|_| refused("ChannelContextUnavailable"))?;
    let listed = CsaTeams::read_answer(answer.body()).map_err(|_| refused("ChannelContextUnavailable"))?;
    validate(&listed.teams, write)
}

fn refused(code: &str) -> EngineError {
    EngineError::TeamNotChanged {
        code: Some(code.to_owned()),
    }
}

fn validate(teams: &[teams_proto::Team], write: &ChannelWrite) -> Result<Decision, EngineError> {
    let target = &write.target;
    let team = teams
        .iter()
        .find(|team| team.id == target.team)
        .ok_or_else(|| refused("ChannelContextChanged"))?;
    if team.group_id.as_deref() != Some(target.team_group.as_str()) {
        return Err(refused("ChannelContextChanged"));
    }
    let Some(channel) = team.channels.iter().find(|channel| channel.id == target.channel) else {
        return if write.action == ChannelChange::Delete {
            Ok(Decision::AlreadyApplied)
        } else {
            Err(refused("ChannelContextChanged"))
        };
    };
    let manager = match channel.kind {
        ChannelType::Standard => team.owner && !team.about.channel_only,
        ChannelType::Private | ChannelType::Shared => channel.owner,
        ChannelType::Unknown => return Err(refused("ChannelContextChanged")),
    };
    if channel.kind == ChannelType::Standard {
        if target.channel_group.is_some() {
            return Err(refused("ChannelContextChanged"));
        }
    } else {
        let group = channel.group.as_ref().ok_or_else(|| refused("ChannelContextChanged"))?;
        let group = if group.starts_with("OID:") {
            group.clone()
        } else {
            format!("OID:{group}@{}", team.about.tenant)
        };
        if target.channel_group.as_ref() != Some(&group) {
            return Err(refused("ChannelContextChanged"));
        }
    }
    let active = !team.archived && !channel.archived;
    let permitted = match &write.action {
        ChannelChange::Edit { name, description } => {
            if channel.name == *name && channel.description == *description {
                return Ok(Decision::AlreadyApplied);
            }
            active && (manager || channel.can_edit && !team.about.guest)
        }
        ChannelChange::Settings(_) => active && manager && channel.kind == ChannelType::Standard && !channel.general,
        ChannelChange::Archive { archived, .. } => {
            if channel.archived == *archived {
                return Ok(Decision::AlreadyApplied);
            }
            !team.archived && manager && !channel.general
        }
        ChannelChange::Delete => {
            !team.archived && !channel.general && (manager || channel.can_delete && !team.about.guest)
        }
        ChannelChange::CreateEmail => active && channel.kind == ChannelType::Standard && !team.about.guest,
        ChannelChange::UpdateEmail { .. } | ChannelChange::RemoveEmail => {
            active && manager && channel.kind == ChannelType::Standard
        }
        ChannelChange::Moderator { .. } => {
            active && manager && channel.kind == ChannelType::Standard && !channel.general
        }
        ChannelChange::AddMember { .. } | ChannelChange::UpdateMember { .. } | ChannelChange::RemoveMember { .. } => {
            active && manager && channel.kind != ChannelType::Standard
        }
    };
    if permitted {
        Ok(Decision::Send)
    } else {
        Err(refused("ChannelPermissionChanged"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use teams_proto::ChannelTarget;
    fn teams() -> Vec<teams_proto::Team> {
        CsaTeams::read_answer(json!({"metadata":{"isPartialData":false},"teams":[{"id":"team","displayName":"Synthetic team","memberRoleString":"admin","teamSiteInformation":{"groupId":"group"},"tenantId":"tenant","channels":[{"id":"channel","displayName":"Synthetic channel","description":"Current","channelType":0}]}]}).to_string().as_bytes()).expect("synthetic complete teams").teams
    }
    fn write(action: ChannelChange) -> ChannelWrite {
        ChannelWrite {
            target: ChannelTarget {
                team: "team".to_owned(),
                team_group: "group".to_owned(),
                channel: "channel".to_owned(),
                channel_group: None,
                mailbox: None,
            },
            action,
        }
    }
    #[test]
    fn wrong_group_or_privacy_type_and_changed_permissions_are_refused() {
        let mut action = write(ChannelChange::Delete);
        action.target.team_group = "different".to_owned();
        assert!(validate(&teams(), &action).is_err());
        let mut listed = teams();
        listed[0].owner = false;
        assert!(validate(&listed, &write(ChannelChange::Delete)).is_err());
        listed[0].owner = true;
        listed[0].channels[0].kind = ChannelType::Private;
        assert!(validate(&listed, &write(ChannelChange::Delete)).is_err());
    }
    #[test]
    fn general_channels_and_archived_team_changes_are_refused() {
        let mut listed = teams();
        listed[0].channels[0].general = true;
        assert!(validate(&listed, &write(ChannelChange::Delete)).is_err());
        listed[0].channels[0].general = false;
        listed[0].archived = true;
        assert!(validate(&listed, &write(ChannelChange::Delete)).is_err());
    }
    #[test]
    fn a_confirmed_desired_state_is_not_sent_again() {
        let mut listed = teams();
        listed[0].channels[0].archived = true;
        assert_eq!(
            validate(
                &listed,
                &write(ChannelChange::Archive {
                    archived: true,
                    site_read_only: false
                })
            )
            .expect("reconciled"),
            Decision::AlreadyApplied
        );
        assert_eq!(
            validate(
                &teams(),
                &write(ChannelChange::Edit {
                    name: "Synthetic channel".to_owned(),
                    description: "Current".to_owned()
                })
            )
            .expect("reconciled"),
            Decision::AlreadyApplied
        );
        listed[0].channels.clear();
        assert_eq!(
            validate(&listed, &write(ChannelChange::Delete)).expect("gone"),
            Decision::AlreadyApplied
        );
    }
}
