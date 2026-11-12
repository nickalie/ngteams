//! The settings' one control for a local AI choice ([`AiChoice`]), the same wherever a setting picks the tool that
//! does some work on this computer (live captions' translation, recaps' summaries, chats' translation, drafts'
//! proofreading and translation): a box titled by the work, of
//!
//! - **the tool**: Automatic (Claude Code when it is installed, else Codex), Claude or Codex, side by side; under it
//!   what goes to the tool, or that the tool isn't installed;
//! - **the model**: a dropdown of the one recommended for the work, the tool's own default, every model the tool
//!   reports ([`Catalog`]) and "Other model…";
//! - **the model's name**, while another model is being typed or one the tool didn't list is set: any name the tool
//!   takes, used on Enter (or when the field is left).
//!
//! A draft's translation adds its language's row on top: the menu the compose box's language button opens
//! ([`crate::draft_translate::menu`]), its button `message-language` and its rows `message-language-<language>`.
//!
//! Changing the tool sets its recommended model again: a model of one tool means nothing to the other. Test selectors:
//! the tool's segments `<setting>-<tool>` (`caption-translator-claude`), the model's dropdown `<setting>-model` and
//! its rows `<setting>-model-<id>` (`recommended`, `default`, `other`, or the model's), the name's field
//! `<setting>-model-name`.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use super::{Pick, group, picker, row_with, segmented};
use crate::draft_translate::menu::{self, LanguageMenu};
use crate::local_ai::{AiChoice, Backend, Catalog, Failure, Model, ToolChoice, Tools, Work};
use crate::settings::SettingsModel;
use crate::theme;

/// What the picker of a work says, and its test selectors.
struct Words {
    title: &'static str,
    tool: &'static str,
    /// What goes to the tool, and when.
    sends: &'static str,
    /// What installing a tool lets the user do.
    install: &'static str,
    /// Why its recommended model is the one.
    model: &'static str,
    selector: &'static str,
    model_selector: &'static str,
    name_selector: &'static str,
}

fn words(work: Work) -> Words {
    match work {
        Work::Captions => Words {
            title: "Captions",
            tool: "Translate captions with",
            sends: "When you translate live captions, their text goes to this tool on your computer and its provider.",
            install: "translate live captions",
            model: "A fast model keeps translations up with the call.",
            selector: "caption-translator",
            model_selector: "caption-translator-model",
            name_selector: "caption-translator-model-name",
        },
        Work::Summary => Words {
            title: "Meeting recaps",
            tool: "Summarize recaps with",
            sends: "When you ask for a recap's AI summary, its transcript goes to this tool on your computer and its \
                    provider.",
            install: "summarize meeting recaps",
            model: "A stronger model writes a better summary of a long meeting.",
            selector: "recap-summarizer",
            model_selector: "recap-summarizer-model",
            name_selector: "recap-summarizer-model-name",
        },
        Work::Chat => Words {
            title: "Chat translation",
            tool: "Translate chats with",
            sends: crate::chat_translate::SETTINGS_NOTE,
            install: "translate chats",
            model: "A fast model keeps your messages from waiting; each request carries the chat, so terms and tone \
                    stay consistent.",
            selector: "chat-translator",
            model_selector: "chat-translator-model",
            name_selector: "chat-translator-model-name",
        },
        Work::Correction => Words {
            title: "Message correction",
            tool: "Correct messages with",
            sends: crate::message_correct::SETTINGS_NOTE,
            install: "correct your messages",
            model: "A stronger model finds the word that fits; you wait only when you ask for a correction.",
            selector: "message-corrector",
            model_selector: "message-corrector-model",
            name_selector: "message-corrector-model-name",
        },
        Work::Draft => Words {
            title: "Message translation",
            tool: "Translate messages with",
            sends: crate::draft_translate::SETTINGS_NOTE,
            install: "translate your messages",
            model: "A stronger model writes the message as a native speaker would; you wait only when you translate.",
            selector: "message-translator",
            model_selector: "message-translator-model",
            name_selector: "message-translator-model-name",
        },
    }
}

/// The control of one work's choice (see the module docs).
pub struct AiPicker {
    model: Entity<SettingsModel>,
    work: Work,
    /// The model's name field, made the first time it shows (it needs the window).
    name: Option<Entity<InputState>>,
    /// "Other model…" was chosen: the name field shows until a name is used or another model is chosen.
    typing: bool,
    /// The field is to be emptied and focused when it next draws.
    focus_name: bool,
    /// The name typed couldn't be used.
    bad_name: bool,
    _observing: [Subscription; 3],
    /// The name field's Enter and leaving, while it is there.
    name_events: Option<Subscription>,
    /// A draft's translation's language menu, made the first time it shows (it needs the window).
    language: Option<Entity<LanguageMenu>>,
}

impl AiPicker {
    pub fn new(model: Entity<SettingsModel>, work: Work, cx: &mut Context<Self>) -> Self {
        let observing = [
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe_global::<Catalog>(|_, cx| cx.notify()),
            cx.observe_global::<Tools>(|_, cx| cx.notify()),
        ];
        Self {
            model,
            work,
            name: None,
            typing: false,
            focus_name: false,
            bad_name: false,
            _observing: observing,
            name_events: None,
            language: None,
        }
    }

    /// The choice the settings hold.
    #[must_use]
    pub fn choice(&self, cx: &gpui::App) -> AiChoice {
        self.model.read(cx).settings().ai(self.work).clone()
    }

    /// Whether the model's name field shows.
    #[must_use]
    pub fn is_typing(&self) -> bool {
        self.typing
    }

    fn set(&self, choice: AiChoice, cx: &mut Context<Self>) {
        let work = self.work;
        self.model.update(cx, |model, cx| {
            model.change(|settings| settings.set_ai(work, choice), cx);
        });
    }

    /// The tool chosen, with its recommended model (unless it was chosen already).
    pub fn choose_tool(&mut self, tool: ToolChoice, cx: &mut Context<Self>) {
        if self.choice(cx).tool != tool {
            self.set(AiChoice::of(tool), cx);
        }
        self.typing = false;
        self.bad_name = false;
        cx.notify();
    }

    /// The model chosen.
    pub fn choose_model(&mut self, model: Model, cx: &mut Context<Self>) {
        let tool = self.choice(cx).tool;
        self.set(AiChoice { tool, model }, cx);
        self.typing = false;
        self.bad_name = false;
        cx.notify();
    }

    /// "Other model…": the name field shown, empty and focused.
    pub fn type_model(&mut self, cx: &mut Context<Self>) {
        self.typing = true;
        self.focus_name = true;
        self.bad_name = false;
        cx.notify();
    }

    /// Uses the name typed, when there is one: as the model, or said why it can't be.
    pub fn use_name(&mut self, cx: &mut Context<Self>) {
        let Some(name) = &self.name else { return };
        let typed = name.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        if let Some(model) = Model::from_key(&typed) {
            self.choose_model(model, cx);
        } else {
            self.bad_name = true;
            cx.notify();
        }
    }

    /// What choosing a pick does: `act` on this picker.
    fn act(cx: &Context<Self>, act: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> super::Apply {
        let this = cx.entity().downgrade();
        Rc::new(move |cx| {
            let _ = this.update(cx, |this, cx| act(this, cx));
        })
    }

    /// The language a draft is translated into, the same in every box: a button opening the menu the compose box's
    /// language button opens.
    fn language_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let palette = theme::current(cx);
        let settings = self.model.clone();
        let language = self
            .language
            .get_or_insert_with(|| cx.new(|cx| LanguageMenu::new(settings, "message-language", window, cx)))
            .clone();
        let chosen = self.model.read(cx).settings().message_translate_to.clone();
        let face = gpui_component::button::Button::new("message-language")
            .outline()
            .min_w(px(220.0))
            .max_w(px(320.0))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(chosen),
                    )
                    .child(
                        crate::icons::Glyph::CHEVRON_DOWN
                            .icon()
                            .size(px(14.0))
                            .text_color(gpui::rgb(palette.muted_foreground)),
                    ),
            );
        row_with(
            "Translate messages into".into(),
            Some(
                "The language Translate in the message box writes in, the same in every chat. Pick one or type any \
                  other."
                    .into(),
            ),
            div()
                .debug_selector(|| "message-language".to_owned())
                .child(menu::popover(
                    &language,
                    "message-language-popover",
                    gpui::Anchor::TopRight,
                    face,
                    cx,
                )),
            palette,
        )
    }

    /// The tool's row: its segments, and what goes to it (or that it isn't installed).
    fn tool_row(
        &self,
        choice: &AiChoice,
        picked: &Result<(Backend, std::path::PathBuf), Failure>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let palette = theme::current(cx);
        let words = words(self.work);
        let picks = ToolChoice::ALL
            .into_iter()
            .map(|tool| Pick {
                key: tool.key().to_owned(),
                label: tool.label().into(),
                status: None,
                chosen: tool == choice.tool,
                apply: Self::act(cx, move |this, cx| this.choose_tool(tool, cx)),
            })
            .collect();
        let note: SharedString = match picked {
            Ok(_) => words.sends.into(),
            Err(Failure::NotInstalled(None)) => format!(
                "Neither Claude Code (claude) nor the Codex CLI (codex) is installed. Install one to {}.",
                words.install
            )
            .into(),
            Err(failure) => format!("{} Install it to {}.", failure.message(), words.install).into(),
        };
        row_with(
            words.tool.into(),
            Some(note),
            div()
                .debug_selector(move || words.selector.to_owned())
                .child(segmented(words.selector, picks, palette)),
            palette,
        )
    }

    /// The model's dropdown: the recommended model, the tool's default, the models the tool lists (and one set that it
    /// doesn't), then "Other model…".
    fn model_picks(
        &self,
        choice: &AiChoice,
        picked: Option<Backend>,
        catalog: &Catalog,
        cx: &Context<Self>,
    ) -> Vec<Pick> {
        let work = self.work;
        let pick = |key: &str, label: String, model: Model| Pick {
            key: key.to_owned(),
            label: label.into(),
            status: None,
            chosen: choice.model == model,
            apply: Self::act(cx, move |this, cx| this.choose_model(model.clone(), cx)),
        };
        let backend = match choice.tool {
            ToolChoice::Only(backend) => Some(backend),
            ToolChoice::Automatic => None,
        };
        // the tool chosen, or the one Automatic comes to
        let recommended = backend.or(picked).map_or_else(
            || "Recommended".to_owned(),
            |backend| format!("Recommended ({})", backend.label(work)),
        );
        let own = backend.map_or_else(
            || "The tool's default".to_owned(),
            |backend| format!("{}'s default", backend.name()),
        );
        let mut picks = vec![
            pick("recommended", recommended, Model::Recommended),
            pick("default", own, Model::ToolDefault),
        ];
        let listed = backend.map_or(&[][..], |backend| catalog.models(backend));
        picks.extend(
            listed
                .iter()
                .map(|listed| pick(&listed.id, listed.label(), Model::Named(listed.id.clone()))),
        );
        if let Model::Named(name) = &choice.model
            && !listed.iter().any(|listed| listed.id == *name)
        {
            picks.push(pick(name, name.clone(), choice.model.clone()));
        }
        if backend.is_some() {
            picks.push(Pick {
                key: "other".to_owned(),
                label: "Other model\u{2026}".into(),
                status: None,
                chosen: false,
                apply: Self::act(cx, Self::type_model),
            });
        }
        picks
    }

    /// The model's name row: its field, made (holding the model set) the first time it shows.
    fn name_row(
        &mut self,
        choice: &AiChoice,
        backend: Backend,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = theme::current(cx);
        let words = words(self.work);
        let name = if let Some(name) = &self.name {
            name.clone()
        } else {
            let held = match &choice.model {
                Model::Named(name) => name.clone(),
                Model::Recommended | Model::ToolDefault => String::new(),
            };
            let name = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Model name")
                    .default_value(held)
            });
            self.name_events = Some(
                cx.subscribe_in(&name, window, |this: &mut Self, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.use_name(cx);
                    }
                }),
            );
            self.name = Some(name.clone());
            name
        };
        if std::mem::take(&mut self.focus_name) {
            name.update(cx, |name, cx| {
                name.set_value("", window, cx);
                name.focus(window, cx);
            });
        }
        let note: SharedString = if self.bad_name {
            "A model's name has no spaces and doesn't start with a dash.".into()
        } else {
            format!(
                "Any model {} takes, by an alias or its full name. Press Enter to use it.",
                backend.name()
            )
            .into()
        };
        row_with(
            "Model name".into(),
            Some(note),
            div()
                .debug_selector(move || words.name_selector.to_owned())
                .w(px(220.0))
                .child(Input::new(&name)),
            palette,
        )
    }
}

impl Render for AiPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let words = words(self.work);
        let choice = self.choice(cx);
        let tools = Tools::of(cx);
        let catalog = Catalog::of(cx);
        let picked = tools.pick(choice.tool);
        let backend = picked.as_ref().ok().map(|(backend, _)| *backend);
        let mut rows = Vec::new();
        if self.work == Work::Draft {
            rows.push(self.language_row(window, cx));
        }
        rows.push(self.tool_row(&choice, &picked, cx));
        let picks = self.model_picks(&choice, backend, &catalog, cx);
        let model_note: SharedString = match choice.tool {
            ToolChoice::Only(backend) if tools.pick(choice.tool).is_ok() && !catalog.is_listed() => {
                format!("Looking for the models {} offers\u{2026}", backend.name()).into()
            }
            _ => words.model.into(),
        };
        rows.push(row_with(
            "Model".into(),
            Some(model_note),
            picker(words.model_selector, picks, palette),
            palette,
        ));
        if let ToolChoice::Only(backend) = choice.tool {
            let unlisted = matches!(&choice.model, Model::Named(name)
                if !catalog.models(backend).iter().any(|listed| listed.id == *name));
            if self.typing || unlisted {
                rows.push(self.name_row(&choice, backend, window, cx));
            }
        }
        group(Some(words.title), rows, palette)
    }
}
