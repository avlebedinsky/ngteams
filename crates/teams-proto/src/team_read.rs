//! What the team pages read (`docs/protocol/teams.md`), each as the web client's pages read it (`proto-lab run
//! team-manage`), and what changes only the account's own view of a team ([`OwnTeamView`]), which reaches nobody: a team's
//! people from the chat service, its tags and tag settings from CSA, its code, its channels and pending invitations from
//! the provisioning service, its analytics, the teams one can join and the people one can add.

use serde_json::{Value, json};

use crate::endpoint::{Endpoint, Method, Service};
use crate::read::Read;
use crate::request::Request;

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// A team's members in a role: `GET v1/threads/<team>/members?view&pageSize&selectMemberRoles`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamRoster {
    pub team: String,
    pub owners: bool,
}

/// How many people a roster's page asks for, as the web client's.
const ROSTER_PAGE: &str = "100";

/// Someone on a team's roster. `Debug` leaves out everything but the MRI.
#[derive(Clone, PartialEq, Eq)]
pub struct RosterMember {
    pub mri: String,
    pub owner: bool,
    pub guest: bool,
    /// A bot, which the web client doesn't count as a person.
    pub bot: bool,
}

impl std::fmt::Debug for RosterMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RosterMember")
            .field("mri", &self.mri)
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

impl TeamRoster {
    /// The people its answer `body` lists, with the service's count of everyone on the team.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<RosterMember> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("members")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|member| member.get("hidden").and_then(Value::as_bool) != Some(true))
            .filter_map(|member| {
                let mri = text(member, "id")?;
                let role = member.get("role").and_then(Value::as_str).unwrap_or("User");
                Some(RosterMember {
                    bot: mri.starts_with("28:"),
                    owner: role == "Admin",
                    guest: role == "Guest",
                    mri,
                })
            })
            .collect()
    }
}

impl Read for TeamRoster {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", self.team.as_str(), "members"])
            .with_query("view", "msnp24Equivalent")
            .with_query("pageSize", ROSTER_PAGE)
            .with_query("selectMemberRoles", if self.owners { "Admin" } else { "User|Guest" })
    }
}

/// A team's thread: `GET v1/threads/<team>?view=msnp24Equivalent`, of which a team's settings are read
/// (`properties.spaceAdminSettings`, text holding JSON).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamThread {
    pub team: String,
}

impl TeamThread {
    /// The settings its answer `body` holds, as the service keeps them.
    #[must_use]
    pub fn read_settings(body: &[u8]) -> Option<String> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        answer
            .pointer("/properties/spaceAdminSettings")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

impl Read for TeamThread {
    fn service(&self) -> Service {
        Service::ChatService
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", self.team.as_str()]).with_query("view", "msnp24Equivalent")
    }
}

/// A team's tags: `GET api/v1/teams/<team>/tagCards?tagType=any` on CSA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamTags {
    pub team: String,
}

/// A tag of a team. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamTag {
    pub id: String,
    pub name: String,
    pub description: String,
    pub members: u64,
    /// The version a change to it names.
    pub version: u64,
}

impl std::fmt::Debug for TeamTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamTag")
            .field("id", &self.id)
            .field("members", &self.members)
            .finish_non_exhaustive()
    }
}

/// A team's tags with who may change them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TagsRead {
    pub tags: Vec<TeamTag>,
    pub owners_only: bool,
    pub automatic: bool,
}

impl TeamTags {
    /// What its answer `body` says; nothing for an answer of another shape.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> TagsRead {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return TagsRead::default();
        };
        let flag = |key: &str| {
            answer
                .get("teamTagSettings")
                .and_then(|settings| settings.get(key))
                .and_then(Value::as_bool)
        };
        TagsRead {
            owners_only: flag("onlyOwnersCanUpdate").unwrap_or(true),
            automatic: flag("enableAutomaticTags").unwrap_or(false),
            tags: answer
                .get("tagCards")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tag| {
                    Some(TeamTag {
                        id: text(tag, "id").or_else(|| text(tag, "tagId"))?,
                        name: text(tag, "tagName").or_else(|| text(tag, "name"))?,
                        description: text(tag, "tagDescription").unwrap_or_default(),
                        members: tag
                            .get("memberCount")
                            .or_else(|| tag.get("membersCount"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0),
                        version: tag.get("version").and_then(Value::as_u64).unwrap_or(0),
                    })
                })
                .collect(),
        }
    }
}

impl Read for TeamTags {
    fn service(&self) -> Service {
        Service::Csa
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["api", "v1", "teams", self.team.as_str(), "tagCards"]).with_query("tagType", "any")
    }
}

/// A team's code: `GET beta/teams/<team>/joinCode` on the middle tier, 204 while it has none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamCode {
    pub team: String,
}

impl TeamCode {
    /// The code an answer's `body` names, in either of the forms the bundle reads.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<String> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        answer
            .as_str()
            .map(str::to_owned)
            .or_else(|| text(&answer, "value"))
            .or_else(|| text(&answer, "joinCode"))
            .filter(|code| !code.is_empty())
    }
}

impl Read for TeamCode {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", self.team.as_str(), "joinCode"])
    }
}

/// A team's channels with how they stand: `GET groups/<group>/channels?mode=IncludeIncomingChannels` on the provisioning
/// service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamChannels {
    pub group: String,
}

/// A channel as Manage team lists it. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct ManagedChannel {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `Standard`, `Private` or `Shared`.
    pub kind: String,
    /// When something last happened in it, as given; `0001-…` when nothing has.
    pub last_activity: String,
    pub archived: bool,
}

impl std::fmt::Debug for ManagedChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManagedChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TeamChannels {
    /// The channels its answer `body` lists, each with a thread and a name.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<ManagedChannel> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("channels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|channel| text(channel, "channelState").is_none_or(|state| state == "Active"))
            .filter_map(|channel| {
                Some(ManagedChannel {
                    id: text(channel, "threadId")?,
                    name: text(channel, "displayName")?,
                    description: text(channel, "description").unwrap_or_default(),
                    kind: text(channel, "channelType").unwrap_or_else(|| "Standard".to_owned()),
                    last_activity: text(channel, "lastActivity").unwrap_or_default(),
                    archived: text(channel, "archiveState").is_some_and(|state| state != "None"),
                })
            })
            .collect()
    }
}

impl Read for TeamChannels {
    fn service(&self) -> Service {
        Service::Provisioning
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["groups", self.group.as_str(), "channels"]).with_query("mode", "IncludeIncomingChannels")
    }
}

/// The invitations waiting for the account (a channel shared with a team it owns): `GET users/me/invitations` on the
/// provisioning service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PendingInvites;

/// An invitation waiting. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingInvite {
    pub id: String,
    pub channel: String,
    pub team: String,
    pub from: String,
}

impl std::fmt::Debug for PendingInvite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingInvite")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl PendingInvites {
    /// The invitations its answer `body` lists.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<PendingInvite> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("invitations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|invite| PendingInvite {
                id: text(invite, "id").unwrap_or_default(),
                channel: text(invite, "channelDisplayName")
                    .or_else(|| text(invite, "displayName"))
                    .unwrap_or_default(),
                team: text(invite, "teamDisplayName").unwrap_or_default(),
                from: text(invite, "invitedBy")
                    .or_else(|| text(invite, "inviterDisplayName"))
                    .unwrap_or_default(),
            })
            .collect()
    }
}

impl Read for PendingInvites {
    fn service(&self) -> Service {
        Service::Provisioning
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", "me", "invitations"])
    }
}

/// A span of analytics, as the page's Time period names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AnalyticsPeriod {
    #[default]
    LastSevenDays,
    LastThirtyDays,
    LastNinetyDays,
}

impl AnalyticsPeriod {
    /// Its `timeperiod`.
    #[must_use]
    pub fn wire(self) -> &'static str {
        match self {
            Self::LastSevenDays => "last-seven-days",
            Self::LastThirtyDays => "last-thirty-days",
            Self::LastNinetyDays => "last-ninety-days",
        }
    }

    /// Its name on the page.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LastSevenDays => "Last Seven Days",
            Self::LastThirtyDays => "Last 30 Days",
            Self::LastNinetyDays => "Last 90 Days",
        }
    }
}

/// How a team did over a time period.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamAnalytics {
    pub team: String,
    pub name: String,
    pub active_users: u64,
    /// `(day, active users)`, oldest first.
    pub active_users_by_day: Vec<(String, u64)>,
    pub guests: u64,
    pub posts: u64,
    pub replies: u64,
    pub reactions: u64,
    pub mentions: u64,
    pub meetings: u64,
}

fn analytics_of(entry: &Value) -> Option<TeamAnalytics> {
    let metrics = entry.get("metrics")?;
    let value = |key: &str| {
        metrics
            .get(key)
            .and_then(|metric| metric.get("value"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    Some(TeamAnalytics {
        team: text(entry, "id").unwrap_or_default(),
        name: text(entry, "displayName").unwrap_or_default(),
        active_users: value("activeUsers"),
        active_users_by_day: metrics
            .pointer("/activeUsers/timeSeries")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|day| Some((text(day, "date")?, day.get("value")?.as_u64()?)))
            .collect(),
        guests: value("guests"),
        posts: value("postMessages"),
        replies: value("replyMessages"),
        reactions: value("reactions"),
        mentions: value("mentions"),
        meetings: value("meetingsOrganized"),
    })
}

/// A team's analytics: `GET beta/analytics/teams/<group>/summary-timeseries` on the middle tier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamAnalyticsRead {
    pub group: String,
    pub period: AnalyticsPeriod,
}

impl TeamAnalyticsRead {
    /// What its answer `body` says.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<TeamAnalytics> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        analytics_of(answer.get("teams")?)
    }
}

impl Read for TeamAnalyticsRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "analytics", "teams", self.group.as_str(), "summary-timeseries"])
            .with_query("metric", "all")
            .with_query("timeperiod", self.period.wire())
            .with_query("includeTimeseries", "activeUsers")
    }
}

/// One channel's analytics, Analytics' "Select a channel":
/// `GET beta/analytics/teams/<group>/channels/<channel>/summary-timeseries` on the middle tier, answered as a team's is
/// under `channels`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelAnalyticsRead {
    pub group: String,
    pub channel: String,
    pub period: AnalyticsPeriod,
}

/// The day by day series a channel's analytics asks for, as the web client's page does.
const CHANNEL_SERIES: &str = "activeUsers,postMessages,replyMessages,reactions,mentions,meetingsOrganized";

impl ChannelAnalyticsRead {
    /// What its answer `body` says.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<TeamAnalytics> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        analytics_of(answer.get("channels")?)
    }
}

impl Read for ChannelAnalyticsRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "beta",
            "analytics",
            "teams",
            self.group.as_str(),
            "channels",
            self.channel.as_str(),
            "summary-timeseries",
        ])
        .with_query("metric", "all")
        .with_query("timeperiod", self.period.wire())
        .with_query("includeTimeseries", CHANNEL_SERIES)
    }
}

/// The analytics of the account's teams: `GET beta/analytics/me/teams/summary-timeseries` on the middle tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllTeamsAnalyticsRead {
    pub period: AnalyticsPeriod,
}

impl AllTeamsAnalyticsRead {
    /// What its answer `body` says, a team a line.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<TeamAnalytics> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("teams")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(analytics_of)
            .collect()
    }
}

impl Read for AllTeamsAnalyticsRead {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "analytics", "me", "teams", "summary-timeseries"])
            .with_query("metric", "all")
            .with_query("timeperiod", self.period.wire())
            .with_query("includeTimeseries", "activeUsers")
            .with_query("pagesize", "40")
            .with_query("nextCursor", "")
    }
}

/// The public teams the "Join a team" page suggests or finds: `GET beta/teams/suggested` and `beta/teams/find`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinableTeams {
    /// What the page's box was given, when anything.
    pub keywords: Option<String>,
}

/// `accessType` of the teams the page lists.
const JOINABLE_ACCESS: &str = "Public";

/// A team one can join. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinableTeam {
    pub group: String,
    pub name: String,
    pub description: String,
    pub public: bool,
}

impl std::fmt::Debug for JoinableTeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinableTeam")
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

impl JoinableTeams {
    /// The teams its answer `body` lists.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<JoinableTeam> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let listed = answer
            .pointer("/value/teams")
            .or_else(|| answer.get("teams"))
            .or_else(|| answer.get("value"))
            .or(Some(&answer))
            .and_then(Value::as_array);
        listed
            .into_iter()
            .flatten()
            .filter_map(|team| {
                Some(JoinableTeam {
                    group: text(team, "groupId").or_else(|| text(team, "id"))?,
                    name: text(team, "displayName")?,
                    description: text(team, "description").unwrap_or_default(),
                    public: team
                        .get("accessType")
                        .is_none_or(|access| access != &json!(1) && access != &json!("Private")),
                })
            })
            .collect()
    }
}

impl Read for JoinableTeams {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        match &self.keywords {
            Some(keywords) => Endpoint::new(["beta", "teams", "find"])
                .with_query("keywords", keywords)
                .with_query("accessType", JOINABLE_ACCESS)
                .with_query("enableKeywordSearch", "false")
                .with_query("top", "100")
                .with_query("skipToken", ""),
            None => Endpoint::new(["beta", "teams", "suggested"])
                .with_query("accessType", JOINABLE_ACCESS)
                .with_query("enableFallback", "true"),
        }
    }
}

/// The people a name finds in the organization, to add to a team: `POST beta/users/searchV2` with the name as a text body
/// on the middle tier, a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddablePeopleSearch {
    pub keyword: String,
}

impl AddablePeopleSearch {
    /// The people its answer `body` names, each with an MRI and a name, read as a team's people search reads them.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<crate::TeamPerson> {
        crate::TeamPeopleSearch::read_answer(body)
    }
}

impl Request for AddablePeopleSearch {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "users", "searchV2"])
            .with_query("includeDLs", "true")
            .with_query("enableGuest", "true")
            .with_query("includeBots", "true")
            .with_query("skypeTeamsInfo", "true")
            .with_query("source", "addToTeam")
    }

    fn body(&self) -> Option<Value> {
        Some(json!(self.keyword))
    }
}

/// The requests a team's owners waiting to let in: `POST beta/teams/findJoinRequests` with `{"GroupIdList"}`, a read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamJoinRequests {
    pub group: String,
}

impl TeamJoinRequests {
    /// The MRIs of the people asking, as its answer `body` lists them.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<String> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("joinRequestsPerTeam")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|teams| teams.values())
            .filter_map(Value::as_array)
            .flatten()
            .filter_map(|request| text(request, "userMri").or_else(|| text(request, "UserMri")))
            .collect()
    }
}

impl Request for TeamJoinRequests {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", "findJoinRequests"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({ "GroupIdList": [self.group] }))
    }
}

/// What changes only the account's own view of a team: its channels hidden or shown. Each reaches nobody, so it is a
/// plain request (`teams_fence::contacting`'s rows carve them out by name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnTeamView {
    /// All of a team's channels hidden for the account: `PUT team/<team>/hide` on the provisioning service.
    HideAll { team: String },
    /// A hidden team shown again: `PUT conversations/show` on the provisioning service.
    ShowAll { team: String },
    /// A channel shown or hidden, its interest: `PUT v1/threads/<channel>/members/<own mri>` on the chat service.
    ChannelInterest { channel: String, own: String, shown: bool },
    /// The property that keeps a channel in the account's list: `PUT v1/users/ME/conversations/<channel>/properties`, by
    /// `isGeneralChannelFavorite` for a general channel, `favorite` for another.
    ChannelFavorite {
        channel: String,
        general: bool,
        shown: bool,
    },
}

impl Request for OwnTeamView {
    fn service(&self) -> Service {
        match self {
            Self::HideAll { .. } | Self::ShowAll { .. } => Service::Provisioning,
            Self::ChannelInterest { .. } | Self::ChannelFavorite { .. } => Service::ChatService,
        }
    }

    fn method(&self) -> Method {
        Method::Put
    }

    fn endpoint(&self) -> Endpoint {
        match self {
            Self::HideAll { team } => Endpoint::new(["team", team.as_str(), "hide"]),
            Self::ShowAll { .. } => Endpoint::new(["conversations", "show"]),
            Self::ChannelInterest { channel, own, .. } => {
                Endpoint::new(["v1", "threads", channel.as_str(), "members", own.as_str()])
            }
            Self::ChannelFavorite { channel, general, .. } => {
                Endpoint::new(["v1", "users", "ME", "conversations", channel.as_str(), "properties"])
                    .with_query("name", favorite_property(*general))
            }
        }
    }

    fn body(&self) -> Option<Value> {
        match self {
            Self::HideAll { .. } => None,
            Self::ShowAll { team } => Some(json!({ "teamIds": [team], "channelIds": [] })),
            Self::ChannelInterest { shown, .. } => Some(json!({
                "interest": if *shown { "Interested" } else { "NotInterested" }
            })),
            Self::ChannelFavorite { general, shown, .. } => {
                let mut body = serde_json::Map::new();
                body.insert(
                    favorite_property(*general).to_owned(),
                    json!(if *shown { "true" } else { "false" }),
                );
                Some(Value::Object(body))
            }
        }
    }
}

fn favorite_property(general: bool) -> &'static str {
    if general {
        "isGeneralChannelFavorite"
    } else {
        "favorite"
    }
}

/// A team's picture as the account reads it: `GET beta/users/<own object id>/profilepicturev2/teams/<group>` on the middle
/// tier, a generated one for a team that has set none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamPicture {
    pub own: String,
    pub group: String,
    pub name: String,
    pub etag: String,
}

impl Read for TeamPicture {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "beta",
            "users",
            self.own.as_str(),
            "profilepicturev2",
            "teams",
            self.group.as_str(),
        ])
        .with_query("etag", &self.etag)
        .with_query("displayName", &self.name)
        .with_query("voidCache", "true")
    }
}

#[cfg(test)]
mod tests {
    //! The answers here are synthetic: the team pages' answers' shapes with made-up ids and names.

    use super::*;

    const TEAM: &str = "19:team0001@thread.tacv2";
    const GROUP: &str = "00000000-0000-4000-8000-0000000000aa";

    /// An endpoint as `path?query`, the query in the order it was given.
    fn shown(endpoint: &Endpoint) -> String {
        let path = endpoint.segments().join("/");
        let query: Vec<String> = endpoint
            .query()
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        if query.is_empty() {
            path
        } else {
            format!("{path}?{}", query.join("&"))
        }
    }

    fn bytes(value: &Value) -> Vec<u8> {
        value.to_string().into_bytes()
    }

    #[test]
    fn a_roster_lists_the_visible_people_with_their_roles() {
        let body = json!({"members": [
            {"id": "8:orgid:owner", "role": "Admin"},
            {"id": "8:orgid:member", "role": "User"},
            {"id": "8:orgid:guest", "role": "Guest"},
            {"id": "8:orgid:no-role"},
            {"id": "28:bot-0001", "role": "User"},
            {"id": "8:orgid:hidden", "role": "User", "hidden": true},
            {"id": "", "role": "User"},
            {"role": "Admin"},
        ]});
        let members = TeamRoster::read_answer(&bytes(&body));
        let mris: Vec<_> = members.iter().map(|member| member.mri.as_str()).collect();
        assert_eq!(
            mris,
            [
                "8:orgid:owner",
                "8:orgid:member",
                "8:orgid:guest",
                "8:orgid:no-role",
                "28:bot-0001"
            ]
        );
        assert!(members[0].owner && !members[0].guest && !members[0].bot);
        assert!(!members[1].owner && !members[1].guest);
        assert!(members[2].guest && !members[2].owner);
        assert!(!members[3].owner && !members[3].guest);
        assert!(members[4].bot);
        assert_eq!(
            format!("{:?}", members[0]),
            "RosterMember { mri: \"8:orgid:owner\", owner: true, .. }"
        );
        assert!(TeamRoster::read_answer(b"not json").is_empty());
        assert!(TeamRoster::read_answer(b"{}").is_empty());
    }

    #[test]
    fn a_roster_asks_the_chat_service_for_one_role_a_page() {
        let owners = TeamRoster {
            team: TEAM.to_owned(),
            owners: true,
        };
        assert_eq!(owners.service(), Service::ChatService);
        assert_eq!(
            shown(&owners.endpoint()),
            format!("v1/threads/{TEAM}/members?view=msnp24Equivalent&pageSize=100&selectMemberRoles=Admin")
        );
        let members = TeamRoster {
            team: TEAM.to_owned(),
            owners: false,
        };
        assert!(shown(&members.endpoint()).ends_with("selectMemberRoles=User|Guest"));
    }

    #[test]
    fn a_teams_thread_holds_its_settings_as_text() {
        let thread = TeamThread { team: TEAM.to_owned() };
        assert_eq!(thread.service(), Service::ChatService);
        assert_eq!(
            shown(&thread.endpoint()),
            format!("v1/threads/{TEAM}?view=msnp24Equivalent")
        );
        let body = json!({"properties": {"spaceAdminSettings": "{\"allowCreateUpdateChannels\":true}"}});
        assert_eq!(
            TeamThread::read_settings(&bytes(&body)).as_deref(),
            Some("{\"allowCreateUpdateChannels\":true}")
        );
        assert_eq!(TeamThread::read_settings(b"{\"properties\": {}}"), None);
        assert_eq!(TeamThread::read_settings(b"nope"), None);
    }

    #[test]
    fn tags_are_read_in_either_naming_with_who_may_change_them() {
        let tags = TeamTags { team: TEAM.to_owned() };
        assert_eq!(tags.service(), Service::Csa);
        assert_eq!(
            shown(&tags.endpoint()),
            format!("api/v1/teams/{TEAM}/tagCards?tagType=any")
        );
        let body = json!({
            "teamTagSettings": {"onlyOwnersCanUpdate": false, "enableAutomaticTags": true},
            "tagCards": [
                {"id": "t1", "tagName": "Designers", "tagDescription": "Draws", "memberCount": 3, "version": 7},
                {"tagId": "t2", "name": "Writers", "membersCount": 2},
                {"id": "t3"},
                {"tagName": "No id"},
            ],
        });
        let read = TeamTags::read_answer(&bytes(&body));
        assert!(!read.owners_only);
        assert!(read.automatic);
        assert_eq!(read.tags.len(), 2);
        assert_eq!(
            (
                read.tags[0].id.as_str(),
                read.tags[0].name.as_str(),
                read.tags[0].description.as_str()
            ),
            ("t1", "Designers", "Draws")
        );
        assert_eq!((read.tags[0].members, read.tags[0].version), (3, 7));
        assert_eq!(
            (read.tags[1].id.as_str(), read.tags[1].name.as_str()),
            ("t2", "Writers")
        );
        assert_eq!((read.tags[1].members, read.tags[1].version), (2, 0));
        assert!(read.tags[1].description.is_empty());
        let debug = format!("{:?}", read.tags[0]);
        assert!(debug.contains("t1") && !debug.contains("Designers"));
        // no settings: owners only, nothing automatic
        let bare = TeamTags::read_answer(b"{}");
        assert!(bare.owners_only && !bare.automatic && bare.tags.is_empty());
        assert_eq!(TeamTags::read_answer(b"]"), TagsRead::default());
    }

    #[test]
    fn a_teams_code_is_read_in_each_form() {
        let code = TeamCode { team: TEAM.to_owned() };
        assert_eq!(code.service(), Service::MiddleTier);
        assert_eq!(shown(&code.endpoint()), format!("beta/teams/{TEAM}/joinCode"));
        assert_eq!(TeamCode::read_answer(b"\"abc1234\"").as_deref(), Some("abc1234"));
        assert_eq!(
            TeamCode::read_answer(b"{\"value\": \"def5678\"}").as_deref(),
            Some("def5678")
        );
        assert_eq!(
            TeamCode::read_answer(b"{\"joinCode\": \"ghi9012\"}").as_deref(),
            Some("ghi9012")
        );
        assert_eq!(TeamCode::read_answer(b"\"\""), None);
        assert_eq!(TeamCode::read_answer(b"{}"), None);
        assert_eq!(TeamCode::read_answer(b""), None);
    }

    #[test]
    fn managed_channels_are_the_active_ones_with_how_they_stand() {
        let channels = TeamChannels {
            group: GROUP.to_owned(),
        };
        assert_eq!(channels.service(), Service::Provisioning);
        assert_eq!(
            shown(&channels.endpoint()),
            format!("groups/{GROUP}/channels?mode=IncludeIncomingChannels")
        );
        let body = json!({"channels": [
            {"threadId": "19:general@thread.tacv2", "displayName": "General", "channelState": "Active",
             "lastActivity": "2026-01-02T03:04:05Z", "archiveState": "None"},
            {"threadId": "19:private@thread.tacv2", "displayName": "Secret plans", "description": "Plans",
             "channelType": "Private", "archiveState": "Archived"},
            {"threadId": "19:gone@thread.tacv2", "displayName": "Gone", "channelState": "Deleted"},
            {"threadId": "19:nameless@thread.tacv2"},
        ]});
        let read = TeamChannels::read_answer(&bytes(&body));
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].kind, "Standard");
        assert_eq!(read[0].last_activity, "2026-01-02T03:04:05Z");
        assert!(!read[0].archived);
        assert_eq!(
            (read[1].kind.as_str(), read[1].description.as_str()),
            ("Private", "Plans")
        );
        assert!(read[1].archived);
        assert!(read[1].last_activity.is_empty());
        let debug = format!("{:?}", read[1]);
        assert!(debug.contains("19:private") && !debug.contains("Secret"));
        assert!(TeamChannels::read_answer(b"[").is_empty());
    }

    #[test]
    fn pending_invitations_are_read_in_either_naming() {
        assert_eq!(PendingInvites.service(), Service::Provisioning);
        assert_eq!(shown(&PendingInvites.endpoint()), "users/me/invitations");
        let body = json!({"invitations": [
            {"id": "i1", "channelDisplayName": "Shared", "teamDisplayName": "Host team", "invitedBy": "Someone"},
            {"id": "i2", "displayName": "Other", "inviterDisplayName": "Another"},
            {},
        ]});
        let invites = PendingInvites::read_answer(&bytes(&body));
        assert_eq!(invites.len(), 3);
        assert_eq!(
            (
                invites[0].channel.as_str(),
                invites[0].team.as_str(),
                invites[0].from.as_str()
            ),
            ("Shared", "Host team", "Someone")
        );
        assert_eq!(
            (invites[1].channel.as_str(), invites[1].from.as_str()),
            ("Other", "Another")
        );
        assert!(invites[1].team.is_empty());
        assert!(invites[2].id.is_empty());
        let debug = format!("{:?}", invites[0]);
        assert!(debug.contains("i1") && !debug.contains("Someone"));
        assert!(PendingInvites::read_answer(b"x").is_empty());
    }

    #[test]
    fn each_period_has_its_wire_and_its_label() {
        assert_eq!(AnalyticsPeriod::default(), AnalyticsPeriod::LastSevenDays);
        let periods = [
            AnalyticsPeriod::LastSevenDays,
            AnalyticsPeriod::LastThirtyDays,
            AnalyticsPeriod::LastNinetyDays,
        ];
        let wires: Vec<_> = periods.iter().map(|period| period.wire()).collect();
        assert_eq!(wires, ["last-seven-days", "last-thirty-days", "last-ninety-days"]);
        let labels: Vec<_> = periods.iter().map(|period| period.label()).collect();
        assert_eq!(labels, ["Last Seven Days", "Last 30 Days", "Last 90 Days"]);
    }

    fn analytics_entry() -> Value {
        json!({
            "id": GROUP,
            "displayName": "Team",
            "metrics": {
                "activeUsers": {"value": 4, "timeSeries": [
                    {"date": "2026-01-01", "value": 1},
                    {"date": "2026-01-02", "value": 3},
                    {"date": "2026-01-03"},
                    {"value": 2},
                ]},
                "guests": {"value": 1},
                "postMessages": {"value": 10},
                "replyMessages": {"value": 20},
                "reactions": {"value": 5},
                "mentions": {"value": 2},
                "meetingsOrganized": {"value": 6},
            },
        })
    }

    #[test]
    fn a_teams_analytics_reads_each_metric_and_the_day_series() {
        let read = TeamAnalyticsRead {
            group: GROUP.to_owned(),
            period: AnalyticsPeriod::LastThirtyDays,
        };
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(
            shown(&read.endpoint()),
            format!(
                "beta/analytics/teams/{GROUP}/summary-timeseries?metric=all&timeperiod=last-thirty-days\
                 &includeTimeseries=activeUsers"
            )
        );
        let analytics =
            TeamAnalyticsRead::read_answer(&bytes(&json!({"teams": analytics_entry()}))).expect("the analytics");
        assert_eq!(
            analytics,
            TeamAnalytics {
                team: GROUP.to_owned(),
                name: "Team".to_owned(),
                active_users: 4,
                active_users_by_day: vec![("2026-01-01".to_owned(), 1), ("2026-01-02".to_owned(), 3)],
                guests: 1,
                posts: 10,
                replies: 20,
                reactions: 5,
                mentions: 2,
                meetings: 6,
            }
        );
        // a team without metrics, and answers of another shape
        assert_eq!(TeamAnalyticsRead::read_answer(b"{\"teams\": {\"id\": \"x\"}}"), None);
        assert_eq!(TeamAnalyticsRead::read_answer(b"{}"), None);
        assert_eq!(TeamAnalyticsRead::read_answer(b"-"), None);
        // a metric left out counts nothing
        let sparse = TeamAnalyticsRead::read_answer(b"{\"teams\": {\"metrics\": {}}}").expect("read");
        assert_eq!(sparse, TeamAnalytics::default());
    }

    #[test]
    fn a_channels_analytics_asks_for_every_series_and_is_read_under_channels() {
        let read = ChannelAnalyticsRead {
            group: GROUP.to_owned(),
            channel: "19:general@thread.tacv2".to_owned(),
            period: AnalyticsPeriod::LastNinetyDays,
        };
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(
            shown(&read.endpoint()),
            format!(
                "beta/analytics/teams/{GROUP}/channels/19:general@thread.tacv2/summary-timeseries?metric=all\
                 &timeperiod=last-ninety-days&includeTimeseries={CHANNEL_SERIES}"
            )
        );
        let analytics =
            ChannelAnalyticsRead::read_answer(&bytes(&json!({"channels": analytics_entry()}))).expect("the analytics");
        assert_eq!((analytics.posts, analytics.meetings), (10, 6));
        assert_eq!(
            ChannelAnalyticsRead::read_answer(&bytes(&json!({"teams": analytics_entry()}))),
            None
        );
        assert_eq!(ChannelAnalyticsRead::read_answer(b""), None);
    }

    #[test]
    fn all_teams_analytics_reads_a_team_a_line() {
        let read = AllTeamsAnalyticsRead {
            period: AnalyticsPeriod::LastSevenDays,
        };
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(
            shown(&read.endpoint()),
            "beta/analytics/me/teams/summary-timeseries?metric=all&timeperiod=last-seven-days\
             &includeTimeseries=activeUsers&pagesize=40&nextCursor="
        );
        let body = json!({"teams": [analytics_entry(), {"id": "no-metrics"}, analytics_entry()]});
        let teams = AllTeamsAnalyticsRead::read_answer(&bytes(&body));
        assert_eq!(teams.len(), 2);
        assert_eq!(teams[0].replies, 20);
        assert!(AllTeamsAnalyticsRead::read_answer(b"{}").is_empty());
        assert!(AllTeamsAnalyticsRead::read_answer(b"?").is_empty());
    }

    #[test]
    fn joinable_teams_are_suggested_or_found_by_keyword() {
        let suggested = JoinableTeams { keywords: None };
        assert_eq!(suggested.service(), Service::MiddleTier);
        assert_eq!(
            shown(&suggested.endpoint()),
            "beta/teams/suggested?accessType=Public&enableFallback=true"
        );
        let found = JoinableTeams {
            keywords: Some("design".to_owned()),
        };
        assert_eq!(
            shown(&found.endpoint()),
            "beta/teams/find?keywords=design&accessType=Public&enableKeywordSearch=false&top=100&skipToken="
        );
    }

    #[test]
    fn joinable_teams_are_read_in_each_shape_with_their_access() {
        let teams = json!([
            {"groupId": "g1", "displayName": "Open", "description": "Anyone", "accessType": 3},
            {"id": "g2", "displayName": "Closed", "accessType": 1},
            {"id": "g3", "displayName": "Closed by name", "accessType": "Private"},
            {"id": "g4", "displayName": "No access type"},
            {"id": "g5"},
            {"displayName": "No id"},
        ]);
        for body in [
            json!({"value": {"teams": teams}}),
            json!({"teams": teams}),
            json!({"value": teams}),
            teams.clone(),
        ] {
            let read = JoinableTeams::read_answer(&bytes(&body));
            let groups: Vec<_> = read.iter().map(|team| team.group.as_str()).collect();
            assert_eq!(groups, ["g1", "g2", "g3", "g4"], "{body}");
            let public: Vec<_> = read.iter().map(|team| team.public).collect();
            assert_eq!(public, [true, false, false, true]);
            assert_eq!(read[0].description, "Anyone");
        }
        let read = JoinableTeams::read_answer(&bytes(&teams));
        let debug = format!("{:?}", read[0]);
        assert!(debug.contains("g1") && !debug.contains("Anyone"));
        assert!(JoinableTeams::read_answer(b"{}").is_empty());
        assert!(JoinableTeams::read_answer(b"!").is_empty());
    }

    #[test]
    fn addable_people_are_searched_by_name_on_the_middle_tier() {
        let search = AddablePeopleSearch {
            keyword: "Ada".to_owned(),
        };
        assert_eq!(search.service(), Service::MiddleTier);
        assert_eq!(search.method(), Method::Post);
        assert_eq!(
            shown(&search.endpoint()),
            "beta/users/searchV2?includeDLs=true&enableGuest=true&includeBots=true&skypeTeamsInfo=true\
             &source=addToTeam"
        );
        assert_eq!(search.body(), Some(json!("Ada")));
        let body = json!({"value": [{"mri": "8:orgid:ada", "displayName": "Ada", "mail": "ada@example.com"}]});
        let people = AddablePeopleSearch::read_answer(&bytes(&body));
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].email.as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn join_requests_are_read_for_one_group_in_either_casing() {
        let read = TeamJoinRequests {
            group: GROUP.to_owned(),
        };
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(read.method(), Method::Post);
        assert_eq!(shown(&read.endpoint()), "beta/teams/findJoinRequests");
        assert_eq!(read.body(), Some(json!({"GroupIdList": [GROUP]})));
        let body = json!({"joinRequestsPerTeam": {
            GROUP: [{"userMri": "8:orgid:asker-1"}, {"UserMri": "8:orgid:asker-2"}, {"other": 1}],
            "other-group": "not a list",
        }});
        assert_eq!(
            TeamJoinRequests::read_answer(&bytes(&body)),
            ["8:orgid:asker-1", "8:orgid:asker-2"]
        );
        assert!(TeamJoinRequests::read_answer(b"{}").is_empty());
        assert!(TeamJoinRequests::read_answer(b"").is_empty());
    }

    #[test]
    fn a_team_hidden_or_shown_goes_to_the_provisioning_service() {
        let hide = OwnTeamView::HideAll { team: TEAM.to_owned() };
        assert_eq!(hide.service(), Service::Provisioning);
        assert_eq!(hide.method(), Method::Put);
        assert_eq!(shown(&hide.endpoint()), format!("team/{TEAM}/hide"));
        assert_eq!(hide.body(), None);
        let show = OwnTeamView::ShowAll { team: TEAM.to_owned() };
        assert_eq!(show.service(), Service::Provisioning);
        assert_eq!(shown(&show.endpoint()), "conversations/show");
        assert_eq!(show.body(), Some(json!({"teamIds": [TEAM], "channelIds": []})));
    }

    #[test]
    fn a_channel_shown_or_hidden_sets_the_accounts_interest_and_favorite() {
        let channel = "19:channel@thread.tacv2";
        for (shown_now, interest) in [(true, "Interested"), (false, "NotInterested")] {
            let request = OwnTeamView::ChannelInterest {
                channel: channel.to_owned(),
                own: "8:orgid:own".to_owned(),
                shown: shown_now,
            };
            assert_eq!(request.service(), Service::ChatService);
            assert_eq!(
                shown(&request.endpoint()),
                format!("v1/threads/{channel}/members/8:orgid:own")
            );
            assert_eq!(request.body(), Some(json!({"interest": interest})));
        }
        let cases = [
            (true, true, "isGeneralChannelFavorite", "true"),
            (true, false, "isGeneralChannelFavorite", "false"),
            (false, true, "favorite", "true"),
            (false, false, "favorite", "false"),
        ];
        for (general, shown_now, property, value) in cases {
            let request = OwnTeamView::ChannelFavorite {
                channel: channel.to_owned(),
                general,
                shown: shown_now,
            };
            assert_eq!(request.service(), Service::ChatService);
            assert_eq!(request.method(), Method::Put);
            assert_eq!(
                shown(&request.endpoint()),
                format!("v1/users/ME/conversations/{channel}/properties?name={property}")
            );
            let mut body = serde_json::Map::new();
            body.insert(property.to_owned(), json!(value));
            assert_eq!(request.body(), Some(Value::Object(body)));
        }
    }

    #[test]
    fn a_teams_picture_is_read_for_the_account_by_group() {
        let picture = TeamPicture {
            own: "00000000-0000-4000-8000-00000000000a".to_owned(),
            group: GROUP.to_owned(),
            name: "Team".to_owned(),
            etag: "etag-1".to_owned(),
        };
        assert_eq!(picture.service(), Service::MiddleTier);
        assert_eq!(
            shown(&picture.endpoint()),
            format!(
                "beta/users/00000000-0000-4000-8000-00000000000a/profilepicturev2/teams/{GROUP}?etag=etag-1\
                 &displayName=Team&voidCache=true"
            )
        );
    }
}
