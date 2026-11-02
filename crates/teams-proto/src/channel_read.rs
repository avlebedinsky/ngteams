//! The reads a new channel's dialog needs (`docs/protocol/channels.md`, "Creating a channel"): whether a private
//! channel's thread is there yet ([`ChannelCreationStatus`], on the provisioning service), and the people on a team a
//! name finds ([`TeamPeopleSearch`], the people picker's search, a read sent as a `POST` on the user profile service).

use serde_json::{Value, json};

use crate::endpoint::{Endpoint, Method, Service};
use crate::read::Read;
use crate::request::Request;

/// `GET teams/<team group>/channels/<channel group>` on the provisioning service: a private channel as it is made. Its
/// answer's `threadId` is the channel's thread, once it has one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelCreationStatus {
    pub team_group: String,
    pub channel_group: String,
}

impl ChannelCreationStatus {
    /// The channel's thread its answer `body` names.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<String> {
        serde_json::from_slice::<Value>(body)
            .ok()?
            .get("threadId")?
            .as_str()
            .filter(|thread| !thread.is_empty())
            .map(str::to_owned)
    }
}

impl Read for ChannelCreationStatus {
    fn service(&self) -> Service {
        Service::Provisioning
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([
            "teams",
            self.team_group.as_str(),
            "channels",
            self.channel_group.as_str(),
        ])
    }
}

/// `POST api/v1.0/teams/<thread>/users/profilesearch?top&includeDLs&includeBots&enableGuest` with `{"keyword"}` on the
/// user profile service: the people on the team a keyword finds, as a mention in a channel of it asked them
/// (`channel-create`): answered with a list of people (`mri`, `displayName`, `email`, `jobTitle`, …).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamPeopleSearch {
    pub team: String,
    pub keyword: String,
}

/// How many people a search asks for, as the capture's asked.
const TOP: &str = "15";

/// Someone a team's people search found. `Debug` leaves everything but the MRI out.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamPerson {
    pub mri: String,
    pub name: String,
    pub email: Option<String>,
    pub job_title: Option<String>,
}

impl std::fmt::Debug for TeamPerson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamPerson")
            .field("mri", &self.mri)
            .finish_non_exhaustive()
    }
}

impl TeamPeopleSearch {
    /// The people its answer `body` names, each with an MRI and a name.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<TeamPerson> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let listed = answer
            .as_array()
            .or_else(|| answer.get("value").and_then(Value::as_array));
        listed
            .into_iter()
            .flatten()
            .filter_map(|person| {
                let text = |key: &str| {
                    person
                        .get(key)
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                        .map(str::to_owned)
                };
                Some(TeamPerson {
                    mri: text("mri")?,
                    name: text("displayName")?,
                    email: text("email")
                        .or_else(|| text("mail"))
                        .or_else(|| text("userPrincipalName")),
                    job_title: text("jobTitle"),
                })
            })
            .collect()
    }
}

impl Request for TeamPeopleSearch {
    fn service(&self) -> Service {
        Service::UserProfile
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["api", "v1.0", "teams", self.team.as_str(), "users", "profilesearch"])
            .with_query("top", TOP)
            .with_query("includeDLs", "false")
            .with_query("includeBots", "false")
            .with_query("enableGuest", "true")
    }

    fn body(&self) -> Option<Value> {
        Some(json!({ "keyword": self.keyword }))
    }
}

/// Whether `base` is the path `[fabric, <region>, templates, api]` of the provisioning service's GTM.
#[must_use]
pub(crate) fn is_provisioning_path(path: &[String]) -> bool {
    matches!(
        path,
        [fabric, region, templates, api]
            if fabric == "fabric"
                && templates == "templates"
                && api == "api"
                && !region.is_empty()
                && region.bytes().all(|byte| byte.is_ascii_lowercase())
    )
}

/// Whether `base` is the path `[api, userprofilesvc, <region>]` of the user profile service's GTM.
#[must_use]
pub(crate) fn is_user_profile_path(path: &[String]) -> bool {
    matches!(
        path,
        [api, service, region]
            if api == "api"
                && service == "userprofilesvc"
                && !region.is_empty()
                && region.bytes().all(|byte| byte.is_ascii_lowercase())
    )
}
