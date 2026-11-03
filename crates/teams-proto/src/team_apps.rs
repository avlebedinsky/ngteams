//! What Manage team's Apps tab and "More create team options" read (`docs/protocol/teams.md`), as the web client reads
//! them (`proto-lab run team-manage`): a team's installed apps from the app catalogue's entitlements, Teams' team
//! templates and one template's channels and apps from the provisioning service, and the Microsoft 365 groups the
//! account could make a team of.

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

/// A team's apps: `POST beta/teams/apps/aggregatedEntitlements?appbarview=userpinned` on the middle tier with the team's
/// thread in a list, a read. Its answer holds every app the team is entitled to by state (`teamsEntitlements.<thread>`)
/// and their definitions (`definitions.<app id>`); the tab lists those `Installed`, the rest being Teams' own built-in
/// ones (`InstalledAndPermanent`, `InstalledAndDeprecated`) and ones offered (`NotInstalled`, `PreConsented`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamApps {
    pub team: String,
}

/// An app installed on a team. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// Its app id (a GUID, or a name like `com.microsoft.teamspace.tab.planner`).
    pub id: String,
    pub name: String,
    /// Who makes it (`developerName`).
    pub developer: String,
    pub description: String,
    /// Its colour (`accentColor`, `#rrggbb`), when it has one.
    pub accent: Option<u32>,
}

impl std::fmt::Debug for InstalledApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstalledApp")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// `#rrggbb` as a colour.
fn colour(value: &str) -> Option<u32> {
    let hex = value.strip_prefix('#').filter(|hex| hex.len() == 6)?;
    u32::from_str_radix(hex, 16).ok()
}

impl TeamApps {
    /// The apps installed on the team, by name, as its answer `body` lists them.
    #[must_use]
    pub fn read_answer(&self, body: &[u8]) -> Vec<InstalledApp> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let value = answer.get("value").unwrap_or(&answer);
        let definitions = value.get("definitions");
        let mut apps: Vec<InstalledApp> = value
            .pointer("/teamsEntitlements")
            .and_then(|teams| teams.get(&self.team))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|entitlement| entitlement.get("state").and_then(Value::as_str) == Some("Installed"))
            .filter_map(|entitlement| {
                let id = text(entitlement, "id")?;
                let definition = definitions.and_then(|definitions| definitions.get(&id))?;
                Some(InstalledApp {
                    name: text(definition, "name")?,
                    developer: text(definition, "developerName").unwrap_or_default(),
                    description: text(definition, "shortDescription").unwrap_or_default(),
                    accent: definition.get("accentColor").and_then(Value::as_str).and_then(colour),
                    id,
                })
            })
            .collect();
        apps.sort_by_key(|app| app.name.to_lowercase());
        apps
    }
}

impl Request for TeamApps {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", "apps", "aggregatedEntitlements"]).with_query("appbarview", "userpinned")
    }

    fn body(&self) -> Option<Value> {
        Some(json!([self.team]))
    }
}

/// The app store's apps, as "Get more apps" opens it: `POST beta/users/apps/store` on the middle tier with the store's
/// first ask as the body, a read. Its answer lists each category's apps by id (`appCategoriesDictionary.<category>.apps`,
/// a hundred at most); their names come from [`AppDefinitions`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppStore;

/// The categories of the store whose apps "Add an app" offers, in this order: Teams' popular apps and Microsoft's own.
const STORE_CATEGORIES: [&str; 2] = ["popularApps", "microsoft"];

impl AppStore {
    /// The ids of the apps its answer `body` lists in [`STORE_CATEGORIES`], each once, in that order.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<String> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = Vec::new();
        for category in STORE_CATEGORIES {
            let listed = answer
                .pointer(&format!("/appCategoriesDictionary/{category}/apps"))
                .and_then(Value::as_array);
            for id in listed.into_iter().flatten().filter_map(|app| text(app, "id")) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }
}

impl Request for AppStore {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "users", "apps", "store"])
    }

    fn body(&self) -> Option<Value> {
        Some(json!({
            "appCategories": [],
            "appFilter": {"categories": [], "metaCapabilities": []},
            "defaultAppsSortType": "RankedByLocale",
            "appsRankingSource": "TenantBased",
            "appsRankingDictionary": {"popularApps": "Default"},
            "appsStoreOptions": {
                "includeIndustries": true,
                "includeSuggestedApps": true,
                "includeSubscriptionApps": true,
                "includeTotalAppsCount": true,
                "includeFrequentCollaboratorsApps": true,
                "includePluginsForCopilot": false,
                "includePreinstalledApps": true,
                "includeAggregatedApps": false,
                "includeAgentBlueprints": false,
            },
        }))
    }
}

/// Apps' definitions by id: `POST beta/users/apps/batchedDefinitions?includeblockedapps=true` on the middle tier with
/// the ids, a read, answered with a list of definitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppDefinitions {
    pub ids: Vec<String>,
}

/// How many ids one [`AppDefinitions`] asks for at most, as the web client's store asked for 47.
pub const DEFINITIONS_PAGE: usize = 50;

/// An app the store offers: what "Add an app" shows, and the definition adding it sends. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct StoreApp {
    pub app: InstalledApp,
    pub definition: crate::AppDefinition,
    /// Whether it brings a bot onto the team.
    pub has_bot: bool,
}

impl std::fmt::Debug for StoreApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreApp")
            .field("id", &self.app.id)
            .finish_non_exhaustive()
    }
}

impl AppDefinitions {
    /// The apps its answer `body` defines, in its order; a definition with no name, or one [`crate::AppDefinition`]
    /// doesn't take, is left out.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<StoreApp> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|definition| {
                let app = InstalledApp {
                    id: text(definition, "id")?,
                    name: text(definition, "name")?,
                    developer: text(definition, "developerName").unwrap_or_default(),
                    description: text(definition, "shortDescription").unwrap_or_default(),
                    accent: definition.get("accentColor").and_then(Value::as_str).and_then(colour),
                };
                let definition = crate::AppDefinition::new(definition.as_object()?.clone())?;
                Some(StoreApp {
                    has_bot: !definition.bots().is_empty(),
                    app,
                    definition,
                })
            })
            .collect()
    }
}

impl Request for AppDefinitions {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "users", "apps", "batchedDefinitions"]).with_query("includeblockedapps", "true")
    }

    fn body(&self) -> Option<Value> {
        Some(json!(self.ids))
    }
}

/// The picture Teams makes for a team from its name, which "Remove" in Change picture sends as the team's picture:
/// `GET beta/teams/defaultProfilePicture?displayName=<name>&highResolution=true` on the middle tier, answered with a JPEG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultTeamPicture {
    pub name: String,
}

impl Read for DefaultTeamPicture {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", "defaultProfilePicture"])
            .with_query("displayName", &self.name)
            .with_query("highResolution", "true")
    }
}

/// The locale the templates are read in and a team from one is made in, as the web client asked for them.
pub const TEMPLATE_LOCALE: &str = "en-US";

/// Teams' team templates: `GET teamtemplates/v1.0/en-us` on the provisioning service.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TeamTemplates;

/// A team template as "Create a team from a template" shows it. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamTemplateCard {
    /// Its id (`com.microsoft.teams.template.ManageAProject`).
    pub id: String,
    pub name: String,
    /// What it's for, in a line.
    pub summary: String,
    pub description: String,
    /// Its first category (`General`, `Retail`).
    pub category: String,
}

impl std::fmt::Debug for TeamTemplateCard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamTemplateCard")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl TeamTemplates {
    /// The templates its answer `body` lists (`value`), in its order.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<TeamTemplateCard> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        answer
            .get("value")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|template| {
                Some(TeamTemplateCard {
                    id: text(template, "id")?,
                    name: text(template, "name")?,
                    summary: text(template, "shortDescription").unwrap_or_default(),
                    description: text(template, "description").unwrap_or_default(),
                    category: template
                        .get("categories")
                        .and_then(Value::as_array)
                        .and_then(|categories| categories.first())
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
            })
            .collect()
    }
}

impl Read for TeamTemplates {
    fn service(&self) -> Service {
        Service::Provisioning
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["teamtemplates", "v1.0", "en-us"])
    }
}

/// One template's channels and apps: `GET teamtemplates/v1.0/<id>/Public/en-US` on the provisioning service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamTemplateRead {
    pub id: String,
}

/// A template's channel. `Debug` leaves the words out.
#[derive(Clone, PartialEq, Eq)]
pub struct TemplateChannel {
    /// Its id in the template (`General`, `com.microsoft.teams.template.ManageAProject.channel1`).
    pub id: String,
    pub name: String,
}

impl std::fmt::Debug for TemplateChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemplateChannel")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// What a template makes: its channels, and its apps by id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TeamTemplateDetails {
    pub channels: Vec<TemplateChannel>,
    pub apps: Vec<String>,
}

impl TeamTemplateRead {
    /// What its answer `body` says (`value.teamDefinition`).
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<TeamTemplateDetails> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        let team = answer.pointer("/value/teamDefinition")?;
        let listed = |key: &str| team.get(key).and_then(Value::as_array).into_iter().flatten();
        Some(TeamTemplateDetails {
            channels: listed("channels")
                .filter_map(|channel| {
                    Some(TemplateChannel {
                        id: text(channel, "id")?,
                        name: text(channel, "displayName")?,
                    })
                })
                .collect(),
            apps: listed("apps").filter_map(|app| text(app, "id")).collect(),
        })
    }
}

impl Read for TeamTemplateRead {
    fn service(&self) -> Service {
        Service::Provisioning
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["teamtemplates", "v1.0", self.id.as_str(), "Public", TEMPLATE_LOCALE])
    }
}

/// The Microsoft 365 groups the account owns that a team could be made of: `GET beta/teams/usergroups?teamType=undefined`
/// on the middle tier, sent with the web client's literal `undefined`. Only an empty list (`[]`) has been seen, so an
/// entry's keys are unknown and only how many there are is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwnedGroups;

impl OwnedGroups {
    /// How many groups its answer `body` lists; `None` when it isn't a list.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<usize> {
        let answer: Value = serde_json::from_slice(body).ok()?;
        answer.as_array().map(Vec::len)
    }
}

impl Read for OwnedGroups {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", "usergroups"]).with_query("teamType", "undefined")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_installed_apps_are_those_installed_on_the_team_by_name() {
        let body = json!({"value": {
            "teamsEntitlements": {"19:t@thread.tacv2": [
                {"id": "b", "state": "Installed"},
                {"id": "a", "state": "Installed", "isNewGlobalPreInstalledApp": true},
                {"id": "p", "state": "InstalledAndPermanent"},
                {"id": "n", "state": "NotInstalled"},
                {"id": "missing", "state": "Installed"},
            ]},
            "definitions": {
                "a": {"name": "Zed", "developerName": "Maker", "shortDescription": "Z", "accentColor": "#6264A7"},
                "b": {"name": "Alpha", "accentColor": "blue"},
                "p": {"name": "Built in"},
                "n": {"name": "Offered"},
            },
        }});
        let apps = TeamApps {
            team: "19:t@thread.tacv2".to_owned(),
        }
        .read_answer(body.to_string().as_bytes());
        let names: Vec<_> = apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Zed"]);
        assert_eq!(apps[1].accent, Some(0x0062_64a7));
        assert_eq!(apps[1].developer, "Maker");
        assert_eq!(apps[0].accent, None);
        assert!(format!("{:?}", apps[1]).contains("\"a\""));
        assert!(!format!("{:?}", apps[1]).contains("Zed"));
        assert!(TeamApps { team: "x".to_owned() }.read_answer(b"nope").is_empty());
    }

    #[test]
    fn the_apps_read_is_a_post_naming_the_team() {
        let read = TeamApps {
            team: "19:t@thread.tacv2".to_owned(),
        };
        assert_eq!(read.method(), Method::Post);
        assert_eq!(read.body(), Some(json!(["19:t@thread.tacv2"])));
        assert_eq!(
            read.endpoint().segments(),
            ["beta", "teams", "apps", "aggregatedEntitlements"]
        );
        assert_eq!(read.service(), Service::MiddleTier);
    }

    #[test]
    fn templates_and_a_templates_channels_and_apps_are_read() {
        let list = json!({"value": [
            {"id": "t1", "name": "Manage a Project", "shortDescription": "Coordinate your project.",
             "description": "Long", "categories": ["General"]},
            {"id": "t2", "name": "Bare"},
            {"name": "No id"},
        ]});
        let cards = TeamTemplates::read_answer(list.to_string().as_bytes());
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].category, "General");
        assert_eq!(cards[1].summary, "");
        assert!(!format!("{:?}", cards[0]).contains("Manage"));
        assert!(TeamTemplates::read_answer(b"x").is_empty());
        assert_eq!(TeamTemplates.endpoint().segments(), ["teamtemplates", "v1.0", "en-us"]);
        assert_eq!(TeamTemplates.service(), Service::Provisioning);
        let details = json!({"value": {"teamDefinition": {
            "channels": [{"id": "General", "displayName": "General"}, {"id": "c1", "displayName": "Planning"}, {"id": "x"}],
            "apps": [{"id": "com.microsoft.teamspace.tab.planner"}, {"name": "no id"}],
        }}});
        let read = TeamTemplateRead { id: "t1".to_owned() };
        let found = TeamTemplateRead::read_answer(details.to_string().as_bytes()).expect("details");
        assert_eq!(found.channels.len(), 2);
        assert_eq!(found.channels[1].name, "Planning");
        assert!(!format!("{:?}", found.channels[1]).contains("Planning"));
        assert_eq!(found.apps, ["com.microsoft.teamspace.tab.planner"]);
        assert_eq!(TeamTemplateRead::read_answer(b"{}"), None);
        assert_eq!(
            read.endpoint().segments(),
            ["teamtemplates", "v1.0", "t1", "Public", "en-US"]
        );
        assert_eq!(read.service(), Service::Provisioning);
    }

    #[test]
    fn the_store_lists_popular_and_microsoft_apps_once() {
        let store = json!({"appCategoriesDictionary": {
            "popularApps": {"apps": [{"id": "a"}, {"id": "b"}]},
            "microsoft": {"apps": [{"id": "b"}, {"id": "c"}, {"version": "1"}]},
            "all": {"apps": [{"id": "z"}]},
        }});
        assert_eq!(AppStore::read_answer(store.to_string().as_bytes()), ["a", "b", "c"]);
        assert!(AppStore::read_answer(b"x").is_empty());
        assert_eq!(AppStore.method(), Method::Post);
        assert!(
            AppStore
                .body()
                .is_some_and(|body| body.get("appsStoreOptions").is_some())
        );
        assert_eq!(AppStore.endpoint().segments(), ["beta", "users", "apps", "store"]);
        assert_eq!(AppStore.service(), Service::MiddleTier);
    }

    #[test]
    fn the_store_s_definitions_name_its_apps_and_say_which_bring_a_bot() {
        let definitions = json!([
            {"id": "a", "name": "Updates", "developerName": "Microsoft Corporation", "bots": [{"id": "b1"}]},
            {"id": "t", "name": "Pages", "staticTabs": []},
            {"id": "x"},
            {"id": "bad", "name": "Bad", "bots": [{"id": 1}]},
        ]);
        let apps = AppDefinitions::read_answer(definitions.to_string().as_bytes());
        assert_eq!(apps.len(), 2);
        assert!(apps[0].has_bot);
        assert!(!apps[1].has_bot);
        assert_eq!(apps[0].definition.id(), "a");
        assert!(!format!("{:?}", apps[0]).contains("Updates"));
        assert!(AppDefinitions::read_answer(b"{}").is_empty());
        let read = AppDefinitions {
            ids: vec!["a".to_owned()],
        };
        assert_eq!(read.body(), Some(json!(["a"])));
        assert_eq!(read.method(), Method::Post);
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(
            read.endpoint().segments(),
            ["beta", "users", "apps", "batchedDefinitions"]
        );
        let picture = DefaultTeamPicture {
            name: "Lab team".to_owned(),
        };
        assert_eq!(
            picture.endpoint().segments(),
            ["beta", "teams", "defaultProfilePicture"]
        );
        assert_eq!(picture.service(), Service::MiddleTier);
    }

    #[test]
    fn the_groups_one_owns_are_only_counted() {
        assert_eq!(OwnedGroups::read_answer(b"[]"), Some(0));
        assert_eq!(OwnedGroups::read_answer(b"[{}, {}]"), Some(2));
        assert_eq!(OwnedGroups::read_answer(b"{}"), None);
        assert_eq!(
            OwnedGroups.endpoint().query(),
            [("teamType".to_owned(), "undefined".to_owned())]
        );
        assert_eq!(OwnedGroups.service(), Service::MiddleTier);
        assert_eq!(colour("#zzzzzz"), None);
    }
}
