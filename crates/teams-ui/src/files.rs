//! The files a message carries, drawn as Teams draws them under the message's text: a grid of cards, two in a row at
//! most ([`COLUMNS`]), one in a row when the bubble is narrow. A file alone has a card as wide as the grid
//! ([`LONE_CARD_WIDTH`]), so its location shows in full. The grid stays in the message's bubble.
//!
//! - **A card** ([`FileCard`]): a header with the file type's icon (Teams' own for a video, a play mark in a white
//!   box, and for a zip, a yellow folder: [`kind_icon`]; another type's in the colour Teams gives that type), the
//!   file's name and, under it, where it is kept (`personal > <owner>`; not in a channel, as Teams), each on one line,
//!   cut with an ellipsis; then
//!   Share (on a video's card only, as Teams has it) and "More options" (…), buttons of Teams' size. Under the header,
//!   a 16:9 picture of the card's width, also in a narrow card: a video's card has the player that plays it in the card
//!   (`crate::video_player`, its thumbnail alone standing in until it plays). Any other card has a picture of the file:
//!   for an image, its thumbnail, asked of the engine by the file's sharing link (`crate::inline::thumbnail`, never a
//!   request of the view's own). A file with no thumbnail (an archive, a document, or a picture that can't be had)
//!   shows the large icon of its type there, as Teams shows a zip. The header and the picture have Teams' shades
//!   ([`header_colour`], [`picture_colour`]).
//! - **An image with a preview** (its entry's `filePreview`, a picture of it on AMS) is drawn as Teams draws it: the
//!   picture alone, no header, at the preview's size scaled down into [`PREVIEW_MAX`] (its aspect ratio kept), with
//!   rounded corners; "More options" over its corner while the pointer is on it. The picture is asked of the engine as
//!   a message's picture is (`crate::inline::picture`); a tinted box of its size stands in while it is read, and the
//!   card is drawn when there is none.
//! - **A click** on the name or the picture opens the file the way the web client does, at its sharing link in the
//!   browser (GPUI's `open_url`); "More options" offers what Teams on the web offers: "Open in browser" (not on a
//!   video's card), "Download", "Share" and "Copy link". Download reads the file through its account's engine by
//!   its sharing link, else its own (a file sent with none), and saves it where the user chooses, as an own server's
//!   file (below). Only a card drawn with no engine gives the browser the sharing link with `download=1`.
//!   Share (the video card's button, and the item) opens Teams' "Share this file" dialog through the view that drew
//!   the card ([`ShareFile`], `crate::share_file`); a card drawn with no such view (a message still being sent, the
//!   read-only app) offers no Share. Teams' "Favorite" (the file in the user's OneDrive favorites) is not offered: the
//!   app has no favorite files.
//! - **An own server's file** ([`EngineFile`]) is read through its account's engine, never in a browser: the server has
//!   no page that shows a file, and a browser holds no token for it. A click and "Open in desktop app" save it in a
//!   new folder in a folder of the user's alone and open it in the system's app for it (its `file://` link; not on a
//!   video's card, as Teams has no "Open in browser" there);
//!   "Download" saves it where the user chooses (the downloads folder without a chooser)
//!   (`crate::recording_download`). Each runs to its end, also when the other one starts while it runs. A line under
//!   the card tells how far the last one got, for a Microsoft file's Download too.
//! - **The keyboard** reaches each card as Teams' does: a card is a Tab stop in its message's order, ringed while the
//!   keyboard is on it ([`crate::keyboard::outline_ring`]), then its own buttons. Enter or Space does what a click does: it opens the file,
//!   or plays a video in its card, the keys going into the player; Escape there pauses it and gives the keys back to
//!   the card (`crate::video_player`). A screen reader names a card by the file's name and kind ([`spoken_name`]):
//!   no capture gives a file's size, so the name has none.
//! - Colours come from the palette but for the file types' own, fixed in every theme as Teams' file icons are.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Entity, FocusHandle, Global, Image, ImageFormat,
    InteractiveElement as _, IntoElement, MouseButton, ObjectFit, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::button::Button;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_proto::chat::{FileKind, SharedFile};

use crate::icons::{Glyph, icon_button, selected_by};
use crate::inline::{PictureSource, PictureState, picture, thumbnail};
use crate::keyboard::outline_ring;
use crate::recording_download::{self, Failure, Progress, Then, download_of};
use crate::rich_text::tint;
use crate::theme::{self, Palette};
use crate::video_player::{VideoPlayer, player_of, plays_inline};

/// A card's width in a grid of two or more files, as Teams draws one.
pub const CARD_WIDTH: f32 = 316.0;

/// The width of the card of a message's only file: the grid's two columns, as Teams draws it.
pub const LONE_CARD_WIDTH: f32 = COLUMNS * CARD_WIDTH;

/// A card's picture's width over its height, whatever the card's width: Teams' 16:9.
pub const PICTURE_RATIO: f32 = 16.0 / 9.0;

/// The size of a header's buttons (Share, "More options"), px: their icons are three quarters of it, Teams' 20 px.
pub const HEADER_BUTTON: f32 = 27.0;

/// How far a card's header is mixed from the bubble toward the text, in a dark and in a light theme: Teams Dark's
/// header is 51 on a bubble of 41, Teams Light's 250 on 255.
const HEADER_LIFT: (f32, f32) = (0.047, 0.023);

/// The height of a card's header: the icon beside the name and the location.
pub const HEADER_HEIGHT: f32 = 64.0;

/// The size of a menu row's icon, px.
const MENU_ICON: f32 = 14.0;

/// The size of the type's icon in a card's header.
const HEADER_ICON: f32 = 32.0;

/// The size of the type's icon in the picture of a file that has no thumbnail.
const PICTURE_ICON: f32 = 40.0;

/// The most cards in one row of a message's grid, as Teams draws them.
pub const COLUMNS: f32 = 2.0;

/// The space between two cards of the grid, across and down.
pub const GRID_GAP: f32 = 10.0;

/// The box an image's preview is scaled down into (width, height), as Teams draws one in a message.
pub const PREVIEW_MAX: (f32, f32) = (360.0, 252.0);

/// The size a preview is held at while it is read, when its entry gives none.
const PREVIEW_UNKNOWN: (u32, u32) = (252, 252);

/// What "More options" offers, in Teams' words.
pub const OPEN_IN_BROWSER: &str = "Open in browser";
pub const OPEN_IN_APP: &str = "Open in desktop app";
pub const DOWNLOAD: &str = "Download";
pub const SHARE: &str = "Share";
pub const COPY_LINK: &str = "Copy link";

/// The colour of a card's header (and of the card under its picture), as Teams draws it: the bubble's surface a shade
/// toward the text.
#[must_use]
pub fn header_colour(palette: Palette) -> u32 {
    let lift = if theme::palette_is_dark(palette) {
        HEADER_LIFT.0
    } else {
        HEADER_LIFT.1
    };
    theme::mix(palette.surface, palette.foreground, lift)
}

/// The colour of a card's picture while it shows no picture of the file (a zip, a document, a video with no
/// thumbnail, one read): the app's background, as Teams draws it, darker than the bubble in a dark theme and a shade
/// under the header in a light one.
#[must_use]
pub const fn picture_colour(palette: Palette) -> u32 {
    palette.background
}

/// Teams' own type icons, in colour (our drawings after them): a video's and a zip's; the others are [`kind_glyph`]s.
fn teams_icon(kind: FileKind) -> Option<Arc<Image>> {
    static VIDEO: OnceLock<Arc<Image>> = OnceLock::new();
    static ZIP: OnceLock<Arc<Image>> = OnceLock::new();
    let (cell, svg): (&OnceLock<Arc<Image>>, &'static [u8]) = match kind {
        FileKind::Video => (&VIDEO, include_bytes!("../assets/file-types/video.svg")),
        FileKind::Archive => (&ZIP, include_bytes!("../assets/file-types/zip.svg")),
        _ => return None,
    };
    Some(Arc::clone(cell.get_or_init(|| {
        Arc::new(Image::from_bytes(ImageFormat::Svg, svg.to_vec()))
    })))
}

/// The icon of a file type as a card draws it, `size` px square: Teams' own for a video and a zip, else its glyph in
/// its colour. Its test selector is `<selector> teams` for Teams' own, `<selector> glyph` for a glyph.
#[must_use]
pub fn kind_icon(kind: FileKind, size: f32, selector: String) -> AnyElement {
    match teams_icon(kind) {
        Some(image) => div()
            .debug_selector(move || format!("{selector} teams"))
            .flex_none()
            .size(px(size))
            .child(img(image).size_full())
            .into_any_element(),
        None => div()
            .debug_selector(move || format!("{selector} glyph"))
            .flex_none()
            .size(px(size))
            .child(
                kind_glyph(kind)
                    .icon()
                    .size(px(size))
                    .text_color(rgb(kind_colour(kind))),
            )
            .into_any_element(),
    }
}

/// The colour Teams draws a file type's icon in, the same in every theme.
#[must_use]
pub const fn kind_colour(kind: FileKind) -> u32 {
    match kind {
        FileKind::Video => 0x0088_6ce4,
        FileKind::Image => 0x0000_9fb4,
        FileKind::Audio => 0x00c2_39b3,
        FileKind::Pdf => 0x00d1_3438,
        FileKind::Word => 0x0029_5ec7,
        FileKind::Excel => 0x0010_8b4b,
        FileKind::PowerPoint => 0x00d3_5230,
        FileKind::Archive => 0x00c1_9c00,
        FileKind::Text | FileKind::Other => 0x0088_8888,
    }
}

/// The icon of a file type.
#[must_use]
pub const fn kind_glyph(kind: FileKind) -> Glyph {
    match kind {
        FileKind::Video => Glyph::FILE_VIDEO,
        FileKind::Image => Glyph::FILE_IMAGE,
        FileKind::Audio => Glyph::FILE_AUDIO,
        FileKind::Pdf | FileKind::Word | FileKind::Text => Glyph::TRANSCRIPT,
        FileKind::Excel => Glyph::FILE_SPREADSHEET,
        FileKind::PowerPoint => Glyph::FILE_CHART,
        FileKind::Archive => Glyph::FILE_ARCHIVE,
        FileKind::Other => Glyph::FILE,
    }
}

/// A file type as a screen reader names it (our words: no capture gives Teams').
#[must_use]
pub const fn kind_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Video => "Video",
        FileKind::Image => "Image",
        FileKind::Audio => "Audio",
        FileKind::Pdf => "PDF",
        FileKind::Word => "Word document",
        FileKind::Excel => "Excel workbook",
        FileKind::PowerPoint => "PowerPoint presentation",
        FileKind::Archive => "Compressed folder",
        FileKind::Text => "Text document",
        FileKind::Other => "File",
    }
}

/// A card's name for a screen reader: the file's name and its kind ("plan.docx, Word document").
#[must_use]
pub fn spoken_name(file: &SharedFile) -> String {
    format!("{}, {}", file.name, kind_name(file.kind()))
}

/// What a card's Share calls with its file: the view that drew it opening "Share this file" (`crate::share_file`).
#[derive(Clone)]
pub struct ShareFile(Rc<Share>);

/// What [`ShareFile`] calls.
type Share = dyn Fn(&SharedFile, &mut Window, &mut App);

impl ShareFile {
    pub fn new(share: impl Fn(&SharedFile, &mut Window, &mut App) + 'static) -> Self {
        Self(Rc::new(share))
    }

    fn open(&self, file: &SharedFile, window: &mut Window, cx: &mut App) {
        (self.0)(file, window, cx);
    }
}

/// The cards of `files`, carried by the message `message`, in a row that wraps; `None` without files. `source` is the
/// engine their thumbnails are asked of (none: no thumbnail, each picture its placeholder); `share` what their Share
/// calls (none: no Share); `in_channel` drops the line of where each file is kept, as Teams' channel cards have none.
#[must_use]
pub fn file_cards(
    message: &str,
    files: &[SharedFile],
    source: Option<&PictureSource>,
    share: Option<&ShareFile>,
    in_channel: bool,
    palette: Palette,
) -> Option<AnyElement> {
    if files.is_empty() {
        return None;
    }
    let message = message.to_owned();
    let width = if files.len() == 1 { LONE_CARD_WIDTH } else { CARD_WIDTH };
    let cards = files.iter().enumerate().map(|(at, file)| FileCard {
        id: format!("{message}-{at}"),
        file: file.clone(),
        source: source.cloned(),
        share: share.cloned(),
        in_channel,
        width,
        palette,
    });
    let cards: Vec<FileCard> = cards.collect();
    Some(
        div()
            .debug_selector(move || format!("file-cards-{message}"))
            .pt_2()
            .max_w(px(COLUMNS * CARD_WIDTH + (COLUMNS - 1.0) * GRID_GAP))
            .flex()
            .flex_wrap()
            .items_start()
            .gap(px(GRID_GAP))
            .children(cards)
            .into_any_element(),
    )
}

/// One file's card (see the module docs). `id` names it among every card drawn: the message's id and the file's place.
#[derive(IntoElement)]
pub struct FileCard {
    id: String,
    file: SharedFile,
    source: Option<PictureSource>,
    share: Option<ShareFile>,
    in_channel: bool,
    width: f32,
    palette: Palette,
}

impl RenderOnce for FileCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let kind = self.file.kind();
        let focus = self.focus(window, cx);
        if let Some(preview) = self.preview(&focus, window, cx) {
            return preview;
        }
        let player = match &self.source {
            Some(source) if plays_inline(&self.file) => Some(player_of(&self.id, &self.file, source, window, cx)),
            _ => None,
        };
        if let Some(player) = &player {
            player.update(cx, |player, _| player.return_to(&focus));
        }
        let picture = match &player {
            Some(player) => player.clone().into_any_element(),
            None => self.picture(kind, cx),
        };
        let ring = outline_ring(&focus, format!("file-card-ring-{}", self.id), self.palette, window, cx);
        let id = self.id.clone();
        self.keyed(
            div().id(SharedString::from(format!("file-card-box-{id}"))),
            &focus,
            player,
        )
        .debug_selector(move || format!("file-card-{id}"))
        .relative()
        .w(px(self.width))
        .max_w_full()
        .flex_none()
        .flex()
        .flex_col()
        .rounded_lg()
        .overflow_hidden()
        .border_1()
        .border_color(rgb(self.palette.border))
        .bg(rgb(header_colour(self.palette)))
        .child(self.header(kind))
        .child(picture)
        .children(self.status(cx))
        .children(ring)
        .into_any_element()
    }
}

/// Whether `file` is drawn as its picture alone ([`FileCard::preview`]) when the picture can be had: an image with a
/// preview on AMS.
#[must_use]
pub fn drawn_as_preview(file: &SharedFile) -> bool {
    file.kind() == FileKind::Image && file.preview.is_some()
}

/// The size `natural` is drawn at inside [`PREVIEW_MAX`]: scaled down to fit, its aspect ratio kept, never up.
#[must_use]
pub fn preview_size(natural: (u32, u32)) -> (f32, f32) {
    #[expect(clippy::cast_precision_loss, reason = "a preview's side is at most 100 000")]
    let (width, height) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let scale = (PREVIEW_MAX.0 / width).min(PREVIEW_MAX.1 / height).min(1.0);
    ((width * scale).round(), (height * scale).round())
}

impl FileCard {
    /// The card's focus handle, a Tab stop, kept from frame to frame.
    fn focus(&self, window: &mut Window, cx: &mut App) -> FocusHandle {
        window
            .use_keyed_state(
                SharedString::from(format!("file-card-focus-{}", self.id)),
                cx,
                |_, cx| cx.focus_handle().tab_stop(true),
            )
            .read(cx)
            .clone()
    }

    /// `card` as the keyboard reaches it: a Tab stop with its screen reader name, whose Enter or Space opens the file,
    /// or plays it in `player` (see the module docs). A press of the pointer leaves the keys where they are, as a click
    /// in the messages does.
    fn keyed(
        &self,
        card: gpui::Stateful<gpui::Div>,
        focus: &FocusHandle,
        player: Option<Entity<VideoPlayer>>,
    ) -> gpui::Stateful<gpui::Div> {
        let (file, own) = (self.file.clone(), self.own());
        card.track_focus(focus)
            .role(gpui::accesskit::Role::Button)
            .aria_label(spoken_name(&self.file))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(move |event: &ClickEvent, window, cx| {
                if !event.is_keyboard() {
                    return;
                }
                match &player {
                    Some(player) => player.update(cx, |player, cx| player.play_from_keys(window, cx)),
                    None => open_file(&file, own.as_ref(), cx),
                }
            })
    }

    /// An image drawn as its preview (see the module docs); `None` when it is drawn as its card: another kind of file,
    /// no preview, no engine to ask, or none to have.
    fn preview(&self, focus: &FocusHandle, window: &mut Window, cx: &mut App) -> Option<AnyElement> {
        let preview = self.file.preview.as_ref().filter(|_| drawn_as_preview(&self.file))?;
        let state = picture(&preview.src, self.source.as_ref()?, cx);
        let given = preview.size;
        let id = self.id.clone();
        let (selector, size, shown) = match state {
            PictureState::Missing => return None,
            PictureState::Reading => (
                "file-preview-reading",
                preview_size(given.unwrap_or(PREVIEW_UNKNOWN)),
                div()
                    .size_full()
                    .bg(tint(self.palette.foreground, 0.06))
                    .into_any_element(),
            ),
            PictureState::Shown(image, natural) => (
                "file-preview",
                preview_size(given.unwrap_or(natural)),
                img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            ),
        };
        let group = SharedString::from(format!("file-preview-{id}"));
        // shown while the keyboard is on the card or on it too, so Tab never lands on a button no one sees
        let keys = focus.contains_focused(window, cx);
        let more_shown = if keys { "shown" } else { "on-hover" };
        let more_selector = format!("file-more-wrap-{id} {more_shown}");
        let more = self.more(FileKind::Image).map(|more| {
            div()
                .debug_selector(move || more_selector.clone())
                .absolute()
                .top_1()
                .right_1()
                .rounded_md()
                .bg(rgb(self.palette.surface))
                .when(!keys, |more| {
                    more.opacity(0.0).group_hover(group.clone(), |style| style.opacity(1.0))
                })
                .child(more)
        });
        let ring = outline_ring(focus, format!("file-card-ring-{id}"), self.palette, window, cx);
        let picture = open_on_click(
            div()
                .id(SharedString::from(format!("file-picture-{id}")))
                .size_full()
                .child(shown),
            &self.file,
            self.own(),
        );
        let status = self.status(cx);
        let preview = self
            .keyed(
                div().id(SharedString::from(format!("file-preview-box-{id}"))),
                focus,
                None,
            )
            .debug_selector(move || format!("{selector}-{id}"))
            .group(group)
            .relative()
            .w(px(size.0))
            .h(px(size.1))
            .max_w_full()
            .flex_none()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(self.palette.border))
            .child(picture)
            .children(more)
            .children(ring)
            .into_any_element();
        Some(match status {
            Some(status) => div()
                .flex()
                .flex_col()
                .gap_1()
                .child(preview)
                .child(status)
                .into_any_element(),
            None => preview,
        })
    }

    /// The icon, the name and "More options".
    fn header(&self, kind: FileKind) -> AnyElement {
        let name = SharedString::from(self.file.name.clone());
        let opens = open_on_click(
            div()
                .id(SharedString::from(format!("file-open-{}", self.id)))
                .debug_selector({
                    let id = self.id.clone();
                    move || format!("file-name-{id}")
                })
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .tooltip({
                    let name = name.clone();
                    move |window, cx| Tooltip::new(name.clone()).build(window, cx)
                })
                .child(kind_icon(kind, HEADER_ICON, format!("file-icon-{}", self.id)))
                .child(self.title(name)),
            &self.file,
            self.own(),
        );
        div()
            .h(px(HEADER_HEIGHT))
            .px_3()
            .flex()
            .items_center()
            .gap_1()
            .child(opens)
            .children((kind == FileKind::Video).then(|| self.share()).flatten())
            .children(self.more(kind))
            .into_any_element()
    }

    /// The name over where the file is kept, each on one line, cut with an ellipsis.
    fn title(&self, name: SharedString) -> AnyElement {
        let line = || div().overflow_hidden().text_ellipsis().whitespace_nowrap();
        let location = self.file.location.clone().filter(|_| !self.in_channel).map(|location| {
            let id = self.id.clone();
            line()
                .debug_selector(move || format!("file-location-{id}"))
                .text_sm()
                .text_color(rgb(self.palette.muted_foreground))
                .child(location)
        });
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(line().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
            .children(location)
            .into_any_element()
    }

    /// Share: "Share this file" for the file ([`ShareFile`]); none without a link or a view to open it.
    fn share(&self) -> Option<AnyElement> {
        self.file.open_link()?;
        let share = self.share.clone()?;
        let file = self.file.clone();
        let button = header_button(format!("file-share-{}", self.id), Glyph::SHARE, SHARE)
            .on_click(move |_, window, cx| share.open(&file, window, cx));
        Some(selected_by(format!("file-share-{}", self.id), button).into_any_element())
    }

    /// "More options": the file's actions, as Teams on the web offers them, when it has a link to act on: "Open in
    /// browser" (not on a video's card), "Download", "Share" ("Share this file", [`ShareFile`]; not without a view to
    /// open it) and "Copy link". Download goes through the engine, as an own server's Open does (see the module docs).
    fn more(&self, kind: FileKind) -> Option<AnyElement> {
        let link = self.file.open_link()?.to_owned();
        let opens = kind != FileKind::Video;
        let own = self.own();
        let download = self.download();
        let id = self.id.clone();
        let (share, file) = (self.share.clone(), self.file.clone());
        let button =
            header_button(format!("file-more-{id}"), Glyph::MORE, "More options").dropdown_menu(move |menu, _, _| {
                let menu = match &own {
                    _ if !opens => menu,
                    Some(own) => {
                        let own = own.clone();
                        menu.item(
                            menu_item(&id, "open", Glyph::EXTERNAL_LINK, OPEN_IN_APP)
                                .on_click(move |_, _, cx| own.open(cx)),
                        )
                    }
                    None => {
                        let link = link.clone();
                        menu.item(
                            menu_item(&id, "open", Glyph::EXTERNAL_LINK, OPEN_IN_BROWSER)
                                .on_click(move |_, _, cx| cx.open_url(&link)),
                        )
                    }
                };
                let menu = match download.clone() {
                    Some(download) => menu.item(
                        menu_item(&id, "download", Glyph::DOWNLOAD, DOWNLOAD)
                            .on_click(move |_, _, cx| download.start(cx)),
                    ),
                    None => menu,
                };
                let copied = link.clone();
                let menu = match &share {
                    Some(share) => {
                        let (share, file) = (share.clone(), file.clone());
                        menu.item(
                            menu_item(&id, "share", Glyph::SHARE, SHARE)
                                .on_click(move |_, window, cx| share.open(&file, window, cx)),
                        )
                    }
                    None => menu,
                };
                menu.item(
                    menu_item(&id, "copy", Glyph::LINK, COPY_LINK)
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))),
                )
            });
        Some(selected_by(format!("file-more-{}", self.id), button).into_any_element())
    }

    /// The file as its own server's, read through `source`'s engine: when it has an engine and its links aren't on
    /// Microsoft's file service.
    fn own(&self) -> Option<EngineFile> {
        self.engine_file()
            .filter(|file| !teams_proto::chat::is_file_link(&file.link))
    }

    /// The file read through `source`'s engine by its sharing link, else its own: when it has an engine and a link.
    fn engine_file(&self) -> Option<EngineFile> {
        let link = self.file.open_link()?.to_owned();
        Some(EngineFile::new(link, self.file.name.clone(), self.source.clone()?))
    }

    /// How "Download" gets the file (see [`Download`]); none when it has no link.
    fn download(&self) -> Option<Download> {
        download_of_file(&self.file, self.source.as_ref())
    }

    /// How far the file's download through its engine got, as a line under its card; none without one this run.
    fn status(&self, cx: &App) -> Option<AnyElement> {
        let file = self.engine_file()?;
        download_line(
            &file.link,
            &file.name,
            &format!("file-download-{}", self.id),
            self.palette,
            cx,
        )
    }

    /// The picture of the file under its header, 16:9 of the card's width: its thumbnail, the picture's shade while it
    /// is read, else the large icon of its type, which is all that a file with no thumbnail (an archive, a document)
    /// shows there.
    fn picture(&self, kind: FileKind, cx: &mut App) -> AnyElement {
        let palette = self.palette;
        // a channel file has no sharing link: its own address names it
        let state = match (&self.source, self.file.open_link()) {
            (Some(source), Some(link)) if kind.has_thumbnail() => thumbnail(link, source, cx),
            _ => PictureState::Missing,
        };
        let id = self.id.clone();
        let (selector, shown) = match state {
            PictureState::Shown(image, _) => (
                "file-thumbnail",
                img(image).size_full().object_fit(ObjectFit::Cover).into_any_element(),
            ),
            PictureState::Reading => ("file-thumbnail-reading", div().size_full().into_any_element()),
            PictureState::Missing => (
                "file-thumbnail-none",
                no_picture(kind, &format!("file-picture-icon-{id}")),
            ),
        };
        open_on_click(
            div()
                .id(SharedString::from(format!("file-picture-{id}")))
                .debug_selector(move || format!("{selector}-{id}"))
                .w_full()
                .aspect_ratio(PICTURE_RATIO)
                .bg(rgb(picture_colour(palette)))
                .child(shown),
            &self.file,
            self.own(),
        )
    }
}

/// The picture of a file that has none to show (see [`picture_colour`]): the large icon of its type in the middle.
/// Its icon's test selector is `<selector> teams` or `<selector> glyph` ([`kind_icon`]).
#[must_use]
pub fn no_picture(kind: FileKind, selector: &str) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(kind_icon(kind, PICTURE_ICON, selector.to_owned()))
        .into_any_element()
}

/// How "Download" gets a file: through its account's engine, which saves it with a line under its card; the browser's
/// download link only for a card drawn with no engine.
#[derive(Clone)]
pub(crate) enum Download {
    Engine(EngineFile),
    Browser(String),
}

impl Download {
    pub(crate) fn start(&self, cx: &mut App) {
        match self {
            Self::Engine(file) => file.download(cx),
            Self::Browser(link) => cx.open_url(link),
        }
    }
}

/// How "Download" gets `file`, read through `source`'s engine when there is one (see [`Download`]).
pub(crate) fn download_of_file(file: &SharedFile, source: Option<&PictureSource>) -> Option<Download> {
    let link = file.open_link()?.to_owned();
    match source {
        Some(source) => Some(Download::Engine(EngineFile::new(
            link,
            file.name.clone(),
            source.clone(),
        ))),
        None => file.download_link().map(Download::Browser),
    }
}

/// A header's button: `glyph` at Teams' size ([`HEADER_BUTTON`]), `tip` its tooltip.
fn header_button(id: String, glyph: Glyph, tip: &'static str) -> Button {
    icon_button(SharedString::from(id), glyph, tip).with_size(px(HEADER_BUTTON))
}

/// `element` opening `file` when clicked, with the pointer's hand: an own server's through its engine ([`EngineFile::open`]),
/// any other in the browser (its sharing link, else its own); as it was when the file has no link to open.
fn open_on_click(element: gpui::Stateful<gpui::Div>, file: &SharedFile, own: Option<EngineFile>) -> AnyElement {
    if file.open_link().is_none() {
        return element.into_any_element();
    }
    let file = file.clone();
    element
        .cursor_pointer()
        .hover(|style| style.opacity(0.85))
        .on_click(move |_, _, cx| open_file(&file, own.as_ref(), cx))
        .into_any_element()
}

/// Opens `file`: an own server's through its engine ([`EngineFile::open`]), any other in the browser (its sharing link,
/// else its own); nothing when it has no link.
fn open_file(file: &SharedFile, own: Option<&EngineFile>, cx: &mut App) {
    match (own, file.open_link()) {
        (Some(own), _) => own.open(cx),
        (None, Some(link)) => cx.open_url(link),
        (None, None) => {}
    }
}

/// Why a file couldn't be downloaded through its engine, as its card's line says it.
fn failed_words(failure: &Failure) -> String {
    match failure {
        Failure::Refused => "You may no longer have access to the file.".to_owned(),
        other => other.words(),
    }
}

/// How far the last download this run of the file at `link` (saved as `name`) through its engine got, as a line; none
/// without one, or after it was cancelled. Its selector is `<selector> <state>`.
pub(crate) fn download_line(link: &str, name: &str, selector: &str, palette: Palette, cx: &App) -> Option<AnyElement> {
    let download = download_of(link, cx)?;
    let download = download.read(cx);
    let (state, words) = match download.progress() {
        Progress::Running { .. } => (
            "running",
            match download.progress().fraction() {
                Some(fraction) => format!("Downloading {name}… {:.0}%", fraction * 100.0),
                None => format!("Downloading {name}…"),
            },
        ),
        Progress::Saved if download.then() == Then::Open => ("saved", format!("Opened {name}")),
        Progress::Saved => ("saved", format!("Saved to {}", download.target().display())),
        Progress::Failed(Failure::Cancelled) => return None,
        Progress::Failed(failure) => ("failed", format!("Couldn't download {name}. {}", failed_words(failure))),
    };
    let selector = format!("{selector} {state}");
    Some(
        div()
            .debug_selector(move || selector.clone())
            .px_3()
            .pb_2()
            .text_xs()
            .text_color(rgb(palette.muted_foreground))
            .child(words)
            .into_any_element(),
    )
}

/// A file read through its account's engine: an own server's to open or download, a Microsoft file's to download
/// (see the module docs).
#[derive(Clone)]
pub struct EngineFile {
    /// Its sharing link, else its own: what the engine reads it by, and what its download is known by.
    link: String,
    name: String,
    source: PictureSource,
}

/// Where Open in desktop app saves an own server's files, in place of `ngteams-files` in the runtime folder (a test's
/// own folder).
pub struct OpenedFilesFolder(pub PathBuf);

impl Global for OpenedFilesFolder {}

/// A new folder in `folder` for one opened file: `1`, `2`, …, the first that is not there. It is made at once, so two
/// files of one name opened together never write to one file.
fn new_folder_in(folder: &Path) -> std::io::Result<PathBuf> {
    for n in 1_u32.. {
        let made = folder.join(n.to_string());
        match std::fs::create_dir(&made) {
            Ok(()) => return Ok(made),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

impl EngineFile {
    /// The file at `link` (its sharing link, else its own), saved as `name`, read through the engine of `source`.
    #[must_use]
    pub fn new(link: String, name: String, source: PictureSource) -> Self {
        Self { link, name, source }
    }

    /// Open in desktop app: saved in a new folder (see [`new_folder_in`]) in a folder of the user's alone (the
    /// runtime one, else the temporary one, or [`OpenedFilesFolder`]; the downloads folder when it can't be had),
    /// then opened in the system's app for it.
    pub fn open(&self, cx: &mut App) {
        let private = cx.try_global::<OpenedFilesFolder>().map_or_else(
            || {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .filter(|dir| dir.is_dir())
                    .unwrap_or_else(std::env::temp_dir)
                    .join("ngteams-files")
            },
            |folder| folder.0.clone(),
        );
        let name = safe_name(&self.name);
        let target = teams_platform::private::create_dir_all(&private)
            .and_then(|()| teams_platform::private::restrict_dir(&private))
            .and_then(|()| new_folder_in(&private))
            .map_or_else(
                |_| {
                    let folder = crate::picture_viewer::downloads().unwrap_or_else(|_| std::env::temp_dir());
                    crate::picture_viewer::free_name(&folder, &name)
                },
                |folder| folder.join(&name),
            );
        recording_download::start_with(
            self.source.file_opener(self.link.clone()),
            self.link.clone(),
            target,
            Then::Open,
            cx,
        );
    }

    /// Download: saved where the user chooses (the downloads folder without a chooser).
    pub fn download(&self, cx: &mut App) {
        let folder = crate::picture_viewer::downloads().unwrap_or_else(|_| std::env::temp_dir());
        let name = safe_name(&self.name);
        let chosen = cx.prompt_for_new_path(&folder, Some(&name));
        let own = self.clone();
        cx.spawn(async move |cx| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => crate::picture_viewer::free_name(&folder, &name),
            };
            cx.update(|cx| {
                recording_download::start_with(
                    own.source.file_opener(own.link.clone()),
                    own.link.clone(),
                    target,
                    Then::Keep,
                    cx,
                );
            });
        })
        .detach();
    }
}

/// `name` as a file name in a folder: the sender's words, so never a path or a name the system refuses.
#[must_use]
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned.to_owned()
    }
}

/// A "More options" row: its icon and words, its test selector `file-menu-<what>-<card>`. The icon is in the menu's
/// own column, so the rows line up.
fn menu_item(card: &str, what: &str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    let selector = format!("file-menu-{what}-{card}");
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div().debug_selector(move || selector.clone()).child(label)
    })
    .icon(menu_icon(glyph))
}

/// A menu row's icon: the kit draws the icons of its column at 10.5 px; scaled to the 14 px of the app's menus, and
/// moved left by what the scale adds on the right, so that it keeps its space from the words.
fn menu_icon(glyph: Glyph) -> gpui_component::Icon {
    let scale = MENU_ICON / 10.5;
    glyph.icon().transform(
        gpui::Transformation::scale(gpui::size(scale, scale))
            .with_translation(gpui::point(px(-(MENU_ICON - 10.5) / 2.0), px(0.0))),
    )
}

#[cfg(test)]
mod tests {
    use super::{header_colour, picture_colour};
    use crate::theme::{DARK_PALETTE, LIGHT_PALETTE};

    /// `colour`'s red, green and blue.
    fn channels(colour: u32) -> [u32; 3] {
        [colour >> 16 & 0xff, colour >> 8 & 0xff, colour & 0xff]
    }

    /// Whether each channel of `colour` is within `near` of `wanted`.
    fn about(colour: u32, wanted: u32, near: u32) -> bool {
        channels(colour).iter().all(|channel| channel.abs_diff(wanted) <= near)
    }

    #[test]
    fn a_cards_header_and_empty_picture_have_teams_shades() {
        // Teams Dark: the bubble 41, the header 51, the picture 31 (darker than the bubble)
        assert!(
            about(header_colour(DARK_PALETTE), 51, 1),
            "{:06x}",
            header_colour(DARK_PALETTE)
        );
        assert!(
            about(picture_colour(DARK_PALETTE), 31, 1),
            "{:06x}",
            picture_colour(DARK_PALETTE)
        );
        // Teams Light: the bubble 255, the header 250, the picture 245 (a shade under the header)
        assert!(
            about(header_colour(LIGHT_PALETTE), 250, 1),
            "{:06x}",
            header_colour(LIGHT_PALETTE)
        );
        assert!(
            about(picture_colour(LIGHT_PALETTE), 245, 5),
            "{:06x}",
            picture_colour(LIGHT_PALETTE)
        );
        let picture: u32 = channels(picture_colour(LIGHT_PALETTE)).iter().sum();
        assert!(picture.abs_diff(3 * 245) <= 6, "{picture}");
    }
}
