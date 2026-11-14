use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State, rejection::JsonRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::{Caller, Error, events, member_of, property_repo, reads};
use crate::app::App;

type Params = HashMap<String, String>;

pub(super) async fn set_conversation(
    State(app): State<Arc<App>>,
    caller: Caller,
    Path(path): Path<Params>,
    Query(query): Query<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    let chat = reads::target(&path)?;
    let (_, members) = member_of(&app, &caller, chat).await?;
    if query.len() != 1 || query.get("name").map(String::as_str) != Some("consumptionhorizon") {
        return Err(Error::BadRequest);
    }
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let horizon = body["consumptionhorizon"].as_str().ok_or(Error::BadRequest)?;
    let parts: Vec<_> = horizon.split(';').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.parse::<u64>().is_err()) {
        return Err(Error::BadRequest);
    }
    if !property_repo::set_conversation(&app.db, chat, &caller.id, "consumptionhorizon", horizon).await? {
        return Err(Error::Denied);
    }
    let resource = json!({"id": chat, "mri": caller.mri(), "consumptionhorizon": horizon});
    events::publish(&app, &caller, &members, "ReadReceipt", &resource).await;
    events::own(
        &app,
        &caller,
        "ConversationUpdate",
        &json!({"id": chat,
        "propertiesUpdated": ["consumptionhorizon"], "properties": {"consumptionhorizon": horizon}}),
    )
    .await;
    Ok(StatusCode::OK)
}

pub(super) async fn user(State(app): State<Arc<App>>, caller: Caller) -> Result<Response, Error> {
    Ok(match property_repo::user(&app.db, &caller.id).await? {
        Some(properties) => Json(properties).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"errorCode": 404,
            "standardizedError": {"errorCode": 404, "errorSubCode": 1}})),
        )
            .into_response(),
    })
}

pub(super) async fn set_user(
    State(app): State<Arc<App>>,
    caller: Caller,
    Query(query): Query<Params>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<StatusCode, Error> {
    if query.len() != 1 {
        return Err(Error::BadRequest);
    }
    let name = query.get("name").ok_or(Error::BadRequest)?;
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let object = body
        .as_object()
        .filter(|body| body.len() == 1)
        .ok_or(Error::BadRequest)?;
    let value = object.get(name).ok_or(Error::BadRequest)?;
    match name.as_str() {
        "userGraphSettings" => {
            let settings = value.as_str().ok_or(Error::BadRequest)?;
            let _: serde_json::Map<String, Value> = serde_json::from_str(settings).map_err(|_| Error::BadRequest)?;
        }
        "appConsumptionHorizon" => {
            let horizon = match value.as_str() {
                Some(text) => serde_json::from_str(text).map_err(|_| Error::BadRequest)?,
                None => value.clone(),
            };
            let times = horizon.as_object().ok_or(Error::BadRequest)?;
            if !(times.contains_key("chat") || times.contains_key("channel"))
                || times.values().any(|time| time.as_i64().is_none_or(|time| time < 0))
            {
                return Err(Error::BadRequest);
            }
        }
        _ => return Err(Error::BadRequest),
    }
    property_repo::set_user(&app.db, &caller.id, name, value).await?;
    events::own(&app, &caller, "CustomUserProperties", &body).await;
    Ok(StatusCode::OK)
}
