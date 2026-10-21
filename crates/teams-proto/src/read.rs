//! Reads: requests that change nothing anyone could notice, always sent as `GET`.
//!
//! Besides the chat service's reads (in [`crate::chat`]), the reads of Microsoft Graph a person's name and picture come
//! from (M2 Task 12): [`GraphUser`] and [`GraphPicture`], in the shapes `proto-lab probe profile` read live
//! (`docs/protocol/graph.md`). Graph is our own reach, not the web client's (it names people through the middle
//! tier's profile lookups, `crate::ShortProfileLookup`), so the probe's answers are the reviewed shape; the transport
//! takes no other Graph endpoint ([`Service::Graph`]).

use std::fmt;

use serde::Deserialize;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};

/// A read. It names only its service and its endpoint under that service's base; the transport always sends it as a
/// `GET`, with no body. So any type may implement it: a read can't become a request of another method, can't carry a
/// body and can't name a host.
pub trait Read {
    fn service(&self) -> Service;
    fn endpoint(&self) -> Endpoint;
}

/// The `$select` of a Graph user read: the record's id and the name it shows, nothing else of the person's record.
pub const GRAPH_USER_SELECT: &str = "id,displayName";

/// The picture size read: 96×96, what an avatar needs (the probe's 96×96 pictures were about 7 KB, the largest size
/// about 230 KB).
pub const GRAPH_PICTURE_SIZE: &str = "96x96";

/// The largest picture kept, in bytes; a longer answer is [`GraphAnswerError::TooLarge`].
pub const MAX_PICTURE: usize = 256 * 1024;

/// The object id an MRI `8:orgid:<object id>` names, when it is a GUID in its hyphenated form; `None` for any other
/// MRI (a bot's, a phone number's, an unknown form).
#[must_use]
pub fn object_id(mri: &str) -> Option<&str> {
    mri.strip_prefix("8:orgid:").filter(|id| is_guid(id))
}

/// A GUID in its hyphenated form, any case.
pub(crate) fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// `GET https://graph.microsoft.com/v1.0/users/<object id>?$select=id,displayName`: the name Graph shows for one
/// account, by its object id. Its answer is read with [`GraphUser::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct GraphUser {
    id: String,
}

impl GraphUser {
    /// The read of the account `mri` names; `None` unless it is `8:orgid:<object id>` ([`object_id`]).
    #[must_use]
    pub fn of(mri: &str) -> Option<Self> {
        object_id(mri).map(|id| Self { id: id.to_owned() })
    }

    /// The name the answer `body` gives (trimmed; `None` when it gives none or an empty one).
    ///
    /// # Errors
    /// [`GraphAnswerError::Malformed`] unless the body is a JSON object with a string `id` (and a `displayName` that
    /// is a string or `null`, when there is one); [`GraphAnswerError::OtherUser`] when its `id` isn't the one asked
    /// for (any case).
    pub fn read_answer(&self, body: &[u8]) -> Result<Option<String>, GraphAnswerError> {
        let answer: UserAnswer = serde_json::from_slice(body).map_err(|_| GraphAnswerError::Malformed)?;
        if !answer.id.eq_ignore_ascii_case(&self.id) {
            return Err(GraphAnswerError::OtherUser);
        }
        Ok(answer
            .display_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty()))
    }
}

/// `Debug` leaves the object id out.
impl fmt::Debug for GraphUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphUser").finish_non_exhaustive()
    }
}

impl Read for GraphUser {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", self.id.as_str()]).with_query("$select", GRAPH_USER_SELECT)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserAnswer {
    id: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// `GET https://graph.microsoft.com/v1.0/users/<object id>/photos/96x96/$value`: one account's picture at 96×96, as
/// image bytes. An account without a picture answers 404 (`ImageNotFound`). Its answer is read with
/// [`GraphPicture::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct GraphPicture {
    id: String,
}

impl GraphPicture {
    /// The read of the account `mri` names; `None` unless it is `8:orgid:<object id>` ([`object_id`]).
    #[must_use]
    pub fn of(mri: &str) -> Option<Self> {
        object_id(mri).map(|id| Self { id: id.to_owned() })
    }

    /// The picture the answer gives.
    ///
    /// # Errors
    /// [`GraphAnswerError::UnknownType`] unless its `Content-Type` is `image/jpeg` (what the probe saw) or
    /// `image/png`; [`GraphAnswerError::TooLarge`] over [`MAX_PICTURE`] bytes; [`GraphAnswerError::Malformed`] for an
    /// empty body.
    pub fn read_answer(&self, answer: &Response) -> Result<Picture, GraphAnswerError> {
        let format = answer
            .content_type()
            .and_then(PictureFormat::of)
            .ok_or(GraphAnswerError::UnknownType)?;
        let bytes = answer.body();
        if bytes.len() > MAX_PICTURE {
            return Err(GraphAnswerError::TooLarge);
        }
        if bytes.is_empty() {
            return Err(GraphAnswerError::Malformed);
        }
        Ok(Picture {
            format,
            bytes: bytes.to_vec(),
        })
    }
}

/// `Debug` leaves the object id out.
impl fmt::Debug for GraphPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphPicture").finish_non_exhaustive()
    }
}

impl Read for GraphPicture {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["users", self.id.as_str(), "photos", GRAPH_PICTURE_SIZE, "$value"])
    }
}

/// A picture's image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PictureFormat {
    Jpeg,
    Png,
}

impl PictureFormat {
    /// The format a `Content-Type` names (its parameters and case ignored), if it is one kept.
    #[must_use]
    pub fn of(content_type: &str) -> Option<Self> {
        let media = content_type.split(';').next().unwrap_or_default().trim();
        if media.eq_ignore_ascii_case("image/jpeg") {
            Some(Self::Jpeg)
        } else if media.eq_ignore_ascii_case("image/png") {
            Some(Self::Png)
        } else {
            None
        }
    }

    /// Its media type.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
        }
    }
}

/// A person's picture as Graph gave it: its format and its bytes, undecoded. `Debug` shows the format and the size.
#[derive(Clone, PartialEq, Eq)]
pub struct Picture {
    pub format: PictureFormat,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for Picture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Picture")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Why a Graph answer wasn't read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GraphAnswerError {
    /// Not the probed answer's shape.
    #[error("not a Graph answer of the probed shape")]
    Malformed,
    /// A user record of another account than the one asked for.
    #[error("the answer is of another account than the one asked for")]
    OtherUser,
    /// A picture whose `Content-Type` isn't an image format kept.
    #[error("a picture of a type that isn't kept")]
    UnknownType,
    /// A picture over [`MAX_PICTURE`] bytes.
    #[error("a picture over {MAX_PICTURE} bytes")]
    TooLarge,
}
