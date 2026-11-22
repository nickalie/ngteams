//! The box's Actions and apps (the "+"): its apps' flyout, behind `NGTEAMS_BOTS=1` for an account whose cloud serves
//! apps ([`crate::apps_flyout`]), with the hidden tools' rows over the apps; and the apps' cards the box sends with its
//! message: each drawn over the tools as Teams draws a chosen result, its app's picture and name over the card, with a
//! close mark that takes it out. With no apps, "+" is a menu of the hidden tools and "Customize toolbar"
//! ([`super::menus`]).
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, anchored, deferred, div, px,
};
use gpui_component::kbd::Kbd;
use gpui_component::{Selectable as _, Sizable as _};
use serde_json::{Value, json};
use teams_proto::chat::AppCard;

use super::menus::{Item, MenuKind, Row};
use super::{Compose, ComposeEvent, TOOL, Writes, measured};
use crate::apps_flyout::{AppsFlyout, FlyoutAction, FlyoutEvent};
use crate::card_view::{CardsElement, ShownCards};
use crate::clock::HourCycle;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// The tooltip of Actions and apps.
pub const ACTIONS_AND_APPS: &str = "Actions and apps";

/// Actions and apps, and the apps' cards the message carries.
#[derive(Default)]
pub(super) struct Apps {
    /// Apps' cards to send with the message (a message extension's result).
    pub(super) cards: Vec<AppCard>,
    /// The flyout, while it is open.
    flyout: Option<(gpui::Entity<AppsFlyout>, gpui::Subscription)>,
    /// Where its button was last drawn, for the flyout to open by.
    at: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
    /// The rows of the hidden tools' actions that the open flyout lists, in its order.
    rows: Vec<Row>,
}

impl Compose {
    /// Whether Actions and apps opens its flyout: the bots are shown, the account's cloud serves apps, and the box
    /// writes a chat's messages, not an edit.
    pub(super) fn offers_apps(&self, cx: &App) -> bool {
        crate::bots_flag::bots_on(cx)
            && self.core.capabilities().apps
            && !self.core.read_only()
            && self.read_only.is_none()
            && self.writes == Writes::Chat
            && !matches!(self.mode, super::Mode::Edit { .. })
    }

    /// "+" while it offers apps: it opens the flyout, the hidden tools' rows over its apps.
    pub(super) fn apps_button(&self, fit: &super::fit::Fit, cx: &mut Context<Self>) -> AnyElement {
        let open = self.apps.flyout.is_some();
        div()
            .relative()
            .child(selected_by(
                "compose-apps",
                icon_button("compose-apps", Glyph::PLUS, ACTIONS_AND_APPS)
                    .with_size(gpui_component::Size::Size(px(TOOL)))
                    .selected(open)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_apps(window, cx))),
            ))
            .child(self.apps_anchor())
            .children(self.anchors(MenuKind::Plus, fit, cx))
            .into_any_element()
    }

    /// Where the flyout opens by: "+", or More options while "+" is in it.
    pub(super) fn apps_anchor(&self) -> AnyElement {
        measured(&self.apps.at)
    }

    /// Opens Actions and apps, or closes it when open.
    pub fn toggle_apps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.apps.flyout.take().is_some() {
            return cx.notify();
        }
        if !self.offers_apps(cx) {
            return;
        }
        let (core, conversation) = (Arc::clone(&self.core), self.conversation.clone());
        let rows = self.flyout_rows(cx);
        let actions = self.flyout_actions(&rows, window, cx);
        let flyout = cx.new(|cx| AppsFlyout::new(core, conversation, window, cx).with_actions(actions));
        let events = cx.subscribe_in(
            &flyout,
            window,
            move |this: &mut Self, _, event: &FlyoutEvent, window, cx| {
                this.apps.flyout = None;
                let rows = std::mem::take(&mut this.apps.rows);
                match event {
                    FlyoutEvent::Close => {}
                    FlyoutEvent::GetMoreApps => cx.emit(ComposeEvent::ShowStore),
                    FlyoutEvent::Dialog(seed) => cx.emit(ComposeEvent::AppDialog(seed.clone())),
                    FlyoutEvent::Card(card) => this.add_app_card(card.as_ref().clone(), window, cx),
                    FlyoutEvent::Customize => this.take(Item::Customize, window, cx),
                    FlyoutEvent::Action(at) => {
                        if let Some(row) = rows.get(*at) {
                            // the keyboard goes back to the text first; an action that opens a picker takes it after
                            this.focus(window, cx);
                            this.take(row.item, window, cx);
                        }
                    }
                }
                cx.notify();
            },
        );
        self.apps.flyout = Some((flyout, events));
        self.apps.rows = rows;
        cx.notify();
    }

    /// The rows of the hidden tools' actions that the flyout lists over its apps, as they are now.
    fn flyout_rows(&self, cx: &App) -> Vec<Row> {
        self.menu_items(MenuKind::Plus, cx)
            .into_iter()
            // the flyout has Customize toolbar at its foot
            .filter(|item| *item != Item::Customize)
            .map(|item| self.row(item, cx))
            .collect()
    }

    /// As the box draws: when a row of the open flyout no longer shows its tool's state, its rows are made again
    /// after this frame, so that each says what its click does now.
    pub(super) fn flyout_follows_tools(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.apps.flyout.is_some() && self.apps.rows != self.flyout_rows(cx) {
            cx.defer_in(window, Self::refresh_flyout);
        }
    }

    fn refresh_flyout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((flyout, _)) = &self.apps.flyout else { return };
        let rows = self.flyout_rows(cx);
        if rows == self.apps.rows {
            return;
        }
        let actions = self.flyout_actions(&rows, window, cx);
        flyout.update(cx, |flyout, cx| flyout.set_actions(actions, cx));
        self.apps.rows = rows;
    }

    /// `rows` as the flyout lists them over its apps: each with its icon, its name, its key and whether it can be
    /// taken now.
    fn flyout_actions(&self, rows: &[Row], window: &Window, cx: &App) -> Vec<FlyoutAction> {
        let text = self.editor.read(cx).focus.clone();
        let befores = std::iter::once(None).chain(rows.iter().map(|row| Some(row.item)));
        rows.iter()
            .zip(befores)
            .map(|(row, before)| FlyoutAction {
                selector: format!("{}-row", row.item.selector()).into(),
                label: row.label.clone(),
                glyph: row.item.glyph(),
                key: row
                    .item
                    .action()
                    .and_then(|action| Kbd::binding_for_action_in(action, &text, window)),
                disabled: row.disabled,
                apart: super::menus::apart(before, row.item),
            })
            .collect()
    }

    /// Actions and apps, while it is open.
    #[must_use]
    pub fn apps_flyout(&self) -> Option<&gpui::Entity<AppsFlyout>> {
        self.apps.flyout.as_ref().map(|(flyout, _)| flyout)
    }

    /// `card` goes with the next message, after the cards the box holds; the box takes the focus.
    pub fn add_app_card(&mut self, card: AppCard, window: &mut Window, cx: &mut Context<Self>) {
        self.apps.cards.push(card);
        self.focus(window, cx);
        cx.notify();
    }

    /// The apps' cards the next message carries.
    #[must_use]
    pub fn app_cards(&self) -> &[AppCard] {
        &self.apps.cards
    }

    fn remove_app_card(&mut self, at: usize, cx: &mut Context<Self>) {
        if at < self.apps.cards.len() {
            self.apps.cards.remove(at);
            cx.notify();
        }
    }

    /// The flyout over the box, by its button as it was last drawn, opening upwards.
    pub(super) fn apps_overlay(&self) -> Option<AnyElement> {
        let ((flyout, _), at) = self.apps.flyout.as_ref().zip(self.apps.at.get())?;
        Some(
            deferred(
                anchored()
                    .anchor(gpui::Anchor::BottomRight)
                    .position(at.top_right())
                    .snap_to_window_with_margin(px(8.0))
                    .child(div().pb_1().child(flyout.clone())),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// The files the box holds, then the apps' cards.
    pub(super) fn box_cards(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut cards: Vec<AnyElement> = self.attachment_cards(palette, cx).into_iter().collect();
        cards.extend(self.app_card_previews(palette, cx));
        cards
    }

    /// The apps' cards the box holds, each with its close mark.
    fn app_card_previews(&self, palette: Palette, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.apps
            .cards
            .iter()
            .enumerate()
            .filter_map(|(at, card)| {
                let shown = teams_proto::chat::card::message_cards(Some(&Value::Array(vec![entry(card)])))?;
                let drawn = CardsElement::new(
                    SharedString::from(format!("compose-app-card-{at}")),
                    ShownCards(Arc::new(shown)),
                    palette,
                    HourCycle::Twelve,
                )
                .submits(false)
                .source(Some(crate::rich_text::PictureSource::new(Arc::clone(&self.core))));
                let selector = format!("compose-app-card-{at}");
                Some(
                    div()
                        .debug_selector(move || selector)
                        .relative()
                        .w(px(crate::card_view::CARD_WIDTH))
                        .max_w_full()
                        .child(drawn)
                        .child(
                            div().absolute().top_2().right_2().child(selected_by(
                                format!("compose-app-card-remove-{at}"),
                                icon_button(
                                    SharedString::from(format!("compose-app-card-remove-{at}")),
                                    Glyph::X,
                                    "Remove card",
                                )
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| this.remove_app_card(at, cx))),
                            )),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// `card` as `properties.cards` names it, for the box to draw it as a sent message's card is drawn.
fn entry(card: &AppCard) -> Value {
    json!({
        "appId": card.app_id,
        "appName": card.app_name,
        "appIcon": card.app_icon,
        "contentType": card.content_type,
        "content": card.content,
    })
}
