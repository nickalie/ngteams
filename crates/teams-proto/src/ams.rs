//! A picture pasted into a message, as Teams shows it in the message (`docs/protocol/chat.md`, "Pictures in messages"):
//! the message's `<img itemtype="http://schema.skype.com/AMSImage">` names a view of a media service (AMS) object
//! (`https://us-api.asm.skype.com/v1/objects/<id>/views/imgo`, or on the async gateway's host), and the web client reads
//! that view from its own AMS base, the region's GTM `amsV2`, whatever host the message names. [`MessagePicture`] is
//! that read: `GET v1/objects/<id>/views/<view>` on [`Service::Ams`], the account's `ic3` token, nothing sent but the
//! object's id and the view's name taken from the message.

use std::fmt;

use url::Url;

use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The longest object id read.
const MAX_OBJECT_ID: usize = 128;

/// The hosts a message names an AMS view on: the media service's (`us-api.asm.skype.com`, the GTM `ams`) and the async
/// gateway's (`us-prod.asyncgw.teams.microsoft.com`, the GTM `amsV2`), each with a name of their own before them.
const PICTURE_HOSTS: [&str; 2] = [".asm.skype.com", ".asyncgw.teams.microsoft.com"];

/// The views of a picture a message names: its original, its full-size rendering, and an animated one's.
const VIEWS: [&str; 3] = ["imgo", "imgpsh_fullsize", "imgpsh_fullsize_anim"];

/// What a picture's bytes are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PastedFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl PastedFormat {
    /// The file extension a picture of this format is named with.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }

    /// The format `bytes` start with (their signature), if they are a picture of one.
    #[must_use]
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }
}

/// A picture's bytes and what they are.
#[derive(Clone, PartialEq, Eq)]
pub struct PastedPicture {
    pub format: PastedFormat,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for PastedPicture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PastedPicture")
            .field("format", &self.format)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// `GET v1/objects/<id>/views/<view>` on the account's AMS base: the view of a picture a message shows. Its answer is
/// read with [`MessagePicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MessagePicture {
    object: String,
    view: &'static str,
}

impl MessagePicture {
    /// The read of the view an `<img>`'s `src` names: `https` on an AMS host ([`PICTURE_HOSTS`]), no port, user info,
    /// query or fragment, the path exactly `/v1/objects/<id>/views/<view>` (the id one segment of ASCII letters, digits,
    /// `-`, `_` and `.`, at most 128 of them; the view one a message names: `imgo`, `imgpsh_fullsize`,
    /// `imgpsh_fullsize_anim`). `None` for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        Self::of_in(&crate::Cloud::Microsoft, src)
    }

    /// Reads a message URL within the account's media service boundary.
    #[must_use]
    pub fn of_in(cloud: &crate::Cloud, src: &str) -> Option<Self> {
        if !cloud.is_microsoft() {
            let url = Url::parse(src.trim()).ok()?;
            let projected = cloud.project(&url, "/ams", "https://us-api.asm.skype.com")?;
            return Self::of(projected.as_str());
        }
        let url = Url::parse(src.trim()).ok()?;
        let host = url.host_str()?;
        let on_ams = PICTURE_HOSTS
            .iter()
            .any(|suffix| host.strip_suffix(suffix).is_some_and(is_host_name));
        let plain = url.scheme() == "https"
            && on_ams
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none();
        if !plain {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let [v1, objects, object, views, view] = segments[..] else {
            return None;
        };
        let view = VIEWS.into_iter().find(|known| *known == view)?;
        (v1 == "v1" && objects == "objects" && views == "views" && is_object_id(object)).then(|| Self {
            object: object.to_owned(),
            view,
        })
    }

    /// The object's id.
    #[must_use]
    pub fn object(&self) -> &str {
        &self.object
    }

    /// The view's name.
    #[must_use]
    pub fn view(&self) -> &str {
        self.view
    }

    /// The picture the answer gives: its bytes when they are a PNG, JPEG, GIF or WebP picture (by their signature), else
    /// `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        let bytes = answer.body();
        let format = PastedFormat::sniff(bytes)?;
        Some(PastedPicture {
            format,
            bytes: bytes.to_vec(),
        })
    }
}

impl Read for MessagePicture {
    fn service(&self) -> Service {
        Service::Ams
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "objects", self.object.as_str(), "views", self.view])
    }
}

/// A host's own name before an AMS domain: lowercase ASCII letters, digits and `-` (`us-api`, `us-prod`).
fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// An object id in one path segment: 1 to [`MAX_OBJECT_ID`] ASCII letters, digits, `-`, `_` and `.` (the media
/// service's are like `0-eus-d9-<hex>`), not `.` or `..`.
pub(crate) fn is_object_id(id: &str) -> bool {
    (1..=MAX_OBJECT_ID).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && id != "."
        && id != ".."
}

/// The view of a sent picture its message names: the object's original (`imgo`) on the media service's host.
const SENT_PICTURE_BASE: &str = "https://us-api.asm.skype.com/v1/objects";

/// The `<img>` a message shows a picture sent into `object` with, at `given` CSS pixels, as the web client writes a
/// pasted picture (`docs/protocol/chat.md`, "Pictures sent"): `itemtype` `AMSImage`, `itemid` the object, its `imgo`
/// view. `None` for an id that isn't an object's.
#[must_use]
pub fn sent_picture(object: &str, given: (u32, u32)) -> Option<crate::chat::Image> {
    sent_picture_in(&crate::Cloud::Microsoft, object, given)
}

/// Constructs a sent picture's image on the account's media origin.
#[must_use]
pub fn sent_picture_in(cloud: &crate::Cloud, object: &str, given: (u32, u32)) -> Option<crate::chat::Image> {
    let base = cloud.service_url(SENT_PICTURE_BASE, "/ams/v1/objects").ok()?;
    is_object_id(object).then(|| crate::chat::Image {
        src: format!("{base}/{object}/views/imgo"),
        alt: Some("image".to_owned()),
        item_type: Some(crate::chat::AMS_IMAGE_ITEMTYPE.to_owned()),
        item_id: Some(object.to_owned()),
        size: Some((given.0.max(1), given.1.max(1))),
    })
}
