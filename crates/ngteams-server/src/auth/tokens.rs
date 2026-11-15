//! The tokens the server mints, in the shapes the client reads from Entra and authz (`docs/protocol/auth.md`):
//!
//! - an access token per audience: `aud` is the resource the client asked for (`<resource>/.default` mapped to
//!   `<resource>`), with `oid`, `tid`, `upn`, `name`, `scp` and `exp`, and `pwd_ver`, the account's password version;
//! - an ID token for the client (`aud` its client id) naming the account;
//! - the skypetoken: `skypeid` (`orgid:<oid>`), `rgn` and the `exp` of the access token it was exchanged for.
//!
//! An admin's access and ID tokens also carry `roles: ["admin"]` (as Entra names app roles), so the client can offer
//! its admin features; the claim says what the account was when the token was minted, and the admin API asks the
//! database on every request.
//!
//! All three are `EdDSA` JWTs from the [`KeyRing`], with this server's issuer; the skypetoken's audience is its own, so
//! no access token passes for a skypetoken, nor the other way round. Refresh tokens are opaque (`crate::db::refresh`).

use std::time::{Duration, SystemTime};

use jsonwebtoken::{Algorithm, Validation};
use serde::{Deserialize, Serialize};

use super::keys::{Invalid, KeyError, KeyRing};
use crate::db::users::User;
use crate::db::{now, seconds, unix};

/// The audience of a skypetoken, which no resource may be named.
pub const SKYPETOKEN_AUDIENCE: &str = "urn:ngteams-server:skypetoken";
/// Clock skew allowed when checking a token's times.
pub(crate) const LEEWAY: u64 = 60;
/// The role an admin's tokens name.
pub const ADMIN_ROLE: &str = "admin";
/// Scopes asked with every resource, which name no resource.
const SIGN_IN_SCOPES: [&str; 4] = ["openid", "profile", "offline_access", "email"];

/// An access token's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub oid: String,
    pub sub: String,
    pub tid: String,
    pub upn: String,
    pub preferred_username: String,
    pub name: String,
    pub scp: String,
    pub azp: String,
    pub ver: String,
    pub uti: String,
    /// `["admin"]` for an admin; absent otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    /// The account's password version when the token was issued (ours, not Entra's): the push channel refuses a
    /// token from before a password reset.
    #[serde(default)]
    pub pwd_ver: i64,
}

/// An ID token's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub oid: String,
    pub sub: String,
    pub tid: String,
    pub preferred_username: String,
    pub name: String,
    pub ver: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
}

/// A skypetoken's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkypeClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub exp: i64,
    /// `orgid:<oid>`: the account's MRI is `8:` and this.
    pub skypeid: String,
    pub rgn: String,
    pub tid: String,
    pub oid: String,
    pub jti: String,
}

impl SkypeClaims {
    /// The account's MRI.
    #[must_use]
    pub fn mri(&self) -> String {
        format!("8:{}", self.skypeid)
    }
}

/// What a signed link carries ([`Issuer::link`]).
#[derive(Serialize, Deserialize)]
struct LinkClaims {
    aud: String,
    iss: String,
    exp: i64,
    sub: String,
    oid: String,
    pwd_ver: i64,
}

/// What a verified link credential names ([`Issuer::verify_link`]).
#[derive(Debug)]
pub struct Link {
    pub subject: String,
    /// The account the link was handed to, and its password version then.
    pub oid: String,
    pub pwd_ver: i64,
}

/// A requested resource, as a token request's `scope` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    /// The token's `aud`.
    pub audience: String,
    /// The token's `scp`.
    pub scope: String,
}

/// The resource a `scope` asks for, when it is exactly one the server mints tokens for: `<resource>/.default` (a
/// resource ending in `/` is asked as `<resource>//.default`) or a named scope `<resource>/<name>`, beside any of
/// `openid profile offline_access email`.
#[must_use]
pub fn requested(scope: &str, allowed: &[String]) -> Option<Requested> {
    let mut resources = scope.split_whitespace().filter(|part| !SIGN_IN_SCOPES.contains(part));
    let resource = resources.next()?;
    if resources.next().is_some() {
        return None;
    }
    let (audience, name) = match resource.strip_suffix("/.default") {
        Some(audience) => (audience, "user_impersonation"),
        None => resource.rsplit_once('/').filter(|(_, name)| !name.is_empty())?,
    };
    allowed.iter().any(|allowed| allowed == audience).then(|| Requested {
        audience: audience.to_owned(),
        scope: name.to_owned(),
    })
}

/// Mints and checks the server's JWTs.
#[derive(Debug)]
pub struct Issuer {
    keys: KeyRing,
    iss: String,
    tenant: String,
    region: String,
    access_lifetime: Duration,
}

/// A minted token and when it runs out.
#[derive(Clone)]
pub struct Minted {
    pub token: String,
    pub expires_at: i64,
}

impl std::fmt::Debug for Minted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Minted")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Issuer {
    /// An issuer for `tenant` at `origin` (`iss` is `<origin>/<tenant>/v2.0`, as Entra's v2 issuers are).
    #[must_use]
    pub fn new(keys: KeyRing, origin: &str, tenant: &str, region: &str, access_lifetime: Duration) -> Self {
        Self {
            keys,
            iss: format!("{origin}/{tenant}/v2.0"),
            tenant: tenant.to_owned(),
            region: region.to_owned(),
            access_lifetime,
        }
    }

    #[must_use]
    pub fn keys(&self) -> &KeyRing {
        &self.keys
    }

    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// An access token for `user` and `requested`, to `client_id`.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn access(&self, user: &User, requested: &Requested, client_id: &str) -> Result<Minted, KeyError> {
        let iat = now();
        let exp = iat + seconds(self.access_lifetime);
        let claims = AccessClaims {
            aud: requested.audience.clone(),
            iss: self.iss.clone(),
            iat,
            nbf: iat,
            exp,
            oid: user.id.clone(),
            sub: user.id.clone(),
            tid: self.tenant.clone(),
            upn: user.upn.clone(),
            preferred_username: user.upn.clone(),
            name: user.display_name.clone(),
            scp: requested.scope.clone(),
            azp: client_id.to_owned(),
            ver: "2.0".to_owned(),
            uti: uuid::Uuid::new_v4().simple().to_string(),
            roles: roles(user),
            pwd_ver: user.password_version,
        };
        Ok(Minted {
            token: self.keys.sign(&claims)?,
            expires_at: exp,
        })
    }

    /// An ID token naming `user`, for `client_id`.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn id_token(&self, user: &User, client_id: &str) -> Result<String, KeyError> {
        let iat = now();
        self.keys.sign(&IdClaims {
            aud: client_id.to_owned(),
            iss: self.iss.clone(),
            iat,
            nbf: iat,
            exp: iat + seconds(self.access_lifetime),
            oid: user.id.clone(),
            sub: user.id.clone(),
            tid: self.tenant.clone(),
            preferred_username: user.upn.clone(),
            name: user.display_name.clone(),
            ver: "2.0".to_owned(),
            roles: roles(user),
        })
    }

    /// A skypetoken for the account an access token names, running out when that token does.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn skypetoken(&self, access: &AccessClaims) -> Result<Minted, KeyError> {
        let claims = SkypeClaims {
            aud: SKYPETOKEN_AUDIENCE.to_owned(),
            iss: self.iss.clone(),
            iat: now(),
            exp: access.exp,
            skypeid: format!("orgid:{}", access.oid),
            rgn: self.region.clone(),
            tid: self.tenant.clone(),
            oid: access.oid.clone(),
            jti: uuid::Uuid::new_v4().simple().to_string(),
        };
        Ok(Minted {
            token: self.keys.sign(&claims)?,
            expires_at: claims.exp,
        })
    }

    /// The claims of an access token for `audience`.
    ///
    /// # Errors
    /// [`Invalid`]: not ours, another audience, or expired.
    pub async fn verify_access(&self, token: &str, audience: &str) -> Result<AccessClaims, Invalid> {
        self.keys.verify(token, &self.validation(audience)).await
    }

    /// The claims of a skypetoken.
    ///
    /// # Errors
    /// [`Invalid`]: not ours, not a skypetoken, or expired.
    pub async fn verify_skypetoken(&self, token: &str) -> Result<SkypeClaims, Invalid> {
        self.keys.verify(token, &self.validation(SKYPETOKEN_AUDIENCE)).await
    }

    /// A short-lived credential for one stored thing (`subject`), carried in a link's query: `audience` says what
    /// the link is for, and it names the account it is handed to (`holder`) with its password version, as an
    /// access token does. Signed like every token, so any node checks a link another node handed out.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn link(&self, audience: &str, subject: &str, holder: &User, lifetime: Duration) -> Result<String, KeyError> {
        self.keys.sign(&LinkClaims {
            aud: audience.to_owned(),
            iss: self.iss.clone(),
            exp: now() + crate::db::seconds(lifetime),
            sub: subject.to_owned(),
            oid: holder.id.clone(),
            pwd_ver: holder.password_version,
        })
    }

    /// What a link credential for `audience` names. Whether its holder still is the account it was is the
    /// caller's check.
    ///
    /// # Errors
    /// [`Invalid`]: not ours, for something else, or expired.
    pub async fn verify_link(&self, token: &str, audience: &str) -> Result<Link, Invalid> {
        let mut validation = self.validation(audience);
        validation.leeway = 0;
        let claims: LinkClaims = self.keys.verify(token, &validation).await?;
        Ok(Link {
            subject: claims.sub,
            oid: claims.oid,
            pwd_ver: claims.pwd_ver,
        })
    }

    fn validation(&self, audience: &str) -> Validation {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.leeway = LEEWAY;
        validation.set_audience(&[audience]);
        validation.set_issuer(&[&self.iss]);
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        validation
    }
}

/// The roles `user`'s tokens name.
fn roles(user: &User) -> Vec<String> {
    if user.is_admin {
        vec![ADMIN_ROLE.to_owned()]
    } else {
        Vec::new()
    }
}

/// Seconds from now until `at` (none when it has passed).
#[must_use]
pub fn seconds_until(at: i64) -> u64 {
    u64::try_from(at - unix(SystemTime::now())).unwrap_or(0)
}
