//! An account's drive under `/sp/personal/<account>/`, in the SharePoint v2.0 shapes the client's file sending uses
//! (`docs/protocol/chat.md`, "Chat-file upload and specific-recipient sharing"): the drive and its chat-files folder
//! with their grants, a file's upload, its direct share with named accounts, and its grants read back. Only a
//! drive's owner reaches it here, but for a file's sharing link's permission (`shares/<token>/permission`, what
//! "Share this file" shows), which anyone with access reads; the people a file is shared with read it through Graph
//! ([`super::graph`]) and the signed links below.
//!
//! `/sp/download/<item>` and `/sp/thumbnail/<item>/<size>` take no token of an account: the link's `tempauth` is the
//! credential ([`crate::auth::tokens::Issuer::link`]), for that one item, for `media.link_lifetime` seconds, and only
//! while the file stays shared with the account the link was handed to.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use futures_util::{StreamExt as _, TryStreamExt as _};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use url::Url;

use super::{Error, account, quota, repo, thumbs};
use crate::app::App;
use crate::db::DbError;
use crate::db::users::{self, User};

/// The folder chat files go to, as Teams names it.
pub(super) const FOLDER: &str = "Microsoft Teams Chat Files";
/// What a download link's credential is for, and a thumbnail link's.
const CONTENT_LINK: &str = "urn:ngteams-server:file-content";
const THUMBNAIL_LINK: &str = "urn:ngteams-server:file-thumbnail";
/// What a file's sharing link lets people do, in the words "Share this file" shows.
const LINK_WORDS: &str = "People you choose can edit";
/// The largest share request.
const INVITE_BODY: usize = 64 * 1024;
/// The accounts one request shares a file with.
const MAX_RECIPIENTS: usize = 500;
/// The most of a refused upload's body that is read before the answer, and how long that may take.
const REFUSED_BODY: usize = 32 * 1024 * 1024;
const REFUSED_WAIT: Duration = Duration::from_secs(10);
/// The names tried for a file whose name the drive already has.
const RENAMES: u32 = 200;

pub(super) fn router() -> Router<Arc<App>> {
    Router::new()
        .route("/sp/personal/{user}/_api/v2.0/{*rest}", any(api))
        .route("/sp/download/{item}", get(download))
        .route("/sp/thumbnail/{item}/{size}", get(thumbnail))
}

pub(super) fn key(item: &str) -> String {
    format!("drive/{item}")
}

/// The account's name in its site's path: its id without the dashes (a site's name takes letters, digits and `_`).
fn site_user(id: &str) -> String {
    id.replace('-', "")
}

/// The account a site's path names.
fn site_owner(user: &str) -> Option<String> {
    let id = uuid::Uuid::try_parse(user).ok()?;
    (user.len() == 32).then(|| id.hyphenated().to_string())
}

pub(super) fn site(app: &App, owner: &str) -> String {
    format!("{}/sp/personal/{}/", app.config.origin(), site_user(owner))
}

/// The address of the file `name` in `owner`'s drive: what the message names it by and its sharing link.
pub(super) fn web_url(app: &App, owner: &str, name: &str) -> Result<String, Error> {
    let mut url = Url::parse(&site(app, owner)).map_err(|_| Error::Missing)?;
    url.path_segments_mut()
        .map_err(|()| Error::Missing)?
        .pop_if_empty()
        .extend(["Documents", FOLDER, name]);
    Ok(url.into())
}

/// The file a sharing link names: a link [`web_url`] made, on this server.
pub(super) async fn shared(app: &App, link: &str) -> Result<repo::Item, Error> {
    let (owner, name) = shared_name(&app.config.origin(), link)?;
    repo::item_named(&app.db, &owner, &name).await?.ok_or(Error::Missing)
}

pub(super) fn shared_name(base: &str, link: &str) -> Result<(String, String), Error> {
    let url = Url::parse(link).map_err(|_| Error::Missing)?;
    let own = Url::parse(base).map_err(|_| Error::Missing)?;
    if url.origin() != own.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Missing);
    }
    let segments: Vec<String> = url
        .path_segments()
        .ok_or(Error::Missing)?
        .map(|segment| {
            percent_encoding::percent_decode_str(segment)
                .decode_utf8_lossy()
                .into_owned()
        })
        .collect();
    let [sp, personal, user, documents, folder, name] = segments.as_slice() else {
        return Err(Error::Missing);
    };
    if sp != "sp" || personal != "personal" || documents != "Documents" || folder != FOLDER {
        return Err(Error::Missing);
    }
    let owner = site_owner(user).ok_or(Error::Missing)?;
    Ok((owner, name.clone()))
}

pub(super) fn download_link(app: &App, holder: &User, item: &repo::Item) -> Result<String, Error> {
    let credential = app.issuer.link(CONTENT_LINK, &item.id, holder, lifetime(app))?;
    Ok(format!(
        "{}/sp/download/{}?tempauth={credential}",
        app.config.origin(),
        item.id
    ))
}

pub(super) fn thumbnail_link(app: &App, holder: &User, item: &repo::Item, size: &str) -> Result<String, Error> {
    let credential = app.issuer.link(THUMBNAIL_LINK, &item.id, holder, lifetime(app))?;
    Ok(format!(
        "{}/sp/thumbnail/{}/{size}?tempauth={credential}",
        app.config.origin(),
        item.id
    ))
}

fn lifetime(app: &App) -> Duration {
    Duration::from_secs(app.config.media.link_lifetime)
}

pub(super) fn drive_id(owner: &str) -> String {
    format!("b!{}", site_user(owner))
}

/// The drive as Graph and SharePoint both describe it.
pub(super) fn drive(app: &App, owner: &User) -> Value {
    json!({
        "id": drive_id(&owner.id), "driveType": "business", "name": "OneDrive",
        "owner": {"user": {"id": owner.id, "displayName": owner.display_name}},
        "webUrl": format!("{}Documents", site(app, &owner.id)),
    })
}

fn grant(user: &str, role: &str) -> Value {
    json!({"id": user, "roles": [role], "grantedToV2": {"user": {"id": user}}})
}

/// A name a file is kept under: one path segment, as the client's own rule has it.
fn valid_name(name: &str) -> bool {
    (1..=255).contains(&name.len())
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '?', ':', '*', '"', '<', '>', '|'])
        && !name.chars().any(char::is_control)
}

fn renamed(name: &str, number: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => format!("{stem} {number}.{extension}"),
        _ => format!("{name} {number}"),
    }
}

async fn api(
    State(app): State<Arc<App>>,
    method: Method,
    Path((user, rest)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, Error> {
    let caller = account(&app, &headers, &app.config.origin()).await?;
    if method == Method::GET
        && let ["shares", share, "permission"] = rest.split('/').collect::<Vec<_>>().as_slice()
    {
        return link_permission(&app, &caller, &user, share).await;
    }
    if site_owner(&user).as_deref() != Some(caller.id.as_str()) {
        return Err(Error::Forbidden);
    }
    let folder = format!("drive/root:/{FOLDER}");
    if method == Method::GET
        && let Some(answer) = drive_read(&app, &caller, &rest, &folder).await?
    {
        return Ok(answer);
    }
    if method == Method::PUT
        && let Some(name) = rest
            .strip_prefix(&format!("{folder}/"))
            .and_then(|rest| rest.strip_suffix(":/content"))
    {
        return upload(&app, &caller, name, &headers, body).await;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["sites", "root", "items", item, "driveItem", "permissions"]) => {
            let item = owned(&app, &caller, item).await?;
            let grants = repo::grants(&app.db, &item.id).await?;
            let value: Vec<Value> = grants
                .iter()
                .map(|user| grant(user, if *user == caller.id { "owner" } else { "write" }))
                .collect();
            Ok(Json(json!({"value": value})).into_response())
        }
        ("POST", ["sites", "root", "items", item, "driveItem", "invite"]) => {
            let item = owned(&app, &caller, item).await?;
            let body = axum::body::to_bytes(body, INVITE_BODY)
                .await
                .map_err(|_| Error::TooLarge)?;
            invite(&app, &caller, &item, &body).await
        }
        ("DELETE", ["sites", "root", "items", item, "driveItem", "permissions", user]) => {
            let item = owned(&app, &caller, item).await?;
            if *user == caller.id || !repo::revoke(&app.db, &item.id, user).await? {
                return Err(Error::Missing);
            }
            Ok(StatusCode::NO_CONTENT.into_response())
        }
        ("DELETE", ["drive", "items", item]) => {
            if remove(&app.db, app.storage.as_ref(), &caller.id, &item_guid(item)).await? {
                Ok(StatusCode::NO_CONTENT.into_response())
            } else {
                Err(Error::Missing)
            }
        }
        _ => Err(Error::Missing),
    }
}

/// What a file's sharing link (its token `share`) on `user`'s site lets people do, for anyone with access: a file of
/// this server is shared only with named accounts, so its link reaches the people it was shared with.
async fn link_permission(app: &App, caller: &User, user: &str, share: &str) -> Result<Response, Error> {
    let link = super::graph::link_of(share)?;
    let item = shared(app, &link).await?;
    if site_owner(user).as_deref() != Some(item.owner_id.as_str()) {
        return Err(Error::Missing);
    }
    if !repo::grants(&app.db, &item.id).await?.contains(&caller.id) {
        return Err(Error::Forbidden);
    }
    Ok(Json(json!({
        "id": item.id,
        "roles": ["write"],
        "link": {"scope": "users", "type": "edit", "localizedDescription": LINK_WORDS, "webUrl": link},
    }))
    .into_response())
}

/// The drive, its chat-files folder with its files, and their grants, as their owner reads them; `None` for
/// another path.
async fn drive_read(app: &App, caller: &User, rest: &str, folder: &str) -> Result<Option<Response>, Error> {
    let answer = if rest == "drive" {
        drive(app, caller)
    } else if rest == "drive/root/permissions" || rest == format!("{folder}:/permissions") {
        json!({"value": [grant(&caller.id, "owner")]})
    } else if rest == folder {
        if !repo::has_items(&app.db, &caller.id).await? {
            return Err(Error::Missing);
        }
        json!({"id": "chatfiles", "name": FOLDER, "folder": {}})
    } else if rest == format!("{folder}:/children") {
        let items = repo::items(&app.db, &caller.id).await?;
        let value = items
            .iter()
            .map(|item| drive_item(app, item))
            .collect::<Result<Vec<Value>, Error>>()?;
        json!({"value": value})
    } else {
        return Ok(None);
    };
    Ok(Some(Json(answer).into_response()))
}

/// The file's id from either name [`drive_item`] gives it: its drive item id (`01` and the id's digits) or the id.
fn item_guid(id: &str) -> String {
    id.strip_prefix("01")
        .filter(|digits| digits.len() == 32)
        .and_then(|digits| uuid::Uuid::try_parse(digits).ok())
        .map_or_else(|| id.to_owned(), |id| id.hyphenated().to_string())
}

/// Removes `owner`'s file, ends its shares, gives its bytes back to the owner's count and clears them and its
/// thumbnails from the store; whether it was there.
pub(super) async fn remove(
    db: &crate::db::Db,
    storage: &dyn crate::storage::Storage,
    owner: &str,
    id: &str,
) -> Result<bool, DbError> {
    if !repo::remove_item(db, owner, id).await? {
        return Ok(false);
    }
    for key in std::iter::once(key(id)).chain(thumbs::keys(id)) {
        super::clear(storage, &key).await;
    }
    Ok(true)
}

async fn owned(app: &App, caller: &User, item: &str) -> Result<repo::Item, Error> {
    repo::item(&app.db, item)
        .await?
        .filter(|item| item.owner_id == caller.id)
        .ok_or(Error::Missing)
}

async fn upload(app: &App, caller: &User, name: &str, headers: &HeaderMap, body: Body) -> Result<Response, Error> {
    if !valid_name(name) {
        return Err(Error::Invalid("The file's name can't be used."));
    }
    let media = &app.config.media;
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok()?.parse::<u64>().ok());
    if declared.is_some_and(|length| length > media.max_file_bytes) {
        return Err(refused(body, Error::TooLarge).await);
    }
    let left = match media.user_quota_bytes {
        0 => u64::MAX,
        quota => quota.saturating_sub(u64::try_from(repo::used(&app.db, &caller.id).await?).unwrap_or(0)),
    };
    if declared.is_some_and(|length| length > left) {
        return Err(refused(body, Error::Quota).await);
    }
    let id = uuid::Uuid::new_v4().hyphenated().to_string();
    let incoming = body.into_data_stream().map_err(std::io::Error::other).boxed();
    let size = app
        .storage
        .put_stream(&key(&id), incoming, media.max_file_bytes.min(left))
        .await
        .map_err(|error| match error {
            crate::storage::StorageError::TooLarge if left < media.max_file_bytes => Error::Quota,
            other => other.into(),
        })?;
    let mut item = repo::Item {
        id,
        owner_id: caller.id.clone(),
        name: name.to_owned(),
        size: i64::try_from(size).map_err(|_| Error::TooLarge)?,
        created_at: crate::db::now(),
    };
    let mut number = 0;
    let kept = loop {
        match repo::add_item(&app.db, &item, quota(app)).await {
            Ok(Some(())) => break Ok(()),
            Ok(None) => break Err(Error::Quota),
            Err(DbError::Conflict) if number < RENAMES => {
                number += 1;
                item.name = renamed(name, number);
            }
            Err(error) => break Err(error.into()),
        }
    };
    if let Err(error) = kept {
        app.storage.delete(&key(&item.id)).await?;
        return Err(error);
    }
    Ok((StatusCode::CREATED, Json(drive_item(app, &item)?)).into_response())
}

/// `why`, once the body of the upload it refuses is read and dropped, up to [`REFUSED_BODY`] and for at most
/// [`REFUSED_WAIT`]: a client still sending when its connection closes loses the answer.
pub(super) async fn refused(body: Body, why: Error) -> Error {
    let mut body = body.into_data_stream();
    let dropped = async {
        let mut left = REFUSED_BODY;
        while left > 0
            && let Some(Ok(bytes)) = body.next().await
        {
            left = left.saturating_sub(bytes.len());
        }
    };
    let _ = tokio::time::timeout(REFUSED_WAIT, dropped).await;
    why
}

/// A file as SharePoint and Graph describe a `driveItem`.
pub(super) fn drive_item(app: &App, item: &repo::Item) -> Result<Value, Error> {
    let url = web_url(app, &item.owner_id, &item.name)?;
    let at = chrono::DateTime::from_timestamp(item.created_at, 0)
        .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    Ok(json!({
        "id": format!("01{}", item.id.replace('-', "").to_uppercase()),
        "name": item.name, "size": item.size, "webUrl": url, "webDavUrl": url,
        "createdDateTime": at, "lastModifiedDateTime": at,
        "file": {"mimeType": mime(&item.name)},
        "parentReference": {"driveId": drive_id(&item.owner_id), "driveType": "business",
            "path": format!("/drive/root:/{FOLDER}")},
        "sharepointIds": {"listItemUniqueId": item.id, "siteUrl": site(app, &item.owner_id)},
    }))
}

pub(super) fn mime(name: &str) -> String {
    mime_guess::from_path(name).first_or_octet_stream().to_string()
}

/// A direct grant to each named account, as the client asks for one: sign-in required, no invitation sent, never
/// a link anyone could open.
async fn invite(app: &App, caller: &User, item: &repo::Item, body: &[u8]) -> Result<Response, Error> {
    let body: Value = serde_json::from_slice(body).map_err(|_| Error::Invalid("The share isn't JSON."))?;
    if body["requireSignIn"] != json!(true) || body["sendInvitation"] != json!(false) {
        return Err(Error::Invalid(
            "A file is shared only with named accounts that sign in.",
        ));
    }
    let recipients = body["recipients"]
        .as_array()
        .filter(|recipients| (1..=MAX_RECIPIENTS).contains(&recipients.len()))
        .ok_or(Error::Invalid("The share names nobody."))?;
    let mut users = Vec::new();
    for recipient in recipients {
        let email = recipient["email"]
            .as_str()
            .ok_or(Error::Invalid("A recipient needs an email."))?;
        let user = users::by_upn(&app.db, email)
            .await?
            .filter(|user| !user.disabled)
            .ok_or(Error::Invalid("A recipient isn't an account here."))?;
        if user.id != caller.id {
            users.push(user.id);
        }
    }
    repo::grant(&app.db, &item.id, &users).await?;
    let value: Vec<Value> = users.iter().map(|user| grant(user, "write")).collect();
    Ok(Json(json!({"value": value})).into_response())
}

/// The item a link's `tempauth` is the credential for, while the account it was handed to is the one it was and the
/// file is still shared with it (a channel file: while the account is in its channel).
async fn linked(app: &App, audience: &str, id: &str, query: &HashMap<String, String>) -> Result<repo::Item, Error> {
    let credential = query.get("tempauth").ok_or(Error::Unauthorized)?;
    let link = app
        .issuer
        .verify_link(credential, audience)
        .await
        .map_err(|_| Error::Unauthorized)?;
    if link.subject != id {
        return Err(Error::Unauthorized);
    }
    users::by_id(&app.db, &link.oid)
        .await?
        .filter(|holder| !holder.disabled && holder.password_version == link.pwd_ver)
        .ok_or(Error::Unauthorized)?;
    let Some(item) = repo::item(&app.db, id).await? else {
        return super::sites::readable(app, id, &link.oid).await?.ok_or(Error::Missing);
    };
    if !repo::grants(&app.db, &item.id).await?.contains(&link.oid) {
        return Err(Error::Forbidden);
    }
    Ok(item)
}

async fn download(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, Error> {
    let item = linked(&app, CONTENT_LINK, &id, &query).await?;
    let size = u64::try_from(item.size).unwrap_or(0);
    let asked = match headers.get(RANGE) {
        None => None,
        Some(range) => {
            let parsed = range
                .to_str()
                .ok()
                .and_then(|range| http_range_header::parse_range_header(range).ok())
                .and_then(|range| range.validate(size).ok());
            match parsed.as_deref() {
                // one range is served; several at once aren't, and get the whole file as a server may answer
                Some([one]) => Some(*one.start()..one.end() + 1),
                Some(_) => None,
                None => {
                    return Ok((
                        StatusCode::RANGE_NOT_SATISFIABLE,
                        [(CONTENT_RANGE, format!("bytes */{size}"))],
                    )
                        .into_response());
                }
            }
        }
    };
    let disposition = format!(
        "attachment; filename*=UTF-8''{}",
        utf8_percent_encode(&item.name, NON_ALPHANUMERIC)
    );
    let mut response = if size == 0 {
        Response::new(Body::empty())
    } else {
        let blob = app
            .storage
            .open(&key(&item.id), asked.clone())
            .await?
            .ok_or(Error::Missing)?;
        let mut response = Response::new(Body::from_stream(blob.bytes));
        let length = blob.range.end - blob.range.start;
        response.headers_mut().insert(CONTENT_LENGTH, length.into());
        if asked.is_some() {
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let range = format!("bytes {}-{}/{size}", blob.range.start, blob.range.end - 1);
            response.headers_mut().insert(CONTENT_RANGE, header(&range));
        }
        response
    };
    let headers = response.headers_mut();
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(CONTENT_TYPE, header(&mime(&item.name)));
    // a file is never rendered on the server's own origin, whatever it holds
    headers.insert(CONTENT_DISPOSITION, header(&disposition));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-store"));
    Ok(response)
}

fn header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"))
}

async fn thumbnail(
    State(app): State<Arc<App>>,
    Path((id, size)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, Error> {
    let item = linked(&app, THUMBNAIL_LINK, &id, &query).await?;
    let (content_type, bytes) = thumbs::picture(&app, &item, &size).await?.ok_or(Error::Missing)?;
    Ok((
        [
            (CONTENT_TYPE, content_type),
            (CACHE_CONTROL, "private, max-age=3600"),
            (X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}
