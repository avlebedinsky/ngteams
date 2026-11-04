//! Service data used by the native workflow views. Unknown definitions are preserved, never converted into a made-up
//! workflow. Exact string values establish channel association; titles and substring matches grant no association.
use serde_json::Value;
use teams_proto::WorkflowFailure;

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub description: String,
    pub properties: Value,
}

impl Card {
    pub fn from_template(value: &Value) -> Option<Self> {
        Some(Self {
            id: value.get("id")?.as_str()?.to_owned(),
            name: value.get("name")?.as_str()?.to_owned(),
            description: value.get("description")?.as_str()?.to_owned(),
            properties: value.clone(),
        })
    }

    /// A template of the Workflows app's own gallery (`galleries/public/templates`): its `name` and the
    /// `properties` the gallery gives, with the definition and connections expanded.
    pub fn from_gallery(value: &Value) -> Option<Self> {
        let properties = value.get("properties")?;
        Some(Self {
            id: value.get("name")?.as_str()?.to_owned(),
            name: properties.get("displayName")?.as_str()?.to_owned(),
            description: properties
                .get("description")
                .or_else(|| properties.get("summary"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            properties: properties.clone(),
        })
    }

    /// The gallery's categories of a template (`categoryNames`). The channel catalog's `categories` are display
    /// names ("Notifications", "Productivity") that don't say its kind.
    fn categories(&self) -> impl Iterator<Item = &str> {
        self.properties
            .get("categoryNames")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
    }

    /// Which of Teams' kinds of template this is, by its gallery category: its card's icon. A schedule is a
    /// schedule; a template filed under updates and actions both shows as an update, as the app draws it.
    pub fn kind(&self) -> TemplateKind {
        // A card without its categories (the channel catalog's) takes them from the gallery's, by template id.
        if self.categories().next().is_none() {
            return if SCHEDULES.contains(&self.id.as_str()) {
                TemplateKind::Schedule
            } else {
                TemplateKind::GetUpdates
            };
        }
        if self.is_kind(TemplateKind::Schedule) {
            TemplateKind::Schedule
        } else if self.is_kind(TemplateKind::TakeAction) && !self.is_kind(TemplateKind::GetUpdates) {
            TemplateKind::TakeAction
        } else {
            TemplateKind::GetUpdates
        }
    }

    /// Whether the gallery files the template under `kind` (the Templates tab's Take action, Get updates and
    /// Schedule).
    pub fn is_kind(&self, kind: TemplateKind) -> bool {
        self.categories().any(|category| match kind {
            TemplateKind::Schedule => category == "schedule",
            TemplateKind::GetUpdates => category == "get_updates",
            TemplateKind::TakeAction => matches!(category, "take_actions" | "take_action"),
        })
    }

    pub fn from_flow(value: &Value) -> Option<Self> {
        let properties = value.get("properties")?;
        Some(Self {
            id: value.get("name")?.as_str()?.to_owned(),
            name: properties.get("displayName")?.as_str()?.to_owned(),
            // A saved workflow keeps its description in its definition, where Teams writes it.
            description: properties
                .get("description")
                .or_else(|| properties.pointer("/definition/description"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            properties: properties.clone(),
        })
    }

    pub fn matches(&self, query: &str) -> bool {
        let text = format!("{} {}", self.name, self.description).to_lowercase();
        query.split_whitespace().all(|term| text.contains(&term.to_lowercase()))
    }

    pub fn in_channel(&self, team: &str, channel: &str) -> bool {
        let Some(definition) = self.properties.get("definition") else {
            return false;
        };
        destination(definition, definition, team, channel)
    }

    pub fn is_ai(&self) -> bool {
        uses_ai_connector(self.properties.get("definition").unwrap_or(&Value::Null))
            || self
                .properties
                .get("connectionReferences")
                .and_then(Value::as_object)
                .is_some_and(|references| {
                    references
                        .values()
                        .any(|reference| reference.get("id").is_some_and(ai_id))
                })
    }

    /// The connectors (`shared_teams`) a template's card shows the icons of: the channel catalog's icons in its
    /// order, or the gallery's connections in the reverse of theirs, as the Workflows app draws them.
    pub fn connectors(&self) -> Vec<&'static str> {
        let mut apps = Vec::new();
        if let Some(references) = self.properties.get("connectionReferences").and_then(Value::as_object)
            && self.properties.get("icons").is_none()
        {
            for reference in references.values().rev() {
                let name = reference
                    .pointer("/api/name")
                    .or_else(|| reference.get("apiName"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        reference
                            .get("id")
                            .and_then(Value::as_str)
                            .and_then(|id| id.rsplit('/').next())
                    });
                if let Some(api) = name.and_then(known_connector)
                    && !apps.contains(&api)
                {
                    apps.push(api);
                }
            }
            return apps;
        }
        for icon in self
            .properties
            .get("icons")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(icon) = icon
                .as_str()
                .filter(|url| url.starts_with("https://static.powerapps.com/"))
            else {
                continue;
            };
            // The icon's folder names its connector.
            let Some(label) = icon
                .rsplit('/')
                .nth(1)
                .and_then(|folder| known_connector(&format!("shared_{folder}")))
            else {
                continue;
            };
            if !apps.contains(&label) {
                apps.push(label);
            }
        }
        apps
    }

    pub fn enabled(&self) -> bool {
        self.properties.get("state").and_then(Value::as_str) == Some("Started")
    }
}

/// Teams' three kinds of template: Take action, Get updates and Schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateKind {
    TakeAction,
    GetUpdates,
    Schedule,
}

/// The connectors whose icons and names the app knows, by API name, in the order the Templates tab's Filter lists
/// them.
pub const CONNECTORS: [(&str, &str); 17] = [
    ("shared_teams", "Microsoft Teams"),
    ("shared_office365users", "Office 365 Users"),
    ("shared_planner", "Planner"),
    ("shared_sharepointonline", "SharePoint"),
    ("shared_office365", "Office 365 Outlook"),
    ("shared_rss", "RSS"),
    ("shared_m365copilotv2", "AI Workflows"),
    ("shared_visualstudioteamservices", "Azure DevOps"),
    ("shared_office365groups", "Office 365 Groups"),
    ("shared_pagerduty", "PagerDuty"),
    ("shared_service-now", "ServiceNow"),
    ("shared_conversionservice", "Content Conversion"),
    ("shared_onenote", "OneNote (Business)"),
    ("shared_github", "GitHub"),
    ("shared_onedriveforbusiness", "OneDrive for Business"),
    ("shared_jira", "Jira"),
    ("shared_approvals", "Standard approvals"),
];

/// A connector the app knows the icon and name of.
pub fn known_connector(api: &str) -> Option<&'static str> {
    CONNECTORS.iter().find(|(name, _)| *name == api).map(|(name, _)| *name)
}

fn ai_id(value: &Value) -> bool {
    matches!(
        value.as_str(),
        Some(
            "/providers/Microsoft.PowerApps/apis/shared_m365copilotv2"
                | "/providers/Microsoft.PowerApps/apis/shared_agentnode"
        )
    )
}
fn uses_ai_connector(value: &Value) -> bool {
    if value.pointer("/inputs/host/apiId").is_some_and(ai_id) {
        return true;
    }
    match value {
        Value::Object(values) => values.values().any(uses_ai_connector),
        Value::Array(values) => values.iter().any(uses_ai_connector),
        _ => false,
    }
}

pub fn copilot_access(value: &Value) -> Result<bool, WorkflowFailure> {
    let settings = value
        .pointer("/userCopilotSettings/value")
        .ok_or(WorkflowFailure::InvalidResponse)?;
    let eligible = settings
        .pointer("/eligibility/isCopilotEligible")
        .and_then(Value::as_bool)
        .ok_or(WorkflowFailure::InvalidResponse)?;
    let allowed = settings
        .get("isCopilotAllowedByAdmin")
        .and_then(Value::as_bool)
        .ok_or(WorkflowFailure::InvalidResponse)?;
    Ok(eligible && allowed)
}

pub fn templates(value: &Value) -> Result<Vec<Card>, WorkflowFailure> {
    // These are the actual channel templates offered by the reference, not invented gallery entries.
    const FEATURED: [&str; 6] = [
        "805739fdad754e799502b6379900922e",
        "8f348e6a1ad64fe8bcb974b02585d987",
        "511d104411f84d659018aed7d68a4c22",
        "190c5a56070e4a8b802089be6455553e",
        "b35fbd284547427cbb4e289d98dd63b4",
        "d271a6f01c2545a28348d8f2cddf4c8f",
    ];
    let mut cards = rows(value)?
        .iter()
        .map(Card::from_template)
        .collect::<Option<Vec<_>>>()
        .ok_or(WorkflowFailure::InvalidResponse)?;
    cards.sort_by_key(|card| FEATURED.iter().position(|id| *id == card.id).unwrap_or(FEATURED.len()));
    Ok(cards)
}

/// Home's templates the gallery files under `schedule` (Follow up on a message, the weekly reminder, Schedule a reply,
/// Schedule a meeting, Send yourself a reminder): the kind of a card whose answer carries no categories.
const SCHEDULES: [&str; 5] = [
    "34df10b7d2824b12a23f3a0c50869f99",
    "805739fdad754e799502b6379900922e",
    "8dfc20ec70f54efcbf3c18cf3e190035",
    "402ac1c1290b4609827b83c671613e93",
    "a045450ffe3e4b35b1d89ffb97db5658",
];

/// The Workflows app's Home: its own sixteen featured templates, in the order the app shows them (eight a page).
const HOME_ORDER: [&str; 16] = [
    "34df10b7d2824b12a23f3a0c50869f99",
    "805739fdad754e799502b6379900922e",
    "8f348e6a1ad64fe8bcb974b02585d987",
    "511d104411f84d659018aed7d68a4c22",
    "190c5a56070e4a8b802089be6455553e",
    "b35fbd284547427cbb4e289d98dd63b4",
    "d271a6f01c2545a28348d8f2cddf4c8f",
    "8dfc20ec70f54efcbf3c18cf3e190035",
    "402ac1c1290b4609827b83c671613e93",
    "4289ec53edec430b8b760234bcc87267",
    "d01492d9fdc74baeb5890d9c17186942",
    "2f11110f8e7a4985b424d75fa992b00c",
    "a045450ffe3e4b35b1d89ffb97db5658",
    "34ac6c11796143e1807d6e16c4c28050",
    "df8b1fb0c17e48a790df8515a1259170",
    "45e229e1cd36483084bc8d2352b543d2",
];

/// The templates of a gallery answer in its own order, each once: what Find templates suggests.
pub fn found(value: &Value) -> Result<Vec<Card>, WorkflowFailure> {
    let mut cards = rows(value)?
        .iter()
        .map(Card::from_gallery)
        .collect::<Option<Vec<_>>>()
        .ok_or(WorkflowFailure::InvalidResponse)?;
    let mut seen = std::collections::HashSet::new();
    cards.retain(|card| seen.insert(card.id.clone()));
    Ok(cards)
}

/// The templates of a gallery answer, Home's own first in the app's order, then the rest in the gallery's (newest
/// first), each once.
pub fn gallery(value: &Value) -> Result<Vec<Card>, WorkflowFailure> {
    let mut cards = found(value)?;
    cards.sort_by_key(|card| {
        HOME_ORDER
            .iter()
            .position(|id| *id == card.id)
            .unwrap_or(HOME_ORDER.len())
    });
    Ok(cards)
}

fn destination(node: &Value, definition: &Value, team: &str, channel: &str) -> bool {
    if node.pointer("/inputs/host/apiId").and_then(Value::as_str)
        == Some("/providers/Microsoft.PowerApps/apis/shared_teams")
        && node.pointer("/inputs/parameters/location").and_then(Value::as_str) == Some("Channel")
    {
        let parameters = node.pointer("/inputs/parameters");
        let group = parameters
            .and_then(|parameters| parameters.get("body/recipient/groupId"))
            .and_then(Value::as_str);
        let target = parameters
            .and_then(|parameters| parameters.get("body/recipient/channelId"))
            .and_then(Value::as_str);
        if group.and_then(|value| resolve(value, definition)) == Some(team)
            && target.and_then(|value| resolve(value, definition)) == Some(channel)
        {
            return true;
        }
    }
    match node {
        Value::Object(values) => values
            .values()
            .any(|value| destination(value, definition, team, channel)),
        Value::Array(values) => values.iter().any(|value| destination(value, definition, team, channel)),
        _ => false,
    }
}
fn resolve<'a>(value: &'a str, definition: &'a Value) -> Option<&'a str> {
    if !value.starts_with('@') {
        return Some(value);
    }
    let key = value.strip_prefix("@parameters('")?.strip_suffix("')")?;
    definition.get("parameters")?.get(key)?.get("defaultValue")?.as_str()
}

pub fn rows(answer: &Value) -> Result<&[Value], WorkflowFailure> {
    answer
        .get("value")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(WorkflowFailure::InvalidResponse)
}

pub fn next(answer: &Value) -> Option<String> {
    ["nextLink", "@odata.nextLink"].iter().find_map(|key| {
        answer
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

pub fn problem(error: &WorkflowFailure) -> &'static str {
    match error {
        WorkflowFailure::Authentication => "Power Automate needs access to your account. Sign in again, then refresh.",
        WorkflowFailure::Forbidden => {
            "Power Automate denied access. Check your organization's Workflows policy and Power Automate permissions."
        }
        WorkflowFailure::NotFound => "This workflow or environment no longer exists. Refresh to see the current list.",
        WorkflowFailure::Throttled => "Power Automate is busy. Wait a moment, then refresh before trying again.",
        WorkflowFailure::Changed => {
            "This workflow changed after you opened it. Refresh and review the current definition before saving."
        }
        WorkflowFailure::ReadOnly => "This app is read-only. Workflow changes and runs are unavailable.",
        WorkflowFailure::InvalidInput => "Check the required trigger inputs and their types, then try again.",
        WorkflowFailure::InvalidResponse => {
            "Power Automate returned data this app could not read. Refresh to try again."
        }
        WorkflowFailure::Connection => {
            "Power Automate could not be reached. Refresh to check the current state before trying a change again."
        }
        WorkflowFailure::Service(400) => {
            "Power Automate rejected this configuration. Check required fields and connected accounts."
        }
        WorkflowFailure::Service(409) => {
            "Power Automate reported a conflict. Refresh the current workflow before changing it again."
        }
        WorkflowFailure::Service(_) => {
            "Power Automate could not complete the request. Refresh to check the current state."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_actual_teams_channel_recipients_establish_channel_association() {
        let unrelated = json!({"name":"synthetic","properties":{"displayName":"Synthetic","definition":{"actions":{"compose":{"type":"Compose","inputs":["group","channel"]}}}}});
        assert!(!Card::from_flow(&unrelated).unwrap().in_channel("group", "channel"));
        let linked = json!({"name":"synthetic","properties":{"displayName":"Synthetic","definition":{"parameters":{"team":{"defaultValue":"group"},"channel":{"defaultValue":"channel"}},"actions":{"post":{"inputs":{"host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_teams"},"parameters":{"location":"Channel","body/recipient/groupId":"@parameters('team')","body/recipient/channelId":"@parameters('channel')"}}}}}}});
        let card = Card::from_flow(&linked).unwrap();
        assert!(card.in_channel("group", "channel"));
        assert!(!card.in_channel("group", "another-channel"));
    }
    #[test]
    fn copilot_access_needs_both_observed_account_entitlements() {
        assert_eq!(copilot_access(&json!({})), Err(WorkflowFailure::InvalidResponse));
        let mut settings = json!({"userCopilotSettings":{"value":{"isCopilotAllowedByAdmin":true,"eligibility":{"isCopilotEligible":false}}}});
        assert_eq!(copilot_access(&settings), Ok(false));
        settings["userCopilotSettings"]["value"]["eligibility"]["isCopilotEligible"] = json!(true);
        assert_eq!(copilot_access(&settings), Ok(true));
        settings["userCopilotSettings"]["value"]["isCopilotAllowedByAdmin"] = json!(false);
        assert_eq!(copilot_access(&settings), Ok(false));
    }
    #[test]
    fn an_ai_name_in_a_message_does_not_classify_a_workflow_as_ai() {
        let mut flow = json!({"name":"synthetic","properties":{"displayName":"Synthetic","definition":{"actions":{"compose":{"inputs":"/providers/Microsoft.PowerApps/apis/shared_m365copilotv2"}}}}});
        assert!(!Card::from_flow(&flow).unwrap().is_ai());
        flow["properties"]["definition"]["actions"]["copilot"] =
            json!({"inputs":{"host":{"apiId":"/providers/Microsoft.PowerApps/apis/shared_m365copilotv2"}}});
        assert!(Card::from_flow(&flow).unwrap().is_ai());
    }

    #[test]
    fn the_apps_gallery_puts_homes_templates_first_and_reads_kinds_and_apps() {
        let entry = |id: &str, kinds: &[&str], apis: &[&str]| {
            let references: serde_json::Map<String, Value> = apis
                .iter()
                .map(|api| ((*api).to_owned(), json!({"api": {"name": api}})))
                .collect();
            json!({"name": id, "properties": {"displayName": id, "description": "Synthetic.",
                "categoryNames": kinds, "connectionReferences": references}})
        };
        let answer = json!({"value": [
            entry("synthetic-newest", &["take_actions"], &["shared_teams", "shared_onenote"]),
            entry("805739fdad754e799502b6379900922e", &["schedule"], &["shared_teams"]),
            entry("synthetic-newest", &["take_actions"], &[]),
            entry("34df10b7d2824b12a23f3a0c50869f99", &["schedule", "get_updates"], &["shared_teams", "shared_office365users"]),
            entry("synthetic-both", &["take_action", "get_updates"], &["shared_teams"])
        ]});
        let cards = gallery(&answer).unwrap();
        let ids: Vec<_> = cards.iter().map(|card| card.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "34df10b7d2824b12a23f3a0c50869f99",
                "805739fdad754e799502b6379900922e",
                "synthetic-newest",
                "synthetic-both"
            ],
            "Home's own first in Home's order, then the gallery's, each once"
        );
        assert_eq!(cards[0].kind(), TemplateKind::Schedule);
        assert_eq!(cards[2].kind(), TemplateKind::TakeAction);
        assert_eq!(
            cards[3].kind(),
            TemplateKind::GetUpdates,
            "updates and actions both shows as an update"
        );
        assert!(cards[3].is_kind(TemplateKind::TakeAction));
        assert_eq!(
            cards[0].connectors(),
            ["shared_office365users", "shared_teams"],
            "the app draws the apps in reverse"
        );
        assert_eq!(cards[2].connectors(), ["shared_onenote", "shared_teams"]);
        let catalog =
            Card::from_template(&json!({"id":"805739fdad754e799502b6379900922e","name":"Weekly","description":"x"}))
                .unwrap();
        assert_eq!(
            catalog.kind(),
            TemplateKind::Schedule,
            "a catalog card without categories"
        );
        assert!(gallery(&json!({"value": [{"name": "no-properties"}]})).is_err());
    }

    #[test]
    fn catalog_dto_is_distinct_from_a_workflow_resource() {
        let card = Card::from_template(
            &json!({"id":"synthetic","name":"Weekly reminder","description":"Synthetic channel update"}),
        )
        .unwrap();
        assert!(card.matches("WEEKLY channel"));
        assert!(!card.matches("unrelated"));
        assert!(Card::from_flow(&json!({"id":"synthetic","name":"Template"})).is_none());
    }
}
