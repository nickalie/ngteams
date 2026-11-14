use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::extract::{State, rejection::JsonRejection};
use axum::http::{StatusCode, header::LOCATION};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use super::{Caller, Error, repo, wire};
use crate::app::App;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Create {
    members: Vec<Member>,
    properties: Properties,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    id: String,
    role: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Properties {
    thread_type: String,
    #[serde(default)]
    fixed_roster: bool,
    #[serde(default, rename = "uniquerosterthread")]
    unique: bool,
    topic: Option<String>,
}

impl Create {
    fn validate(&self, caller: &Caller) -> Result<Vec<String>, Error> {
        let p = &self.properties;
        if p.thread_type != "chat"
            || p.fixed_roster != p.unique
            || self.members.len() < 2
            || (p.unique && (self.members.len() != 2 || p.topic.is_some()))
        {
            return Err(Error::BadRequest);
        }
        let members: Vec<_> = self
            .members
            .iter()
            .map(|member| {
                let id = member.id.strip_prefix("8:orgid:").ok_or(Error::BadRequest)?;
                if member.role != "Admin" || uuid::Uuid::parse_str(id).is_err() {
                    return Err(Error::BadRequest);
                }
                Ok(id.to_owned())
            })
            .collect::<Result<_, _>>()?;
        if members.iter().collect::<HashSet<_>>().len() != members.len() {
            return Err(Error::BadRequest);
        }
        if !members.contains(&caller.id) {
            return Err(Error::Denied);
        }
        Ok(members)
    }
}

pub(super) async fn create(
    State(app): State<Arc<App>>,
    caller: Caller,
    body: Result<Json<Create>, JsonRejection>,
) -> Result<Response, Error> {
    let Json(body) = body.map_err(|_| Error::BadRequest)?;
    let members = body.validate(&caller)?;
    let id = if body.properties.unique {
        let mut ids = members.clone();
        ids.sort();
        format!("19:{}@unq.gbl.spaces", ids.join("_"))
    } else {
        format!("19:{}@thread.v2", uuid::Uuid::new_v4().simple())
    };
    let at = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX);
    let chat = repo::Chat {
        id,
        creator: caller.id.clone(),
        topic: body.properties.topic,
        unique_roster: body.properties.unique,
        created_at: at,
        version: at,
    };
    let inserted = repo::create(&app.db, &chat, &members).await?;
    let (chat, members) = super::member_of(&app, &caller, &chat.id).await?;
    let thread = wire::thread(&app, &caller, &chat, &members);
    if inserted {
        super::events::publish(&app, &caller, &members, "ThreadUpdate", &thread).await;
    }
    Ok((
        StatusCode::CREATED,
        [(LOCATION, format!("{}/v1/threads/{}", wire::base(&app), chat.id))],
        Json(json!({"threadResource": thread})),
    )
        .into_response())
}
