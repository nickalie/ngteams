//! A series' recurrence in the scheduling form, as Teams' form offers it: its repeat drop-down (Does not repeat,
//! Every weekday (Mon - Fri), Daily, Weekly, Monthly, Yearly, Custom) whose presets follow the start's day (weekly on
//! its weekday, monthly on its week of the month, yearly on its date), the series' end (a date, or never), the summary
//! line Teams writes under it ("Occurs every Tuesday starting 10/27/2026"), and the custom recurrence dialog (repeat
//! every n days, weeks on chosen days, months on a day or a weekday of the month, years; the end).
//!
//! What a series sends is `teams-proto`'s [`Recurrence`]: the pattern, the start's day and the end's, as midnights
//! of the zone it repeats in (`docs/protocol/calendar.md`, "A series"). Its days are the start zone's: a start zone
//! other than the desktop's gives the weekday, the first day and the label of the day it is there
//! ([`super::zones::series_day`]).

use std::fmt::Write as _;

use chrono::{Datelike as _, NaiveDate};
use gpui::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::date_picker::DatePicker;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use teams_proto::{Recurrence, RecurrencePattern as Pattern, WeekIndex, Weekday};

use super::EventForm;
use super::zones::{Tz, midnight};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::{self, Palette};

/// A choice of the repeat drop-down that sets a pattern from the start's day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Weekdays,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Preset {
    pub const ALL: [Self; 5] = [Self::Weekdays, Self::Daily, Self::Weekly, Self::Monthly, Self::Yearly];

    /// Its label in the drop-down (Teams' own).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Weekdays => "Every weekday (Mon - Fri)",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
            Self::Yearly => "Yearly",
        }
    }

    /// Its selector's key.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Weekdays => "weekdays",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }

    /// The pattern it sets for a series starting on `start`, as Teams' form sets it: weekly on the start's weekday,
    /// monthly on its week of the month (the last when it is in the month's last seven days), yearly on its date.
    #[must_use]
    pub fn pattern(self, start: NaiveDate) -> Pattern {
        match self {
            Self::Weekdays => Pattern::Weekly {
                interval: 1,
                days: Weekday::WORK_WEEK.to_vec(),
            },
            Self::Daily => Pattern::Daily { interval: 1 },
            Self::Weekly => Pattern::Weekly {
                interval: 1,
                days: vec![weekday(start)],
            },
            Self::Monthly => Pattern::RelativeMonthly {
                interval: 1,
                index: week_index(start),
                day: weekday(start),
            },
            Self::Yearly => Pattern::Yearly {
                month: month(start),
                day: day(start),
            },
        }
    }
}

/// The form's recurrence: how the series repeats, and its last day when it ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repeat {
    pub pattern: Pattern,
    pub until: Option<NaiveDate>,
}

impl Repeat {
    /// `preset` for a series starting on `start`, never ending.
    #[must_use]
    pub fn preset(preset: Preset, start: NaiveDate) -> Self {
        Self {
            pattern: preset.pattern(start),
            until: None,
        }
    }

    /// The preset it is for a series starting on `start`; `None` for a custom one.
    #[must_use]
    pub fn which(&self, start: NaiveDate) -> Option<Preset> {
        Preset::ALL
            .into_iter()
            .find(|preset| preset.pattern(start) == self.pattern)
    }

    /// The same, the series now starting on `to` (it started on `from`): a preset follows the start's day, a custom
    /// pattern stays; an end before the new start is dropped.
    #[must_use]
    pub fn moved(&self, from: NaiveDate, to: NaiveDate) -> Self {
        let pattern = self
            .which(from)
            .map_or_else(|| self.pattern.clone(), |preset| preset.pattern(to));
        Self {
            pattern,
            until: self.until.filter(|until| *until >= to),
        }
    }

    /// What the drop-down shows for a series starting on `start`.
    #[must_use]
    pub fn label(&self, start: NaiveDate) -> &'static str {
        self.which(start).map_or("Custom", Preset::label)
    }

    /// What it sends for a series starting on `start`, its days as `zone`'s midnights (the desktop's without one).
    #[must_use]
    pub fn recurrence(&self, start: NaiveDate, zone: Option<Tz>) -> Recurrence {
        Recurrence {
            pattern: self.pattern.clone(),
            start: midnight(start, zone),
            end: self.until.map(|until| midnight(until, zone)),
        }
    }

    /// The recurrence an event read gives, its days in the offset it was written with.
    #[must_use]
    pub fn of(recurrence: &Recurrence) -> Self {
        Self {
            pattern: recurrence.pattern.clone(),
            until: recurrence.end.and_then(civil_date),
        }
    }

    /// Teams' summary of the series an event read gives, from its first day; `None` when that day doesn't read.
    #[must_use]
    pub fn summary_of(recurrence: &Recurrence) -> Option<String> {
        Some(Self::of(recurrence).summary(civil_date(recurrence.start)?))
    }

    /// Teams' summary of it, for a series starting on `start`: "Occurs every Tuesday starting 10/27/2026", with "until
    /// <date>" when it ends.
    #[must_use]
    pub fn summary(&self, start: NaiveDate) -> String {
        let starting = format!(" starting {}", short(start));
        let until = self
            .until
            .map(|until| format!(" until {}", short(until)))
            .unwrap_or_default();
        let every = |interval: u32, one: &str, many: &str| {
            if interval == 1 {
                format!("every {one}")
            } else {
                format!("every {interval} {many}")
            }
        };
        let text = match &self.pattern {
            Pattern::Weekly { interval: 1, days } if all_days(days) => format!("Occurs every day{starting}"),
            Pattern::Weekly { interval: 1, days } if work_week(days) => {
                return format!("Occurs every Monday through Friday{until}");
            }
            Pattern::Daily { interval } => format!("Occurs {}{starting}", every(*interval, "day", "days")),
            Pattern::Weekly { interval: 1, days } => format!("Occurs every {}{starting}", day_list(days)),
            Pattern::Weekly { interval, days } => {
                format!("Occurs every {interval} weeks on {}{starting}", day_list(days))
            }
            Pattern::Monthly { interval, day } => {
                format!("Occurs {} on day {day}{starting}", every(*interval, "month", "months"))
            }
            Pattern::RelativeMonthly { interval, index, day } => format!(
                "Occurs {} on the {} {}{starting}",
                every(*interval, "month", "months"),
                index.name(),
                day.name()
            ),
            Pattern::Yearly { month, day } => {
                return format!("Occurs every year on day {day} of {}{until}", month_name(*month));
            }
            Pattern::RelativeYearly { month, index, day } => {
                return format!(
                    "Occurs every year on the {} {} of {}{until}",
                    index.name(),
                    day.name(),
                    month_name(*month)
                );
            }
        };
        format!("{text}{until}")
    }
}

/// How often the custom dialog repeats by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    pub const ALL: [Self; 4] = [Self::Day, Self::Week, Self::Month, Self::Year];

    fn label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Year => "Year",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }
}

/// The custom recurrence dialog's choices while it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Custom {
    pub interval: u32,
    pub unit: Unit,
    /// The days a weekly series repeats on.
    pub days: Vec<Weekday>,
    /// A monthly or yearly series on the start's weekday of its week (the fourth Tuesday) rather than its date.
    pub relative: bool,
    pub until: Option<NaiveDate>,
}

/// The most a series repeats every.
const MAX_INTERVAL: u32 = 99;

impl Custom {
    /// Turns `day` on among a weekly series' days, or off when it is on.
    fn toggle(&mut self, day: Weekday) {
        if self.days.contains(&day) {
            self.days.retain(|known| *known != day);
        } else {
            self.days.push(day);
        }
    }

    /// The dialog on `repeat` (a series starting on `start`), or on a weekly series when there is none.
    #[must_use]
    pub fn of(repeat: Option<&Repeat>, start: NaiveDate) -> Self {
        let mut custom = Self {
            interval: 1,
            unit: Unit::Week,
            days: vec![weekday(start)],
            relative: false,
            until: repeat.and_then(|repeat| repeat.until),
        };
        match repeat.map(|repeat| &repeat.pattern) {
            Some(Pattern::Daily { interval }) => (custom.unit, custom.interval) = (Unit::Day, *interval),
            Some(Pattern::Weekly { interval, days }) => (custom.interval, custom.days) = (*interval, days.clone()),
            Some(Pattern::Monthly { interval, .. }) => (custom.unit, custom.interval) = (Unit::Month, *interval),
            Some(Pattern::RelativeMonthly { interval, .. }) => {
                (custom.unit, custom.interval, custom.relative) = (Unit::Month, *interval, true);
            }
            Some(Pattern::Yearly { .. }) => custom.unit = Unit::Year,
            Some(Pattern::RelativeYearly { .. }) => (custom.unit, custom.relative) = (Unit::Year, true),
            None => {}
        }
        custom
    }

    /// The recurrence it makes for a series starting on `start` (a weekly one with no day chosen repeats on the
    /// start's weekday).
    #[must_use]
    pub fn repeat(&self, start: NaiveDate) -> Repeat {
        let interval = self.interval.clamp(1, MAX_INTERVAL);
        let pattern = match (self.unit, self.relative) {
            (Unit::Day, _) => Pattern::Daily { interval },
            (Unit::Week, _) => {
                let mut days = self.days.clone();
                if days.is_empty() {
                    days.push(weekday(start));
                }
                days.sort();
                Pattern::Weekly { interval, days }
            }
            (Unit::Month, false) => Pattern::Monthly {
                interval,
                day: day(start),
            },
            (Unit::Month, true) => Pattern::RelativeMonthly {
                interval,
                index: week_index(start),
                day: weekday(start),
            },
            (Unit::Year, false) => Pattern::Yearly {
                month: month(start),
                day: day(start),
            },
            (Unit::Year, true) => Pattern::RelativeYearly {
                month: month(start),
                index: week_index(start),
                day: weekday(start),
            },
        };
        Repeat {
            pattern,
            until: self.until.filter(|until| *until >= start),
        }
    }
}

fn weekday(date: NaiveDate) -> Weekday {
    Weekday::ALL[usize::try_from(date.weekday().num_days_from_sunday()).unwrap_or(0) % 7]
}

/// The week of its month `date` is in, the last when it is in the month's last seven days (Teams' rule).
fn week_index(date: NaiveDate) -> WeekIndex {
    let last_day = days_in_month(date);
    if date.day() + 7 > last_day {
        return WeekIndex::Last;
    }
    WeekIndex::ALL[usize::try_from((date.day() - 1) / 7).unwrap_or(0).min(3)]
}

fn days_in_month(date: NaiveDate) -> u32 {
    let first = date.with_day(1).unwrap_or(date);
    let next = first.checked_add_months(chrono::Months::new(1)).unwrap_or(first);
    u32::try_from((next - first).num_days()).unwrap_or(31)
}

fn month(date: NaiveDate) -> u8 {
    u8::try_from(date.month()).unwrap_or(1)
}

fn day(date: NaiveDate) -> u8 {
    u8::try_from(date.day()).unwrap_or(1)
}

fn short(date: NaiveDate) -> String {
    date.format("%-m/%-d/%Y").to_string()
}

fn month_name(month: u8) -> &'static str {
    const NAMES: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    NAMES[usize::from(month.clamp(1, 12)) - 1]
}

fn all_days(days: &[Weekday]) -> bool {
    Weekday::ALL.iter().all(|day| days.contains(day))
}

fn work_week(days: &[Weekday]) -> bool {
    days.len() == 5 && Weekday::WORK_WEEK.iter().all(|day| days.contains(day))
}

/// "Tuesday", "Tuesday and Thursday", "Monday, Wednesday and Friday", Sunday first.
fn day_list(days: &[Weekday]) -> String {
    let mut days = days.to_vec();
    days.sort();
    days.dedup();
    let names: Vec<&str> = days.iter().map(|day| day.name()).collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The day a range's midnight falls on, in the offset it was written with.
fn civil_date(time: teams_proto::ZonedTime) -> Option<NaiveDate> {
    let shifted = time.ms.checked_add(i64::from(time.offset_minutes) * 60_000)?;
    chrono::DateTime::from_timestamp_millis(shifted).map(|time| time.date_naive())
}

/// The one-letter names of the custom dialog's day toggles, Sunday first.
const DAY_LETTERS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];

impl EventForm {
    /// What the repeat drop-down shows ("Weekly", "Custom"), for a series.
    #[must_use]
    pub fn repeat_label(&self) -> Option<&'static str> {
        Some(self.repeat.as_ref()?.label(self.series_day()))
    }

    /// The repeat row a series shows under its time: the drop-down, the end, and the summary.
    pub(super) fn repeat_row(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let repeat = self.repeat.clone()?;
        let start = self.series_day();
        let summary = repeat.summary(start);
        let menu = self.repeat_menu(repeat, start, cx);
        let content = div()
            .debug_selector(|| "event-repeat-row".to_owned())
            .flex_1()
            .min_w_0()
            .py_1()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(selected_by("event-repeat", menu))
                    .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child("Ends"))
                    .child(
                        div().w(px(170.0)).child(
                            DatePicker::new(&self.until_picker)
                                .small()
                                .placeholder("Never")
                                .cleanable(true),
                        ),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "event-repeat-summary".to_owned())
                    .px_3()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(summary),
            );
        Some(super::field_row(palette, Glyph::REPEAT, content, false))
    }

    /// The repeat drop-down: Does not repeat (unless the series is fixed), the presets, and Custom.
    fn repeat_menu(&self, repeat: Repeat, start: NaiveDate, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        let fixed = self.series_fixed();
        let label = repeat.label(start);
        Button::new("event-repeat")
            .ghost()
            .small()
            .tooltip("Repeat")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(label)
                    .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
            )
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.min_w(px(240.0));
                if !fixed {
                    let form = form.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Does not repeat").on_click(move |_, window, cx: &mut App| {
                            let _ = form.update(cx, |form, cx| form.set_repeat(None, window, cx));
                        }),
                    );
                }
                let chosen = repeat.which(start);
                let menu = Preset::ALL.into_iter().fold(menu, |menu, preset| {
                    let form = form.clone();
                    menu.item(
                        PopupMenuItem::new(preset.label())
                            .checked(chosen == Some(preset))
                            .on_click(move |_, window, cx: &mut App| {
                                let _ = form.update(cx, |form, cx| form.choose_preset(preset, window, cx));
                            }),
                    )
                });
                let form = form.clone();
                menu.item(PopupMenuItem::new("Custom").checked(chosen.is_none()).on_click(
                    move |_, window, cx: &mut App| {
                        let _ = form.update(cx, |form, cx| form.open_custom(window, cx));
                    },
                ))
            })
    }

    /// Repeats the event by `preset` from its day, keeping the end it has.
    fn choose_preset(&mut self, preset: Preset, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.series_day();
        let until = self.repeat.as_ref().and_then(|repeat| repeat.until);
        let mut repeat = Repeat::preset(preset, start);
        repeat.until = until;
        self.set_repeat(Some(repeat), window, cx);
    }

    /// The custom recurrence dialog, over the form while it is open.
    pub(super) fn custom_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let custom = self.custom.clone()?;
        let start = self.series_day();
        let preview = custom.repeat(start);
        let dialog = div()
            .debug_selector(|| "event-custom-repeat".to_owned())
            .id("event-custom-repeat")
            .occlude()
            .w(px(460.0))
            .p_5()
            .flex()
            .flex_col()
            .gap_4()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Set recurrence"),
            )
            .child(custom_line(palette, "Starts", div().child(short(start))))
            .child(custom_line(
                palette,
                "Repeat every",
                Self::interval_controls(&custom, cx),
            ))
            .children(Self::unit_details(&custom, start, palette, cx))
            .child(custom_line(
                palette,
                "Ends",
                div().w(px(200.0)).child(
                    DatePicker::new(&self.custom_until_picker)
                        .small()
                        .placeholder("Select date")
                        .cleanable(true),
                ),
            ))
            .child(
                div()
                    .debug_selector(|| "event-custom-summary".to_owned())
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(preview.summary(start)),
            )
            .child(Self::custom_buttons(cx));
        Some(
            div()
                .id("event-custom-backdrop")
                .occlude()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .on_click(cx.listener(|form, _, _, cx| {
                    form.custom = None;
                    cx.notify();
                }))
                .child(dialog),
        )
    }

    /// Repeat every: the number with − and +, and the unit.
    fn interval_controls(custom: &Custom, cx: &mut Context<Self>) -> gpui::Div {
        let interval = custom.interval;
        let units = Unit::ALL.into_iter().map(|unit| {
            let id = SharedString::from(format!("event-custom-unit-{}", unit.key()));
            selected_by(
                id.clone(),
                Button::new(id)
                    .small()
                    .when(unit == custom.unit, gpui_component::button::ButtonVariants::primary)
                    .when(unit != custom.unit, gpui_component::button::Button::outline)
                    .label(unit.label())
                    .on_click(cx.listener(move |form, _, _, cx| form.edit_custom(|custom| custom.unit = unit, cx))),
            )
        });
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(selected_by(
                "event-custom-less",
                icon_button("event-custom-less", Glyph::MINUS, "Less often")
                    .small()
                    .disabled(interval <= 1)
                    .on_click(cx.listener(|form, _, _, cx| {
                        form.edit_custom(|custom| custom.interval = custom.interval.saturating_sub(1).max(1), cx);
                    })),
            ))
            .child(
                div()
                    .debug_selector(|| "event-custom-interval".to_owned())
                    .w(px(28.0))
                    .flex()
                    .justify_center()
                    .child(interval.to_string()),
            )
            .child(selected_by(
                "event-custom-more",
                icon_button("event-custom-more", Glyph::PLUS, "More often")
                    .small()
                    .disabled(interval >= MAX_INTERVAL)
                    .on_click(cx.listener(|form, _, _, cx| {
                        form.edit_custom(|custom| custom.interval = (custom.interval + 1).min(MAX_INTERVAL), cx);
                    })),
            ))
            .child(div().w(px(8.0)))
            .children(units)
    }

    /// What the unit asks: the days of a week; a month's or a year's date or weekday.
    fn unit_details(custom: &Custom, start: NaiveDate, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        match custom.unit {
            Unit::Day => None,
            Unit::Week => Some(custom_line(palette, "On", Self::day_toggles(custom, palette, cx))),
            Unit::Month | Unit::Year => {
                let yearly = custom.unit == Unit::Year;
                let absolute = if yearly {
                    format!("On {} {}", month_name(month(start)), start.day())
                } else {
                    format!("On day {}", start.day())
                };
                let mut relative = format!("On the {} {}", week_index(start).name(), weekday(start).name());
                if yearly {
                    let _ = write!(relative, " of {}", month_name(month(start)));
                }
                Some(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(choice("event-custom-on-date", absolute, !custom.relative, false, cx))
                        .child(choice("event-custom-on-weekday", relative, custom.relative, true, cx)),
                )
            }
        }
    }

    /// The week's day toggles, Sunday first.
    fn day_toggles(custom: &Custom, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .gap_1()
            .children(Weekday::ALL.into_iter().zip(DAY_LETTERS).map(|(day, letter)| {
                let on = custom.days.contains(&day);
                let id = SharedString::from(format!("event-custom-day-{}", day.number()));
                div()
                    .id(id.clone())
                    .debug_selector(move || id.to_string())
                    .size(px(30.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .cursor_pointer()
                    .text_sm()
                    .when(on, |toggle| {
                        toggle
                            .bg(rgb(palette.brand))
                            .border_color(rgb(palette.brand))
                            .text_color(rgb(palette.on_brand))
                    })
                    .when(!on, |toggle| {
                        toggle
                            .border_color(rgb(palette.border))
                            .hover(move |toggle| toggle.bg(rgb(palette.hover)))
                    })
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(day.name()).build(window, cx))
                    .child(letter)
                    .on_click(cx.listener(move |form, _, _, cx| form.edit_custom(|custom| custom.toggle(day), cx)))
            }))
    }

    fn custom_buttons(cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .justify_end()
            .gap_2()
            .child(selected_by(
                "event-custom-cancel",
                Button::new("event-custom-cancel")
                    .outline()
                    .label("Cancel")
                    .on_click(cx.listener(|form, _, _, cx| {
                        form.custom = None;
                        cx.notify();
                    })),
            ))
            .child(selected_by(
                "event-custom-save",
                Button::new("event-custom-save")
                    .primary()
                    .label("Save")
                    .on_click(cx.listener(|form, _, window, cx| form.save_custom(window, cx))),
            ))
    }
}

/// A line of the custom dialog: its name on the left, what it sets beside it.
fn custom_line(palette: Palette, name: &'static str, content: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(96.0))
                .flex_none()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(name),
        )
        .child(content)
}

/// One of a month's or a year's two choices: a round mark and its text.
fn choice(id: &'static str, text: String, chosen: bool, relative: bool, cx: &mut Context<EventForm>) -> AnyElement {
    let palette = theme::current(cx);
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .gap_2()
        .py_1()
        .cursor_pointer()
        .child(
            div()
                .size(px(16.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(rgb(if chosen { palette.brand } else { palette.border }))
                .when(chosen, |mark| {
                    mark.child(div().size(px(8.0)).rounded_full().bg(rgb(palette.brand)))
                }),
        )
        .child(div().text_sm().child(text))
        .on_click(cx.listener(move |form, _, _, cx| form.edit_custom(|custom| custom.relative = relative, cx)))
        .into_any_element()
}

/// The series' end a date picker gives, a day before `start` taken as none.
pub(super) fn picked_until(date: Option<NaiveDate>, start: NaiveDate) -> Option<NaiveDate> {
    date.filter(|date| *date >= start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).expect("a date")
    }

    #[test]
    fn the_presets_follow_the_start_s_day_as_teams_sets_them() {
        let tuesday = date(10, 27);
        assert_eq!(
            Preset::Weekly.pattern(tuesday),
            Pattern::Weekly {
                interval: 1,
                days: vec![Weekday::Tuesday]
            }
        );
        // the 27th of a 31-day month is in its last seven days
        assert_eq!(
            Preset::Monthly.pattern(tuesday),
            Pattern::RelativeMonthly {
                interval: 1,
                index: WeekIndex::Last,
                day: Weekday::Tuesday
            }
        );
        assert_eq!(
            Preset::Monthly.pattern(date(10, 13)),
            Pattern::RelativeMonthly {
                interval: 1,
                index: WeekIndex::Second,
                day: Weekday::Tuesday
            }
        );
        assert_eq!(Preset::Yearly.pattern(tuesday), Pattern::Yearly { month: 10, day: 27 });
        let weekly = Repeat::preset(Preset::Weekly, tuesday);
        assert_eq!(weekly.which(tuesday), Some(Preset::Weekly));
        assert_eq!(
            weekly.moved(tuesday, date(10, 29)).pattern,
            Preset::Weekly.pattern(date(10, 29))
        );
    }

    #[test]
    fn a_read_series_is_summed_up_from_its_first_day_as_written() {
        let tuesday = date(10, 27);
        let read = Repeat::preset(Preset::Weekly, tuesday).recurrence(tuesday, None);
        assert_eq!(
            Repeat::summary_of(&read).as_deref(),
            Some("Occurs every Tuesday starting 10/27/2026")
        );
        let unreadable = Recurrence {
            start: teams_proto::ZonedTime {
                ms: i64::MAX,
                offset_minutes: 60,
            },
            ..read
        };
        assert_eq!(Repeat::summary_of(&unreadable), None);
    }

    #[test]
    fn the_summary_reads_as_teams_writes_it() {
        let tuesday = date(10, 27);
        assert_eq!(
            Repeat::preset(Preset::Weekly, tuesday).summary(tuesday),
            "Occurs every Tuesday starting 10/27/2026"
        );
        assert_eq!(
            Repeat::preset(Preset::Weekdays, tuesday).summary(tuesday),
            "Occurs every Monday through Friday"
        );
        let mut daily = Repeat {
            pattern: Pattern::Daily { interval: 3 },
            until: Some(date(11, 30)),
        };
        assert_eq!(
            daily.summary(tuesday),
            "Occurs every 3 days starting 10/27/2026 until 11/30/2026"
        );
        daily.pattern = Pattern::Weekly {
            interval: 2,
            days: vec![Weekday::Thursday, Weekday::Monday, Weekday::Tuesday],
        };
        assert_eq!(
            daily.summary(tuesday),
            "Occurs every 2 weeks on Monday, Tuesday and Thursday starting 10/27/2026 until 11/30/2026"
        );
        assert_eq!(
            Repeat::preset(Preset::Monthly, tuesday).summary(tuesday),
            "Occurs every month on the last Tuesday starting 10/27/2026"
        );
        assert_eq!(
            Repeat::preset(Preset::Yearly, tuesday).summary(tuesday),
            "Occurs every year on day 27 of October"
        );
    }

    #[test]
    fn the_custom_dialog_round_trips_a_recurrence() {
        let tuesday = date(10, 27);
        for preset in Preset::ALL {
            let repeat = Repeat::preset(preset, tuesday);
            let custom = Custom::of(Some(&repeat), tuesday);
            assert_eq!(custom.repeat(tuesday), repeat, "{preset:?}");
        }
        let mut custom = Custom::of(None, tuesday);
        custom.days.clear();
        assert_eq!(custom.repeat(tuesday), Repeat::preset(Preset::Weekly, tuesday));
        custom.unit = Unit::Month;
        assert_eq!(
            custom.repeat(tuesday).pattern,
            Pattern::Monthly { interval: 1, day: 27 }
        );
    }

    #[test]
    fn a_read_recurrence_ends_on_its_written_day() {
        let recurrence = Repeat {
            pattern: Pattern::Daily { interval: 1 },
            until: Some(date(12, 1)),
        }
        .recurrence(date(10, 27), None);
        assert_eq!(Repeat::of(&recurrence).until, Some(date(12, 1)));
        // in a zone of its own, its days are that zone's midnights, read back as the same days
        let honolulu = Repeat {
            pattern: Pattern::Daily { interval: 1 },
            until: Some(date(12, 1)),
        }
        .recurrence(date(10, 27), Some(chrono_tz::Pacific::Honolulu));
        assert_eq!(honolulu.start.offset_minutes, -600);
        assert_eq!(Repeat::of(&honolulu).until, Some(date(12, 1)));
        assert_eq!(civil_date(honolulu.start), Some(date(10, 27)));
    }
}
