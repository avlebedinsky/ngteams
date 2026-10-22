//! Migrations, applied in order on open. The schema version is SQLite's `user_version`; migration `n` (1-based) takes
//! the store from version `n - 1` to `n` in one transaction, so a failed one leaves the store where it was. Each
//! transaction reads the version under the write lock, so two processes opening a new store at once apply each
//! migration once. A
//! migration once released is never edited: a change is a new file appended to [`MIGRATIONS`].

use rusqlite::{Connection, TransactionBehavior};

use crate::StoreError;

/// The schema version this build writes: the number of migrations.
pub const SCHEMA_VERSION: u32 = 9;

/// The migrations, in order.
const MIGRATIONS: [&str; SCHEMA_VERSION as usize] = [
    include_str!("migrations/0001_initial.sql"),
    include_str!("migrations/0002_outbox_budget.sql"),
    include_str!("migrations/0003_sync_chains.sql"),
    include_str!("migrations/0004_outbox_unreadable_seen.sql"),
    include_str!("migrations/0005_outbox_by_conversation.sql"),
    include_str!("migrations/0006_messages_by_sender.sql"),
    include_str!("migrations/0007_profiles.sql"),
    include_str!("migrations/0008_search.sql"),
    include_str!("migrations/0009_teams.sql"),
];

pub(crate) fn version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Applies every migration past the store's version; refuses a store from a newer build.
pub(crate) fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    loop {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found = version(&tx)?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        let Some(sql) = MIGRATIONS.get(found as usize) else {
            return Ok(());
        };
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", found + 1)?;
        tx.commit()?;
    }
}
