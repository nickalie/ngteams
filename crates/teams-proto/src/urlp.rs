//! The picture of a link's preview, as Teams shows it on the card under a message (`docs/protocol/chat.md`, "Link
//! previews"): the message's `properties.links` entry names it as the async gateway's URL preview service's thumbnail of
//! the page's picture (`https://<region>-prod.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail?url=<picture>`),
//! the service fetching the page's picture itself, so the reader's client never reaches the page's site.
//! [`LinkPreviewPicture`] is that read: `GET urlp/v1/url/image/Thumbnail?url=<picture>` on [`Service::Ams`] (the same
//! async gateway, with the same `ic3` token and `x-ms-migration: True` the web client's URL preview reads carry),
//! nothing sent but the picture's link taken from the message.

use url::Url;

use crate::ams::{PastedFormat, PastedPicture};
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The longest picture link read.
const MAX_LINK: usize = 2048;

/// The thumbnail's path on the async gateway.
const THUMBNAIL_PATH: [&str; 5] = ["urlp", "v1", "url", "image", "Thumbnail"];

/// `GET urlp/v1/url/image/Thumbnail?url=<picture>` on the account's async gateway: a link preview's picture. Its answer
/// is read with [`LinkPreviewPicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkPreviewPicture {
    picture: String,
}

impl LinkPreviewPicture {
    /// The read of the thumbnail a preview's `previewurl` names: `https` on an async gateway host
    /// (`<name>.asyncgw.teams.microsoft.com`), no port, user info or fragment, the path exactly
    /// `/urlp/v1/url/image/Thumbnail` and the query one `url`, an `http` or `https` link of at most 2048 bytes. `None`
    /// for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        Self::of_in(&crate::Cloud::Microsoft, src)
    }

    /// Reads a message URL within the account's media service boundary.
    #[must_use]
    pub fn of_in(cloud: &crate::Cloud, src: &str) -> Option<Self> {
        if !cloud.is_microsoft() {
            let url = Url::parse(src.trim()).ok()?;
            let projected = cloud.project(&url, "/ams", "https://urlp.asyncgw.teams.microsoft.com")?;
            return Self::of(projected.as_str());
        }
        let url = Url::parse(src.trim()).ok()?;
        let host = url.host_str()?;
        let on_gateway = host
            .strip_suffix(".asyncgw.teams.microsoft.com")
            .is_some_and(is_host_name);
        let plain = url.scheme() == "https"
            && on_gateway
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        let segments: Vec<&str> = url.path_segments()?.collect();
        if !plain || segments != THUMBNAIL_PATH {
            return None;
        }
        let mut pairs = url.query_pairs();
        let (name, picture) = pairs.next()?;
        if name != "url" || pairs.next().is_some() || picture.len() > MAX_LINK {
            return None;
        }
        let parsed = Url::parse(&picture).ok()?;
        matches!(parsed.scheme(), "http" | "https").then(|| Self {
            picture: picture.into_owned(),
        })
    }

    /// The `src` a card's picture (`picture`, an `http` or `https` link of at most 2048 bytes) is asked for by
    /// (`crate::chat::card`): the async gateway's thumbnail of it, as a link preview's picture is read, so the reader's
    /// client never reaches the picture's site. The host it names is a stand-in: the read goes to the account's own
    /// gateway ([`Service::Ams`]), as every thumbnail read does. `None` for any other link.
    #[must_use]
    pub fn card_picture_src(picture: &str) -> Option<String> {
        Self::card_picture_src_in(&crate::Cloud::Microsoft, picture)
    }

    /// Constructs a card thumbnail URL on the account's media service.
    #[must_use]
    pub fn card_picture_src_in(cloud: &crate::Cloud, picture: &str) -> Option<String> {
        let picture = picture.trim();
        let parsed = Url::parse(picture).ok()?;
        if !matches!(parsed.scheme(), "http" | "https") || picture.len() > MAX_LINK {
            return None;
        }
        let mut src = cloud
            .service_url(
                "https://urlp.asyncgw.teams.microsoft.com/urlp/v1/url/image/Thumbnail",
                "/ams/urlp/v1/url/image/Thumbnail",
            )
            .ok()?;
        src.query_pairs_mut().append_pair("url", picture);
        Self::of_in(cloud, src.as_str()).map(|_| src.into())
    }

    /// The page's picture the thumbnail is of.
    #[must_use]
    pub fn picture(&self) -> &str {
        &self.picture
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

impl Read for LinkPreviewPicture {
    fn service(&self) -> Service {
        Service::Ams
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(THUMBNAIL_PATH).with_query("url", self.picture.as_str())
    }
}

/// A host's own name before the async gateway's domain: lowercase ASCII letters, digits and `-` (`us-prod`).
fn is_host_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}
