//! The rail's Store, as browser Teams has it (`docs/protocol/apps.md`):
//!
//! - **Store**: "Do more in Teams" with "Search agents and apps", then "Added by your org" (the organization's apps)
//!   and "Popular on Teams", each app a tile (its picture, name, maker, a line about it, its rating; an app the
//!   account added carries a check). Each section's "See all" opens all of its apps. A search lists the store's apps whose name, maker or line holds the words, as
//!   "Search results for …". A tile opens the app's details ([`details::AppDetailsDialog`]).
//! - **Manage your apps** (the header's gear): the apps the account added, Find by name, and a row per app that opens
//!   to where it is added ("Personal app") with Remove, which asks first, in Teams' words.
//!
//! Every read goes through [`Core`]; adding and removing are [`teams_proto::TeamWrite`]s, sent only from a confirming
//! button, and offered nowhere in the read-only mode.

pub mod chat_apps;
pub mod details;
pub mod place;

use std::collections::BTreeMap;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, RenderImage, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, img, px, rgb,
};
use gpui_component::Disableable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use teams_core::AppListing;
use teams_proto::{Rating, StoreApp, TeamWrite};

use crate::bridge::Core;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::decode_app_tile;
use crate::people::team_initials;
use crate::theme::{self, Palette};

/// The longest side a tile's picture is decoded at: thrice the largest tile, for scaled screens.
const DECODED_SIDE: u32 = 156;
/// A store tile's width, px.
const TILE_WIDTH: f32 = 280.0;
/// A store tile's picture, px.
const TILE_PICTURE: f32 = 52.0;
/// How many apps a store section shows.
const SECTION_MOST: usize = 24;

/// Which page of the store shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppsView {
    #[default]
    Store,
    Manage,
}

/// A section of the store's front page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreSection {
    /// "Added by your org".
    Org,
    /// "Popular on Teams".
    Popular,
}

impl StoreSection {
    fn title(self) -> &'static str {
        match self {
            Self::Org => "Added by your org",
            Self::Popular => "Popular on Teams",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Org => "org",
            Self::Popular => "popular",
        }
    }

    fn ids(self, listing: &AppListing) -> &[String] {
        match self {
            Self::Org => &listing.sections.org,
            Self::Popular => &listing.sections.popular,
        }
    }
}

/// What the page asks of the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppsPageEvent {
    /// A tile or a row was opened: its app's details.
    Details(Box<StoreApp>),
}

/// The store and "Manage your apps" (see the module docs).
pub struct AppsPage {
    core: Arc<dyn Core>,
    view: AppsView,
    /// The section whose "See all" shows all of its apps.
    see_all: Option<StoreSection>,
    search: Entity<InputState>,
    find: Entity<InputState>,
    listing: Option<AppListing>,
    /// The apps the account added, as last read.
    mine: Option<Vec<StoreApp>>,
    /// The row of "Manage your apps" opened to its places.
    expanded: Option<String>,
    /// The app whose Remove asks first.
    removing: Option<StoreApp>,
    remove_failed: bool,
    /// The Remove sent and not answered yet: kept apart from the reads, which a refresh replaces.
    removing_now: Option<Task<()>>,
    pictures: BTreeMap<String, Option<Arc<RenderImage>>>,
    reading: Vec<Task<()>>,
    focus: FocusHandle,
    _typed: [Subscription; 2],
}

impl EventEmitter<AppsPageEvent> for AppsPage {}

impl AppsPage {
    /// The store, its apps and the account's being read.
    pub fn new(core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search agents and apps"));
        let find = cx.new(|cx| InputState::new(window, cx).placeholder("Find by name"));
        let typed = [
            cx.subscribe(&search, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&find, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let mut page = Self {
            core,
            view: AppsView::Store,
            see_all: None,
            search,
            find,
            listing: None,
            mine: None,
            expanded: None,
            removing: None,
            remove_failed: false,
            removing_now: None,
            pictures: BTreeMap::new(),
            reading: Vec::new(),
            focus: cx.focus_handle(),
            _typed: typed,
        };
        page.refresh(cx);
        page
    }

    /// Reads the store and the account's apps again.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let listing = self.core.app_listing();
        let mine = self.core.user_apps();
        self.reading = vec![cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let listing = listing.await.unwrap_or_default();
            let mine = mine.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.listing = Some(listing);
                this.mine = Some(mine);
                this.ask_pictures(cx);
                cx.notify();
            });
        })];
    }

    /// The page shown.
    #[must_use]
    pub fn view(&self) -> AppsView {
        self.view
    }

    /// Shows the store, or "Manage your apps".
    pub fn show(&mut self, view: AppsView, window: &mut Window, cx: &mut Context<Self>) {
        self.view = view;
        self.see_all = None;
        self.removing = None;
        match view {
            AppsView::Store => self.search.update(cx, |search, cx| search.focus(window, cx)),
            AppsView::Manage => self.find.update(cx, |find, cx| find.focus(window, cx)),
        }
        cx.notify();
    }

    /// A section's "See all" (`Some`), or back to the store's front page (`None`).
    pub fn see_all(&mut self, section: Option<StoreSection>, cx: &mut Context<Self>) {
        self.see_all = section;
        cx.notify();
    }

    /// The section whose apps all show.
    #[must_use]
    pub fn seeing_all(&self) -> Option<StoreSection> {
        self.see_all
    }

    /// The search's words set to `words`, as typed.
    pub fn set_search(&mut self, words: &str, window: &mut Window, cx: &mut Context<Self>) {
        let words = words.to_owned();
        self.search.update(cx, |search, cx| search.set_value(words, window, cx));
        cx.notify();
    }

    /// The store as read, once it was.
    #[must_use]
    pub fn listing(&self) -> Option<&AppListing> {
        self.listing.as_ref()
    }

    /// Whether the account added the app with `id`.
    #[must_use]
    pub fn added(&self, id: &str) -> bool {
        self.mine.iter().flatten().any(|store| store.app.id == id)
    }

    /// The store's rating of the app with `id`.
    #[must_use]
    pub fn rating(&self, id: &str) -> Option<Rating> {
        self.listing.as_ref()?.sections.ratings.get(id).copied()
    }

    /// The search's words, trimmed.
    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    /// The apps of a section, in the store's order: all of them, or as many as its row on the front page shows.
    fn section(&self, section: StoreSection, all: bool) -> Vec<StoreApp> {
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        let most = if all { usize::MAX } else { SECTION_MOST };
        section
            .ids(listing)
            .iter()
            .filter_map(|id| listing.app(id).cloned())
            .take(most)
            .collect()
    }

    /// The store's apps whose name, maker or line holds the search's words, in the store's order.
    #[must_use]
    pub fn found(&self, cx: &App) -> Vec<StoreApp> {
        let query = self.query(cx);
        let Some(listing) = &self.listing else {
            return Vec::new();
        };
        listing
            .sections
            .ids()
            .iter()
            .filter_map(|id| listing.app(id))
            .filter(|store| {
                [&store.app.name, &store.app.developer, &store.app.description]
                    .iter()
                    .any(|text| text.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    /// The account's apps "Manage your apps" lists: those whose name holds Find by name's words.
    #[must_use]
    pub fn mine_shown(&self, cx: &App) -> Vec<StoreApp> {
        let query = self.find.read(cx).value().trim().to_lowercase();
        self.mine
            .iter()
            .flatten()
            .filter(|store| query.is_empty() || store.app.name.to_lowercase().contains(&query))
            .cloned()
            .collect()
    }

    /// Asks once for the picture of each app the page may draw.
    fn ask_pictures(&mut self, cx: &mut Context<Self>) {
        let apps: Vec<(String, String)> = self
            .listing
            .iter()
            .flat_map(|listing| listing.apps.iter())
            .chain(self.mine.iter().flatten())
            .filter_map(|store| {
                let url = store.definition.large_image_url()?;
                Some((store.app.id.clone(), url.to_owned()))
            })
            .collect();
        for (id, url) in apps {
            if self.pictures.contains_key(&id) {
                continue;
            }
            self.pictures.insert(id.clone(), None);
            let reply = self.core.app_picture(url);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let Ok(Some(bytes)) = reply.await else { return };
                let decoded = cx
                    .background_executor()
                    .spawn(async move { decode_app_tile(&bytes, DECODED_SIDE) })
                    .await;
                let Some((image, _)) = decoded else { return };
                let _ = this.update(cx, |this, cx| {
                    this.pictures.insert(id, Some(Arc::new(image)));
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// The picture of the app with `id`, once drawable.
    #[must_use]
    pub fn picture_of(&self, id: &str) -> Option<Arc<RenderImage>> {
        self.pictures.get(id).cloned().flatten()
    }

    /// Whether the app with `id` is drawn with its picture (else its initials on its colour).
    #[must_use]
    pub fn shows_picture(&self, id: &str) -> bool {
        self.pictures.get(id).is_some_and(Option::is_some)
    }

    /// Opens an app's details.
    pub fn open_details(&mut self, store: StoreApp, cx: &mut Context<Self>) {
        cx.emit(AppsPageEvent::Details(Box::new(store)));
    }

    /// Opens or closes a row of "Manage your apps".
    pub fn toggle_row(&mut self, id: &str, cx: &mut Context<Self>) {
        self.expanded = if self.expanded.as_deref() == Some(id) {
            None
        } else {
            Some(id.to_owned())
        };
        cx.notify();
    }

    /// A row's Remove: asks first.
    pub fn ask_remove(&mut self, store: StoreApp, cx: &mut Context<Self>) {
        self.removing = Some(store);
        self.remove_failed = false;
        cx.notify();
    }

    /// The question's Cancel.
    pub fn keep(&mut self, cx: &mut Context<Self>) {
        self.removing = None;
        self.remove_failed = false;
        cx.notify();
    }

    /// The question's Remove: the app off the account, gone from the list once the service took it.
    pub fn remove(&mut self, cx: &mut Context<Self>) {
        if self.removing_now.is_some() {
            return;
        }
        let Some(store) = self.removing.clone() else {
            return;
        };
        let id = store.app.id.clone();
        let reply = self
            .core
            .team_write(TeamWrite::RemovePersonalApp { app: id.clone() }, None);
        cx.notify();
        self.removing_now = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |this, cx| this.removed(&id, answer.is_ok(), cx));
        }));
    }

    /// The reply to the Remove of the app with `id`: gone from the list when `taken`. The question may now be about a
    /// different app, or closed: only its own reply changes it.
    fn removed(&mut self, id: &str, taken: bool, cx: &mut Context<Self>) {
        self.removing_now = None;
        let asked = self.removing.as_ref().is_some_and(|open| open.app.id == id);
        if taken {
            self.mine
                .iter_mut()
                .for_each(|mine| mine.retain(|listed| listed.app.id != id));
        }
        if asked {
            self.remove_failed = !taken;
            if taken {
                self.removing = None;
            }
        }
        cx.notify();
    }

    /// Whether Remove's question shows, and whether its write failed.
    #[must_use]
    pub fn removing(&self) -> Option<(&StoreApp, bool)> {
        self.removing.as_ref().map(|store| (store, self.remove_failed))
    }

    fn header(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let header = div()
            .h(px(48.0))
            .flex_none()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(rgb(palette.border))
            .debug_selector(|| "apps-header".to_owned());
        match self.view {
            AppsView::Store => header
                .child(div().text_lg().font_weight(gpui::FontWeight::BOLD).child("Store"))
                .child(div().flex_1())
                .child(selected_by(
                    "apps-manage",
                    icon_button("apps-manage", Glyph::SETTINGS, "Manage your apps")
                        .on_click(cx.listener(|this, _, window, cx| this.show(AppsView::Manage, window, cx))),
                )),
            AppsView::Manage => header
                .child(selected_by(
                    "apps-back",
                    icon_button("apps-back", Glyph::ARROW_LEFT, "Back to the store")
                        .on_click(cx.listener(|this, _, window, cx| this.show(AppsView::Store, window, cx))),
                ))
                .child(
                    div()
                        .size(px(28.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .bg(rgb(palette.brand))
                        .text_color(rgb(palette.on_brand))
                        .child(Glyph::SETTINGS.icon().size(px(16.0))),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child("Manage your apps"),
                ),
        }
    }

    fn hero(&self, palette: Palette) -> gpui::Div {
        div()
            .w_full()
            .py_10()
            .px_4()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .bg(rgb(palette.selected))
            .child(
                div()
                    .text_size(px(34.0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Do more in Teams"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child("Discover agents and apps that can help you tackle any task."),
            )
            .child(div().pt_4().w(px(560.0)).max_w_full().child(selected_by(
                "apps-search",
                Input::new(&self.search).prefix(Glyph::SEARCH.icon().size(px(14.0))),
            )))
    }

    fn picture(&self, store: &StoreApp, side: f32, palette: Palette) -> gpui::Div {
        app_picture(
            store,
            self.pictures.get(&store.app.id).cloned().flatten(),
            side,
            palette,
        )
        .when(self.added(&store.app.id), |face| face.child(added_check(palette)))
    }

    fn tile(&self, at: &str, store: &StoreApp, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let selector = format!("apps-tile-{at}");
        let opened = store.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .w(px(TILE_WIDTH))
            .p_3()
            .flex()
            .gap_3()
            .rounded_lg()
            .cursor_pointer()
            .hover(move |tile| tile.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_details(opened.clone(), cx)))
            .child(self.picture(store, TILE_PICTURE, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(store.app.developer.clone()),
                    )
                    .child(
                        div()
                            .pt_1()
                            .text_sm()
                            .max_h(px(40.0))
                            .overflow_hidden()
                            .child(store.app.description.clone()),
                    )
                    .child(rating_line(self.rating(&store.app.id), palette)),
            )
            .into_any_element()
    }

    fn org_tile(&self, at: usize, store: &StoreApp, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let selector = format!("apps-org-{at}");
        let opened = store.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .w(px(220.0))
            .p_3()
            .flex()
            .gap_3()
            .rounded_lg()
            .cursor_pointer()
            .hover(move |tile| tile.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.open_details(opened.clone(), cx)))
            .child(self.picture(store, TILE_PICTURE, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .max_h(px(40.0))
                            .overflow_hidden()
                            .child(store.app.description.clone()),
                    ),
            )
            .into_any_element()
    }

    fn section_view(
        title: impl Into<SharedString>,
        caption: Option<&'static str>,
        selector: &'static str,
        tiles: Vec<AnyElement>,
        palette: Palette,
    ) -> gpui::Div {
        Self::section_with(
            div().text_xl().font_weight(gpui::FontWeight::BOLD).child(title.into()),
            caption,
            selector,
            tiles,
            palette,
        )
    }

    /// A section of the front page: its title with "See all" on the right, as Teams has it, over its row of tiles.
    fn front_section(
        section: StoreSection,
        caption: Option<&'static str>,
        tiles: Vec<AnyElement>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let key = section.key();
        let see_all = selected_by(
            format!("apps-{key}-see-all"),
            Button::new(SharedString::from(format!("apps-{key}-see-all")))
                .ghost()
                .label("See all")
                .text_color(rgb(palette.brand))
                .on_click(cx.listener(move |this, _, _, cx| this.see_all(Some(section), cx))),
        );
        let title = div()
            .flex()
            .items_center()
            .child(
                div()
                    .flex_1()
                    .text_xl()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(section.title()),
            )
            .child(see_all);
        let selector = match section {
            StoreSection::Org => "apps-org",
            StoreSection::Popular => "apps-popular",
        };
        Self::section_with(title, caption, selector, tiles, palette)
    }

    /// All the apps of `section`, under its title and a way back to the store's front page.
    fn all_of(
        &self,
        body: gpui::Stateful<gpui::Div>,
        section: StoreSection,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tiles: Vec<AnyElement> = self
            .section(section, true)
            .iter()
            .enumerate()
            .map(|(at, store)| self.tile(&format!("all-{at}"), store, palette, cx))
            .collect();
        let title = div()
            .flex()
            .items_center()
            .gap_2()
            .child(selected_by(
                "apps-all-back",
                icon_button("apps-all-back", Glyph::ARROW_LEFT, "Back to the store")
                    .on_click(cx.listener(|this, _, _, cx| this.see_all(None, cx))),
            ))
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child(format!(
                "{} ({})",
                section.title(),
                tiles.len()
            )));
        body.child(Self::section_with(title, None, "apps-all", tiles, palette))
            .into_any_element()
    }

    fn section_with(
        title: gpui::Div,
        caption: Option<&'static str>,
        selector: &'static str,
        tiles: Vec<AnyElement>,
        palette: Palette,
    ) -> gpui::Div {
        div()
            .debug_selector(move || selector.to_owned())
            .w_full()
            .max_w(px(1200.0))
            .px_8()
            .pt_6()
            .flex()
            .flex_col()
            .gap_1()
            .child(title)
            .children(caption.map(|caption| div().text_sm().text_color(rgb(palette.muted_foreground)).child(caption)))
            .child(div().pt_2().flex().flex_wrap().gap_2().children(tiles))
    }

    /// The search's results under the hero.
    fn results(&self, mut body: gpui::Stateful<gpui::Div>, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let found = self.found(cx);
        let count = found.len();
        let tiles: Vec<AnyElement> = found
            .iter()
            .enumerate()
            .map(|(at, store)| self.tile(&format!("found-{at}"), store, palette, cx))
            .collect();
        let words = self.search.read(cx).value().trim().to_owned();
        body = body.child(
            div()
                .w_full()
                .max_w(px(1200.0))
                .px_8()
                .pt_6()
                .debug_selector(|| "apps-results".to_owned())
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Search results for \"{words}\"")),
                ),
        );
        body = if count == 0 {
            body.child(
                div()
                    .debug_selector(|| "apps-none".to_owned())
                    .pt_8()
                    .text_color(rgb(palette.muted_foreground))
                    .child("No apps match your search."),
            )
        } else {
            body.child(Self::section_view(
                format!("Agents and apps ({count})"),
                None,
                "apps-found",
                tiles,
                palette,
            ))
        };
        body.into_any_element()
    }

    fn store_body(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let query = self.query(cx);
        let body = div()
            // See all is a page of its own: it opens at its top, and the front page keeps its place for Back
            .id(if self.see_all.is_some() {
                "apps-store-all"
            } else {
                "apps-store"
            })
            .debug_selector(|| "apps-store".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .items_center()
            .pb_8()
            .child(self.hero(palette));
        if self.listing.is_none() {
            return body
                .child(crate::team_dialogs::waiting("apps-loading", "Loading apps…", palette))
                .into_any_element();
        }
        if !query.is_empty() {
            return self.results(body, palette, cx);
        }
        if let Some(section) = self.see_all {
            return self.all_of(body, section, palette, cx);
        }
        let org: Vec<AnyElement> = self
            .section(StoreSection::Org, false)
            .iter()
            .enumerate()
            .map(|(at, store)| self.org_tile(at, store, palette, cx))
            .collect();
        let popular: Vec<AnyElement> = self
            .section(StoreSection::Popular, false)
            .iter()
            .enumerate()
            .map(|(at, store)| self.tile(&format!("popular-{at}"), store, palette, cx))
            .collect();
        if org.is_empty() && popular.is_empty() {
            return body
                .child(
                    div()
                        .debug_selector(|| "apps-empty".to_owned())
                        .pt_8()
                        .text_color(rgb(palette.muted_foreground))
                        .child("No apps are available right now."),
                )
                .into_any_element();
        }
        let org = (!org.is_empty()).then(|| {
            Self::front_section(
                StoreSection::Org,
                Some("Made available by your organization's technology team"),
                org,
                palette,
                cx,
            )
        });
        let popular =
            (!popular.is_empty()).then(|| Self::front_section(StoreSection::Popular, None, popular, palette, cx));
        body.children(org).children(popular).into_any_element()
    }

    fn remove_question(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (store, failed) = self.removing()?;
        let name = store.app.name.clone();
        Some(
            div()
                .debug_selector(|| "apps-remove-question".to_owned())
                .mx_6()
                .my_2()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_md()
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(format!("Remove {name}")),
                )
                .when(failed, |question| {
                    question.child(crate::team_dialogs::problem(
                        "apps-remove-problem",
                        format!("There was an issue while trying to remove {name}. Please try again."),
                    ))
                })
                .child(format!("Are you sure you want to remove {name}?"))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(selected_by(
                            "apps-remove-cancel",
                            Button::new("apps-remove-cancel")
                                .outline()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| this.keep(cx))),
                        ))
                        .child(selected_by(
                            "apps-remove-confirm",
                            Button::new("apps-remove-confirm")
                                .primary()
                                .label("Remove")
                                .disabled(self.removing_now.is_some())
                                .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                        )),
                ),
        )
    }

    /// A row's chevron, which opens or closes it to where its app is added.
    fn expand_button(at: usize, id: String, open: bool, cx: &mut Context<Self>) -> gpui::Div {
        let (glyph, tip) = if open {
            (Glyph::CHEVRON_DOWN, "Hide where it's added")
        } else {
            (Glyph::CHEVRON_RIGHT, "Show where it's added")
        };
        selected_by(
            format!("apps-mine-{at}-expand"),
            icon_button(SharedString::from(format!("apps-mine-{at}-expand")), glyph, tip)
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_row(&id, cx))),
        )
    }

    fn manage_row(&self, at: usize, store: &StoreApp, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let id = store.app.id.clone();
        let open = self.expanded.as_deref() == Some(id.as_str());
        let removed = store.clone();
        let detailed = store.clone();
        let read_only = self.core.read_only();
        let row = div()
            .debug_selector(move || format!("apps-mine-{at}"))
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .child(Self::expand_button(at, id, open, cx))
            .child(self.picture(store, 36.0, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::BOLD)
                            .truncate()
                            .child(store.app.name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(rgb(palette.muted_foreground))
                            .truncate()
                            .child(store.app.developer.clone()),
                    ),
            )
            .child(selected_by(
                format!("apps-mine-{at}-details"),
                icon_button(
                    SharedString::from(format!("apps-mine-{at}-details")),
                    Glyph::INFO,
                    "View details",
                )
                .on_click(cx.listener(move |this, _, _, cx| this.open_details(detailed.clone(), cx))),
            ));
        let places = open.then(|| {
            div()
                .debug_selector(move || format!("apps-mine-{at}-places"))
                .pl(px(72.0))
                .pr_4()
                .pb_3()
                .flex()
                .items_center()
                .gap_2()
                .child(app_picture(store, self.picture_of(&store.app.id), 18.0, palette))
                .child(div().flex_1().text_sm().child("Personal app"))
                .when(!read_only, |line| {
                    line.child(selected_by(
                        format!("apps-mine-{at}-remove"),
                        icon_button(
                            SharedString::from(format!("apps-mine-{at}-remove")),
                            Glyph::TRASH,
                            "Remove",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.ask_remove(removed.clone(), cx))),
                    ))
                })
        });
        div()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(row)
            .children(places)
    }

    fn manage_body(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let rows: Vec<gpui::Div> = self
            .mine_shown(cx)
            .iter()
            .enumerate()
            .map(|(at, store)| self.manage_row(at, store, palette, cx))
            .collect();
        let toolbar = div()
            .px_6()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .border_b_1()
            .border_color(rgb(palette.border))
            .child(selected_by(
                "apps-get-more",
                Button::new("apps-get-more")
                    .outline()
                    .icon(Glyph::PLUS.icon())
                    .label("Get more apps")
                    .on_click(cx.listener(|this, _, window, cx| this.show(AppsView::Store, window, cx))),
            ))
            .child(div().flex_1())
            .child(div().w(px(240.0)).child(selected_by(
                "apps-find",
                Input::new(&self.find).suffix(Glyph::SEARCH.icon().size(px(14.0))),
            )));
        let list: AnyElement = match &self.mine {
            None => crate::team_dialogs::waiting("apps-mine-loading", "Loading…", palette).into_any_element(),
            Some(mine) if mine.is_empty() => note("apps-mine-none", "You haven't added any apps yet.", palette),
            Some(_) if rows.is_empty() => note("apps-mine-none", "No apps match your search.", palette),
            Some(_) => div().flex().flex_col().px_2().children(rows).into_any_element(),
        };
        div()
            .id("apps-mine")
            .debug_selector(|| "apps-mine".to_owned())
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .child(toolbar)
            .children(self.remove_question(palette, cx))
            .child(list)
            .into_any_element()
    }
}

impl Render for AppsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let body = match self.view {
            AppsView::Store => self.store_body(palette, cx),
            AppsView::Manage => self.manage_body(palette, cx),
        };
        div()
            .debug_selector(|| "apps-page".to_owned())
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .text_color(rgb(palette.foreground))
            .child(self.header(palette, cx))
            .child(body)
    }
}

fn note(selector: &'static str, words: &'static str, palette: Palette) -> AnyElement {
    div()
        .debug_selector(move || selector.to_owned())
        .py_8()
        .text_center()
        .text_color(rgb(palette.muted_foreground))
        .child(words)
        .into_any_element()
}

/// An app's picture at `side`: its tile once drawable, else its initials on its colour, a rounded square either way.
pub(crate) fn app_picture(
    store: &StoreApp,
    picture: Option<Arc<RenderImage>>,
    side: f32,
    palette: Palette,
) -> gpui::Div {
    let face = div().relative().size(px(side)).flex_none().rounded_md();
    match picture {
        Some(picture) => face
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .child(img(picture).size_full().rounded_md()),
        None => face
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(store.app.accent.unwrap_or(palette.brand)))
            .text_color(rgb(0x00ff_ffff))
            .text_size(px((side * 0.36).max(9.0)))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(team_initials(&store.app.name)),
    }
}

/// The check an added app's picture carries at its bottom right, as Teams draws it.
fn added_check(palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "apps-added-check".to_owned())
        .absolute()
        .right(px(-4.0))
        .bottom(px(-4.0))
        .size(px(20.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .child(Glyph::CHECK.icon().size(px(12.0)))
}

/// "★ 4.0 (881 ratings)", or "No ratings yet".
fn rating_line(rating: Option<Rating>, palette: Palette) -> gpui::Div {
    let line = div().pt_1().flex().items_center().gap_1().text_xs();
    match rating.filter(|rating| rating.count > 0 || rating.tenths > 0) {
        Some(rating) => line
            .child(Glyph::STAR.icon().size(px(12.0)).text_color(rgb(palette.brand)))
            .child(div().font_weight(gpui::FontWeight::BOLD).child(stars(rating)))
            .child(format!("({})", ratings_count(rating.count))),
        None => line
            .text_color(rgb(palette.muted_foreground))
            .child(Glyph::STAR.icon().size(px(12.0)))
            .child("No ratings yet"),
    }
}

/// A rating's stars as Teams writes them: `4.0`.
#[must_use]
pub fn stars(rating: Rating) -> String {
    format!("{}.{}", rating.tenths / 10, rating.tenths % 10)
}

/// How many rated, as Teams writes it: `1 rating`, `881 ratings`, `1.1K ratings`, `74K ratings`.
#[must_use]
pub fn ratings_count(count: u64) -> String {
    match count {
        1 => "1 rating".to_owned(),
        _ => format!("{} ratings", short_count(count)),
    }
}

/// A count as Teams shortens it: `881`, `1.1K`, `74K`, `2.3M`.
#[must_use]
pub fn short_count(count: u64) -> String {
    let short = |count: u64, unit: u64, suffix: &str| {
        if count < unit * 10 {
            let tenths = (count * 10 + unit / 2) / unit;
            if tenths.is_multiple_of(10) {
                format!("{}{suffix}", tenths / 10)
            } else {
                format!("{}.{}{suffix}", tenths / 10, tenths % 10)
            }
        } else {
            format!("{}{suffix}", (count + unit / 2) / unit)
        }
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => short(count, 1_000, "K"),
        _ => short(count, 1_000_000, "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_stars_read_as_teams_writes_them() {
        assert_eq!(short_count(881), "881");
        assert_eq!(short_count(1_140), "1.1K");
        assert_eq!(short_count(4_000), "4K");
        assert_eq!(short_count(74_012), "74K");
        assert_eq!(short_count(113_400), "113K");
        assert_eq!(short_count(2_340_000), "2.3M");
        assert_eq!(ratings_count(1), "1 rating");
        assert_eq!(ratings_count(160_960), "161K ratings");
        assert_eq!(stars(Rating { tenths: 40, count: 1 }), "4.0");
        assert_eq!(stars(Rating { tenths: 43, count: 1 }), "4.3");
    }
}
