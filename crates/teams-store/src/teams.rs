//! The account's teams and their channels as CSA last listed them (M2 Task 16), replaced whole by each read, and the
//! reply chain each stored channel message belongs to (its post, as the message's `conversationLink` named it). A
//! channel's messages are kept like a chat's ([`crate::messages`]); its conversation row is the chat service's.

use std::fmt;

use rusqlite::{OptionalExtension, params};

use crate::{Applied, Store, StoreError};

/// A team as last listed. `Debug` leaves the names out.
#[derive(Clone, PartialEq, Eq)]
pub struct Team {
    /// Its id: its general channel's.
    pub id: String,
    pub name: String,
    pub archived: bool,
    /// Its channels in the order listed, the general one first.
    pub channels: Vec<TeamChannel>,
}

impl fmt::Debug for Team {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Team")
            .field("id", &self.id)
            .field("archived", &self.archived)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

/// A team's channel as last listed. `Debug` leaves the name out.
#[derive(Clone, PartialEq, Eq)]
pub struct TeamChannel {
    pub id: String,
    pub name: String,
    pub general: bool,
}

impl fmt::Debug for TeamChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeamChannel")
            .field("id", &self.id)
            .field("general", &self.general)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Replaces the stored teams with `teams` (a team or a channel listed twice keeps its first place), read at
    /// `read_at` (ms): [`Applied::Inserted`] for the first list, [`Applied::Updated`] when it differs from the stored
    /// one, [`Applied::Ignored`] when it doesn't (only the read time moves).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn replace_teams(&mut self, teams: &[Team], read_at: i64) -> Result<Applied, StoreError> {
        let read_before = self.teams_read_at()?;
        let stored = self.teams()?;
        let teams = deduplicated(teams);
        let applied = match read_before {
            None => Applied::Inserted,
            Some(_) if stored != teams => Applied::Updated,
            Some(_) => Applied::Ignored,
        };
        let tx = self.conn.transaction()?;
        if applied != Applied::Ignored {
            tx.execute("DELETE FROM teams", [])?;
            let mut team_row =
                tx.prepare_cached("INSERT INTO teams (id, name, archived, position) VALUES (?1, ?2, ?3, ?4)")?;
            let mut channel_row = tx.prepare_cached(
                "INSERT INTO team_channels (id, team_id, name, general, position) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for (position, team) in (0_i64..).zip(&teams) {
                team_row.execute(params![team.id, team.name, team.archived, position])?;
                for (place, channel) in (0_i64..).zip(&team.channels) {
                    channel_row.execute(params![channel.id, team.id, channel.name, channel.general, place])?;
                }
            }
        }
        tx.execute(
            "INSERT INTO teams_read (id, read_at) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET read_at = excluded.read_at",
            [read_at],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    /// The stored teams, in the order listed, each with its channels.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn teams(&self) -> Result<Vec<Team>, StoreError> {
        let mut teams: Vec<Team> = self
            .conn
            .prepare_cached("SELECT id, name, archived FROM teams ORDER BY position")?
            .query_map([], |row| {
                Ok(Team {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    archived: row.get(2)?,
                    channels: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut channels = self
            .conn
            .prepare_cached("SELECT id, name, general FROM team_channels WHERE team_id = ?1 ORDER BY position")?;
        for team in &mut teams {
            team.channels = channels
                .query_map([&team.id], |row| {
                    Ok(TeamChannel {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        general: row.get(2)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
        }
        Ok(teams)
    }

    /// When the stored teams were last read (ms); `None` before the first read.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn teams_read_at(&self) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .prepare_cached("SELECT read_at FROM teams_read WHERE id = 1")?
            .query_row([], |row| row.get(0))
            .optional()?)
    }

    /// Records the post `root` that the stored channel message `message` of `conversation` belongs to (its own id for a
    /// post); nothing for a message the store doesn't hold.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn set_reply_root(&mut self, conversation: &str, message: &str, root: &str) -> Result<(), StoreError> {
        self.conn
            .prepare_cached(
                "INSERT INTO reply_roots (conversation_id, message_id, root_id)
                 SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM messages WHERE conversation_id = ?1 AND id = ?2)
                 ON CONFLICT (conversation_id, message_id) DO UPDATE SET root_id = excluded.root_id",
            )?
            .execute(params![conversation, message, root])?;
        Ok(())
    }

    /// The recorded posts of a conversation's stored messages, as (message id, post id), in no set order.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn reply_roots(&self, conversation: &str) -> Result<Vec<(String, String)>, StoreError> {
        let roots = self
            .conn
            .prepare_cached("SELECT message_id, root_id FROM reply_roots WHERE conversation_id = ?1")?
            .query_map([conversation], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(roots)
    }
}

/// `teams` with a team or a channel listed again (by id, anywhere in the list) left out.
fn deduplicated(teams: &[Team]) -> Vec<Team> {
    let mut seen_teams = std::collections::HashSet::new();
    let mut seen_channels = std::collections::HashSet::new();
    teams
        .iter()
        .filter(|team| seen_teams.insert(team.id.clone()))
        .map(|team| Team {
            channels: team
                .channels
                .iter()
                .filter(|channel| seen_channels.insert(channel.id.clone()))
                .cloned()
                .collect(),
            ..team.clone()
        })
        .collect()
}
