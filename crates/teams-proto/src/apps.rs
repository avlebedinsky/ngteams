//! What the Apps page, an app's details and a chat's apps read (`docs/protocol/apps.md`), as browser Teams reads them:
//! the apps the account added for itself, the apps added to a chat, the store's sections (the apps the organization
//! added, popular apps, Microsoft's) with each app's rating, and what an app's definition says about it: what it does in
//! Teams, where it can be added, the permissions it asks for, its version and its links. The writes that add and remove
//! an app are [`crate::TeamWrite`]'s, as a team's are.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::AppDefinition;
use crate::endpoint::{Endpoint, Method, Service};
use crate::read::Read;
use crate::request::Request;
use crate::team_apps::{StoreApp, store_app};

/// The apps the account added for itself: `POST beta/users/apps/aggregatedEntitlements?appbarview=userpinned` on the
/// middle tier with an empty list of teams, a read. Its answer holds the account's entitlements by state
/// (`userEntitlements.<object id>`) and their definitions (`definitions.<app id>`); the account's own apps are those
/// `Installed` or `PreConsented`, as "Manage your apps" lists them, the rest being Teams' built-in ones
/// (`InstalledAndPermanent`, `InstalledAndDeprecated`) or the store's offers (`Featured`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserApps;

impl UserApps {
    /// The apps the account added, by name, as its answer `body` lists them.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<StoreApp> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let value = answer.get("value").unwrap_or(&answer);
        let entitlements: Vec<&Value> = value
            .get("userEntitlements")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(serde_json::Map::values)
            .filter_map(Value::as_array)
            .flatten()
            .collect();
        let definitions = value.get("definitions");
        listed(entitlements, &["Installed", "PreConsented"], |id| {
            definitions.and_then(|definitions| definitions.get(id))
        })
    }

    /// Every app its answer `body` defines (`definitions`), whatever the account's entitlement to it: Teams' own and
    /// the store's offers too, as Teams finds a bot's app among them.
    #[must_use]
    pub fn defined(body: &[u8]) -> Vec<StoreApp> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let value = answer.get("value").unwrap_or(&answer);
        value
            .get("definitions")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(serde_json::Map::values)
            .filter_map(store_app)
            .collect()
    }
}

impl Request for UserApps {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn method(&self) -> Method {
        Method::Post
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "users", "apps", "aggregatedEntitlements"]).with_query("appbarview", "userpinned")
    }

    fn body(&self) -> Option<Value> {
        Some(json!([]))
    }
}

/// A chat's apps: `GET beta/chats/<chat>/apps/chatentitlements` on the middle tier. Its answer lists the chat's
/// entitlements by state (`value.entitlements`) and their definitions (`value.definitions`, a list); the chat's added
/// apps are those `Installed`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatApps {
    pub chat: String,
}

impl ChatApps {
    /// The apps added to the chat, by name, as its answer `body` lists them.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Vec<StoreApp> {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Vec::new();
        };
        let value = answer.get("value").unwrap_or(&answer);
        let entitlements: Vec<&Value> = value
            .get("entitlements")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let definitions: Vec<&Value> = value
            .get("definitions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        listed(entitlements, &["Installed"], |id| {
            definitions
                .iter()
                .copied()
                .find(|definition| definition.get("id").and_then(Value::as_str) == Some(id))
        })
    }
}

impl Read for ChatApps {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "chats", self.chat.as_str(), "apps", "chatentitlements"])
    }
}

/// The apps among `entitlements` in one of `states`, each with the definition `definition` finds for its id, by name;
/// one without a readable definition is left out.
fn listed<'a>(
    entitlements: Vec<&Value>,
    states: &[&str],
    definition: impl Fn(&str) -> Option<&'a Value>,
) -> Vec<StoreApp> {
    let mut apps: Vec<StoreApp> = entitlements
        .into_iter()
        .filter(|entitlement| {
            entitlement
                .get("state")
                .and_then(Value::as_str)
                .is_some_and(|state| states.contains(&state))
        })
        .filter_map(|entitlement| store_app(definition(entitlement.get("id")?.as_str()?)?))
        .collect();
    apps.sort_by_key(|app| app.app.name.to_lowercase());
    apps.dedup_by(|one, other| one.app.id == other.app.id);
    apps
}

/// An app's rating in the store: its average in tenths of a star (`41` is 4.1) and how many rated it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rating {
    pub tenths: u16,
    pub count: u64,
}

/// The store's sections as the Apps page shows them, each a list of app ids in the store's order: the apps the
/// organization added (`adminPreInstalledApps`, "Added by your org"), the popular ones (`popularApps`) and Microsoft's
/// (`microsoft`); and each listed app's rating.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoreSections {
    pub org: Vec<String>,
    pub popular: Vec<String>,
    pub microsoft: Vec<String>,
    pub ratings: BTreeMap<String, Rating>,
}

impl StoreSections {
    /// The sections of the store's answer `body` ([`crate::AppStore`]'s read).
    #[must_use]
    pub fn read(body: &[u8]) -> Self {
        let Ok(answer) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        let mut sections = Self::default();
        let mut section = |category: &str| -> Vec<String> {
            let listed = answer
                .pointer(&format!("/appCategoriesDictionary/{category}/apps"))
                .and_then(Value::as_array);
            let mut ids = Vec::new();
            for app in listed.into_iter().flatten() {
                let Some(id) = app.get("id").and_then(Value::as_str).filter(|id| !id.is_empty()) else {
                    continue;
                };
                if let Some(rating) = rating(app) {
                    sections.ratings.insert(id.to_owned(), rating);
                }
                if !ids.iter().any(|known| known == id) {
                    ids.push(id.to_owned());
                }
            }
            ids
        };
        let org = section("adminPreInstalledApps");
        let popular = section("popularApps");
        let microsoft = section("microsoft");
        sections.org = org;
        sections.popular = popular;
        sections.microsoft = microsoft;
        sections
    }

    /// Every app id the sections list, each once, in their order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = Vec::new();
        for id in self.org.iter().chain(&self.popular).chain(&self.microsoft) {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    }
}

fn rating(app: &Value) -> Option<Rating> {
    let rating = app.get("rating")?;
    let average = rating
        .get("average")?
        .as_f64()
        .filter(|average| (0.0..=5.0).contains(average))?;
    // 0..=50 by the filter above
    #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "0..=50")]
    let tenths = (average * 10.0).round() as u16;
    Some(Rating {
        tenths,
        count: rating.get("count").and_then(Value::as_u64).unwrap_or_default(),
    })
}

/// What an app does in Teams, as its details' "Capabilities in Teams" lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppCapability {
    /// It has a bot (`bots`).
    Bot,
    /// It puts its content into messages (`inputExtensions`, or a manifest's `composeExtensions`).
    Messaging,
    /// It has a page of its own (a `staticTabs` entry with no scopes or the `personal` one).
    PersonalApp,
    /// It can be a tab in a chat or channel (`configurableTabs`, `galleryTabs`).
    Tab,
}

impl AppCapability {
    /// Its name, as Teams shows it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bot => "Bot",
            Self::Messaging => "Messaging",
            Self::PersonalApp => "Personal app",
            Self::Tab => "Tab",
        }
    }

    /// Its line, as Teams shows it.
    #[must_use]
    pub fn line(self) -> &'static str {
        match self {
            Self::Bot => "Chat using prompts to complete simple tasks.",
            Self::Messaging => "Insert app content in messages to send more than just text.",
            Self::PersonalApp => "Use the app privately outside of a chat, channel, or meeting.",
            Self::Tab => "Collaborate in a chat or channel by adding a tab for the app.",
        }
    }
}

/// What an app may read and do, as its details' "Permissions overview" says it in plain words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionOverview {
    /// Under "Permission to read and access".
    pub reads: Vec<&'static str>,
    /// Under "Permission to perform these actions".
    pub actions: Vec<&'static str>,
}

/// Where an app can be added: for the account itself, to a chat, to a team.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AppPlaces {
    pub personal: bool,
    pub chat: bool,
    pub team: bool,
}

impl AppDefinition {
    fn text_of(&self, key: &str) -> Option<&str> {
        self.field(key).and_then(Value::as_str).filter(|text| !text.is_empty())
    }

    fn listed(&self, key: &str) -> impl Iterator<Item = &Value> {
        self.field(key).and_then(Value::as_array).into_iter().flatten()
    }

    /// Who makes it (`developerName`).
    #[must_use]
    pub fn developer(&self) -> Option<&str> {
        self.text_of("developerName")
    }

    /// What it is, in a line (`shortDescription`).
    #[must_use]
    pub fn short_description(&self) -> Option<&str> {
        self.text_of("shortDescription")
    }

    /// What it is, at length (`longDescription`).
    #[must_use]
    pub fn long_description(&self) -> Option<&str> {
        self.text_of("longDescription")
    }

    /// Its version (`version`).
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.text_of("version")
    }

    /// Its privacy policy (`privacyUrl`), when it is an `https` link.
    #[must_use]
    pub fn privacy_url(&self) -> Option<&str> {
        self.text_of("privacyUrl").filter(|url| is_https(url))
    }

    /// Its terms of use (`termsOfUseUrl`), when it is an `https` link.
    #[must_use]
    pub fn terms_url(&self) -> Option<&str> {
        self.text_of("termsOfUseUrl").filter(|url| is_https(url))
    }

    /// Its maker's site (`developerUrl`), when it is an `https` link.
    #[must_use]
    pub fn developer_url(&self) -> Option<&str> {
        self.text_of("developerUrl").filter(|url| is_https(url))
    }

    /// What it does in Teams, in Teams' order.
    #[must_use]
    pub fn capabilities(&self) -> Vec<AppCapability> {
        let has = |key: &str| self.listed(key).next().is_some();
        [
            (AppCapability::Bot, has("bots")),
            (
                AppCapability::Messaging,
                has("inputExtensions") || has("composeExtensions"),
            ),
            (AppCapability::PersonalApp, self.has_personal_page()),
            (AppCapability::Tab, has("configurableTabs") || has("galleryTabs")),
        ]
        .into_iter()
        .filter_map(|(capability, has)| has.then_some(capability))
        .collect()
    }

    /// Where it can be added, by the scopes of its bots, tabs and message extensions (any case): `personal` (or a page
    /// of its own), `groupChat`, `team`.
    #[must_use]
    pub fn places(&self) -> AppPlaces {
        let mut places = AppPlaces {
            personal: self.has_personal_page(),
            ..AppPlaces::default()
        };
        let lists = [
            "bots",
            "inputExtensions",
            "composeExtensions",
            "configurableTabs",
            "galleryTabs",
            "staticTabs",
        ];
        let scopes = lists
            .iter()
            .flat_map(|list| self.listed(list))
            .filter_map(|entry| entry.get("scopes").and_then(Value::as_array))
            .flatten()
            .filter_map(Value::as_str);
        for scope in scopes {
            match scope.to_ascii_lowercase().as_str() {
                "personal" => places.personal = true,
                "groupchat" => places.chat = true,
                "team" => places.team = true,
                _ => {}
            }
        }
        places
    }

    /// Whether it has a page of its own: a `staticTabs` entry with no scopes, or with `personal` among them.
    fn has_personal_page(&self) -> bool {
        self.listed("staticTabs")
            .any(|tab| match tab.get("scopes").and_then(Value::as_array) {
                None => true,
                Some(scopes) => scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|scope| scope.eq_ignore_ascii_case("personal")),
            })
    }

    /// The permissions it asks for, as its definition names them: its manifest's (`permissions`: `Identity`,
    /// `MessageTeamMembers`) and its resource-specific ones (`authorization.permissions.resourceSpecific[].name`), each
    /// once.
    #[must_use]
    pub fn permissions(&self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let specific = self
            .field("authorization")
            .and_then(|authorization| authorization.pointer("/permissions/resourceSpecific"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|permission| permission.get("name"));
        for name in self.listed("permissions").chain(specific).filter_map(Value::as_str) {
            if !name.is_empty() && !names.iter().any(|known| known == name) {
                names.push(name.to_owned());
            }
        }
        names
    }

    /// Its permissions in Teams' plain words: who the account is (a web application or the `Identity` permission),
    /// what is in its chats (a bot or a resource-specific permission); sending messages (a bot), or only
    /// notifications (activities).
    #[must_use]
    pub fn permission_overview(&self) -> PermissionOverview {
        let bots = self.listed("bots").next().is_some();
        let identity = self.field("webApplicationInfo").is_some()
            || self.listed("permissions").any(|name| name.as_str() == Some("Identity"));
        let specific = self
            .permissions()
            .iter()
            .any(|name| name != "Identity" && name != "MessageTeamMembers");
        let activities = self.field("activities").is_some_and(|activities| {
            ["activityTypes", "activityItems"].iter().any(|key| {
                activities
                    .get(*key)
                    .and_then(Value::as_array)
                    .is_some_and(|items| !items.is_empty())
            })
        });
        let mut overview = PermissionOverview::default();
        if identity {
            overview.reads.push("Information related to you");
        }
        if bots || specific {
            overview.reads.push("Information in your chats, channels, and meetings");
        }
        if bots {
            overview.actions.push("Send and receive messages and notifications");
        } else if activities {
            overview.actions.push("Send you notifications");
        }
        overview
    }

    /// How many languages it is offered in (`supportedLanguages`).
    #[must_use]
    pub fn language_count(&self) -> usize {
        self.listed("supportedLanguages").count()
    }

    /// The bot it brings that people can mention in a group chat (`groupChat` scope) or a team's channel (`team`), as
    /// its MRI: one that isn't notification-only, as Teams' picker offers none of those.
    #[must_use]
    pub fn mentionable_bot(&self, team: bool) -> Option<crate::Mri> {
        self.bot_with_scope(if team { "team" } else { "groupchat" })
    }

    /// The bot it brings that a person can chat with one to one (a bot with the `personal` scope that isn't
    /// notification-only), as its MRI.
    #[must_use]
    pub fn personal_bot(&self) -> Option<crate::Mri> {
        self.bot_with_scope("personal")
    }

    /// The first bot it brings with `scope` (in any case) that isn't notification-only, as its MRI.
    fn bot_with_scope(&self, scope: &str) -> Option<crate::Mri> {
        self.listed("bots").find_map(|bot| {
            let scoped = bot
                .get("scopes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|listed| listed.eq_ignore_ascii_case(scope));
            let quiet = bot.get("isNotificationOnly").and_then(Value::as_bool) == Some(true);
            let id = bot.get("id")?.as_str().filter(|id| !id.is_empty())?;
            (scoped && !quiet).then(|| crate::Mri::new(format!("28:{id}")))
        })
    }
}

fn is_https(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(value: &Value) -> AppDefinition {
        AppDefinition::new(value.as_object().cloned().expect("an object")).expect("a definition")
    }

    #[test]
    fn the_account_s_apps_are_those_it_added_by_name() {
        let body = json!({"value": {
            "userEntitlements": {"00000000-0000-4000-8000-00000000000a": [
                {"id": "b", "state": "Installed"},
                {"id": "a", "state": "Installed", "installedVersion": "1.0"},
                {"id": "p", "state": "InstalledAndPermanent"},
                {"id": "gone", "state": "Installed"},
            ]},
            "definitions": {
                "a": {"id": "a", "name": "Zed", "developerName": "Maker"},
                "b": {"id": "b", "name": "Alpha"},
                "p": {"id": "p", "name": "Built in"},
            },
        }});
        let apps = UserApps::read_answer(body.to_string().as_bytes());
        let names: Vec<_> = apps.iter().map(|app| app.app.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Zed"]);
        assert_eq!(apps[1].app.developer, "Maker");
        assert!(UserApps::read_answer(b"nope").is_empty());
        assert_eq!(UserApps.method(), Method::Post);
        assert_eq!(UserApps.body(), Some(json!([])));
        assert_eq!(UserApps.service(), Service::MiddleTier);
        assert_eq!(
            UserApps.endpoint().segments(),
            ["beta", "users", "apps", "aggregatedEntitlements"]
        );
    }

    #[test]
    fn the_account_s_apps_include_those_it_has_pre_consented() {
        let body = json!({"value": {
            "userEntitlements": {"00000000-0000-4000-8000-00000000000a": [
                {"id": "c", "state": "Installed"},
                {"id": "a", "state": "PreConsented", "installedVersion": "2.2.0"},
                {"id": "f", "state": "Featured"},
                {"id": "d", "state": "InstalledAndDeprecated"},
            ]},
            "definitions": {
                "a": {"id": "a", "name": "Made-up Approvals"},
                "c": {"id": "c", "name": "Made-up Copilot"},
                "f": {"id": "f", "name": "Made-up News"},
                "d": {"id": "d", "name": "Made-up Wiki"},
            },
        }});
        let apps = UserApps::read_answer(body.to_string().as_bytes());
        let names: Vec<_> = apps.iter().map(|app| app.app.name.as_str()).collect();
        assert_eq!(names, ["Made-up Approvals", "Made-up Copilot"]);
    }

    #[test]
    fn a_chat_s_apps_are_those_added_to_it() {
        let body = json!({"value": {
            "entitlements": [
                {"id": "x", "state": "Installed", "installedBy": "someone"},
                {"id": "x", "state": "Installed"},
                {"id": "y", "state": "PreConsented"},
                {"id": "z", "state": "Installed"},
            ],
            "definitions": [
                {"id": "x", "name": "Approvals made up", "bots": [{"id": "bx", "scopes": ["GroupChat"]}]},
                {"id": "y", "name": "Offered"},
            ],
        }});
        let apps = ChatApps::read_answer(body.to_string().as_bytes());
        assert_eq!(apps.len(), 1);
        assert!(apps[0].has_bot);
        assert!(ChatApps::read_answer(b"[]").is_empty());
        let read = ChatApps {
            chat: "19:c@thread.v2".to_owned(),
        };
        assert_eq!(read.service(), Service::MiddleTier);
        assert_eq!(
            read.endpoint().segments(),
            ["beta", "chats", "19:c@thread.v2", "apps", "chatentitlements"]
        );
    }

    #[test]
    fn the_store_s_sections_and_ratings_are_read() {
        let body = json!({"appCategoriesDictionary": {
            "adminPreInstalledApps": {"apps": [{"id": "org", "rating": {"count": 160_960, "average": 4.289}}]},
            "popularApps": {"apps": [{"id": "p1", "rating": {"average": 1.632}}, {"id": "p1"}, {"version": "1"}]},
            "microsoft": {"apps": [{"id": "org"}, {"id": "m1", "rating": {"average": 9.0, "count": 1}}]},
        }});
        let sections = StoreSections::read(body.to_string().as_bytes());
        assert_eq!(sections.org, ["org"]);
        assert_eq!(sections.popular, ["p1"]);
        assert_eq!(sections.microsoft, ["org", "m1"]);
        assert_eq!(sections.ids(), ["org", "p1", "m1"]);
        assert_eq!(
            sections.ratings.get("org"),
            Some(&Rating {
                tenths: 43,
                count: 160_960
            })
        );
        assert_eq!(sections.ratings.get("p1").map(|rating| rating.count), Some(0));
        assert_eq!(sections.ratings.get("m1"), None, "a rating past five stars isn't taken");
        assert_eq!(StoreSections::read(b"x"), StoreSections::default());
    }

    fn full_definition() -> AppDefinition {
        definition(&json!({
            "id": "a", "name": "Made-up", "developerName": "Maker", "shortDescription": "Short",
            "longDescription": "Long", "version": "2.2.0", "privacyUrl": "https://example.com/privacy",
            "termsOfUseUrl": "http://example.com/terms", "developerUrl": "not a link",
            "permissions": ["Identity", "Identity"],
            "authorization": {"permissions": {"resourceSpecific": [{"name": "ChatMessage.Read.Chat", "type": "Application"}]}},
            "bots": [{"id": "b1", "scopes": ["Team", "GroupChat"]}, {"id": "b2", "scopes": ["personal"]}],
            "inputExtensions": [{"botId": "b1", "scopes": ["Personal"]}],
            "staticTabs": [{"entityId": "home"}],
            "configurableTabs": [{"scopes": ["team"]}],
        }))
    }

    #[test]
    fn a_definition_says_what_the_app_does_and_where_it_goes() {
        let app = full_definition();
        assert_eq!(
            app.capabilities(),
            [
                AppCapability::Bot,
                AppCapability::Messaging,
                AppCapability::PersonalApp,
                AppCapability::Tab
            ]
        );
        assert_eq!(
            app.places(),
            AppPlaces {
                personal: true,
                chat: true,
                team: true
            }
        );
        assert_eq!(app.permissions(), ["Identity", "ChatMessage.Read.Chat"]);
        assert_eq!(app.version(), Some("2.2.0"));
        assert_eq!(app.developer(), Some("Maker"));
        assert_eq!(app.short_description(), Some("Short"));
        assert_eq!(app.long_description(), Some("Long"));
        assert_eq!(app.privacy_url(), Some("https://example.com/privacy"));
        assert_eq!(app.terms_url(), None, "only an https link is offered");
        assert_eq!(app.developer_url(), None);
        assert_eq!(app.personal_bot(), Some(crate::Mri::new("28:b2")));
    }

    #[test]
    fn a_definition_s_permissions_and_capabilities_read_in_teams_words() {
        let app = full_definition();
        assert_eq!(
            app.permission_overview(),
            PermissionOverview {
                reads: vec![
                    "Information related to you",
                    "Information in your chats, channels, and meetings"
                ],
                actions: vec!["Send and receive messages and notifications"],
            }
        );
        let notifier = definition(&json!({"id": "n", "supportedLanguages": ["en-us", "de"],
            "activities": {"activityTypes": [{"type": "x"}]}}));
        assert_eq!(
            notifier.permission_overview(),
            PermissionOverview {
                reads: Vec::new(),
                actions: vec!["Send you notifications"],
            }
        );
        assert_eq!(notifier.language_count(), 2);
        let polls = definition(&json!({"id": "p", "staticTabs": [{"scopes": ["GroupChat", "Team"]}],
            "galleryTabs": [{"scopes": ["GroupChat"]}]}));
        assert_eq!(
            polls.capabilities(),
            [AppCapability::Tab],
            "a page only for chats isn't a personal app"
        );
        assert_eq!(
            polls.places(),
            AppPlaces {
                personal: false,
                chat: true,
                team: true
            }
        );
        let bare =
            definition(&json!({"id": "t", "bots": [{"id": "n", "scopes": ["personal"], "isNotificationOnly": true}]}));
        assert_eq!(bare.capabilities(), [AppCapability::Bot]);
        assert_eq!(bare.personal_bot(), None, "a notification-only bot isn't chatted with");
        assert!(bare.permissions().is_empty());
        for capability in [
            AppCapability::Bot,
            AppCapability::Messaging,
            AppCapability::PersonalApp,
            AppCapability::Tab,
        ] {
            assert!(!capability.name().is_empty() && capability.line().ends_with('.'));
        }
    }

    #[test]
    fn a_bot_people_can_mention_is_one_for_the_place_that_says_something() {
        let app = full_definition();
        assert_eq!(
            app.mentionable_bot(true).map(|bot| bot.as_str().to_owned()),
            Some("28:b1".to_owned())
        );
        assert_eq!(
            app.mentionable_bot(false).map(|bot| bot.as_str().to_owned()),
            Some("28:b1".to_owned())
        );
        let quiet = definition(&json!({"id": "q", "bots": [
            {"id": "n", "scopes": ["team", "groupChat"], "isNotificationOnly": true},
            {"id": "", "scopes": ["team"]},
            {"id": "p", "scopes": ["personal"]},
        ]}));
        assert_eq!(quiet.mentionable_bot(true), None);
        assert_eq!(quiet.mentionable_bot(false), None);
    }

    #[test]
    fn every_app_the_account_s_answer_defines_is_read_whatever_its_entitlement() {
        let body = json!({"value": {
            "userEntitlements": {"00000000-0000-4000-8000-00000000000a": [{"id": "a", "state": "Installed"}]},
            "definitions": {
                "a": {"id": "a", "name": "Made-up Added"},
                "o": {"id": "o", "name": "Made-up Offered", "bots": [{"id": "bo", "scopes": ["personal"]}]},
                "x": {"id": "x"},
            },
        }});
        let mut names: Vec<_> = UserApps::defined(body.to_string().as_bytes())
            .into_iter()
            .map(|app| app.app.name)
            .collect();
        names.sort();
        assert_eq!(names, ["Made-up Added", "Made-up Offered"]);
        assert!(UserApps::defined(b"nope").is_empty());
        assert!(UserApps::defined(b"{}").is_empty());
    }
}
