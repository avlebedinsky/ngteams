//! A first-party connector connection signed in without a browser, as the Teams Workflows app does when a template
//! opens (`docs/protocol/workflows.md`): the connection is created, Power Automate issues its own on-behalf-of token
//! bundle for the actor, and the bundle's API Hub token is posted to the connection's service-issued first-party
//! sign-in on a Microsoft consent host. Each step is one attempt. The bundle never leaves this function: the caller
//! receives only the created connection.
use serde_json::Value;

use super::{failure, status};
use crate::http::{Auth, Outgoing, plain_headers};
use crate::workflow::{CONSENT_TOKEN_AUDIENCE, is_first_party_login, platform_host};
use crate::{Credentials, HttpTransport, Service, WorkflowFailure, WorkflowRequest, WorkflowResponse};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn workflow_authorize_connection(
        &self,
        request: &WorkflowRequest,
        environment: &str,
    ) -> WorkflowResponse {
        let created = self.workflow_http(request).await?;
        if connected(&created) {
            return Ok(created);
        }
        let login = created
            .pointer("/properties/consentInfo/firstPartyLoginUri")
            .and_then(Value::as_str)
            .filter(|link| is_first_party_login(link))
            .and_then(|link| url::Url::parse(link).ok())
            .ok_or(WorkflowFailure::InvalidResponse)?;
        let bundle = self.on_behalf_of_bundle(environment).await?;
        let token = bundle
            .pointer("/audienceToToken")
            .and_then(|tokens| tokens.get(CONSENT_TOKEN_AUDIENCE))
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or(WorkflowFailure::InvalidResponse)?;
        let body = serde_json::to_vec(&serde_json::json!({"accessToken": token}))
            .map_err(|_| WorkflowFailure::InvalidResponse)?;
        let answer = self
            .send_as(
                reqwest::Method::POST,
                &login,
                &plain_headers(Service::Workflows, true),
                Auth::bearer(None),
                Some(body),
            )
            .await
            .map_err(failure)?;
        if !answer.is_success() {
            return Err(status(answer.status()));
        }
        Ok(created)
    }

    /// Power Automate's on-behalf-of tokens for the actor, read with the account's own Flow token on the environment's
    /// Power Platform host.
    async fn on_behalf_of_bundle(&self, environment: &str) -> WorkflowResponse {
        let host = platform_host(environment).ok_or(WorkflowFailure::InvalidResponse)?;
        let url = url::Url::parse(&format!(
            "https://{host}/powerautomate/users/me/onBehalfOfTokenBundle?api-version=1"
        ))
        .map_err(|_| WorkflowFailure::InvalidResponse)?;
        let token = self.token(Service::Workflows).await.map_err(failure)?;
        let outgoing = Outgoing {
            method: reqwest::Method::POST,
            url: &url,
            service: Service::Workflows,
            body: None,
            headers: &[],
            range: None,
        };
        let answer = self.attempt(&outgoing, token.as_ref()).await.map_err(failure)?;
        if answer.status() == 401
            && let Some(token) = &token
        {
            let _ = self
                .credentials
                .bearer_rejected(crate::workflow::WORKFLOW_AUDIENCE, token)
                .await;
        }
        if !answer.is_success() {
            return Err(status(answer.status()));
        }
        serde_json::from_slice(answer.body()).map_err(|_| WorkflowFailure::InvalidResponse)
    }
}

/// Whether the service already reports the connection signed in.
fn connected(connection: &Value) -> bool {
    connection
        .pointer("/properties/statuses")
        .and_then(Value::as_array)
        .is_some_and(|statuses| {
            statuses
                .iter()
                .any(|status| status.get("status").and_then(Value::as_str) == Some("Connected"))
        })
}
