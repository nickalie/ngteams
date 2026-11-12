//! The account's own activity feed's writes (`48:notifications`, `docs/protocol/chat.md`, "The activity feed"): an
//! item marked read or unread, and everything marked read. They reach nobody but the account: the feed is its own.
//! The feed's "seen" horizon, put when the Activity pane opens, is a [`ReadReceipt`](super::ReadReceipt) on the feed.

use serde_json::json;

use super::{Prepared, Wire};
use crate::endpoint::{Endpoint, Method, Service};

/// The account's activity feed: a `48:` stream of the chat service (`threadType` `streamofnotifications`).
pub const ACTIVITY_FEED: &str = "48:notifications";

/// Marks one feed item read or unread: `PUT v1/users/ME/conversations/48:notifications/messages/<item>/properties
/// ?name=isread` with `{"isread": "true"}` or `{"isread": "false"}` (the value a string), as the web client sends it
/// when an item is opened and from an item's "Mark as read" or "Mark as unread" (team-manage, 2026-11-12). The service
/// answers 200 and moves the item's `version` to the time of the change, so a sync brings it back changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetActivityRead {
    item: String,
    read: bool,
}

impl SetActivityRead {
    /// Item `item` (its id: digits) marked read (`read`) or unread; `None` for an id in another form.
    #[must_use]
    pub fn new(item: &str, read: bool) -> Option<Self> {
        digits(item).then(|| Self {
            item: item.to_owned(),
            read,
        })
    }
}

impl Prepared for SetActivityRead {
    fn wire(&self) -> Option<Wire> {
        let endpoint = Endpoint::new([
            "v1",
            "users",
            "ME",
            "conversations",
            ACTIVITY_FEED,
            "messages",
            self.item.as_str(),
            "properties",
        ])
        .with_query("name", "isread");
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint,
            body: Some(json!({"isread": if self.read { "true" } else { "false" }})),
        })
    }
}

/// Marks the whole feed read as of a time: `PUT v1/users/ME/conversations/48:notifications/properties
/// ?name=consumptionHorizonBookmark` with `{"consumptionHorizonBookmark": "0;<ms>;0"}`, the header menu's "Mark all as
/// read" (team-manage, 2026-11-12; the web client then puts the feed's `consumptionhorizon` too). Every item whose
/// `version` is no later than that time reads as read from then on, unless it is marked unread after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkFeedRead {
    time_ms: i64,
}

impl MarkFeedRead {
    /// Everything up to `time_ms` (ms since the Unix epoch) read.
    #[must_use]
    pub fn at(time_ms: i64) -> Self {
        Self { time_ms }
    }

    /// The bookmark as sent: `0;<ms>;0`.
    #[must_use]
    pub fn bookmark(&self) -> String {
        format!("0;{};0", self.time_ms)
    }
}

impl Prepared for MarkFeedRead {
    fn wire(&self) -> Option<Wire> {
        let endpoint = Endpoint::new(["v1", "users", "ME", "conversations", ACTIVITY_FEED, "properties"])
            .with_query("name", "consumptionHorizonBookmark");
        Some(Wire {
            raw: None,
            service: Service::ChatService,
            headers: Vec::new(),
            method: Method::Put,
            endpoint,
            body: Some(json!({"consumptionHorizonBookmark": self.bookmark()})),
        })
    }
}

fn digits(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}
