use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde_json::{Value, json};
use teams_auth::secrets::Secret;
use tokio::sync::mpsc;

use super::super::Node;

#[derive(Clone)]
pub enum Reply {
    Normal,
    Json(u16, Value),
    Text,
    Huge,
    Slow,
}

pub struct Got {
    pub activity: Value,
    pub signed: bool,
    pub no_user_token: bool,
}

struct Trust {
    keys: JwkSet,
    bot: String,
    origin: String,
    users: Vec<Secret>,
}

struct Shared {
    trust: Mutex<Option<Trust>>,
    reply: Mutex<Reply>,
    sender: mpsc::UnboundedSender<Got>,
}

pub struct Bot {
    pub url: String,
    shared: Arc<Shared>,
    receiver: tokio::sync::Mutex<mpsc::UnboundedReceiver<Got>>,
    task: tokio::task::JoinHandle<()>,
}

impl Bot {
    pub async fn start() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            trust: Mutex::new(None),
            reply: Mutex::new(Reply::Normal),
            sender,
        });
        let router = axum::Router::new()
            .route("/messages", axum::routing::post(receive))
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/messages", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            url,
            shared,
            receiver: tokio::sync::Mutex::new(receiver),
            task,
        }
    }

    pub async fn trust(&self, node: &Node, bot: &str, users: Vec<Secret>) {
        let keys = node
            .http
            .get(format!("{}/bot/v1/.well-known/keys", node.endpoint))
            .send()
            .await
            .unwrap()
            .json::<JwkSet>()
            .await
            .unwrap();
        *self.shared.trust.lock().unwrap() = Some(Trust {
            keys,
            bot: bot.into(),
            origin: node.endpoint.clone(),
            users,
        });
    }

    pub fn reply(&self, reply: Reply) {
        *self.shared.reply.lock().unwrap() = reply;
    }

    pub async fn next(&self) -> Got {
        let got = tokio::time::timeout(Duration::from_secs(15), self.receiver.lock().await.recv())
            .await
            .expect("the bot gets an activity")
            .unwrap();
        assert!(got.signed, "the bot rejects an invalid activity token");
        assert!(got.no_user_token, "a user token reached the bot");
        got
    }

    pub async fn quiet(&self) {
        assert!(
            tokio::time::timeout(Duration::from_millis(400), self.receiver.lock().await.recv())
                .await
                .is_err(),
            "the bot got an unexpected activity"
        );
    }
}

impl Drop for Bot {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn authentic(trust: &Trust, headers: &HeaderMap, activity: &Value) -> bool {
    let Some(token) = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    let Ok(header) = decode_header(token) else { return false };
    let Some(key) = header.kid.as_deref().and_then(|kid| trust.keys.find(kid)) else {
        return false;
    };
    let Ok(key) = DecodingKey::from_jwk(key) else {
        return false;
    };
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[&trust.bot]);
    validation.set_issuer(&[format!("{}/bot", trust.origin)]);
    validation.validate_nbf = true;
    validation.set_required_spec_claims(&["exp", "iat", "nbf", "iss", "aud"]);
    let Ok(claims) = decode::<Value>(token, &key, &validation) else {
        return false;
    };
    claims.claims["serviceurl"] == activity["serviceUrl"]
        && activity["serviceUrl"] == format!("{}/bot/", trust.origin)
        && activity["recipient"]["id"] == format!("28:{}", trust.bot)
}

async fn receive(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: String) -> Response {
    let activity: Value = serde_json::from_str(&body).unwrap();
    let (signed, no_user_token) = {
        let guard = shared.trust.lock().unwrap();
        let trust = guard.as_ref().unwrap();
        let no_user_token = activity.get("userAadToken").is_none()
            && trust.users.iter().all(|token| {
                !body.contains(token.expose())
                    && headers.values().all(|v| {
                        !v.as_bytes()
                            .windows(token.expose().len())
                            .any(|part| part == token.expose().as_bytes())
                    })
            });
        (authentic(trust, &headers, &activity), no_user_token)
    };
    let reply = shared.reply.lock().unwrap().clone();
    let response = normal(&activity);
    let _ = shared.sender.send(Got {
        activity,
        signed,
        no_user_token,
    });
    if !signed || !no_user_token {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match reply {
        Reply::Normal => response,
        Reply::Json(status, body) => (StatusCode::from_u16(status).unwrap(), axum::Json(body)).into_response(),
        Reply::Text => (StatusCode::OK, "this is not JSON").into_response(),
        Reply::Huge => axum::Json(json!({"text": "x".repeat(300 * 1024)})).into_response(),
        Reply::Slow => {
            tokio::time::sleep(Duration::from_secs(12)).await;
            axum::Json(json!({"task": {"type": "message", "value": "too late"}})).into_response()
        }
    }
}

pub fn card(title: &str) -> Value {
    json!({"type": "AdaptiveCard", "version": "1.5",
        "body": [{"type": "TextBlock", "text": title}, {"type": "Input.Text", "id": "answer", "label": "Answer"}],
        "actions": [{"type": "Action.Submit", "title": "Send", "data": {"poll": 7}},
            {"type": "Action.Execute", "title": "Vote", "verb": "vote", "data": {"poll": 7}}]})
}

pub fn attachment(title: &str) -> Value {
    json!({"contentType": "application/vnd.microsoft.card.adaptive", "content": card(title)})
}

fn normal(activity: &Value) -> Response {
    let body = match activity["name"].as_str().unwrap_or("") {
        "composeExtension/fetchCommands" => json!({"composeExtension": {"type": "command", "commands": [
            {"id": "poll", "type": "action", "title": "New poll", "fetchTask": true},
            {"id": "search", "type": "query", "title": "Find poll"}]}}),
        "composeExtension/query" | "composeExtension/selectItem" => json!({"composeExtension": {
            "type": "result", "attachmentLayout": "list", "attachments": [{
                "contentType": "application/vnd.microsoft.card.thumbnail", "content": {"title": "Poll 7"},
                "preview": {"contentType": "application/vnd.microsoft.card.thumbnail", "content": {
                    "title": "Poll 7", "tap": {"type": "invoke", "value": {"poll": 7}}}}}]}}),
        "composeExtension/fetchTask" | "task/fetch" => json!({"task": {"type": "continue", "value": {
            "title": "New poll", "width": 600, "height": "medium", "card": attachment("Ask")}}}),
        "composeExtension/submitAction" | "task/submit" => json!({"task": {"type": "message", "value": "Saved"}}),
        "adaptiveCard/action" => json!({"statusCode": 200, "type": "application/vnd.microsoft.card.adaptive",
            "value": card("Voted")}),
        _ => return StatusCode::OK.into_response(),
    };
    axum::Json(body).into_response()
}
