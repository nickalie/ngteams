//! The settings' one control for a local AI choice ([`AiChoice`]), the same wherever a setting picks the tool that
//! does some work on this computer (live captions' translation, recaps' summaries, chats' translation, drafts'
//! proofreading and translation): a box titled by the work, of
//!
//! - **the provider**: a dropdown of Automatic (the first provider in the user's order that can work) and each AI
//!   provider that is on ([`crate::local_ai::providers`]); under it what goes to the provider, or why it cannot work;
//! - **the model**: a dropdown of the one recommended for the work, the provider's own default, every model the
//!   provider reports (a tool's [`Catalog`], an API's [`ApiModels`]) and "Other model…";
//! - **the model's name**, while another model is being typed or one the tool didn't list is set: any name the tool
//!   takes, used on Enter (or when the field is left).
//!
//! A translation adds its language's row on top ([`crate::draft_translate::menu`]), each its own setting:
//!
//! - a draft's ("Translate my messages into"): the menu the compose box's row "Translation language" opens, its button
//!   `message-language` and its rows `message-language-<language>`;
//! - a chat's ("Translate messages into this language", Teams' words): the language a message's "Translate" shows it
//!   in, its button `reading-language` and its rows `reading-language-<language>`.
//!
//! Changing the provider sets its recommended model again: a model of one provider means nothing to the other. Test
//! selectors: the provider's dropdown `<setting>` and its rows `<setting>-<provider id>` (`caption-translator-claude`,
//! `caption-translator-automatic`), the model's dropdown `<setting>-model` and
//! its rows `<setting>-model-<id>` (`recommended`, `default`, `other`, or the model's), the name's field
//! `<setting>-model-name`.

use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div, px,
};
use gpui_component::input::{Input, InputEvent, InputState};

use super::{Pick, group, picker, row_with};
use crate::draft_translate::menu::{self, LanguageMenu, Translation};
use crate::local_ai::providers::{ApiModels, Picked};
use crate::local_ai::{
    AiChoice, Backend, Catalog, Failure, Listed, Model, Provider, Providers, ToolChoice, Tools, Work,
};
use crate::settings::SettingsModel;
use crate::theme;

/// What the picker of a work says, and its test selectors.
struct Words {
    title: &'static str,
    tool: &'static str,
    /// What goes to the tool, and when: [`ON_THIS_COMPUTER`] says where.
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
            sends: "When you translate live captions, their text, the speakers' names, the call's title and the names of \
                    the people in it go to this tool on your computer and its provider.",
            install: "translate live captions",
            model: "A fast model keeps translations up with the call.",
            selector: "caption-translator",
            model_selector: "caption-translator-model",
            name_selector: "caption-translator-model-name",
        },
        Work::Summary => Words {
            title: "Meeting recaps",
            tool: "Summarize recaps with",
            sends: "When you ask for a recap's AI summary, its transcript, the meeting's title and date, and the names \
                    of the people who spoke go to this tool on your computer and its provider.",
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

/// Where a tool's note says that the work goes.
const ON_THIS_COMPUTER: &str = "this tool on your computer and its provider";

/// The note of an API provider named `name`: the tool's note for `work`, with the provider in the tool's place, so
/// that the two always name the same data.
fn api_note(work: Work, name: &str) -> String {
    let sends = words(work)
        .sends
        .replacen(ON_THIS_COMPUTER, &format!("{name} with your API key"), 1);
    format!("{sends} The model runs on its server.")
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
    _observing: [Subscription; 5],
    /// The name field's Enter and leaving, while it is there.
    name_events: Option<Subscription>,
    /// A translation's language menu, made the first time it shows (it needs the window).
    language: Option<Entity<LanguageMenu>>,
}

impl AiPicker {
    pub fn new(model: Entity<SettingsModel>, work: Work, cx: &mut Context<Self>) -> Self {
        let observing = [
            cx.observe(&model, |_, _, cx| cx.notify()),
            cx.observe_global::<Catalog>(|_, cx| cx.notify()),
            cx.observe_global::<Tools>(|_, cx| cx.notify()),
            cx.observe_global::<Providers>(|_, cx| cx.notify()),
            cx.observe_global::<ApiModels>(|_, cx| cx.notify()),
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

    /// The provider chosen, with its recommended model (unless it was chosen already).
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

    /// The language of `translation`, the same in every box or chat: a button opening its menu.
    fn language_row(
        &mut self,
        translation: Translation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let palette = theme::current(cx);
        let settings = self.model.clone();
        let (selector, popover, note) = match translation {
            Translation::Draft => (
                "message-language",
                "message-language-popover",
                "The language Translate in the message box writes your message in, the same in every chat. Pick one \
                 or type any other.",
            ),
            Translation::Reading => (
                "reading-language",
                "reading-language-popover",
                "The language a message shows in when you translate it in a chat. Pick one or type any other.",
            ),
        };
        let language = self
            .language
            .get_or_insert_with(|| cx.new(|cx| LanguageMenu::new(settings, translation, selector, window, cx)))
            .clone();
        let chosen = translation.language(self.model.read(cx).settings()).to_owned();
        let face = gpui_component::button::Button::new(selector)
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
            translation.title().into(),
            Some(note.into()),
            div().debug_selector(move || selector.to_owned()).child(menu::popover(
                &language,
                popover,
                gpui::Anchor::TopRight,
                face,
                cx,
            )),
            palette,
        )
    }

    /// The provider's row: its dropdown, and what goes to it (or why it cannot work).
    fn tool_row(
        &self,
        choice: &AiChoice,
        list: &[Provider],
        picked: &Result<Picked, Failure>,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let palette = theme::current(cx);
        let words = words(self.work);
        let tools = cx.try_global::<Tools>().cloned().unwrap_or_default();
        let pick = |tool: ToolChoice, label: String| Pick {
            key: tool.key().to_owned(),
            label: label.into(),
            status: None,
            chosen: tool == choice.tool,
            apply: Self::act(cx, move |this, cx| this.choose_tool(tool.clone(), cx)),
        };
        let mut picks = vec![pick(ToolChoice::Automatic, "Automatic".to_owned())];
        for provider in list {
            let tool = ToolChoice::Only(provider.id.clone());
            if !provider.enabled && tool != choice.tool {
                continue;
            }
            let label = if !provider.enabled {
                format!("{} (off)", provider.name)
            } else if provider.ready(&tools) {
                provider.name.clone()
            } else if provider.kind.is_cli() {
                format!("{} (not installed)", provider.name)
            } else {
                format!("{} (no key)", provider.name)
            };
            picks.push(pick(tool, label));
        }
        if let ToolChoice::Only(id) = &choice.tool
            && !list.iter().any(|provider| provider.id == *id)
        {
            let label = Backend::from_key(id).filter(|backend| backend.is_cli()).map_or_else(
                || "Removed provider".to_owned(),
                |backend| format!("{} (not installed)", backend.name()),
            );
            picks.push(pick(choice.tool.clone(), label));
        }
        let note: SharedString = match picked {
            Ok(picked) if !picked.backend.is_cli() => api_note(self.work, &picked.name).into(),
            Ok(_) => words.sends.into(),
            Err(failure @ Failure::NotInstalled(None)) => format!(
                "{} Install or turn on one of the two, or turn on or add an API provider under AI providers, to {}.",
                failure.message(),
                words.install
            )
            .into(),
            Err(failure @ Failure::NotInstalled(Some(_))) => {
                format!("{} Install it to {}.", failure.message(), words.install).into()
            }
            Err(failure) => failure.message().into(),
        };
        row_with(
            words.tool.into(),
            Some(note),
            picker(words.selector, picks, palette),
            palette,
        )
    }

    /// The model's dropdown: the recommended model, the provider's default, the models it lists (and one set that it
    /// doesn't), then "Other model…".
    fn model_picks(
        &self,
        choice: &AiChoice,
        picked: Option<&Picked>,
        listed: &[Listed],
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
        let chosen = matches!(choice.tool, ToolChoice::Only(_));
        // the provider chosen, or the one Automatic comes to
        let recommended = picked.map_or_else(
            || "Recommended".to_owned(),
            |picked| match picked.backend.recommended(work) {
                Some(_) if picked.backend.is_cli() => format!("Recommended ({})", picked.backend.label(work)),
                Some(model) => format!("Recommended ({model})"),
                None => format!("Recommended ({}'s model)", picked.name),
            },
        );
        let own = picked.filter(|_| chosen).map_or_else(
            || "The provider's default".to_owned(),
            |picked| format!("{}'s default", picked.name),
        );
        let mut picks = vec![
            pick("recommended", recommended, Model::Recommended),
            pick("default", own, Model::ToolDefault),
        ];
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
        if chosen {
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
        provider: &str,
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
            format!("Any model {provider} takes, by an alias or its full name. Press Enter to use it.").into()
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

impl AiPicker {
    /// The models the provider chosen lists, and whether they are still being looked for.
    fn listed(
        choice: &AiChoice,
        list: &[Provider],
        picked: Option<&Picked>,
        cx: &mut Context<Self>,
    ) -> (Vec<Listed>, bool) {
        // on every render, as before the API providers: the tools' listing starts on first use, so a tool found
        // later (Try again) lists its models too
        let catalog = Catalog::of(cx);
        let ToolChoice::Only(id) = &choice.tool else {
            return (Vec::new(), false);
        };
        let Some(picked) = picked else {
            return (Vec::new(), false);
        };
        if picked.backend.is_cli() {
            return (catalog.models(picked.backend).to_vec(), !catalog.is_listed());
        }
        let Some(provider) = list.iter().find(|provider| provider.id == *id) else {
            return (Vec::new(), false);
        };
        match ApiModels::of(provider, cx) {
            Some(Ok(listed)) => (listed, false),
            Some(Err(_)) => (Vec::new(), false),
            None => (Vec::new(), true),
        }
    }
}

impl Render for AiPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let words = words(self.work);
        let choice = self.choice(cx);
        let list = Providers::of(cx);
        let picked = crate::local_ai::pick(&choice.tool, cx);
        let (listed, looking) = Self::listed(&choice, &list, picked.as_ref().ok(), cx);
        let mut rows = Vec::new();
        match self.work {
            Work::Draft => rows.push(self.language_row(Translation::Draft, window, cx)),
            Work::Chat => rows.push(self.language_row(Translation::Reading, window, cx)),
            Work::Captions | Work::Summary | Work::Correction => {}
        }
        rows.push(self.tool_row(&choice, &list, &picked, cx));
        let picks = self.model_picks(&choice, picked.as_ref().ok(), &listed, cx);
        let model_note: SharedString = match (&picked, looking) {
            (Ok(picked), true) => format!("Looking for the models {} offers\u{2026}", picked.name).into(),
            _ => words.model.into(),
        };
        rows.push(row_with(
            "Model".into(),
            Some(model_note),
            picker(words.model_selector, picks, palette),
            palette,
        ));
        if let ToolChoice::Only(id) = &choice.tool {
            let unlisted = matches!(&choice.model, Model::Named(name)
                if !listed.iter().any(|listed| listed.id == *name));
            if self.typing || unlisted {
                let name = list
                    .iter()
                    .find(|provider| provider.id == *id)
                    .map_or_else(|| id.clone(), |provider| provider.name.clone());
                rows.push(self.name_row(&choice, &name, window, cx));
            }
        }
        group(Some(words.title), rows, palette)
    }
}

#[cfg(test)]
#[path = "../../tests/support/ai_picker_units.rs"]
mod tests;
