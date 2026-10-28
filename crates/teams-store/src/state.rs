//! Per-conversation state that isn't a message: each member's read horizon, and where the conversation's sync stands
//! (plus where the conversation list's sync stands).

use rusqlite::{OptionalExtension, params};

use crate::{Applied, Store, StoreError};

/// A member's read horizon in a conversation (`consumptionhorizon`). The horizon set later wins, even when it moves
/// back (a chat marked unread).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadHorizon {
    pub conversation_id: String,
    /// The member's MRI.
    pub user: String,
    /// The arrival time (milliseconds) of the last message read: later ones are unread.
    pub last_read_arrival: i64,
    /// When the horizon was set, in milliseconds: what orders two horizons of the same member.
    pub updated: i64,
    /// The service's horizon string, kept as it came.
    pub raw: Option<String>,
}

/// Where a conversation's message sync stands. The links are the service's own (`syncState`, `backwardLink`), kept as
/// given and followed only on the same chat base.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    pub conversation_id: String,
    /// The link that brings what changed since the last sync.
    pub sync_link: Option<String>,
    /// The link to the next older page of history not yet stored.
    pub backward_link: Option<String>,
    /// The oldest arrival time (milliseconds) on the page `backward_link` came with: every message newer than that is
    /// stored, apart from a gap's. None when no time on those pages could be read.
    pub backward_oldest: Option<i64>,
    /// How far back the next sync answer is read (ms): the `lastCompleteSegmentEndTime` the kept `sync_link` came
    /// with. Every message that arrived up to then is stored, or kept in the gap. None when unknown (a state of an
    /// older schema, or an answer without the time).
    pub sync_until: Option<i64>,
    /// The rest of a sync answer that was cut short (its own `backwardLink`): what changed since the sync before it,
    /// not yet stored. Read before older history, and never in its place.
    pub gap_link: Option<String>,
    /// How far back the gap is read (ms): the `sync_until` of the sync before the answer it is the rest of. None when
    /// there is no gap, or it is unknown.
    pub gap_until: Option<i64>,
    /// The whole history is stored: there is no older page and no gap.
    pub history_complete: bool,
    /// When this state was written, in milliseconds.
    pub updated: i64,
}

impl Store {
    /// Writes a member's read horizon unless the stored one was set at the same time or later.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the
    /// write fails.
    pub fn set_read_horizon(&mut self, horizon: &ReadHorizon) -> Result<Applied, StoreError> {
        let tx = self.conn.transaction()?;
        Self::require_conversation(&tx, &horizon.conversation_id)?;
        let stored: Option<i64> = tx
            .query_row(
                "SELECT updated FROM read_horizons WHERE conversation_id = ?1 AND user_mri = ?2",
                params![horizon.conversation_id, horizon.user],
                |row| row.get(0),
            )
            .optional()?;
        let applied = match stored {
            None => Applied::Inserted,
            Some(updated) if horizon.updated > updated => Applied::Updated,
            Some(_) => return Ok(Applied::Ignored),
        };
        tx.execute(
            "INSERT INTO read_horizons (conversation_id, user_mri, last_read_arrival, updated, raw)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT DO UPDATE SET last_read_arrival = excluded.last_read_arrival, updated = excluded.updated,
                 raw = excluded.raw",
            params![
                horizon.conversation_id,
                horizon.user,
                horizon.last_read_arrival,
                horizon.updated,
                horizon.raw
            ],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    /// A member's read horizon in a conversation.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn read_horizon(&self, conversation_id: &str, user: &str) -> Result<Option<ReadHorizon>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT last_read_arrival, updated, raw FROM read_horizons
                 WHERE conversation_id = ?1 AND user_mri = ?2",
                params![conversation_id, user],
                |row| {
                    Ok(ReadHorizon {
                        conversation_id: conversation_id.to_owned(),
                        user: user.to_owned(),
                        last_read_arrival: row.get(0)?,
                        updated: row.get(1)?,
                        raw: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replaces a conversation's sync state.
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the
    /// write fails.
    pub fn set_sync_state(&mut self, state: &SyncState) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        Self::require_conversation(&tx, &state.conversation_id)?;
        tx.execute(
            "INSERT INTO conversation_sync (conversation_id, sync_link, backward_link, backward_oldest, sync_until,
                 gap_link, gap_until, history_complete, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT DO UPDATE SET sync_link = excluded.sync_link, backward_link = excluded.backward_link,
                 backward_oldest = excluded.backward_oldest, sync_until = excluded.sync_until,
                 gap_link = excluded.gap_link, gap_until = excluded.gap_until,
                 history_complete = excluded.history_complete, updated = excluded.updated",
            params![
                state.conversation_id,
                state.sync_link,
                state.backward_link,
                state.backward_oldest,
                state.sync_until,
                state.gap_link,
                state.gap_until,
                state.history_complete,
                state.updated
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// A conversation's sync state; none before its first sync.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn sync_state(&self, conversation_id: &str) -> Result<Option<SyncState>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT sync_link, backward_link, backward_oldest, sync_until, gap_link, gap_until, history_complete,
                     updated
                 FROM conversation_sync WHERE conversation_id = ?1",
                [conversation_id],
                |row| {
                    Ok(SyncState {
                        conversation_id: conversation_id.to_owned(),
                        sync_link: row.get(0)?,
                        backward_link: row.get(1)?,
                        backward_oldest: row.get(2)?,
                        sync_until: row.get(3)?,
                        gap_link: row.get(4)?,
                        gap_until: row.get(5)?,
                        history_complete: row.get(6)?,
                        updated: row.get(7)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replaces the conversation list's sync link (none: the next list sync starts from scratch).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn set_conversation_list_sync_link(&mut self, link: Option<&str>, updated: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO conversation_list_sync (id, sync_link, updated) VALUES (1, ?1, ?2)
             ON CONFLICT DO UPDATE SET sync_link = excluded.sync_link, updated = excluded.updated",
            params![link, updated],
        )?;
        Ok(())
    }

    /// The conversation list's sync link; none before the first list sync.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn conversation_list_sync_link(&self) -> Result<Option<String>, StoreError> {
        let link: Option<Option<String>> = self
            .conn
            .query_row("SELECT sync_link FROM conversation_list_sync WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(link.flatten())
    }

    /// Keeps `conversation_id` to be synced again by every later catch-up, until [`Store::set_synced`].
    ///
    /// # Errors
    ///
    /// [`StoreError::UnknownConversation`] when the conversation isn't stored, [`StoreError::Sqlite`] when the
    /// write fails.
    pub fn set_unsynced(&mut self, conversation_id: &str) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        Self::require_conversation(&tx, conversation_id)?;
        tx.execute(
            "INSERT INTO unsynced_conversations (conversation_id) VALUES (?1) ON CONFLICT DO NOTHING",
            [conversation_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `conversation_id` was synced: no later catch-up has to sync it again. Whether it had been kept to be.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn set_synced(&mut self, conversation_id: &str) -> Result<bool, StoreError> {
        let removed = self.conn.execute(
            "DELETE FROM unsynced_conversations WHERE conversation_id = ?1",
            [conversation_id],
        )?;
        Ok(removed > 0)
    }

    /// The conversations kept to be synced again ([`Store::set_unsynced`]), by id.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn unsynced_conversations(&self) -> Result<Vec<String>, StoreError> {
        let mut statement = self
            .conn
            .prepare("SELECT conversation_id FROM unsynced_conversations ORDER BY conversation_id")?;
        let ids = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }
}
