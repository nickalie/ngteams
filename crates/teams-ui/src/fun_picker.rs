//! The compose box's picker, as Teams' "Emoji, GIFs and Stickers" (Teams' GIFs tab is not here): the tabs All, Emoji
//! and Stickers over one "Find something fun" search field.
//!
//! - **All**: Emoji (six of Recent's) and Stickers (four: the recent ones first, then the user's packs', then Teams'
//!   popular ones), each with "See all",
//!   which opens its tab. A search lists the emoji and the stickers it finds; Enter picks the first emoji found, or the
//!   first sticker when no emoji is.
//! - **Emoji**: the emoji picker ([`ReactionPicker::embedded`]), searched from this field.
//! - **Stickers**: Recent, then "Sticker packs": a tile for each pack the user added, then Teams' own packs in Teams'
//!   order with their headers ([`crate::stickers::teams`], for an account of Microsoft's cloud: an own server has
//!   none), the first four, "See all" for every one; and "Add a sticker pack" (import a folder, a `.zip` file or chosen
//!   files of pictures and Telegram stickers, [`crate::stickers::import`], or a Telegram or a Signal pack from its
//!   link, [`TelegramImport`], [`SignalImport`], drawn in the picker's place until it starts or is cancelled). A pack's
//!   tile opens its stickers (Teams' previews, read from Teams' sticker CDN with no credential and kept by the engine);
//!   a pack the user added has "More options" to rename or remove it. A search lists the user's stickers whose label or
//!   pack has its words, then Teams' stickers as Teams finds them (by name or keyword). How an import goes (its
//!   progress, what it added and skipped, why it failed) shows here until it is closed.
//!
//! A pick only tells ([`FunPickerEvent`]): the box puts an emoji in its text, and a sticker in the box as a picture: one
//! of the user's packs is sent to the media service at once, and one of Teams' is named by its address on Teams'
//! sticker CDN, with nothing sent. A box that takes no picture offers no sticker. The picker is reached by the keyboard
//! as by the pointer: its search field has the focus as it opens; Up from there reaches the tabs (Left and Right choose
//! one), Down the items of All and Stickers (the arrows move among them, Enter or Space picks one, Up from the first row
//! goes back to the field); on Emoji, the keyboard picks by a search and Enter, as in the emoji picker it shows;
//! Ctrl+Tab and Ctrl+Shift+Tab change the tab from anywhere in it; Escape or a click outside closes it.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, img, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Sizable as _, Size};

use crate::bridge::Core;
use crate::icons::{Glyph, selected_by};
use crate::inline::{PictureSource, PictureState};
use crate::reaction_picker::{ReactionPicker, ReactionPickerEvent, Section};
use crate::reactions;
use crate::signal_import::{SignalImport, SignalImportEvent};
use crate::sticker_editor::{Captioned, Draft, StickerEditor, StickerEditorEvent};
use crate::stickers::teams;
use crate::stickers::{ImportKind, ImportState, Shelf, StickerRef, Thumb};
use crate::telegram_import::{TelegramImport, TelegramImportEvent};
use crate::theme::{self, Palette};

/// The picker's width, px: the emoji picker's.
pub const WIDTH: f32 = crate::reaction_picker::WIDTH;
/// The tab's area under the search field, px: the emoji list and its section tabs.
const CONTENT_HEIGHT: f32 = 300.0;
/// The picker's padding, px: fixed, so that its grids fit at each font size.
const PADDING: f32 = 7.0;
/// The inside of the picker (its padding and border taken off), px.
const INNER: f32 = WIDTH - 2.0 * PADDING - 2.0;
/// The gap between tiles, px.
const GAP: f32 = 4.0;
/// The emoji All shows, and the most a search shows there.
const ALL_EMOJI: usize = 6;
const ALL_EMOJI_FOUND: usize = 12;
/// The stickers All shows, and the most a search shows there.
const ALL_STICKERS: usize = 4;
const ALL_STICKERS_FOUND: usize = 8;
/// The packs the Stickers tab shows before "See all".
const HOME_PACKS: usize = 4;
/// The skipped files a summary names; "and N more" after.
const SKIPPED_SHOWN: usize = 5;
/// What the Stickers tab says the user can add when there is no pack.
pub const NO_PACKS_HINT: &str = "Add your own: PNG, WebP or GIF pictures or Telegram stickers from a folder or a .zip \
                                 file, or a Telegram or Signal sticker pack from its link.";

/// What the picker tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FunPickerEvent {
    /// An emoji picked, by its key.
    Emoji(String),
    /// A sticker picked.
    Sticker(StickerRef),
    /// One of Teams' stickers with its caption drawn in ("Edit sticker"'s Done).
    Captioned(Captioned),
    /// Closed without a pick.
    Dismissed,
}

/// A tab of the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    All,
    Emoji,
    Stickers,
}

impl Tab {
    const ALL: [Self; 3] = [Self::All, Self::Emoji, Self::Stickers];

    fn title(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Emoji => "Emoji",
            Self::Stickers => "Stickers",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Emoji => "emoji",
            Self::Stickers => "stickers",
        }
    }
}

/// What the Stickers tab shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StickerView {
    /// Recent and the first packs.
    Home,
    /// Every pack ("See all").
    Packs,
    /// A pack's stickers, opened from Home or from every pack.
    Pack { id: String, from_packs: bool },
    /// One of Teams' packs, opened from Home or from every pack.
    TeamsPack { id: String, from_packs: bool },
}

/// Something in a tab that a click or the keyboard acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Emoji(&'static str),
    Sticker(StickerRef),
    Pack(String),
    /// One of Teams' packs, by its id.
    TeamsPack(String),
    SeeAllEmoji,
    SeeAllStickers,
    SeeAllPacks,
    AddPack,
    ImportFolder,
    ImportArchive,
    ImportFiles,
    ImportTelegram,
    ImportSignal,
    Back,
    PackMenu,
    Rename,
    SaveName,
    CancelName,
    Remove,
    ConfirmRemove,
    CancelRemove,
    OpenImported,
    CloseImport,
}

impl Item {
    /// Its test selector.
    #[must_use]
    pub fn selector(&self) -> String {
        match self {
            Self::Emoji(key) => format!("fun-emoji-{key}"),
            Self::Sticker(sticker) => sticker_selector(sticker),
            Self::Pack(id) => format!("sticker-pack-{id}"),
            Self::TeamsPack(id) => format!("teams-pack-{}", id.to_lowercase()),
            Self::SeeAllEmoji => "emoji-see-all".to_owned(),
            Self::SeeAllStickers => "stickers-see-all".to_owned(),
            Self::SeeAllPacks => "sticker-packs-see-all".to_owned(),
            Self::AddPack => "sticker-pack-add".to_owned(),
            Self::ImportFolder => "sticker-import-folder".to_owned(),
            Self::ImportArchive => "sticker-import-zip".to_owned(),
            Self::ImportFiles => "sticker-import-files".to_owned(),
            Self::ImportTelegram => "sticker-import-telegram".to_owned(),
            Self::ImportSignal => "sticker-import-signal".to_owned(),
            Self::Back => "sticker-back".to_owned(),
            Self::PackMenu => "sticker-pack-menu".to_owned(),
            Self::Rename => "sticker-pack-rename".to_owned(),
            Self::SaveName => "sticker-rename-save".to_owned(),
            Self::CancelName => "sticker-rename-cancel".to_owned(),
            Self::Remove => "sticker-pack-remove".to_owned(),
            Self::ConfirmRemove => "sticker-pack-remove-confirm".to_owned(),
            Self::CancelRemove => "sticker-pack-remove-cancel".to_owned(),
            Self::OpenImported => "sticker-import-open".to_owned(),
            Self::CloseImport => "sticker-import-close".to_owned(),
        }
    }
}

/// A sticker's test selector: `sticker-<pack>-<file's number>`, or `sticker-teams-<id>` for one of Teams'.
#[must_use]
pub fn sticker_selector(sticker: &StickerRef) -> String {
    if sticker.is_teams() {
        return format!("sticker-teams-{}", sticker.file.to_lowercase());
    }
    let number = sticker.file.split('.').next().unwrap_or_default();
    format!("sticker-{}-{number}", sticker.pack)
}

/// A line of a tab's content: drawn as one row of the scrolled area, and, when it has items, a row of the keyboard's.
enum Line {
    /// A section's title, with its actions on the right.
    Heading { title: SharedString, actions: Vec<Item> },
    /// A row of emoji or stickers, `columns` to a row.
    Cells { items: Vec<Item>, columns: usize },
    /// A row of pack tiles, two to a row.
    Packs(Vec<Item>),
    /// A menu's row.
    Menu(Item),
    /// A note in muted text.
    Note(SharedString),
    /// The back arrow of a pack's or every pack's view.
    Back,
    /// How the last import stands, with its actions.
    Import(Vec<Item>),
    /// No pack yet: what to do, with the import actions.
    Empty,
    /// The pack's name being edited, with Save and Cancel.
    Renaming,
    /// "Remove <pack>?", with Remove and Cancel.
    Removing(SharedString),
}

impl Line {
    fn items(&self) -> Vec<Item> {
        match self {
            Self::Heading { actions, .. } => actions.clone(),
            Self::Cells { items, .. } | Self::Packs(items) | Self::Import(items) => items.clone(),
            Self::Menu(item) => vec![item.clone()],
            Self::Back => vec![Item::Back],
            Self::Empty => vec![
                Item::ImportFolder,
                Item::ImportArchive,
                Item::ImportTelegram,
                Item::ImportSignal,
            ],
            Self::Renaming => vec![Item::SaveName, Item::CancelName],
            Self::Removing(_) => vec![Item::ConfirmRemove, Item::CancelRemove],
            Self::Note(_) => Vec::new(),
        }
    }
}

/// What opens under a heading of the Stickers tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Panel {
    None,
    /// The add-a-pack menu, or the pack's "More options".
    Menu,
    /// "Remove <pack>?"
    Removing,
}

/// Where the keyboard is in the picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Search,
    Tabs,
    /// The tab's items: row and column of the keyboard's rows.
    Items(usize, usize),
}

pub struct FunPicker {
    tab: Tab,
    view: StickerView,
    search: Entity<InputState>,
    emoji: Entity<ReactionPicker>,
    shelf: Entity<Shelf>,
    /// Whether stickers are offered (the box takes pictures).
    stickers: bool,
    /// Whether Teams' own packs are offered: stickers are, and the account is on Microsoft's cloud.
    teams: bool,
    /// Where Teams' pictures are read: the account's engine.
    pictures: PictureSource,
    tabs_focus: FocusHandle,
    items_focus: FocusHandle,
    /// The item the keyboard is on, while the items have the focus.
    at: (usize, usize),
    scroll: ScrollHandle,
    /// The add-a-pack menu, the pack's menu or the question before a removal, open.
    panel: Panel,
    /// The pack's new name being typed, with the subscription to its Enter.
    renaming: Option<(Entity<InputState>, Subscription)>,
    /// What went wrong with the last rename or removal.
    problem: Option<SharedString>,
    /// "Import a Telegram sticker pack", open in the picker's place.
    telegram: Option<(Entity<TelegramImport>, Subscription)>,
    /// "Import a Signal sticker pack", open in the picker's place.
    signal: Option<(Entity<SignalImport>, Subscription)>,
    /// "Edit sticker", open in the place of the tab's content.
    editing: Option<(Entity<StickerEditor>, Subscription)>,
    core: Arc<dyn Core>,
    /// The item drawn with the keyboard's ring, and whether the tabs have the focus, as this frame draws them.
    ring: Option<Item>,
    tabs_ringed: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<FunPickerEvent> for FunPicker {}

impl FunPicker {
    /// A picker on its All tab, its search field focused; stickers from `shelf` when `stickers`.
    pub fn new(
        core: Arc<dyn Core>,
        shelf: Entity<Shelf>,
        stickers: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find something fun"));
        let teams = stickers && core.capabilities().teams_stickers;
        let pictures = PictureSource::new(Arc::clone(&core));
        let emoji = cx.new(|cx| ReactionPicker::embedded(Arc::clone(&core), window, cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&search, window, |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.searched(cx);
                }
            }),
            cx.subscribe(&emoji, |_, _, event: &ReactionPickerEvent, cx| {
                if let ReactionPickerEvent::Picked(key) = event {
                    cx.emit(FunPickerEvent::Emoji(key.clone()));
                }
            }),
            cx.observe(&shelf, |_, _, cx| cx.notify()),
        ];
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app: a key is the picker's only while the focus is in it
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            let taken = this.update(cx, |this, cx| {
                this.holds_focus(window, cx) && this.key(event, window, cx)
            });
            if matches!(taken, Ok(true)) {
                cx.stop_propagation();
            }
        }));
        search.update(cx, |search, cx| search.focus(window, cx));
        Self {
            tab: Tab::All,
            view: StickerView::Home,
            search,
            emoji,
            shelf,
            stickers,
            teams,
            pictures,
            tabs_focus: cx.focus_handle(),
            items_focus: cx.focus_handle(),
            at: (0, 0),
            scroll: ScrollHandle::new(),
            panel: Panel::None,
            renaming: None,
            problem: None,
            telegram: None,
            signal: None,
            editing: None,
            core,
            ring: None,
            tabs_ringed: false,
            _subscriptions: subscriptions,
        }
    }

    #[must_use]
    pub fn tab(&self) -> Tab {
        self.tab
    }

    #[must_use]
    pub fn sticker_view(&self) -> &StickerView {
        &self.view
    }

    /// Where the keyboard is.
    #[must_use]
    pub fn place(&self, window: &Window, cx: &App) -> Option<Place> {
        if self.search.focus_handle(cx).contains_focused(window, cx) {
            Some(Place::Search)
        } else if self.tabs_focus.is_focused(window) {
            Some(Place::Tabs)
        } else if self.items_focus.is_focused(window) {
            Some(Place::Items(self.at.0, self.at.1))
        } else {
            None
        }
    }

    /// The item the keyboard is on, while the items have the focus.
    #[must_use]
    pub fn current(&self, window: &Window, cx: &App) -> Option<Item> {
        if !self.items_focus.is_focused(window) {
            return None;
        }
        self.rows(cx).get(self.at.0)?.get(self.at.1).cloned()
    }

    /// The items of the tab, row by row, as the keyboard moves among them.
    #[must_use]
    pub fn rows(&self, cx: &App) -> Vec<Vec<Item>> {
        self.lines(cx)
            .iter()
            .map(Line::items)
            .filter(|items| !items.is_empty())
            .collect()
    }

    /// The words of the Stickers tab's no-pack state, if the tab shows it.
    #[must_use]
    pub fn no_packs_hint(&self, cx: &App) -> Option<&'static str> {
        self.lines(cx)
            .iter()
            .any(|line| matches!(line, Line::Empty))
            .then_some(NO_PACKS_HINT)
    }

    /// Whether the focus is in the picker.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.place(window, cx).is_some()
            || self
                .renaming
                .as_ref()
                .is_some_and(|(field, _)| field.focus_handle(cx).contains_focused(window, cx))
    }

    /// Opens `tab`, its search kept.
    pub fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if tab == Tab::Stickers && !self.stickers {
            return;
        }
        self.tab = tab;
        self.view = StickerView::Home;
        self.close_menus();
        self.at = (0, 0);
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }

    /// Opens `view` of the Stickers tab, the search dropped.
    pub fn show_stickers(&mut self, view: StickerView, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Stickers;
        self.view = view;
        self.close_menus();
        self.at = (0, 0);
        self.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        if !self.query(cx).is_empty() {
            self.search.update(cx, |search, cx| search.set_value("", window, cx));
            self.emoji.update(cx, |emoji, cx| emoji.search_for("", cx));
        }
        cx.notify();
    }

    /// Puts `query` in the search field, as if typed.
    pub fn search_for(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_value(query, window, cx));
        self.searched(cx);
    }

    fn close_menus(&mut self) {
        self.panel = Panel::None;
        self.renaming = None;
        self.problem = None;
        self.editing = None;
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_owned()
    }

    /// The search field's X: the search dropped, the field focused.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| {
            search.set_value("", window, cx);
            search.focus(window, cx);
        });
        self.searched(cx);
    }

    fn searched(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        self.emoji.update(cx, |emoji, cx| emoji.search_for(&query, cx));
        self.at = (0, 0);
        cx.notify();
    }

    /// Imports the folder or archive at `source` into the shelf (as the add-a-pack menu's chooser does).
    pub fn import_from(&mut self, source: PathBuf, kind: ImportKind, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.shelf.update(cx, |shelf, cx| shelf.import(source, kind, cx));
        cx.notify();
    }

    /// The "Import a Telegram sticker pack" panel, while it is open.
    #[must_use]
    pub fn telegram_import(&self) -> Option<&Entity<TelegramImport>> {
        self.telegram.as_ref().map(|(panel, _)| panel)
    }

    /// Opens "Import a Telegram sticker pack" in the picker's place.
    pub fn open_telegram_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.signal = None;
        let panel = cx.new(|cx| TelegramImport::new(window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event: &TelegramImportEvent, window, cx| {
            this.telegram = None;
            if let TelegramImportEvent::Start(link) = event {
                this.show_stickers(StickerView::Home, window, cx);
                this.shelf.update(cx, |shelf, cx| shelf.import_link(link.clone(), cx));
            }
            this.search.update(cx, |search, cx| search.focus(window, cx));
            cx.notify();
        });
        self.telegram = Some((panel, events));
        cx.notify();
    }

    /// The "Import a Signal sticker pack" panel, while it is open.
    #[must_use]
    pub fn signal_import(&self) -> Option<&Entity<SignalImport>> {
        self.signal.as_ref().map(|(panel, _)| panel)
    }

    /// Opens "Import a Signal sticker pack" in the picker's place.
    pub fn open_signal_import(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.telegram = None;
        let panel = cx.new(|cx| SignalImport::new(window, cx));
        let events = cx.subscribe_in(&panel, window, |this, _, event: &SignalImportEvent, window, cx| {
            this.signal = None;
            if let SignalImportEvent::Start(link) = event {
                this.show_stickers(StickerView::Home, window, cx);
                this.shelf.update(cx, |shelf, cx| shelf.import_signal(link.clone(), cx));
            }
            this.search.update(cx, |search, cx| search.focus(window, cx));
            cx.notify();
        });
        self.signal = Some((panel, events));
        cx.notify();
    }

    /// Whether a click on `sticker` opens "Edit sticker": it is one of Teams' that takes a caption.
    fn takes_caption(sticker: &StickerRef) -> bool {
        sticker.is_teams() && Draft::of(&sticker.file).is_some()
    }

    /// "Edit sticker", while it is open.
    #[must_use]
    pub fn sticker_editor(&self) -> Option<&Entity<StickerEditor>> {
        self.editing.as_ref().map(|(editor, _)| editor)
    }

    /// Opens "Edit sticker" for Teams' sticker `id` in the place of the tab's content; its Done tells the box.
    pub fn open_editor(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = Draft::of(id) else {
            return;
        };
        let core = Arc::clone(&self.core);
        let editor = cx.new(|cx| StickerEditor::new(&core, draft, window, cx));
        let events = cx.subscribe_in(&editor, window, |this, _, event: &StickerEditorEvent, window, cx| {
            this.editing = None;
            match event {
                StickerEditorEvent::Done(captioned) => cx.emit(FunPickerEvent::Captioned(captioned.clone())),
                StickerEditorEvent::Cancel => {
                    this.items_focus.focus(window, cx);
                    this.reveal(cx);
                }
            }
            cx.notify();
        });
        self.editing = Some((editor, events));
        cx.notify();
    }

    /// Imports the chosen `files` into the shelf (as the add-a-pack menu's chooser does).
    pub fn import_files(&mut self, files: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.panel = Panel::None;
        self.shelf.update(cx, |shelf, cx| shelf.import_files(files, cx));
        cx.notify();
    }

    /// The user picks picture and sticker files to import.
    fn choose_files(cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import".into()),
            ..PathPromptOptions::default()
        });
        cx.spawn(async move |this, cx| {
            let files = picked.await.ok().and_then(Result::ok).flatten().unwrap_or_default();
            if !files.is_empty() {
                let _ = this.update(cx, |this, cx| this.import_files(files, cx));
            }
        })
        .detach();
    }

    /// The user picks a folder or a `.zip` file to import.
    fn choose_import(kind: ImportKind, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: kind == ImportKind::Archive,
            directories: kind == ImportKind::Folder,
            multiple: false,
            prompt: Some("Import".into()),
            ..PathPromptOptions::default()
        });
        cx.spawn(async move |this, cx| {
            let source = picked
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|paths| paths.into_iter().next());
            if let Some(source) = source {
                let _ = this.update(cx, |this, cx| this.import_from(source, kind, cx));
            }
        })
        .detach();
    }

    /// Acts on `item`, as its click or Enter does.
    pub fn activate(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Emoji(key) => cx.emit(FunPickerEvent::Emoji((*key).to_owned())),
            Item::Sticker(sticker) if Self::takes_caption(sticker) => self.open_editor(&sticker.file, window, cx),
            Item::Sticker(sticker) => cx.emit(FunPickerEvent::Sticker(sticker.clone())),
            Item::Pack(id) => {
                let from_packs = self.view == StickerView::Packs;
                self.show_stickers(
                    StickerView::Pack {
                        id: id.clone(),
                        from_packs,
                    },
                    window,
                    cx,
                );
            }
            Item::TeamsPack(id) => {
                let from_packs = self.view == StickerView::Packs;
                self.show_stickers(
                    StickerView::TeamsPack {
                        id: id.clone(),
                        from_packs,
                    },
                    window,
                    cx,
                );
            }
            Item::SeeAllEmoji => self.show(Tab::Emoji, cx),
            Item::SeeAllStickers => self.show_stickers(StickerView::Home, window, cx),
            Item::SeeAllPacks => self.show_stickers(StickerView::Packs, window, cx),
            Item::AddPack | Item::PackMenu => self.toggle_menu(cx),
            Item::ImportFolder => Self::choose_import(ImportKind::Folder, cx),
            Item::ImportArchive => Self::choose_import(ImportKind::Archive, cx),
            Item::ImportFiles => Self::choose_files(cx),
            Item::ImportTelegram => self.open_telegram_import(window, cx),
            Item::ImportSignal => self.open_signal_import(window, cx),
            Item::Back => {
                let back = match &self.view {
                    StickerView::Pack { from_packs: true, .. } | StickerView::TeamsPack { from_packs: true, .. } => {
                        StickerView::Packs
                    }
                    _ => StickerView::Home,
                };
                self.show_stickers(back, window, cx);
            }
            Item::Rename => self.start_rename(window, cx),
            Item::SaveName => self.save_name(window, cx),
            Item::CancelName => {
                self.renaming = None;
                self.problem = None;
                self.items_focus.focus(window, cx);
                cx.notify();
            }
            Item::Remove => {
                self.panel = Panel::Removing;
                self.problem = None;
                cx.notify();
            }
            Item::ConfirmRemove => self.remove_pack(window, cx),
            Item::CancelRemove => {
                self.panel = Panel::None;
                cx.notify();
            }
            Item::OpenImported => {
                let imported = match self.shelf.read(cx).import_state() {
                    Some(ImportState::Done(imported)) => Some(imported.pack.id.clone()),
                    _ => None,
                };
                self.shelf.update(cx, Shelf::dismiss_import);
                if let Some(id) = imported {
                    self.show_stickers(StickerView::Pack { id, from_packs: false }, window, cx);
                }
            }
            Item::CloseImport => self.shelf.update(cx, Shelf::dismiss_import),
        }
    }

    /// The pack the Stickers tab shows.
    fn shown_pack(&self) -> Option<String> {
        match &self.view {
            StickerView::Pack { id, .. } => Some(id.clone()),
            _ => None,
        }
    }

    fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(name) = self
            .shown_pack()
            .and_then(|id| self.shelf.read(cx).pack(&id).map(|pack| pack.name.clone()))
        else {
            return;
        };
        let field = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let subscription = cx.subscribe_in(&field, window, |this: &mut Self, _, event: &InputEvent, window, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.save_name(window, cx);
            }
        });
        field.update(cx, |field, cx| field.focus(window, cx));
        self.panel = Panel::None;
        self.problem = None;
        self.renaming = Some((field, subscription));
        cx.notify();
    }

    fn renaming_field(&self) -> Option<Entity<InputState>> {
        self.renaming.as_ref().map(|(field, _)| field.clone())
    }

    fn save_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(id), Some(field)) = (self.shown_pack(), self.renaming_field()) else {
            return;
        };
        let name = field.read(cx).value().to_string();
        match self.shelf.update(cx, |shelf, cx| shelf.rename(&id, &name, cx)) {
            Ok(()) => {
                self.renaming = None;
                self.problem = None;
                self.items_focus.focus(window, cx);
            }
            Err(error) => self.problem = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn remove_pack(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.shown_pack() else { return };
        match self.shelf.update(cx, |shelf, cx| shelf.remove(&id, cx)) {
            Ok(()) => self.show_stickers(StickerView::Home, window, cx),
            Err(error) => {
                self.panel = Panel::None;
                self.problem = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    /// Enter in the search field: the first emoji found (on All and Emoji), else the first sticker found.
    fn pick_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.query(cx).is_empty() {
            return;
        }
        if self.tab != Tab::Stickers
            && let Some(first) = self.emoji.read(cx).found().and_then(|found| found.first().copied())
        {
            cx.emit(FunPickerEvent::Emoji(first.to_owned()));
            return;
        }
        if self.tab != Tab::Emoji
            && let Some(first) = self.found_stickers(cx).into_iter().next()
        {
            self.activate(&Item::Sticker(first), window, cx);
        }
    }

    /// The user's stickers the search finds, then Teams' (as Teams finds them).
    fn found_stickers(&self, cx: &App) -> Vec<StickerRef> {
        if !self.stickers {
            return Vec::new();
        }
        let query = self.query(cx);
        let mut found = self.shelf.read(cx).search(&query);
        if self.teams {
            found.extend(
                teams::catalog()
                    .search(&query)
                    .into_iter()
                    .map(|sticker| StickerRef::teams(&sticker.id)),
            );
        }
        found
    }

    /// Recent's stickers this picker offers: Teams' only to an account that has them.
    fn recent(&self, cx: &App) -> Vec<StickerRef> {
        self.shelf
            .read(cx)
            .recent()
            .iter()
            .filter(|sticker| self.teams || !sticker.is_teams())
            .take(crate::stickers::store::RECENT)
            .cloned()
            .collect()
    }

    // --- the keyboard ---

    /// Handles `event` while the focus is in the picker; whether it took it.
    fn key(&mut self, event: &gpui::KeystrokeEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        let key = keystroke.key.as_str();
        if modifiers.control && key == "tab" {
            self.step_tab(if modifiers.shift { -1 } else { 1 }, window, cx);
            return true;
        }
        if modifiers.number_of_modifiers() != 0 {
            return false;
        }
        // the held Escape's first push acted (it ended the rename or closed the picker): its repeats do nothing more
        if key == "escape" && event.is_held {
            return true;
        }
        let renaming = self
            .renaming
            .as_ref()
            .is_some_and(|(field, _)| field.focus_handle(cx).contains_focused(window, cx));
        if renaming {
            if key == "escape" {
                self.activate(&Item::CancelName, window, cx);
                return true;
            }
            return false;
        }
        if key == "escape" {
            cx.emit(FunPickerEvent::Dismissed);
            return true;
        }
        match self.place(window, cx) {
            Some(Place::Search) => self.search_key(key, window, cx),
            Some(Place::Tabs) => self.tabs_key(key, window, cx),
            Some(Place::Items(..)) => self.items_key(key, window, cx),
            None => false,
        }
    }

    fn search_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match key {
            "enter" => {
                self.pick_first(window, cx);
                true
            }
            "up" => {
                self.tabs_focus.focus(window, cx);
                cx.notify();
                true
            }
            "down" if self.tab != Tab::Emoji && !self.rows(cx).is_empty() => {
                self.at = (0, 0);
                self.items_focus.focus(window, cx);
                self.reveal(cx);
                cx.notify();
                true
            }
            _ => false,
        }
    }

    fn tabs_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match key {
            "left" => self.step_tab(-1, window, cx),
            "right" => self.step_tab(1, window, cx),
            "home" => self.show(Tab::All, cx),
            "end" => self.show(*self.tabs().last().unwrap_or(&Tab::All), cx),
            "down" | "enter" | "space" => {
                self.search.update(cx, |search, cx| search.focus(window, cx));
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    fn items_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let rows = self.rows(cx);
        if rows.is_empty() {
            self.search.update(cx, |search, cx| search.focus(window, cx));
            return true;
        }
        let (row, column) = (self.at.0.min(rows.len() - 1), self.at.1);
        let column = column.min(rows[row].len() - 1);
        self.at = match key {
            "left" if column > 0 => (row, column - 1),
            "left" if row > 0 => (row - 1, rows[row - 1].len() - 1),
            "right" if column + 1 < rows[row].len() => (row, column + 1),
            "right" if row + 1 < rows.len() => (row + 1, 0),
            "down" if row + 1 < rows.len() => (row + 1, column.min(rows[row + 1].len() - 1)),
            "up" if row > 0 => (row - 1, column.min(rows[row - 1].len() - 1)),
            "up" => {
                self.search.update(cx, |search, cx| search.focus(window, cx));
                cx.notify();
                return true;
            }
            "home" => (0, 0),
            "end" => (rows.len() - 1, rows[rows.len() - 1].len() - 1),
            "enter" | "space" => {
                let item = rows[row][column].clone();
                self.activate(&item, window, cx);
                return true;
            }
            "left" | "right" | "down" => (row, column),
            _ => return false,
        };
        self.reveal(cx);
        cx.notify();
        true
    }

    /// The tabs offered.
    fn tabs(&self) -> Vec<Tab> {
        Tab::ALL
            .into_iter()
            .filter(|tab| *tab != Tab::Stickers || self.stickers)
            .collect()
    }

    fn step_tab(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let tabs = self.tabs();
        let at = tabs.iter().position(|tab| *tab == self.tab).unwrap_or_default();
        let next = (at.cast_signed() + by)
            .rem_euclid(tabs.len().cast_signed())
            .cast_unsigned();
        self.show(tabs[next], cx);
        if self.items_focus.is_focused(window) {
            self.search.update(cx, |search, cx| search.focus(window, cx));
        }
    }

    /// Opens or closes the add-a-pack or the pack's menu, the whole of an opened one in view.
    fn toggle_menu(&mut self, cx: &mut Context<Self>) {
        self.panel = if self.panel == Panel::Menu {
            Panel::None
        } else {
            Panel::Menu
        };
        self.renaming = None;
        self.reveal_menu(cx);
        cx.notify();
    }

    /// Scrolls an opened menu's last row into view, so that none of its rows is hidden below the list.
    fn reveal_menu(&self, cx: &App) {
        if let Some(index) = self.lines(cx).iter().rposition(|line| matches!(line, Line::Menu(_))) {
            self.scroll.scroll_to_item(index);
        }
    }

    /// Scrolls the item the keyboard is on into view.
    fn reveal(&self, cx: &App) {
        let lines = self.lines(cx);
        let mut row = 0;
        for (index, line) in lines.iter().enumerate() {
            if line.items().is_empty() {
                continue;
            }
            if row == self.at.0 {
                self.scroll.scroll_to_item(index);
                return;
            }
            row += 1;
        }
    }

    // --- the tabs' content ---

    fn lines(&self, cx: &App) -> Vec<Line> {
        match self.tab {
            Tab::All => self.all_lines(cx),
            Tab::Emoji => Vec::new(),
            Tab::Stickers => self.sticker_lines(cx),
        }
    }

    fn all_lines(&self, cx: &App) -> Vec<Line> {
        let query = self.query(cx);
        let mut lines = Vec::new();
        let emoji = self.emoji.read(cx);
        let (emoji, stickers): (Vec<&'static str>, Vec<StickerRef>) = if query.is_empty() {
            let mut emoji = emoji.section(Section::Recent);
            emoji.truncate(ALL_EMOJI);
            (emoji, self.all_stickers(cx))
        } else {
            let mut found = emoji.found().unwrap_or_default();
            found.truncate(ALL_EMOJI_FOUND);
            let mut stickers = self.found_stickers(cx);
            stickers.truncate(ALL_STICKERS_FOUND);
            (found, stickers)
        };
        lines.push(Line::Heading {
            title: "Emoji".into(),
            actions: vec![Item::SeeAllEmoji],
        });
        if emoji.is_empty() {
            lines.push(Line::Note("No emoji matches".into()));
        }
        lines.extend(emoji.chunks(ALL_EMOJI).map(|row| Line::Cells {
            items: row.iter().map(|key| Item::Emoji(key)).collect(),
            columns: ALL_EMOJI,
        }));
        if !self.stickers {
            return lines;
        }
        lines.push(Line::Heading {
            title: "Stickers".into(),
            actions: vec![Item::SeeAllStickers],
        });
        if stickers.is_empty() {
            lines.push(Line::Note(if query.is_empty() {
                "Your sticker packs show here. Add one in Stickers.".into()
            } else {
                "No stickers match".into()
            }));
        }
        lines.extend(sticker_rows(&stickers, ALL_STICKERS));
        lines
    }

    /// All's stickers: the recent ones, then the user's packs', then Teams' popular ones, [`ALL_STICKERS`] in all.
    fn all_stickers(&self, cx: &App) -> Vec<StickerRef> {
        let held = self.shelf.read(cx);
        let own = held.packs().iter().flat_map(|pack| {
            pack.stickers.iter().map(|sticker| StickerRef {
                pack: pack.id.clone(),
                file: sticker.file.clone(),
            })
        });
        let popular = teams::catalog()
            .popular()
            .into_iter()
            .filter(|_| self.teams)
            .map(|sticker| StickerRef::teams(&sticker.id));
        let mut stickers = self.recent(cx);
        for sticker in own.chain(popular) {
            if stickers.len() >= ALL_STICKERS {
                break;
            }
            if !stickers.contains(&sticker) {
                stickers.push(sticker);
            }
        }
        stickers.truncate(ALL_STICKERS);
        stickers
    }

    fn sticker_lines(&self, cx: &App) -> Vec<Line> {
        let query = self.query(cx);
        if !query.is_empty() {
            let found = self.found_stickers(cx);
            if found.is_empty() {
                return vec![Line::Note("No stickers match".into())];
            }
            return sticker_rows(&found, 3).collect();
        }
        let held = self.shelf.read(cx);
        let mut lines = Vec::new();
        if self.view != StickerView::Home {
            lines.push(Line::Back);
        }
        if let Some(state) = held.import_state() {
            lines.push(Line::Import(match state {
                ImportState::Running(_) => Vec::new(),
                ImportState::Done(_) => vec![Item::OpenImported, Item::CloseImport],
                ImportState::Failed(_) => vec![Item::CloseImport],
            }));
        }
        match &self.view {
            StickerView::Home | StickerView::Packs if held.packs().is_empty() && !self.teams => lines.push(Line::Empty),
            StickerView::Home => {
                let recent = self.recent(cx);
                if !recent.is_empty() {
                    lines.push(Line::Heading {
                        title: "Recent".into(),
                        actions: Vec::new(),
                    });
                    lines.extend(sticker_rows(&recent, 4));
                }
                self.packs_lines(&mut lines, held, true);
            }
            StickerView::Packs => self.packs_lines(&mut lines, held, false),
            StickerView::TeamsPack { id, .. } => self.teams_pack_lines(&mut lines, id),
            StickerView::Pack { id, .. } => self.pack_lines(&mut lines, held, id),
        }
        lines
    }

    /// One of Teams' packs: its stickers, with no title, as Teams shows one.
    fn teams_pack_lines(&self, lines: &mut Vec<Line>, id: &str) {
        let catalog = teams::catalog();
        let Some(pack) = catalog.pack(id).filter(|_| self.teams) else {
            lines.push(Line::Note("That sticker pack isn't there anymore.".into()));
            return;
        };
        let stickers: Vec<StickerRef> = catalog
            .stickers_of(&pack.id)
            .into_iter()
            .map(|sticker| StickerRef::teams(&sticker.id))
            .collect();
        lines.extend(sticker_rows(&stickers, 3));
    }

    /// A pack the user added: its name and menu, a rename or removal under way, then its stickers.
    fn pack_lines(&self, lines: &mut Vec<Line>, held: &Shelf, id: &str) {
        let Some(pack) = held.pack(id) else {
            lines.push(Line::Note("That sticker pack isn't there anymore.".into()));
            return;
        };
        lines.push(Line::Heading {
            title: pack.name.clone().into(),
            actions: vec![Item::PackMenu],
        });
        if self.panel == Panel::Menu {
            lines.push(Line::Menu(Item::Rename));
            lines.push(Line::Menu(Item::Remove));
        }
        if self.renaming.is_some() {
            lines.push(Line::Renaming);
        }
        if self.panel == Panel::Removing {
            let count = pack.stickers.len();
            let plural = if count == 1 { "" } else { "s" };
            lines.push(Line::Removing(
                format!("Remove \u{201c}{}\u{201d} and its {count} sticker{plural}?", pack.name).into(),
            ));
        }
        if let Some(problem) = &self.problem {
            lines.push(Line::Note(problem.clone()));
        }
        let stickers: Vec<StickerRef> = pack
            .stickers
            .iter()
            .map(|sticker| StickerRef {
                pack: pack.id.clone(),
                file: sticker.file.clone(),
            })
            .collect();
        lines.extend(sticker_rows(&stickers, 3));
    }

    fn packs_lines(&self, lines: &mut Vec<Line>, held: &Shelf, home: bool) {
        let mut actions = vec![Item::AddPack];
        if home {
            actions.push(Item::SeeAllPacks);
        }
        lines.push(Line::Heading {
            title: "Sticker packs".into(),
            actions,
        });
        if self.panel == Panel::Menu {
            lines.push(Line::Menu(Item::ImportFolder));
            lines.push(Line::Menu(Item::ImportArchive));
            lines.push(Line::Menu(Item::ImportFiles));
            lines.push(Line::Menu(Item::ImportTelegram));
            lines.push(Line::Menu(Item::ImportSignal));
        }
        let teams_packs = teams::catalog().packs().iter().filter(|_| self.teams);
        let packs: Vec<Item> = held
            .packs()
            .iter()
            .map(|pack| Item::Pack(pack.id.clone()))
            .chain(teams_packs.map(|pack| Item::TeamsPack(pack.id.clone())))
            .take(if home { HOME_PACKS } else { usize::MAX })
            .collect();
        lines.extend(packs.chunks(2).map(|row| Line::Packs(row.to_vec())));
    }
}

/// Rows of `stickers`, `columns` to a row.
fn sticker_rows(stickers: &[StickerRef], columns: usize) -> impl Iterator<Item = Line> + '_ {
    stickers.chunks(columns).map(move |row| Line::Cells {
        items: row.iter().cloned().map(Item::Sticker).collect(),
        columns,
    })
}

// --- drawing ---

/// A tile's side for `columns` to a row, px.
fn tile_side(columns: usize) -> f32 {
    #[expect(clippy::cast_precision_loss, reason = "a handful of columns")]
    let columns = columns.max(1) as f32;
    (INNER - GAP * (columns - 1.0)) / columns
}

impl FunPicker {
    /// `element` with the keyboard's ring when it is on `item`.
    fn ringed(
        &self,
        item: &Item,
        element: gpui::Stateful<gpui::Div>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on = self.ring.as_ref() == Some(item);
        let item = item.clone();
        let element = element
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| this.activate(&item, window, cx)));
        if on {
            element.border_2().border_color(rgb(palette.brand)).into_any_element()
        } else {
            element
                .border_2()
                .border_color(gpui::transparent_black())
                .into_any_element()
        }
    }

    fn line(&mut self, index: usize, line: &Line, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let drawn = match line {
            Line::Heading { title, actions } => self.heading(title.clone(), actions, palette, cx),
            Line::Cells { items, columns } => div()
                .flex()
                .gap(px(GAP))
                .pb(px(GAP))
                .children(items.iter().map(|item| self.cell(item, *columns, palette, cx)))
                .into_any_element(),
            Line::Packs(items) => div()
                .flex()
                .gap(px(GAP))
                .pb(px(GAP))
                .children(items.iter().map(|item| self.pack_tile(item, palette, cx)))
                .into_any_element(),
            Line::Menu(item) => self.menu_row(item, palette, cx),
            Line::Note(text) => div()
                .py_2()
                .px_1()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(text.clone())
                .into_any_element(),
            Line::Back => self.text_action(&Item::Back, Some(Glyph::ARROW_LEFT), "Back", palette, cx),
            Line::Import(items) => self.import_status(items, palette, cx),
            Line::Empty => self.empty(palette, cx),
            Line::Renaming => self.renaming_row(palette, cx),
            Line::Removing(text) => self.removing_row(text.clone(), palette, cx),
        };
        div().id(("fun-line", index)).child(drawn).into_any_element()
    }

    fn heading(
        &mut self,
        title: SharedString,
        actions: &[Item],
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .pt_2()
            .pb_1()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child(title),
            )
            .children(actions.iter().map(|item| match item {
                Item::AddPack => self.icon_action(item, Glyph::PLUS, "Add a sticker pack", palette, cx),
                Item::PackMenu => self.icon_action(item, Glyph::MORE, "More options", palette, cx),
                _ => self.text_action(item, None, "See all", palette, cx),
            }))
            .into_any_element()
    }

    fn icon_action(
        &mut self,
        item: &Item,
        glyph: Glyph,
        tip: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = item.selector();
        let element = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .p_0p5()
            .rounded_md()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
            .child(glyph.icon().size(px(16.0)).text_color(rgb(palette.foreground)));
        let element = if self.panel == Panel::Menu && matches!(item, Item::AddPack | Item::PackMenu) {
            element.bg(rgb(palette.selected))
        } else {
            element
        };
        self.ringed(item, element, palette, cx)
    }

    fn text_action(
        &mut self,
        item: &Item,
        glyph: Option<Glyph>,
        text: &'static str,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = item.selector();
        let after = glyph.is_none().then(|| {
            Glyph::CHEVRON_RIGHT
                .icon()
                .size(px(14.0))
                .text_color(rgb(palette.muted_foreground))
        });
        let element = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_md()
            .text_sm()
            .text_color(rgb(palette.foreground))
            .hover(move |style| style.bg(rgb(palette.hover)))
            .children(glyph.map(|glyph| glyph.icon().size(px(16.0))))
            .child(text)
            .children(after);
        div()
            .flex()
            .child(self.ringed(item, element, palette, cx))
            .into_any_element()
    }

    fn cell(&mut self, item: &Item, columns: usize, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let side = tile_side(columns);
        let selector = item.selector();
        let base = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .size(px(side))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_md();
        let element = match item {
            Item::Emoji(key) => {
                let tip: SharedString = reactions::catalog()
                    .get(key)
                    .map_or(*key, |(reaction, _)| reaction.description)
                    .into();
                base.hover(move |style| style.bg(rgb(palette.hover)))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .child(reactions::emoji(key, px(32.0)))
            }
            Item::Sticker(sticker) => {
                let label: SharedString = if sticker.is_teams() {
                    teams::catalog()
                        .sticker(&sticker.file)
                        .map_or_else(|| "Sticker".into(), |held| held.name.clone().into())
                } else {
                    self.shelf
                        .read(cx)
                        .sticker(sticker)
                        .map_or_else(|| "Sticker".into(), |held| held.label.clone().into())
                };
                let picture = self.picture(sticker, px(side - 10.0), palette, cx);
                let editable = Self::takes_caption(sticker);
                base.relative()
                    .bg(rgb(palette.hover))
                    .hover(move |style| style.bg(rgb(palette.selected)))
                    .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
                    .child(picture)
                    .when(editable, |cell| cell.child(edit_mark(palette)))
            }
            _ => base,
        };
        self.ringed(item, element, palette, cx)
    }

    /// A sticker's picture, fitted in `side`.
    fn picture(
        &mut self,
        sticker: &StickerRef,
        side: gpui::Pixels,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if sticker.is_teams() {
            let catalog = teams::catalog();
            return match catalog.sticker(&sticker.file) {
                Some(held) => self.preview(&catalog.preview(held), side, palette, cx),
                None => broken(palette),
            };
        }
        let shown = self.shelf.read(cx).sticker(sticker).map_or((1, 1), |held| held.size);
        match self.shelf.update(cx, |shelf, cx| shelf.thumb(sticker, cx)) {
            Thumb::Ready(image) => {
                #[expect(clippy::cast_precision_loss, reason = "a sticker is at most 180 px")]
                let (width, height) = (shown.0 as f32, shown.1 as f32);
                let scale = side / px(width.max(height).max(1.0));
                img(image)
                    .w(px(width * scale))
                    .h(px(height * scale))
                    .flex_none()
                    .into_any_element()
            }
            Thumb::Loading => div().size(side).into_any_element(),
            Thumb::Broken => broken(palette),
        }
    }

    /// A picture of Teams' sticker CDN at `src` (a preview or a pack's header), fitted in `side`; read by the account's
    /// engine the first time it is drawn.
    fn preview(&self, src: &str, side: gpui::Pixels, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        match crate::inline::picture(src, &self.pictures, cx) {
            PictureState::Shown(image, (width, height)) => {
                #[expect(clippy::cast_precision_loss, reason = "a preview is a few hundred pixels")]
                let (width, height) = (width as f32, height as f32);
                let scale = side / px(width.max(height).max(1.0));
                img(image)
                    .w(px(width * scale))
                    .h(px(height * scale))
                    .flex_none()
                    .into_any_element()
            }
            PictureState::Reading => div().size(side).into_any_element(),
            PictureState::Missing => broken(palette),
        }
    }

    fn pack_tile(&mut self, item: &Item, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (name, header) = match item {
            Item::Pack(id) => {
                let (name, first) = self.shelf.read(cx).pack(id).map_or_else(
                    || (SharedString::default(), None),
                    |pack| {
                        (
                            pack.name.clone().into(),
                            pack.stickers.first().map(|sticker| StickerRef {
                                pack: pack.id.clone(),
                                file: sticker.file.clone(),
                            }),
                        )
                    },
                );
                (name, first.map(|first| self.picture(&first, px(28.0), palette, cx)))
            }
            Item::TeamsPack(id) => match teams::catalog().pack(id) {
                Some(pack) => (
                    pack.name.clone().into(),
                    Some(
                        div()
                            .relative()
                            .child(self.preview(&pack.header(), px(28.0), palette, cx))
                            .when(teams::catalog().editable(id), |header| header.child(edit_mark(palette)))
                            .into_any_element(),
                    ),
                ),
                None => (SharedString::default(), None),
            },
            _ => return div().into_any_element(),
        };
        let selector = item.selector();
        let tip = name.clone();
        let element = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .w(px(tile_side(2)))
            .h(px(46.0))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .rounded_md()
            .bg(rgb(palette.hover))
            .hover(move |style| style.bg(rgb(palette.selected)))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child(name),
            )
            .children(header);
        self.ringed(item, element, palette, cx)
    }

    fn menu_row(&mut self, item: &Item, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let (glyph, text, danger) = match item {
            Item::ImportFolder => (Glyph::FOLDER, "Import a folder of pictures", false),
            Item::ImportArchive => (Glyph::FILE_ARCHIVE, "Import a .zip file of pictures", false),
            Item::ImportFiles => (Glyph::FILE_IMAGE, "Import picture or sticker files", false),
            Item::ImportTelegram => (Glyph::LINK, "Import a Telegram sticker pack", false),
            Item::ImportSignal => (Glyph::LINK, "Import a Signal sticker pack", false),
            Item::Rename => (Glyph::PENCIL, "Rename", false),
            _ => (Glyph::TRASH, "Remove", true),
        };
        let colour = if danger { theme::DANGER } else { palette.foreground };
        let selector = item.selector();
        let element = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .w_full()
            .px_2()
            .py_1p5()
            .flex()
            .items_center()
            .gap_2()
            .rounded_md()
            .text_sm()
            .text_color(rgb(colour))
            .hover(move |style| style.bg(rgb(palette.hover)))
            .child(glyph.icon().size(px(14.0)))
            .child(text);
        self.ringed(item, element, palette, cx)
    }

    fn button(
        &mut self,
        item: &Item,
        text: &'static str,
        primary: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selector = item.selector();
        let (background, foreground) = if primary {
            (palette.brand, palette.on_brand)
        } else {
            (palette.surface, palette.foreground)
        };
        let element = div()
            .id(ElementId::Name(selector.clone().into()))
            .debug_selector(move || selector.clone())
            .px_3()
            .py_1()
            .rounded_md()
            .text_sm()
            .bg(rgb(background))
            .text_color(rgb(foreground))
            .child(text);
        let element = if primary {
            element
        } else {
            element.border_1().border_color(rgb(palette.border))
        };
        self.ringed(item, element, palette, cx)
    }

    fn import_status(&mut self, items: &[Item], palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let state = self.shelf.read(cx).import_state().cloned();
        let body = match &state {
            Some(ImportState::Running(progress)) => running(*progress, self.shelf.read(cx).refused(), palette),
            Some(ImportState::Done(imported)) => {
                let count = imported.pack.stickers.len();
                let plural = if count == 1 { "" } else { "s" };
                summary(
                    format!(
                        "Added \u{201c}{}\u{201d} with {count} sticker{plural}.",
                        imported.pack.name
                    ),
                    &imported.skipped,
                    palette,
                )
            }
            Some(ImportState::Failed(error)) => {
                let skipped = match error {
                    crate::stickers::ImportError::NoStickers(skipped) => skipped.as_slice(),
                    _ => &[],
                };
                summary(error.to_string(), skipped, palette).text_color(rgb(theme::DANGER))
            }
            None => div(),
        };
        let mut actions = Vec::new();
        for item in items {
            actions.push(match item {
                Item::OpenImported => self.button(item, "Show", true, palette, cx),
                _ => self.button(item, "Close", false, palette, cx),
            });
        }
        div()
            .debug_selector(|| "sticker-import".to_owned())
            .my_1()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .text_color(rgb(palette.foreground))
            .child(body)
            .child(div().flex().gap_2().children(actions))
            .into_any_element()
    }

    fn empty(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let folder = self.button(&Item::ImportFolder, "Import a folder", true, palette, cx);
        let archive = self.button(&Item::ImportArchive, "Import a .zip file", false, palette, cx);
        let telegram = self.button(&Item::ImportTelegram, "Import a Telegram pack", false, palette, cx);
        let signal = self.button(&Item::ImportSignal, "Import a Signal pack", false, palette, cx);
        div()
            .debug_selector(|| "sticker-packs-empty".to_owned())
            .pt_6()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(48.0))
                    .rounded_full()
                    .bg(rgb(palette.selected))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Glyph::IMAGE_PLUS.icon().size(px(22.0)).text_color(rgb(palette.brand))),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(palette.foreground))
                    .child("No sticker packs yet"),
            )
            .child(
                div()
                    .px_4()
                    .text_center()
                    .text_sm()
                    .text_color(rgb(palette.muted_foreground))
                    .child(NO_PACKS_HINT),
            )
            .child(div().pt_1().flex().gap_2().child(folder).child(archive))
            .child(div().flex().gap_2().child(telegram).child(signal))
            .into_any_element()
    }

    fn renaming_row(&mut self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(field) = self.renaming_field() else {
            return div().into_any_element();
        };
        let save = self.button(&Item::SaveName, "Save", true, palette, cx);
        let cancel = self.button(&Item::CancelName, "Cancel", false, palette, cx);
        div()
            .py_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .debug_selector(|| "sticker-rename-field".to_owned())
                    .child(Input::new(&field).with_size(Size::Small)),
            )
            .child(div().flex().gap_2().child(save).child(cancel))
            .into_any_element()
    }

    fn removing_row(&mut self, text: SharedString, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let remove = self.button(&Item::ConfirmRemove, "Remove", true, palette, cx);
        let cancel = self.button(&Item::CancelRemove, "Cancel", false, palette, cx);
        div()
            .my_1()
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .text_sm()
            .text_color(rgb(palette.foreground))
            .child(text)
            .child(div().flex().gap_2().child(remove).child(cancel))
            .into_any_element()
    }

    fn tab_strip(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.tabs_ringed;
        let tabs = self.tabs().into_iter().map(|tab| {
            let chosen = tab == self.tab;
            let selector = format!("fun-tab-{}", tab.key());
            let underline =
                div()
                    .h(px(3.0))
                    .w_full()
                    .rounded_full()
                    .bg(rgb(if chosen { palette.brand } else { palette.surface }));
            let label = div()
                .px_2()
                .text_sm()
                .text_color(rgb(if chosen {
                    palette.foreground
                } else {
                    palette.muted_foreground
                }))
                .child(tab.title());
            let label = if chosen {
                label.font_weight(FontWeight::SEMIBOLD)
            } else {
                label
            };
            let label = if chosen && focused {
                label.rounded_md().border_2().border_color(rgb(palette.brand))
            } else {
                label.border_2().border_color(gpui::transparent_black())
            };
            div()
                .id(ElementId::Name(selector.clone().into()))
                .debug_selector(move || selector.clone())
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .pt_1()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show(tab, cx);
                    // typing goes on in the search field, as in Teams
                    this.search.update(cx, |search, cx| search.focus(window, cx));
                }))
                .child(label)
                .child(underline)
        });
        div()
            .id("fun-tabs")
            .track_focus(&self.tabs_focus)
            .flex()
            .gap_3()
            .px_1()
            .border_b_1()
            .border_color(rgb(palette.border))
            .children(tabs)
            .into_any_element()
    }

    /// The search field's end, as in Teams: a magnifier while it is empty, then an X that clears it.
    fn search_mark(&self, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.search.read(cx).value().is_empty() {
            let magnifier = Glyph::SEARCH
                .icon()
                .size(px(16.0))
                .text_color(rgb(palette.muted_foreground));
            return selected_by("fun-search-icon", magnifier).into_any_element();
        }
        div()
            .id("fun-search-clear")
            .debug_selector(|| "fun-search-clear".to_owned())
            .flex_none()
            .p_0p5()
            .rounded_md()
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .tooltip(|window, cx| Tooltip::new("Clear search").build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.clear_search(window, cx)))
            .child(Glyph::X.icon().size(px(16.0)).text_color(rgb(palette.foreground)))
            .into_any_element()
    }
}

/// A picture that can't be shown: the picture mark.
/// Teams' pencil mark on a sticker that takes a caption, and on a pack of them: a small dark disc at the bottom right.
fn edit_mark(palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "sticker-edit-mark".to_owned())
        .absolute()
        .right(px(2.0))
        .bottom(px(2.0))
        .size(px(16.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(rgb(palette.foreground))
        .child(Glyph::PENCIL.icon().size(px(10.0)).text_color(rgb(palette.background)))
}

fn broken(palette: Palette) -> AnyElement {
    Glyph::IMAGE
        .icon()
        .size(px(20.0))
        .text_color(rgb(palette.muted_foreground))
        .into_any_element()
}

/// A running import: its progress, and why a second import is refused, when one was.
fn running(progress: crate::stickers::Progress, refused: bool, palette: Palette) -> gpui::Div {
    let text = if progress.total == 0 {
        "Importing stickers\u{2026}".to_owned()
    } else {
        format!("Importing stickers\u{2026} {} of {}", progress.done, progress.total)
    };
    #[expect(clippy::cast_precision_loss, reason = "at most a thousand files")]
    let share = if progress.total == 0 {
        0.0
    } else {
        progress.done as f32 / progress.total as f32
    };
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .debug_selector(|| "sticker-import-progress".to_owned())
                .child(text),
        )
        .children(refused.then(|| {
            div()
                .debug_selector(|| "sticker-import-busy".to_owned())
                .text_color(rgb(palette.muted_foreground))
                .child("Wait until this pack is imported, then add the next one.")
        }))
        .child(
            div().h(px(4.0)).w_full().rounded_full().bg(rgb(palette.border)).child(
                div()
                    .h_full()
                    .w(gpui::relative(share))
                    .rounded_full()
                    .bg(rgb(palette.brand)),
            ),
        )
}

/// An import's outcome: `headline`, then the files skipped and why ([`SKIPPED_SHOWN`] named, "and N more").
fn summary(headline: String, skipped: &[crate::stickers::Skipped], palette: Palette) -> gpui::Div {
    let mut lines = div()
        .debug_selector(|| "sticker-import-summary".to_owned())
        .flex()
        .flex_col()
        .gap_0p5()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(headline));
    if !skipped.is_empty() {
        let count = skipped.len();
        let plural = if count == 1 { "" } else { "s" };
        lines = lines.child(format!("Skipped {count} file{plural}:"));
        lines = lines.children(skipped.iter().take(SKIPPED_SHOWN).map(|skipped| {
            div()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .truncate()
                .child(format!("{skipped}"))
        }));
        if count > SKIPPED_SHOWN {
            lines = lines.child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.muted_foreground))
                    .child(format!("and {} more", count - SKIPPED_SHOWN)),
            );
        }
    }
    lines
}

impl FunPicker {
    /// The picker's raised surface, with nothing in it yet.
    fn frame(palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .debug_selector(|| "compose-emoji-picker".to_owned())
            .occlude()
            .w(px(WIDTH))
            .p(px(PADDING))
            .flex()
            .flex_col()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(FunPickerEvent::Dismissed)))
    }
}

impl Render for FunPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        self.ring = self.current(window, cx);
        self.tabs_ringed = self.tabs_focus.is_focused(window);
        if let Some(panel) = self
            .telegram
            .as_ref()
            .map(|(panel, _)| panel.clone().into_any_element())
            .or_else(|| self.signal.as_ref().map(|(panel, _)| panel.clone().into_any_element()))
        {
            return Self::frame(palette, cx).child(panel);
        }
        if let Some((editor, _)) = &self.editing {
            return Self::frame(palette, cx)
                .child(self.tab_strip(palette, cx))
                .child(editor.clone());
        }
        let content = if self.tab == Tab::Emoji {
            div().h(px(CONTENT_HEIGHT)).child(self.emoji.clone()).into_any_element()
        } else {
            let lines = self.lines(cx);
            let drawn: Vec<AnyElement> = lines
                .iter()
                .enumerate()
                .map(|(index, line)| self.line(index, line, palette, cx))
                .collect();
            div()
                .id("fun-content")
                .debug_selector(|| "fun-content".to_owned())
                .track_focus(&self.items_focus)
                .h(px(CONTENT_HEIGHT))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .children(drawn)
                .into_any_element()
        };
        Self::frame(palette, cx)
            .child(self.tab_strip(palette, cx))
            .child(
                div().debug_selector(|| "fun-search".to_owned()).child(
                    Input::new(&self.search)
                        .with_size(Size::Small)
                        .suffix(self.search_mark(palette, cx)),
                ),
            )
            .child(content)
    }
}
