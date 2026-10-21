//! Reads by `POST` (M2 Task 11): the native client's fourth sealed list, for requests the web client sends as a `POST`
//! although they change nothing (CLAUDE.md, "The native client's fourth list", the user's decision of 2026-10-21).
//!
//! The list is the set of types implementing [`PostRead`], and the trait is sealed, so only this crate adds to it.
//! Each entry is added only after a review, in its exact captured shape: its [`Service`] (and so its host), the
//! method (`POST`), the path, every query parameter and its value, the headers that change what it does, and the
//! body schema (a non-empty JSON list of MRIs). It names the MRIs it asks about ([`PostRead::names`]), and it goes out
//! only inside an [`AuthorizedPostRead`], next to the [`PostReadPermit`] `Fence::check_post_read` gave for exactly
//! that entry and that ordered list (in a lab build: every one a lab account, none twice).
//!
//! The transport then reads the request it is about to send back against the entry's captured shape (the URL's
//! path and query, every header but the token, the body as bytes) and sends nothing that differs in any way, nor one
//! whose body names other MRIs than the permit's: [`HttpError::Unknown`](crate::HttpError::Unknown). It logs the
//! request to `fence.log` before it sends it, sends nothing it couldn't log, and sends each one once: a read by `POST`
//! is never retried under its permit.
//!
//! An entry goes on the list only when its captured answer names nobody but the accounts asked for, it hands no token
//! to a third party, and it is never a search or a suggestion list.
//!
//! The entries:
//! - [`ShortProfileLookup`]: the middle tier's `users/fetchShortProfile` by MRI, in the query the web client always
//!   sent (`isMailAddress=false&…`), the browser fence's `short_profile` (`crates/proto-lab/src/post_read.rs`).

use std::fmt;

use serde::Deserialize;
use serde_json::Value;
use teams_fence::{Mri, PostReadPermit};

use crate::endpoint::{Endpoint, Service};

/// A reviewed read by `POST` (see the module docs). Sealed: no other crate can implement it.
pub trait PostRead: sealed::Sealed {
    /// The entry's name: what the fence's decision and `fence.log` call it.
    const KIND: &'static str;

    /// The MRIs it asks about, in the order its body lists them.
    fn names(&self) -> &[Mri];
}

mod sealed {
    use super::{PostReadWire, Shape};

    /// The seal on [`PostRead`](super::PostRead): a public trait no other crate can name, implemented for this crate's
    /// entries only. It carries the entry's captured shape and its wire form, built from the same fields as its names.
    pub trait Sealed {
        /// The shape the capture showed, which the transport checks the request it sends against.
        fn shape(&self) -> &'static Shape;

        /// What the transport sends.
        fn wire(&self) -> PostReadWire;
    }

    impl Sealed for super::ShortProfileLookup {
        fn shape(&self) -> &'static Shape {
            &super::SHORT_PROFILE
        }

        fn wire(&self) -> PostReadWire {
            PostReadWire::lookup(&super::SHORT_PROFILE, &self.names)
        }
    }

    #[cfg(feature = "test-support")]
    impl Sealed for super::ProbePostRead {
        fn shape(&self) -> &'static Shape {
            &super::SHORT_PROFILE
        }

        fn wire(&self) -> PostReadWire {
            let mut wire = PostReadWire::lookup(&super::SHORT_PROFILE, &self.lookup.names);
            for (name, value) in &self.query {
                wire.endpoint = wire.endpoint.with_query(name.clone(), value.clone());
            }
            wire.headers.extend(self.headers.iter().cloned());
            if let Some(body) = &self.body {
                wire.body = body.clone();
            }
            wire
        }
    }
}

/// An entry's shape as the capture showed it: the service, the path under its base, the query (each pair once,
/// nothing else, in any order) and the headers it is sent with besides the token (each once, exactly these values,
/// all of them). Its body is always a non-empty JSON list of MRIs. Only this crate makes or reads one.
#[derive(Debug)]
pub struct Shape {
    service: Service,
    path: &'static [&'static str],
    query: &'static [(&'static str, &'static str)],
    headers: &'static [(&'static str, &'static str)],
}

/// A read by `POST` as the transport sends it: its endpoint (the path and the query), the headers the entry adds to
/// its service's (none for a reviewed entry), and its JSON body. Only this crate makes or reads one.
#[derive(Clone, Debug)]
pub struct PostReadWire {
    endpoint: Endpoint,
    headers: Vec<(String, String)>,
    body: Value,
}

impl PostReadWire {
    /// `shape`'s endpoint, its query in the captured order, with `names` as the body.
    fn lookup(shape: &Shape, names: &[Mri]) -> Self {
        let endpoint = shape
            .query
            .iter()
            .fold(Endpoint::new(shape.path.iter().copied()), |endpoint, (name, value)| {
                endpoint.with_query(*name, *value)
            });
        Self {
            endpoint,
            headers: Vec::new(),
            body: Value::Array(names.iter().map(|mri| Value::from(mri.as_str())).collect()),
        }
    }

    pub(crate) fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub(crate) fn headers(&self) -> &[(String, String)] {
        &self.headers
    }

    pub(crate) fn body(&self) -> &Value {
        &self.body
    }
}

impl Shape {
    pub(crate) fn service(&self) -> Service {
        self.service
    }

    /// The MRIs a request names, when the request is exactly this shape: `path` is the shape's, `query` its pairs
    /// (each once, nothing else, in any order), `headers` (by lower-cased name, the token left out) its headers (each
    /// once, exactly their values, all of them), and `body` a non-empty JSON list of strings and nothing else.
    /// `None` for anything else: the request is unknown.
    pub(crate) fn read_back(
        &self,
        path: &[String],
        query: &[(String, String)],
        headers: &[(String, String)],
        body: &[u8],
    ) -> Option<Vec<Mri>> {
        let same_path = path.iter().map(String::as_str).eq(self.path.iter().copied());
        (same_path && same_pairs(query, self.query) && same_pairs(headers, self.headers))
            .then(|| mri_list(body))
            .flatten()
    }
}

/// Whether `pairs` are `wanted`, each once and nothing else, in any order.
fn same_pairs(pairs: &[(String, String)], wanted: &[(&str, &str)]) -> bool {
    let mut pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let mut wanted = wanted.to_vec();
    pairs.sort_unstable();
    wanted.sort_unstable();
    pairs == wanted
}

/// A body that is a non-empty JSON list of strings and nothing else (one JSON value, nothing after it).
fn mri_list(body: &[u8]) -> Option<Vec<Mri>> {
    let names: Vec<String> = serde_json::from_slice(body).ok()?;
    (!names.is_empty()).then(|| names.into_iter().map(Mri::new).collect())
}

/// A read by `POST` together with the permit for it. The only way to make one is [`AuthorizedPostRead::new`], which
/// checks that the permit was given for this entry and exactly its MRIs, and consumes the permit: one permit covers
/// one request.
#[derive(Debug)]
pub struct AuthorizedPostRead<R> {
    request: R,
    permit: PostReadPermit,
}

impl<R: PostRead> AuthorizedPostRead<R> {
    /// Pairs `request` with `permit`.
    ///
    /// # Errors
    /// [`PostReadMismatch`] when the permit was given for another entry or for any other list of MRIs (other MRIs,
    /// more or fewer, another order). The permit is dropped with it.
    pub fn new(request: R, permit: PostReadPermit) -> Result<Self, PostReadMismatch> {
        if permit.kind() == R::KIND && permit.names() == request.names() {
            Ok(Self { request, permit })
        } else {
            Err(PostReadMismatch {
                permitted: (permit.kind(), permit.names().len()),
                requested: (R::KIND, request.names().len()),
            })
        }
    }

    #[must_use]
    pub fn request(&self) -> &R {
        &self.request
    }

    #[must_use]
    pub fn permit(&self) -> &PostReadPermit {
        &self.permit
    }

    pub(crate) fn shape(&self) -> &'static Shape {
        sealed::Sealed::shape(&self.request)
    }

    pub(crate) fn wire(&self) -> PostReadWire {
        sealed::Sealed::wire(&self.request)
    }
}

/// A permit used for a read by `POST` it wasn't given for. Shows both entries' kinds and how many MRIs each names,
/// never the MRIs.
#[derive(Debug, thiserror::Error)]
#[error(
    "the permit for {} naming {} MRI(s) doesn't cover {} naming {} MRI(s), or not the same ones",
    permitted.0, permitted.1, requested.0, requested.1
)]
pub struct PostReadMismatch {
    permitted: (&'static str, usize),
    requested: (&'static str, usize),
}

/// The query every captured profile lookup sent (ab-chat-full, ab-chat and load captures of 2026-10-20), in the
/// order it was sent; the browser fence's `LOOKUP_QUERY`. `isMailAddress=true` (a lookup by address) isn't on it.
const LOOKUP_QUERY: &[(&str, &str)] = &[
    ("isMailAddress", "false"),
    ("enableGuest", "true"),
    ("skypeTeamsInfo", "true"),
    ("canBeSmtpAddress", "false"),
    ("includeIBBarredUsers", "true"),
    ("includeDisabledAccounts", "true"),
];

/// The headers a profile lookup goes out with besides its token: the captured values of those that could change what
/// it does or name someone, each pinned to the one value every captured lookup carried (5869 lookups in the captures
/// of 2026-10-20, the browser fence's `LOOKUP_HEADER_VALUES`), and `Accept` (the answer's format, free in the browser
/// fence's review; the transport asks every service for JSON). `x-ms-object-id` is sent empty, as it always was: it
/// would name an account by its object id. Nothing that names the client's components or the session
/// (`x-ms-client-caller`, `x-ms-request-id`, `x-ms-session-id`) and no cookie.
const LOOKUP_HEADERS: &[(&str, &str)] = &[
    ("accept", "application/json"),
    ("content-type", "application/json;charset=UTF-8"),
    ("x-ms-client-request-type", "0"),
    ("x-ms-client-type", "cdlworker"),
    ("x-ms-migration", "True"),
    ("x-ms-object-id", ""),
    ("x-ms-test-user", "False"),
];

/// `users/fetchShortProfile` (see [`ShortProfileLookup`]).
static SHORT_PROFILE: Shape = Shape {
    service: Service::MiddleTier,
    path: &["beta", "users", "fetchShortProfile"],
    query: LOOKUP_QUERY,
    headers: LOOKUP_HEADERS,
};

/// `POST <middle tier>/beta/users/fetchShortProfile?isMailAddress=false&enableGuest=true&skypeTeamsInfo=true&
/// canBeSmtpAddress=false&includeIBBarredUsers=true&includeDisabledAccounts=true` with a JSON list of MRIs: the short
/// profiles (names, addresses, object ids) of exactly those accounts. Reviewed from the web client's lookups the
/// browser fence let through for lab accounts (ab-chat-full of 2026-10-20: each answer named only the accounts asked
/// for); the browser fence's `short_profile`. Its answer is read with [`ShortProfileLookup::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShortProfileLookup {
    names: Vec<Mri>,
}

impl ShortProfileLookup {
    /// The lookup of `names`, in that order; `None` when it names nobody. Whether it may go out is the fence's
    /// decision ([`teams_fence::Fence::check_post_read`] with [`PostRead::KIND`] and [`PostRead::names`]).
    #[must_use]
    pub fn new(names: impl IntoIterator<Item = Mri>) -> Option<Self> {
        let names: Vec<Mri> = names.into_iter().collect();
        (!names.is_empty()).then_some(Self { names })
    }

    /// The profiles the answer `body` gives, each of an MRI this lookup asked for.
    ///
    /// # Errors
    /// [`AnswerError::Malformed`] unless the body is `{"type": …, "value": [profile, …]}` with an `mri` in each
    /// profile; [`AnswerError::Unasked`] when any profile is of an MRI the lookup didn't ask for (the answer is
    /// dropped whole: an entry's answer names only the accounts asked for).
    pub fn read_answer(&self, body: &[u8]) -> Result<Vec<ShortProfile>, AnswerError> {
        let answer: Answer = serde_json::from_slice(body).map_err(|_| AnswerError::Malformed)?;
        let unasked = answer
            .value
            .iter()
            .filter(|profile| !self.names.contains(&profile.mri))
            .count();
        if unasked > 0 {
            return Err(AnswerError::Unasked { count: unasked });
        }
        Ok(answer.value.into_iter().map(ShortProfile::from).collect())
    }
}

impl PostRead for ShortProfileLookup {
    const KIND: &'static str = "short_profile";

    fn names(&self) -> &[Mri] {
        &self.names
    }
}

/// The middle tier's answer to a profile lookup, as captured: `{"type": "…IUserIdentity", "value": [profile, …]}`.
#[derive(Deserialize)]
struct Answer {
    value: Vec<RawProfile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawProfile {
    #[serde(deserialize_with = "mri")]
    mri: Mri,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    given_name: Option<String>,
    #[serde(default)]
    surname: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    user_principal_name: Option<String>,
    #[serde(default)]
    object_id: Option<String>,
    #[serde(default)]
    user_type: Option<String>,
}

fn mri<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Mri, D::Error> {
    String::deserialize(deserializer).map(Mri::new)
}

/// One account's short profile as the middle tier gave it. A field the answer left out is `None` (the captures show
/// `givenName` and `surname` left out for an account without them).
#[derive(Clone, PartialEq, Eq)]
pub struct ShortProfile {
    pub mri: Mri,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub email: Option<String>,
    pub user_principal_name: Option<String>,
    pub object_id: Option<String>,
    /// `Member`, or another directory user type as given.
    pub user_type: Option<String>,
}

impl From<RawProfile> for ShortProfile {
    fn from(raw: RawProfile) -> Self {
        Self {
            mri: raw.mri,
            display_name: raw.display_name,
            given_name: raw.given_name,
            surname: raw.surname,
            email: raw.email,
            user_principal_name: raw.user_principal_name,
            object_id: raw.object_id,
            user_type: raw.user_type,
        }
    }
}

/// `Debug` names the profile by its MRI only: the rest is someone's name and addresses.
impl fmt::Debug for ShortProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShortProfile")
            .field("mri", &self.mri)
            .finish_non_exhaustive()
    }
}

/// Why a profile lookup's answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    /// Not the captured answer's shape.
    #[error("not a profile lookup's answer")]
    Malformed,
    /// Profiles of MRIs the lookup didn't ask for.
    #[error("{count} profile(s) of MRIs the lookup didn't ask for")]
    Unasked { count: usize },
}

/// A profile lookup with something added that its entry doesn't name (a query pair, a header, another body): the
/// transport's tests need one to show it sends no request off its entry's captured shape. Tests only
/// (`test-support`), which a lab binary refuses to run with.
#[cfg(feature = "test-support")]
#[derive(Clone, Debug)]
pub struct ProbePostRead {
    lookup: ShortProfileLookup,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<Value>,
}

#[cfg(feature = "test-support")]
impl ProbePostRead {
    /// `lookup` as it is, until something is added.
    #[must_use]
    pub fn new(lookup: ShortProfileLookup) -> Self {
        Self {
            lookup,
            query: Vec::new(),
            headers: Vec::new(),
            body: None,
        }
    }

    /// Adds `name=value` to the query.
    #[must_use]
    pub fn with_query(mut self, name: &str, value: &str) -> Self {
        self.query.push((name.to_owned(), value.to_owned()));
        self
    }

    /// Adds a header (its name lower-cased, as the transport sends it).
    #[must_use]
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.to_owned()));
        self
    }

    /// Sends `body` instead of the list of MRIs.
    #[must_use]
    pub fn with_body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
}

#[cfg(feature = "test-support")]
impl PostRead for ProbePostRead {
    const KIND: &'static str = "test_probe";

    fn names(&self) -> &[Mri] {
        self.lookup.names()
    }
}
