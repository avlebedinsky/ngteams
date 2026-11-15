//! People: what the directory knows of each account beside its sign-in (`repo`), and the reads the client names and
//! draws people from: the middle tier's short profiles, Graph's users and pictures (`crate::routes::graph`) and
//! Loki's person card.

pub mod repo;

use std::marker::PhantomData;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};

pub use repo::{Person, Profile, out_of_office};

use crate::app::App;
use crate::auth::extract::{Audience, Bearer};
use crate::db::users::{self, User};

/// The account a request of audience `A` comes from: one that can sign in, with a token from after its last
/// password reset.
pub(crate) struct Caller<A> {
    pub id: String,
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
            user,
            audience: PhantomData,
        })
    }
}
