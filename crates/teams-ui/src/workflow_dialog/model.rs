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
        has_literal(definition, team) && has_literal(definition, channel)
    }

    pub fn enabled(&self) -> bool {
        self.properties.get("state").and_then(Value::as_str) == Some("Started")
    }
}

fn has_literal(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(text) => text == expected,
        Value::Array(values) => values.iter().any(|value| has_literal(value, expected)),
        Value::Object(values) => values.values().any(|value| has_literal(value, expected)),
        _ => false,
    }
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
