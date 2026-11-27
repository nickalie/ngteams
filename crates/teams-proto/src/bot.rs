//! Bot invokes: what a client asks an app's bot through its own chat service, `POST v1/agents/28:<bot>/invoke`, and
//! the answers it reads back (`docs/protocol/bots.md`). The client never reaches a bot itself: the chat service does.
//!
//! The body is Teams' own: `name`, `appId`, `value`, `imdisplayname`, `conversation` (`{"id": …}`, when the invoke
//! acts in one) and, for a Microsoft account only, `userAadToken`: the account's token for the Teams service
//! (`https://api.spaces.skype.com`), which goes to Microsoft's chat service as Teams sends it there. An own-server
//! account's body carries no token at all: its bearer already names the caller. The transport adds the token when it
//! sends ([`crate::Transport::deliver_bot_invoke`]), so no value here ever holds one.
//!
//! The `composeExtension/fetchCommands`, `fetchTask` and `submitAction` shapes and their answers were seen in the lab
//! (the Workflows and Approvals apps); the others follow the public Teams and Bot Framework documentation and are
//! marked unseen in `bots.md` until a capture shows them.
use serde_json::{Map, Value, json};
use teams_auth::secrets::Secret;

use crate::{Endpoint, Mri};

/// An invoke of an app's bot, built from typed values (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct BotInvoke {
    bot: Mri,
    app_id: String,
    display_name: String,
    conversation: Option<String>,
    theme: Theme,
    kind: InvokeKind,
}

/// What an invoke asks.
#[derive(Clone, Debug, PartialEq)]
pub enum InvokeKind {
    /// `composeExtension/fetchCommands`: the app's message extension commands for a context (seen).
    FetchCommands { context: CommandContext },
    /// `composeExtension/fetchTask`: an action command's dialog, on a message when run from one (seen).
    FetchTask {
        command: String,
        context: CommandContext,
        message: Option<MessagePayload>,
    },
    /// `composeExtension/submitAction`: an action command's dialog submitted, its card's data and answers (seen).
    SubmitAction {
        command: String,
        context: CommandContext,
        data: Map<String, Value>,
        message: Option<MessagePayload>,
    },
    /// `composeExtension/query`: a search command's query, its parameters by name (unseen).
    Query {
        command: String,
        parameters: Vec<(String, String)>,
        skip: u32,
        count: u32,
    },
    /// `composeExtension/selectItem`: a search result chosen, its `preview.tap` value (unseen).
    SelectItem { value: Value },
    /// `adaptiveCard/action`: a card's `Action.Execute`, pressed or its `refresh` (unseen).
    Execute(CardInvoke),
    /// `task/fetch`: a card button's dialog (`msteams.type` `task/fetch`) (unseen).
    TaskFetch(CardInvoke),
    /// `task/submit`: a card dialog's submit (unseen).
    TaskSubmit(CardInvoke),
    /// A card's plain `Action.Submit` or `messageBack`: the bot gets a message carrying `value`, and a `messageBack`'s
    /// `text` as its `text` (own design, unseen).
    Submit(CardInvoke),
    /// `signin/verifyState`: the code the bot's sign-in page showed once the person signed in, which the bot redeems
    /// for its own token at its token service (unseen).
    VerifyState { code: String },
}

/// A card action's invoke: the card's message, the action's id and verb, and its data merged with the card's answers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardInvoke {
    /// The card's message id.
    pub message: Option<String>,
    /// The action's `id`.
    pub id: Option<String>,
    /// An `Action.Execute`'s `verb`.
    pub verb: Option<String>,
    pub data: Value,
    /// An `Action.Execute` sent by a `refresh` rather than pressed.
    pub automatic: bool,
    /// A `messageBack`'s `text`: what the bot reads as the message's text ([`InvokeKind::Submit`] only).
    pub text: Option<String>,
}

/// Where a message extension's command runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommandContext {
    /// On a message ("More actions" on it).
    #[default]
    Message,
    /// From the compose box (seen).
    Compose,
    /// From the command box.
    CommandBox,
}

/// The client's theme, as an invoke tells the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Default,
    Dark,
    Contrast,
}

/// The message an action command runs on, as Teams describes it to the app (`messagePayload`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePayload {
    pub id: String,
    /// The post a channel message belongs to; none in a chat.
    pub reply_to: Option<String>,
    /// When it was sent (ISO 8601, ms, `Z`).
    pub created: String,
    pub from: PayloadSender,
    /// Its HTML, as the service holds it.
    pub html: String,
    /// Teams' link to it ([`crate::MessageRun::link_to`]).
    pub link: String,
}

/// Who sent a [`MessagePayload`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadSender {
    /// A person: their directory object id and name.
    User { id: String, name: String },
    /// An app's bot: its id and name.
    App { id: String, name: String },
}

impl InvokeKind {
    /// The invoke's `name`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::FetchCommands { .. } => "composeExtension/fetchCommands",
            Self::FetchTask { .. } => "composeExtension/fetchTask",
            Self::SubmitAction { .. } => "composeExtension/submitAction",
            Self::Query { .. } => "composeExtension/query",
            Self::SelectItem { .. } => "composeExtension/selectItem",
            Self::Execute(_) => "adaptiveCard/action",
            Self::TaskFetch(_) => "task/fetch",
            Self::TaskSubmit(_) => "task/submit",
            Self::Submit(_) => "message",
            Self::VerifyState { .. } => "signin/verifyState",
        }
    }

    /// The card message the invoke acts on, when it acts on one.
    fn card_message(&self) -> Option<&str> {
        match self {
            Self::Execute(card) | Self::TaskFetch(card) | Self::TaskSubmit(card) | Self::Submit(card) => {
                card.message.as_deref()
            }
            _ => None,
        }
    }

    fn value(&self, theme: Theme) -> Value {
        let context = json!({"theme": theme.wire()});
        match self {
            Self::FetchCommands { context: on } => json!({"commandContext": on.fetch_commands_wire()}),
            Self::FetchTask {
                command,
                context: on,
                message,
            } => {
                let mut value = json!({"commandId": command, "commandContext": on.wire(), "context": context});
                put_payload(&mut value, message.as_ref());
                value
            }
            Self::SubmitAction {
                command,
                context: on,
                data,
                message,
            } => {
                let mut value = json!({
                    "commandId": command,
                    "commandContext": on.wire(),
                    "data": data,
                    "context": context,
                });
                put_payload(&mut value, message.as_ref());
                value
            }
            Self::Query {
                command,
                parameters,
                skip,
                count,
            } => json!({
                "commandId": command,
                "commandContext": CommandContext::Compose.wire(),
                "parameters": parameters
                    .iter()
                    .map(|(name, value)| json!({"name": name, "value": value}))
                    .collect::<Vec<_>>(),
                "queryOptions": {"skip": skip, "count": count},
                "context": context,
            }),
            Self::SelectItem { value } => value.clone(),
            Self::Execute(card) => json!({
                "action": {"type": "Action.Execute", "id": card.id, "verb": card.verb, "data": card.data},
                "trigger": if card.automatic { "automatic" } else { "manual" },
            }),
            Self::TaskFetch(card) | Self::TaskSubmit(card) => json!({"data": card.data, "context": context}),
            Self::Submit(card) => card.data.clone(),
            Self::VerifyState { code } => json!({ "state": code }),
        }
    }
}

fn put_payload(value: &mut Value, message: Option<&MessagePayload>) {
    if let (Some(message), Some(object)) = (message, value.as_object_mut()) {
        object.insert("messagePayload".to_owned(), message.wire());
    }
}

impl CommandContext {
    /// As a dialog's or a query's invoke names it.
    const fn wire(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::Compose => "compose",
            Self::CommandBox => "commandbox",
        }
    }

    /// As `fetchCommands` names it: capitalised (`Message` seen; the others by analogy, unseen).
    const fn fetch_commands_wire(self) -> &'static str {
        match self {
            Self::Message => "Message",
            Self::Compose => "Compose",
            Self::CommandBox => "CommandBox",
        }
    }
}

impl Theme {
    const fn wire(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Dark => "dark",
            Self::Contrast => "contrast",
        }
    }
}

impl MessagePayload {
    fn wire(&self) -> Value {
        let from = match &self.from {
            PayloadSender::User { id, name } => {
                json!({"user": {"userIdentityType": "aadUser", "id": id, "displayName": name}})
            }
            PayloadSender::App { id, name } => {
                json!({"application": {"applicationIdentityType": "bot", "id": id, "displayName": name}})
            }
        };
        let mut payload = json!({
            "id": self.id,
            "createdDateTime": self.created,
            "deleted": false,
            "importance": "normal",
            "locale": "en-us",
            "from": from,
            "body": {"contentType": "html", "content": self.html},
            "attachments": [],
            "mentions": [],
            "reactions": [],
            "linkToMessage": self.link,
        });
        if let (Some(post), Some(object)) = (&self.reply_to, payload.as_object_mut()) {
            object.insert("replyToId".to_owned(), json!(post));
        }
        payload
    }
}

impl BotInvoke {
    /// An invoke of the bot `bot` (`28:<bot id>`) of the app `app_id`, asked by the account named `display_name`;
    /// `None` unless `bot` is a bot's MRI and `app_id` is given.
    #[must_use]
    pub fn new(bot: Mri, app_id: impl Into<String>, display_name: impl Into<String>, kind: InvokeKind) -> Option<Self> {
        let app_id = app_id.into();
        let id = bot.as_str().strip_prefix("28:")?;
        if id.is_empty() || id.contains(['/', '?', '#']) || app_id.trim().is_empty() {
            return None;
        }
        Some(Self {
            bot,
            app_id,
            display_name: display_name.into(),
            conversation: None,
            theme: Theme::Default,
            kind,
        })
    }

    /// The invoke acts in `conversation` (a chat, or a channel post's `<channel>;messageid=<post>` chain).
    #[must_use]
    pub fn in_conversation(mut self, conversation: impl Into<String>) -> Self {
        self.conversation = Some(conversation.into());
        self
    }

    /// The client shows `theme`.
    #[must_use]
    pub const fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        self
    }

    #[must_use]
    pub const fn bot(&self) -> &Mri {
        &self.bot
    }

    #[must_use]
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    #[must_use]
    pub fn conversation(&self) -> Option<&str> {
        self.conversation.as_deref()
    }

    #[must_use]
    pub const fn kind(&self) -> &InvokeKind {
        &self.kind
    }

    /// `v1/agents/28:<bot>/invoke` on the chat service.
    #[must_use]
    pub fn endpoint(&self) -> Endpoint {
        Endpoint::new(["v1", "agents", self.bot.as_str(), "invoke"])
    }

    /// The body, with `spaces` as its `userAadToken` when given (a Microsoft account's Teams service token) and none
    /// otherwise.
    #[must_use]
    pub(crate) fn body(&self, spaces: Option<&Secret>) -> Value {
        let mut body = Map::new();
        body.insert("name".to_owned(), json!(self.kind.name()));
        body.insert("appId".to_owned(), json!(self.app_id));
        body.insert("value".to_owned(), self.kind.value(self.theme));
        body.insert("imdisplayname".to_owned(), json!(self.display_name));
        if let Some(token) = spaces {
            body.insert("userAadToken".to_owned(), json!(token.expose()));
        }
        if let Some(conversation) = &self.conversation {
            body.insert("conversation".to_owned(), json!({"id": conversation}));
        }
        if let Some(message) = self.kind.card_message() {
            body.insert("messageId".to_owned(), json!(message));
        }
        if let InvokeKind::Submit(CardInvoke { text: Some(text), .. }) = &self.kind {
            body.insert("text".to_owned(), json!(text));
        }
        Value::Object(body)
    }

    /// The body as an own-server account's invoke sends it, which is also what a log may show of any invoke.
    #[must_use]
    pub fn body_without_token(&self) -> Value {
        self.body(None)
    }
}

/// What a bot's answer to an invoke says, as the chat service hands it back.
#[derive(Clone, Debug, PartialEq)]
pub enum InvokeAnswer {
    /// A success with nothing in it (seen: `submitAction`, and `fetchCommands` of an app with none).
    Done,
    /// A message extension's commands (seen).
    Commands(Vec<ComposeCommand>),
    /// A search command's results: each an attachment (a card and its `preview`), as a list or a grid (unseen).
    Results { grid: bool, attachments: Vec<Value> },
    /// A line of text to show (`composeExtension` or `task` `message`, an `Action.Execute`'s message) (unseen).
    Message(String),
    /// A dialog to open (seen with a card).
    Dialog(TaskDialog),
    /// An `Action.Execute`'s card, which takes the pressed card's place (unseen).
    Card(Value),
    /// The bot's error (`application/vnd.microsoft.error`) (unseen).
    Error { code: Option<String>, message: String },
    /// The bot asks the user to sign in (a `loginRequest`, a `composeExtension` `auth`) (unseen).
    SignIn(Value),
    /// The service answered with an error status.
    Refused(u16),
    /// A success of a shape not read here, kept as it came.
    Unknown(Value),
}

/// A message extension command, as `fetchCommands` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeCommand {
    pub id: String,
    /// `action` opens a dialog; `query` searches.
    pub action: bool,
    pub title: String,
    /// Its dialog comes from a `fetchTask` (else from the app's definition).
    pub fetch_task: bool,
    /// Its icon's link.
    pub icon: Option<String>,
    /// Its dialog's width, as the app gives it (`"medium"`, `600`).
    pub width: Option<DialogSize>,
}

/// A dialog an app opens (`task` `continue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskDialog {
    pub title: Option<String>,
    /// Its Adaptive Card (an attachment's `content`).
    pub card: Option<Value>,
    /// A web page in its place: the app opens in Teams on the web.
    pub url: Option<String>,
    pub width: Option<DialogSize>,
    pub height: Option<DialogSize>,
}

/// A dialog's width or height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogSize {
    Small,
    Medium,
    Large,
    Pixels(u32),
}

impl DialogSize {
    pub(crate) fn of(value: Option<&Value>) -> Option<Self> {
        match value? {
            Value::Number(number) => number.as_u64().and_then(|px| u32::try_from(px).ok()).map(Self::Pixels),
            Value::String(word) => match word.to_ascii_lowercase().as_str() {
                "small" => Some(Self::Small),
                "medium" => Some(Self::Medium),
                "large" => Some(Self::Large),
                other => other.parse().ok().map(Self::Pixels),
            },
            _ => None,
        }
    }
}

/// The universal action responses' `type`s.
const CARD: &str = "application/vnd.microsoft.card.adaptive";
const MESSAGE: &str = "application/vnd.microsoft.activity.message";
const ERROR: &str = "application/vnd.microsoft.error";
const LOGIN: &str = "application/vnd.microsoft.activity.loginRequest";

impl InvokeAnswer {
    /// The answer of `status` with `body`: the chat service's envelope (`{"responseType": <kind>, <kind>: …}`, seen),
    /// or a bot's own answer as the documentation gives it (a `composeExtension` or `task` object, a universal
    /// action's `{statusCode, type, value}`).
    #[must_use]
    pub fn read(status: u16, body: &[u8]) -> Self {
        if !(200..300).contains(&status) {
            return Self::Refused(status);
        }
        if body.iter().all(u8::is_ascii_whitespace) {
            return Self::Done;
        }
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return Self::Unknown(Value::String(String::from_utf8_lossy(body).into_owned()));
        };
        Self::of(value)
    }

    fn of(value: Value) -> Self {
        let kind = value.get("responseType").and_then(Value::as_str).map(str::to_owned);
        let answer = match kind.as_deref() {
            Some(kind) => value.get(kind).and_then(|inner| Self::of_kind(kind, inner)),
            None => value
                .get("composeExtension")
                .and_then(|inner| Self::of_kind("composeExtension", inner))
                .or_else(|| value.get("task").and_then(|inner| Self::of_kind("task", inner)))
                .or_else(|| universal(&value)),
        };
        answer.unwrap_or(Self::Unknown(value))
    }

    fn of_kind(kind: &str, inner: &Value) -> Option<Self> {
        match kind {
            "composeExtension" => compose(inner),
            "task" => task(inner),
            _ => universal(inner),
        }
    }
}

fn compose(inner: &Value) -> Option<InvokeAnswer> {
    Some(match inner.get("type")?.as_str()? {
        "command" => InvokeAnswer::Commands(inner.get("commands")?.as_array()?.iter().filter_map(command).collect()),
        "result" => InvokeAnswer::Results {
            grid: inner.get("attachmentLayout").and_then(Value::as_str) == Some("grid"),
            attachments: inner
                .get("attachments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        },
        "message" => InvokeAnswer::Message(inner.get("text")?.as_str()?.to_owned()),
        "auth" | "config" => InvokeAnswer::SignIn(inner.get("suggestedActions").cloned().unwrap_or(Value::Null)),
        _ => return None,
    })
}

fn command(item: &Value) -> Option<ComposeCommand> {
    let text = |key: &str| item.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(ComposeCommand {
        id: text("id")?,
        action: item.get("type").and_then(Value::as_str) != Some("query"),
        title: text("title").unwrap_or_default(),
        fetch_task: item.get("fetchTask").and_then(Value::as_bool).unwrap_or(false),
        icon: item
            .get("imageUrls")
            .and_then(Value::as_array)
            .and_then(|urls| urls.first())
            .and_then(Value::as_str)
            .or_else(|| item.get("iconUrl").and_then(Value::as_str))
            .map(str::to_owned),
        width: DialogSize::of(item.pointer("/taskInfo/width")),
    })
}

fn task(inner: &Value) -> Option<InvokeAnswer> {
    let value = inner.get("value");
    Some(match inner.get("type")?.as_str()? {
        "continue" => {
            let value = value?;
            let card = value
                .get("card")
                .and_then(|card| card.get("content").or(Some(card)))
                .filter(|card| card.get("type").and_then(Value::as_str) == Some("AdaptiveCard"))
                .cloned();
            InvokeAnswer::Dialog(TaskDialog {
                title: value.get("title").and_then(Value::as_str).map(str::to_owned),
                card,
                url: value.get("url").and_then(Value::as_str).map(str::to_owned),
                width: DialogSize::of(value.get("width")),
                height: DialogSize::of(value.get("height")),
            })
        }
        "message" => InvokeAnswer::Message(value?.as_str()?.to_owned()),
        _ => return None,
    })
}

fn universal(inner: &Value) -> Option<InvokeAnswer> {
    let value = inner.get("value");
    Some(match inner.get("type")?.as_str()? {
        CARD => InvokeAnswer::Card(value?.clone()),
        MESSAGE => InvokeAnswer::Message(value?.as_str()?.to_owned()),
        ERROR => InvokeAnswer::Error {
            code: value
                .and_then(|error| error.get("code"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            message: value
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        LOGIN => InvokeAnswer::SignIn(value.cloned().unwrap_or(Value::Null)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(kind: InvokeKind) -> BotInvoke {
        BotInvoke::new(
            Mri::new("28:00000000-0000-4000-8000-0000000000b0"),
            "00000000-0000-4000-8000-0000000000a0",
            "Lab A",
            kind,
        )
        .unwrap()
    }

    #[test]
    fn only_a_bot_with_an_app_is_invoked() {
        let kind = || InvokeKind::FetchCommands {
            context: CommandContext::Message,
        };
        assert!(BotInvoke::new(Mri::new("8:orgid:x"), "app", "A", kind()).is_none());
        assert!(BotInvoke::new(Mri::new("28:"), "app", "A", kind()).is_none());
        assert!(BotInvoke::new(Mri::new("28:x/y"), "app", "A", kind()).is_none());
        assert!(BotInvoke::new(Mri::new("28:x"), " ", "A", kind()).is_none());
        let made = invoke(kind()).in_conversation("19:c@thread.v2");
        assert_eq!(made.app_id(), "00000000-0000-4000-8000-0000000000a0");
        assert_eq!(made.conversation(), Some("19:c@thread.v2"));
        assert_eq!(made.bot().as_str(), "28:00000000-0000-4000-8000-0000000000b0");
        assert_eq!(made.kind().name(), "composeExtension/fetchCommands");
    }

    #[test]
    fn the_token_is_in_the_body_only_when_given() {
        let made = invoke(InvokeKind::FetchCommands {
            context: CommandContext::Message,
        });
        let token = Secret::new("synthetic-spaces-token");
        assert_eq!(made.body(Some(&token))["userAadToken"], "synthetic-spaces-token");
        assert!(made.body_without_token().get("userAadToken").is_none());
        assert!(made.body_without_token().get("conversation").is_none());
    }

    #[test]
    fn a_sign_in_code_goes_as_the_verify_states_state_in_the_conversation() {
        let verify = invoke(InvokeKind::VerifyState {
            code: "402917".to_owned(),
        })
        .in_conversation("19:c@unq.gbl.spaces");
        let body = verify.body_without_token();
        assert_eq!(body["name"], "signin/verifyState");
        assert_eq!(body["value"], json!({"state": "402917"}));
        assert_eq!(body["conversation"]["id"], "19:c@unq.gbl.spaces");
        assert!(body.get("messageId").is_none());
    }

    #[test]
    fn documented_kinds_have_their_names_and_values() {
        let card = CardInvoke {
            message: Some("1700000000000".to_owned()),
            id: Some("approve".to_owned()),
            verb: Some("approve".to_owned()),
            data: json!({"x": 1}),
            automatic: true,
            text: None,
        };
        let execute = invoke(InvokeKind::Execute(card.clone())).with_theme(Theme::Dark);
        let body = execute.body_without_token();
        assert_eq!(body["name"], "adaptiveCard/action");
        assert_eq!(body["messageId"], "1700000000000");
        assert_eq!(body["value"]["action"]["verb"], "approve");
        assert_eq!(body["value"]["trigger"], "automatic");
        assert_eq!(
            invoke(InvokeKind::TaskFetch(card.clone()))
                .with_theme(Theme::Contrast)
                .body_without_token()["value"]["context"]["theme"],
            "contrast"
        );
        assert_eq!(
            invoke(InvokeKind::TaskSubmit(card.clone())).kind().name(),
            "task/submit"
        );
        assert_eq!(
            invoke(InvokeKind::Submit(card)).body_without_token()["value"],
            json!({"x": 1})
        );
        let query = invoke(InvokeKind::Query {
            command: "search".to_owned(),
            parameters: vec![("q".to_owned(), "lab".to_owned())],
            skip: 0,
            count: 25,
        });
        let body = query.body_without_token();
        assert_eq!(body["value"]["parameters"][0], json!({"name": "q", "value": "lab"}));
        assert_eq!(body["value"]["commandContext"], "compose");
        assert_eq!(
            invoke(InvokeKind::SelectItem {
                value: json!({"id": 3})
            })
            .body_without_token()["value"],
            json!({"id": 3})
        );
        assert_eq!(
            invoke(InvokeKind::FetchCommands {
                context: CommandContext::CommandBox
            })
            .body_without_token()["value"]["commandContext"],
            "CommandBox"
        );
        assert_eq!(CommandContext::CommandBox.wire(), "commandbox");
        assert_eq!(CommandContext::Compose.fetch_commands_wire(), "Compose");
    }

    #[test]
    fn a_message_backs_text_goes_beside_its_value() {
        let card = CardInvoke {
            data: json!({"x": 1}),
            ..CardInvoke::default()
        };
        let submit = invoke(InvokeKind::Submit(card.clone())).body_without_token();
        assert_eq!(submit["value"], json!({"x": 1}));
        assert!(submit.get("text").is_none());
        let message_back = invoke(InvokeKind::Submit(CardInvoke {
            text: Some("join".to_owned()),
            ..card
        }))
        .body_without_token();
        assert_eq!(message_back["text"], "join");
        assert_eq!(message_back["value"], json!({"x": 1}));
    }

    #[test]
    fn answers_read_in_each_form() {
        assert_eq!(InvokeAnswer::read(200, b""), InvokeAnswer::Done);
        assert_eq!(InvokeAnswer::read(502, b"{}"), InvokeAnswer::Refused(502));
        assert!(matches!(InvokeAnswer::read(200, b"not json"), InvokeAnswer::Unknown(_)));
        assert!(matches!(
            InvokeAnswer::read(200, b"{\"x\":1}"),
            InvokeAnswer::Unknown(_)
        ));
        let bare = br#"{"composeExtension": {"type": "message", "text": "Done."}}"#;
        assert_eq!(InvokeAnswer::read(200, bare), InvokeAnswer::Message("Done.".to_owned()));
        let auth = br#"{"composeExtension": {"type": "auth", "suggestedActions": {"actions": []}}}"#;
        assert!(matches!(InvokeAnswer::read(200, auth), InvokeAnswer::SignIn(_)));
        let task = br#"{"task": {"type": "message", "value": "Thanks"}}"#;
        assert_eq!(
            InvokeAnswer::read(200, task),
            InvokeAnswer::Message("Thanks".to_owned())
        );
    }

    #[test]
    fn dialogs_and_universal_answers_read() {
        let page = br#"{"task": {"type": "continue", "value": {"url": "https://example.invalid/", "title": "T", "width": "large", "height": "320"}}}"#;
        let InvokeAnswer::Dialog(dialog) = InvokeAnswer::read(200, page) else {
            panic!("a dialog");
        };
        assert_eq!(dialog.url.as_deref(), Some("https://example.invalid/"));
        assert_eq!(dialog.width, Some(DialogSize::Large));
        assert_eq!(dialog.height, Some(DialogSize::Pixels(320)));
        assert!(dialog.card.is_none());
        let message = br#"{"statusCode": 200, "type": "application/vnd.microsoft.activity.message", "value": "Saved"}"#;
        assert_eq!(
            InvokeAnswer::read(200, message),
            InvokeAnswer::Message("Saved".to_owned())
        );
        let login = br#"{"statusCode": 401, "type": "application/vnd.microsoft.activity.loginRequest", "value": {}}"#;
        assert!(matches!(InvokeAnswer::read(200, login), InvokeAnswer::SignIn(_)));
        assert_eq!(DialogSize::of(Some(&json!("small"))), Some(DialogSize::Small));
        assert_eq!(DialogSize::of(Some(&json!(true))), None);
    }
}
