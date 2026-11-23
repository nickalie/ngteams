//! A sticker from Teams' own packs, as a message carries it (`docs/protocol/chat.md`, "Stickers"): an `<img>` of
//! itemtype `http://schema.skype.com/Sticker` whose `src` is the sticker's picture on Teams' public sticker CDN
//! (`https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/<pack>-<w>x<h>[/<locale>]/<name>.gif`
//! `.png` or `.jpg`, with `?v=5`), and the small preview of it the picker shows ([`StickerPreview`]).
//! The web client draws both as a browser draws any `<img>`: a public file, read with no credential. [`StickerPicture`]
//! is that read, `GET <pack folder>[/<locale>]/<file>` on [`Service::Stickers`], with no token: nothing is sent but the
//! folder, the locale and the file's name taken from the message (the query is left out: the CDN serves the same file
//! without it). [`StickerPreview`] is `GET stickers/<locale>/assets/<pack>/Preview/<file>` or
//! `GET meme/assets/Preview/<file>` on [`Service::StickerPreviews`], the same way.

use url::Url;

use crate::ams::{PastedFormat, PastedPicture};
use crate::client::Response;
use crate::endpoint::{Endpoint, Service};
use crate::read::Read;

/// The host Teams' web client reads its stickers from ([`Service::Stickers`]).
pub const STICKERS_HOST: &str = "teams.public.onecdn.static.microsoft";

/// The version Teams' web client asks its sticker files at (`stickerUrlConfig.cdnVersion`), as `?v=5`.
const CDN_VERSION: u32 = 5;

/// The other host of Teams' public `evergreen-assets`, which serves the same sticker files.
const STATICS_HOST: &str = "statics.teams.cdn.office.net";

/// The path of the sticker files on both hosts.
pub(crate) const STICKERS_PATH: [&str; 2] = ["evergreen-assets", "stickerassets"];

/// The longest pack folder read (`clippy-250x250`, `officedrama-250x250`, `meme-500x500`).
const MAX_FOLDER: usize = 64;

/// The longest locale read (`en-us`).
const MAX_LOCALE: usize = 16;

/// The longest file name read (`Clippy_HelloThere.gif`).
const MAX_FILE: usize = 96;

/// The path of the previews on both hosts ([`Service::StickerPreviews`]).
pub(crate) const PREVIEWS_PATH: [&str; 2] = ["evergreen-assets", "funstuff"];

/// The longest pack name read in a preview's path (`DonutFriends`).
const MAX_PACK: usize = 64;

/// `GET https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/<folder>[/<locale>]/<file>`: a
/// sticker a message shows. Its answer is read with [`StickerPicture::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StickerPicture {
    folder: String,
    locale: Option<String>,
    file: String,
}

impl StickerPicture {
    /// The read of the sticker an `<img>`'s `src` names: `https` on [`STICKERS_HOST`] or Teams' static CDN host, no
    /// port, user info or fragment (a query is left out), the path `/evergreen-assets/stickerassets/<folder>/<file>`
    /// or `…/<folder>/<locale>/<file>`: the folder lowercase ASCII letters, digits and `-`; the locale lowercase ASCII
    /// letters and `-`; the file ASCII letters, digits, `-` and `_` ending in `.gif`, `.png` or `.jpg` (a meme's
    /// picture with no caption). `None` for anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        let segments = cdn_segments(src)?;
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let (folder, locale, file) = match segments[..] {
            ["evergreen-assets", "stickerassets", folder, file] => (folder, None, file),
            ["evergreen-assets", "stickerassets", folder, locale, file] if is_locale(locale) => {
                (folder, Some(locale), file)
            }
            _ => return None,
        };
        (is_folder(folder) && is_file(file)).then(|| Self {
            folder: folder.to_owned(),
            locale: locale.map(str::to_owned),
            file: file.to_owned(),
        })
    }

    /// The address on [`STICKERS_HOST`] of `file` in pack folder `folder` (`clippy-250x250`), drawn for `locale` when
    /// given, as Teams' box names it in a message.
    #[must_use]
    pub fn src(folder: &str, locale: Option<&str>, file: &str) -> String {
        let mut path = STICKERS_PATH.to_vec();
        path.push(folder);
        path.extend(locale);
        path.push(file);
        cdn_src(&path)
    }

    /// The pack's folder (`clippy-250x250`).
    #[must_use]
    pub fn folder(&self) -> &str {
        &self.folder
    }

    /// The locale its picture is drawn for (`en-us`), when the path names one.
    #[must_use]
    pub fn locale(&self) -> Option<&str> {
        self.locale.as_deref()
    }

    /// Its file's name (`Clippy_HelloThere.gif`).
    #[must_use]
    pub fn file(&self) -> &str {
        &self.file
    }

    /// The picture the answer gives: its bytes when they are of the format its file's name says (by their signature),
    /// else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        picture_of(answer, format_of(&self.file)?)
    }
}

/// `GET https://teams.public.onecdn.static.microsoft/evergreen-assets/funstuff/<path>`: the preview of one of Teams' own
/// stickers, or the header of one of its packs, as the picker shows them (a PNG). Its answer is read with
/// [`StickerPreview::read_answer`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StickerPreview {
    segments: Vec<String>,
}

impl StickerPreview {
    /// The read of the preview `src` names: `https` on [`STICKERS_HOST`] or Teams' static CDN host, no port, user info
    /// or fragment (a query is left out), the path `/evergreen-assets/funstuff/stickers/<locale>/assets/<pack>/Preview/
    /// <file>` or `/evergreen-assets/funstuff/meme/assets/Preview/<file>`: the locale lowercase ASCII letters and `-`;
    /// the pack ASCII letters, digits and `_`; the file ASCII letters, digits, `-` and `_` ending in `.png`. `None` for
    /// anything else.
    #[must_use]
    pub fn of(src: &str) -> Option<Self> {
        let segments = cdn_segments(src)?;
        let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
        let fits = match parts[..] {
            [
                "evergreen-assets",
                "funstuff",
                "stickers",
                locale,
                "assets",
                pack,
                "Preview",
                file,
            ] => is_locale(locale) && is_pack(pack) && is_preview(file),
            ["evergreen-assets", "funstuff", "meme", "assets", "Preview", file] => is_preview(file),
            _ => false,
        };
        fits.then(|| Self {
            segments: segments[PREVIEWS_PATH.len()..].to_vec(),
        })
    }

    /// The address on [`STICKERS_HOST`] of the preview `file` of pack `pack`, drawn for `locale`.
    #[must_use]
    pub fn src(locale: &str, pack: &str, file: &str) -> String {
        let mut path = PREVIEWS_PATH.to_vec();
        path.extend(["stickers", locale, "assets", pack, "Preview", file]);
        cdn_src(&path)
    }

    /// The address on [`STICKERS_HOST`] of the preview `file` of the Meme pack, which is drawn for no locale.
    #[must_use]
    pub fn meme_src(file: &str) -> String {
        let mut path = PREVIEWS_PATH.to_vec();
        path.extend(["meme", "assets", "Preview", file]);
        cdn_src(&path)
    }

    /// Its path under `evergreen-assets/funstuff/` (`stickers/en-us/assets/Clippy/Preview/Clippy_HelloThere.png`).
    #[must_use]
    pub fn path(&self) -> String {
        self.segments.join("/")
    }

    /// The picture the answer gives: its bytes when they are a PNG (by their signature), else `None`.
    #[must_use]
    pub fn read_answer(&self, answer: &Response) -> Option<PastedPicture> {
        picture_of(answer, PastedFormat::Png)
    }
}

impl Read for StickerPreview {
    fn service(&self) -> Service {
        Service::StickerPreviews
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(self.segments.iter().map(String::as_str))
    }
}

/// The address of `path` on [`STICKERS_HOST`], at Teams' version of its files.
fn cdn_src(path: &[&str]) -> String {
    format!("https://{STICKERS_HOST}/{}?v={CDN_VERSION}", path.join("/"))
}

/// The decoded path segments of `src` when it is a plain `https` URL on either host of Teams' public assets.
fn cdn_segments(src: &str) -> Option<Vec<String>> {
    let url = Url::parse(src.trim()).ok()?;
    let plain = url.scheme() == "https"
        && matches!(url.host_str(), Some(STICKERS_HOST | STATICS_HOST))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none();
    if !plain {
        return None;
    }
    Some(url.path_segments()?.map(str::to_owned).collect())
}

/// The answer's bytes when their signature is `format`'s.
fn picture_of(answer: &Response, format: PastedFormat) -> Option<PastedPicture> {
    let bytes = answer.body();
    (PastedFormat::sniff(bytes)? == format).then(|| PastedPicture {
        format,
        bytes: bytes.to_vec(),
    })
}

/// The format a sticker's file's name says.
fn format_of(file: &str) -> Option<PastedFormat> {
    match file.rsplit_once('.')?.1 {
        "gif" => Some(PastedFormat::Gif),
        "png" => Some(PastedFormat::Png),
        "jpg" => Some(PastedFormat::Jpeg),
        _ => None,
    }
}

impl Read for StickerPicture {
    fn service(&self) -> Service {
        Service::Stickers
    }

    fn endpoint(&self) -> Endpoint {
        let mut segments = vec![self.folder.as_str()];
        segments.extend(self.locale.as_deref());
        segments.push(self.file.as_str());
        Endpoint::new(segments)
    }
}

fn is_folder(folder: &str) -> bool {
    (1..=MAX_FOLDER).contains(&folder.len())
        && folder
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_locale(locale: &str) -> bool {
    (2..=MAX_LOCALE).contains(&locale.len()) && locale.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'-')
}

fn is_file(file: &str) -> bool {
    is_name(file, ".gif") || is_name(file, ".png") || is_name(file, ".jpg")
}

fn is_preview(file: &str) -> bool {
    is_name(file, ".png")
}

fn is_name(file: &str, extension: &str) -> bool {
    file.len() <= MAX_FILE
        && file.strip_suffix(extension).is_some_and(|stem| {
            !stem.is_empty()
                && stem
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

fn is_pack(pack: &str) -> bool {
    (1..=MAX_PACK).contains(&pack.len()) && pack.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
