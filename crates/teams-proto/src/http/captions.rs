//! The caption service's two credential forms. No caller supplies a URL outside the validated protocol types.
use base64::Engine as _;
use serde_json::Value;
use teams_auth::secrets::Secret;

use super::{Auth, Credentials, HttpError, HttpTransport, Refusal, Response, plain_headers};
use crate::captions::{CaptionWrite, CaptionWriteKind, RECORDER_AUDIENCE};
use crate::{IC3_AUDIENCE, Service};

impl<C: Credentials> HttpTransport<C> {
    pub(super) async fn deliver_caption_write(&self, write: &CaptionWrite) -> Result<Response, HttpError> {
        if self.read_only {
            return Err(Self::refused_read_only("POST", write.url().as_str()));
        }
        let recorder = self
            .credentials
            .bearer(RECORDER_AUDIENCE)
            .await
            .map_err(HttpError::Credentials)?;
        let skype = self
            .credentials
            .skypetoken()
            .await
            .map_err(HttpError::Credentials)?
            .ok_or(HttpError::Refused(Refusal::NoSkypetoken))?;
        let actor = write.from().mri.as_str();
        if !recorder_actor(&recorder, actor) || !skype_actor(&skype, actor) {
            return Err(HttpError::Refused(Refusal::Token));
        }
        let ic3 = if write.kind() == CaptionWriteKind::Invite {
            let token = self
                .credentials
                .bearer(IC3_AUDIENCE)
                .await
                .map_err(HttpError::Credentials)?;
            if !bearer_actor(&token, IC3_AUDIENCE, actor) {
                return Err(HttpError::Refused(Refusal::Token));
            }
            Some(token)
        } else {
            None
        };
        // Renewal can await; verify the canonical call's actor and current service link immediately before send.
        if self
            .call_state
            .as_ref()
            .is_none_or(|state| !state.caption_allowed(write))
        {
            return Err(HttpError::Refused(Refusal::CallState));
        }
        let mut headers = plain_headers(Service::Calling, true);
        headers.extend_from_slice(write.headers());
        let auth = match (ic3.as_ref(), write.kind()) {
            (Some(token), _) => Auth::Bearer(token),
            // a recording's start and stop carry the recorder token alone, as the web client sends them
            (None, CaptionWriteKind::Record | CaptionWriteKind::StopRecord) => Auth::Bearer(&recorder),
            (None, _) => Auth::Recorder {
                bearer: &recorder,
                skype: &skype,
            },
        };
        let answer = self
            .send_as(
                reqwest::Method::POST,
                write.url(),
                &headers,
                auth,
                Some(write.body(&recorder, &skype)),
            )
            .await?;
        if answer.status() == 401 {
            if let Some(token) = ic3.as_ref() {
                let _ = self.credentials.bearer_rejected(IC3_AUDIENCE, token).await;
            } else {
                let _ = self.credentials.bearer_rejected(RECORDER_AUDIENCE, &recorder).await;
            }
        }
        Ok(answer)
    }
}

fn payload(token: &Secret) -> Option<Value> {
    let mut parts = token.expose().split('.');
    let (_, Some(body), Some(_), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body).ok()?).ok()
}

fn bearer_actor(token: &Secret, audience: &str, actor: &str) -> bool {
    let Some(claims) = payload(token) else { return false };
    claims.get("aud").and_then(Value::as_str) == Some(audience)
        && claims.get("oid").and_then(Value::as_str).is_some_and(|oid| {
            actor
                .strip_prefix("8:orgid:")
                .is_some_and(|own| own.eq_ignore_ascii_case(oid))
        })
}

fn recorder_actor(token: &Secret, actor: &str) -> bool {
    let Some(claims) = payload(token) else { return false };
    bearer_actor(token, RECORDER_AUDIENCE, actor)
        && claims.get("scp").and_then(Value::as_str).is_some_and(|scopes| {
            scopes
                .split_whitespace()
                .any(|scope| scope == "access_recorder_service")
        })
}

fn skype_actor(token: &Secret, actor: &str) -> bool {
    payload(token)
        .and_then(|claims| claims.get("skypeid").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|id| actor == format!("8:{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn token(value: &Value) -> Secret {
        Secret::new(format!(
            "synthetic.{}.synthetic",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        ))
    }
    #[test]
    fn recorder_scope_audience_and_actor_and_skype_actor_must_all_match() {
        let actor = "8:orgid:00000000-0000-4000-8000-00000000000a";
        let mut claims = json!({"aud":RECORDER_AUDIENCE,"oid":"00000000-0000-4000-8000-00000000000a","scp":"access_recorder_service"});
        assert!(recorder_actor(&token(&claims), actor));
        claims["scp"] = json!("unrelated");
        assert!(!recorder_actor(&token(&claims), actor));
        claims["scp"] = json!("access_recorder_service");
        claims["aud"] = json!("other-resource");
        assert!(!recorder_actor(&token(&claims), actor));
        claims["aud"] = json!(RECORDER_AUDIENCE);
        claims["oid"] = json!("00000000-0000-4000-8000-00000000000b");
        assert!(!recorder_actor(&token(&claims), actor));
        assert!(skype_actor(
            &token(&json!({"skypeid":"orgid:00000000-0000-4000-8000-00000000000a"})),
            actor
        ));
        assert!(!skype_actor(
            &token(&json!({"skypeid":"orgid:00000000-0000-4000-8000-00000000000b"})),
            actor
        ));
    }
}
