//! A bounded first-party Flow route. Cloud mutations are never executed by the lab: a server-side scheduled flow
//! cannot revalidate the lab's full human membership before each later notification. This check is independent of
//! the historical fence classifier. Personal application use remains unrestricted, subject to read-only mode.
use crate::workflow::{WorkflowFailure, WorkflowRequest, WorkflowResponse};
use crate::{Credentials, HttpError, HttpTransport};

use super::{Outgoing, Record};

impl<C: Credentials> HttpTransport<C> {
    /// Executes one native workflow operation. Writes have exactly one attempt, including unknown outcomes.
    ///
    /// # Errors
    /// A lab/read-only mutation, authentication failure, non-success status, invalid response or connection failure.
    pub async fn workflow(&self, request: &WorkflowRequest) -> WorkflowResponse {
        if request.writes() && self.is_read_only() {
            return Err(WorkflowFailure::ReadOnly);
        }
        if request.writes() && !self.is_personal() {
            return Err(WorkflowFailure::LabCloudMutation);
        }
        self.check_workflow_version(request).await?;
        self.workflow_http(request).await
    }

    async fn check_workflow_version(&self, request: &WorkflowRequest) -> Result<(), WorkflowFailure> {
        let WorkflowRequest::Update {
            environment,
            flow,
            expected_modified: Some(expected),
            ..
        } = request
        else {
            return Ok(());
        };
        let current = self
            .workflow_http(&WorkflowRequest::Get {
                environment: environment.clone(),
                flow: flow.clone(),
            })
            .await?;
        if current.get("name").and_then(serde_json::Value::as_str) != Some(flow)
            || current
                .pointer("/properties/lastModifiedTime")
                .and_then(serde_json::Value::as_str)
                != Some(expected)
        {
            return Err(WorkflowFailure::Changed);
        }
        Ok(())
    }

    async fn workflow_http(&self, request: &WorkflowRequest) -> WorkflowResponse {
        let service = request.service();
        let endpoint = request.checked_endpoint()?;
        let url = self.url_for(service, &endpoint).map_err(failure)?;
        let method =
            reqwest::Method::from_bytes(request.method().as_bytes()).map_err(|_| WorkflowFailure::InvalidResponse)?;
        let bytes = request.body().map(|body| body.to_string().into_bytes());
        let outgoing = Outgoing {
            method,
            url: &url,
            service,
            body: bytes.as_deref(),
            range: None,
        };
        let answer = if request.writes() {
            let token = self.token(service).await.map_err(failure)?;
            self.attempt(&outgoing, Record::Unjudged("workflow"), token.as_ref())
                .await
        } else {
            self.exchange(outgoing, Record::Nothing).await
        }
        .map_err(failure)?;
        if !answer.is_success() {
            return Err(status(answer.status()));
        }
        if answer.body().is_empty() {
            Ok(serde_json::Value::Null)
        } else {
            serde_json::from_slice(answer.body()).map_err(|_| WorkflowFailure::InvalidResponse)
        }
    }
}

fn status(code: u16) -> WorkflowFailure {
    match code {
        401 => WorkflowFailure::Authentication,
        403 => WorkflowFailure::Forbidden,
        404 => WorkflowFailure::NotFound,
        429 => WorkflowFailure::Throttled,
        other => WorkflowFailure::Service(other),
    }
}

fn failure(error: HttpError) -> WorkflowFailure {
    match error {
        HttpError::Credentials(_) => WorkflowFailure::Authentication,
        HttpError::Status(answer) => status(answer.status()),
        _ => WorkflowFailure::Connection,
    }
}
