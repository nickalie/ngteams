//! Files a message carries (`properties.files`, `docs/protocol/chat.md`, "Files: received"): each a file the sender
//! shared from their OneDrive or a team's SharePoint, named by its file name, its type and its links. Read, never
//! fetched: [`SharedFile`] keeps the name, the type and the two links a reader opens it by, and a view asks the engine
//! for its thumbnail ([`crate::FileThumbnails`]).
//!
//! - **The shape** (seen in a received message, written down as `tests/fixtures/chatsvc/files.json`): `properties.files`
//!   is a JSON string of a list; an entry's `@type` is `http://schema.skype.com/File`, its `fileName` (and `title`)
//!   the file's name, `fileType` (and `type`) its extension, `fileInfo.shareUrl` the sharing link the web client opens
//!   it by (`https://<tenant>-my.sharepoint.com/:v:/g/personal/<user>/<id>`), `objectUrl` (and `fileInfo.fileUrl`) the
//!   file's own address in its library, `state` `active` (null in a file shared on from another chat, as Teams' "Share
//!   this file" posts it: [`crate::chat::share_entry`]), `filePreview.previewUrl` empty for a video; for an image,
//!   the view of a picture of it on AMS (`https://<region>-api.asm.skype.com/v1/objects/<id>/views/imgo`) with its
//!   `previewWidth` and `previewHeight`, which Teams draws in the message in place of the file's card
//!   ([`SharedFile::preview`]).
//! - **Read strictly, shown in part**: an entry is a file when its `@type` is a file's (or it has none), it has a
//!   name, and its `state` (when there is one and it isn't null) is `active`. Only an `https` URL on a SharePoint host
//!   ([`is_file_link`]) is kept as its link. A file with no such link is drawn by its name and type, never opened or
//!   fetched (its preview is not kept), and its entry is kept as unread too. `files` is a list, a JSON string of one
//!   (also encoded twice), or one entry alone. Any other entry, and a `files` of another shape, is kept as it came
//!   ([`SharedFiles::unread`]): a view says the message has content it can't show, never a guess at it, and still
//!   draws the files it read.
//! - **Location**: the card's second line is the entry's `chicletBreadcrumbs` (`sourceTeamName > sourceChannelName`,
//!   `personal > <owner>` for a file of someone's OneDrive), else the path of the file's site ([`location_of`]).
//!   Teams' card for a received file shows it under the name. An own server's site names its owner by the account's
//!   id ([`SharedFile::site_owner`]); the view puts the owner's name in its place.

use serde_json::{Map, Value};
use url::Url;

/// An entry's `@type`.
pub const FILE_TYPE: &str = "http://schema.skype.com/File";

/// The longest file name kept, in characters; a longer one is cut there.
const MAX_NAME: usize = 400;

/// The largest side a preview's given size may have, in pixels; a larger one is no size.
const MAX_PREVIEW_SIDE: u64 = 100_000;

/// The longest link kept, in bytes; an entry with only longer ones isn't read.
pub(super) const MAX_LINK: usize = 4096;

/// What kind of file it is, by its extension: what its icon shows and whether it is drawn with a picture of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Video,
    Image,
    Audio,
    Pdf,
    Word,
    Excel,
    PowerPoint,
    Archive,
    Text,
    Other,
}

impl FileKind {
    /// The kind of a file with `extension` (any case, with or without its dot).
    #[must_use]
    pub fn of(extension: &str) -> Self {
        let extension = extension.trim().trim_start_matches('.').to_ascii_lowercase();
        match extension.as_str() {
            "mp4" | "mov" | "m4v" | "webm" | "avi" | "mkv" | "wmv" | "3gp" | "mpg" | "mpeg" => Self::Video,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "svg" => Self::Image,
            "mp3" | "wav" | "m4a" | "aac" | "ogg" | "flac" | "wma" | "opus" => Self::Audio,
            "pdf" => Self::Pdf,
            "doc" | "docx" | "docm" | "rtf" | "odt" => Self::Word,
            "xls" | "xlsx" | "xlsm" | "csv" | "ods" => Self::Excel,
            "ppt" | "pptx" | "pptm" | "odp" => Self::PowerPoint,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" => Self::Archive,
            "txt" | "md" | "log" | "json" | "xml" | "yaml" | "yml" => Self::Text,
            _ => Self::Other,
        }
    }

    /// Whether Teams draws a picture of it on its card (a video's frame, an image): the thumbnail is asked for only then.
    #[must_use]
    pub const fn has_thumbnail(self) -> bool {
        matches!(self, Self::Video | Self::Image)
    }
}

/// A file a message carries, as read from its entry in `properties.files`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SharedFile {
    /// The file's name, as its card shows it.
    pub name: String,
    /// Its extension, lowercase without the dot (`fileType`, else the name's own); empty when it has none.
    pub extension: String,
    /// The sharing link the web client opens it by (`fileInfo.shareUrl`), when it is a SharePoint `https` link.
    pub share_url: Option<String>,
    /// The file's own address in its library (`objectUrl`, else `fileInfo.fileUrl`), when it is a SharePoint `https`
    /// link.
    pub object_url: Option<String>,
    /// A picture of it on AMS (`filePreview`), when its link is a view of an AMS object a message's picture is read
    /// from ([`crate::MessagePicture::of`]).
    pub preview: Option<FilePreview>,
    /// Where it is kept, as its card's second line names it: the path of its site (`personal > <owner>`), when the
    /// entry names one ([`location_of`]).
    pub location: Option<String>,
    /// Its entry as it came (a JSON object), which a share of it to another chat posts again
    /// ([`crate::chat::share_entry`]); `None` for a file not read from an entry.
    pub entry: Option<String>,
}

/// A picture of a file (an image's), as its entry's `filePreview` names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FilePreview {
    /// The view of the AMS object (`previewUrl`), read as a message's picture is.
    pub src: String,
    /// Its size (`previewWidth`, `previewHeight`), when both are whole numbers from 1 to 100 000.
    pub size: Option<(u32, u32)>,
}

impl SharedFile {
    /// Its kind, by its extension.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        FileKind::of(&self.extension)
    }

    /// The link a click opens it by in the browser, as the web client does: the sharing link, else the file's own.
    #[must_use]
    pub fn open_link(&self) -> Option<&str> {
        self.share_url.as_deref().or(self.object_url.as_deref())
    }

    /// The link that has the browser download it: the sharing link with `download=1` (SharePoint's own way to have a
    /// sharing link hand the file over rather than show it); `None` without a sharing link.
    #[must_use]
    pub fn download_link(&self) -> Option<String> {
        let mut url = Url::parse(self.share_url.as_deref()?).ok()?;
        url.query_pairs_mut().append_pair("download", "1");
        Some(url.into())
    }

    /// The MRI of the account whose drive on an own server keeps it (`8:orgid:<id>`), when its sharing link (else its
    /// own) is in such a drive: `/sp/personal/<id without dashes>/…`. Its [`location`](Self::location) names that
    /// account by the id, which a view replaces with the account's name, as Teams names a site's owner.
    #[must_use]
    pub fn site_owner(&self) -> Option<String> {
        let link = Url::parse(self.open_link()?).ok()?;
        let mut parts = link.path_segments()?;
        let (Some("sp"), Some("personal"), Some(user)) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        let hex = user.to_ascii_lowercase();
        if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(format!(
            "8:orgid:{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ))
    }
}

/// What a message's `properties.files` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedFiles {
    /// The files read, in their order. A file with no link it is opened by is here and also in `unread`.
    pub files: Vec<SharedFile>,
    /// What couldn't be read, kept as it came: the whole value when it is not a list, a JSON string of one (also
    /// encoded twice) or one entry alone; else a list of the entries that weren't read, and of the files with no link.
    /// `None` when everything was read (or there was nothing).
    pub unread: Option<Value>,
}

/// Reads `properties.files` (`None`: there is none) as [`SharedFiles`].
#[must_use]
pub fn shared_files(value: Option<&Value>) -> SharedFiles {
    shared_files_in(&crate::Cloud::Microsoft, value)
}

/// Reads files using only links on the account's file and media services.
#[must_use]
pub fn shared_files_in(cloud: &crate::Cloud, value: Option<&Value>) -> SharedFiles {
    let list = match entries_of(value) {
        Ok(list) => list,
        Err(unread) => {
            return SharedFiles {
                files: Vec::new(),
                unread,
            };
        }
    };
    let mut read = Reading::default();
    for entry in list {
        read.add(read_entry(cloud, &entry), entry);
    }
    read.done()
}

/// Reads `properties.files` for a view that knows no account: an entry is read on Microsoft's services
/// ([`shared_files`]), else on the own server its links name ([`shared_files_in`] of that server). A file itself is
/// read only through its account's engine, which takes only its own cloud's links, so an entry on another server is
/// drawn and never fetched.
#[must_use]
pub fn shared_files_any(value: Option<&Value>) -> SharedFiles {
    let list = match entries_of(value) {
        Ok(list) => list,
        Err(unread) => {
            return SharedFiles {
                files: Vec::new(),
                unread,
            };
        }
    };
    let mut read = Reading::default();
    for entry in list {
        let found = match read_entry(&crate::Cloud::Microsoft, &entry) {
            Entry::File(file) => Entry::File(file),
            other => match entry
                .as_object()
                .and_then(own_cloud_of)
                .map(|cloud| read_entry(&cloud, &entry))
            {
                Some(Entry::File(file)) => Entry::File(file),
                _ => other,
            },
        };
        read.add(found, entry);
    }
    read.done()
}

/// What one entry of `properties.files` is.
enum Entry {
    /// A file, with a link it is opened by.
    File(SharedFile),
    /// A file named with no link this build takes: its card is drawn without one, and the entry is kept as unread too.
    Linkless(SharedFile),
    /// Not a file's entry.
    Not,
}

/// The files and the unread entries, as they are read one by one.
#[derive(Default)]
struct Reading {
    files: Vec<SharedFile>,
    unread: Vec<Value>,
}

impl Reading {
    fn add(&mut self, entry: Entry, raw: Value) {
        match entry {
            Entry::File(file) => self.files.push(file),
            Entry::Linkless(file) => {
                self.files.push(file);
                self.unread.push(raw);
            }
            Entry::Not => self.unread.push(raw),
        }
    }

    fn done(self) -> SharedFiles {
        SharedFiles {
            files: self.files,
            unread: (!self.unread.is_empty()).then_some(Value::Array(self.unread)),
        }
    }
}

/// The entries of `properties.files`: a list, or a JSON string of one (also when the string is itself a JSON string of
/// one, or holds a lone surrogate escape, which a browser reads and `serde_json` refuses), or one entry alone. `Err`
/// holds what is kept as unread: nothing for no value, else the whole value.
fn entries_of(value: Option<&Value>) -> Result<Vec<Value>, Option<Value>> {
    let Some(value) = value else {
        return Err(None);
    };
    let mut decoded = value.clone();
    // a JSON string of a list, at most twice over
    for _ in 0..2 {
        let Value::String(text) = &decoded else { break };
        let parsed = serde_json::from_str::<Value>(text)
            .ok()
            .or_else(|| serde_json::from_str::<Value>(&mend_lone_surrogates(text)?).ok());
        match parsed {
            Some(parsed) => decoded = parsed,
            None => break,
        }
    }
    match decoded {
        Value::Array(list) => Ok(list),
        Value::Object(entry) => Ok(vec![Value::Object(entry)]),
        _ => Err(Some(value.clone())),
    }
}

/// `text` with each `\u` escape of a lone UTF-16 surrogate replaced by `\uFFFD`, as a browser's JSON reader keeps
/// such a string; `None` when it has none.
fn mend_lone_surrogates(text: &str) -> Option<String> {
    let escape = |at: usize| -> Option<u16> {
        let digits = text.get(at + 2..at + 6)?;
        (text.get(at..at + 2)? == "\\u").then(|| u16::from_str_radix(digits, 16).ok())?
    };
    let mut mended = String::with_capacity(text.len());
    let mut changed = false;
    let mut at = 0;
    while let Some(offset) = text[at..].find('\\') {
        let start = at + offset;
        mended.push_str(&text[at..start]);
        match escape(start) {
            Some(0xD800..=0xDBFF) if escape(start + 6).is_some_and(|low| (0xDC00..=0xDFFF).contains(&low)) => {
                mended.push_str(&text[start..start + 12]);
                at = start + 12;
            }
            Some(0xD800..=0xDFFF) => {
                mended.push_str("\\uFFFD");
                changed = true;
                at = start + 6;
            }
            _ => {
                // any other escape, its next character with it: an escaped backslash never starts an escape
                let next = text[start + 1..].chars().next().map_or(0, char::len_utf8);
                mended.push_str(&text[start..start + 1 + next]);
                at = start + 1 + next;
            }
        }
    }
    mended.push_str(&text[at..]);
    changed.then_some(mended)
}

/// The own server an entry's links name: the origin of its sharing link, else of its own address.
pub(super) fn own_cloud_of(entry: &Map<String, Value>) -> Option<crate::Cloud> {
    let info = entry.get("fileInfo").and_then(Value::as_object);
    let link = info
        .and_then(|info| info.get("shareUrl"))
        .or_else(|| entry.get("objectUrl"))
        .or_else(|| info.and_then(|info| info.get("fileUrl")))?
        .as_str()?;
    let url = Url::parse(link.trim()).ok()?;
    crate::Cloud::own(&url.origin().ascii_serialization()).ok()
}

/// One entry, as the module docs read it.
fn read_entry(cloud: &crate::Cloud, entry: &Value) -> Entry {
    let Some(entry) = entry.as_object() else {
        return Entry::Not;
    };
    let Some(file) = file_of(cloud, entry) else {
        return Entry::Not;
    };
    if file.share_url.is_none() && file.object_url.is_none() {
        // never fetched: its picture on AMS is not kept either
        Entry::Linkless(SharedFile { preview: None, ..file })
    } else {
        Entry::File(file)
    }
}

/// Whether an entry's `@type` is a file's: [`FILE_TYPE`], another Skype schema type of a file (`…/<Kind>File`), or none.
fn is_file_type(kind: Option<&Value>) -> bool {
    match kind {
        None | Some(Value::Null) => true,
        Some(kind) => kind.as_str().is_some_and(|kind| {
            kind == FILE_TYPE
                || kind
                    .strip_prefix("http://schema.skype.com/")
                    .is_some_and(|name| name.ends_with("File") && name.bytes().all(|b| b.is_ascii_alphanumeric()))
        }),
    }
}

/// The file an entry names, with only the links of `cloud`'s file service kept.
fn file_of(cloud: &crate::Cloud, entry: &Map<String, Value>) -> Option<SharedFile> {
    fn text(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
    let info = entry.get("fileInfo").and_then(Value::as_object);
    if !is_file_type(entry.get("@type")) {
        return None;
    }
    // a file shared on from another chat comes with `state` null, as Teams posts it
    if entry
        .get("state")
        .is_some_and(|state| !state.is_null() && state.as_str() != Some("active"))
    {
        return None;
    }
    let name: String = text(entry.get("fileName"))
        .or_else(|| text(entry.get("title")))?
        .chars()
        .take(MAX_NAME)
        .collect();
    let link = |value: Option<&Value>| {
        text(value)
            .filter(|link| is_file_link_in(cloud, link))
            .map(str::to_owned)
    };
    let share_url = link(info.and_then(|info| info.get("shareUrl")));
    let object_url = link(entry.get("objectUrl")).or_else(|| link(info.and_then(|info| info.get("fileUrl"))));
    let extension = text(entry.get("fileType"))
        .or_else(|| text(entry.get("type")))
        .map(str::to_owned)
        .or_else(|| name.rsplit_once('.').map(|(_, extension)| extension.to_owned()))
        .unwrap_or_default()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let extension = if extension.len() <= 16 && extension.chars().all(|c| c.is_ascii_alphanumeric()) {
        extension
    } else {
        String::new()
    };
    Some(SharedFile {
        name,
        extension,
        share_url,
        object_url,
        preview: entry
            .get("filePreview")
            .and_then(Value::as_object)
            .and_then(|preview| read_preview(cloud, preview)),
        location: location_of(entry),
        entry: serde_json::to_string(entry).ok(),
    })
}

/// The longest location kept, in characters; a longer one is cut there.
const MAX_LOCATION: usize = 200;

/// Where an entry's file is kept, as Teams names it on the card: its `chicletBreadcrumbs` as Teams draws them
/// (`sourceTeamName > sourceChannelName`, lab capture 2026-11-24), else the path of its site (`fileInfo.siteUrl`, else
/// `baseUrl`, else `sharepointIds.siteUrl`; the first one with a path), each part decoded, joined by ` > `
/// (`personal > <owner>`). An own server's site path starts with `sp`, which is not a part of it. `None` when neither
/// names one.
#[must_use]
pub fn location_of(entry: &Map<String, Value>) -> Option<String> {
    breadcrumbs(entry).or_else(|| site_location(entry))
}

/// The entry's `chicletBreadcrumbs`, its team's and channel's names joined by ` > ` (either alone when the other is
/// empty); `None` when it has neither.
fn breadcrumbs(entry: &Map<String, Value>) -> Option<String> {
    let crumbs = entry.get("chicletBreadcrumbs")?.as_object()?;
    let parts: Vec<String> = ["sourceTeamName", "sourceChannelName"]
        .into_iter()
        .filter_map(|key| crumbs.get(key)?.as_str())
        .map(|part| part.chars().filter(|c| !c.is_control()).collect::<String>())
        .filter(|part| !part.trim().is_empty())
        .collect();
    let location: String = parts.join(" > ").chars().take(MAX_LOCATION).collect();
    (!location.is_empty()).then_some(location)
}

/// The path of the first site link of the entry that has one, as [`location_of`] names it.
fn site_location(entry: &Map<String, Value>) -> Option<String> {
    let info = entry.get("fileInfo").and_then(Value::as_object);
    let ids = entry.get("sharepointIds").and_then(Value::as_object);
    [
        info.and_then(|info| info.get("siteUrl")),
        entry.get("baseUrl"),
        ids.and_then(|ids| ids.get("siteUrl")),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_str)
    .find_map(site_path)
}

/// The path of one site link, as [`location_of`] names it; `None` when it has none.
fn site_path(link: &str) -> Option<String> {
    let site = Url::parse(link.trim()).ok()?;
    let mut parts: Vec<String> = site
        .path_segments()?
        .filter(|part| !part.is_empty())
        .map(|part| {
            percent_encoding::percent_decode_str(part)
                .decode_utf8_lossy()
                .chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
        })
        .filter(|part| !part.trim().is_empty())
        .collect();
    if parts.first().is_some_and(|part| part == "sp") && parts.len() > 1 {
        parts.remove(0);
    }
    let location: String = parts.join(" > ").chars().take(MAX_LOCATION).collect();
    (!location.is_empty()).then_some(location)
}

/// An entry's `filePreview`, when its `previewUrl` is a view of an AMS object.
fn read_preview(cloud: &crate::Cloud, preview: &Map<String, Value>) -> Option<FilePreview> {
    let src = preview.get("previewUrl")?.as_str()?.trim();
    crate::MessagePicture::of_in(cloud, src)?;
    let side = |key: &str| {
        preview
            .get(key)
            .and_then(Value::as_u64)
            .filter(|side| (1..=MAX_PREVIEW_SIDE).contains(side))
            .and_then(|side| u32::try_from(side).ok())
    };
    Some(FilePreview {
        src: src.to_owned(),
        size: side("previewWidth").zip(side("previewHeight")),
    })
}

/// Whether `link` is one a file is opened by: an `https` URL with no user info or port, on a SharePoint host
/// (`<tenant>.sharepoint.com`, `<tenant>-my.sharepoint.com`), at most [`MAX_LINK`] bytes.
#[must_use]
pub fn is_file_link(link: &str) -> bool {
    is_file_link_in(&crate::Cloud::Microsoft, link)
}

/// Whether a file link belongs to this account's file service.
#[must_use]
pub fn is_file_link_in(cloud: &crate::Cloud, link: &str) -> bool {
    if !cloud.is_microsoft() {
        return link.len() <= MAX_LINK
            && Url::parse(link).is_ok_and(|url| cloud.contains(&url) && url.path().starts_with("/sp/"));
    }
    if link.len() > MAX_LINK {
        return false;
    }
    let Ok(url) = Url::parse(link) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.host_str().is_some_and(is_sharepoint_host)
}

/// A SharePoint host: `<name>.sharepoint.com`, the name lowercase ASCII letters, digits and `-`.
#[must_use]
pub fn is_sharepoint_host(host: &str) -> bool {
    host.strip_suffix(".sharepoint.com").is_some_and(|name| {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    })
}
