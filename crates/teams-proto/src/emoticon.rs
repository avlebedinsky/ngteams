//! A reaction's animation as Teams draws it under the pointer (`docs/protocol/chat.md`, "Reaction pictures"): `GET
//! https://statics.teams.cdn.office.net/evergreen-assets/personal-expressions/v2/assets/emoticons/<id>/default/50_anim_f.png`
//! ([`ReactionAnimation`], a read on [`Service::Emoticons`], no token), a PNG of the animation's 50×50 frames stacked
//! top to bottom. The web client reads it from the same path for its picker.

use std::fmt;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The largest animation kept, in bytes (the 50 px strips seen were 5–60 KB).
pub const MAX_ANIMATION: usize = 512 * 1024;
/// The strip read: 50 px frames, the size the web client's picker draws.
const STRIP: &str = "50_anim_f.png";

/// `GET …/emoticons/<id>/default/50_anim_f.png`: a reaction's animation strip, by its id (the reaction's key without a
/// skin tone). Its answer is read with [`ReactionAnimation::read_answer`].
#[derive(Clone, PartialEq, Eq)]
pub struct ReactionAnimation {
    id: String,
}

impl ReactionAnimation {
    /// The read of the reaction `id`; `None` unless it is in the form Teams' reaction ids take (lower-case ASCII
    /// letters, digits and `_`, at most 64 of them), so it is always one path segment of a picture.
    #[must_use]
    pub fn of(id: &str) -> Option<Self> {
        let plain = !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
        plain.then(|| Self { id: id.to_owned() })
    }

    /// The strip the answer gives: its bytes when it is a PNG of at most [`MAX_ANIMATION`] bytes, else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<Vec<u8>> {
        let png = answer.content_type().is_some_and(|content_type| {
            content_type
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("image/png"))
        });
        let bytes = answer.body();
        (png && !bytes.is_empty() && bytes.len() <= MAX_ANIMATION).then(|| bytes.to_vec())
    }
}

impl fmt::Debug for ReactionAnimation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReactionAnimation").field("id", &self.id).finish()
    }
}

impl Read for ReactionAnimation {
    fn service(&self) -> Service {
        Service::Emoticons
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new([self.id.as_str(), "default", STRIP])
    }
}
