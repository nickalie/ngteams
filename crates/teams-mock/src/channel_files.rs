//! A channel's files (`docs/protocol/channels.md`, "Files in a channel"): the folder a channel's thread names, and the
//! team site's REST steps the web client uploads by, `addStub` (400 with SharePoint's code when the folder has the
//! name), `StartUpload`, `FinishUpload`, a read of a file by its id and `recycle`. Each takes an account's token; what
//! was uploaded is kept ([`crate::ChatServiceMock::channel_files`]).

use std::collections::BTreeMap;

use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::{json, status};
use crate::world::State;

/// The mock's team site, on Microsoft's cloud.
pub const SITE: &str = "https://mock.sharepoint.com/sites/mockteam";

/// The site's path, the start of every server-relative path in it.
const SITE_PATH: &str = "/sites/mockteam";

/// The document library a channel's folder is in.
const LIBRARY: &str = "Shared Documents";

/// A file a channel's folder holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelFile {
    /// Its `UniqueId`.
    pub id: String,
    /// Its server-relative path (`/sites/mockteam/Shared Documents/<folder>/<name>`).
    pub path: String,
    pub bytes: Vec<u8>,
    /// Sent to the recycle bin.
    pub recycled: bool,
}

/// The files of the mock's channels, by `UniqueId`.
#[derive(Debug, Default)]
pub(crate) struct ChannelFiles {
    pub(crate) files: BTreeMap<String, ChannelFile>,
}

/// The folder keys a channel's thread carries, its folder named `folder`.
pub(crate) fn thread_properties(folder: &str) -> [(&'static str, Value); 4] {
    [
        ("sharepointSiteUrl", json!(SITE)),
        ("sharepointRootLibrary", json!(LIBRARY)),
        ("sharepointChannelDocsFolder", json!(folder)),
        (
            "channelDocsFolderRelativeUrl",
            json!(format!("{SITE_PATH}/{LIBRARY}/{folder}")),
        ),
    ]
}

/// The answer to `request` when it is one of the site's steps; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let rest = request.url.path().strip_prefix(&format!("{SITE_PATH}/_api/web/"))?;
    let rest = percent_decode_str(rest).decode_utf8().ok()?.into_owned();
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if token.and_then(|token| state.account_by_token(token)).is_none() {
        return Some(status(401));
    }
    let query: BTreeMap<String, String> = request.url.query_pairs().into_owned().collect();
    let method = request.method.as_str();
    let files = &mut state.channel_files.files;
    Some(
        if rest == "GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)" && method == "POST" {
            add_stub(files, &query)
        } else if let Some((id, step)) = by_id(&rest) {
            match (method, step) {
                ("GET", "") => files
                    .get(&id)
                    .filter(|file| !file.recycled)
                    .map_or_else(|| status(404), |file| json(200, &sp_file(file))),
                ("POST", step) if step.starts_with("/StartUpload(") => match files.get_mut(&id) {
                    Some(file) if !file.recycled => {
                        file.bytes.clone_from(&request.body);
                        json(200, &json!({"value": file.bytes.len().to_string()}))
                    }
                    _ => status(404),
                },
                ("POST", step) if step.starts_with("/FinishUpload(") => files
                    .get(&id)
                    .filter(|file| !file.recycled)
                    .map_or_else(|| status(404), |file| json(200, &sp_file(file))),
                ("DELETE", "/recycle") => match files.get_mut(&id) {
                    Some(file) if !file.recycled => {
                        file.recycled = true;
                        status(200)
                    }
                    _ => status(404),
                },
                _ => status(404),
            }
        } else {
            status(404)
        },
    )
}

/// `addStub`: an empty file of the name, or SharePoint's 400 when the folder has one of the name.
fn add_stub(files: &mut BTreeMap<String, ChannelFile>, query: &BTreeMap<String, String>) -> ResponseTemplate {
    let (Some(folder), Some(name)) = (literal(query.get("@v")), literal(query.get("@fn"))) else {
        return status(400);
    };
    if !folder.starts_with(&format!("{SITE_PATH}/{LIBRARY}/")) || name.contains('/') {
        return status(400);
    }
    let path = format!("{folder}/{name}");
    if files.values().any(|file| file.path == path && !file.recycled) {
        return json(
            400,
            &json!({"odata.error": {"code": "-2130575257, Microsoft.SharePoint.SPException",
                "message": {"lang": "en-US", "value": format!("A file with the name {path} already exists.")}}}),
        );
    }
    let id = format!("00000000-0000-4000-8000-{:012x}", files.len() + 1);
    let file = ChannelFile {
        id: id.clone(),
        path,
        bytes: Vec::new(),
        recycled: false,
    };
    let answer = json(200, &sp_file(&file));
    files.insert(id, file);
    answer
}

/// `GetFileById('<id>')<rest>`: the id and the rest.
fn by_id(rest: &str) -> Option<(String, &str)> {
    let rest = rest.strip_prefix("GetFileById('")?;
    let (id, rest) = rest.split_once("')")?;
    Some((id.to_ascii_lowercase(), rest))
}

/// An `OData` string literal's text (`'…'`, a `''` a quote).
fn literal(value: Option<&String>) -> Option<String> {
    let inner = value?.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(inner.replace("''", "'"))
}

fn sp_file(file: &ChannelFile) -> Value {
    let name = file.path.rsplit('/').next().unwrap_or_default();
    json!({"odata.type": "SP.File", "UniqueId": file.id, "Name": name, "ServerRelativeUrl": file.path,
        "Length": file.bytes.len().to_string(), "Exists": true})
}
