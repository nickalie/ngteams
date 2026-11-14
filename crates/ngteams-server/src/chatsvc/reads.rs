use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use serde_json::{Value, json};

use super::{Caller, Error, member_of, repo, wire};
use crate::app::App;

type Params = HashMap<String, String>;

pub(super) fn target(path: &Params) -> Result<&str, Error> {
    path.get("chat").map(String::as_str).ok_or(Error::BadRequest)
}

pub(super) async fn thread(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
) -> Result<Json<Value>, Error> {
    let id = target(&path)?;
    if id == "48:generic" {
        return Err(Error::InvalidThread);
    }
    let (chat, members) = member_of(&app, &caller, id).await?;
    Ok(Json(wire::thread(&app, &caller, &chat, &members)))
}

pub(super) async fn members(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
) -> Result<Json<Value>, Error> {
    let (chat, members) = member_of(&app, &caller, target(&path)?).await?;
    Ok(Json(json!({
        "eTag": format!("\"{}\"", chat.version), "isDeleted": false,
        "members": members.iter().map(|member| wire::member(&app, member, false)).collect::<Vec<_>>(),
        "rosterVersion": 0, "totalMemberCount": members.len(),
    })))
}

pub(super) async fn conversations(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    if !path
        .get("owner")
        .is_some_and(|owner| owner == "ME" || *owner == caller.mri())
    {
        return Err(Error::Denied);
    }
    let current = repo::watermark(&app.db).await?;
    let since = number(&query, "syncState", 0)?;
    let until = number(&query, "until", current)?;
    let after = number(&query, "after", since)?;
    if until > current || since > until || after < since || after > until {
        return Err(Error::BadRequest);
    }
    let size = number(&query, "pageSize", 200)?.clamp(1, 200);
    let mut chats = repo::list(&app.db, &caller.id, after, until, size + 1).await?;
    let more = i64::try_from(chats.len()).unwrap_or(i64::MAX) > size;
    chats.truncate(usize::try_from(size).unwrap_or(200));
    let link = format!("{}/v1/users/ME/conversations", wire::base(&app));
    let mut metadata = json!({"syncState": format!("{link}?syncState={until}"), "forwardLink": ""});
    if more && let Some(last) = chats.last() {
        metadata["forwardLink"] = json!(format!(
            "{link}?syncState={since}&until={until}&after={}&pageSize={size}",
            last.version
        ));
    }
    let mut conversations = Vec::new();
    for chat in &chats {
        let mut entry = wire::conversation(&app, &caller, chat);
        if let Some(message) = super::message_repo::latest(&app.db, &chat.id).await? {
            entry["lastMessage"] = message.value()?;
            if let Some(preview) = entry["lastMessage"].as_object_mut() {
                preview.remove("properties");
            }
            entry["properties"]["isemptyconversation"] = "False".into();
        }
        if let Some(message) = super::message_repo::last_updated(&app.db, &chat.id).await? {
            entry["lastUpdatedMessageId"] = message.id.into();
            entry["lastUpdatedMessageVersion"] = message.version.into();
        }
        if let Some(properties) = entry["properties"].as_object_mut() {
            properties.extend(super::property_repo::conversation(&app.db, &chat.id, &caller.id).await?);
        }
        conversations.push(entry);
    }
    Ok(Json(json!({"conversations": conversations, "_metadata": metadata})))
}

pub(super) fn number(query: &Params, name: &str, default: i64) -> Result<i64, Error> {
    match query.get(name) {
        None => Ok(default),
        Some(value) => value.parse().ok().filter(|value| *value >= 0).ok_or(Error::BadRequest),
    }
}

pub(super) fn missing_copy(_caller: Caller) -> std::future::Ready<Error> {
    std::future::ready(Error::Missing)
}
