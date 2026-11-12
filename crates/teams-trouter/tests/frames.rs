//! The socket.io 0.9 reading on M1 Task 14's synthetic fixtures (`tests/fixtures/trouter/`): every frame of the
//! captured session reads as its packet, `trouter.connected` and the message-loss report hand over what the client
//! keeps, and every captured delivery reads into its typed chat event. The connect URL's allowlist too.
//!
//! Ids, tokens and hosts here are the fixtures' made-up ones.

use serde_json::{Value, json};
use teams_proto::DroppedIndicator;
use teams_proto::chat::{AppConsumptionHorizon, MessageType};
use teams_trouter::frames::{Connected, DecodeError, Event, Incoming, decode, message_loss};
use teams_trouter::socket::allowed;
use teams_trouter::{Change, ChatEvent, Delivery};
use url::Url;

const SESSION: &str = include_str!("../../../tests/fixtures/trouter/session.json");
const DELIVERIES: &str = include_str!("../../../tests/fixtures/trouter/deliveries.json");

const LAB_A: &str = "00000000-0000-4000-8000-00000000000a";
const LAB_B: &str = "00000000-0000-4000-8000-00000000000b";

#[cfg(test)]
fn chat() -> String {
    format!("19:{LAB_A}_{LAB_B}@unq.gbl.spaces")
}

/// A fixture with its GUID placeholders filled in.
#[cfg(test)]
fn filled(text: &str) -> Value {
    let mut text = text
        .replace("<guid:a>", LAB_A)
        .replace("<guid:b>", LAB_B)
        .replace("<guid:tenant>", "00000000-0000-4000-8000-000000000001");
    for n in (1..=40).rev() {
        text = text.replace(&format!("<guid:{n}>"), &format!("00000000-0000-4000-8000-{n:012}"));
    }
    serde_json::from_str(&text).unwrap()
}

#[cfg(test)]
fn session_frame(name: &str) -> String {
    let session: Value = serde_json::from_str(SESSION).unwrap();
    session["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| frame["name"] == name)
        .unwrap_or_else(|| panic!("no frame {name:?}"))["frame"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[cfg(test)]
fn event(text: &str) -> teams_trouter::frames::Event {
    match decode(text).unwrap() {
        Incoming::Event(event) => event,
        other => panic!("not an event: {other:?}"),
    }
}

#[test]
fn every_frame_of_the_captured_session_reads_as_its_packet() {
    let session: Value = serde_json::from_str(SESSION).unwrap();
    let frames = session["frames"].as_array().unwrap();
    assert!(frames.len() >= 12);
    for frame in frames {
        let text = frame["frame"].as_str().unwrap();
        let read = decode(text).unwrap_or_else(|error| panic!("{}: {error}", frame["name"]));
        let kind = match read {
            Incoming::Disconnect { .. } => '0',
            Incoming::Connect => '1',
            Incoming::Heartbeat => '2',
            Incoming::Delivery(_) => '3',
            Incoming::Event(_) => '5',
            Incoming::Ack { .. } => '6',
            Incoming::Other { .. } => '?',
        };
        assert!(text.starts_with(kind), "{}", frame["name"]);
    }
    assert_eq!(
        decode(&session_frame(
            "the service closing a socket the client was slow to authenticate"
        ))
        .unwrap(),
        Incoming::Disconnect {
            reason: Some("timeout".to_owned())
        }
    );
    assert_eq!(
        decode(&session_frame("the client's disconnect")).unwrap(),
        Incoming::Disconnect { reason: None }
    );
    assert_eq!(
        decode(&session_frame("the service's answer to the keep-alive")).unwrap(),
        Incoming::Ack {
            id: 3,
            args: serde_json::json!(["pong"])
        }
    );
    let ping = event(&session_frame(
        "the keep-alive, every 40 s and after every response the client sends",
    ));
    assert_eq!(
        (ping.id, ping.acknowledged, ping.name.as_str()),
        (Some(3), true, "ping")
    );
    let auth = event(&session_frame(
        "the first socket's authentication, without reconnect parameters",
    ));
    assert_eq!(
        (auth.id, auth.acknowledged, auth.name.as_str()),
        (None, false, "user.authenticate")
    );
}

#[test]
fn trouter_connected_hands_over_the_endpoint_its_push_url_and_where_to_reconnect() {
    let connected = Connected::from_event(&event(&session_frame("the endpoint and its push URLs"))).unwrap();
    assert_eq!(connected.id, "SyntheticEndpoint00000A");
    assert_eq!(
        connected.push_url,
        "https://pub-ent-zz-01-f.trouter.teams.microsoft.com:3443/v4/f/SyntheticEndpoint00000A/"
    );
    assert_eq!(
        connected.reconnect_url.as_deref(),
        Some("wss://pub-ent-zz-01-t.trouter.teams.microsoft.com:443/v4/c")
    );
    assert_eq!(connected.ttl, Some(600));
    let params = connected.params.unwrap();
    assert_eq!(
        (params.sr.as_str(), params.sp.as_str()),
        ("SyntheticEndpoint00000A", "pub-ent-zz-01")
    );
    assert_eq!(
        (params.se.as_str(), params.st.as_str()),
        ("1700000600000", "1700000000000")
    );
    // the push URL is one the messaging registration takes
    assert!(teams_proto::EndpointRegistration::messaging(LAB_A, &connected.push_url).is_some());

    let other = event(r#"5:1::{"name":"trouter.connected","args":[{"id":"x"}]}"#);
    assert_eq!(Connected::from_event(&other), None, "no surl");
}

#[test]
fn the_message_loss_report_hands_its_indicators_back_as_they_came() {
    let report = event(&session_frame(
        "the service's report of what the endpoint missed while away (sent more than once)",
    ));
    assert_eq!(
        message_loss(&report),
        Some(vec![DroppedIndicator {
            tag: "messaging".to_owned(),
            etag: "2000-01-01T00:00:00.0000001Z".to_owned(),
        }])
    );
    assert_eq!(message_loss(&event(r#"5:2::{"name":"ping"}"#)), None);
}

#[cfg(test)]
fn deliveries() -> Vec<(String, Delivery)> {
    let fixture = filled(DELIVERIES);
    fixture["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let Incoming::Delivery(raw) = decode(case["frame"].as_str().unwrap()).unwrap() else {
                panic!("{} isn't a delivery", case["name"]);
            };
            (case["name"].as_str().unwrap().to_owned(), Delivery::from_raw(&raw))
        })
        .collect()
}

#[test]
fn the_captured_new_messages_read_as_typing_and_a_message() {
    let read = deliveries();
    assert_eq!(read.len(), 8);
    assert_eq!(
        read[0].1.event,
        ChatEvent::Typing {
            conversation: chat(),
            from: Some(format!("8:orgid:{LAB_A}"))
        }
    );
    let ChatEvent::Message { conversation, message } = &read[1].1.event else {
        panic!("{}: {:?}", read[1].0, read[1].1.event);
    };
    assert_eq!(conversation, &chat());
    assert_eq!(message.message_type, MessageType::RichTextHtml);
    assert_eq!(message.content, "<p>lab message synthetic-0001</p>");
    assert_eq!(
        message.client_message_id.as_known().map(String::as_str),
        Some("1000000000000000001")
    );
    assert_eq!(message.sender(), Some(format!("8:orgid:{LAB_A}").as_str()));
    for (name, delivery) in &read {
        assert!(!delivery.stored, "{name}: a live delivery");
        assert!(delivery.message_id.is_some(), "{name}");
    }
}

#[test]
fn the_captured_message_updates_read_as_an_edit_reactions_and_a_delete() {
    let read = deliveries();
    for (index, change, version) in [
        (3, Change::Edit, 1_700_000_015_000),
        (4, Change::Reaction, 1_700_000_020_000),
        (5, Change::Reaction, 1_700_000_021_000),
        (6, Change::Delete, 1_700_000_030_000),
    ] {
        let (name, delivery) = &read[index];
        let ChatEvent::MessageUpdate {
            conversation,
            change: got,
            message,
        } = &delivery.event
        else {
            panic!("{name}: {:?}", delivery.event);
        };
        assert_eq!((conversation, *got), (&chat(), change), "{name}");
        assert_eq!(message.id, "1700000001000", "{name}");
        assert_eq!(message.version.as_known(), Some(&version), "{name}");
    }
}

#[test]
fn the_captured_chat_updates_read_as_a_chat_update_and_the_read_horizon() {
    let read = deliveries();
    let ChatEvent::ConversationUpdate { conversation } = &read[2].1.event else {
        panic!("{}: {:?}", read[2].0, read[2].1.event);
    };
    assert_eq!(conversation.id, chat());
    assert_eq!(conversation.last_message().unwrap().id, "1700000001000");
    let ChatEvent::ReadHorizon { conversation, horizon } = &read[7].1.event else {
        panic!("{}: {:?}", read[7].0, read[7].1.event);
    };
    assert_eq!(conversation, &chat());
    assert_eq!(horizon.last_read_arrival, Some(1_700_000_001_000));
    assert_eq!(horizon.client_message_id.as_deref(), Some("1000000000000000001"));
}

#[test]
fn the_accounts_own_properties_are_read_only_for_its_mark_all_read_horizon() {
    let body = |resource: Value| json!({"resourceType": "CustomUserProperties", "resource": resource}).to_string();
    assert_eq!(
        ChatEvent::from_body(&body(json!({"appConsumptionHorizon": "{\"chat\":5,\"channel\":7}"}))),
        ChatEvent::AppReadHorizon(AppConsumptionHorizon { chat: 5, channel: 7 })
    );
    assert_eq!(
        ChatEvent::from_body(&body(json!({"recentReactions": "[]", "inQuarantine": false}))),
        ChatEvent::Unknown {
            resource_type: "CustomUserProperties".to_owned()
        }
    );
}

#[test]
fn a_delivery_with_a_negative_id_is_read() {
    // the service's delivery ids are signed 32-bit numbers: about one in ten live ones is negative
    for id in ["-2134536761", "-1", "0", "2135268894"] {
        let frame = format!(
            r#"3:::{{"id":{id},"method":"POST","url":"/v4/f/E/messaging","headers":{{}},"body":"{{\"resourceType\":\"NewMessage\",\"resource\":{{\"id\":\"1\",\"messagetype\":\"Text\",\"to\":\"19:x@thread.v2\"}}}}"}}"#
        );
        let Incoming::Delivery(raw) = decode(&frame).unwrap() else {
            panic!("{id}: a delivery");
        };
        assert_eq!(raw.id.to_string(), id);
        assert_eq!(Delivery::from_raw(&raw).id, raw.id);
    }
    for id in ["1.5", "\"7\"", "null"] {
        let frame = format!(r#"3:::{{"id":{id},"method":"POST","url":"/v4/f/E/messaging","headers":{{}},"body":""}}"#);
        assert!(decode(&frame).is_err(), "{id}");
    }
}

#[test]
fn a_stored_replay_is_told_from_a_live_delivery_and_an_unknown_kind_is_kept_by_its_name() {
    let stored = r#"3:::{"id":7,"method":"POST","url":"/v4/f/E/messaging","headers":{"trouter-request":"{\"id\":\"x\",\"mstore\":true,\"flow\":\"messaging\",\"seq\":3}"},"body":"{\"type\":\"EventMessage\",\"resourceType\":\"ThreadUpdate\",\"resource\":{}}"}"#;
    let Incoming::Delivery(raw) = decode(stored).unwrap() else {
        panic!("a delivery");
    };
    let delivery = Delivery::from_raw(&raw);
    assert!(delivery.stored);
    assert_eq!(delivery.id, 7);
    assert_eq!(
        delivery.event,
        ChatEvent::Unknown {
            resource_type: "ThreadUpdate".to_owned()
        }
    );
    assert!(
        !format!("{raw:?}").contains("EventMessage"),
        "a delivery's Debug shows no body"
    );
    assert_eq!(
        ChatEvent::from_body("not json"),
        ChatEvent::Unknown {
            resource_type: String::new()
        }
    );
    // a message without a chat to put it in isn't guessed at
    assert_eq!(
        ChatEvent::from_body(r#"{"resourceType":"NewMessage","resource":{"id":"1","messagetype":"Text"}}"#),
        ChatEvent::Unknown {
            resource_type: "NewMessage".to_owned()
        }
    );
}

#[test]
fn a_control_message_is_a_signal_never_a_message() {
    // the captured typing delivery, with another messagetype
    let fixture = filled(DELIVERIES);
    let Incoming::Delivery(raw) = decode(fixture["deliveries"][0]["frame"].as_str().unwrap()).unwrap() else {
        panic!("a delivery");
    };
    let body = |kind: &str| {
        let mut body: Value = serde_json::from_str(&raw.body).unwrap();
        body["resource"]["messagetype"] = Value::from(kind);
        ChatEvent::from_body(&body.to_string())
    };
    assert_eq!(
        body("Control/ClearTyping"),
        ChatEvent::TypingStopped {
            conversation: chat(),
            from: Some(format!("8:orgid:{LAB_A}"))
        }
    );
    // a control no capture showed isn't a message either
    assert_eq!(
        body("Control/LiveState"),
        ChatEvent::Unknown {
            resource_type: "NewMessage".to_owned()
        }
    );
    assert!(matches!(body("RichText/Html"), ChatEvent::Message { .. }));
    assert!(MessageType::from("Control/ClearTyping".to_owned()).is_control());
    assert!(!MessageType::RichTextHtml.is_control());
}

#[test]
fn a_socket_connects_only_to_the_global_or_a_regional_trouter_host() {
    for url in [
        "wss://go.trouter.teams.microsoft.com/v4/c?tc=%7B%7D&timeout=40",
        "wss://go.trouter.teams.microsoft.com:443/v4/c",
        "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com/v4/c?epid=x",
        "wss://pub-ent-zz-01-t.trouter.teams.microsoft.com:443/v4/c",
    ] {
        assert!(allowed(&Url::parse(url).unwrap()), "{url}");
    }
    for url in [
        "ws://go.trouter.teams.microsoft.com/v4/c",
        "https://go.trouter.teams.microsoft.com/v4/c",
        "wss://go.trouter.teams.microsoft.com:8443/v4/c",
        "wss://go.trouter.teams.microsoft.com/v4/f",
        "wss://go.trouter.teams.microsoft.com/v4/c#x",
        "wss://user@go.trouter.teams.microsoft.com/v4/c",
        "wss://pub-ent-zz-01-f.trouter.teams.microsoft.com/v4/c",
        "wss://-t.trouter.teams.microsoft.com/v4/c",
        "wss://x-t.trouter.teams.microsoft.com.example.test/v4/c",
        "wss://evil.example.test/v4/c",
        "wss://127.0.0.1/v4/c",
    ] {
        assert!(!allowed(&Url::parse(url).unwrap()), "{url}");
    }
}

#[test]
fn malformed_packet_envelopes_and_typed_payloads_are_rejected() {
    for text in ["1", "1:", "10::", "-1::", "a::", "１::", ":id:"] {
        assert_eq!(decode(text), Err(DecodeError::NotAPacket), "{text}");
    }
    for text in ["3::", "3:::null", "3:::[]", "3:::false", "3:::not-json"] {
        assert_eq!(decode(text), Err(DecodeError::Data(3)), "{text}");
    }
    for kind in [4, 7, 8, 9] {
        assert_eq!(
            decode(&format!("{kind}:::opaque:payload")),
            Ok(Incoming::Other { kind })
        );
    }
    for data in ["", "null", "[]", "{}", r#"{"reason":7}"#, "bad-json"] {
        assert_eq!(
            decode(&format!("0:::{data}")),
            Ok(Incoming::Disconnect { reason: None })
        );
    }
    assert_eq!(
        decode(r#"0:::{"reason":"synthetic:closed"}"#),
        Ok(Incoming::Disconnect {
            reason: Some("synthetic:closed".to_owned())
        })
    );
}

#[test]
fn deliveries_require_typed_identifiers_methods_urls_and_object_headers() {
    let valid = serde_json::json!({"id": 7, "method": "POST", "url": "/synthetic"});
    for key in ["id", "method", "url"] {
        let mut missing = valid.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert_eq!(
            decode(&format!("3:::{missing}")),
            Err(DecodeError::Data(3)),
            "missing {key}"
        );
        for bad in [
            Value::Null,
            Value::Bool(true),
            serde_json::json!([]),
            serde_json::json!({}),
        ] {
            let mut wrong = valid.clone();
            wrong[key] = bad;
            assert_eq!(decode(&format!("3:::{wrong}")), Err(DecodeError::Data(3)), "{wrong}");
        }
    }
    for bad in [
        Value::Null,
        Value::Bool(false),
        serde_json::json!([]),
        serde_json::json!("text"),
    ] {
        let mut wrong = valid.clone();
        wrong["headers"] = bad;
        assert_eq!(decode(&format!("3:::{wrong}")), Err(DecodeError::Data(3)), "{wrong}");
    }
    for id in [i64::MIN, i64::MAX] {
        let mut data = valid.clone();
        data["id"] = Value::from(id);
        let Incoming::Delivery(delivery) = decode(&format!("3:::{data}")).unwrap() else {
            panic!("a delivery");
        };
        assert_eq!(delivery.id, id);
        assert!(delivery.body.is_empty());
        assert!(delivery.headers.is_empty());
    }
    let mut overflow = valid;
    overflow["id"] = Value::from(u64::MAX);
    assert_eq!(decode(&format!("3:::{overflow}")), Err(DecodeError::Data(3)));
}

#[test]
fn stored_delivery_headers_are_case_insensitive_and_only_boolean_true_marks_replay() {
    for value in ["not-json", "null", "{}", r#"{"mstore":false}"#, r#"{"mstore":"true"}"#] {
        let data = serde_json::json!({
            "id": 1, "method": "POST", "url": "/synthetic",
            "headers": {"TrOuTeR-ReQuEsT": value, "Numeric": 7}, "body": 7
        });
        let Incoming::Delivery(raw) = decode(&format!("3:::{data}")).unwrap() else {
            panic!("a delivery");
        };
        assert_eq!(raw.header("TROUTER-REQUEST"), Some(value));
        assert_eq!(raw.header("Numeric"), None);
        assert_eq!(raw.header("missing"), None);
        assert!(!raw.is_stored(), "{value}");
        assert!(raw.body.is_empty());
    }
    let data = serde_json::json!({
        "id": 1, "method": "POST", "url": "/synthetic",
        "headers": {"TROUTER-REQUEST": r#"{"mstore":true}"#}, "body": "synthetic:payload"
    });
    let Incoming::Delivery(raw) = decode(&format!("3:::{data}")).unwrap() else {
        panic!("a delivery");
    };
    assert!(raw.is_stored());
    let debug = format!("{raw:?}");
    assert!(debug.contains("body_len: 17"));
    assert!(!debug.contains("synthetic:payload"));
    assert!(!debug.contains("mstore"));
}

#[test]
fn event_ids_and_arguments_preserve_valid_values_and_reject_invalid_shapes() {
    for (id, number, acknowledged) in [
        ("", None, false),
        ("0007", Some(7), false),
        ("0+", Some(0), true),
        ("18446744073709551615+", Some(u64::MAX), true),
    ] {
        let read = event(&format!(
            r#"5:{id}::{{"name":"synthetic:event","args":[{{"colon":"a:b"}},7]}}"#
        ));
        assert_eq!(read.id, number);
        assert_eq!(read.acknowledged, acknowledged);
        assert_eq!(read.name, "synthetic:event");
        assert_eq!(read.args, vec![serde_json::json!({"colon":"a:b"}), Value::from(7)]);
    }
    for id in ["+", "-1", "1++", " 1", "１", "18446744073709551616"] {
        assert_eq!(
            decode(&format!(r#"5:{id}::{{"name":"synthetic"}}"#)),
            Err(DecodeError::Data(5)),
            "{id}"
        );
    }
    for data in ["", "null", "[]", "{}", r#"{"name":7}"#, r#"{"name":"x","args":false}"#] {
        assert_eq!(decode(&format!("5:::{data}")), Err(DecodeError::Data(5)), "{data}");
    }
    for data in [r#"{"name":"synthetic"}"#, r#"{"name":"synthetic","args":null}"#] {
        assert_eq!(event(&format!("5:::{data}")).args, Vec::<Value>::new());
    }
    assert_eq!(decode("5::"), Err(DecodeError::Data(5)));
}

#[test]
fn acknowledgements_accept_empty_or_json_payloads_but_require_an_unsigned_id() {
    for (data, id, args) in [
        ("0+", 0, Value::Null),
        ("0007+[]", 7, serde_json::json!([])),
        (
            "18446744073709551615+{\"value\":\"a:b+c\"}",
            u64::MAX,
            serde_json::json!({"value":"a:b+c"}),
        ),
    ] {
        assert_eq!(decode(&format!("6:::{data}")), Ok(Incoming::Ack { id, args }));
    }
    for data in [
        "",
        "7",
        "+[]",
        "-1+[]",
        "1.5+[]",
        "18446744073709551616+[]",
        "7+not-json",
    ] {
        assert_eq!(decode(&format!("6:::{data}")), Err(DecodeError::Data(6)), "{data}");
    }
    assert_eq!(decode("6::"), Err(DecodeError::Data(6)));
}

#[cfg(test)]
fn synthetic_event(name: &str, args: Vec<Value>) -> Event {
    Event {
        id: None,
        acknowledged: false,
        name: name.to_owned(),
        args,
    }
}

#[test]
fn connected_requires_its_identity_and_push_url_and_treats_optional_metadata_independently() {
    assert_eq!(Connected::from_event(&synthetic_event("other", vec![])), None);
    assert_eq!(
        Connected::from_event(&synthetic_event("trouter.connected", vec![])),
        None
    );
    for info in [
        Value::Null,
        serde_json::json!({"surl":"/synthetic"}),
        serde_json::json!({"id":7,"surl":"/synthetic"}),
        serde_json::json!({"id":"x","surl":false}),
    ] {
        assert_eq!(
            Connected::from_event(&synthetic_event("trouter.connected", vec![info])),
            None
        );
    }
    for (ttl, expected) in [
        (Value::Null, None),
        (Value::from(-1), None),
        (Value::from(1.5), None),
        (Value::from("-1"), None),
        (Value::from("18446744073709551616"), None),
        (Value::from("0007"), Some(7)),
        (Value::from(0), Some(0)),
        (Value::from(u64::MAX), Some(u64::MAX)),
    ] {
        let info = serde_json::json!({"id":"synthetic-id","surl":"/synthetic","ttl":ttl,"reconnectUrl":false});
        let connected = Connected::from_event(&synthetic_event("trouter.connected", vec![info])).unwrap();
        assert_eq!(connected.id, "synthetic-id");
        assert_eq!(connected.push_url, "/synthetic");
        assert_eq!(connected.ttl, expected);
        assert_eq!(connected.reconnect_url, None);
        assert_eq!(connected.params, None);
    }
}

#[test]
fn reconnect_parameters_are_retained_only_when_all_seven_strings_are_present() {
    let params =
        serde_json::json!({"issuer":"issuer","scae":"scae","sig":"sig","sr":"sr","sp":"sp","se":"se","st":"st"});
    let read = |params: Value| {
        Connected::from_event(&synthetic_event(
            "trouter.connected",
            vec![serde_json::json!({
                "id":"synthetic-id", "surl":"/synthetic", "connectparams":params
            })],
        ))
        .unwrap()
    };
    let complete = read(params.clone()).params.unwrap();
    assert_eq!(
        (complete.issuer.as_str(), complete.scae.as_str(), complete.sig.as_str()),
        ("issuer", "scae", "sig")
    );
    assert_eq!(
        (
            complete.sr.as_str(),
            complete.sp.as_str(),
            complete.se.as_str(),
            complete.st.as_str()
        ),
        ("sr", "sp", "se", "st")
    );
    for key in ["issuer", "scae", "sig", "sr", "sp", "se", "st"] {
        let mut missing = params.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert_eq!(read(missing).params, None, "missing {key}");
        let mut wrong = params.clone();
        wrong[key] = Value::from(7);
        assert_eq!(read(wrong).params, None, "non-string {key}");
    }
}

#[test]
fn message_loss_is_all_or_nothing_and_preserves_empty_and_ordered_reports() {
    let read = |args| message_loss(&synthetic_event("trouter.message_loss", args));
    assert_eq!(read(vec![]), None);
    for info in [
        Value::Null,
        serde_json::json!({}),
        serde_json::json!({"droppedIndicators":{}}),
    ] {
        assert_eq!(read(vec![info]), None);
    }
    assert_eq!(read(vec![serde_json::json!({"droppedIndicators":[]})]), Some(vec![]));
    let first = serde_json::json!({"tag":"first","etag":"one"});
    for invalid in [
        Value::Null,
        serde_json::json!({"tag":"x"}),
        serde_json::json!({"etag":"x"}),
        serde_json::json!({"tag":7,"etag":"x"}),
        serde_json::json!({"tag":"x","etag":7}),
    ] {
        assert_eq!(
            read(vec![serde_json::json!({"droppedIndicators":[first,invalid]})]),
            None
        );
    }
    assert_eq!(
        read(vec![
            serde_json::json!({"droppedIndicators":[first,{"tag":"second","etag":"two"}]})
        ]),
        Some(vec![
            DroppedIndicator {
                tag: "first".to_owned(),
                etag: "one".to_owned()
            },
            DroppedIndicator {
                tag: "second".to_owned(),
                etag: "two".to_owned()
            }
        ])
    );
}

#[test]
fn a_conversation_update_keeps_the_bookmark_beside_its_seen_horizon() {
    let event = ChatEvent::from_body(
        &json!({
            "resourceType": "ConversationUpdate",
            "resource": {"id": "48:notifications", "properties": {
                "consumptionhorizon": "1000;2000;1",
                "consumptionHorizonBookmark": "0;3000;0"
            }, "propertiesUpdated": ["consumptionhorizon", "consumptionHorizonBookmark"]}
        })
        .to_string(),
    );
    let ChatEvent::ConversationUpdate { conversation } = event else {
        panic!("both properties must survive");
    };
    assert_eq!(conversation.horizon_bookmark(), Some(3000));
    assert_eq!(
        conversation.consumption_horizon().unwrap().last_read_arrival,
        Some(1000)
    );
}
