//! Outgoing webhooks: a team's owner registers one on the middle tier with the client's own session, a channel post
//! that @mentions it goes to a test receiver on loopback signed with HMAC-SHA256, and the receiver's answer comes
//! back to the real engine as the webhook's reply in the post's thread. Teams, people, webhooks and messages are
//! synthetic.

mod client;
mod concurrency;
mod delivery;
pub(crate) mod receiver;
mod registry;

use std::sync::Arc;

use base64::Engine as _;
use reqwest::Method;
use serde_json::{Value, json};
use teams_auth::secrets::MemorySecretStore;
use teams_auth::session::Session;
use teams_auth::token::SPACES_RESOURCE;
use teams_core::account::MIDDLE_TIER_GTM;
use teams_proto::TeamWrite;

use crate::bots::Registry;
use crate::support::Backend;
use crate::teams::{ORIGIN, Seat};

/// The webhooks of a team, under the middle tier.
pub fn hooks_path(team: &str) -> String {
    format!("beta/teams/{team}/outgoingWebhooks")
}

/// A server with an administrator (`root`), a team owner Ada, a member Bert and Cora, who is on no team.
pub struct Crew {
    pub registry: Registry,
    pub ada: Seat,
    pub bert: Seat,
    pub cora: Seat,
    pub team: String,
    pub group: String,
    pub channel: String,
    sessions: Vec<(String, Arc<Session<MemorySecretStore>>)>,
}

/// The server's settings for a receiver on loopback over plain HTTP.
pub fn loopback(config: &mut ngteams_server::config::Config) {
    config.public_origin = url::Url::parse(ORIGIN).unwrap();
    config.bots.allow_http_endpoints = true;
    config.bots.allowed_networks = vec!["127.0.0.0/8".parse().unwrap()];
}

impl Crew {
    pub async fn open(backend: Backend) -> Self {
        Self::open_with(backend, |_| {}).await
    }

    pub async fn open_with(backend: Backend, configure: impl FnOnce(&mut ngteams_server::config::Config)) -> Self {
        let registry = Registry::open_with(backend, |config| {
            loopback(config);
            configure(config);
        })
        .await;
        Self::over(registry).await
    }

    async fn over(registry: Registry) -> Self {
        let server = &registry.server;
        let ada = Seat::new(server, "ada@own.test", "Ada Lovelace").await;
        let bert = Seat::new(server, "bert@own.test", "Bert Sample").await;
        let cora = Seat::new(server, "cora@own.test", "Cora Outside").await;
        let (team, group) = ada.make_team("Rocket (synthetic)", false).await;
        ada.add(&team, &group, &[&bert]).await;
        let channel = ada.make_channel(&team, &group, "Launch", true).await;
        let mut sessions = Vec::new();
        for upn in ["ada@own.test", "bert@own.test", "cora@own.test"] {
            sessions.push((upn.to_owned(), Arc::new(server.session(upn).await)));
        }
        Self {
            registry,
            ada,
            bert,
            cora,
            team,
            group,
            channel,
            sessions,
        }
    }

    /// A middle-tier request as `upn` (`root@own.test` for the administrator) with that account's own token; the
    /// answer's status and JSON (null without one).
    pub async fn mt(&self, upn: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, bytes) = self.mt_raw(upn, method, path, body).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    pub async fn mt_raw(&self, upn: &str, method: Method, path: &str, body: Option<Value>) -> (u16, Vec<u8>) {
        let session = if upn == crate::bots::ROOT {
            Arc::clone(&self.registry.session)
        } else {
            let (_, session) = self.sessions.iter().find(|(name, _)| name == upn).unwrap();
            Arc::clone(session)
        };
        session.skypetoken().await.unwrap();
        let base = session.service_base(MIDDLE_TIER_GTM).unwrap();
        let base = base.replace(ORIGIN, &self.registry.server.origin);
        let token = session.access_token(SPACES_RESOURCE).await.unwrap();
        let mut request = reqwest::Client::new()
            .request(method, format!("{}/{path}", base.trim_end_matches('/')))
            .bearer_auth(token.token.expose());
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.bytes().await.unwrap().to_vec())
    }

    /// Ada registers a webhook for the team; its answer, with the token.
    pub async fn create(&self, name: &str, url: &str) -> Value {
        let body = json!({"name": name, "callbackUrl": url, "description": format!("{name} answers")});
        let (status, answer) = self
            .mt("ada@own.test", Method::POST, &hooks_path(&self.team), Some(body))
            .await;
        assert_eq!(status, 201, "{answer}");
        answer["value"].clone()
    }

    /// The team's members may not register webhooks: its owners still may.
    pub async fn close_to_members(&self) {
        let read = self
            .ada
            .engine
            .team_settings(self.team.as_str())
            .await
            .unwrap()
            .unwrap();
        let mut settings: Value = serde_json::from_str(&read).unwrap();
        settings["userSettings"]["uploadCustomApp"] = json!(false);
        let write = TeamWrite::Settings {
            team: self.team.clone(),
            settings: settings.to_string(),
        };
        self.ada.engine.team_write(write, None).await.unwrap();
    }

    pub async fn stop(self) {
        self.ada.stop();
        self.bert.stop();
        self.cora.stop();
        self.registry.server.stop().await;
    }
}

/// The key a webhook's token names.
pub fn key_of(token: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(token).unwrap()
}
