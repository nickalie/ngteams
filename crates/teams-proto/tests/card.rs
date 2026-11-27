//! A card message's cards (`teams_proto::chat::card`): the Swift payload read into the model, the markdown, the date
//! and time functions. Every card here is synthetic, in the shapes the Workflows bot's cards took.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use teams_proto::LinkPreviewPicture;
use teams_proto::chat::card::{
    ADAPTIVE, ActionKind, ActionStyle, BotAction, CardMessage, ChoiceStyle, ContainerStyle, DateFormat, HAlign,
    ImageSize, InputKind, Kind, Spacing, TextColor, TextInputStyle, TextPart, TextSize, TextWeight, VAlign, Via, Width,
    adaptive_card_with_inputs, card_message, card_message_with_inputs, input, markdown_html, message_cards,
    plain_words, text_parts, youtube_picture,
};
use teams_proto::chat::{Unshown, unshown_parts};

/// A `RichText/Media_Card` content carrying `activity`, as the chat service gives it.
fn content(activity: &Value) -> String {
    let b64 = STANDARD.encode(serde_json::to_vec(activity).unwrap());
    format!(
        "<URIObject type=\"SWIFT.1\" url_thumbnail=\"https://example.invalid/card.png\">Card - access it on \
         <a href=\"https://go.skype.com/cards.unsupported\">https://go.skype.com/cards.unsupported</a>. \
         <Title>Card</Title><Swift b64=\"{b64}\"></Swift><Description></Description></URIObject>"
    )
}

fn adaptive(card: &Value) -> CardMessage {
    let activity = json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}],
    });
    card_message(&content(&activity)).unwrap()
}

/// The webhook template's post: a bold title, its text and the footer the template adds.
#[test]
fn reads_the_webhook_templates_card() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "version": "1.2",
        "body": [
            {"type": "TextBlock", "text": "Build finished", "weight": "bolder", "wrap": true},
            {"type": "TextBlock", "text": "All checks passed.", "wrap": true},
            {"type": "Container", "separator": true, "items": [
                {"type": "TextBlock", "size": "small", "weight": "lighter", "wrap": true,
                 "text": "Sam Lab used a Workflow template to send this card. [Get template](https://teams.microsoft.com/l/task/x?url=y)"}
            ]}
        ]
    }));
    assert!(!message.unshown);
    let card = &message.cards[0];
    assert!(!card.unshown);
    assert_eq!(card.body.len(), 3);
    let Kind::Text(title) = &card.body[0].kind else {
        panic!("a text")
    };
    assert_eq!(title.text, "Build finished");
    assert_eq!(title.look.weight, TextWeight::Bolder);
    assert!(title.wrap);
    assert!(card.body[2].separator);
    let Kind::Container(footer) = &card.body[2].kind else {
        panic!("a container")
    };
    let Kind::Text(footer) = &footer.items[0].kind else {
        panic!("a text")
    };
    assert_eq!(footer.look.size, TextSize::Small);
    assert_eq!(footer.look.weight, TextWeight::Lighter);
    assert_eq!(
        markdown_html(&footer.text),
        "<p>Sam Lab used a Workflow template to send this card. \
         <a href=\"https://teams.microsoft.com/l/task/x?url=y\">Get template</a></p>"
    );
    assert_eq!(
        plain_words(&message),
        vec![
            "Build finished".to_owned(),
            "All checks passed.".to_owned(),
            footer.text.clone()
        ]
    );
}

/// The Planner card's shapes: columns of widths, an emphasis container with a border and a click, hidden elements,
/// pictures of their own size and a button with Teams' icon.
fn planner_like() -> CardMessage {
    adaptive(&json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "ColumnSet", "columns": [
                {"type": "Column", "width": "auto", "items": [
                    {"type": "Image", "url": "https://example.invalid/icon.png", "width": "16px", "height": "16px"}
                ]},
                {"type": "Column", "width": "stretch", "verticalContentAlignment": "center", "items": [
                    {"type": "TextBlock", "text": "Task created", "isSubtle": true, "wrap": true}
                ]},
                {"type": "Column", "width": 2, "items": []},
                {"type": "Column", "width": "40px", "items": []}
            ]},
            {"type": "Container", "style": "emphasis", "showBorder": true, "roundedCorners": true, "spacing": "small",
             "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/message", "title": "Open"},
             "items": [{"type": "TextBlock", "text": "Due", "isVisible": false, "id": "due"}]}
        ],
        "actions": [
            {"type": "Action.OpenUrl", "title": "View item", "iconUrl": "icon:Open", "url": "https://example.invalid/t"},
            {"type": "Action.Submit", "title": "Approve", "style": "positive"},
            {"type": "Action.ToggleVisibility", "title": "More", "targetElements": ["due", {"elementId": "x", "isVisible": true}]},
            {"type": "Action.ShowCard", "title": "Comment", "card": {"type": "AdaptiveCard", "body": [
                {"type": "TextBlock", "text": "Thanks"}
            ]}}
        ]
    }))
}

#[test]
fn reads_columns_and_containers() {
    let message = planner_like();
    let card = &message.cards[0];
    assert!(!card.unshown);
    let Kind::Columns(set) = &card.body[0].kind else {
        panic!("columns")
    };
    let widths: Vec<Width> = set.columns.iter().map(|column| column.width).collect();
    assert_eq!(
        widths,
        vec![Width::Auto, Width::Weight(1.0), Width::Weight(2.0), Width::Pixels(40.0)]
    );
    assert_eq!(set.columns[1].vertical, VAlign::Center);
    let Kind::Image(icon) = &set.columns[0].items[0].kind else {
        panic!("an image")
    };
    assert_eq!(
        (icon.width, icon.height, icon.size),
        (Some(16.0), Some(16.0), ImageSize::Auto)
    );
    let Kind::Text(subtle) = &set.columns[1].items[0].kind else {
        panic!("a text")
    };
    assert!(subtle.look.subtle);
    assert_eq!(card.body[1].spacing, Spacing::Small);
    let Kind::Container(container) = &card.body[1].kind else {
        panic!("a container")
    };
    assert_eq!(container.frame.style, Some(ContainerStyle::Emphasis));
    assert!(container.frame.border && container.frame.rounded);
    assert!(matches!(
        container.select_action.as_deref().map(|action| &action.kind),
        Some(ActionKind::OpenUrl(url)) if url == "https://example.invalid/message"
    ));
    assert_eq!(container.items[0].id.as_deref(), Some("due"));
    assert!(!container.items[0].visible);
}

#[test]
fn reads_actions() {
    let message = planner_like();
    let card = &message.cards[0];
    assert_eq!(card.actions.len(), 4);
    assert_eq!(card.actions[0].teams_icon(), Some("Open"));
    assert_eq!(
        card.actions[1].kind,
        ActionKind::Submit(BotAction {
            id: None,
            via: Via::Submit,
            data: None,
            inputs: true
        })
    );
    assert_eq!(card.actions[1].style, ActionStyle::Positive);
    assert_eq!(
        card.actions[2].kind,
        ActionKind::ToggleVisibility(vec![("due".to_owned(), None), ("x".to_owned(), Some(true))])
    );
    let ActionKind::ShowCard(shown) = &card.actions[3].kind else {
        panic!("a shown card")
    };
    assert_eq!(shown.body.len(), 1);
}

/// Facts, rich text runs, image sets and tables.
#[test]
fn reads_facts_rich_text_image_sets_and_tables() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "FactSet", "facts": [{"title": "Plan", "value": "Lab plan"}, {"title": "Due"}]},
            {"type": "RichTextBlock", "horizontalAlignment": "center", "inlines": [
                "plain ",
                {"type": "TextRun", "text": "loud", "weight": "bolder", "italic": true, "color": "attention",
                 "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/r"}}
            ]},
            {"type": "ImageSet", "imageSize": "small", "images": [
                {"type": "Image", "url": "https://example.invalid/1.png", "style": "person"},
                {"type": "Image", "url": "javascript:alert(1)"}
            ]},
            {"type": "Table", "firstRowAsHeader": false, "columns": [{"width": 1}, {"width": "60px"}], "rows": [
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "a"}]},
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "b"}]}
                ]}
            ]}
        ]
    }));
    let card = &message.cards[0];
    let Kind::Facts(facts) = &card.body[0].kind else {
        panic!("facts")
    };
    assert_eq!(
        facts,
        &vec![
            ("Plan".to_owned(), "Lab plan".to_owned()),
            ("Due".to_owned(), String::new())
        ]
    );
    let Kind::RichText(rich) = &card.body[1].kind else {
        panic!("rich text")
    };
    assert_eq!(rich.align, HAlign::Center);
    assert_eq!(rich.runs[0].text, "plain ");
    assert!(rich.runs[1].marks.italic);
    assert_eq!(rich.runs[1].look.color, TextColor::Attention);
    assert!(rich.runs[1].select_action.is_some());
    let Kind::ImageSet { images, size } = &card.body[2].kind else {
        panic!("an image set")
    };
    assert_eq!(*size, ImageSize::Small);
    assert_eq!(images.len(), 1, "a picture of a link that isn't http(s) is left out");
    assert!(images[0].person);
    assert!(card.unshown, "and the card says it has content it can't show");
    let Kind::Table(table) = &card.body[3].kind else {
        panic!("a table")
    };
    assert_eq!(table.columns, vec![Width::Weight(1.0), Width::Pixels(60.0)]);
    assert!(!table.header);
    assert!(table.grid);
    assert_eq!(table.rows[0].len(), 2);
}

/// An input, an unknown element without a fallback and an unknown action mark the card unshown; an element's
/// fallback stands in for it, and `"drop"` leaves it out quietly.
#[test]
fn what_it_cant_show_is_never_guessed_at() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Media", "sources": [], "fallback": {"type": "TextBlock", "text": "No media here"}},
            {"type": "Rating", "fallback": "drop"},
        ]
    }));
    let card = &message.cards[0];
    assert!(!card.unshown);
    assert_eq!(card.body.len(), 1);
    let Kind::Text(text) = &card.body[0].kind else {
        panic!("the fallback")
    };
    assert_eq!(text.text, "No media here");

    let with_input = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "comment"}, {"type": "TextBlock", "text": "kept"}],
        "actions": [{"type": "Action.Popover", "title": "?"}]
    }));
    assert!(with_input.cards[0].unshown, "the chat view doesn't draw inputs yet");
    assert_eq!(with_input.cards[0].body.len(), 2);
    assert!(matches!(&with_input.cards[0].body[0].kind, Kind::Input(input) if input.id == "comment"));
    assert!(with_input.cards[0].actions.is_empty());

    let other =
        json!({"attachments": [{"contentType": "application/vnd.microsoft.teams.card.o365connector", "content": {}}]});
    let other = card_message(&content(&other)).unwrap();
    assert!(other.unshown);
    assert!(other.cards.is_empty());

    assert_eq!(card_message("<p>no card</p>"), None);
    assert_eq!(
        card_message("<URIObject><Swift b64=\"not base64!\"></Swift></URIObject>"),
        None
    );
}

/// A hero card is read as the Adaptive Card Teams draws for it.
#[test]
fn reads_a_hero_card() {
    let activity = json!({"attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": {
        "title": "Hello", "subtitle": "from a bot", "text": "Body",
        "images": [{"url": "https://example.invalid/h.png"}],
        "buttons": [{"type": "openUrl", "title": "Docs", "value": "https://example.invalid/docs"},
                    {"type": "imBack", "title": "Yes", "value": "yes"}]
    }}]});
    let message = card_message(&content(&activity)).unwrap();
    let card = &message.cards[0];
    assert_eq!(card.body.len(), 4);
    let Kind::Text(title) = &card.body[0].kind else {
        panic!("a title")
    };
    assert_eq!(
        (title.look.size, title.look.weight),
        (TextSize::Medium, TextWeight::Bolder)
    );
    let Kind::Image(image) = &card.body[3].kind else {
        panic!("a picture")
    };
    assert_eq!(image.size, ImageSize::Stretch);
    assert!(matches!(&card.actions[0].kind, ActionKind::OpenUrl(url) if url.ends_with("/docs")));
    assert_eq!(
        card.actions[1].kind,
        ActionKind::Submit(BotAction {
            id: None,
            via: Via::ImBack("yes".to_owned()),
            data: Some(json!("yes")),
            inputs: false
        })
    );
}

/// A hero card's other buttons that send to the bot, each with what it carries.
#[test]
fn reads_a_hero_cards_bot_buttons() {
    let activity = json!({"attachments": [{"contentType": "application/vnd.microsoft.card.hero", "content": {
        "title": "Pick",
        "buttons": [
            {"type": "messageBack", "title": "A", "text": "a", "displayText": "Chose A", "value": {"pick": "a"}},
            {"type": "postBack", "title": "B", "value": "b"},
            {"type": "invoke", "title": "C", "value": {"pick": "c"}},
            {"type": "imBack", "title": "D", "value": {"not": "text"}}
        ]
    }}]});
    let message = card_message(&content(&activity)).unwrap();
    let vias: Vec<Via> = message.cards[0]
        .actions
        .iter()
        .map(|action| match &action.kind {
            ActionKind::Submit(bot) => bot.via.clone(),
            other => panic!("not a bot button: {other:?}"),
        })
        .collect();
    assert_eq!(
        vias,
        vec![
            Via::MessageBack {
                text: Some("a".to_owned()),
                display_text: Some("Chose A".to_owned()),
                value: Some(json!({"pick": "a"}))
            },
            Via::PostBack(Some(json!("b"))),
            Via::Invoke(Some(json!({"pick": "c"}))),
            Via::PostBack(Some(json!({"not": "text"}))),
        ]
    );
    assert!(!message.cards[0].unshown);
}

#[test]
fn reads_a_sign_in_and_an_oauth_card_as_their_text_and_sign_in_button() {
    for kind in ["application/vnd.microsoft.card.oauth", "application/vnd.microsoft.card.signin"] {
        let activity = json!({"attachments": [{"contentType": kind, "content": {
            "text": "Sign in to Synthetic Tools", "connectionName": "tools",
            "tokenExchangeResource": {"id": "x", "uri": "api://synthetic"},
            "buttons": [
                {"type": "signin", "title": "Sign in", "value": "https://tools.example.test/sign-in?x=1"},
                {"type": "signin", "title": "Broken", "value": "javascript:alert(1)"}
            ]
        }}]});
        let message = card_message(&content(&activity)).unwrap();
        let card = &message.cards[0];
        assert_eq!(plain_words(&message), ["Sign in to Synthetic Tools"], "{kind}");
        assert_eq!(card.actions.len(), 1, "{kind}");
        assert_eq!(card.actions[0].title, "Sign in");
        assert!(matches!(&card.actions[0].kind, ActionKind::Submit(BotAction {
            via: Via::SignIn(Some(page)), data: None, inputs: false, ..
        }) if page == "https://tools.example.test/sign-in?x=1"));
        assert!(card.unshown, "a button that isn't a link is said, not drawn");
    }
}

/// The bot buttons of a card: `Action.Submit` with each `msteams` kind, `Action.Execute` with its verb.
fn bot_buttons(card: &teams_proto::chat::card::Card) -> Vec<&BotAction> {
    card.actions
        .iter()
        .map(|action| match &action.kind {
            ActionKind::Submit(bot) => bot,
            other => panic!("not a bot button: {other:?}"),
        })
        .collect()
}

/// `Action.Submit` with each `msteams` kind and `Action.Execute` with its verb keep what an invoke needs.
#[test]
fn bot_actions_keep_their_data_verb_and_msteams() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [{"type": "TextBlock", "text": "Request 7"}],
        "actions": [
            {"type": "Action.Execute", "id": "ok", "title": "Approve", "verb": "approve", "data": {"request": 7}},
            {"type": "Action.Submit", "title": "Note", "associatedInputs": "None",
             "data": {"msteams": {"type": "messageBack", "text": "note", "displayText": "Noted", "value": {"n": 1}}}},
            {"type": "Action.Submit", "title": "Yes", "data": {"msteams": {"type": "imBack", "value": "Yes"}}},
            {"type": "Action.Submit", "title": "Go", "data": {"msteams": {"type": "invoke", "value": {"go": 1}}}},
            {"type": "Action.Submit", "title": "Sign in", "data": {"msteams": {"type": "signin", "value": "https://example.invalid/in"}}},
            {"type": "Action.Submit", "title": "Form", "data": {"msteams": {"type": "task/fetch"}, "form": 1}},
            {"type": "Action.Submit", "title": "Odd", "data": {"msteams": {"type": "unknown"}}},
            {"type": "Action.Submit", "title": "Bare imBack", "data": {"msteams": {"type": "imBack"}}}
        ]
    }));
    let card = &message.cards[0];
    assert!(!card.unshown);
    let bots = bot_buttons(card);
    assert_eq!(bots[0].id.as_deref(), Some("ok"));
    assert_eq!(bots[0].data, Some(json!({"request": 7})));
    assert!(bots[0].inputs);
    assert!(!bots[1].inputs);
    assert!(matches!(&bots[1].via, Via::MessageBack { display_text: Some(shown), .. } if shown == "Noted"));
    assert_eq!(bots[1].data.as_ref().unwrap()["msteams"]["text"], "note");
    let vias: Vec<Via> = bots[2..].iter().map(|bot| bot.via.clone()).collect();
    assert_eq!(
        vias,
        vec![
            Via::ImBack("Yes".to_owned()),
            Via::Invoke(Some(json!({"go": 1}))),
            Via::SignIn(Some("https://example.invalid/in".to_owned())),
            Via::TaskFetch,
            Via::Submit,
            Via::Submit,
        ]
    );
}

/// A card's `refresh` keeps its `Action.Execute` and the users it runs for; another action makes none.
#[test]
fn a_cards_refresh_is_kept() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "refresh": {
            "action": {"type": "Action.Execute", "verb": "refreshCard", "data": {"request": 7}},
            "userIds": ["8:orgid:<guid:b>", 3]
        },
        "body": [{"type": "TextBlock", "text": "Request 7"}]
    }));
    let refresh = message.cards[0].refresh.as_ref().unwrap();
    assert_eq!(
        refresh.action.via,
        Via::Execute {
            verb: Some("refreshCard".to_owned())
        }
    );
    assert_eq!(refresh.user_ids, vec!["8:orgid:<guid:b>".to_owned()]);
    let no_refresh = adaptive(&json!({"type": "AdaptiveCard", "refresh": {"action": {"type": "Action.Submit"}}}));
    assert!(no_refresh.cards[0].refresh.is_none());
}

/// A text input, with its starting answer.
#[test]
fn reads_text_inputs() {
    let text = input(
        &json!({"type": "Input.Text", "id": "t", "label": "Name", "isRequired": true,
        "errorMessage": "Say a name", "placeholder": "Your name", "maxLength": 40, "regex": "^\\w+$",
        "style": "Email", "value": "lab"}),
    )
    .unwrap();
    assert_eq!(
        (text.id.as_str(), text.label.as_deref(), text.required),
        ("t", Some("Name"), true)
    );
    assert_eq!(text.error_message.as_deref(), Some("Say a name"));
    assert_eq!(
        text.kind,
        InputKind::Text {
            placeholder: Some("Your name".to_owned()),
            multiline: false,
            max_length: Some(40),
            regex: Some("^\\w+$".to_owned()),
            style: TextInputStyle::Email,
            value: Some("lab".to_owned())
        }
    );
    assert_eq!(text.initial_answer().as_deref(), Some("lab"));
    for (style, expected) in [
        ("tel", TextInputStyle::Tel),
        ("url", TextInputStyle::Url),
        ("password", TextInputStyle::Password),
        ("other", TextInputStyle::Text),
    ] {
        let read = input(&json!({"type": "Input.Text", "id": "s", "style": style})).unwrap();
        assert!(matches!(read.kind, InputKind::Text { style, .. } if style == expected));
        assert_eq!(read.initial_answer(), None);
    }
}

/// Number, date, time and toggle inputs, with their starting answers.
#[test]
fn reads_number_date_time_and_toggle_inputs() {
    let number = input(&json!({"type": "Input.Number", "id": "n", "min": 1, "max": 9.5, "value": 3})).unwrap();
    assert!(matches!(
        number.kind,
        InputKind::Number {
            min: Some(1.0),
            max: Some(9.5),
            value: Some(3.0),
            ..
        }
    ));
    assert_eq!(number.initial_answer().as_deref(), Some("3"));
    let date = input(&json!({"type": "Input.Date", "id": "d", "min": "2026-01-01", "value": "2026-01-02"})).unwrap();
    assert_eq!(date.initial_answer().as_deref(), Some("2026-01-02"));
    assert!(matches!(
        date.kind,
        InputKind::Date {
            min: Some(_),
            max: None,
            ..
        }
    ));
    let time = input(&json!({"type": "Input.Time", "id": "h", "max": "17:00"})).unwrap();
    assert!(matches!(
        time.kind,
        InputKind::Time {
            max: Some(_),
            value: None,
            ..
        }
    ));

    let toggle =
        input(&json!({"type": "Input.Toggle", "id": "g", "title": "Notify", "valueOn": "yes", "value": "yes"}))
            .unwrap();
    assert_eq!(
        toggle.kind,
        InputKind::Toggle {
            title: "Notify".to_owned(),
            value_on: "yes".to_owned(),
            value_off: "false".to_owned(),
            on: true
        }
    );
    assert_eq!(toggle.initial_answer().as_deref(), Some("yes"));
    let off = input(&json!({"type": "Input.Toggle", "id": "g"})).unwrap();
    assert_eq!(off.initial_answer().as_deref(), Some("false"));
}

/// Choice sets: their own choices, filtered, and the directory's people.
#[test]
fn reads_choice_sets() {
    let choices = input(&json!({"type": "Input.ChoiceSet", "id": "c", "isMultiSelect": true, "style": "expanded",
        "value": "a,b", "choices": [{"title": "A", "value": "a"}, {"title": "B", "value": "b"}, {"title": "no value"}]}))
    .unwrap();
    let InputKind::ChoiceSet {
        choices: listed,
        multi,
        style,
        chosen,
        dataset,
        ..
    } = &choices.kind
    else {
        panic!("a choice set")
    };
    assert_eq!((listed.len(), *multi, *style), (2, true, ChoiceStyle::Expanded));
    assert_eq!(chosen, &vec!["a".to_owned(), "b".to_owned()]);
    assert!(dataset.is_none() && !choices.asks_for_people());
    assert_eq!(choices.initial_answer().as_deref(), Some("a,b"));
    let filtered = input(&json!({"type": "Input.ChoiceSet", "id": "f", "style": "filtered"})).unwrap();
    assert!(matches!(
        filtered.kind,
        InputKind::ChoiceSet {
            style: ChoiceStyle::Filtered,
            ..
        }
    ));
    assert_eq!(filtered.initial_answer(), None);
    let people = input(&json!({"type": "Input.ChoiceSet", "id": "p",
        "choices.data": {"type": "Data.Query", "dataset": "graph.microsoft.com/users"}}))
    .unwrap();
    assert!(people.asks_for_people());
}

/// An input with a `fallback` gets its fallback, as an element of a type the chat view doesn't draw does: `"drop"`
/// leaves it out quietly, an element takes its place, and the card is not unshown.
#[test]
fn an_input_with_a_fallback_gets_its_fallback() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Input.Text", "id": "comment", "fallback": "drop"},
            {"type": "Input.Toggle", "id": "agree", "title": "Agree",
             "fallback": {"type": "TextBlock", "text": "Answer in Teams"}},
            {"type": "TextBlock", "text": "kept"},
        ]
    }));
    let card = &message.cards[0];
    assert!(!card.unshown);
    assert_eq!(card.body.len(), 2);
    let texts: Vec<&str> = card
        .body
        .iter()
        .map(|element| match &element.kind {
            Kind::Text(text) => text.text.as_str(),
            other => panic!("not a text: {other:?}"),
        })
        .collect();
    assert_eq!(texts, ["Answer in Teams", "kept"]);
}

/// Read for a view that draws inputs, every input is read, one with a `fallback` too, and no input marks the card
/// unshown; in a shown card as well. An input of a kind not read still gets its fallback.
#[test]
fn a_view_that_draws_inputs_reads_every_input() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Input.Text", "id": "comment", "fallback": "drop"},
            {"type": "Input.Toggle", "id": "agree", "title": "Agree",
             "fallback": {"type": "TextBlock", "text": "Answer in Teams"}},
            {"type": "Input.Rating", "id": "stars", "fallback": {"type": "TextBlock", "text": "No stars"}},
        ],
        "actions": [{"type": "Action.ShowCard", "title": "More", "card": {"type": "AdaptiveCard", "body": [
            {"type": "Input.Date", "id": "due"}
        ]}}]
    });
    let activity = json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}],
    });
    let message = card_message_with_inputs(&content(&activity)).unwrap();
    let read = &message.cards[0];
    assert!(!read.unshown);
    let ids: Vec<String> = read
        .body
        .iter()
        .map(|element| match &element.kind {
            Kind::Input(input) => input.id.clone(),
            Kind::Text(text) => text.text.clone(),
            other => panic!("not an input or a text: {other:?}"),
        })
        .collect();
    assert_eq!(ids, ["comment", "agree", "No stars"]);
    let ActionKind::ShowCard(shown) = &read.actions[0].kind else {
        panic!("not a shown card");
    };
    assert!(matches!(shown.body[0].kind, Kind::Input(_)));
    assert!(!shown.unshown);
    assert_eq!(adaptive_card_with_inputs(&card), *read);
    // read for a view that draws none, the input without a fallback (the shown card's) marks it unshown
    assert!(adaptive(&card).cards[0].unshown);
}

/// A bot's suggested actions are read as a hero card's buttons; one of a kind not read is left out.
#[test]
fn reads_suggested_actions() {
    let activity = json!({
        "type": "message/card",
        "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive",
                         "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Pick one"}]}}],
        "suggestedActions": {"actions": [
            {"type": "imBack", "title": "Red", "value": "red"},
            {"type": "messageBack", "title": "Blue", "text": "blue", "displayText": "Blue please", "value": {"c": 2}},
            {"type": "showImage", "title": "Not read", "value": "https://example.invalid/i.png"},
        ]}
    });
    let message = card_message(&content(&activity)).unwrap();
    let titles: Vec<&str> = message.suggested.iter().map(|action| action.title.as_str()).collect();
    assert_eq!(titles, ["Red", "Blue"]);
    assert!(matches!(
        &message.suggested[0].kind,
        ActionKind::Submit(BotAction { via: Via::ImBack(text), .. }) if text == "red"
    ));
    assert!(adaptive(&json!({"type": "AdaptiveCard"})).suggested.is_empty());
}

/// An input without its id, or of a kind not read, isn't one; in a card it is unshown.
#[test]
fn inputs_not_read() {
    assert!(input(&json!({"type": "Input.Text"})).is_none(), "an input needs its id");
    assert!(input(&json!({"type": "Input.Rating", "id": "r"})).is_none());
    assert!(input(&json!({"id": "r"})).is_none());
    let unknown = adaptive(&json!({"type": "AdaptiveCard", "body": [{"type": "Input.Rating", "id": "r"}]}));
    assert!(unknown.cards[0].unshown && unknown.cards[0].body.is_empty());
}

#[test]
fn markdown_is_adaptive_cards_subset() {
    assert_eq!(
        markdown_html("**bold** and _it_ and *it* ~~gone~~"),
        "<p><b>bold</b> and <i>it</i> and <i>it</i> <s>gone</s></p>"
    );
    assert_eq!(
        markdown_html("1. Go there.\r\n2. Select **More options**(**...**).\r\n- a\n- b"),
        "<ol><li>Go there.</li><li>Select <b>More options</b>(<b>...</b>).</li></ol><ul><li>a</li><li>b</li></ul>"
    );
    assert_eq!(markdown_html("one\ntwo\n\nthree"), "<p>one two</p><p>three</p>");
    assert_eq!(
        markdown_html("<script>x</script> & [bad](javascript:x) [ok](mailto:a@b.c)"),
        "<p>&lt;script&gt;x&lt;/script&gt; &amp; bad <a href=\"mailto:a@b.c\">ok</a></p>"
    );
    assert_eq!(markdown_html("2 * 3 = 6, snake_case"), "<p>2 * 3 = 6, snake_case</p>");
    assert_eq!(markdown_html(""), "");
}

#[test]
fn finds_date_and_time_functions() {
    assert_eq!(
        text_parts("Sam {{TIME(2026-11-04T18:15:06Z)}} on {{DATE(2026-11-04T18:15:06Z, SHORT)}}."),
        vec![
            TextPart::Text("Sam "),
            TextPart::Time {
                at_ms: 1_793_816_106_000
            },
            TextPart::Text(" on "),
            TextPart::Date {
                at_ms: 1_793_816_106_000,
                format: DateFormat::Short
            },
            TextPart::Text("."),
        ]
    );
    assert_eq!(
        text_parts("{{DATE(2026-11-04T21:15:06+03:00)}}"),
        vec![TextPart::Date {
            at_ms: 1_793_816_106_000,
            format: DateFormat::Compact
        }]
    );
    assert_eq!(
        text_parts("{{NOPE(1)}} {{DATE(bad)}}"),
        vec![
            TextPart::Text("{{"),
            TextPart::Text("NOPE(1)}} {{"),
            TextPart::Text("DATE(bad)}}"),
        ]
    );
}

/// A card's picture is asked for by the async gateway's thumbnail of its link, never fetched from its site.
#[test]
fn a_card_picture_is_read_through_the_gateway() {
    let src = LinkPreviewPicture::card_picture_src("https://example.invalid/a b.png?x=1&y=2").unwrap();
    let picture = LinkPreviewPicture::of(&src).unwrap();
    assert_eq!(picture.picture(), "https://example.invalid/a b.png?x=1&y=2");
    assert_eq!(
        LinkPreviewPicture::card_picture_src("ftp://example.invalid/a.png"),
        None
    );
    assert_eq!(LinkPreviewPicture::card_picture_src("data:image/png;base64,AAAA"), None);
}

/// The synthetic message of a YouTube link with its app's card in `properties.cards`, in the delivered shape.
const APP_CARD: &str = include_str!("../../../tests/fixtures/chatsvc/app-card.json");
/// The fixture's made-up video's watch link.
const WATCH: &str = "https://www.youtube.com/watch?v=Ab3_dEf-Gh9";

fn app_card_properties() -> serde_json::Map<String, Value> {
    let fixture: Value = serde_json::from_str(APP_CARD).unwrap();
    fixture["read_back"]["properties"].as_object().unwrap().clone()
}

/// A `properties.cards` value of one entry.
fn cards_of(entry: &Value) -> Value {
    Value::String(json!([entry]).to_string())
}

/// The fixture's one app card.
fn app_card() -> teams_proto::chat::card::Card {
    let message = message_cards(app_card_properties().get("cards")).unwrap();
    assert!(!message.unshown);
    assert_eq!(message.cards.len(), 1);
    message.cards.into_iter().next().unwrap()
}

#[test]
fn reads_a_link_s_app_card_its_app_and_its_video() {
    let card = app_card();
    assert!(!card.unshown);
    let app = card.app.as_ref().unwrap();
    assert_eq!(app.id.as_deref(), Some("com.microsoft.teamspace.tab.youtube"));
    assert_eq!(app.name.as_deref(), Some("YouTube"));
    // the delivered icon is the async gateway's copy of the app's icon: the picture is the link inside it
    assert_eq!(
        app.icon_picture().as_deref(),
        Some("https://res.cdn.office.net/teamsappdata/made-up/com.microsoft.teamspace.tab.youtube_largeimage.png")
    );
    let Kind::Media(media) = &card.body[0].kind else {
        panic!("a media element")
    };
    assert_eq!(media.link(), Some(WATCH));
    assert_eq!(media.sources[0].mime_type.as_deref(), Some("video/mp4"));
    assert_eq!(
        media.picture().as_deref(),
        Some("https://i.ytimg.com/vi/Ab3_dEf-Gh9/hqdefault.jpg")
    );
}

#[test]
fn reads_a_link_s_app_card_its_text_and_its_open_button() {
    let card = app_card();
    let Kind::Text(title) = &card.body[1].kind else {
        panic!("the title")
    };
    assert_eq!(title.text, "A made-up video for the test");
    assert_eq!(
        (title.look.size, title.look.weight),
        (TextSize::Large, TextWeight::Bolder)
    );
    assert_eq!(title.max_lines, Some(4));
    let Kind::Text(channel) = &card.body[2].kind else {
        panic!("the channel")
    };
    assert!(channel.look.subtle);
    assert_eq!(card.body[2].spacing, Spacing::Small);
    assert_eq!(card.body[3].spacing, Spacing::None);
    let Kind::Actions(actions) = &card.body[4].kind else {
        panic!("the buttons")
    };
    assert_eq!(card.body[4].spacing, Spacing::Medium);
    assert_eq!(actions[0].title, "Open");
    assert_eq!(actions[0].kind, ActionKind::OpenTab(WATCH.to_owned()));
    assert_eq!(
        card.select_action.as_deref().map(|action| &action.kind),
        Some(&ActionKind::OpenTab(WATCH.to_owned()))
    );
}

#[test]
fn a_link_s_app_card_is_shown_but_a_card_this_build_cannot_draw_is_not() {
    assert!(unshown_parts(&app_card_properties()).is_empty());
    let unshown = |entry: Value| {
        let mut properties = app_card_properties();
        properties.insert("cards".to_owned(), cards_of(&entry));
        let parts = unshown_parts(&properties);
        matches!(parts.as_slice(), [Unshown::Cards(_)])
    };
    assert!(unshown(
        json!({"appName": "Made-up", "contentType": "application/vnd.made-up.card", "content": {}})
    ));
    assert!(unshown(json!({"appName": "Made-up", "contentType": ADAPTIVE})));
    assert!(unshown(json!({
        "contentType": ADAPTIVE,
        "content": {"type": "AdaptiveCard", "body": [{"type": "Made.Up"}]},
    })));
    // a media element without a link it may open
    assert!(unshown(json!({
        "contentType": ADAPTIVE,
        "content": {"type": "AdaptiveCard", "body": [{"type": "Media", "sources": [{"url": "javascript:x"}]}]},
    })));
    assert!(!unshown(json!({
        "contentType": ADAPTIVE,
        "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Made-up"}]},
    })));
    let mut not_a_list = app_card_properties();
    not_a_list.insert("cards".to_owned(), Value::String("not json".to_owned()));
    assert!(matches!(unshown_parts(&not_a_list).as_slice(), [Unshown::Cards(_)]));
    // none, or an empty list: no cards
    assert_eq!(message_cards(None), None);
    assert_eq!(message_cards(Some(&json!("[]"))), None);
    assert_eq!(message_cards(Some(&json!([]))), None);
}

#[test]
fn an_app_icon_s_gateway_copy_is_read_by_the_gateway_s_rules() {
    let picture = |icon: &str| {
        teams_proto::chat::card::CardApp {
            icon: Some(icon.to_owned()),
            ..Default::default()
        }
        .icon_picture()
    };
    let inner = "https://res.cdn.office.net/made-up/icon.png";
    let gateway = "https://us-prod.asyncgw.teams.microsoft.com/urlp/v1/url/content";
    let copy = format!("{gateway}?url=https%3a%2f%2fres.cdn.office.net%2fmade-up%2ficon.png");
    assert_eq!(picture(&copy).as_deref(), Some(inner));
    assert_eq!(picture(inner).as_deref(), Some(inner), "the app's own icon");
    // a gateway link the gateway's rules refuse is not read through: it stays a link of its own
    for refused in [
        copy.replacen("https://", "http://", 1),
        format!("{copy}&url=https%3a%2f%2fother.example%2fx.png"),
        format!("{gateway}/more?url=https%3a%2f%2fres.cdn.office.net%2fmade-up%2ficon.png"),
    ] {
        assert_ne!(picture(&refused).as_deref(), Some(inner), "{refused}");
    }
    assert_eq!(picture("data:image/png;base64,AAAA"), None);
}

#[test]
fn a_tab_action_without_a_website_link_stays_a_bot_action() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "actions": [{"type": "Action.Submit", "title": "Open", "data": {"msteams": {"type": "invoke",
            "value": {"type": "tab/tabInfoAction", "tabInfo": {"websiteUrl": "javascript:x"}}}}}],
    }));
    assert!(matches!(message.cards[0].actions[0].kind, ActionKind::Submit(_)));
}

#[test]
fn a_media_s_picture_is_its_poster_or_youtube_s_picture_of_its_video() {
    let message = adaptive(&json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Media", "poster": "https://example.invalid/poster.png", "altText": "A made-up clip",
                  "sources": [{"mimeType": "video/mp4", "url": "https://example.invalid/clip.mp4"}]}],
    }));
    let Kind::Media(media) = &message.cards[0].body[0].kind else {
        panic!("a media element")
    };
    assert_eq!(media.picture().as_deref(), Some("https://example.invalid/poster.png"));
    assert_eq!(media.alt.as_deref(), Some("A made-up clip"));
    assert_eq!(media.link(), Some("https://example.invalid/clip.mp4"));
    let hq = Some("https://i.ytimg.com/vi/Ab3_dEf-Gh9/hqdefault.jpg".to_owned());
    for link in [
        "https://youtu.be/Ab3_dEf-Gh9?si=madeup",
        "https://www.youtube.com/watch?v=Ab3_dEf-Gh9&t=42",
        "https://youtube.com/watch?feature=share&v=Ab3_dEf-Gh9",
        "https://m.youtube.com/watch?v=Ab3_dEf-Gh9",
        "https://music.youtube.com/watch?v=Ab3_dEf-Gh9",
        "https://www.youtube.com/shorts/Ab3_dEf-Gh9",
        "https://www.youtube.com/embed/Ab3_dEf-Gh9",
        "https://www.youtube.com/live/Ab3_dEf-Gh9?feature=share",
        "http://www.youtube-nocookie.com/embed/Ab3_dEf-Gh9",
    ] {
        assert_eq!(youtube_picture(link), hq, "{link}");
    }
    for link in [
        "https://vimeo.com/123456789",
        "https://www.youtube.com/watch?v=short",
        "https://www.youtube.com/watch?v=Ab3_dEf-Gh9x",
        "https://www.youtube.com/watch",
        "https://www.youtube.com/@channel",
        "https://notyoutube.com/watch?v=Ab3_dEf-Gh9",
        "https://youtu.be.example.invalid/Ab3_dEf-Gh9",
        "ftp://youtu.be/Ab3_dEf-Gh9",
        "https://www.youtube.com/watch?v=Ab3_dEf%2FGh",
    ] {
        assert_eq!(youtube_picture(link), None, "{link}");
    }
}
