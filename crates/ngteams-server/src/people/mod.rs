//! People: what the directory knows of each account beside its sign-in (`repo`), and the reads the client names and
//! draws people from: the middle tier's short profiles, Graph's users and pictures (`crate::routes::graph`) and
//! Loki's person card.

mod edit;
mod loki;
mod picture;
pub mod repo;
mod short;

use std::marker::PhantomData;
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::post;

pub use edit::{ProfileChanges, ProfileError, account, update_profile};
pub(crate) use picture::picture;
pub use picture::{MAX_SOURCE, PictureError, remove_picture, set_picture};
pub use repo::{Person, Profile, job_titles, out_of_office};

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::db::users::{self, User};

/// The account a request of audience `A` comes from: one that can sign in, with a token from after its last
/// password reset.
pub(crate) struct Caller<A> {
    pub id: String,
    /// The server's tenant, as the token names it.
    pub tenant: String,
    pub user: User,
    audience: PhantomData<A>,
}

impl<A: Audience> FromRequestParts<Arc<App>> for Caller<A> {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Self::Rejection> {
        let bearer = Bearer::<A>::from_request_parts(parts, app)
            .await
            .map_err(IntoResponse::into_response)?;
        let user = match users::by_id(&app.db, &bearer.claims.oid).await {
            Ok(Some(user)) if !user.disabled => user,
            Ok(_) => return Err(StatusCode::FORBIDDEN.into_response()),
            Err(error) => {
                tracing::error!(%error, "the caller's account couldn't be read");
                return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
            }
        };
        if user.password_version != bearer.claims.pwd_ver {
            return Err(StatusCode::UNAUTHORIZED.into_response());
        }
        Ok(Self {
            id: bearer.claims.oid,
            tenant: bearer.claims.tid,
            user,
            audience: PhantomData,
        })
    }
}

/// The largest body of a read by POST.
const BODY_LIMIT: usize = 64 * 1024;

/// The people reads: the middle tier's short profiles, Graph's users and pictures, Loki's person card.
pub(crate) fn router(app: &App) -> Router<Arc<App>> {
    let short_profiles = format!(
        "{}/beta/users/fetchShortProfile",
        crate::services::middle_tier_prefix(&app.config)
    );
    Router::new()
        .route(&short_profiles, post(short::short_profiles))
        .merge(crate::routes::graph::router())
        .merge(loki::router())
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}

/// The 503 a request gets when the database or the file store doesn't answer.
pub(crate) fn unavailable(error: &dyn std::fmt::Display) -> StatusCode {
    tracing::error!(%error, "people request failed");
    StatusCode::SERVICE_UNAVAILABLE
}
