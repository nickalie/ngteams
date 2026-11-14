use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Caller, Error, events, member_of, message_repo, mutations, reads};
use crate::app::App;
use crate::db::DbError;

type Params = HashMap<String, String>;

#[derive(Serialize, Deserialize)]
struct Emotion {
    key: String,
    users: Vec<User>,
}

#[derive(Serialize, Deserialize)]
struct User {
    mri: String,
    time: i64,
    value: String,
}

pub(super) async fn react(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
    method: Method,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<&'static str, Error> {
    if query.len() != 1 || query.get("name").map(String::as_str) != Some("emotions") {
        return Err(Error::BadRequest);
    }
    let chat = reads::target(&path)?;
    let (_, members) = member_of(&app, &caller, chat).await?;
    let id = mutations::message_id(&path)?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let key = body["emotions"]["key"]
        .as_str()
        .filter(|key| !key.is_empty())
        .ok_or(Error::BadRequest)?;
    let remove = method == Method::DELETE;
    let value = if remove {
        None
    } else {
        let value = &body["emotions"]["value"];
        let value = value
            .as_str()
            .map_or_else(|| value.as_i64().map(|n| n.to_string()), |s| Some(s.to_owned()))
            .ok_or(Error::BadRequest)?;
        if value.parse::<i64>().ok().is_none_or(|n| n < 0) {
            return Err(Error::BadRequest);
        }
        Some(value)
    };
    let result = message_repo::update(
        &app.db,
        chat,
        id,
        &caller.id,
        message_repo::Who::AnyMember,
        |current, at| apply(current, at, key, value.as_deref(), &caller.mri()),
    )
    .await?;
    let changed = matches!(&result, message_repo::Updated::Message(_));
    let row = mutations::updated(result)?;
    if changed {
        events::publish(&app, &caller, &members, "MessageUpdate", &row.value()?).await;
    }
    Ok(if remove { "null" } else { "200" })
}

fn apply(body: &mut Value, at: i64, key: &str, value: Option<&str>, actor: &str) -> Result<bool, DbError> {
    if body["properties"].get("deletetime").is_some() {
        return Err(DbError::Conflict);
    }
    let mut emotions: Vec<Emotion> = body["properties"]
        .get("emotions")
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()
        .map_err(|_| super::property_repo::corrupt())?
        .unwrap_or_default();
    let existing = emotions.iter().position(|emotion| emotion.key == key);
    if value.is_none() && existing.is_none_or(|index| !emotions[index].users.iter().any(|user| user.mri == actor)) {
        return Ok(false);
    }
    let index = existing.unwrap_or_else(|| {
        emotions.push(Emotion {
            key: key.into(),
            users: Vec::new(),
        });
        emotions.len() - 1
    });
    let emotion = emotions.get_mut(index).ok_or(DbError::Conflict)?;
    emotion.users.retain(|user| user.mri != actor);
    if let Some(value) = value {
        emotion.users.push(User {
            mri: actor.into(),
            time: at,
            value: value.into(),
        });
    }
    let counts: serde_json::Map<String, Value> = emotions
        .iter()
        .filter(|emotion| !emotion.users.is_empty())
        .map(|emotion| (emotion.key.clone(), json!(emotion.users.len())))
        .collect();
    body["annotationsSummary"] = if counts.is_empty() {
        json!({})
    } else {
        json!({"emotions": counts})
    };
    body["properties"]["emotions"] = json!(emotions);
    Ok(true)
}
