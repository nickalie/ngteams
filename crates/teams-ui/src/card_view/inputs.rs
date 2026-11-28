//! A bot card's inputs, drawn as Teams' cards draw them: the label over the control, a red
//! mark on a required one, the card's error under one that failed its check. Each control keeps its answer in the
//! message's [`CardBot`].
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Entity, Focusable, HighlightStyle, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, Styled, StyledText, div, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::date_picker::DatePicker;
use gpui_component::input::{Input as TextInput, InputState, Textarea};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::radio::Radio;
use teams_proto::chat::card::{Choice, ChoiceStyle, Input, InputKind};

use super::{BotDraw, Draw};
use crate::bot_card::{self, CardBot, Field, error_of, field_key};
use crate::icons::{Glyph, Spinning as _, selected_by};
use crate::theme::{self, Palette};

/// How tall a multi-line text input is, px.
const MULTILINE_HEIGHT: f32 = 72.0;
/// How tall a choice list's menu grows before it scrolls, px.
const MENU_HEIGHT: f32 = 280.0;

/// What one input is drawn with.
struct Place<'a> {
    bot: &'a Entity<CardBot>,
    key: String,
    selector: String,
    off: bool,
    palette: Palette,
}

/// Input `input` of the card being drawn, at `path`.
pub(super) fn view(draw: &Draw<'_>, input: &Input) -> AnyElement {
    let Some(bot) = draw.bot else {
        return div().into_any_element();
    };
    let key = field_key(draw.at, &input.id);
    let place = Place {
        bot: bot.entity,
        selector: draw.selector(&format!("c{}-in-{}", draw.at, input.id)),
        off: bot.read_only || bot.shown.busy.contains_key(&draw.at),
        palette: draw.palette,
        key,
    };
    let control = control(&place, input, bot.shown.fields.get(&place.key));
    let invalid = bot.shown.invalid.contains(&place.key);
    let label_selector = format!("{}-label", place.selector);
    div()
        .key_context(super::INPUT_CONTEXT)
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .when_some(input.label.as_deref(), |field, label| {
            field.child(
                div()
                    .debug_selector(move || label_selector)
                    .text_sm()
                    .text_color(rgb(draw.palette.foreground))
                    .child(label_text(label, input.required)),
            )
        })
        .child(control)
        .when(invalid, |field| {
            let selector = format!("{}-error", place.selector);
            let text_selector = format!("{selector}-text");
            field.child(
                div()
                    .debug_selector(move || selector)
                    .flex()
                    .items_start()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(theme::DANGER))
                    .child(div().pt_0p5().child(Glyph::CIRCLE_ALERT.icon().size(px(12.0))))
                    .child(
                        div()
                            .debug_selector(move || text_selector)
                            .flex_1()
                            .min_w_0()
                            .child(error_of(input)),
                    ),
            )
        })
        .into_any_element()
}

/// Under sign-in card `at` once its page was opened: the code that page shows, and Verify, which sends it to the bot.
pub(super) fn sign_in_code(draw: &Draw<'_>, bot: &BotDraw<'_>, at: usize) -> AnyElement {
    let selector = draw.selector(&format!("c{at}-code"));
    let key = field_key(at, bot_card::SIGN_IN_CODE);
    let busy = bot.shown.busy.get(&at).map(String::as_str);
    let verifying = busy == Some(format!("verify-{at}").as_str());
    let field = match bot.shown.fields.get(&key) {
        Some(Field::Text(state)) => keys_to(
            selected_by(selector.clone(), TextInput::new(state).disabled(busy.is_some())),
            state,
            busy.is_some(),
        )
        .flex_1()
        .min_w_0()
        .into_any_element(),
        // its control is made on the next frame
        _ => div().flex_1().into_any_element(),
    };
    let entity = bot.entity.clone();
    let verify = selected_by(
        format!("{selector}-verify"),
        Button::new(SharedString::from(format!("{selector}-verify")))
            .label("Verify")
            .primary()
            .spinning(verifying)
            .disabled(busy.is_some())
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                entity.update(cx, |bot, cx| bot.verify(at, cx));
            }),
    )
    .flex_none();
    let invalid = bot.shown.invalid.contains(&key);
    let label_selector = format!("{selector}-label");
    let error_selector = format!("{selector}-error");
    div()
        .debug_selector(move || label_selector)
        .key_context(super::INPUT_CONTEXT)
        .w_full()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_sm()
                .text_color(rgb(draw.palette.foreground))
                .child(bot_card::CODE_LABEL),
        )
        .child(div().w_full().flex().items_center().gap_2().child(field).child(verify))
        .when(invalid, |column| {
            column.child(
                div()
                    .debug_selector(move || error_selector)
                    .flex()
                    .items_start()
                    .gap_1()
                    .text_xs()
                    .text_color(rgb(theme::DANGER))
                    .child(div().pt_0p5().child(Glyph::CIRCLE_ALERT.icon().size(px(12.0))))
                    .child(div().flex_1().min_w_0().child(bot_card::CODE_NEEDED)),
            )
        })
        .into_any_element()
}

/// An input's label, wrapped in the card's width, a required one's red mark after its last word.
fn label_text(label: &str, required: bool) -> StyledText {
    if !required {
        return StyledText::new(label.to_owned());
    }
    let mark = HighlightStyle {
        color: Some(rgb(theme::DANGER).into()),
        ..HighlightStyle::default()
    };
    StyledText::new(format!("{label} *")).with_highlights([(label.len() + 1..label.len() + 2, mark)])
}

fn control(place: &Place<'_>, input: &Input, field: Option<&Field>) -> AnyElement {
    let selector = place.selector.clone();
    match (&input.kind, field) {
        (InputKind::Time { .. }, Some(Field::Text(state))) => {
            keys_to(time_field(place, state), state, place.off).into_any_element()
        }
        (_, Some(Field::Text(state))) => keys_to(
            selected_by(selector, TextInput::new(state).disabled(place.off)),
            state,
            place.off,
        )
        .w_full()
        .into_any_element(),
        (_, Some(Field::Multiline(state))) => keys_to(
            selected_by(
                selector,
                Textarea::new(state).h(px(MULTILINE_HEIGHT)).disabled(place.off),
            ),
            state,
            place.off,
        )
        .w_full()
        .into_any_element(),
        (InputKind::Date { placeholder, .. }, Some(Field::Date(state))) => selected_by(
            selector,
            DatePicker::new(state)
                .placeholder(placeholder.clone().unwrap_or_else(|| "Select a date".to_owned()))
                .w_full()
                .disabled(place.off),
        )
        .w_full()
        .into_any_element(),
        (InputKind::Toggle { title, .. }, Some(Field::Toggle(on))) => toggle(place, title, *on),
        (
            InputKind::ChoiceSet {
                choices,
                multi,
                style,
                placeholder,
                ..
            },
            Some(Field::Choice(chosen)),
        ) => {
            if *style == ChoiceStyle::Expanded {
                expanded(place, choices, *multi, chosen)
            } else {
                compact(place, choices, *multi, chosen, placeholder.as_deref())
            }
        }
        // its control is made on the next frame
        _ => div().into_any_element(),
    }
}

fn toggle(place: &Place<'_>, title: &str, on: bool) -> AnyElement {
    let (bot, key) = (place.bot.clone(), place.key.clone());
    selected_by(
        place.selector.clone(),
        Checkbox::new(SharedString::from(place.selector.clone()))
            .label(title.to_owned())
            .checked(on)
            .disabled(place.off)
            .on_click(move |checked: &bool, _, cx| {
                let checked = *checked;
                bot.update(cx, |bot, cx| bot.flip(&key, checked, cx));
            }),
    )
    .into_any_element()
}

/// An expanded choice set: a radio button per choice, or a check box per choice when several may be chosen.
fn expanded(place: &Place<'_>, choices: &[Choice], multi: bool, chosen: &[String]) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1p5()
        .children(choices.iter().map(|choice| {
            let selector = format!("{}-{}", place.selector, choice.value);
            let picked = chosen.contains(&choice.value);
            let pick = picker(place, &choice.value, multi);
            if multi {
                selected_by(
                    selector.clone(),
                    Checkbox::new(SharedString::from(selector))
                        .label(choice.title.clone())
                        .checked(picked)
                        .disabled(place.off)
                        .on_click(move |_, _, cx| pick(cx)),
                )
            } else {
                selected_by(
                    selector.clone(),
                    Radio::new(SharedString::from(selector))
                        .label(choice.title.clone())
                        .checked(picked)
                        .disabled(place.off)
                        .on_click(move |_, _, cx| pick(cx)),
                )
            }
        }))
        .into_any_element()
}

/// What a click on choice `value` does.
fn picker(place: &Place<'_>, value: &str, multi: bool) -> impl Fn(&mut App) + Clone + 'static {
    let (bot, key, value) = (place.bot.clone(), place.key.clone(), value.to_owned());
    move |cx: &mut App| {
        bot.update(cx, |bot, cx| bot.choose(&key, &value, multi, cx));
    }
}

/// A compact choice set: a drop-down showing what is chosen, its choices in a menu, each chosen one checked.
fn compact(
    place: &Place<'_>,
    choices: &[Choice],
    multi: bool,
    chosen: &[String],
    placeholder: Option<&str>,
) -> AnyElement {
    let palette = place.palette;
    let titles: Vec<String> = choices
        .iter()
        .filter(|choice| chosen.contains(&choice.value))
        .map(|choice| choice.title.clone())
        .collect();
    let (face, faint) = if titles.is_empty() {
        (placeholder.unwrap_or("Select").to_owned(), true)
    } else {
        (titles.join(", "), false)
    };
    let face = face_of(face, faint, palette);
    let items: Vec<(String, String, bool)> = choices
        .iter()
        .map(|choice| {
            (
                choice.title.clone(),
                choice.value.clone(),
                chosen.contains(&choice.value),
            )
        })
        .collect();
    let (bot, key, selector) = (place.bot.clone(), place.key.clone(), place.selector.clone());
    selected_by(
        place.selector.clone(),
        Button::new(SharedString::from(place.selector.clone()))
            .outline()
            .w_full()
            .disabled(place.off)
            .child(face)
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.scrollable(true).max_h(px(MENU_HEIGHT));
                items.iter().fold(menu, |menu, (title, value, picked)| {
                    let (bot, key, value) = (bot.clone(), key.clone(), value.clone());
                    let row = format!("{selector}-{value}");
                    let (title, picked) = (title.clone(), *picked);
                    menu.item(
                        PopupMenuItem::element(move |_, _| {
                            let row = row.clone();
                            div()
                                .debug_selector(move || row.clone())
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .w(px(14.0))
                                        .when(picked, |mark| mark.child(Glyph::CHECK.icon().size(px(14.0)))),
                                )
                                .child(title.clone())
                        })
                        .on_click(move |_, _, cx| {
                            bot.update(cx, |bot, cx| bot.choose(&key, &value, multi, cx));
                        }),
                    )
                })
            }),
    )
    .w_full()
    .into_any_element()
}

/// A drop-down's face: what is chosen (its placeholder, faint, when nothing is), and its chevron.
fn face_of(face: String, faint: bool, palette: Palette) -> gpui::Div {
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
                .when(faint, |face| face.text_color(rgb(palette.muted_foreground)))
                .child(face),
        )
        .child(
            Glyph::CHEVRON_DOWN
                .icon()
                .size(px(14.0))
                .text_color(rgb(palette.muted_foreground)),
        )
}

/// `control`, the field of text `state`, with the keys going to its text after any press on it.
fn keys_to<T: Focusable>(control: gpui::Div, state: &Entity<T>, off: bool) -> gpui::Div {
    let state = state.clone();
    control.key_context(super::TEXT_CONTEXT).when(!off, |control| {
        control.on_mouse_down(MouseButton::Left, move |_, window, cx| {
            // a press beside the text focuses the kit's frame, which shows the ring but takes no text
            let focus = state.read(cx).focus_handle(cx);
            if !focus.is_focused(window) {
                focus.focus(window, cx);
            }
        })
    })
}

/// A time: typed, or chosen from the half hours under its chevron, as Teams' time picker offers them. A chosen time
/// clears the field's error mark.
fn time_field(place: &Place<'_>, state: &Entity<InputState>) -> gpui::Div {
    let (bot, key) = (place.bot.clone(), place.key.clone());
    let rows = format!("{}-times", place.selector);
    crate::time_picker::time_field(state, &place.selector, &rows, place.off, move |cx| {
        bot.update(cx, |bot, cx| bot.picked(&key, cx));
    })
    .w_full()
}
