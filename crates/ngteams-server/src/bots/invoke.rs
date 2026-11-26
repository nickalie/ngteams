//! A person's invoke of a bot (`POST /api/chatsvc/<region>/v1/agents/28:<bot>/invoke`, `docs/protocol/bots.md`): a
//! synchronous proxy. The server checks that the person may use the bot's app and, when the request names a
//! conversation, that both are in it (with none, the activity names their 1:1 chat). It posts the invoke activity
//! to the bot once through the address guard ([`crate::outbound`]), and hands back the bot's answer in the chat
//! service's envelope. Nothing is kept, so any node serves it.
//!
//! Only `name`, `value`, the conversation, the message and a `messageBack`'s `text` pass from the request into the
//! activity: a `userAadToken` a client sends is never read, so no person's token reaches a bot. A bot's `401`, `403`
//! or `407`, or a redirect, reaches the client as `502`: they are the bot's, never a sign that the person's token was
//! refused.

use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::{Value, json};

use super::Bot;
use crate::app::App;
use crate::auth::extract::{Bearer, Ic3};
use crate::chatsvc::service::{InvokePlace, invoke_place};
use crate::db::DbError;
use crate::outbound::OutboundError;

/// The largest invoke a client may send: a dialog's answers, or a message's payload.
const MAX_INVOKE: usize = 256 * 1024;
/// The longest invoke name taken.
const MAX_NAME: usize = 128;

pub(super) fn router() -> axum::Router<Arc<App>> {
    axum::Router::new()
        .route("/api/chatsvc/{region}/v1/agents/{bot}/invoke", post(invoke))
        .layer(DefaultBodyLimit::max(MAX_INVOKE))
}

/// Why an invoke wasn't sent, or what became of it.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    BadRequest,
    Denied,
    Missing,
    /// The bot's answer can't be handed on, or it couldn't be reached.
    BadGateway,
    Timeout,
    Unavailable,
}

impl From<DbError> for Failure {
    fn from(error: DbError) -> Self {
        tracing::error!(%error, "bot invoke database failure");
        Self::Unavailable
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::BadRequest => (StatusCode::BAD_REQUEST, 201, "Invalid invoke"),
            Self::Denied => (StatusCode::FORBIDDEN, 209, "AclCheckFailed"),
            Self::Missing => (StatusCode::NOT_FOUND, 201, "NotFound"),
            Self::BadGateway => (StatusCode::BAD_GATEWAY, 502, "The app's answer can't be used"),
            Self::Timeout => (StatusCode::GATEWAY_TIMEOUT, 504, "The app didn't answer in time"),
            Self::Unavailable => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        (status, Json(json!({"errorCode": code, "message": message}))).into_response()
    }
}

/// What the request asks, as read from its body.
struct Asked {
    name: String,
    app_id: String,
    value: Value,
    conversation: Option<String>,
    message: Option<i64>,
    text: Option<String>,
}

fn read(body: &[u8]) -> Result<Asked, Failure> {
    let body: Value = serde_json::from_slice(body).map_err(|_| Failure::BadRequest)?;
    let text = |key: &str| body.get(key).and_then(Value::as_str).map(str::to_owned);
    let name = text("name")
        .filter(|name| !name.is_empty() && name.len() <= MAX_NAME)
        .ok_or(Failure::BadRequest)?;
    let conversation = match body.get("conversation") {
        None | Some(Value::Null) => None,
        Some(conversation) => Some(
            conversation
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or(Failure::BadRequest)?
                .to_owned(),
        ),
    };
    let message = match body.get("messageId") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) => Some(id.parse::<i64>().map_err(|_| Failure::BadRequest)?),
        Some(Value::Number(id)) => Some(id.as_i64().ok_or(Failure::BadRequest)?),
        Some(_) => return Err(Failure::BadRequest),
    };
    if message.is_some() && conversation.is_none() {
        return Err(Failure::BadRequest);
    }
    Ok(Asked {
        app_id: text("appId").unwrap_or_default(),
        value: body.get("value").cloned().unwrap_or(Value::Null),
        text: text("text").filter(|_| name == "message"),
        name,
        conversation,
        message,
    })
}

async fn invoke(
    State(app): State<Arc<App>>,
    Path((region, bot)): Path<(String, String)>,
    bearer: Bearer<Ic3>,
    body: Bytes,
) -> Response {
    if region != app.config.region {
        return Failure::Missing.into_response();
    }
    match ask(&app, &bearer.claims.oid, &bot, &body).await {
        Ok(response) => response,
        Err(failure) => failure.into_response(),
    }
}

/// Where the activity goes: the conversation's id and shape.
struct Conversation {
    id: String,
    place: InvokePlace,
}

async fn ask(app: &App, person_id: &str, bot: &str, body: &[u8]) -> Result<Response, Failure> {
    let person = crate::db::users::by_id(&app.db, person_id)
        .await?
        .filter(|person| !person.disabled)
        .ok_or(Failure::Denied)?;
    let asked = read(body)?;
    let bot_id = bot.strip_prefix("28:").ok_or(Failure::Missing)?;
    let bot = super::bot(&app.db, bot_id).await?.ok_or(Failure::Missing)?;
    if asked.app_id != bot.app_id && asked.app_id != bot.id {
        return Err(Failure::Missing);
    }
    if !super::may_use(&app.db, &bot, &person.id).await? {
        return Err(Failure::Denied);
    }
    let conversation = conversation(app, &asked, &person.id, &bot).await?;
    let activity = activity(app, &asked, &person, &bot, &conversation);
    let answer = send(app, &bot, activity).await;
    tracing::info!(name = %asked.name, bot = %bot.id, status = answer.as_ref().map_or_else(
        |failure| format!("{failure:?}"), |response| response.status().as_u16().to_string()), "bot invoke");
    answer
}

async fn conversation(app: &App, asked: &Asked, person: &str, bot: &Bot) -> Result<Conversation, Failure> {
    let Some(id) = &asked.conversation else {
        // no conversation open (a command list asked from the compose box): the person's 1:1 chat with the bot
        return Ok(Conversation {
            id: super::personal_chat(person, &bot.id),
            place: InvokePlace::Chat { personal: true },
        });
    };
    let (thread, post) = match id.split_once(";messageid=") {
        None => (id.as_str(), None),
        Some((thread, post)) => {
            let post = post
                .parse::<i64>()
                .ok()
                .filter(|post| *post > 0)
                .ok_or(Failure::BadRequest)?;
            (thread, Some(post))
        }
    };
    let messages: Vec<i64> = [post, asked.message].into_iter().flatten().collect();
    match invoke_place(app, thread, person, &bot.id, &messages).await? {
        InvokePlace::Denied => Err(Failure::Denied),
        InvokePlace::NoMessage => Err(Failure::Missing),
        place => Ok(Conversation { id: id.clone(), place }),
    }
}

/// The activity the bot gets: an `invoke` with its name and value, or for a card's plain submit a `message` carrying
/// the value (and a `messageBack`'s text).
fn activity(
    app: &App,
    asked: &Asked,
    person: &crate::db::users::User,
    bot: &Bot,
    conversation: &Conversation,
) -> Value {
    let kind = match &conversation.place {
        InvokePlace::Channel { .. } => "channel",
        InvokePlace::Chat { personal: false } => "groupChat",
        _ => "personal",
    };
    let from = super::activity::Sender {
        id: &person.id,
        name: &person.display_name,
    };
    let to = super::activity::Recipient {
        id: &bot.id,
        name: &bot.name,
    };
    let mut activity = super::activity::envelope(kind, &conversation.id, &from, &to, crate::chatsvc::now());
    activity["id"] = format!("f:{}", uuid::Uuid::new_v4().simple()).into();
    activity["channelData"]["source"] = json!({"name": "message"});
    activity["entities"] = super::activity::client_info();
    activity["value"] = asked.value.clone();
    if asked.name == "message" {
        activity["type"] = "message".into();
        activity["text"] = asked.text.clone().unwrap_or_default().into();
        activity["textFormat"] = "plain".into();
    } else {
        activity["type"] = "invoke".into();
        activity["name"] = asked.name.clone().into();
    }
    if let Some(message) = asked.message {
        activity["replyToId"] = message.to_string().into();
        activity["channelData"]["legacy"] = json!({"replyToId": message.to_string()});
    }
    if let InvokePlace::Channel { channel, team, group } = &conversation.place {
        activity["channelData"]["teamsTeamId"] = team.clone().into();
        activity["channelData"]["teamsChannelId"] = channel.clone().into();
        activity["channelData"]["team"] = json!({"id": team, "aadGroupId": group});
        activity["channelData"]["channel"] = json!({ "id": channel });
    }
    super::activity::addressed(&mut activity, &super::service_url(app, bot), app.issuer.tenant());
    activity
}

/// Posts `activity` to the bot once; the answer to hand back.
async fn send(app: &App, bot: &Bot, activity: Value) -> Result<Response, Failure> {
    let endpoint = bot.endpoint.as_deref().ok_or(Failure::BadGateway)?;
    let service_url = activity["serviceUrl"].as_str().unwrap_or_default();
    let token = if bot.signed {
        Some(app.bot_keys.activity_token(&bot.id, service_url).map_err(|error| {
            tracing::warn!(%error, "an invoke's token couldn't be made");
            Failure::Unavailable
        })?)
    } else {
        None
    };
    let body = activity.to_string().into_bytes();
    match app.outbound.post_json(endpoint, token.as_deref(), body).await {
        Ok(answer) => handed_back(answer.status, &answer.body),
        Err(OutboundError::Timeout) => Err(Failure::Timeout),
        Err(_) => Err(Failure::BadGateway),
    }
}

/// The bot's answer as the chat service gives it: a success with its JSON body (a message extension's or a dialog's
/// answer in the `{"responseType": <kind>, <kind>: …}` envelope), or the bot's error status.
fn handed_back(status: u16, body: &[u8]) -> Result<Response, Failure> {
    let empty = body.iter().all(u8::is_ascii_whitespace);
    let value = if empty {
        None
    } else {
        Some(serde_json::from_slice::<Value>(body).map_err(|_| Failure::BadGateway)?)
    };
    let status = match status {
        401 | 403 | 407 => return Err(Failure::BadGateway),
        200..=299 | 400..=599 => StatusCode::from_u16(status).map_err(|_| Failure::BadGateway)?,
        _ => return Err(Failure::BadGateway),
    };
    Ok(match value {
        None => status.into_response(),
        Some(value) => (
            status,
            [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
            enveloped(value).to_string(),
        )
            .into_response(),
    })
}

/// A bot's `{"composeExtension": …}` or `{"task": …}` with the `responseType` Teams' chat service names it by.
fn enveloped(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut()
        && !object.contains_key("responseType")
        && let Some(kind) = ["composeExtension", "task"]
            .into_iter()
            .find(|kind| object.contains_key(*kind))
    {
        object.insert("responseType".to_owned(), kind.into());
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bots_answer_is_enveloped_by_its_kind_and_its_auth_statuses_become_502() {
        let compose = enveloped(json!({"composeExtension": {"type": "result"}}));
        assert_eq!(compose["responseType"], "composeExtension");
        let task = enveloped(json!({"task": {"type": "message", "value": "x"}}));
        assert_eq!(task["responseType"], "task");
        let universal = json!({"statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "x"});
        assert_eq!(enveloped(universal.clone()), universal);
        let given = json!({"responseType": "task", "task": {}, "composeExtension": {}});
        assert_eq!(enveloped(given.clone()), given);
        for status in [401, 403, 407, 302, 100] {
            assert_eq!(handed_back(status, b"").unwrap_err(), Failure::BadGateway, "{status}");
        }
        assert_eq!(handed_back(200, b"<html>").unwrap_err(), Failure::BadGateway);
        assert_eq!(handed_back(501, b" ").unwrap().status(), 501);
        assert_eq!(handed_back(200, b"").unwrap().status(), 200);
    }

    #[test]
    fn only_a_named_invoke_is_read_and_a_message_needs_its_conversation() {
        assert_eq!(read(b"[1]").err(), Some(Failure::BadRequest));
        assert_eq!(read(br#"{"name": ""}"#).err(), Some(Failure::BadRequest));
        assert_eq!(
            read(br#"{"name": "task/fetch", "messageId": "7"}"#).err(),
            Some(Failure::BadRequest)
        );
        assert_eq!(
            read(br#"{"name": "task/fetch", "conversation": {"id": 3}}"#).err(),
            Some(Failure::BadRequest)
        );
        let asked = read(
            br#"{"name": "task/fetch", "conversation": {"id": "19:c@thread.v2"}, "messageId": 7,
            "text": "kept only for a message", "userAadToken": "x"}"#,
        )
        .unwrap();
        assert_eq!(asked.message, Some(7));
        assert_eq!(asked.text, None);
        assert_eq!(asked.value, Value::Null);
        let message = read(br#"{"name": "message", "conversation": {"id": "c"}, "text": "hi"}"#).unwrap();
        assert_eq!(message.text.as_deref(), Some("hi"));
    }
}
