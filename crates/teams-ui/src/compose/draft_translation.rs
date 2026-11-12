//! The box's side of translating a draft ([`crate::draft_translate`]): Translate (the toolbar's button or Ctrl+Shift+Y)
//! sends what the box holds to the tool the settings name and puts the translation in its place; the language button
//! beside it shows the language every box translates into and opens the menu that changes it.
//!
//! - **One edit**: the translation replaces the draft as one edit, so Ctrl+Z brings the draft as written back.
//! - **Of the draft it read**: the draft changing while the tool works drops the translation, saying so; a result that
//!   comes after that is thrown away.
//! - **Progress and cancel**: a line over the box says what is being translated and by which tool, with Cancel; the
//!   button again or Escape cancels too, and cancelling ends the tool's process. The draft is never touched then.
//! - **Failure keeps the draft**: the line says why, with Try again; with no tool installed it says so and opens the
//!   setting.
//! - **Per box**: a chat's box keeps its run across navigation, and it can't land in any other box.

use std::mem;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement as _, KeyBinding,
    ParentElement as _, SharedString, Styled as _, Subscription, Window, actions, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{Disableable as _, Selectable as _, Sizable as _, Size};

use super::{Compose, TOOL};
use crate::draft_translate::menu::{self, LanguageMenu, LanguageMenuEvent};
use crate::draft_translate::{self, Limits, MAX_DRAFT, Outcome, Request, Translating};
use crate::editor::doc::Doc;
use crate::icons::{Glyph, icon_button, icon_button_with_key, selected_by};
use crate::local_ai::{Failure, Tools, Work};
use crate::message_correct::draft;
use crate::theme::{self, Palette};
use crate::trace::TraceLine;

actions!(
    compose,
    [
        /// Translates the draft into the language the settings name, or stops the translation running.
        TranslateDraft,
        /// Opens the settings' General page, where the translating tool is chosen.
        OpenTranslationSettings
    ]
);

/// The language button's longest label, in characters, before it is cut short.
const LANGUAGE_LABEL: usize = 12;

/// Binds the key of translating a draft.
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("ctrl-shift-y", TranslateDraft, Some(super::CONTEXT))]);
}

/// Where a box's translation stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DraftTranslationStatus {
    Idle,
    /// The tool is working on the draft.
    Running,
    /// The draft had no words to translate.
    NothingToTranslate,
    /// The draft changed, so its translation was dropped.
    DraftChanged,
    /// Why the translation failed; the draft is as it was.
    Failed(SharedString),
}

/// Where a box's translation stands, with what that needs.
#[derive(Default)]
enum State {
    #[default]
    Idle,
    Running {
        draft: Doc,
        language: SharedString,
        /// What the box calls the tool.
        tool: SharedString,
        _waiting: gpui::Task<()>,
        _translating: Translating,
    },
    Nothing,
    Changed,
    Failed {
        why: SharedString,
        /// No tool was found: the line offers the setting, and Try again looks for one again.
        missing: bool,
    },
}

/// The box's translation.
pub(super) struct DraftTranslation {
    state: State,
    /// The draft a line about nothing to translate, or a failure, is about: it goes once the draft changes.
    about: Option<Doc>,
    limits: Limits,
    /// The language menu, made the first time the button draws (it needs the window).
    menu: Option<(Entity<LanguageMenu>, Subscription)>,
}

impl DraftTranslation {
    pub(super) fn new() -> Self {
        Self {
            state: State::Idle,
            about: None,
            limits: Limits::default(),
            menu: None,
        }
    }
}

impl Compose {
    /// How long the tool may take to translate a draft (tests).
    pub fn set_draft_translation_limits(&mut self, limits: Limits) {
        self.draft_translation.limits = limits;
    }

    /// Where the box's translation stands.
    #[must_use]
    pub fn draft_translation_status(&self) -> DraftTranslationStatus {
        match &self.draft_translation.state {
            State::Idle => DraftTranslationStatus::Idle,
            State::Running { .. } => DraftTranslationStatus::Running,
            State::Nothing => DraftTranslationStatus::NothingToTranslate,
            State::Changed => DraftTranslationStatus::DraftChanged,
            State::Failed { why, .. } => DraftTranslationStatus::Failed(why.clone()),
        }
    }

    /// Whether the box's draft is being translated.
    #[must_use]
    pub fn translating_draft(&self) -> bool {
        matches!(self.draft_translation.state, State::Running { .. })
    }

    /// The language menu of the box, once drawn (tests, the tour).
    #[must_use]
    pub fn language_menu(&self) -> Option<Entity<LanguageMenu>> {
        self.draft_translation.menu.as_ref().map(|(menu, _)| menu.clone())
    }

    /// The button or its key: the draft translated, or the translation running stopped.
    pub fn translate_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.translating_draft() {
            self.cancel_draft_translation(window, cx);
        } else {
            self.start_draft_translation(window, cx);
        }
    }

    /// Whether the box holds what a translation can start from.
    fn can_translate_draft(&self, cx: &App) -> bool {
        !self.editor.read(cx).is_blank() && !self.translating() && self.starting.is_none()
    }

    fn start_draft_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.read_only.is_some() || !self.can_translate_draft(cx) {
            return;
        }
        // the translation replaces the draft a correction was of
        self.forget_correction();
        let doc = self.editor.read(cx).doc().clone();
        let encoded = draft::encode(&doc);
        self.draft_translation.about = Some(doc.clone());
        if !encoded.has_words() {
            self.draft_translation.state = State::Nothing;
            return cx.notify();
        }
        if encoded.text.len() > MAX_DRAFT {
            self.draft_translation.state = State::Failed {
                why: "This message is too long to translate.".into(),
                missing: false,
            };
            return cx.notify();
        }
        let settings = self.settings.read(cx).settings();
        let choice = settings.ai(Work::Draft).clone();
        let language = settings.message_translate_to.clone();
        let (backend, program) = match Tools::of(cx).pick(choice.tool) {
            Ok(picked) => picked,
            Err(failure) => return self.draft_translation_failed(&failure, cx),
        };
        let translating = Translating::start(
            Request {
                backend,
                program,
                model: choice.model.clone(),
                language: language.clone(),
                draft: encoded,
                limits: self.draft_translation.limits,
            },
            draft_translate::engine(cx),
        );
        let done = translating.done();
        let waiting = cx.spawn_in(window, async move |this, cx| {
            if let Ok(outcome) = done.recv().await {
                let _ = this.update_in(cx, |this, _, cx| this.draft_translation_came(outcome, cx));
            }
        });
        self.draft_translation.state = State::Running {
            draft: doc,
            language: language.into(),
            tool: backend.label_of(Work::Draft, &choice.model).into(),
            _waiting: waiting,
            _translating: translating,
        };
        self.trace
            .record(&TraceLine::new("compose", "translating draft").conversation(&self.conversation));
        cx.notify();
    }

    fn draft_translation_failed(&mut self, failure: &Failure, cx: &mut Context<Self>) {
        let missing = matches!(failure, Failure::NotInstalled(_));
        let hint = if missing {
            " Install one, or choose another tool in Settings under General."
        } else {
            ""
        };
        self.draft_translation.state = State::Failed {
            why: format!("Your message wasn't translated. {}{hint}", failure.message()).into(),
            missing,
        };
        cx.notify();
    }

    /// The tool's outcome: put in the box as one edit, unless the box no longer holds the draft it read.
    fn draft_translation_came(&mut self, outcome: Outcome, cx: &mut Context<Self>) {
        if !self.draft_translation_holds(cx) {
            return;
        }
        let State::Running { .. } = mem::take(&mut self.draft_translation.state) else {
            return;
        };
        match outcome {
            Err(failure) => self.draft_translation_failed(&failure, cx),
            Ok(translated) => {
                if translated != *self.editor.read(cx).doc() {
                    self.editor.update(cx, |editor, cx| editor.replace_doc(translated, cx));
                }
                self.trace
                    .record(&TraceLine::new("compose", "draft translated").conversation(&self.conversation));
                cx.notify();
            }
        }
    }

    /// Whether the draft being translated is still what the box holds; if not, the translation is dropped, told unless
    /// the box was emptied. A line about an earlier draft goes with it.
    fn draft_translation_holds(&mut self, cx: &mut Context<Self>) -> bool {
        let editor = self.editor.read(cx);
        let State::Running { draft, .. } = &self.draft_translation.state else {
            let stale = match &self.draft_translation.state {
                State::Changed => editor.is_blank(),
                State::Nothing | State::Failed { .. } => self.draft_translation.about.as_ref() != Some(editor.doc()),
                State::Idle | State::Running { .. } => false,
            };
            if stale {
                self.draft_translation.state = State::Idle;
                cx.notify();
            }
            return true;
        };
        if draft == editor.doc() {
            return true;
        }
        self.draft_translation.state = if editor.is_blank() { State::Idle } else { State::Changed };
        cx.notify();
        false
    }

    /// What the box holds may have changed: a translation of something else goes.
    pub(super) fn draft_translation_follows_draft(&mut self, cx: &mut Context<Self>) {
        self.draft_translation_holds(cx);
    }

    /// Whatever the translation was, it ends (the box sent, or was emptied); a running one's tool is stopped.
    pub(super) fn forget_draft_translation(&mut self) {
        self.draft_translation.state = State::Idle;
    }

    /// Whether Escape has a translation's line to end.
    pub(super) fn draft_translation_on_show(&self) -> bool {
        !matches!(self.draft_translation.state, State::Idle)
    }

    /// Cancel: the translation stops, its tool's process ends, the draft stays as it is and the box takes the keyboard
    /// back.
    pub fn cancel_draft_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.translating_draft() {
            self.trace
                .record(&TraceLine::new("compose", "draft translation cancelled").conversation(&self.conversation));
        }
        self.forget_draft_translation();
        self.focus(window, cx);
        cx.notify();
    }

    /// Try again after a failure, or after the draft changed; tools are looked for again if none was found.
    pub fn retry_draft_translation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.draft_translation.state, State::Failed { missing: true, .. }) {
            cx.set_global(Tools::detect());
        }
        self.draft_translation.state = State::Idle;
        self.start_draft_translation(window, cx);
    }

    /// Opens the language menu (tests, the tour).
    pub fn open_language_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let menu = self.language_menu_entity(window, cx);
        menu.update(cx, |menu, cx| menu.set_open(true, window, cx));
    }

    fn language_menu_entity(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<LanguageMenu> {
        if let Some((menu, _)) = &self.draft_translation.menu {
            return menu.clone();
        }
        let settings = self.settings.clone();
        let menu = cx.new(|cx| LanguageMenu::new(settings, "compose-language", window, cx));
        let chosen = cx.subscribe_in(&menu, window, |this: &mut Self, _, event, window, cx| match event {
            LanguageMenuEvent::Chosen | LanguageMenuEvent::Escaped => this.focus(window, cx),
        });
        self.draft_translation.menu = Some((menu.clone(), chosen));
        menu
    }

    // --- drawing ---

    /// Makes the language menu before the box draws, as its button needs it.
    pub(super) fn prepare_language_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.language_menu_entity(window, cx);
    }

    /// Translate and the language button, in the box's toolbar: Translate lit while it runs, when it stops it.
    pub(super) fn translate_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.translating_draft();
        let language = self.settings.read(cx).settings().message_translate_to.clone();
        let tip: SharedString = if busy {
            "Stop translating".into()
        } else {
            format!("Translate to {language}").into()
        };
        let translate = icon_button_with_key("compose-translate", Glyph::LANGUAGES, tip, &TranslateDraft)
            .with_size(Size::Size(px(TOOL)))
            .selected(busy)
            .disabled(!busy && !self.can_translate_draft(cx))
            .on_click(cx.listener(|this, _, window, cx| this.translate_draft(window, cx)));
        let face = Button::new("compose-language")
            .ghost()
            .compact()
            .with_size(Size::Small)
            .label(short(&language))
            .dropdown_caret(true)
            .tooltip(format!("Translation language: {language}"))
            .accessibility_label(format!("Translation language: {language}"));
        div()
            .flex()
            .flex_none()
            .items_center()
            .child(selected_by("compose-translate", translate))
            .children(self.language_menu().map(|menu| {
                selected_by(
                    "compose-language",
                    menu::popover(&menu, "compose-language-popover", gpui::Anchor::BottomRight, face, cx),
                )
            }))
            .into_any_element()
    }

    /// What the box says of its translation, over it.
    pub(super) fn draft_translation_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.draft_translation.state {
            State::Idle => None,
            State::Running { language, tool, .. } => Some(Self::translating_line(language, tool, palette, cx)),
            State::Nothing => Some(Self::nothing_line(palette, cx)),
            State::Changed => Some(Self::draft_changed_line(palette, cx)),
            State::Failed { why, missing } => Some(Self::translation_failed_line(why.clone(), *missing, cx)),
        }
    }

    fn translating_line(
        language: &SharedString,
        tool: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cancel = Button::new("compose-draft-translation-cancel")
            .label("Cancel")
            .ghost()
            .with_size(Size::Small)
            .on_click(cx.listener(|this, _, window, cx| this.cancel_draft_translation(window, cx)));
        notice("compose-draft-translating", spinner(cx))
            .text_color(rgb(palette.muted_foreground))
            .child(words(
                format!("Translating your message to {language} with {tool}…").into(),
            ))
            .child(selected_by("compose-draft-translation-cancel", cancel))
            .into_any_element()
    }

    fn nothing_line(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        notice(
            "compose-draft-translation-nothing",
            glyph(Glyph::INFO, palette.muted_foreground),
        )
        .text_color(rgb(palette.muted_foreground))
        .child(words("There are no words to translate.".into()))
        .child(Self::dismiss_translation("Dismiss", cx))
        .into_any_element()
    }

    fn draft_changed_line(palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        notice(
            "compose-draft-translation-changed",
            glyph(Glyph::INFO, palette.muted_foreground),
        )
        .text_color(rgb(palette.muted_foreground))
        .child(words("Your message changed, so it wasn't translated.".into()))
        .child(Self::retry_translation_button("Translate again", cx))
        .child(Self::dismiss_translation("Dismiss", cx))
        .into_any_element()
    }

    fn translation_failed_line(why: SharedString, missing: bool, cx: &mut Context<Self>) -> AnyElement {
        let settings = missing.then(|| {
            selected_by(
                "compose-draft-translation-settings",
                Button::new("compose-draft-translation-settings")
                    .label("Open settings")
                    .outline()
                    .with_size(Size::Small)
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenTranslationSettings), cx)),
            )
        });
        notice(
            "compose-draft-translation-failed",
            glyph(Glyph::LANGUAGES, theme::DANGER),
        )
        .border_l_2()
        .border_color(rgb(theme::DANGER))
        .child(words(why))
        .children(settings)
        .child(Self::retry_translation_button("Try again", cx))
        .child(Self::dismiss_translation("Dismiss", cx))
        .into_any_element()
    }

    fn retry_translation_button(label: &'static str, cx: &mut Context<Self>) -> AnyElement {
        selected_by(
            "compose-draft-translation-retry",
            Button::new("compose-draft-translation-retry")
                .label(label)
                .outline()
                .with_size(Size::Small)
                .on_click(cx.listener(|this, _, window, cx| this.retry_draft_translation(window, cx))),
        )
        .into_any_element()
    }

    fn dismiss_translation(tip: &'static str, cx: &mut Context<Self>) -> AnyElement {
        selected_by(
            "compose-draft-translation-dismiss",
            icon_button("compose-draft-translation-dismiss", Glyph::X, tip)
                .with_size(Size::XSmall)
                .on_click(cx.listener(|this, _, window, cx| this.cancel_draft_translation(window, cx))),
        )
        .into_any_element()
    }
}

/// `language` cut to fit the toolbar's button.
fn short(language: &str) -> String {
    if language.chars().count() <= LANGUAGE_LABEL {
        return language.to_owned();
    }
    let cut: String = language.chars().take(LANGUAGE_LABEL - 1).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// A line over the box, its mark first.
fn notice(selector: &'static str, mark: AnyElement) -> gpui::Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .px_2()
        .py_1()
        .rounded_md()
        .text_xs()
        .child(mark)
}

fn glyph(glyph: Glyph, tint: u32) -> AnyElement {
    glyph.icon().size(px(14.0)).text_color(rgb(tint)).into_any_element()
}

/// Teams' ring, turning while the tool works (still when animations are off).
fn spinner(cx: &App) -> AnyElement {
    use gpui::{Animation, AnimationExt as _, Transformation, percentage};
    let ring = || Glyph::LOADING.icon().size(px(14.0));
    if !crate::motion::on(cx) {
        return ring().into_any_element();
    }
    ring()
        .with_animation(
            "compose-draft-translating-ring",
            Animation::new(std::time::Duration::from_millis(900)).repeat(),
            move |icon, turn| icon.transform(Transformation::rotate(percentage(turn))),
        )
        .into_any_element()
}

fn words(text: SharedString) -> gpui::Div {
    div().flex_1().min_w_0().child(text)
}
