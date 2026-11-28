//! The text of a card, for a reader of the text alone (`ngteamsctl`, the search index): the lines a reader sees on the
//! card, in their order, from the model the views draw.
//!
//! - **Text** is a text block's markdown as plain text, a rich text block's runs, each fact as `title: value`; a date
//!   or a time function is written in UTC (`2026-11-04`, `18:15 UTC`), as the reader's time zone is not known here.
//! - **Containers, columns and tables** give their elements in order, a table row on one line with ` | ` between its
//!   cells. A hidden element is left out, as Teams does not show it.
//! - **Actions** give their titles; a link's action gives its link after its title, and `Action.ShowCard` also gives
//!   the text of the card it shows.
//! - **Pictures, media and inputs** give no text.
//! - **An Office 365 connector card** ([`CONNECTOR`], a webhook's `MessageCard`) is read here only, from the fields of
//!   its public schema: its title, its summary when it is not the title, its text, each section's activity title,
//!   subtitle and text, title, text and facts, and the names of its actions (an `OpenUri` with its link).
//!
//! A field the model does not read is not guessed at: it gives no text.

use serde_json::Value;
use time::OffsetDateTime;

use super::{
    Action, ActionKind, CARD_TYPE, Card, CardMessage, Element, Kind, TextPart, activity, array_of, attachment_card,
    is_link, markdown_html, message_cards, str_of, string_of, text_parts,
};
use crate::chat::parse_html;

/// An Office 365 connector card attachment's `contentType`.
pub const CONNECTOR: &str = "application/vnd.microsoft.teams.card.o365connector";

/// The text of a card message's `content`: each attachment's text in its order, an empty line between two. `None`
/// when it carries no card JSON; empty when its cards give no text.
#[must_use]
pub fn card_message_text(content: &str) -> Option<String> {
    let activity = activity(content)?;
    let attachments = activity.get("attachments")?.as_array()?;
    Some(joined(attachments.iter().map(attachment_text)))
}

/// The text of the cards a message carries: a card message's (`message_type` [`CARD_TYPE`], its `content`), else the
/// link's app cards of a text message (`properties`, their JSON text). `None` when it carries no card.
#[must_use]
pub fn message_cards_text(message_type: &str, content: &str, properties: Option<&str>) -> Option<String> {
    if message_type == CARD_TYPE {
        return card_message_text(content);
    }
    let properties = serde_json::from_str::<Value>(properties?).ok()?;
    message_cards(properties.get("cards")).map(|cards| cards_text(&cards))
}

/// The text of each card of `message`, an empty line between two.
#[must_use]
pub fn cards_text(message: &CardMessage) -> String {
    joined(message.cards.iter().map(card_text))
}

fn joined(texts: impl Iterator<Item = String>) -> String {
    texts.filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n\n")
}

fn attachment_text(attachment: &Value) -> String {
    match (str_of(attachment, "contentType"), attachment.get("content")) {
        (Some(CONNECTOR), Some(card)) => connector_text(card),
        _ => attachment_card(attachment, false)
            .map(|card| card_text(&card))
            .unwrap_or_default(),
    }
}

fn card_text(card: &Card) -> String {
    let mut lines = Lines::default();
    lines.elements(&card.body);
    lines.actions(&card.actions);
    lines.text()
}

fn connector_text(card: &Value) -> String {
    let mut lines = Lines::default();
    let title = string_of(card, "title");
    if let Some(title) = &title {
        lines.connector(title);
    }
    if let Some(summary) = string_of(card, "summary").filter(|summary| Some(summary) != title.as_ref()) {
        lines.connector(&summary);
    }
    if let Some(text) = string_of(card, "text") {
        lines.connector(&text);
    }
    for section in array_of(card, "sections") {
        for key in ["activityTitle", "activitySubtitle", "activityText", "title", "text"] {
            if let Some(text) = string_of(section, key) {
                lines.connector(&text);
            }
        }
        for fact in array_of(section, "facts") {
            lines.fact(
                &connector_plain(str_of(fact, "name").unwrap_or_default()),
                &connector_plain(str_of(fact, "value").unwrap_or_default()),
            );
        }
        lines.connector_actions(section);
    }
    lines.connector_actions(card);
    lines.text()
}

/// A card's lines, each one not empty.
#[derive(Default)]
struct Lines(Vec<String>);

impl Lines {
    fn text(self) -> String {
        self.0.join("\n")
    }

    fn push(&mut self, line: &str) {
        let line = line.trim();
        if !line.is_empty() {
            self.0.push(line.to_owned());
        }
    }

    fn markdown(&mut self, text: &str) {
        self.push(&plain(text));
    }

    fn connector(&mut self, text: &str) {
        // The HTML read joins whitespace, so each raw line is read alone to keep the markdown's lines.
        for raw in text.lines() {
            for line in connector_plain(raw).lines() {
                self.markdown(line);
            }
        }
    }

    fn fact(&mut self, title: &str, value: &str) {
        let (title, value) = (plain(title), plain(value));
        if title.is_empty() {
            self.push(&value);
        } else {
            self.push(&format!("{title}: {value}"));
        }
    }

    fn elements(&mut self, elements: &[Element]) {
        for element in elements.iter().filter(|element| element.visible) {
            self.element(&element.kind);
        }
    }

    fn element(&mut self, kind: &Kind) {
        match kind {
            Kind::Text(block) => self.markdown(&block.text),
            Kind::RichText(block) => {
                let runs: String = block.runs.iter().map(|run| expanded(&run.text)).collect();
                self.push(&runs);
            }
            Kind::Container(container) => self.elements(&container.items),
            Kind::Columns(set) => {
                for column in set.columns.iter().filter(|column| column.visible) {
                    self.elements(&column.items);
                }
            }
            Kind::Facts(facts) => {
                for (title, value) in facts {
                    self.fact(title, value);
                }
            }
            Kind::Actions(actions) => self.actions(actions),
            Kind::Table(table) => {
                for row in &table.rows {
                    let cells: Vec<String> = row
                        .iter()
                        .map(|cell| {
                            let mut lines = Self::default();
                            lines.elements(cell);
                            lines.0.join(" ")
                        })
                        .collect();
                    if cells.iter().any(|cell| !cell.is_empty()) {
                        self.push(&cells.join(" | "));
                    }
                }
            }
            Kind::Image(_) | Kind::ImageSet { .. } | Kind::Input(_) | Kind::Media(_) => {}
        }
    }

    fn actions(&mut self, actions: &[Action]) {
        for action in actions {
            match &action.kind {
                ActionKind::OpenUrl(url) | ActionKind::OpenTab(url) => self.link(&action.title, url),
                ActionKind::ShowCard(card) => {
                    self.push(&action.title);
                    self.elements(&card.body);
                    self.actions(&card.actions);
                }
                ActionKind::ToggleVisibility(_) | ActionKind::Submit(_) => self.push(&action.title),
            }
        }
    }

    fn link(&mut self, title: &str, url: &str) {
        if title.trim().is_empty() {
            self.push(url);
        } else {
            self.push(&format!("{} ({url})", title.trim()));
        }
    }

    /// The `potentialAction` of a connector card or of one of its sections.
    fn connector_actions(&mut self, item: &Value) {
        for action in array_of(item, "potentialAction") {
            let name = str_of(action, "name").unwrap_or_default();
            let link = (str_of(action, "@type") == Some("OpenUri"))
                .then(|| open_uri(action))
                .flatten();
            match link {
                Some(url) => self.link(name, url),
                None => self.push(name),
            }
        }
    }
}

/// An `OpenUri` action's link: its target for the `default` platform, else its first.
fn open_uri(action: &Value) -> Option<&str> {
    let targets = array_of(action, "targets");
    targets
        .iter()
        .find(|target| str_of(target, "os") == Some("default"))
        .or_else(|| targets.first())
        .and_then(|target| str_of(target, "uri"))
        .filter(|url| is_link(url))
}

/// A connector card's text, which can hold HTML (`<br/>`, `&#x274C;`) in its markdown, with that HTML read.
fn connector_plain(text: &str) -> String {
    if text.contains(['<', '&']) {
        parse_html(text).plain_text()
    } else {
        text.to_owned()
    }
}

/// A card text's markdown as plain text, its date and time functions written in UTC.
fn plain(text: &str) -> String {
    parse_html(&markdown_html(&expanded(text)))
        .plain_text()
        .trim()
        .to_owned()
}

fn expanded(text: &str) -> String {
    text_parts(text)
        .into_iter()
        .map(|part| match part {
            TextPart::Text(text) => text.to_owned(),
            TextPart::Date { at_ms, .. } => utc(at_ms).map_or_else(String::new, |at| {
                format!("{}-{:02}-{:02}", at.year(), u8::from(at.month()), at.day())
            }),
            TextPart::Time { at_ms } => {
                utc(at_ms).map_or_else(String::new, |at| format!("{:02}:{:02} UTC", at.hour(), at.minute()))
            }
        })
        .collect()
}

fn utc(at_ms: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(at_ms) * 1_000_000).ok()
}
