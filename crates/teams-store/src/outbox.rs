//! Writes waiting to go out. One row per command, keyed by the `clientmessageid` it keeps across every attempt; the
//! core serializes the command into `payload` and the store doesn't read it.
//!
//! A row belongs to the run that made it ([`Store::run_id`]): only that run can begin an attempt on it. Rows an
//! earlier process left pending or in flight are listed by [`Store::outbox_left_by_earlier_runs`] and go out again
//! only through [`Store::resend`], an explicit command; nothing is resent on start (the web client's late resend of a
//! failed send, seen in M0, is what this rules out).
//!
//! States: `pending` → [`Store::begin_attempt`] → `sending` → [`Store::finish_attempt`] → `sent`, `denied`, `failed`
//! (all final) or back to `pending` for a retry after `not_before`. Our message coming back (Trouter or a sync) marks
//! its row `sent` whatever state it is in ([`Store::outbox_arrived`]).

use std::fmt;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef};
use rusqlite::{Connection, OptionalExtension, Row, ToSql, params};

use crate::{Store, StoreError, now_ms};

/// What a row asks the service to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxKind {
    Send,
    Edit,
    Delete,
    AddReaction,
    RemoveReaction,
    ReadReceipt,
    CreateChat,
}

/// Where a row stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxState {
    /// Waiting for an attempt (the first, or a retry after `not_before`).
    Pending,
    /// An attempt was begun and hasn't finished; left like this by an earlier run, whether it arrived is unknown.
    Sending,
    /// The service took it, or our message came back.
    Sent,
    /// The fence denied it: nothing was sent.
    Denied,
    /// The service refused it for good.
    Failed,
}

macro_rules! text_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $name {
            /// The stored text.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            fn from_text(text: &str) -> Option<Self> {
                match text { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $name {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let text = value.as_str()?;
                Self::from_text(text).ok_or_else(|| FromSqlError::Other(Box::new(UnknownText(text.to_owned()))))
            }
        }
    };
}

text_enum!(OutboxKind {
    Send => "send",
    Edit => "edit",
    Delete => "delete",
    AddReaction => "add_reaction",
    RemoveReaction => "remove_reaction",
    ReadReceipt => "read_receipt",
    CreateChat => "create_chat",
});

text_enum!(OutboxState {
    Pending => "pending",
    Sending => "sending",
    Sent => "sent",
    Denied => "denied",
    Failed => "failed",
});

/// A stored kind or state this build doesn't know.
#[derive(Debug)]
struct UnknownText(String);

impl fmt::Display for UnknownText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown outbox value {:?}", self.0)
    }
}

impl std::error::Error for UnknownText {}

/// A command to put in the outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewOutboxEntry {
    /// The conversation it writes to; none for a chat that doesn't exist yet.
    pub conversation_id: Option<String>,
    pub kind: OutboxKind,
    /// Fresh for each command, kept across its attempts.
    pub client_message_id: String,
    /// The command as the core serialized it.
    pub payload: String,
}

/// An outbox row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEntry {
    pub id: i64,
    /// The run it belongs to: only that run sends it.
    pub run_id: i64,
    pub client_message_id: String,
    pub conversation_id: Option<String>,
    pub kind: OutboxKind,
    pub payload: String,
    pub state: OutboxState,
    /// How many attempts were begun.
    pub attempts: u32,
    /// No attempt before this time (milliseconds): a `Retry-After`.
    pub not_before: Option<i64>,
    /// Why the last attempt didn't send it.
    pub last_error: Option<String>,
    /// The service's id for the message, once known.
    pub message_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// How an attempt ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    Sent {
        message_id: Option<String>,
    },
    /// Try again with the same `clientmessageid`, not before `not_before`.
    Retry {
        not_before: i64,
        error: String,
    },
    Denied {
        reason: String,
    },
    Failed {
        error: String,
    },
}

const OUTBOX_COLUMNS: &str = "id, run_id, client_message_id, conversation_id, kind, payload, state, attempts, \
                              not_before, last_error, message_id, created_at, updated_at";

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<OutboxEntry> {
    Ok(OutboxEntry {
        id: row.get(0)?,
        run_id: row.get(1)?,
        client_message_id: row.get(2)?,
        conversation_id: row.get(3)?,
        kind: row.get(4)?,
        payload: row.get(5)?,
        state: row.get(6)?,
        attempts: row.get(7)?,
        not_before: row.get(8)?,
        last_error: row.get(9)?,
        message_id: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

fn entry(conn: &Connection, id: i64) -> Result<OutboxEntry, StoreError> {
    let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE id = ?1");
    conn.query_row(&sql, [id], entry_from_row)
        .optional()?
        .ok_or(StoreError::UnknownOutboxEntry(id))
}

fn entries(conn: &Connection, filter: &str, values: impl rusqlite::Params) -> Result<Vec<OutboxEntry>, StoreError> {
    let sql = format!("SELECT {OUTBOX_COLUMNS} FROM outbox WHERE {filter} ORDER BY id");
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement.query_map(values, entry_from_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl Store {
    /// Puts a command in the outbox as a pending row of this run.
    ///
    /// # Errors
    ///
    /// [`StoreError::DuplicateClientMessageId`] when a row has its `clientmessageid` already, [`StoreError::Sqlite`]
    /// when the write fails.
    pub fn enqueue(&mut self, new: &NewOutboxEntry) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let taken: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM outbox WHERE client_message_id = ?1)",
            [&new.client_message_id],
            |row| row.get(0),
        )?;
        if taken {
            return Err(StoreError::DuplicateClientMessageId(new.client_message_id.clone()));
        }
        let now = now_ms();
        tx.execute(
            "INSERT INTO outbox (run_id, client_message_id, conversation_id, kind, payload, state, created_at,
                 updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                self.run_id,
                new.client_message_id,
                new.conversation_id,
                new.kind,
                new.payload,
                OutboxState::Pending,
                now
            ],
        )?;
        let stored = entry(&tx, tx.last_insert_rowid())?;
        tx.commit()?;
        Ok(stored)
    }

    /// One outbox row, if it exists.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_entry(&self, id: i64) -> Result<Option<OutboxEntry>, StoreError> {
        match entry(&self.conn, id) {
            Ok(found) => Ok(Some(found)),
            Err(StoreError::UnknownOutboxEntry(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The outbox row with a `clientmessageid`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_by_client_message_id(&self, client_message_id: &str) -> Result<Option<OutboxEntry>, StoreError> {
        Ok(entries(&self.conn, "client_message_id = ?1", [client_message_id])?
            .into_iter()
            .next())
    }

    /// This run's pending rows whose `not_before` has passed at `now` (milliseconds), oldest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_due(&self, now: i64) -> Result<Vec<OutboxEntry>, StoreError> {
        entries(
            &self.conn,
            "run_id = ?1 AND state = ?2 AND (not_before IS NULL OR not_before <= ?3)",
            params![self.run_id, OutboxState::Pending, now],
        )
    }

    /// Rows an earlier run left pending or in flight: never sent on their own, only through [`Store::resend`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn outbox_left_by_earlier_runs(&self) -> Result<Vec<OutboxEntry>, StoreError> {
        entries(
            &self.conn,
            "run_id <> ?1 AND state IN (?2, ?3)",
            params![self.run_id, OutboxState::Pending, OutboxState::Sending],
        )
    }

    /// Begins an attempt on one of this run's pending rows: it becomes `sending` and its attempt count goes up.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row that isn't
    /// pending or belongs to another run, [`StoreError::Sqlite`] when the write fails.
    pub fn begin_attempt(&mut self, id: i64) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state != OutboxState::Pending || row.run_id != self.run_id {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        tx.execute(
            "UPDATE outbox SET state = ?2, attempts = attempts + 1, updated_at = ?3 WHERE id = ?1",
            params![id, OutboxState::Sending, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Finishes this run's attempt on a row. A row already `sent` (our message came back first) stays as it is,
    /// whatever the outcome.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row with no attempt
    /// of this run in flight, [`StoreError::Sqlite`] when the write fails.
    pub fn finish_attempt(&mut self, id: i64, outcome: &AttemptOutcome) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state == OutboxState::Sent {
            return Ok(row);
        }
        if row.state != OutboxState::Sending || row.run_id != self.run_id {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        let (state, not_before, error, message_id) = match outcome {
            AttemptOutcome::Sent { message_id } => (OutboxState::Sent, None, None, message_id.as_deref()),
            AttemptOutcome::Retry { not_before, error } => (OutboxState::Pending, Some(*not_before), Some(error), None),
            AttemptOutcome::Denied { reason } => (OutboxState::Denied, None, Some(reason), None),
            AttemptOutcome::Failed { error } => (OutboxState::Failed, None, Some(error), None),
        };
        tx.execute(
            "UPDATE outbox SET state = ?2, not_before = ?3, last_error = COALESCE(?4, last_error),
                 message_id = COALESCE(?5, message_id), updated_at = ?6
             WHERE id = ?1",
            params![id, state, not_before, error, message_id, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Adopts a row into this run as pending, with its `clientmessageid` unchanged: the explicit resend of a row an
    /// earlier run left, or of one that was denied or failed. For a row left `sending`, the caller first looks for its
    /// `clientmessageid` in a sync: the earlier attempt may have arrived.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownOutboxEntry`] for no such row, [`StoreError::OutboxNotSendable`] for a row already sent,
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn resend(&mut self, id: i64) -> Result<OutboxEntry, StoreError> {
        let tx = self.conn.transaction()?;
        let row = entry(&tx, id)?;
        if row.state == OutboxState::Sent {
            return Err(StoreError::OutboxNotSendable { id, state: row.state });
        }
        tx.execute(
            "UPDATE outbox SET run_id = ?2, state = ?3, not_before = NULL, updated_at = ?4 WHERE id = ?1",
            params![id, self.run_id, OutboxState::Pending, now_ms()],
        )?;
        let updated = entry(&tx, id)?;
        tx.commit()?;
        Ok(updated)
    }

    /// Our own message came back (Trouter or a sync) with `client_message_id`: its row, if any, is `sent` with the
    /// service's `message_id`.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn outbox_arrived(
        &mut self,
        client_message_id: &str,
        message_id: &str,
    ) -> Result<Option<OutboxEntry>, StoreError> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE outbox SET state = ?2, message_id = ?3, not_before = NULL, updated_at = ?4
             WHERE client_message_id = ?1",
            params![client_message_id, OutboxState::Sent, message_id, now_ms()],
        )?;
        let row = if changed == 0 {
            None
        } else {
            entries(&tx, "client_message_id = ?1", [client_message_id])?.pop()
        };
        tx.commit()?;
        Ok(row)
    }
}
