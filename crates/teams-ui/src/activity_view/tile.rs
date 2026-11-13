//! An app item's tile: the picture its app's definition names (`largeImageUrl`), drawn as Teams draws an app in the
//! pane, a rounded square at the avatar's size on a bordered surface, with no badge. The definition and the picture
//! are read once per app by the engine, which caches both; until the picture is drawable, or when it can't be had
//! (no definition, no URL, a read that failed, a URL off the CDN's app folders, bytes that don't decode), the item
//! keeps the initials and badge every item has, and nothing is said.

use std::sync::Arc;

use gpui::{
    AsyncApp, Context, InteractiveElement as _, IntoElement, ParentElement as _, RenderImage, Styled as _, WeakEntity,
    div, img, px, rgb,
};
use teams_core::activity::ActivityItem;

use super::{AVATAR, ActivityView};
use crate::inline::decode_within;
use crate::theme::Palette;

/// The longest side a tile is decoded at: thrice the avatar, for scaled screens.
const DECODED_SIDE: u32 = 96;

impl ActivityView {
    /// Asks for the tile of each app the items name, once each.
    pub(super) fn ask_app_tiles(&mut self, cx: &mut Context<Self>) {
        let asked: Vec<String> = self
            .feed
            .items
            .iter()
            .filter_map(|item| item.teams_app_id.clone())
            .collect();
        for app in asked {
            if self.app_tiles.contains_key(&app) {
                continue;
            }
            self.app_tiles.insert(app.clone(), None);
            let definition = self.core.app_definition(app.clone());
            let core = Arc::clone(&self.core);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Some(url) = definition
                    .await
                    .ok()
                    .flatten()
                    .and_then(|definition| definition.large_image_url().map(str::to_owned))
                else {
                    return;
                };
                let Ok(Some(bytes)) = core.app_picture(url).await else {
                    return;
                };
                let decoded = cx
                    .background_executor()
                    .spawn(async move { decode_within(&bytes, DECODED_SIDE) })
                    .await;
                let Some((image, _)) = decoded else { return };
                let _ = this.update(cx, |view, cx| {
                    view.app_tiles.insert(app, Some(Arc::new(image)));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// Whether the item is drawn with its app's tile (else the initials and the badge).
    #[must_use]
    pub fn shows_app_tile(&self, item: &ActivityItem) -> bool {
        self.tile_image(item).is_some()
    }

    fn tile_image(&self, item: &ActivityItem) -> Option<&Arc<RenderImage>> {
        self.app_tiles.get(item.teams_app_id.as_ref()?)?.as_ref()
    }

    /// The item's app tile, when its picture is drawable.
    pub(super) fn app_tile(&self, item: &ActivityItem, palette: Palette) -> Option<impl IntoElement> {
        let image = self.tile_image(item)?;
        Some(tile(&item.id, Arc::clone(image), palette))
    }
}

/// `image` in Teams' app tile: a rounded square the avatar's size, on the surface with a hairline border, the picture
/// filling it (an app's picture carries its own margin).
fn tile(id: &str, image: Arc<RenderImage>, palette: Palette) -> impl IntoElement {
    let id = id.to_owned();
    div()
        .debug_selector(move || format!("activity-app-tile-{id}"))
        .flex_none()
        .size(px(AVATAR))
        .rounded_md()
        .overflow_hidden()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .child(img(image).size_full().rounded_md())
}
