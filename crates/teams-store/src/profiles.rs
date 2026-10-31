//! People's names and pictures as the account last read them (M2 Task 12), by MRI: what the core read from Graph or
//! the middle tier's profile lookup, kept so that the chat list and the chat view name people without reading again,
//! and read again by age (the core's decision). A row is replaced whole: what to keep of the last one when a read
//! failed is the core's to decide.

use std::fmt;

use rusqlite::{OptionalExtension, Row, params};

use crate::{Store, StoreError};

/// Where a stored name came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NameSource {
    /// Microsoft Graph's user record.
    Graph,
    /// The middle tier's profile lookup (`users/fetchShortProfile`).
    ShortProfile,
}

impl NameSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Graph => "graph",
            Self::ShortProfile => "short_profile",
        }
    }

    /// The source a stored value names; `None` for one this build doesn't know.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "graph" => Some(Self::Graph),
            "short_profile" => Some(Self::ShortProfile),
            _ => None,
        }
    }
}

/// A stored picture: its media type and its bytes, undecoded. `Debug` shows the type and the size.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredPicture {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for StoredPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredPicture")
            .field("media_type", &self.media_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Whether a read settled someone's picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PictureState {
    /// No read settled it yet.
    Unread,
    /// The account has none.
    None,
    Some(StoredPicture),
}

/// Someone's name and picture as last read. `Debug` leaves the name out.
#[derive(Clone, PartialEq, Eq)]
pub struct Profile {
    pub mri: String,
    /// The name a read gave; `None` when none did.
    pub name: Option<String>,
    /// Where `name` came from; `None` with no name, or a source this build doesn't know.
    pub name_source: Option<NameSource>,
    pub picture: PictureState,
    /// When the last read that settled the name and the picture finished, in milliseconds; 0 while none has.
    pub read_at: i64,
}

impl fmt::Debug for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Profile")
            .field("mri", &self.mri)
            .field("named", &self.name.is_some())
            .field("name_source", &self.name_source)
            .field("picture", &self.picture)
            .field("read_at", &self.read_at)
            .finish()
    }
}

impl Store {
    /// Someone's stored profile.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn profile(&self, mri: &str) -> Result<Option<Profile>, StoreError> {
        let profile = self
            .conn
            .prepare_cached(
                "SELECT mri, name, name_source, picture_state, picture_type, picture, read_at FROM profiles
                 WHERE mri = ?1",
            )?
            .query_row([mri], profile_of)
            .optional()?;
        Ok(profile)
    }

    /// Up to `limit` people whose stored name holds `query` (in any case, any script's: compared lowercased in Rust,
    /// as SQLite's own `lower` folds only ASCII), as (MRI, name), names that begin with it first, then by name: what
    /// the scheduling form's attendee picker suggests.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the read fails.
    pub fn profiles_named(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>, StoreError> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self
            .conn
            .prepare_cached("SELECT mri, name FROM profiles WHERE name IS NOT NULL")?;
        let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut found: Vec<(bool, String, (String, String))> = Vec::new();
        for row in rows {
            let (mri, name) = row?;
            let lower = name.to_lowercase();
            if lower.contains(&query) {
                found.push((!lower.starts_with(&query), lower, (mri, name)));
            }
        }
        found.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        Ok(found.into_iter().take(limit).map(|(_, _, person)| person).collect())
    }

    /// Stores `profile` in place of the one stored for its MRI.
    ///
    /// # Errors
    ///
    /// [`StoreError::Sqlite`] when the write fails.
    pub fn put_profile(&mut self, profile: &Profile) -> Result<(), StoreError> {
        let (state, media_type, bytes) = match &profile.picture {
            PictureState::Unread => ("unread", None, None),
            PictureState::None => ("none", None, None),
            PictureState::Some(picture) => ("some", Some(picture.media_type.as_str()), Some(&picture.bytes[..])),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO profiles (mri, name, name_source, picture_state, picture_type, picture, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (mri) DO UPDATE SET name = excluded.name, name_source = excluded.name_source,
                     picture_state = excluded.picture_state, picture_type = excluded.picture_type,
                     picture = excluded.picture, read_at = excluded.read_at",
            )?
            .execute(params![
                profile.mri,
                profile.name,
                profile.name_source.map(NameSource::as_str),
                state,
                media_type,
                bytes,
                profile.read_at,
            ])?;
        Ok(())
    }
}

fn profile_of(row: &Row<'_>) -> rusqlite::Result<Profile> {
    let source: Option<String> = row.get(2)?;
    let state: String = row.get(3)?;
    let media_type: Option<String> = row.get(4)?;
    let bytes: Option<Vec<u8>> = row.get(5)?;
    let picture = match (state.as_str(), media_type, bytes) {
        ("some", Some(media_type), Some(bytes)) => PictureState::Some(StoredPicture { media_type, bytes }),
        ("none", ..) => PictureState::None,
        // never read, or a state this build doesn't know: read it again
        _ => PictureState::Unread,
    };
    Ok(Profile {
        mri: row.get(0)?,
        name: row.get(1)?,
        name_source: source.as_deref().and_then(NameSource::parse),
        picture,
        read_at: row.get(6)?,
    })
}
