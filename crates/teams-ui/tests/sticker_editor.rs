//! Teams' "Edit sticker" in the compose box's picker (`teams_ui::sticker_editor`) on GPUI's test context, over the
//! fake engine (`common`): a captioned sticker's pencil mark and its editor, the caption drawn as it is typed, Done
//! putting the drawn picture in the box (sent to the media service at once) and Send posting it as a sticker, a meme's
//! two lines, the keyboard's reach, Cancel, and a picture that can't be read. The templates are made-up pictures; the
//! stickers' ids and words are Teams'.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

#[expect(dead_code, reason = "the shared stand-ins hold more than this test uses")]
mod common;

use std::sync::Arc;

use common::{LAB_A, LAB_B, Rig, Written};
use gpui::{AppContext as _, Entity, Modifiers, VisualTestContext, px, size};
use image::{ImageFormat, Rgba, RgbaImage};
use teams_core::Action;
use teams_proto::chat::MessageText;
use teams_store::Member;
use teams_ui::chat_view::ChatView;
use teams_ui::compose::Compose;
use teams_ui::fun_picker::{FunPicker, Item, Place, StickerView};
use teams_ui::settings::SettingsModel;
use teams_ui::sticker_editor::{self, Press, StickerEditor};
use teams_ui::stickers::{Shelf, StickerRef};
use teams_ui::{SettingsFile, ShellParts, ViewTrace};

const AB: &str = "19:00000000-0000-4000-8000-00000000000a_00000000-0000-4000-8000-00000000000b@unq.gbl.spaces";

/// A made-up picture of `width` by `height`.
fn picture(width: u32, height: u32) -> Vec<u8> {
    let picture = RgbaImage::from_pixel(width, height, Rgba([230, 230, 230, 255]));
    let mut bytes = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    bytes
}

fn draw(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.executor().advance_clock(sticker_editor::SETTLE);
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }
    cx.run_until_parked();
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

/// An account of Microsoft's cloud, with the a–b chat, the previews of `ids` and their templates (`width` by
/// `height`) readable through the engine.
fn rig(ids: &[&str], width: u32, height: u32) -> Rig {
    let rig = Rig::start();
    rig.core
        .members
        .lock()
        .unwrap()
        .insert(AB.to_owned(), vec![Member::new(LAB_A, None), Member::new(LAB_B, None)]);
    rig.cx.update(teams_ui::window::init);
    let catalog = teams_ui::stickers::teams::catalog();
    let mut pictures = rig.core.pictures.lock().unwrap();
    for id in ids {
        let held = catalog.sticker(id).unwrap();
        let shown = [(catalog.preview(held), picture(60, 60))];
        let template = catalog.template(held).map(|src| (src, picture(width, height)));
        for (src, bytes) in shown.into_iter().chain(template) {
            let format = teams_proto::PastedFormat::Png;
            pictures.insert(src, Arc::new(teams_proto::PastedPicture { format, bytes }));
        }
    }
    drop(pictures);
    rig
}

/// The a–b chat's view in a window of its own, its settings in `dir`, the compose box focused.
fn chat_window(rig: &Rig, dir: &std::path::Path) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let settings_file = SettingsFile::new(dir.join("settings.toml"));
    let parts = ShellParts {
        core: rig.cx.read(|cx| rig.bridge.read(cx).core()),
        models: rig.models.clone(),
        settings: rig.cx.update(|cx| cx.new(|_| SettingsModel::load(settings_file))),
        trace: Arc::new(ViewTrace::new(Written::default())),
    };
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (view, cx) = app.add_window_view(|window, cx| ChatView::new(parts, AB.to_owned(), window, cx));
    cx.simulate_resize(size(px(800.0), px(760.0)));
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

fn editor(picker: &Entity<FunPicker>, cx: &mut VisualTestContext) -> Option<Entity<StickerEditor>> {
    cx.update(|_, cx| picker.read(cx).sticker_editor().cloned())
}

fn typed(editor: &Entity<StickerEditor>, cx: &mut VisualTestContext) -> Vec<String> {
    cx.update(|_, cx| editor.read(cx).typed(cx))
}

fn can_finish(editor: &Entity<StickerEditor>, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| editor.read(cx).can_finish(cx))
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

/// Opens Teams' pack `pack` in the picker.
fn open_pack(view: &Entity<ChatView>, pack: &str, cx: &mut VisualTestContext) -> Entity<FunPicker> {
    keys(cx, "ctrl-.");
    let picker = picker(view, cx).unwrap();
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::TeamsPack(pack.to_owned()), window, cx);
        });
    });
    draw(cx);
    picker
}

#[test]
fn a_captioned_sticker_is_edited_put_in_the_box_and_sent_as_a_sticker() {
    let dir = tempfile::tempdir().unwrap();
    // the template of "So cool" is 334 by 250, as on Teams' CDN
    let rig = rig(&["OfficeDrama_SoCool"], 334, 250);
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "OfficeDrama", cx);
    assert!(drawn(cx, "sticker-edit-mark"), "its stickers have Teams' pencil mark");
    click(
        cx,
        &teams_ui::fun_picker::sticker_selector(&StickerRef::teams("OfficeDrama_SoCool")),
    );

    let editor = editor(&picker, cx).expect("a click opens Edit sticker");
    assert!(picker_open(&view, cx), "the picker stays open");
    assert!(
        drawn(cx, "sticker-editor") && drawn(cx, "fun-tab-stickers"),
        "in the place of the stickers"
    );
    assert_eq!(
        typed(&editor, cx),
        ["THAT'S SO COOL"],
        "the field starts with the sticker's words"
    );
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).drawn()),
        Some((334, 250)),
        "the template, drawn"
    );
    assert!(
        rig.core.asked.lock().unwrap().contains(&"message_picture"),
        "the template is read through the engine"
    );
    assert!(can_finish(&editor, cx));

    done_puts_the_drawn_sticker_in_the_box(&rig, &view, &editor, dir.path(), cx);
}

/// "Go team" typed in "So cool"'s editor, then Done: the drawn PNG in the box, sent to the media service at once and
/// first in Recent; Send posts it as a sticker.
fn done_puts_the_drawn_sticker_in_the_box(
    rig: &Rig,
    view: &Entity<ChatView>,
    editor: &Entity<StickerEditor>,
    dir: &std::path::Path,
    cx: &mut VisualTestContext,
) {
    keys(cx, "ctrl-a");
    cx.simulate_input("Go team");
    draw(cx);
    assert_eq!(typed(editor, cx), ["Go team"]);
    click(cx, Press::Done.selector());
    assert!(picker(view, cx).is_none(), "Done closes the picker");
    assert!(drawn(cx, "editor-picture-1"), "the drawn sticker is in the box");
    let uploads = rig.core.picture_uploads.lock().unwrap().clone();
    assert_eq!(uploads.len(), 1, "sent to the media service at once");
    assert_eq!(uploads[0].1.format, teams_proto::PastedFormat::Png);
    let sent_picture = image::load_from_memory(&uploads[0].1.bytes).unwrap();
    assert_eq!(
        (sent_picture.width(), sent_picture.height()),
        (334, 250),
        "at the template's size"
    );
    assert_ne!(uploads[0].1.bytes, picture(334, 250), "with the caption drawn in");
    let shelf = cx.update(|_, cx| Shelf::of(Some(dir.join("stickers")), cx));
    assert_eq!(
        cx.update(|_, cx| shelf.read(cx).recent().to_vec()),
        [StickerRef::teams("OfficeDrama_SoCool")],
        "Recent has it, as in Teams"
    );

    keys(cx, "enter");
    let posted = sent(rig);
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].pictures(), ["0-made-up-d1-1"]);
    // shown at 240 by 180, labelled by its caption, its picture's words as Teams' box gives them
    assert_eq!(
        posted[0].html(),
        "<p>&nbsp;</p><readonly aria-label=\"Go team\" contenteditable=\"false\" \
         itemtype=\"http://schema.skype.com/Sticker\" title=\"Go team\"><img \
         src=\"https://us-api.asm.skype.com/v1/objects/0-made-up-d1-1/views/imgo\" \
         itemtype=\"http://schema.skype.com/AMSImage\" width=\"240\" height=\"180\" alt=\"Sticker image, Go team\" \
         id=\"x_0-made-up-d1-1\" itemid=\"0-made-up-d1-1\"></readonly><p>&nbsp;</p>"
    );
}

fn picker_open(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> bool {
    picker(view, cx).is_some()
}

#[test]
fn a_memes_two_lines_are_reached_by_the_keyboard_and_done_by_enter() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["grumpycat"], 250, 249);
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "Meme", cx);
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::Sticker(StickerRef::teams("grumpycat")), window, cx);
        });
    });
    draw(cx);
    let editor = editor(&picker, cx).unwrap();
    assert!(drawn(cx, "sticker-editor-top") && drawn(cx, "sticker-editor-bottom"));
    assert!(!drawn(cx, "sticker-editor-caption"));
    assert_eq!(typed(&editor, cx), ["", ""], "a meme's lines start empty");
    assert!(!can_finish(&editor, cx), "Done waits for words");
    click(cx, Press::Done.selector());
    assert!(
        editor_open(&picker, cx),
        "a click on Done while it isn't offered does nothing"
    );

    // the top line had the focus as it opened; the click took it to the panel, so a click gives it back
    click(cx, "sticker-editor-top");
    cx.simulate_input("such wow");
    draw(cx);
    assert_eq!(typed(&editor, cx), ["such wow", ""]);
    assert!(can_finish(&editor, cx));
    keys(cx, "tab");
    cx.simulate_input("very cat");
    draw(cx);
    assert_eq!(
        typed(&editor, cx),
        ["such wow", "very cat"],
        "Tab goes to the bottom line"
    );
    // Tab: Done, then Cancel, then the back arrow, then the search field, then the top line again
    keys(cx, "tab tab tab tab tab");
    cx.simulate_input("!");
    draw(cx);
    let lines = typed(&editor, cx);
    assert!(
        lines[0].contains('!') && lines[1] == "very cat",
        "round to the top line: {lines:?}"
    );
    // Shift+Tab: the search field, the back arrow, Cancel, then Done
    keys(cx, "shift-tab shift-tab shift-tab shift-tab");
    keys(cx, "enter");
    assert!(self::picker(&view, cx).is_none(), "Enter on Done puts it in the box");
    assert!(drawn(cx, "editor-picture-1"));
    keys(cx, "enter");
    let posted = sent(&rig);
    assert!(
        posted[0].html().contains("aria-label=\"grumpycat such wow"),
        "a meme is named by its name and its lines: {}",
        posted[0].html()
    );
    assert!(posted[0].html().contains("width=\"181\" height=\"180\""));
}

fn editor_open(picker: &Entity<FunPicker>, cx: &mut VisualTestContext) -> bool {
    editor(picker, cx).is_some()
}

#[test]
fn cancel_the_back_arrow_and_escape_go_back_to_the_stickers() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["Devs_Approved"], 300, 250);
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "Devs", cx);
    let open = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            picker.update(cx, |picker, cx| {
                picker.activate(&Item::Sticker(StickerRef::teams("Devs_Approved")), window, cx);
            });
        });
        draw(cx);
    };
    for close in [
        |cx: &mut VisualTestContext| click(cx, Press::Cancel.selector()),
        |cx: &mut VisualTestContext| click(cx, Press::Back.selector()),
        |cx: &mut VisualTestContext| keys(cx, "escape"),
    ] {
        open(cx);
        assert!(editor_open(&picker, cx));
        close(cx);
        assert!(!editor_open(&picker, cx));
        assert!(picker_open(&view, cx), "the picker stays open");
        assert_eq!(
            cx.update(|_, cx| picker.read(cx).sticker_view().clone()),
            StickerView::TeamsPack {
                id: "Devs".to_owned(),
                from_packs: false
            }
        );
    }
    assert!(!drawn(cx, "editor-picture-1"), "nothing went in the box");
    assert!(rig.core.picture_uploads.lock().unwrap().is_empty());
}

#[test]
fn an_empty_caption_offers_no_done_and_space_on_cancel_closes() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["Legal_Non_Sequitur"], 376, 251);
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "Legal", cx);
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::Sticker(StickerRef::teams("Legal_Non_Sequitur")), window, cx);
        });
    });
    draw(cx);
    let editor = editor(&picker, cx).unwrap();
    assert_eq!(typed(&editor, cx), ["NON SEQUITUR"]);
    cx.update(|window, cx| editor.update(cx, |editor, cx| editor.set_typed(&["   "], window, cx)));
    draw(cx);
    assert!(!can_finish(&editor, cx), "words only of spaces are none");
    // with no words, Tab from the field skips Done, which isn't offered, to Cancel; Space presses it
    cx.update(|window, cx| editor.update(cx, |editor, cx| editor.set_typed(&[""], window, cx)));
    draw(cx);
    keys(cx, "tab space");
    assert!(!editor_open(&picker, cx), "Space on Cancel closes it");
}

#[test]
fn a_template_that_cannot_be_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&[], 1, 1);
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "OfficeDrama", cx);
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::Sticker(StickerRef::teams("OfficeDrama_Hey")), window, cx);
        });
    });
    draw(cx);
    let editor = editor(&picker, cx).unwrap();
    assert!(cx.update(|_, cx| editor.read(cx).failed()));
    assert!(!can_finish(&editor, cx));
    assert!(!drawn(cx, Press::Done.selector()), "no Done");
    assert!(drawn(cx, Press::Back.selector()), "the back arrow stays");
    assert_eq!(sticker_editor::LOAD_FAILED, "Failed to load image");
}

#[test]
fn a_sticker_with_no_caption_still_goes_straight_in() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&[], 1, 1);
    let catalog = teams_ui::stickers::teams::catalog();
    let held = catalog.sticker("Teamsquatch_BugSquash").unwrap();
    let format = teams_proto::PastedFormat::Png;
    rig.core.pictures.lock().unwrap().insert(
        catalog.picture(held),
        Arc::new(teams_proto::PastedPicture {
            format,
            bytes: picture(60, 60),
        }),
    );
    let (view, cx) = chat_window(&rig, dir.path());
    let picker = open_pack(&view, "Teamsquatch", cx);
    assert!(
        !drawn(cx, "sticker-edit-mark"),
        "no pencil mark on a pack with no captions"
    );
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::Sticker(StickerRef::teams("Teamsquatch_BugSquash")), window, cx);
        });
    });
    draw(cx);
    assert!(!picker_open(&view, cx), "no editor: it goes in the box");
    assert!(drawn(cx, "editor-picture-1"));
}

/// "So cool"'s editor, open in the a–b chat, its caption field focused.
fn so_cool(
    rig: &Rig,
    dir: &std::path::Path,
) -> (Entity<FunPicker>, Entity<StickerEditor>, &'static mut VisualTestContext) {
    let (view, cx) = chat_window(rig, dir);
    let picker = open_pack(&view, "OfficeDrama", cx);
    cx.update(|window, cx| {
        picker.update(cx, |picker, cx| {
            picker.activate(&Item::Sticker(StickerRef::teams("OfficeDrama_SoCool")), window, cx);
        });
    });
    draw(cx);
    let editor = editor(&picker, cx).unwrap();
    (picker, editor, cx)
}

#[test]
fn a_caption_keeps_its_first_150_characters_and_refuses_more_keys() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["OfficeDrama_SoCool"], 334, 250);
    let (_, editor, cx) = so_cool(&rig, dir.path());
    let first = "made up words ".repeat(11);
    let first: String = first.chars().take(149).collect();
    keys(cx, "ctrl-a");
    cx.simulate_input(&first);
    draw(cx);
    for key in ["a", "b", "c"] {
        cx.simulate_input(key);
        draw(cx);
    }
    let full = format!("{first}a");
    assert_eq!(typed(&editor, cx), [full.as_str()], "the keys past 150 are refused");
    // a key at the start of a full caption is refused too: the end is not cut
    keys(cx, "ctrl-home");
    cx.simulate_input("Z");
    draw(cx);
    assert_eq!(typed(&editor, cx), [full.as_str()]);
    // with room again, a key goes where the cursor is
    keys(cx, "ctrl-end backspace");
    cx.simulate_input("Q");
    draw(cx);
    assert_eq!(typed(&editor, cx), [format!("{first}Q")]);
}

#[test]
fn fast_typing_draws_the_picture_once_the_keys_settle() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["OfficeDrama_SoCool"], 334, 250);
    let (_, editor, cx) = so_cool(&rig, dir.path());
    assert!(!cx.update(|_, cx| editor.read(cx).drawing()));
    keys(cx, "ctrl-a");
    for key in "a fast typist".chars() {
        cx.simulate_input(&key.to_string());
        cx.run_until_parked();
    }
    assert!(
        cx.update(|_, cx| editor.read(cx).drawing()),
        "no draw starts before the keys settle"
    );
    // the keys went on after the draw was asked for: it waits for the fields to stay unchanged a whole SETTLE
    cx.executor().advance_clock(sticker_editor::SETTLE);
    cx.run_until_parked();
    assert!(cx.update(|_, cx| editor.read(cx).drawing()));
    cx.executor().advance_clock(sticker_editor::SETTLE);
    cx.run_until_parked();
    assert!(
        !cx.update(|_, cx| editor.read(cx).drawing()),
        "one draw takes all the keys typed"
    );
    assert_eq!(cx.update(|_, cx| editor.read(cx).drawn()), Some((334, 250)));
}

#[test]
fn the_top_row_is_the_back_arrow_beside_the_search_field() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig(&["OfficeDrama_SoCool"], 334, 250);
    let (picker, _, cx) = so_cool(&rig, dir.path());
    let back = cx.debug_bounds(Press::Back.selector()).unwrap();
    let search = cx
        .debug_bounds("fun-search")
        .expect("the search field is in the editor's top row, as in Teams");
    let picture = cx.debug_bounds("sticker-editor-picture").unwrap();
    assert!(search.left() > back.right(), "beside the back arrow");
    assert!(
        search.top() < back.center().y && back.center().y < search.bottom(),
        "on one row"
    );
    assert!(search.bottom() <= picture.top(), "above the picture");
    // Shift+Tab from the caption goes to the search field; a search there shows what it finds
    keys(cx, "shift-tab");
    cx.simulate_input("cool");
    draw(cx);
    assert!(!editor_open(&picker, cx), "a search closes the editor");
    assert_eq!(
        cx.update(|window, cx| picker.read(cx).place(window, cx)),
        Some(Place::Search),
        "the typing stays in the search field"
    );
    let found = cx.update(|_, cx| picker.read(cx).rows(cx)).concat();
    assert!(
        found.contains(&Item::Sticker(StickerRef::teams("OfficeDrama_SoCool"))),
        "{found:?}"
    );
}
