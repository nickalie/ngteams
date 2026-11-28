//! A card a workflow posted (`RichText/Media_Card`), drawn as Teams draws it (`teams_ui::card_view`), on GPUI's test
//! context over the fake engine: the card on its own surface beside the Workflows bot's avatar, its text, its picture
//! asked of the engine by the gateway's thumbnail of its link, its buttons (a link opened, a shown card opened and
//! closed, elements toggled, a bot's button opening its message in Teams on the web), and the note for what it can't show.
//! Every card here is synthetic: every word, link and id is made up.

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use gpui::{Bounds, Modifiers, Pixels, VisualTestContext};
use serde_json::{Value, json};
use teams_proto::LinkPreviewPicture;
use teams_store::Message;
use teams_ui::card_view::CARD_WIDTH;
use teams_ui::chat_list;
use teams_ui::chat_view::{Body, MessageAction};

use super::{NOON, chat_window, draw, items, message, rig_with, trace};

/// The Workflows bot, as it posts.
const WORKFLOWS: &str = "28:358f0194-6b0e-4dd3-af35-c24fe8a9ec87";
const ID: &str = "501";
const ICON: &str = "https://example.invalid/icon.png";

/// A card message from the Workflows bot carrying the Adaptive Card `card`.
fn card_message(id: &str, card: &Value) -> Message {
    attachments_message(
        id,
        &json!([{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]),
    )
}

/// A card message from the Workflows bot carrying `attachments`.
fn attachments_message(id: &str, attachments: &Value) -> Message {
    let activity = json!({"type": "message/card", "attachments": attachments});
    let b64 = STANDARD.encode(serde_json::to_vec(&activity).unwrap());
    let content = format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    );
    let mut stored = message(id, WORKFLOWS, &content, NOON);
    stored.message_type = "RichText/Media_Card".to_owned();
    stored.sender_name = Some("Workflows".to_owned());
    stored
}

fn workflow_card() -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "body": [
            {"type": "TextBlock", "text": "Lab build finished", "weight": "bolder", "wrap": true},
            {"type": "Image", "url": ICON, "width": "16px", "height": "16px"},
            {"type": "TextBlock", "id": "details", "isVisible": false, "text": "Hidden details", "wrap": true},
            {"type": "Container", "separator": true, "items": [
                {"type": "TextBlock", "size": "small", "weight": "lighter", "wrap": true,
                 "text": "Lab A used a Workflow template to send this card. [Get template](https://example.invalid/t)"}
            ]}
        ],
        "actions": [
            {"type": "Action.OpenUrl", "title": "View item", "iconUrl": "icon:Open", "url": "https://example.invalid/item"},
            {"type": "Action.ToggleVisibility", "title": "Details", "targetElements": ["details"]},
            {"type": "Action.ShowCard", "title": "More", "card": {"type": "AdaptiveCard", "body": [
                {"type": "TextBlock", "text": "Shown card text"}
            ]}},
            {"type": "Action.Submit", "title": "Approve"}
        ]
    })
}

/// A made-up PNG, 32 by 32.
fn png() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(32, 32, image::Rgba([40, 160, 200, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
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

#[test]
fn a_workflow_card_is_drawn_on_its_surface_beside_the_apps_avatar() {
    let rig = rig_with(vec![card_message(ID, &workflow_card())]);
    let src = LinkPreviewPicture::card_picture_src(ICON).unwrap();
    rig.core.pictures.lock().unwrap().insert(src, png());
    let (written_trace, written) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let shown = items(&view, cx);
    assert!(matches!(shown[0].body, Body::Cards(_)), "{:?}", shown[0].body);
    assert_eq!(shown[0].body.kind(), "cards");
    assert_eq!(shown[0].sender_name.as_ref(), "Workflows");
    // a card is reacted to, copied and marked unread
    assert_eq!(
        shown[0].actions(),
        [MessageAction::React, MessageAction::Copy, MessageAction::MarkUnread]
    );
    let card = bounds(cx, &format!("card-{ID}-c0")).expect("the card's surface");
    assert!(card.size.width <= gpui::px(CARD_WIDTH), "{card:?}");
    let title = bounds(cx, &format!("card-{ID}-c0b0.")).expect("its title");
    assert!(card.contains(&title.center()));
    // the picture, at the pixel size the card gives it
    let picture = bounds(cx, &format!("card-{ID}-c0b1.")).expect("its picture");
    assert_eq!(
        (picture.size.width, picture.size.height),
        (gpui::px(16.0), gpui::px(16.0))
    );
    // the hidden element isn't drawn, the footer is, and nothing says the card has content it can't show
    assert!(bounds(cx, &format!("card-{ID}-c0b2.")).is_none());
    assert!(bounds(cx, &format!("card-{ID}-c0b3.0.")).is_some());
    assert!(bounds(cx, &format!("card-{ID}-c0unshown")).is_none());
    // the Workflows bot's avatar is its app's tile
    assert!(bounds(cx, &format!("avatar-{ID}")).is_some());
    assert!(
        !written.text().contains("Lab build finished"),
        "the trace names no text"
    );
}

#[test]
fn a_cards_buttons_open_links_toggle_and_show_cards_and_send_to_teams_on_the_web_in_read_only_mode() {
    let rig = rig_with(vec![card_message(ID, &workflow_card())]);
    *rig.core.read_only.lock().unwrap() = true;
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    click(cx, &format!("card-{ID}-c0a0"));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.invalid/item"));
    // the toggle shows the hidden element, and hides it again
    click(cx, &format!("card-{ID}-c0a1"));
    assert!(bounds(cx, &format!("card-{ID}-c0b2.")).is_some());
    click(cx, &format!("card-{ID}-c0a1"));
    assert!(bounds(cx, &format!("card-{ID}-c0b2.")).is_none());
    // the shown card opens under the buttons, and closes again
    click(cx, &format!("card-{ID}-c0a2"));
    assert!(bounds(cx, &format!("card-{ID}-c0a2cb0.")).is_some());
    click(cx, &format!("card-{ID}-c0a2"));
    assert!(bounds(cx, &format!("card-{ID}-c0a2cb0.")).is_none());
    // a button that sends to the bot opens its message in Teams on the web, never a bot invoke
    click(cx, &format!("card-{ID}-c0a3"));
    assert_eq!(
        cx.opened_url(),
        Some(teams_proto::workflow::MessageRun::link_to(super::AB, ID, None))
    );
}

/// An input takes no place in the elements' paths: the text after it is still the first element.
#[test]
fn a_cloud_without_bot_invokes_draws_no_part_of_a_card_that_sends_to_the_bot() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Container", "selectAction": {"type": "Action.Submit"},
             "items": [{"type": "TextBlock", "text": "Pick me"}]}
        ],
        "actions": [{"type": "Action.Submit", "title": "Approve"}]
    });
    for apps in [true, false] {
        let rig = rig_with(vec![card_message(ID, &card)]);
        *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
            apps,
            ..teams_core::Capabilities::ALL
        });
        let (written_trace, _) = trace();
        let (_view, cx) = chat_window(&rig, written_trace);
        // the container is drawn either way; a click on it sends to the bot only where the cloud serves bots
        assert!(bounds(cx, &format!("card-{ID}-c0b0.")).is_some());
        assert_eq!(bounds(cx, &format!("card-{ID}-c0b0.s")).is_some(), apps, "apps {apps}");
        assert_eq!(bounds(cx, &format!("card-{ID}-c0a0")).is_some(), apps, "apps {apps}");
    }
}

#[test]
fn a_card_with_an_input_says_it_has_content_it_cant_show() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "Input.Text", "id": "answer"}, {"type": "TextBlock", "text": "Your answer"}]
    });
    let rig = rig_with(vec![card_message(ID, &card)]);
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        apps: false,
        ..teams_core::Capabilities::ALL
    });
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    assert!(bounds(cx, &format!("card-{ID}-c0b0.")).is_some());
    assert!(bounds(cx, &format!("card-{ID}-c0b1.")).is_none());
    assert!(bounds(cx, &format!("card-{ID}-c0unshown")).is_some());
}

#[test]
fn an_input_with_a_fallback_shows_its_fallback() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "Input.Text", "id": "answer", "fallback": {"type": "TextBlock", "text": "Answer in Teams"}},
            {"type": "Input.Toggle", "id": "agree", "title": "Agree", "fallback": "drop"},
            {"type": "TextBlock", "text": "kept"}
        ]
    });
    let rig = rig_with(vec![card_message(ID, &card)]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    assert!(bounds(cx, &format!("card-{ID}-c0b0.")).is_some(), "the fallback");
    assert!(bounds(cx, &format!("card-{ID}-c0b1.")).is_some(), "the text");
    assert!(bounds(cx, &format!("card-{ID}-c0unshown")).is_none());
}

#[test]
fn a_toggle_in_a_shown_card_shows_its_hidden_element_on_the_first_click() {
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "Top"}],
        "actions": [{"type": "Action.ShowCard", "title": "More", "card": {
            "type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "id": "inner", "isVisible": false, "text": "Inner"}],
            "actions": [{"type": "Action.ToggleVisibility", "title": "Reveal", "targetElements": ["inner"]}]
        }}]
    });
    let rig = rig_with(vec![card_message(ID, &card)]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    click(cx, &format!("card-{ID}-c0a0"));
    assert!(bounds(cx, &format!("card-{ID}-c0a0cb0.")).is_none());
    click(cx, &format!("card-{ID}-c0a0ca0"));
    assert!(bounds(cx, &format!("card-{ID}-c0a0cb0.")).is_some());
}

#[test]
fn a_facts_title_stays_beside_its_value_when_the_value_before_wraps() {
    let long = "a value long enough to wrap over several lines of its card, and then over a few more lines still, \
                as a task's description does";
    let card = json!({
        "type": "AdaptiveCard",
        "body": [{"type": "FactSet", "facts": [
            {"title": "Description", "value": long},
            {"title": "Due", "value": "Friday"}
        ]}]
    });
    let rig = rig_with(vec![card_message(ID, &card)]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    let cell = |cx: &mut VisualTestContext, at: &str| bounds(cx, &format!("card-{ID}-c0b0.{at}")).expect(at);
    let first_value = cell(cx, "0v");
    let (title, value) = (cell(cx, "1t"), cell(cx, "1v"));
    assert!(
        first_value.size.height > value.size.height * 1.5,
        "the first value wraps: {first_value:?} {value:?}"
    );
    assert_eq!(
        title.origin.y, value.origin.y,
        "the second fact's title is on its value's row"
    );
    assert!(title.origin.y >= first_value.origin.y + first_value.size.height);
    assert!(value.origin.x > title.origin.x + title.size.width);
}

#[test]
fn an_attachment_that_isnt_a_card_is_said_never_left_blank() {
    let connector = json!({"contentType": "application/vnd.microsoft.teams.card.o365connector", "content": {}});
    let adaptive = json!({
        "contentType": "application/vnd.microsoft.card.adaptive",
        "content": {"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "Shown"}]}
    });
    let rig = rig_with(vec![
        attachments_message("601", &json!([connector.clone()])),
        attachments_message("602", &json!([adaptive, connector])),
    ]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    // nothing it draws: the message the app doesn't show, with Teams on the web to see it
    assert!(bounds(cx, "unsupported-601").is_some());
    assert!(bounds(cx, "cards-601").is_none());
    // a card and an attachment of another kind: the card, and the note under it
    assert!(bounds(cx, "card-602-c0b0.").is_some());
    assert!(bounds(cx, "card-602-unshown").is_some());
}

#[test]
fn a_cards_preview_line_says_it_sent_a_card() {
    let stored = card_message(ID, &workflow_card());
    assert_eq!(chat_list::message_line(&stored), "Sent a card");
    assert_eq!(
        chat_list::preview(&[stored], super::common::LAB_A).as_ref(),
        "Sent a card"
    );
}

/// A card of every kind of element: facts, rich text, a set of pictures, a table, columns with a separator between
/// them, containers of each style (in one bled to the card's edges), a centred picture that opens a link, a stretched
/// one, a text that doesn't wrap, a heading and an action set whose shown card has a button of its own.
fn every_kind() -> Value {
    let styles = ["default", "emphasis", "good", "attention", "warning", "accent"];
    let colours = ["accent", "warning", "attention", "light", "dark", "default"];
    let containers: Vec<Value> = styles
        .iter()
        .zip(colours)
        .enumerate()
        .map(|(at, (style, colour))| {
            json!({"type": "Container", "id": format!("styled-{at}"), "style": style, "showBorder": true,
                   "items": [{"type": "TextBlock", "text": style, "color": colour}]})
        })
        .collect();
    json!({
        "type": "AdaptiveCard",
        "body": [
            {"type": "FactSet", "facts": [
                {"title": "Plan", "value": "**Lab** plan"},
                {"title": "Due", "value": "{{DATE(2026-01-01T12:00:00Z, LONG)}}"}
            ]},
            {"type": "RichTextBlock", "horizontalAlignment": "right", "inlines": [
                "plain ",
                {"type": "TextRun", "text": "loud", "weight": "bolder", "italic": true, "strikethrough": true,
                 "underline": true, "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/r"}}
            ]},
            {"type": "ImageSet", "images": [
                {"type": "Image", "url": ICON, "style": "person"},
                {"type": "Image", "url": ICON}
            ]},
            {"type": "Table", "columns": [{"width": 1}, {"width": "60px"}], "rows": [
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "head"}]},
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "er"}]}
                ]},
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "a"}]},
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "b"}]}
                ]}
            ]},
            {"type": "ColumnSet", "horizontalAlignment": "center", "columns": [
                {"type": "Column", "width": "auto", "items": [{"type": "TextBlock", "text": "left"}]},
                {"type": "Column", "width": "50px", "separator": true, "spacing": "large",
                 "items": [{"type": "TextBlock", "text": "mid"}]},
                {"type": "Column", "width": 2, "verticalContentAlignment": "bottom", "spacing": "none",
                 "selectAction": {"type": "Action.ToggleVisibility", "targetElements": ["styled-1"]},
                 "items": [{"type": "TextBlock", "text": "right", "color": "good"}]}
            ]},
            {"type": "Container", "id": "styled", "bleed": true, "style": "emphasis",
             "verticalContentAlignment": "center", "items": containers},
            {"type": "Image", "url": ICON, "size": "small", "horizontalAlignment": "center",
             "selectAction": {"type": "Action.OpenUrl", "url": "https://example.invalid/picture"}},
            {"type": "Image", "url": ICON, "size": "stretch", "horizontalAlignment": "right"},
            {"type": "TextBlock", "text": "a long line that never wraps over a second line in its card", "wrap": false,
             "spacing": "extraLarge", "fontType": "monospace"},
            {"type": "TextBlock", "text": "Heading", "style": "heading", "size": "extraLarge", "spacing": "padding"},
            {"type": "ActionSet", "actions": [
                {"type": "Action.ShowCard", "title": "Inner", "card": {"type": "AdaptiveCard",
                 "body": [{"type": "TextBlock", "text": "inner", "size": "large", "isSubtle": true}],
                 "actions": [{"type": "Action.OpenUrl", "title": "Out", "url": "https://example.invalid/out",
                              "style": "destructive"}]}}
            ]}
        ]
    })
}

#[test]
fn every_kind_of_element_is_drawn() {
    let rig = rig_with(vec![card_message(ID, &every_kind())]);
    let src = LinkPreviewPicture::card_picture_src(ICON).unwrap();
    rig.core.pictures.lock().unwrap().insert(src, png());
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    // tall enough for the whole card, so a click reaches each part of it
    cx.simulate_resize(gpui::size(gpui::px(800.0), gpui::px(2000.0)));
    draw(cx);
    for at in 0..11 {
        assert!(bounds(cx, &format!("card-{ID}-c0b{at}.")).is_some(), "element {at}");
    }
    // the table's cells, the columns, the styled containers
    for selector in ["c0b3.1.1.0.", "c0b4.2.0.", "c0b5.5.0."] {
        assert!(bounds(cx, &format!("card-{ID}-{selector}")).is_some(), "{selector}");
    }
    // a click on the column hides the container its toggle names
    click(cx, &format!("card-{ID}-c0b4.2.s"));
    assert!(bounds(cx, &format!("card-{ID}-c0b5.1.")).is_none());
    // the picture opens its link
    click(cx, &format!("card-{ID}-c0b6.s"));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.invalid/picture"));
    // the action set's shown card, and its own button
    click(cx, &format!("card-{ID}-c0b10.0"));
    click(cx, &format!("card-{ID}-c0b10.0ca0"));
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.invalid/out"));
}
