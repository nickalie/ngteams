//! The activity a webhook gets, in the shape of Microsoft's documentation and samples (`docs/protocol/bots.md`
//! "Outgoing webhooks"), its HMAC, and what of an answer the server keeps.

use base64::Engine as _;
use serde_json::{Value, json};

/// The locale activities name: the server has no account locale yet.
const LOCALE: &str = "en-US";

/// The webhook ids a message's body mentions (`28:<id>`), once each.
pub(crate) fn mentioned(body: &Value) -> Vec<String> {
    let mut ids: Vec<String> = crate::chatsvc::mentions(body)
        .iter()
        .filter_map(|entry| entry["mri"].as_str()?.strip_prefix("28:"))
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// A channel message that mentions a webhook.
pub(crate) struct Mention<'a> {
    pub id: i64,
    pub channel: &'a str,
    /// The post of its thread: itself for a post.
    pub root: i64,
    pub team: &'a str,
    pub body: &'a Value,
    pub at: i64,
}

/// Who wrote it.
pub(crate) struct Author<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

/// The activity for `mention`, without the parts [`addressed`] adds when it is sent.
pub(crate) fn pending(mention: &Mention<'_>, author: &Author<'_>) -> Value {
    let time = crate::chatsvc::iso(mention.at);
    let html = mention.body["content"].as_str().unwrap_or_default();
    json!({
        "type": "message",
        "id": mention.id.to_string(),
        "timestamp": time, "localTimestamp": time,
        "channelId": crate::apps::keys::CHANNEL,
        "from": {"id": format!("29:{}", author.id), "name": author.name, "aadObjectId": author.id},
        "conversation": {"isGroup": true, "conversationType": "channel",
            "id": format!("{};messageid={}", mention.channel, mention.root), "name": null},
        "recipient": null,
        "textFormat": "plain",
        "locale": LOCALE,
        "text": text(html),
        "attachments": [{"contentType": "text/html", "content": html}],
        "entities": [{"type": "clientInfo", "locale": LOCALE}],
        "channelData": {
            "teamsChannelId": mention.channel, "teamsTeamId": mention.team,
            "channel": {"id": mention.channel}, "team": {"id": mention.team},
        },
    })
}

/// `activity` as it is sent: the service URL Teams names (a webhook can't call it) and the tenant.
pub(crate) fn addressed(activity: &mut Value, service_url: &str, tenant: &str) {
    activity["serviceUrl"] = service_url.into();
    activity["conversation"]["tenantId"] = tenant.into();
    activity["channelData"]["tenant"] = json!({ "id": tenant });
}

/// The text of the client's HTML as Teams gives a webhook a message's `text`: each mention as `<at>name</at>`, one
/// line for each paragraph.
pub(crate) fn text(html: &str) -> String {
    let fragment = scraper::Html::parse_fragment(html);
    let mut out = String::new();
    walk(fragment.root_element(), &mut out);
    out.trim().to_owned()
}

fn walk(element: scraper::ElementRef<'_>, out: &mut String) {
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
            continue;
        }
        let Some(inner) = scraper::ElementRef::wrap(child) else {
            continue;
        };
        let tag = inner.value();
        if tag
            .attr("itemtype")
            .is_some_and(|kind| kind.ends_with("schema.skype.com/Mention"))
        {
            out.push_str("<at>");
            out.extend(inner.text());
            out.push_str("</at>");
            continue;
        }
        if matches!(tag.name(), "p" | "div" | "br") && !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        walk(inner, out);
    }
}

/// The `Authorization` value for `body`: `HMAC` and the base64 HMAC-SHA256 of its exact bytes under `key`.
pub(crate) fn sign(key: &[u8], body: &[u8]) -> String {
    let key = aws_lc_rs::hmac::Key::new(aws_lc_rs::hmac::HMAC_SHA256, key);
    let tag = aws_lc_rs::hmac::sign(&key, body);
    format!(
        "HMAC {}",
        base64::engine::general_purpose::STANDARD.encode(tag.as_ref())
    )
}

/// An answer as the server keeps it: a webhook's cards act only by opening a link (Teams' rule), so every other
/// action and the suggested actions go.
pub(crate) fn kept(mut answer: Value) -> Value {
    if let Some(map) = answer.as_object_mut() {
        map.remove("suggestedActions");
    }
    if let Some(attachments) = answer.get_mut("attachments") {
        keep_open_url(attachments);
    }
    answer
}

fn keep_open_url(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in ["actions", "buttons"] {
                if let Some(Value::Array(items)) = map.get_mut(key) {
                    items.retain(opens_url);
                }
            }
            for key in ["selectAction", "tap"] {
                if map.get(key).is_some_and(|action| !opens_url(action)) {
                    map.remove(key);
                }
            }
            if map.get("type").and_then(Value::as_str).is_some_and(open_url_kind)
                && map
                    .get("fallback")
                    .is_some_and(|fallback| fallback.is_object() && !opens_url(fallback))
            {
                map.insert("fallback".to_owned(), "drop".into());
            }
            map.values_mut().for_each(keep_open_url);
        }
        Value::Array(items) => items.iter_mut().for_each(keep_open_url),
        _ => {}
    }
}

fn opens_url(action: &Value) -> bool {
    action["type"].as_str().is_some_and(open_url_kind)
}

fn open_url_kind(kind: &str) -> bool {
    kind.eq_ignore_ascii_case("Action.OpenUrl") || kind.eq_ignore_ascii_case("openUrl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_of_apps_are_found_in_either_form_once_each() {
        let text = json!({"properties": {"mentions":
            "[{\"mri\":\"28:a\",\"itemid\":\"0\"},{\"mri\":\"8:orgid:u\"},{\"mri\":\"28:a\"},{\"mri\":\"28:\"}]"}});
        assert_eq!(mentioned(&text), ["a"]);
        let listed = json!({"properties": {"mentions": [{"mri": "28:b"}, {"mri": "28:a"}]}});
        assert_eq!(mentioned(&listed), ["a", "b"]);
        assert!(mentioned(&json!({"properties": {"mentions": "not json"}})).is_empty());
        assert!(mentioned(&json!({})).is_empty());
    }

    #[test]
    fn a_messages_text_names_each_mention_with_at_and_keeps_its_paragraphs() {
        let html = "<p><span itemtype=\"http://schema.skype.com/Mention\" itemscope itemid=\"0\">Echo</span> \
                    status &amp; more</p><p>second <b>line</b></p>";
        assert_eq!(text(html), "<at>Echo</at> status & more\nsecond line");
        assert_eq!(text("plain"), "plain");
    }

    #[test]
    fn the_hmac_is_the_one_microsofts_sample_checks() {
        // RFC 4231 test case 2: the key "Jefe" and its message
        let signed = sign(b"Jefe", b"what do ya want for nothing?");
        let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        let bytes: Vec<u8> = (0..expected.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&expected[at..at + 2], 16).unwrap())
            .collect();
        assert_eq!(
            signed,
            format!("HMAC {}", base64::engine::general_purpose::STANDARD.encode(bytes))
        );
    }

    #[test]
    fn an_answer_keeps_only_open_url_actions() {
        let answer = json!({"type": "message", "suggestedActions": {"actions": []}, "attachments": [
            {"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "t",
                "buttons": [{"type": "imBack", "value": "x"}, {"type": "openUrl", "value": "https://a.example.test"}],
                "tap": {"type": "imBack", "value": "x"}}},
            {"contentType": "application/vnd.microsoft.card.adaptive", "content": {"type": "AdaptiveCard",
                "selectAction": {"type": "Action.Submit"},
                "body": [{"type": "Container", "selectAction": {"type": "Action.OpenUrl", "url": "https://b.example.test"}}],
                "actions": [{"type": "Action.ShowCard", "card": {}}, {"type": "Action.OpenUrl", "url": "https://c.example.test"}]}}]});
        let kept = kept(answer);
        assert!(kept.get("suggestedActions").is_none());
        let hero = &kept["attachments"][0]["content"];
        assert_eq!(
            hero["buttons"],
            json!([{"type": "openUrl", "value": "https://a.example.test"}])
        );
        assert!(hero.get("tap").is_none());
        let card = &kept["attachments"][1]["content"];
        assert!(card.get("selectAction").is_none());
        assert_eq!(card["body"][0]["selectAction"]["url"], "https://b.example.test");
        assert_eq!(
            card["actions"],
            json!([{"type": "Action.OpenUrl", "url": "https://c.example.test"}])
        );
    }

    #[test]
    fn open_url_fallbacks_never_become_submit_actions() {
        let unsafe_action = json!({"type": "Action.OpenUrl", "url": "x", "fallback": {
            "type": "Action.OpenUrl", "url": "y", "fallback": {
                "type": "Action.Submit", "data": {"msteams": {"type": "imBack", "value": "send"}}
            }
        }});
        let mut safe_action = unsafe_action.clone();
        safe_action["fallback"]["fallback"] = "drop".into();
        let answer = kept(json!({"attachments": [{"content": {
            "actions": [unsafe_action.clone()], "buttons": [unsafe_action.clone()],
            "selectAction": unsafe_action.clone(), "tap": unsafe_action
        }}]}));
        let card = &answer["attachments"][0]["content"];
        assert_eq!(card["actions"][0], safe_action);
        assert_eq!(card["buttons"][0], safe_action);
        assert_eq!(card["selectAction"], safe_action);
        assert_eq!(card["tap"], safe_action);
    }

    #[test]
    fn the_activity_names_the_thread_its_team_and_its_author() {
        let body = json!({"content": "<p>hi</p>"});
        let mention = Mention {
            id: 7,
            channel: "19:c@thread.tacv2",
            root: 5,
            team: "19:t@thread.tacv2",
            body: &body,
            at: 1_700_000_000_000,
        };
        let mut activity = pending(&mention, &Author { id: "u", name: "Ada" });
        assert_eq!(activity["conversation"]["id"], "19:c@thread.tacv2;messageid=5");
        assert_eq!(activity["id"], "7");
        assert_eq!(activity["text"], "hi");
        addressed(&mut activity, "https://own.example.test/bot/", "t");
        assert_eq!(activity["channelData"]["tenant"]["id"], "t");
        assert_eq!(activity["conversation"]["tenantId"], "t");
        assert_eq!(activity["serviceUrl"], "https://own.example.test/bot/");
    }
}
