//! Local search over the stored messages' plain text (M2 Task 15), in SQLite's FTS5: the account's own store, never
//! the service's search.
//!
//! - **What is indexed**: the text the core hands with each message it stores ([`Store::upsert_message_indexed`]): an
//!   edit (a newer version) replaces its text, a delete removes it. The store keeps the service's HTML and can't read
//!   it as text, so the messages stored before the index existed (or by [`Store::upsert_message`], which indexes
//!   nothing) are indexed by the core's backfill: [`Store::messages_to_index`] lists them a batch at a time in a fixed
//!   order, [`Store::index_messages`] stores their text. What is indexed is kept in the store, so a backfill that
//!   stopped goes on where it was at the next start.
//! - **The query** ([`Store::search`]) is the user's input, never FTS5's syntax: each whitespace-separated term is
//!   quoted as a string and matched as a prefix, all of them in a message ([`match_expression`]), so `"`, `*`, `-`,
//!   `:`, `NEAR`, `AND`, `OR`, `NOT` and parentheses are looked for as the text they are, and never make the query
//!   fail. Hits come the newest first, each with the text around its match ([`SearchHit::snippet`]).

use std::fmt;

use rusqlite::{Connection, OptionalExtension, params};

use crate::messages::{MESSAGE_COLUMNS, message_from_row};
use crate::{Message, Store, StoreError, sql_limit};

/// Where a match starts in a hit's snippet: a private-use character, removed from every indexed text.
pub const MATCH_START: char = '\u{E000}';
/// Where a match ends in a hit's snippet.
pub const MATCH_END: char = '\u{E001}';
/// What stands for text left out of a snippet.
pub const ELLIPSIS: &str = "…";
/// How many tokens a snippet holds at most.
pub const SNIPPET_TOKENS: u32 = 16;
/// The most terms of a query that are looked for; the rest are left out.
pub const MAX_TERMS: usize = 16;

/// A message to index and its text: what the core read from it.
#[derive(Clone, PartialEq, Eq)]
pub struct IndexText {
    pub conversation_id: String,
    pub message_id: String,
    /// The message's version the text was read from: an index row of a newer version is kept.
    pub version: i64,
    /// Its plain text; empty for a message with none (deleted, or not a text message).
    pub text: String,
}

impl fmt::Debug for IndexText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexText")
            .field("conversation_id", &self.conversation_id)
            .field("message_id", &self.message_id)
            .field("version", &self.version)
            .field("text", &self.text.len())
            .finish()
    }
}

/// Where the backfill's last batch ended: the next starts after it ([`Store::messages_to_index`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexCursor {
    pub conversation_id: String,
    pub message_id: String,
}

/// One message a search found. `Debug` leaves the snippet's text and the sender's name out.
#[derive(Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub conversation_id: String,
    pub message_id: String,
    /// Its arrival time (ms): where the chat view finds it.
    pub arrival_time: i64,
    pub sender: Option<String>,
    /// The name the message carried (`imdisplayname`).
    pub sender_name: Option<String>,
    /// The indexed text around the match, at most [`SNIPPET_TOKENS`] tokens, each matched term between
    /// [`MATCH_START`] and [`MATCH_END`], and [`ELLIPSIS`] where text is left out.
    pub snippet: String,
}

impl fmt::Debug for SearchHit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchHit")
            .field("conversation_id", &self.conversation_id)
            .field("message_id", &self.message_id)
            .field("arrival_time", &self.arrival_time)
            .field("sender", &self.sender)
            .field("sender_named", &self.sender_name.is_some())
            .field("snippet", &self.snippet.len())
            .finish()
    }
}

/// The FTS5 query for the user's `input`: each whitespace-separated term with a letter or a digit in it, quoted as an
/// FTS5 string (its `"` doubled) and matched as a prefix, at most [`MAX_TERMS`] of them, all needed. `None` when no
/// term has a letter or a digit: nothing can match it.
#[must_use]
pub fn match_expression(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .filter(|term| term.chars().any(char::is_alphanumeric))
        .take(MAX_TERMS)
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// `text` without the characters that mark a match in a snippet.
fn indexable(text: &str) -> String {
    text.chars()
        .filter(|&ch| ch != MATCH_START && ch != MATCH_END)
        .collect()
}

/// Indexes one message's text at its version, in place of an older version's; an index row of a newer version stays.
/// A message that isn't stored is left out. Whether it was indexed.
fn index_text(conn: &Connection, entry: &IndexText) -> rusqlite::Result<bool> {
    let stored: Option<(i64, i64)> = conn
        .prepare_cached("SELECT id, version FROM search_entries WHERE conversation_id = ?1 AND message_id = ?2")?
        .query_row(params![entry.conversation_id, entry.message_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let id = match stored {
        Some((_, version)) if version > entry.version => return Ok(false),
        Some((id, _)) => {
            conn.prepare_cached("DELETE FROM message_search WHERE rowid = ?1")?
                .execute([id])?;
            conn.prepare_cached("UPDATE search_entries SET version = ?2 WHERE id = ?1")?
                .execute(params![id, entry.version])?;
            id
        }
        None => {
            let known: bool = conn
                .prepare_cached("SELECT EXISTS (SELECT 1 FROM messages WHERE conversation_id = ?1 AND id = ?2)")?
                .query_row(params![entry.conversation_id, entry.message_id], |row| row.get(0))?;
            if !known {
                return Ok(false);
            }
            conn.prepare_cached(
                "INSERT INTO search_entries (conversation_id, message_id, version) VALUES (?1, ?2, ?3)",
            )?
            .execute(params![entry.conversation_id, entry.message_id, entry.version])?;
            conn.last_insert_rowid()
        }
    };
    let text = indexable(&entry.text);
    if !text.trim().is_empty() {
        conn.prepare_cached("INSERT INTO message_search (rowid, text) VALUES (?1, ?2)")?
            .execute(params![id, text])?;
    }
    Ok(true)
}

impl Store {
    /// Stores a message as [`Store::upsert_message`] does and, when that wrote it (new or newer), indexes the text
    /// `text` reads from the message as stored, in the same transaction: an edit replaces the text, a delete (stored
    /// without content) removes it when `text` gives none for it.
    ///
    /// # Errors
    ///
    /// As [`Store::upsert_message`]'s.
    pub fn upsert_message_indexed(
        &mut self,
        message: &crate::Message,
        reactions: Option<&[crate::Reaction]>,
        text: impl FnOnce(&crate::Message) -> String,
    ) -> Result<crate::Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let applied = crate::messages::upsert(&tx, message, reactions)?;
        if applied != crate::Applied::Ignored {
            let entry = IndexText {
                conversation_id: message.conversation_id.clone(),
                message_id: message.id.clone(),
                version: message.version,
                text: text(message),
            };
            index_text(&tx, &entry)?;
        }
        tx.commit()?;
        Ok(applied)
    }

    /// Up to `limit` stored messages the index doesn't hold at their version, after `after` in the order of their
    /// conversation and id (from the first when `None`): the backfill's next batch. Every message after `after` is
    /// looked at once in that order, so a backfill that goes on from the last batch's end reaches each message once.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn messages_to_index(&self, after: Option<&IndexCursor>, limit: usize) -> Result<Vec<Message>, StoreError> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE (?1 IS NULL OR (conversation_id, id) > (?1, ?2))
               AND NOT EXISTS (SELECT 1 FROM search_entries AS entry
                   WHERE entry.conversation_id = messages.conversation_id AND entry.message_id = messages.id
                     AND entry.version >= messages.version)
             ORDER BY conversation_id, id LIMIT ?3"
        );
        let (conversation, id) = after
            .map(|cursor| (cursor.conversation_id.as_str(), cursor.message_id.as_str()))
            .unzip();
        let rows = self
            .conn
            .prepare_cached(&sql)?
            .query_map(params![conversation, id, sql_limit(limit)], message_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Indexes the texts of a batch of messages in one transaction, each at its version (an index row of a newer
    /// version stays; a message no longer stored is left out). How many were indexed.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails: nothing of the batch is indexed then.
    pub fn index_messages(&mut self, texts: &[IndexText]) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let mut indexed = 0;
        for entry in texts {
            if index_text(&tx, entry)? {
                indexed += 1;
            }
        }
        tx.commit()?;
        Ok(indexed)
    }

    /// The stored messages whose indexed text holds every term of `input` (each as a prefix; see
    /// [`match_expression`]), the newest first, at most `limit`. Input with no letter or digit finds nothing.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn search(&self, input: &str, limit: usize) -> Result<Vec<SearchHit>, StoreError> {
        let Some(expression) = match_expression(input) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT entry.conversation_id, entry.message_id, message.arrival_time, message.sender,
                    message.sender_name,
                    snippet(message_search, 0, '{MATCH_START}', '{MATCH_END}', '{ELLIPSIS}', {SNIPPET_TOKENS})
             FROM message_search
             JOIN search_entries AS entry ON entry.id = message_search.rowid
             JOIN messages AS message
               ON message.conversation_id = entry.conversation_id AND message.id = entry.message_id
             WHERE message_search MATCH ?1
             ORDER BY message.arrival_time DESC, message.id DESC LIMIT ?2"
        );
        let hits = self
            .conn
            .prepare_cached(&sql)?
            .query_map(params![expression, sql_limit(limit)], |row| {
                Ok(SearchHit {
                    conversation_id: row.get(0)?,
                    message_id: row.get(1)?,
                    arrival_time: row.get(2)?,
                    sender: row.get(3)?,
                    sender_name: row.get(4)?,
                    snippet: row.get(5)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(hits)
    }
}
