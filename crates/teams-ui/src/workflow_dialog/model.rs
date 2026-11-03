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

    pub fn from_flow(value: &Value) -> Option<Self> {
        let properties = value.get("properties")?;
        Some(Self {
            id: value.get("name")?.as_str()?.to_owned(),
            name: properties.get("displayName")?.as_str()?.to_owned(),
            description: properties
                .get("description")
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

    pub fn connectors(&self) -> Vec<&'static str> {
        let mut apps = Vec::new();
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
            let label = match icon.rsplit('/').nth(1) {
                Some("teams") => "Teams",
                Some("planner") => "Planner",
                Some("sharepointonline") => "SharePoint",
                Some("office365") => "Outlook",
                Some("office365users") => "Microsoft 365",
                _ => continue,
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
        WorkflowFailure::LabCloudMutation => {
            "Cloud workflows cannot run from a lab account because their future recipients cannot be checked before every run."
        }
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
