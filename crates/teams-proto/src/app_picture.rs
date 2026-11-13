//! An app's tile, the picture its definition's `largeImageUrl` names (`docs/protocol/chat.md`, "App items"): a public
//! file on Microsoft's CDN, which the web client's `<img>` loads with no credential. [`AppPicture`] is that read, a
//! `GET` of the same path and version on [`Service::AppPictures`] (`https://res.cdn.office.net/teamsappdata/`) or
//! [`Service::StaticAppPictures`] (`https://statics.teams.cdn.office.net/`), with no token. The CDN answers
//! `application/octet-stream`, so the answer is taken by its bytes' signature.

use url::Url;

use crate::ams::{PastedFormat, PastedPicture};
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The app pictures' CDN host ([`Service::AppPictures`]).
pub const APP_PICTURES_HOST: &str = "res.cdn.office.net";
/// The folder every app picture on [`APP_PICTURES_HOST`] lies under.
pub const APP_PICTURES_PATH: &str = "teamsappdata";
/// Teams' static CDN host, where older definitions' pictures lie ([`Service::StaticAppPictures`]).
pub const STATIC_APP_PICTURES_HOST: &str = "statics.teams.cdn.office.net";

/// The most path segments taken (the longest captured had 9 under the folder).
const MAX_SEGMENTS: usize = 16;
/// The longest segment taken.
const MAX_SEGMENT: usize = 128;
/// The longest version taken (`1.2.25`).
const MAX_VERSION: usize = 64;

/// `GET` of an app's tile on Microsoft's CDN. Its answer is read with [`AppPicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AppPicture {
    service: Service,
    segments: Vec<String>,
    version: Option<String>,
}

impl AppPicture {
    /// The read of the picture a definition's `largeImageUrl` names: `https`, no port, user info or fragment, either
    /// `res.cdn.office.net/teamsappdata/…` or `statics.teams.cdn.office.net/app-assets/…` or
    /// `…/evergreen-assets/apps/…`; each segment ASCII letters, digits, `-`, `_`, `.`, `+` and `=` (not `.` or `..`),
    /// the last a `.png`, `.jpg` or `.jpeg` (any case); no query, or `v=<version>` alone (letters, digits, `.`, `-`,
    /// `_`). `None` for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        let url = Url::parse(src.trim()).ok()?;
        let plain = url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        if !plain {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        if segments.len() > MAX_SEGMENTS || !segments.iter().all(|segment| is_segment(segment)) {
            return None;
        }
        let (service, path) = match (url.host_str()?, &segments[..]) {
            (APP_PICTURES_HOST, [APP_PICTURES_PATH, path @ ..]) => (Service::AppPictures, path),
            (STATIC_APP_PICTURES_HOST, path @ (["app-assets", ..] | ["evergreen-assets", "apps", ..])) => {
                (Service::StaticAppPictures, path)
            }
            _ => return None,
        };
        if !path.last().is_some_and(|file| is_picture_file(file)) {
            return None;
        }
        Some(Self {
            service,
            segments: path.iter().map(|segment| (*segment).to_owned()).collect(),
            version: version(&url).ok()?,
        })
    }

    /// The picture the answer gives: its bytes when they are a PNG, JPEG, GIF or WebP by their signature, else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        let bytes = answer.body();
        PastedFormat::sniff(bytes).map(|format| PastedPicture {
            format,
            bytes: bytes.to_vec(),
        })
    }

    /// What names this picture whichever run reads it: its service's host, path and version.
    #[must_use]
    pub fn key(&self) -> String {
        let host = if self.service == Service::StaticAppPictures {
            STATIC_APP_PICTURES_HOST
        } else {
            APP_PICTURES_HOST
        };
        let version = self.version.as_deref().unwrap_or_default();
        format!("app/{host}/{}?v={version}", self.segments.join("/"))
    }
}

impl Read for AppPicture {
    fn service(&self) -> Service {
        self.service
    }

    fn endpoint(&self) -> Endpoint {
        let endpoint = Endpoint::new(self.segments.iter().map(String::as_str));
        match &self.version {
            Some(version) => endpoint.with_query("v", version),
            None => endpoint,
        }
    }
}

/// `Ok(None)` with no query, `Ok(Some(version))` with `v=<version>` alone, `Err(())` for any other query.
fn version(url: &Url) -> Result<Option<String>, ()> {
    if url.query().is_none_or(str::is_empty) {
        return Ok(None);
    }
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match &pairs[..] {
        [(name, version)] if name == "v" && is_version(version) => Ok(Some(version.clone())),
        _ => Err(()),
    }
}

fn is_segment(segment: &str) -> bool {
    (1..=MAX_SEGMENT).contains(&segment.len())
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'='))
}

fn is_picture_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg"]
        .iter()
        .any(|extension| lower.strip_suffix(extension).is_some_and(|stem| !stem.is_empty()))
}

fn is_version(version: &str) -> bool {
    (1..=MAX_VERSION).contains(&version.len())
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}
