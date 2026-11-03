//! Power Automate's channel catalog and classic Flow resource provider operations.
//!
//! Paths are grounded in the current Teams reference and Microsoft's Power Automate client source; see
//! `docs/protocol/workflows.md`. A request cannot choose an origin or a credential audience.
mod inputs;
pub use inputs::valid_trigger_inputs;

use serde_json::Value;

use crate::{Endpoint, Service};

/// The audience requested by the Teams Workflows host, including its trailing slash.
pub const WORKFLOW_AUDIENCE: &str = "https://service.flow.microsoft.com/";
/// The first-party Logic Flows runtime audience.
pub const RUNTIME_AUDIENCE: &str = "https://apihub.azure.com";
/// The separately authenticated connection service audience.
pub const CONNECTION_AUDIENCE: &str = "https://service.powerapps.com/";
const API_VERSION: &str = "2016-11-01";

/// An explicitly chosen native management operation. Definitions remain service JSON: unknown fields survive edits.
#[derive(Clone, Debug, PartialEq)]
pub enum WorkflowRequest {
    /// Teams' channel template catalog.
    Catalog,
    /// Own Teams Copilot access settings, a captured read by POST.
    CopilotEligibility,
    /// Environments accessible to the signed-in actor.
    Environments,
    /// A gallery template with its actual definition and connector metadata.
    Template { id: String },
    /// Microsoft’s Logic Flows connector metadata in the chosen environment.
    LogicFlowsMetadata { environment: String },
    /// A run whose payload must reach the trigger body through the authenticated connector runtime.
    RunWithInputs {
        environment: String,
        flow: String,
        trigger: String,
        inputs: Value,
    },
    /// Connections in the chosen environment, never tenant-wide connection discovery.
    Connections { environment: String },
    /// Creates a Microsoft connector connection without accepting user credentials in this app.
    CreateConnection {
        environment: String,
        connector: String,
        connection: String,
    },
    /// The service's OAuth consent link, with a fixed Microsoft portal redirect.
    ConnectionConsent {
        environment: String,
        connector: String,
        connection: String,
    },
    /// A continuation returned by the same environment's exact flow-list route.
    Page { environment: String, next: String },
    /// All workflows the actor may see in an environment.
    List { environment: String },
    /// Read a whole workflow before editing or running it.
    Get { environment: String, flow: String },
    /// Recent runs; continuation is reported, never silently treated as complete.
    History { environment: String, flow: String },
    /// The next page of the same flow’s run history.
    HistoryPage {
        environment: String,
        flow: String,
        next: String,
    },
    /// Per-action status of one run, without automatically reading any signed input/output content links.
    RunActions {
        environment: String,
        flow: String,
        run: String,
    },
    /// Reads a webhook trigger's service-issued callback address. It never invokes that address.
    WebhookAddress {
        environment: String,
        flow: String,
        trigger: String,
    },
    /// Create a workflow from a reviewed definition and connection references.
    Create { environment: String, properties: Value },
    /// Update the whole editable properties, preserving fields from the last read.
    Update {
        environment: String,
        flow: String,
        properties: Value,
        expected_modified: Option<String>,
    },
    /// Start or stop the workflow's triggers.
    SetEnabled {
        environment: String,
        flow: String,
        enabled: bool,
    },
    /// Delete after an explicit confirmation.
    Delete { environment: String, flow: String },
    /// Run a trigger through the authenticated management endpoint, without callback URLs or implicit resends.
    Run {
        environment: String,
        flow: String,
        trigger: String,
    },
}

impl WorkflowRequest {
    /// Whether this operation can change something or trigger future contact.
    #[must_use]
    pub fn writes(&self) -> bool {
        !matches!(
            self,
            Self::Catalog
                | Self::CopilotEligibility
                | Self::Environments
                | Self::Template { .. }
                | Self::Connections { .. }
                | Self::LogicFlowsMetadata { .. }
                | Self::Page { .. }
                | Self::List { .. }
                | Self::Get { .. }
                | Self::History { .. }
                | Self::HistoryPage { .. }
                | Self::RunActions { .. }
                | Self::WebhookAddress { .. }
        )
    }

    /// The service method.
    #[must_use]
    pub fn method(&self) -> &'static str {
        match self {
            Self::CopilotEligibility
            | Self::Create { .. }
            | Self::SetEnabled { .. }
            | Self::Run { .. }
            | Self::RunWithInputs { .. }
            | Self::ConnectionConsent { .. }
            | Self::WebhookAddress { .. } => "POST",
            Self::CreateConnection { .. } => "PUT",
            Self::Update { .. } => "PATCH",
            Self::Delete { .. } => "DELETE",
            _ => "GET",
        }
    }

    /// The first-party service, chosen by operation rather than by a URL supplied by the caller.
    #[must_use]
    pub fn service(&self) -> Service {
        match self {
            Self::CopilotEligibility => Service::MiddleTier,
            Self::RunWithInputs { .. } => Service::WorkflowRuntime,
            Self::LogicFlowsMetadata { .. } => Service::WorkflowConnections,
            Self::Connections { .. } | Self::CreateConnection { .. } | Self::ConnectionConsent { .. } => {
                Service::WorkflowConnections
            }
            _ => Service::Workflows,
        }
    }

    /// The exact path, built from individually encoded segments; no arbitrary URLs or continuations enter here.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        if matches!(self, Self::CopilotEligibility) {
            return Endpoint::new(["beta", "users", "useraggregatesettings"]);
        }
        if self.service() == Service::WorkflowConnections {
            return self.connection_endpoint();
        }
        let mut path = vec!["providers".to_owned(), "Microsoft.ProcessSimple".to_owned()];
        if matches!(self, Self::Catalog) {
            path.extend(["galleries", "public", "teamsworkflowtemplates"].map(str::to_owned));
            return Endpoint::new(path)
                .with_query("$filter", "properties/categoryNames/any(x:x eq 'microsoftteams_channel') and properties/categoryNames/all(x:x ne 'reported')")
                .with_query("useNewConfigUrl", "true")
                .with_query("supportBigTiles", "true");
        }
        if let Self::Template { id } = self {
            path.extend([
                "galleries".to_owned(),
                "public".to_owned(),
                "templates".to_owned(),
                id.clone(),
            ]);
            return Endpoint::new(path).with_query("$expand", "definition,connectionReferences.api.properties");
        }
        path.push("environments".to_owned());
        if let Some(environment) = self.environment() {
            path.extend([environment.to_owned(), "flows".to_owned()]);
        }
        match self {
            Self::Get { flow, .. } | Self::Update { flow, .. } | Self::Delete { flow, .. } => path.push(flow.clone()),
            Self::History { flow, .. } => path.extend([flow.clone(), "runs".to_owned()]),
            Self::RunActions { flow, run, .. } => {
                path.extend([flow.clone(), "runs".to_owned(), run.clone(), "actions".to_owned()]);
            }
            Self::WebhookAddress { flow, trigger, .. } => path.extend([
                flow.clone(),
                "triggers".to_owned(),
                trigger.clone(),
                "listCallbackUrl".to_owned(),
            ]),
            Self::SetEnabled { flow, enabled, .. } => {
                path.extend([flow.clone(), if *enabled { "start" } else { "stop" }.to_owned()]);
            }
            Self::Run { flow, trigger, .. } => {
                path.extend([flow.clone(), "triggers".to_owned(), trigger.clone(), "run".to_owned()]);
            }
            _ => {}
        }
        Endpoint::new(path).with_query("api-version", API_VERSION)
    }

    fn connection_endpoint(&self) -> Endpoint {
        let mut path = vec!["providers".to_owned(), "Microsoft.PowerApps".to_owned()];
        match self {
            Self::Connections { .. } => path.push("connections".to_owned()),
            Self::LogicFlowsMetadata { .. } => path.extend(["apis", "shared_logicflows"].map(str::to_owned)),
            Self::CreateConnection {
                connector, connection, ..
            }
            | Self::ConnectionConsent {
                connector, connection, ..
            } => {
                path.extend([
                    "apis".to_owned(),
                    connector.clone(),
                    "connections".to_owned(),
                    connection.clone(),
                ]);
                if matches!(self, Self::ConnectionConsent { .. }) {
                    path.push("getConsentLink".to_owned());
                }
            }
            _ => {}
        }
        Endpoint::new(path).with_query("api-version", API_VERSION).with_query(
            "$filter",
            format!(
                "environment eq '{}'",
                self.environment().unwrap_or_default().replace('\'', "''")
            ),
        )
    }

    /// A continuation may only read the same exact flow-list endpoint on the same Microsoft host.
    ///
    /// # Errors
    /// An off-origin, credential-bearing, fragmented or differently scoped continuation is refused.
    pub fn checked_endpoint(&self) -> Result<Endpoint, WorkflowFailure> {
        let (next, baseline) = match self {
            Self::Page { environment, next } => (
                next,
                Self::List {
                    environment: environment.clone(),
                }
                .endpoint(),
            ),
            Self::HistoryPage {
                environment,
                flow,
                next,
            } => (
                next,
                Self::History {
                    environment: environment.clone(),
                    flow: flow.clone(),
                }
                .endpoint(),
            ),
            _ => return Ok(self.endpoint()),
        };
        let url = url::Url::parse(next).map_err(|_| WorkflowFailure::InvalidResponse)?;
        let expected: Vec<_> = baseline.segments().iter().map(String::as_str).collect();
        let segments: Vec<_> = url.path_segments().ok_or(WorkflowFailure::InvalidResponse)?.collect();
        if url.scheme() != "https"
            || url.host_str() != Some("api.flow.microsoft.com")
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || segments != expected
        {
            return Err(WorkflowFailure::InvalidResponse);
        }
        Ok(url
            .query_pairs()
            .fold(Endpoint::new(expected), |endpoint, (key, value)| {
                endpoint.with_query(key, value)
            }))
    }

    /// Environment selected from discovery, when the operation is environment scoped.
    #[must_use]
    pub fn environment(&self) -> Option<&str> {
        match self {
            Self::Catalog | Self::CopilotEligibility | Self::Environments | Self::Template { .. } => None,
            Self::LogicFlowsMetadata { environment }
            | Self::RunWithInputs { environment, .. }
            | Self::RunActions { environment, .. }
            | Self::WebhookAddress { environment, .. }
            | Self::Page { environment, .. }
            | Self::Connections { environment }
            | Self::CreateConnection { environment, .. }
            | Self::ConnectionConsent { environment, .. }
            | Self::List { environment }
            | Self::Get { environment, .. }
            | Self::History { environment, .. }
            | Self::HistoryPage { environment, .. }
            | Self::Create { environment, .. }
            | Self::Update { environment, .. }
            | Self::SetEnabled { environment, .. }
            | Self::Delete { environment, .. }
            | Self::Run { environment, .. } => Some(environment),
        }
    }

    /// Only create and update carry properties. Runtime callbacks never receive the account's bearer token.
    #[must_use]
    pub fn body(&self) -> Option<Value> {
        match self {
            Self::CopilotEligibility => Some(
                serde_json::json!({"userPropertiesSettings":true,"voiceAdminSettings":true,"adminSettings":true,"teamsUserPhoneNumbers":true,"tenantSettingsV2":true,"tenantVoiceSettings":true,"userResourcesSettings":true,"teamsOfficeCloudPolicy":true,"clientSettings":true,"userCopilotSettings":true,"teamsMessagingConfiguration":true,"teamsLinkPolicy":true,"educationSettings":true,"userLanguageAccountSettings":true,"teamsAcsFederationConfiguration":true,"smbTenantSettings":true,"tenantSiteUrlAndDomains":true,"deviceStoreMetadata":true}),
            ),
            Self::Create { properties, .. } | Self::Update { properties, .. } => {
                Some(serde_json::json!({"properties": properties}))
            }
            Self::Run { .. } => Some(serde_json::json!({})),
            Self::CreateConnection { environment, .. } => Some(
                serde_json::json!({"properties": {"connectionParameters": {}, "environment": {"name": environment}}}),
            ),
            Self::ConnectionConsent { .. } => {
                Some(serde_json::json!({"redirectUrl": "https://make.powerautomate.com"}))
            }
            _ => None,
        }
    }
}

/// The distinction matters: denied access is not an empty workflow list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkflowFailure {
    /// Sign in again or grant this first-party service access.
    Authentication,
    /// The service explicitly denied this operation.
    Forbidden,
    /// The selected environment or workflow no longer exists.
    NotFound,
    /// The service throttled the operation. Writes are not automatically repeated.
    Throttled,
    /// A cloud definition cannot enforce the lab's fresh audience verification at every future run.
    LabCloudMutation,
    /// Another change followed the definition the user reviewed.
    Changed,
    /// Read-only application mode.
    ReadOnly,
    /// The supplied trigger inputs do not satisfy the freshly read schema.
    InvalidInput,
    /// The response did not have the required shape.
    InvalidResponse,
    /// The connection failed; a write's outcome may be unknown and must be refreshed before retrying.
    Connection,
    /// A service failure, retaining only its status (never payloads or credentials).
    Service(u16),
}

/// One settled service operation, without swallowing errors or inventing an entitlement.
pub type WorkflowResponse = Result<Value, WorkflowFailure>;

/// Whether a service-issued OAuth consent link stays on a Microsoft consent host.
#[must_use]
pub fn is_workflow_consent_link(link: &str) -> bool {
    let Ok(url) = url::Url::parse(link) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some_and(|host| {
            host.ends_with(".consent.azure-apim.net") || host.ends_with(".consent.azure-apihub.net")
        })
}
