//! Pictures messages show, read when a view shows one ([`InlinePicture`]): a picture pasted into a message
//! ([`teams_proto::MessagePicture`]: a `GET` of the picture's view from the account's AMS base, the way the web client
//! reads it), or a GIF picked from Teams' GIF picker ([`teams_proto::GiphyPicture`]: a `GET` of the GIF from Giphy's
//! media host, no token), as [`crate::emoticons`] reads reactions' animations.
//!
//! - **Kept for the run**, in memory, up to [`KEPT_BYTES`] of them (the oldest read let go first: a view keeps what it
//!   decoded, so a picture let go is read again only when a view asks for it anew); a view the service has none of (an
//!   answer that isn't a success, or isn't a picture) is kept as missing. A read that failed on the way isn't kept and
//!   is answered [`EngineError::PictureUnread`], so the next ask reads again.
//! - **One read per picture at a time**: asks while its read runs wait for that read.
//! - A `src` that names neither ([`InlinePicture::of`]) is answered `None` with nothing read.
//! - Nothing is stored and nothing is written.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::rc::Rc;
use std::sync::Arc;

use teams_proto::{Client, Credentials, HttpError, HttpTransport, InlinePicture, PastedPicture};

use crate::engine::{EngineError, Responder};

/// A picture's bytes and their format, shared with the views.
pub type PictureBytes = Arc<PastedPicture>;

/// How many bytes of pictures are kept at most.
pub const KEPT_BYTES: usize = 64 * 1024 * 1024;

/// What is known of one picture.
pub(crate) enum Known {
    /// Read: its bytes, or `None` when the service has none.
    Read(Option<PictureBytes>),
    /// Its read runs; these wait for it.
    Reading(Vec<Responder<Option<PictureBytes>>>),
}

/// The pictures kept, by what names each (a message's AMS view or GIF; a shared file, [`crate::thumbnails`]).
pub(crate) struct Kept<K> {
    pub(crate) known: HashMap<K, Known>,
    /// The pictures read, oldest first, and their bytes in all.
    order: VecDeque<K>,
    bytes: usize,
}

impl<K> Default for Kept<K> {
    fn default() -> Self {
        Self {
            known: HashMap::new(),
            order: VecDeque::new(),
            bytes: 0,
        }
    }
}

impl<K: Clone + Eq + Hash> Kept<K> {
    /// Keeps `picture`'s answer, letting the oldest go while more than [`KEPT_BYTES`] are kept; the asks it had waiting.
    pub(crate) fn keep(&mut self, picture: K, answer: Option<PictureBytes>) -> Option<Known> {
        self.bytes += answer.as_ref().map_or(0, |bytes| bytes.bytes.len());
        self.order.push_back(picture.clone());
        let waiting = self.known.insert(picture, Known::Read(answer));
        while self.bytes > KEPT_BYTES {
            let Some(oldest) = self.order.pop_front() else { break };
            // a picture read again since keeps its asks
            if !matches!(self.known.get(&oldest), Some(Known::Read(_))) {
                continue;
            }
            if let Some(Known::Read(Some(bytes))) = self.known.remove(&oldest) {
                self.bytes = self.bytes.saturating_sub(bytes.bytes.len());
            }
        }
        waiting
    }
}

pub(crate) struct Pictures<C> {
    client: Arc<Client<HttpTransport<C>>>,
    kept: Rc<RefCell<Kept<InlinePicture>>>,
}

impl<C: Credentials + 'static> Pictures<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self {
            client,
            kept: Rc::default(),
        }
    }

    /// Answers `reply` with the picture `src` names: at once when it is known, else once its read is over (the read
    /// started now unless one runs). A `src` no read can name is answered `None`.
    pub(crate) fn picture(&self, src: &str, reply: Responder<Option<PictureBytes>>) {
        let Some(picture) = InlinePicture::of(src) else {
            let _ = reply.send(Ok(None));
            return;
        };
        let mut kept = self.kept.borrow_mut();
        match kept.known.get_mut(&picture) {
            Some(Known::Read(bytes)) => {
                let _ = reply.send(Ok(bytes.clone()));
            }
            Some(Known::Reading(waiting)) => waiting.push(reply),
            None => {
                kept.known.insert(picture.clone(), Known::Reading(vec![reply]));
                // on the engine's `LocalSet`: the read waits on nothing of the engine's loop
                tokio::task::spawn_local(read_picture(Arc::clone(&self.client), picture, Rc::clone(&self.kept)));
            }
        }
    }
}

/// Reads `picture` and answers every ask that waits for it; keeps it, or that there is none, unless the read failed on
/// the way ([`EngineError::PictureUnread`]).
async fn read_picture<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    picture: InlinePicture,
    kept: Rc<RefCell<Kept<InlinePicture>>>,
) {
    let answer = match client.read(&picture).await {
        Ok(answer) => Some(picture.read_answer(&answer).map(Arc::new)),
        Err(HttpError::Status(_) | HttpError::BodyTooLarge) => Some(None),
        Err(error) => {
            log::info!("a message's picture couldn't be read: {error}");
            None
        }
    };
    let waiting = match answer.clone() {
        Some(bytes) => kept.borrow_mut().keep(picture, bytes),
        None => kept.borrow_mut().known.remove(&picture),
    };
    let Some(Known::Reading(waiting)) = waiting else { return };
    for reply in waiting {
        let _ = reply.send(answer.clone().ok_or(EngineError::PictureUnread));
    }
}

#[cfg(test)]
mod tests {
    use teams_proto::PastedFormat;

    use super::*;

    fn view(n: usize) -> InlinePicture {
        InlinePicture::of(&format!(
            "https://us-api.asm.skype.com/v1/objects/0-eus-d9-{n:032x}/views/imgo"
        ))
        .unwrap()
    }

    fn bytes(len: usize) -> PictureBytes {
        Arc::new(PastedPicture {
            format: PastedFormat::Png,
            bytes: vec![0; len],
        })
    }

    #[test]
    fn the_oldest_pictures_are_let_go_past_the_bytes_kept() {
        let mut kept = Kept::default();
        let third = KEPT_BYTES / 3 + 1;
        for n in 0..3 {
            kept.keep(view(n), Some(bytes(third)));
        }
        assert!(!kept.known.contains_key(&view(0)), "the oldest let go");
        assert!(kept.known.contains_key(&view(1)) && kept.known.contains_key(&view(2)));
        assert_eq!(kept.bytes, 2 * third);
        // a view with none costs nothing, and is kept
        kept.keep(view(9), None);
        assert!(matches!(kept.known.get(&view(9)), Some(Known::Read(None))));
        assert_eq!(kept.bytes, 2 * third);
    }
}
