//! The parent's side of the sign-in window (`teams-signin`, M2 Task 24): Microsoft's sign-in page in a `WebKitGTK`
//! webview, in a process of its own, which hands back the redirect that carries the authorization code.
//!
//! The wire is one JSON line each way:
//! - on the window's stdin, the request: `{"authorize": <the authorize URL>, "redirect_uri": <the registered redirect
//!   URI>}` ([`WindowRequest`]);
//! - on its stdout, the answer ([`WindowAnswer`]): `{"redirect": <the URL Entra redirected to>}`, `{"closed": true}`
//!   when the window was closed before a redirect, or `{"failed": <a short reason>}`.
//!
//! The window never redeems anything and never logs the redirect: the code goes only into the answer. Here the answer
//! is tied to the request ([`Authorization::code_from_redirect`]: the registered redirect URI, the request's `state`
//! once), and only then is the code redeemed ([`sign_in_with_window`] → [`TokenClient::exchange_code`]), which stores
//! the refresh token only once the ID token names the authorization's account (in the lab, the alias's account; in the
//! personal build, the account it signs in).
//!
//! No error or `Debug` output carries the redirect URL, the code or the state.

use std::fmt;
use std::io::{self, BufRead as _, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

use crate::redirect::{RedirectError, RedirectUri};
use crate::refresh::SignIn;
use crate::secrets::{Secret, SecretStore};
use crate::token::{Authorization, TokenClient, TokenError};

/// The longest line either side takes (the redirect's URL with its code is a few kilobytes).
pub const LINE_LIMIT: usize = 64 * 1024;
/// The longest reason a `failed` answer may give.
pub const REASON_LIMIT: usize = 300;
/// How long the window has to exit after its answer before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(10);
const EXIT_POLL: Duration = Duration::from_millis(50);

/// Why the window gave no code. Never carries the redirect URL, the code or the state.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("the sign-in window can't be started: {0}")]
    Spawn(#[source] io::Error),
    #[error("talking to the sign-in window failed: {0}")]
    Io(#[source] io::Error),
    /// A line that isn't the protocol's: not JSON, a key it doesn't have, a value of the wrong type.
    #[error("the sign-in window's line isn't the protocol's")]
    Malformed,
    #[error("a line of the sign-in window is longer than {LINE_LIMIT} bytes")]
    TooLong,
    /// The window ended without a line.
    #[error("the sign-in window ended without an answer")]
    NoAnswer,
    /// The window was closed before Entra redirected.
    #[error("the sign-in window was closed before the sign-in finished")]
    Closed,
    /// The window gave up, for this reason.
    #[error("the sign-in window failed: {0}")]
    Failed(String),
    /// The redirect isn't the answer to this request, or Entra refused.
    #[error(transparent)]
    Redirect(#[from] RedirectError),
}

/// Why signing in through the window stored nothing ([`sign_in_with_window`]).
#[derive(Debug, thiserror::Error)]
pub enum WindowSignInError {
    #[error(transparent)]
    Window(#[from] WindowError),
    #[error(transparent)]
    Token(#[from] TokenError),
}

/// What the window is asked to do: open `authorize`, and catch the redirect to `redirect_uri` before it loads.
#[derive(Clone, PartialEq, Eq)]
pub struct WindowRequest {
    pub authorize: String,
    pub redirect_uri: String,
}

impl fmt::Debug for WindowRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // the authorize URL carries the request's state
        f.debug_struct("WindowRequest")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

impl WindowRequest {
    /// The request for `authorization`: its authorize URL and redirect URI.
    #[must_use]
    pub fn for_authorization(authorization: &Authorization) -> Self {
        Self {
            authorize: authorization.url().to_owned(),
            redirect_uri: authorization.redirect_uri().to_owned(),
        }
    }

    /// The request as one line, without its newline.
    #[must_use]
    pub fn to_line(&self) -> String {
        json!({"authorize": self.authorize, "redirect_uri": self.redirect_uri}).to_string()
    }

    /// Reads a request line: exactly the two keys, each a string, the redirect URI a usable one
    /// ([`RedirectUri::parse`]).
    ///
    /// # Errors
    /// [`WindowError::TooLong`] or [`WindowError::Malformed`].
    pub fn parse(line: &str) -> Result<Self, WindowError> {
        let object = object_of(line)?;
        if object.len() != 2 {
            return Err(WindowError::Malformed);
        }
        let text = |key: &str| object.get(key).and_then(Value::as_str).map(str::to_owned);
        let (Some(authorize), Some(redirect_uri)) = (text("authorize"), text("redirect_uri")) else {
            return Err(WindowError::Malformed);
        };
        RedirectUri::parse(&redirect_uri).map_err(|_| WindowError::Malformed)?;
        Ok(Self {
            authorize,
            redirect_uri,
        })
    }
}

/// The window's answer.
pub enum WindowAnswer {
    /// The URL Entra redirected to (a [`Secret`]: it carries the code).
    Redirect(Secret),
    /// The window was closed before a redirect.
    Closed,
    /// The window gave up: a short reason that never holds the redirect.
    Failed(String),
}

impl fmt::Debug for WindowAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect(_) => f.write_str("Redirect(<redacted>)"),
            Self::Closed => f.write_str("Closed"),
            Self::Failed(reason) => f.debug_tuple("Failed").field(reason).finish(),
        }
    }
}

impl WindowAnswer {
    /// The answer as one line, without its newline (a [`Secret`]: a redirect's line carries the code).
    #[must_use]
    pub fn to_line(&self) -> Secret {
        let value = match self {
            Self::Redirect(url) => json!({"redirect": url.expose()}),
            Self::Closed => json!({"closed": true}),
            Self::Failed(reason) => json!({"failed": short_reason(reason)}),
        };
        Secret::new(value.to_string())
    }

    /// Reads an answer line: exactly one of its three forms.
    ///
    /// # Errors
    /// [`WindowError::TooLong`] or [`WindowError::Malformed`].
    pub fn parse(line: &str) -> Result<Self, WindowError> {
        let object = object_of(line)?;
        let mut entries = object.into_iter();
        let (Some((key, value)), None) = (entries.next(), entries.next()) else {
            return Err(WindowError::Malformed);
        };
        match (key.as_str(), value) {
            ("redirect", Value::String(url)) => Ok(Self::Redirect(Secret::new(url))),
            ("closed", Value::Bool(true)) => Ok(Self::Closed),
            ("failed", Value::String(reason)) if reason.len() <= REASON_LIMIT => Ok(Self::Failed(reason)),
            _ => Err(WindowError::Malformed),
        }
    }
}

/// `reason` cut to [`REASON_LIMIT`] bytes at a character boundary.
fn short_reason(reason: &str) -> &str {
    let mut end = reason.len().min(REASON_LIMIT);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    &reason[..end]
}

fn object_of(line: &str) -> Result<Map<String, Value>, WindowError> {
    if line.len() > LINE_LIMIT {
        return Err(WindowError::TooLong);
    }
    match serde_json::from_str(line) {
        Ok(Value::Object(object)) => Ok(object),
        _ => Err(WindowError::Malformed),
    }
}

/// Reads one line of at most [`LINE_LIMIT`] bytes, without its newline; `None` at the end of the stream. The buffer is
/// zeroed when dropped: a redirect's line carries the code.
///
/// # Errors
/// [`WindowError::TooLong`], [`WindowError::Malformed`] (not UTF-8) or [`WindowError::Io`].
pub fn read_line(reader: impl Read) -> Result<Option<Zeroizing<String>>, WindowError> {
    let limit = u64::try_from(LINE_LIMIT).unwrap_or(u64::MAX) + 1;
    let mut bytes = Zeroizing::new(Vec::new());
    BufReader::new(reader.take(limit))
        .read_until(b'\n', &mut bytes)
        .map_err(WindowError::Io)?;
    if bytes.is_empty() {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    } else if bytes.len() > LINE_LIMIT {
        return Err(WindowError::TooLong);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| WindowError::Malformed)?;
    Ok(Some(Zeroizing::new(text.to_owned())))
}

/// Asks the window on `input` for `authorization`'s sign-in and reads its answer from `output`: the code, once the
/// redirect is shown to answer this request ([`Authorization::code_from_redirect`]).
///
/// # Errors
/// [`WindowError::Closed`], [`WindowError::Failed`] or [`WindowError::NoAnswer`] when the window gave no redirect;
/// [`WindowError::Redirect`] when the redirect isn't this request's or carries Entra's refusal; [`WindowError::Io`],
/// [`WindowError::Malformed`] or [`WindowError::TooLong`] on the wire.
pub fn code_from_window(
    authorization: &Authorization,
    mut input: impl Write,
    output: impl Read,
) -> Result<Secret, WindowError> {
    let mut line = WindowRequest::for_authorization(authorization).to_line();
    line.push('\n');
    input
        .write_all(line.as_bytes())
        .and_then(|()| input.flush())
        .map_err(WindowError::Io)?;
    let answer = read_line(output)?.ok_or(WindowError::NoAnswer)?;
    match WindowAnswer::parse(&answer)? {
        WindowAnswer::Redirect(url) => Ok(authorization.code_from_redirect(url.expose())?),
        WindowAnswer::Closed => Err(WindowError::Closed),
        WindowAnswer::Failed(reason) => Err(WindowError::Failed(reason)),
    }
}

/// Starts `command` (the `teams-signin` binary with its arguments) with its stdin and stdout piped, asks it for
/// `authorization`'s sign-in and reads its answer ([`code_from_window`]); then waits for it to exit, killing it after
/// [`EXIT_GRACE`]. Blocks until the window answers: the person signing in takes their time.
///
/// # Errors
/// [`WindowError::Spawn`] when it can't be started, else as [`code_from_window`].
pub fn run_window(mut command: Command, authorization: &Authorization) -> Result<Secret, WindowError> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(WindowError::Spawn)?;
    let result = match (child.stdin.take(), child.stdout.take()) {
        (Some(input), Some(output)) => code_from_window(authorization, input, output),
        _ => Err(WindowError::NoAnswer),
    };
    reap(&mut child);
    result
}

/// Waits for `child` to exit, killing it after [`EXIT_GRACE`].
fn reap(child: &mut Child) {
    let deadline = Instant::now() + EXIT_GRACE;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => std::thread::sleep(EXIT_POLL),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Signs `authorization`'s account in through the window `command` starts ([`run_window`], on tokio's blocking pool),
/// then redeems the code ([`TokenClient::exchange_code`]): the refresh token is stored only once the ID token names
/// the authorization's account.
///
/// # Errors
/// [`WindowSignInError::Window`] when the window gave no code for this request, [`WindowSignInError::Token`] when the
/// redemption failed or named another account (nothing stored then).
pub async fn sign_in_with_window<S: SecretStore>(
    client: &TokenClient,
    store: &S,
    authorization: &Authorization,
    command: Command,
) -> Result<SignIn, WindowSignInError> {
    let request = authorization.clone();
    let code = tokio::task::spawn_blocking(move || run_window(command, &request))
        .await
        .map_err(|error| WindowError::Io(io::Error::other(error.to_string())))??;
    Ok(client.exchange_code(store, authorization, code.expose()).await?)
}
