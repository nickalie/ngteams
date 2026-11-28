//! The compose box's "Actions and apps" (the "+") for an account
//! whose cloud serves apps, as Teams' flyout (`docs/protocol/bots.md`, "Message extensions"):
//!
//! - **The list**: "Search for apps" over the account's apps that have a message extension command for the compose
//!   box ([`Core::message_extensions`], read each time it opens), each its picture and name, then "Get more apps",
//!   which opens the store. Teams lists its own actions (Schedule message, Set delivery options, Schedule meeting,
//!   Record video clip) above the apps; this app lists the box's own there ([`FlyoutAction`]: the tools the user hid
//!   from the toolbar), while no search is typed, and Up and Down go through them first. "Customize toolbar" is at
//!   its foot, beside "Get more apps".
//! - **An app**: its commands come from its definition, or from the app itself when it asks for that
//!   (`composeExtension/fetchCommands`). An app with one action command opens its dialog at once
//!   ([`crate::app_dialog`]); else its pane shows a tab per command, the search commands' results under a search
//!   field (`composeExtension/query`, run at once with no words when the command says so, again as the words change).
//! - **A result** chosen: its card goes into the compose box, as Teams puts it there; one whose preview names an
//!   `invoke` asks the app for its card first (`composeExtension/selectItem`).
//!
//! The flyout asks the engine; it builds no request ([`crate::bridge::Core`]). Up and Down move in its lists, which
//! scroll to keep the lit row in view; Enter in a field of it chooses (a focused button of it takes its own Enter),
//! Escape goes back or closes it.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, RenderImage, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::kbd::Kbd;
use serde_json::Value;
use teams_core::bots::BotAsk;
use teams_proto::chat::AppCard;
use teams_proto::{CommandContext, ExtensionApp, ExtensionCommand, InvokeAnswer, InvokeKind};

use crate::app_dialog::{DialogApp, DialogSeed, Opened, SIGN_IN, app_card_of};
use crate::bot_card::{NOT_REACHED, Target};
use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

/// The flyout's width, px, as Teams'.
pub const WIDTH: f32 = 340.0;
/// The tallest the flyout's list grows, px.
const LIST_HEIGHT: f32 = 300.0;
/// How long the words of a search rest before they are sent.
const SEARCH_AFTER: Duration = Duration::from_millis(350);
/// How many results a search asks for.
const RESULTS: u32 = 25;
/// "Get more apps".
pub const GET_MORE_APPS: &str = "Get more apps";
/// The search field's words.
pub const SEARCH_APPS: &str = "Search for apps";
/// What the list says when the account has no app with an action.
pub const NO_APPS: &str = "Apps you add that work in messages show here.";
/// What the list says when no app's name has the words.
pub const NO_MATCH: &str = "No apps match your search.";
/// What a search with no result says.
pub const NO_RESULTS: &str = "No results.";

/// What the flyout tells its compose box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlyoutEvent {
    /// It closes with nothing chosen.
    Close,
    /// "Get more apps": the store opens.
    GetMoreApps,
    /// An action command was chosen: its dialog opens over the window.
    Dialog(Box<DialogSeed>),
    /// A result was chosen: its card goes into the box.
    Card(Box<AppCard>),
    /// The action of its row at this place was chosen ([`AppsFlyout::with_actions`]).
    Action(usize),
    /// "Customize toolbar": the box lists its buttons to show or hide.
    Customize,
}

/// An action the box lists over the apps, as Teams lists its own: the action of a tool the user hid from the toolbar.
#[derive(Clone)]
pub struct FlyoutAction {
    /// Its row's test selector.
    pub selector: SharedString,
    pub label: SharedString,
    pub glyph: Glyph,
    /// Its key, as bound in the box.
    pub key: Option<Kbd>,
    /// It cannot be taken now.
    pub disabled: bool,
    /// A line above it: it starts a group apart from the actions before it.
    pub apart: bool,
}

/// A search's results, as they stand.
#[derive(Clone, Debug, PartialEq)]
enum Results {
    Idle,
    Searching,
    Found(Vec<Found>),
    /// The app's message, or why it wasn't reached.
    Said(String),
}

/// A result: its preview's words, the attachment itself, and its preview's `invoke` tap.
#[derive(Clone, Debug, PartialEq)]
struct Found {
    title: String,
    text: String,
    attachment: Value,
    tap: Option<Value>,
}

/// An app's pane.
struct Pane {
    app: ExtensionApp,
    commands: Vec<ExtensionCommand>,
    chosen: usize,
    query: Entity<InputState>,
    results: Results,
    /// The flyout's `asked` when this pane's last search went: an answer of an older ask is dropped.
    asked: u64,
    waiting: Option<Task<()>>,
    _words: Subscription,
}

/// The flyout (see the module docs).
pub struct AppsFlyout {
    core: Arc<dyn Core>,
    conversation: String,
    focus: FocusHandle,
    search: Entity<InputState>,
    apps: Option<Vec<ExtensionApp>>,
    pictures: BTreeMap<String, Option<Arc<RenderImage>>>,
    pane: Option<Pane>,
    /// The box's actions, over the apps while no search is typed.
    actions: Vec<FlyoutAction>,
    /// The row Up and Down stand on: the actions' rows, then the apps'.
    highlight: usize,
    /// The shown list's scroll: the app list's or the results'.
    rows: ScrollHandle,
    /// Bumped by each app chosen and each search sent, and never reset, so that a late answer of an ask for a pane
    /// that is gone (Back, then a different app) never matches an ask of the pane that is open now.
    asked: u64,
    _search: Subscription,
}

impl EventEmitter<FlyoutEvent> for AppsFlyout {}

impl AppsFlyout {
    /// The flyout of the compose box of `conversation`; the account's apps are read now.
    pub fn new(core: Arc<dyn Core>, conversation: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(SEARCH_APPS));
        let watch = cx.subscribe(&search, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.light(0);
            }
            cx.notify();
        });
        search.update(cx, |search, cx| search.focus(window, cx));
        let reply = core.message_extensions();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let apps = reply.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.apps = Some(apps);
                this.ask_pictures(cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            core,
            conversation,
            focus: cx.focus_handle(),
            search,
            apps: None,
            pictures: BTreeMap::new(),
            pane: None,
            actions: Vec::new(),
            highlight: 0,
            rows: ScrollHandle::new(),
            asked: 0,
            _search: watch,
        }
    }

    /// Whether the keys are in the flyout.
    #[must_use]
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The flyout with `actions` over its apps.
    #[must_use]
    pub fn with_actions(mut self, actions: Vec<FlyoutAction>) -> Self {
        self.actions = actions;
        self
    }

    /// The box's actions changed while it is open (a job started or stopped): its rows show them now.
    pub fn set_actions(&mut self, actions: Vec<FlyoutAction>, cx: &mut Context<Self>) {
        self.actions = actions;
        cx.notify();
    }

    /// The box's actions it holds, as it lists them while no search is typed.
    #[must_use]
    pub fn actions(&self) -> &[FlyoutAction] {
        &self.actions
    }

    /// The actions it lists now: none while a search is typed.
    fn shown_actions(&self, cx: &gpui::App) -> &[FlyoutAction] {
        if self.search.read(cx).value().trim().is_empty() {
            &self.actions
        } else {
            &[]
        }
    }

    /// The action at `at` chosen, unless it cannot be taken now.
    fn choose_action(&mut self, at: usize, cx: &mut Context<Self>) {
        if self.actions.get(at).is_some_and(|action| !action.disabled) {
            cx.emit(FlyoutEvent::Action(at));
        }
    }

    /// The apps it lists: those with a command for the compose box whose name has the search's words.
    #[must_use]
    pub fn listed(&self, cx: &gpui::App) -> Vec<ExtensionApp> {
        let words = self.search.read(cx).value().trim().to_lowercase();
        self.apps
            .iter()
            .flatten()
            .filter(|app| app.installed && !app.commands_in(CommandContext::Compose).is_empty())
            .filter(|app| words.is_empty() || app.name().to_lowercase().contains(&words))
            .cloned()
            .collect()
    }

    /// Whether the account's apps were read.
    #[must_use]
    pub fn loaded(&self) -> bool {
        self.apps.is_some()
    }

    /// The open app's name, its pane's tabs and its search's results' titles, for a test.
    #[must_use]
    pub fn pane(&self) -> Option<(String, Vec<String>, Vec<String>)> {
        let pane = self.pane.as_ref()?;
        let found = match &pane.results {
            Results::Found(found) => found.iter().map(|item| item.title.clone()).collect(),
            _ => Vec::new(),
        };
        Some((
            pane.app.name().to_owned(),
            pane.commands.iter().map(|command| command.title.clone()).collect(),
            found,
        ))
    }

    /// The app `id` chosen, as a click on its row does.
    pub fn choose_app(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(app) = self.apps.iter().flatten().find(|app| app.id() == id).cloned() else {
            return;
        };
        let commands = app.commands_in(CommandContext::Compose);
        self.asked += 1;
        let asked = self.asked;
        if let Some(bot) = app.fetches_commands() {
            let reply = self.core.bot_invoke(BotAsk {
                bot: bot.clone(),
                app_id: app.id().to_owned(),
                conversation: Some(self.conversation.clone()),
                kind: InvokeKind::FetchCommands {
                    context: CommandContext::Compose,
                },
            });
            let fallback = commands;
            cx.spawn_in(window, async move |this, cx| {
                let fetched = match reply.await {
                    Ok(InvokeAnswer::Commands(fetched)) if !fetched.is_empty() => fetched
                        .iter()
                        .map(|command| ExtensionCommand::fetched(&bot, command, CommandContext::Compose))
                        .collect(),
                    _ => fallback,
                };
                // a different app chosen meanwhile, or this one twice: only the last choice opens
                let _ = this.update_in(cx, |this, window, cx| {
                    (this.asked == asked && this.pane.is_none()).then(|| this.open_pane(app, fetched, window, cx))
                });
            })
            .detach();
            return;
        }
        self.open_pane(app, commands, window, cx);
    }

    fn open_pane(
        &mut self,
        app: ExtensionApp,
        commands: Vec<ExtensionCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if commands.is_empty() {
            return;
        }
        if let [command] = commands.as_slice()
            && command.action
        {
            return self.open_dialog(&app, command.clone(), cx);
        }
        let chosen = commands.iter().position(|command| !command.action).unwrap_or(0);
        let query = cx.new(|cx| InputState::new(window, cx));
        let words = cx.subscribe(&query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.search_soon(cx);
            }
        });
        query.update(cx, |query, cx| query.focus(window, cx));
        self.pane = Some(Pane {
            app,
            commands,
            chosen,
            query,
            results: Results::Idle,
            asked: 0,
            waiting: None,
            _words: words,
        });
        self.light(0);
        self.choose_command(chosen, window, cx);
    }

    /// The pane's command `at` chosen: an action's dialog opens; a search's field takes its words, and a search that
    /// runs at once goes.
    pub fn choose_command(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        let Some(command) = pane.commands.get(at).cloned() else {
            return;
        };
        if command.action {
            let app = pane.app.clone();
            return self.open_dialog(&app, command, cx);
        }
        pane.chosen = at;
        pane.results = Results::Idle;
        let parameter = command.parameters.first();
        let placeholder = parameter.map_or_else(
            || command.title.clone(),
            |parameter| {
                if parameter.description.is_empty() {
                    parameter.title.clone()
                } else {
                    parameter.description.clone()
                }
            },
        );
        let start = parameter
            .and_then(|parameter| parameter.value.clone())
            .unwrap_or_default();
        pane.query.update(cx, |query, cx| {
            query.set_placeholder(placeholder, window, cx);
            query.set_value(start, window, cx);
        });
        if command.initial_run {
            self.search(cx);
        }
        cx.notify();
    }

    fn open_dialog(&mut self, app: &ExtensionApp, command: ExtensionCommand, cx: &mut Context<Self>) {
        let seed = DialogSeed {
            app: DialogApp::of(&app.store),
            target: Target {
                bot: command.bot.clone(),
                app_id: app.id().to_owned(),
                conversation: self.conversation.clone(),
            },
            opened: Opened::Command {
                command: Box::new(command),
                context: CommandContext::Compose,
                message: None,
            },
            in_teams: Some(teams_proto::chat::web_chat_link(&self.conversation)),
        };
        cx.emit(FlyoutEvent::Dialog(Box::new(seed)));
    }

    /// The search sent once its words rest.
    fn search_soon(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        pane.waiting = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            cx.background_executor().timer(SEARCH_AFTER).await;
            let _ = this.update(cx, Self::search);
        }));
    }

    /// The chosen search command's query, with the field's words, sent now.
    pub fn search(&mut self, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_mut() else {
            return;
        };
        let Some(command) = pane.commands.get(pane.chosen).cloned() else {
            return;
        };
        let words = pane.query.read(cx).value().trim().to_owned();
        let parameters = command
            .parameters
            .first()
            .map(|parameter| vec![(parameter.name.clone(), words)])
            .unwrap_or_default();
        self.asked += 1;
        let asked = self.asked;
        pane.asked = asked;
        pane.results = Results::Searching;
        let reply = self.core.bot_invoke(BotAsk {
            bot: command.bot.clone(),
            app_id: pane.app.id().to_owned(),
            conversation: Some(self.conversation.clone()),
            kind: InvokeKind::Query {
                command: command.id,
                parameters,
                skip: 0,
                count: RESULTS,
            },
        });
        cx.notify();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let results = results_of(reply.await.map_err(|error| error.to_string()));
            let _ = this.update(cx, |this, cx| {
                if let Some(pane) = this.pane.as_mut().filter(|pane| pane.asked == asked) {
                    pane.results = results;
                    this.light(0);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Result `at` chosen: its card (asked of the app first when its preview says so) goes into the box.
    pub fn choose_result(&mut self, at: usize, cx: &mut Context<Self>) {
        let Some(pane) = self.pane.as_ref() else {
            return;
        };
        let Results::Found(found) = &pane.results else {
            return;
        };
        let Some(item) = found.get(at).cloned() else {
            return;
        };
        let app = DialogApp::of(&pane.app.store);
        let asked = pane.asked;
        let Some(tap) = item.tap.clone() else {
            if let Some(card) = app_card_of(&item.attachment, &app) {
                cx.emit(FlyoutEvent::Card(Box::new(card)));
            }
            return;
        };
        let Some(command) = pane.commands.get(pane.chosen) else {
            return;
        };
        let reply = self.core.bot_invoke(BotAsk {
            bot: command.bot.clone(),
            app_id: pane.app.id().to_owned(),
            conversation: Some(self.conversation.clone()),
            kind: InvokeKind::SelectItem { value: tap },
        });
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await.map_err(|error| error.to_string());
            let _ = this.update(cx, |this, cx| this.selected(answer, &item, &app, asked, cx));
        })
        .detach();
    }

    /// The app's answer to a result chosen: its card goes into the box (the result's own when the app gave none);
    /// else what it said shows. The answer is dropped when the pane of the search that the result came from
    /// (`asked`) is gone.
    fn selected(
        &mut self,
        answer: Result<InvokeAnswer, String>,
        item: &Found,
        app: &DialogApp,
        asked: u64,
        cx: &mut Context<Self>,
    ) {
        if self.pane.as_ref().is_none_or(|pane| pane.asked != asked) {
            return;
        }
        let chosen = match &answer {
            Ok(InvokeAnswer::Results { attachments, .. }) => {
                attachments.iter().find_map(|attachment| app_card_of(attachment, app))
            }
            Ok(InvokeAnswer::Done) => app_card_of(&item.attachment, app),
            _ => None,
        };
        if let Some(card) = chosen {
            return cx.emit(FlyoutEvent::Card(Box::new(card)));
        }
        if let Some(pane) = self.pane.as_mut() {
            pane.results = results_of(answer);
            cx.notify();
        }
    }

    /// Back from an app's pane to the list; from the list, closed.
    pub fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane.take().is_some() {
            self.light(0);
            self.search.update(cx, |search, cx| search.focus(window, cx));
            cx.notify();
        } else {
            cx.emit(FlyoutEvent::Close);
        }
    }

    /// Row `at` lit, and the list scrolled to show it.
    fn light(&mut self, at: usize) {
        self.highlight = at;
        self.rows.scroll_to_item(at);
    }

    /// Whether the keyboard is in the search field, the pane's query field or the flyout itself. Enter then
    /// chooses the lit row. A focused button of the flyout (the foot, a command) gets its Enter itself.
    fn typing(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus.is_focused(window)
            || self.search.read(cx).focus_handle(cx).is_focused(window)
            || self
                .pane
                .as_ref()
                .is_some_and(|pane| pane.query.read(cx).focus_handle(cx).is_focused(window))
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let actions = if self.pane.is_some() {
            0
        } else {
            self.shown_actions(cx).len()
        };
        let rows = match &self.pane {
            Some(pane) => match &pane.results {
                Results::Found(found) => found.len(),
                _ => 0,
            },
            None => actions + self.listed(cx).len(),
        };
        match event.keystroke.key.as_str() {
            "escape" => self.back(window, cx),
            "down" if rows > 0 => self.light((self.highlight + 1).min(rows - 1)),
            "up" => self.light(self.highlight.saturating_sub(1)),
            "enter" if rows > 0 && self.typing(window, cx) => {
                let at = self.highlight.min(rows - 1);
                if self.pane.is_some() {
                    self.choose_result(at, cx);
                } else if at < actions {
                    self.choose_action(at, cx);
                } else {
                    let id = self.listed(cx)[at - actions].id().to_owned();
                    self.choose_app(&id, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Asks once for each app's picture.
    fn ask_pictures(&mut self, cx: &mut Context<Self>) {
        let wanted: Vec<(String, String)> = self
            .apps
            .iter()
            .flatten()
            .filter_map(|app| Some((app.id().to_owned(), app.store.definition.large_image_url()?.to_owned())))
            .collect();
        for (id, url) in wanted {
            if self.pictures.contains_key(&id) {
                continue;
            }
            self.pictures.insert(id.clone(), None);
            let reply = self.core.app_picture(url);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(Some(bytes)) = reply.await else { return };
                let decoded = cx
                    .background_executor()
                    .spawn(async move { crate::inline::decode_app_tile(&bytes, 48) })
                    .await;
                let Some((image, _)) = decoded else { return };
                let _ = this.update(cx, |this, cx| {
                    this.pictures.insert(id, Some(Arc::new(image)));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn picture(&self, app: &ExtensionApp, side: f32, palette: Palette) -> gpui::Div {
        crate::apps_page::app_picture(
            &app.store,
            self.pictures.get(app.id()).cloned().flatten(),
            side,
            palette,
        )
    }

    /// The box's actions' rows, a line over each that starts a group and under the last, as Teams draws its own over
    /// the apps. Each is one child of the list, so that the row Up and Down light is the child the list scrolls to.
    fn action_rows(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let actions = self.shown_actions(cx).to_vec();
        let last = actions.len().saturating_sub(1);
        actions
            .into_iter()
            .enumerate()
            .map(|(at, action)| {
                div()
                    .when(action.apart, |row| {
                        row.pt_1().mt_1().border_t_1().border_color(rgb(palette.border))
                    })
                    .when(at == last, |row| {
                        row.pb_1().mb_1().border_b_1().border_color(rgb(palette.border))
                    })
                    .child(self.action_row(at, action, palette, cx))
                    .into_any_element()
            })
            .collect()
    }

    fn action_row(&self, at: usize, action: FlyoutAction, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let selector = action.selector.clone();
        let tint = if action.disabled {
            palette.muted_foreground
        } else {
            palette.foreground
        };
        div()
            .id(action.selector.clone())
            .debug_selector(move || selector.to_string())
            .w_full()
            .min_h(px(36.0))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .rounded_md()
            .text_sm()
            .text_color(rgb(tint))
            .when(!action.disabled, |row| {
                row.cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_action(at, cx)))
            })
            .when(at == self.highlight, |row| row.bg(rgb(palette.hover)))
            .child(action.glyph.icon().size(px(16.0)))
            .child(div().flex_1().min_w_0().truncate().child(action.label))
            .children(action.key.map(|key| {
                let selector = format!("{}-key", action.selector);
                div().debug_selector(move || selector).child(key)
            }))
            .into_any_element()
    }

    /// The flyout's foot: "Customize toolbar" of the box, and "Get more apps".
    fn foot(cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .justify_between()
            .child(selected_by(
                "compose-customize-row",
                Button::new("apps-flyout-customize")
                    .ghost()
                    .small()
                    .icon(Glyph::SETTINGS.icon())
                    .label(crate::compose::CUSTOMIZE)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FlyoutEvent::Customize))),
            ))
            .child(selected_by(
                "apps-flyout-more",
                Button::new("apps-flyout-more")
                    .outline()
                    .small()
                    .icon(Glyph::PLUS.icon())
                    .label(GET_MORE_APPS)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(FlyoutEvent::GetMoreApps))),
            ))
    }

    fn list(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let listed = self.listed(cx);
        let actions = self.shown_actions(cx).len();
        let note = |selector: &'static str, words: &'static str| {
            div()
                .debug_selector(move || selector.to_owned())
                .px_2()
                .py_4()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(words)
                .into_any_element()
        };
        let rows: Vec<AnyElement> = if self.apps.is_none() {
            vec![note("apps-flyout-loading", crate::app_dialog::LOADING)]
        } else if listed.is_empty() {
            let searching = !self.search.read(cx).value().trim().is_empty();
            vec![if searching {
                note("apps-flyout-no-match", NO_MATCH)
            } else {
                note("apps-flyout-empty", NO_APPS)
            }]
        } else {
            listed
                .iter()
                .enumerate()
                .map(|(at, app)| {
                    let id = app.id().to_owned();
                    row(
                        format!("apps-flyout-app-{id}"),
                        actions + at == self.highlight,
                        palette,
                        self.picture(app, 20.0, palette),
                        app.name().to_owned(),
                        None,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.choose_app(&id, window, cx)))
                    .into_any_element()
                })
                .collect()
        };
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "apps-flyout-search".to_owned())
                    .child(Input::new(&self.search).prefix(Glyph::SEARCH.icon().size(px(14.0)))),
            )
            .child(
                div()
                    .id("apps-flyout-list")
                    .debug_selector(|| "apps-flyout-list".to_owned())
                    .max_h(px(LIST_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&self.rows)
                    .flex()
                    .flex_col()
                    .children(self.action_rows(palette, cx))
                    .children(rows),
            )
            .child(div().h(px(1.0)).bg(rgb(palette.border)))
            .child(Self::foot(cx))
            .into_any_element()
    }

    fn pane_view(&self, pane: &Pane, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let head = div()
            .flex()
            .items_center()
            .gap_2()
            .child(selected_by(
                "apps-flyout-back",
                icon_button("apps-flyout-back", Glyph::ARROW_LEFT, "Back")
                    .on_click(cx.listener(|this, _, window, cx| this.back(window, cx))),
            ))
            .child(self.picture(&pane.app, 20.0, palette))
            .child(
                div()
                    .debug_selector(|| "apps-flyout-pane-name".to_owned())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(pane.app.name().to_owned())),
            );
        let tabs = (pane.commands.len() > 1).then(|| {
            div()
                .flex()
                .flex_wrap()
                .gap_3()
                .children(pane.commands.iter().enumerate().map(|(at, command)| {
                    let chosen = at == pane.chosen && !command.action;
                    div()
                        .id(SharedString::from(format!("apps-flyout-command-{at}")))
                        .debug_selector(move || format!("apps-flyout-command-{at}"))
                        .pb_1()
                        .text_sm()
                        .cursor_pointer()
                        .border_b_2()
                        .border_color(rgb(if chosen { palette.brand } else { palette.surface }))
                        .when(chosen, |tab| tab.font_weight(gpui::FontWeight::SEMIBOLD))
                        .text_color(rgb(if chosen {
                            palette.foreground
                        } else {
                            palette.muted_foreground
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| this.choose_command(at, window, cx)))
                        .child(SharedString::from(command.title.clone()))
                }))
        });
        let searches = pane.commands.get(pane.chosen).is_some_and(|command| !command.action);
        let field = searches.then(|| {
            div()
                .debug_selector(|| "apps-flyout-query".to_owned())
                .child(Input::new(&pane.query).prefix(Glyph::SEARCH.icon().size(px(14.0))))
        });
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(head)
            .children(tabs)
            .children(field)
            .child(self.results(pane, palette, cx))
            .into_any_element()
    }

    fn results(&self, pane: &Pane, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let said = |selector: &'static str, words: String| {
            div()
                .debug_selector(move || selector.to_owned())
                .px_2()
                .py_4()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(words))
                .into_any_element()
        };
        let rows: Vec<AnyElement> = match &pane.results {
            Results::Idle => Vec::new(),
            Results::Searching => vec![said("apps-flyout-searching", crate::app_dialog::LOADING.to_owned())],
            Results::Said(words) => vec![said("apps-flyout-said", words.clone())],
            Results::Found(found) if found.is_empty() => vec![said("apps-flyout-no-results", NO_RESULTS.to_owned())],
            Results::Found(found) => found
                .iter()
                .enumerate()
                .map(|(at, item)| {
                    let initial = item.title.chars().next().map(String::from).unwrap_or_default();
                    let face = div()
                        .size(px(32.0))
                        .flex_none()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(crate::rich_text::tint(palette.brand, 0.14))
                        .text_color(rgb(palette.brand))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(initial);
                    let text = (!item.text.is_empty()).then(|| item.text.clone());
                    row(
                        format!("apps-flyout-result-{at}"),
                        at == self.highlight,
                        palette,
                        face,
                        item.title.clone(),
                        text,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_result(at, cx)))
                    .into_any_element()
                })
                .collect(),
        };
        div()
            .id("apps-flyout-results")
            .debug_selector(|| "apps-flyout-results".to_owned())
            .max_h(px(LIST_HEIGHT))
            .overflow_y_scroll()
            .track_scroll(&self.rows)
            .flex()
            .flex_col()
            .children(rows)
            .into_any_element()
    }
}

/// A row of a list: a picture, a name, and under it a line when given; the row Up and Down stand on is lit.
fn row(
    selector: String,
    lit: bool,
    palette: Palette,
    picture: gpui::Div,
    name: String,
    line: Option<String>,
) -> gpui::Stateful<gpui::Div> {
    let id = SharedString::from(selector.clone());
    div()
        .id(id)
        .debug_selector(move || selector)
        .w_full()
        .min_h(px(36.0))
        .px_2()
        .py_1()
        .flex()
        .items_center()
        .gap_3()
        .rounded_md()
        .cursor_pointer()
        .when(lit, |row| row.bg(rgb(palette.hover)))
        .hover(move |row| row.bg(rgb(palette.hover)))
        .child(picture)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().truncate().child(SharedString::from(name)))
                .children(line.map(|line| {
                    div()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .line_clamp(2)
                        .child(SharedString::from(line))
                })),
        )
}

/// A search's answer as the pane shows it.
fn results_of(answer: Result<InvokeAnswer, String>) -> Results {
    match answer {
        Ok(InvokeAnswer::Results { attachments, .. }) => Results::Found(attachments.iter().map(found_of).collect()),
        Ok(InvokeAnswer::Message(text)) => Results::Said(text),
        Ok(InvokeAnswer::Done) => Results::Found(Vec::new()),
        Ok(InvokeAnswer::SignIn(_)) => Results::Said(SIGN_IN.to_owned()),
        Ok(InvokeAnswer::Error { message, .. }) if !message.trim().is_empty() => Results::Said(message),
        _ => Results::Said(NOT_REACHED.to_owned()),
    }
}

/// A result's words, from its preview (a hero or thumbnail card), else from the card itself; and its preview's
/// `invoke` tap.
fn found_of(attachment: &Value) -> Found {
    let shown = attachment
        .pointer("/preview/content")
        .or_else(|| attachment.get("content"));
    let text = |key: &str| {
        shown
            .and_then(|card| card.get(key))
            .and_then(Value::as_str)
            .map(plain)
            .unwrap_or_default()
    };
    let mut title = text("title");
    if title.is_empty() {
        title = shown
            .and_then(|card| card.get("body"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|element| element.get("text").and_then(Value::as_str))
            .map(plain)
            .unwrap_or_default();
    }
    let tap = shown
        .and_then(|card| card.get("tap"))
        .filter(|tap| tap.get("type").and_then(Value::as_str) == Some("invoke"))
        .and_then(|tap| tap.get("value"))
        .cloned();
    Found {
        title,
        text: text("text"),
        attachment: attachment.clone(),
        tap,
    }
}

/// A preview's words with its markup taken out, on one line.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => out.push(if c.is_whitespace() { ' ' } else { c }),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Render for AppsFlyout {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let inside = match self.pane.take() {
            Some(pane) => {
                let drawn = self.pane_view(&pane, palette, cx);
                self.pane = Some(pane);
                drawn
            }
            None => self.list(palette, cx),
        };
        div()
            .id("apps-flyout")
            .debug_selector(|| "apps-flyout".to_owned())
            .key_context("AppsFlyout")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)))
            .occlude()
            .w(px(WIDTH))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(inside)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_result_reads_from_its_preview_else_its_card() {
        let with_preview = json!({
            "contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Card"}]},
            "preview": {"contentType": "application/vnd.microsoft.card.thumbnail", "content": {
                "title": "<b>Record</b> 1", "text": "A  made-up\\nline", "tap": {"type": "invoke", "value": {"id": 1}}
            }}
        });
        let found = found_of(&with_preview);
        assert_eq!(found.title, "Record 1");
        assert_eq!(found.tap, Some(json!({"id": 1})));
        let bare = found_of(&json!({"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Only card"}]}}));
        assert_eq!(bare.title, "Only card");
        assert!(bare.tap.is_none());
        let open = found_of(&json!({"preview": {"content": {"title": "T", "tap": {"type": "openUrl", "value": "x"}}}}));
        assert!(open.tap.is_none());
    }

    #[test]
    fn answers_read_as_the_pane_shows_them() {
        assert_eq!(results_of(Ok(InvokeAnswer::Done)), Results::Found(Vec::new()));
        assert_eq!(
            results_of(Ok(InvokeAnswer::Message("Hi".to_owned()))),
            Results::Said("Hi".to_owned())
        );
        assert_eq!(results_of(Err("x".to_owned())), Results::Said(NOT_REACHED.to_owned()));
        assert_eq!(
            results_of(Ok(InvokeAnswer::SignIn(Value::Null))),
            Results::Said(SIGN_IN.to_owned())
        );
        assert_eq!(
            results_of(Ok(InvokeAnswer::Error {
                code: None,
                message: "No".to_owned()
            })),
            Results::Said("No".to_owned())
        );
    }
}
