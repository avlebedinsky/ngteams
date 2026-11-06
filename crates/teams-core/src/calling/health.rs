//! A running call's health, as Teams' Call health pane shows it (`EngineRequests::call_health`): its media's own
//! statistics (`teams_media::health`), read when the pane asks. It isn't a call action: nothing is sent and the call
//! is left as it is. libwebrtc answers on its own thread, so the read waits off the engine's.

use std::sync::Arc;

use teams_call::CallId;
use teams_proto::{Credentials, HttpError, Transport};

pub use teams_media::health::{CallHealth, MediaHealth, NetworkHealth, StreamHealth};

use super::Calling;
use crate::engine::Responder;

impl<C, T> Calling<C, T>
where
    C: Credentials + 'static,
    T: Transport<Error = HttpError> + Send + 'static,
{
    /// Answers `reply` with the health of `call`'s media; `None` when it has none running.
    pub(super) fn health(&self, call: CallId, reply: Responder<Option<CallHealth>>) {
        let Some(live) = self.live.as_ref().filter(|live| live.call == call) else {
            let _ = reply.send(Ok(None));
            return;
        };
        let session = Arc::clone(&live.session);
        tokio::task::spawn_blocking(move || {
            let _ = reply.send(Ok(Some(session.health())));
        });
    }
}
