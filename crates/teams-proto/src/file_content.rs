//! A file a message carries, read part by part to play it in the chat (`docs/protocol/graph.md`, "A shared file's
//! content"): a video's card plays it inline, as Teams' card does.
//!
//! - [`FileDownload`] is a [`Read`] on [`Service::Graph`] with the account's Graph token:
//!   `GET shares/u!<the sharing link, base64url>/driveItem`, Graph's documented way to reach a shared file by its
//!   sharing link (the same sharing token as [`crate::FileThumbnails`]). Its answer is the file's item: its `size` and
//!   its `@microsoft.graph.downloadUrl`, a short-lived pre-authenticated link to the file's bytes.
//! - [`FileContentLink`] is that link as Graph handed it out: an `https` link on a SharePoint host, which carries its
//!   own credential in its query (`tempauth`). It is made only from Graph's answer ([`FileDownload::read_answer`]), its
//!   `Debug` shows nothing of it, and [`crate::HttpTransport::file_range`] reads it with no token of the account's
//!   ([`Service::FileContent`]): the account's credentials never reach it.
//! - [`FileRange`] is one part of the file as that read answered it: a `206` with its `Content-Range`, or a `200` with
//!   the whole file (a server that ignores `Range` for a small file).
//! - [`StreamStart`] is where a streamed read of the file starts and how much it holds, as its answer's status and
//!   headers say ([`crate::HttpTransport::file_stream`]: the same `GET` with `Range`, its body handed on part by part
//!   as it arrives, never held whole), what a playing video reads by.
//!
//! No capture shows either read (the web client plays a video through SharePoint's own media API with a SharePoint
//! token the native client doesn't ask for): they are our own reach, as the thumbnails' are. A file the account can't
//! read answers 403 or 404, and the card says it can't be played.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use url::Url;

use crate::chat::is_sharepoint_host;
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The longest content link taken, in bytes (its `tempauth` makes it long).
const MAX_CONTENT_LINK: usize = 8192;

/// The most bytes one ranged read asks for: well under the transport's [`crate::MAX_BODY`].
pub const MAX_RANGE: u32 = 2 * 1024 * 1024;

/// The most bytes one streamed read asks for ([`crate::HttpTransport::file_stream`]): its body is handed on as it
/// arrives, so this bounds only how long one answer runs.
pub const MAX_STREAM: u64 = 64 * 1024 * 1024;

/// `GET https://graph.microsoft.com/v1.0/shares/u!<link>/driveItem`: the item of the file a sharing link names, with
/// its size and its download link. Its answer is read with [`FileDownload::read_answer`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileDownload {
    /// `u!` and the sharing link, base64url without padding (Graph's sharing token).
    share: String,
    cloud: crate::Cloud,
}

impl FileDownload {
    /// The read for the file `share_url` is the sharing link of; `None` unless it is an `https` link on a SharePoint
    /// host ([`crate::chat::is_file_link`]).
    #[must_use]
    pub fn of(share_url: &str) -> Option<Self> {
        Self::of_in(&crate::Cloud::Microsoft, share_url)
    }

    /// Resolves a sharing link only within this account's file service.
    #[must_use]
    pub fn of_in(cloud: &crate::Cloud, share_url: &str) -> Option<Self> {
        crate::chat::is_file_link_in(cloud, share_url).then(|| Self {
            cloud: cloud.clone(),
            share: format!("u!{}", URL_SAFE_NO_PAD.encode(share_url.as_bytes())),
        })
    }

    /// The sharing token the read names (`u!<base64url>`).
    #[must_use]
    pub fn share(&self) -> &str {
        &self.share
    }

    /// The file's size and download link, when the answer is an item with a `size` and a
    /// `@microsoft.graph.downloadUrl` that [`FileContentLink`] takes; `None` for anything else (a folder, an answer
    /// without the link).
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<FileContent> {
        let item: Value = serde_json::from_slice(answer.body()).ok()?;
        let size = item.get("size")?.as_u64()?;
        let link = item.get("@microsoft.graph.downloadUrl")?.as_str()?;
        Some(FileContent {
            link: FileContentLink::parse_in(&self.cloud, link)?,
            size,
        })
    }
}

/// `Debug` leaves the link out.
impl fmt::Debug for FileDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileDownload").finish_non_exhaustive()
    }
}

impl Read for FileDownload {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["shares", self.share.as_str(), "driveItem"])
    }
}

/// A file's size and the link its bytes are read at, as Graph answered ([`FileDownload::read_answer`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileContent {
    pub link: FileContentLink,
    /// The file's size in bytes, as Graph gave it.
    pub size: u64,
}

/// A file's bytes as Graph handed out their link (see the module docs): read by [`crate::HttpTransport::file_range`]
/// with no token.
#[derive(Clone, PartialEq, Eq)]
pub struct FileContentLink(Url);

impl FileContentLink {
    /// `link` when it is one a file's bytes are read from ([`is_content_link`]).
    pub(crate) fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link).ok()?;
        is_content_link(&url).then_some(Self(url))
    }

    pub(crate) fn parse_in(cloud: &crate::Cloud, link: &str) -> Option<Self> {
        if cloud.is_microsoft() {
            return Self::parse(link);
        }
        let url = Url::parse(link).ok()?;
        is_content_link_in(cloud, &url).then_some(Self(url))
    }

    /// The link (`pub(crate)`: nothing outside this crate sees its credential).
    pub(crate) const fn url(&self) -> &Url {
        &self.0
    }
}

/// `Debug` leaves the link out: its query carries a credential.
impl fmt::Debug for FileContentLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileContentLink").finish_non_exhaustive()
    }
}

/// Whether `url` is one a file's bytes are read from: `https`, no user info, port or fragment, at most
/// [`MAX_CONTENT_LINK`] bytes, on a SharePoint host ([`is_sharepoint_host`]).
#[must_use]
pub fn is_content_link(url: &Url) -> bool {
    is_content_link_in(&crate::Cloud::Microsoft, url)
}

pub(crate) fn is_content_link_in(cloud: &crate::Cloud, url: &Url) -> bool {
    if !cloud.is_microsoft() {
        return url.as_str().len() <= MAX_CONTENT_LINK && cloud.contains(url) && url.path().starts_with("/sp/");
    }
    url.as_str().len() <= MAX_CONTENT_LINK
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && url.host_str().is_some_and(is_sharepoint_host)
}

/// One part of a file, as a ranged read answered it.
#[derive(Clone, PartialEq, Eq)]
pub struct FileRange {
    /// Where the part starts in the file.
    pub offset: u64,
    pub bytes: Vec<u8>,
    /// The file's whole size, as the answer gave it.
    pub total: u64,
}

impl fmt::Debug for FileRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileRange")
            .field("offset", &self.offset)
            .field("len", &self.bytes.len())
            .field("total", &self.total)
            .finish()
    }
}

impl FileRange {
    /// The part of a file the answer to a read of `len` bytes from `offset` holds: a `206` whose `Content-Range`
    /// (`bytes <first>-<last>/<total>`) starts at `offset`, its body exactly that range, `len` bytes or fewer only
    /// where the file ends; or a `200` (the whole file, at most the transport's `MAX_BODY`), cut to what was asked.
    /// `None` for any other answer: a part shorter than asked before the file's end would leave a gap the reader can't
    /// see.
    #[must_use]
    pub fn read(answer: Response, offset: u64, len: u32) -> Option<Self> {
        match answer.status() {
            206 => {
                let (first, last, total) = content_range(answer.content_range()?)?;
                let body = answer.into_body();
                let got = u64::try_from(body.len()).ok()?;
                let whole = got == u64::from(len) || last.checked_add(1)? == total;
                let fits = first == offset
                    && last.checked_sub(first)?.checked_add(1)? == got
                    && got <= u64::from(len)
                    && last < total
                    && whole;
                fits.then_some(Self {
                    offset,
                    bytes: body,
                    total,
                })
            }
            200 => {
                let body = answer.into_body();
                let total = u64::try_from(body.len()).ok()?;
                let start = usize::try_from(offset).ok().filter(|start| *start <= body.len())?;
                let end = start.saturating_add(usize::try_from(len).ok()?).min(body.len());
                Some(Self {
                    offset,
                    bytes: body.get(start..end)?.to_vec(),
                    total,
                })
            }
            _ => None,
        }
    }
}

/// Where a streamed read of a file starts, how many bytes its body holds and the file's whole size, as its answer said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamStart {
    pub offset: u64,
    /// The bytes its body holds (what was asked, or fewer where the file ends).
    pub len: u64,
    pub total: u64,
}

impl StreamStart {
    /// What the answer to a streamed read of `len` bytes from `offset` holds: a `206` whose `Content-Range` (`range`,
    /// `bytes <first>-<last>/<total>`) starts at `offset` and holds `len` bytes, or fewer only where the file ends;
    /// or a `200` (the whole file, `content_length` its size) to a read from the file's start, of which `len` bytes are
    /// taken. `None` for anything else: a part that isn't the one asked for would leave a gap the reader can't see.
    #[must_use]
    pub fn read(status: u16, range: Option<&str>, content_length: Option<u64>, offset: u64, len: u64) -> Option<Self> {
        match status {
            206 => {
                let (first, last, total) = content_range(range?)?;
                let got = last.checked_sub(first)?.checked_add(1)?;
                let whole = got == len || last.checked_add(1)? == total;
                (first == offset && got <= len && last < total && whole).then_some(Self {
                    offset,
                    len: got,
                    total,
                })
            }
            200 if offset == 0 => {
                let total = content_length?;
                Some(Self {
                    offset,
                    len: len.min(total),
                    total,
                })
            }
            _ => None,
        }
    }
}

/// `bytes <first>-<last>/<total>` as its three numbers, `first <= last`.
fn content_range(header: &str) -> Option<(u64, u64, u64)> {
    let rest = header.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (first, last) = range.split_once('-')?;
    let number = |text: &str| -> Option<u64> {
        let text = text.trim();
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let (first, last, total) = (number(first)?, number(last)?, number(total)?);
    (first <= last).then_some((first, last, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_content_range_is_read_whole_or_not_at_all() {
        assert_eq!(content_range("bytes 0-99/1000"), Some((0, 99, 1000)));
        assert_eq!(content_range("bytes 100-100/101"), Some((100, 100, 101)));
        for refused in [
            "bytes 5-4/10",
            "bytes */10",
            "bytes 0-9/*",
            "items 0-9/10",
            "bytes -1-9/10",
            "bytes 0-9",
        ] {
            assert_eq!(content_range(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_partial_answer_is_taken_only_for_the_range_asked() {
        let answer = |range: &str, body: &[u8]| Response::new(206, body.to_vec()).with_content_range(range);
        let part = FileRange::read(answer("bytes 10-13/20", b"abcd"), 10, 4).unwrap();
        assert_eq!((part.offset, part.bytes.as_slice(), part.total), (10, &b"abcd"[..], 20));
        // shorter at the file's end, never before it
        assert!(FileRange::read(answer("bytes 18-19/20", b"ab"), 18, 4).is_some());
        assert!(FileRange::read(answer("bytes 10-11/20", b"ab"), 10, 4).is_none());
        // another start, a body that isn't the range, more than asked, past the end
        assert!(FileRange::read(answer("bytes 11-14/20", b"abcd"), 10, 4).is_none());
        assert!(FileRange::read(answer("bytes 10-13/20", b"abc"), 10, 4).is_none());
        assert!(FileRange::read(answer("bytes 10-15/20", b"abcdef"), 10, 4).is_none());
        assert!(FileRange::read(answer("bytes 10-13/12", b"abcd"), 10, 4).is_none());
        assert!(FileRange::read(Response::new(206, b"abcd".to_vec()), 10, 4).is_none());
    }

    #[test]
    fn a_whole_answer_is_cut_to_the_range_asked() {
        let whole = || Response::new(200, b"0123456789".to_vec());
        let part = FileRange::read(whole(), 8, 4).unwrap();
        assert_eq!((part.bytes.as_slice(), part.total), (&b"89"[..], 10));
        assert!(FileRange::read(whole(), 11, 4).is_none());
        assert!(FileRange::read(Response::new(404, Vec::new()), 0, 4).is_none());
    }

    #[test]
    fn only_a_sharepoint_link_is_a_content_link() {
        assert!(
            FileContentLink::parse("https://contoso-my.sharepoint.com/_layouts/15/download.aspx?tempauth=x").is_some()
        );
        for refused in [
            "http://contoso-my.sharepoint.com/download.aspx",
            "https://contoso-my.sharepoint.com:444/download.aspx",
            "https://user@contoso-my.sharepoint.com/download.aspx",
            "https://contoso.example.com/download.aspx",
            "https://contoso-my.sharepoint.com/download.aspx#x",
        ] {
            assert!(FileContentLink::parse(refused).is_none(), "{refused}");
        }
    }

    #[test]
    fn a_streamed_read_starts_where_it_was_asked() {
        let asked = |status, range, length| StreamStart::read(status, range, length, 1_000, 500);
        assert_eq!(
            asked(206, Some("bytes 1000-1499/9000"), None),
            Some(StreamStart {
                offset: 1_000,
                len: 500,
                total: 9_000
            })
        );
        // fewer only where the file ends
        assert_eq!(
            asked(206, Some("bytes 1000-1199/1200"), None).map(|start| start.len),
            Some(200)
        );
        assert_eq!(asked(206, Some("bytes 1000-1199/9000"), None), None);
        assert_eq!(asked(206, Some("bytes 900-1399/9000"), None), None);
        assert_eq!(asked(206, Some("bytes 1000-1599/9000"), None), None);
        assert_eq!(asked(206, None, None), None);
        // a whole file only for a read from its start
        assert_eq!(asked(200, None, Some(9_000)), None);
        assert_eq!(
            StreamStart::read(200, None, Some(300), 0, 500),
            Some(StreamStart {
                offset: 0,
                len: 300,
                total: 300
            })
        );
        assert_eq!(StreamStart::read(200, None, None, 0, 500), None);
    }

    #[test]
    fn the_item_answer_gives_the_size_and_the_link() {
        let read = FileDownload::of("https://contoso-my.sharepoint.com/:v:/g/personal/someone/made-up").unwrap();
        let answer = Response::new(
            200,
            br#"{"id":"x","size":1234,"@microsoft.graph.downloadUrl":"https://contoso-my.sharepoint.com/_layouts/15/download.aspx?UniqueId=x&tempauth=y"}"#
                .to_vec(),
        );
        let content = read.read_answer(&answer).unwrap();
        assert_eq!(content.size, 1234);
        let no_link = Response::new(200, br#"{"id":"x","size":1234}"#.to_vec());
        assert!(read.read_answer(&no_link).is_none());
        let elsewhere = Response::new(
            200,
            br#"{"size":1,"@microsoft.graph.downloadUrl":"https://example.com/x"}"#.to_vec(),
        );
        assert!(read.read_answer(&elsewhere).is_none());
        assert!(FileDownload::of("https://example.com/x").is_none());
    }
}
