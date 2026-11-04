//! Payload-bearing runs use the actual first-party Logic Flows connector. A generic request cannot choose its
//! runtime, and an API Hub credential never goes to a callback, template URL or third-party connector.
use super::{failure, status};
use crate::http::Outgoing;
use crate::{Credentials, HttpTransport, Service, WorkflowFailure, WorkflowRequest, WorkflowResponse};
use serde_json::Value;

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn workflow_run_inputs(
        &self,
        environment: &str,
        flow: &str,
        trigger: &str,
        inputs: &Value,
    ) -> WorkflowResponse {
        let current = self
            .workflow_http(&WorkflowRequest::Get {
                environment: environment.to_owned(),
                flow: flow.to_owned(),
            })
            .await?;
        check_trigger(&current, flow, trigger, inputs)?;
        let metadata = self
            .workflow_http(&WorkflowRequest::LogicFlowsMetadata {
                environment: environment.to_owned(),
            })
            .await?;
        let mut url = runtime(&metadata, environment)?;
        url.path_segments_mut()
            .map_err(|()| WorkflowFailure::InvalidResponse)?
            .push(flow)
            .push("triggers")
            .push(trigger)
            .push("run");
        if !crate::http::decoded_path(&url).is_some_and(|path| {
            path.iter()
                .map(String::as_str)
                .eq(["apim", "logicflows", flow, "triggers", trigger, "run"])
        }) {
            return Err(WorkflowFailure::InvalidInput);
        }
        url.query_pairs_mut().append_pair("api-version", "2016-11-01");
        let bytes = serde_json::to_vec(inputs).map_err(|_| WorkflowFailure::InvalidInput)?;
        let token = self.token(Service::WorkflowRuntime).await.map_err(failure)?;
        let outgoing = Outgoing {
            method: reqwest::Method::POST,
            url: &url,
            service: Service::WorkflowRuntime,
            body: Some(&bytes),
            headers: &[],
            range: None,
        };
        let answer = self.attempt(&outgoing, token.as_ref()).await.map_err(failure)?;
        if answer.status() == 401
            && let Some(token) = &token
        {
            let _ = self
                .credentials
                .bearer_rejected(crate::workflow::RUNTIME_AUDIENCE, token)
                .await;
        }
        if !answer.is_success() {
            return Err(status(answer.status()));
        }
        // An accepted runtime may return plain text or nothing, rather than management-resource JSON.
        Ok(serde_json::from_slice(answer.body()).unwrap_or(Value::Null))
    }
}
fn check_trigger(current: &Value, flow: &str, trigger: &str, inputs: &Value) -> Result<(), WorkflowFailure> {
    let definition = current
        .pointer("/properties/definition/triggers")
        .and_then(|triggers| triggers.get(trigger))
        .ok_or(WorkflowFailure::Changed)?;
    let kind = definition.get("kind").and_then(Value::as_str);
    if current.get("name").and_then(Value::as_str) != Some(flow)
        || !matches!(
            kind,
            Some("Button" | "PowerApp" | "PowerAppV2" | "Skills" | "PowerPages")
        )
    {
        return Err(WorkflowFailure::InvalidInput);
    }
    let schema = definition
        .pointer("/inputs/schema")
        .ok_or(WorkflowFailure::InvalidInput)?;
    if !inputs.is_object() || !crate::workflow::valid_trigger_inputs(schema, inputs) {
        return Err(WorkflowFailure::InvalidInput);
    }
    Ok(())
}
fn runtime(metadata: &Value, environment: &str) -> Result<url::Url, WorkflowFailure> {
    if metadata.get("name").and_then(Value::as_str) != Some("shared_logicflows")
        || metadata.get("id").and_then(Value::as_str) != Some("/providers/Microsoft.PowerApps/apis/shared_logicflows")
    {
        return Err(WorkflowFailure::InvalidResponse);
    }
    let address = metadata
        .pointer("/properties/primaryRuntimeUrl")
        .and_then(Value::as_str)
        .ok_or(WorkflowFailure::InvalidResponse)?;
    let url = url::Url::parse(address).map_err(|_| WorkflowFailure::InvalidResponse)?;
    let prefix = format!("{}.", environment.to_ascii_lowercase());
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
        || url.path() != "/apim/logicflows"
        || !url
            .host_str()
            .is_some_and(|host| host.starts_with(&prefix) && host.ends_with(".azure-apihub.net"))
    {
        return Err(WorkflowFailure::InvalidResponse);
    }
    Ok(url)
}
