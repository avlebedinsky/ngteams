//! A bot invoke's one attempt ([`crate::bot`]): the chat service's own bearer, and in a Microsoft account's body the
//! Teams service token Teams puts there for the same action. An own-server account's body carries no token.
use teams_auth::token::SPACES_RESOURCE;

use super::{Auth, Credentials, HttpError, HttpTransport, Response, plain_headers};
use crate::Service;
use crate::bot::BotInvoke;

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn deliver_bot_invoke_write(&self, invoke: &BotInvoke) -> Result<Response, HttpError> {
        let url = self.url_for(Service::ChatService, &invoke.endpoint())?;
        if self.read_only {
            return Err(Self::refused_read_only("POST", url.as_str()));
        }
        let spaces = if self.cloud.is_microsoft() {
            Some(
                self.credentials
                    .bearer(SPACES_RESOURCE)
                    .await
                    .map_err(HttpError::Credentials)?,
            )
        } else {
            None
        };
        let token = self.token(Service::ChatService).await?;
        let body = invoke.body(spaces.as_ref()).to_string().into_bytes();
        let headers = plain_headers(Service::ChatService, true);
        let answer = self
            .send_as(
                reqwest::Method::POST,
                &url,
                &headers,
                Auth::bearer(token.as_ref()),
                Some(body),
            )
            .await?;
        if let (401, Some(audience), Some(refused)) = (answer.status(), super::audience(Service::ChatService), &token) {
            let _ = self.credentials.bearer_rejected(audience, refused).await;
        }
        Ok(answer)
    }
}
