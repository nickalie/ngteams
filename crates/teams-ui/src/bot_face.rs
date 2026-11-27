//! A bot as the views show it, as Teams does: by its app's name, with the app's icon on a rounded tile rather than a
//! person's circle (a chat list's row, a bot chat's header and its "new conversation" intro, its messages, the
//! mention picker). The engine finds the app by the bot's id ([`Core::bot_app`]); a bot no app names keeps the name
//! its messages give and Teams' bot head. Nothing is asked while the account's cloud serves no apps
//! ([`teams_core::Capabilities::apps`]).

use gpui::{Div, ParentElement as _, Styled as _, div, img, px, rgb};
use teams_proto::StoreApp;

use crate::bridge::Core;
use crate::icons::Glyph;
use crate::people::{self, Picture};
use crate::theme::Palette;

/// A bot's app, and its icon once read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BotApp {
    pub app: StoreApp,
    pub picture: Option<Picture>,
}

impl BotApp {
    /// The app's name, as Teams names the bot.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.app.app.name
    }
}

/// Whether `mri` is a bot's or a service's (`28:`).
#[must_use]
pub fn is_bot(mri: &str) -> bool {
    mri.starts_with("28:")
}

/// The app of `bot` (`28:<id>`) with its icon, as the engine finds it; `None` when no app names it or the account's
/// cloud serves neither apps nor outgoing webhooks. Without apps, only a webhook the engine read names it.
pub async fn bot_app(core: &dyn Core, bot: &str) -> Option<BotApp> {
    let served = core.capabilities();
    if !(served.apps || served.outgoing_webhooks) || !is_bot(bot) {
        return None;
    }
    let app = if served.apps {
        core.bot_app(bot.to_owned())
    } else {
        core.known_bot_app(bot.to_owned())
    };
    let app = app.await.ok().flatten()?;
    Some(with_picture(core, app).await)
}

/// [`bot_app`] for the other one a 1:1 chat's id names, which may be a person: only an app the engine already knows
/// of, or the account's, names it.
pub async fn known_bot_app(core: &dyn Core, id: &str) -> Option<BotApp> {
    if !core.capabilities().apps {
        return None;
    }
    let app = core.known_bot_app(id.to_owned()).await.ok().flatten()?;
    Some(with_picture(core, app).await)
}

/// `app` with its icon, read by the engine (once a run); an outgoing webhook's picture from its team's middle tier.
pub async fn with_picture(core: &dyn Core, app: StoreApp) -> BotApp {
    if let Some(read) = app.definition.outgoing_webhook() {
        let picture = match read.version {
            Some(_) => core
                .outgoing_webhook_picture(read)
                .await
                .ok()
                .flatten()
                .and_then(|bytes| people::sniffed_image(&bytes)),
            None => None,
        };
        return BotApp { app, picture };
    }
    let picture = match app.definition.large_image_url() {
        Some(url) => core
            .app_picture(url.to_owned())
            .await
            .ok()
            .flatten()
            .and_then(|bytes| people::app_image(&bytes)),
        None => None,
    };
    BotApp { app, picture }
}

/// A bot's avatar, `side` across: its app's icon on a tile with a border; the app's initials on its colour while
/// there is no icon; Teams' bot head on a quiet tile for a bot no app names.
#[must_use]
pub fn tile(app: Option<&BotApp>, side: f32, palette: Palette) -> Div {
    let face = div()
        .size(px(side))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .overflow_hidden();
    match app {
        Some(BotApp {
            picture: Some(picture), ..
        }) => face
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(img(std::sync::Arc::clone(picture.image())).size(px(side * 0.8))),
        Some(BotApp { app, picture: None }) => face
            .bg(rgb(app.app.accent.unwrap_or(palette.brand)))
            .text_color(rgb(0x00ff_ffff))
            .text_size(px((side * 0.36).max(9.0)))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(people::team_initials(&app.app.name)),
        None => face
            .bg(rgb(palette.hover))
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::BOT.icon().size(px(side * 0.5))),
    }
}
