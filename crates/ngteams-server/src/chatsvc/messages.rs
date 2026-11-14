use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::{StatusCode, header::LOCATION};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

use super::{Caller, Error, events, member_of, message_repo, reads, repo, wire};
use crate::app::App;
use crate::db::DbError;

type Params = HashMap<String, String>;

pub(super) const MESSAGE_PROPERTIES: [&str; 6] = ["mentions", "cards", "links", "files", "formatVariant", "qtdMsgs"];

pub(super) async fn post(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Response, Error> {
    let chat = reads::target(&path)?;
    let (_, members) = member_of(&app, &caller, chat).await?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    if body["messagetype"].as_str() == Some("Control/Typing") {
        return typing(&app, &caller, chat, &members, &body).await;
    }
    let draft = draft(&app, &caller, chat, &body)?;
    let (row, inserted) = message_repo::insert(&app.db, &draft).await?.ok_or(Error::Denied)?;
    if inserted {
        events::publish(&app, &caller, &members, "NewMessage", &row.value()?).await;
    }
    Ok((
        StatusCode::CREATED,
        [(
            LOCATION,
            format!(
                "{}/v1/users/ME/conversations/{chat}/messages/{}",
                wire::base(&app),
                row.id
            ),
        )],
        Json(json!({"OriginalArrivalTime": row.id})),
    )
        .into_response())
}

pub(super) fn draft(app: &App, caller: &Caller, chat: &str, sent: &Value) -> Result<message_repo::Message, Error> {
    let kind = sent["messagetype"].as_str().ok_or(Error::BadRequest)?;
    if !(kind == "Text" || kind == "RichText" || kind.starts_with("RichText/")) {
        return Err(Error::BadRequest);
    }
    let content = sent["content"].as_str().ok_or(Error::BadRequest)?;
    if sent
        .get("from")
        .is_some_and(|from| from.as_str() != Some(caller.mri().as_str()))
    {
        return Err(Error::Denied);
    }
    let mut properties = Map::new();
    if let Some(sent) = sent.get("properties") {
        let sent = sent.as_object().ok_or(Error::BadRequest)?;
        for key in MESSAGE_PROPERTIES {
            if let Some(value) = sent.get(key) {
                properties.insert(key.into(), value.clone());
            }
        }
    }
    let mut body = json!({
        "type": "Message", "conversationid": chat,
        "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", wire::base(app)),
        "from": format!("{}/v1/users/ME/contacts/{}", wire::base(app), caller.mri()),
        "fromDisplayNameInToken": caller.name, "imdisplayname": caller.name,
        "messagetype": kind, "content": content, "properties": properties,
    });
    for key in ["contenttype", "clientmessageid", "amsreferences"] {
        if let Some(value) = sent.get(key) {
            body[key] = value.clone();
        }
    }
    let client_id = match sent.get("clientmessageid") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .ok_or(Error::BadRequest)?
                .to_owned(),
        ),
    };
    Ok(message_repo::Message {
        chat_id: chat.into(),
        id: 0,
        owner_id: caller.id.clone(),
        client_id,
        version: 0,
        body: body.to_string(),
    })
}

pub(super) async fn page(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    let chat = reads::target(&path)?;
    if chat == "48:generic" {
        return Err(Error::InvalidThread);
    }
    member_of(&app, &caller, chat).await?;
    let current = repo::watermark(&app.db).await?;
    let until = reads::number(&query, "until", current)?;
    let since = reads::number(&query, "syncState", 0)?;
    let before = reads::number(&query, "before", i64::MAX)?;
    if until > current || since > until {
        return Err(Error::BadRequest);
    }
    let size = reads::number(&query, "pageSize", 200)?.clamp(1, 200);
    let incremental = query.contains_key("syncState");
    let rows = message_repo::page(
        &app.db,
        chat,
        message_repo::Window {
            since,
            until,
            before,
            size: size + 1,
            incremental,
        },
    )
    .await?;
    let (messages, more) = bounded_messages(&rows, size)?;
    let link = format!("{}/v1/users/ME/conversations/{chat}/messages", wire::base(&app));
    let mut metadata = json!({"syncState": format!("{link}?syncState={until}"),
        "lastCompleteSegmentStartTime": since, "lastCompleteSegmentEndTime": until});
    if more && let Some(last) = rows.get(messages.len().saturating_sub(1)) {
        metadata["lastCompleteSegmentStartTime"] = if incremental {
            last.id.max(since.saturating_add(1))
        } else {
            last.id
        }
        .into();
        if incremental {
            metadata["backwardLink"] = json!(format!(
                "{link}?syncState={since}&until={until}&before={}&pageSize={size}",
                last.id
            ));
        } else {
            metadata["backwardLink"] = json!(format!("{link}?until={until}&before={}&pageSize={size}", last.id));
        }
    }
    Ok(Json(
        json!({"messages": messages, "tenantId": caller.tenant, "_metadata": metadata}),
    ))
}

pub(super) async fn around(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
) -> Result<Json<Value>, Error> {
    let chat = reads::target(&path)?;
    member_of(&app, &caller, chat).await?;
    let at = reads::number(&path, "time", 0)?;
    let size = reads::number(&query, "pageSize", 200)?.clamp(1, 200);
    if query.get("direction").map(String::as_str) != Some("BIDIRECTIONAL") {
        return Err(Error::BadRequest);
    }
    let rows = message_repo::around(&app.db, chat, at, size).await?;
    let (messages, more) = bounded_messages(&rows, size)?;
    let mut metadata = json!({"lastCompleteSegmentStartTime": 0, "lastCompleteSegmentEndTime": 0});
    if more && let Some(last) = rows.get(messages.len().saturating_sub(1)) {
        metadata["backwardLink"] = json!(format!(
            "{}/v1/users/ME/conversations/{chat}/messages?before={}&pageSize={size}",
            wire::base(&app),
            last.id
        ));
    }
    Ok(Json(
        json!({"messages": messages, "tenantId": caller.tenant, "_metadata": metadata}),
    ))
}

fn bounded_messages(rows: &[message_repo::Message], size: i64) -> Result<(Vec<Value>, bool), DbError> {
    let mut messages = Vec::new();
    let mut bytes = 2;
    for row in rows.iter().take(usize::try_from(size).unwrap_or(200)) {
        let length = row.body.len() + 1;
        if !messages.is_empty() && bytes + length > wire::MAX_PAGE_BYTES {
            break;
        }
        bytes += length;
        messages.push(row.value()?);
    }
    let more = messages.len() < rows.len();
    Ok((messages, more))
}

async fn typing(
    app: &App,
    caller: &Caller,
    chat: &str,
    members: &[super::repo::Member],
    body: &Value,
) -> Result<Response, Error> {
    if body["content"].as_str() != Some("") {
        return Err(Error::BadRequest);
    }
    let at = repo::tick(&app.db).await?;
    let resource = json!({
        "type": "Message", "id": at.to_string(), "version": at.to_string(), "clientmessageid": at.to_string(),
        "conversationid": chat, "conversationLink": format!("{}/v1/users/ME/conversations/{chat}", wire::base(app)),
        "from": format!("{}/v1/users/ME/contacts/{}", wire::base(app), caller.mri()), "imdisplayname": "",
        "messagetype": "Control/Typing", "contenttype": "Application/Message", "content": "",
        "properties": {"importance": "", "subject": ""},
    });
    events::publish(app, caller, members, "NewMessage", &resource).await;
    Ok((StatusCode::CREATED, Json(json!({"OriginalArrivalTime": at}))).into_response())
}
