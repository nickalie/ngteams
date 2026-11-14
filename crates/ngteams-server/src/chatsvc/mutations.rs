use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{Caller, Error, events, member_of, message_repo, messages, reads};
use crate::app::App;

type Params = HashMap<String, String>;

pub(super) async fn edit(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let chat = reads::target(&path)?;
    let (_, members) = member_of(&app, &caller, chat).await?;
    let id = message_id(&path)?;
    let Json(mut sent) = body.map_err(|_| Error::BadRequest)?;
    if !sent.is_object() {
        return Err(Error::BadRequest);
    }
    if sent["from"]
        .as_str()
        .is_some_and(|from| from != format!("worker/{}", caller.mri()))
    {
        return Err(Error::Denied);
    }
    sent["from"] = caller.mri().into();
    let draft = messages::draft(&app, &caller, chat, &sent)?.value()?;
    let changed = message_repo::update(&app.db, chat, id, &caller.id, true, |current, at| {
        if current["properties"].get("deletetime").is_some() {
            return Err(crate::db::DbError::Conflict);
        }
        current["content"] = draft["content"].clone();
        current["messagetype"] = draft["messagetype"].clone();
        for key in ["mentions", "cards", "links", "files", "formatVariant", "qtdMsgs"] {
            if let Some(properties) = current["properties"].as_object_mut() {
                properties.remove(key);
                if let Some(value) = draft["properties"].get(key) {
                    properties.insert(key.into(), value.clone());
                }
            }
        }
        current["properties"]["edittime"] = json!(at);
        Ok(true)
    })
    .await?;
    let row = updated(changed)?;
    events::publish(&app, &caller, &members, "MessageUpdate", &row.value()?).await;
    Ok(StatusCode::OK)
}

pub(super) fn message_id(path: &Params) -> Result<i64, Error> {
    path.get("message")
        .and_then(|id| id.parse().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::BadRequest)
}

pub(super) fn updated(result: message_repo::Updated) -> Result<message_repo::Message, Error> {
    match result {
        message_repo::Updated::Missing => Err(Error::Missing),
        message_repo::Updated::Denied => Err(Error::Denied),
        message_repo::Updated::Message(row) | message_repo::Updated::Unchanged(row) => Ok(row),
    }
}

pub(super) async fn delete(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    if query.get("behavior").map(String::as_str) != Some("softDelete") {
        return Err(Error::Missing);
    }
    let chat = reads::target(&path)?;
    let (_, members) = member_of(&app, &caller, chat).await?;
    let id = message_id(&path)?;
    let result = message_repo::update(&app.db, chat, id, &caller.id, true, |body, at| {
        body["content"] = "".into();
        body["properties"] = json!({"deletetime": at});
        if let Some(client) = body.get("clientmessageid").cloned() {
            body["skypeeditedid"] = client;
        }
        if let Some(object) = body.as_object_mut() {
            object.remove("amsreferences");
            object.remove("annotationsSummary");
        }
        Ok(true)
    })
    .await?;
    let row = updated(result)?;
    events::publish(&app, &caller, &members, "MessageUpdate", &row.value()?).await;
    Ok(Json(Value::Null))
}
