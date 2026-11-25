//! A rich-text editor of our own, as Teams' message box edits (WYSIWYG: the formatting shown as it will look, never
//! Markdown source), drawn with our own text elements, never the kit's text area or its Markdown or HTML view. The
//! compose box writes messages with it and the calendar's event form can write an event's body with it: it is a
//! component, [`Editor`], with a format bar of its own ([`toolbar::FormatBar`]).
//!
//! - **The document** ([`doc`]): paragraphs, headings, quote lines, bulleted and numbered list items (nested), blocks
//!   of code and rules, of runs carrying marks (bold, italic, underline, strikethrough, inline code), a font colour, a
//!   highlight, a size, a link or a mention (an atom). It is read as `teams-proto`'s rich text and written back as it
//!   ([`convert`]), so what is sent is what Teams' editor sends and what our renderer draws (`crate::rich_text`): the
//!   editor draws its runs with the renderer's own styles ([`crate::rich_text::highlight`]).
//! - **Keys** (Teams' own, from its web client's compose hotkeys): Ctrl+B, Ctrl+I, Ctrl+U, Ctrl+Alt+X strikethrough,
//!   Ctrl+Shift+B a block of code (Ctrl+Shift+C stays the call's key), Ctrl+Shift+7 and Ctrl+Shift+8 numbered and bulleted
//!   lists, Ctrl+Alt+4 a quote, Ctrl+Alt+1 to 3 headings and Ctrl+Alt+P a paragraph; Ctrl+Z and Ctrl+Y (Ctrl+Shift+Z)
//!   undo and redo; Ctrl+C, Ctrl+X, Ctrl+V (Ctrl+Shift+V as plain text); Tab and Shift+Tab nest and un-nest a list
//!   item. **Enter** ([`EnterMode`]): with [`EnterMode::Send`] (the message box as Teams first shows it) Enter asks to
//!   send ([`EditorEvent::Submit`]) and Shift+Enter starts a new paragraph; with [`EnterMode::NewLine`] (the box
//!   expanded) Enter starts a new paragraph, Shift+Enter breaks the line and Ctrl+Enter asks to send. In a list
//!   Enter starts the next item (on an empty item it ends the list) and Shift+Enter breaks the line; in a block of
//!   code Enter breaks the line (twice at its end leaves it). Ctrl+Enter always asks to send.
//! - **Copy and paste**: a copy puts the selection's plain text on the clipboard with the rich text itself beside it,
//!   so a paste into an editor keeps every format it has; a paste of HTML another program offers keeps what the editor
//!   writes; any other paste, and every paste into a block of code, is plain text, a paragraph per line (in a list
//!   item an item per line, in a block of code its lines). A picture on the clipboard, or picture files a file manager
//!   copied, go in the box as pictures of their own, where its owner takes them ([`pictures`]).
//! - **Typing goes through GPUI's input handler** ([`EntityInputHandler`]), so an input method composes text in place.

pub mod convert;
pub mod doc;
mod input;
mod keys;
pub mod pictures;
pub mod toolbar;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, Pixels, ScrollHandle, SharedString,
    Subscription, TextLayout, Window,
};
use teams_proto::chat::{FontSize, Marks, Node, Rgb};

pub use convert::CantLoad;
use doc::{Attrs, Block, Doc, Kind, MAX_INDENT, Pos, Run, Selection};
pub use keys::{
    Bold, BulletList, CodeBlock, Copy, Cut, Enter, Escape, Heading1, Heading2, Heading3, InlineCode, Italic, MoveDown,
    MoveUp, NumberedList, Paragraph, Paste, Quote, Redo, ShiftEnter, ShiftTab, Strikethrough, Submit, Tab,
    ToggleExpanded, Underline, Undo,
};

/// The editor's key context.
pub const CONTEXT: &str = "RichEditor";

/// How long typing goes on as one step of undo.
const TYPING_STEP: Duration = Duration::from_secs(1);

/// The most steps undo keeps.
const UNDO_STEPS: usize = 100;

/// What Enter does outside a list and a block of code.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EnterMode {
    /// Enter asks to send; Shift+Enter starts a new paragraph (Teams' message box as it first shows).
    #[default]
    Send,
    /// Enter starts a new paragraph, Shift+Enter breaks the line, Ctrl+Enter asks to send (the box expanded).
    NewLine,
}

/// What the editor tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorEvent {
    /// What it holds changed.
    Changed,
    /// The selection moved (no change of text).
    Moved,
    Focus,
    Blur,
    /// The user asked to send what it holds.
    Submit,
    /// A picture was put in the box ([`pictures`]), or its Retry pressed: its owner sends it, by its id.
    Picture(u64),
    /// A paste brought files that aren't pictures: its owner attaches them.
    Files(Vec<std::path::PathBuf>),
}

/// One step of undo: the document and the selection before it.
#[derive(Clone, Debug)]
struct Snapshot {
    doc: Doc,
    selection: Selection,
}

/// What kind of edit a step is, for typing to go on as one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Typing,
    Other,
}

/// Where each block was laid out on the last frame: its text's layout and the row it is drawn in.
#[derive(Clone, Default)]
pub(crate) struct Laid {
    pub text: Option<TextLayout>,
    pub row: Option<gpui::Bounds<Pixels>>,
}

/// How a drag extends the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Granularity {
    Char,
    Word,
    Block,
}

/// The rich-text editor (see the module docs).
pub struct Editor {
    pub(crate) focus: FocusHandle,
    doc: Doc,
    selection: Selection,
    /// An input method's text being composed, between two places.
    marked: Option<(Pos, Pos)>,
    /// The attributes the next text typed takes, after a format was turned on or off with nothing selected.
    pending: Option<Attrs>,
    /// Where Up and Down aim, across lines of other widths.
    goal_x: Option<Pixels>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_step: Option<(Step, Instant)>,
    placeholder: SharedString,
    enter: EnterMode,
    /// While the left button drags: how it extends, and the selection the press made.
    drag: Option<(Granularity, Selection)>,
    /// Each block's layout on the last frame.
    laid: Rc<RefCell<Vec<Laid>>>,
    /// The cursor is to be scrolled into view on the next frame.
    reveal: Rc<Cell<bool>>,
    scroll: ScrollHandle,
    /// The tallest the editor grows before it scrolls.
    max_height: Option<Pixels>,
    /// The least height it keeps.
    min_height: Option<Pixels>,
    pub(crate) read_only: bool,
    /// The most characters it holds (a line break is one), as a text field's `maxLength`.
    max_chars: Option<usize>,
    /// The pictures put in the box, by id ([`pictures`]), kept while it lives so undo brings one back.
    pictures: HashMap<u64, pictures::Picture>,
    next_picture: u64,
    takes_pictures: bool,
    _focus: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for Editor {}

impl Focusable for Editor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Editor {
    /// An empty editor; its keys bound once per app.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        keys::bind(cx);
        // a stop of Tab's order, so Shift+Tab from the controls after it comes back to it
        let focus = cx.focus_handle().tab_stop(true);
        let subscriptions = vec![
            cx.on_focus(&focus, window, |_, _, cx| {
                cx.emit(EditorEvent::Focus);
                cx.notify();
            }),
            cx.on_blur(&focus, window, |this: &mut Self, _, cx| {
                this.drag = None;
                cx.emit(EditorEvent::Blur);
                cx.notify();
            }),
        ];
        Self {
            focus,
            doc: Doc::default(),
            selection: Selection::default(),
            marked: None,
            pending: None,
            goal_x: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_step: None,
            placeholder: SharedString::default(),
            enter: EnterMode::default(),
            drag: None,
            laid: Rc::default(),
            reveal: Rc::default(),
            scroll: ScrollHandle::new(),
            max_height: None,
            min_height: None,
            read_only: false,
            max_chars: None,
            pictures: HashMap::new(),
            next_picture: 0,
            takes_pictures: false,
            _focus: subscriptions,
        }
    }

    /// It, showing `placeholder` while it is empty.
    #[must_use]
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Shows `placeholder` while it is empty from now on.
    pub fn set_placeholder(&mut self, placeholder: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// It, holding at most `max` characters: of what is typed or pasted, only what fits goes in, and any other edit
    /// past `max` is refused.
    #[must_use]
    pub fn max_chars(mut self, max: usize) -> Self {
        self.max_chars = Some(max);
        self
    }

    /// Holds at most `max` characters from now on (see [`Self::max_chars`]); `None`: no limit.
    pub fn set_max_chars(&mut self, max: Option<usize>) {
        self.max_chars = max;
    }

    /// Sets what Enter does.
    pub fn set_enter_mode(&mut self, mode: EnterMode, cx: &mut Context<Self>) {
        self.enter = mode;
        cx.notify();
    }

    #[must_use]
    pub fn enter_mode(&self) -> EnterMode {
        self.enter
    }

    /// Sets the least and the most height it is drawn at (it scrolls past the most).
    pub fn set_heights(&mut self, min: Option<Pixels>, max: Option<Pixels>, cx: &mut Context<Self>) {
        self.min_height = min;
        self.max_height = max;
        cx.notify();
    }

    /// Makes it take no edit (it still selects and copies).
    pub fn set_read_only(&mut self, read_only: bool, cx: &mut Context<Self>) {
        self.read_only = read_only;
        cx.notify();
    }

    /// Gives it the focus.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    #[must_use]
    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    // --- what it holds ---

    /// The document.
    #[must_use]
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// The selection.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Its text, a line per block.
    #[must_use]
    pub fn text(&self) -> String {
        self.doc.plain_text()
    }

    /// Whether it shows nothing (whitespace only, no mention).
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.doc.is_blank()
    }

    /// Whether it holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.doc.blocks.len() == 1 && self.doc.blocks[0].is_empty() && self.doc.blocks[0].kind == Kind::Paragraph
    }

    /// Replaces what it holds with `doc`, the cursor at its end; no undo step, no [`EditorEvent::Changed`] (a load,
    /// not an edit).
    pub fn set_doc(&mut self, doc: Doc, cx: &mut Context<Self>) {
        self.doc = doc;
        let end = self.doc.end();
        self.selection = Selection::caret(end);
        self.reset_transient();
        self.undo.clear();
        self.redo.clear();
        self.last_step = None;
        self.reveal.set(true);
        cx.notify();
    }

    /// Loads `nodes` (a text kept as it was until edited, an event's body: [`convert::Load::Body`]), their mentions'
    /// MRIs by `itemid` in `mentioned`.
    ///
    /// # Errors
    /// [`CantLoad::Image`]: a picture, which the editor can't send back.
    pub fn load<S: BuildHasher>(
        &mut self,
        nodes: &[Node],
        mentioned: &HashMap<String, String, S>,
        cx: &mut Context<Self>,
    ) -> Result<(), CantLoad> {
        let doc = convert::from_nodes(nodes, mentioned, convert::Load::Body)?;
        self.set_doc(doc, cx);
        Ok(())
    }

    /// Replaces what it holds with `doc` as one edit (it can be undone), the cursor at its end.
    pub fn replace_doc(&mut self, doc: Doc, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, move |current, _| {
            *current = doc;
            Selection::caret(current.end())
        });
    }

    /// Empties it, as an edit (it can be undone).
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.is_empty() {
            return;
        }
        self.edit(Step::Other, cx, |doc, _| {
            *doc = Doc::default();
            Selection::default()
        });
    }

    fn reset_transient(&mut self) {
        self.marked = None;
        self.pending = None;
        self.goal_x = None;
        self.drag = None;
    }

    // --- the cursor's block, for a mention typed after `@` ---

    /// The text of the cursor's block, the cursor's offset in it and the mentions' ranges in it.
    #[must_use]
    pub fn cursor_block(&self) -> (String, usize, Vec<Range<usize>>) {
        let block = &self.doc.blocks[self.selection.head.block];
        (block.text(), self.selection.head.offset, block.mention_ranges())
    }

    /// Puts a mention of `mri`, shown as `name`, in place of `range` of the cursor's block, and a space after it.
    pub fn insert_mention(&mut self, range: Range<usize>, mri: &str, name: &str, cx: &mut Context<Self>) {
        let block = self.selection.head.block;
        let mri = mri.to_owned();
        let name = name.to_owned();
        self.edit(Step::Other, cx, move |doc, _| {
            let host = &mut doc.blocks[block];
            let start = host.snap(range.start, false);
            let attrs = host.attrs_at(start);
            host.remove(start..range.end.max(start));
            let mention = Attrs {
                mention: Some(mri),
                link: None,
                ..attrs.clone()
            };
            let end = host.insert_runs(start, vec![Run::new(name, mention), Run::new(" ", attrs)]);
            Selection::caret(Pos::new(block, end))
        });
    }

    /// Types `text` at the cursor, in place of the selection (an emoji picked, say).
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        self.type_text(text, cx);
    }

    // --- editing ---

    /// Makes an edit: `change` changes the document and gives the selection after it. One undo step, but typing
    /// within [`TYPING_STEP`] of the last typing goes on as the same step.
    fn edit(&mut self, step: Step, cx: &mut Context<Self>, change: impl FnOnce(&mut Doc, Selection) -> Selection) {
        if self.read_only {
            return;
        }
        let limit = self.max_chars.map(|max| (max, self.doc.clone(), self.selection));
        let now = Instant::now();
        let continues = step == Step::Typing
            && self
                .last_step
                .is_some_and(|(last, at)| last == Step::Typing && now.duration_since(at) < TYPING_STEP);
        if !continues {
            self.undo.push(Snapshot {
                doc: self.doc.clone(),
                selection: self.selection,
            });
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
        }
        let last_step = self.last_step;
        let redo = std::mem::take(&mut self.redo);
        self.last_step = Some((step, now));
        let selection = change(&mut self.doc, self.selection);
        if let Some((max, doc, before)) = limit
            && chars(&self.doc) > max.max(chars(&doc))
        {
            self.doc = doc;
            self.selection = before;
            self.last_step = last_step;
            self.redo = redo;
            if !continues {
                self.undo.pop();
            }
            return;
        }
        self.redo.clear();
        self.selection = Selection {
            anchor: self.doc.clamp(selection.anchor, false),
            head: self.doc.clamp(selection.head, false),
        };
        self.goal_x = None;
        self.reveal.set(true);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    /// Moves the selection to `selection` (no change of text).
    fn select(&mut self, selection: Selection, cx: &mut Context<Self>) {
        let selection = Selection {
            anchor: self.doc.clamp(selection.anchor, false),
            head: self.doc.clamp(selection.head, true),
        };
        if selection != self.selection {
            self.pending = None;
            self.last_step = None;
            self.selection = selection;
            self.reveal.set(true);
            cx.emit(EditorEvent::Moved);
        }
        cx.notify();
    }

    /// Moves the cursor to `head`, the selection's other end staying with `extend`.
    fn move_to(&mut self, head: Pos, extend: bool, cx: &mut Context<Self>) {
        let anchor = if extend { self.selection.anchor } else { head };
        self.select(Selection { anchor, head }, cx);
    }

    /// Types `text` in place of the selection, with the pending attributes or those of the text before the cursor.
    fn type_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        if text.contains('\n') {
            return self.paste_plain(text, cx);
        }
        let (start, end) = self.selection.range();
        let text = self.fit(start, end, text);
        if text.is_empty() {
            return;
        }
        let pending = self.pending.take();
        self.edit(Step::Typing, cx, move |doc, selection| {
            let (start, end) = selection.range();
            let at = doc.delete(start, end);
            let attrs = pending.unwrap_or_else(|| doc.blocks[at.block].attrs_at(at.offset));
            Selection::caret(doc.insert_text(at, &text, attrs))
        });
    }

    /// Of `text` put between `start` and `end`, what fits in [`Self::max_chars`].
    fn fit(&self, start: Pos, end: Pos, text: &str) -> String {
        let Some(max) = self.max_chars else {
            return text.to_owned();
        };
        let kept = chars(&self.doc) - self.doc.text_between(start, end).chars().count();
        text.chars().take(max.saturating_sub(kept)).collect()
    }

    /// Deletes the selection; whether there was one.
    fn delete_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selection.is_empty() {
            return false;
        }
        self.edit(Step::Other, cx, |doc, selection| {
            let (start, end) = selection.range();
            Selection::caret(doc.delete(start, end))
        });
        true
    }

    /// Backspace: the selection, else the character (or mention) before the cursor; at a block's start a list item,
    /// a quote line, a heading or a block of code becomes a paragraph (a nested item moves out a level), an indented
    /// paragraph moves back a step, and a paragraph joins the block before it.
    fn backspace(&mut self, word: bool, cx: &mut Context<Self>) {
        if self.delete_selection(cx) {
            return;
        }
        let head = self.selection.head;
        if head.offset > 0 {
            let from = if word {
                self.doc.word_before(head)
            } else {
                self.doc.before(head).unwrap_or(head)
            };
            return self.edit(Step::Typing, cx, move |doc, _| Selection::caret(doc.delete(from, head)));
        }
        let block = &self.doc.blocks[head.block];
        match (block.kind, block.indent) {
            (kind, indent) if kind.is_list() && indent > 0 => self.set_indent_of(head.block, indent - 1, cx),
            (Kind::Paragraph, indent) if indent > 0 => self.set_indent_of(head.block, indent - 1, cx),
            (Kind::Bullet | Kind::Numbered | Kind::Quote | Kind::Heading(_) | Kind::Code, _) => {
                self.edit(Step::Other, cx, move |doc, selection| {
                    doc.blocks[head.block].kind = Kind::Paragraph;
                    doc.blocks[head.block].indent = 0;
                    selection
                });
            }
            _ if head.block == 0 => {}
            _ => self.edit(Step::Other, cx, move |doc, _| {
                let before = head.block - 1;
                if doc.blocks[before].kind.is_atom() {
                    doc.blocks.remove(before);
                    return Selection::caret(Pos::new(before, 0));
                }
                if doc.blocks[head.block].kind.is_atom() {
                    doc.blocks.remove(head.block);
                    return Selection::caret(Pos::new(before, doc.blocks[before].len()));
                }
                let end = Pos::new(before, doc.blocks[before].len());
                Selection::caret(doc.delete(end, head))
            }),
        }
    }

    /// Delete: the selection, else the character (or mention) after the cursor; at a block's end the next block
    /// joins it (a rule after it goes).
    fn delete_forward(&mut self, word: bool, cx: &mut Context<Self>) {
        if self.delete_selection(cx) {
            return;
        }
        let head = self.selection.head;
        let to = if word {
            self.doc.word_after(head)
        } else {
            match self.doc.after(head) {
                Some(to) => to,
                None => return,
            }
        };
        self.edit(Step::Other, cx, move |doc, _| {
            if to.block > head.block && doc.blocks[to.block].kind.is_atom() {
                doc.blocks.remove(to.block);
                return Selection::caret(head);
            }
            Selection::caret(doc.delete(head, to))
        });
    }

    /// Enter (see the module docs): `shift` with Shift held, `send` with Ctrl.
    pub fn enter(&mut self, shift: bool, send: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if send {
            cx.emit(EditorEvent::Submit);
            return;
        }
        let head = self.selection.head;
        let block = &self.doc.blocks[head.block];
        let kind = block.kind;
        let empty = block.is_empty();
        let at_end = head.offset == block.len() && self.selection.is_empty();
        let ends_in_break = block.text().ends_with('\n');
        match kind {
            Kind::Code if !shift && at_end && (empty || ends_in_break) => {
                // Enter on the empty line a block of code ends in leaves it
                self.edit(Step::Other, cx, move |doc, _| {
                    let code = &mut doc.blocks[head.block];
                    let len = code.len();
                    code.remove(len.saturating_sub(1)..len);
                    if code.is_empty() {
                        code.kind = Kind::Paragraph;
                        return Selection::caret(Pos::new(head.block, 0));
                    }
                    doc.blocks.insert(head.block + 1, Block::default());
                    Selection::caret(Pos::new(head.block + 1, 0))
                });
            }
            Kind::Code => self.line_break(cx),
            kind if kind.is_list() && shift => self.line_break(cx),
            kind if (kind.is_list() || kind == Kind::Quote) && empty => {
                let indent = self.doc.blocks[head.block].indent;
                if kind.is_list() && indent > 0 {
                    self.set_indent_of(head.block, indent - 1, cx);
                } else {
                    self.edit(Step::Other, cx, move |doc, selection| {
                        doc.blocks[head.block].kind = Kind::Paragraph;
                        doc.blocks[head.block].indent = 0;
                        selection
                    });
                }
            }
            kind if kind.is_list() || kind == Kind::Quote => self.split(cx),
            _ => match (self.enter, shift) {
                (EnterMode::Send, false) => cx.emit(EditorEvent::Submit),
                (EnterMode::Send, true) | (EnterMode::NewLine, false) => self.split(cx),
                (EnterMode::NewLine, true) => self.line_break(cx),
            },
        }
    }

    /// Cuts the block at the cursor (the selection deleted first).
    fn split(&mut self, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, |doc, selection| {
            let (start, end) = selection.range();
            let at = doc.delete(start, end);
            Selection::caret(doc.split(at))
        });
    }

    /// Breaks the line inside the block (`<br>`).
    fn line_break(&mut self, cx: &mut Context<Self>) {
        let pending = self.pending.clone();
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = selection.range();
            let at = doc.delete(start, end);
            let attrs = pending.unwrap_or_else(|| doc.blocks[at.block].attrs_at(at.offset));
            Selection::caret(doc.insert_text(at, "\n", attrs))
        });
    }

    // --- formats ---

    /// The attributes at the cursor: the pending ones, else those of the selection's first character, else of the
    /// text before the cursor.
    #[must_use]
    pub fn current_attrs(&self) -> Attrs {
        if let Some(pending) = &self.pending {
            return pending.clone();
        }
        let (start, end) = self.selection.range();
        if start != end {
            let first = self.doc.after(start).unwrap_or(end);
            let block = &self.doc.blocks[start.block];
            if first.block == start.block {
                return block
                    .ranges()
                    .find(|(range, _)| range.start <= start.offset && start.offset < range.end)
                    .map(|(_, run)| run.attrs.continued())
                    .unwrap_or_default();
            }
        }
        self.doc.blocks[start.block].attrs_at(start.offset)
    }

    /// Whether `mark` is on where the cursor is: on all the text selected, or for the text typed next.
    #[must_use]
    pub fn has_mark(&self, mark: Marks) -> bool {
        let (start, end) = self.selection.range();
        if start == end || self.pending.is_some() {
            return self.current_attrs().marks.contains(mark);
        }
        self.doc
            .all(start, end, &|attrs| attrs.marks.contains(mark))
            .unwrap_or(false)
    }

    /// The kind of the cursor's block.
    #[must_use]
    pub fn block_kind(&self) -> Kind {
        self.doc.blocks[self.selection.head.block].kind
    }

    /// Changes the attributes of the selection with `change`, or of the text typed next when nothing is selected.
    fn format(&mut self, cx: &mut Context<Self>, change: impl Fn(&mut Attrs) + 'static) {
        if self.selection.is_empty() {
            let mut attrs = self.current_attrs();
            change(&mut attrs);
            self.pending = Some(attrs);
            cx.emit(EditorEvent::Moved);
            cx.notify();
            return;
        }
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = selection.range();
            doc.apply(start, end, &change);
            selection
        });
    }

    /// Turns `mark` on for the selection, or off when all of it has it.
    pub fn toggle_mark(&mut self, mark: Marks, cx: &mut Context<Self>) {
        let on = !self.has_mark(mark);
        self.format(cx, move |attrs| {
            attrs.marks = if on {
                attrs.marks.with(mark)
            } else {
                attrs.marks.without(mark)
            };
        });
    }

    /// Sets the font colour (`None`: automatic).
    pub fn set_color(&mut self, color: Option<Rgb>, cx: &mut Context<Self>) {
        self.format(cx, move |attrs| attrs.span.color = color);
    }

    /// Sets the highlight (`None`: none).
    pub fn set_highlight(&mut self, color: Option<Rgb>, cx: &mut Context<Self>) {
        self.format(cx, move |attrs| attrs.span.highlight = color);
    }

    /// Sets the size (`None`: medium).
    pub fn set_size(&mut self, size: Option<FontSize>, cx: &mut Context<Self>) {
        self.format(cx, move |attrs| attrs.span.size = size);
    }

    /// Takes the marks, colours and size off the selection (links and mentions stay).
    pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
        self.format(cx, |attrs| {
            attrs.marks = Marks::NONE;
            attrs.span = teams_proto::chat::SpanStyle::default();
        });
    }

    /// Makes the selected blocks `kind`, or paragraphs when all of them are already (a list's or a quote's toggle).
    pub fn toggle_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let (start, end) = self.selection.range();
        let range = self.doc.blocks_of(start, end);
        let all = self.doc.blocks[range.clone()].iter().all(|block| block.kind == kind);
        self.set_kind(if all { Kind::Paragraph } else { kind }, cx);
    }

    /// Makes the selected blocks `kind` (a rule stays a rule). A block of code takes the selected blocks' text as its
    /// lines; a block of code made something else gives a block per line.
    pub fn set_kind(&mut self, kind: Kind, cx: &mut Context<Self>) {
        if kind == Kind::Code {
            return self.make_code(cx);
        }
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = selection.range();
            let range = doc.blocks_of(start, end);
            set_kinds(doc, range, kind);
            Selection {
                anchor: doc.clamp(selection.anchor, false),
                head: doc.clamp(selection.head, false),
            }
        });
    }

    /// The selected blocks as one block of code, their text its lines; or, when the cursor is already in one, its
    /// lines as paragraphs.
    fn make_code(&mut self, cx: &mut Context<Self>) {
        if self.block_kind() == Kind::Code {
            return self.set_kind(Kind::Paragraph, cx);
        }
        self.edit(Step::Other, cx, |doc, selection| {
            let (start, end) = selection.range();
            let range = doc.blocks_of(start, end);
            let text = doc.blocks[range.clone()]
                .iter()
                .filter(|block| !block.kind.is_atom())
                .map(Block::text)
                .collect::<Vec<_>>()
                .join("\n");
            let len = text.len();
            let code = Block::new(Kind::Code, 0, vec![Run::new(text, Attrs::default())]);
            let at = range.start;
            doc.blocks.splice(range, [code]);
            Selection::caret(Pos::new(at, len))
        });
    }

    /// The inline-code button: a selection within one line becomes inline code (or stops being it); anything else
    /// a block of code.
    pub fn code(&mut self, cx: &mut Context<Self>) {
        let (start, end) = self.selection.range();
        let one_line = start.block == end.block && !self.doc.text_between(start, end).contains('\n');
        if start != end && one_line && self.block_kind() != Kind::Code {
            self.toggle_mark(Marks::CODE, cx);
        } else {
            self.make_code(cx);
        }
    }

    fn set_indent_of(&mut self, block: usize, indent: u8, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, move |doc, selection| {
            doc.blocks[block].indent = indent;
            selection
        });
    }

    /// Indents the selected blocks a step (`deeper`) or un-indents them: a list item nests or un-nests (out of the
    /// outermost list it becomes a paragraph), a paragraph moves a step.
    pub fn indent(&mut self, deeper: bool, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = selection.range();
            for index in doc.blocks_of(start, end) {
                let block = &mut doc.blocks[index];
                if !matches!(block.kind, Kind::Paragraph | Kind::Bullet | Kind::Numbered) {
                    continue;
                }
                if deeper {
                    block.indent = (block.indent + 1).min(MAX_INDENT);
                } else if block.indent > 0 {
                    block.indent -= 1;
                } else if block.kind.is_list() {
                    block.kind = Kind::Paragraph;
                }
            }
            selection
        });
    }

    /// Puts a horizontal rule at the cursor.
    pub fn insert_rule(&mut self, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, |doc, selection| {
            let (start, end) = selection.range();
            let at = doc.delete(start, end);
            Selection::caret(doc.insert_fragment(at, vec![Block::new(Kind::Rule, 0, Vec::new())]))
        });
    }

    /// The link at the cursor: its text, its address and its range in the cursor's block.
    #[must_use]
    pub fn link_at_cursor(&self) -> Option<(String, String, Range<usize>)> {
        let head = self.selection.head;
        let block = &self.doc.blocks[head.block];
        let ranges: Vec<(Range<usize>, &Run)> = block.ranges().collect();
        let (index, href) = ranges.iter().enumerate().find_map(|(index, (range, run))| {
            let href = run.attrs.link.as_ref()?;
            (range.start <= head.offset && head.offset <= range.end).then(|| (index, href.clone()))
        })?;
        let mut start = ranges[index].0.start;
        let mut end = ranges[index].0.end;
        for (range, run) in ranges[..index].iter().rev() {
            if run.attrs.link.as_ref() != Some(&href) {
                break;
            }
            start = range.start;
        }
        for (range, run) in &ranges[index + 1..] {
            if run.attrs.link.as_ref() != Some(&href) {
                break;
            }
            end = range.end;
        }
        let text = block.text()[start..end].to_owned();
        Some((text, href, start..end))
    }

    /// The selected text (within one block), for the link dialog to show.
    #[must_use]
    pub fn selected_text(&self) -> String {
        let (start, end) = self.selection.range();
        self.doc.text_between(start, end)
    }

    /// Makes `text` a link to `href`: the link at the cursor changed, the selection made one, or `text` typed as one.
    pub fn insert_link(&mut self, text: &str, href: &str, cx: &mut Context<Self>) {
        let text = if text.trim().is_empty() { href } else { text }.to_owned();
        let href = href.to_owned();
        let existing = self.link_at_cursor().map(|(_, _, range)| range);
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = match existing {
                Some(range) if selection.is_empty() => (
                    Pos::new(selection.head.block, range.start),
                    Pos::new(selection.head.block, range.end),
                ),
                _ => selection.range(),
            };
            let at = doc.delete(start, end);
            let attrs = Attrs {
                link: Some(href),
                mention: None,
                ..doc.blocks[at.block].attrs_at(at.offset)
            };
            let end = doc.insert_text(at, &text, attrs);
            Selection::caret(end)
        });
    }

    /// Takes the link off the link at the cursor (its text stays).
    pub fn remove_link(&mut self, cx: &mut Context<Self>) {
        let Some((_, _, range)) = self.link_at_cursor() else {
            return;
        };
        self.edit(Step::Other, cx, move |doc, selection| {
            doc.blocks[selection.head.block].apply(range, &|attrs: &mut Attrs| attrs.link = None);
            selection
        });
    }

    // --- undo ---

    /// Undoes the last step.
    pub fn undo(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let Some(snapshot) = self.undo.pop() else { return };
        self.redo.push(Snapshot {
            doc: std::mem::replace(&mut self.doc, snapshot.doc),
            selection: self.selection,
        });
        self.restored(snapshot.selection, cx);
    }

    /// Does again the step undone last.
    pub fn redo(&mut self, cx: &mut Context<Self>) {
        if self.read_only {
            return;
        }
        let Some(snapshot) = self.redo.pop() else { return };
        self.undo.push(Snapshot {
            doc: std::mem::replace(&mut self.doc, snapshot.doc),
            selection: self.selection,
        });
        self.restored(snapshot.selection, cx);
    }

    fn restored(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.selection = Selection {
            anchor: self.doc.clamp(selection.anchor, false),
            head: self.doc.clamp(selection.head, false),
        };
        self.reset_transient();
        self.last_step = None;
        self.reveal.set(true);
        cx.emit(EditorEvent::Changed);
        cx.notify();
    }

    // --- clipboard ---

    /// Copies the selection: its plain text, and its rich text beside it for a paste into an editor.
    pub fn copy(&self, cx: &mut App) -> bool {
        let (start, end) = self.selection.range();
        if start == end {
            return false;
        }
        let fragment = Doc::new(self.doc.fragment(start, end));
        let (nodes, mentioned) = convert::to_nodes(&fragment);
        let text = self.doc.text_between(start, end);
        let metadata = input::clipboard_metadata(&nodes, &mentioned);
        cx.write_to_clipboard(ClipboardItem::new_string_with_metadata(text, metadata));
        true
    }

    /// Cuts the selection.
    pub fn cut(&mut self, cx: &mut Context<Self>) {
        if self.copy(cx) {
            self.delete_selection(cx);
        }
    }

    /// Pastes what the clipboard holds: an editor's rich text with its formats, else its text. Into a block of code
    /// it is plain text, as Teams pastes there; and rich text that shows nothing (a paragraph break copied alone) gives
    /// way to the text beside it.
    ///
    /// Pictures and files are taken first ([`pictures`]) unless `plain`; `scale` is the screen's, which a picture's
    /// shown size is its pixels' over.
    pub fn paste(&mut self, plain: bool, scale: f32, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else { return };
        if !plain && self.paste_pictures(&item, scale, cx) {
            return;
        }
        let text = item.text();
        // the metadata of the text the item holds, beside a picture or files too
        let metadata = item.entries().iter().find_map(|entry| match entry {
            gpui::ClipboardEntry::String(text) => text.metadata.as_deref(),
            _ => None,
        });
        let rich = (!plain).then(|| metadata.and_then(input::doc_of_metadata)).flatten();
        let in_code = self.block_kind() == Kind::Code;
        match rich {
            Some(doc) if !in_code && (!doc.is_blank() || text.is_none()) => self.paste_doc(doc, cx),
            rich => {
                if let Some(text) = text.or_else(|| rich.map(|doc| doc.plain_text())) {
                    self.paste_plain(&text, cx);
                }
            }
        }
    }

    /// Pastes `text`: in a block of code its lines stay in the block; in a list item each line is an item of its
    /// list; elsewhere a paragraph per line.
    pub fn paste_plain(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let (start, end) = self.selection.range();
        let text = self.fit(start, end, &text);
        let kind = self.block_kind();
        if kind == Kind::Code || !text.contains('\n') {
            let attrs = self.pending.take();
            return self.edit(Step::Other, cx, move |doc, selection| {
                let (start, end) = selection.range();
                let at = doc.delete(start, end);
                let attrs = attrs.unwrap_or_else(|| doc.blocks[at.block].attrs_at(at.offset));
                Selection::caret(doc.insert_text(at, &text, attrs))
            });
        }
        let (kind, indent) = if kind.is_list() {
            (kind, self.doc.blocks[self.selection.head.block].indent)
        } else {
            (Kind::Paragraph, 0)
        };
        let blocks: Vec<Block> = text
            .split('\n')
            .map(|line| Block::new(kind, indent, vec![Run::new(line, Attrs::default())]))
            .collect();
        self.paste_doc(Doc::new(blocks), cx);
    }

    /// Pastes `doc` at the cursor, in place of the selection.
    pub fn paste_doc(&mut self, doc: Doc, cx: &mut Context<Self>) {
        self.edit(Step::Other, cx, move |into, selection| {
            let (start, end) = selection.range();
            let at = into.delete(start, end);
            Selection::caret(into.insert_fragment(at, doc.blocks))
        });
    }
}

/// Makes `doc`'s blocks in `range` of `kind`: a block of code becomes a block per line, a rule stays, and the indent
/// is kept only from list to list.
fn set_kinds(doc: &mut Doc, range: Range<usize>, kind: Kind) {
    let mut index = range.start;
    let mut last = range.end;
    while index < last {
        let block = &mut doc.blocks[index];
        if block.kind == Kind::Code && kind != Kind::Code {
            let lines: Vec<Block> = block
                .text()
                .split('\n')
                .map(|line| Block::new(kind, 0, vec![Run::new(line, Attrs::default())]))
                .collect();
            let count = lines.len();
            doc.blocks.splice(index..=index, lines);
            index += count;
            last += count - 1;
            continue;
        }
        if !block.kind.is_atom() {
            if !(kind.is_list() && block.kind.is_list()) {
                block.indent = 0;
            }
            block.kind = kind;
        }
        index += 1;
    }
}

/// How many characters `doc` holds, a line break between two blocks as one.
fn chars(doc: &Doc) -> usize {
    doc.plain_text().chars().count()
}
