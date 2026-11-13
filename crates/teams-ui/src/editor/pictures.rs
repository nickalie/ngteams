//! Pictures in the editor, as Teams' message box takes them: a paste (Ctrl+V, the context menu's Paste) of a picture on
//! the clipboard (a screenshot, a browser's "Copy image") or of picture files a file manager copied puts each picture
//! in the box where the cursor is, a block of its own ([`Kind::Picture`]), drawn as a thumbnail with a Remove button.
//!
//! - **What a paste takes** ([`Pasted::of`]): the editor's own rich text first (a copy within the app); then the files a
//!   file manager copied (`text/uri-list`), their pictures put in the box and any other file handed to the owner
//!   ([`EditorEvent::Files`]) to attach; then a picture, unless text of its own comes with it (a spreadsheet's cells
//!   offer their picture beside their text, and their text is pasted); then text, as before. Ctrl+Shift+V pastes the
//!   text alone. An editor that takes no pictures ([`Editor::set_takes_pictures`], the event form's) pastes text only.
//! - **Sending** is its owner's: each picture put in the box is told ([`EditorEvent::Picture`]) and its owner sends it
//!   (`crate::compose`: the media service, readable by the chat alone) and says how that went
//!   ([`Editor::set_picture_state`]). The message names a sent picture ([`Editor::picture_node`]); a box whose pictures
//!   aren't all sent waits ([`Editor::pictures_ready`]).
//! - A picture's bytes stay in the editor while it lives, so undo brings a removed picture back as it was.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, ClipboardEntry, ClipboardItem, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    RenderImage, SharedString, Styled as _, div, img, px, rgb,
};
use gpui_component::Sizable as _;
use teams_proto::{PastedFormat, PastedPicture};

use super::doc::{Block, Kind, Selection};
use super::{Editor, EditorEvent, Step};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::theme::Palette;

/// The largest picture file a paste reads, in bytes.
const MAX_FILE: u64 = teams_proto::MAX_SENT_PICTURE as u64;

/// How a picture's send stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PictureState {
    /// Being sent to the media service.
    Sending,
    /// Sent: the media service's object it is in.
    Sent(String),
    /// Not sent, and why; Retry sends it again.
    Failed(SharedString),
}

/// A picture in the editor: its bytes, as drawn, and how its send stands.
#[derive(Clone)]
pub struct Picture {
    pub bytes: Arc<PastedPicture>,
    pub image: Arc<RenderImage>,
    /// Its size in pixels.
    pub own: (u32, u32),
    /// The size it is shown at, in CSS pixels: its pixels' size over the screen's scale, as a browser gives a pasted
    /// picture (a dense screen's screenshot half its pixels' size).
    pub given: (u32, u32),
    pub state: PictureState,
}

impl std::fmt::Debug for Picture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Picture")
            .field("bytes", &self.bytes)
            .field("own", &self.own)
            .field("given", &self.given)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl Picture {
    /// The picture `bytes` hold (a PNG, JPEG, GIF or WebP picture, by their signature) on a screen of `scale`; `None`
    /// when they hold none, it doesn't decode, or it exceeds [`crate::inline::decode`]'s source limits.
    #[must_use]
    pub fn of(bytes: Vec<u8>, scale: f32) -> Option<Self> {
        let format = PastedFormat::sniff(&bytes)?;
        let bytes = Arc::new(PastedPicture { format, bytes });
        let (image, own) = crate::inline::decode(&bytes)?;
        let scale = if scale.is_finite() && scale >= 1.0 { scale } else { 1.0 };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "a picture's side is at most 16384 pixels, and positive"
        )]
        let given = (
            ((own.0 as f32 / scale).round() as u32).max(1),
            ((own.1 as f32 / scale).round() as u32).max(1),
        );
        Some(Self {
            bytes,
            image: Arc::new(image),
            own,
            given,
            state: PictureState::Sending,
        })
    }

    /// The picture a local file holds, read when it is a file of at most [`MAX_FILE`] bytes.
    #[must_use]
    pub fn of_file(path: &Path, scale: f32) -> Option<Self> {
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > MAX_FILE {
            return None;
        }
        Self::of(std::fs::read(path).ok()?, scale)
    }
}

/// What a paste takes from the clipboard (see the module docs).
#[derive(Debug, Default)]
pub struct Pasted {
    /// Pictures to put in the box, in order.
    pub pictures: Vec<Picture>,
    /// Files that aren't pictures, for the owner to attach.
    pub files: Vec<PathBuf>,
}

impl Pasted {
    /// The pictures and files a paste of `item` takes; `None` when it pastes text (or nothing).
    #[must_use]
    pub fn of(item: &ClipboardItem, scale: f32) -> Option<Self> {
        let mut string = None;
        let mut image = None;
        let mut paths = None;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::String(text) => string = string.or(Some(text)),
                ClipboardEntry::Image(picture) => image = image.or(Some(picture)),
                ClipboardEntry::ExternalPaths(files) => paths = paths.or(Some(files)),
            }
        }
        if string
            .and_then(|text| text.metadata.as_deref())
            .is_some_and(super::input::is_own_rich_text)
        {
            return None;
        }
        if let Some(paths) = paths {
            let mut pasted = Self::default();
            for path in paths.paths() {
                match Picture::of_file(path, scale) {
                    Some(picture) => pasted.pictures.push(picture),
                    None => pasted.files.push(path.clone()),
                }
            }
            return Some(pasted);
        }
        let image = image?;
        // text of its own beside the picture (cells, a document's selection) is what is pasted
        if string.is_some_and(|text| has_own_text(&text.text)) {
            return None;
        }
        Some(Self {
            pictures: vec![Picture::of(image.bytes.clone(), scale)?],
            files: Vec::new(),
        })
    }
}

/// Whether text offered beside a picture says more than the picture: neither blank nor just an address (a browser's
/// "Copy image" offers the picture with its HTML, and some offer its address as text).
fn has_own_text(text: &str) -> bool {
    let text = text.trim();
    let address = ["http://", "https://", "file://", "data:"]
        .iter()
        .any(|scheme| text.starts_with(scheme));
    !text.is_empty() && (text.contains(char::is_whitespace) || !address)
}

impl Editor {
    /// Whether a paste may put pictures in the box (see the module docs); off until its owner can send them.
    pub fn set_takes_pictures(&mut self, takes: bool) {
        self.takes_pictures = takes;
    }

    #[must_use]
    pub fn takes_pictures(&self) -> bool {
        self.takes_pictures
    }

    /// Pastes the clipboard's pictures and files, if it pastes those (see the module docs); whether it did.
    pub(super) fn paste_pictures(&mut self, item: &ClipboardItem, scale: f32, cx: &mut Context<Self>) -> bool {
        if !self.takes_pictures || self.block_kind() == Kind::Code {
            return false;
        }
        let Some(pasted) = Pasted::of(item, scale) else {
            return false;
        };
        for picture in pasted.pictures {
            self.insert_picture(picture, cx);
        }
        if !pasted.files.is_empty() {
            cx.emit(EditorEvent::Files(pasted.files));
        }
        true
    }

    /// Puts `picture` in the box at the cursor, in place of the selection, and tells the owner to send it; its id.
    pub fn insert_picture(&mut self, mut picture: Picture, cx: &mut Context<Self>) -> u64 {
        self.next_picture += 1;
        let id = self.next_picture;
        picture.state = PictureState::Sending;
        self.pictures.insert(id, picture);
        self.edit(Step::Other, cx, move |doc, selection| {
            let (start, end) = selection.range();
            let at = doc.delete(start, end);
            Selection::caret(doc.insert_fragment(at, vec![Block::new(Kind::Picture(id), 0, Vec::new())]))
        });
        cx.emit(EditorEvent::Picture(id));
        id
    }

    /// Picture `id`, while the editor keeps it.
    #[must_use]
    pub fn picture(&self, id: u64) -> Option<&Picture> {
        self.pictures.get(&id)
    }

    /// Says how picture `id`'s send went.
    pub fn set_picture_state(&mut self, id: u64, state: PictureState, cx: &mut Context<Self>) {
        if let Some(picture) = self.pictures.get_mut(&id) {
            picture.state = state;
            cx.notify();
        }
    }

    /// The pictures the box shows, in order.
    #[must_use]
    pub fn shown_pictures(&self) -> Vec<u64> {
        self.doc
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                Kind::Picture(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// Whether every picture the box shows is sent.
    #[must_use]
    pub fn pictures_ready(&self) -> bool {
        self.shown_pictures().into_iter().all(|id| {
            self.pictures
                .get(&id)
                .is_some_and(|picture| matches!(picture.state, PictureState::Sent(_)))
        })
    }

    /// The message's picture for picture `id`, once it is sent.
    #[must_use]
    pub fn picture_node(&self, id: u64) -> Option<teams_proto::chat::Image> {
        let picture = self.pictures.get(&id)?;
        match &picture.state {
            PictureState::Sent(object) => teams_proto::sent_picture(object, picture.given),
            _ => None,
        }
    }

    /// Takes picture `id` out of the box (an edit: undo brings it back).
    pub fn remove_picture(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(index) = self.doc.blocks.iter().position(|block| block.kind == Kind::Picture(id)) else {
            return;
        };
        self.edit(Step::Other, cx, move |doc, _| {
            doc.blocks.remove(index);
            if doc.blocks.is_empty() {
                doc.blocks.push(Block::default());
            }
            let at = index.min(doc.blocks.len() - 1);
            Selection::caret(super::doc::Pos::new(at, 0))
        });
    }
}

/// A picture's frame's corners' radius in the box, as a message's picture's.
const FRAME_RADIUS: f32 = 6.0;

impl Editor {
    /// Block `index`'s row when it is picture `id`: its thumbnail as Teams' box shows a pasted picture (sized as a
    /// message's picture is, [`crate::inline::Shape`]), a Remove button over its corner, how its send stands, and the
    /// block's empty text after it (where the cursor stands beside the picture).
    pub(super) fn picture_row(
        &self,
        index: usize,
        id: u64,
        palette: Palette,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = self.block_text(index, palette, focused, cx);
        let frame = self.pictures.get(&id).map(|picture| {
            // as wide as the box was on the last frame lets it be, as a message's picture is in its bubble
            let room = Some(self.scroll.bounds().size.width).filter(|width| *width > px(0.0));
            Self::thumbnail(id, picture, room, palette, cx)
        });
        div()
            .w_full()
            .flex()
            .items_end()
            .py_1()
            .children(frame)
            .child(div().flex_1().min_w_0().child(text))
            .into_any_element()
    }

    fn thumbnail(
        id: u64,
        picture: &Picture,
        room: Option<gpui::Pixels>,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shape = crate::inline::Shape::of(picture.given, picture.own, room);
        let outer = shape.outer();
        let remove = selected_by(
            format!("editor-picture-remove-{id}"),
            icon_button(("editor-picture-remove", id), Glyph::X, "Remove picture")
                .xsmall()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.remove_picture(id, cx);
                    this.focus(window, cx);
                })),
        );
        let status = Self::picture_status(id, &picture.state, palette, cx);
        div()
            .debug_selector(move || format!("editor-picture-{id}"))
            .relative()
            .flex_none()
            .w(outer.width)
            .h(outer.height)
            .max_w_full()
            .border_1()
            .border_color(rgb(palette.border))
            .rounded(px(FRAME_RADIUS))
            .bg(rgb(palette.background))
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_center()
            .child(
                img(Arc::clone(&picture.image))
                    .w(shape.image.width)
                    .h(shape.image.height)
                    .flex_none(),
            )
            // their clicks theirs, not the editor's press-and-drag layer under the rows
            // on a chip of the box's surface, so it shows on any picture
            .child(
                div()
                    .absolute()
                    .top_1()
                    .right_1()
                    .rounded_md()
                    .bg(rgb(palette.surface))
                    .occlude()
                    .child(remove),
            )
            .children(status.map(|status| {
                div()
                    .absolute()
                    .left_1()
                    .right_1()
                    .bottom_1()
                    .flex()
                    .occlude()
                    .child(status)
            }))
            .into_any_element()
    }

    /// What a picture's corner says while it isn't sent: "Uploading…", or why it failed with Retry.
    fn picture_status(id: u64, state: &PictureState, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pill = || div().px_2().py_0p5().rounded_md().bg(rgb(palette.surface)).text_xs();
        match state {
            PictureState::Sent(_) => None,
            PictureState::Sending => Some(
                pill()
                    .text_color(rgb(palette.brand))
                    .child("Uploading…")
                    .into_any_element(),
            ),
            PictureState::Failed(why) => Some(
                pill()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(rgb(crate::theme::DANGER))
                    .child(div().flex_1().min_w_0().truncate().child(why.clone()))
                    .child(selected_by(
                        format!("editor-picture-retry-{id}"),
                        icon_button(("editor-picture-retry", id), Glyph::REFRESH, "Retry upload")
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_picture_state(id, PictureState::Sending, cx);
                                cx.emit(EditorEvent::Picture(id));
                            })),
                    ))
                    .into_any_element(),
            ),
        }
    }
}
