//! A bot's pictures (the Bot Connector's attachments: `POST v3/conversations/{id}/attachments`, `GET
//! v3/attachments/{id}` and its `views/original`). Each is kept as a media object of the bot's own, made for the chat
//! it was put in (`crate::media`), so the chat's people read it as a pasted picture and nobody else does. A bot's
//! message shows a picture it names (by its connector URL, or a `data:` URL it carries) as Teams shows a pasted one.

use axum::Json;
use axum::body::Bytes;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use super::Bot;
use crate::app::App;
use crate::media::BotPicture;

/// The view a bot reads its picture by: the bytes as they were sent.
const ORIGINAL: &str = "original";
/// The longest file name kept.
const MAX_NAME: usize = 255;

/// Why a bot's picture wasn't kept or read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Refused {
    /// Not a picture the client shows, or too large.
    NotAPicture,
    /// The bot isn't in the chat.
    Denied,
    /// No such picture, or none the bot may read.
    Missing,
    /// The bot's storage is full.
    Quota,
    Failed,
}

/// `POST v3/conversations/{id}/attachments` (`AttachmentData`: `type`, `name`, `originalBase64`): the picture kept
/// for `chat`; its id.
pub(super) async fn upload(app: &App, bot: &Bot, chat: &str, body: &[u8]) -> Result<Response, Refused> {
    let data: Value = serde_json::from_slice(body).map_err(|_| Refused::NotAPicture)?;
    let bytes = data["originalBase64"]
        .as_str()
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .ok_or(Refused::NotAPicture)?;
    let name = data["name"].as_str().unwrap_or("image");
    let id = keep(app, bot, chat, name, bytes.into()).await?;
    Ok(Json(json!({ "id": id })).into_response())
}

async fn keep(app: &App, bot: &Bot, chat: &str, name: &str, bytes: Bytes) -> Result<String, Refused> {
    if crate::chatsvc::service::bot_roster(app, chat, &bot.id)
        .await
        .map_err(|_| Refused::Failed)?
        .is_none()
    {
        return Err(Refused::Denied);
    }
    let name = file_name(name);
    match crate::media::keep_bot_picture(app, &bot.id, chat, &name, bytes).await {
        BotPicture::Made(id) => Ok(id),
        BotPicture::NotAPicture => Err(Refused::NotAPicture),
        BotPicture::Quota => Err(Refused::Quota),
        BotPicture::Failed => Err(Refused::Failed),
    }
}

/// A name a bot gave, as a file name: no control characters or path, at most [`MAX_NAME`] bytes.
fn file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut kept: String = base.chars().filter(|c| !c.is_control()).collect();
    while kept.len() > MAX_NAME {
        kept.pop();
    }
    if kept.trim().is_empty() {
        "image".to_owned()
    } else {
        kept
    }
}

/// `GET v3/attachments/{id}` (`AttachmentInfo`) or `…/views/original` (the bytes).
pub(super) async fn read(app: &App, bot: &Bot, path: &[&str]) -> Result<Response, Refused> {
    let (id, view) = match path {
        [id] => (*id, None),
        [id, views, view] if views.eq_ignore_ascii_case("views") => (*id, Some(*view)),
        _ => return Err(Refused::Missing),
    };
    let picture = crate::media::bot_picture(app, &bot.id, id)
        .await
        .ok_or(Refused::Missing)?;
    Ok(match view {
        None => Json(json!({"name": picture.filename, "type": picture.content_type,
            "views": [{"viewId": ORIGINAL, "size": picture.bytes.len()}]}))
        .into_response(),
        Some(view) if view.eq_ignore_ascii_case(ORIGINAL) => {
            ([(CONTENT_TYPE, picture.content_type)], picture.bytes).into_response()
        }
        Some(_) => return Err(Refused::Missing),
    })
}

#[derive(Default)]
pub(super) struct Taken {
    pub html: Vec<String>,
    pub ids: Vec<String>,
    created: Vec<String>,
}

impl Taken {
    pub async fn release(&self, app: &App, bot: &Bot) {
        crate::media::release(app, &bot.id, &self.created).await;
    }
}

pub(super) fn extract(activity: &mut Value) -> Vec<Value> {
    let Some(attachments) = activity.get_mut("attachments").and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let (pictures, rest): (Vec<Value>, Vec<Value>) = attachments.drain(..).partition(is_picture);
    *attachments = rest;
    pictures
}

pub(super) async fn take(app: &App, bot: &Bot, chat: &str, pictures: &[Value]) -> Result<Taken, Refused> {
    let mut taken = Taken::default();
    for picture in pictures {
        if let Err(error) = take_one(app, bot, chat, picture, &mut taken).await {
            taken.release(app, bot).await;
            return Err(error);
        }
    }
    Ok(taken)
}

async fn take_one(app: &App, bot: &Bot, chat: &str, picture: &Value, taken: &mut Taken) -> Result<(), Refused> {
    let url = picture["contentUrl"].as_str().unwrap_or_default();
    let id = match url.strip_prefix("data:") {
        Some(data) => {
            let bytes = data_url(data).ok_or(Refused::NotAPicture)?;
            let name = picture["name"].as_str().unwrap_or("image");
            let id = keep(app, bot, chat, name, bytes.into()).await?;
            taken.created.push(id.clone());
            id
        }
        None => own(app, bot, chat, url).await?,
    };
    let size = crate::media::bot_picture(app, &bot.id, &id)
        .await
        .and_then(|picture| {
            image::ImageReader::new(std::io::Cursor::new(picture.bytes))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()
        })
        .ok_or(Refused::Missing)?;
    taken.html.push(html(&app.config.origin(), &id, size));
    taken.ids.push(id);
    Ok(())
}

fn is_picture(attachment: &Value) -> bool {
    attachment["contentType"]
        .as_str()
        .is_some_and(|kind| kind.to_ascii_lowercase().starts_with("image/"))
        && attachment["contentUrl"].is_string()
}

/// The bytes of a `data:` URL's base64 body (after `data:`).
fn data_url(data: &str) -> Option<Vec<u8>> {
    let (meta, body) = data.split_once(',')?;
    if !meta.to_ascii_lowercase().ends_with(";base64") {
        return None;
    }
    STANDARD.decode(body.trim()).ok()
}

/// The id of the bot's own picture a connector URL (`…/v3/attachments/<id>/views/original`) names.
async fn own(app: &App, bot: &Bot, chat: &str, url: &str) -> Result<String, Refused> {
    let path = url::Url::parse(url).map_err(|_| Refused::NotAPicture)?;
    let segments: Vec<&str> = path.path_segments().map_or_else(Vec::new, |segments| {
        segments.filter(|segment| !segment.is_empty()).collect()
    });
    let id = match segments.as_slice() {
        [.., v3, attachments, id, views, _] | [.., v3, attachments, id, views]
            if v3.eq_ignore_ascii_case("v3")
                && attachments.eq_ignore_ascii_case("attachments")
                && views.eq_ignore_ascii_case("views") =>
        {
            *id
        }
        _ => return Err(Refused::NotAPicture),
    };
    if !crate::media::bot_owns_picture(app, &bot.id, chat, id).await {
        return Err(Refused::Missing);
    }
    Ok(id.to_owned())
}

/// A pasted picture's HTML, as the client writes one (`teams_proto::chat::compose`).
fn html(origin: &str, id: &str, (width, height): (u32, u32)) -> String {
    let id = ammonia::clean_text(id);
    format!(
        "<p><img src=\"{origin}/ams/v1/objects/{id}/views/imgo\" itemtype=\"http://schema.skype.com/AMSImage\" \
         width=\"{width}\" height=\"{height}\" alt=\"image\" id=\"x_{id}\" itemid=\"{id}\"></p>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_data_urls_and_picture_attachments_are_read_strictly() {
        assert_eq!(file_name("../../etc/chart.png"), "chart.png");
        assert_eq!(file_name("a\u{0}b\\c.png"), "c.png");
        assert_eq!(file_name("  "), "image");
        assert_eq!(file_name(&"x".repeat(300)).len(), MAX_NAME);
        assert_eq!(data_url("image/png;base64,aGk="), Some(b"hi".to_vec()));
        assert_eq!(data_url("image/png,hi"), None);
        assert!(is_picture(&json!({"contentType": "IMAGE/png", "contentUrl": "x"})));
        assert!(!is_picture(&json!({"contentType": "image/png"})));
        assert!(!is_picture(
            &json!({"contentType": "application/pdf", "contentUrl": "x"})
        ));
        let shown = html("https://own.example.test", "0-own-d1-ab", (4, 3));
        assert!(shown.contains("src=\"https://own.example.test/ams/v1/objects/0-own-d1-ab/views/imgo\""));
        assert!(shown.contains("width=\"4\" height=\"3\""));
    }
}
