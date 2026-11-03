//! Native workflow requests stay on their owning account's engine. The transport authenticates Flow separately;
//! no view, callback, external connector or template URL ever receives this account's Teams credentials.
use teams_proto::{Credentials, HttpError, Transport, WorkflowRequest, WorkflowResponse};

use crate::{channels::Channels, engine::Responder};

impl<C, T> Channels<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + 'static,
{
    pub(crate) fn workflow(&self, request: WorkflowRequest, reply: Responder<WorkflowResponse>) {
        let client = std::sync::Arc::clone(&self.client);
        let outbox = std::sync::Arc::clone(&self.outbox);
        tokio::task::spawn_local(async move {
            let answer = if request.writes() {
                outbox.try_workflow(&request).await
            } else {
                client.transport().workflow(&request).await
            };
            let _ = reply.send(Ok(answer));
        });
    }
}
