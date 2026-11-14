//! A picture of a file a message carries, for its card (`docs/protocol/graph.md`, "A shared file's thumbnail"): the
//! file's thumbnails as Microsoft Graph lists them for its sharing link, then the picture of the one the card draws.
//!
//! - [`FileThumbnails`] is a [`Read`] on [`Service::Graph`] with the account's Graph token:
//!   `GET shares/u!<the sharing link, base64url>/driveItem/thumbnails`, Graph's documented way to reach a shared file by
//!   its sharing link, the link taken from the message ([`crate::chat::SharedFile::share_url`], a SharePoint `https`
//!   link). Its answer lists thumbnail sets, each with a `small`, `medium` and `large` picture's `url`, `width` and
//!   `height`.
//! - [`ThumbnailLink`] is the `url` of the first set's `large` (else `medium`) picture, as Graph handed it out: a
//!   pre-authenticated `https` link on SharePoint's media host (`<name>.svc.ms`) or on a SharePoint host, which carries
//!   its own credential in its query. It is made only from Graph's answer ([`FileThumbnails::read_answer`]), its `Debug`
//!   shows nothing of it, and [`crate::HttpTransport::file_thumbnail`] reads it with no token of the account's
//!   ([`Service::FileThumbnail`]): the account's credentials never reach the media host.
//!
//! No capture shows either read (the web client draws a file's card from SharePoint's own API, with a SharePoint
//! token the native client doesn't ask for): they are our own reach, as Graph's reads of a person's name and picture
//! are. A file Graph has no thumbnail of answers an empty list; a file the account can't read, 403 or 404: either is
//! no picture, and the card shows its placeholder.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use url::Url;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The longest thumbnail link taken, in bytes (a pre-authenticated link's credential makes it long).
const MAX_THUMBNAIL_LINK: usize = 8192;

/// The sizes taken from a thumbnail set, the first there is: a card is about 320 pixels wide, `large` is up to 800 on
/// its longer side and `medium` up to 176.
const SIZES: [&str; 2] = ["large", "medium"];

/// SharePoint's media host's domain: thumbnails are served from `<name>.svc.ms`.
pub const THUMBNAIL_HOST_SUFFIX: &str = ".svc.ms";

/// `GET https://graph.microsoft.com/v1.0/shares/u!<link>/driveItem/thumbnails`: the thumbnail sets of the file a
/// sharing link names. Its answer is read with [`FileThumbnails::read_answer`].
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FileThumbnails {
    /// `u!` and the sharing link, base64url without padding (Graph's sharing token).
    share: String,
    cloud: crate::Cloud,
}

impl FileThumbnails {
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

    /// The link of the picture a card draws: the first thumbnail set's `large` picture, else its `medium` one, when
    /// its `url` is a link [`ThumbnailLink`] takes; `None` for an empty list or any other answer.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<ThumbnailLink> {
        let answer: Value = serde_json::from_slice(answer.body()).ok()?;
        let set = answer.get("value")?.as_array()?.first()?;
        SIZES
            .iter()
            .filter_map(|size| set.get(size)?.get("url")?.as_str())
            .find_map(|link| ThumbnailLink::parse_in(&self.cloud, link))
    }
}

/// `Debug` leaves the link out.
impl fmt::Debug for FileThumbnails {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileThumbnails").finish_non_exhaustive()
    }
}

impl Read for FileThumbnails {
    fn service(&self) -> Service {
        Service::Graph
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["shares", self.share.as_str(), "driveItem", "thumbnails"])
    }
}

/// A thumbnail's picture as Graph handed out its link (see the module docs): read by
/// [`crate::HttpTransport::file_thumbnail`] with no token.
#[derive(Clone, PartialEq, Eq)]
pub struct ThumbnailLink(Url);

impl ThumbnailLink {
    /// `link` when it is one a thumbnail is read from ([`is_thumbnail_link`]).
    pub(crate) fn parse(link: &str) -> Option<Self> {
        let url = Url::parse(link).ok()?;
        is_thumbnail_link(&url).then_some(Self(url))
    }

    pub(crate) fn parse_in(cloud: &crate::Cloud, link: &str) -> Option<Self> {
        if cloud.is_microsoft() {
            return Self::parse(link);
        }
        let url = Url::parse(link).ok()?;
        is_thumbnail_link_in(cloud, &url).then_some(Self(url))
    }

    /// The link (`pub(crate)`: nothing outside this crate sees its credential).
    pub(crate) const fn url(&self) -> &Url {
        &self.0
    }
}

/// `Debug` leaves the link out: its query carries a credential.
impl fmt::Debug for ThumbnailLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThumbnailLink").finish_non_exhaustive()
    }
}

/// Whether `url` is one a thumbnail's picture is read from: `https`, no user info, port or fragment, at most
/// [`MAX_THUMBNAIL_LINK`] bytes, on SharePoint's media host (`<name>.svc.ms`, the name lowercase ASCII letters, digits
/// and `-`) or a SharePoint host ([`crate::chat::is_sharepoint_host`]).
#[must_use]
pub fn is_thumbnail_link(url: &Url) -> bool {
    is_thumbnail_link_in(&crate::Cloud::Microsoft, url)
}

pub(crate) fn is_thumbnail_link_in(cloud: &crate::Cloud, url: &Url) -> bool {
    if !cloud.is_microsoft() {
        return url.as_str().len() <= MAX_THUMBNAIL_LINK && cloud.contains(url) && url.path().starts_with("/sp/");
    }
    let media_host = |host: &str| {
        host.strip_suffix(THUMBNAIL_HOST_SUFFIX).is_some_and(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
    };
    url.as_str().len() <= MAX_THUMBNAIL_LINK
        && url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.fragment().is_none()
        && url
            .host_str()
            .is_some_and(|host| media_host(host) || crate::chat::is_sharepoint_host(host))
}
