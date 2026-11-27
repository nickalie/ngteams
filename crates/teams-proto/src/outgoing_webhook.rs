//! A team's outgoing webhooks on the middle tier, `beta/teams/<team>/outgoingWebhooks` (`docs/protocol/bots.md`,
//! "Outgoing webhooks"): the list, a webhook's picture, and the create answer that shows the security token once.
//! The writes are [`crate::TeamWrite`]'s `CreateOutgoingWebhook`, `ChangeOutgoingWebhook` and
//! `RemoveOutgoingWebhook`; their body is a [`WebhookForm`].
//!
//! The routes are the own server's design: no capture of Teams' own form exists, because a webhook made in the lab
//! tenant sends lab channel content to an outside address. A capture changes the client and the server together.

use base64::Engine as _;
use serde_json::{Map, Value, json};

use crate::endpoint::{Endpoint, Service};
use crate::read::Read;
use crate::{AppDefinition, InstalledApp, StoreApp};

/// A team's outgoing webhook as the list shows it. `Debug` leaves out its address.
#[derive(Clone, PartialEq, Eq)]
pub struct OutgoingWebhook {
    pub id: String,
    /// `28:<id>`: who its replies come from and who a mention of it names.
    pub mri: String,
    /// The team's thread.
    pub team: String,
    pub name: String,
    pub description: String,
    /// Whether the account may change or remove it.
    pub can_manage: bool,
    /// Who made it.
    pub created_by: String,
    /// Its address, only for those who may change it.
    pub callback_url: Option<String>,
    /// Where its picture is read, when it has one.
    pub picture_url: Option<String>,
}

impl std::fmt::Debug for OutgoingWebhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutgoingWebhook")
            .field("id", &self.id)
            .field("can_manage", &self.can_manage)
            .finish_non_exhaustive()
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

impl OutgoingWebhook {
    fn of(value: &Value) -> Option<Self> {
        let id = text(value, "id").filter(|id| !id.is_empty())?;
        let name = text(value, "name").filter(|name| !name.is_empty())?;
        Some(Self {
            mri: text(value, "mri").unwrap_or_else(|| format!("28:{id}")),
            team: text(value, "teamId").unwrap_or_default(),
            description: text(value, "description").unwrap_or_default(),
            can_manage: value.get("canManage").and_then(Value::as_bool).unwrap_or(false),
            created_by: text(value, "createdBy").unwrap_or_default(),
            callback_url: text(value, "callbackUrl"),
            picture_url: text(value, "pictureUrl"),
            id,
            name,
        })
    }

    /// The webhooks the list's answer `body` gives, in its order; an entry with no id or no name is left out.
    #[must_use]
    pub fn list_answer(body: &[u8]) -> Vec<Self> {
        serde_json::from_slice::<Value>(body)
            .ok()
            .as_ref()
            .and_then(|answer| answer.get("value"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Self::of)
            .collect()
    }

    /// It as an app whose bot (`28:<id>`) the team's people mention in its channels, as Teams lists it with the team's
    /// apps; its definition names the webhook ([`AppDefinition::outgoing_webhook`]) for its picture.
    #[must_use]
    pub fn app(&self) -> StoreApp {
        let mut definition = Map::new();
        definition.insert("id".to_owned(), json!(self.id));
        definition.insert("name".to_owned(), json!(self.name));
        definition.insert("shortDescription".to_owned(), json!(self.description));
        definition.insert("developerName".to_owned(), json!(OUTGOING_WEBHOOK_MAKER));
        definition.insert("bots".to_owned(), json!([{"id": self.id, "scopes": ["team"]}]));
        definition.insert(
            WEBHOOK_KEY.to_owned(),
            json!({"team": self.team, "picture": self.picture_version()}),
        );
        StoreApp {
            app: InstalledApp {
                id: self.id.clone(),
                name: self.name.clone(),
                developer: OUTGOING_WEBHOOK_MAKER.to_owned(),
                description: self.description.clone(),
                accent: None,
            },
            definition: AppDefinition::made(definition),
            has_bot: true,
        }
    }

    /// The version its picture's address names (`v`), which changes with the picture.
    #[must_use]
    pub fn picture_version(&self) -> Option<&str> {
        let url = self.picture_url.as_deref()?;
        let (_, query) = url.split_once('?')?;
        query.split('&').find_map(|pair| pair.strip_prefix("v="))
    }
}

/// Who the apps list names as an outgoing webhook's maker.
pub const OUTGOING_WEBHOOK_MAKER: &str = "Outgoing webhook";

/// The key of [`OutgoingWebhook::app`]'s definition that names the webhook: not a catalogue field.
const WEBHOOK_KEY: &str = "ngteamsOutgoingWebhook";

impl AppDefinition {
    /// The picture read of the outgoing webhook this definition stands for ([`OutgoingWebhook::app`]); `None` for a
    /// catalogue app.
    #[must_use]
    pub fn outgoing_webhook(&self) -> Option<OutgoingWebhookPicture> {
        let webhook = self.field(WEBHOOK_KEY)?;
        Some(OutgoingWebhookPicture {
            team: text(webhook, "team")?,
            webhook: self.id().to_owned(),
            version: text(webhook, "picture"),
        })
    }
}

/// A webhook's security token: its HMAC key in base64, which the create answer alone shows. `Debug` leaves it out.
#[derive(Clone, PartialEq, Eq)]
pub struct SecurityToken(String);

impl SecurityToken {
    /// The token's text, for the dialog that shows it and its Copy.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecurityToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecurityToken(..)")
    }
}

/// A new webhook and its token, as the create answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWebhook {
    pub webhook: OutgoingWebhook,
    pub token: SecurityToken,
}

impl CreatedWebhook {
    /// The webhook and its token in the create's answer `body`; `None` without either.
    #[must_use]
    pub fn read_answer(body: &[u8]) -> Option<Self> {
        let answer = serde_json::from_slice::<Value>(body).ok()?;
        let value = answer.get("value")?;
        let token = text(value, "securityToken").filter(|token| !token.is_empty())?;
        Some(Self {
            webhook: OutgoingWebhook::of(value)?,
            token: SecurityToken(token),
        })
    }
}

/// What a create or a change does with the picture.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum WebhookPicture {
    #[default]
    Keep,
    Remove,
    /// A PNG or a JPEG.
    Set(Vec<u8>),
}

/// The fields a create or a change sends; a field left `None` is not sent (a change keeps it). `Debug` leaves out the
/// address and the picture.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WebhookForm {
    pub name: Option<String>,
    pub callback_url: Option<String>,
    pub description: Option<String>,
    pub picture: WebhookPicture,
}

impl std::fmt::Debug for WebhookForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let picture = match self.picture {
            WebhookPicture::Keep => "keep",
            WebhookPicture::Remove => "remove",
            WebhookPicture::Set(_) => "set",
        };
        f.debug_struct("WebhookForm")
            .field("name", &self.name)
            .field("picture", &picture)
            .finish_non_exhaustive()
    }
}

impl WebhookForm {
    /// The body of a create or a change: the picture in base64, `null` to remove it.
    #[must_use]
    pub fn body(&self) -> Value {
        let mut body = Map::new();
        for (key, field) in [
            ("name", &self.name),
            ("callbackUrl", &self.callback_url),
            ("description", &self.description),
        ] {
            if let Some(field) = field {
                body.insert(key.to_owned(), json!(field));
            }
        }
        match &self.picture {
            WebhookPicture::Keep => {}
            WebhookPicture::Remove => {
                body.insert("picture".to_owned(), Value::Null);
            }
            WebhookPicture::Set(bytes) => {
                body.insert(
                    "picture".to_owned(),
                    json!(base64::engine::general_purpose::STANDARD.encode(bytes)),
                );
            }
        }
        Value::Object(body)
    }
}

/// A team's webhooks: `GET beta/teams/<team>/outgoingWebhooks`, answered as [`OutgoingWebhook::list_answer`] reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingWebhooks {
    pub team: String,
}

impl Read for OutgoingWebhooks {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        Endpoint::new(["beta", "teams", self.team.as_str(), "outgoingWebhooks"])
    }
}

/// A webhook's picture: `GET …/outgoingWebhooks/<id>/picture`, with the version its list entry named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutgoingWebhookPicture {
    pub team: String,
    pub webhook: String,
    pub version: Option<String>,
}

impl Read for OutgoingWebhookPicture {
    fn service(&self) -> Service {
        Service::MiddleTier
    }

    fn endpoint(&self) -> Endpoint {
        let endpoint = Endpoint::new([
            "beta",
            "teams",
            self.team.as_str(),
            "outgoingWebhooks",
            self.webhook.as_str(),
            "picture",
        ]);
        match &self.version {
            Some(version) => endpoint.with_query("v", version),
            None => endpoint,
        }
    }
}
