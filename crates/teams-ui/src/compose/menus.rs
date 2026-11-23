//! The box's menus of tools ([`fit`]):
//!
//! - **"+"** (Actions and apps): the actions of the tools that do not show on the toolbar, each with its icon and key:
//!   Teams' own tools first, then the AI tools (Correct, Translate and the translation language, which opens the
//!   language menu by "+"), then "Customize toolbar". The AI tools are there until the user shows them. With apps
//!   offered, the same rows are over the apps in its flyout, as Teams lists its own actions there, and "Customize
//!   toolbar" is at the flyout's foot.
//! - **"More options"**, on a stacked box's row: the tools that do not fit; with "+" in it, the hidden tools too.
//! - **The customize list** ([`super::customize`]): a right click on the toolbar, or "Customize toolbar", lists the
//!   buttons that can hide.
//!
//! A row keeps its tool's action, its key (bound in the box's key context as before) and its state (checked while its
//! button would be lit).

use gpui::{
    AnyElement, App, Context, DismissEvent, Entity, FocusHandle, Focusable as _, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, Styled as _, Subscription, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::{PopupMenu, PopupMenuItem};
use gpui_component::popover::Popover;
use gpui_component::{Selectable as _, Sizable as _, Size};

use super::fit::{Fit, Tool};
use super::{AttachFiles, Compose, CorrectDraft, TOOL, ToggleEmoji, TranslateDraft, apps::ACTIONS_AND_APPS};
use crate::chat_view::measured;
use crate::draft_translate::menu;
use crate::editor;
use crate::icons::{Glyph, icon_button, selected_by};
use crate::keyboard;

/// The row that opens the customize list.
pub const CUSTOMIZE: &str = "Customize toolbar";

/// A menu of the box's tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuKind {
    /// "More options" of a stacked box.
    More,
    /// "+" while it offers no apps: the hidden tools and "Customize toolbar".
    Plus,
}

impl MenuKind {
    /// Its button's selector; `<selector>-menu` is its menu's.
    #[must_use]
    pub fn selector(self) -> &'static str {
        match self {
            Self::More => "compose-more",
            Self::Plus => "compose-apps",
        }
    }

    fn popover(self) -> &'static str {
        match self {
            Self::More => "compose-more-popover",
            Self::Plus => "compose-apps-popover",
        }
    }

    fn menu_selector(self) -> &'static str {
        match self {
            Self::More => "compose-more-menu",
            Self::Plus => "compose-apps-menu",
        }
    }

    fn face(self) -> (Glyph, &'static str) {
        match self {
            Self::More => (Glyph::MORE, "More options"),
            Self::Plus => (Glyph::PLUS, ACTIONS_AND_APPS),
        }
    }

    /// Where its menu opens: More is on the left of a stacked row, the others near the box's right.
    fn anchor(self) -> gpui::Anchor {
        if self == Self::More {
            gpui::Anchor::BottomLeft
        } else {
            gpui::Anchor::BottomRight
        }
    }
}

/// What a menu row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Format,
    Emoji,
    Attach,
    Loop,
    Apps,
    Correct,
    Translate,
    Language,
    Customize,
}

impl Item {
    /// Its selector; with `-row`, its row's in a menu.
    #[must_use]
    pub(crate) fn selector(self) -> &'static str {
        match self {
            Self::Format => "compose-format",
            Self::Emoji => "compose-emoji",
            Self::Attach => "compose-attach",
            Self::Loop => "compose-loop",
            Self::Apps => "compose-apps",
            Self::Correct => "compose-correct",
            Self::Translate => "compose-translate",
            Self::Language => "compose-language",
            Self::Customize => "compose-customize",
        }
    }

    pub(crate) fn glyph(self) -> Glyph {
        match self {
            Self::Format => Glyph::TEXT_FORMAT,
            Self::Emoji => Glyph::EMOJI,
            Self::Attach => Glyph::PAPERCLIP,
            Self::Loop => Glyph::REPEAT,
            Self::Apps => Glyph::PLUS,
            Self::Correct => Glyph::SPELL_CHECK,
            Self::Translate => Glyph::LANGUAGES,
            Self::Language => Glyph::GLOBE,
            Self::Customize => Glyph::SETTINGS,
        }
    }

    /// Whether it is an AI tool's row: the AI tools' rows stand apart from Teams' own.
    fn uses_ai(self) -> bool {
        matches!(self, Self::Correct | Self::Translate | Self::Language)
    }

    pub(crate) fn action(self) -> Option<&'static dyn gpui::Action> {
        match self {
            Self::Format => Some(&editor::ToggleExpanded),
            Self::Emoji => Some(&ToggleEmoji),
            Self::Attach => Some(&AttachFiles),
            Self::Correct => Some(&CorrectDraft),
            Self::Translate => Some(&TranslateDraft),
            Self::Loop | Self::Apps | Self::Language | Self::Customize => None,
        }
    }
}

/// Whether a line goes above `item`, after `before`: Customize, and the AI tools' rows, stand apart from the rows
/// before them.
pub(super) fn apart(before: Option<Item>, item: Item) -> bool {
    before.is_some_and(|before| item == Item::Customize || before.uses_ai() != item.uses_ai())
}

/// A tool's actions as menu rows; `apps`: whether "+" offers apps, so that its row in More opens them.
fn items_of(tool: Tool, apps: bool) -> Vec<Item> {
    match tool {
        Tool::Format => vec![Item::Format],
        Tool::Emoji => vec![Item::Emoji],
        Tool::Attach => vec![Item::Attach],
        Tool::Loop => vec![Item::Loop],
        Tool::Apps if apps => vec![Item::Apps],
        Tool::Apps => Vec::new(),
        Tool::Correct => vec![Item::Correct],
        Tool::Translate => vec![Item::Translate, Item::Language],
    }
}

/// The menu that holds the hidden tools and "Customize toolbar": "+" on the row, else More.
fn plus_home(fit: &Fit) -> MenuKind {
    if fit.row.contains(&Tool::Apps) {
        MenuKind::Plus
    } else {
        MenuKind::More
    }
}

/// The menu whose button anchors what `tool` opens (its picker, its menu) while the tool has no button of its own.
fn anchor_home(fit: &Fit, tool: Tool) -> Option<MenuKind> {
    if fit.more.contains(&tool) {
        Some(MenuKind::More)
    } else if fit.hidden.contains(&tool) {
        Some(plus_home(fit))
    } else {
        None
    }
}

/// The holders of the menus' buttons. A click on a button does not give it the keys, thus Tab in a menu that a click
/// opened goes on from its button's holder, which is in the row but no stop of Tab's order.
pub(super) struct Triggers {
    more: FocusHandle,
    plus: FocusHandle,
}

impl Triggers {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            more: cx.focus_handle().tab_stop(false),
            plus: cx.focus_handle().tab_stop(false),
        }
    }

    fn of(&self, kind: MenuKind) -> &FocusHandle {
        match kind {
            MenuKind::More => &self.more,
            MenuKind::Plus => &self.plus,
        }
    }
}

/// A menu of tools while it is open: its menu and the rows it was built of.
pub(super) struct ToolMenu {
    kind: MenuKind,
    menu: Entity<PopupMenu>,
    rows: Vec<Row>,
    /// The tool the keyboard was on when the menu opened: Escape gives the keyboard back to it, as in Teams.
    back_to: Option<FocusHandle>,
    _dismissed: Subscription,
}

/// What a menu row shows: the menu is built again when one of its rows changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Row {
    pub(crate) item: Item,
    pub(crate) label: SharedString,
    pub(crate) disabled: bool,
    pub(crate) checked: bool,
}

impl Compose {
    /// The rows `kind` lists now.
    pub(super) fn menu_items(&self, kind: MenuKind, cx: &App) -> Vec<Item> {
        let fit = self.fit(cx);
        let apps = self.offers_apps(cx);
        let hidden = |items: &mut Vec<Item>| {
            items.extend(fit.hidden.iter().flat_map(|tool| items_of(*tool, apps)));
            items.push(Item::Customize);
        };
        let mut items = Vec::new();
        match kind {
            MenuKind::Plus => hidden(&mut items),
            MenuKind::More => {
                items.extend(fit.more.iter().flat_map(|tool| items_of(*tool, apps)));
                if plus_home(&fit) == MenuKind::More {
                    hidden(&mut items);
                }
            }
        }
        items
    }

    /// The selectors of the rows `kind` lists now; with `-row`, each row's.
    #[must_use]
    pub fn menu_selectors(&self, kind: MenuKind, cx: &App) -> Vec<&'static str> {
        self.menu_items(kind, cx).into_iter().map(Item::selector).collect()
    }

    fn menu_rows(&self, kind: MenuKind, cx: &App) -> Vec<Row> {
        self.menu_items(kind, cx)
            .into_iter()
            .map(|item| self.row(item, cx))
            .collect()
    }

    /// What a row says, and whether it is off or lit, as its tool's button would be.
    pub(super) fn row(&self, item: Item, cx: &App) -> Row {
        let (label, disabled, checked): (SharedString, bool, bool) = match item {
            Item::Format => {
                let label = if self.expanded {
                    "Hide formatting options"
                } else {
                    "Show formatting options"
                };
                (label.into(), false, self.expanded)
            }
            Item::Emoji => ("Emoji".into(), false, self.emoji.is_some()),
            Item::Attach => ("Attach files".into(), !self.accepts_files(), false),
            Item::Loop => ("Loop components".into(), true, false),
            Item::Apps => (ACTIONS_AND_APPS.into(), false, self.apps_flyout().is_some()),
            Item::Correct => {
                let (label, lit, can) = self.correct_state(cx);
                (label.into(), !lit && !can, lit)
            }
            Item::Translate => {
                let (label, busy, can) = self.translate_state(cx);
                (label, !busy && !can, busy)
            }
            Item::Language => {
                let language = self.settings.read(cx).settings().draft_translate_to.clone();
                (format!("Translation language: {language}").into(), false, false)
            }
            Item::Customize => (CUSTOMIZE.into(), false, self.customize.is_some()),
        };
        Row {
            item,
            label,
            disabled,
            checked,
        }
    }

    /// Does what a row's tool does.
    pub(super) fn take(&mut self, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        match item {
            Item::Format => {
                // the format bar opens for the text: its keys stay there when the menu closes
                if let Some(menu) = &mut self.menu {
                    menu.back_to = None;
                }
                self.toggle_expanded(window, cx);
            }
            Item::Emoji => self.toggle_emoji(window, cx),
            Item::Attach => self.choose_files(window, cx),
            Item::Apps => self.toggle_apps(window, cx),
            Item::Correct => self.correct_draft(window, cx),
            Item::Translate => self.translate_draft(window, cx),
            Item::Language => self.open_language_menu(window, cx),
            Item::Customize => self.customize_toolbar(window, cx),
            Item::Loop => {}
        }
    }

    /// Opens the menu `kind` (its button, tests, the tour), or the one asked for in place of another that is open. The
    /// keyboard goes into the menu with no row selected: Down selects its first row, Up its last row that can be taken,
    /// and after that each key skips the rows that are off.
    pub fn open_menu(&mut self, kind: MenuKind, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.as_ref().is_some_and(|menu| menu.kind == kind) {
            return;
        }
        let rows = self.menu_rows(kind, cx);
        if rows.is_empty() {
            return;
        }
        let back_to = match &self.menu {
            Some(open) => open.back_to.clone(),
            None => self.tools.drawn_on(),
        };
        self.menu = Some(self.tool_menu(kind, rows, back_to, window, cx));
        cx.notify();
    }

    /// Opens "More options".
    pub fn open_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(MenuKind::More, window, cx);
    }

    /// The menu `kind` of `rows`, the keyboard in it.
    fn tool_menu(
        &self,
        kind: MenuKind,
        rows: Vec<Row>,
        back_to: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> ToolMenu {
        let compose = cx.entity().downgrade();
        let text = self.editor.read(cx).focus.clone();
        let built = rows.clone();
        let menu = PopupMenu::build(window, cx, move |menu, window, _| {
            let mut before: Option<Item> = None;
            built
                .iter()
                .fold(menu.min_w(px(260.0)).action_context(text.clone()), |menu, row| {
                    let compose = compose.clone();
                    let item = row.item;
                    let menu = if apart(before, item) { menu.separator() } else { menu };
                    before = Some(item);
                    menu.item(menu_row(row, &text, window).on_click(move |_, window, cx| {
                        let _ = compose.update(cx, |this, cx| this.take(item, window, cx));
                    }))
                })
        });
        let dismissed = cx.subscribe_in(
            &menu,
            window,
            move |this: &mut Self, _, _: &DismissEvent, window, cx| {
                if this.menu.as_ref().is_some_and(|menu| menu.kind == kind) {
                    this.menu_closed(false, window, cx);
                }
            },
        );
        menu.focus_handle(cx).focus(window, cx);
        ToolMenu {
            kind,
            menu,
            rows,
            back_to,
            _dismissed: dismissed,
        }
    }

    /// As the box draws: when a row of the open menu no longer shows its tool's state, the menu is built again after
    /// this frame.
    pub(super) fn menus_follow_tools(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.rows != self.menu_rows(menu.kind, cx))
        {
            cx.defer_in(window, Self::refresh_menu);
        }
    }

    /// Builds the open menu again when a tool's state changed (a job started or stopped, the draft or the language
    /// changed), so that each row says what its click does now. A menu with no rows left closes.
    fn refresh_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.menu else { return };
        let kind = open.kind;
        let rows = self.menu_rows(kind, cx);
        if rows == open.rows {
            return;
        }
        if rows.is_empty() {
            return self.close_more(window, cx);
        }
        let back_to = open.back_to.clone();
        self.menu = Some(self.tool_menu(kind, rows, back_to, window, cx));
        cx.notify();
    }

    /// The open menu closed: by its button (`to_text`: Escape, a press outside it) or by the menu itself (a row
    /// taken, Escape). The keyboard goes back to the tool it was on when the menu opened while the keyboard is still in
    /// the menu, or in the text where the menu itself puts it (except after a row that sends it to the text). Else it
    /// goes to the text if `to_text`: a press or a row that sent the keyboard elsewhere keeps it there.
    fn menu_closed(&mut self, to_text: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let keys_near = menu.menu.focus_handle(cx).contains_focused(window, cx)
            || (!to_text && self.editor.read(cx).focus.is_focused(window));
        match menu.back_to {
            Some(tool) if keys_near => tool.focus(window, cx),
            _ if to_text => self.focus(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Tab or Shift+Tab in the open menu: it closes, and the keyboard goes on as from the tool it opened from, else as
    /// from its button, else to the text.
    fn tab_from_menu(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let from = menu.back_to.or_else(|| Some(self.triggers.of(menu.kind).clone()));
        if !self.tools.leave_from(from, forward, window, cx) {
            self.focus(window, cx);
        }
        cx.notify();
    }

    /// Closes the open menu of tools, the keyboard back in the text.
    pub fn close_more(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            self.focus(window, cx);
            cx.notify();
        }
    }

    /// Whether "More options" is open.
    #[must_use]
    pub fn more_open(&self) -> bool {
        self.menu_open() == Some(MenuKind::More)
    }

    /// The menu of tools that is open.
    #[must_use]
    pub fn menu_open(&self) -> Option<MenuKind> {
        self.menu.as_ref().map(|menu| menu.kind)
    }

    /// The button of the menu `kind` on the box's row: a menu of its rows, each with its icon, its key and its state.
    /// Escape closes it and gives the keyboard back to the tool it was on when the menu opened, or to the text when
    /// it was on no tool; Tab and Shift+Tab close it and go on as from that tool. What a row of it opens (the language menu, the emoji picker, apps) opens by this button.
    pub(super) fn menu_button(&self, kind: MenuKind, fit: &Fit, cx: &mut Context<Self>) -> AnyElement {
        let open = self.menu_open() == Some(kind);
        let menu = self
            .menu
            .as_ref()
            .filter(|menu| menu.kind == kind)
            .map(|menu| menu.menu.clone());
        let compose = cx.entity().downgrade();
        let weak = compose.clone();
        let (glyph, tip) = kind.face();
        let button = icon_button(kind.selector(), glyph, tip).with_size(Size::Size(px(TOOL)));
        let popover = Popover::new(kind.popover())
            .anchor(kind.anchor())
            .appearance(false)
            .trigger(button.selected(open))
            .open(open)
            .on_open_change(move |open, window, cx| {
                let open = *open;
                let _ = compose.update(cx, |this, cx| {
                    if open {
                        this.open_menu(kind, window, cx);
                    } else {
                        this.menu_closed(true, window, cx);
                    }
                });
            })
            .content(move |_, _, _| {
                let tab = |forward| {
                    let compose = weak.clone();
                    move |window: &mut Window, cx: &mut App| {
                        let _ = compose.update(cx, |this, cx| this.tab_from_menu(forward, window, cx));
                    }
                };
                let (out, back) = (tab(true), tab(false));
                div()
                    .debug_selector(move || kind.menu_selector().to_owned())
                    .on_action(move |_: &keyboard::row::Out, window, cx| out(window, cx))
                    .on_action(move |_: &keyboard::row::Back, window, cx| back(window, cx))
                    .children(menu.clone())
            });
        selected_by(kind.selector(), popover)
            .track_focus(self.triggers.of(kind))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .relative()
            .children(self.anchors(kind, fit, cx))
            .into_any_element()
    }

    /// What opens by the button of `kind` while it has no button of its own: the emoji picker, the language menu,
    /// apps' flyout and the customize list.
    pub(super) fn anchors(&self, kind: MenuKind, fit: &Fit, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut anchors = Vec::new();
        if anchor_home(fit, Tool::Emoji) == Some(kind) {
            anchors.push(measured(&self.emoji_at));
        }
        if anchor_home(fit, Tool::Apps) == Some(kind) {
            anchors.push(self.apps_anchor());
        }
        if plus_home(fit) == kind {
            anchors.push(measured(&self.customize_at));
        }
        if anchor_home(fit, Tool::Translate) == Some(kind) {
            anchors.extend(self.hidden_language_anchor(kind, cx));
        }
        anchors
    }

    /// The language menu's anchor by the button of `kind`: a face of no width at its right, so the menu its row opens
    /// shows by it.
    fn hidden_language_anchor(&self, kind: MenuKind, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu: Entity<menu::LanguageMenu> = self.language_menu()?;
        // the language row is the way to it: the anchor is no stop of the keyboard
        let face = Button::new("compose-language")
            .ghost()
            .with_size(Size::XSmall)
            .tab_stop(false);
        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .w(px(0.0))
                .h_full()
                .overflow_hidden()
                .child(menu::popover(
                    &menu,
                    "compose-language-popover",
                    kind.anchor(),
                    face,
                    cx,
                ))
                .into_any_element(),
        )
    }
}

/// A tool's row in a menu: its icon, its name, its key as bound where the `text` is, and a check while the tool is
/// lit (`<item>-row`, `<item>-row-key`, `<item>-check` for tests). The check is the row's own: the kit draws a row's
/// icon in place of its check. The language row shows a chevron, as it opens a menu of its own.
fn menu_row(row: &Row, text: &FocusHandle, window: &Window) -> PopupMenuItem {
    let selector = format!("{}-row", row.item.selector());
    let check = row.checked.then(|| format!("{}-check", row.item.selector()));
    let label = row.label.clone();
    let key_selector = format!("{selector}-key");
    let key = row
        .item
        .action()
        .and_then(|action| Kbd::binding_for_action_in(action, text, window));
    let opens = row.item == Item::Language;
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        let key = key.clone().map(|key| {
            let key_selector = key_selector.clone();
            div().debug_selector(move || key_selector).child(key)
        });
        let check = check.clone().map(|check| {
            div()
                .debug_selector(move || check.clone())
                .child(Glyph::CHECK.icon().size(px(14.0)))
        });
        let chevron = opens.then(|| Glyph::CHEVRON_RIGHT.icon().size(px(14.0)));
        div()
            .debug_selector(move || selector.clone())
            .flex_1()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(label.clone())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .children(key)
                    .child(div().flex_none().w(px(14.0)).children(check).children(chevron)),
            )
    })
    .icon(row.item.glyph().icon())
    .disabled(row.disabled)
}
