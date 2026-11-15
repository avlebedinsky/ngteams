//! An account's picture: what the operator uploads, cut square and kept as a 96 by 96 JPEG in the file store, the
//! size the client draws avatars from (Graph's `photos/96x96/$value`). Each upload has a key of its own, and the
//! profile names the one that is read.

use std::io::Cursor;

use bytes::Bytes;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;

use super::repo;
use crate::db::users;
use crate::db::{Db, DbError};
use crate::storage::{Storage, StorageError};

/// The largest picture taken.
pub const MAX_SOURCE: usize = 4 * 1024 * 1024;
/// The side of the picture kept.
pub const SIDE: u32 = 96;
const QUALITY: u8 = 85;

/// Why a picture wasn't kept.
#[derive(Debug, thiserror::Error)]
pub enum PictureError {
    #[error("the picture is over 4 MiB")]
    TooLarge,
    #[error("the file isn't a PNG, JPEG, GIF or WebP picture")]
    NotAPicture,
    #[error("no such account")]
    NoAccount,
    #[error(transparent)]
    Database(#[from] DbError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// `source` cut to its middle square, at [`SIDE`], as a JPEG.
fn square(source: &[u8]) -> Option<Bytes> {
    let picture = crate::media::decode(source)?;
    let side = picture.width().min(picture.height());
    let (left, top) = ((picture.width() - side) / 2, (picture.height() - side) / 2);
    let cut = picture
        .crop_imm(left, top, side, side)
        .resize_exact(SIDE, SIDE, FilterType::Triangle)
        .to_rgb8();
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(Cursor::new(&mut bytes), QUALITY)
        .encode_image(&cut)
        .ok()?;
    Some(bytes.into())
}

async fn clear(storage: &dyn Storage, key: &str) {
    if let Err(error) = storage.delete(key).await {
        tracing::warn!(%error, key, "a replaced profile picture wasn't cleared");
    }
}

/// Keeps `source` as the picture of the account `id`, in place of its earlier one.
///
/// # Errors
/// See [`PictureError`].
pub async fn set_picture(db: &Db, storage: &dyn Storage, id: &str, source: Bytes) -> Result<(), PictureError> {
    if source.len() > MAX_SOURCE {
        return Err(PictureError::TooLarge);
    }
    users::by_id(db, id)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(PictureError::NoAccount)?;
    let picture = crate::media::decoding(source, square)
        .await
        .ok_or(PictureError::NotAPicture)?;
    let key = format!("profile-pictures/{id}/{}", uuid::Uuid::new_v4().simple());
    let size = i64::try_from(picture.len()).unwrap_or(i64::MAX);
    storage.put(&key, picture).await?;
    let mut profile = repo::profile(db, id).await?;
    let earlier = profile.picture_key.replace(key);
    profile.picture_size = size;
    profile.updated_at = crate::db::now();
    repo::put_profile(db, &profile).await?;
    if let Some(earlier) = earlier {
        clear(storage, &earlier).await;
    }
    Ok(())
}

/// Takes the picture of the account `id` away; whether it had one.
///
/// # Errors
/// A database failure.
pub async fn remove_picture(db: &Db, storage: &dyn Storage, id: &str) -> Result<bool, PictureError> {
    let mut profile = repo::profile(db, id).await?;
    let Some(key) = profile.picture_key.take() else {
        return Ok(false);
    };
    profile.picture_size = 0;
    profile.updated_at = crate::db::now();
    repo::put_profile(db, &profile).await?;
    clear(storage, &key).await;
    Ok(true)
}

/// The picture of the account `id`, when it has one.
pub(crate) async fn picture(db: &Db, storage: &dyn Storage, id: &str) -> Result<Option<Bytes>, PictureError> {
    match repo::profile(db, id).await?.picture_key {
        Some(key) => Ok(storage.get(&key).await?),
        None => Ok(None),
    }
}
