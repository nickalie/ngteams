//! The activity feed as the store keeps it: items read from the feed's messages (shapes as the web client's feed reads
//! gave them, made up), reactions folded, read state from `isread` and the all-read bookmark, the rail's count, the
//! filters and the search's words.
#![cfg(test)]

use serde_json::{Value, json};
use teams_core::activity::{ActivityFeed, ActivityFilter, ActivityItem, ActivityKind, FEED, MeetingChange, Mention};
use teams_store::{Conversation, ConversationKind, Message, ReadHorizon, Store};

const ME: &str = "8:orgid:00000000-0000-4000-8000-00000000000a";
const B: &str = "8:orgid:00000000-0000-4000-8000-00000000000b";
const C: &str = "8:orgid:00000000-0000-4000-8000-00000000000c";
const GROUP: &str = "19:made-up-group@thread.v2";
const ONE_TO_ONE: &str = "19:made-up_one@unq.gbl.spaces";
const CHANNEL: &str = "19:made-up-channel@thread.tacv2";

fn activity(kind: &str, sub: &str, from: &str, name: &str, thread: &str, message: u64) -> Value {
    json!({
        "activityType": kind, "activitySubtype": sub, "activityId": 900 + message,
        "sourceThreadId": thread, "sourceMessageId": message, "sourceUserId": from,
        "sourceUserImDisplayName": name, "messagePreview": "  a  made-up\npreview ",
    })
}

fn message(id: i64, version: i64, properties: &Value) -> Message {
    Message {
        conversation_id: FEED.to_owned(),
        id: id.to_string(),
        version,
        client_message_id: Some(format!("{id}0")),
        sender: Some(ME.to_owned()),
        sender_name: None,
        message_type: "Text".to_owned(),
        content: String::new(),
        properties: Some(properties.to_string()),
        arrival_time: id,
        compose_time: Some(id),
        edit_time: None,
        delete_time: None,
    }
}

fn item(id: i64, activity: &Value, read: bool) -> Message {
    message(id, id, &json!({"activity": activity, "isread": read}))
}

#[test]
fn an_item_reads_who_what_and_where() {
    let mut properties = activity("mentionInChat", "person", C, "Lab C", GROUP, 1000);
    properties["activityTimestamp"] = json!("2026-01-01T00:00:00.500Z");
    properties["sourceThreadType"] = json!("chat");
    properties["sourceReplyChainId"] = json!("999");
    let item = ActivityItem::from_message(&item(2000, &properties, false)).expect("an item");
    assert_eq!(item.kind, ActivityKind::Mention(Mention::You));
    assert_eq!(item.title(), "Lab C mentioned you");
    assert_eq!(item.preview, "a made-up preview");
    assert_eq!(item.thread.as_deref(), Some(GROUP));
    assert_eq!(item.message.as_deref(), Some("1000"));
    assert_eq!(item.reply_chain.as_deref(), Some("999"));
    assert_eq!(item.activity_id.as_deref(), Some("1900"));
    assert_eq!(item.at_ms, 1_767_225_600_500);
    assert_eq!(item.arrival_ms, 2000);
    assert!(!item.read_flag);
    assert!(!item.in_channel());
    assert_eq!(item.opens(), Some(GROUP));
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "one assertion per kind of item")]
fn each_kind_has_teams_title() {
    let titled = |kind: &str, sub: &str| {
        let item =
            ActivityItem::from_message(&item(1, &activity(kind, sub, B, "Lab B", GROUP, 5), false)).expect("an item");
        (item.kind.clone(), item.title())
    };
    assert_eq!(
        titled("mentionInChat", "everyone"),
        (
            ActivityKind::Mention(Mention::Everyone),
            "Lab B mentioned Everyone".to_owned()
        )
    );
    assert_eq!(titled("mention", "team").1, "Lab B mentioned your team");
    assert_eq!(titled("mention", "channel").1, "Lab B mentioned a channel");
    assert_eq!(titled("mention", "tag").1, "Lab B mentioned your tag");
    assert_eq!(
        titled("reactionInChat", "heart"),
        (
            ActivityKind::Reaction("heart".to_owned()),
            "Lab B reacted to your message".to_owned()
        )
    );
    assert_eq!(titled("like", "").0, ActivityKind::Reaction("like".to_owned()));
    assert_eq!(titled("replyToReply", "").1, "Lab B replied");
    assert_eq!(titled("call", "missedCall").1, "Missed call from Lab B");
    assert_eq!(titled("call", "voicemail").1, "Voicemail from Lab B");
    assert_eq!(titled("voicemail", "").0, ActivityKind::Voicemail);
    assert_eq!(titled("call", "callEnded").0, ActivityKind::Other);
    assert_eq!(titled("msGraph", "privateMeetingUpdated").1, "Lab B updated");
    assert_eq!(titled("msGraph", "privateMeetingCreated").1, "Lab B invited you");
    assert_eq!(titled("msGraph", "channelMeetingCanceled").1, "Lab B canceled");
    assert_eq!(
        titled("msGraph", "privateMeetingForwarded").1,
        "Lab B forwarded a meeting"
    );
    assert_eq!(titled("calendar", "").0, ActivityKind::Meeting(MeetingChange::Updated));
    assert_eq!(
        titled("msGraph", "customNotification"),
        (ActivityKind::App, "Lab B".to_owned())
    );
    assert_eq!(titled("somethingNew", "x"), (ActivityKind::Other, "Lab B".to_owned()));
    assert_eq!(
        titled("teamMembershipChange", "addedToTeam").1,
        "A team owner added you to a team"
    );
}

#[test]
fn an_app_item_takes_its_own_title_and_a_team_add_its_teams_name() {
    let mut app = activity("msGraph", "customNotification", "28:app", "", CHANNEL, 1);
    app["sourceAppDisplayName"] = json!("Workflows");
    app["activityContext"] = json!({"templateParameters": "{\"customMessage\":\"Follow up on a message (AI)\"}"});
    let app = ActivityItem::from_message(&item(1, &app, false)).expect("an item");
    assert_eq!(app.title(), "Follow up on a message (AI)");
    assert_eq!(app.actor_name, "Workflows");
    assert_eq!(app.message, None, "1 is the service's placeholder");
    assert!(app.in_channel());
    let mut object = activity("msGraph", "customNotification", "28:app", "", CHANNEL, 1);
    object["activityContext"] = json!({"templateParameters": {"customMessage": " Made-up "}});
    let object = ActivityItem::from_message(&item(1, &object, false)).expect("an item");
    assert_eq!(object.custom_title.as_deref(), Some("Made-up"));
    let mut team = activity("teamMembershipChange", "addedToTeam", "28:app", "", CHANNEL, 1);
    team["sourceThreadTopic"] = json!("Made-up team");
    team["sourceThreadType"] = json!("space");
    let team = ActivityItem::from_message(&item(1, &team, true)).expect("an item");
    assert_eq!(team.title(), "A team owner added you to Made-up team");
    assert!(team.read_flag);
    let mut nameless = activity("call", "missedCall", B, "", "", 1);
    nameless["sourceUserImDisplayName"] = Value::Null;
    let nameless = ActivityItem::from_message(&item(1, &nameless, false)).expect("an item");
    assert_eq!(nameless.title(), "Missed call from Someone");
    assert_eq!(nameless.opens(), None);
}

#[test]
fn each_kind_has_its_own_key() {
    let kinds = [
        (ActivityKind::Mention(Mention::You), "mention"),
        (ActivityKind::Mention(Mention::Everyone), "everyone"),
        (ActivityKind::Mention(Mention::Team), "team-mention"),
        (ActivityKind::Mention(Mention::Channel), "channel-mention"),
        (ActivityKind::Mention(Mention::Tag), "tag-mention"),
        (ActivityKind::Reaction("heart".to_owned()), "reaction"),
        (ActivityKind::Reply, "reply"),
        (ActivityKind::MissedCall, "missed-call"),
        (ActivityKind::Voicemail, "voicemail"),
        (ActivityKind::AddedToTeam, "added-to-team"),
        (ActivityKind::Meeting(MeetingChange::Invited), "meeting-invited"),
        (ActivityKind::Meeting(MeetingChange::Updated), "meeting-updated"),
        (ActivityKind::Meeting(MeetingChange::Canceled), "meeting-canceled"),
        (ActivityKind::Meeting(MeetingChange::Forwarded), "meeting-forwarded"),
        (ActivityKind::App, "app"),
        (ActivityKind::Other, "other"),
    ];
    for (kind, key) in &kinds {
        assert_eq!(kind.key(), *key);
    }
    let keys: std::collections::BTreeSet<&str> = kinds.iter().map(|(kind, _)| kind.key()).collect();
    assert_eq!(keys.len(), kinds.len(), "no two kinds share a key");
}

#[test]
fn a_message_with_no_activity_or_removed_is_no_item() {
    assert_eq!(
        ActivityItem::from_message(&message(1, 1, &json!({"isread": false}))),
        None
    );
    let mut removed = item(1, &activity("mention", "person", B, "Lab B", GROUP, 5), false);
    removed.delete_time = Some(2);
    assert_eq!(ActivityItem::from_message(&removed), None);
    let mut unreadable = removed.clone();
    unreadable.delete_time = None;
    unreadable.properties = Some("not json".to_owned());
    assert_eq!(ActivityItem::from_message(&unreadable), None);
    unreadable.properties = None;
    assert_eq!(ActivityItem::from_message(&unreadable), None);
}

#[test]
fn isread_reads_as_a_boolean_or_its_text() {
    let flag = |value: Value| {
        let mut message = item(1, &activity("mention", "person", B, "Lab B", GROUP, 5), false);
        message.properties =
            Some(json!({"activity": activity("mention", "person", B, "Lab B", GROUP, 5), "isread": value}).to_string());
        ActivityItem::from_message(&message).expect("an item").read_flag
    };
    assert!(flag(json!(true)));
    assert!(flag(json!("True")));
    assert!(!flag(json!("false")));
    assert!(!flag(Value::Null));
}

#[test]
fn reactions_to_one_message_fold_into_the_newest() {
    let messages = [
        item(
            1000,
            &activity("reactionInChat", "like", B, "Lab B", ONE_TO_ONE, 50),
            true,
        ),
        item(
            2000,
            &activity("reactionInChat", "heart", C, "Lab C", ONE_TO_ONE, 50),
            false,
        ),
        item(
            3000,
            &activity("reactionInChat", "like", C, "Lab C", ONE_TO_ONE, 51),
            true,
        ),
        item(
            4000,
            &activity("mentionInChat", "person", B, "Lab B", ONE_TO_ONE, 50),
            true,
        ),
    ];
    let feed = ActivityFeed::new(&messages, None, None);
    assert_eq!(feed.items.len(), 3);
    let folded = feed.item("1000").expect("folded");
    assert_eq!(folded.id, "2000");
    assert_eq!(folded.ids, ["2000", "1000"]);
    assert_eq!(folded.others, 1);
    assert_eq!(folded.title(), "Lab C +1 reacted to your message");
    assert!(!folded.read_flag, "unread while any of its own is");
    assert_eq!(feed.items[0].id, "4000", "the newest first");
}

#[test]
fn the_same_person_reacting_twice_is_not_counted_again() {
    let messages = [
        item(
            1000,
            &activity("reactionInChat", "like", B, "Lab B", ONE_TO_ONE, 50),
            true,
        ),
        item(
            2000,
            &activity("reactionInChat", "heart", B, "Lab B", ONE_TO_ONE, 50),
            true,
        ),
    ];
    let feed = ActivityFeed::new(&messages, None, None);
    assert_eq!(feed.items.len(), 1);
    assert_eq!(feed.items[0].others, 0);
    assert!(feed.is_read(&feed.items[0]));
}

#[test]
fn the_bookmark_reads_older_versions_read_and_unread_marks_stay_unread() {
    let messages = [
        item(1000, &activity("mention", "person", B, "Lab B", GROUP, 5), false),
        // marked unread after the bookmark: its version moved past it
        message(
            2000,
            9000,
            &json!({"activity": activity("mention", "person", C, "Lab C", GROUP, 6), "isread": false}),
        ),
        item(8500, &activity("call", "missedCall", B, "Lab B", "", 1), true),
    ];
    let feed = ActivityFeed::new(&messages, Some(1500), Some(8000));
    let read: Vec<bool> = feed.items.iter().map(|item| feed.is_read(item)).collect();
    // newest first: the missed call (read), the unread mark, the bookmarked one
    assert_eq!(read, [true, false, true]);
    assert_eq!(feed.unseen(), 1, "the unread mark is newer than the horizon");
    let unbookmarked = ActivityFeed::new(&messages, Some(1500), None);
    assert_eq!(unbookmarked.unseen(), 1, "the one at 1000 is older than the horizon");
    assert_eq!(ActivityFeed::new(&messages, None, None).unseen(), 2);
}

#[test]
fn the_horizon_to_put_is_the_newest_items_until_it_is_there() {
    let messages = [
        item(1000, &activity("mention", "person", B, "Lab B", GROUP, 5), false),
        item(2000, &activity("mention", "person", C, "Lab C", GROUP, 6), false),
    ];
    let feed = ActivityFeed::new(&messages, Some(1000), None);
    assert_eq!(feed.seen_horizon().map(|item| item.id.as_str()), Some("2000"));
    assert_eq!(ActivityFeed::new(&messages, Some(2000), None).seen_horizon(), None);
    assert_eq!(ActivityFeed::new(&[], None, None).seen_horizon(), None);
}

#[test]
fn each_chip_keeps_its_kind_and_the_words_narrow_it() {
    let messages = [
        item(1, &activity("mentionInChat", "person", B, "Lab B", GROUP, 5), false),
        item(2, &activity("reactionInChat", "like", C, "Lab C", GROUP, 6), true),
        item(3, &activity("call", "missedCall", B, "Lab B", "", 1), true),
        item(4, &activity("reply", "", C, "Lab C", CHANNEL, 7), true),
        item(5, &activity("call", "voicemail", B, "Lab B", "", 1), false),
        item(
            6,
            &activity("msGraph", "customNotification", "28:app", "Workflows", CHANNEL, 1),
            true,
        ),
    ];
    let feed = ActivityFeed::new(&messages, None, None);
    let shown = |filter: ActivityFilter, words: &str| -> Vec<String> {
        feed.shown(filter, words).map(|item| item.id.clone()).collect()
    };
    assert_eq!(shown(ActivityFilter::All, "").len(), 6);
    assert_eq!(shown(ActivityFilter::Unread, ""), ["5", "1"]);
    assert_eq!(shown(ActivityFilter::Mentions, ""), ["1"]);
    assert_eq!(shown(ActivityFilter::Reactions, ""), ["2"]);
    assert_eq!(shown(ActivityFilter::MissedCalls, ""), ["3"]);
    assert_eq!(shown(ActivityFilter::Replies, ""), ["4"]);
    assert_eq!(shown(ActivityFilter::Voicemail, ""), ["5"]);
    assert_eq!(shown(ActivityFilter::Apps, ""), ["6"]);
    assert_eq!(shown(ActivityFilter::All, "lab  c"), ["4", "2"]);
    assert_eq!(shown(ActivityFilter::All, "  "), shown(ActivityFilter::All, ""));
    assert_eq!(shown(ActivityFilter::All, "  WORKFLOWS "), ["6"]);
}

#[test]
fn chips_have_words_and_keys_both_ways() {
    for filter in std::iter::once(ActivityFilter::All).chain(ActivityFilter::CHIPS) {
        assert_eq!(ActivityFilter::of_key(filter.key()), Some(filter));
        assert!(!filter.label().is_empty());
    }
    assert_eq!(ActivityFilter::of_key("nothing"), None);
    assert_eq!(ActivityFilter::default(), ActivityFilter::All);
    assert_eq!(ActivityFilter::Mentions.label(), "@Mentions");
}

#[test]
fn the_feed_is_read_from_the_store_with_its_horizon_and_bookmark() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut store = Store::open(&dir.path().join("store.sqlite")).expect("store");
    store
        .upsert_conversation(&Conversation {
            id: FEED.to_owned(),
            kind: ConversationKind::Other("notifications".to_owned()),
            topic: None,
            version: 1,
            last_activity: None,
        })
        .expect("feed");
    for message in [
        item(1000, &activity("mention", "person", B, "Lab B", GROUP, 5), false),
        item(2000, &activity("mention", "person", C, "Lab C", GROUP, 6), false),
    ] {
        store.upsert_message(&message, None).expect("item");
    }
    store
        .set_read_horizon(&ReadHorizon {
            conversation_id: FEED.to_owned(),
            user: ME.to_owned(),
            last_read_arrival: 1000,
            updated: 5,
            raw: None,
        })
        .expect("horizon");
    store.set_horizon_bookmark(FEED, 1500).expect("bookmark");
    let feed = ActivityFeed::from_store(&store, ME).expect("feed");
    assert_eq!(feed.items.len(), 2);
    assert_eq!(feed.seen_ms, Some(1000));
    assert_eq!(feed.bookmark_ms, Some(1500));
    assert_eq!(feed.unseen(), 1);
}
