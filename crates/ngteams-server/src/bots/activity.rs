//! Activities both ways, as the `msteams` channel shapes them (`docs/server.md` "Bots"):
//!
//! - to a bot: a chat event as the activity a bot SDK reads (`message`, `messageUpdate`, `messageDelete`, `typing`),
//!   its ids the chat service's own, so a reply, an update or a delete names the message itself. The parts known only
//!   when it is sent (where the bot answers, the tenant) are added then ([`addressed`]);
//! - from a bot: an activity as the chat service messages it makes: Markdown (the default), plain or XML text as the
//!   client's HTML, cleaned; card attachments as the client's card message.

use serde_json::{Value, json};

use crate::workflows::hook::{CARD_KINDS, MAX_CARDS, activity_content, card};

/// The message types a bot's message is kept as.
pub(crate) const HTML: &str = "RichText/Html";
pub(crate) const CARD: &str = "RichText/Media_Card";

/// The locale activities name: the server has no account locale yet.
pub(super) const LOCALE: &str = "en-US";

/// The bot an activity goes to.
pub(crate) struct Recipient<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

/// The account an activity comes from.
pub(crate) struct Sender<'a> {
    pub id: &'a str,
    pub name: &'a str,
}

/// The conversation of an event.
pub(crate) struct Place<'a> {
    pub id: &'a str,
    /// A 1:1 chat (`personal`), or a group chat.
    pub personal: bool,
    /// In a team's channel: the channel and the post of the message's thread.
    pub channel: Option<&'a InChannel>,
}

/// A team's channel an activity is about, as the `msteams` channel names it.
#[derive(Clone, Debug)]
pub(crate) struct InChannel {
    pub channel: String,
    pub team: String,
    pub team_name: String,
    pub group: String,
    /// The post of the message's thread: its own id for a post.
    pub root: Option<i64>,
}

impl InChannel {
    /// The conversation's id: a message's is its post's reply chain, `<channel>;messageid=<post>`.
    fn conversation(&self) -> String {
        match self.root {
            Some(root) => format!("{};messageid={root}", self.channel),
            None => self.channel.clone(),
        }
    }

    fn mark(&self, activity: &mut Value) {
        let data = &mut activity["channelData"];
        data["teamsTeamId"] = self.team.clone().into();
        data["teamsChannelId"] = self.channel.clone().into();
        data["team"] = json!({"id": self.team, "name": self.team_name, "aadGroupId": self.group});
        data["channel"] = json!({ "id": self.channel });
    }
}

/// Where an app was added or taken off, for the events its bot gets.
pub(crate) enum Installed<'a> {
    /// For the person, in their 1:1 chat with the bot.
    Personal(&'a str),
    /// To a group chat.
    Group(&'a str),
    /// To a team: its general channel names it.
    Team(&'a InChannel),
}

impl Installed<'_> {
    fn envelope(&self, from: &Sender<'_>, to: &Recipient<'_>, at: i64) -> Value {
        let (kind, id) = match self {
            Self::Personal(chat) => ("personal", (*chat).to_owned()),
            Self::Group(chat) => ("groupChat", (*chat).to_owned()),
            Self::Team(channel) => ("channel", channel.conversation()),
        };
        let mut activity = envelope(kind, &id, from, to, at);
        if let Self::Team(channel) = self {
            channel.mark(&mut activity);
        }
        activity
    }
}

/// `installationUpdate` for the bot `to`: its app `added` (or taken off) by `from`.
pub(crate) fn installation(
    place: &Installed<'_>,
    from: &Sender<'_>,
    to: &Recipient<'_>,
    added: bool,
    at: i64,
) -> Value {
    let mut activity = place.envelope(from, to, at);
    activity["type"] = "installationUpdate".into();
    activity["id"] = format!("install-{}", uuid::Uuid::new_v4().simple()).into();
    activity["action"] = if added { "add" } else { "remove" }.into();
    activity
}

/// `conversationUpdate` for the bot `to`: `members` (MRIs: `28:<bot>`, `8:orgid:<id>`) `added` to the conversation
/// (or gone from it) by `from`.
pub(crate) fn members_changed(
    place: &Installed<'_>,
    from: &Sender<'_>,
    to: &Recipient<'_>,
    members: &[String],
    added: bool,
    at: i64,
) -> Value {
    let mut activity = place.envelope(from, to, at);
    activity["type"] = "conversationUpdate".into();
    activity["id"] = format!("members-{}", uuid::Uuid::new_v4().simple()).into();
    let listed: Vec<Value> = members.iter().map(|mri| account_of(mri)).collect();
    activity[if added { "membersAdded" } else { "membersRemoved" }] = Value::Array(listed);
    if matches!(place, Installed::Team(_)) {
        let kind = if added { "teamMemberAdded" } else { "teamMemberRemoved" };
        activity["channelData"]["eventType"] = kind.into();
    }
    activity
}

/// A member as the Bot Framework names it: a bot by its MRI, a person as `29:<id>` with their object id.
fn account_of(mri: &str) -> Value {
    match mri.strip_prefix("8:orgid:") {
        Some(id) => json!({"id": format!("29:{id}"), "aadObjectId": id}),
        None => json!({ "id": mri }),
    }
}

/// What happened, with the message's body as the chat service keeps it.
pub(crate) enum Event<'a> {
    Message {
        id: i64,
        body: &'a Value,
    },
    Edited {
        id: i64,
        body: &'a Value,
    },
    Deleted {
        id: i64,
    },
    Typing {
        at: i64,
    },
    /// A reaction `key` put on (`added`) or taken off the message `id` of `owner`.
    Reaction {
        id: i64,
        owner: &'a str,
        key: &'a str,
        added: bool,
    },
}

impl Event<'_> {
    /// The message it is about; none for typing.
    pub(crate) const fn message(&self) -> Option<i64> {
        match self {
            Self::Message { id, .. } | Self::Edited { id, .. } | Self::Deleted { id } | Self::Reaction { id, .. } => {
                Some(*id)
            }
            Self::Typing { .. } => None,
        }
    }

    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Self::Message { .. } => "message",
            Self::Edited { .. } => "messageUpdate",
            Self::Deleted { .. } => "messageDelete",
            Self::Typing { .. } => "typing",
            Self::Reaction { .. } => "messageReaction",
        }
    }
}

/// The parts every activity has: when, from whom, to which bot and in which conversation (`kind`: `personal`,
/// `groupChat` or `channel`).
pub(crate) fn envelope(kind: &str, conversation: &str, from: &Sender<'_>, to: &Recipient<'_>, at: i64) -> Value {
    let time = crate::chatsvc::iso(at);
    json!({
        "timestamp": time, "localTimestamp": time, "channelId": crate::apps::keys::CHANNEL, "locale": LOCALE,
        "from": {"id": format!("29:{}", from.id), "name": from.name, "aadObjectId": from.id},
        "recipient": {"id": format!("28:{}", to.id), "name": to.name},
        "conversation": {"id": conversation, "conversationType": kind, "isGroup": kind != "personal"},
        "channelData": {},
    })
}

/// The `entities` of an activity a person's client started.
pub(crate) fn client_info() -> Value {
    json!([{"type": "clientInfo", "locale": LOCALE}])
}

/// The activity for `event`, without the parts [`addressed`] adds.
pub(crate) fn pending(event: &Event<'_>, place: &Place<'_>, from: &Sender<'_>, to: &Recipient<'_>, at: i64) -> Value {
    let kind = match (place.personal, place.channel) {
        (_, Some(_)) => "channel",
        (true, None) => "personal",
        (false, None) => "groupChat",
    };
    let conversation = place
        .channel
        .map_or_else(|| place.id.to_owned(), InChannel::conversation);
    let mut activity = envelope(kind, &conversation, from, to, at);
    if let Some(channel) = place.channel {
        channel.mark(&mut activity);
    }
    activity["type"] = event.kind().into();
    match event {
        Event::Message { id, body } | Event::Edited { id, body } => {
            activity["id"] = id.to_string().into();
            let html = body["content"].as_str().unwrap_or_default();
            activity["textFormat"] = "plain".into();
            activity["attachments"] = json!([{"contentType": "text/html", "content": html}]);
            if place.personal {
                activity["text"] = plain(html).into();
                activity["entities"] = client_info();
            } else {
                activity["text"] = crate::outgoing::mention_text(html).into();
                activity["entities"] = mention_entities(body);
            }
            if matches!(event, Event::Edited { .. }) {
                activity["channelData"]["eventType"] = "editMessage".into();
            }
        }
        Event::Deleted { id } => {
            activity["id"] = id.to_string().into();
            activity["channelData"]["eventType"] = "softDeleteMessage".into();
        }
        Event::Typing { at } => {
            activity["id"] = format!("typing-{at}").into();
        }
        Event::Reaction { id, key, added, .. } => {
            activity["id"] = format!("reaction-{id}-{at}").into();
            activity["replyToId"] = id.to_string().into();
            let listed = if *added { "reactionsAdded" } else { "reactionsRemoved" };
            activity[listed] = json!([{ "type": key }]);
        }
    }
    activity
}

/// The `entities` of a message in a group chat or channel: the client's, and each mention as the Bot Framework names
/// it (`mentioned` the account, `text` its `<at>` span in the activity's text).
fn mention_entities(body: &Value) -> Value {
    let listed: Vec<Value> = match &body["properties"]["mentions"] {
        Value::String(text) => serde_json::from_str(text).unwrap_or_default(),
        Value::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    let mut entities = vec![json!({"type": "clientInfo", "locale": LOCALE})];
    for mention in &listed {
        let (Some(mri), Some(name)) = (mention["mri"].as_str(), mention["displayName"].as_str()) else {
            continue;
        };
        let mut mentioned = account_of(mri);
        mentioned["name"] = name.into();
        entities.push(json!({"type": "mention", "mentioned": mentioned, "text": format!("<at>{name}</at>")}));
    }
    Value::Array(entities)
}

/// `activity` as it is sent: where the bot answers and the tenant.
pub(crate) fn addressed(activity: &mut Value, service_url: &str, tenant: &str) {
    activity["serviceUrl"] = service_url.into();
    activity["conversation"]["tenantId"] = tenant.into();
    activity["channelData"]["tenant"] = json!({ "id": tenant });
}

/// The text of the client's HTML, as Teams gives a bot a message's `text`.
pub(crate) fn plain(html: &str) -> String {
    let fragment = scraper::Html::parse_fragment(html);
    let text: String = fragment.root_element().text().collect();
    text.trim().to_owned()
}

/// Why a bot's activity makes no message.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refused {
    /// Neither text nor an attachment the client draws.
    Empty,
    /// An attachment of a kind the server doesn't carry, or a card the client can't read.
    Attachment,
}

/// One chat-service message a bot's activity makes: its type and content.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Made {
    pub kind: &'static str,
    pub content: String,
}

/// The messages `activity` makes: its text, then one card message. If `has_pictures` is true, the caller adds
/// the pictures to the HTML message, which can have no text.
pub(crate) fn messages(activity: &Value, origin: &str, has_pictures: bool) -> Result<Vec<Made>, Refused> {
    let mut made = Vec::new();
    let text = activity["text"].as_str().filter(|text| !text.trim().is_empty());
    if text.is_some() || has_pictures {
        let format = activity["textFormat"].as_str().unwrap_or("markdown");
        let content = text.map(|text| html(text, format)).unwrap_or_default();
        made.push(Made { kind: HTML, content });
    }
    let attachments = activity["attachments"].as_array().map_or(&[][..], Vec::as_slice);
    if !attachments.is_empty() {
        if attachments.len() > MAX_CARDS {
            return Err(Refused::Attachment);
        }
        let cards = attachments
            .iter()
            .map(|attachment| {
                let kind = attachment["contentType"].as_str().ok_or(Refused::Attachment)?;
                let content = &attachment["content"];
                if CARD_KINDS.contains(&kind) && card(kind, content) {
                    Ok(json!({"contentType": kind, "content": content}))
                } else {
                    Err(Refused::Attachment)
                }
            })
            .collect::<Result<Vec<Value>, Refused>>()?;
        let mut carried = json!({"type": "message/card", "attachments": cards});
        if let Some(suggested) = activity.get("suggestedActions").filter(|value| value.is_object()) {
            carried["suggestedActions"] = suggested.clone();
        }
        made.push(Made {
            kind: CARD,
            content: activity_content(&carried, origin),
        });
    }
    if made.is_empty() {
        return Err(Refused::Empty);
    }
    Ok(made)
}

/// A bot's text as the client's HTML: Markdown, plain text or XML (HTML), cleaned to what a message may carry.
pub(crate) fn html(text: &str, format: &str) -> String {
    let raw = match format {
        "plain" => {
            let escaped = ammonia::clean_text(text);
            format!("<p>{}</p>", escaped.replace("&#10;", "<br>"))
        }
        "xml" => text.to_owned(),
        _ => {
            let mut options = pulldown_cmark::Options::empty();
            options.insert(pulldown_cmark::Options::ENABLE_STRIKETHROUGH);
            options.insert(pulldown_cmark::Options::ENABLE_TABLES);
            let parser = pulldown_cmark::Parser::new_ext(text, options);
            let mut html = String::new();
            pulldown_cmark::html::push_html(&mut html, parser);
            html
        }
    };
    clean(&raw)
}

/// HTML a bot sent, without scripts, styles, event handlers, pictures from elsewhere or anything but plain links.
fn clean(html: &str) -> String {
    ammonia::Builder::default()
        .rm_tags(["img"])
        .url_schemes(["https", "http", "mailto"].into())
        .clean(html)
        .to_string()
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_plain_and_xml_text_become_clean_html() {
        let markdown = html(
            "**bold** _it_ ~~gone~~ [a](https://a.example.test) <script>x()</script>",
            "markdown",
        );
        assert!(markdown.contains("<strong>bold</strong>"), "{markdown}");
        assert!(markdown.contains("<em>it</em>") && markdown.contains("<del>gone</del>"));
        assert!(markdown.contains("href=\"https://a.example.test\""));
        assert!(!markdown.contains("script"));
        assert_eq!(html("a < b\nc", "plain"), "<p>a &lt; b<br>c</p>");
        let xml = html(
            "<b onclick=\"x()\">hi</b><img src=\"https://t.example.test/p.png\"><a href=\"javascript:x()\">j</a>",
            "xml",
        );
        assert_eq!(xml, "<b>hi</b><a rel=\"noopener noreferrer\">j</a>");
    }

    #[test]
    fn an_activity_makes_its_text_then_one_card_message_and_refuses_what_the_client_cant_draw() {
        let card = json!({"type": "AdaptiveCard", "body": [{"type": "TextBlock", "text": "x"}]});
        let both = json!({"text": "see below", "attachments": [
            {"contentType": "application/vnd.microsoft.card.adaptive", "content": card},
            {"contentType": "application/vnd.microsoft.card.hero", "content": {"title": "h"}}],
            "suggestedActions": {"actions": [{"type": "imBack", "title": "Yes", "value": "yes"}]}});
        let made = messages(&both, "https://own.example.test", false).unwrap();
        assert_eq!(made.len(), 2);
        assert_eq!(made[0].kind, HTML);
        assert_eq!(made[1].kind, CARD);
        let parsed = teams_card(&made[1].content);
        assert_eq!(parsed["attachments"].as_array().unwrap().len(), 2);
        assert_eq!(parsed["suggestedActions"]["actions"][0]["value"], "yes");
        assert_eq!(messages(&json!({"text": "  "}), "o", false), Err(Refused::Empty));
        let file = json!({"attachments": [{"contentType": "image/png", "contentUrl": "https://x.example.test/a.png"}]});
        assert_eq!(messages(&file, "o", false), Err(Refused::Attachment));
        let broken = json!({"attachments": [{"contentType": "application/vnd.microsoft.card.adaptive",
            "content": {"type": "AdaptiveCard", "body": [{"text": "no type"}]}}]});
        assert_eq!(messages(&broken, "o", false), Err(Refused::Attachment));
    }

    fn teams_card(content: &str) -> Value {
        use base64::Engine as _;
        let start = content.find("b64=\"").unwrap() + 5;
        let end = start + content[start..].find('"').unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&content[start..end])
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn a_users_message_reaches_the_bot_as_plain_text_with_its_html_beside_it() {
        let body = json!({"content": "<p>Hello <b>bot</b> &amp; co</p>"});
        let activity = pending(
            &Event::Message { id: 42, body: &body },
            &Place {
                id: "19:a_b@unq.gbl.spaces",
                personal: true,
                channel: None,
            },
            &Sender { id: "u", name: "Ada" },
            &Recipient { id: "b", name: "Echo" },
            1_700_000_000_000,
        );
        assert_eq!(activity["text"], "Hello bot & co");
        assert_eq!(activity["id"], "42");
        assert_eq!(activity["attachments"][0]["contentType"], "text/html");
        let mut sent = activity;
        addressed(&mut sent, "https://own.example.test/bot/", "t");
        assert_eq!(sent["channelData"]["tenant"]["id"], "t");
        assert_eq!(sent["conversation"]["tenantId"], "t");
    }
}
