//! The editor's keys (Teams' compose hotkeys, from its web client) and what each does.

use gpui::{App, Context, Global, KeyBinding, Window, actions};
use teams_proto::chat::Marks;

use super::doc::{Kind, Pos, Selection};
use super::{CONTEXT, Editor, EditorEvent, Granularity};

actions!(
    rich_editor,
    [
        MoveLeft,
        MoveRight,
        MoveUp,
        MoveDown,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        LineStart,
        LineEnd,
        SelectLineStart,
        SelectLineEnd,
        DocStart,
        DocEnd,
        SelectDocStart,
        SelectDocEnd,
        SelectAll,
        Backspace,
        Delete,
        BackspaceWord,
        DeleteWord,
        /// Enter: a new paragraph, the next list item, or the send (see the editor's docs).
        Enter,
        /// Shift+Enter: a line break or a new paragraph (see the editor's docs).
        ShiftEnter,
        /// Ctrl+Enter: the send.
        Submit,
        /// Tab: nests a list item; the editor hands it on elsewhere.
        Tab,
        ShiftTab,
        /// Escape: the editor hands it on to its owner.
        Escape,
        Copy,
        Cut,
        Paste,
        PastePlain,
        Undo,
        Redo,
        Bold,
        Italic,
        Underline,
        Strikethrough,
        InlineCode,
        CodeBlock,
        BulletList,
        NumberedList,
        Quote,
        Heading1,
        Heading2,
        Heading3,
        Paragraph,
        /// Ctrl+Shift+X: the owner expands or collapses its box (Teams' "format" key); the editor hands it on.
        ToggleExpanded,
    ]
);

/// Marks the editor's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the editor's keys in its context, once per app.
pub(super) fn bind(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("left", MoveLeft, context),
        KeyBinding::new("right", MoveRight, context),
        KeyBinding::new("up", MoveUp, context),
        KeyBinding::new("down", MoveDown, context),
        KeyBinding::new("shift-left", SelectLeft, context),
        KeyBinding::new("shift-right", SelectRight, context),
        KeyBinding::new("shift-up", SelectUp, context),
        KeyBinding::new("shift-down", SelectDown, context),
        KeyBinding::new("ctrl-left", WordLeft, context),
        KeyBinding::new("ctrl-right", WordRight, context),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, context),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, context),
        KeyBinding::new("home", LineStart, context),
        KeyBinding::new("end", LineEnd, context),
        KeyBinding::new("shift-home", SelectLineStart, context),
        KeyBinding::new("shift-end", SelectLineEnd, context),
        KeyBinding::new("ctrl-home", DocStart, context),
        KeyBinding::new("ctrl-end", DocEnd, context),
        KeyBinding::new("ctrl-shift-home", SelectDocStart, context),
        KeyBinding::new("ctrl-shift-end", SelectDocEnd, context),
        KeyBinding::new("secondary-a", SelectAll, context),
        KeyBinding::new("backspace", Backspace, context),
        KeyBinding::new("shift-backspace", Backspace, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("ctrl-backspace", BackspaceWord, context),
        KeyBinding::new("ctrl-delete", DeleteWord, context),
        KeyBinding::new("enter", Enter, context),
        KeyBinding::new("shift-enter", ShiftEnter, context),
        KeyBinding::new("secondary-enter", Submit, context),
        KeyBinding::new("tab", Tab, context),
        KeyBinding::new("shift-tab", ShiftTab, context),
        KeyBinding::new("escape", Escape, context),
        KeyBinding::new("secondary-c", Copy, context),
        KeyBinding::new("ctrl-insert", Copy, context),
        KeyBinding::new("secondary-x", Cut, context),
        KeyBinding::new("shift-delete", Cut, context),
        KeyBinding::new("secondary-v", Paste, context),
        KeyBinding::new("shift-insert", Paste, context),
        KeyBinding::new("secondary-shift-v", PastePlain, context),
        KeyBinding::new("secondary-z", Undo, context),
        KeyBinding::new("secondary-shift-z", Redo, context),
        KeyBinding::new("secondary-y", Redo, context),
        KeyBinding::new("secondary-b", Bold, context),
        KeyBinding::new("secondary-i", Italic, context),
        KeyBinding::new("secondary-u", Underline, context),
        KeyBinding::new("secondary-alt-x", Strikethrough, context),
        // no Ctrl+Shift+C for inline code: the message box takes the keys first as a chat opens, and Ctrl+Shift+C
        // calls the chat shown (the shell's), as Teams' calling keys do from anywhere in the chat
        KeyBinding::new("secondary-shift-b", CodeBlock, context),
        KeyBinding::new("secondary-shift-7", NumberedList, context),
        KeyBinding::new("secondary-&", NumberedList, context),
        KeyBinding::new("secondary-shift-8", BulletList, context),
        KeyBinding::new("secondary-*", BulletList, context),
        KeyBinding::new("secondary-alt-4", Quote, context),
        KeyBinding::new("secondary-alt-1", Heading1, context),
        KeyBinding::new("secondary-alt-2", Heading2, context),
        KeyBinding::new("secondary-alt-3", Heading3, context),
        KeyBinding::new("secondary-alt-p", Paragraph, context),
        KeyBinding::new("secondary-shift-x", ToggleExpanded, context),
    ]);
    #[cfg(target_os = "macos")]
    bind_macos(cx);
}

/// macOS's text keys beside the others: Option moves by words, Command to a line's or the text's ends, as its text
/// views do (Ctrl and Home/End do the same elsewhere).
#[cfg(target_os = "macos")]
fn bind_macos(cx: &mut App) {
    let context = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("alt-left", WordLeft, context),
        KeyBinding::new("alt-right", WordRight, context),
        KeyBinding::new("alt-shift-left", SelectWordLeft, context),
        KeyBinding::new("alt-shift-right", SelectWordRight, context),
        KeyBinding::new("cmd-left", LineStart, context),
        KeyBinding::new("cmd-right", LineEnd, context),
        KeyBinding::new("cmd-shift-left", SelectLineStart, context),
        KeyBinding::new("cmd-shift-right", SelectLineEnd, context),
        KeyBinding::new("cmd-up", DocStart, context),
        KeyBinding::new("cmd-down", DocEnd, context),
        KeyBinding::new("cmd-shift-up", SelectDocStart, context),
        KeyBinding::new("cmd-shift-down", SelectDocEnd, context),
        KeyBinding::new("alt-backspace", BackspaceWord, context),
        KeyBinding::new("alt-delete", DeleteWord, context),
    ]);
}

impl Editor {
    /// Wires the editor's actions on its root element.
    pub(super) fn on_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::InteractiveElement as _;
        let root = root
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| this.horizontal(false, false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| this.horizontal(true, false, false, cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.horizontal(false, true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.horizontal(true, true, false, cx)))
            .on_action(cx.listener(|this, _: &WordLeft, _, cx| this.horizontal(false, false, true, cx)))
            .on_action(cx.listener(|this, _: &WordRight, _, cx| this.horizontal(true, false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectWordLeft, _, cx| this.horizontal(false, true, true, cx)))
            .on_action(cx.listener(|this, _: &SelectWordRight, _, cx| this.horizontal(true, true, true, cx)))
            .on_action(cx.listener(|this, _: &MoveUp, _, cx| this.vertical(false, false, cx)))
            .on_action(cx.listener(|this, _: &MoveDown, _, cx| this.vertical(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectUp, _, cx| this.vertical(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectDown, _, cx| this.vertical(true, true, cx)))
            .on_action(cx.listener(|this, _: &LineStart, _, cx| this.line_edge(false, false, cx)))
            .on_action(cx.listener(|this, _: &LineEnd, _, cx| this.line_edge(true, false, cx)))
            .on_action(cx.listener(|this, _: &SelectLineStart, _, cx| this.line_edge(false, true, cx)))
            .on_action(cx.listener(|this, _: &SelectLineEnd, _, cx| this.line_edge(true, true, cx)))
            .on_action(cx.listener(|this, _: &DocStart, _, cx| this.move_to(Pos::default(), false, cx)))
            .on_action(cx.listener(|this, _: &DocEnd, _, cx| this.move_to(this.doc.end(), false, cx)))
            .on_action(cx.listener(|this, _: &SelectDocStart, _, cx| this.move_to(Pos::default(), true, cx)))
            .on_action(cx.listener(|this, _: &SelectDocEnd, _, cx| this.move_to(this.doc.end(), true, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| {
                let end = this.doc.end();
                this.select(
                    Selection {
                        anchor: Pos::default(),
                        head: end,
                    },
                    cx,
                );
            }));
        Self::on_edit_actions(root, cx)
    }

    fn on_edit_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::InteractiveElement as _;
        let root = root
            .on_action(cx.listener(|this, _: &Backspace, _, cx| this.backspace(false, cx)))
            .on_action(cx.listener(|this, _: &BackspaceWord, _, cx| this.backspace(true, cx)))
            .on_action(cx.listener(|this, _: &Delete, _, cx| this.delete_forward(false, cx)))
            .on_action(cx.listener(|this, _: &DeleteWord, _, cx| this.delete_forward(true, cx)))
            .on_action(cx.listener(|this, _: &Enter, window, cx| this.enter(false, false, window, cx)))
            .on_action(cx.listener(|this, _: &ShiftEnter, window, cx| this.enter(true, false, window, cx)))
            .on_action(cx.listener(|this, _: &Submit, window, cx| this.enter(false, true, window, cx)))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_shift_tab))
            .on_action(cx.listener(|this, _: &Copy, _, cx| {
                if !this.copy(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.cut(cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.paste(false, window.scale_factor(), cx)))
            .on_action(cx.listener(|this, _: &PastePlain, window, cx| this.paste(true, window.scale_factor(), cx)))
            .on_action(cx.listener(|this, _: &Undo, _, cx| this.undo(cx)))
            .on_action(cx.listener(|this, _: &Redo, _, cx| this.redo(cx)));
        Self::on_format_actions(root, cx)
    }

    fn on_format_actions(root: gpui::Div, cx: &mut Context<Self>) -> gpui::Div {
        use gpui::InteractiveElement as _;
        root.on_action(cx.listener(|this, _: &Bold, _, cx| this.toggle_mark(Marks::BOLD, cx)))
            .on_action(cx.listener(|this, _: &Italic, _, cx| this.toggle_mark(Marks::ITALIC, cx)))
            .on_action(cx.listener(|this, _: &Underline, _, cx| this.toggle_mark(Marks::UNDERLINE, cx)))
            .on_action(cx.listener(|this, _: &Strikethrough, _, cx| this.toggle_mark(Marks::STRIKE, cx)))
            .on_action(cx.listener(|this, _: &InlineCode, _, cx| this.toggle_mark(Marks::CODE, cx)))
            .on_action(cx.listener(|this, _: &CodeBlock, _, cx| this.make_code(cx)))
            .on_action(cx.listener(|this, _: &BulletList, _, cx| this.toggle_kind(Kind::Bullet, cx)))
            .on_action(cx.listener(|this, _: &NumberedList, _, cx| this.toggle_kind(Kind::Numbered, cx)))
            .on_action(cx.listener(|this, _: &Quote, _, cx| this.toggle_kind(Kind::Quote, cx)))
            .on_action(cx.listener(|this, _: &Heading1, _, cx| this.set_kind(Kind::Heading(1), cx)))
            .on_action(cx.listener(|this, _: &Heading2, _, cx| this.set_kind(Kind::Heading(2), cx)))
            .on_action(cx.listener(|this, _: &Heading3, _, cx| this.set_kind(Kind::Heading(3), cx)))
            .on_action(cx.listener(|this, _: &Paragraph, _, cx| this.set_kind(Kind::Paragraph, cx)))
    }

    /// Tab nests the selected list items; elsewhere it is handed on.
    fn on_tab(&mut self, _: &Tab, _: &mut Window, cx: &mut Context<Self>) {
        if self.block_kind().is_list() {
            self.indent(true, cx);
        } else {
            cx.propagate();
        }
    }

    fn on_shift_tab(&mut self, _: &ShiftTab, _: &mut Window, cx: &mut Context<Self>) {
        if self.block_kind().is_list() {
            self.indent(false, cx);
        } else {
            cx.propagate();
        }
    }

    /// Left or Right (`forward`), by a character or a word, extending the selection or not. Without Shift, a
    /// selection collapses to its edge.
    fn horizontal(&mut self, forward: bool, extend: bool, word: bool, cx: &mut Context<Self>) {
        let head = self.selection.head;
        if !extend && !word && !self.selection.is_empty() {
            let (start, end) = self.selection.range();
            return self.move_to(if forward { end } else { start }, false, cx);
        }
        let to = match (forward, word) {
            (false, false) => self.doc.before(head),
            (true, false) => self.doc.after(head),
            (false, true) => Some(self.doc.word_before(head)),
            (true, true) => Some(self.doc.word_after(head)),
        };
        self.goal_x = None;
        if let Some(to) = to {
            self.move_to(to, extend, cx);
        }
    }

    /// Up or Down (`down`) a line, aiming at the column it started from.
    fn vertical(&mut self, down: bool, extend: bool, cx: &mut Context<Self>) {
        let from = if !extend && !self.selection.is_empty() {
            let (start, end) = self.selection.range();
            if down { end } else { start }
        } else {
            self.selection.head
        };
        let goal = self.goal_x.or_else(|| self.caret_point(from).map(|point| point.x));
        // on the first or last line the cursor stays, as the box's text area kept it
        let target = goal
            .and_then(|goal| self.line_neighbour(from, down, goal))
            .unwrap_or(from);
        self.move_to(target, extend, cx);
        self.goal_x = goal;
    }

    /// Home or End (`end`) of the cursor's line as drawn.
    fn line_edge(&mut self, end: bool, extend: bool, cx: &mut Context<Self>) {
        let head = self.selection.head;
        let target = self.line_edge_of(head, end).unwrap_or_else(|| {
            let len = self.doc.blocks[head.block].len();
            Pos::new(head.block, if end { len } else { 0 })
        });
        self.goal_x = None;
        self.move_to(target, extend, cx);
    }

    /// A press at `at` (a click count, Shift held): places the cursor, selects a word or a block, or extends the
    /// selection.
    pub(super) fn press(&mut self, at: Pos, clicks: usize, shift: bool, cx: &mut Context<Self>) {
        let (granularity, selection) = match clicks {
            0 | 1 if shift => (
                Granularity::Char,
                Selection {
                    anchor: self.selection.anchor,
                    head: at,
                },
            ),
            0 | 1 => (Granularity::Char, Selection::caret(at)),
            2 => (Granularity::Word, self.word_selection(at)),
            _ => (Granularity::Block, self.block_selection(at)),
        };
        self.goal_x = None;
        self.select(selection, cx);
        self.drag = Some((granularity, self.selection));
    }

    /// The pointer dragged to `at`: the selection's moving end follows, by the press's granularity.
    pub(super) fn drag_to(&mut self, at: Pos, cx: &mut Context<Self>) {
        let Some((granularity, pressed)) = self.drag else {
            return;
        };
        let (from, to) = pressed.range();
        let (start, end) = match granularity {
            Granularity::Char => (at, at),
            Granularity::Word => self.word_selection(at).range(),
            Granularity::Block => self.block_selection(at).range(),
        };
        let selection = match granularity {
            Granularity::Char => Selection {
                anchor: pressed.anchor,
                head: at,
            },
            _ if start < from => Selection {
                anchor: to,
                head: start,
            },
            _ => Selection {
                anchor: from,
                head: end,
            },
        };
        self.select(selection, cx);
    }

    pub(super) fn release(&mut self) {
        self.drag = None;
    }

    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    fn word_selection(&self, at: Pos) -> Selection {
        let text = self.doc.blocks[at.block].text();
        let word = crate::selection::word_at(&text, at.offset);
        let block = &self.doc.blocks[at.block];
        Selection {
            anchor: Pos::new(at.block, block.snap(word.start, false)),
            head: Pos::new(at.block, block.snap(word.end, true)),
        }
    }

    fn block_selection(&self, at: Pos) -> Selection {
        Selection {
            anchor: Pos::new(at.block, 0),
            head: Pos::new(at.block, self.doc.blocks[at.block].len()),
        }
    }

    /// Emits the send, as Ctrl+Enter does.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        cx.emit(EditorEvent::Submit);
    }
}
