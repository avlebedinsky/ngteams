//! Bounded first-party workflow operations, preserving read-only mode, version checks and single-shot writes.
mod connect;
mod inputs;
mod platform;

use crate::workflow::{WorkflowFailure, WorkflowRequest, WorkflowResponse};
use crate::{Credentials, HttpError, HttpTransport};

use super::Outgoing;

impl<C: Credentials> HttpTransport<C> {
    /// Executes one native workflow operation. Writes have exactly one attempt, including unknown outcomes.
    ///
    /// # Errors
    /// A read-only mutation, authentication failure, non-success status, invalid response or connection failure.
    pub async fn workflow(&self, request: &WorkflowRequest) -> WorkflowResponse {
        if request.writes() && self.is_read_only() {
            return Err(WorkflowFailure::ReadOnly);
        }
        if let WorkflowRequest::RunWithInputs {
            environment,
            flow,
            trigger,
            inputs,
        } = request
        {
            return self.workflow_run_inputs(environment, flow, trigger, inputs).await;
        }
        if let WorkflowRequest::AuthorizeConnection { environment, .. } = request {
            return self.workflow_authorize_connection(request, environment).await;
        }
        if request.platform_endpoint().is_some() {
            return self.workflow_platform(request).await;
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
            headers: &[],
            range: None,
        };
        let answer = if request.writes() {
            let token = self.token(service).await.map_err(failure)?;
            let answer = self.attempt(&outgoing, token.as_ref()).await;
            if let (Ok(answer), Some(audience), Some(token)) = (&answer, super::audience(service), &token)
                && answer.status() == 401
            {
                let _ = self.credentials.bearer_rejected(audience, token).await;
            }
            answer
        } else {
            self.exchange(outgoing).await
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
