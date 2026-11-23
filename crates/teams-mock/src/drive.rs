//! Each account's own OneDrive, for a file sent in a chat (`docs/protocol/chat.md`, "Files"): Graph's `me/drive`, `me`
//! and `users/<id>`, the middle tier's short profiles of the people a file is shared with, and the personal site's
//! drive, its "Microsoft Teams Chat Files" folder, the content `PUT`, a file's permissions and its `invite`. Each takes
//! an account's token. An upload's answer has the shape SharePoint gives: an Office file's `webUrl` is its `Doc.aspx`
//! viewer link and its `webDavUrl` the path; any other file has the path in both. A test can change the next upload's
//! answer ([`DriveTwist`]); what was uploaded and whom it was shared with is kept ([`crate::ChatServiceMock::drive_files`]).

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::{Request, ResponseTemplate};

use crate::chatsvc::{json, status};
use crate::world::State;

/// The host of every account's personal site.
pub const DRIVE_HOST: &str = "https://mock-my.sharepoint.com";

/// The folder Teams puts the files sent in chats in.
const FOLDER: &str = "Microsoft%20Teams%20Chat%20Files";

/// How long a "never answers" upload waits: past every limit a test gives an upload.
const NEVER: Duration = Duration::from_hours(1);

/// A change to the next upload's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DriveTwist {
    /// The answer comes after this time.
    Slow(Duration),
    /// No answer comes.
    Never,
    /// The answer's `webDavUrl` is this address.
    WebDavUrl(String),
}

/// A file on an account's drive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveFile {
    /// The account that uploaded it.
    pub owner: String,
    pub name: String,
    /// Its `listItemUniqueId`.
    pub id: String,
    /// The path a message posts (`webDavUrl`).
    pub path: String,
    pub size: usize,
    /// The people it is shared with, by MRI.
    pub shared_with: Vec<String>,
}

/// The files of every drive and the twists queued for the next uploads.
#[derive(Debug, Default)]
pub(crate) struct Drives {
    pub(crate) files: BTreeMap<String, DriveFile>,
    pub(crate) twists: VecDeque<DriveTwist>,
}

/// An account's object id: its MRI after `8:orgid:`.
fn object_id(mri: &str) -> &str {
    mri.strip_prefix("8:orgid:").unwrap_or(mri)
}

/// The made-up address of account `mri`.
fn address(mri: &str) -> String {
    format!("user-{}@mock.example.test", object_id(mri))
}

/// The personal site's path segment of account `mri`.
fn personal(mri: &str) -> String {
    format!("lab_{}", object_id(mri).replace('-', "_"))
}

fn is_office(name: &str) -> bool {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    ["xlsx", "docx", "pptx", "xls", "doc", "ppt"]
        .iter()
        .any(|office| office.eq_ignore_ascii_case(extension))
}

/// The answer to `request` when it is one of the drive's steps; `None` for any other path.
pub(crate) fn serve(state: &mut State, request: &Request) -> Option<ResponseTemplate> {
    let path = request.url.path().to_owned();
    let method = request.method.as_str();
    let mine = matches!(path.as_str(), "/v1.0/me" | "/v1.0/me/drive")
        || path
            .strip_prefix("/v1.0/users/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        || path.ends_with("/beta/users/fetchShortProfile")
        || path.starts_with("/personal/");
    if !mine {
        return None;
    }
    let token = request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(caller) = token
        .and_then(|token| state.account_by_token(token))
        .map(|account| account.mri.clone())
    else {
        return Some(status(401));
    };
    Some(match (method, path.as_str()) {
        ("GET", "/v1.0/me/drive") => json(
            200,
            &json!({"id": format!("drive-{}", object_id(&caller)), "driveType": "business",
                "owner": {"user": {"id": object_id(&caller)}},
                "webUrl": format!("{DRIVE_HOST}/personal/{}/Documents", personal(&caller))}),
        ),
        ("GET", "/v1.0/me") => json(
            200,
            &json!({"id": object_id(&caller), "userPrincipalName": address(&caller)}),
        ),
        ("GET", user) if user.starts_with("/v1.0/users/") => {
            let id = user.trim_start_matches("/v1.0/users/");
            match state.accounts.iter().find(|account| object_id(&account.mri) == id) {
                Some(account) => json(
                    200,
                    &json!({"id": id, "userPrincipalName": address(&account.mri), "mail": address(&account.mri)}),
                ),
                None => status(404),
            }
        }
        ("POST", _) if path.ends_with("/beta/users/fetchShortProfile") => short_profiles(state, &request.body),
        _ => site(state, &caller, request)?,
    })
}

/// The middle tier's profiles of the MRIs the body names.
fn short_profiles(state: &State, body: &[u8]) -> ResponseTemplate {
    let asked: Value = serde_json::from_slice(body).unwrap_or_default();
    let mut names = Vec::new();
    collect_mris(&asked, &mut names);
    let value: Vec<Value> = state
        .accounts
        .iter()
        .filter(|account| names.contains(&account.mri))
        .map(|account| {
            json!({"mri": account.mri, "displayName": account.display_name, "email": address(&account.mri),
                "userPrincipalName": address(&account.mri), "objectId": object_id(&account.mri)})
        })
        .collect();
    json(
        200,
        &json!({"type": "Microsoft.SkypeSpaces.MiddleTier.Models.IUserIdentity", "value": value}),
    )
}

fn collect_mris(value: &Value, names: &mut Vec<String>) {
    match value {
        Value::String(text) if text.starts_with("8:") => names.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| collect_mris(item, names)),
        Value::Object(map) => map.values().for_each(|item| collect_mris(item, names)),
        _ => {}
    }
}

/// A step on the caller's personal site; `None` when the path is on no site of the caller.
fn site(state: &mut State, caller: &str, request: &Request) -> Option<ResponseTemplate> {
    let base = format!("/personal/{}/_api/v2.0", personal(caller));
    let rest = request.url.path().strip_prefix(&base)?.to_owned();
    let owner = || json!({"id": "owner", "roles": ["owner"], "grantedToV2": {"user": {"id": object_id(caller)}}});
    let folder = format!("/drive/root:/{FOLDER}");
    Some(match (request.method.as_str(), rest.as_str()) {
        ("GET", "/drive") => json(200, &json!({"owner": {"user": {"id": object_id(caller)}}})),
        ("GET", path) if path == folder => {
            json(200, &json!({"id": "chat-files", "name": "Microsoft Teams Chat Files"}))
        }
        ("GET", path) if path == format!("{folder}:/permissions") => json(200, &json!({"value": [owner()]})),
        ("PUT", path) if path.starts_with(&format!("{folder}/")) && path.ends_with(":/content") => {
            let name = path[folder.len() + 1..path.len() - ":/content".len()].to_owned();
            let rename = request
                .url
                .query_pairs()
                .any(|(key, value)| key == "@name.conflictBehavior" && value == "rename");
            upload(state, caller, &name, request.body.len(), rename)
        }
        ("GET", path) if path.starts_with("/sites/root/items/") && path.ends_with("/driveItem/permissions") => {
            let id = item_of(path, "/driveItem/permissions");
            match state.drive.files.get(&id).filter(|file| file.owner == caller) {
                Some(file) => {
                    let mut grants = vec![owner()];
                    grants.extend(file.shared_with.iter().map(|mri| {
                        json!({"id": format!("grant-{}", object_id(mri)), "roles": ["write"],
                            "grantedToV2": {"user": {"id": object_id(mri)}}})
                    }));
                    json(200, &json!({"value": grants}))
                }
                None => status(404),
            }
        }
        ("POST", path) if path.starts_with("/sites/root/items/") && path.ends_with("/driveItem/invite") => {
            invite(state, caller, &item_of(path, "/driveItem/invite"), &request.body)
        }
        _ => status(404),
    })
}

fn item_of(path: &str, end: &str) -> String {
    path.trim_start_matches("/sites/root/items/")
        .trim_end_matches(end)
        .to_ascii_lowercase()
}

/// The content `PUT` of `name` (still percent-encoded) by `caller`: the file kept, and SharePoint's 201. With `rename`,
/// a name already in the folder gets the next free number, as SharePoint gives it.
fn upload(state: &mut State, caller: &str, name: &str, size: usize, rename: bool) -> ResponseTemplate {
    let asked = percent_encoding::percent_decode_str(name)
        .decode_utf8_lossy()
        .into_owned();
    let taken = |candidate: &str| {
        state
            .drive
            .files
            .values()
            .any(|file| file.owner == caller && file.name.eq_ignore_ascii_case(candidate))
    };
    let mut name = asked.clone();
    let mut number = 0;
    while rename && taken(&name) {
        number += 1;
        name = numbered(&asked, number);
    }
    let twist = state.drive.twists.pop_front();
    let id = format!("00000000-0000-4000-9000-{:012x}", state.drive.files.len() + 1);
    let base = format!("{DRIVE_HOST}/personal/{}/", personal(caller));
    let encoded: String = percent_encoding::utf8_percent_encode(&name, percent_encoding::NON_ALPHANUMERIC)
        .to_string()
        .replace("%2E", ".");
    let path = format!("{base}Documents/{FOLDER}/{encoded}");
    let web_url = if is_office(&name) {
        format!("{base}_layouts/15/Doc.aspx?sourcedoc=%7B{id}%7D&file={encoded}&action=default&mobileredirect=true")
    } else {
        path.clone()
    };
    let web_dav_url = match &twist {
        Some(DriveTwist::WebDavUrl(url)) => url.clone(),
        _ => path.clone(),
    };
    state.drive.files.insert(
        id.clone(),
        DriveFile {
            owner: caller.to_owned(),
            name: name.clone(),
            id: id.clone(),
            path,
            size,
            shared_with: Vec::new(),
        },
    );
    let answer = json(
        201,
        &json!({"id": format!("ITEM{}", state.drive.files.len()), "name": name, "size": size,
            "webUrl": web_url, "webDavUrl": web_dav_url, "parentReference": {"driveId": format!("drive-{}", object_id(caller))},
            "sharepointIds": {"listItemUniqueId": id}}),
    );
    match twist {
        Some(DriveTwist::Slow(delay)) => answer.set_delay(delay),
        Some(DriveTwist::Never) => answer.set_delay(NEVER),
        _ => answer,
    }
}

/// `name` with `number` after its stem.
fn numbered(name: &str, number: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} {number}.{extension}"),
        _ => format!("{name} {number}"),
    }
}

/// `invite`: each recipient whose address is an account's gets write access.
fn invite(state: &mut State, caller: &str, id: &str, body: &[u8]) -> ResponseTemplate {
    let body: Value = serde_json::from_slice(body).unwrap_or_default();
    let emails: Vec<String> = body["recipients"]
        .as_array()
        .map(|recipients| {
            recipients
                .iter()
                .filter_map(|recipient| recipient["email"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let people: Vec<String> = state
        .accounts
        .iter()
        .filter(|account| emails.contains(&address(&account.mri)))
        .map(|account| account.mri.clone())
        .collect();
    match state.drive.files.get_mut(id).filter(|file| file.owner == caller) {
        Some(file) if people.len() == emails.len() => {
            for person in people {
                if !file.shared_with.contains(&person) {
                    file.shared_with.push(person);
                }
            }
            json(200, &json!({"value": []}))
        }
        Some(_) => status(400),
        None => status(404),
    }
}

#[cfg(test)]
mod tests {
    use super::numbered;

    #[test]
    fn a_numbered_name_keeps_the_extension() {
        assert_eq!(numbered("made-up report.docx", 1), "made-up report 1.docx");
        assert_eq!(numbered("made-up notes", 2), "made-up notes 2");
        assert_eq!(numbered(".profile", 1), ".profile 1");
    }
}
