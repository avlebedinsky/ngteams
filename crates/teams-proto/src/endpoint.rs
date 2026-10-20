//! What every class of request names: the [`Service`] it goes to and its [`Endpoint`] under that service's base; a
//! request that isn't a read also names its [`Method`]. Never a host or an absolute URL: the transport builds the URL
//! from the service's base, which it alone knows, and checks the result against its host allowlist.

/// A service the native transport reaches, and so the only hosts it sends to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Service {
    /// The chat service at the account's chat base, the region's GTM `chatServiceAfd`
    /// (`https://<AFD host>/api/chatsvc/<region>`, or a regional `https://<name>.msg.teams.microsoft.com`): the bases
    /// [`teams_fence::chat_service_path`] accepts a request under, so the transport and the fence's live membership
    /// read recognize the chat service by one check. Its endpoints start with `v1`.
    ChatService,
    /// The registrar, `https://<AFD host>/registrar/…`, on the chat base's AFD host (`teams.microsoft.com` when the
    /// chat base is a regional host). It registers an endpoint's Trouter URL (an own-state request, added by the task
    /// that reviews it).
    Registrar,
    /// Trouter's HTTP side, `https://go.trouter.teams.microsoft.com/`.
    Trouter,
}

/// A request's path under its service's base, as path segments (each sent as one encoded segment, so a `/` inside one
/// stays inside it), and its query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    segments: Vec<String>,
    query: Vec<(String, String)>,
}

impl Endpoint {
    /// The path `segments`, with no query. The transport refuses an endpoint with an empty, `.` or `..` segment.
    #[must_use]
    pub fn new<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(Into::into).collect(),
            query: Vec::new(),
        }
    }

    /// Adds `name=value` to the query, after any added before.
    #[must_use]
    pub fn with_query(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    #[must_use]
    pub fn query(&self) -> &[(String, String)] {
        &self.query
    }
}

/// The method of a request that isn't a read: a read is always a `GET` and names no method.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    pub(crate) fn to_reqwest(self) -> reqwest::Method {
        match self {
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Patch => reqwest::Method::PATCH,
            Self::Delete => reqwest::Method::DELETE,
        }
    }
}
