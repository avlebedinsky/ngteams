//! The database: Postgres (the source of truth several nodes share) or SQLite (one node), through Diesel.
//!
//! The foundation schema ([`schema`]) and the chat service's own schema each serve both backends.
//! Each query is written once inside [`with_conn!`],
//! which runs it on whichever connection the pool holds. Postgres goes through diesel-async's tokio-postgres
//! connection; SQLite through Diesel's own connection on the blocking pool, one connection per node (its writes
//! serialize anyway). Foundation times are Unix seconds; chat arrival times and versions are Unix milliseconds.
//!
//! Other processes may open the same SQLite file at once (the operator's commands beside a running or starting
//! server): every SQLite connection waits on a lock another holds ([`SQLITE_BUSY_TIMEOUT_MS`]) before it does
//! anything else, and pending migrations run as one write transaction a second migrating process waits for. A wait
//! that runs out is [`DbError::Locked`], which names the lock and the time it waited.
//!
//! SQLite's own `lower` folds ASCII only, so each SQLite connection replaces it with Rust's Unicode one: a
//! case-insensitive match written with [`lower`] then finds "Élodie" on both backends.

pub mod keys;
pub mod meta;
pub mod refresh;
pub mod schema;
pub mod users;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diesel::{ConnectionError, SqliteConnection};
use diesel_async::pooled_connection::bb8::{Pool as Bb8, RunError};
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig, PoolError};
use diesel_async::sync_connection_wrapper::SyncConnectionWrapper;
use diesel_async::{AsyncConnection, AsyncMigrationHarness, AsyncPgConnection, SimpleAsyncConnection};
use diesel_migrations::{EmbeddedMigrations, MigrationHarness, embed_migrations};
use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;

pub type Sqlite = SyncConnectionWrapper<SqliteConnection>;

const POSTGRES_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/postgres");
const SQLITE_MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations/sqlite");

/// The Postgres advisory lock key migrations hold ("ngts").
const MIGRATION_LOCK: i64 = 0x6e67_7473;
/// How long a request waits for a pooled connection.
const CHECKOUT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long SQLite waits on a lock another process holds (the admin CLI beside a running server).
const SQLITE_BUSY_TIMEOUT_MS: u32 = 5000;
/// How long a process waits for another one's SQLite migrations, as a Postgres node waits for the advisory lock.
const SQLITE_MIGRATION_WAIT_MS: u32 = 120_000;
/// SQLite's message for `SQLITE_BUSY`: a lock another connection holds, after the busy timeout.
const SQLITE_BUSY_MESSAGE: &str = "database is locked";

/// Why a database call failed. Never carries a value from a row.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("can't connect to the database: {0}")]
    Connect(#[from] ConnectionError),
    #[error("no database connection: {0}")]
    Pool(String),
    #[error("database query failed: {0}")]
    Query(diesel::result::Error),
    /// Its source is the [`Locked`], so a caller finds it in the chain of an error that wraps this one.
    #[error("{0}")]
    Locked(#[from] Locked),
    #[error("database migration failed: {0}")]
    Migration(String),
    #[error("a unique value is already taken")]
    Conflict,
}

/// Another process held the SQLite write lock for all of the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("another process holds the SQLite write lock: waited {} s for it", .waited.as_secs())]
pub struct Locked {
    pub waited: Duration,
}

impl Locked {
    /// After the wait of each statement.
    pub(crate) fn statement() -> Self {
        Self::after(SQLITE_BUSY_TIMEOUT_MS)
    }

    /// After the wait for the migrations.
    fn migration() -> Self {
        Self::after(SQLITE_MIGRATION_WAIT_MS)
    }

    fn after(milliseconds: u32) -> Self {
        Self {
            waited: Duration::from_millis(u64::from(milliseconds)),
        }
    }
}

/// Whether `error` is SQLite's `SQLITE_BUSY`: the busy timeout ran out.
pub(crate) fn busy(error: &diesel::result::Error) -> bool {
    matches!(error, diesel::result::Error::DatabaseError(_, info) if info.message() == SQLITE_BUSY_MESSAGE)
}

impl From<diesel::result::Error> for DbError {
    fn from(error: diesel::result::Error) -> Self {
        if busy(&error) {
            Self::Locked(Locked::statement())
        } else {
            Self::Query(error)
        }
    }
}

/// A new SQLite connection whose setup met a lock past the wait.
fn pool_locked(error: &PoolError) -> bool {
    match error {
        PoolError::ConnectionError(ConnectionError::CouldntSetupConfiguration(error))
        | PoolError::QueryError(error) => busy(error),
        PoolError::ConnectionError(_) => false,
    }
}

impl From<RunError> for DbError {
    fn from(error: RunError) -> Self {
        match error {
            RunError::User(error) => error.into(),
            RunError::TimedOut => Self::Pool(RunError::TimedOut.to_string()),
        }
    }
}

impl From<PoolError> for DbError {
    fn from(error: PoolError) -> Self {
        if pool_locked(&error) {
            Self::Locked(Locked::statement())
        } else {
            Self::Pool(error.to_string())
        }
    }
}

/// The pool, of one backend or the other.
#[derive(Clone)]
pub enum Pool {
    Postgres(Bb8<AsyncPgConnection>),
    Sqlite(Bb8<Sqlite>),
}

/// The server's database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    pool: Pool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let backend = match self.pool {
            Pool::Postgres(_) => "postgres",
            Pool::Sqlite(_) => "sqlite",
        };
        f.debug_struct("Db").field("backend", &backend).finish()
    }
}

/// Runs `$body` with `$conn` bound to a `&mut` connection of the pool's backend: the body is compiled once per
/// backend, so it must be a query both accept.
#[macro_export]
#[doc(hidden)]
macro_rules! with_conn {
    ($db:expr, |$conn:ident| $body:expr) => {
        match $db.pool() {
            $crate::db::Pool::Postgres(pool) => {
                let mut pooled = pool.get().await?;
                let $conn: &mut ::diesel_async::AsyncPgConnection = &mut pooled;
                $body
            }
            $crate::db::Pool::Sqlite(pool) => {
                let mut pooled = pool.get().await?;
                let $conn: &mut $crate::db::Sqlite = &mut pooled;
                $body
            }
        }
    };
}
pub use with_conn;

impl Db {
    /// Connects to `url` (`postgres://…` or `sqlite://<path>`) with at most `max` connections (SQLite: one), and
    /// brings the schema up to date.
    ///
    /// # Errors
    /// The database can't be reached, or a migration fails.
    pub async fn connect(url: &str, max: u32) -> Result<Self, DbError> {
        let pool = if let Some(path) = url.strip_prefix("sqlite://") {
            let mut config = ManagerConfig::default();
            config.custom_setup = Box::new(sqlite_setup);
            let manager = AsyncDieselConnectionManager::<Sqlite>::new_with_config(path, config);
            Pool::Sqlite(
                Bb8::builder()
                    .max_size(1)
                    .connection_timeout(CHECKOUT_TIMEOUT)
                    .build(manager)
                    .await?,
            )
        } else {
            let manager = AsyncDieselConnectionManager::<AsyncPgConnection>::new(url);
            Pool::Postgres(
                Bb8::builder()
                    .max_size(max.max(1))
                    .connection_timeout(CHECKOUT_TIMEOUT)
                    .build(manager)
                    .await?,
            )
        };
        let db = Self { pool };
        db.migrate().await?;
        Ok(db)
    }

    #[must_use]
    pub fn pool(&self) -> &Pool {
        &self.pool
    }

    /// Applies the migrations not yet applied. On Postgres a session advisory lock makes nodes starting at once take
    /// turns; the lock goes with the dedicated connection. On SQLite a read first finds whether any are pending, so
    /// a migrated file takes no write lock here. Pending ones run in one `BEGIN IMMEDIATE` transaction, which waits
    /// up to [`SQLITE_MIGRATION_WAIT_MS`]: a second process that migrates at once waits for it, then finds them
    /// applied.
    ///
    /// # Errors
    /// A migration fails.
    pub async fn migrate(&self) -> Result<(), DbError> {
        match &self.pool {
            Pool::Postgres(pool) => {
                let mut connection = pool.dedicated_connection().await?;
                connection
                    .batch_execute(&format!("SELECT pg_advisory_lock({MIGRATION_LOCK})"))
                    .await?;
                run_migrations(connection, POSTGRES_MIGRATIONS).await
            }
            Pool::Sqlite(pool) => {
                let mut connection = pool.dedicated_connection().await?;
                connection.spawn_blocking(|sqlite| Ok(migrate_sqlite(sqlite))).await?
            }
        }
    }

    /// Whether the database answers (readiness).
    ///
    /// # Errors
    /// It doesn't.
    pub async fn ping(&self) -> Result<(), DbError> {
        with_conn!(self, |conn| conn.batch_execute("SELECT 1").await?);
        Ok(())
    }
}

diesel::define_sql_function! {
    /// SQL `lower`, Unicode-aware on both backends (Postgres: as the database's locale folds).
    fn lower(text: diesel::sql_types::Text) -> diesel::sql_types::Text;
}

/// A SQLite connection with a busy timeout, WAL, foreign keys on and the Unicode `lower`. The timeout comes first:
/// switching to WAL takes a lock another process may hold.
fn sqlite_setup(path: &str) -> BoxFuture<'_, diesel::ConnectionResult<Sqlite>> {
    async move {
        let mut connection = Sqlite::establish(path).await?;
        connection
            .batch_execute(&format!("PRAGMA busy_timeout = {SQLITE_BUSY_TIMEOUT_MS}"))
            .await
            .map_err(ConnectionError::CouldntSetupConfiguration)?;
        wal(&mut connection)
            .await
            .map_err(ConnectionError::CouldntSetupConfiguration)?;
        connection
            .batch_execute("PRAGMA foreign_keys = ON")
            .await
            .map_err(ConnectionError::CouldntSetupConfiguration)?;
        connection
            .spawn_blocking(|sqlite| lower_utils::register_impl(sqlite, |text: String| text.to_lowercase()))
            .await
            .map_err(ConnectionError::CouldntSetupConfiguration)?;
        Ok(connection)
    }
    .boxed()
}

/// Puts the file in WAL mode, which it keeps; once it is in WAL this changes nothing. The switch reads the file and
/// then asks for its write lock, so while another process switches a new file at the same moment SQLite fails it at
/// once (`SQLITE_BUSY`, as waiting could deadlock) whatever the busy timeout. It then waits for that write lock
/// before it holds any other (`BEGIN IMMEDIATE`, which the busy timeout covers), by when the other's switch is done.
async fn wal(connection: &mut Sqlite) -> diesel::QueryResult<()> {
    if connection.batch_execute("PRAGMA journal_mode = WAL").await.is_ok() {
        return Ok(());
    }
    connection
        .batch_execute("BEGIN IMMEDIATE; ROLLBACK; PRAGMA journal_mode = WAL")
        .await
}

/// The SQLite migrations, if any are pending: a read first, then the write lock (with the longer wait), inside which
/// the harness reads the applied ones again. A wait past [`SQLITE_MIGRATION_WAIT_MS`] is [`DbError::Locked`].
fn migrate_sqlite(sqlite: &mut SqliteConnection) -> Result<(), DbError> {
    use diesel::connection::SimpleConnection as _;

    if !sqlite_pending(sqlite) {
        return Ok(());
    }
    sqlite.batch_execute(&format!("PRAGMA busy_timeout = {SQLITE_MIGRATION_WAIT_MS}"))?;
    sqlite
        .immediate_transaction(|sqlite| sqlite.run_pending_migrations(SQLITE_MIGRATIONS).map(|_| ()))
        .map_err(|error: Box<dyn std::error::Error + Send + Sync>| {
            match error.downcast_ref::<diesel::result::Error>() {
                Some(error) if busy(error) => DbError::Locked(Locked::migration()),
                _ => DbError::Migration(error.to_string()),
            }
        })
}

/// Whether a SQLite migration is not yet applied, by a read that takes no write lock. If the read fails (as on a new
/// file with no migrations table), it says yes: the harness then finds out under the lock.
fn sqlite_pending(sqlite: &mut SqliteConnection) -> bool {
    use diesel::RunQueryDsl as _;
    use diesel::migration::MigrationSource;

    #[derive(diesel::QueryableByName)]
    struct Applied {
        #[diesel(sql_type = diesel::sql_types::Text)]
        version: String,
    }

    let Ok(applied) = diesel::sql_query("SELECT version FROM __diesel_schema_migrations").load::<Applied>(sqlite)
    else {
        return true;
    };
    let Ok(migrations) = MigrationSource::<diesel::sqlite::Sqlite>::migrations(&SQLITE_MIGRATIONS) else {
        return true;
    };
    migrations.iter().any(|migration| {
        let version = migration.name().version().to_string();
        !applied.iter().any(|applied| applied.version == version)
    })
}

/// Runs the pending migrations on the blocking pool (the harness blocks on the connection's futures).
async fn run_migrations<C>(connection: C, migrations: EmbeddedMigrations) -> Result<(), DbError>
where
    C: AsyncConnection + 'static,
    AsyncMigrationHarness<C>: MigrationHarness<C::Backend>,
{
    tokio::task::spawn_blocking(move || {
        let mut harness = AsyncMigrationHarness::new(connection);
        harness
            .run_pending_migrations(migrations)
            .map(|_| ())
            .map_err(|error| DbError::Migration(error.to_string()))
    })
    .await
    .map_err(|error| DbError::Migration(error.to_string()))?
}

/// Now, in Unix seconds.
#[must_use]
pub fn now() -> i64 {
    unix(SystemTime::now())
}

/// A time in Unix seconds (0 before the epoch, saturating far ahead).
#[must_use]
pub fn unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// A duration in whole seconds, as a column takes it.
#[must_use]
pub fn seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

/// A unique-key violation as [`DbError::Conflict`], any other error as it is.
pub(crate) fn conflict(error: diesel::result::Error) -> DbError {
    match error {
        diesel::result::Error::DatabaseError(diesel::result::DatabaseErrorKind::UniqueViolation, _) => {
            DbError::Conflict
        }
        error => DbError::from(error),
    }
}

#[cfg(test)]
mod tests {
    use diesel::connection::SimpleConnection as _;
    use diesel::{Connection as _, SqliteConnection};

    use super::*;

    /// A new file another process writes before it is in WAL (as while it switches it): the switch can't wait for
    /// that writer, so the connection waits for its write lock instead, then switches the file.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_waits_for_another_process_writing_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sqlite");
        let mut other = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        other
            .batch_execute("CREATE TABLE held (x); BEGIN IMMEDIATE; INSERT INTO held VALUES (1);")
            .unwrap();
        let url = format!("sqlite://{}", path.display());
        let connecting = tokio::spawn(async move { Db::connect(&url, 1).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!connecting.is_finished(), "{:?}", connecting.await);
        other.batch_execute("COMMIT").unwrap();
        connecting.await.unwrap().unwrap();
        // the header's read and write versions: 2 is WAL
        assert_eq!(std::fs::read(&path).unwrap()[18..20], [2, 2]);
    }

    /// A migrated file whose write lock another process holds: the connection reads that no migration is pending and
    /// takes no lock, and a write waits the statement wait, then is `Locked`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_migrated_file_connects_beside_a_held_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sqlite");
        let url = format!("sqlite://{}", path.display());
        drop(Db::connect(&url, 1).await.unwrap());
        let mut other = SqliteConnection::establish(path.to_str().unwrap()).unwrap();
        other.batch_execute("BEGIN IMMEDIATE").unwrap();
        let started = std::time::Instant::now();
        let db = Db::connect(&url, 1).await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        let written = async {
            with_conn!(db, |conn| conn.batch_execute("CREATE TABLE written (x)").await?);
            Ok::<_, DbError>(())
        };
        let written = written.await;
        assert!(
            matches!(written, Err(DbError::Locked(Locked { waited })) if waited == Duration::from_secs(5)),
            "{written:?}"
        );
        other.batch_execute("ROLLBACK").unwrap();
    }
}
