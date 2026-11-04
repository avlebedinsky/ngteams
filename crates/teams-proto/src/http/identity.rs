//! Shared authenticated-account checks and logged reads used by write preflights.

use super::{Credentials, HttpError, HttpTransport, Refusal, Response};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn verify_account_actor(&self) -> Result<(), HttpError> {
        let own = self
            .account
            .as_ref()
            .and_then(|mri| crate::object_id(mri.as_str()))
            .ok_or(HttpError::Refused(Refusal::NotAllowed))?;
        let response = self
            .preflight_read(
                crate::Service::Graph,
                crate::Endpoint::new(["me"]).with_query("$select", "id,userPrincipalName"),
            )
            .await?;
        let observed: serde_json::Value =
            serde_json::from_slice(response.body()).map_err(|_| HttpError::Refused(Refusal::NotAllowed))?;
        if observed.get("id").and_then(serde_json::Value::as_str) != Some(own) {
            return Err(HttpError::Refused(Refusal::NotAllowed));
        }
        Ok(())
    }

    pub(super) async fn preflight_read(
        &self,
        service: crate::Service,
        endpoint: crate::Endpoint,
    ) -> Result<Response, HttpError> {
        struct Read {
            service: crate::Service,
            endpoint: crate::Endpoint,
        }
        impl crate::Read for Read {
            fn service(&self) -> crate::Service {
                self.service
            }
            fn endpoint(&self) -> crate::Endpoint {
                self.endpoint.clone()
            }
        }
        crate::Transport::read(self, &Read { service, endpoint }).await
    }
}
