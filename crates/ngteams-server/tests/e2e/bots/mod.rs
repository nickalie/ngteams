//! Bots: the app registry an administrator fills through the real admin API, the credentials a bot gets a token
//! with in MSAL's exact form, and the keys the server signs activities to a bot with. Apps, bots and their names here
//! are synthetic.

mod attachments;
mod connector;
mod credentials;
mod delivery;
pub mod endpoint;
mod invoke;
mod keys;
mod people;
mod registry;

use std::io::{Cursor, Write as _};
use std::sync::Arc;

use bytes::Bytes;
use ngteams_server::config::Config;
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::admin::{AdminClient, FirstAdmin};
use teams_auth::secrets::{MemorySecretStore, Secret};
use teams_auth::session::Session;
use teams_auth::token::SPACES_RESOURCE;

use crate::support::{Backend, PASSWORD, TestServer};

pub const ROOT: &str = "root@own.test";
pub const APPS: &str = "/api/admin/v1/apps";

/// A server with an administrator signed in through the client's own session.
pub struct Registry {
    pub server: TestServer,
    pub session: Arc<Session<MemorySecretStore>>,
}

impl Registry {
    pub async fn open(backend: Backend) -> Self {
        Self::open_with(backend, |_| {}).await
    }

    pub async fn open_with(backend: Backend, configure: impl FnOnce(&mut Config)) -> Self {
        let server = TestServer::start_with(backend, |config| {
            config.setup.require_code = false;
            configure(config);
        })
        .await;
        Self::over(server).await
    }

    /// The administrator of `server`: the first-admin setup, then a sign-in.
    pub async fn over(server: TestServer) -> Self {
        let admin = AdminClient::with_loopback(&server.origin).unwrap();
        if admin.setup_state().await.unwrap().needs_admin {
            let password = Secret::new(PASSWORD);
            let first = FirstAdmin {
                code: None,
                upn: ROOT,
                display_name: "Root",
                password: &password,
            };
            admin.create_first_admin(first).await.unwrap();
        }
        let session = Arc::new(server.session(ROOT).await);
        Self { server, session }
    }

    pub async fn token(&self) -> String {
        let token = self.session.access_token(SPACES_RESOURCE).await.unwrap();
        token.token.expose().to_owned()
    }

    /// An admin API call with the administrator's token; the answer's status and JSON (null without one).
    pub async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let token = self.token().await;
        call(&self.server, method, path, Some(&token), body).await
    }

    /// Registers a bot from the form; the answer, with its secret.
    pub async fn register(&self, name: &str, endpoint: &str) -> Value {
        let body = json!({"name": name, "endpoint": endpoint});
        let (status, answer) = self.call(Method::POST, APPS, Some(body)).await;
        assert_eq!(status, 201, "{answer}");
        answer
    }

    /// Uploads a Teams app package; the status and answer.
    pub async fn upload(&self, path: &str, method: Method, package: Bytes) -> (u16, Value) {
        let token = self.token().await;
        let response = reqwest::Client::new()
            .request(method, self.server.url(path))
            .bearer_auth(token)
            .header("content-type", "application/zip")
            .body(package)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

pub async fn call(
    server: &TestServer,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = reqwest::Client::new().request(method, server.url(path));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

pub fn app_path(id: &str) -> String {
    format!("{APPS}/{id}")
}

pub fn error_code(answer: &Value) -> &str {
    answer["error"]["code"].as_str().unwrap_or_default()
}

/// A JWT's claims, unverified.
pub fn claims(token: &str) -> Value {
    use base64::Engine as _;
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

/// A PNG of `width` by `height`.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([u8::try_from(x % 256).unwrap(), u8::try_from(y % 256).unwrap(), 200, 255])
    });
    let mut bytes = Vec::new();
    picture
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

/// A synthetic Teams manifest (schema 1.17) for an app `id` with the bot `bot`.
pub fn manifest(id: &str, bot: &str, version: &str) -> Value {
    json!({
        "$schema": "https://developer.microsoft.com/en-us/json-schemas/teams/v1.17/MicrosoftTeams.schema.json",
        "manifestVersion": "1.17",
        "version": version,
        "id": id,
        "developer": {
            "name": "Synthetic Tools",
            "websiteUrl": "https://tools.example.test",
            "privacyUrl": "https://tools.example.test/privacy",
            "termsOfUseUrl": "https://tools.example.test/terms"
        },
        "icons": {"color": "color.png", "outline": "outline.png"},
        "name": {"short": "Polls (synthetic)", "full": "Synthetic Polls for teams"},
        "description": {"short": "Quick polls", "full": "Runs quick polls in chats and channels. Synthetic."},
        "accentColor": "#4F6BED",
        "bots": [{
            "botId": bot,
            "scopes": ["personal", "team", "groupChat"],
            "supportsFiles": false,
            "isNotificationOnly": false,
            "commandLists": [{"scopes": ["personal"], "commands": [{"title": "poll", "description": "Start a poll"}]}]
        }],
        "composeExtensions": [{
            "botId": bot,
            "commands": [{"id": "searchPolls", "type": "query", "title": "Search polls",
                          "parameters": [{"name": "q", "title": "Query"}]}]
        }],
        "permissions": ["identity", "messageTeamMembers"],
        "validDomains": ["tools.example.test"]
    })
}

/// A package: `manifest.json` and the icons, with `extra` files.
pub fn package(manifest: &Value, color: &[u8], outline: &[u8], extra: &[(&str, &[u8])]) -> Bytes {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let manifest = serde_json::to_vec_pretty(manifest).unwrap();
    let files = [
        ("manifest.json", manifest.as_slice()),
        ("color.png", color),
        ("outline.png", outline),
    ];
    for (name, bytes) in files.iter().chain(extra) {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    Bytes::from(zip.finish().unwrap().into_inner())
}

/// A well-formed package of the app `id` with the bot `bot`.
pub fn good_package(id: &str, bot: &str, version: &str) -> Bytes {
    package(&manifest(id, bot, version), &png(192, 192), &png(32, 32), &[])
}
