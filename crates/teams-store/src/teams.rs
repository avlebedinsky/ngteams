//! The account's teams and their channels as CSA last listed them (M2 Task 16), replaced whole by each read, its
//! favourites (chats and channels) as the same read listed them (schema 11), and the reply chain each stored channel message belongs to (its post, as the message's `conversationLink` named it). A
//! channel's messages are kept like a chat's ([`crate::messages`]); its conversation row is the chat service's.

use std::fmt;

use rusqlite::{OptionalExtension, params};

use crate::{Applied, Store, StoreError};

/// A team as last listed. `Debug` leaves the names out.
#[derive(Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the service says of a team and of the account in it: each stands alone, no state machine"
)]
pub struct Team {
    /// Its id: its general channel's.
    pub id: String,
    pub name: String,
    pub archived: bool,
    /// Its Microsoft 365 group, when the read named one: a channel's creation names it.
    pub group_id: Option<String>,
    /// Its mail address, when the read named one: a private channel's request routes by it.
    pub mailbox: Option<String>,
    /// Whether the account owns it.
    pub owner: bool,
    /// Whether the account may add a channel to it.
    pub can_add_channel: bool,
    /// Whether the account may add a private channel to it.
    pub can_add_private_channel: bool,
    /// What the team pages show of it.
    pub about: TeamAbout,
    /// Its channels in the order listed, the general one first.
    pub channels: Vec<TeamChannel>,
}

/// What "Your teams and channels" and Manage team show of a team. `Debug` leaves the words out.
#[derive(Clone, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "what the service says of a team and of the account in it: each stands alone, no state machine"
)]
pub struct TeamAbout {
    /// The tenant it belongs to: a link to it names it.
    pub tenant: String,
    pub description: String,
    /// Anyone in the organization can join it.
    pub public: bool,
    /// Its people, bots left out.
    pub people: u32,
    /// The account is a guest on it.
    pub guest: bool,
    /// Its people come from a rule, not from hands.
    pub dynamic_membership: bool,
    /// It holds everyone in the organization.
    pub tenant_wide: bool,
    /// The account is only on some of its channels.
    pub channel_only: bool,
    /// Its channels are hidden for the account.
    pub hidden: bool,
    /// Its picture's version.
    pub picture_etag: Option<String>,
}

impl fmt::Debug for TeamAbout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TeamAbout")
            .field("public", &self.public)
            .field("people", &self.people)
            .field("hidden", &self.hidden)
            .finish_non_exhaustive()
    }
}

impl Team {
    /// Whether the account can add people or ask to, as Teams decides: an owner; on a public team that isn't everyone's
    /// or a private one, a member (who sends a request on a private team). Never a guest, on a team of a rule's people or
    /// on one the account is on only through channels, or on an archived one.
    #[must_use]
    pub fn can_add_member(&self) -> bool {
        let about = &self.about;
        !self.archived
            && !about.dynamic_membership
            && !about.guest
            && !about.channel_only
            && (self.owner || !about.tenant_wide)
    }

    /// Whether the account can leave, as Teams decides: not a team of a rule's people or one it is on only through
    /// channels, and an org-wide team only as an owner.
    #[must_use]
    pub fn can_leave(&self) -> bool {
        let about = &self.about;
        !about.dynamic_membership && !about.channel_only && (self.owner || !about.tenant_wide)
    }

    /// Whether the account can archive, restore, delete and edit it: an owner of the whole team.
    #[must_use]
    pub fn can_administer(&self) -> bool {
        self.owner && !self.about.channel_only
    }

    /// How many channels it has hidden for the account.
    #[must_use]
    pub fn hidden_channels(&self) -> usize {
        self.channels.iter().filter(|channel| !channel.shown).count()
    }
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
    pub kind: ChannelKind,
    /// Whether it is in the account's channel list.
    pub shown: bool,
}

/// Who a channel is for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// Everyone on the team.
    #[default]
    Standard,
    /// Specific people on the team.
    Private,
    /// People or teams it was shared with.
    Shared,
}

impl ChannelKind {
    /// How the store keeps it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Private => "private",
            Self::Shared => "shared",
        }
    }

    /// The kind a stored key names; a key nothing wrote reads as standard.
    #[must_use]
    pub fn of_key(key: &str) -> Self {
        match key {
            "private" => Self::Private,
            "shared" => Self::Shared,
            _ => Self::Standard,
        }
    }
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
            let mut team_row = tx.prepare_cached(
                "INSERT INTO teams (id, name, archived, position, group_id, mailbox, owner, can_add_channel,
                 can_add_private_channel, tenant, description, public, people, guest, dynamic_membership, tenant_wide,
                 channel_only, hidden, picture_etag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            )?;
            let mut channel_row = tx.prepare_cached(
                "INSERT INTO team_channels (id, team_id, name, general, position, kind, shown)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for (position, team) in (0_i64..).zip(&teams) {
                team_row.execute(params![
                    team.id,
                    team.name,
                    team.archived,
                    position,
                    team.group_id,
                    team.mailbox,
                    team.owner,
                    team.can_add_channel,
                    team.can_add_private_channel,
                    team.about.tenant,
                    team.about.description,
                    team.about.public,
                    team.about.people,
                    team.about.guest,
                    team.about.dynamic_membership,
                    team.about.tenant_wide,
                    team.about.channel_only,
                    team.about.hidden,
                    team.about.picture_etag
                ])?;
                for (place, channel) in (0_i64..).zip(&team.channels) {
                    channel_row.execute(params![
                        channel.id,
                        team.id,
                        channel.name,
                        channel.general,
                        place,
                        channel.kind.key(),
                        channel.shown
                    ])?;
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
            .prepare_cached(
                "SELECT id, name, archived, group_id, mailbox, owner, can_add_channel, can_add_private_channel,
                 tenant, description, public, people, guest, dynamic_membership, tenant_wide, channel_only, hidden, picture_etag
                 FROM teams ORDER BY position",
            )?
            .query_map([], |row| {
                Ok(Team {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    archived: row.get(2)?,
                    group_id: row.get(3)?,
                    mailbox: row.get(4)?,
                    owner: row.get(5)?,
                    can_add_channel: row.get(6)?,
                    can_add_private_channel: row.get(7)?,
                    about: TeamAbout {
                        tenant: row.get(8)?,
                        description: row.get(9)?,
                        public: row.get(10)?,
                        people: row.get(11)?,
                        guest: row.get(12)?,
                        dynamic_membership: row.get(13)?,
                        tenant_wide: row.get(14)?,
                        channel_only: row.get(15)?,
                        hidden: row.get(16)?,
                        picture_etag: row.get(17)?,
                    },
                    channels: Vec::new(),
                })
            })?
            .collect::<Result<_, _>>()?;
        let mut channels = self.conn.prepare_cached(
            "SELECT id, name, general, kind, shown FROM team_channels WHERE team_id = ?1 ORDER BY position",
        )?;
        for team in &mut teams {
            team.channels = channels
                .query_map([&team.id], |row| {
                    Ok(TeamChannel {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        general: row.get(2)?,
                        kind: ChannelKind::of_key(&row.get::<_, String>(3)?),
                        shown: row.get(4)?,
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

    /// Replaces the stored favourites (chats and channels) with `favorites` (conversation ids, in the order Teams lists
    /// them; one listed twice keeps its first place), read at `read_at` (ms): [`Applied::Inserted`] for the first list,
    /// [`Applied::Updated`] when it differs from the stored one, [`Applied::Ignored`] when it doesn't (only the read
    /// time moves).
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn replace_favorites(&mut self, favorites: &[String], read_at: i64) -> Result<Applied, StoreError> {
        let mut listed: Vec<String> = Vec::with_capacity(favorites.len());
        for id in favorites {
            if !listed.contains(id) {
                listed.push(id.clone());
            }
        }
        let applied = match self.favorites()? {
            None => Applied::Inserted,
            Some(stored) if stored != listed => Applied::Updated,
            Some(_) => Applied::Ignored,
        };
        let tx = self.conn.transaction()?;
        if applied != Applied::Ignored {
            tx.execute("DELETE FROM favorites", [])?;
            let mut row = tx.prepare_cached("INSERT INTO favorites (position, conversation_id) VALUES (?1, ?2)")?;
            for (position, id) in (0_i64..).zip(&listed) {
                row.execute(params![position, id])?;
            }
        }
        tx.execute(
            "INSERT INTO favorites_read (id, read_at) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET read_at = excluded.read_at",
            [read_at],
        )?;
        tx.commit()?;
        Ok(applied)
    }

    /// The stored favourites' ids (chats and channels) in their order; `None` before a read could read them.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn favorites(&self) -> Result<Option<Vec<String>>, StoreError> {
        let read = self
            .conn
            .prepare_cached("SELECT read_at FROM favorites_read WHERE id = 1")?
            .query_row([], |row| row.get::<_, i64>(0))
            .optional()?;
        if read.is_none() {
            return Ok(None);
        }
        let ids = self
            .conn
            .prepare_cached("SELECT conversation_id FROM favorites ORDER BY position")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(Some(ids))
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
