//! One item of the Activity pane as Teams draws it (see the parent module's docs).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, ElementId, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, deferred, div, px, rgb,
};
use gpui_component::button::Button;
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable as _, Size};
use teams_core::activity::{ActivityItem, ActivityKind, MeetingChange, Mention};

use super::{AVATAR, ActivityView, ActivityViewEvent, BADGE, activity_time, menu_surface};
use crate::clock;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::people;
use crate::theme::{self, Palette};

/// What an item's badge shows.
enum Badge {
    /// A reaction, by its key: drawn as the chat draws it.
    Reaction(String),
    At,
    Glyph(Glyph),
}

fn badge_of(kind: &ActivityKind) -> Badge {
    match kind {
        ActivityKind::Reaction(key) => Badge::Reaction(key.clone()),
        ActivityKind::Mention(Mention::You) => Badge::At,
        ActivityKind::Mention(_) | ActivityKind::AddedToTeam => Badge::Glyph(Glyph::USERS),
        ActivityKind::Reply => Badge::Glyph(Glyph::REPLY),
        ActivityKind::MissedCall => Badge::Glyph(Glyph::PHONE_MISSED),
        ActivityKind::Voicemail => Badge::Glyph(Glyph::MAIL),
        ActivityKind::Meeting(MeetingChange::Canceled) => Badge::Glyph(Glyph::CALENDAR_CANCEL),
        ActivityKind::Meeting(_) => Badge::Glyph(Glyph::CALENDAR),
        ActivityKind::App => Badge::Glyph(Glyph::WORKFLOW),
        ActivityKind::Other => Badge::Glyph(Glyph::BELL),
    }
}

impl ActivityView {
    /// The item's whole row.
    pub(super) fn row(&self, item: &ActivityItem, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id.clone();
        let group: SharedString = format!("activity-row-{id}").into();
        let unread = !self.is_read(item);
        let selected = self.selected.as_deref() == Some(id.as_str());
        let menu_open = self.item_menu.as_deref() == Some(id.as_str());
        let opened = id.clone();
        div()
            .debug_selector({
                let id = id.clone();
                move || format!("activity-item-{id}")
            })
            .id(ElementId::Name(format!("activity-item-{id}").into()))
            .group(group.clone())
            .relative()
            .w_full()
            .pl_4()
            .pr_3()
            .py_2p5()
            .flex()
            .gap_3()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |row| row.bg(rgb(palette.selected)))
            .when(!selected, |row| row.hover(move |row| row.bg(rgb(palette.hover))))
            .on_click(cx.listener(move |this, _, _, cx| this.open(&opened, cx)))
            .children(unread.then(|| unread_dot(palette)))
            .child(self.face(item, palette))
            .child(self.body(item, unread, palette, cx))
            .child(self.corner(item, &group, menu_open, palette, cx))
            .children(menu_open.then(|| self.item_menu_view(item, unread, palette, cx)))
            .into_any_element()
    }

    /// The avatar with the kind's badge.
    fn face(&self, item: &ActivityItem, palette: Palette) -> impl IntoElement {
        let name: Option<SharedString> = (!item.actor_name.is_empty()).then(|| item.actor_name.clone().into());
        let picture = item
            .actor
            .as_ref()
            .and_then(|actor| self.pictures.get(actor))
            .and_then(Option::as_ref);
        let avatar = people::avatar(name.as_ref(), picture, Size::Medium);
        let badge = div()
            .debug_selector({
                let id = item.id.clone();
                move || format!("activity-badge-{id}")
            })
            .absolute()
            .right(px(-4.0))
            .bottom(px(-4.0))
            .size(px(BADGE + 2.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgb(palette.surface))
            .text_size(px(10.0));
        let badge = match badge_of(&item.kind) {
            Badge::Reaction(key) => badge.child(crate::reactions::emoji(&key, px(BADGE - 4.0))),
            Badge::At => badge
                .text_color(rgb(theme::DANGER))
                .font_weight(gpui::FontWeight::BOLD)
                .child("@"),
            Badge::Glyph(glyph) => {
                let ink = if item.kind == ActivityKind::MissedCall {
                    theme::DANGER
                } else {
                    palette.brand
                };
                badge.text_color(rgb(ink)).child(glyph.icon().size(px(11.0)))
            }
        };
        div().relative().flex_none().size(px(AVATAR)).child(avatar).child(badge)
    }

    /// The title, the preview, the place and a missed call's buttons.
    fn body(&self, item: &ActivityItem, unread: bool, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let title: SharedString = item.title().into();
        let tip = title.clone();
        let title = div()
            .id(ElementId::Name(format!("activity-title-{}", item.id).into()))
            .debug_selector({
                let id = item.id.clone();
                move || format!("activity-title-{id}")
            })
            .line_clamp(2)
            .when(unread, |title| title.font_weight(gpui::FontWeight::SEMIBOLD))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(title);
        let preview = (!item.preview.is_empty()).then(|| {
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(item.preview.clone())
        });
        let place = self.place(item, cx);
        let place = (!place.is_empty()).then(|| div().text_xs().truncate().child(place));
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(title)
            .children(preview)
            .children(place)
            .children(self.call_buttons(item, cx))
    }

    /// A missed call's Call and Chat, as Teams' feed offers them.
    fn call_buttons(&self, item: &ActivityItem, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let actor = item.actor.clone().filter(|_| item.kind == ActivityKind::MissedCall)?;
        let (call, chat) = (actor.clone(), actor);
        let id = &item.id;
        let call_button = Button::new(ElementId::Name(format!("activity-call-{id}").into()))
            .label("Call")
            .small()
            .outline()
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ActivityViewEvent::CallBack(call.clone()));
            }));
        let chat_button = Button::new(ElementId::Name(format!("activity-chat-{id}").into()))
            .label("Chat")
            .small()
            .outline()
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.emit(ActivityViewEvent::ChatWith(chat.clone()));
            }));
        Some(
            div()
                .pt_1()
                .flex()
                .gap_2()
                .when(!self.core.read_only(), |row| {
                    row.child(selected_by(format!("activity-call-{id}"), call_button))
                })
                .child(selected_by(format!("activity-chat-{id}"), chat_button)),
        )
    }

    /// The date, which gives way to "More options" while the row is hovered or its menu open.
    fn corner(
        &self,
        item: &ActivityItem,
        group: &SharedString,
        menu_open: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let when = activity_time(item.at_ms, &chrono::Local::now(), clock::cycle(cx));
        let id = item.id.clone();
        let more = selected_by(
            format!("activity-item-more-{id}"),
            icon_button(
                ElementId::Name(format!("activity-item-more-{id}").into()),
                Glyph::ELLIPSIS,
                "More options",
            )
            .xsmall()
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.item_menu = if this.item_menu.as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id.clone())
                };
                cx.notify();
            })),
        );
        div()
            .relative()
            .flex_none()
            .min_w(px(36.0))
            .h(px(20.0))
            .flex()
            .justify_end()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .when(menu_open, |date| date.invisible())
                    .when(!menu_open, |date| {
                        date.group_hover(group.clone(), gpui::Styled::invisible)
                    })
                    .child(when),
            )
            .child(
                div()
                    .absolute()
                    .top(px(-4.0))
                    .right_0()
                    .when(!menu_open, |more| {
                        more.invisible().group_hover(group.clone(), gpui::Styled::visible)
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(more),
            )
    }

    /// An item's menu: Mark as read or unread, Remove (none of them in the read-only mode).
    fn item_menu_view(
        &self,
        item: &ActivityItem,
        unread: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let read_only = self.core.read_only();
        let (marked, removed) = (item.id.clone(), item.id.clone());
        let (glyph, words) = if unread {
            (Glyph::EYE, "Mark as read")
        } else {
            (Glyph::EYE_OFF, "Mark as unread")
        };
        let mark = crate::shell::menu_row("activity-item-mark", palette)
            .child(glyph.icon().size(px(14.0)))
            .child(words)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.mark(&marked, unread, cx);
            }));
        let remove = crate::shell::menu_row("activity-item-remove", palette)
            .child(Glyph::X.icon().size(px(14.0)))
            .child("Remove")
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.remove(&removed, cx);
            }));
        let menu = menu_surface("activity-item-menu", palette)
            .top(px(30.0))
            .right(px(8.0))
            .w(px(200.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.item_menu = None;
                cx.notify();
            }))
            .when(read_only, |menu| {
                menu.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(rgb(palette.muted_foreground))
                        .child("This window only reads your activity"),
                )
            })
            .when(!read_only, |menu| menu.child(mark).child(remove));
        deferred(menu).with_priority(1).into_any_element()
    }
}

/// The dot at an unread item's left edge.
fn unread_dot(palette: Palette) -> impl IntoElement {
    div()
        .debug_selector(|| "activity-unread-dot".to_owned())
        .absolute()
        .left(px(6.0))
        .top(px(22.0))
        .size(px(5.0))
        .rounded_full()
        .bg(rgb(palette.foreground))
}
