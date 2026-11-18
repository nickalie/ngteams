use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::{Caller, Error, events, member_of, message_repo, messages, reads, wire};
use crate::app::App;

type Params = HashMap<String, String>;

pub(super) async fn edit(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let chat = reads::target(&path)?;
    let (found, _) = member_of(&app, &caller, chat).await?;
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
    let draft = messages::draft(&app, &caller, chat, &found.id, &sent).await?.value()?;
    let composed = sent.get("composetime").filter(|time| time.is_string()).cloned();
    let change = message_repo::Change {
        chat: &found.id,
        id,
        actor: &caller.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::Edited,
        base: &wire::base(&app),
    };
    let changed = message_repo::update(&app.db, change, |current, at| {
        if current["properties"].get("deletetime").is_some() {
            return Err(crate::db::DbError::Conflict);
        }
        current["content"] = draft["content"].clone();
        current["messagetype"] = draft["messagetype"].clone();
        for key in messages::MESSAGE_PROPERTIES.into_iter().chain([messages::VALID_QUOTES]) {
            if let Some(properties) = current["properties"].as_object_mut() {
                properties.remove(key);
                if let Some(value) = draft["properties"].get(key) {
                    properties.insert(key.into(), value.clone());
                }
            }
        }
        current["properties"]["edittime"] = json!(at);
        // Teams keeps the time the message was composed at, as the editing client wrote it
        if let Some(composed) = composed.as_ref() {
            current["properties"]["composetime"] = composed.clone();
        }
        Ok(true)
    })
    .await?;
    let changed = updated(changed)?;
    if changed.queued {
        crate::bots::wake(&app).await;
    }
    events::tell(
        &app,
        "MessageUpdate",
        &changed.row.value()?,
        &changed.readers,
        &changed.told,
    )
    .await;
    Ok(StatusCode::OK)
}

/// Notes the picture IDs before a change removes them from the message.
pub(super) fn keep(shown: &Mutex<Vec<String>>, body: &Value) {
    let named = body["amsreferences"].as_array().into_iter().flatten();
    if let Ok(mut shown) = shown.lock() {
        *shown = named.filter_map(Value::as_str).map(str::to_owned).collect();
    }
}

pub(super) fn message_id(path: &Params) -> Result<i64, Error> {
    path.get("message")
        .and_then(|id| id.parse().ok())
        .filter(|id| *id > 0)
        .ok_or(Error::BadRequest)
}

/// The change as made; one that changed nothing has nobody to tell.
pub(super) fn updated(result: message_repo::Updated) -> Result<message_repo::Changed, Error> {
    match result {
        message_repo::Updated::Missing => Err(Error::Missing),
        message_repo::Updated::Denied => Err(Error::Denied),
        message_repo::Updated::Message(changed) => Ok(changed),
        message_repo::Updated::Unchanged(row) => Ok(message_repo::Changed {
            row,
            readers: Vec::new(),
            told: Vec::new(),
            queued: false,
        }),
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
    let (found, _) = member_of(&app, &caller, chat).await?;
    let id = message_id(&path)?;
    let shown = Mutex::new(Vec::new());
    let change = message_repo::Change {
        chat: &found.id,
        id,
        actor: &caller.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::Deleted,
        base: &wire::base(&app),
    };
    let result = message_repo::update(&app.db, change, |body, at| {
        keep(&shown, body);
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
    let changed = updated(result)?;
    if changed.queued {
        crate::bots::wake(&app).await;
    }
    let resource = changed.row.value();
    // before the event: a fan-out that hangs past the request's deadline would leave the pictures charged
    crate::media::release(&app, &caller.id, &shown.into_inner().unwrap_or_default()).await;
    events::tell(&app, "MessageUpdate", &resource?, &changed.readers, &changed.told).await;
    Ok(Json(Value::Null))
}
