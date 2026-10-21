//! People as the views show them (M2 Task 12): a name and a picture, both from the engine
//! ([`Core::person`](crate::Core::person): their profile's name (Graph's, else the middle tier's profile lookup's),
//! else their newest message's, else one the binary knew) and their picture's bytes, decoded by GPUI only once a view
//! draws them. Without a picture an avatar shows the initials of a name; without a name, a blank one.
//!
//! A picture reaches a view only as bytes from the engine ([`Shown::of`]): nothing here names a URL, and GPUI's HTTP
//! client stays the null one. A [`Picture`]'s `Debug` shows its format and size, never its bytes.

use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

use gpui::{Image, ImageFormat, SharedString};
use gpui_component::avatar::Avatar;
use gpui_component::{Sizable as _, Size};
use teams_core::{NameFrom, Person};
use teams_store::StoredPicture;

/// Someone as a view shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The name to show; `None` when nothing gave one (a view shows the MRI).
    pub name: Option<SharedString>,
    /// Where the name came from.
    pub from: Option<NameFrom>,
    /// Their picture, from the engine's bytes; `None` when they have none (or it wasn't read yet).
    pub picture: Option<Picture>,
}

impl Shown {
    /// `person` as a view shows them; a picture of a format GPUI can't be handed is none.
    #[must_use]
    pub fn of(person: Person) -> Self {
        Self {
            name: person.name.map(SharedString::from),
            from: person.from,
            picture: person.picture.as_ref().and_then(image_of),
        }
    }

    /// Whether the name came from a profile (Graph's or the lookup's), which a message's own name doesn't override.
    #[must_use]
    pub fn profiled(&self) -> bool {
        matches!(self.from, Some(NameFrom::Graph | NameFrom::ShortProfile))
    }
}

/// What an avatar beside a name shows: a picture, else the initials of the name when it is one (`named`: not an
/// MRI), else nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Face {
    pub named: bool,
    pub picture: Option<Picture>,
}

impl Face {
    /// The avatar of `size` beside `name`.
    #[must_use]
    pub fn avatar(&self, name: &SharedString, size: Size) -> Avatar {
        avatar(self.named.then_some(name), self.picture.as_ref(), size)
    }
}

/// A picture as a view holds it, for GPUI to decode when it is drawn. Its `Debug` shows only its format and size, as
/// the engine's [`StoredPicture`] does: GPUI's own [`Image`] would print every byte.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture(Arc<Image>);

impl Picture {
    /// The image GPUI is handed.
    #[must_use]
    pub fn image(&self) -> &Arc<Image> {
        &self.0
    }
}

impl Deref for Picture {
    type Target = Image;

    fn deref(&self) -> &Image {
        &self.0
    }
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("format", &self.0.format)
            .field("bytes", &self.0.bytes.len())
            .finish()
    }
}

/// The picture of a stored one: JPEG and PNG, the formats the engine keeps.
#[must_use]
pub fn image_of(picture: &StoredPicture) -> Option<Picture> {
    let format = match picture.media_type.as_str() {
        "image/jpeg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        _ => return None,
    };
    Some(Picture(Arc::new(Image::from_bytes(format, picture.bytes.clone()))))
}

/// An avatar of `size`: the picture when there is one, else the initials of `name`, else a blank one.
#[must_use]
pub fn avatar(name: Option<&SharedString>, picture: Option<&Picture>, size: Size) -> Avatar {
    let mut avatar = Avatar::new().with_size(size);
    if let Some(name) = name {
        avatar = avatar.name(name.clone());
    }
    if let Some(picture) = picture {
        avatar = avatar.src(Arc::clone(picture.image()));
    }
    avatar
}
