//! The bytes of the files messages carry, read part by part when a view plays a video inline: the file's item from
//! Microsoft Graph by its sharing link ([`FileDownload`], its size and download link), then the parts asked for at
//! that link ([`HttpTransport::file_range`], `Range`, no token), as `docs/protocol/graph.md` records.
//!
//! - **The download link** is kept for the run per file, in memory, for [`LINK_LIFE`] (Graph's links last about an
//!   hour); an older one is asked for again, and so is one the file's host refused (a 401 or 403: it expired), once
//!   per part. One read of a file's item at a time: asks while it runs wait for it.
//! - **A part** is read when asked, never kept here (the player keeps what it read); at most
//!   [`teams_proto::MAX_RANGE`] bytes of it.
//! - A file the account can't read (no item, no download link, a 4xx answer from Graph or from the file's host but the
//!   refused link's one 401 or 403, a part that isn't the one asked for), or a link that isn't a SharePoint sharing
//!   link ([`FileDownload::of`]), is [`EngineError::FileUnavailable`]. A read that failed on the way (the network, a
//!   5xx or a throttle still there after the transport's one retry) is [`EngineError::FileUnread`], and the next ask
//!   reads again.
//! - Nothing is stored and nothing is written.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teams_proto::{Client, Credentials, FileContent, FileDownload, FileRange, HttpError, HttpTransport};
use tokio::sync::oneshot;

use crate::engine::{EngineError, Responder};

/// How long a download link Graph handed out is used before it is asked for again.
pub const LINK_LIFE: Duration = Duration::from_mins(40);

/// Why a file's item gave no link: it can't be had, or the read failed on the way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkFailure {
    Unavailable,
    Unread,
}

impl From<LinkFailure> for EngineError {
    fn from(failure: LinkFailure) -> Self {
        match failure {
            LinkFailure::Unavailable => Self::FileUnavailable,
            LinkFailure::Unread => Self::FileUnread,
        }
    }
}

type LinkAnswer = Result<FileContent, LinkFailure>;

/// A file's download link: known since when, or being read for those waiting.
enum Link {
    Known(FileContent, Instant),
    Reading(Vec<oneshot::Sender<LinkAnswer>>),
}

type Links = Rc<RefCell<HashMap<FileDownload, Link>>>;

pub(crate) struct FileContents<C> {
    client: Arc<Client<HttpTransport<C>>>,
    links: Links,
}

impl<C: Credentials + 'static> FileContents<C> {
    pub(crate) fn new(client: Arc<Client<HttpTransport<C>>>) -> Self {
        Self {
            client,
            links: Rc::default(),
        }
    }

    /// Answers `reply` with `len` bytes of the file `share_url` is the sharing link of, from `offset`, once read.
    pub(crate) fn range(&self, share_url: &str, offset: u64, len: u32, reply: Responder<FileRange>) {
        let Some(file) = FileDownload::of(share_url) else {
            let _ = reply.send(Err(EngineError::FileUnavailable));
            return;
        };
        // on the engine's `LocalSet`: the reads wait on nothing of the engine's loop
        let (client, links) = (Arc::clone(&self.client), Rc::clone(&self.links));
        tokio::task::spawn_local(async move {
            let _ = reply.send(read_range(&client, &links, &file, offset, len).await);
        });
    }
}

/// The part of `file` asked for: at the link known, else one read now; a link the host refused is read again once.
async fn read_range<C: Credentials + 'static>(
    client: &Arc<Client<HttpTransport<C>>>,
    links: &Links,
    file: &FileDownload,
    offset: u64,
    len: u32,
) -> Result<FileRange, EngineError> {
    let mut fresh = false;
    loop {
        let content = link(client, links, file).await?;
        match client.transport().file_range(&content.link, offset, len).await {
            Ok(answer) => return FileRange::read(answer, offset, len).ok_or(EngineError::FileUnavailable),
            Err(HttpError::Status(answer)) if matches!(answer.status(), 401 | 403) && !fresh => {
                // the link expired: forget it (unless another ask replaced it meanwhile) and ask Graph again
                let mut known = links.borrow_mut();
                if let Some(Link::Known(kept, _)) = known.get(file)
                    && *kept == content
                {
                    known.remove(file);
                }
                fresh = true;
            }
            Err(HttpError::Status(answer)) if !gone(answer.status()) => {
                log::info!("a part of a shared file couldn't be read: HTTP {}", answer.status());
                return Err(EngineError::FileUnread);
            }
            Err(HttpError::Status(_) | HttpError::BodyTooLarge) => return Err(EngineError::FileUnavailable),
            Err(error) => {
                log::info!("a part of a shared file couldn't be read: {error}");
                return Err(EngineError::FileUnread);
            }
        }
    }
}

/// `file`'s download link: the one known while it is younger than [`LINK_LIFE`], else Graph's answer, read once for
/// every ask that waits for it.
async fn link<C: Credentials + 'static>(
    client: &Arc<Client<HttpTransport<C>>>,
    links: &Links,
    file: &FileDownload,
) -> Result<FileContent, EngineError> {
    let waiting = {
        let mut known = links.borrow_mut();
        match known.get_mut(file) {
            Some(Link::Known(content, at)) if at.elapsed() < LINK_LIFE => return Ok(content.clone()),
            Some(Link::Reading(waiting)) => {
                let (tell, told) = oneshot::channel();
                waiting.push(tell);
                Some(told)
            }
            Some(Link::Known(..)) | None => {
                known.insert(file.clone(), Link::Reading(Vec::new()));
                None
            }
        }
    };
    if let Some(told) = waiting {
        return Ok(told.await.unwrap_or(Err(LinkFailure::Unread))?);
    }
    let answer = read_link(client, file).await;
    let waiting = match &answer {
        Ok(content) => links
            .borrow_mut()
            .insert(file.clone(), Link::Known(content.clone(), Instant::now())),
        Err(_) => links.borrow_mut().remove(file),
    };
    if let Some(Link::Reading(waiting)) = waiting {
        for tell in waiting {
            let _ = tell.send(answer.clone());
        }
    }
    Ok(answer?)
}

/// Whether an answer's `status` says the file can't be had: a 4xx but a throttle (`429`); a 5xx may pass.
fn gone(status: u16) -> bool {
    (400..500).contains(&status) && status != 429
}

/// Graph's item of `file`, read once.
async fn read_link<C: Credentials + 'static>(client: &Client<HttpTransport<C>>, file: &FileDownload) -> LinkAnswer {
    match client.read(file).await {
        Ok(answer) => file.read_answer(&answer).ok_or(LinkFailure::Unavailable),
        Err(HttpError::Status(answer)) if !gone(answer.status()) => {
            log::info!("a shared file's item couldn't be read: HTTP {}", answer.status());
            Err(LinkFailure::Unread)
        }
        Err(HttpError::Status(_) | HttpError::BodyTooLarge) => Err(LinkFailure::Unavailable),
        Err(error) => {
            log::info!("a shared file's item couldn't be read: {error}");
            Err(LinkFailure::Unread)
        }
    }
}
