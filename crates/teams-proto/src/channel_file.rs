//! A channel's files: uploaded into the channel's folder on its team's SharePoint site, as Teams' web client uploads
//! them (`docs/protocol/channels.md`, "Files in a channel"). The team's site gives the team its access, so a channel
//! file gets no share and no grant: the message names the file and every member of the team can open it.
//!
//! The folder comes only from the channel's own thread on the chat service (`sharepointSiteUrl`,
//! `channelDocsFolderRelativeUrl`), read again before each write ([`crate::HttpTransport`]'s preflight), so a write
//! reaches no folder but that channel's, and a write that names a file reaches only a file in that folder.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::Value;
use url::Url;

use crate::write::RawBody;
use crate::{Endpoint, Method, Read, Service};

/// What an `OData` string in a query is encoded with: all but the unreserved characters, as the web client sends it.
const QUERY: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');

/// The longest server-relative folder path or file path taken.
const MAX_PATH: usize = 400;

/// The SharePoint error code of an `addStub` whose name the folder has already (an answer of 400).
pub const NAME_TAKEN: &str = "-2130575257";

/// A team's SharePoint site: `https://<tenant>.sharepoint.com/sites/<name>` (or `/teams/<name>`), or the same path
/// under an own server's `/sp`. Its token goes only to this site's origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamSite {
    /// The site's address with a trailing `/`.
    base: String,
}

impl TeamSite {
    /// The team site at `url`, checked against the account's cloud.
    #[must_use]
    pub fn from_url_in(cloud: &crate::Cloud, url: &str) -> Option<Self> {
        let parsed = Url::parse(url).ok()?;
        let shaped = if cloud.is_microsoft() {
            parsed.clone()
        } else {
            cloud.project(&parsed, "/sp", "https://own.sharepoint.com")?
        };
        let host = shaped.host_str()?;
        let parts: Vec<_> = shaped.path_segments()?.filter(|part| !part.is_empty()).collect();
        let named = |name: &str| {
            !name.is_empty()
                && name.len() <= 128
                && name != "."
                && name != ".."
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        };
        if shaped.scheme() != "https"
            || shaped.port().is_some()
            || !shaped.username().is_empty()
            || shaped.password().is_some()
            || shaped.query().is_some()
            || shaped.fragment().is_some()
            || !crate::chat::is_sharepoint_host(host)
            || host.ends_with("-my.sharepoint.com")
            || parts.len() != 2
            || !matches!(parts[0], "sites" | "teams")
            || !named(parts[1])
        {
            return None;
        }
        let mut base = parsed;
        base.set_query(None);
        let path = format!("{}/", base.path().trim_end_matches('/'));
        base.set_path(&path);
        Some(Self { base: base.into() })
    }

    /// The site's address with a trailing `/`.
    #[must_use]
    pub fn base(&self) -> &str {
        &self.base
    }

    /// The site's address without the trailing `/`, as a channel file's entry names it.
    #[must_use]
    pub fn url(&self) -> &str {
        self.base.trim_end_matches('/')
    }

    /// The site's path on its host (`/sites/<name>`): the start of every server-relative path in it.
    #[must_use]
    pub fn path(&self) -> String {
        Url::parse(&self.base).map_or_else(|_| String::new(), |url| url.path().trim_end_matches('/').to_owned())
    }

    /// The origin its token is for.
    #[must_use]
    pub fn audience(&self) -> String {
        Url::parse(&self.base).map_or_else(|_| String::new(), |url| url.origin().ascii_serialization())
    }

    /// Whether `url` is on this site.
    #[must_use]
    pub fn holds(&self, url: &Url) -> bool {
        url.as_str().starts_with(&self.base) && url.username().is_empty() && url.password().is_none()
    }
}

/// A channel's folder on its team's site: the channel (its conversation id), the site, and the folder's
/// server-relative path (`/sites/<name>/Shared Documents/<channel folder>`), decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelFolder {
    channel: String,
    site: TeamSite,
    path: String,
}

impl ChannelFolder {
    /// The folder the channel's thread (`v1/threads/<channel>`, its `properties`) names: `sharepointSiteUrl` and
    /// `channelDocsFolderRelativeUrl`, else `sharepointRootLibrary` and `sharepointChannelDocsFolder` under the site.
    /// `None` when the thread isn't `channel`'s, names no site of the cloud, or a folder outside its site.
    #[must_use]
    pub fn from_thread_in(cloud: &crate::Cloud, channel: &str, thread: &Value) -> Option<Self> {
        if !crate::is_team_thread_id(channel) || thread.get("id").and_then(Value::as_str) != Some(channel) {
            return None;
        }
        let properties = thread.get("properties")?;
        let text = |key: &str| {
            properties
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        };
        let site = TeamSite::from_url_in(cloud, text("sharepointSiteUrl")?)?;
        let path = match text("channelDocsFolderRelativeUrl") {
            Some(path) => path.to_owned(),
            None => format!(
                "{}/{}/{}",
                site.path(),
                text("sharepointRootLibrary")?,
                text("sharepointChannelDocsFolder")?
            ),
        };
        let path = path.trim_end_matches('/').to_owned();
        let inside = path.strip_prefix(&format!("{}/", site.path()))?;
        if path.len() > MAX_PATH || inside.split('/').count() < 2 || !inside.split('/').all(valid_name) {
            return None;
        }
        Some(Self {
            channel: channel.to_owned(),
            site,
            path,
        })
    }

    /// The channel (a team thread's id) the folder is of.
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    #[must_use]
    pub const fn site(&self) -> &TeamSite {
        &self.site
    }

    /// The folder's server-relative path, decoded.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether a server-relative path names a file right in this folder.
    #[must_use]
    pub fn holds_path(&self, path: &str) -> bool {
        path.strip_prefix(&format!("{}/", self.path))
            .is_some_and(|name| !name.contains('/') && valid_name(name))
    }

    /// The address of a file in the folder, as a channel file's entry names it (`objectUrl`, `fileUrl`): the site's
    /// origin and the server-relative path, as SharePoint gives it.
    fn file_url(&self, path: &str) -> String {
        format!("{}{path}", self.site.audience())
    }

    /// The site's REST address of a file of it by its id (`GetFileById('<id>')`).
    fn file_by_id(&self, file: &str) -> Option<Url> {
        if !crate::read::is_guid(file) {
            return None;
        }
        Url::parse(&format!("{}_api/web/GetFileById('{file}')", self.site.base())).ok()
    }

    /// The read of a file of the folder by its id: its server-relative path, which the transport checks is in the
    /// folder before a write names that file.
    #[must_use]
    pub fn file_read(&self, file: &str) -> Option<Url> {
        let mut url = self.file_by_id(file)?;
        url.set_query(Some("$select=ServerRelativeUrl,UniqueId"));
        Some(url)
    }
}

/// The channel's thread on the chat service, read for its folder ([`ChannelFolder::from_thread_in`]).
pub struct ChannelThread {
    channel: String,
}

impl ChannelThread {
    /// The read of `channel`'s thread; `None` for an id that isn't a team thread's.
    #[must_use]
    pub fn of(channel: &str) -> Option<Self> {
        crate::is_team_thread_id(channel).then(|| Self {
            channel: channel.to_owned(),
        })
    }
}

impl Read for ChannelThread {
    fn service(&self) -> Service {
        Service::ChatService
    }
    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "threads", self.channel.as_str()]).with_query("view", "msnp24Equivalent")
    }
}

/// Which step of a channel file's upload, or its removal, a [`ChannelFileWrite`] is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelFileStep {
    /// An empty file of the name in the folder (`addStub`); a 400 with [`NAME_TAKEN`] when the folder has the name.
    AddStub,
    /// The file's bytes into it (`StartUpload`); answers the bytes taken.
    StartUpload,
    /// The upload ended at the offset (`FinishUpload`); answers the file.
    FinishUpload,
    /// The file to the site's recycle bin (`recycle`), as Remove does in the web client.
    Recycle,
}

/// One write of a channel file on its team's site, checked again by the transport before it goes: the channel's
/// folder as the chat service names it now, and (except [`ChannelFileStep::AddStub`]) the file in that folder.
#[derive(Clone, Debug)]
pub struct ChannelFileWrite {
    folder: ChannelFolder,
    step: ChannelFileStep,
    file: Option<String>,
    upload: Option<String>,
    url: Url,
    method: Method,
    body: RawBody,
}

impl ChannelFileWrite {
    /// An empty file named `name` in the folder; `None` for a name SharePoint can't hold.
    #[must_use]
    pub fn add_stub(folder: &ChannelFolder, name: &str) -> Option<Self> {
        if !valid_name(name) || folder.path.len() + name.len() + 1 > MAX_PATH {
            return None;
        }
        let mut url = Url::parse(&format!(
            "{}_api/web/GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)",
            folder.site.base()
        ))
        .ok()?;
        url.set_query(Some(&format!("@v={}&@fn={}", odata(&folder.path), odata(name))));
        Some(Self::new(
            folder,
            ChannelFileStep::AddStub,
            None,
            None,
            url,
            Method::Post,
            Vec::new(),
        ))
    }

    /// The bytes of the file `file` (named `name`, made by [`Self::add_stub`]), sent whole in one fresh upload.
    #[must_use]
    pub fn start_upload(folder: &ChannelFolder, file: &str, name: &str, bytes: Vec<u8>) -> Option<Self> {
        if bytes.len() as u64 > crate::MAX_ATTACHMENT_SIZE || !valid_name(name) {
            return None;
        }
        let upload = crate::search::fresh_id();
        let mut url = Url::parse(&format!(
            "{}/StartUpload(uploadId=guid'{upload}')",
            folder.file_by_id(file)?
        ))
        .ok()?;
        url.set_query(Some(&located(folder, name)));
        Some(Self::new(
            folder,
            ChannelFileStep::StartUpload,
            Some(file),
            Some(upload),
            url,
            Method::Post,
            bytes,
        ))
    }

    /// The end of the upload `start` began, at `offset` bytes (all of them).
    #[must_use]
    pub fn finish_upload(start: &Self, name: &str, offset: u64) -> Option<Self> {
        let (file, upload) = (start.file.as_deref()?, start.upload.as_deref()?);
        if start.step != ChannelFileStep::StartUpload || !valid_name(name) {
            return None;
        }
        let mut url = Url::parse(&format!(
            "{}/FinishUpload(uploadId=guid'{upload}',fileOffset={offset})",
            start.folder.file_by_id(file)?
        ))
        .ok()?;
        url.set_query(Some(&located(&start.folder, name)));
        Some(Self::new(
            &start.folder,
            ChannelFileStep::FinishUpload,
            Some(file),
            Some(upload.to_owned()),
            url,
            Method::Post,
            Vec::new(),
        ))
    }

    /// The file `file` of the folder to the site's recycle bin.
    #[must_use]
    pub fn recycle(folder: &ChannelFolder, file: &str) -> Option<Self> {
        let url = Url::parse(&format!("{}/recycle", folder.file_by_id(file)?)).ok()?;
        Some(Self::new(
            folder,
            ChannelFileStep::Recycle,
            Some(file),
            None,
            url,
            Method::Delete,
            Vec::new(),
        ))
    }

    fn new(
        folder: &ChannelFolder,
        step: ChannelFileStep,
        file: Option<&str>,
        upload: Option<String>,
        url: Url,
        method: Method,
        body: Vec<u8>,
    ) -> Self {
        Self {
            folder: folder.clone(),
            step,
            file: file.map(str::to_owned),
            upload,
            url,
            method,
            body: body.into(),
        }
    }

    #[must_use]
    pub const fn folder(&self) -> &ChannelFolder {
        &self.folder
    }

    #[must_use]
    pub const fn step(&self) -> &ChannelFileStep {
        &self.step
    }

    /// The file the write names (its `UniqueId`); `None` for [`ChannelFileStep::AddStub`].
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        self.file.as_deref()
    }

    #[must_use]
    pub const fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub const fn method(&self) -> Method {
        self.method
    }

    #[must_use]
    pub fn body(&self) -> &[u8] {
        self.body.bytes()
    }
}

/// The file an `addStub` or `FinishUpload` answered (`SP.File`): its id, name, path and length, checked to be right in
/// `folder`.
#[must_use]
pub fn uploaded_file(folder: &ChannelFolder, body: &[u8]) -> Option<crate::UploadedFile> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let guid = body.get("UniqueId")?.as_str()?;
    let name = body.get("Name")?.as_str()?;
    let path = body.get("ServerRelativeUrl")?.as_str()?;
    let size = match body.get("Length")? {
        Value::String(length) => length.parse().ok()?,
        length => length.as_u64()?,
    };
    if !crate::read::is_guid(guid) || path != format!("{}/{name}", folder.path) || !folder.holds_path(path) {
        return None;
    }
    Some(crate::UploadedFile::in_channel(
        folder,
        guid,
        name,
        &folder.file_url(path),
        size,
    ))
}

/// The server-relative path of a file a read by id answered ([`ChannelFolder::file_read`]), when its id is `file`.
#[must_use]
pub fn read_file_path(body: &[u8], file: &str) -> Option<String> {
    let body: Value = serde_json::from_slice(body).ok()?;
    let id = body.get("UniqueId")?.as_str()?;
    id.eq_ignore_ascii_case(file)
        .then(|| body.get("ServerRelativeUrl")?.as_str().map(str::to_owned))?
}

/// Whether an answer of 400 to an `addStub` says the folder has the name already.
#[must_use]
pub fn is_name_taken(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|body| body.pointer("/odata.error/code")?.as_str().map(str::to_owned))
        .is_some_and(|code| code.starts_with(NAME_TAKEN))
}

/// The name the web client tries after `name` is taken `attempt` times: `notes 1.txt`, `notes 2.txt`, ….
#[must_use]
pub fn numbered_name(name: &str, attempt: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} {attempt}.{extension}"),
        _ => format!("{name} {attempt}"),
    }
}

/// `@folder` and `@file`, which the web client adds to the steps after `addStub`.
fn located(folder: &ChannelFolder, name: &str) -> String {
    format!("@folder={}&@file={}", odata(&folder.path), odata(name))
}

/// An `OData` string literal (`'…'`, a `'` doubled) as a query value.
fn odata(text: &str) -> String {
    utf8_percent_encode(&format!("'{}'", text.replace('\'', "''")), QUERY).to_string()
}

/// A file or folder name SharePoint can hold: none of `"*:<>?/\|#%`, no control character, not `.` or `..`, not
/// starting or ending with a space.
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name.trim() == name
        && !name.contains(['/', '\\', '?', ':', '*', '"', '<', '>', '|', '#', '%'])
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
#[path = "channel_file_tests.rs"]
mod tests;
