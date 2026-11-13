//! The calendar's grids: the time grid of a day or a week (the days' headers, the all-day strip, the hours with their
//! half-hour lines, the events' blocks side by side where they overlap, the current time's line) and the month's.

use chrono::{Datelike as _, NaiveDate, NaiveDateTime, Timelike as _};
use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, point,
    prelude::FluentBuilder as _, px, relative, rgb,
};

use super::{CalendarView, GUTTER, HOUR_HEIGHT, MIN_MINUTES, MONTH_LISTED, Toggle, hour_label, mix, time_text};
use crate::calendar::{self, Look, Mode, Shown, Slot};
use crate::clock::{self, HourCycle};
use gpui_component::Sizable as _;

use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

impl CalendarView {
    /// The time grid of the days shown: their headers, the all-day strip, and the hours with the events; in Split view
    /// each calendar ticked has its own columns side by side, under its name and its close.
    pub(super) fn time_grid(&mut self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let span = self.span();
        let now = calendar::now().naive_local();
        let cycle = clock::cycle(cx);
        if self.toggles.on(Toggle::ScrollDue) {
            self.toggles.set(Toggle::ScrollDue, false);
            let focus = self.focus_at.take().filter(|at| span.contains(at.date()));
            let hour = if let Some(at) = focus {
                f32::from(u8::try_from(at.hour()).unwrap_or(8))
                    + f32::from(u8::try_from(at.minute()).unwrap_or(0)) / 60.0
                    - 1.0
            } else if span.contains(now.date()) {
                f32::from(u8::try_from(now.hour()).unwrap_or(8)) - 1.5
            } else {
                7.5
            };
            self.scroll
                .set_offset(point(px(0.0), px(-hour.clamp(0.0, 16.0) * HOUR_HEIGHT)));
        }
        let groups = self.grid_groups();
        let (headers, strip, body) = self.grid_parts(&groups, now, cycle, palette, cx);
        let body = div()
            .id("calendar-hours")
            .debug_selector(|| "calendar-hours".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(body);
        div()
            .debug_selector(|| "calendar-grid".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(headers)
            .children(strip)
            .child(body)
    }

    /// The calendars the grid shows: in Split view each ticked one on its own (the account's, then each person's),
    /// else one group of them all.
    fn grid_groups(&self) -> Vec<Group> {
        if !self.split() {
            let mut events = (*self.visible()).clone();
            events.extend(self.people_shown(None));
            events.sort_by_key(|shown| shown.start);
            return vec![Group {
                owner: None,
                name: String::new(),
                events,
            }];
        }
        let mut groups = Vec::new();
        if !self.toggles.on(Toggle::Hidden) {
            groups.push(Group {
                owner: None,
                name: "Calendar".to_owned(),
                events: (*self.visible()).clone(),
            });
        }
        for person in self.others.list.iter().filter(|person| person.ticked) {
            groups.push(Group {
                owner: Some(person.address.clone()),
                name: person.name.clone(),
                events: self.people_shown(Some(&person.address)),
            });
        }
        if groups.is_empty() {
            groups.push(Group {
                owner: None,
                name: "Calendar".to_owned(),
                events: Vec::new(),
            });
        }
        groups
    }

    /// The grid's three parts over `groups`: the headers (each group's name in Split view over its days), the all-day
    /// strip (when any day has an all-day event) and the hours' columns beside the gutter.
    fn grid_parts(
        &self,
        groups: &[Group],
        now: NaiveDateTime,
        cycle: HourCycle,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> (gpui::Div, Option<gpui::Div>, gpui::Div) {
        let span = self.span();
        let split = self.split();
        let mut headers = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(div().w(px(GUTTER)).flex_none());
        let mut strips = div().flex().flex_1();
        let mut any_all_day = false;
        let mut body = div().flex().child(hours_gutter(cycle, palette));
        for (index, group) in groups.iter().enumerate() {
            let apart = split && index > 0;
            let days = div()
                .flex()
                .children(span.dates().map(|date| day_header(date, now.date(), palette, cx)));
            headers = headers.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .when(apart, gpui::Styled::ml_3)
                    .children(split.then(|| self.group_header(index, group, palette, cx)))
                    .child(days),
            );
            let all_day: Vec<Vec<Shown>> = span
                .dates()
                .map(|date| {
                    let on = |shown: &&Shown| shown.all_day() && shown.on(date);
                    group.events.iter().filter(on).cloned().collect()
                })
                .collect();
            any_all_day |= all_day.iter().any(|day| !day.is_empty());
            let strip = all_day_strip(all_day, cycle, palette, self, cx);
            strips = strips.child(strip.flex_1().min_w_0().when(apart, gpui::Styled::ml_3));
            let columns: Vec<AnyElement> = span
                .dates()
                .map(|date| self.day_column(date, &group.events, now, palette, cx))
                .collect();
            body = body.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .when(apart, gpui::Styled::ml_3)
                    .children(columns),
            );
        }
        let strip = any_all_day.then(|| {
            div()
                .debug_selector(|| "calendar-all-day".to_owned())
                .flex()
                .flex_none()
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(div().w(px(GUTTER)).flex_none())
                .child(strips)
        });
        (headers, strip, body)
    }

    /// A calendar's name over its columns in Split view, in its colour, with its close (which unticks it).
    fn group_header(&self, index: usize, group: &Group, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let color = group
            .owner
            .as_deref()
            .map_or(palette.brand, |owner| self.person_color(owner));
        let owner = group.owner.clone();
        div()
            .debug_selector(move || format!("calendar-split-{index}"))
            .h(px(40.0))
            .px_3()
            .flex()
            .items_center()
            .border_b_2()
            .border_color(rgb(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_color(rgb(color))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(group.name.clone()),
            )
            .child(selected_by(
                format!("calendar-split-close-{index}"),
                icon_button(
                    SharedString::from(format!("calendar-split-close-{index}")),
                    Glyph::X,
                    "Hide this calendar",
                )
                .small()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(owner) = &owner {
                        this.tick_person(Some(owner), false, cx);
                    } else {
                        this.toggles.set(Toggle::Hidden, true);
                        cx.notify();
                    }
                })),
            ))
    }

    /// A day's column of hours: its hour and half-hour lines, its events and, today, the current time's line.
    pub(super) fn day_column(
        &self,
        date: NaiveDate,
        events: &[Shown],
        now: NaiveDateTime,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cycle = clock::cycle(cx);
        let lines = (0..24).flat_map(|hour| {
            let top = units(hour) * HOUR_HEIGHT;
            [
                div()
                    .absolute()
                    .top(px(top))
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    .bg(rgb(palette.border)),
                div()
                    .absolute()
                    .top(px(top + HOUR_HEIGHT / 2.0))
                    .left_0()
                    .right_0()
                    .h(px(1.0))
                    // the renderer draws no dashes on a line this thin: a fainter line stands for Teams' dotted one
                    .bg(rgb(mix(palette.background, palette.border, 550))),
            ]
        });
        let cells = (0..48u16).map(|half| half_hour(date, half, cx)).collect::<Vec<_>>();
        let selected = self
            .selection
            .filter(|selection| selection.date == date)
            .map(|selection| {
                div()
                    .debug_selector(|| "calendar-selection".to_owned())
                    .absolute()
                    .top(px(f32::from(selection.first) * HOUR_HEIGHT / 2.0))
                    .h(px(f32::from(selection.last - selection.first + 1) * HOUR_HEIGHT / 2.0))
                    .left_0()
                    .right_0()
                    .rounded_sm()
                    .bg(rgb(mix(palette.background, palette.brand, 300)))
                    .border_1()
                    .border_color(rgb(palette.brand))
            });
        let slots = calendar::day_slots(events, date, MIN_MINUTES);
        let blocks: Vec<AnyElement> = slots
            .into_iter()
            .map(|slot| {
                let color = slot.shown.owner.as_deref().map(|owner| self.person_color(owner));
                block(slot, color, cycle, palette, cx)
            })
            .collect();
        let now_line = self.now_line(date, now, palette);
        div()
            .flex_1()
            .w_0()
            .min_w_0()
            .relative()
            .h(px(24.0 * HOUR_HEIGHT))
            .border_l_1()
            .border_color(rgb(palette.border))
            .when(calendar::weekend(date), |column| {
                column.bg(rgb(mix(palette.background, palette.surface, 500)))
            })
            .children(lines)
            .children(selected)
            .children(cells)
            .children(blocks)
            .children(now_line)
            .into_any_element()
    }

    /// The current time's line: in today's column the brand line with its dot, across the other days a faint one.
    pub(super) fn now_line(&self, date: NaiveDate, now: NaiveDateTime, palette: Palette) -> Option<gpui::Div> {
        if !self.span().contains(now.date()) {
            return None;
        }
        let minutes = now.hour() * 60 + now.minute();
        let top = units(minutes) / 60.0 * HOUR_HEIGHT;
        if date == now.date() {
            Some(
                div()
                    .debug_selector(|| "calendar-now".to_owned())
                    .absolute()
                    .top(px(top - 5.0))
                    .left(px(-5.0))
                    .right_0()
                    .h(px(10.0))
                    .flex()
                    .items_center()
                    .child(div().size(px(10.0)).rounded_full().bg(rgb(palette.brand)))
                    .child(div().flex_1().h(px(2.0)).bg(rgb(palette.brand))),
            )
        } else {
            Some(div().absolute().top(px(top)).left_0().right_0().h(px(1.0)).bg(rgb(mix(
                palette.background,
                palette.brand,
                450,
            ))))
        }
    }

    /// The month view: the weekday names over six weeks of days, each listing its events.
    pub(super) fn month_view(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let span = self.span();
        let events = {
            let mut events = (*self.visible()).clone();
            events.extend(self.people_shown(None));
            events.sort_by_key(|shown| shown.start);
            events
        };
        let today = calendar::now().date_naive();
        let names = div()
            .flex()
            .flex_none()
            .border_b_1()
            .border_color(rgb(palette.border))
            .children(
                [
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                    "Sunday",
                ]
                .map(|name| {
                    div()
                        .flex_1()
                        .w_0()
                        .px_2()
                        .py_2()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child(name)
                }),
            );
        let dates: Vec<NaiveDate> = span.dates().collect();
        let weeks = dates.chunks(7).map(|week| {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .border_b_1()
                .border_color(rgb(palette.border))
                .children(
                    week.iter()
                        .map(|date| self.month_day(*date, &events, today, palette, cx)),
                )
        });
        div()
            .debug_selector(|| "calendar-month".to_owned())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(names)
            .children(weeks)
    }

    /// A month's day: its number (today's on the brand colour), its first events and how many more there are.
    pub(super) fn month_day(
        &self,
        date: NaiveDate,
        events: &[Shown],
        today: NaiveDate,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cycle = clock::cycle(cx);
        let listed = calendar::day_list(events, date);
        let more = listed.len().saturating_sub(MONTH_LISTED);
        let in_month = date.month() == self.anchor.month();
        let is_today = date == today;
        let number = div()
            .size(px(24.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .text_sm()
            .when(is_today, |number| {
                number
                    .bg(rgb(palette.brand))
                    .text_color(rgb(palette.on_brand))
                    .font_weight(gpui::FontWeight::BOLD)
            })
            .when(!is_today && !in_month, |number| {
                number.text_color(rgb(palette.muted_foreground))
            })
            .child(if date.day() == 1 {
                date.format("%b %-d").to_string()
            } else {
                date.day().to_string()
            });
        let more = (more > 0).then(|| {
            div()
                .id(SharedString::from(format!("calendar-more-{}", date.format("%Y%m%d"))))
                .px_1()
                .text_xs()
                .cursor_pointer()
                .text_color(rgb(palette.brand))
                .child(format!("+{more} more"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.go_to(date, cx);
                    this.set_mode(Mode::Day, cx);
                }))
        });
        div()
            .flex_1()
            .w_0()
            .min_w_0()
            .p_1()
            .flex()
            .flex_col()
            .gap_0p5()
            .overflow_hidden()
            .border_l_1()
            .border_color(rgb(palette.border))
            .when(!in_month, |cell| {
                cell.bg(rgb(mix(palette.background, palette.surface, 500)))
            })
            .child(number)
            .children(listed.into_iter().take(MONTH_LISTED).map(|shown| {
                let color = shown.owner.as_deref().map(|owner| self.person_color(owner));
                chip(shown, color, false, cycle, palette, cx)
            }))
            .children(more)
            .into_any_element()
    }
}

/// A day's header: its number and weekday, today's in the brand colour under a brand line; clicked, the day.
fn day_header(date: NaiveDate, today: NaiveDate, palette: Palette, cx: &mut Context<CalendarView>) -> AnyElement {
    let is_today = date == today;
    let color = if is_today { palette.brand } else { palette.foreground };
    let selector = format!("calendar-day-{}", date.format("%Y-%m-%d"));
    div()
        .debug_selector(move || selector.clone())
        .id(SharedString::from(format!("calendar-day-{}", date.format("%Y%m%d"))))
        .flex_1()
        .w_0()
        .min_w_0()
        .relative()
        .h(px(68.0))
        .px_4()
        .flex()
        .flex_col()
        .justify_center()
        .border_l_1()
        .border_color(rgb(palette.border))
        // Teams' brand line over today
        .when(is_today, |header| {
            header.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .h(px(3.0))
                    .bg(crate::theme::brand_gradient(palette, 90.0)),
            )
        })
        .cursor_pointer()
        .hover(move |header| header.bg(rgb(palette.hover)))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.go_to(date, cx);
            this.set_mode(Mode::Day, cx);
        }))
        .child(
            div()
                .text_2xl()
                .text_color(rgb(color))
                // today's number lit on a disc of the brand gradient
                .when(is_today, |number| {
                    number
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .size(px(36.0))
                        .ml(px(-6.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(crate::theme::brand_gradient(palette, 135.0))
                        .shadow(crate::theme::glow(palette, palette.brand, 0.35, 10.0, 2.0))
                        .text_color(rgb(palette.on_brand))
                })
                .child(date.day().to_string()),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(if is_today {
                    palette.brand
                } else {
                    palette.muted_foreground
                }))
                .child(date.format("%a").to_string()),
        )
        .into_any_element()
}

/// A calendar's part of the all-day strip under the days' headers: each day's all-day events as chips.
fn all_day_strip(
    all_day: Vec<Vec<Shown>>,
    cycle: HourCycle,
    palette: Palette,
    view: &CalendarView,
    cx: &mut Context<CalendarView>,
) -> gpui::Div {
    let days: Vec<gpui::Div> = all_day
        .into_iter()
        .map(|day| {
            div()
                .flex_1()
                .w_0()
                .min_w_0()
                .p_1()
                .flex()
                .flex_col()
                .gap_1()
                .border_l_1()
                .border_color(rgb(palette.border))
                .children(day.into_iter().map(|shown| {
                    let color = shown.owner.as_deref().map(|owner| view.person_color(owner));
                    chip(shown, color, true, cycle, palette, cx)
                }))
        })
        .collect();
    div().flex().children(days)
}

/// One half hour of a day's column, behind the events: a click selects it, a drag stretches the selection over more,
/// and a double click schedules an event there, as Teams' calendar does.
fn half_hour(date: NaiveDate, half: u16, cx: &mut Context<CalendarView>) -> gpui::Stateful<gpui::Div> {
    let selector = format!("calendar-slot-{}-{half}", date.format("%Y-%m-%d"));
    div()
        .debug_selector(move || selector.clone())
        .id(SharedString::from(format!(
            "calendar-slot-{}-{half}",
            date.format("%Y%m%d")
        )))
        .absolute()
        .top(px(f32::from(half) * HOUR_HEIGHT / 2.0))
        .left_0()
        .right_0()
        .h(px(HOUR_HEIGHT / 2.0))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.press_slot(date, half, event.click_count, cx);
            }),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            this.drag_slot(date, half, event.pressed_button == Some(MouseButton::Left), cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, _| this.release_slot()),
        )
}

/// The hours' labels left of the time grid.
fn hours_gutter(cycle: HourCycle, palette: Palette) -> gpui::Div {
    div()
        .w(px(GUTTER))
        .flex_none()
        .relative()
        .h(px(24.0 * HOUR_HEIGHT))
        .children((1..24).map(|hour| {
            div()
                .absolute()
                .top(px(units(hour) * HOUR_HEIGHT + 4.0))
                .left(px(8.0))
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(hour_label(hour, cycle))
        }))
}

/// An event's block in the time grid: the account's own in the brand colour, opening its card beside it; a person's
/// busy time in their calendar's `color`, opening nothing.
fn block(
    slot: Slot,
    color: Option<u32>,
    cycle: HourCycle,
    palette: Palette,
    cx: &mut Context<CalendarView>,
) -> AnyElement {
    let top = units(slot.top) / 60.0 * HOUR_HEIGHT;
    let height = units(slot.length) / 60.0 * HOUR_HEIGHT;
    let width = 1.0 / units(slot.columns);
    let left = units(slot.column) * width;
    let shown = slot.shown;
    let look = shown.look();
    let mark = mark(&shown, palette);
    let text = block_text(&shown, height, cycle, palette);
    let selector = format!("calendar-event-{}", shown.event.id);
    let id = SharedString::from(format!("calendar-event-{}-{}", shown.event.id, slot.top));
    let placed = div()
        .absolute()
        .top(px(top + 1.0))
        .left(relative(left))
        .w(relative(width))
        .h(px((height - 2.0).max(14.0)))
        .pr(px(4.0))
        .pl(px(2.0));
    if let Some(color) = color {
        return placed
            .child(
                person_face(color, look, palette)
                    .debug_selector(move || selector.clone())
                    .id(id)
                    .size_full()
                    .flex()
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().w(px(4.0)).h_full().flex_none().rounded_l_md().bg(rgb(color)))
                    .child(text),
            )
            .into_any_element();
    }
    placed
        .child(
            event_face(look, palette)
                .debug_selector(move || selector.clone())
                .id(id)
                .size_full()
                .relative()
                .flex()
                .overflow_hidden()
                .cursor_pointer()
                // a press on an event is the event's, not a selection of the half hour behind it
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(edge(look, palette))
                .child(text)
                .children(mark.map(|mark| div().absolute().right(px(4.0)).bottom(px(3.0)).child(mark)))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    // a double click opens the account's own event to change, as Teams does
                    let own = shown.event.is_organizer || super::own_channel_meeting(&shown.event, this.core.me());
                    if event.click_count() >= 2 && own && !shown.event.cancelled {
                        this.edit_event(shown.event.id.clone(), cx);
                    } else {
                        this.open_card_at(shown.clone(), event.position(), cx);
                    }
                })),
        )
        .into_any_element()
}

/// A block's text, by its `height`: the title and organizer on one line in a short block, else under each other with
/// the time once there is room for it.
fn block_text(shown: &Shown, height: f32, cycle: HourCycle, palette: Palette) -> gpui::Div {
    let title = shown.title();
    let organizer = shown.event.organizer_name.clone().filter(|_| !shown.event.is_organizer);
    let text = div()
        .flex_1()
        .min_w_0()
        .pl_2()
        .pr_1()
        .py_1()
        .overflow_hidden()
        .text_sm();
    if height < 40.0 {
        return text
            .flex()
            .gap_1()
            .whitespace_nowrap()
            .child(
                div()
                    .flex_none()
                    .max_w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(title),
            )
            .children(organizer.map(|name| {
                div()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(rgb(palette.muted_foreground))
                    .child(name)
            }));
    }
    let time = (height >= 72.0).then(|| format!("{} – {}", time_text(shown.start, cycle), time_text(shown.end, cycle)));
    text.flex()
        .flex_col()
        .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().child(title))
        .children(organizer.map(|name| {
            div()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(rgb(palette.muted_foreground))
                .child(name)
        }))
        .children(time.map(|time| {
            div()
                .text_xs()
                .whitespace_nowrap()
                .text_color(rgb(palette.muted_foreground))
                .child(time)
        }))
}

/// An event as one line (the all-day strip's, a month day's): its time unless all day, its title, its mark.
fn chip(
    shown: Shown,
    color: Option<u32>,
    all_day: bool,
    cycle: HourCycle,
    palette: Palette,
    cx: &mut Context<CalendarView>,
) -> AnyElement {
    let look = shown.look();
    let selector = format!("calendar-event-{}", shown.event.id);
    let id = SharedString::from(format!(
        "calendar-chip-{}-{}",
        shown.event.id,
        shown.start.and_utc().timestamp()
    ));
    let time = (!all_day && !shown.all_day()).then(|| time_text(shown.start, cycle));
    let mark = mark(&shown, palette);
    let face = match color {
        Some(color) => person_face(color, look, palette),
        None => event_face(look, palette).cursor_pointer(),
    };
    let edge = match color {
        Some(color) => div().w(px(4.0)).h_full().flex_none().rounded_l_md().bg(rgb(color)),
        None => edge(look, palette),
    };
    let chip = face
        .debug_selector(move || selector.clone())
        .id(id)
        .h(px(24.0))
        .flex()
        .items_center()
        .gap_1()
        .overflow_hidden()
        .text_sm()
        .child(edge)
        .children(time.map(|time| {
            div()
                .pl_1()
                .flex_none()
                .text_color(rgb(palette.muted_foreground))
                .child(time)
        }))
        .child(
            div()
                .pl_1()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .child(shown.title()),
        )
        .children(mark.map(|mark| div().pr_1().child(mark)));
    if color.is_some() {
        return chip.into_any_element();
    }
    chip.on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
        this.open_card_at(shown.clone(), event.position(), cx);
    }))
    .into_any_element()
}

/// A person's busy time's block or chip: tinted with their calendar's colour, a tentative one lighter.
fn person_face(color: u32, look: Look, palette: Palette) -> gpui::Div {
    let tint = if look == Look::Tentative { 140 } else { 260 };
    div().rounded_md().bg(rgb(mix(palette.background, color, tint)))
}

/// One calendar's columns of the time grid.
struct Group {
    /// Whose: `None` for the account's own.
    owner: Option<String>,
    /// Its name over its columns in Split view.
    name: String,
    events: Vec<Shown>,
}

/// An event's block or chip by its look: filled with the brand colour's tint, a cancelled one only outlined.
fn event_face(look: Look, palette: Palette) -> gpui::Div {
    let face = div().rounded_md();
    match look {
        // tinted from the brand colour into the accent, lifted a little off the grid
        Look::Busy | Look::Tentative => face
            .bg(crate::theme::gradient(
                mix(palette.background, palette.brand, 220),
                mix(palette.background, palette.accent, 200),
                160.0,
            ))
            .shadow(crate::theme::glow(palette, palette.brand, 0.12, 6.0, 1.0))
            .hover(move |face| face.bg(rgb(mix(palette.background, palette.brand, 320)))),
        Look::Cancelled => face
            .bg(rgb(palette.background))
            .border_1()
            .border_color(rgb(mix(palette.background, palette.brand, 600)))
            .text_color(rgb(palette.muted_foreground))
            .hover(move |face| face.bg(rgb(palette.hover))),
    }
}

/// An event's coloured left edge: solid when accepted, striped while tentative or not answered, faint when cancelled.
fn edge(look: Look, palette: Palette) -> gpui::Div {
    let edge = div().w(px(4.0)).h_full().flex_none().rounded_l_md().overflow_hidden();
    match look {
        Look::Busy => edge.bg(crate::theme::brand_gradient(palette, 180.0)),
        Look::Cancelled => edge.bg(rgb(mix(palette.background, palette.brand, 400))),
        Look::Tentative => edge
            .flex()
            .flex_col()
            .bg(rgb(mix(palette.background, palette.brand, 350)))
            .children((0..40).map(|stripe| {
                div()
                    .h(px(3.0))
                    .flex_none()
                    .when(stripe % 2 == 0, |stripe| stripe.bg(rgb(palette.brand)))
            })),
    }
}

/// The recurring mark of an event of a series, crossed out when it is cancelled.
fn mark(shown: &Shown, palette: Palette) -> Option<AnyElement> {
    if !shown.event.kind.recurring() && !shown.event.cancelled {
        return None;
    }
    let glyph = if shown.event.cancelled {
        Glyph::REPEAT_OFF
    } else {
        Glyph::REPEAT
    };
    Some(
        glyph
            .icon()
            .size(px(14.0))
            .text_color(rgb(palette.muted_foreground))
            .into_any_element(),
    )
}

/// A small count (hours, minutes of a day, columns) as a float for a length.
fn units(count: impl TryInto<u16>) -> f32 {
    f32::from(count.try_into().unwrap_or(u16::MAX))
}
