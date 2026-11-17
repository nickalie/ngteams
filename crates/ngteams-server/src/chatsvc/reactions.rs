use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Caller, Error, events, member_of, message_repo, mutations, reads, streams};
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
    let chat = reads::target(&path)?;
    let name = query.get("name").map(String::as_str).filter(|_| query.len() == 1);
    if name == Some("isread") && method == Method::PUT && matches!(chat, streams::FEED | streams::MENTIONS) {
        return mark_read(&app, &caller, &path, body).await;
    }
    if name == Some("meeting") && method == Method::PUT {
        return title_meeting(&app, &caller, chat, &path, body).await;
    }
    if name != Some("emotions") {
        return Err(Error::BadRequest);
    }
    let (found, _) = member_of(&app, &caller, chat).await?;
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
    let change = message_repo::Change {
        chat: &found.id,
        id,
        actor: &caller.id,
        who: message_repo::Who::AnyMember,
        effect: if remove {
            message_repo::Effect::Unreacted(key)
        } else {
            message_repo::Effect::Reacted(key)
        },
        base: &super::wire::base(&app),
    };
    let result = message_repo::update(&app.db, change, |current, at| {
        apply(current, at, key, value.as_deref(), &caller.mri())
    })
    .await?;
    let changed = mutations::updated(result)?;
    events::tell(
        &app,
        "MessageUpdate",
        &changed.row.value()?,
        &changed.readers,
        &changed.told,
    )
    .await;
    Ok(if remove { "null" } else { "200" })
}

/// The longest meeting title a post takes.
const LONGEST_TITLE: usize = 255;

/// `PUT …/messages/<post>/properties?name=meeting` with `{"meeting": {"meetingtitle"}}`: the author of a post that
/// names a meeting (a channel's Meet now, posted as its organizer's) titles it, as the web client titles it from its
/// pre-join. A post that names no meeting is left as it is.
async fn title_meeting(
    app: &App,
    caller: &Caller,
    chat: &str,
    path: &Params,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<&'static str, Error> {
    let (found, _) = member_of(app, caller, chat).await?;
    let id = mutations::message_id(path)?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let title = body["meeting"]["meetingtitle"]
        .as_str()
        .map(str::trim)
        .filter(|title| !title.is_empty() && title.chars().count() <= LONGEST_TITLE)
        .ok_or(Error::BadRequest)?;
    let change = message_repo::Change {
        chat: &found.id,
        id,
        actor: &caller.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::None,
        base: &super::wire::base(app),
    };
    let result = message_repo::update(&app.db, change, |current, _| {
        let meeting = current["properties"]["meeting"]
            .as_str()
            .map(serde_json::from_str::<Value>);
        let Some(Ok(mut meeting)) = meeting else {
            return Ok(false);
        };
        meeting["meetingtitle"] = json!(title);
        current["properties"]["meeting"] = json!(meeting.to_string());
        Ok(true)
    })
    .await?;
    let changed = mutations::updated(result)?;
    if !changed.told.is_empty() {
        events::tell(
            app,
            "MessageUpdate",
            &changed.row.value()?,
            &changed.readers,
            &changed.told,
        )
        .await;
    }
    Ok("200")
}

/// `PUT …/48:notifications/messages/<item>/properties?name=isread`: the account's own item read or unread. Its
/// version moves, so the account's other clients sync the mark.
async fn mark_read(
    app: &App,
    caller: &Caller,
    path: &Params,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<&'static str, Error> {
    let (found, _) = member_of(app, caller, reads::target(path)?).await?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let read = match &body["isread"] {
        Value::Bool(read) => *read,
        Value::String(text) if text == "true" => true,
        Value::String(text) if text == "false" => false,
        _ => return Err(Error::BadRequest),
    };
    let change = message_repo::Change {
        chat: &found.id,
        id: mutations::message_id(path)?,
        actor: &caller.id,
        who: message_repo::Who::Owner,
        effect: message_repo::Effect::None,
        base: &super::wire::base(app),
    };
    let result = message_repo::update(&app.db, change, |item, _| {
        if item["properties"].get("deletetime").is_some() {
            return Err(DbError::Conflict);
        }
        item["properties"]["isread"] = json!(read);
        Ok(true)
    })
    .await?;
    let changed = mutations::updated(result)?;
    events::tell(
        app,
        "MessageUpdate",
        &changed.row.value()?,
        &changed.readers,
        &changed.told,
    )
    .await;
    Ok("")
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
