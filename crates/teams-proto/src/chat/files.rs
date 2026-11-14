//! Files a message carries (`properties.files`, `docs/protocol/chat.md`, "Files: received"): each a file the sender
//! shared from their OneDrive or a team's SharePoint, named by its file name, its type and its links. Read, never
//! fetched: [`SharedFile`] keeps the name, the type and the two links a reader opens it by, and a view asks the engine
//! for its thumbnail ([`crate::FileThumbnails`]).
//!
//! - **The shape** (seen in a received message, written down as `tests/fixtures/chatsvc/files.json`): `properties.files`
//!   is a JSON string of a list; an entry's `@type` is `http://schema.skype.com/File`, its `fileName` (and `title`)
//!   the file's name, `fileType` (and `type`) its extension, `fileInfo.shareUrl` the sharing link the web client opens
//!   it by (`https://<tenant>-my.sharepoint.com/:v:/g/personal/<user>/<id>`), `objectUrl` (and `fileInfo.fileUrl`) the
//!   file's own address in its library, `state` `active`, `filePreview.previewUrl` empty for a video; for an image,
//!   the view of a picture of it on AMS (`https://<region>-api.asm.skype.com/v1/objects/<id>/views/imgo`) with its
//!   `previewWidth` and `previewHeight`, which Teams draws in the message in place of the file's card
//!   ([`SharedFile::preview`]).
//! - **Read strictly**: an entry is read only when its `@type` is the file's, it has a name, its `state` (when there is
//!   one) is `active`, and at least one of its links is an `https` URL on a SharePoint host ([`is_file_link`]). Any
//!   other entry, and a `files` that isn't a list, is kept as it came ([`SharedFiles::unread`]): a view says the
//!   message has content it can't show, never a guess at it.

use serde_json::{Map, Value};
use url::Url;

/// An entry's `@type`.
pub const FILE_TYPE: &str = "http://schema.skype.com/File";

/// The longest file name kept, in characters; a longer one is cut there.
const MAX_NAME: usize = 400;

/// The largest side a preview's given size may have, in pixels; a larger one is no size.
const MAX_PREVIEW_SIDE: u64 = 100_000;

/// The longest link kept, in bytes; an entry with only longer ones isn't read.
const MAX_LINK: usize = 4096;

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
}

/// What a message's `properties.files` holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SharedFiles {
    /// The entries read, in their order.
    pub files: Vec<SharedFile>,
    /// What couldn't be read, kept as it came: the whole value when it isn't a list (or a JSON string of one), else a
    /// list of the entries that weren't read. `None` when everything was read (or there was nothing).
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
    let Some(value) = value else {
        return SharedFiles::default();
    };
    let list = match value {
        Value::Array(list) => list.clone(),
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Array(list)) => list,
            _ => {
                return SharedFiles {
                    files: Vec::new(),
                    unread: Some(value.clone()),
                };
            }
        },
        other => {
            return SharedFiles {
                files: Vec::new(),
                unread: Some(other.clone()),
            };
        }
    };
    let mut files = Vec::new();
    let mut unread = Vec::new();
    for entry in list {
        match entry.as_object().and_then(|entry| read_entry(cloud, entry)) {
            Some(file) => files.push(file),
            None => unread.push(entry),
        }
    }
    SharedFiles {
        files,
        unread: (!unread.is_empty()).then_some(Value::Array(unread)),
    }
}

/// One entry, when it is a file's in the shape the module docs give.
fn read_entry(cloud: &crate::Cloud, entry: &Map<String, Value>) -> Option<SharedFile> {
    fn text(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
    let info = entry.get("fileInfo").and_then(Value::as_object);
    if text(entry.get("@type")) != Some(FILE_TYPE) {
        return None;
    }
    if entry.get("state").is_some_and(|state| state.as_str() != Some("active")) {
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
    if share_url.is_none() && object_url.is_none() {
        return None;
    }
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
    })
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
