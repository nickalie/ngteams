//! Every reaction Teams offers, to pick one: the "more reactions" a message's quick reactions open.
//!
//! As Teams' picker: a search field over one list that scrolls through every reaction, "Recent" first and then each of
//! Teams' categories under its heading ([`reactions::catalog`]'s, in Teams' order, one per picture:
//! [`reactions::Catalog::pickable`]), with a tab per section under it. The tab of the section at the top of the list
//! is marked as the list scrolls, and a tab clicked scrolls the list to its section. Recent is the reactions the
//! account set most recently ([`Core::recent_reactions`], from the store), then the reactions Teams' config offers a
//! new account as recent ([`DEFAULT_RECENT`]), [`RECENT`] in all. Typing searches every category by description and id
//! (the list then holds what was found); Enter picks the first reaction found; Escape, or a click outside, dismisses
//! it. A reaction the account already set on the message is marked, and picking it takes it back. The reaction under
//! the pointer plays its animation, as Teams' picker does ([`reactions::animation`]). The list is laid out only as it
//! comes into view. The picker only tells what was picked ([`ReactionPickerEvent`]); the chat view writes it. The
//! emoji pickers of a shared file's message and of an event's description are the same picker
//! ([`ReactionPicker::for_emoji`]), nothing marked: Teams offers the same emoji in each, and the box puts the one picked
//! into its text. The compose box's picker ([`crate::fun_picker`]) shows it as its Emoji tab
//! ([`ReactionPicker::embedded`]): the list and its section tabs alone, searched from that picker's field.

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, EventEmitter, Focusable as _,
    InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, list, px, rgb,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::tooltip::Tooltip;
use gpui_component::{Icon, Sizable as _, Size};

use crate::bridge::Core;
use crate::reactions;
use crate::theme::{self, Palette};

/// The picker's width, px: a row of cells and its padding.
pub const WIDTH: f32 = 8.0 * CELL + 2.0 * PADDING + 4.0;
/// The picker's padding, px.
const PADDING: f32 = 8.0;
/// The reactions Recent holds.
pub const RECENT: usize = 24;
/// What Teams' config offers a new account as recent reactions (`recentReactions.defaultReactions`), in its order:
/// Recent after the account's own.
pub const DEFAULT_RECENT: [&str; 36] = [
    "surprised",
    "cry",
    "angry",
    "hearteyes",
    "cool",
    "handsinair",
    "fistbump",
    "fire",
    "1f389_partypopper",
    "1f440_eyes",
    "1f4af_hundredpointssymbol",
    "1f4a1_electriclightbulb",
    "2714_heavycheckmark",
    "2795_heavyplussign",
    "2716_heavymultiplicationx",
    "1f51c_soon",
    "1f6a2_ship",
    "think",
    "1f97a_pleadingface",
    "loudlycrying",
    "giggle",
    "relieved",
    "whew",
    "1f4ce_paperclip",
    "upsidedownface",
    "smilingfacewithtear",
    "screamingfear",
    "speechless",
    "blankface",
    "stareyes",
    "1f92a_zanyface",
    "unamused",
    "party",
    "rofl",
    "highfive",
    "rock",
];
/// The cells in a row ([`WIDTH`] is eight cells wide).
const COLUMNS: usize = 8;
/// A cell of the grid, px.
const CELL: f32 = 40.0;
/// A reaction drawn in a cell, px.
const EMOJI: f32 = 28.0;
/// The list's height, px.
const LIST_HEIGHT: f32 = 6.5 * CELL;
/// A section's tab, px.
const TAB: f32 = 20.0;
/// How far beyond the list's view rows are laid out, px.
const OVERDRAW: f32 = 2.0 * CELL;

/// What the picker tells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReactionPickerEvent {
    /// The reaction picked, by its key.
    Picked(String),
    /// Closed without a pick.
    Dismissed,
}

/// A section of the list: Recent, a category (its index among the catalog's), or what a search found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Recent,
    Category(usize),
    Found,
}

/// A row of the list: a section's heading, or a row of its reactions (indices in the catalog).
enum Row {
    Heading(Section),
    Cells(Section, Vec<usize>),
}

impl Section {
    /// The section in an element's id.
    fn key(self) -> String {
        match self {
            Self::Recent => "recent".to_owned(),
            Self::Category(index) => format!("category-{index}"),
            Self::Found => "found".to_owned(),
        }
    }
}

impl Row {
    fn section(&self) -> Section {
        match self {
            Self::Heading(section) | Self::Cells(section, _) => *section,
        }
    }
}

pub struct ReactionPicker {
    search: Entity<InputState>,
    /// Recent's reactions (indices in the catalog).
    recent: Vec<usize>,
    /// The reactions found for the search; `None` while nothing is searched.
    found: Option<Vec<usize>>,
    rows: Vec<Row>,
    list: ListState,
    /// The keys the account set on the message.
    mine: HashSet<String>,
    /// The reaction under the pointer.
    hovered: Option<&'static str>,
    /// Where Recent and the animations are read from.
    core: Arc<dyn Core>,
    /// What the picker is for: "more reactions" on a message, or the compose box's emoji.
    flavour: Flavour,
    _search_events: Subscription,
    /// Escape, before the field's own binding.
    _escape: Subscription,
}

impl EventEmitter<ReactionPickerEvent> for ReactionPicker {}

/// What a picker is for, in its words and its test selector.
#[derive(Clone, Copy)]
struct Flavour {
    selector: &'static str,
    placeholder: &'static str,
    /// What a search that finds nothing says.
    nothing_found: &'static str,
    /// Drawn inside another picker, which has the search field and the dismissal.
    embedded: bool,
}

/// "More reactions" on a message.
const REACTIONS: Flavour = Flavour {
    selector: "reaction-picker-more",
    placeholder: "Find a reaction",
    nothing_found: "No reaction matches",
    embedded: false,
};

/// The emoji picker of a shared file's message and of an event's description.
const COMPOSE_EMOJI: Flavour = Flavour {
    selector: "compose-emoji-picker",
    placeholder: "Search emoji",
    nothing_found: "No emoji matches",
    embedded: false,
};

/// The compose box's picker's Emoji tab.
const EMOJI_TAB: Flavour = Flavour {
    selector: "compose-emoji-tab",
    placeholder: "",
    nothing_found: "No emoji matches",
    embedded: true,
};

impl ReactionPicker {
    /// A picker for a message the account set `mine` on, its search field focused, its list at the top.
    pub fn new(mine: HashSet<String>, core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::make(mine, core, REACTIONS, window, cx)
    }

    /// The emoji picker of a shared file's message or an event's description: the emoji Teams offers, none of them
    /// marked, its search field focused.
    pub fn for_emoji(core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::make(HashSet::new(), core, COMPOSE_EMOJI, window, cx)
    }

    /// The Emoji tab of the compose box's picker ([`crate::fun_picker`]): the emoji Teams offers, none marked, with no
    /// search field of its own ([`Self::search_for`]) and no dismissal.
    pub fn embedded(core: Arc<dyn Core>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::make(HashSet::new(), core, EMOJI_TAB, window, cx)
    }

    /// Searches for `query`, as typed in the search field (an embedded picker's comes from its owner's field).
    pub fn search_for(&mut self, query: &str, cx: &mut Context<Self>) {
        let found = (!query.trim().is_empty()).then(|| reactions::catalog().search(query));
        if found.is_some() || self.found.is_some() {
            self.found = found;
            self.rebuild();
        }
        cx.notify();
    }

    /// The reactions found for the search, in order; `None` while nothing is searched.
    #[must_use]
    pub fn found(&self) -> Option<Vec<&'static str>> {
        self.found.as_ref().map(|found| {
            found
                .iter()
                .filter_map(|&index| reactions::catalog().at(index).map(|reaction| reaction.id))
                .collect()
        })
    }

    /// A picker of `flavour` with `mine` marked.
    fn make(
        mine: HashSet<String>,
        core: Arc<dyn Core>,
        flavour: Flavour,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(flavour.placeholder));
        let search_events = cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.searched(cx),
                InputEvent::PressEnter { .. } => this.pick_first(cx),
                _ => {}
            },
        );
        let this = cx.weak_entity();
        // the interceptor sees every keystroke of the app: an Escape is the picker's only while its search field has
        // the focus (a picker left open in a shell not shown, another account's, never takes the Escape meant for
        // what is shown)
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            let keystroke = &event.keystroke;
            if keystroke.key != "escape" || keystroke.modifiers.number_of_modifiers() != 0 {
                return;
            }
            let dismissed = this.update(cx, |this, cx| {
                let held = this.holds_focus(window, cx);
                if held {
                    cx.emit(ReactionPickerEvent::Dismissed);
                }
                held
            });
            if matches!(dismissed, Ok(true)) {
                cx.stop_propagation();
            }
        });
        if !flavour.embedded {
            search.update(cx, |search, cx| search.focus(window, cx));
        }
        let list = ListState::new(0, ListAlignment::Top, px(OVERDRAW));
        let scrolled = cx.weak_entity();
        // the tab of the section at the top follows the list
        list.set_scroll_handler(move |_, _, cx| {
            let _ = scrolled.update(cx, |_, cx| cx.notify());
        });
        let recent = cx.spawn(async move |this, cx| {
            let Ok(keys) = core_recent(&this, cx).await else { return };
            let _ = this.update(cx, |this, cx| {
                this.recent = recent_reactions(&keys);
                this.rebuild();
                cx.notify();
            });
        });
        recent.detach();
        let mut picker = Self {
            search,
            recent: recent_reactions(&[]),
            found: None,
            rows: Vec::new(),
            list,
            mine,
            hovered: None,
            core,
            flavour,
            _search_events: search_events,
            _escape: escape,
        };
        picker.rebuild();
        picker
    }

    /// The keys in the list, in order (what a search found, while one is under way).
    #[must_use]
    pub fn shown(&self) -> Vec<&'static str> {
        self.rows
            .iter()
            .flat_map(|row| match row {
                Row::Cells(_, cells) => cells.as_slice(),
                Row::Heading(_) => &[],
            })
            .filter_map(|&index| reactions::catalog().at(index).map(|reaction| reaction.id))
            .collect()
    }

    /// The keys of a section, in order.
    #[must_use]
    pub fn section(&self, section: Section) -> Vec<&'static str> {
        self.rows
            .iter()
            .filter(|row| row.section() == section)
            .flat_map(|row| match row {
                Row::Cells(_, cells) => cells.as_slice(),
                Row::Heading(_) => &[],
            })
            .filter_map(|&index| reactions::catalog().at(index).map(|reaction| reaction.id))
            .collect()
    }

    /// The section at the top of the list, whose tab is marked; `None` while a search is under way.
    #[must_use]
    pub fn active(&self) -> Option<Section> {
        if self.found.is_some() {
            return None;
        }
        self.rows.get(self.list.logical_scroll_top().item_ix).map(Row::section)
    }

    /// Scrolls the list to `section` (and drops the search).
    pub fn show(&mut self, section: Section, window: &mut Window, cx: &mut Context<Self>) {
        if self.found.take().is_some() {
            self.search.update(cx, |search, cx| search.set_value("", window, cx));
            self.rebuild();
        }
        if let Some(at) = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Heading(heading) if *heading == section))
        {
            self.list.scroll_to(ListOffset {
                item_ix: at,
                offset_in_item: px(0.0),
            });
        }
        cx.notify();
    }

    /// Picks `key`.
    pub fn pick(&mut self, key: &str, cx: &mut Context<Self>) {
        cx.emit(ReactionPickerEvent::Picked(key.to_owned()));
    }

    /// The reaction under the pointer.
    #[must_use]
    pub fn hovered(&self) -> Option<&'static str> {
        self.hovered
    }

    fn searched(&mut self, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value();
        self.search_for(&query, cx);
    }

    /// Enter: the first reaction found, when something was searched.
    fn pick_first(&mut self, cx: &mut Context<Self>) {
        let first = self
            .found
            .as_ref()
            .and_then(|found| found.first())
            .and_then(|&index| reactions::catalog().at(index));
        if let Some(reaction) = first {
            self.pick(reaction.id, cx);
        }
    }

    /// The rows for what is shown, the list back at its top.
    fn rebuild(&mut self) {
        let catalog = reactions::catalog();
        let sections: Vec<(Section, Vec<usize>)> =
            match &self.found {
                Some(found) => vec![(Section::Found, found.clone())],
                None => [(Section::Recent, self.recent.clone())]
                    .into_iter()
                    .chain(catalog.categories().iter().enumerate().map(|(index, category)| {
                        (Section::Category(index), catalog.pickable(category.reactions.clone()))
                    }))
                    .filter(|(_, reactions)| !reactions.is_empty())
                    .collect(),
            };
        self.rows.clear();
        if sections.is_empty() {
            self.rows.push(Row::Heading(Section::Found));
        }
        for (section, reactions) in sections {
            self.rows.push(Row::Heading(section));
            self.rows.extend(
                reactions
                    .chunks(COLUMNS)
                    .map(|cells| Row::Cells(section, cells.to_vec())),
            );
        }
        // every row is a cell high: the list knows its whole height before it lays the rows out, so it scrolls through
        // all of them at once
        self.list.reset_with_uniform_height(self.rows.len(), px(CELL));
    }

    /// Whether the focus is still in the picker (its search field): a click outside it may have moved it elsewhere.
    pub fn holds_focus(&self, window: &Window, cx: &App) -> bool {
        self.search.focus_handle(cx).contains_focused(window, cx)
    }

    fn hover(&mut self, id: &'static str, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            self.hovered = Some(id);
        } else if self.hovered == Some(id) {
            self.hovered = None;
        }
        cx.notify();
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let palette = theme::current(cx);
        match self.rows.get(index) {
            Some(Row::Heading(section)) => div()
                .h(px(CELL))
                .flex()
                .items_end()
                .px_1()
                .pb_1()
                .text_xs()
                .text_color(rgb(palette.muted_foreground))
                .child(self.title(*section))
                .into_any_element(),
            Some(Row::Cells(section, cells)) => div()
                .flex()
                .children(
                    cells
                        .iter()
                        .filter_map(|&index| self.cell(*section, index, palette, cx)),
                )
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn title(&self, section: Section) -> SharedString {
        match section {
            Section::Recent => "Recent".into(),
            Section::Category(index) => reactions::catalog()
                .categories()
                .get(index)
                .map_or("", |category| category.title)
                .into(),
            Section::Found if self.found.as_ref().is_none_or(Vec::is_empty) => self.flavour.nothing_found.into(),
            Section::Found => "Found".into(),
        }
    }

    fn cell(
        &self,
        section: Section,
        index: usize,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let reaction = reactions::catalog().at(index)?;
        let (id, description): (&'static str, SharedString) = (reaction.id, reaction.description.into());
        let animated = (self.hovered == Some(id))
            .then(|| reactions::animation(id, &self.core, cx))
            .flatten();
        let cell = div()
            // a reaction can show twice (in Recent and in its category): each copy is an element of its own
            .id(ElementId::Name(format!("emoji-{}-{id}", section.key()).into()))
            .debug_selector(move || format!("emoji-{id}"))
            .size(px(CELL))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(move |style| style.bg(rgb(palette.hover)))
            .tooltip(move |window, cx| Tooltip::new(description.clone()).build(window, cx))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| this.hover(id, *hovered, cx)))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(id, cx)))
            .child(reactions::emoji_playing(id, px(EMOJI), animated));
        Some(if self.mine.contains(id) {
            cell.bg(rgb(palette.selected))
        } else {
            cell
        })
    }

    fn tabs(&self, palette: Palette, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let catalog = reactions::catalog();
        let active = self.active();
        let categories = catalog.categories().iter().enumerate().map(|(index, category)| {
            let face = catalog
                .pickable(category.reactions.clone())
                .first()
                .and_then(|&first| catalog.at(first))
                .map(|reaction| reactions::emoji(reaction.id, px(TAB)));
            (Section::Category(index), category.title, face)
        });
        let recent = Icon::default()
            .data(include_bytes!("../assets/icons/clock.svg"))
            .size(px(TAB))
            .text_color(rgb(palette.muted_foreground))
            .into_any_element();
        let tabs = [(Section::Recent, "Recent", Some(recent))]
            .into_iter()
            .chain(categories)
            .map(|(section, title, face)| {
                let title = SharedString::from(title);
                let selector = match section {
                    Section::Category(index) => format!("reaction-category-{index}"),
                    Section::Recent | Section::Found => "reaction-recent".to_owned(),
                };
                let tab = div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector.clone())
                    .p_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(move |style| style.bg(rgb(palette.hover)))
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| this.show(section, window, cx)))
                    .children(face);
                if active == Some(section) {
                    tab.bg(rgb(palette.selected))
                } else {
                    tab
                }
            });
        div()
            .flex()
            .justify_between()
            .pt_1()
            .border_t_1()
            .border_color(rgb(palette.border))
            .children(tabs)
    }
}

impl Render for ReactionPicker {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let rows = list(
            self.list.clone(),
            cx.processor(|this: &mut Self, index: usize, _window, cx| this.render_row(index, cx)),
        )
        .w_full()
        .h(px(LIST_HEIGHT));
        let selector = self.flavour.selector;
        if self.flavour.embedded {
            return div()
                .debug_selector(move || selector.to_owned())
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(rows)
                .child(self.tabs(palette, cx));
        }
        div()
            .debug_selector(move || selector.to_owned())
            .occlude()
            .w(px(WIDTH))
            // in px, not rems: a larger font must not narrow the row of cells
            .p(px(PADDING))
            .flex()
            .flex_col()
            .gap_1()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ReactionPickerEvent::Dismissed)))
            .child(Input::new(&self.search).with_size(Size::Small).cleanable(true))
            .child(rows)
            .child(self.tabs(palette, cx))
    }
}

/// Asks the engine for the account's recent reactions.
async fn core_recent(
    this: &gpui::WeakEntity<ReactionPicker>,
    cx: &mut gpui::AsyncApp,
) -> Result<Vec<String>, teams_core::EngineError> {
    let reply = this
        .update(cx, |this, _| this.core.recent_reactions(RECENT))
        .map_err(|_| teams_core::EngineError::Stopped)?;
    reply.await
}

/// Recent: the account's own recent `keys` (a key with a skin tone as its reaction), then [`DEFAULT_RECENT`], each
/// reaction once and only one the picker offers, [`RECENT`] in all.
fn recent_reactions(keys: &[String]) -> Vec<usize> {
    let catalog = reactions::catalog();
    let indices = keys
        .iter()
        .map(String::as_str)
        .chain(DEFAULT_RECENT)
        .filter_map(|key| catalog.get(key))
        .filter_map(|(reaction, _)| catalog.index_of(reaction.id));
    let mut recent = catalog.pickable(indices);
    recent.truncate(RECENT);
    recent
}
