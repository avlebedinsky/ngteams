//! Thumbnails of the files messages carry, read when a view shows a file's card: the file's thumbnail sets from
//! Microsoft Graph by its sharing link ([`FileThumbnails`]), then the picture at the link Graph handed out
//! ([`HttpTransport::file_thumbnail`], no token), as `docs/protocol/graph.md` records.
//!
//! - **Kept for the run**, in memory, beside the messages' pictures and as they are ([`crate::pictures`]): up to
//!   `pictures::KEPT_BYTES` of them, the oldest let go first; a file Graph lists no thumbnail of, can't be read by the account
//!   (an answer that isn't a success) or whose picture isn't one is kept as missing. A read that failed on the way
//!   isn't kept and is answered [`EngineError::PictureUnread`], so the next ask reads again.
//! - **One read per file at a time**: asks while its reads run wait for them.
//! - A link that isn't a SharePoint sharing link ([`FileThumbnails::of`]) is answered `None` with nothing read.
//! - Nothing is stored and nothing is written.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use teams_proto::{Client, Credentials, FileThumbnails, HttpError, HttpTransport, PastedFormat, PastedPicture};

use crate::engine::{EngineError, Responder};
use crate::pictures::{Kept, Known, PictureBytes};

pub(crate) struct Thumbnails<C> {
    client: Arc<Client<HttpTransport<C>>>,
    kept: Rc<RefCell<Kept<FileThumbnails>>>,
}

impl<C: Credentials + 'static> Thumbnails<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self {
            client,
            kept: Rc::default(),
        }
    }

    /// Answers `reply` with the thumbnail of the file `share_url` is the sharing link of: at once when it is known,
    /// else once its reads are over (started now unless they run). A link no read can name is answered `None`.
    pub(crate) fn thumbnail(&self, share_url: &str, reply: Responder<Option<PictureBytes>>) {
        let Some(file) = FileThumbnails::of(share_url) else {
            let _ = reply.send(Ok(None));
            return;
        };
        let mut kept = self.kept.borrow_mut();
        match kept.known.get_mut(&file) {
            Some(Known::Read(bytes)) => {
                let _ = reply.send(Ok(bytes.clone()));
            }
            Some(Known::Reading(waiting)) => waiting.push(reply),
            None => {
                kept.known.insert(file.clone(), Known::Reading(vec![reply]));
                // on the engine's `LocalSet`: the reads wait on nothing of the engine's loop
                tokio::task::spawn_local(read_thumbnail(Arc::clone(&self.client), file, Rc::clone(&self.kept)));
            }
        }
    }
}

/// The thumbnail of `file`: `Ok(None)` when there is none to have, `Err` when a read failed on the way.
async fn thumbnail_of<C: Credentials + 'static>(
    client: &Client<HttpTransport<C>>,
    file: &FileThumbnails,
) -> Result<Option<PictureBytes>, HttpError> {
    let listed = match client.read(file).await {
        Ok(answer) => answer,
        Err(HttpError::Status(_) | HttpError::BodyTooLarge) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(link) = file.read_answer(&listed) else {
        return Ok(None);
    };
    let answer = match client.transport().file_thumbnail(&link).await {
        Ok(answer) => answer,
        Err(HttpError::Status(_) | HttpError::BodyTooLarge) => return Ok(None),
        Err(error) => return Err(error),
    };
    let bytes = answer.body();
    Ok(PastedFormat::sniff(bytes).map(|format| {
        Arc::new(PastedPicture {
            format,
            bytes: bytes.to_vec(),
        })
    }))
}

/// Reads `file`'s thumbnail and answers every ask that waits for it; keeps it, or that there is none, unless a read
/// failed on the way ([`EngineError::PictureUnread`]).
async fn read_thumbnail<C: Credentials + 'static>(
    client: Arc<Client<HttpTransport<C>>>,
    file: FileThumbnails,
    kept: Rc<RefCell<Kept<FileThumbnails>>>,
) {
    let answer = match thumbnail_of(&client, &file).await {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            log::info!("a file's thumbnail couldn't be read: {error}");
            None
        }
    };
    let waiting = match answer.clone() {
        Some(bytes) => kept.borrow_mut().keep(file, bytes),
        None => kept.borrow_mut().known.remove(&file),
    };
    let Some(Known::Reading(waiting)) = waiting else { return };
    for reply in waiting {
        let _ = reply.send(answer.clone().ok_or(EngineError::PictureUnread));
    }
}
