//! A file's thumbnails, for its card: made from an image file the first time they are asked for, in Graph's three
//! sizes, and kept in the store beside a note of what was made, so every node serves them without decoding again.
//! A file that isn't a picture the `image` crate reads, or is too large to decode, has none.

use std::collections::BTreeMap;
use std::io::Cursor;

use bytes::Bytes;
use image::{DynamicImage, ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};

use super::{Error, drive, repo};
use crate::app::App;

/// Graph's thumbnail sizes: the longest side of each.
pub(super) const SIZES: [(&str, u32); 3] = [("small", 96), ("medium", 176), ("large", 800)];
/// The largest file decoded for its thumbnails.
const MAX_SOURCE: i64 = 32 * 1024 * 1024;
/// The largest picture decoded, by side and by memory.
const MAX_SIDE: u32 = 16_384;
const MAX_DECODED: u64 = 256 * 1024 * 1024;

/// What was made for a file: each size's width and height; none when the file has no thumbnail.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct Made {
    pub sizes: BTreeMap<String, (u32, u32)>,
}

fn note_key(item: &str) -> String {
    format!("drive-thumbnails/{item}/made.json")
}

fn picture_key(item: &str, size: &str) -> String {
    format!("drive-thumbnails/{item}/{size}.jpg")
}

/// The thumbnails of `item`, made now when nobody asked before.
pub(super) async fn of(app: &App, item: &repo::Item) -> Result<Made, Error> {
    if let Some(note) = app.storage.get(&note_key(&item.id)).await? {
        return Ok(serde_json::from_slice(&note).unwrap_or_default());
    }
    let mut made = Made::default();
    if item.size <= MAX_SOURCE
        && let Some(source) = app.storage.get(&drive::key(&item.id)).await?
        && let Ok(Some(pictures)) = tokio::task::spawn_blocking(move || render(&source)).await
    {
        for (size, width, height, bytes) in pictures {
            app.storage.put(&picture_key(&item.id, size), bytes).await?;
            made.sizes.insert(size.to_owned(), (width, height));
        }
    }
    let note = serde_json::to_vec(&made).unwrap_or_default();
    app.storage.put(&note_key(&item.id), note.into()).await?;
    Ok(made)
}

/// One thumbnail's bytes, when the file has it.
pub(super) async fn picture(
    app: &App,
    item: &repo::Item,
    size: &str,
) -> Result<Option<(&'static str, Bytes)>, Error> {
    if !SIZES.iter().any(|(known, _)| *known == size) {
        return Ok(None);
    }
    Ok(app
        .storage
        .get(&picture_key(&item.id, size))
        .await?
        .map(|bytes| ("image/jpeg", bytes)))
}

/// Each size of `source` as a JPEG, never larger than the picture itself; `None` when it isn't a picture.
fn render(source: &[u8]) -> Option<Vec<(&'static str, u32, u32, Bytes)>> {
    let mut reader = ImageReader::new(Cursor::new(source)).with_guessed_format().ok()?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODED);
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let mut made = Vec::new();
    for (size, side) in SIZES {
        let scaled = if picture.width() <= side && picture.height() <= side {
            picture.clone()
        } else {
            picture.thumbnail(side, side)
        };
        let flat = DynamicImage::ImageRgb8(scaled.to_rgb8());
        let mut bytes = Cursor::new(Vec::new());
        flat.write_to(&mut bytes, ImageFormat::Jpeg).ok()?;
        made.push((size, flat.width(), flat.height(), Bytes::from(bytes.into_inner())));
    }
    Some(made)
}
