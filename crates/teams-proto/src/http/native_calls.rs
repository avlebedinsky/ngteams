//! Native call proof uses the account's independent actor/member reader and account-local link bindings.
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::{Credentials, HttpError, HttpTransport, Refusal, Response};
use crate::lab_calls::{CallDeliveryData, CallObservation, CallObserver, CallPlan, LabCallBindings};
use crate::write::CallWire;
use teams_lab_targets::TargetError;

pub(super) struct CallAttempt {
    key: u64,
    plan: CallPlan,
}

impl<C: Credentials> HttpTransport<C> {
    /// The same account-local call evidence used by this transport and its calling socket.
    #[must_use]
    pub fn lab_call_bindings(&self) -> Option<Arc<LabCallBindings>> {
        self.lab_targets.as_ref().map(|context| Arc::clone(&context.calls))
    }

    pub(super) async fn preflight_native_call(&self, wire: &CallWire) -> Result<Option<CallAttempt>, HttpError> {
        let Some(context) = &self.lab_targets else {
            return Ok(None);
        };
        let refused = |error| HttpError::Refused(Refusal::LabTarget(error));
        let plan = context.calls.plan(wire).map_err(refused)?;
        let members = if plan.cleanup {
            None
        } else {
            let members = self
                .preflight_lab_chat(&plan.chat)
                .await?
                .ok_or_else(|| refused(TargetError::Membership))?;
            let peers: std::collections::BTreeSet<_> = members
                .iter()
                .chain(plan.added.iter())
                .chain(plan.peers.iter())
                .collect();
            for peer in peers.into_iter().filter(|mri| Some(mri.as_str()) != self.lab_own()) {
                let account = self
                    .lab_roster()
                    .and_then(|roster| roster.account(peer))
                    .ok_or_else(|| refused(TargetError::Recipients))?;
                self.lab_peer(peer, &account.upn).await?;
            }
            Some(members)
        };
        let key = context.calls.begin(&plan, members).map_err(refused)?;
        Ok(Some(CallAttempt { key, plan }))
    }

    pub(super) fn native_call_answer(&self, attempt: Option<CallAttempt>, response: &Response) {
        if let (Some(context), Some(attempt)) = (&self.lab_targets, attempt) {
            context
                .calls
                .answered(attempt.key, &attempt.plan, response.status(), response.body());
        }
    }

    async fn verify_incoming(&self, key: u64, chat: &str, connection: &str) -> CallObservation {
        let Ok(Some(members)) = self.preflight_lab_chat(chat).await else {
            return CallObservation::Ignored;
        };
        for peer in members.iter().filter(|mri| Some(mri.as_str()) != self.lab_own()) {
            let Some(account) = self.lab_roster().and_then(|roster| roster.account(peer)) else {
                return CallObservation::Ignored;
            };
            if self.lab_peer(peer, &account.upn).await.is_err() {
                return CallObservation::Ignored;
            }
        }
        match self
            .lab_targets
            .as_ref()
            .map(|context| context.calls.confirm_incoming(key, connection, &members))
        {
            Some(Ok(())) => CallObservation::Allowed,
            _ => CallObservation::Ignored,
        }
    }
}

impl<C: Credentials + 'static> CallObserver for HttpTransport<C> {
    fn call_bindings(&self) -> Option<Arc<LabCallBindings>> {
        self.lab_call_bindings()
    }

    fn observe_call(
        self: Arc<Self>,
        delivery: CallDeliveryData,
    ) -> Pin<Box<dyn Future<Output = CallObservation> + Send>> {
        // Record synchronously before the socket polls its next frame: an end revokes receipts already waiting on I/O.
        let plan = self
            .lab_targets
            .as_ref()
            .map(|context| context.calls.record_delivery(&delivery));
        Box::pin(async move {
            use crate::lab_calls::DeliveryPlan;
            match plan {
                None => CallObservation::Allowed,
                Some(Ok(DeliveryPlan::Known(observed))) => observed,
                Some(Ok(DeliveryPlan::Incoming { key, chat })) => {
                    self.verify_incoming(key, &chat, &delivery.connection).await
                }
                Some(Err(_)) => CallObservation::Ignored,
            }
        })
    }
}
