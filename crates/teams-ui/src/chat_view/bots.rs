//! A chat's bot cards and suggested actions:
//!
//! - **Cards**: each card message keeps its bot state ([`CardBot`]) while the view lives: its answers, its buttons'
//!   asks and what the bot answered ([`crate::bot_card`]). A card whose `refresh` names the account runs it by itself
//!   when it is first drawn; any card with a `refresh` offers "Refresh card" in its message's "More options". Where the
//!   bot put its card in a card's place, that card's `refresh` is the one offered and run. When the message's cards
//!   change (an edit), its bot state starts again ([`CardBot::renewed`]), and a card that names the account refreshes
//!   by itself again.
//! - **Suggested actions**: when the newest message of a 1:1 chat is a bot's and suggests actions, they stand as
//!   buttons over the compose box, as Teams shows them; a click posts the user's answer (an `imBack`) or asks the bot.
//!   They ask in a place of their own ([`SUGGESTED`]), not as a card: while their ask goes they wait and the cards
//!   don't, and what the bot answered shows under them. They go once anyone writes again.
//!
//! In read-only mode a card's inputs are drawn but take nothing, its buttons open the message in Teams on the web, and
//! no refresh or suggested action is offered.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div, px, rgb,
};
use gpui_component::button::{Button, ButtonRounded};
use gpui_component::{Disableable as _, Sizable as _};
use teams_proto::chat::card::{ActionKind, Card};

use super::{Body, ChatView, MessageItem, Place, writes_to};
use crate::bot_card::{CardBot, SUGGESTED, Target};
use crate::card_view::{BotHook, CardState, bot_line};
use crate::icons::{Glyph, Spinning as _};
use crate::theme::Palette;

/// "Refresh card" in a message's "More options".
pub const REFRESH_CARD: &str = "Refresh card";

/// What a chat's cards show besides what they say ([`ChatView`]'s, per message).
pub(super) struct HeldCards {
    /// Toggled elements, opened cards.
    pub(super) states: HashMap<String, CardState>,
    /// Each card message's bot state, made as it is first drawn.
    bots: RefCell<HashMap<String, HeldBot>>,
    /// The apps' message actions "More actions" lists ([`super::app_actions`]).
    pub(super) app_actions: super::app_actions::AppActions,
}

/// A card message's bot state, and the cards it was made for.
struct HeldBot {
    bot: Entity<CardBot>,
    /// The message's version when its cards were last compared.
    version: i64,
    cards: Vec<Card>,
}

impl HeldCards {
    pub(super) fn new() -> Self {
        Self {
            states: HashMap::new(),
            bots: RefCell::default(),
            app_actions: super::app_actions::AppActions::default(),
        }
    }
}

impl ChatView {
    /// Whether this chat's cards take answers and ask their bot: the account's cloud serves bot invokes
    /// (`Capabilities::apps`); else the cards are only read and drawn.
    pub(super) fn asks_bots(&self) -> bool {
        self.core.capabilities().apps
    }

    /// Where a message's bot is asked and its user's answers go: a channel's post's chain, else the view's own place.
    pub(super) fn bot_conversation(&self, message: &str) -> String {
        match &self.place {
            Place::Channel => teams_proto::reply_chain_id(&self.conversation, self.root_of(message))
                .unwrap_or_else(|| self.conversation.clone()),
            place => writes_to(&self.conversation, place),
        }
    }

    /// `item`'s bot state, made when first asked for, and made again when its cards changed; a card that names the
    /// account then refreshes by itself.
    pub(super) fn card_bot(&self, item: &MessageItem, cx: &mut Context<Self>) -> Option<Entity<CardBot>> {
        if !self.asks_bots() {
            return None;
        }
        let bots = &self.held_cards.bots;
        let cards = cards_of(item);
        let old = {
            let mut held = bots.borrow_mut();
            match held.get_mut(&item.id) {
                Some(entry) if entry.version == item.version => return Some(entry.bot.clone()),
                Some(entry) if entry.cards == cards => {
                    entry.version = item.version;
                    return Some(entry.bot.clone());
                }
                Some(entry) => Some(entry.bot.clone()),
                None => None,
            }
        };
        // a changed message's old state goes, and an answer to an ask of its cards with it
        let bot = if let Some(old) = old {
            let renewed = old.read(cx).renewed();
            cx.new(|_| renewed)
        } else {
            let target = Target::of_sender(item.sender.as_deref(), &self.bot_conversation(&item.id));
            let core = std::sync::Arc::clone(&self.core);
            let id = item.id.clone();
            cx.new(|_| CardBot::new(core, id, target))
        };
        // a `task/fetch` button's dialog opens over the window
        self.follow_card_dialogs(&bot, &item.id, item.sender_name.to_string(), cx);
        // what its bot answered shows at once
        cx.observe(&bot, |_, _, cx| cx.notify()).detach();
        bots.borrow_mut().insert(
            item.id.clone(),
            HeldBot {
                bot: bot.clone(),
                version: item.version,
                cards: cards.to_vec(),
            },
        );
        if !self.core.read_only() {
            let me = self.core.me().to_owned();
            for (at, card) in cards.iter().enumerate() {
                if let Some(refresh) = card
                    .refresh
                    .as_ref()
                    .filter(|refresh| CardBot::refreshes_for(refresh, &me))
                {
                    let refresh = refresh.clone();
                    bot.update(cx, |bot, cx| bot.refresh(at, &refresh, true, cx));
                }
            }
        }
        Some(bot)
    }

    /// What `item`'s cards are drawn with besides their state: its bot state.
    pub(super) fn bot_hook(&self, item: &MessageItem, cx: &mut Context<Self>) -> Option<BotHook> {
        Some(BotHook {
            bot: self.card_bot(item, cx)?,
            read_only: self.core.read_only(),
        })
    }

    /// The `refresh` of `message`'s cards, run by hand: each card shown that has one.
    pub fn refresh_card(&mut self, message: &str, cx: &mut Context<Self>) {
        self.options = None;
        let Some(item) = self.message_items().find(|item| item.id == message).cloned() else {
            return;
        };
        let Some(bot) = self.card_bot(&item, cx) else {
            return;
        };
        for (at, card) in shown_cards(&item, Some(&bot), cx).iter().enumerate() {
            if let Some(refresh) = &card.refresh {
                bot.update(cx, |bot, cx| bot.refresh(at, refresh, false, cx));
            }
        }
        cx.notify();
    }

    /// What a click on button `action` of card `card` of `message` does (a button of the card itself, not of a card a
    /// `ShowCard` opened), when it asks the bot: the card shown (the bot's, when it put one in the place), its answers
    /// checked, then the bot asked. A button that doesn't ask the bot (read-only mode) does nothing here.
    pub fn press_card_action(&mut self, message: &str, card: usize, action: usize, cx: &mut Context<Self>) {
        let Some(item) = self.message_items().find(|item| item.id == message).cloned() else {
            return;
        };
        let Some(bot) = self.card_bot(&item, cx) else {
            return;
        };
        let Some(shown) = shown_cards(&item, Some(&bot), cx).get(card).cloned() else {
            return;
        };
        let Some(ActionKind::Submit(pressed)) = shown.actions.get(action).map(|action| &action.kind) else {
            return;
        };
        let reaches = bot.read(cx).target().is_some();
        if !crate::bot_card::asks(reaches, self.core.read_only()) {
            return;
        }
        let mut inputs = Vec::new();
        crate::bot_card::card_inputs(&shown, false, &mut inputs);
        let key = format!("c{card}a{action}");
        bot.update(cx, |bot, cx| bot.press(card, &key, pressed, &inputs, cx));
    }

    /// "Refresh card" for the more-options menu of `message`, when the app writes and a card of
    /// the message has a `refresh`.
    pub(super) fn refresh_entry(&self, message: &str, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let item = self.message_items().find(|item| item.id == message)?;
        let offered = self.asks_bots() && !self.core.read_only() && !item.actions().is_empty() && {
            let bot = self.held_cards.bots.borrow().get(message).map(|held| held.bot.clone());
            shown_cards(item, bot.as_ref(), cx)
                .iter()
                .any(|card| card.refresh.is_some())
        };
        if !offered {
            return None;
        }
        let message = message.to_owned();
        let row = div()
            .id("action-refresh-card")
            .debug_selector(|| "action-refresh-card".to_owned())
            .w_full()
            .h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .hover(move |row| row.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.refresh_card(&message, cx)))
            .child(Glyph::REFRESH.icon().size(px(14.0)))
            .child(div().flex_1().child(REFRESH_CARD));
        Some(crate::keyboard::stop("action-refresh-card", row, palette).into_any_element())
    }

    /// The newest message's suggested actions over the compose box (see the module docs).
    pub(super) fn suggested_actions(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.asks_bots() || self.core.read_only() || teams_proto::one_to_one_pair(&self.conversation).is_none() {
            return None;
        }
        let item = self.message_items().last()?;
        let Body::Cards(cards) = &item.body else {
            return None;
        };
        let from_bot = item.sender.as_deref().is_some_and(|sender| sender.starts_with("28:"));
        if !from_bot || cards.0.suggested.is_empty() {
            return None;
        }
        let bot = self.card_bot(item, cx)?;
        let shown = bot.read(cx).shown();
        let busy = shown.busy.get(&SUGGESTED).cloned();
        let line = shown.lines.get(&SUGGESTED).map(|line| {
            let in_teams = teams_proto::workflow::MessageRun::link_to(&self.conversation, &item.id, None);
            bot_line(palette, "suggested-answer".to_owned(), line, Some(&in_teams))
        });
        let buttons = cards.0.suggested.iter().enumerate().filter_map(|(at, action)| {
            let ActionKind::Submit(sent) = &action.kind else {
                return None;
            };
            let (bot, sent, key) = (bot.clone(), sent.clone(), format!("suggested-{at}"));
            let id = SharedString::from(key.clone());
            let selector = key.clone();
            let spinning = busy.as_deref() == Some(key.as_str());
            Some(
                div().debug_selector(move || selector).child(
                    Button::new(ElementId::Name(id))
                        .label(action.title.clone())
                        .outline()
                        .rounded(ButtonRounded::Large)
                        .small()
                        .spinning(spinning)
                        .disabled(busy.is_some())
                        .on_click(move |_, _, cx| {
                            bot.update(cx, |bot, cx| bot.press(SUGGESTED, &key, &sent, &[], cx));
                        }),
                ),
            )
        });
        let bar = div()
            .id("suggested-actions")
            .debug_selector(|| "suggested-actions".to_owned())
            .w_full()
            .flex()
            .flex_wrap()
            .justify_end()
            .gap_2()
            .text_color(rgb(palette.foreground))
            .children(buttons);
        Some(
            div()
                .w_full()
                .px_4()
                .pb_2()
                .flex()
                .flex_col()
                .items_end()
                .gap_1()
                .child(bar)
                .children(line)
                .into_any_element(),
        )
    }
}

/// `item`'s cards as they are shown: in a card's place, the card its bot (`bot`) put there.
fn shown_cards(item: &MessageItem, bot: Option<&Entity<CardBot>>, cx: &App) -> Vec<Arc<Card>> {
    cards_of(item)
        .iter()
        .enumerate()
        .map(|(at, card)| {
            bot.and_then(|bot| bot.read(cx).replaced(at))
                .unwrap_or_else(|| Arc::new(card.clone()))
        })
        .collect()
}

/// `item`'s cards, when it is a card message.
fn cards_of(item: &MessageItem) -> &[Card] {
    match &item.body {
        Body::Cards(cards) => &cards.0.cards,
        _ => &[],
    }
}
