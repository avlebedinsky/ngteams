//! Messages by conversation and id, ordered by their `version`: an edit or a soft delete is a newer version of the
//! same message, and a version that arrives late (a sync after the Trouter delivery, or the other way round) changes
//! nothing. Reactions are kept per user, each with the time of its last change.

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::{Applied, Store, StoreError, sql_limit};

/// One message as the store keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub conversation_id: String,
    /// The service's message id.
    pub id: String,
    /// The service's `version`: a newer one replaces the row, an older or equal one is ignored.
    pub version: i64,
    /// The `clientmessageid` it was sent with: how our own message coming back is matched to its outbox row.
    pub client_message_id: Option<String>,
    /// The sender's MRI.
    pub sender: Option<String>,
    /// The sender's display name as the message carried it (`imdisplayname`).
    pub sender_name: Option<String>,
    /// `messagetype` as the service sent it (`RichText/Html`, `Text`, `ThreadActivity/…`, …).
    pub message_type: String,
    /// The content as the service sent it (HTML for rich text); empty once deleted.
    pub content: String,
    /// The message's `properties` object (mentions, files, …) as JSON text; none once deleted.
    pub properties: Option<String>,
    /// `originalarrivaltime`, in milliseconds: the order of a conversation.
    pub arrival_time: i64,
    pub compose_time: Option<i64>,
    /// When it was last edited, if ever.
    pub edit_time: Option<i64>,
    /// When it was deleted (a soft delete keeps the row, without its content).
    pub delete_time: Option<i64>,
}

/// One user's reaction to a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reaction {
    /// The reaction's key (`like`, `heart`, …).
    pub key: String,
    /// The reacting user's MRI.
    pub user: String,
    /// When it was set, in milliseconds.
    pub time: i64,
}

/// One user's reaction set or removed. The change with the later `time` wins, so a late one can't undo a newer one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionChange {
    pub conversation_id: String,
    pub message_id: String,
    pub key: String,
    pub user: String,
    pub time: i64,
    /// Set (`true`) or removed.
    pub present: bool,
}

impl ReactionChange {
    #[must_use]
    pub fn add(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64) -> Self {
        Self::new(conversation_id, message_id, key, user, time, true)
    }

    #[must_use]
    pub fn remove(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64) -> Self {
        Self::new(conversation_id, message_id, key, user, time, false)
    }

    fn new(conversation_id: &str, message_id: &str, key: &str, user: &str, time: i64, present: bool) -> Self {
        Self {
            conversation_id: conversation_id.to_owned(),
            message_id: message_id.to_owned(),
            key: key.to_owned(),
            user: user.to_owned(),
            time,
            present,
        }
    }
}

/// Where a page of messages ends: the next page starts just before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageCursor {
    pub arrival_time: i64,
    pub id: String,
}

/// A page of a conversation's messages, the newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePage {
    pub messages: Vec<Message>,
    /// The cursor for the next (older) page; none when this page reaches the oldest stored message.
    pub next: Option<PageCursor>,
}

const MESSAGE_COLUMNS: &str = "conversation_id, id, version, client_message_id, sender, sender_name, message_type, \
                               content, properties, arrival_time, compose_time, edit_time, delete_time";

fn message_from_row(row: &Row<'_>) -> rusqlite::Result<Message> {
    Ok(Message {
        conversation_id: row.get(0)?,
        id: row.get(1)?,
        version: row.get(2)?,
        client_message_id: row.get(3)?,
        sender: row.get(4)?,
        sender_name: row.get(5)?,
        message_type: row.get(6)?,
        content: row.get(7)?,
        properties: row.get(8)?,
        arrival_time: row.get(9)?,
        compose_time: row.get(10)?,
        edit_time: row.get(11)?,
        delete_time: row.get(12)?,
    })
}

/// Writes the message's row (a new one or a newer version of a stored one), without its content once deleted.
fn write_message(conn: &Connection, message: &Message) -> rusqlite::Result<()> {
    let deleted = message.delete_time.is_some();
    let content = if deleted { "" } else { message.content.as_str() };
    let properties = if deleted { None } else { message.properties.as_deref() };
    conn.execute(
        &format!(
            "INSERT INTO messages ({MESSAGE_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (conversation_id, id) DO UPDATE SET version = excluded.version,
                 client_message_id = excluded.client_message_id, sender = excluded.sender,
                 sender_name = excluded.sender_name, message_type = excluded.message_type,
                 content = excluded.content, properties = excluded.properties,
                 arrival_time = excluded.arrival_time, compose_time = excluded.compose_time,
                 edit_time = excluded.edit_time, delete_time = excluded.delete_time"
        ),
        params![
            message.conversation_id,
            message.id,
            message.version,
            message.client_message_id,
            message.sender,
            message.sender_name,
            message.message_type,
            content,
            properties,
            message.arrival_time,
            message.compose_time,
            message.edit_time,
            message.delete_time,
        ],
    )?;
    conn.execute(
        "UPDATE conversations SET last_activity = MAX(COALESCE(last_activity, ?2), ?2) WHERE id = ?1",
        params![message.conversation_id, message.arrival_time],
    )?;
    Ok(())
}

/// Replaces a message's reactions with `reactions` (all set), or drops them all.
fn replace_reactions(conn: &Connection, message: &Message, reactions: &[Reaction]) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM reactions WHERE conversation_id = ?1 AND message_id = ?2",
        params![message.conversation_id, message.id],
    )?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO reactions (conversation_id, message_id, reaction, user_mri, time, present)
         VALUES (?1, ?2, ?3, ?4, ?5, 1)
         ON CONFLICT DO UPDATE SET time = MAX(time, excluded.time)",
    )?;
    for reaction in reactions {
        insert.execute(params![
            message.conversation_id,
            message.id,
            reaction.key,
            reaction.user,
            reaction.time
        ])?;
    }
    Ok(())
}

impl Store {
    /// Writes a message unless the stored one is as new or newer (by `version`), and moves the conversation's last
    /// activity up to its arrival. A deleted message (`delete_time` set) is stored without content, properties or
    /// reactions. `reactions`, when given, is the message's whole reaction set at this version and replaces the
    /// stored one; when not, the stored reactions stay.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the write
    /// fails.
    pub fn upsert_message(&mut self, message: &Message, reactions: Option<&[Reaction]>) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        Self::require_conversation(&tx, &message.conversation_id)?;
        let stored: Option<i64> = tx
            .query_row(
                "SELECT version FROM messages WHERE conversation_id = ?1 AND id = ?2",
                params![message.conversation_id, message.id],
                |row| row.get(0),
            )
            .optional()?;
        let applied = match stored {
            None => Applied::Inserted,
            Some(version) if message.version > version => Applied::Updated,
            Some(_) => return Ok(Applied::Ignored),
        };
        write_message(&tx, message)?;
        if message.delete_time.is_some() {
            replace_reactions(&tx, message, &[])?;
        } else if let Some(reactions) = reactions {
            replace_reactions(&tx, message, reactions)?;
        }
        tx.commit()?;
        Ok(applied)
    }

    /// Applies one user's reaction change unless a change at the same or a later time is stored. A change on a
    /// deleted message is ignored.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownMessage`] when the message isn't stored, [`StoreError::Sqlite`] when the write fails.
    pub fn apply_reaction(&mut self, change: &ReactionChange) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        let message: Option<Option<i64>> = tx
            .query_row(
                "SELECT delete_time FROM messages WHERE conversation_id = ?1 AND id = ?2",
                params![change.conversation_id, change.message_id],
                |row| row.get(0),
            )
            .optional()?;
        match message {
            None => {
                return Err(StoreError::UnknownMessage {
                    conversation: change.conversation_id.clone(),
                    message: change.message_id.clone(),
                });
            }
            Some(Some(_deleted)) => return Ok(Applied::Ignored),
            Some(None) => {}
        }
        let key = params![change.conversation_id, change.message_id, change.key, change.user];
        let stored: Option<i64> = tx
            .query_row(
                "SELECT time FROM reactions
                 WHERE conversation_id = ?1 AND message_id = ?2 AND reaction = ?3 AND user_mri = ?4",
                key,
                |row| row.get(0),
            )
            .optional()?;
        let applied = match stored {
            None => Applied::Inserted,
            Some(time) if change.time > time => Applied::Updated,
            Some(_) => return Ok(Applied::Ignored),
        };
        tx.execute(
            "INSERT INTO reactions (conversation_id, message_id, reaction, user_mri, time, present)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT DO UPDATE SET time = excluded.time, present = excluded.present",
            params![
                change.conversation_id,
                change.message_id,
                change.key,
                change.user,
                change.time,
                change.present
            ],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    /// A message's reactions that are set, by key and then user.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn reactions(&self, conversation_id: &str, message_id: &str) -> Result<Vec<Reaction>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT reaction, user_mri, time FROM reactions
             WHERE conversation_id = ?1 AND message_id = ?2 AND present = 1 ORDER BY reaction, user_mri",
        )?;
        let rows = statement.query_map(params![conversation_id, message_id], |row| {
            Ok(Reaction {
                key: row.get(0)?,
                user: row.get(1)?,
                time: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One message.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn message(&self, conversation_id: &str, id: &str) -> Result<Option<Message>, StoreError> {
        let sql = format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 AND id = ?2");
        Ok(self
            .conn
            .query_row(&sql, params![conversation_id, id], message_from_row)
            .optional()?)
    }

    /// The message sent with `client_message_id` in a conversation: how our own message is recognised when it comes
    /// back through Trouter or a sync.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn message_by_client_id(
        &self,
        conversation_id: &str,
        client_message_id: &str,
    ) -> Result<Option<Message>, StoreError> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages WHERE conversation_id = ?1 AND client_message_id = ?2
             ORDER BY arrival_time, id LIMIT 1"
        );
        Ok(self
            .conn
            .query_row(&sql, params![conversation_id, client_message_id], message_from_row)
            .optional()?)
    }

    /// Up to `limit` messages of a conversation older than `before` (from the newest when none), the newest first.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn messages_before(
        &self,
        conversation_id: &str,
        before: Option<&PageCursor>,
        limit: usize,
    ) -> Result<MessagePage, StoreError> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE conversation_id = ?1 AND (?2 IS NULL OR arrival_time < ?2 OR (arrival_time = ?2 AND id < ?3))
             ORDER BY arrival_time DESC, id DESC LIMIT ?4"
        );
        let mut statement = self.conn.prepare_cached(&sql)?;
        let (arrival, id) = before.map(|cursor| (cursor.arrival_time, cursor.id.as_str())).unzip();
        // one more than asked for, to know whether an older page exists
        let rows = statement.query_map(
            params![conversation_id, arrival, id, sql_limit(limit).saturating_add(1)],
            message_from_row,
        )?;
        let mut messages: Vec<Message> = rows.collect::<Result<_, _>>()?;
        let more = messages.len() > limit;
        messages.truncate(limit);
        let next = messages.last().filter(|_| more).map(|last| PageCursor {
            arrival_time: last.arrival_time,
            id: last.id.clone(),
        });
        Ok(MessagePage { messages, next })
    }

    /// How many messages of a conversation `me` hasn't read: text messages from others, not deleted, that arrived
    /// after `me`'s read horizon (all of them without one).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn unread_count(&self, conversation_id: &str, me: &str) -> Result<u64, StoreError> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM messages
             WHERE conversation_id = ?1 AND delete_time IS NULL AND sender IS NOT ?2
               AND (message_type = 'Text' OR message_type GLOB 'RichText*')
               AND NOT EXISTS (SELECT 1 FROM read_horizons AS horizon
                   WHERE horizon.conversation_id = ?1 AND horizon.user_mri = ?2
                     AND horizon.last_read_arrival >= messages.arrival_time)",
            params![conversation_id, me],
            |row| row.get(0),
        )?;
        Ok(u64::try_from(count).unwrap_or_default())
    }
}
