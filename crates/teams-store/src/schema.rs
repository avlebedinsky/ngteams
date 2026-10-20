//! Migrations, applied in order on open. The schema version is SQLite's `user_version`; migration `n` (1-based) takes
//! the store from version `n - 1` to `n` in one transaction, so a failed one leaves the store where it was. A
//! migration once released is never edited: a change is a new file appended to [`MIGRATIONS`].

use rusqlite::Connection;

use crate::StoreError;

/// The schema version this build writes: the number of migrations.
pub const SCHEMA_VERSION: u32 = 1;

/// The migrations, in order.
const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [include_str!("migrations/0001_initial.sql")];

pub(crate) fn version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Applies every migration past the store's version; refuses a store from a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let found = version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    for (to, sql) in (1..=SCHEMA_VERSION).zip(MIGRATIONS).skip(found as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", to)?;
        tx.commit()?;
    }
    Ok(())
}
