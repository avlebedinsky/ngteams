//! The native client's local store, one SQLite file per account (`<data_dir>/store/<alias>.sqlite`, mode `0600`, in
//! WAL mode), outside the repo.
//!
//! - [`conversations`]: conversations by thread version, their members by roster version, the list by last activity;
//! - [`messages`]: messages by id and version (an edit or a soft delete is a newer version), reactions per user, pages
//!   of a conversation, the unread count;
//! - [`state`]: read horizons and where each conversation's sync stands;
//! - [`outbox`]: writes waiting to go out, keyed by `clientmessageid`; a run sends only its own rows;
//! - [`schema`]: the migrations, applied in order on open.
//!
//! Every versioned write is an upsert that never lets an older version overwrite a newer one, so a Trouter delivery
//! and a sync that bring the same change in either order leave the same rows ([`Applied`] says what happened). The
//! store keeps rows; the chat service's JSON is parsed by `teams-proto` and the decisions are `teams-core`'s.
//!
//! M1 Task 11 (`docs/plans/20261020-native-teams-m1-headless-chat-core.md`).

pub mod conversations;
mod file;
pub mod messages;
pub mod outbox;
pub mod schema;
pub mod state;

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

pub use conversations::{Conversation, ConversationKind, Member};
pub use messages::{Message, MessagePage, PageCursor, Reaction, ReactionChange};
pub use outbox::{AttemptOutcome, NewOutboxEntry, OutboxEntry, OutboxKind, OutboxState};
pub use schema::SCHEMA_VERSION;
pub use state::{ReadHorizon, SyncState};

/// How long a write waits for another connection's lock (a second process on the same account) before it fails.
/// Every transaction takes the write lock when it begins (`BEGIN IMMEDIATE`), so one that reads before it writes
/// waits here too, rather than failing at once when another connection wrote in between.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What a store operation can fail with.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store file {}: {source}", path.display())]
    File { path: PathBuf, source: std::io::Error },
    #[error("the store's journal mode is {0}, not wal")]
    NotWal(String),
    #[error("the store's schema version {found} is newer than this build's {supported}")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("no conversation {0} in the store")]
    UnknownConversation(String),
    #[error("no message {message} in conversation {conversation}")]
    UnknownMessage { conversation: String, message: String },
    #[error("an outbox row with clientmessageid {0} exists already")]
    DuplicateClientMessageId(String),
    #[error("no outbox row {0}")]
    UnknownOutboxEntry(i64),
    #[error("outbox row {id} is {} and not this run's to send", state.as_str())]
    OutboxNotSendable { id: i64, state: OutboxState },
}

/// What a versioned write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// There was no row: it was written.
    Inserted,
    /// The stored row was older: it was replaced.
    Updated,
    /// The stored row is as new or newer: nothing changed.
    Ignored,
}

/// One account's store. Writes take `&mut self`; the core owns it on one thread.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    run_id: i64,
}

impl Store {
    /// Opens the store at `path`, creating it (and its directory, mode `0700`) when missing, with the file `0600`
    /// (a looser existing file is tightened); turns on WAL and foreign keys, applies the pending migrations, and starts
    /// a new run (see [`Store::run_id`]).
    ///
    /// # Errors
    ///
    /// [`StoreError::File`] when the file or its directory can't be made private, [`StoreError::NotWal`] when SQLite
    /// keeps another journal mode, [`StoreError::SchemaTooNew`] for a store written by a newer build,
    /// [`StoreError::Sqlite`] when SQLite refuses it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        file::prepare(path)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.set_transaction_behavior(TransactionBehavior::Immediate);
        // SQLite answers with the mode it took: the old one when it can't switch
        let mode = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::NotWal(mode));
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        schema::migrate(&mut conn)?;
        conn.execute("INSERT INTO runs (started_at) VALUES (?1)", [now_ms()])?;
        let run_id = conn.last_insert_rowid();
        Ok(Self { conn, run_id })
    }

    /// This process's run: the outbox rows it made or adopted. Rows of earlier runs are never sent on their own.
    #[must_use]
    pub fn run_id(&self) -> i64 {
        self.run_id
    }

    /// The schema version the store is at.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the version can't be read.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        schema::version(&self.conn)
    }

    fn require_conversation(conn: &Connection, id: &str) -> Result<(), StoreError> {
        let known: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM conversations WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )?;
        if known {
            Ok(())
        } else {
            Err(StoreError::UnknownConversation(id.to_owned()))
        }
    }
}

/// Milliseconds since the Unix epoch, for the store's own bookkeeping (run starts, outbox row times).
fn now_ms() -> i64 {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
}

/// A `LIMIT` from a count: anything past `i64::MAX` is no limit at all.
fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}
