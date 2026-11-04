//! Reads Teams' card editor sends to the environment's Power Platform host rather than to the classic Flow API: a
//! connector operation's definition and the choices its dynamic lists and folder trees offer
//! (`docs/protocol/workflows.md`). The host is derived from the environment's name, never taken from a caller or a
//! response, and only the account's own Flow token goes there, as for the on-behalf-of bundle.
use super::{failure, status};
use crate::http::Outgoing;
use crate::workflow::platform_host;
use crate::{Credentials, HttpTransport, Service, WorkflowFailure, WorkflowRequest, WorkflowResponse};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn workflow_platform(&self, request: &WorkflowRequest) -> WorkflowResponse {
        let endpoint = request.platform_endpoint().ok_or(WorkflowFailure::InvalidResponse)?;
        let host = request
            .environment()
            .and_then(platform_host)
            .ok_or(WorkflowFailure::InvalidResponse)?;
        let mut url = url::Url::parse(&format!("https://{host}/")).map_err(|_| WorkflowFailure::InvalidResponse)?;
        url.path_segments_mut()
            .map_err(|()| WorkflowFailure::InvalidResponse)?
            .pop_if_empty()
            .extend(endpoint.segments());
        url.query_pairs_mut().extend_pairs(endpoint.query());
        let method =
            reqwest::Method::from_bytes(request.method().as_bytes()).map_err(|_| WorkflowFailure::InvalidResponse)?;
        let body = request.body().map(|body| body.to_string().into_bytes());
        let outgoing = Outgoing {
            method,
            url: &url,
            service: Service::Workflows,
            body: body.as_deref(),
            headers: &[],
            range: None,
        };
        // A read, though the choices go by POST: the transport's one renewal or wait-and-retry applies.
        let answer = self.exchange(outgoing).await.map_err(failure)?;
        if !answer.is_success() {
            return Err(status(answer.status()));
        }
        serde_json::from_slice(answer.body()).map_err(|_| WorkflowFailure::InvalidResponse)
    }
}
