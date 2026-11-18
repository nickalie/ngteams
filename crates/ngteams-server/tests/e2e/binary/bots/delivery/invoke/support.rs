use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use serde_json::{Value, json};
use teams_core::bots::BotAsk;
use teams_proto::bot::{CardInvoke, CommandContext, InvokeAnswer, InvokeKind};

use super::super::{
    ADA, Actor, BEA, Backend, CORA, Conn, Deployment, Node, Secret, bot_token, encode, id_of, keep_log, one_node,
    register, synced,
};
use super::endpoint::{Bot, Got, attachment};

diesel::table! {
    chat_members (chat_id, user_id) {
        chat_id -> Text,
        user_id -> Text,
        position -> Integer,
        joined_at -> BigInt,
        history_from -> BigInt,
    }
}

pub struct Case {
    pub node: Node,
    pub bot: Bot,
    pub bot_id: String,
    pub root: Secret,
    pub chat: String,
    pub ada: Actor,
    pub bea: Actor,
    pub cora: Actor,
    pub secret: String,
    name: String,
    _deployment: Deployment,
}

impl Case {
    pub async fn open(backend: Backend, name: &str) -> Self {
        eprintln!("bot invoke QA: {name}: {backend:?}");
        let table = "allow_http_endpoints = true\nallowed_networks = [\"127.0.0.0/8\"]\n";
        let (deployment, node, _, root) = one_node(backend, table).await;
        let bot = Bot::start().await;
        let (bot_id, secret) = register(&node, &root, "Invoke QA (synthetic)", &bot.url, false).await;
        let ada = Actor::sign_in(&node, ADA).await;
        let bea = Actor::sign_in(&node, BEA).await;
        let cora = Actor::sign_in(&node, CORA).await;
        let mut users = Vec::new();
        for actor in [&ada, &bea, &cora] {
            let session = &actor.session;
            for resource in [teams_proto::IC3_AUDIENCE, teams_auth::token::SPACES_RESOURCE] {
                users.push(session.access_token(resource).await.unwrap().token);
            }
        }
        bot.trust(&node, &bot_id, users).await;
        let chat = group(&node, &ada, &bea).await;
        seed_bot(&node, &chat, &bot_id).await;
        Self {
            node,
            bot,
            bot_id,
            root,
            chat,
            ada,
            bea,
            cora,
            secret,
            name: format!("invoke-{name}-{backend:?}"),
            _deployment: deployment,
        }
    }

    pub async fn connector(&self) -> Conn<'_> {
        Conn {
            node: &self.node,
            base: format!("{}/bot/", self.node.endpoint),
            token: Some(bot_token(&self.node, &self.bot_id, &self.secret).await),
        }
    }

    pub async fn invoke(&self, actor: &Actor, kind: InvokeKind) -> InvokeAnswer {
        actor
            .engine()
            .bot_invoke(BotAsk {
                bot: teams_proto::Mri::new(format!("28:{}", self.bot_id)),
                app_id: self.bot_id.clone(),
                conversation: Some(self.chat.clone()),
                kind,
            })
            .await
            .unwrap()
    }

    pub async fn got(&self, actor: &Actor, name: &str) -> Got {
        let got = self.bot.next().await;
        assert_eq!(got.activity["conversation"]["id"], self.chat);
        assert_eq!(got.activity["from"]["id"], format!("29:{}", id_of(actor)));
        assert_eq!(got.activity["from"]["aadObjectId"], id_of(actor));
        assert_eq!(got.activity["channelId"], "msteams");
        if name == "message" || name == "messageReaction" {
            assert_eq!(got.activity["type"], name);
        } else {
            assert_eq!(got.activity["type"], "invoke");
            assert_eq!(got.activity["conversation"]["conversationType"], "groupChat");
            assert_eq!(got.activity["conversation"]["isGroup"], true);
            assert_eq!(got.activity["name"], name);
        }
        got
    }

    pub async fn card(&self) -> String {
        let (status, answer) = self
            .connector()
            .await
            .send(
                &self.chat,
                json!({"type": "message", "attachments": [attachment("Lunch?")]}),
            )
            .await;
        assert_eq!(status, 200);
        let id = answer["id"].as_str().unwrap().to_owned();
        synced(&self.bea, &self.chat).await;
        let messages = synced(&self.ada, &self.chat).await;
        let shown = messages.iter().find(|message| message.id == id).unwrap();
        assert_eq!(shown.message_type, "RichText/Media_Card");
        assert!(
            teams_proto::chat::card::card_message_with_inputs(&shown.content)
                .unwrap()
                .drawn()
        );
        id
    }

    pub async fn raw(&self, actor: &Actor, body: &Value) -> u16 {
        let token = actor
            .session
            .access_token(teams_proto::IC3_AUDIENCE)
            .await
            .unwrap()
            .token;
        let mut body = body.clone();
        body["userAadToken"] = json!(token.expose());
        self.node
            .http
            .post(format!(
                "{}/api/chatsvc/own/v1/agents/28:{}/invoke",
                self.node.endpoint, self.bot_id
            ))
            .bearer_auth(token.expose())
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16()
    }

    pub async fn close(mut self) {
        keep_log(&self.node, &self.name);
        self.ada.engine.shutdown();
        self.bea.engine.shutdown();
        self.cora.engine.shutdown();
        self.node.stop().await;
    }
}

async fn group(node: &Node, ada: &Actor, bea: &Actor) -> String {
    let token = ada.session.access_token(teams_proto::IC3_AUDIENCE).await.unwrap().token;
    let body = json!({"members": [
        {"id": ada.mri.as_str(), "role": "Admin"},
        {"id": bea.mri.as_str(), "role": "Admin"}],
        "properties": {"threadType": "chat", "fixedRoster": false, "uniquerosterthread": false,
            "topic": "Synthetic bot QA"}});
    let response = node
        .http
        .post(format!("{}/api/chatsvc/own/v1/threads", node.endpoint))
        .bearer_auth(token.expose())
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    let body: Value = response.json().await.unwrap();
    body["threadResource"]["id"].as_str().unwrap().to_owned()
}

async fn seed_bot(node: &Node, chat: &str, bot: &str) {
    let config = ngteams_server::config::Config::load(Some(&node.config)).unwrap();
    let db = ngteams_server::db::Db::connect(&config.database_url, 1).await.unwrap();
    let query = diesel::insert_into(chat_members::table).values((
        chat_members::chat_id.eq(chat),
        chat_members::user_id.eq(bot),
        chat_members::position.eq(2),
        chat_members::joined_at.eq(0_i64),
        chat_members::history_from.eq(0_i64),
    ));
    let result: Result<usize, ngteams_server::db::DbError> =
        async { ngteams_server::with_conn!(&db, |conn| Ok(query.execute(conn).await?)) }.await;
    assert_eq!(result.unwrap(), 1);
}

pub fn fetch() -> InvokeKind {
    InvokeKind::FetchCommands {
        context: CommandContext::Compose,
    }
}

pub fn action(message: &str, data: Value) -> CardInvoke {
    CardInvoke {
        message: Some(message.into()),
        data,
        verb: Some("vote".into()),
        ..CardInvoke::default()
    }
}

pub fn activity_path(chat: &str, message: &str) -> String {
    format!("v3/conversations/{}/activities/{message}", encode(chat))
}
