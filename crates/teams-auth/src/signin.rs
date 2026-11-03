//! The in-process sign-in window handoff. The window catches a redirect and returns it directly to the
//! authorization that opened it; the code is redeemed only after its redirect URI and state match, and its ID token
//! names the requested account. No pipe, JSON line or child window process carries the authorization code.

use std::{fmt, io};

use crate::redirect::RedirectError;
use crate::refresh::SignIn;
use crate::secrets::{Secret, SecretStore};
use crate::token::{Authorization, TokenClient, TokenError};

/// Why the window gave no code. Never carries the redirect URL, the code or the state.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("the sign-in window task failed: {0}")]
    Io(#[source] io::Error),
    #[error("the sign-in window was closed before the sign-in finished")]
    Closed,
    #[error("the sign-in window was closed after it refused to go to {0}")]
    Refused(String),
    #[error("the sign-in window failed: {0}")]
    Failed(String),
    #[error(transparent)]
    Redirect(#[from] RedirectError),
}

/// Why signing in through the window stored nothing ([`sign_in_with`]).
#[derive(Debug, thiserror::Error)]
pub enum WindowSignInError {
    #[error(transparent)]
    Window(#[from] WindowError),
    #[error(transparent)]
    Token(#[from] TokenError),
}

/// What the window is asked to do: open `authorize`, and catch the redirect to `redirect_uri` before it loads.
#[derive(Clone, PartialEq, Eq)]
pub struct SignInRequest {
    pub authorize: String,
    pub redirect_uri: String,
}

impl fmt::Debug for SignInRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the authorize URL carries the request's state
        f.debug_struct("SignInRequest")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl SignInRequest {
    /// The request for `authorization`: its authorize URL and redirect URI.
    #[must_use]
    pub fn for_authorization(authorization: &Authorization) -> Self {
        Self {
            authorize: authorization.url().to_owned(),
            redirect_uri: authorization.redirect_uri().to_owned(),
        }
    }
}

/// The window's answer.
#[derive(Clone)]
pub enum SignInAnswer {
    /// The URL Entra redirected to (a [`Secret`]: it carries the code).
    Redirect(Secret),
    /// The window was closed before a redirect.
    Closed,
    /// The window was closed before a redirect, after it refused to go to this host (off its host bound).
    Refused(String),
    /// The window gave up: a short reason that never holds the redirect.
    Failed(String),
}

impl fmt::Debug for SignInAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect(_) => f.write_str("Redirect(<redacted>)"),
            Self::Closed => f.write_str("Closed"),
            Self::Refused(host) => f.debug_tuple("Refused").field(host).finish(),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

/// The code in a window's `answer` to `authorization`'s request, once the redirect is shown to answer this request
/// ([`Authorization::code_from_redirect`]).
///
/// # Errors
/// [`WindowError::Closed`], [`WindowError::Refused`] or [`WindowError::Failed`] when the window gave no redirect;
/// [`WindowError::Redirect`] when the redirect isn't this request's or carries Entra's refusal.
pub fn code_from_answer(authorization: &Authorization, answer: SignInAnswer) -> Result<Secret, WindowError> {
    match answer {
        SignInAnswer::Redirect(url) => Ok(authorization.code_from_redirect(url.expose())?),
        SignInAnswer::Closed => Err(WindowError::Closed),
        SignInAnswer::Refused(host) => Err(WindowError::Refused(host)),
        SignInAnswer::Failed(reason) => Err(WindowError::Failed(reason)),
    }
}

/// Signs `authorization`'s account in through `window`, which shows the request's page and gives its answer
/// (blocking, run on tokio's blocking pool: the person signing in takes their time; the personal build's in-app
/// window, say), then redeems the code ([`TokenClient::exchange_code`]): the refresh token is stored only once the ID
/// token names the authorization's account.
///
/// # Errors
/// [`WindowSignInError::Window`] when the window gave no code for this request, [`WindowSignInError::Token`] when the
/// redemption failed or named another account (nothing stored then).
pub async fn sign_in_with<S: SecretStore>(
    client: &TokenClient,
    store: &S,
    authorization: &Authorization,
    window: impl FnOnce(SignInRequest) -> SignInAnswer + Send + 'static,
) -> Result<SignIn, WindowSignInError> {
    let request = authorization.clone();
    let code = tokio::task::spawn_blocking(move || {
        let answer = window(SignInRequest::for_authorization(&request));
        code_from_answer(&request, answer)
    })
    .await
    .map_err(|error| WindowError::Io(io::Error::other(error.to_string())))??;
    Ok(client.exchange_code(store, authorization, code.expose()).await?)
}
