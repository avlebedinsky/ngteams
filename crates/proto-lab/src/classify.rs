//! What a browser request is, as far as the fence is concerned.
//!
//! [`classify`] sorts a request into:
//! - [`Classification::Read`]: `GET`, `HEAD` or `OPTIONS`, which change nothing anyone else sees;
//! - [`Classification::Own`]: a mutating request on the seeded, reviewed own-state list (the Entra token endpoint and
//!   Teams' authz exchange), which changes only the signed-in account's own session; `proto-lab login` adds Entra's
//!   sign-in form ([`Classifier::sign_in`]);
//! - [`Classification::Write`]: a mapped write with its [`Target`], judged by [`teams_fence::Fence::check`];
//! - [`Classification::Unknown`]: everything else, denied.
//!
//! Until the write-target mapping exists (M0 Task 11) nothing maps to [`Classification::Write`], so the fence is
//! fail-closed: every mutating request outside the own-state list is [`Classification::Unknown`]. Any method other than
//! the three reading ones counts as mutating, including ones the classifier has never heard of.
//!
//! An own-state rule matches the method, the exact origin (scheme, host and port) and the path segment by segment; a
//! `*` segment stands for exactly one non-empty segment (an Entra tenant). The query plays no part.

use teams_fence::Target;
use url::Url;

/// A reviewed kind of own-state request: it changes only the signed-in account's own session state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OwnKind {
    /// The Entra token endpoint: code exchange and refresh.
    EntraToken,
    /// Teams' authz exchange: an access token for a skypetoken and the regional endpoints.
    Authz,
    /// Entra's sign-in form: the username lookup, the password and "Stay signed in?". Only `proto-lab login` allows
    /// it ([`Classifier::sign_in`]).
    EntraSignIn,
}

impl OwnKind {
    /// The name used in `fence.log`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EntraToken => "entra_token",
            Self::Authz => "authz",
            Self::EntraSignIn => "entra_sign_in",
        }
    }
}

/// What the fence makes of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Classification {
    /// A reading method: let through.
    Read,
    /// A mutating request on the own-state list: let through.
    Own(OwnKind),
    /// A mapped write: let through only with a permit from the fence.
    Write(Target),
    /// A mutating request nothing classifies: denied.
    Unknown,
}

/// An exact origin and a path matched segment by segment; the query plays no part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlPattern {
    origin: String,
    path: Vec<String>,
}

impl UrlPattern {
    /// `origin` is `scheme://host[:port]` as a browser serializes it (no default port, lowercase host); `path` starts
    /// with `/`, and a `*` segment stands for one non-empty segment.
    #[must_use]
    pub fn new(origin: &str, path: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            path: segments(path).map(str::to_owned).collect(),
        }
    }

    /// Whether `url` has exactly this origin and a path this pattern matches.
    #[must_use]
    pub fn matches(&self, url: &Url) -> bool {
        if self.origin != url.origin().ascii_serialization() {
            return false;
        }
        let mut path = segments(url.path());
        let mut pattern = self.path.iter();
        loop {
            match (pattern.next(), path.next()) {
                (None, None) => return true,
                (Some(wanted), Some(segment)) if !segment.is_empty() && (wanted == "*" || wanted == segment) => {}
                _ => return false,
            }
        }
    }
}

/// One entry of the own-state list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnRule {
    kind: OwnKind,
    method: String,
    url: UrlPattern,
}

impl OwnRule {
    /// `origin` and `path` as [`UrlPattern::new`] takes them.
    #[must_use]
    pub fn new(kind: OwnKind, method: &str, origin: &str, path: &str) -> Self {
        Self {
            kind,
            method: method.to_ascii_uppercase(),
            url: UrlPattern::new(origin, path),
        }
    }

    fn matches(&self, method: &str, url: &Url) -> bool {
        self.method == method && self.url.matches(url)
    }
}

/// The path's segments after the leading `/`.
fn segments(path: &str) -> std::str::Split<'_, char> {
    path.strip_prefix('/').unwrap_or(path).split('/')
}

/// The authz endpoints of the commercial cloud, as the web client names them.
const AUTHZ: [(&str, &str); 4] = [
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authz"),
    ("https://teams.microsoft.com", "/api/authsvc/v1.0/authzcore"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authz"),
    ("https://authsvc.teams.microsoft.com", "/v1.0/authzcore"),
];

/// Entra's sign-in form on `login.microsoftonline.com`, as the sign-in page posts it (`*` is `common`,
/// `organizations` or a tenant): the username lookup, the password, and the answer to "Stay signed in?". Nothing
/// of multi-factor sign-in (`SAS/*`) or of the account's security info is on it.
const SIGN_IN: [&str; 4] = ["/*/GetCredentialType", "/*/login", "/kmsi", "/*/kmsi"];

/// The classifier: the own-state list (and, from M0 Task 11, the write mapping).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classifier {
    own: Vec<OwnRule>,
}

impl Classifier {
    /// The seeded own-state list: `POST` to the Entra v2 token endpoint of any tenant on `login.microsoftonline.com`,
    /// and `POST` to Teams' authz endpoints.
    #[must_use]
    pub fn seeded() -> Self {
        let mut own = vec![OwnRule::new(
            OwnKind::EntraToken,
            "POST",
            "https://login.microsoftonline.com",
            "/*/oauth2/v2.0/token",
        )];
        own.extend(
            AUTHZ
                .iter()
                .map(|(origin, path)| OwnRule::new(OwnKind::Authz, "POST", origin, path)),
        );
        Self { own }
    }

    /// The seeded list plus Entra's sign-in form ([`OwnKind::EntraSignIn`]): what `proto-lab login` needs to type a
    /// lab account's password into Microsoft's sign-in page under the fence. The form changes only that account's own
    /// sign-in session. Nothing else uses it: `status` and the scenarios never sign in.
    #[must_use]
    pub fn sign_in() -> Self {
        let mut classifier = Self::seeded();
        classifier.own.extend(
            SIGN_IN
                .iter()
                .map(|path| OwnRule::new(OwnKind::EntraSignIn, "POST", "https://login.microsoftonline.com", path)),
        );
        classifier
    }

    /// The same list plus one more own-state rule. Tests use it to stand a local endpoint in for a Microsoft one.
    #[must_use]
    pub fn with_own(mut self, rule: OwnRule) -> Self {
        self.own.push(rule);
        self
    }

    /// Classifies one request. The body is unused until writes are mapped (M0 Task 11).
    #[must_use]
    pub fn classify(&self, method: &str, url: &str, _body: Option<&[u8]>) -> Classification {
        let method = method.to_ascii_uppercase();
        if !is_mutating(&method) {
            return Classification::Read;
        }
        let Ok(url) = Url::parse(url) else {
            return Classification::Unknown;
        };
        self.own
            .iter()
            .find(|rule| rule.matches(&method, &url))
            .map_or(Classification::Unknown, |rule| Classification::Own(rule.kind))
    }
}

/// Whether a method can change anything: every method but `GET`, `HEAD` and `OPTIONS` (in any case).
#[must_use]
pub fn is_mutating(method: &str) -> bool {
    !["GET", "HEAD", "OPTIONS"]
        .iter()
        .any(|reading| reading.eq_ignore_ascii_case(method))
}

/// Classifies one request with the seeded classifier.
#[must_use]
pub fn classify(method: &str, url: &str, body: Option<&[u8]>) -> Classification {
    Classifier::seeded().classify(method, url, body)
}
