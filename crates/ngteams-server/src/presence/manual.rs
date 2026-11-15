//! The manual status of each account, kept in the database: it outlasts restarts of the nodes and of Redis, which
//! hold the endpoints only.

use std::collections::HashMap;

use diesel::prelude::*;
use diesel_async::RunQueryDsl;

use crate::db::{Db, DbError, with_conn};

diesel::table! {
    presence_manual (user_id) {
        user_id -> Text,
        availability -> Text,
        expires_at -> BigInt,
    }
}

/// How long a manual status holds when the account doesn't reset it (seconds).
pub(super) const LIFETIME: i64 = 30 * 86_400;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// A manual status and when it expires.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Manual {
    pub availability: String,
    /// Unix milliseconds.
    pub until: i64,
}

impl Manual {
    fn of(availability: String, expires_at: i64) -> Self {
        Self {
            availability,
            until: expires_at.saturating_mul(1000),
        }
    }
}

/// Sets the manual status of the account `oid` for [`LIFETIME`], or clears it; the status it now has.
pub(super) async fn set(db: &Db, oid: &str, manual: Option<&str>) -> Result<Option<Manual>, DbError> {
    let Some(availability) = manual else {
        with_conn!(db, |conn| diesel::delete(presence_manual::table.find(oid))
            .execute(conn)
            .await?);
        return Ok(None);
    };
    let expires_at = now().saturating_add(LIFETIME);
    with_conn!(db, |conn| diesel::insert_into(presence_manual::table)
        .values((
            presence_manual::user_id.eq(oid),
            presence_manual::availability.eq(availability),
            presence_manual::expires_at.eq(expires_at),
        ))
        .on_conflict(presence_manual::user_id)
        .do_update()
        .set((
            presence_manual::availability.eq(availability),
            presence_manual::expires_at.eq(expires_at),
        ))
        .execute(conn)
        .await?);
    Ok(Some(Manual::of(availability.to_owned(), expires_at)))
}

/// The manual status of each of the accounts `oids` that has one, expired or not: the sweep tells the watchers of
/// one that expired.
pub(super) async fn of(db: &Db, oids: &[&str]) -> Result<HashMap<String, Manual>, DbError> {
    if oids.is_empty() {
        return Ok(HashMap::new());
    }
    let found: Vec<(String, String, i64)> = with_conn!(db, |conn| presence_manual::table
        .filter(presence_manual::user_id.eq_any(oids))
        .select((
            presence_manual::user_id,
            presence_manual::availability,
            presence_manual::expires_at,
        ))
        .load(conn)
        .await?);
    Ok(found
        .into_iter()
        .map(|(oid, availability, expires_at)| (oid, Manual::of(availability, expires_at)))
        .collect())
}

/// Forgets the manual status of the account `oid` when it expired.
pub(super) async fn forget_expired(db: &Db, oid: &str) -> Result<(), DbError> {
    let now = now();
    with_conn!(db, |conn| diesel::delete(
        presence_manual::table
            .find(oid)
            .filter(presence_manual::expires_at.le(now))
    )
    .execute(conn)
    .await?);
    Ok(())
}
