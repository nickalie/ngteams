//! The compose box's picker (`teams_ui::fun_picker`) on GPUI's test context, over the fake engine (`common`): the tabs
//! All, Emoji and Stickers, a sticker put in the box and sent as Teams sends one, the import of a folder from the
//! picker, a pack renamed and removed, the keyboard's reach, and a box that offers no sticker; Teams' own packs for an
//! account of Microsoft's cloud (their previews, a pick sent by its address, the search, the keyboard) and none for an
//! own server's. The settings live in a directory of their own per test, beside the stickers. Every picture, name and
//! id is made up, but for Teams' packs' names and ids.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, VisualTestContext, px, size};
use image::{ImageFormat, Rgba, RgbaImage};
use teams_core::{Action, EngineError};
use teams_proto::chat::MessageText;
use teams_store::Member;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::{Compose, ComposeEvent, Loaded, Mode, STICKER_LOADING_FIRST};
use teams_ui::editor::doc::Doc;
use teams_ui::fun_picker::{FunPicker, Item, Place, StickerView, Tab};
use teams_ui::settings::SettingsModel;
use teams_ui::shell::Shell;
use teams_ui::stickers::import::{self, ImportKind};
use teams_ui::stickers::{Shelf, StickerRef};
use teams_ui::{SettingsFile, ShellParts, ViewTrace};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";
/// A group chat (made up).
const OTHER: &str = "19:labgroup0001@thread.v2";

/// A made-up still picture, `side` square, in `format`.
fn picture(side: u32, colour: [u8; 3], format: ImageFormat) -> Vec<u8> {
    let picture = RgbaImage::from_pixel(side, side, Rgba([colour[0], colour[1], colour[2], 255]));
    let mut bytes = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut bytes), format).unwrap();
    bytes
}

/// A folder `name` of made-up stickers in `root`: two pictures and a note.
fn pack_folder(root: &Path, name: &str) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("happy_cat.png"), picture(300, [200, 40, 40], ImageFormat::Png)).unwrap();
    std::fs::write(dir.join("sleepy_cat.png"), picture(90, [40, 40, 200], ImageFormat::Png)).unwrap();
    std::fs::write(dir.join("notes.txt"), b"made up").unwrap();
    dir
}

/// The test's settings directory and what it keeps.
struct Setup {
    _dir: tempfile::TempDir,
    settings: std::path::PathBuf,
    stickers: std::path::PathBuf,
    sources: tempfile::TempDir,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            settings: dir.path().join("settings.toml"),
            stickers: dir.path().join("stickers"),
            _dir: dir,
            sources: tempfile::tempdir().unwrap(),
        }
    }

    /// A pack "Lab cats" imported before the app starts.
    fn with_cats(self) -> Self {
        let folder = pack_folder(self.sources.path(), "Lab cats");
        import::import(&folder, ImportKind::Folder, &self.stickers, &mut |_| {}).unwrap();
        self
    }
}

/// An account with only the user's packs: an own server's, which has none of Teams'.
fn new_rig() -> Rig {
    let rig = teams_rig();
    *rig.core.capabilities.lock().unwrap() = Some(teams_core::Capabilities {
        teams_stickers: false,
        ..teams_core::Capabilities::ALL
    });
    rig
}

/// An account of Microsoft's cloud: Teams' packs too.
fn teams_rig() -> Rig {
    let rig = Rig::start();
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.cx.update(teams_ui::window::init);
    rig
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
}

/// The a–b chat's view with its settings in `place`, in a window of its own, the compose box focused.
fn chat_window(rig: &Rig, place: &Setup) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let settings_file = SettingsFile::new(place.settings.clone());
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::load(settings_file))),
        trace: Arc::new(ViewTrace::new(Written::default())),
    };
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(700.0)));
    cx.update(|window, _| window.activate_window());
    draw(cx);
    let compose = compose(&view, cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    (view, cx)
}

fn compose(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Entity<Compose> {
    cx.update(|_, cx| view.read(cx).compose().clone())
}

fn picker(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<Entity<FunPicker>> {
    let compose = compose(view, cx);
    cx.update(|_, cx| compose.read(cx).emoji_picker().cloned())
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let bounds = cx
        .debug_bounds(selector.to_owned().leak())
        .unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn keys(cx: &mut VisualTestContext, keystrokes: &str) {
    cx.simulate_keystrokes(keystrokes);
    draw(cx);
}

fn drawn(cx: &mut VisualTestContext, selector: &str) -> bool {
    cx.debug_bounds(selector.to_owned().leak()).is_some()
}

fn sent(rig: &Rig) -> Vec<MessageText> {
    std::mem::take(&mut *rig.core.actions.lock().unwrap())
        .into_iter()
        .filter_map(|action| match action {
            Action::Send { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

fn sticker(pack: &str, file: &str) -> StickerRef {
    StickerRef {
        pack: pack.to_owned(),
        file: file.to_owned(),
    }
}

/// All's tabs and rows: six emoji and the packs' first stickers, each section with "See all"; no GIFs tab.
fn all_tab_holds_emoji_and_stickers(picker: &Entity<FunPicker>, cx: &mut VisualTestContext) {
    for tab in ["fun-tab-all", "fun-tab-emoji", "fun-tab-stickers", "fun-search"] {
        assert!(drawn(cx, tab), "{tab}");
    }
    assert!(!drawn(cx, "fun-tab-gifs"), "no GIFs tab");
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(rows[0], [Item::SeeAllEmoji]);
    assert_eq!(rows[1].len(), 6);
    assert!(matches!(rows[1][0], Item::Emoji(_)));
    assert_eq!(rows[2], [Item::SeeAllStickers]);
    assert_eq!(
        rows[3],
        [
            Item::Sticker(sticker("lab-cats", "001.png")),
            Item::Sticker(sticker("lab-cats", "002.png"))
        ]
    );
    assert!(drawn(cx, "sticker-lab-cats-001"));
}

#[test]
fn the_picker_has_teams_tabs_and_see_all_opens_each() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    click(cx, "compose-emoji");
    let picker = picker(&view, cx).expect("the button opens the picker");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).tab()),
        Tab::All,
        "it opens on All, as Teams' does"
    );
    all_tab_holds_emoji_and_stickers(&picker, cx);

    click(cx, "emoji-see-all");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Emoji);
    assert!(drawn(cx, "compose-emoji-tab"), "the emoji picker's list");
    click(cx, "fun-tab-all");
    click(cx, "stickers-see-all");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Stickers);
    assert!(drawn(cx, "sticker-pack-lab-cats"), "the pack's tile");
    assert!(!drawn(cx, "sticker-packs-empty"));
    click(cx, "sticker-packs-see-all");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Packs
    );
    click(cx, "sticker-pack-lab-cats");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Pack {
            id: "lab-cats".to_owned(),
            from_packs: true
        }
    );
    click(cx, "sticker-back");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Packs
    );
    keys(cx, "escape");
    assert!(self::picker(&view, cx).is_none(), "Escape closes it");
    assert!(sent(&rig).is_empty());
}

#[test]
fn a_sticker_picked_goes_in_the_box_is_sent_at_once_and_its_message_is_teams_sticker() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    click(cx, "compose-emoji");
    click(cx, "fun-tab-stickers");
    click(cx, "sticker-pack-lab-cats");
    click(cx, "sticker-lab-cats-001");
    assert!(picker(&view, cx).is_none(), "a pick closes the picker");
    let uploads = rig.core.picture_uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 1, "sent to the media service at once");
    assert_eq!(uploads[0].0, AB);
    assert_eq!(uploads[0].1.format, teams_proto::PastedFormat::Png);
    assert_eq!(
        uploads[0].1.bytes,
        std::fs::read(place.stickers.join("lab-cats/001.png")).unwrap(),
        "the kept sticker's bytes"
    );
    assert!(drawn(cx, "editor-picture-1"), "it shows in the box");
    assert!(sent(&rig).is_empty(), "nothing is posted until Send");
    // Recent has it now, for every box
    let shelf = cx.update(|_, cx| Shelf::of(Some(place.stickers.clone()), cx));
    assert_eq!(
        cx.update(|_, cx| shelf.read(cx).recent().to_vec()),
        [sticker("lab-cats", "001.png")]
    );

    keys(cx, "enter");
    let posted = sent(&rig);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].pictures(), ["0-made-up-d1-1"]);
    assert_eq!(
        posted[0].html(),
        "<p>&nbsp;</p><readonly aria-label=\"Happy cat\" contenteditable=\"false\" \
         itemtype=\"http://schema.skype.com/Sticker\" title=\"Happy cat\"><img \
         src=\"https://us-api.asm.skype.com/v1/objects/0-made-up-d1-1/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" width=\"180\" height=\"180\" alt=\"Happy cat\" \
         id=\"x_0-made-up-d1-1\" itemid=\"0-made-up-d1-1\"></readonly><p>&nbsp;</p>"
    );
    // read back, it is a sticker
    let parsed = teams_proto::chat::parse_html(posted[0].html());
    assert!(
        parsed
            .nodes
            .iter()
            .any(|node| matches!(node, teams_proto::chat::Node::Sticker(_))),
        "{parsed:?}"
    );
    assert!(!drawn(cx, "editor-picture-1"), "the box is emptied");

    // All shows it first now
    click(cx, "compose-emoji");
    let picker = picker(&view, cx).unwrap();
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(rows[3][0], Item::Sticker(sticker("lab-cats", "001.png")));
}

#[test]
fn a_sticker_beside_text_is_sent_in_the_text() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (_view, cx) = chat_window(&rig, &place);
    cx.simulate_input("Look");
    draw(cx);
    click(cx, "compose-emoji");
    click(cx, "fun-tab-stickers");
    click(cx, "sticker-pack-lab-cats");
    click(cx, "sticker-lab-cats-002");
    keys(cx, "enter");
    let posted = sent(&rig);
    let html = posted[0].html();
    assert!(
        html.starts_with("<p>Look</p><readonly aria-label=\"Sleepy cat\""),
        "{html}"
    );
    assert!(
        html.contains("width=\"90\" height=\"90\""),
        "shown at its own size: {html}"
    );
}

#[test]
fn a_sticker_does_not_split_a_code_block() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    cx.simulate_input("let x");
    keys(cx, "ctrl-shift-b");
    let compose = compose(&view, cx);
    let in_code = cx.update(|_, cx| compose.read(cx).editor().read(cx).block_kind());
    assert_eq!(in_code, teams_ui::editor::doc::Kind::Code);
    click(cx, "compose-emoji");
    click(cx, "fun-tab-stickers");
    click(cx, "sticker-pack-lab-cats");
    click(cx, "sticker-lab-cats-001");
    assert!(picker(&view, cx).is_none(), "the pick closes the picker");
    assert!(rig.core.picture_uploads.lock().unwrap().is_empty(), "nothing is sent");
    assert!(!drawn(cx, "editor-picture-1"));
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some("A sticker can't go in a code block.".into())
    );
    let blocks = cx.update(|_, cx| compose.read(cx).editor().read(cx).doc().blocks.len());
    assert_eq!(blocks, 1, "the code block stays whole");
    let shelf = cx.update(|_, cx| Shelf::of(Some(place.stickers.clone()), cx));
    assert!(
        cx.update(|_, cx| shelf.read(cx).recent().is_empty()),
        "not sent: not recent"
    );
}

#[test]
fn search_finds_emoji_and_stickers_and_enter_picks_the_first() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    cx.simulate_input("cat");
    draw(cx);
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert!(
        rows.iter().flatten().any(|item| matches!(item, Item::Emoji(_))),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .flatten()
            .any(|item| *item == Item::Sticker(sticker("lab-cats", "002.png")))
    );
    click(cx, "fun-tab-stickers");
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(
        rows,
        [vec![
            Item::Sticker(sticker("lab-cats", "001.png")),
            Item::Sticker(sticker("lab-cats", "002.png"))
        ]],
        "the search kept"
    );
    keys(cx, "enter");
    assert!(
        self::picker(&view, cx).is_none(),
        "the search field has the focus back after a tab's click"
    );
    assert!(drawn(cx, "editor-picture-1"), "the first sticker found");

    keys(cx, "ctrl-.");
    cx.simulate_input("lab cats happy");
    draw(cx);
    click(cx, "fun-tab-stickers");
    let picker = self::picker(&view, cx).unwrap();
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).rows(cx)),
        [vec![Item::Sticker(sticker("lab-cats", "001.png"))]],
        "by its pack's name and its label"
    );
    keys(cx, "escape ctrl-.");
    cx.simulate_input("qqqzz");
    draw(cx);
    let picker = self::picker(&view, cx).unwrap();
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).rows(cx)),
        [vec![Item::SeeAllEmoji], vec![Item::SeeAllStickers]],
        "nothing found"
    );
    keys(cx, "enter");
    assert!(
        self::picker(&view, cx).is_some(),
        "Enter with nothing found picks nothing"
    );
}

#[test]
fn the_keyboard_reaches_the_tabs() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    let place_of = |cx: &mut VisualTestContext| cx.update(|window, cx| picker.read(cx).place(window, cx));
    assert_eq!(place_of(cx), Some(Place::Search), "the search field has the focus");
    keys(cx, "up");
    assert_eq!(place_of(cx), Some(Place::Tabs));
    keys(cx, "right");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Emoji);
    keys(cx, "right");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Stickers);
    keys(cx, "right");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::All, "it wraps");
    keys(cx, "end");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Stickers);
    keys(cx, "down");
    assert_eq!(place_of(cx), Some(Place::Search));
    // Ctrl+Tab changes the tab from anywhere
    keys(cx, "ctrl-tab");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::All);
    keys(cx, "ctrl-shift-tab ctrl-shift-tab");
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::Emoji);
    keys(cx, "escape");
    assert!(self::picker(&view, cx).is_none());
}

#[test]
fn the_keyboard_reaches_the_packs_and_the_stickers() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    let place_of = |cx: &mut VisualTestContext| cx.update(|window, cx| picker.read(cx).place(window, cx));
    keys(cx, "up end down down");
    assert_eq!(place_of(cx), Some(Place::Items(0, 0)));
    let current = |cx: &mut VisualTestContext| cx.update(|window, cx| picker.read(cx).current(window, cx));
    assert_eq!(current(cx), Some(Item::AddPack));
    keys(cx, "right");
    assert_eq!(current(cx), Some(Item::SeeAllPacks));
    keys(cx, "down");
    assert_eq!(current(cx), Some(Item::Pack("lab-cats".to_owned())));
    keys(cx, "enter");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Pack {
            id: "lab-cats".to_owned(),
            from_packs: false
        }
    );
    // Back, the pack's menu, then its stickers
    assert_eq!(current(cx), Some(Item::Back));
    keys(cx, "down");
    assert_eq!(current(cx), Some(Item::PackMenu));
    keys(cx, "down");
    assert_eq!(current(cx), Some(Item::Sticker(sticker("lab-cats", "001.png"))));
    keys(cx, "right");
    assert_eq!(current(cx), Some(Item::Sticker(sticker("lab-cats", "002.png"))));
    keys(cx, "home");
    assert_eq!(current(cx), Some(Item::Back));
    keys(cx, "up");
    assert_eq!(place_of(cx), Some(Place::Search), "Up from the first row");
    keys(cx, "down end space");
    assert!(self::picker(&view, cx).is_none(), "Space picks");
    assert!(drawn(cx, "editor-picture-1"));
}

#[test]
fn a_folder_imported_from_the_picker_shows_its_progress_and_summary_then_its_pack() {
    let place = Setup::new();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    click(cx, "compose-emoji");
    let picker = picker(&view, cx).unwrap();
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(
        rows.last(),
        Some(&vec![Item::SeeAllStickers]),
        "All has no sticker yet: {rows:?}"
    );
    click(cx, "fun-tab-stickers");
    assert!(drawn(cx, "sticker-packs-empty"), "what to do with no pack");
    assert!(drawn(cx, "sticker-import-folder") && drawn(cx, "sticker-import-zip"));

    let folder = pack_folder(place.sources.path(), "Lab cats");
    cx.update(|_, cx| picker.update(cx, |picker, cx| picker.import_from(folder, ImportKind::Folder, cx)));
    // before the import's thread ran: its progress line
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(drawn(cx, "sticker-import-progress"), "the import's progress");
    let shelf = cx.update(|_, cx| Shelf::of(Some(place.stickers.clone()), cx));
    assert!(cx.update(|_, cx| shelf.read(cx).importing()));
    a_second_import_is_refused_while_one_runs(&picker, &shelf, &place, cx);
    assert!(drawn(cx, "sticker-import-summary"), "the import's summary");
    let state = cx.update(|_, cx| shelf.read(cx).import_state().cloned());
    let Some(teams_ui::stickers::ImportState::Done(imported)) = state else {
        panic!("{state:?}");
    };
    assert_eq!(imported.pack.stickers.len(), 2);
    assert_eq!(imported.skipped.len(), 1, "the note");
    click(cx, "sticker-import-open");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Pack {
            id: "lab-cats".to_owned(),
            from_packs: false
        }
    );
    assert!(
        cx.update(|_, cx| shelf.read(cx).import_state().is_none()),
        "the summary closed"
    );
    assert!(place.stickers.join("lab-cats/pack.toml").is_file());
    the_pack_is_there_after_a_restart(&place);
}

/// A second import while one runs: refused, and the user told, until the first ends.
fn a_second_import_is_refused_while_one_runs(
    picker: &Entity<FunPicker>,
    shelf: &Entity<Shelf>,
    place: &Setup,
    cx: &mut VisualTestContext,
) {
    assert!(!drawn(cx, "sticker-import-busy"));
    let second = pack_folder(place.sources.path(), "Lab dogs");
    cx.update(|_, cx| picker.update(cx, |picker, cx| picker.import_from(second, ImportKind::Folder, cx)));
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
    assert!(drawn(cx, "sticker-import-busy"), "the refusal is told");
    assert!(cx.update(|_, cx| shelf.read(cx).refused()));
    draw(cx);
    assert!(!drawn(cx, "sticker-import-busy"), "the note goes with the import");
    assert!(!cx.update(|_, cx| shelf.read(cx).refused()));
    assert_eq!(
        cx.update(|_, cx| shelf.read(cx).packs().len()),
        1,
        "the refused import made no pack"
    );
}

/// The next run's app, a new one over the same directory, finds the pack on the disk.
fn the_pack_is_there_after_a_restart(place: &Setup) {
    let again = new_rig();
    let (_view, cx) = chat_window(&again, place);
    click(cx, "compose-emoji");
    click(cx, "fun-tab-stickers");
    assert!(drawn(cx, "sticker-pack-lab-cats"));
}

#[test]
fn a_pack_is_renamed_and_removed_from_its_menu() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    click(cx, "compose-emoji");
    click(cx, "fun-tab-stickers");
    click(cx, "sticker-pack-lab-cats");
    click(cx, "sticker-pack-menu");
    assert!(drawn(cx, "sticker-pack-rename") && drawn(cx, "sticker-pack-remove"));
    click(cx, "sticker-pack-rename");
    assert!(drawn(cx, "sticker-rename-field"));
    keys(cx, "ctrl-a");
    cx.simulate_input("Office cats");
    keys(cx, "enter");
    let shelf = cx.update(|_, cx| Shelf::of(Some(place.stickers.clone()), cx));
    assert_eq!(cx.update(|_, cx| shelf.read(cx).packs()[0].name.clone()), "Office cats");
    assert_eq!(
        teams_ui::stickers::store::list(&place.stickers)[0].name,
        "Office cats",
        "kept"
    );
    assert!(
        picker(&view, cx).is_some(),
        "Enter in the name field saves, and the picker stays"
    );

    // a name left blank is refused, and Escape leaves the name as it was
    click(cx, "sticker-pack-menu");
    click(cx, "sticker-pack-rename");
    keys(cx, "ctrl-a backspace enter");
    assert!(drawn(cx, "sticker-rename-field"), "still editing");
    keys(cx, "escape");
    assert!(!drawn(cx, "sticker-rename-field"));
    assert!(picker(&view, cx).is_some(), "Escape ends the rename only");

    click(cx, "sticker-pack-menu");
    click(cx, "sticker-pack-remove");
    click(cx, "sticker-pack-remove-cancel");
    assert!(place.stickers.join("lab-cats").is_dir());
    click(cx, "sticker-pack-menu");
    click(cx, "sticker-pack-remove");
    click(cx, "sticker-pack-remove-confirm");
    assert!(!place.stickers.join("lab-cats").exists());
    let picker = picker(&view, cx).unwrap();
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Home
    );
    assert!(drawn(cx, "sticker-packs-empty"));
}

#[test]
fn a_picker_without_stickers_has_no_stickers_tab() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    // New message's box takes no picture: its picker offers no sticker
    let (view, cx) = chat_window(&rig, &place);
    let compose = compose(&view, cx);
    cx.update(|_, cx| compose.update(cx, Compose::for_new_chat));
    click(cx, "compose-emoji");
    assert!(picker(&view, cx).is_some());
    assert!(drawn(cx, "fun-tab-all") && drawn(cx, "fun-tab-emoji"));
    assert!(
        !drawn(cx, "fun-tab-stickers"),
        "New message's picker has no Stickers tab"
    );

    let core = rig.cx.read(|cx| rig.bridge.read(cx).core());
    let stickers = place.stickers.clone();
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (picker, cx) = app.add_window_view(|window, cx| {
        let shelf = Shelf::of(Some(stickers), cx);
        FunPicker::new(core, shelf, false, window, cx)
    });
    draw(cx);
    assert!(drawn(cx, "fun-tab-all") && drawn(cx, "fun-tab-emoji"));
    assert!(!drawn(cx, "fun-tab-stickers"));
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(rows.len(), 2, "emoji only: {rows:?}");
    cx.update(|_, cx| picker.update(cx, |picker, cx| picker.show(Tab::Stickers, cx)));
    assert_eq!(cx.update(|_, cx| picker.read(cx).tab()), Tab::All);
}

#[test]
fn the_search_field_shows_a_magnifier_then_an_x_that_clears_it() {
    let place = Setup::new().with_cats();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    assert!(drawn(cx, "fun-search-icon"), "the empty field's magnifier");
    assert!(!drawn(cx, "fun-search-clear"));
    let unsearched = cx.update(|_, cx| picker.read(cx).rows(cx));
    cx.simulate_input("cat");
    draw(cx);
    assert!(drawn(cx, "fun-search-clear"), "the X, once something is typed");
    assert!(!drawn(cx, "fun-search-icon"));
    assert_ne!(cx.update(|_, cx| picker.read(cx).rows(cx)), unsearched);
    click(cx, "fun-search-clear");
    assert!(self::picker(&view, cx).is_some(), "the X does not close the picker");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).rows(cx)),
        unsearched,
        "the search dropped"
    );
    assert!(drawn(cx, "fun-search-icon") && !drawn(cx, "fun-search-clear"));
    assert_eq!(
        cx.update(|window, cx| picker.read(cx).place(window, cx)),
        Some(Place::Search),
        "typing goes on in the field"
    );
    click(cx, "fun-tab-emoji");
    cx.simulate_input("cat");
    draw(cx);
    click(cx, "fun-search-clear");
    assert!(drawn(cx, "fun-search-icon"), "the Emoji tab's search dropped too");
}

/// A folder `name` of `count` made-up stickers in `root`.
fn many_folder(root: &Path, name: &str, count: u8) -> std::path::PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for index in 0..count {
        let colour = [40 * index, 120, 200 - 20 * index];
        std::fs::write(
            dir.join(format!("dot_{index}.png")),
            picture(120, colour, ImageFormat::Png),
        )
        .unwrap();
    }
    dir
}

/// Every item of the picker's rows lies inside its content's width.
fn rows_fit(picker: &Entity<FunPicker>, cx: &mut VisualTestContext, what: &str) {
    let content = cx.debug_bounds("fun-content").expect("the content is drawn");
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    let mut seen = 0;
    for item in rows.iter().flatten() {
        let selector = item.selector();
        let Some(bounds) = cx.debug_bounds(selector.clone().leak()) else {
            continue;
        };
        seen += 1;
        assert!(
            bounds.left() >= content.left() && bounds.right() <= content.right() + px(0.5),
            "{what}: {selector} at {bounds:?} is cut at the content's edge {content:?}"
        );
    }
    assert!(seen > 2, "{what}: the items are drawn: {rows:?}");
}

#[test]
fn every_grid_fits_the_picker_at_each_font_size() {
    let place = Setup::new().with_cats();
    import::import(
        &many_folder(place.sources.path(), "Lab dots", 6),
        ImportKind::Folder,
        &place.stickers,
        &mut |_| {},
    )
    .unwrap();
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    for font in [14.0, 20.0, 24.0] {
        cx.update(|window, _| window.set_rem_size(px(font)));
        keys(cx, "ctrl-.");
        let picker = picker(&view, cx).unwrap();
        rows_fit(&picker, cx, &format!("All at {font}"));
        click(cx, "fun-tab-stickers");
        rows_fit(&picker, cx, &format!("Stickers at {font}"));
        click(cx, "sticker-pack-lab-dots");
        rows_fit(&picker, cx, &format!("a pack at {font}"));
        keys(cx, "escape");
    }
}

/// A made-up picture's bytes for Teams' sticker `id`'s preview and its own picture, as the engine reads them.
fn teams_pictures(rig: &Rig, ids: &[&str]) {
    let catalog = teams_ui::stickers::teams::catalog();
    let mut pictures = rig.core.pictures.lock().unwrap();
    for id in ids {
        let held = catalog.sticker(id).unwrap();
        for src in [catalog.preview(held), catalog.picture(held)] {
            let bytes = picture(60, [40, 160, 40], ImageFormat::Png);
            pictures.insert(
                src,
                Arc::new(teams_proto::PastedPicture {
                    format: teams_proto::PastedFormat::Png,
                    bytes,
                }),
            );
        }
    }
}

fn teams(id: &str) -> Item {
    Item::Sticker(StickerRef::teams(id))
}

#[test]
fn teams_packs_follow_the_user_s_in_teams_order_and_open_with_their_previews() {
    let place = Setup::new().with_cats();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_Admire"]);
    let (view, cx) = chat_window(&rig, &place);
    click(cx, "compose-emoji");
    let picker = picker(&view, cx).unwrap();
    // All: the user's stickers, then Teams' popular ones
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(
        rows[3],
        [
            Item::Sticker(sticker("lab-cats", "001.png")),
            Item::Sticker(sticker("lab-cats", "002.png")),
            teams("OfficeDrama_OhYeah"),
            teams("doge"),
        ]
    );
    click(cx, "fun-tab-stickers");
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(rows[0], [Item::AddPack, Item::SeeAllPacks]);
    assert_eq!(
        rows[1],
        [Item::Pack("lab-cats".to_owned()), Item::TeamsPack("Popular".to_owned())]
    );
    assert_eq!(
        rows[2],
        [
            Item::TeamsPack("Clippy".to_owned()),
            Item::TeamsPack("OfficeDrama".to_owned())
        ]
    );
    assert_eq!(rows.len(), 3, "the first four packs");
    assert!(drawn(cx, "teams-pack-popular") && drawn(cx, "teams-pack-clippy"));
    click(cx, "sticker-packs-see-all");
    let tiles: Vec<Item> = cx
        .update(|_, cx| picker.read(cx).rows(cx))
        .into_iter()
        .skip(2)
        .flatten()
        .collect();
    assert_eq!(tiles.len(), 19, "the user's pack and Teams' 18: {tiles:?}");
    assert_eq!(tiles.last(), Some(&Item::TeamsPack("Animalopolis".to_owned())));

    a_teams_pack_opens_with_no_menu(&rig, &picker, cx);
}

/// Clippy's pack opened from every pack: Back, then its stickers three to a row, their previews read through the
/// engine; Back goes to every pack again.
fn a_teams_pack_opens_with_no_menu(rig: &Rig, picker: &Entity<FunPicker>, cx: &mut VisualTestContext) {
    click(cx, "teams-pack-clippy");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::TeamsPack {
            id: "Clippy".to_owned(),
            from_packs: true
        }
    );
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(
        rows[0],
        [Item::Back],
        "no menu: Teams' packs are not the user's to change"
    );
    assert_eq!(
        rows[1],
        [
            teams("Clippy_Admire"),
            teams("Clippy_AreYouThere"),
            teams("Clippy_Bandaged")
        ],
        "three to a row, in Teams' order"
    );
    assert!(drawn(cx, "sticker-teams-clippy_admire"));
    assert!(!drawn(cx, "sticker-pack-menu"));
    assert!(
        rig.core.asked.lock().unwrap().contains(&"message_picture"),
        "the previews are read through the engine"
    );
    click(cx, "sticker-back");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::Packs
    );
}

#[test]
fn a_teams_sticker_goes_in_the_box_and_is_sent_by_its_address_with_no_upload() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Teamsquatch_BugSquash"]);
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&teams("Teamsquatch_BugSquash"), window, cx);
        });
    });
    draw(cx);
    assert!(self::picker(&view, cx).is_none(), "a pick closes the picker");
    assert!(drawn(cx, "editor-picture-1"), "it shows in the box once read");
    assert!(
        rig.core.picture_uploads.lock().unwrap().is_empty(),
        "nothing goes to the media service"
    );
    let shelf = cx.update(|_, cx| Shelf::of(Some(place.stickers.clone()), cx));
    assert_eq!(
        cx.update(|_, cx| shelf.read(cx).recent().to_vec()),
        [StickerRef::teams("Teamsquatch_BugSquash")]
    );
    keys(cx, "enter");
    let posted = sent(&rig);
    assert_eq!(posted.len(), 1);
    assert!(posted[0].pictures().is_empty(), "no AMS reference, as Teams sends it");
    // the capture's form, byte for byte (a sticker team-manage sent from a to b, 2026-11-22)
    assert_eq!(
        posted[0].html(),
        "<p>&nbsp;</p><readonly aria-label=\"teamsquatch squashing a bug\" contenteditable=\"false\" \
         itemtype=\"http://schema.skype.com/Sticker\" title=\"teamsquatch squashing a bug\"><img \
         src=\"https://teams.public.onecdn.static.microsoft/evergreen-assets/stickerassets/teamsquatch-250x250/\
         Teamsquatch_BugSquash.png?v=5\" height=\"180\" alt=\"teamsquatch squashing a bug\" width=\"180\" \
         data-image-type=\"standard\" data-image-mode=\"single\" itemtype=\"http://schema.skype.com/Sticker\">\
         </readonly><p>&nbsp;</p>"
    );

    // Recent shows it first now, on the Stickers tab
    keys(cx, "ctrl-.");
    click(cx, "fun-tab-stickers");
    let picker = self::picker(&view, cx).unwrap();
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(rows[0], [teams("Teamsquatch_BugSquash")]);
}

#[test]
fn a_teams_sticker_that_cannot_be_read_says_so() {
    let place = Setup::new();
    let rig = teams_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.activate(&teams("Clippy_Nap"), window, cx)));
    draw(cx);
    let compose = compose(&view, cx);
    assert!(!drawn(cx, "editor-picture-1"));
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some("That sticker can't be read. Check your connection, then try again.".into())
    );
    assert!(
        teams_ui::stickers::store::recent(&place.stickers).is_empty(),
        "a sticker that never reached the box isn't Recent"
    );
}

#[test]
fn recent_shows_the_last_eight_sent() {
    let place = Setup::new();
    let ids: Vec<String> = teams_ui::stickers::teams::catalog()
        .stickers_of("Clippy")
        .into_iter()
        .take(10)
        .map(|held| held.id.clone())
        .collect();
    for id in &ids {
        teams_ui::stickers::store::record_recent(&place.stickers, &StickerRef::teams(id)).unwrap();
    }
    let rig = teams_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    click(cx, "fun-tab-stickers");
    let picker = picker(&view, cx).unwrap();
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    let recent: Vec<Item> = rows[..2].iter().flatten().cloned().collect();
    let last: Vec<Item> = ids.iter().rev().take(8).map(|id| teams(id)).collect();
    assert_eq!(recent, last, "two rows of four, the last sent first");
    assert_eq!(rows[2], [Item::AddPack, Item::SeeAllPacks]);
}

#[test]
fn a_teams_sticker_still_read_goes_with_its_draft_to_another_box() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    let (other_view, other_cx) = chat_window(&rig, &place);
    pick_held_teams_sticker(&rig, &view, "Clippy_HelloThere", cx);
    let (compose, other) = (compose(&view, cx), self::compose(&other_view, other_cx));
    cx.update(|_, cx| compose.update(cx, |compose, cx| compose.hand_draft_to(&other, cx)));
    release_held_pictures(&rig);
    draw(cx);
    draw(other_cx);
    assert!(!drawn(cx, "editor-picture-1"), "the box the draft left");
    assert!(drawn(other_cx, "editor-picture-1"), "the box that holds the draft");
}

#[test]
fn send_waits_for_a_teams_sticker_still_read() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    pick_held_teams_sticker(&rig, &view, "Clippy_HelloThere", cx);
    cx.simulate_input("hi");
    keys(cx, "enter");
    assert!(sent(&rig).is_empty(), "Send waits for the sticker");
    let compose = compose(&view, cx);
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some("Wait for your pictures to upload, or retry or remove a failed one.".into())
    );
    release_held_pictures(&rig);
    draw(cx);
    assert!(drawn(cx, "editor-picture-1"));
    keys(cx, "enter");
    let html = sent(&rig).pop().unwrap().html().to_owned();
    assert!(html.contains("Clippy_HelloThere.gif"), "{html}");
}

/// Picks Teams' sticker `id` in `view`'s box while the engine holds its read.
fn pick_held_teams_sticker(rig: &Rig, view: &Entity<ChatView>, id: &str, cx: &mut VisualTestContext) {
    keys(cx, "ctrl-.");
    let picker = picker(view, cx).unwrap();
    *rig.core.hold_pictures.lock().unwrap() = true;
    cx.update(|window, cx| picker.update(cx, |picker, cx| picker.activate(&teams(id), window, cx)));
    draw(cx);
}

fn release_held_pictures(rig: &Rig) {
    *rig.core.hold_pictures.lock().unwrap() = false;
    let held = std::mem::take(&mut *rig.core.held_pictures.lock().unwrap());
    assert_eq!(held.len(), 1);
    for (src, answer) in held {
        answer.send(Ok(rig.core.pictures.lock().unwrap().get(&src).cloned()));
    }
}

#[test]
fn an_edit_waits_for_a_teams_sticker_still_read() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    pick_held_teams_sticker(&rig, &view, "Clippy_HelloThere", cx);
    let compose = compose(&view, cx);
    let events = Rc::new(RefCell::new(Vec::new()));
    let seen = Rc::clone(&events);
    cx.update(|_, cx| {
        cx.subscribe(&compose, move |_, event: &ComposeEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    keys(cx, "up");
    assert!(!events.borrow().contains(&ComposeEvent::EditLast), "Up starts no edit");
    let loaded = Loaded {
        doc: Doc::default(),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    assert_eq!(cx.update(|_, cx| compose.read(cx).mode().clone()), Mode::New);
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).problem().cloned()),
        Some(STICKER_LOADING_FIRST.into())
    );
    release_held_pictures(&rig);
    draw(cx);
    assert!(drawn(cx, "editor-picture-1"), "the sticker stays in the new message");
}

#[test]
fn an_edit_called_off_drops_the_teams_sticker_it_still_reads() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    let compose = compose(&view, cx);
    let loaded = Loaded {
        doc: Doc::default(),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    pick_held_teams_sticker(&rig, &view, "Clippy_HelloThere", cx);
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.cancel(window, cx)));
    assert_eq!(cx.update(|_, cx| compose.read(cx).mode().clone()), Mode::New);
    release_held_pictures(&rig);
    draw(cx);
    assert!(
        !drawn(cx, "editor-picture-1"),
        "the new message doesn't get the edit's sticker"
    );
    assert!(
        teams_ui::stickers::store::recent(&place.stickers).is_empty(),
        "a sticker that never reached the box isn't Recent"
    );
}

#[test]
fn a_refused_edit_does_not_come_back_over_a_teams_sticker_still_read() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    let compose = compose(&view, cx);
    let loaded = Loaded {
        doc: Doc::default(),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    cx.simulate_input("edited");
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    rig.core.write_errors.lock().unwrap().push_back(EngineError::Stopped);
    *rig.core.hold_pictures.lock().unwrap() = true;
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| compose.send(window, cx));
        picker.update(cx, |picker, cx| {
            picker.activate(&teams("Clippy_HelloThere"), window, cx);
        });
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).mode().clone()),
        Mode::New,
        "the refused edit stays out of a box that reads a new sticker"
    );
    release_held_pictures(&rig);
    draw(cx);
    assert!(drawn(cx, "editor-picture-1"), "the sticker stays in the new message");
}

#[test]
fn a_box_with_only_a_teams_sticker_still_read_survives_a_chat_switch() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig
            .cx
            .update(|cx| cx.new(|_| SettingsModel::load(SettingsFile::new(place.settings.clone())))),
        trace: Arc::new(ViewTrace::new(Written::default())),
    };
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (shell, cx) = app.add_window_view(|window, cx| Shell::new(parts, window, cx));
    cx.simulate_resize(size(px(800.0), px(700.0)));
    cx.update(|window, _| window.activate_window());
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let chat = |cx: &mut VisualTestContext| cx.update(|_, cx| shell.read(cx).chat().unwrap().clone());
    let view = chat(cx);
    let first = compose(&view, cx);
    cx.update(|window, cx| first.update(cx, |compose, cx| compose.focus(window, cx)));
    draw(cx);
    pick_held_teams_sticker(&rig, &view, "Clippy_HelloThere", cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(OTHER.to_owned(), window, cx)));
    draw(cx);
    release_held_pictures(&rig);
    draw(cx);
    cx.update(|window, cx| shell.update(cx, |shell, cx| shell.open_chat(AB.to_owned(), window, cx)));
    draw(cx);
    let view = chat(cx);
    assert_eq!(compose(&view, cx), first, "the box is kept while its sticker is read");
    assert!(drawn(cx, "editor-picture-1"));
}

#[test]
fn the_search_finds_teams_stickers_after_the_user_s_and_the_keyboard_picks_one() {
    let place = Setup::new().with_cats();
    let rig = teams_rig();
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    click(cx, "fun-tab-stickers");
    cx.simulate_input("hello");
    draw(cx);
    let picker = picker(&view, cx).unwrap();
    let found: Vec<Item> = cx
        .update(|_, cx| picker.read(cx).rows(cx))
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(
        found,
        [
            teams("Clippy_HelloThere"),
            teams("OfficeDrama_Hey"),
            teams("Broccoli_Wave"),
            teams("Bee_Hi"),
            teams("WordArt_Hello"),
            teams("Animalopolis_weird_lemur"),
        ],
        "as Teams' search found them"
    );
    keys(cx, "down");
    let current = |cx: &mut VisualTestContext| cx.update(|window, cx| picker.read(cx).current(window, cx));
    assert_eq!(current(cx), Some(teams("Clippy_HelloThere")));
    keys(cx, "enter");
    assert!(self::picker(&view, cx).is_none());
    assert!(drawn(cx, "editor-picture-1"));
    keys(cx, "enter");
    let html = sent(&rig).pop().unwrap().html().to_owned();
    assert!(
        html.contains("stickerassets/clippy-250x250/en-us/Clippy_HelloThere.gif?v=5"),
        "{html}"
    );
    // the user's stickers come first
    keys(cx, "ctrl-.");
    cx.simulate_input("cat");
    draw(cx);
    click(cx, "fun-tab-stickers");
    let picker = self::picker(&view, cx).unwrap();
    let found: Vec<Item> = cx
        .update(|_, cx| picker.read(cx).rows(cx))
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(
        found[..2],
        [
            Item::Sticker(sticker("lab-cats", "001.png")),
            Item::Sticker(sticker("lab-cats", "002.png"))
        ]
    );
    assert!(
        found[2..]
            .iter()
            .all(|item| matches!(item, Item::Sticker(found) if found.is_teams()))
    );
}

#[test]
fn the_keyboard_opens_a_teams_pack_and_picks_its_sticker() {
    let place = Setup::new();
    let rig = teams_rig();
    teams_pictures(&rig, &["OfficeDrama_OhYeah"]);
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    let current = |cx: &mut VisualTestContext| cx.update(|window, cx| picker.read(cx).current(window, cx));
    keys(cx, "up end down down");
    assert_eq!(current(cx), Some(Item::AddPack));
    keys(cx, "down");
    assert_eq!(current(cx), Some(Item::TeamsPack("Popular".to_owned())));
    keys(cx, "enter");
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
        StickerView::TeamsPack {
            id: "Popular".to_owned(),
            from_packs: false
        }
    );
    keys(cx, "down");
    assert_eq!(
        current(cx),
        Some(teams("OfficeDrama_OhYeah")),
        "Back, then the stickers: no menu"
    );
    keys(cx, "space");
    assert!(self::picker(&view, cx).is_none(), "Space picks");
    assert!(drawn(cx, "editor-picture-1"));
}

#[test]
fn an_own_server_account_sees_only_the_user_s_packs() {
    let place = Setup::new().with_cats();
    teams_ui::stickers::store::record_recent(&place.stickers, &sticker("lab-cats", "002.png")).unwrap();
    // a work account's Teams stickers after it, more than the file keeps of them, keep the user's own in Recent
    let catalog = teams_ui::stickers::teams::catalog();
    let clippy = catalog.stickers_of("Clippy");
    assert!(clippy.len() > teams_ui::stickers::store::RECENT_KEPT);
    for held in clippy {
        teams_ui::stickers::store::record_recent(&place.stickers, &StickerRef::teams(&held.id)).unwrap();
    }
    let rig = new_rig();
    let (view, cx) = chat_window(&rig, &place);
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    let all: Vec<Item> = cx
        .update(|_, cx| picker.read(cx).rows(cx))
        .into_iter()
        .flatten()
        .collect();
    assert!(
        all.iter()
            .all(|item| !matches!(item, Item::Sticker(found) if found.is_teams())),
        "no Teams sticker in All, Recent's neither: {all:?}"
    );
    click(cx, "fun-tab-stickers");
    let rows = cx.update(|_, cx| picker.read(cx).rows(cx));
    assert_eq!(
        rows[0],
        [Item::Sticker(sticker("lab-cats", "002.png"))],
        "Recent: the user's only"
    );
    assert!(
        rows.iter().flatten().all(|item| !matches!(item, Item::TeamsPack(_))),
        "no Teams pack: {rows:?}"
    );
    assert!(!drawn(cx, "teams-pack-popular"));
    cx.simulate_input("hello");
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| picker.read(cx).rows(cx)),
        Vec::<Vec<Item>>::new(),
        "the search finds none of Teams'"
    );
    // a Teams sticker asked for anyway goes nowhere: its picture isn't even read
    teams_pictures(&rig, &["Clippy_HelloThere"]);
    rig.core.asked.lock().unwrap().clear();
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&teams("Clippy_HelloThere"), window, cx);
        });
    });
    draw(cx);
    assert!(!drawn(cx, "editor-picture-1"));
    assert!(sent(&rig).is_empty());
    assert!(!rig.core.asked.lock().unwrap().contains(&"message_picture"));
    let compose = compose(&view, cx);
    assert_eq!(cx.update(|_, cx| compose.read(cx).problem().cloned()), None);
}

#[test]
fn a_refused_edit_comes_back_once_the_teams_sticker_read_fails() {
    let place = Setup::new();
    let rig = teams_rig();
    let (view, cx) = chat_window(&rig, &place);
    let compose = compose(&view, cx);
    let loaded = Loaded {
        doc: Doc::default(),
        quotes: Vec::new(),
    };
    cx.update(|window, cx| compose.update(cx, |compose, cx| compose.edit("1".to_owned(), loaded, window, cx)));
    cx.simulate_input("edited");
    keys(cx, "ctrl-.");
    let picker = picker(&view, cx).unwrap();
    rig.core.write_errors.lock().unwrap().push_back(EngineError::Stopped);
    *rig.core.hold_pictures.lock().unwrap() = true;
    cx.update(|window, cx| {
        compose.update(cx, |compose, cx| compose.send(window, cx));
        picker.update(cx, |picker, cx| {
            picker.activate(&teams("Clippy_HelloThere"), window, cx);
        });
    });
    draw(cx);
    assert_eq!(cx.update(|_, cx| compose.read(cx).mode().clone()), Mode::New);
    release_held_pictures(&rig);
    draw(cx);
    assert!(!drawn(cx, "editor-picture-1"), "the sticker could not be read");
    assert_eq!(
        cx.update(|_, cx| compose.read(cx).mode().clone()),
        Mode::Edit {
            message_id: "1".to_owned(),
            quotes: Vec::new(),
        },
        "the refused edit comes back to the empty box"
    );
    assert_eq!(cx.update(|_, cx| compose.read(cx).value(cx)), "edited");
}
