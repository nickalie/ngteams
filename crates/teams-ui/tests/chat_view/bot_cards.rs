//! A bot's card with the bots shown (`NGTEAMS_BOTS=1`, `teams_ui::bot_card`), on GPUI's test context over the fake
//! engine: its inputs drawn, their checks, a button that asks the bot with the answers, each answer shown (a card in
//! the card's place, a message, an error, a sign-in request, a bot not reached), `imBack` posting the user's message,
//! the refresh by itself and by hand, the suggested actions over the compose box, and read-only mode, which asks
//! nothing. Every card, bot and id here is synthetic.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{Bounds, Entity, Modifiers, Pixels, VisualTestContext, px, size};
use serde_json::{Value, json};
use teams_core::{Action, EngineError, Event};
use teams_proto::bot::TaskDialog;
use teams_proto::{InvokeAnswer, InvokeKind};
use teams_store::Message;
use teams_ui::bots_flag::BotsFlag;
use teams_ui::chat_view::{ChatView, ChatViewEvent};

use super::common::{LAB_A, Rig};
use super::{AB, NOON, chat_window, draw, message, rig_with, trace};

/// A made-up bot.
const BOT: &str = "28:00000000-0000-4000-8000-0000000000b0";
const ID: &str = "601";

/// A card message from the bot carrying `activity`'s attachments and suggested actions.
fn bot_message(id: &str, activity: &Value) -> Message {
    let b64 = STANDARD.encode(serde_json::to_vec(activity).unwrap());
    let content = format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    );
    let mut stored = message(id, BOT, &content, NOON);
    stored.message_type = "RichText/Media_Card".to_owned();
    stored.sender_name = Some("Synthetic Bot".to_owned());
    stored
}

fn card_message(id: &str, card: &Value) -> Message {
    bot_message(
        id,
        &json!({"type": "message/card", "attachments": [
            {"contentType": "application/vnd.microsoft.card.adaptive", "content": card}
        ]}),
    )
}

/// The rig with `messages`, the bots shown.
fn bots_rig(messages: Vec<Message>) -> Rig {
    let rig = rig_with(messages);
    rig.cx.update(|cx| cx.set_global(BotsFlag(true)));
    rig
}

/// The chat view in a window tall enough for the whole order form.
fn tall_window(rig: &Rig) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(rig, written_trace);
    cx.simulate_resize(size(px(800.0), px(1400.0)));
    draw(cx);
    (view, cx)
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let at = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(at.center(), Modifiers::none());
    draw(cx);
}

fn input(id: &str) -> String {
    format!("card-{ID}-c0-in-{id}")
}

/// A card asking for a name (required) and more, with a plain submit and an `Action.Execute`.
fn form_card() -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Order lunch", "weight": "bolder"},
            {"type": "Input.Text", "id": "name", "label": "Your name", "isRequired": true,
             "errorMessage": "Enter your name"},
            {"type": "Input.Text", "id": "note", "label": "Note", "isMultiline": true, "placeholder": "Anything else?"},
            {"type": "Input.Number", "id": "count", "label": "How many", "min": 1, "max": 9, "value": 1},
            {"type": "Input.Date", "id": "day", "label": "Day", "value": "2026-11-20"},
            {"type": "Input.Time", "id": "at", "label": "Time"},
            {"type": "Input.Toggle", "id": "veg", "title": "Vegetarian", "valueOn": "yes", "valueOff": "no"},
            {"type": "Input.ChoiceSet", "id": "dish", "label": "Dish", "value": "soup", "choices": [
                {"title": "Soup", "value": "soup"}, {"title": "Salad", "value": "salad"}
            ]},
            {"type": "Input.ChoiceSet", "id": "size", "label": "Size", "style": "expanded", "choices": [
                {"title": "Small", "value": "s"}, {"title": "Large", "value": "l"}
            ]},
            {"type": "Input.ChoiceSet", "id": "extras", "label": "Extras", "style": "expanded",
             "isMultiSelect": true, "choices": [
                {"title": "Bread", "value": "bread"}, {"title": "Juice", "value": "juice"}
            ]}
        ],
        "actions": [
            {"type": "Action.Submit", "title": "Send", "data": {"form": "lunch"}},
            {"type": "Action.Execute", "title": "Order", "verb": "order", "data": {"form": "lunch"}}
        ]
    })
}

#[test]
fn with_the_bots_shown_each_input_of_a_card_is_drawn() {
    let rig = bots_rig(vec![card_message(ID, &form_card())]);
    let (_view, cx) = tall_window(&rig);
    for id in [
        "name",
        "note",
        "count",
        "day",
        "at",
        "veg",
        "dish",
        "size-s",
        "extras-juice",
    ] {
        assert!(bounds(cx, &input(id)).is_some(), "{id} is drawn");
    }
    // a text input takes the card's width, as Teams draws it
    let card = bounds(cx, &format!("card-{ID}-c0")).unwrap();
    let name = bounds(cx, &input("name")).unwrap();
    assert!(name.size.width >= card.size.width - px(40.0), "{name:?} in {card:?}");
    assert_eq!(card.size.width, px(teams_ui::card_view::CARD_WIDTH));
    // the card says nothing of content it can't show, and its buttons ask the bot (no link out)
    assert!(bounds(cx, &format!("card-{ID}-c0unshown")).is_none());
    assert!(bounds(cx, &format!("card-{ID}-c0a0")).is_some());
}

#[test]
fn a_required_input_left_empty_stops_the_send_and_says_why() {
    let rig = bots_rig(vec![card_message(ID, &form_card())]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &format!("{}-error", input("name"))).is_some());
    assert!(rig.core.bot_asks.lock().unwrap().is_empty(), "nothing goes");
    click(cx, &input("name"));
    cx.simulate_input("Ada");
    draw(cx);
    // typing takes the mark away
    assert!(bounds(cx, &format!("{}-error", input("name"))).is_none());
    click(cx, &input("veg"));
    click(cx, &input("size-l"));
    click(cx, &input("extras-juice"));
    click(cx, &format!("card-{ID}-c0a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].bot.as_str(), BOT);
    assert_eq!(asks[0].conversation.as_deref(), Some(AB));
    let InvokeKind::Submit(sent) = &asks[0].kind else {
        panic!("a plain submit: {:?}", asks[0].kind);
    };
    assert_eq!(sent.message.as_deref(), Some(ID));
    assert_eq!(
        sent.data,
        json!({"form": "lunch", "name": "Ada", "count": "1", "day": "2026-11-20", "veg": "yes", "dish": "soup",
               "size": "l", "extras": "juice"})
    );
}

/// The chat list's own keys (`a` reacts, `1` picks a reaction, `r` replies) are text in a card's input.
#[test]
fn the_chat_lists_keys_are_text_in_a_cards_input() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "said"}],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("said"));
    cx.simulate_input("a1 era");
    click(cx, &format!("card-{ID}-c0a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(
        matches!(&asks[..], [ask] if matches!(&ask.kind, InvokeKind::Submit(sent) if sent.data == json!({"said": "a1 era"}))),
        "{asks:?}"
    );
}

#[test]
fn a_number_out_of_its_range_stops_the_send() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Number", "id": "count", "min": 1, "max": 9}],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("count"));
    cx.simulate_input("12");
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &format!("{}-error", input("count"))).is_some());
    assert!(rig.core.bot_asks.lock().unwrap().is_empty());
}

#[test]
fn an_execute_puts_the_bots_card_in_the_cards_place() {
    let rig = bots_rig(vec![card_message(ID, &form_card())]);
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(InvokeAnswer::Card(json!({
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "Ordered"}]
        }))));
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("name"));
    cx.simulate_input("Ada");
    click(cx, &format!("card-{ID}-c0a1"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let InvokeKind::Execute(sent) = &asks[0].kind else {
        panic!("an execute: {:?}", asks[0].kind);
    };
    assert_eq!(sent.verb.as_deref(), Some("order"));
    assert!(!sent.automatic);
    // the card is the bot's now: one text, no input, no button
    assert!(bounds(cx, &format!("card-{ID}-c0b0.")).is_some());
    assert!(bounds(cx, &input("name")).is_none());
    assert!(bounds(cx, &format!("card-{ID}-c0a0")).is_none());
}

#[test]
fn the_bots_message_error_and_sign_in_request_show_under_the_card() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Ask"}],
        "actions": [{"type": "Action.Execute", "title": "Go", "verb": "go"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let answers = [
        Ok(InvokeAnswer::Message("Saved".to_owned())),
        Ok(InvokeAnswer::Error {
            code: Some("BadRequest".to_owned()),
            message: "No lunch today".to_owned(),
        }),
        Ok(InvokeAnswer::SignIn(Value::Null)),
        Ok(InvokeAnswer::Dialog(TaskDialog {
            title: None,
            card: None,
            url: Some("https://example.invalid/dialog".to_owned()),
            width: None,
            height: None,
        })),
        Err(EngineError::BotNotReached {
            why: "made up".to_owned(),
        }),
        Ok(InvokeAnswer::Done),
    ];
    rig.core.bot_answers.lock().unwrap().extend(answers);
    let (_view, cx) = tall_window(&rig);
    let line = format!("card-{ID}-c0answer");
    for at in 0..5 {
        click(cx, &format!("card-{ID}-c0a0"));
        assert!(bounds(cx, &line).is_some());
        // a sign-in request or a dialog goes on in Teams on the web
        if at == 3 {
            click(cx, &line);
            assert_eq!(
                cx.opened_url(),
                Some(teams_proto::workflow::MessageRun::link_to(AB, ID, None))
            );
        }
    }
    // a bot that answers nothing leaves no line
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &line).is_none());
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 6);
}

#[test]
fn an_im_back_posts_the_users_message_and_asks_nothing() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Pick"}],
        "actions": [{"type": "Action.Submit", "title": "Red",
                     "data": {"msteams": {"type": "imBack", "value": "red please"}}}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(rig.core.bot_asks.lock().unwrap().is_empty());
    let actions = rig.core.actions.lock().unwrap().clone();
    assert!(
        actions.iter().any(|action| matches!(
            action,
            Action::Send { conversation, text } if conversation == AB && text.html().contains("red please")
        )),
        "{actions:?}"
    );
}

#[test]
fn read_only_mode_asks_no_bot() {
    let rig = bots_rig(vec![card_message(ID, &form_card())]);
    *rig.core.read_only.lock().unwrap() = true;
    let (_view, cx) = tall_window(&rig);
    assert!(bounds(cx, &input("name")).is_some(), "the inputs show");
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(rig.core.bot_asks.lock().unwrap().is_empty());
    // the button opens the message in Teams on the web, as without the bots
    assert_eq!(
        cx.opened_url(),
        Some(teams_proto::workflow::MessageRun::link_to(AB, ID, None))
    );
}

fn refresh_card(users: &[&str]) -> Value {
    json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Status"}],
        "refresh": {"action": {"type": "Action.Execute", "verb": "status", "data": {"k": 1}}, "userIds": users}
    })
}

#[test]
fn a_refresh_that_names_the_account_runs_by_itself_once() {
    let rig = bots_rig(vec![card_message(ID, &refresh_card(&[LAB_A]))]);
    let (_view, cx) = tall_window(&rig);
    draw(cx);
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert!(
        matches!(&asks[0].kind, InvokeKind::Execute(sent) if sent.automatic && sent.verb.as_deref() == Some("status"))
    );
}

#[test]
fn refresh_card_in_more_options_runs_it_by_hand() {
    let rig = bots_rig(vec![card_message(ID, &refresh_card(&[]))]);
    let (view, cx) = tall_window(&rig);
    assert!(
        rig.core.bot_asks.lock().unwrap().is_empty(),
        "it names no one: nothing by itself"
    );
    view.update(cx, |view, cx| view.toggle_options(ID, cx));
    click(cx, "action-refresh-card");
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(matches!(&asks[..], [ask] if matches!(&ask.kind, InvokeKind::Execute(sent) if !sent.automatic)));
}

#[test]
fn without_the_bots_no_refresh_is_offered_or_run() {
    let rig = rig_with(vec![card_message(ID, &refresh_card(&[LAB_A]))]);
    let (view, cx) = tall_window(&rig);
    view.update(cx, |view, cx| view.toggle_options(ID, cx));
    assert!(bounds(cx, "action-refresh-card").is_none());
    assert!(rig.core.bot_asks.lock().unwrap().is_empty());
}

#[test]
fn where_the_cloud_serves_no_bots_a_card_asks_nothing() {
    for card in [form_card(), refresh_card(&[LAB_A])] {
        let rig = bots_rig(vec![card_message(ID, &card)]);
        *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
            apps: false,
            ..teams_core::Capabilities::ALL
        });
        let (view, cx) = tall_window(&rig);
        // the card is drawn as without the bots: no input, no button that sends to the bot, no refresh
        assert!(bounds(cx, &input("name")).is_none());
        assert!(bounds(cx, &format!("card-{ID}-c0a0")).is_none());
        view.update(cx, |view, cx| view.toggle_options(ID, cx));
        assert!(bounds(cx, "action-refresh-card").is_none());
        assert!(rig.core.bot_asks.lock().unwrap().is_empty());
    }
}

fn suggesting() -> Message {
    bot_message(
        ID,
        &json!({
            "type": "message/card",
            "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive",
                             "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Colour?"}]}}],
            "suggestedActions": {"actions": [
                {"type": "imBack", "title": "Red", "value": "red"},
                {"type": "messageBack", "title": "Blue", "text": "blue", "value": {"c": "blue"}}
            ]}
        }),
    )
}

#[test]
fn suggested_actions_stand_over_the_compose_box_and_answer_the_bot() {
    let rig = bots_rig(vec![suggesting()]);
    let (_view, cx) = tall_window(&rig);
    let compose = bounds(cx, "compose-toolbar").expect("the compose box");
    let bar = bounds(cx, "suggested-actions").expect("the suggested actions");
    assert!(bar.bottom() <= compose.top(), "{bar:?} over {compose:?}");
    click(cx, "suggested-0");
    assert!(rig.core.actions.lock().unwrap().iter().any(|action| matches!(
        action,
        Action::Send { text, .. } if text.html().contains("red")
    )));
    click(cx, "suggested-1");
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(
        matches!(&asks[..], [ask] if matches!(&ask.kind, InvokeKind::Submit(sent) if sent.data == json!({"c": "blue"})))
    );
}

#[test]
fn suggested_actions_go_once_someone_writes_again() {
    let rig = bots_rig(vec![
        suggesting(),
        message("602", super::common::LAB_B, "<p>later</p>", NOON + 1),
    ]);
    let (_view, cx) = tall_window(&rig);
    assert!(bounds(cx, "suggested-actions").is_none());
}

#[test]
fn a_choice_and_a_time_come_from_their_menus() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Input.ChoiceSet", "id": "dish", "placeholder": "Pick a dish", "choices": [
                {"title": "Soup", "value": "soup"}, {"title": "Salad", "value": "salad"}
            ]},
            {"type": "Input.ChoiceSet", "id": "sides", "isMultiSelect": true, "choices": [
                {"title": "Bread", "value": "bread"}, {"title": "Juice", "value": "juice"}
            ]},
            {"type": "Input.Time", "id": "at"}
        ],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("dish"));
    click(cx, &format!("{}-salad", input("dish")));
    for side in ["bread", "juice"] {
        click(cx, &input("sides"));
        click(cx, &format!("{}-{side}", input("sides")));
    }
    click(cx, &format!("{}-times", input("at")));
    click(cx, &format!("{}-times-130AM", input("at")));
    click(cx, &format!("card-{ID}-c0a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert!(
        matches!(&asks[..], [ask] if matches!(&ask.kind, InvokeKind::Submit(sent)
            if sent.data == json!({"dish": "salad", "sides": "bread,juice", "at": "01:30"}))),
        "{asks:?}"
    );
}

#[test]
fn a_message_back_posts_its_display_text_and_asks_the_bot() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Join?"}],
        "actions": [{"type": "Action.Submit", "title": "Join", "data": {"msteams": {
            "type": "messageBack", "displayText": "I'm in", "text": "join", "value": {"join": true}
        }}}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    let posted: Vec<String> = rig
        .core
        .actions
        .lock()
        .unwrap()
        .iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(text.html().to_owned()),
            _ => None,
        })
        .collect();
    // its displayText is posted ("I'm in", its apostrophe maybe escaped), never its text for the bot
    assert!(
        matches!(&posted[..], [html] if html.contains("m in") && !html.contains("join")),
        "{posted:?}"
    );
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let [ask] = &asks[..] else {
        panic!("one ask: {asks:?}");
    };
    let InvokeKind::Submit(sent) = &ask.kind else {
        panic!("a submit: {:?}", ask.kind);
    };
    assert_eq!(sent.text.as_deref(), Some("join"));
    assert_eq!(sent.data["msteams"]["value"], json!({"join": true}));
}

/// Two messageBack buttons with one value and different texts send different texts.
#[test]
fn a_hero_message_back_sends_its_text_with_its_value() {
    let activity = json!({"type": "message/card", "attachments": [{
        "contentType": "application/vnd.microsoft.card.hero",
        "content": {"title": "Vote", "buttons": [
            {"type": "messageBack", "title": "Up", "text": "up", "value": {"poll": 1}},
            {"type": "messageBack", "title": "Down", "text": "down", "value": {"poll": 1}}
        ]}
    }]});
    let rig = bots_rig(vec![bot_message(ID, &activity)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    click(cx, &format!("card-{ID}-c0a1"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let sent: Vec<(Option<String>, Value)> = asks
        .iter()
        .filter_map(|ask| match &ask.kind {
            InvokeKind::Submit(sent) => Some((sent.text.clone(), sent.data.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent,
        [
            (Some("up".to_owned()), json!({"poll": 1})),
            (Some("down".to_owned()), json!({"poll": 1}))
        ]
    );
}

/// A typed time that can't be read as one fails, though the input is optional: nothing goes, and it is marked.
#[test]
fn an_unreadable_optional_time_stops_the_send() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Time", "id": "at"}],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("at"));
    cx.simulate_input("abc");
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &format!("{}-error", input("at"))).is_some());
    assert!(rig.core.bot_asks.lock().unwrap().is_empty(), "nothing goes");
}

/// A message of cards, each a card of its own.
fn cards_message(id: &str, cards: &[Value]) -> Message {
    let attachments: Vec<Value> = cards
        .iter()
        .map(|card| json!({"contentType": "application/vnd.microsoft.card.adaptive", "content": card}))
        .collect();
    bot_message(id, &json!({"type": "message/card", "attachments": attachments}))
}

/// A card that refreshes with `verb` for the account.
fn refreshing(verb: &str) -> Value {
    json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Status"}],
        "refresh": {"action": {"type": "Action.Execute", "verb": verb}, "userIds": [LAB_A]}
    })
}

/// The verbs of the `Action.Execute`s asked so far, in order.
fn verbs(rig: &Rig) -> Vec<String> {
    rig.core
        .bot_asks
        .lock()
        .unwrap()
        .iter()
        .filter_map(|ask| match &ask.kind {
            InvokeKind::Execute(sent) => sent.verb.clone(),
            _ => None,
        })
        .collect()
}

/// An ask of one card doesn't hold back the others of its message.
#[test]
fn each_card_of_a_message_refreshes_by_itself_and_by_hand() {
    let rig = bots_rig(vec![cards_message(ID, &[refreshing("one"), refreshing("two")])]);
    let (view, cx) = tall_window(&rig);
    draw(cx);
    assert_eq!(verbs(&rig), ["one", "two"]);
    view.update(cx, |view, cx| view.toggle_options(ID, cx));
    click(cx, "action-refresh-card");
    assert_eq!(verbs(&rig), ["one", "two", "one", "two"]);
}

/// A card the bot put in a card's place: its own `refresh` is the one offered and run by hand.
#[test]
fn refresh_card_runs_the_refresh_of_the_bots_card() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Ask"}],
        "actions": [{"type": "Action.Execute", "title": "Go", "verb": "go"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(InvokeAnswer::Card(json!({
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "Going"}],
            "refresh": {"action": {"type": "Action.Execute", "verb": "status"}, "userIds": []}
        }))));
    let (view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    view.update(cx, |view, cx| view.toggle_options(ID, cx));
    click(cx, "action-refresh-card");
    assert_eq!(verbs(&rig), ["go", "status"]);
}

/// The bot edits its message: the card it put in the place and the answers typed go, the new card shows.
#[test]
fn an_edit_of_the_message_drops_the_bots_card_and_the_answers() {
    let first = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "said"}],
        "actions": [{"type": "Action.Execute", "title": "Approve", "verb": "approve"}]
    });
    let rig = bots_rig(vec![card_message(ID, &first)]);
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(InvokeAnswer::Card(json!({
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "Approved by A"}]
        }))));
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("said"));
    cx.simulate_input("typed");
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &input("said")).is_none(), "the bot's card is shown");
    let edited = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "said"}],
        "actions": [{"type": "Action.Execute", "title": "Reopen", "verb": "reopen"}]
    });
    edit(&rig, card_message(ID, &edited), 2);
    draw(cx);
    // the edited card shows, its input empty again
    assert!(bounds(cx, &input("said")).is_some());
    click(cx, &format!("card-{ID}-c0a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let Some(InvokeKind::Execute(sent)) = asks.last().map(|ask| &ask.kind) else {
        panic!("an execute: {asks:?}");
    };
    assert_eq!(sent.verb.as_deref(), Some("reopen"));
    assert_eq!(sent.data, json!({}), "the answer typed in the old card is gone");
}

/// The bot edits its message: `stored` (version `version`) takes the place of the chat's first message.
fn edit(rig: &Rig, mut stored: Message, version: i64) {
    stored.version = version;
    rig.core.messages.lock().unwrap().get_mut(AB).unwrap()[0] = stored;
    rig.send(Event::ConversationChanged {
        conversation: AB.to_owned(),
    });
}

/// A card with an input `said` and an `Action.Execute` of `verb`.
fn asking(verb: &str) -> Value {
    json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "said"}],
        "actions": [{"type": "Action.Execute", "title": "Go", "verb": verb}]
    })
}

/// The answer the bot holds back first, given now.
fn answer_held(rig: &Rig, answer: InvokeAnswer) {
    let held = rig.core.held_bot_answers.lock().unwrap().remove(0);
    held.send(Ok(answer));
}

/// The bot answered no card: an edit still drops the answer typed and the bot's line.
#[test]
fn an_edit_drops_the_typed_answers_and_the_line() {
    let rig = bots_rig(vec![card_message(ID, &asking("approve"))]);
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(InvokeAnswer::Message("Noted".to_owned())));
    let (_view, cx) = tall_window(&rig);
    click(cx, &input("said"));
    cx.simulate_input("typed");
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_some(), "the bot's line");
    edit(&rig, card_message(ID, &asking("reopen")), 2);
    draw(cx);
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_none(), "the line went");
    click(cx, &format!("card-{ID}-c0a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let Some(InvokeKind::Execute(sent)) = asks.last().map(|ask| &ask.kind) else {
        panic!("an execute: {asks:?}");
    };
    assert_eq!(sent.verb.as_deref(), Some("reopen"));
    assert_eq!(sent.data, json!({}), "the answer typed in the old card is gone");
}

/// A card that names the account refreshes by itself again for each new version of the message's cards.
#[test]
fn an_edited_card_refreshes_by_itself_again() {
    let rig = bots_rig(vec![card_message(ID, &refreshing("first"))]);
    let (_view, cx) = tall_window(&rig);
    draw(cx);
    assert_eq!(verbs(&rig), ["first"]);
    edit(&rig, card_message(ID, &refreshing("second")), 2);
    draw(cx);
    draw(cx);
    assert_eq!(verbs(&rig), ["first", "second"], "once for the edited card");
}

/// While card 0's ask goes, its button waits; the input and the button of card 1 don't.
#[test]
fn an_ask_holds_back_only_its_own_card() {
    let rig = bots_rig(vec![cards_message(ID, &[asking("one"), asking("two")])]);
    *rig.core.hold_bot_answers.lock().unwrap() = true;
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(verbs(&rig), ["one"], "card 0 waits for its ask");
    click(cx, &format!("card-{ID}-c1-in-said"));
    cx.simulate_input("typed");
    click(cx, &format!("card-{ID}-c1a0"));
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert_eq!(verbs(&rig), ["one", "two"]);
    assert!(
        matches!(&asks[1].kind, InvokeKind::Execute(sent) if sent.data == json!({"said": "typed"})),
        "{asks:?}"
    );
}

/// An answer to an ask of the cards before an edit shows nowhere: the edited card stays.
#[test]
fn a_late_answer_to_an_ask_before_an_edit_shows_nowhere() {
    let rig = bots_rig(vec![card_message(ID, &refresh_card(&[]))]);
    *rig.core.hold_bot_answers.lock().unwrap() = true;
    let (view, cx) = tall_window(&rig);
    view.update(cx, |view, cx| view.toggle_options(ID, cx));
    click(cx, "action-refresh-card");
    assert_eq!(verbs(&rig), ["status"]);
    edit(&rig, card_message(ID, &asking("reopen")), 2);
    draw(cx);
    answer_held(
        &rig,
        InvokeAnswer::Card(json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Old"}]})),
    );
    draw(cx);
    assert!(bounds(cx, &input("said")).is_some(), "the edited card stays");
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_none());
}

/// A suggested action asks in a place of its own: card 0 doesn't wait for it, and its line shows under the actions.
#[test]
fn a_suggested_action_waits_alone_and_answers_under_the_actions() {
    let rig = bots_rig(vec![bot_message(
        ID,
        &json!({
            "type": "message/card",
            "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": asking("card")}],
            "suggestedActions": {"actions": [
                {"type": "messageBack", "title": "Blue", "text": "blue", "value": {"c": "blue"}}
            ]}
        }),
    )]);
    *rig.core.hold_bot_answers.lock().unwrap() = true;
    let (_view, cx) = tall_window(&rig);
    click(cx, "suggested-0");
    click(cx, "suggested-0");
    assert_eq!(
        rig.core.bot_asks.lock().unwrap().len(),
        1,
        "the action waits for its ask"
    );
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(verbs(&rig), ["card"], "the card doesn't wait");
    answer_held(&rig, InvokeAnswer::Message("Blue it is".to_owned()));
    draw(cx);
    assert!(bounds(cx, "suggested-answer").is_some());
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_none());
}

/// A send that fails its checks on card 0 keeps its marks when a button of card 1 sends.
#[test]
fn the_error_marks_of_one_card_stay_when_another_card_sends() {
    let required = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "name", "isRequired": true, "errorMessage": "Enter your name"}],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![cards_message(ID, &[required, asking("two")])]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    let error = format!("{}-error", input("name"));
    assert!(bounds(cx, &error).is_some());
    click(cx, &format!("card-{ID}-c1a0"));
    assert_eq!(verbs(&rig), ["two"]);
    assert!(bounds(cx, &error).is_some(), "card 0's mark stays");
}

#[test]
fn a_sign_in_button_opens_its_page_and_a_dialog_button_asks_for_its_dialog() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Connect"}],
        "actions": [
            {"type": "Action.Submit", "title": "Sign in",
             "data": {"msteams": {"type": "signin", "value": "https://example.invalid/sign-in"}}},
            {"type": "Action.Submit", "title": "Details", "data": {"msteams": {"type": "task/fetch"}, "item": 7}}
        ]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let dialog = TaskDialog {
        title: Some("Details".to_owned()),
        card: Some(json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "More"}]})),
        url: None,
        width: None,
        height: None,
    };
    rig.core
        .bot_answers
        .lock()
        .unwrap()
        .push_back(Ok(InvokeAnswer::Dialog(dialog.clone())));
    let (view, cx) = tall_window(&rig);
    let told: std::rc::Rc<std::cell::RefCell<Vec<ChatViewEvent>>> = std::rc::Rc::default();
    let kept = std::rc::Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.invalid/sign-in"));
    click(cx, &format!("card-{ID}-c0a1"));
    cx.run_until_parked();
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    let [ask] = asks.as_slice() else {
        panic!("one ask: {asks:?}");
    };
    let InvokeKind::TaskFetch(fetched) = &ask.kind else {
        panic!("a task/fetch: {ask:?}");
    };
    assert_eq!(fetched.message.as_deref(), Some(ID));
    assert_eq!(fetched.data["item"], 7);
    // the dialog opens over the window, from the card's message
    let told = told.borrow();
    let Some(ChatViewEvent::AppDialog(seed)) = told.last() else {
        panic!("a dialog: {told:?}");
    };
    assert_eq!(
        seed.opened,
        teams_ui::app_dialog::Opened::Task {
            dialog,
            message: Some(ID.to_owned())
        }
    );
    assert_eq!(seed.target.bot.as_str(), BOT);
}

/// A made-up label of about 150 characters, longer than a card is wide.
const LONG: &str = "A made-up question with a very long label that goes on past the width of the card, so that it must \
                    wrap onto a second and maybe a third line without cutting a word";

#[test]
fn a_long_label_placeholder_and_error_wrap_inside_the_card() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Input.Text", "id": "said", "label": LONG, "isRequired": true, "errorMessage": LONG,
             "placeholder": LONG},
            {"type": "Input.ChoiceSet", "id": "pick", "label": LONG, "placeholder": LONG, "choices": [
                {"title": "Soup", "value": "soup"}
            ]}
        ],
        "actions": [{"type": "Action.Submit", "title": "Send"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    let card = bounds(cx, &format!("card-{ID}-c0")).unwrap();
    for part in ["said-label", "said-error-text", "said", "pick-label", "pick"] {
        let drawn = bounds(cx, &input(part)).unwrap_or_else(|| panic!("{part} is drawn"));
        assert!(
            drawn.right() <= card.right(),
            "{part} {drawn:?} stays in the card {card:?}"
        );
        assert!(drawn.size.height > px(0.0), "{part} {drawn:?}");
    }
    // the label wraps: it is taller than one line of its input under it
    let label = bounds(cx, &input("said-label")).unwrap();
    assert!(label.size.height > px(30.0), "{label:?}");
}

#[test]
fn a_narrow_cards_answer_line_wraps_under_it_and_the_card_keeps_its_width() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Ask"}],
        "actions": [{"type": "Action.Execute", "title": "Go", "verb": "go"}]
    });
    let rig = bots_rig(vec![card_message(ID, &card)]);
    rig.core.bot_answers.lock().unwrap().extend([
        Ok(InvokeAnswer::Error {
            code: Some("BadRequest".to_owned()),
            message: LONG.to_owned(),
        }),
        Ok(InvokeAnswer::SignIn(Value::Null)),
    ]);
    let (_view, cx) = tall_window(&rig);
    let surface = format!("card-{ID}-c0");
    let before = bounds(cx, &surface).unwrap();
    assert!(before.size.width < px(300.0), "a narrow card: {before:?}");
    for _ in 0..2 {
        click(cx, &format!("card-{ID}-c0a0"));
        let after = bounds(cx, &surface).unwrap();
        let line = bounds(cx, &format!("card-{ID}-c0answer")).unwrap();
        assert_eq!(after.size.width, before.size.width, "the card keeps its width");
        assert!(
            line.right() <= after.right(),
            "the line {line:?} wraps under the card {after:?}"
        );
        assert!(line.size.height > px(20.0), "the line wraps: {line:?}");
    }
}

/// An OAuth card as an SDK's sign-in prompt sends it.
fn oauth_message(id: &str) -> Message {
    bot_message(
        id,
        &json!({"type": "message/card", "attachments": [
            {"contentType": "application/vnd.microsoft.card.oauth", "content": {
                "text": "Sign in to Synthetic Tools", "connectionName": "tools",
                "buttons": [{"type": "signin", "title": "Sign in", "value": "https://example.invalid/sign-in"}]}}
        ]}),
    )
}

/// Types a code with a space in it, verifies it (refused: the field and a line stay), then verifies it again.
fn type_code_and_verify_twice(cx: &mut VisualTestContext, code: &str) {
    click(cx, code);
    cx.simulate_input("123 456");
    draw(cx);
    assert!(bounds(cx, &format!("{code}-error")).is_none(), "typing takes the mark away");
    click(cx, &format!("{code}-verify"));
    cx.run_until_parked();
    draw(cx);
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_some());
    assert!(bounds(cx, code).is_some());
    click(cx, &format!("{code}-verify"));
    cx.run_until_parked();
    draw(cx);
}

#[test]
fn a_sign_in_card_opens_its_page_then_sends_the_code_it_showed() {
    let rig = bots_rig(vec![oauth_message(ID)]);
    rig.core.bot_answers.lock().unwrap().extend([
        Ok(InvokeAnswer::Refused(404)),
        Ok(InvokeAnswer::Done),
    ]);
    let (_view, cx) = tall_window(&rig);
    let code = format!("card-{ID}-c0-code");
    assert!(bounds(cx, &code).is_none(), "no code field before the page opens");
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.invalid/sign-in"));
    assert!(bounds(cx, &code).is_some(), "the code field shows");
    // what isn't six digits goes nowhere
    click(cx, &format!("{code}-verify"));
    assert!(bounds(cx, &format!("{code}-error")).is_some());
    assert!(rig.core.bot_asks.lock().unwrap().is_empty());
    type_code_and_verify_twice(cx, &code);
    let asks = rig.core.bot_asks.lock().unwrap().clone();
    assert_eq!(asks.len(), 2);
    for ask in &asks {
        assert!(
            matches!(&ask.kind, InvokeKind::VerifyState { code } if code == "123456"),
            "{ask:?}"
        );
        assert_eq!(ask.bot.as_str(), BOT);
        assert_eq!(ask.conversation.as_deref(), Some(AB));
    }
    // taken: the field goes, the card says the user is signed in
    assert!(bounds(cx, &code).is_none());
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_some());
}

#[test]
fn a_code_stays_after_a_server_error_and_the_bots_error_then_signs_in() {
    let rig = bots_rig(vec![oauth_message(ID)]);
    rig.core.bot_answers.lock().unwrap().extend([
        Ok(InvokeAnswer::Refused(500)),
        Ok(InvokeAnswer::Error {
            code: None,
            message: " ".to_owned(),
        }),
        Ok(InvokeAnswer::Done),
    ]);
    let (_view, cx) = tall_window(&rig);
    let code = format!("card-{ID}-c0-code");
    click(cx, &format!("card-{ID}-c0a0"));
    type_code_and_verify_twice(cx, &code);
    assert!(bounds(cx, &code).is_some(), "the bot's error keeps the field");
    click(cx, &format!("{code}-verify"));
    cx.run_until_parked();
    draw(cx);
    assert_eq!(rig.core.bot_asks.lock().unwrap().len(), 3);
    assert!(bounds(cx, &code).is_none(), "the third answer signs the user in");
    assert!(bounds(cx, &format!("card-{ID}-c0answer")).is_some());
}

#[test]
fn without_the_bots_a_sign_in_card_opens_in_teams_and_offers_no_code() {
    let rig = rig_with(vec![oauth_message(ID)]);
    let (_view, cx) = tall_window(&rig);
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(
        cx.opened_url(),
        Some(teams_proto::workflow::MessageRun::link_to(AB, ID, None))
    );
    assert!(bounds(cx, &format!("card-{ID}-c0-code")).is_none());
}
