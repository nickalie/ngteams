//! A GIF picked from Teams' GIF picker, as a message carries it (`docs/protocol/chat.md`, "Pictures in messages"): an
//! `<img>` whose `src` is Giphy's media CDN (`https://media<n>.giphy.com/media/v1.<token>/<id>/giphy.gif`, the token
//! Giphy's own note of where it was picked, sometimes with `itemtype="http://schema.skype.com/Giphy"`). The web client
//! draws it as a browser draws any `<img>`: Giphy's public file, read with no credential. [`GiphyPicture`] is that read,
//! `GET media/<id>/<file>` on [`Service::Giphy`] (`https://media.giphy.com/`), with no token: nothing is sent but the
//! GIF's id and its file's name taken from the message; the token, the shard's host and any query are left out (Giphy
//! serves the same file without them).

use url::Url;

use crate::ams::{MessagePicture, PastedFormat, PastedPicture};
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;
use crate::urlp::LinkPreviewPicture;

/// The longest GIF id read (Giphy's are about 13 to 20 letters and digits).
const MAX_ID: usize = 64;

/// The longest file name read (`giphy.gif`, `200w.gif`, `giphy-downsized.gif`).
const MAX_FILE: usize = 48;

/// The longest token taken (`v1.` and Giphy's base64url note of the search it was picked from).
const MAX_TOKEN: usize = 1024;

/// `GET https://media.giphy.com/media/<id>/<file>`: a GIF a message shows. Its answer is read with
/// [`GiphyPicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GiphyPicture {
    id: String,
    file: String,
}

impl GiphyPicture {
    /// The read of the GIF an `<img>`'s `src` names: `https` on Giphy's media host (`media.giphy.com`, or a shard of it,
    /// `media0.giphy.com` to `media9.giphy.com`), no port, user info or fragment (a query is left out), the path
    /// `/media/<id>/<file>` or `/media/v1.<token>/<id>/<file>`: the id ASCII letters and digits, at most 64; the file
    /// lowercase ASCII letters, digits, `-` and `_` ending in `.gif`, at most 48 bytes; the token ASCII letters, digits,
    /// `-`, `_` and `=`. `None` for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        let url = Url::parse(src.trim()).ok()?;
        let host = url.host_str()?;
        let plain = url.scheme() == "https"
            && is_media_host(host)
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        if !plain {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let (id, file) = match segments[..] {
            ["media", id, file] => (id, file),
            ["media", token, id, file] if is_token(token) => (id, file),
            _ => return None,
        };
        (is_id(id) && is_file(file)).then(|| Self {
            id: id.to_owned(),
            file: file.to_owned(),
        })
    }

    /// The GIF's id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Its file's name (`giphy.gif`).
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The picture the answer gives: its bytes when they are a GIF (by their signature), else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        let bytes = answer.body();
        (PastedFormat::sniff(bytes)? == PastedFormat::Gif).then(|| PastedPicture {
            format: PastedFormat::Gif,
            bytes: bytes.to_vec(),
        })
    }
}

impl Read for GiphyPicture {
    fn service(&self) -> Service {
        Service::Giphy
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["media", self.id.as_str(), self.file.as_str()])
    }
}

/// A picture a message shows, as the engine reads it: a picture pasted into it, from AMS, a GIF from Giphy, or the
/// picture of a link's preview, from the async gateway's URL preview service.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum InlinePicture {
    Ams(MessagePicture),
    Giphy(GiphyPicture),
    LinkPreview(LinkPreviewPicture),
}

impl InlinePicture {
    /// The read of the picture an `<img>`'s `src` names: a view of an AMS object ([`MessagePicture::of`]) or a GIF on
    /// Giphy's media host ([`GiphyPicture::of`]), or a link preview's `previewurl` ([`LinkPreviewPicture::of`]); `None`
    /// for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        Self::of_in(&crate::Cloud::Microsoft, src)
    }

    /// Reads account media URLs within their cloud; public GIF URLs keep their credential-free route.
    #[must_use]
    pub fn of_in(cloud: &crate::Cloud, src: &str) -> Option<Self> {
        MessagePicture::of_in(cloud, src)
            .map(Self::Ams)
            .or_else(|| GiphyPicture::of(src).map(Self::Giphy))
            .or_else(|| LinkPreviewPicture::of_in(cloud, src).map(Self::LinkPreview))
    }

    /// The picture the answer gives, as its read takes it ([`MessagePicture::read_answer`],
    /// [`GiphyPicture::read_answer`]).
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        match self {
            Self::Ams(picture) => picture.read_answer(answer),
            Self::Giphy(picture) => picture.read_answer(answer),
            Self::LinkPreview(picture) => picture.read_answer(answer),
        }
    }
}

impl Read for InlinePicture {
    fn service(&self) -> Service {
        match self {
            Self::Ams(picture) => picture.service(),
            Self::Giphy(picture) => picture.service(),
            Self::LinkPreview(picture) => picture.service(),
        }
    }

    fn endpoint(&self) -> Endpoint {
        match self {
            Self::Ams(picture) => picture.endpoint(),
            Self::Giphy(picture) => picture.endpoint(),
            Self::LinkPreview(picture) => picture.endpoint(),
        }
    }
}

/// Giphy's media host or one of its shards: `media.giphy.com`, `media0.giphy.com` … `media9.giphy.com`.
fn is_media_host(host: &str) -> bool {
    host.strip_suffix(".giphy.com")
        .and_then(|name| name.strip_prefix("media"))
        .is_some_and(|shard| shard.is_empty() || (shard.len() == 1 && shard.bytes().all(|byte| byte.is_ascii_digit())))
}

fn is_id(id: &str) -> bool {
    (1..=MAX_ID).contains(&id.len()) && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn is_file(file: &str) -> bool {
    file.len() <= MAX_FILE
        && file.strip_suffix(".gif").is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_'))
        })
}

fn is_token(token: &str) -> bool {
    token.len() <= MAX_TOKEN
        && token.strip_prefix("v1.").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'='))
        })
}
