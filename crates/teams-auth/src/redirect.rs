//! The authorization redirect: recognizing the registered redirect URI, and taking the `code` (or Entra's `error`)
//! from the redirect once its `state` is the request's.
//!
//! The browser step never lets the redirect load: the lab catches it from the `Location` of Entra's 302 (the
//! `nativeclient` page, or the `msauth.com.microsoft.teams2://auth` custom scheme Chrome won't navigate to) and hands
//! the URL here. [`Authorization::code_from_redirect`] refuses anything that isn't a redirect to the authorization's own
//! redirect URI carrying its `state`, so a code from another request, or a URL a page made up, is never redeemed.
//!
//! The code comes back as a [`Secret`]. No error carries the code, the state, or the URL.

use url::Url;

use crate::secrets::Secret;
use crate::token::{Authorization, EntraError, split_description};

/// A registered redirect URI, compared by scheme, host, port and path; the query and fragment are where the answer
/// travels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedirectUri {
    url: Url,
}

impl RedirectUri {
    /// A redirect URI: an absolute URL with a host and no user info, query or fragment.
    ///
    /// # Errors
    /// [`RedirectError::InvalidRedirectUri`] otherwise.
    pub fn parse(uri: &str) -> Result<Self, RedirectError> {
        let url = Url::parse(uri).map_err(|_| RedirectError::InvalidRedirectUri)?;
        let bare = url.host_str().is_some_and(|host| !host.is_empty())
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if bare {
            Ok(Self { url })
        } else {
            Err(RedirectError::InvalidRedirectUri)
        }
    }

    /// Whether `candidate` is a URL to this redirect URI: the same scheme, host, port and path (exactly: no extra
    /// slash, no other path), with no user info. The query and fragment don't matter.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        let Ok(candidate) = Url::parse(candidate) else {
            return false;
        };
        candidate.scheme() == self.url.scheme()
            && candidate.host_str() == self.url.host_str()
            && candidate.port_or_known_default() == self.url.port_or_known_default()
            && candidate.path() == self.url.path()
            && candidate.username().is_empty()
            && candidate.password().is_none()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.url.as_str()
    }
}

/// Why a redirect gave no code. Never carries the code, the state or the URL.
#[derive(Debug, thiserror::Error)]
pub enum RedirectError {
    /// The redirect URI itself isn't usable (see [`RedirectUri::parse`]).
    #[error("the redirect URI must be an absolute URL with a host and no user info, query or fragment")]
    InvalidRedirectUri,
    /// The URL isn't a redirect to the authorization's redirect URI.
    #[error("not a redirect to the registered redirect URI")]
    NotTheRedirect,
    /// No `state` came back, so the answer can't be tied to the request.
    #[error("the redirect carries no state")]
    MissingState,
    /// The `state` isn't the request's: the answer to some other request, or made up.
    #[error("the redirect's state is not the request's")]
    StateMismatch,
    /// A parameter the answer is read from came twice.
    #[error("the redirect carries {0} more than once")]
    Repeated(&'static str),
    /// Neither a `code` nor an `error`.
    #[error("the redirect carries neither a code nor an error")]
    MissingCode,
    /// Entra answered with an error instead of a code (cut before its trace part, at most 300 characters).
    #[error("Entra refused the authorization: {0}")]
    Refused(Box<EntraError>),
}

impl Authorization {
    /// The authorization code in `location`, the URL Entra redirected to, once it is shown to answer this request:
    /// a URL to this authorization's redirect URI whose query carries this authorization's `state` (once).
    ///
    /// # Errors
    /// [`RedirectError::NotTheRedirect`], [`RedirectError::MissingState`], [`RedirectError::StateMismatch`] or
    /// [`RedirectError::Repeated`] when the URL can't be tied to this request; [`RedirectError::Refused`] when Entra
    /// answered with an error; [`RedirectError::MissingCode`] when it answered with neither.
    pub fn code_from_redirect(&self, location: &str) -> Result<Secret, RedirectError> {
        let redirect = RedirectUri::parse(self.redirect_uri())?;
        if !redirect.matches(location) {
            return Err(RedirectError::NotTheRedirect);
        }
        let url = Url::parse(location).map_err(|_| RedirectError::NotTheRedirect)?;
        let param = |name: &'static str| -> Result<Option<String>, RedirectError> {
            let mut values = url.query_pairs().filter(|(key, _)| key == name).map(|(_, value)| value);
            let first = values.next().map(std::borrow::Cow::into_owned);
            match values.next() {
                Some(_) => Err(RedirectError::Repeated(name)),
                None => Ok(first),
            }
        };
        let state = param("state")?.ok_or(RedirectError::MissingState)?;
        if state != self.state() {
            return Err(RedirectError::StateMismatch);
        }
        if let Some(error) = param("error")? {
            let (code, description) = split_description(&param("error_description")?.unwrap_or_default());
            return Err(RedirectError::Refused(Box::new(EntraError {
                error,
                codes: code.into_iter().collect(),
                description,
                correlation_id: None,
                suberror: None,
            })));
        }
        match param("code")? {
            Some(code) if !code.is_empty() => Ok(Secret::new(code)),
            _ => Err(RedirectError::MissingCode),
        }
    }
}
