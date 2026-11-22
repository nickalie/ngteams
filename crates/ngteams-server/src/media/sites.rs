//! A team's site under `/sp/sites/<site>/`, in the SharePoint REST shapes Teams uploads a channel's files with
//! (`docs/protocol/channels.md`, "Files in a channel"): an empty file in the channel's folder (`addStub`), its bytes
//! (`StartUpload`), the end of the upload (`FinishUpload`), a read of a file by its id, and `recycle`.
//!
//! A team has one site, named by its group id, and each channel one folder in the site's `Shared Documents`. The
//! channel's thread names the folder ([`thread_properties`]). A file is reached only by an account in its channel's
//! thread now, so a private channel's files stay its members'. Nothing is shared or granted: a member reads a file
//! through Graph by its address ([`super::graph`]) and the signed links of [`super::drive`]. The bytes are kept
//! under a drive item's kind of key (the ids are UUIDs, so they never meet a drive item's), and count against the
//! account that added the file, as a chat file's do.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use futures_util::{StreamExt as _, TryStreamExt as _};
use percent_encoding::percent_decode_str;
use serde_json::{Value, json};
use url::Url;

use super::site_repo::{self, ChannelFile, Filled};
use super::{Error, account, drive, quota, repo, thumbs};
use crate::app::App;
use crate::db::DbError;
use crate::db::users::User;
use crate::teams::repo::{self as teams, Channel, Team};

/// The document library the channels' folders are in, and the general channel's folder, as Teams names them.
const LIBRARY: &str = "Shared Documents";
const GENERAL: &str = "General";
const ADD_STUB: &str = "GetFolderByServerRelativeUrl(@v)/Files/addStub(@fn)";
/// SharePoint's error code of an `addStub` whose name the folder has already.
const NAME_TAKEN: &str = "-2130575257, Microsoft.SharePoint.SPException";

pub(super) fn router() -> Router<Arc<App>> {
    Router::new().route("/sp/sites/{site}/_api/web/{*rest}", any(api))
}

/// The team's site name: its group id, which is unique and has only the characters a site name takes.
fn site_name(team: &Team) -> &str {
    &team.group_id
}

/// The channel's folder name: `General` for the general channel; else the channel's name, or a name made of its id
/// when the name has a character a folder can't have.
fn folder_name(channel: &Channel) -> String {
    if channel.general() {
        GENERAL.to_owned()
    } else if valid_name(&channel.name) && !channel.name.eq_ignore_ascii_case(GENERAL) {
        channel.name.clone()
    } else {
        let id: String = channel.id.chars().filter(char::is_ascii_alphanumeric).collect();
        format!("Channel {id}")
    }
}

fn site_path(site: &str) -> String {
    format!("/sp/sites/{site}")
}

fn folder_path(site: &str, folder: &str) -> String {
    format!("{}/{LIBRARY}/{folder}", site_path(site))
}

/// The keys a channel's thread names its folder by (`sharepointSiteUrl`, …), as the chat service's thread has them.
pub(crate) fn thread_properties(app: &App, team: &Team, channel: &Channel) -> [(&'static str, Value); 4] {
    let site = site_name(team);
    let folder = folder_name(channel);
    [
        (
            "sharepointSiteUrl",
            json!(format!("{}{}", app.config.origin(), site_path(site))),
        ),
        ("sharepointRootLibrary", json!(LIBRARY)),
        ("channelDocsFolderRelativeUrl", json!(folder_path(site, &folder))),
        ("sharepointChannelDocsFolder", json!(folder)),
    ]
}

/// A file or folder name SharePoint holds, as the client's own rule has it: one path segment, none of
/// `"*:<>?/\|#%`, no control character, no space at its ends, not `.` or `..`.
fn valid_name(name: &str) -> bool {
    (1..=255).contains(&name.len())
        && name != "."
        && name != ".."
        && name.trim() == name
        && !name.contains(['/', '\\', '?', ':', '*', '"', '<', '>', '|', '#', '%'])
        && !name.chars().any(char::is_control)
}

/// One step on a file by its id (`GetFileById('<id>')<step>`).
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Read,
    Start(String),
    Finish(String, u64),
    Recycle,
}

/// The file id and the step of a path under `_api/web/`; `None` for another path.
fn file_step(rest: &str) -> Option<(String, Step)> {
    let (id, step) = rest.strip_prefix("GetFileById('")?.split_once("')")?;
    let id = guid(id)?;
    let step = match step {
        "" => Step::Read,
        "/recycle" => Step::Recycle,
        _ => {
            if let Some(args) = step
                .strip_prefix("/StartUpload(")
                .and_then(|args| args.strip_suffix(')'))
            {
                Step::Start(upload_id(args)?)
            } else {
                let args = step.strip_prefix("/FinishUpload(")?.strip_suffix(')')?;
                let (upload, offset) = args.split_once(',')?;
                let offset = offset.strip_prefix("fileOffset=")?.parse().ok()?;
                Step::Finish(upload_id(upload)?, offset)
            }
        }
    };
    Some((id, step))
}

fn upload_id(argument: &str) -> Option<String> {
    guid(argument.strip_prefix("uploadId=guid'")?.strip_suffix('\'')?)
}

/// A GUID in its hyphenated form, lowercase.
fn guid(text: &str) -> Option<String> {
    let id = uuid::Uuid::try_parse(text).ok()?;
    (text.len() == 36).then(|| id.hyphenated().to_string())
}

/// The values of a query, each percent-decoded (a `+` stays a `+`, as SharePoint reads it).
fn query_values(query: &str) -> HashMap<String, String> {
    let decode = |text: &str| {
        percent_decode_str(text)
            .decode_utf8()
            .ok()
            .map(std::borrow::Cow::into_owned)
    };
    query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((decode(key)?, decode(value)?))
        })
        .collect()
}

/// The text of an `OData` string literal: `'…'`, a quote inside doubled.
fn literal(value: &str) -> Option<String> {
    let inner = value.strip_prefix('\'')?.strip_suffix('\'')?;
    if inner.replace("''", "").contains('\'') {
        return None;
    }
    Some(inner.replace("''", "'"))
}

async fn api(
    State(app): State<Arc<App>>,
    method: Method,
    Path((site, rest)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Error> {
    let caller = account(&app, &headers, &app.config.origin()).await?;
    let team = on_team(&app, &caller, &site).await?;
    let query = query_values(query.as_deref().unwrap_or_default());
    if rest == ADD_STUB {
        return if method == Method::POST {
            add_stub(&app, &caller, &team, &query).await
        } else {
            Err(Error::Missing)
        };
    }
    let (id, step) = file_step(&rest).ok_or(Error::Missing)?;
    let file = site_repo::file(&app.db, &id)
        .await?
        .filter(|file| file.team_id == team.id)
        .ok_or(Error::Missing)?;
    if !site_repo::in_thread(&app.db, &file.channel_id, &caller.id).await? {
        return Err(Error::Forbidden);
    }
    let site = site_name(&team);
    match (method, step) {
        (Method::GET, Step::Read) => Ok(Json(sp_file(site, &file)).into_response()),
        (Method::POST, Step::Start(upload)) => {
            located(site, &file, &query)?;
            start(&app, &caller, &file, &upload, &headers, body).await
        }
        (Method::POST, Step::Finish(upload, offset)) => {
            located(site, &file, &query)?;
            finish(&caller, site, &file, &upload, offset)
        }
        (Method::DELETE, Step::Recycle) => {
            remove(&app.db, app.storage.as_ref(), None, &file.id).await?;
            Ok(Json(json!({"value": uuid::Uuid::new_v4().hyphenated().to_string()})).into_response())
        }
        _ => Err(Error::Missing),
    }
}

/// The team whose site `site` is, when `caller` is on it.
async fn on_team(app: &App, caller: &User, site: &str) -> Result<Team, Error> {
    let team = teams::team_by_group(&app.db, site).await?.ok_or(Error::Missing)?;
    if teams::membership(&app.db, &team.id, &caller.id).await?.is_none() {
        return Err(Error::Forbidden);
    }
    Ok(team)
}

/// `@folder` and `@file`, when a step names them: they must be the file's.
fn located(site: &str, file: &ChannelFile, query: &HashMap<String, String>) -> Result<(), Error> {
    let wrong = |key: &str, want: &str| {
        query
            .get(key)
            .is_some_and(|value| literal(value).as_deref() != Some(want))
    };
    if wrong("@folder", &folder_path(site, &file.folder)) || wrong("@file", &file.name) {
        return Err(Error::Invalid("The step names another file."));
    }
    Ok(())
}

/// `addStub`: an empty file of the name in a channel's folder, or SharePoint's 400 when the folder has the name.
async fn add_stub(app: &App, caller: &User, team: &Team, query: &HashMap<String, String>) -> Result<Response, Error> {
    let (Some(path), Some(name)) = (
        query.get("@v").and_then(|value| literal(value)),
        query.get("@fn").and_then(|value| literal(value)),
    ) else {
        return Err(Error::Invalid("The folder and the name are OData strings."));
    };
    if !valid_name(&name) {
        return Err(Error::Invalid("The file's name can't be used."));
    }
    let site = site_name(team);
    let folder = path
        .strip_prefix(&format!("{}/{LIBRARY}/", site_path(site)))
        .filter(|folder| valid_name(folder))
        .ok_or(Error::Missing)?;
    let channels = teams::channels_of(&app.db, std::slice::from_ref(&team.id)).await?;
    let channel = channels
        .iter()
        .find(|channel| folder_name(channel) == folder)
        .ok_or(Error::Missing)?;
    if !site_repo::in_thread(&app.db, &channel.id, &caller.id).await? {
        return Err(Error::Forbidden);
    }
    if channel.archived || team.archived {
        return Err(Error::Forbidden);
    }
    let file = ChannelFile {
        id: uuid::Uuid::new_v4().hyphenated().to_string(),
        team_id: team.id.clone(),
        channel_id: channel.id.clone(),
        folder: folder.to_owned(),
        name: name.clone(),
        owner_id: caller.id.clone(),
        stage: site_repo::STUB.to_owned(),
        upload_id: None,
        size: 0,
        created_at: crate::db::now(),
    };
    match site_repo::add(&app.db, &file).await {
        Ok(()) => Ok(Json(sp_file(site, &file)).into_response()),
        Err(DbError::Conflict) => Ok(name_taken(&format!("{path}/{name}"))),
        Err(error) => Err(error.into()),
    }
}

fn name_taken(path: &str) -> Response {
    let message = format!("A file with the name {path} already exists.");
    let body = json!({"odata.error": {"code": NAME_TAKEN, "message": {"lang": "en-US", "value": message}}});
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

/// `StartUpload`: the empty file's bytes, streamed to the store, by the account that added it.
async fn start(
    app: &App,
    caller: &User,
    file: &ChannelFile,
    upload: &str,
    headers: &HeaderMap,
    body: Body,
) -> Result<Response, Error> {
    if file.owner_id != caller.id {
        return Err(Error::Forbidden);
    }
    let media = &app.config.media;
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|length| length > media.max_file_bytes) {
        return Err(drive::refused(body, Error::TooLarge).await);
    }
    let left = match media.user_quota_bytes {
        0 => u64::MAX,
        quota => quota.saturating_sub(u64::try_from(repo::used(&app.db, &caller.id).await?).unwrap_or(0)),
    };
    if declared.is_some_and(|length| length > left) {
        return Err(drive::refused(body, Error::Quota).await);
    }
    if !site_repo::claim(&app.db, &file.id, upload).await? {
        return Err(drive::refused(body, Error::Invalid("The file has its bytes already.")).await);
    }
    let incoming = body.into_data_stream().map_err(std::io::Error::other).boxed();
    let key = drive::key(&file.id);
    let stored = app
        .storage
        .put_stream(&key, incoming, media.max_file_bytes.min(left))
        .await;
    let size = match stored {
        Ok(size) => i64::try_from(size).map_err(|_| Error::TooLarge),
        Err(crate::storage::StorageError::TooLarge) if left < media.max_file_bytes => Err(Error::Quota),
        Err(error) => Err(error.into()),
    };
    let filled = match size {
        Ok(size) => site_repo::fill(&app.db, file, upload, size, quota(app))
            .await
            .map(|filled| (filled, size))
            .map_err(Error::from),
        Err(error) => Err(error),
    };
    let answer = match filled {
        Ok((Filled::Done, size)) => return Ok(Json(json!({"value": size.to_string()})).into_response()),
        Ok((Filled::Quota, _)) => Error::Quota,
        Ok((Filled::Gone, _)) => Error::Missing,
        Err(error) => error,
    };
    super::clear(app.storage.as_ref(), &key).await;
    site_repo::unclaim(&app.db, &file.id, upload).await?;
    Err(answer)
}

/// `FinishUpload`: the end of the upload that filled the file, at its whole length.
fn finish(caller: &User, site: &str, file: &ChannelFile, upload: &str, offset: u64) -> Result<Response, Error> {
    if file.owner_id != caller.id {
        return Err(Error::Forbidden);
    }
    if file.stage != site_repo::FILLED
        || file.upload_id.as_deref() != Some(upload)
        || u64::try_from(file.size).ok() != Some(offset)
    {
        return Err(Error::Invalid("No upload of the file ended there."));
    }
    Ok(Json(sp_file(site, file)).into_response())
}

/// Removes the file (of `owner`, when one is named) with its bytes and thumbnails; whether it was there.
pub(super) async fn remove(
    db: &crate::db::Db,
    storage: &dyn crate::storage::Storage,
    owner: Option<&str>,
    id: &str,
) -> Result<bool, DbError> {
    if let Some(owner) = owner
        && site_repo::file(db, id).await?.is_none_or(|file| file.owner_id != owner)
    {
        return Ok(false);
    }
    if !site_repo::remove(db, id).await? {
        return Ok(false);
    }
    for key in std::iter::once(drive::key(id)).chain(thumbs::keys(id)) {
        super::clear(storage, &key).await;
    }
    Ok(true)
}

fn iso(at: i64) -> String {
    chrono::DateTime::from_timestamp(at, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// The file as SharePoint's REST API describes one (`SP.File`).
fn sp_file(site: &str, file: &ChannelFile) -> Value {
    json!({
        "odata.type": "SP.File", "UniqueId": file.id, "Name": file.name,
        "ServerRelativeUrl": format!("{}/{}", folder_path(site, &file.folder), file.name),
        "Length": file.size.to_string(), "Exists": true, "CheckOutType": 2, "Level": 1,
        "MajorVersion": 1, "MinorVersion": 0, "TimeCreated": iso(file.created_at),
        "TimeLastModified": iso(file.created_at),
    })
}

/// The file's address, as a post names it: the server's origin and its server-relative path, the spaces raw.
fn web_url(app: &App, site: &str, file: &ChannelFile) -> String {
    format!(
        "{}{}/{}",
        app.config.origin(),
        folder_path(site, &file.folder),
        file.name
    )
}

/// The file as Graph describes a `driveItem` of the site's library.
fn drive_item(app: &App, site: &str, file: &ChannelFile) -> Value {
    let url = web_url(app, site, file);
    json!({
        "id": format!("01{}", file.id.replace('-', "").to_uppercase()),
        "name": file.name, "size": file.size, "webUrl": url, "webDavUrl": url,
        "createdDateTime": iso(file.created_at), "lastModifiedDateTime": iso(file.created_at),
        "file": {"mimeType": drive::mime(&file.name)},
        "parentReference": {"driveId": format!("b!{}", site.replace('-', "")), "driveType": "documentLibrary",
            "path": format!("/drive/root:/{}", file.folder)},
        "sharepointIds": {"listItemUniqueId": file.id,
            "siteUrl": format!("{}{}", app.config.origin(), site_path(site))},
    })
}

/// The site, folder and name of a file's address on this server (`<origin>/sp/sites/<site>/Shared
/// Documents/<folder>/<name>`, its spaces raw or encoded); `None` for any other link.
fn link_place(base: &str, link: &str) -> Option<(String, String, String)> {
    let url = Url::parse(link).ok()?;
    let own = Url::parse(base).ok()?;
    if url.origin() != own.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments: Vec<String> = url
        .path_segments()?
        .map(|segment| {
            percent_decode_str(segment)
                .decode_utf8()
                .map(std::borrow::Cow::into_owned)
        })
        .collect::<Result<_, _>>()
        .ok()?;
    let [sp, sites, site, library, folder, name] = segments.as_slice() else {
        return None;
    };
    (sp == "sp" && sites == "sites" && library == LIBRARY).then(|| (site.clone(), folder.clone(), name.clone()))
}

/// The channel file a link names, as Graph describes it, when `caller` is in its channel; `None` when the link
/// names no channel file's address.
pub(super) async fn by_link(app: &App, caller: &User, link: &str) -> Result<Option<(repo::Item, Value)>, Error> {
    let Some((site, folder, name)) = link_place(&app.config.origin(), link) else {
        return Ok(None);
    };
    let team = on_team(app, caller, &site).await?;
    let file = site_repo::named(&app.db, &team.id, &folder, &name)
        .await?
        .ok_or(Error::Missing)?;
    if !site_repo::in_thread(&app.db, &file.channel_id, &caller.id).await? {
        return Err(Error::Forbidden);
    }
    Ok(Some((file.item(), drive_item(app, &site, &file))))
}

/// The channel file `id` as a signed link reads it, while `user` is in its channel; `None` when no channel file has
/// the id.
pub(super) async fn readable(app: &App, id: &str, user: &str) -> Result<Option<repo::Item>, Error> {
    let Some(file) = site_repo::file(&app.db, id).await? else {
        return Ok(None);
    };
    if !site_repo::in_thread(&app.db, &file.channel_id, user).await? {
        return Err(Error::Forbidden);
    }
    Ok(Some(file.item()))
}

/// Whether a channel file has the id.
pub(super) async fn exists(db: &crate::db::Db, id: &str) -> Result<bool, DbError> {
    Ok(site_repo::file(db, id).await?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_odata_string_is_quoted_with_each_quote_inside_doubled() {
        assert_eq!(literal("'notes.txt'").as_deref(), Some("notes.txt"));
        assert_eq!(literal("'Ada''s notes.txt'").as_deref(), Some("Ada's notes.txt"));
        assert_eq!(literal("''").as_deref(), Some(""));
        assert_eq!(literal("'a'''").as_deref(), Some("a'"));
        for refused in ["notes.txt", "'", "'notes.txt", "notes.txt'", "'Ada's'"] {
            assert_eq!(literal(refused), None, "{refused}");
        }
    }

    #[test]
    fn query_values_are_percent_decoded_and_a_plus_stays_a_plus() {
        let values = query_values("@v=%27%2Fsp%2Fsites%2Fs%2FShared%20Documents%2FGeneral%27&@fn=%27a+b%27&odd");
        assert_eq!(values["@v"], "'/sp/sites/s/Shared Documents/General'");
        assert_eq!(values["@fn"], "'a+b'");
        assert_eq!(values.len(), 2);
        assert!(query_values("@v=%FF").is_empty());
    }

    #[test]
    fn a_file_step_names_a_guid_and_one_of_the_steps() {
        let id = "0b7d5a8c-0000-4000-8000-00000000000a";
        let upload = "1c8e6b9d-0000-4000-8000-00000000000b";
        assert_eq!(
            file_step(&format!("GetFileById('{id}')")),
            Some((id.to_owned(), Step::Read))
        );
        assert_eq!(
            file_step(&format!("GetFileById('{}')/recycle", id.to_uppercase())),
            Some((id.to_owned(), Step::Recycle))
        );
        assert_eq!(
            file_step(&format!("GetFileById('{id}')/StartUpload(uploadId=guid'{upload}')")),
            Some((id.to_owned(), Step::Start(upload.to_owned())))
        );
        assert_eq!(
            file_step(&format!(
                "GetFileById('{id}')/FinishUpload(uploadId=guid'{upload}',fileOffset=21)"
            )),
            Some((id.to_owned(), Step::Finish(upload.to_owned(), 21)))
        );
        for refused in [
            "GetFileById('not-a-guid')".to_owned(),
            format!("GetFileById('{}')", id.replace('-', "")),
            format!("GetFileById('{id}')/CheckOut()"),
            format!("GetFileById('{id}')/StartUpload(uploadId='{upload}')"),
            format!("GetFileById('{id}')/FinishUpload(uploadId=guid'{upload}')"),
            format!("GetFileById('{id}')/FinishUpload(uploadId=guid'{upload}',fileOffset=-1)"),
            format!("GetFolderById('{id}')"),
        ] {
            assert_eq!(file_step(&refused), None, "{refused}");
        }
    }

    #[test]
    fn a_files_address_names_its_site_folder_and_name() {
        let base = "https://own.example.test";
        let place = |link: &str| link_place(base, link);
        let want = Some(("site".to_owned(), "General".to_owned(), "a b.txt".to_owned()));
        assert_eq!(
            place("https://own.example.test/sp/sites/site/Shared Documents/General/a b.txt"),
            want
        );
        assert_eq!(
            place("https://own.example.test/sp/sites/site/Shared%20Documents/General/a%20b.txt"),
            want
        );
        for refused in [
            "https://other.example.test/sp/sites/site/Shared Documents/General/a.txt",
            "https://u@own.example.test/sp/sites/site/Shared Documents/General/a.txt",
            "https://own.example.test/sp/sites/site/Shared Documents/General/a.txt?x=1",
            "https://own.example.test/sp/sites/site/Shared Documents/General/a.txt#x",
            "https://own.example.test/sp/sites/site/Documents/General/a.txt",
            "https://own.example.test/sp/personal/site/Shared Documents/General/a.txt",
            "https://own.example.test/sp/sites/site/Shared Documents/General/sub/a.txt",
            "https://own.example.test/sp/sites/site/Shared Documents/General/%FF",
        ] {
            assert_eq!(place(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_channels_folder_is_general_its_name_or_a_name_of_its_id() {
        let channel = |id: &str, name: &str| Channel {
            id: id.to_owned(),
            team_id: "19:team@thread.tacv2".to_owned(),
            group_id: None,
            name: name.to_owned(),
            description: String::new(),
            private: false,
            layout: String::new(),
            archived: false,
            shown_by_default: true,
            settings: None,
            created_at: 0,
            version: 0,
        };
        assert_eq!(folder_name(&channel("19:team@thread.tacv2", "Allgemein")), "General");
        assert_eq!(
            folder_name(&channel("19:x1@thread.tacv2", "Launch plans")),
            "Launch plans"
        );
        for refused in ["a#b", "general", " lead", "50%", ""] {
            assert_eq!(
                folder_name(&channel("19:x1@thread.tacv2", refused)),
                "Channel 19x1threadtacv2",
                "{refused}"
            );
        }
    }
}
