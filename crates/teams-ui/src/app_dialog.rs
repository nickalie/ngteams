//! An app's dialog (Teams' task module, `docs/protocol/bots.md`),
//! over the window as Teams opens it from a message extension's action command (the compose box's "Actions and apps",
//! a message's "More actions") or a card's `task/fetch` button:
//!
//! - **The head**: the app's picture, its name, and under it the command's title (or the dialog's own), then Close,
//!   as Teams' "Praise · Send praise to people".
//! - **A card**: the app's Adaptive Card across the dialog, its inputs drawn and checked as a chat card's
//!   ([`crate::bot_card`]); its Submit sends the answers to the command (`composeExtension/submitAction`) or to the
//!   button (`task/submit`), once.
//! - **The answer**: nothing (the dialog closes), a next dialog (it takes this one's place), a result (its card goes
//!   into the compose box, as Teams puts it there, and the dialog closes: in a channel the box of the post's thread,
//!   which opens, and a toast when no box shown writes there), a message (the dialog says it), or an error (said under
//!   the card, the dialog stays).
//! - **A web page**: a dialog that is a web page (`taskInfo.url`, or an answer with `url`) needs Teams' JavaScript
//!   host, which this app doesn't have (no web view: AGENTS.md's design invariants), so the dialog says the app opens
//!   in Teams on the web and offers that link.
//!
//! The dialog asks the engine ([`Core::bot_invoke`]); it builds no request. In read-only mode it is never opened.
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement as _, Render, RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, WeakEntity, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{FocusTrapElement as _, Sizable as _};
use serde_json::Value;
use teams_core::bots::BotAsk;
use teams_proto::bot::MessagePayload;
use teams_proto::chat::AppCard;
use teams_proto::chat::card::{self, CardMessage};
use teams_proto::{CommandContext, DialogSize, ExtensionCommand, InvokeAnswer, InvokeKind, StoreApp, TaskDialog};

use crate::bot_card::{CardBot, CardBotEvent, NOT_REACHED, SubmitTo, Target};
use crate::bridge::Core;
use crate::card_view::{BotHook, CardState, CardsElement, ShownCards};
use crate::clock::HourCycle;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::team_dialogs::TeamDialogEvent;
use crate::theme::{self, Palette};

/// What the dialog says while the app's dialog is read.
pub const LOADING: &str = "Loading…";
/// The title of a web-page dialog's stand-in.
pub const WEB_TITLE: &str = "This app opens in Teams on the web";
/// What a web-page dialog's stand-in says under its title.
pub const WEB_LINE: &str = "Its window is a web page, which this app can't show. Continue in Teams on the web.";
/// The button that opens Teams on the web.
pub const OPEN_IN_TEAMS: &str = "Open in Teams on the web";
/// What a dialog with nothing to show says.
pub const NOTHING: &str = "The app sent nothing to show here.";
/// What an answer of a shape this app doesn't read says.
pub const UNREAD: &str = "The app answered in a way this app can't show.";
/// What a sign-in request says.
pub const SIGN_IN: &str = "Sign-in needed. Open Teams on the web to sign in to the app.";
/// What the toast says when a result's card has no box to go into: the chat or channel it is for is not shown.
pub const NO_BOX: &str = "The app's card can't go into a message here.";
/// A dialog's width when the app gives none, px: Teams' "medium".
const DEFAULT_WIDTH: f32 = 600.0;

/// The app a dialog is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogApp {
    pub id: String,
    pub name: String,
    /// Its definition, when known: its picture and accent.
    pub store: Option<StoreApp>,
}

impl DialogApp {
    /// The app of `store`.
    #[must_use]
    pub fn of(store: &StoreApp) -> Self {
        Self {
            id: store.app.id.clone(),
            name: store.app.name.clone(),
            store: Some(store.clone()),
        }
    }
}

/// What the dialog opens on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// A message extension's action command, run in `context` (on `message` when run from one): its dialog is the
    /// app's answer to `fetchTask`, or the definition's own.
    Command {
        command: Box<ExtensionCommand>,
        context: CommandContext,
        message: Option<Box<MessagePayload>>,
    },
    /// A card's `task/fetch` button was answered with `dialog`; `message` is the card's.
    Task {
        dialog: TaskDialog,
        message: Option<String>,
    },
}

/// What a dialog opens with (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSeed {
    pub app: DialogApp,
    /// The bot asked, and the conversation it acts in (where a result's card goes).
    pub target: Target,
    pub opened: Opened,
    /// Teams on the web at this place: where a web-page dialog goes on.
    pub in_teams: Option<String>,
}

/// Where the dialog stands.
enum Stage {
    Loading,
    Card {
        cards: ShownCards,
        bot: Entity<CardBot>,
        _answers: Subscription,
    },
    Web,
    /// The app's message.
    Note(String),
    Failed(String),
}

/// The dialog (see the module docs).
pub struct AppDialog {
    core: Arc<dyn Core>,
    seed: DialogSeed,
    focus: FocusHandle,
    stage: Stage,
    title: Option<String>,
    width: f32,
    state: CardState,
    /// What the last submit's answer says under the card.
    error: Option<String>,
    picture: Option<Arc<RenderImage>>,
    /// The cards' version: a next dialog's card starts with answers of its own.
    round: usize,
}

impl EventEmitter<TeamDialogEvent> for AppDialog {}

impl AppDialog {
    /// Opens on `seed`: a command's dialog is asked of the app (or read from its definition), a button's is shown.
    pub fn new(core: Arc<dyn Core>, seed: DialogSeed, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut dialog = Self {
            core,
            seed: seed.clone(),
            focus,
            stage: Stage::Loading,
            title: None,
            width: DEFAULT_WIDTH,
            state: CardState::default(),
            error: None,
            picture: None,
            round: 0,
        };
        if seed.app.store.is_some() {
            dialog.ask_picture(cx);
        } else {
            dialog.find_app(cx);
        }
        match seed.opened {
            Opened::Command {
                command,
                context,
                message,
            } => {
                dialog.title = Some(command.title.clone());
                if command.fetch_task {
                    dialog.fetch(&command, context, message, cx);
                } else {
                    let info = command.task.clone();
                    dialog.width = width_of(info.as_ref().and_then(|info| info.width));
                    dialog.stage = if info.and_then(|info| info.url).is_some() {
                        Stage::Web
                    } else {
                        Stage::Failed(NOTHING.to_owned())
                    };
                }
            }
            Opened::Task { dialog: shown, .. } => dialog.show(shown, cx),
        }
        dialog
    }

    /// The app's own dialog for `command`, asked (`composeExtension/fetchTask`).
    fn fetch(
        &mut self,
        command: &ExtensionCommand,
        context: CommandContext,
        message: Option<Box<MessagePayload>>,
        cx: &mut Context<Self>,
    ) {
        let reply = self.core.bot_invoke(self.ask(InvokeKind::FetchTask {
            command: command.id.clone(),
            context,
            message: message.map(|message| *message),
        }));
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await.map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| this.answered(answer, cx));
        })
        .detach();
    }

    fn ask(&self, kind: InvokeKind) -> BotAsk {
        BotAsk {
            bot: self.seed.target.bot.clone(),
            app_id: self.seed.target.app_id.clone(),
            conversation: Some(self.seed.target.conversation.clone()),
            kind,
        }
    }

    /// Where the dialog's card sends its submit.
    fn submit_to(&self) -> SubmitTo {
        match &self.seed.opened {
            Opened::Command {
                command,
                context,
                message,
            } => SubmitTo::Command {
                command: command.id.clone(),
                context: *context,
                message: message.clone(),
            },
            Opened::Task { .. } => SubmitTo::Task,
        }
    }

    /// The card message a `task/submit` names: the card the button was on.
    fn message(&self) -> String {
        match &self.seed.opened {
            Opened::Task {
                message: Some(message), ..
            } => message.clone(),
            _ => String::new(),
        }
    }

    /// `dialog` shown: its card, or the web page's stand-in.
    fn show(&mut self, dialog: TaskDialog, cx: &mut Context<Self>) {
        if let Some(title) = dialog.title.clone().filter(|title| !title.trim().is_empty()) {
            self.title = Some(title);
        }
        self.width = width_of(dialog.width);
        self.error = None;
        self.state = CardState::default();
        self.round += 1;
        self.stage = match (dialog.card, dialog.url) {
            (Some(content), _) => {
                let shown = card::adaptive_card_with_inputs(&content);
                let cards = ShownCards(Arc::new(CardMessage {
                    cards: vec![shown],
                    ..CardMessage::default()
                }));
                let made = CardBot::new(Arc::clone(&self.core), self.message(), Some(self.seed.target.clone()))
                    .in_dialog(self.submit_to());
                let bot = cx.new(|_| made);
                // a `task/fetch` button on the dialog's card opens its next dialog in this one's place
                let answers = cx.subscribe(&bot, |this, _, event: &CardBotEvent, cx| match event {
                    CardBotEvent::Answered(answer) => this.answered(answer.clone(), cx),
                    CardBotEvent::Dialog { dialog, .. } => this.show(dialog.clone(), cx),
                });
                cx.observe(&bot, |_, _, cx| cx.notify()).detach();
                Stage::Card {
                    cards,
                    bot,
                    _answers: answers,
                }
            }
            (None, Some(_)) => Stage::Web,
            (None, None) => Stage::Failed(NOTHING.to_owned()),
        };
        cx.notify();
    }

    /// The app's answer to the dialog's fetch or submit (see the module docs).
    fn answered(&mut self, answer: Result<InvokeAnswer, String>, cx: &mut Context<Self>) {
        let loading = matches!(self.stage, Stage::Loading);
        let problem = match answer {
            Ok(InvokeAnswer::Done) if loading => Some(NOTHING.to_owned()),
            Ok(InvokeAnswer::Done) => return Self::close(cx),
            Ok(InvokeAnswer::Dialog(dialog)) => return self.show(dialog, cx),
            Ok(InvokeAnswer::Results { attachments, .. }) => {
                let card = attachments
                    .iter()
                    .find_map(|attachment| app_card_of(attachment, &self.seed.app));
                return match card {
                    Some(card) => {
                        let conversation = self.seed.target.conversation.clone();
                        cx.emit(TeamDialogEvent::AppCard {
                            conversation,
                            card: Box::new(card),
                        });
                    }
                    None => Self::close(cx),
                };
            }
            Ok(InvokeAnswer::Message(text)) => {
                self.stage = Stage::Note(text);
                self.error = None;
                None
            }
            Ok(InvokeAnswer::Error { message, .. }) if !message.trim().is_empty() => Some(message),
            Ok(InvokeAnswer::SignIn(_)) => Some(SIGN_IN.to_owned()),
            Ok(InvokeAnswer::Error { .. } | InvokeAnswer::Refused(_)) | Err(_) => Some(NOT_REACHED.to_owned()),
            Ok(InvokeAnswer::Card(_) | InvokeAnswer::Commands(_) | InvokeAnswer::Unknown(_)) => Some(UNREAD.to_owned()),
        };
        if let Some(problem) = problem {
            if loading {
                self.stage = Stage::Failed(problem);
            } else {
                self.error = Some(problem);
            }
        }
        cx.notify();
    }

    fn close(cx: &mut Context<Self>) {
        cx.emit(TeamDialogEvent::Closed);
    }

    /// The app of a card's bot, found by the engine: its name and picture for the head.
    fn find_app(&mut self, cx: &mut Context<Self>) {
        let reply = self.core.bot_app(self.seed.target.bot.as_str().to_owned());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Some(store)) = reply.await else { return };
            let _ = this.update(cx, |this, cx| {
                this.seed.app = DialogApp::of(&store);
                this.ask_picture(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The app's picture, read once.
    fn ask_picture(&mut self, cx: &mut Context<Self>) {
        let Some(url) = self
            .seed
            .app
            .store
            .as_ref()
            .and_then(|store| store.definition.large_image_url())
        else {
            return;
        };
        let reply = self.core.app_picture(url.to_owned());
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let Ok(Some(bytes)) = reply.await else { return };
            let decoded = cx
                .background_executor()
                .spawn(async move { crate::inline::decode_app_tile(&bytes, 96) })
                .await;
            let Some((image, _)) = decoded else { return };
            let _ = this.update(cx, |this, cx| {
                this.picture = Some(Arc::new(image));
                cx.notify();
            });
        })
        .detach();
    }

    /// Where it stands, for a test.
    #[must_use]
    pub fn stage_name(&self) -> &'static str {
        match self.stage {
            Stage::Loading => "loading",
            Stage::Card { .. } => "card",
            Stage::Web => "web",
            Stage::Note(_) => "note",
            Stage::Failed(_) => "failed",
        }
    }

    /// What its card's submit's answer says under it, for a test.
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Presses button `action` of the dialog's card, as a click does (a test's).
    pub fn press(&mut self, action: usize, cx: &mut Context<Self>) {
        let Stage::Card { cards, bot, .. } = &self.stage else {
            return;
        };
        let Some(card) = cards.0.cards.first() else {
            return;
        };
        let Some(card::ActionKind::Submit(pressed)) = card.actions.get(action).map(|action| &action.kind) else {
            return;
        };
        let mut inputs = Vec::new();
        crate::bot_card::card_inputs(card, false, &mut inputs);
        let (pressed, key) = (pressed.clone(), format!("c0a{action}"));
        bot.update(cx, |bot, cx| bot.press(0, &key, &pressed, &inputs, cx));
    }

    fn head(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let picture = match &self.seed.app.store {
            Some(store) => crate::apps_page::app_picture(store, self.picture.clone(), 32.0, palette),
            None => div()
                .size(px(32.0))
                .flex_none()
                .rounded_md()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(palette.brand))
                .text_color(rgb(palette.on_brand))
                .child(Glyph::BOT.icon().size(px(18.0))),
        };
        let subtitle = self.title.clone().filter(|title| *title != self.seed.app.name);
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .child(picture)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .debug_selector(|| "app-dialog-app".to_owned())
                            .text_sm()
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(SharedString::from(self.seed.app.name.clone())),
                    )
                    .children(subtitle.map(|subtitle| {
                        div()
                            .debug_selector(|| "app-dialog-title".to_owned())
                            .text_xs()
                            .truncate()
                            .child(SharedString::from(subtitle))
                    })),
            )
            .child(selected_by(
                "app-dialog-close",
                icon_button("app-dialog-close", Glyph::X, "Close").on_click(cx.listener(|_, _, _, cx| Self::close(cx))),
            ))
    }

    fn body(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.stage {
            Stage::Loading => centred(palette)
                .debug_selector(|| "app-dialog-loading".to_owned())
                .child(Glyph::LOADING.icon().size(px(20.0)))
                .child(LOADING)
                .into_any_element(),
            Stage::Card { cards, bot, .. } => {
                let state_of = cx.entity().downgrade();
                let on_state: crate::card_view::OnState = std::rc::Rc::new(move |state, _, cx| {
                    let _ = state_of.update(cx, |this, cx| {
                        this.state = state;
                        cx.notify();
                    });
                });
                let drawn = CardsElement::new(
                    SharedString::from(format!("app-dialog-{}", self.round)),
                    cards.clone(),
                    palette,
                    HourCycle::Twelve,
                )
                .bare(true)
                .in_teams(self.seed.in_teams.clone())
                .bot(Some(BotHook {
                    bot: bot.clone(),
                    read_only: self.core.read_only(),
                }))
                .source(Some(crate::rich_text::PictureSource::new(Arc::clone(&self.core))))
                .state(self.state.clone(), on_state);
                div()
                    .id("app-dialog-card")
                    .debug_selector(|| "app-dialog-card".to_owned())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(drawn)
                    .into_any_element()
            }
            Stage::Web => self.web(palette, cx),
            Stage::Note(text) => Self::note(text, palette, cx),
            Stage::Failed(problem) => centred(palette)
                .debug_selector(|| "app-dialog-failed".to_owned())
                .child(Glyph::CIRCLE_ALERT.icon().size(px(20.0)).text_color(rgb(theme::DANGER)))
                .child(SharedString::from(problem.clone()))
                .into_any_element(),
        }
    }

    /// The stand-in of a dialog that is a web page.
    fn web(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let link = self.seed.in_teams.clone();
        centred(palette)
            .debug_selector(|| "app-dialog-web".to_owned())
            .child(
                div()
                    .size(px(48.0))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(crate::rich_text::tint(palette.brand, 0.12))
                    .text_color(rgb(palette.brand))
                    .child(Glyph::GLOBE.icon().size(px(22.0))),
            )
            .child(
                div()
                    .text_color(rgb(palette.foreground))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(WEB_TITLE),
            )
            .child(div().max_w(px(380.0)).text_center().text_sm().child(WEB_LINE))
            .children(link.map(|link| {
                selected_by(
                    "app-dialog-open-web",
                    Button::new("app-dialog-open-web")
                        .primary()
                        .label(OPEN_IN_TEAMS)
                        .icon(Glyph::OPEN_OUT.icon())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.open_url(&link);
                            Self::close(cx);
                        })),
                )
            }))
            .into_any_element()
    }

    /// The app's message, with Close.
    fn note(text: &str, palette: Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        centred(palette)
            .debug_selector(|| "app-dialog-note".to_owned())
            .child(
                div()
                    .text_color(rgb(palette.foreground))
                    .text_center()
                    .child(SharedString::from(text.to_owned())),
            )
            .child(selected_by(
                "app-dialog-done",
                Button::new("app-dialog-done")
                    .label("Close")
                    .small()
                    .on_click(cx.listener(|_, _, _, cx| Self::close(cx))),
            ))
            .into_any_element()
    }
}

/// A column in the middle of the dialog's body, its words muted.
fn centred(palette: Palette) -> gpui::Div {
    div()
        .flex_1()
        .min_h(px(200.0))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        .text_color(rgb(palette.muted_foreground))
}

/// A dialog's width as its app gives it, px: Teams' small, medium and large, or the app's own, held between 320 and
/// 960.
#[must_use]
pub fn width_of(size: Option<DialogSize>) -> f32 {
    match size {
        Some(DialogSize::Small) => 400.0,
        Some(DialogSize::Large) => 800.0,
        Some(DialogSize::Pixels(px)) => f32::from(u16::try_from(px).unwrap_or(u16::MAX)).clamp(320.0, 960.0),
        Some(DialogSize::Medium) | None => DEFAULT_WIDTH,
    }
}

/// The card of an app's result (`attachment`: an Adaptive Card, a hero or a thumbnail card, never its `preview`) as
/// the compose box sends it ([`AppCard`]); `None` for an attachment of another kind.
#[must_use]
pub fn app_card_of(attachment: &Value, app: &DialogApp) -> Option<AppCard> {
    let content_type = attachment.get("contentType")?.as_str()?;
    if !matches!(
        content_type,
        "application/vnd.microsoft.card.adaptive"
            | "application/vnd.microsoft.card.hero"
            | "application/vnd.microsoft.card.thumbnail"
    ) {
        return None;
    }
    let content = attachment.get("content")?.clone();
    Some(AppCard::new(
        app.id.clone(),
        app.name.clone(),
        app.store
            .as_ref()
            .and_then(|store| store.definition.large_image_url())
            .map(str::to_owned),
        content_type.to_owned(),
        content,
    ))
}

impl Render for AppDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "app-dialog-error".to_owned())
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(rgb(theme::DANGER))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)))
                .child(SharedString::from(error))
        });
        crate::team_dialogs::surface("app-dialog", palette, window)
            .w(px(self.width))
            .key_context("TeamDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    cx.stop_propagation();
                    Self::close(cx);
                }
            }))
            .child(self.head(palette, cx))
            .child(self.body(palette, cx))
            .when_some(error, gpui::ParentElement::child)
            .focus_trap("app-dialog-trap", &self.focus)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn widths_follow_teams_sizes() {
        assert!((width_of(None) - 600.0).abs() < f32::EPSILON);
        assert!((width_of(Some(DialogSize::Small)) - 400.0).abs() < f32::EPSILON);
        assert!((width_of(Some(DialogSize::Large)) - 800.0).abs() < f32::EPSILON);
        assert!((width_of(Some(DialogSize::Pixels(100))) - 320.0).abs() < f32::EPSILON);
        assert!((width_of(Some(DialogSize::Pixels(5000))) - 960.0).abs() < f32::EPSILON);
    }

    #[test]
    fn only_a_drawable_card_goes_into_a_message() {
        let app = DialogApp {
            id: "app".to_owned(),
            name: "App".to_owned(),
            store: None,
        };
        let card = app_card_of(
            &json!({"contentType": "application/vnd.microsoft.card.thumbnail", "content": {"title": "T"}}),
            &app,
        )
        .unwrap();
        assert_eq!(card.app_id, "app");
        assert_eq!(card.content["title"], "T");
        assert!(app_card_of(&json!({"contentType": "text/html", "content": "x"}), &app).is_none());
        assert!(app_card_of(&json!({"content": {}}), &app).is_none());
    }
}
