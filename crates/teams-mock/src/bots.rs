//! The chat service's bot invokes (`POST v1/agents/28:<bot>/invoke`, `docs/protocol/bots.md`) and the bots a test
//! scripts ([`crate::ChatServiceMock::script_bot`]): each invoke is checked and kept
//! ([`crate::ChatServiceMock::bot_invokes`]), then answered as the bot's script says for its `name`, and the bot may
//! post a message into the invoke's conversation, as a bot that answers a submit does.
//!
//! The token rule is checked here as the client must keep it: a Microsoft account's invoke carries its
//! `userAadToken`, an own-server account's carries none; either way wrong is a 400, as is a body without the fields
//! Teams sends. A bot nobody scripted, or an invoke name its script doesn't answer, is a 404 with no body (the
//! service's own answers to those weren't seen), and a conversation the caller isn't in a 403.
use std::collections::BTreeMap;

use serde_json::Value;
use wiremock::ResponseTemplate;

use crate::chatsvc::{Call, json, status};
use crate::reads::member_of;
use crate::world::State;

/// The synthetic answers of `tests/fixtures/bots/answers.json`, by name.
const ANSWERS: &str = include_str!("../../../tests/fixtures/bots/answers.json");

/// The synthetic answer `name` of `tests/fixtures/bots/answers.json` (`commands`, `dialog`, `card`, …), for a script.
#[must_use]
pub fn bot_answer(name: &str) -> Option<Value> {
    serde_json::from_str::<Value>(ANSWERS).ok()?.get(name).cloned()
}

/// The made-up apps with message extensions of `tests/fixtures/bots/extension-apps.json`.
const EXTENSION_APPS: &str = include_str!("../../../tests/fixtures/bots/extension-apps.json");

/// The made-up apps with message extensions (`tests/fixtures/bots/extension-apps.json`): each app's definition and
/// its entitlement's state there, in order, for [`crate::ChatServiceMock::add_app`] and
/// [`crate::ChatServiceMock::install_app`].
#[must_use]
pub fn extension_apps() -> Vec<(Value, String)> {
    let fixture = extension_apps_answer();
    let value = &fixture["value"];
    value["userEntitlements"]
        .as_object()
        .into_iter()
        .flat_map(serde_json::Map::values)
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|entitlement| {
            let id = entitlement.get("id")?.as_str()?;
            let state = entitlement.get("state")?.as_str()?.to_owned();
            Some((value["definitions"].get(id)?.clone(), state))
        })
        .collect()
}

/// The answer of `tests/fixtures/bots/extension-apps.json` as a whole, its placeholders filled with the mock's made-up
/// ids (the account's, `…e00N` for each app and `…eb0N` for its bot), as the middle tier gives the account's apps.
#[must_use]
pub fn extension_apps_answer() -> Value {
    let filled = [
        ("<guid:a>", "00000000-0000-4000-8000-00000000000a"),
        ("<guid:app-finder>", "00000000-0000-4000-8000-00000000e001"),
        ("<guid:app-praise>", "00000000-0000-4000-8000-00000000e002"),
        ("<guid:app-tasks>", "00000000-0000-4000-8000-00000000e003"),
        ("<guid:app-tickets>", "00000000-0000-4000-8000-00000000e004"),
        ("<guid:bot-finder>", "00000000-0000-4000-8000-00000000eb01"),
        ("<guid:bot-praise>", "00000000-0000-4000-8000-00000000eb02"),
        ("<guid:bot-tasks>", "00000000-0000-4000-8000-00000000eb03"),
        ("<guid:bot-tickets>", "00000000-0000-4000-8000-00000000eb04"),
    ]
    .iter()
    .fold(EXTENSION_APPS.to_owned(), |text, (placeholder, id)| {
        text.replace(placeholder, id)
    });
    serde_json::from_str(&filled).unwrap_or(Value::Null)
}

/// How a scripted bot answers one invoke name.
#[derive(Clone, Debug, PartialEq)]
pub struct BotReply {
    status: u16,
    body: Option<Value>,
    post: Option<(String, String)>,
}

impl BotReply {
    /// A 200 carrying `body`.
    #[must_use]
    pub fn answer(body: Value) -> Self {
        Self {
            status: 200,
            body: Some(body),
            post: None,
        }
    }

    /// A 200 with nothing in it.
    #[must_use]
    pub const fn done() -> Self {
        Self {
            status: 200,
            body: None,
            post: None,
        }
    }

    /// An error status with no body.
    #[must_use]
    pub const fn status(status: u16) -> Self {
        Self {
            status,
            body: None,
            post: None,
        }
    }

    /// The bot also posts a message of `message_type` with `content` into the invoke's conversation.
    #[must_use]
    pub fn posting(mut self, message_type: impl Into<String>, content: impl Into<String>) -> Self {
        self.post = Some((message_type.into(), content.into()));
        self
    }
}

/// An invoke the mock took.
#[derive(Clone, Debug, PartialEq)]
pub struct BotInvokeRecord {
    /// The caller's MRI.
    pub caller: String,
    /// The bot of the path, `28:<bot id>`.
    pub bot: String,
    /// The body, as sent.
    pub body: Value,
}

/// A scripted bot.
#[derive(Clone, Debug, Default)]
pub(crate) struct Script {
    name: String,
    replies: BTreeMap<String, BotReply>,
}

/// The bots scripted and the invokes taken.
#[derive(Clone, Debug, Default)]
pub(crate) struct Bots {
    scripts: BTreeMap<String, Script>,
    pub(crate) invokes: Vec<BotInvokeRecord>,
}

impl Bots {
    pub(crate) fn script(&mut self, bot: &str, name: &str, replies: &[(&str, BotReply)]) {
        let script = self.scripts.entry(bot.to_owned()).or_default();
        name.clone_into(&mut script.name);
        for (invoke, reply) in replies {
            script.replies.insert((*invoke).to_owned(), reply.clone());
        }
    }
}

/// `POST v1/agents/<bot>/invoke`.
pub(crate) fn invoke(state: &mut State, call: &Call<'_>, bot: &str) -> ResponseTemplate {
    let Some(body) = call.object() else {
        return status(400);
    };
    let text = |key: &str| body.get(key).and_then(Value::as_str);
    let (Some(name), Some(_), Some(_), true) = (
        text("name"),
        text("appId"),
        text("imdisplayname"),
        body.contains_key("value"),
    ) else {
        return status(400);
    };
    let token = body.get("userAadToken");
    let token_kept = if state.cloud.is_microsoft() {
        token.and_then(Value::as_str).is_some_and(|token| !token.is_empty())
    } else {
        token.is_none()
    };
    if !token_kept || !bot.starts_with("28:") {
        return status(400);
    }
    let conversation = body
        .get("conversation")
        .and_then(|conversation| conversation.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(conversation) = &conversation {
        let stored = conversation
            .split_once(";messageid=")
            .map_or(conversation.as_str(), |(channel, _)| channel);
        if let Err(code) = member_of(state, stored, &call.caller) {
            return status(code);
        }
    }
    state.bots.invokes.push(BotInvokeRecord {
        caller: call.caller.clone(),
        bot: bot.to_owned(),
        body: Value::Object(body.clone()),
    });
    let Some(script) = state.bots.scripts.get(bot) else {
        return status(404);
    };
    let Some(reply) = script.replies.get(name).cloned() else {
        return status(404);
    };
    let speaker = script.name.clone();
    if let (Some((message_type, content)), Some(conversation)) = (&reply.post, &conversation) {
        let _ = state.add_message(conversation, (bot, &speaker), message_type, content, |_| {});
    }
    match reply.body {
        Some(answer) => json(reply.status, &answer),
        None if (200..300).contains(&reply.status) => {
            ResponseTemplate::new(reply.status).set_body_raw(Vec::new(), "text/plain; charset=utf-8")
        }
        None => status(reply.status),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixture_answer_is_there_and_replies_build() {
        for name in [
            "commands",
            "noCommands",
            "dialog",
            "results",
            "message",
            "taskMessage",
            "card",
            "error",
            "selected",
            "webDialog",
            "submitDialog",
        ] {
            assert!(bot_answer(name).is_some(), "{name}");
        }
        assert!(bot_answer("nothing").is_none());
        let apps = extension_apps();
        assert_eq!(apps.len(), 4);
        assert_eq!(apps[3].1, "Featured");
        assert!(extension_apps_answer()["value"]["definitions"].is_object());
        let reply = BotReply::status(502).posting("Text", "x");
        assert_eq!(reply.status, 502);
        assert_eq!(reply.post, Some(("Text".to_owned(), "x".to_owned())));
        let mut bots = Bots::default();
        bots.script("28:b", "Bot", &[("task/fetch", BotReply::done())]);
        bots.script("28:b", "Bot 2", &[("task/submit", BotReply::done())]);
        assert_eq!(bots.scripts["28:b"].replies.len(), 2);
        assert_eq!(bots.scripts["28:b"].name, "Bot 2");
    }
}
