//! The pictures messages show, cached across runs: a message's picture on AMS, a GIF on Giphy, a file's thumbnail, as
//! the engine last read them (`teams-core`'s `pictures.rs` and `thumbnails.rs`), so a later run shows them without
//! reading them again.
//!
//! - **A file of its own** beside the account's store ([`PictureCache::file_beside`]: `<store>-cache/pictures.sqlite`,
//!   `0600` in a `0700` directory, WAL), never the store itself: its bytes are read and written off the engine's
//!   thread, and `ngteams snapshot` (which copies the stores, `store/*.sqlite`) leaves it out. Only the account's
//!   engine opens it, under the account's engine lock.
//! - **Keyed by a hash**: a picture is found by the SHA-256 of what names it (the core's key: an AMS object and view,
//!   a Giphy id and file, a file's sharing token), so no link, id or token is written as such.
//! - **Bounded**: once the pictures hold more bytes than the cache's limit, the least recently asked for go first.
//!   An entry older than the age its reader gives is dropped when asked for (a file's thumbnail changes with the file;
//!   a message's picture never does).
//! - **Checked**: a picture is kept with its length and SHA-256, and one whose bytes no longer match them is dropped
//!   when asked for, so its reader reads it again.
//! - **Disposable**: a file that can't be opened as the cache (not a database, a broken one) is removed and made
//!   anew, empty; one of a newer build's schema is left as it is and not used.
//! - Its schema is SQLite's `user_version`, migrated as the store's is ([`MIGRATIONS`]: a migration once released is
//!   never edited, a change is a new one appended).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension as _, TransactionBehavior, params};
use sha2::{Digest as _, Sha256};

use crate::{StoreError, file, now_ms, schema};

/// How many bytes of pictures an account's cache holds at most, by default.
pub const PICTURE_CACHE_BYTES: u64 = 512 * 1024 * 1024;

/// The cache's migrations, in order.
const MIGRATIONS: [&str; 1] = [include_str!("migrations/pictures/0001_initial.sql")];

/// Drops the least recently used pictures past the first `?1` bytes, newest first.
const EVICT: &str = "DELETE FROM pictures WHERE id IN (
    SELECT id FROM (
        SELECT id, SUM(size) OVER (ORDER BY used_at DESC, id DESC) AS kept FROM pictures
    ) WHERE kept > ?1
)";

/// An account's picture cache (see the module docs).
#[derive(Debug)]
pub struct PictureCache {
    conn: Connection,
    /// The bytes it holds at most.
    limit: u64,
}

impl PictureCache {
    /// Where the cache of the store at `store` lives: `<store>-cache/pictures.sqlite`.
    #[must_use]
    pub fn file_beside(store: &Path) -> PathBuf {
        let mut dir = store.as_os_str().to_owned();
        dir.push("-cache");
        PathBuf::from(dir).join("pictures.sqlite")
    }

    /// Opens the cache at `path` holding at most `limit` bytes of pictures, creating it (and its directory, `0700`)
    /// when missing, the file `0600`. A file that can't be opened as the cache is removed and made anew, once.
    ///
    /// # Errors
    ///
    /// [`StoreError::SchemaTooNew`] for a cache a newer build made (left as it is); [`StoreError::File`] when the file
    /// or its directory can't be made private or a broken file can't be removed; [`StoreError::Sqlite`] or
    /// [`StoreError::NotWal`] when a new file can't be used either.
    pub fn open(path: &Path, limit: u64) -> Result<Self, StoreError> {
        match Self::connect(path, limit) {
            Err(error @ StoreError::SchemaTooNew { .. }) => Err(error),
            Err(error) => {
                log::info!("the picture cache couldn't be opened ({error}): it is made anew, empty");
                remove(path)?;
                Self::connect(path, limit)
            }
            opened => opened,
        }
    }

    fn connect(path: &Path, limit: u64) -> Result<Self, StoreError> {
        file::prepare(path)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(crate::BUSY_TIMEOUT)?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        let mode = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::NotWal(mode));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        schema::apply(&mut conn, &MIGRATIONS, "picture cache")?;
        Ok(Self { conn, limit })
    }

    /// The bytes of the picture `key` names, if the cache holds them and they were stored less than `max_age` ago (any
    /// age when `None`); older ones, and ones that no longer match the length and SHA-256 they were stored with, are
    /// dropped. A picture found counts as used now.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the cache can't be read or written.
    pub fn get(&self, key: &str, max_age: Option<Duration>) -> Result<Option<Vec<u8>>, StoreError> {
        let found = self
            .conn
            .query_row(
                "SELECT id, size, digest, stored_at FROM pictures WHERE key = ?1",
                [hash(key.as_bytes())],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, size, digest, stored_at)) = found else {
            return Ok(None);
        };
        let now = now_ms();
        let expired = max_age.is_some_and(|age| {
            let age = i64::try_from(age.as_millis()).unwrap_or(i64::MAX);
            stored_at.saturating_add(age) <= now
        });
        let bytes = if expired {
            None
        } else {
            let bytes: Vec<u8> = self
                .conn
                .query_row("SELECT bytes FROM pictures WHERE id = ?1", [id], |row| row.get(0))?;
            let intact = i64::try_from(bytes.len()).is_ok_and(|len| len == size) && hash(&bytes) == digest;
            intact.then_some(bytes)
        };
        let Some(bytes) = bytes else {
            self.conn.execute("DELETE FROM pictures WHERE id = ?1", [id])?;
            return Ok(None);
        };
        self.conn
            .execute("UPDATE pictures SET used_at = ?2 WHERE id = ?1", params![id, now])?;
        Ok(Some(bytes))
    }

    /// Keeps `bytes` as the picture `key` names (in place of one kept before), stored and used now, then, when the
    /// pictures hold more than the cache's limit, lets the least recently used go until they don't (this one too, when
    /// it alone is larger).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the cache can't be written.
    pub fn put(&mut self, key: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let now = now_ms();
        let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let limit = i64::try_from(self.limit).unwrap_or(i64::MAX);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO pictures (key, size, digest, stored_at, used_at, bytes)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)
             ON CONFLICT (key) DO UPDATE SET size = excluded.size, digest = excluded.digest,
                 stored_at = excluded.stored_at, used_at = excluded.used_at, bytes = excluded.bytes",
            params![hash(key.as_bytes()), size, hash(bytes), now, bytes],
        )?;
        if total(&tx)? > limit {
            tx.execute(EVICT, [limit])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// How many bytes of pictures the cache holds.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the cache can't be read.
    pub fn bytes(&self) -> Result<u64, StoreError> {
        Ok(u64::try_from(total(&self.conn)?).unwrap_or(0))
    }
}

/// The pictures' bytes in all, from the index by use (no row's bytes read).
fn total(conn: &Connection) -> Result<i64, StoreError> {
    Ok(conn.query_row("SELECT COALESCE(SUM(size), 0) FROM pictures", [], |row| row.get(0))?)
}

/// A SHA-256: what the cache finds a picture by (of its key), and what its bytes are checked against.
fn hash(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

/// Removes the cache's file and SQLite's files beside it, the ones there are.
fn remove(path: &Path) -> Result<(), StoreError> {
    let sides = file::SIDE_FILES.map(|suffix| file::side_file(path, suffix));
    for gone in std::iter::once(path.to_path_buf()).chain(sides) {
        match std::fs::remove_file(&gone) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(StoreError::File {
                    path: gone,
                    source: error,
                });
            }
            _ => {}
        }
    }
    Ok(())
}
