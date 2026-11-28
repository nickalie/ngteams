//! The text of a card (`teams_proto::chat::card::card_message_text`, `cards_text`). Every card here is synthetic, in
//! the shapes that monitoring alerts, webhooks and bots post.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use teams_proto::chat::card::{
    ADAPTIVE, CARD_TYPE, CONNECTOR, HERO, THUMBNAIL, card_message_text, cards_text, message_cards, message_cards_text,
};

/// A `RichText/Media_Card` content that carries `activity`, as the chat service gives it.
fn content(activity: &Value) -> String {
    let b64 = STANDARD.encode(serde_json::to_vec(activity).unwrap());
    format!(
        "<URIObject type=\"SWIFT.1\">Card - access it on <a href=\"https://go.skype.com/cards.unsupported\">\
         https://go.skype.com/cards.unsupported</a>. <Title>Card</Title><Swift b64=\"{b64}\"></Swift></URIObject>"
    )
}

fn text_of(attachments: &Value) -> String {
    card_message_text(&content(&json!({"type": "message/card", "attachments": attachments}))).unwrap()
}

fn adaptive(body: &Value, actions: &Value) -> String {
    text_of(&json!([{"contentType": ADAPTIVE, "content": {
        "type": "AdaptiveCard", "version": "1.5", "body": body, "actions": actions,
    }}]))
}

#[test]
fn an_alert_card_gives_its_texts_facts_and_links_in_order() {
    let text = adaptive(
        &json!([
            {"type": "TextBlock", "text": "**[FIRING:1] HighLatency**", "weight": "Bolder"},
            {"type": "TextBlock", "text": "The p99 latency is over 2 s.\n\nSee the _runbook_."},
            {"type": "FactSet", "facts": [
                {"title": "severity", "value": "critical"},
                {"title": "", "value": "no title"},
            ]},
            {"type": "TextBlock", "text": "hidden", "isVisible": false},
            {"type": "RichTextBlock", "inlines": [
                {"type": "TextRun", "text": "Since "},
                {"type": "TextRun", "text": "{{DATE(2026-11-04T18:15:06Z, SHORT)}} {{TIME(2026-11-04T18:15:06Z)}}"},
            ]},
        ]),
        &json!([
            {"type": "Action.OpenUrl", "title": "Open the dashboard", "url": "https://example.invalid/d"},
            {"type": "Action.OpenUrl", "title": " ", "url": "https://example.invalid/bare"},
            {"type": "Action.Submit", "title": "Acknowledge", "data": {"ack": 1}},
            {"type": "Action.ToggleVisibility", "title": "More", "targetElements": ["x"]},
            {"type": "Action.ShowCard", "title": "Details", "card": {"type": "AdaptiveCard",
                "body": [{"type": "TextBlock", "text": "pod: api-1"}],
                "actions": [{"type": "Action.OpenUrl", "title": "Logs", "url": "https://example.invalid/l"}]}},
        ]),
    );
    assert_eq!(
        text,
        "[FIRING:1] HighLatency\n\
         The p99 latency is over 2 s.\nSee the runbook.\n\
         severity: critical\n\
         no title\n\
         Since 2026-11-04 18:15 UTC\n\
         Open the dashboard (https://example.invalid/d)\n\
         https://example.invalid/bare\n\
         Acknowledge\n\
         More\n\
         Details\n\
         pod: api-1\n\
         Logs (https://example.invalid/l)"
    );
}

#[test]
fn nested_containers_columns_and_tables_give_their_text_in_order() {
    let text = adaptive(
        &json!([
            {"type": "Container", "items": [
                {"type": "TextBlock", "text": "outer"},
                {"type": "Container", "items": [
                    {"type": "ColumnSet", "columns": [
                        {"type": "Column", "items": [{"type": "TextBlock", "text": "left"}]},
                        {"type": "Column", "isVisible": false, "items": [{"type": "TextBlock", "text": "gone"}]},
                        {"type": "Column", "items": [
                            {"type": "Container", "items": [{"type": "TextBlock", "text": "deep"}]},
                        ]},
                    ]},
                ]},
            ]},
            {"type": "Table", "columns": [{"width": 1}, {"width": 1}], "rows": [
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "host"}]},
                    {"type": "TableCell", "items": [{"type": "TextBlock", "text": "db-1"}]},
                ]},
                {"type": "TableRow", "cells": [
                    {"type": "TableCell", "items": [{"type": "Image", "url": "https://example.invalid/i.png"}]},
                ]},
            ]},
            {"type": "ActionSet", "actions": [{"type": "Action.Submit", "title": "Silence"}]},
        ]),
        &json!([]),
    );
    assert_eq!(text, "outer\nleft\ndeep\nhost | db-1\nSilence");
}

#[test]
fn a_card_without_text_gives_no_text() {
    assert_eq!(adaptive(&json!([]), &json!([])), "");
    let picture = adaptive(
        &json!([
            {"type": "Image", "url": "https://example.invalid/p.png", "altText": "a graph"},
            {"type": "ImageSet", "images": [{"type": "Image", "url": "https://example.invalid/q.png"}]},
            {"type": "Input.Text", "id": "comment", "fallback": "drop"},
        ]),
        &json!([]),
    );
    assert_eq!(picture, "");
    assert_eq!(text_of(&json!([])), "");
    assert_eq!(
        text_of(&json!([{"contentType": "application/vnd.other", "content": {}}])),
        ""
    );
    assert_eq!(card_message_text("<p>no card</p>"), None);
    assert_eq!(card_message_text(&content(&json!({"type": "message"}))), None);
}

#[test]
fn hero_and_thumbnail_cards_give_their_title_subtitle_text_and_buttons() {
    let text = text_of(&json!([
        {"contentType": HERO, "content": {"title": "Build 42", "subtitle": "main", "text": "Passed",
            "images": [{"url": "https://example.invalid/h.png"}],
            "buttons": [{"type": "openUrl", "title": "Open", "value": "https://example.invalid/b"}]}},
        {"contentType": THUMBNAIL, "content": {"title": "Second card"}},
    ]));
    assert_eq!(
        text,
        "Build 42\nmain\nPassed\nOpen (https://example.invalid/b)\n\nSecond card"
    );
}

#[test]
fn a_connector_card_gives_its_title_text_sections_facts_and_actions() {
    let text = text_of(&json!([{"contentType": CONNECTOR, "content": {
        "@type": "MessageCard",
        "title": "Disk almost full",
        "summary": "Disk alert",
        "text": "The disk of **db-1** is 95% full.",
        "themeColor": "FF0000",
        "sections": [
            {"activityTitle": "vmalert", "activitySubtitle": "group disk", "activityText": "firing",
             "activityImage": "https://example.invalid/a.png",
             "title": "Labels", "text": "From the rule.",
             "facts": [{"name": "instance", "value": "db-1:9100"}, {"value": "only a value"}],
             "potentialAction": [{"@type": "OpenUri", "name": "Silence", "targets": [
                 {"os": "iOS", "uri": "https://example.invalid/ios"},
                 {"os": "default", "uri": "https://example.invalid/s"},
             ]}]},
        ],
        "potentialAction": [
            {"@type": "OpenUri", "name": "Source", "targets": [{"os": "android", "uri": "https://example.invalid/g"}]},
            {"@type": "OpenUri", "name": "Bad link", "targets": [{"uri": "javascript:alert(1)"}]},
            {"@type": "HttpPOST", "name": "Resolve", "target": "https://example.invalid/r"},
        ],
    }}]));
    assert_eq!(
        text,
        "Disk almost full\nDisk alert\nThe disk of db-1 is 95% full.\n\
         vmalert\ngroup disk\nfiring\nLabels\nFrom the rule.\n\
         instance: db-1:9100\nonly a value\n\
         Silence (https://example.invalid/s)\n\
         Source (https://example.invalid/g)\nBad link\nResolve"
    );
    let same = text_of(&json!([{"contentType": CONNECTOR, "content": {"title": "Up", "summary": "Up"}}]));
    assert_eq!(same, "Up");
    let empty = text_of(&json!([{"contentType": CONNECTOR, "content": {}}]));
    assert_eq!(empty, "");
}

#[test]
fn a_link_s_app_cards_give_their_text() {
    let cards = json!([{"appId": "synthetic-app", "appName": "Synthetic", "attachment": null,
        "contentType": ADAPTIVE, "content": {"type": "AdaptiveCard",
            "body": [{"type": "TextBlock", "text": "A video title"}]}}]);
    let message = message_cards(Some(&cards)).unwrap();
    assert_eq!(cards_text(&message), "A video title");
}

#[test]
fn a_message_s_cards_give_their_text_by_its_type() {
    let card = content(&json!({"attachments": [{"contentType": HERO, "content": {"title": "Card"}}]}));
    assert_eq!(message_cards_text(CARD_TYPE, &card, None).as_deref(), Some("Card"));
    let properties = json!({"cards": [{"contentType": ADAPTIVE, "content": {"type": "AdaptiveCard",
        "body": [{"type": "TextBlock", "text": "App card"}]}}]})
    .to_string();
    assert_eq!(
        message_cards_text("RichText/Html", "<p>link</p>", Some(&properties)).as_deref(),
        Some("App card")
    );
    assert_eq!(message_cards_text("RichText/Html", "<p>link</p>", Some("{}")), None);
    assert_eq!(
        message_cards_text("RichText/Html", "<p>link</p>", Some("not json")),
        None
    );
    assert_eq!(message_cards_text("Text", "plain", None), None);
}
