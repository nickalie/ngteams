//! A bot's card in a chat, behind `NGTEAMS_BOTS=1` ([`crate::bots_flag`]): what one message's cards hold besides
//! what they say (`docs/protocol/bots.md`), as Teams' web client keeps it per card.
//!
//! - **Answers**: each input's control ([`Field`]), made when the card is first drawn, keyed by the card and the
//!   input's id. A button that sends gathers the answers of its card (and of the cards a `ShowCard` opened it from),
//!   checks them (required, `regex`, `maxLength`, `min`/`max`) and marks those that fail with the card's own
//!   `errorMessage`; nothing goes until all pass.
//! - **A button that sends**: `Action.Submit` (plain, `messageBack`, `invoke`, a hero card's `postBack`) and
//!   `Action.Execute` ask the bot through the engine (`Core::bot_invoke`), its data merged with the answers by input id;
//!   `imBack` and `messageBack`'s `displayText` post a message as the user, through the ordinary write; `messageBack`'s
//!   `text` goes to the bot with its value. While one goes, the buttons and inputs of its card wait; the other cards of
//!   the message don't. `signin` opens its page in the browser, and a field under the card then takes the code that
//!   page shows, which goes to the bot as `signin/verifyState`. `task/fetch` asks the bot for its dialog, which the
//!   view opens ([`CardBotEvent::Dialog`], [`crate::app_dialog`]).
//! - **A card in an app's dialog** ([`CardBot::in_dialog`]): its plain submit goes to the dialog's command
//!   (`composeExtension/submitAction`) or its button (`task/submit`), and the answer goes to the dialog
//!   ([`CardBotEvent::Answered`]), never under the card.
//! - **The answer**: a card takes the pressed card's place; a message, an error or a sign-in request shows as a line
//!   under the card; a bot that wasn't reached is said there too, and nothing is sent again.
//! - **Refresh**: a card's `refresh` runs by itself, once for each version of its cards while the view lives, for a
//!   user it names ([`CardBot::refreshes_for`]); anyone may run it by hand ("Refresh card"). It is the `refresh` of
//!   the card shown: the bot's card when it put one in the place.
//! - **An edit**: when the message's cards change, its state starts again ([`CardBot::renewed`]): answers, the bot's
//!   cards and lines go, an answer to an ask of the old cards is not shown, and a card that names the user refreshes
//!   by itself again.
//! - **Suggested actions** ask in a slot of their own ([`SUGGESTED`]): they wait for their ask alone, and their line
//!   shows under them.
//!
//! The view asks the engine; it builds no request ([`crate::bridge::Core`]).
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::NaiveDate;
use gpui::{AppContext as _, Context, Entity, EventEmitter, Subscription, Window};
use gpui_component::date_picker::{DatePickerEvent, DatePickerState};
use gpui_component::input::{InputEvent, InputState, TextareaState};
use serde_json::{Map, Value};
use teams_core::Action;
use teams_core::bots::BotAsk;
use teams_proto::bot::MessagePayload;
use teams_proto::chat::MessageText;
use teams_proto::chat::card::{ActionKind, BotAction, Card, Element, Input, InputKind, Kind, Refresh, Via};
use teams_proto::{CardInvoke, CommandContext, InvokeAnswer, InvokeKind, Mri, TaskDialog};

use crate::bridge::Core;

/// The line under a card when its bot couldn't be reached.
pub const NOT_REACHED: &str = "The app didn't respond. Try again.";
/// The line under a card whose bot asks the user to sign in.
pub const SIGN_IN_NEEDED: &str = "Sign-in needed. Open this message in Teams on the web to sign in to the app.";
/// The line under a card whose bot answered with a dialog this build doesn't open yet.
pub const DIALOG_IN_TEAMS: &str = "The app continues in a dialog. Open this message in Teams on the web to go on.";
/// What a required input left empty says, when its card gives no words of its own.
pub const REQUIRED: &str = "This is required.";
/// The id of a sign-in card's code field among its inputs' controls: no card input has it.
pub const SIGN_IN_CODE: &str = "#sign-in-code";
/// What a sign-in card's code field asks for.
pub const CODE_LABEL: &str = "Enter the code from the sign-in page";
/// The code field's placeholder.
pub const CODE_PLACEHOLDER: &str = "Code";
/// What a code that isn't six digits says.
pub const CODE_NEEDED: &str = "Enter the six-digit code.";
/// The line under a sign-in card once the bot took the code.
pub const SIGNED_IN: &str = "You're signed in.";
/// The line under a sign-in card whose bot didn't take the code.
pub const CODE_FAILED: &str = "That code didn't work. Check it, or select Sign in again.";
/// What an input whose answer doesn't fit says, when its card gives no words of its own.
pub const NOT_VALID: &str = "Enter a valid value.";
/// What a button that opens a dialog says in its tooltip.
pub const DIALOG_TIP: &str = "Opens in Teams on the web, where this app's dialog runs";
/// The place of a message's suggested actions among its cards: after all of them, so that their ask and its answer
/// do not touch a card.
pub const SUGGESTED: usize = usize::MAX;

/// The bot a card's buttons reach, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The bot, `28:<bot id>`.
    pub bot: Mri,
    /// Its app's id.
    pub app_id: String,
    /// The chat, or a channel post's `<channel>;messageid=<post>` chain: where the card is, and where a message the
    /// user posts from it goes.
    pub conversation: String,
}

impl Target {
    /// The bot that sent a message, when one did (`28:`), in `conversation`. Its app's id is the bot's own until the
    /// engine knows each bot's app.
    #[must_use]
    pub fn of_sender(sender: Option<&str>, conversation: &str) -> Option<Self> {
        let bot = Mri::new(sender?);
        let id = bot.as_str().strip_prefix("28:")?.to_owned();
        (!id.is_empty()).then(|| Self {
            bot,
            app_id: id,
            conversation: conversation.to_owned(),
        })
    }
}

/// What shows under a card after its bot answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// The bot's message.
    Note(String),
    /// The bot's error, or the bot not reached.
    Error(String),
    /// The bot asks the user to sign in.
    SignIn,
    /// The bot answered with a dialog: it goes on in Teams on the web.
    Dialog,
}

/// Where a card's plain submit goes (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SubmitTo {
    /// The bot, as a message carrying the answers: a card in a chat.
    #[default]
    Message,
    /// A message extension's dialog: `composeExtension/submitAction` of `command`, run in `context`, on `message`
    /// when run from one.
    Command {
        command: String,
        context: CommandContext,
        message: Option<Box<MessagePayload>>,
    },
    /// A card button's dialog: `task/submit`.
    Task,
}

/// What a card's bot state tells its view.
#[derive(Clone, Debug)]
pub enum CardBotEvent {
    /// A `task/fetch` button's bot answered with a dialog, to open over the window.
    Dialog { target: Target, dialog: TaskDialog },
    /// A dialog's submit was answered (a card [`CardBot::in_dialog`]); `Err` says why the bot wasn't reached.
    Answered(Result<InvokeAnswer, String>),
}

/// An input's control.
#[derive(Clone)]
pub enum Field {
    /// A text, a number or a time, typed.
    Text(Entity<InputState>),
    Multiline(Entity<TextareaState>),
    Date(Entity<DatePickerState>),
    Toggle(bool),
    /// The values chosen.
    Choice(Vec<String>),
}

/// One message's cards' answers and what their bot did (see the module docs).
pub struct CardBot {
    core: Arc<dyn Core>,
    message: String,
    target: Option<Target>,
    fields: HashMap<String, Field>,
    watched: Vec<Subscription>,
    invalid: HashSet<String>,
    replaced: HashMap<usize, Arc<Card>>,
    lines: HashMap<usize, Line>,
    /// Per card, the key of the button whose ask is on its way.
    busy: HashMap<usize, String>,
    refreshed: HashSet<usize>,
    /// The cards whose sign-in page was opened, which wait for its code.
    signing_in: HashSet<usize>,
    submit_to: SubmitTo,
}

impl EventEmitter<CardBotEvent> for CardBot {}

/// What a card is drawn with from its [`CardBot`], read once a frame.
#[derive(Clone, Default)]
pub struct Shown {
    pub fields: HashMap<String, Field>,
    pub invalid: HashSet<String>,
    pub lines: HashMap<usize, Line>,
    /// Per card, the key of the button whose ask is on its way.
    pub busy: HashMap<usize, String>,
    /// The bot is known: a button that sends asks it.
    pub reaches: bool,
    /// The cards that wait for a sign-in's code.
    pub signing_in: HashSet<usize>,
}

impl CardBot {
    /// The cards of `message`, whose buttons reach `target` (none: they open in Teams on the web).
    #[must_use]
    pub fn new(core: Arc<dyn Core>, message: String, target: Option<Target>) -> Self {
        Self {
            core,
            message,
            target,
            fields: HashMap::new(),
            watched: Vec::new(),
            invalid: HashSet::new(),
            replaced: HashMap::new(),
            lines: HashMap::new(),
            busy: HashMap::new(),
            refreshed: HashSet::new(),
            signing_in: HashSet::new(),
            submit_to: SubmitTo::Message,
        }
    }

    /// The same, for a card in an app's dialog: its plain submit goes as `submit_to` says, and the answer to the
    /// dialog.
    #[must_use]
    pub fn in_dialog(mut self, submit_to: SubmitTo) -> Self {
        self.submit_to = submit_to;
        self
    }

    /// The same message's state started again, for its changed cards: nothing of the old cards stays, so a card that
    /// names the user refreshes by itself again.
    #[must_use]
    pub fn renewed(&self) -> Self {
        Self::new(Arc::clone(&self.core), self.message.clone(), self.target.clone()).in_dialog(self.submit_to.clone())
    }

    #[must_use]
    pub fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    /// The card the bot put in the place of card `at`.
    #[must_use]
    pub fn replaced(&self, at: usize) -> Option<Arc<Card>> {
        self.replaced.get(&at).cloned()
    }

    /// What its cards are drawn with.
    #[must_use]
    pub fn shown(&self) -> Shown {
        Shown {
            fields: self.fields.clone(),
            invalid: self.invalid.clone(),
            lines: self.lines.clone(),
            busy: self.busy.clone(),
            reaches: self.target.is_some(),
            signing_in: self.signing_in.clone(),
        }
    }

    /// The controls of card `at`'s inputs, made for those that have none yet.
    pub fn make_fields(&mut self, at: usize, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        let mut inputs = Vec::new();
        card_inputs(card, true, &mut inputs);
        for input in inputs {
            let key = field_key(at, &input.id);
            if !self.fields.contains_key(&key) {
                let field = self.field(&key, &input, window, cx);
                self.fields.insert(key, field);
            }
        }
        let code = field_key(at, SIGN_IN_CODE);
        if self.signing_in.contains(&at) && !self.fields.contains_key(&code) {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(CODE_PLACEHOLDER));
            self.watched.push(
                cx.subscribe(&state, move |this, _, event: &InputEvent, cx| match event {
                    InputEvent::PressEnter { .. } => this.verify(at, cx),
                    InputEvent::Change if this.invalid.remove(&field_key(at, SIGN_IN_CODE)) => cx.notify(),
                    _ => {}
                }),
            );
            self.fields.insert(code, Field::Text(state));
        }
    }

    /// The code typed under sign-in card `at`, sent to the bot (`signin/verifyState`); a code that isn't six digits
    /// is marked and goes nowhere.
    pub fn verify(&mut self, at: usize, cx: &mut Context<Self>) {
        let key = field_key(at, SIGN_IN_CODE);
        if self.busy.contains_key(&at) {
            return;
        }
        let typed = match self.fields.get(&key) {
            Some(Field::Text(state)) => state.read(cx).value().to_string(),
            _ => String::new(),
        };
        let code: String = typed.chars().filter(|c| !c.is_whitespace()).collect();
        if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
            self.invalid.insert(key);
            cx.notify();
            return;
        }
        self.invalid.remove(&key);
        self.ask(at, &format!("verify-{at}"), InvokeKind::VerifyState { code }, false, cx);
    }

    /// The bot's answer to a sign-in code of card `at`: taken, the field goes and the card says so; refused, it stays.
    fn verified(&mut self, at: usize, answer: Result<InvokeAnswer, teams_core::EngineError>) {
        let line = match answer {
            Ok(InvokeAnswer::Refused(400 | 401 | 403 | 404 | 412)) => Line::Error(CODE_FAILED.to_owned()),
            Ok(InvokeAnswer::Error { message, .. }) => Line::Error(if message.trim().is_empty() {
                CODE_FAILED.to_owned()
            } else {
                message
            }),
            Ok(InvokeAnswer::Refused(_)) | Err(_) => Line::Error(NOT_REACHED.to_owned()),
            Ok(_) => {
                self.signing_in.remove(&at);
                self.fields.remove(&field_key(at, SIGN_IN_CODE));
                Line::Note(SIGNED_IN.to_owned())
            }
        };
        self.lines.insert(at, line);
    }

    fn field(&mut self, key: &str, input: &Input, window: &mut Window, cx: &mut Context<Self>) -> Field {
        let start = input.initial_answer().unwrap_or_default();
        match &input.kind {
            InputKind::Text {
                multiline: true,
                placeholder,
                ..
            } => {
                let placeholder = placeholder.clone().unwrap_or_default();
                let state = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(start)
                });
                self.watch_text(key, &state, cx);
                Field::Multiline(state)
            }
            InputKind::Text { placeholder, .. }
            | InputKind::Number { placeholder, .. }
            | InputKind::Time { placeholder, .. } => {
                let masked = matches!(
                    input.kind,
                    InputKind::Text {
                        style: teams_proto::chat::card::TextInputStyle::Password,
                        ..
                    }
                );
                let placeholder = placeholder.clone().unwrap_or_else(|| {
                    if matches!(input.kind, InputKind::Time { .. }) {
                        "Select a time".to_owned()
                    } else {
                        String::new()
                    }
                });
                let state = cx.new(|cx| {
                    InputState::new(window, cx)
                        .placeholder(placeholder)
                        .default_value(start)
                        .masked(masked)
                });
                self.watch_text(key, &state, cx);
                Field::Text(state)
            }
            InputKind::Date { value, .. } => {
                let state = cx.new(|cx| DatePickerState::new(window, cx).date_format("%-m/%-d/%Y"));
                if let Some(day) = value.as_deref().and_then(parse_date).and_then(crate::kit_date::to_kit) {
                    state.update(cx, |state, cx| state.set_date(day, window, cx));
                }
                let key = key.to_owned();
                self.watched
                    .push(cx.subscribe(&state, move |this, _, _: &DatePickerEvent, cx| {
                        this.invalid.remove(&key);
                        cx.notify();
                    }));
                Field::Date(state)
            }
            InputKind::Toggle { on, .. } => Field::Toggle(*on),
            InputKind::ChoiceSet { chosen, .. } => Field::Choice(chosen.clone()),
        }
    }

    fn watch_text<T: gpui::EventEmitter<InputEvent>>(&mut self, key: &str, state: &Entity<T>, cx: &mut Context<Self>) {
        let key = key.to_owned();
        self.watched
            .push(cx.subscribe(state, move |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) && this.invalid.remove(&key) {
                    cx.notify();
                }
            }));
    }

    /// A switch flipped.
    pub fn flip(&mut self, key: &str, on: bool, cx: &mut Context<Self>) {
        self.fields.insert(key.to_owned(), Field::Toggle(on));
        self.invalid.remove(key);
        cx.notify();
    }

    /// A time chosen from the list of a time field.
    pub fn picked(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.invalid.remove(key) {
            cx.notify();
        }
    }

    /// A choice chosen (in a set of many, chosen or taken back).
    pub fn choose(&mut self, key: &str, value: &str, multi: bool, cx: &mut Context<Self>) {
        let mut chosen = match self.fields.get(key) {
            Some(Field::Choice(chosen)) => chosen.clone(),
            _ => Vec::new(),
        };
        if !multi {
            chosen = vec![value.to_owned()];
        } else if chosen.iter().any(|known| known == value) {
            chosen.retain(|known| known != value);
        } else {
            chosen.push(value.to_owned());
        }
        self.fields.insert(key.to_owned(), Field::Choice(chosen));
        self.invalid.remove(key);
        cx.notify();
    }

    /// The answers of `inputs` (card `at`'s), by input id, each a string as the card sends it; `None` (each failing one
    /// marked) when one doesn't pass its checks. The marks of the other cards stay.
    pub fn answers(&mut self, at: usize, inputs: &[Input], cx: &Context<Self>) -> Option<Map<String, Value>> {
        let mut answers = Map::new();
        let mut failed = HashSet::new();
        for input in inputs {
            let key = field_key(at, &input.id);
            let answer = match (&input.kind, self.answer_of(&key, input, cx)) {
                // a time typed that can't be read as one fails, whether it is required or not
                (InputKind::Time { .. }, Some(typed)) => {
                    let Some(time) = crate::time_picker::parse_time(&typed) else {
                        failed.insert(key);
                        continue;
                    };
                    Some(time.format("%H:%M").to_string())
                }
                (_, answer) => answer,
            };
            if !passes(input, answer.as_deref()) {
                failed.insert(key);
                continue;
            }
            if let Some(answer) = answer {
                answers.insert(input.id.clone(), Value::String(answer));
            }
        }
        let prefix = format!("{at}/");
        self.invalid.retain(|key| !key.starts_with(&prefix));
        let passed = failed.is_empty();
        self.invalid.extend(failed);
        passed.then_some(answers)
    }

    /// An input's answer as its control holds it (as the card starts it, before its control is made); `None` when
    /// empty. A time is as typed.
    fn answer_of(&self, key: &str, input: &Input, cx: &gpui::App) -> Option<String> {
        let answer = match self.fields.get(key) {
            Some(Field::Text(state)) => state.read(cx).value().trim().to_owned(),
            Some(Field::Multiline(state)) => state.read(cx).value().trim().to_owned(),
            Some(Field::Date(state)) => state
                .read(cx)
                .date()
                .start()
                .and_then(crate::kit_date::from_kit)
                .map(|day| day.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            Some(Field::Toggle(on)) => toggle_answer(input, *on),
            Some(Field::Choice(chosen)) => chosen.join(","),
            None => input.initial_answer().unwrap_or_default(),
        };
        (!answer.is_empty()).then_some(answer)
    }

    /// `action`, a button of card `at` (the button `key`) pressed, its answers those of `inputs` (see the module docs).
    pub fn press(&mut self, at: usize, key: &str, action: &BotAction, inputs: &[Input], cx: &mut Context<Self>) {
        let Some(target) = self.target.clone() else {
            return;
        };
        if self.busy.contains_key(&at) {
            return;
        }
        let answers = if action.inputs {
            let Some(answers) = self.answers(at, inputs, cx) else {
                cx.notify();
                return;
            };
            answers
        } else {
            Map::new()
        };
        let card = |data: Option<&Value>, verb: Option<String>| CardInvoke {
            message: (!self.message.is_empty()).then(|| self.message.clone()),
            id: action.id.clone(),
            verb,
            data: merged(data, &answers),
            ..CardInvoke::default()
        };
        let kind = match &action.via {
            Via::Execute { verb } => InvokeKind::Execute(card(action.data.as_ref(), verb.clone())),
            Via::TaskFetch => InvokeKind::TaskFetch(card(action.data.as_ref(), None)),
            Via::Submit | Via::Invoke(_) | Via::PostBack(_) | Via::MessageBack { .. }
                if self.submit_to != SubmitTo::Message =>
            {
                self.dialog_submit(card(action.data.as_ref(), None))
            }
            Via::ImBack(text) => {
                self.post(&target, text, cx);
                return;
            }
            Via::MessageBack { text, display_text, .. } => {
                if let Some(shown) = display_text.as_deref().filter(|shown| !shown.trim().is_empty()) {
                    self.post(&target, shown, cx);
                }
                InvokeKind::Submit(CardInvoke {
                    text: text.clone().filter(|text| !text.is_empty()),
                    ..card(action.data.as_ref(), None)
                })
            }
            Via::Submit | Via::Invoke(_) | Via::PostBack(_) => InvokeKind::Submit(card(action.data.as_ref(), None)),
            Via::SignIn(page) => {
                if let Some(page) = page.as_deref().filter(|page| teams_proto::chat::card::is_link(page)) {
                    cx.open_url(page);
                    self.signing_in.insert(at);
                    self.lines.remove(&at);
                    cx.notify();
                }
                return;
            }
        };
        self.ask(at, key, kind, false, cx);
    }

    /// A dialog's submit of `card`, as [`SubmitTo`] says.
    fn dialog_submit(&self, card: CardInvoke) -> InvokeKind {
        match &self.submit_to {
            SubmitTo::Command {
                command,
                context,
                message,
            } => InvokeKind::SubmitAction {
                command: command.clone(),
                context: *context,
                data: match card.data {
                    Value::Object(data) => data,
                    other => Map::from_iter([("data".to_owned(), other)]),
                },
                message: message.as_deref().cloned(),
            },
            SubmitTo::Task | SubmitTo::Message => InvokeKind::TaskSubmit(card),
        }
    }

    /// Card `at`'s `refresh` run: by itself (`automatic`, once for each version of its cards while the view lives) or
    /// by hand.
    pub fn refresh(&mut self, at: usize, refresh: &Refresh, automatic: bool, cx: &mut Context<Self>) {
        if self.target.is_none() || self.busy.contains_key(&at) || (automatic && !self.refreshed.insert(at)) {
            return;
        }
        let kind = InvokeKind::Execute(CardInvoke {
            message: Some(self.message.clone()),
            id: refresh.action.id.clone(),
            verb: match &refresh.action.via {
                Via::Execute { verb } => verb.clone(),
                _ => None,
            },
            data: refresh.action.data.clone().unwrap_or_else(|| Value::Object(Map::new())),
            automatic,
            ..CardInvoke::default()
        });
        self.ask(at, &format!("refresh-{at}"), kind, automatic, cx);
    }

    /// Whether a card's `refresh` runs by itself for the account `me`: the card names it, by its MRI or its object id.
    #[must_use]
    pub fn refreshes_for(refresh: &Refresh, me: &str) -> bool {
        let object = teams_proto::object_id(me);
        refresh
            .user_ids
            .iter()
            .any(|user| user == me || Some(user.as_str()) == object)
    }

    fn ask(&mut self, at: usize, key: &str, kind: InvokeKind, quiet: bool, cx: &mut Context<Self>) {
        let Some(target) = &self.target else {
            return;
        };
        let fetching = matches!(kind, InvokeKind::TaskFetch(_));
        let verifying = matches!(kind, InvokeKind::VerifyState { .. });
        let to_dialog = matches!(kind, InvokeKind::SubmitAction { .. } | InvokeKind::TaskSubmit(_));
        let reply = self.core.bot_invoke(BotAsk {
            bot: target.bot.clone(),
            app_id: target.app_id.clone(),
            conversation: Some(target.conversation.clone()),
            kind,
        });
        let target = target.clone();
        self.busy.insert(at, key.to_owned());
        self.lines.remove(&at);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&at);
                match answer {
                    answer if verifying => this.verified(at, answer),
                    Ok(InvokeAnswer::Dialog(dialog)) if fetching => cx.emit(CardBotEvent::Dialog { target, dialog }),
                    answer if to_dialog => cx.emit(CardBotEvent::Answered(answer.map_err(|error| error.to_string()))),
                    answer => this.apply(at, answer, quiet),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The bot's answer to an ask of card `at`, shown; a refresh that ran by itself (`quiet`) says nothing of a failure.
    fn apply(&mut self, at: usize, answer: Result<InvokeAnswer, teams_core::EngineError>, quiet: bool) {
        let line = match answer {
            Ok(InvokeAnswer::Card(card)) => {
                let card = teams_proto::chat::card::adaptive_card_with_inputs(&card);
                let prefix = format!("{at}/");
                self.fields.retain(|key, _| !key.starts_with(&prefix));
                self.invalid.retain(|key| !key.starts_with(&prefix));
                self.replaced.insert(at, Arc::new(card));
                None
            }
            Ok(InvokeAnswer::Message(text)) => Some(Line::Note(text)),
            Ok(InvokeAnswer::Error { message, .. }) if !quiet => Some(Line::Error(if message.trim().is_empty() {
                NOT_REACHED.to_owned()
            } else {
                message
            })),
            Ok(InvokeAnswer::SignIn(_)) => Some(Line::SignIn),
            Ok(InvokeAnswer::Dialog(_)) if !quiet => Some(Line::Dialog),
            Ok(InvokeAnswer::Refused(_)) | Err(_) if !quiet => Some(Line::Error(NOT_REACHED.to_owned())),
            _ => None,
        };
        if let Some(line) = line {
            self.lines.insert(at, line);
        }
    }

    /// `text` posted as the user's message where the card is.
    fn post(&self, target: &Target, text: &str, cx: &mut Context<Self>) {
        post_message(&self.core, &target.conversation, text, cx);
    }
}

/// `text` posted as the user's message in `conversation`, through the outbox (a suggested action, an `imBack`).
pub fn post_message<T>(core: &Arc<dyn Core>, conversation: &str, text: &str, cx: &mut Context<T>) {
    let Ok(text) = MessageText::plain(text) else {
        return;
    };
    let reply = core.write(Action::Send {
        conversation: conversation.to_owned(),
        text,
    });
    cx.background_spawn(async move {
        let _ = reply.await;
    })
    .detach();
}

/// Whether a button that sends asks the bot: the bot is known (`reaches`) and the app isn't `read_only` (a
/// `task/fetch` button asks for its dialog). Else it opens the message in Teams on the web.
#[must_use]
pub fn asks(reaches: bool, read_only: bool) -> bool {
    reaches && !read_only
}

/// The key of card `at`'s input `id`.
#[must_use]
pub fn field_key(at: usize, id: &str) -> String {
    format!("{at}/{id}")
}

/// The inputs of `card`'s body (inside its containers, columns and tables too), and those of the cards its `ShowCard`s
/// open when `shown_cards`.
pub fn card_inputs(card: &Card, shown_cards: bool, inputs: &mut Vec<Input>) {
    element_inputs(&card.body, shown_cards, inputs);
    if shown_cards {
        action_inputs(&card.actions, inputs);
    }
}

fn action_inputs(actions: &[teams_proto::chat::card::Action], inputs: &mut Vec<Input>) {
    for action in actions {
        if let ActionKind::ShowCard(shown) = &action.kind {
            card_inputs(shown, true, inputs);
        }
    }
}

fn element_inputs(elements: &[Element], shown_cards: bool, inputs: &mut Vec<Input>) {
    for element in elements {
        match &element.kind {
            Kind::Input(input) => inputs.push(input.clone()),
            Kind::Container(container) => element_inputs(&container.items, shown_cards, inputs),
            Kind::Columns(set) => {
                for column in &set.columns {
                    element_inputs(&column.items, shown_cards, inputs);
                }
            }
            Kind::Table(table) => {
                for cell in table.rows.iter().flatten() {
                    element_inputs(cell, shown_cards, inputs);
                }
            }
            Kind::Actions(actions) if shown_cards => action_inputs(actions, inputs),
            _ => {}
        }
    }
}

/// A switch's answer: its `valueOn` or its `valueOff`.
fn toggle_answer(input: &Input, on: bool) -> String {
    match &input.kind {
        InputKind::Toggle {
            value_on, value_off, ..
        } => if on { value_on } else { value_off }.clone(),
        _ => String::new(),
    }
}

/// Whether `answer` passes `input`'s checks.
fn passes(input: &Input, answer: Option<&str>) -> bool {
    let Some(answer) = answer.filter(|answer| !answer.is_empty()) else {
        return !input.required;
    };
    match &input.kind {
        InputKind::Text { max_length, regex, .. } => {
            max_length.is_none_or(|most| answer.chars().count() <= most as usize)
                && regex.as_deref().is_none_or(|pattern| {
                    regex::Regex::new(&format!("^(?:{pattern})$")).is_ok_and(|regex| regex.is_match(answer))
                })
        }
        InputKind::Number { min, max, .. } => answer
            .parse::<f64>()
            .is_ok_and(|number| min.is_none_or(|min| number >= min) && max.is_none_or(|max| number <= max)),
        InputKind::Date { min, max, .. } | InputKind::Time { min, max, .. } => {
            min.as_deref().is_none_or(|min| answer >= min) && max.as_deref().is_none_or(|max| answer <= max)
        }
        // A required switch is filled only when it is on (as in Adaptive Cards); its `valueOff` is not an answer.
        InputKind::Toggle { value_on, .. } => !input.required || answer == value_on,
        InputKind::ChoiceSet { .. } => true,
    }
}

/// What a failing input says: its card's words, else ours.
#[must_use]
pub fn error_of(input: &Input) -> String {
    input
        .error_message
        .clone()
        .unwrap_or_else(|| if input.required { REQUIRED } else { NOT_VALID }.to_owned())
}

/// An action's data with the answers merged in by input id: an object gains them, none becomes them, any other data
/// goes as it is.
fn merged(data: Option<&Value>, answers: &Map<String, Value>) -> Value {
    match data {
        Some(Value::Object(object)) => {
            let mut object = object.clone();
            object.extend(answers.clone());
            Value::Object(object)
        }
        None | Some(Value::Null) => Value::Object(answers.clone()),
        Some(other) => other.clone(),
    }
}

/// A card's date, `YYYY-MM-DD`.
fn parse_date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use teams_proto::chat::card::input;

    use super::{merged, passes, toggle_answer};

    #[test]
    fn answers_are_checked_as_the_card_says() {
        let text = input(&json!({"type": "Input.Text", "id": "t", "regex": "[a-z]+", "maxLength": 4})).unwrap();
        assert!(passes(&text, Some("abc")));
        assert!(!passes(&text, Some("abcde")));
        assert!(!passes(&text, Some("ab1")));
        assert!(passes(&text, None));
        let number =
            input(&json!({"type": "Input.Number", "id": "n", "min": 1, "max": 5, "isRequired": true})).unwrap();
        assert!(!passes(&number, None));
        assert!(!passes(&number, Some("9")));
        assert!(!passes(&number, Some("x")));
        assert!(passes(&number, Some("3")));
        let date = input(&json!({"type": "Input.Date", "id": "d", "min": "2026-01-01"})).unwrap();
        assert!(!passes(&date, Some("2025-12-31")));
        assert!(passes(&date, Some("2026-01-02")));
    }

    #[test]
    fn a_required_switch_passes_only_when_on() {
        let required =
            input(&json!({"type": "Input.Toggle", "id": "s", "title": "I agree", "isRequired": true})).unwrap();
        assert!(passes(&required, Some(&toggle_answer(&required, true))));
        assert!(!passes(&required, Some(&toggle_answer(&required, false))));
        let custom = input(&json!({
            "type": "Input.Toggle", "id": "c", "title": "Yes", "isRequired": true, "valueOn": "yes", "valueOff": "no"
        }))
        .unwrap();
        assert!(passes(&custom, Some("yes")));
        assert!(!passes(&custom, Some("no")));
        let optional = input(&json!({"type": "Input.Toggle", "id": "o", "title": "News"})).unwrap();
        assert!(passes(&optional, Some(&toggle_answer(&optional, false))));
        assert!(passes(&optional, Some(&toggle_answer(&optional, true))));
    }

    #[test]
    fn answers_merge_into_the_data() {
        let answers = json!({"a": "1"}).as_object().unwrap().clone();
        assert_eq!(merged(Some(&json!({"k": 2})), &answers), json!({"k": 2, "a": "1"}));
        assert_eq!(merged(None, &answers), json!({"a": "1"}));
        assert_eq!(merged(Some(&json!("text")), &answers), json!("text"));
    }
}
