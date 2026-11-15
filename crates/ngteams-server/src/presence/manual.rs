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

/// Sets the manual status of the account `oid` for [`LIFETIME`], or clears it.
pub(super) async fn set(db: &Db, oid: &str, manual: Option<&str>) -> Result<(), DbError> {
    let Some(availability) = manual else {
        with_conn!(db, |conn| diesel::delete(presence_manual::table.find(oid))
            .execute(conn)
            .await?);
        return Ok(());
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
    Ok(())
}

/// The manual status of each of the accounts `oids` that has one that hasn't expired.
pub(super) async fn of(db: &Db, oids: &[&str]) -> Result<HashMap<String, String>, DbError> {
    if oids.is_empty() {
        return Ok(HashMap::new());
    }
    let now = now();
    let found: Vec<(String, String)> = with_conn!(db, |conn| presence_manual::table
        .filter(presence_manual::user_id.eq_any(oids))
        .filter(presence_manual::expires_at.gt(now))
        .select((presence_manual::user_id, presence_manual::availability))
        .load(conn)
        .await?);
    Ok(found.into_iter().collect())
}
