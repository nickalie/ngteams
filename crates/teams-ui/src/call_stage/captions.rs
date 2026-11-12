//! Teams' Language and speech menu and ephemeral caption pane, with the captions translated on this computer when the
//! user picked a language in its settings' "Translate to" ([`crate::translate`]): a line shows its translation alone,
//! as Teams shows translated captions, and its text as spoken (dimmed) until the translation comes.
use super::*;
use crate::translate::{self, CaptionTranslation, Shown, Status};
use teams_core::CaptionPhase;
use teams_proto::captions::SPOKEN_LANGUAGES;

pub(super) fn more(
    menu: PopupMenu,
    stage: WeakEntity<CallStage>,
    visible: bool,
    available: bool,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if !available {
        return menu;
    }
    menu.submenu("Language and speech", window, cx, move |menu, _, _| {
        let stage = stage.clone();
        menu.item(
            PopupMenuItem::element(move |_, _| {
                div()
                    .debug_selector(|| "call-toggle-captions".to_owned())
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(if visible {
                        "Hide live captions"
                    } else {
                        "Show live captions"
                    })
                    .child(div().text_xs().child("Alt+Shift+C"))
            })
            .checked(visible)
            .on_click(move |_, _, cx| {
                if let Some(stage) = stage.upgrade() {
                    stage.update(cx, |stage, cx| stage.ask(Ask::ToggleCaptions, cx));
                }
            }),
        )
    })
}

impl CallStage {
    pub(super) fn caption_pane(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.calls.read(cx).captions_shown() {
            return None;
        }
        let state = self.calls.read(cx).captions().clone();
        let language_label = language_label(&state.spoken_language);
        let header = self.caption_header(palette, &state.spoken_language, cx);
        let status = Self::caption_status(&state, language_label, cx);
        let status_now = self.translation.read(cx).status().clone();
        let translation_failed = Self::translation_notice(&status_now, palette, cx);
        let lines = self.caption_lines(&state.lines, palette, cx);
        let transcript = div()
            .id("call-caption-text")
            .debug_selector(|| "call-caption-text".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.caption_scroll)
            .children(lines);
        let confirmation = self.caption_confirmation(palette, cx);
        let language_error = (state.phase == CaptionPhase::On)
            .then_some(state.error)
            .flatten()
            .map(|error| {
                div()
                    .px_4()
                    .py_1()
                    .text_sm()
                    .text_color(rgb(theme::DANGER))
                    .child(error)
            });
        Some(
            div()
                .debug_selector(|| "call-captions".to_owned())
                // shrinks before the stage goes below MIN_STAGE_HEIGHT in a short window
                .flex_initial()
                .h(px(if confirmation.is_some() { 380.0 } else { 244.0 }))
                .min_h(px(150.0))
                .flex()
                .flex_col()
                .bg(rgb(palette.surface))
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(header)
                .children(translation_failed.filter(|_| state.phase == CaptionPhase::On))
                .children(status)
                .when(state.phase != CaptionPhase::Failed && !state.lines.is_empty(), |view| {
                    view.child(transcript)
                })
                .children(language_error)
                .children(confirmation)
                .into_any_element(),
        )
    }
    /// Each line with text: its speaker over its text, translated when there is a translation.
    fn caption_lines(&self, lines: &[teams_core::Caption], palette: Palette, cx: &App) -> Vec<AnyElement> {
        let translation = self.translation.read(cx);
        lines
            .iter()
            .filter(|line| !line.text.is_empty())
            .map(|line| {
                let (text, waiting) = match translation.shown(line) {
                    Shown::Translated(text) | Shown::Spoken(text) => (text, false),
                    Shown::Waiting(text) => (text, true),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .px_4()
                    .py_1p5()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(SharedString::from(
                                line.display_name.clone().unwrap_or_else(|| "Speaker".into()),
                            )),
                    )
                    .child(
                        div()
                            .text_size(px(self.caption_font))
                            .when(waiting, |text| text.text_color(rgb(palette.muted_foreground)))
                            .child(SharedString::from(text.to_owned())),
                    )
                    .into_any_element()
            })
            .collect()
    }
    fn caption_status(
        state: &teams_core::CallCaptions,
        language_label: &str,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Div> {
        let message = if state.phase == CaptionPhase::Starting {
            Some("Starting live captions…".to_owned())
        } else if state.phase == CaptionPhase::Failed {
            state.error.clone()
        } else if state.lines.is_empty() {
            Some(format!(
                "Captions will be shown in {}.",
                if language_label == "Meeting spoken language" {
                    "the meeting's spoken language"
                } else {
                    language_label
                }
            ))
        } else {
            None
        };
        message.map(|message| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(SharedString::from(message))
                .when(state.phase == CaptionPhase::Failed, |view| {
                    view.child(
                        Button::new("call-caption-retry")
                            .label("Try again")
                            .outline()
                            .on_click(cx.listener(|this, _, _, cx| this.ask(Ask::Captions(true), cx))),
                    )
                })
        })
    }

    fn caption_settings(&self, current: &str, cx: &Context<Self>) -> AnyElement {
        let stage = cx.weak_entity();
        let current = current.to_owned();
        let language_label = language_label(&current);
        let font = self.caption_font;
        let top = self.caption_top;
        let translate_to = self.settings.read(cx).settings().caption_translate_to;
        let settings_model = self.settings.clone();
        let settings = icon_button("call-caption-settings", Glyph::SETTINGS, "Caption settings")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, window, cx| {
                let current = current.clone();
                let stage_language = stage.clone();
                let stage_font = stage.clone();
                let stage_position = stage.clone();
                let settings_model = settings_model.clone();
                menu.submenu(
                    format!("Meeting spoken language: {language_label}"),
                    window,
                    cx,
                    move |mut menu, _, _| {
                        for &(code, label) in SPOKEN_LANGUAGES {
                            menu = menu.item(language_item(stage_language.clone(), code, label, &current));
                        }
                        menu.scrollable(true).max_h(px(360.0))
                    },
                )
                .submenu(
                    format!(
                        "Translate to: {}",
                        translate_to
                            .and_then(translate::language)
                            .map_or("Off", |(_, name)| name)
                    ),
                    window,
                    cx,
                    move |mut menu, _, _| {
                        menu = menu.item(translate_item(settings_model.clone(), None, "Off", translate_to));
                        for &(code, name) in translate::TARGET_LANGUAGES {
                            menu = menu.item(translate_item(settings_model.clone(), Some(code), name, translate_to));
                        }
                        menu.scrollable(true).max_h(px(360.0))
                    },
                )
                .submenu("Caption styles", window, cx, move |mut menu, _, _| {
                    for (size, label) in [(16.0, "Small text"), (18.0, "Medium text"), (22.0, "Large text")] {
                        let stage = stage_font.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| div().child(label))
                                .checked((size - font).abs() < f32::EPSILON)
                                .on_click(move |_, _, cx| pick_font(&stage, size, cx)),
                        );
                    }
                    menu
                })
                .submenu("Position", window, cx, move |mut menu, _, _| {
                    for (at_top, label) in [(false, "At bottom"), (true, "At top")] {
                        let stage = stage_position.clone();
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| div().child(label))
                                .checked(at_top == top)
                                .on_click(move |_, _, cx| pick_position(&stage, at_top, cx)),
                        );
                    }
                    menu
                })
            });
        selected_by("call-caption-settings", settings).into_any_element()
    }
    fn caption_header(&self, palette: Palette, current: &str, cx: &mut Context<Self>) -> gpui::Div {
        let settings = self.caption_settings(current, cx);
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_4()
            .py_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Live captions")
                    .children(translated_into(self.translation.read(cx).status())),
            )
            .child(
                div().flex().items_center().gap_1().child(settings).child(selected_by(
                    "call-caption-hide",
                    icon_button("call-caption-hide", Glyph::X, "Hide live captions")
                        .on_click(cx.listener(|this, _, _, cx| this.ask(Ask::Captions(false), cx))),
                )),
            )
    }
    fn caption_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        self.caption_language.as_ref().map(|code| {
            let label = SPOKEN_LANGUAGES
                .iter()
                .find(|(language, _)| *language == code)
                .map_or(code.as_str(), |(_, label)| *label);
            div()
                .debug_selector(|| "call-caption-language-confirm".to_owned())
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .border_t_1()
                .border_color(rgb(palette.border))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Is this the language that everyone is speaking?"),
                )
                .child(SharedString::from(format!(
                    "The spoken language that everyone is speaking will change to {label} for this meeting."
                )))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("call-caption-language-cancel")
                                .label("Cancel")
                                .outline()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.caption_language = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("call-caption-language-update")
                                .label("Update")
                                .primary()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(language) = this.caption_language.take() {
                                        this.ask(Ask::CaptionLanguage(language), cx);
                                    }
                                    cx.notify();
                                })),
                        ),
                )
        })
    }
}

impl CallStage {
    /// Translating stopped: why, that the captions show as spoken, and Try again.
    fn translation_notice(status: &Status, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let Status::Failed { failure, language } = status else {
            return None;
        };
        let retry = Button::new("call-caption-translate-retry")
            .label("Try again")
            .outline()
            .small()
            .on_click(cx.listener(|this, _, _, cx| {
                this.translation.update(cx, CaptionTranslation::retry);
            }));
        Some(
            div()
                .debug_selector(|| "call-caption-translate-failed".to_owned())
                .mx_4()
                .my_1()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .rounded_lg()
                .border_1()
                .border_color(rgb(theme::DANGER))
                .text_sm()
                .child(Glyph::CIRCLE_ALERT.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(SharedString::from(format!(
                            "Captions can't be translated into {language}. {}",
                            failure.message()
                        )))
                        .child(
                            div()
                                .text_color(rgb(palette.muted_foreground))
                                .child("Showing captions as spoken."),
                        ),
                )
                .child(selected_by("call-caption-translate-retry", retry)),
        )
    }
}

/// The header's note of the translation: the language and the tool doing it.
fn translated_into(status: &Status) -> Option<gpui::Div> {
    let Status::Translating { language, by, .. } = status else {
        return None;
    };
    Some(
        div()
            .debug_selector(|| "call-caption-translated".to_owned())
            .text_xs()
            .overflow_hidden()
            .text_ellipsis()
            .whitespace_nowrap()
            .child(SharedString::from(format!("Translated into {language} by {by}"))),
    )
}

/// A row of "Translate to": a language, or Off (`None`).
fn translate_item(
    settings: Entity<SettingsModel>,
    code: Option<&'static str>,
    label: &'static str,
    current: Option<&'static str>,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("call-caption-translate-{}", code.unwrap_or("off")))
            .child(label)
    })
    .checked(code == current)
    .on_click(move |_, _, cx| {
        settings.update(cx, |settings, cx| {
            settings.change(|settings| settings.caption_translate_to = code, cx);
        });
    })
}

fn language_label(current: &str) -> &'static str {
    SPOKEN_LANGUAGES
        .iter()
        .find(|(code, _)| *code == current)
        .map_or("Meeting spoken language", |(_, label)| *label)
}

fn pick_language(stage: &WeakEntity<CallStage>, code: &str, cx: &mut App) {
    if let Some(stage) = stage.upgrade() {
        stage.update(cx, |stage, cx| {
            stage.caption_language = Some(code.into());
            cx.notify();
        });
    }
}
fn pick_font(stage: &WeakEntity<CallStage>, size: f32, cx: &mut App) {
    if let Some(stage) = stage.upgrade() {
        stage.update(cx, |stage, cx| {
            stage.caption_font = size;
            cx.notify();
        });
    }
}
fn pick_position(stage: &WeakEntity<CallStage>, top: bool, cx: &mut App) {
    if let Some(stage) = stage.upgrade() {
        stage.update(cx, |stage, cx| {
            stage.caption_top = top;
            cx.notify();
        });
    }
}

fn language_item(
    stage: WeakEntity<CallStage>,
    code: &'static str,
    label: &'static str,
    current: &str,
) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("call-caption-language-{code}"))
            .child(label)
    })
    .checked(code == current)
    .on_click(move |_, _, cx| pick_language(&stage, code, cx))
}
