//! A picture pasted into a message, sent as Teams' web client sends one (`docs/protocol/chat.md`, "Pictures sent"):
//! first an object on the async gateway that only the message's conversation may read ([`CreatePicture`]), then the
//! picture's bytes put into it ([`UploadPicture`]); the message then shows the object's `imgo` view
//! ([`crate::ams::picture_image`]).

use serde_json::json;

use super::{Prepared, RawBody, Wire};
use crate::ams::{PastedFormat, PastedPicture, is_object_id};
use crate::endpoint::{Endpoint, Method, Service};

/// The header the web client's creation carried, naming how the object is shared.
const SHARING_HEADER: &str = "x-ams-post-sharing-mode";

/// How a pasted picture is shared: inline, in its message.
const INLINE: &str = "Inline";

/// The largest picture sent, in bytes: Teams' own limit on a pasted picture isn't known, so this is the largest
/// picture the native client reads back ([`crate::http::MAX_PICTURE_BODY`]).
pub const MAX_SENT_PICTURE: usize = crate::http::MAX_PICTURE_BODY;

/// `POST v1/objects/` on the async gateway: an object for a picture that only `conversation` may read, as the web
/// client's creation was captured (`{"type": "pish/image", "permissions": {"<conversation>": ["read"]},
/// "sharingMode": "Inline", "filename": "MicrosoftTeams-image.png"}`, `x-ams-post-sharing-mode: Inline`). Its answer
/// names the object ([`CreatePicture::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatePicture {
    conversation: String,
    filename: String,
}

impl CreatePicture {
    /// The object for a picture of `format` in `conversation` (a chat's or a channel's thread id; a channel thread's
    /// reply names its channel). `None` for an empty conversation id.
    #[must_use]
    pub fn new(conversation: &str, format: PastedFormat) -> Option<Self> {
        let conversation = crate::stored_conversation(conversation);
        if conversation.is_empty() || conversation.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            conversation: conversation.to_owned(),
            filename: format!("MicrosoftTeams-image.{}", format.extension()),
        })
    }

    /// The conversation that may read the object.
    #[must_use]
    pub fn conversation(&self) -> &str {
        &self.conversation
    }

    /// The object's id an answer of `201` (or any success) gives: `{"id": "<object>"}`, the id one path segment of
    /// ASCII letters, digits, `-`, `_` and `.`.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<String> {
        let answer: serde_json::Value = serde_json::from_slice(body).ok()?;
        let id = answer.get("id")?.as_str()?;
        is_object_id(id).then(|| id.to_owned())
    }
}

impl Prepared for CreatePicture {
    fn wire(&self) -> Option<Wire> {
        let mut permissions = serde_json::Map::new();
        permissions.insert(self.conversation.clone(), json!(["read"]));
        Some(Wire {
            raw: None,
            service: Service::Ams,
            method: Method::Post,
            endpoint: Endpoint::new(["v1", "objects"]),
            headers: vec![(SHARING_HEADER, INLINE.to_owned())],
            body: Some(json!({
                "type": "pish/image",
                "permissions": permissions,
                "sharingMode": INLINE,
                "filename": self.filename,
            })),
        })
    }
}

/// `PUT v1/objects/<id>/content/imgpsh` on the async gateway: the picture's bytes, `application/octet-stream`, into the
/// object [`CreatePicture`] made, as the web client sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadPicture {
    object: String,
    bytes: RawBody,
}

impl UploadPicture {
    /// `picture` into `object`; `None` for an id that isn't one, or a picture larger than [`MAX_SENT_PICTURE`].
    #[must_use]
    pub fn new(object: &str, picture: &PastedPicture) -> Option<Self> {
        (is_object_id(object) && !picture.bytes.is_empty() && picture.bytes.len() <= MAX_SENT_PICTURE).then(|| Self {
            object: object.to_owned(),
            bytes: picture.bytes.clone().into(),
        })
    }

    /// The object it fills.
    #[must_use]
    pub fn object(&self) -> &str {
        &self.object
    }
}

impl Prepared for UploadPicture {
    fn wire(&self) -> Option<Wire> {
        Some(Wire {
            raw: Some(self.bytes.clone()),
            service: Service::Ams,
            method: Method::Put,
            endpoint: Endpoint::new(["v1", "objects", self.object.as_str(), "content", "imgpsh"]),
            headers: vec![("content-type", "application/octet-stream".to_owned())],
            body: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png() -> PastedPicture {
        PastedPicture {
            format: PastedFormat::Png,
            bytes: b"\x89PNG\r\n\x1a\nmade-up".to_vec(),
        }
    }

    #[test]
    fn the_creation_is_the_captured_body_for_the_conversation_alone() {
        let create = CreatePicture::new("19:made-up-a_made-up-b@unq.gbl.spaces", PastedFormat::Png).unwrap();
        let wire = create.wire().unwrap();
        assert_eq!(wire.service(), Service::Ams);
        assert_eq!(wire.method(), Method::Post);
        assert_eq!(wire.endpoint().segments(), ["v1", "objects"]);
        assert_eq!(wire.headers(), [(SHARING_HEADER, "Inline".to_owned())]);
        assert_eq!(
            wire.body(),
            Some(&json!({
                "type": "pish/image",
                "permissions": {"19:made-up-a_made-up-b@unq.gbl.spaces": ["read"]},
                "sharingMode": "Inline",
                "filename": "MicrosoftTeams-image.png",
            }))
        );
        let reply = CreatePicture::new("19:made-up@thread.tacv2;messageid=1", PastedFormat::Jpeg).unwrap();
        assert_eq!(
            reply.conversation(),
            "19:made-up@thread.tacv2",
            "a thread's reply: its channel"
        );
        assert!(CreatePicture::new("", PastedFormat::Png).is_none());
    }

    #[test]
    fn the_answer_names_one_object_or_nothing() {
        assert_eq!(
            CreatePicture::read_answer(br#"{"id":"0-eus-d15-00000000000000000000000000000000"}"#).as_deref(),
            Some("0-eus-d15-00000000000000000000000000000000")
        );
        for body in [
            &br#"{"id":"../x"}"#[..],
            br#"{"id":""}"#,
            b"{}",
            b"not json",
            br#"{"id":"a/b"}"#,
        ] {
            assert_eq!(CreatePicture::read_answer(body), None);
        }
    }

    #[test]
    fn the_upload_puts_the_bytes_into_the_object_alone() {
        let upload = UploadPicture::new("0-eus-d15-abc", &png()).unwrap();
        let wire = upload.wire().unwrap();
        assert_eq!(wire.method(), Method::Put);
        assert_eq!(
            wire.endpoint().segments(),
            ["v1", "objects", "0-eus-d15-abc", "content", "imgpsh"]
        );
        assert_eq!(wire.body_bytes(), Some(png().bytes));
        assert_eq!(
            wire.headers(),
            [("content-type", "application/octet-stream".to_owned())]
        );
        assert!(UploadPicture::new("../x", &png()).is_none());
        let empty = PastedPicture {
            format: PastedFormat::Png,
            bytes: Vec::new(),
        };
        assert!(UploadPicture::new("0-eus-d15-abc", &empty).is_none());
    }
}
