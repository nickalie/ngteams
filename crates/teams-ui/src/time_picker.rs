//! A card's time input, as Teams' time picker draws it: a text box that takes a typed time, and the half hours of the
//! day under its chevron. A workflow's run dialog and a bot's card use it.
use chrono::NaiveTime;
use gpui::{App, Entity, Focusable as _, InteractiveElement as _, ParentElement as _, SharedString, Window, div, px};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input as TextInput, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::{Disableable as _, Sizable as _};

use crate::icons::{Glyph, icon_button, selected_by};

/// The key context of the chevron and its list of half hours: keys there are not the field's text.
pub(crate) const CHOICE_CONTEXT: &str = "TimeChoice";

/// How tall the list of half hours grows before it scrolls, px.
const LIST_HEIGHT: f32 = 240.0;

/// A time as typed or chosen: `8:00 AM`, `8 am`, `20:00`.
pub(crate) fn parse_time(text: &str) -> Option<NaiveTime> {
    let mut text = text.trim().to_uppercase().replace('.', "");
    // an hour alone ("8 pm") is on the hour
    if !text.contains(':') {
        let digits = text.chars().take_while(char::is_ascii_digit).count();
        text.insert_str(digits, ":00");
    }
    ["%I:%M %p", "%I:%M%p", "%H:%M"]
        .iter()
        .find_map(|format| NaiveTime::parse_from_str(&text, format).ok())
}

/// A half hour as the time list names it: `8:30 AM`.
pub(crate) fn clock(minutes: u32) -> String {
    let (hour, minute) = (minutes / 60, minutes % 60);
    let shown = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!("{shown}:{minute:02} {}", if hour < 12 { "AM" } else { "PM" })
}

/// The time field of `state`, found by `selector`: typed, or chosen from the half hours under its chevron. Each row of
/// the list is found by `rows` and its time without spaces and colons (`<rows>-830AM`). A chosen time goes into the
/// field, then `on_pick` runs. `off`: the field takes nothing.
pub(crate) fn time_field(
    state: &Entity<InputState>,
    selector: &str,
    rows: &str,
    off: bool,
    on_pick: impl Fn(&mut App) + Clone + 'static,
) -> gpui::Div {
    let target = state.clone();
    let rows = rows.to_owned();
    let times = format!("{selector}-times");
    selected_by(
        selector.to_owned(),
        TextInput::new(state).disabled(off).suffix(
            selected_by(
                times.clone(),
                icon_button(SharedString::from(times), Glyph::CHEVRON_DOWN, "Choose a time")
                    .ghost()
                    .xsmall()
                    .disabled(off)
                    .dropdown_menu(move |mut menu, _, _| {
                        for half in 0..48_u32 {
                            let shown = clock(half * 30);
                            let (target, words, on_pick) = (target.clone(), shown.clone(), on_pick.clone());
                            let row = format!("{rows}-{}", shown.replace([' ', ':'], ""));
                            menu = menu.item(
                                PopupMenuItem::element(move |_, _| {
                                    let row = row.clone();
                                    div().debug_selector(move || row.clone()).child(shown.clone())
                                })
                                .on_click(move |_, window, cx| {
                                    target.update(cx, |state, cx| state.set_value(words.clone(), window, cx));
                                    keys_to_the_chevron(&target, window, cx);
                                    on_pick(cx);
                                }),
                            );
                        }
                        menu.scrollable(true).max_h(px(LIST_HEIGHT))
                    }),
            )
            .key_context(CHOICE_CONTEXT),
        ),
    )
}

/// A chosen time gives the keys to the chevron, the stop after the field, also when a click opened the list: then the
/// repeats of the Enter that chose it go to no control that had the keys before.
fn keys_to_the_chevron(field: &Entity<InputState>, window: &mut Window, cx: &mut App) {
    field.read(cx).focus_handle(cx).focus(window, cx);
    window.focus_next(cx);
}

#[cfg(test)]
mod tests {
    use super::{clock, parse_time};

    #[test]
    fn times_read_as_typed_or_chosen() {
        assert_eq!(
            parse_time("8:00 AM").map(|time| time.to_string()),
            Some("08:00:00".to_owned())
        );
        assert_eq!(
            parse_time("8 pm").map(|time| time.to_string()),
            Some("20:00:00".to_owned())
        );
        assert_eq!(
            parse_time("20:30").map(|time| time.to_string()),
            Some("20:30:00".to_owned())
        );
        assert_eq!(parse_time("soon"), None);
        assert_eq!(clock(0), "12:00 AM");
        assert_eq!(clock(13 * 60 + 30), "1:30 PM");
    }
}
