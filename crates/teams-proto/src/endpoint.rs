//! What every class of request names: the [`Service`] it goes to and its [`Endpoint`] under that service's base; a
//! request that isn't a read also names its [`Method`]. Never a host or an absolute URL: the transport builds the URL
//! from the service's base, which it alone knows, and checks the result against that base.

/// A service the native transport reaches, and so the only hosts it sends to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Service {
    /// The chat service at the account's chat base, the region's GTM `chatServiceAfd`
    /// (`https://<AFD host>/api/chatsvc/<region>`, or a regional `https://<name>.msg.teams.microsoft.com`): the bases
    /// [`teams_fence::chat_service_path`] accepts a request under, so the transport and the fence's live membership
    /// read recognize the chat service by one check. Its endpoints start with `v1`.
    ChatService,
    /// The registrar, `https://<AFD host>/registrar/…`, on the chat base's AFD host (`teams.microsoft.com` when the
    /// chat base is a regional host). It registers an endpoint's Trouter URL ([`crate::EndpointRegistration`]).
    Registrar,
    /// Trouter's HTTP side, `https://go.trouter.teams.microsoft.com/`.
    Trouter,
    /// The middle tier at the account's middle-tier base, the region's GTM `middleTier`
    /// (`https://<AFD host>/api/mt/part/<partition>`, the partition a region and a number: `amer-03`), which a
    /// transport reaches only once it was given one (`HttpTransport::with_middle_tier`). Its endpoints are under
    /// `beta/users/`. It takes the account's token for `https://api.spaces.skype.com`. The profile lookups go here
    /// ([`crate::ShortProfileLookup`]).
    MiddleTier,
    /// Microsoft Graph, `https://graph.microsoft.com/v1.0/`, with the account's token for `https://graph.microsoft.com`:
    /// a user's name (`users/<object id>?$select=id,displayName`, [`crate::GraphUser`]) and their 96×96 picture
    /// (`users/<object id>/photos/96x96/$value`, [`crate::GraphPicture`]). Graph is a contacting service: a plain
    /// request to it that isn't a read is refused.
    Graph,
    /// The chat service aggregator (CSA) at the account's CSA base, the region's GTM `chatSvcAggAfd`
    /// (`https://<AFD host>/api/csa/<region>`), which a transport reaches only once it was given one
    /// (`HttpTransport::with_csa`), with the account's token for `https://chatsvcagg.teams.microsoft.com`: the account's
    /// teams and their channels (`api/v3/teams/users/me` with the web client's query, [`crate::CsaTeams`]).
    Csa,
    /// UPS, the presence service, at the account's presence base, the region's GTM `presenceUPS`
    /// (`https://<AFD host>/ups/<geo>`, the geo lowercase letters: `noam`), which a transport reaches only once it was
    /// given one (`HttpTransport::with_presence`), with the account's token for
    /// `https://presence.teams.microsoft.com/`: the presence read (`v1/presence/getpresence/`, [`crate::PresenceRead`])
    /// and the account's own availability, as an authorized write (`v1/me/endpoints/`,
    /// [`crate::SetEndpointAvailability`]; `v1/me/forceavailability/`, [`crate::ForceAvailability`]), every path sent
    /// with the trailing slash every captured UPS path had.
    Presence,
    /// The calling service: the flight proxy, `https://api.flightproxy.teams.microsoft.com/api/v2/`, with the account's
    /// skypetoken (`X-Skypetoken`, not a bearer token). The call writes go here (`crate::StartCall` and the others):
    /// the call start to `epconv`, every other write to a link the call's services handed out
    /// (`ep/<service host>/…`, a [`crate::CallLink`]), sent exactly as given and checked against the base. Only
    /// authorized call writes are sent here: a read or a plain request on it is refused.
    Calling,
    /// The TURN relay's credentials, `https://teams.microsoft.com/trap/`, with the account's `ic3` token and
    /// `x-ms-migration: True` and `api-version: 2`, as the web client's Entra form of the read
    /// (`docs/protocol/auth.md`): [`crate::RelayTokens`].
    Relay,
    /// Teams' reaction pictures on its public CDN,
    /// `https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/`, with no token
    /// (static files any browser reads): a reaction's animation ([`crate::ReactionAnimation`]).
    Emoticons,
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
