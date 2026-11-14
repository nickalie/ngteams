//! The bytes of the files messages carry, read part by part when a view plays a video inline: the file's item from
//! Microsoft Graph by its sharing link ([`FileDownload`], its size and download link), then the parts asked for at
//! that link ([`HttpTransport::file_range`], `Range`, no token), as `docs/protocol/graph.md` records.
//!
//! - **The download link** is kept for the run per file, in memory, for [`LINK_LIFE`] (Graph's links last about an
//!   hour); an older one is asked for again, and so is one the file's host refused (a 401 or 403: it expired), once
//!   per part. One read of a file's item at a time: asks while it runs wait for it.
//! - **A part** is read when asked, never kept here (the player keeps what it read); at most
//!   [`teams_proto::MAX_RANGE`] bytes of it.
//! - **A stream** ([`FileStream`], what a playing video reads by) is a part of up to [`teams_proto::MAX_STREAM`] bytes
//!   handed on as it arrives ([`HttpTransport::file_stream`]), a few of its pieces held at a time: its reader takes
//!   them as fast as it wants them, and dropping it (or its [`crate::engine::Reply`] before it was answered) stops the
//!   read at once, so a seek's new place never shares the network with the old one's. A stream whose body stalls for
//!   [`STALLED`] fails ([`EngineError::FileUnread`]).
//! - **Its size** ([`FileContents::size`]) is the item's, read with its link: asked for when a video's card is drawn,
//!   so its link is already known when Play is pressed.
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

use teams_proto::{
    Client, Credentials, FileBody, FileContent, FileDownload, FileRange, HttpError, HttpTransport, StreamStart,
};
use tokio::sync::{mpsc, oneshot};

use crate::engine::{EngineError, Responder};

/// How long a download link Graph handed out is used before it is asked for again.
pub const LINK_LIFE: Duration = Duration::from_mins(40);

/// How long a stream's body may stay silent before the stream fails.
pub const STALLED: Duration = Duration::from_secs(20);

/// The pieces of a stream held for its reader at a time (each as the network handed it: a few kilobytes).
const STREAM_PIECES: usize = 64;

/// A part of a file streamed as it arrives ([`crate::EngineRequests::file_stream`]): where it starts, how many bytes it
/// holds, the file's whole size, and its bytes piece by piece. Dropped, its read stops.
#[derive(Debug)]
pub struct FileStream {
    pub offset: u64,
    /// The bytes it holds (what was asked, or fewer where the file ends).
    pub len: u64,
    /// The file's whole size.
    pub total: u64,
    pieces: mpsc::Receiver<Result<Vec<u8>, EngineError>>,
}

impl FileStream {
    /// A stream of `bytes` from `offset` of a file of `total` bytes, in pieces of `piece` bytes: for a stand-in of the
    /// engine (a UI test's fake core) to hand back.
    #[must_use]
    pub fn of(offset: u64, bytes: &[u8], total: u64, piece: usize) -> Self {
        let pieces: Vec<&[u8]> = bytes.chunks(piece.max(1)).collect();
        let (tell, told) = mpsc::channel(pieces.len().max(1));
        for piece in pieces {
            // the receiver is held right here and there is room for every piece
            let _ = tell.try_send(Ok(piece.to_vec()));
        }
        Self {
            offset,
            len: bytes.len() as u64,
            total,
            pieces: told,
        }
    }

    /// The next piece, in order; `None` once all of it came. [`EngineError::FileUnread`] when the read failed on the
    /// way (it ends there).
    pub async fn next(&mut self) -> Option<Result<Vec<u8>, EngineError>> {
        self.pieces.recv().await
    }
}

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
        let Some(file) = FileDownload::of_in(self.client.transport().cloud(), share_url) else {
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

impl<C: Credentials + 'static> FileContents<C> {
    /// Answers `reply` with a stream of `len` bytes of the file `share_url` is the sharing link of, from `offset`, once
    /// its answer's headers came; its pieces follow as they arrive, until its reader drops it.
    pub(crate) fn stream(&self, share_url: &str, offset: u64, len: u64, mut reply: Responder<FileStream>) {
        let Some(file) = FileDownload::of_in(self.client.transport().cloud(), share_url) else {
            let _ = reply.send(Err(EngineError::FileUnavailable));
            return;
        };
        let (client, links) = (Arc::clone(&self.client), Rc::clone(&self.links));
        tokio::task::spawn_local(async move {
            let opened = tokio::select! {
                opened = open_stream(&client, &links, &file, offset, len) => opened,
                // asked for and let go before it was answered (the player sought elsewhere)
                () = reply.closed() => return,
            };
            let (body, start) = match opened {
                Ok(opened) => opened,
                Err(error) => {
                    let _ = reply.send(Err(error));
                    return;
                }
            };
            let (tell, pieces) = mpsc::channel(STREAM_PIECES);
            let stream = FileStream {
                offset,
                len: start.len,
                total: start.total,
                pieces,
            };
            if reply.send(Ok(stream)).is_ok() {
                hand_on(body, &tell).await;
            }
        });
    }

    /// Answers `reply` with the size of the file `share_url` is the sharing link of, as its item gives it; its link is
    /// kept for the reads that follow.
    pub(crate) fn size(&self, share_url: &str, reply: Responder<u64>) {
        let Some(file) = FileDownload::of_in(self.client.transport().cloud(), share_url) else {
            let _ = reply.send(Err(EngineError::FileUnavailable));
            return;
        };
        let (client, links) = (Arc::clone(&self.client), Rc::clone(&self.links));
        tokio::task::spawn_local(async move {
            let _ = reply.send(link(&client, &links, &file).await.map(|content| content.size));
        });
    }
}

/// Hands `body`'s pieces on to `tell` as they arrive, until all came, it fails, it stalls for [`STALLED`], or its reader
/// let go of it (dropping `body` stops the read).
async fn hand_on(mut body: FileBody, tell: &mpsc::Sender<Result<Vec<u8>, EngineError>>) {
    loop {
        let piece = tokio::select! {
            piece = tokio::time::timeout(STALLED, body.chunk()) => piece,
            () = tell.closed() => return,
        };
        let piece = match piece {
            Ok(Ok(Some(bytes))) => Ok(bytes),
            Ok(Ok(None)) => return,
            Ok(Err(error)) => {
                log::info!("a shared file's stream broke off: {error}");
                Err(EngineError::FileUnread)
            }
            Err(_) => {
                log::info!("a shared file's stream stalled");
                Err(EngineError::FileUnread)
            }
        };
        let failed = piece.is_err();
        if tell.send(piece).await.is_err() || failed {
            return;
        }
    }
}

/// A stream of the part of `file` asked for, as [`read_range`] reads a part: at the link known, else one read now; a
/// link the host refused is read again once.
async fn open_stream<C: Credentials + 'static>(
    client: &Arc<Client<HttpTransport<C>>>,
    links: &Links,
    file: &FileDownload,
    offset: u64,
    len: u64,
) -> Result<(FileBody, StreamStart), EngineError> {
    let mut fresh = false;
    loop {
        let content = link(client, links, file).await?;
        // a read past the end asks for the file's last byte, which tells its size
        let len = len.min(content.size.saturating_sub(offset)).max(1);
        match client.transport().file_stream(&content.link, offset, len).await {
            Ok(body) => {
                let start = body.start().ok_or(EngineError::FileUnavailable)?;
                return Ok((body, start));
            }
            Err(HttpError::Status(answer)) if matches!(answer.status(), 401 | 403) && !fresh => {
                // the link expired: forget it and ask Graph again
                forget(links, file, &content);
                fresh = true;
            }
            Err(HttpError::Status(answer)) if !gone(answer.status()) => {
                log::info!("a shared file's stream couldn't be read: HTTP {}", answer.status());
                return Err(EngineError::FileUnread);
            }
            Err(HttpError::Status(_) | HttpError::BodyTooLarge | HttpError::Refused(_)) => {
                return Err(EngineError::FileUnavailable);
            }
            Err(error) => {
                log::info!("a shared file's stream couldn't be read: {error}");
                return Err(EngineError::FileUnread);
            }
        }
    }
}

/// Forgets `file`'s link `content`, which its host refused (it expired), unless another ask replaced it meanwhile.
fn forget(links: &Links, file: &FileDownload, content: &FileContent) {
    let mut known = links.borrow_mut();
    if let Some(Link::Known(kept, _)) = known.get(file)
        && kept == content
    {
        known.remove(file);
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
                // the link expired: forget it and ask Graph again
                forget(links, file, &content);
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
/// every ask that waits for it. The read runs apart from the ask that started it (on the engine's `LocalSet`), so an
/// ask let go of while it runs (a stream's, by a seek) leaves it to answer the others, never a read no one finishes.
async fn link<C: Credentials + 'static>(
    client: &Arc<Client<HttpTransport<C>>>,
    links: &Links,
    file: &FileDownload,
) -> Result<FileContent, EngineError> {
    let (tell, told) = oneshot::channel();
    {
        let mut known = links.borrow_mut();
        match known.get_mut(file) {
            Some(Link::Known(content, at)) if at.elapsed() < LINK_LIFE => return Ok(content.clone()),
            Some(Link::Reading(waiting)) => waiting.push(tell),
            Some(Link::Known(..)) | None => {
                known.insert(file.clone(), Link::Reading(vec![tell]));
                let (client, links, file) = (Arc::clone(client), Rc::clone(links), file.clone());
                tokio::task::spawn_local(async move {
                    let answer = read_link(&client, &file).await;
                    settle(&links, &file, &answer);
                });
            }
        }
    }
    Ok(told.await.unwrap_or(Err(LinkFailure::Unread))?)
}

/// Keeps `file`'s link `answer` (or forgets the read that failed) and hands it to every ask waiting for it.
fn settle(links: &Links, file: &FileDownload, answer: &LinkAnswer) {
    let waiting = match answer {
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
