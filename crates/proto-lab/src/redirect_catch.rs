//! Authorization redirect capture: response redirects and script navigations never load the registered callback.
use serde_json::{Value, json};
use teams_auth::redirect::{RedirectError, RedirectUri};
use teams_auth::secrets::Secret;
use tokio::sync::mpsc;
use url::Url;

/// Why a [`RedirectCatch`] can't be made.
#[derive(Debug, thiserror::Error)]
pub enum CatchError {
    /// The redirect URI isn't usable.
    #[error(transparent)]
    RedirectUri(#[from] RedirectError),
    /// The authorize endpoint has no HTTP(S) origin whose responses could be paused.
    #[error("the authorize endpoint must be an absolute http(s) URL")]
    AuthorizeEndpoint,
}

/// Catches the authorization redirect to one registered redirect URI (see the module docs). Made for one
/// `proto-lab auth` run; the URL it catches carries the authorization code, so it is handed over as a [`Secret`].
#[derive(Debug)]
pub struct RedirectCatch {
    /// The origin whose responses pause at the response stage: the authorize endpoint's.
    login_origin: String,
    redirect: RedirectUri,
    caught: mpsc::UnboundedSender<Secret>,
}

impl RedirectCatch {
    /// A catch for redirects to `redirect_uri` from the origin of `authorize_endpoint`, and the receiver of the URLs it
    /// catches.
    ///
    /// # Errors
    /// [`CatchError::RedirectUri`] when `redirect_uri` isn't a bare absolute URL ([`RedirectUri::parse`]);
    /// [`CatchError::AuthorizeEndpoint`] when `authorize_endpoint` has no HTTP(S) origin.
    pub fn new(
        authorize_endpoint: &str,
        redirect_uri: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Secret>), CatchError> {
        let redirect = RedirectUri::parse(redirect_uri)?;
        let login_origin = Url::parse(authorize_endpoint)
            .ok()
            .filter(|url| matches!(url.scheme(), "https" | "http"))
            .map(|url| url.origin().ascii_serialization())
            .ok_or(CatchError::AuthorizeEndpoint)?;
        let (caught, receiver) = mpsc::unbounded_channel();
        let catch = Self {
            login_origin,
            redirect,
            caught,
        };
        Ok((catch, receiver))
    }

    /// The `Fetch` pattern that pauses the login origin's responses.
    pub(crate) fn response_pattern(&self) -> Value {
        json!({"urlPattern": format!("{}/*", self.login_origin), "requestStage": "Response"})
    }

    /// Whether `url` is the redirect; if it is, it goes to the receiver.
    pub(crate) fn take(&self, url: &str) -> bool {
        let caught = self.redirect.matches(url);
        if caught {
            // a receiver that is gone has stopped waiting; the redirect is failed all the same
            let _ = self.caught.send(Secret::new(url));
        }
        caught
    }
}

/// The URL a paused 3xx response redirects to (its `Location`, resolved against the request's URL).
pub(crate) fn redirect_target(params: &Value) -> Option<String> {
    let status = params["responseStatusCode"].as_u64()?;
    if !(300..400).contains(&status) {
        return None;
    }
    let location = params["responseHeaders"].as_array()?.iter().find(|header| {
        header["name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("location"))
    })?["value"]
        .as_str()?;
    let base = Url::parse(params["request"]["url"].as_str()?).ok()?;
    base.join(location).ok().map(String::from)
}

/// Whether a `Fetch.requestPaused` event is at the response stage.
pub(crate) fn at_response_stage(params: &Value) -> bool {
    params.get("responseStatusCode").is_some() || params.get("responseErrorReason").is_some()
}
