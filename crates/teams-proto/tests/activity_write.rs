//! The activity feed's writes as the web client sends them (team-manage, 2026-11-12), and the feed's all-read bookmark
//! as the conversation list carries it.
#![cfg(test)]

use serde_json::{Value, json};
use teams_proto::chat::Conversation;
use teams_proto::{ACTIVITY_FEED, MarkFeedRead, Request, SetActivityRead};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn path(request: &impl Request) -> Vec<String> {
    request.endpoint().segments().to_vec()
}

#[test]
fn an_item_is_marked_read_or_unread_by_its_isread_property() -> TestResult {
    let read = SetActivityRead::new("1794315280134", true).ok_or("no request")?;
    assert_eq!(read.method(), teams_proto::Method::Put);
    assert_eq!(
        path(&read),
        [
            "v1",
            "users",
            "ME",
            "conversations",
            ACTIVITY_FEED,
            "messages",
            "1794315280134",
            "properties"
        ]
    );
    assert_eq!(read.endpoint().query(), [("name".to_owned(), "isread".to_owned())]);
    assert_eq!(read.body(), Some(json!({"isread": "true"})));
    let unread = SetActivityRead::new("1794315280134", false).ok_or("no request")?;
    assert_eq!(unread.body(), Some(json!({"isread": "false"})));
    assert!(unread.is_write());
    Ok(())
}

#[test]
fn an_item_id_in_another_form_makes_no_request() {
    assert_eq!(SetActivityRead::new("", true), None);
    assert_eq!(SetActivityRead::new("12/../34", true), None);
    assert_eq!(SetActivityRead::new("19:chat@thread.v2", false), None);
}

#[test]
fn mark_all_read_puts_the_feeds_bookmark() {
    let request = MarkFeedRead::at(1_794_491_924_391);
    assert_eq!(request.bookmark(), "0;1794491924391;0");
    assert_eq!(request.method(), teams_proto::Method::Put);
    assert_eq!(
        path(&request),
        ["v1", "users", "ME", "conversations", ACTIVITY_FEED, "properties"]
    );
    assert_eq!(
        request.endpoint().query(),
        [("name".to_owned(), "consumptionHorizonBookmark".to_owned())]
    );
    assert_eq!(
        request.body(),
        Some(json!({"consumptionHorizonBookmark": "0;1794491924391;0"}))
    );
}

fn listed(properties: &Value) -> TestResult<Conversation> {
    Ok(serde_json::from_value(json!({
        "id": ACTIVITY_FEED,
        "version": 1,
        "properties": properties,
        "threadProperties": {"threadType": "streamofnotifications"},
    }))?)
}

#[test]
fn the_bookmark_is_read_from_the_listed_feed() -> TestResult {
    let feed = listed(&json!({
        "consumptionhorizon": "1794315245771;1794491895849;297781463309",
        "consumptionHorizonBookmark": "0;1794491924391;0",
    }))?;
    assert_eq!(feed.horizon_bookmark(), Some(1_794_491_924_391));
    let horizon = feed.consumption_horizon().ok_or("no horizon")?;
    assert_eq!(horizon.last_read_arrival, Some(1_794_315_245_771));
    assert_eq!(listed(&json!({}))?.horizon_bookmark(), None);
    assert_eq!(
        listed(&json!({"consumptionHorizonBookmark": "garbled"}))?.horizon_bookmark(),
        None
    );
    assert_eq!(
        listed(&json!({"consumptionHorizonBookmark": 5}))?.horizon_bookmark(),
        None
    );
    Ok(())
}
