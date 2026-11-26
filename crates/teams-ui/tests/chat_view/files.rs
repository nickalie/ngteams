//! The files a message carries, drawn as Teams draws them (`teams_ui::files`), on GPUI's test context over the fake
//! engine: a card per file under the text, side by side and wrapping when the window is narrow, its picture 16:9 of
//! the card's width however narrow; a video's card holding its player, the video's thumbnail asked of the engine by
//! its sharing link and shown alone in it until it plays (the controls only while the pointer is on it; the large
//! video icon without a thumbnail), a document's card the large icon of its type; Teams' type icons and header
//! buttons; a click on the name opens the sharing link, "More options" offers Teams' items; Share (the video card's
//! button and the item) asks for "Share this file" for its file, and the read-only app offers no Share;
//! an own server's file names its owner; a channel's cards show no location; nothing about the files is said to be unshown; an image with a preview is
//! drawn as the picture alone, its card when the preview can't be had. The message is the synthetic fixture
//! `tests/fixtures/chatsvc/files.json`: every name, link and id is made up.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Bounds, Entity, Modifiers, Pixels, VisualTestContext, px, size};
use serde_json::Value;
use teams_proto::chat::shared_files;
use teams_store::Message;
use teams_ui::ViewTrace;
use teams_ui::chat_view::{ChatView, ChatViewEvent, Place, UnshownParts};
use teams_ui::settings::Settings;

use super::common::{LAB_A, LAB_B, Rig};
use super::{NOON, chat_window, draw, items, message, rig_with, trace};

const FIXTURE: &str = include_str!("../../../../tests/fixtures/chatsvc/files.json");
const GRID: &str = include_str!("../../../../tests/fixtures/chatsvc/files-grid.json");
const ID: &str = "301";

/// The fixture's message, from b, as the store keeps it (its `properties` JSON text).
fn files_message() -> Message {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let read_back = &fixture["read_back"];
    let mut stored = message(ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    stored.properties = Some(read_back["properties"].to_string());
    stored
}

/// The sharing links of the fixture's files, in order.
fn share_urls() -> Vec<String> {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    shared_files(Some(&fixture["read_back"]["properties"]["files"]))
        .files
        .into_iter()
        .map(|file| file.share_url.unwrap())
        .collect()
}

/// A made-up PNG thumbnail, 16 by 9.
fn thumbnail() -> teams_core::PictureBytes {
    let mut bytes = Vec::new();
    image::RgbaImage::from_pixel(160, 90, image::Rgba([30, 160, 60, 255]))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    Arc::new(teams_proto::PastedPicture {
        format: teams_proto::PastedFormat::Png,
        bytes,
    })
}

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    draw(cx);
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let found = bounds(cx, selector).unwrap_or_else(|| panic!("{selector} is drawn"));
    cx.simulate_click(found.center(), Modifiers::none());
    draw(cx);
}

fn rig() -> Rig {
    rig_with(vec![files_message()])
}

/// The chat with the fixture's message, the engine holding the video's thumbnail.
fn shown_with_thumbnail() -> (Rig, Entity<ChatView>, &'static mut VisualTestContext) {
    let rig = rig();
    rig.core
        .thumbnails
        .lock()
        .unwrap()
        .insert(share_urls()[0].clone(), thumbnail());
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    (rig, view, cx)
}

#[test]
fn a_messages_files_are_cards_side_by_side_under_its_text() {
    let (_rig, view, cx) = shown_with_thumbnail();
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files.len(), 2);
    assert_eq!(
        item.unshown,
        UnshownParts::default(),
        "every file read: nothing said unshown"
    );
    assert!(bounds(cx, &format!("unshown-{ID}")).is_none());

    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    let document = bounds(cx, &format!("file-card-{ID}-1")).expect("the document's card");
    // side by side, Teams' width each
    assert!(
        document.origin.x > video.origin.x + video.size.width - px(1.0),
        "{video:?} {document:?}"
    );
    assert_eq!(document.origin.y, video.origin.y);
    assert!(
        (f32::from(video.size.width) - teams_ui::files::CARD_WIDTH).abs() < 1.0,
        "{video:?}"
    );
    // under the text
    let text = bounds(cx, &format!("bubble-{ID}")).expect("the bubble");
    assert!(video.origin.y > text.origin.y + px(40.0), "{video:?} in {text:?}");
    // a document's card has its picture too: the large icon of its type, as Teams draws a zip
    assert_eq!(document.size.height, video.size.height);
    assert!(bounds(cx, &format!("file-thumbnail-none-{ID}-1")).is_some());
    // the name stays within its card, cut with an ellipsis rather than widening it
    let name = bounds(cx, &format!("file-name-{ID}-0")).expect("the name");
    assert!(
        name.origin.x + name.size.width <= video.origin.x + video.size.width,
        "{name:?}"
    );
}

#[test]
fn a_videos_thumbnail_is_asked_of_the_engine_and_shown_in_its_player_and_a_click_on_its_name_opens_it() {
    let (rig, _view, cx) = shown_with_thumbnail();
    let shares = share_urls();
    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    // the video's card holds its player, its thumbnail (asked of the engine by its sharing link) standing in until it
    // plays, the card's width; the document has neither
    let player = bounds(cx, &format!("video-player-{ID}-0")).expect("the video's player");
    assert!(
        player.size.width >= video.size.width - px(3.0),
        "{player:?} in {video:?}"
    );
    let asked = rig.core.asked();
    assert_eq!(asked.iter().filter(|asked| **asked == "file_thumbnail").count(), 1);
    let poster = bounds(cx, &format!("video-poster-{ID}-0")).expect("the thumbnail is drawn");
    assert!(
        poster.size.width >= video.size.width - px(3.0),
        "{poster:?} in {video:?}"
    );
    for none in ["video-player", "video-poster", "file-thumbnail-reading"] {
        assert!(bounds(cx, &format!("{none}-{ID}-1")).is_none(), "{none}");
    }

    // a click on the name opens the file at its sharing link, through GPUI's opener; another file's name opens its own
    click(cx, &format!("file-name-{ID}-0"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[0].as_str()));
    click(cx, &format!("file-name-{ID}-1"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[1].as_str()));
}

#[test]
fn a_video_without_a_thumbnail_shows_the_large_video_icon_and_narrow_cards_wrap() {
    let rig = rig();
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    cx.simulate_resize(size(px(480.0), px(700.0)));
    draw(cx);
    assert!(bounds(cx, &format!("video-player-{ID}-0")).is_some(), "the player");
    assert!(bounds(cx, &format!("video-poster-{ID}-0")).is_none(), "no thumbnail");
    // never a black box with the controls over it: the large video icon on the picture's shade, the controls hidden
    assert!(bounds(cx, &format!("video-no-thumbnail-{ID}-0")).is_some());
    assert!(bounds(cx, &format!("video-icon-{ID}-0 teams")).is_some());
    assert!(bounds(cx, &format!("video-controls-{ID}-0 on-hover")).is_some());
    let video = bounds(cx, &format!("file-card-{ID}-0")).expect("the video's card");
    let document = bounds(cx, &format!("file-card-{ID}-1")).expect("the document's card");
    assert!(
        document.origin.y >= video.origin.y + video.size.height,
        "wrapped: {video:?} {document:?}"
    );
}

#[test]
fn more_options_download_the_file_and_copy_its_link() {
    let rig = rig();
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    click(cx, &format!("file-more-{ID}-1"));
    assert!(
        bounds(cx, &format!("file-menu-open-{ID}-1")).is_some(),
        "Open in browser"
    );
    click(cx, &format!("file-menu-download-{ID}-1"));
    assert_eq!(cx.opened_url(), Some(format!("{}?download=1", shares[1])));
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-copy-{ID}-0"));
    assert_eq!(copied(cx).as_deref(), Some(shares[0].as_str()));
}

/// What the clipboard holds.
fn copied(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

/// What `view` tells, from now on.
fn told(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<ChatViewEvent>>> {
    let told: Rc<RefCell<Vec<ChatViewEvent>>> = Rc::default();
    let kept = Rc::clone(&told);
    cx.update(|_, cx| {
        cx.subscribe(view, move |_, event: &ChatViewEvent, _| {
            kept.borrow_mut().push(event.clone());
        })
        .detach();
    });
    told
}

/// The files "Share this file" was asked for, by their sharing links.
fn shares_asked(told: &Rc<RefCell<Vec<ChatViewEvent>>>) -> Vec<String> {
    told.borrow()
        .iter()
        .filter_map(|event| match event {
            ChatViewEvent::ShareFile(file) => file.share_url.clone(),
            _ => None,
        })
        .collect()
}

#[test]
fn more_options_offer_what_teams_offers_on_a_video_and_on_a_document() {
    let rig = rig();
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let told = told(&view, cx);
    // a video's card: Download, Share, Copy link; no Open in browser, as Teams on the web has it
    click(cx, &format!("file-more-{ID}-0"));
    assert!(
        bounds(cx, &format!("file-menu-open-{ID}-0")).is_none(),
        "no Open on a video"
    );
    assert!(bounds(cx, &format!("file-menu-download-{ID}-0")).is_some());
    assert!(bounds(cx, &format!("file-menu-copy-{ID}-0")).is_some());
    cx.simulate_keystrokes("escape");
    draw(cx);
    // its Share asks for "Share this file", not a menu of its own
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-share-{ID}-0"));
    assert_eq!(shares_asked(&told), [shares[0].clone()]);
    assert!(
        bounds(cx, &format!("file-menu-share-copy-{ID}-0")).is_none(),
        "no menu of Share's own"
    );
    // a document's card: Open in browser, Download, Share, Copy link
    click(cx, &format!("file-more-{ID}-1"));
    for row in ["open", "download", "share", "copy"] {
        assert!(bounds(cx, &format!("file-menu-{row}-{ID}-1")).is_some(), "{row}");
    }
    click(cx, &format!("file-menu-share-{ID}-1"));
    assert_eq!(shares_asked(&told), [shares[0].clone(), shares[1].clone()]);
}

#[test]
fn share_on_a_videos_card_asks_for_share_this_file() {
    let rig = rig();
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    let told = told(&view, cx);
    click(cx, &format!("file-share-{ID}-0"));
    assert_eq!(shares_asked(&told), [shares[0].clone()]);
    let asked = told.borrow().clone();
    let [ChatViewEvent::ShareFile(file)] = &asked[..] else {
        panic!("Share asked once: {asked:?}");
    };
    assert!(file.entry.is_some(), "the file keeps its entry, which the share posts");
    assert!(
        bounds(cx, &format!("file-menu-share-copy-{ID}-0")).is_none(),
        "Share opens no menu of its own"
    );
}

#[test]
fn the_read_only_app_offers_no_share_on_a_file() {
    let rig = rig();
    *rig.core.read_only.lock().unwrap() = true;
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    assert!(bounds(cx, &format!("file-share-{ID}-0")).is_none(), "no Share button");
    click(cx, &format!("file-more-{ID}-0"));
    assert!(
        bounds(cx, &format!("file-menu-copy-{ID}-0")).is_some(),
        "Copy link stays"
    );
    assert!(
        bounds(cx, &format!("file-menu-share-{ID}-0")).is_none(),
        "no Share item"
    );
}

#[test]
fn a_videos_card_shows_its_thumbnail_alone_and_the_controls_only_under_the_pointer() {
    let (_rig, _view, cx) = shown_with_thumbnail();
    assert!(bounds(cx, &format!("video-poster-{ID}-0")).is_some(), "the still frame");
    assert!(
        bounds(cx, &format!("video-controls-{ID}-0 on-hover")).is_some(),
        "the controls wait for the pointer"
    );
    assert!(bounds(cx, &format!("video-controls-{ID}-0 shown")).is_none());
}

/// Whether `inner`'s height is 9/16 of its width, to a pixel.
fn sixteen_nine(inner: Bounds<Pixels>) -> bool {
    (f32::from(inner.size.height) - f32::from(inner.size.width) * 9.0 / 16.0).abs() <= 1.0
}

#[test]
fn in_a_narrow_window_each_cards_picture_stays_16_9_of_its_width() {
    let (_rig, _view, cx) = shown_with_thumbnail();
    cx.simulate_resize(size(px(330.0), px(900.0)));
    draw(cx);
    for at in 0..2 {
        let card = bounds(cx, &format!("file-card-{ID}-{at}")).expect("the card");
        assert!(
            f32::from(card.size.width) < teams_ui::files::CARD_WIDTH - 20.0,
            "narrow: {card:?}"
        );
    }
    let player = bounds(cx, &format!("video-player-{ID}-0")).expect("the player");
    assert!(sixteen_nine(player), "the video: {player:?}");
    let document = bounds(cx, &format!("file-thumbnail-none-{ID}-1")).expect("the document's picture");
    assert!(sixteen_nine(document), "the document: {document:?}");
    // wide again: the full card's 16:9
    cx.simulate_resize(size(px(1100.0), px(900.0)));
    draw(cx);
    let player = bounds(cx, &format!("video-player-{ID}-0")).expect("the player");
    assert!(sixteen_nine(player), "{player:?}");
    assert!((f32::from(player.size.height) - 178.0).abs() <= 2.0, "{player:?}");
}

#[test]
fn a_cards_header_has_teams_type_icons_and_buttons_of_teams_size() {
    let (_rig, _view, cx) = shown_with_thumbnail();
    // Teams' own icon for a video (the play mark in a white box); a document's is its type's glyph
    assert!(bounds(cx, &format!("file-icon-{ID}-0 teams")).is_some());
    assert!(bounds(cx, &format!("file-icon-{ID}-1 glyph")).is_some());
    for button in [
        format!("file-share-{ID}-0"),
        format!("file-more-{ID}-0"),
        format!("file-more-{ID}-1"),
    ] {
        let drawn = bounds(cx, &button).expect("the button");
        let side = f32::from(drawn.size.width);
        assert!((25.0..=32.0).contains(&side), "{button}: {drawn:?}");
    }
}

#[test]
fn a_zips_card_has_teams_folder_icon_in_its_header_and_its_picture() {
    let (_view, cx) = grid_chat(grid_message(|files| files));
    assert!(bounds(cx, &format!("file-icon-{ID}-0 teams")).is_some(), "the header's");
    assert!(
        bounds(cx, &format!("file-picture-icon-{ID}-0 teams")).is_some(),
        "the picture's"
    );
}

/// The own server's account id of b, as its drive's path names it: b's MRI's id without the dashes.
const OWN_SITE: &str = "https://chat.example:8443/sp/personal/0000000000004000800000000000000b/";

/// A message from b of one document in an own server's drive: `owner` the id of the drive's account (in its path).
fn own_drive_message(owner: &str) -> Message {
    own_drive_message_from(LAB_B, owner)
}

/// A message from `sender` of one document in an own server's drive: `owner` as [`own_drive_message`] takes it.
fn own_drive_message_from(sender: &str, owner: &str) -> Message {
    let site = OWN_SITE.replace("0000000000004000800000000000000b", owner);
    let share = format!("{site}Documents/Microsoft%20Teams%20Chat%20Files/made-up%20plan.pdf");
    let entry = serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up plan.pdf",
        "fileType": "pdf", "state": "active", "objectUrl": share,
        "fileInfo": {"shareUrl": share, "fileUrl": share, "siteUrl": site}, "baseUrl": site});
    let mut stored = message(ID, sender, "", NOON);
    stored.properties = Some(serde_json::json!({"files": Value::Array(vec![entry]).to_string()}).to_string());
    stored
}

#[test]
fn an_own_servers_file_shared_on_names_its_owner_who_sent_nothing_in_the_chat() {
    let (view, cx) = grid_chat(own_drive_message_from(LAB_A, "0000000000004000800000000000000b"));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files[0].location.as_deref(), Some("personal > Lab Bee"));
}

#[test]
fn an_own_servers_file_names_its_owner_by_name_not_by_the_accounts_id() {
    let (view, cx) = grid_chat(own_drive_message("0000000000004000800000000000000b"));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files[0].location.as_deref(), Some("personal > Lab Bee"));
    // an account the view doesn't know keeps its id
    let unknown = "00000000000040008000000000000ddd";
    let (view, cx) = grid_chat(own_drive_message(unknown));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files[0].location, Some(format!("personal > {unknown}")));
}

/// A made-up image file's preview on AMS, as an entry's `filePreview` names it.
const PREVIEW_SRC: &str = "https://us-api.asm.skype.com/v1/objects/0-made-up-photo-0001/views/imgo";

/// A message of one image file and no text, from b: the fixture's first entry made a JPEG with a preview on AMS,
/// 501 by 500.
fn image_message() -> Message {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut properties = fixture["read_back"]["properties"].clone();
    let entries: Value = serde_json::from_str(properties["files"].as_str().unwrap()).unwrap();
    let mut entry = entries[0].clone();
    for (key, value) in [("fileName", "made-up-photo.jpg"), ("title", "made-up-photo.jpg")] {
        entry[key] = Value::from(value);
    }
    entry["fileType"] = Value::from("jpg");
    entry["type"] = Value::from("jpg");
    entry["filePreview"] = serde_json::json!({"previewUrl": PREVIEW_SRC, "previewWidth": 501, "previewHeight": 500});
    properties["files"] = Value::from(Value::Array(vec![entry]).to_string());
    let mut stored = message(ID, LAB_B, "", NOON);
    stored.properties = Some(properties.to_string());
    stored
}

#[test]
fn an_images_preview_is_drawn_in_place_of_its_card_at_teams_size() {
    let rig = rig_with(vec![image_message()]);
    rig.core
        .pictures
        .lock()
        .unwrap()
        .insert(PREVIEW_SRC.to_owned(), thumbnail());
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    let preview = bounds(cx, &format!("file-preview-{ID}-0")).expect("the preview is drawn");
    // the entry's 501 by 500, scaled down into Teams' box, its aspect ratio kept
    assert_eq!(teams_ui::files::preview_size((501, 500)), (253.0, 252.0));
    assert!((f32::from(preview.size.width) - 253.0).abs() < 1.0, "{preview:?}");
    assert!((f32::from(preview.size.height) - 252.0).abs() < 1.0, "{preview:?}");
    // the picture alone: no card, no header with the name, no thumbnail asked of Graph
    assert!(bounds(cx, &format!("file-card-{ID}-0")).is_none());
    assert!(bounds(cx, &format!("file-name-{ID}-0")).is_none());
    assert!(!rig.core.asked().contains(&"file_thumbnail"));
    // a click opens the file at its sharing link; "More options" is still there
    click(cx, &format!("file-preview-{ID}-0"));
    assert_eq!(cx.opened_url().as_deref(), Some(shares[0].as_str()));
    assert!(bounds(cx, &format!("file-more-{ID}-0")).is_some());
}

/// A made-up video in another user's OneDrive, shared with a link for specific people.
const LONE_SHARE: &str = "https://contoso-my.sharepoint.com/:v:/p/made_up_user_contoso_test/MadeUpToken01";
const LONE_SITE: &str = "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/";
const LONE_OBJECT: &str =
    "https://contoso-my.sharepoint.com/personal/made_up_user_contoso_test/Documents/Chat%20files/made-up%20clip.mp4";

#[test]
fn a_messages_only_file_has_a_card_as_wide_as_the_grid_its_location_whole() {
    let entry = serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up clip.mp4",
        "fileType": "mp4", "state": "active", "objectUrl": LONE_OBJECT, "fileInfo": {"shareUrl": LONE_SHARE, "siteUrl": LONE_SITE}});
    let mut stored = message(ID, LAB_B, "<p>a short clip</p>", NOON);
    stored.properties = Some(serde_json::json!({"files": Value::Array(vec![entry]).to_string()}).to_string());
    let rig = rig_with(vec![stored]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    cx.simulate_resize(size(px(1100.0), px(900.0)));
    let card = bounds(cx, &format!("file-card-{ID}-0")).expect("the card");
    assert!(
        (f32::from(card.size.width) - teams_ui::files::LONE_CARD_WIDTH).abs() < 1.0,
        "{card:?}"
    );
    let player = bounds(cx, &format!("video-player-{ID}-0")).expect("the player");
    assert!(sixteen_nine(player), "{player:?}");
    let location = bounds(cx, &format!("file-location-{ID}-0")).expect("the location");
    assert!(location.size.width > px(300.0), "{location:?}");
}

#[test]
fn an_image_whose_preview_cant_be_had_is_drawn_as_its_card() {
    let rig = rig_with(vec![image_message()]);
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    assert!(rig.core.asked().contains(&"message_picture"));
    assert!(bounds(cx, &format!("file-preview-{ID}-0")).is_none());
    assert!(bounds(cx, &format!("file-card-{ID}-0")).is_some(), "its card instead");
    assert!(bounds(cx, &format!("file-name-{ID}-0")).is_some());
}

/// A made-up own server's sharing link of a document, as its drive names it.
const OWN_SHARE: &str = "https://chat.example:8443/sp/personal/lab_b/Documents/made-up%20notes.pdf";

/// Made-up bytes standing for the own server's document.
const OWN_BYTES: &[u8] = b"%PDF-1.7 made-up notes, every byte synthetic";

/// A message of one document on an own server, from b.
fn own_file_message() -> Message {
    let entry = serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up notes.pdf",
        "fileType": "pdf", "state": "active", "objectUrl": OWN_SHARE, "fileInfo": {"shareUrl": OWN_SHARE}});
    let mut stored = message(ID, LAB_B, "<p>the notes</p>", NOON);
    stored.properties = Some(serde_json::json!({"files": Value::Array(vec![entry]).to_string()}).to_string());
    stored
}

/// The chat with the own server's document, the engine holding its bytes when `bytes`, Open in desktop app saving in
/// the test's own folder (handed back), not the runtime folder the installed app uses.
fn own_file_chat(bytes: bool) -> (Rig, tempfile::TempDir, &'static mut VisualTestContext) {
    own_files_chat(own_file_message(), if bytes { &[(OWN_SHARE, OWN_BYTES)] } else { &[] })
}

/// The chat with `message`, the engine holding each file's bytes by its sharing link, as [`own_file_chat`].
fn own_files_chat(
    message: Message,
    files: &[(&str, &[u8])],
) -> (Rig, tempfile::TempDir, &'static mut VisualTestContext) {
    let rig = rig_with(vec![message]);
    for (share, bytes) in files {
        rig.core
            .file_bytes
            .lock()
            .unwrap()
            .insert((*share).to_owned(), bytes.to_vec());
    }
    let opened = tempfile::tempdir().unwrap();
    let folder = opened.path().to_owned();
    rig.cx
        .update(|cx| cx.set_global(teams_ui::files::OpenedFilesFolder(folder)));
    let (written_trace, _) = trace();
    let (_view, cx) = chat_window(&rig, written_trace);
    (rig, opened, cx)
}

/// Answers the streams the engine held with the whole file of `bytes`, in the order they were asked, and stops holding
/// any more.
fn answer_held(rig: &Rig, bytes: &[&[u8]]) {
    *rig.core.hold_streams_from.lock().unwrap() = None;
    let held: Vec<_> = rig.core.held_streams.lock().unwrap().drain(..).collect();
    assert_eq!(held.len(), bytes.len(), "a stream held for each");
    for (answer, bytes) in held.into_iter().zip(bytes) {
        answer.send(Ok(teams_core::FileStream::of(0, bytes, bytes.len() as u64, 4096)));
    }
}

/// The file the last `file://` link opened was of.
fn opened_file(cx: &mut VisualTestContext) -> std::path::PathBuf {
    let opened = cx.opened_url().expect("the file is opened");
    assert!(opened.starts_with("file://"), "{opened}");
    url::Url::parse(&opened).unwrap().to_file_path().unwrap()
}

#[test]
fn a_click_on_an_own_servers_file_reads_it_through_the_engine_and_opens_it_in_its_app() {
    let (rig, opened, cx) = own_file_chat(true);
    assert!(bounds(cx, &format!("file-card-{ID}-0")).is_some(), "the file's card");
    click(cx, &format!("file-name-{ID}-0"));
    // never the server's link in a browser, which holds no token for it: the engine's bytes, opened as a local file
    let path = opened_file(cx);
    assert_eq!(
        path,
        opened.path().join("1").join("made-up notes.pdf"),
        "in a new folder, by its name"
    );
    assert_eq!(std::fs::read(&path).unwrap(), OWN_BYTES);
    assert!(rig.core.asked().contains(&"file_stream"));
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
}

/// Starts an Open of the own server's document and a Download of it, the first while the second runs (`open_first`:
/// the Open first): both end, the file opened and saved where chosen.
fn an_open_and_a_download_both_end(open_first: bool) {
    let (rig, opened, cx) = own_file_chat(true);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let target = chosen.path().join("notes.pdf");
    let open = |cx: &mut VisualTestContext| click(cx, &format!("file-name-{ID}-0"));
    let download = |cx: &mut VisualTestContext| {
        click(cx, &format!("file-more-{ID}-0"));
        click(cx, &format!("file-menu-download-{ID}-0"));
        cx.simulate_new_path_selection(|_| Some(target.clone()));
        draw(cx);
    };
    if open_first {
        open(cx);
        assert!(bounds(cx, &format!("file-download-{ID}-0 running")).is_some());
        download(cx);
    } else {
        download(cx);
        assert!(bounds(cx, &format!("file-download-{ID}-0 running")).is_some());
        open(cx);
    }
    answer_held(&rig, &[OWN_BYTES, OWN_BYTES]);
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), OWN_BYTES, "saved where chosen");
    let path = opened_file(cx);
    assert!(path.starts_with(opened.path()), "{}", path.display());
    assert_eq!(std::fs::read(&path).unwrap(), OWN_BYTES);
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
}

#[test]
fn a_download_of_an_own_servers_file_started_while_it_opens_still_saves_it() {
    an_open_and_a_download_both_end(true);
}

#[test]
fn an_open_of_an_own_servers_file_started_while_it_downloads_still_opens_it() {
    an_open_and_a_download_both_end(false);
}

/// Downloads the own server's document to a place, then (while that runs) opens it (`open_between`) or downloads it to
/// another place, then downloads it to the first place again: the first download is handed back (one writer of its
/// `.part` file), the card shows it again, and each place gets the whole file.
fn a_download_again_to_where_one_still_runs_is_that_one(open_between: bool) {
    let (rig, opened, cx) = own_file_chat(true);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let first = chosen.path().join("notes.pdf");
    let other = chosen.path().join("other notes.pdf");
    let download = |cx: &mut VisualTestContext, target: &std::path::Path| {
        click(cx, &format!("file-more-{ID}-0"));
        click(cx, &format!("file-menu-download-{ID}-0"));
        let target = target.to_owned();
        cx.simulate_new_path_selection(move |_| Some(target));
        draw(cx);
    };
    download(cx, &first);
    if open_between {
        click(cx, &format!("file-name-{ID}-0"));
    } else {
        download(cx, &other);
    }
    download(cx, &first);
    assert!(bounds(cx, &format!("file-download-{ID}-0 running")).is_some());
    // two streams only: the third request started no second writer of the first place's `.part` file
    answer_held(&rig, &[OWN_BYTES, OWN_BYTES]);
    draw(cx);
    assert_eq!(std::fs::read(&first).unwrap(), OWN_BYTES, "saved where chosen first");
    assert!(!teams_ui::recording_download::part_path(&first).exists());
    if open_between {
        let path = opened_file(cx);
        assert!(path.starts_with(opened.path()), "{}", path.display());
        assert_eq!(std::fs::read(&path).unwrap(), OWN_BYTES);
    } else {
        assert_eq!(std::fs::read(&other).unwrap(), OWN_BYTES, "saved where chosen second");
    }
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
}

#[test]
fn a_download_to_where_one_still_runs_behind_an_open_is_that_one() {
    a_download_again_to_where_one_still_runs_is_that_one(true);
}

#[test]
fn a_download_to_where_one_still_runs_behind_another_download_is_that_one() {
    a_download_again_to_where_one_still_runs_is_that_one(false);
}

/// Another own server's sharing link of a document of the same name, in another folder of its drive.
const OWN_SHARE_OTHER: &str = "https://chat.example:8443/sp/personal/lab_b/Documents/older/made-up%20notes.pdf";

#[test]
fn two_own_servers_files_of_one_name_opened_together_are_saved_apart() {
    let entry = |share: &str| {
        serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up notes.pdf",
            "fileType": "pdf", "state": "active", "objectUrl": share, "fileInfo": {"shareUrl": share}})
    };
    let mut stored = message(ID, LAB_B, "<p>the notes, twice</p>", NOON);
    stored.properties = Some(
        serde_json::json!({"files": Value::Array(vec![entry(OWN_SHARE), entry(OWN_SHARE_OTHER)]).to_string()})
            .to_string(),
    );
    let other: &[u8] = b"%PDF-1.7 the older made-up notes";
    let (rig, opened, cx) = own_files_chat(stored, &[(OWN_SHARE, OWN_BYTES), (OWN_SHARE_OTHER, other)]);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    click(cx, &format!("file-name-{ID}-0"));
    click(cx, &format!("file-name-{ID}-1"));
    answer_held(&rig, &[OWN_BYTES, other]);
    draw(cx);
    let saved = |folder: &str| std::fs::read(opened.path().join(folder).join("made-up notes.pdf")).unwrap();
    assert_eq!(saved("1"), OWN_BYTES);
    assert_eq!(saved("2"), other);
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
    assert!(bounds(cx, &format!("file-download-{ID}-1 saved")).is_some());
}

#[test]
fn a_download_of_another_file_to_where_one_still_runs_fails_and_the_first_saves() {
    let entry = |share: &str| {
        serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up notes.pdf",
            "fileType": "pdf", "state": "active", "objectUrl": share, "fileInfo": {"shareUrl": share}})
    };
    let mut stored = message(ID, LAB_B, "<p>the notes, twice</p>", NOON);
    stored.properties = Some(
        serde_json::json!({"files": Value::Array(vec![entry(OWN_SHARE), entry(OWN_SHARE_OTHER)]).to_string()})
            .to_string(),
    );
    let other: &[u8] = b"%PDF-1.7 the older made-up notes";
    let (rig, _opened, cx) = own_files_chat(stored, &[(OWN_SHARE, OWN_BYTES), (OWN_SHARE_OTHER, other)]);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let target = chosen.path().join("made-up notes.pdf");
    for file in 0..2 {
        click(cx, &format!("file-more-{ID}-{file}"));
        click(cx, &format!("file-menu-download-{ID}-{file}"));
        let target = target.clone();
        cx.simulate_new_path_selection(move |_| Some(target));
        draw(cx);
    }
    // the second file's download never writes the first one's `.part` file: it fails and says why
    assert!(bounds(cx, &format!("file-download-{ID}-1 failed")).is_some());
    let failed = cx.update(|_, cx| {
        teams_ui::recording_download::download_of(OWN_SHARE_OTHER, cx)
            .map(|download| download.read(cx).progress().clone())
    });
    assert_eq!(
        failed,
        Some(teams_ui::recording_download::Progress::Failed(
            teams_ui::recording_download::Failure::Busy
        ))
    );
    answer_held(&rig, &[OWN_BYTES]);
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), OWN_BYTES, "the first file, whole");
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
}

/// A download to `<name>.part` saves its file by a rename over the `.part` file of a download to `<name>`, and the
/// reverse: whichever starts second fails, and the first one saves its own bytes.
#[test]
fn a_download_to_the_part_file_of_a_running_download_fails_and_the_first_saves() {
    for second_is_part in [true, false] {
        let entry = |share: &str| {
            serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up notes.pdf",
                "fileType": "pdf", "state": "active", "objectUrl": share, "fileInfo": {"shareUrl": share}})
        };
        let mut stored = message(ID, LAB_B, "<p>the notes, twice</p>", NOON);
        stored.properties = Some(
            serde_json::json!({"files": Value::Array(vec![entry(OWN_SHARE), entry(OWN_SHARE_OTHER)]).to_string()})
                .to_string(),
        );
        let other: &[u8] = b"%PDF-1.7 the older made-up notes";
        let (rig, _opened, cx) = own_files_chat(stored, &[(OWN_SHARE, OWN_BYTES), (OWN_SHARE_OTHER, other)]);
        *rig.core.hold_streams_from.lock().unwrap() = Some(0);
        let chosen = tempfile::tempdir().unwrap();
        let report = chosen.path().join("made-up notes.pdf");
        let staging = teams_ui::recording_download::part_path(&report);
        let (first, second) = if second_is_part {
            (report.clone(), staging.clone())
        } else {
            (staging.clone(), report.clone())
        };
        for (file, place) in [(0, first.clone()), (1, second)] {
            click(cx, &format!("file-more-{ID}-{file}"));
            click(cx, &format!("file-menu-download-{ID}-{file}"));
            cx.simulate_new_path_selection(move |_| Some(place));
            draw(cx);
        }
        assert!(
            bounds(cx, &format!("file-download-{ID}-1 failed")).is_some(),
            "second is part: {second_is_part}"
        );
        let failed = cx.update(|_, cx| {
            teams_ui::recording_download::download_of(OWN_SHARE_OTHER, cx)
                .map(|download| download.read(cx).progress().clone())
        });
        assert_eq!(
            failed,
            Some(teams_ui::recording_download::Progress::Failed(
                teams_ui::recording_download::Failure::Busy
            ))
        );
        // one stream only: the second download never wrote
        answer_held(&rig, &[OWN_BYTES]);
        draw(cx);
        assert_eq!(std::fs::read(&first).unwrap(), OWN_BYTES, "the first file, whole");
        assert!(!teams_ui::recording_download::part_path(&first).exists());
        assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
    }
}

/// Another writer that the app can't see (another ngteams) saves its own file over the `.part` file of a running
/// download by a rename: the download fails as it ends, does not save that file to its place, and keeps it.
#[test]
fn a_download_whose_part_file_another_writer_replaced_fails_and_keeps_that_file() {
    let (rig, _opened, cx) = own_file_chat(true);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let target = chosen.path().join("made-up notes.pdf");
    let staging = teams_ui::recording_download::part_path(&target);
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-download-{ID}-0"));
    let place = target.clone();
    cx.simulate_new_path_selection(move |_| Some(place));
    draw(cx);
    assert!(bounds(cx, &format!("file-download-{ID}-0 running")).is_some());
    let theirs: &[u8] = b"the other writer's file";
    let other = chosen.path().join("theirs");
    std::fs::write(&other, theirs).unwrap();
    std::fs::rename(&other, &staging).unwrap();
    answer_held(&rig, &[OWN_BYTES]);
    draw(cx);
    let failed = cx.update(|_, cx| {
        teams_ui::recording_download::download_of(OWN_SHARE, cx).map(|download| download.read(cx).progress().clone())
    });
    assert_eq!(
        failed,
        Some(teams_ui::recording_download::Progress::Failed(
            teams_ui::recording_download::Failure::Busy
        ))
    );
    assert!(!target.exists(), "not saved to its place");
    assert_eq!(
        std::fs::read(&staging).unwrap(),
        theirs,
        "the other writer's file stays"
    );
}

/// The second place is the first one by another path (a link to its folder): its job finds the place of the first one
/// with the links of their folders resolved, and fails before it opens the `.part` file of the first one.
#[cfg(unix)]
#[test]
fn a_download_of_another_file_to_where_one_runs_by_another_path_fails_and_the_first_saves() {
    let entry = |share: &str| {
        serde_json::json!({"@type": teams_proto::chat::FILE_TYPE, "fileName": "made-up notes.pdf",
            "fileType": "pdf", "state": "active", "objectUrl": share, "fileInfo": {"shareUrl": share}})
    };
    let mut stored = message(ID, LAB_B, "<p>the notes, twice</p>", NOON);
    stored.properties = Some(
        serde_json::json!({"files": Value::Array(vec![entry(OWN_SHARE), entry(OWN_SHARE_OTHER)]).to_string()})
            .to_string(),
    );
    let other: &[u8] = b"%PDF-1.7 the older made-up notes";
    let (rig, _opened, cx) = own_files_chat(stored, &[(OWN_SHARE, OWN_BYTES), (OWN_SHARE_OTHER, other)]);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let folder = chosen.path().join("folder");
    std::fs::create_dir(&folder).unwrap();
    let alias = chosen.path().join("alias");
    std::os::unix::fs::symlink(&folder, &alias).unwrap();
    let target = folder.join("made-up notes.pdf");
    for (file, place) in [(0, folder.clone()), (1, alias.clone())] {
        click(cx, &format!("file-more-{ID}-{file}"));
        click(cx, &format!("file-menu-download-{ID}-{file}"));
        let place = place.join("made-up notes.pdf");
        cx.simulate_new_path_selection(move |_| Some(place));
        draw(cx);
    }
    assert!(bounds(cx, &format!("file-download-{ID}-1 failed")).is_some());
    let failed = cx.update(|_, cx| {
        teams_ui::recording_download::download_of(OWN_SHARE_OTHER, cx)
            .map(|download| download.read(cx).progress().clone())
    });
    assert_eq!(
        failed,
        Some(teams_ui::recording_download::Progress::Failed(
            teams_ui::recording_download::Failure::Busy
        ))
    );
    // one stream only: the second download never wrote the `.part` file of the first one
    answer_held(&rig, &[OWN_BYTES]);
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), OWN_BYTES, "the first file, whole");
    assert!(!teams_ui::recording_download::part_path(&target).exists());
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
}

/// Another writer that the app can't see (another ngteams, or a path to the same place that the app does not find)
/// has the `.part` file locked: the download fails before it writes, and that file and its bytes stay.
#[test]
fn a_download_whose_part_file_another_writer_has_locked_fails_and_keeps_that_file() {
    let (rig, _opened, cx) = own_file_chat(true);
    *rig.core.hold_streams_from.lock().unwrap() = Some(0);
    let chosen = tempfile::tempdir().unwrap();
    let target = chosen.path().join("made-up notes.pdf");
    let staging = teams_ui::recording_download::part_path(&target);
    let theirs: &[u8] = b"the other writer's bytes";
    std::fs::write(&staging, theirs).unwrap();
    let locked = std::fs::OpenOptions::new().write(true).open(&staging).unwrap();
    locked.try_lock().unwrap();
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-download-{ID}-0"));
    let place = target.clone();
    cx.simulate_new_path_selection(move |_| Some(place));
    draw(cx);
    let failed = cx.update(|_, cx| {
        teams_ui::recording_download::download_of(OWN_SHARE, cx).map(|download| download.read(cx).progress().clone())
    });
    assert_eq!(
        failed,
        Some(teams_ui::recording_download::Progress::Failed(
            teams_ui::recording_download::Failure::Busy
        ))
    );
    assert!(bounds(cx, &format!("file-download-{ID}-0 failed")).is_some());
    assert!(!target.exists(), "not saved to its place");
    // On Windows the lock is mandatory: a read through a second handle fails while it is held.
    drop(locked);
    assert_eq!(
        std::fs::read(&staging).unwrap(),
        theirs,
        "the other writer's file stays"
    );
}

#[test]
fn more_options_of_an_own_servers_file_downloads_it_through_the_engine_where_chosen() {
    let (_rig, _opened, cx) = own_file_chat(true);
    click(cx, &format!("file-more-{ID}-0"));
    assert!(
        bounds(cx, &format!("file-menu-open-{ID}-0")).is_some(),
        "Open in desktop app"
    );
    click(cx, &format!("file-menu-download-{ID}-0"));
    assert!(cx.did_prompt_for_new_path());
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("notes.pdf");
    cx.simulate_new_path_selection(|_| Some(target.clone()));
    draw(cx);
    assert_eq!(std::fs::read(&target).unwrap(), OWN_BYTES);
    assert_eq!(cx.opened_url(), None, "saved, not opened");
    assert!(bounds(cx, &format!("file-download-{ID}-0 saved")).is_some());
    // the link to copy is still the file's own
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-copy-{ID}-0"));
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some(OWN_SHARE));
}

#[test]
fn an_own_servers_file_the_engine_cant_read_says_so_under_its_card() {
    let (_rig, _opened, cx) = own_file_chat(false);
    click(cx, &format!("file-more-{ID}-0"));
    click(cx, &format!("file-menu-download-{ID}-0"));
    let folder = tempfile::tempdir().unwrap();
    let target = folder.path().join("notes.pdf");
    cx.simulate_new_path_selection(|_| Some(target.clone()));
    draw(cx);
    assert!(bounds(cx, &format!("file-download-{ID}-0 failed")).is_some());
    assert!(!target.exists());
    assert!(!teams_ui::recording_download::part_path(&target).exists());
}

#[test]
fn a_file_name_from_a_message_is_never_a_path() {
    use teams_ui::files::safe_name;
    assert_eq!(safe_name("made-up notes.pdf"), "made-up notes.pdf");
    assert_eq!(safe_name("../../.bashrc"), "_.._.bashrc");
    assert_eq!(safe_name("a\\b:c\u{7}.txt"), "a_b_c_.txt");
    assert_eq!(safe_name(" .. "), "file");
}

/// The grid fixture's message (five files and no text), from b, its `properties.files` changed by `change`.
fn grid_message(change: impl FnOnce(String) -> String) -> Message {
    let fixture: Value = serde_json::from_str(GRID).unwrap();
    let read_back = &fixture["read_back"];
    let mut properties = read_back["properties"].clone();
    properties["files"] = Value::from(change(properties["files"].as_str().unwrap().to_owned()));
    let mut stored = message(ID, LAB_B, read_back["content"].as_str().unwrap(), NOON);
    stored.properties = Some(properties.to_string());
    stored
}

fn grid_chat(message: Message) -> (Entity<ChatView>, &'static mut VisualTestContext) {
    let rig = Box::leak(Box::new(rig_with(vec![message])));
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(rig, written_trace);
    cx.simulate_resize(size(px(1100.0), px(1400.0)));
    draw(cx);
    (view, cx)
}

#[test]
fn five_files_are_a_grid_of_two_columns_with_where_each_is_kept_and_share_on_the_videos() {
    let (view, cx) = grid_chat(grid_message(|files| files));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files.len(), 5);
    assert_eq!(item.unshown, UnshownParts::default());
    assert!(bounds(cx, &format!("unshown-{ID}")).is_none());
    let card = |at: usize, cx: &mut VisualTestContext| bounds(cx, &format!("file-card-{ID}-{at}")).expect("a card");
    let cards: Vec<_> = (0..5).map(|at| card(at, cx)).collect();
    // two in a row, then the next row under them, the fifth alone under the first
    assert_eq!(cards[1].origin.y, cards[0].origin.y);
    assert!(cards[1].origin.x > cards[0].origin.x + cards[0].size.width);
    for (at, above) in [(2, 0), (3, 1), (4, 2)] {
        assert_eq!(cards[at].origin.x, cards[above].origin.x, "card {at}");
        assert!(
            cards[at].origin.y >= cards[above].origin.y + cards[above].size.height,
            "card {at}"
        );
    }
    // each card has its picture: the zip's type icon, the videos' players
    assert!(bounds(cx, &format!("file-thumbnail-none-{ID}-0")).is_some());
    for at in 1..5 {
        assert!(bounds(cx, &format!("video-player-{ID}-{at}")).is_some(), "video {at}");
    }
    headers_have_location_and_share_on_the_videos(cx);
}

#[test]
fn a_channel_posts_files_show_their_names_without_where_they_are_kept() {
    const CHANNEL: &str = "19:made-up-channel@thread.tacv2";
    let mut post = grid_message(|files| files);
    post.conversation_id = CHANNEL.to_owned();
    let rig = Box::leak(Box::new(rig_with(Vec::new())));
    rig.core.messages.lock().unwrap().insert(CHANNEL.to_owned(), vec![post]);
    let parts = super::parts::parts(rig, Settings::default(), Arc::new(ViewTrace::off()));
    let app: &'static mut gpui::TestAppContext = Box::leak(Box::new(rig.cx.clone()));
    let (_, cx) = app.add_window_view(|window, cx| {
        let compose = ChatView::channel_box(&parts, CHANNEL, &Place::Channel, "Post in channel".into(), window, cx);
        ChatView::in_place(parts, CHANNEL.to_owned(), Place::Channel, compose, window, cx)
    });
    cx.simulate_resize(size(px(1100.0), px(1400.0)));
    for at in 0..5 {
        assert!(bounds(cx, &format!("file-name-{ID}-{at}")).is_some(), "the name {at}");
        assert!(
            bounds(cx, &format!("file-location-{ID}-{at}")).is_none(),
            "no location {at}"
        );
    }
}

/// Where each file of the grid is kept, under its name; Share on the videos alone, More options on each.
fn headers_have_location_and_share_on_the_videos(cx: &mut VisualTestContext) {
    for at in 0..5 {
        let location = bounds(cx, &format!("file-location-{ID}-{at}")).expect("the location");
        let name = bounds(cx, &format!("file-name-{ID}-{at}")).expect("the name");
        assert!(location.origin.y > name.origin.y, "{location:?} {name:?}");
        assert!(bounds(cx, &format!("file-more-{ID}-{at}")).is_some());
        assert_eq!(
            bounds(cx, &format!("file-share-{ID}-{at}")).is_some(),
            at > 0,
            "share {at}"
        );
    }
}

#[test]
fn five_files_whose_list_serde_refuses_show_their_cards_not_the_fallback() {
    // a name cut in the middle of an emoji: a lone surrogate escape in the list's JSON, which a browser reads
    let (view, cx) = grid_chat(grid_message(|files| {
        files.replacen("made-up-camera-take-1.mp4\"", "made-up-camera-take-1 \\ud83c.mp4\"", 2)
    }));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files.len(), 5, "every file read");
    assert_eq!(item.unshown, UnshownParts::default(), "nothing said unshown");
    assert!(bounds(cx, &format!("unshown-{ID}")).is_none(), "no fallback line");
    for at in 0..5 {
        assert!(bounds(cx, &format!("file-card-{ID}-{at}")).is_some(), "card {at}");
    }
}

#[test]
fn a_file_without_a_sharepoint_link_is_a_card_without_actions_beside_the_good_ones() {
    let off = "https://files.example.test/made-up-camera-take-2.mp4";
    let (view, cx) = grid_chat(grid_message(|files| {
        let mut entries: Value = serde_json::from_str(&files).unwrap();
        for key in ["shareUrl", "fileUrl"] {
            entries[2]["fileInfo"][key] = Value::from(off);
        }
        entries[2]["objectUrl"] = Value::from(off);
        entries.to_string()
    }));
    let item = items(&view, cx).remove(0);
    assert_eq!(item.files.len(), 5);
    assert!(item.unshown.files, "the message still says what it can't show");
    assert!(bounds(cx, &format!("unshown-{ID}")).is_some());
    for at in 0..5 {
        assert!(bounds(cx, &format!("file-card-{ID}-{at}")).is_some(), "card {at}");
    }
    // its card names it, with nothing to open, share or download, and no player
    assert!(bounds(cx, &format!("file-name-{ID}-2")).is_some());
    for none in ["file-more", "file-share", "video-player"] {
        assert!(bounds(cx, &format!("{none}-{ID}-2")).is_none(), "{none}");
    }
    assert!(bounds(cx, &format!("file-thumbnail-none-{ID}-2")).is_some());
}

/// Whether the keyboard's ring is on the card of file `at`.
fn ringed(cx: &mut VisualTestContext, at: usize) -> bool {
    bounds(cx, &format!("file-card-ring-{ID}-{at}")).is_some()
}

/// Each key of `keys` down and up, as a person presses it: a focused card is clicked on the key's release. Tab and
/// Shift+Tab move as the kit's root moves them in the app's window, which the test's has not.
fn press(cx: &mut VisualTestContext, keys: &str) {
    for key in keys.split(' ') {
        if key == "tab" || key == "shift-tab" {
            cx.update(|window, cx| {
                if key == "tab" {
                    window.focus_next(cx);
                } else {
                    window.focus_prev(cx);
                }
            });
            draw(cx);
            continue;
        }
        let keystroke = gpui::Keystroke::parse(key).unwrap();
        cx.simulate_event(gpui::KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
            prefer_character_input: false,
        });
        cx.simulate_event(gpui::KeyUpEvent { keystroke });
        draw(cx);
    }
}

/// The keys into the list (Enter on it, as from Escape in the box), then Tab until the card of file `at` has them: how
/// many presses that took.
fn tab_to_card(view: &Entity<ChatView>, cx: &mut VisualTestContext, at: usize) -> usize {
    cx.update(|window, cx| view.read(cx).focus_handle().clone().focus(window, cx));
    press(cx, "enter");
    for presses in 0..30 {
        if ringed(cx, at) {
            return presses;
        }
        press(cx, "tab");
    }
    panic!("Tab never reaches the card of file {at}");
}

#[test]
fn the_keyboard_reaches_each_card_in_order_and_enter_opens_a_document() {
    let (_rig, view, cx) = shown_with_thumbnail();
    let shares = share_urls();
    let to_video = tab_to_card(&view, cx, 0);
    assert!(!ringed(cx, 1), "one card at a time");
    // the video's card, then its Share and "More options", then (the player's controls, shown while they have the
    // keys) the document's card
    let mut presses = 0;
    while !ringed(cx, 1) {
        press(cx, "tab");
        presses += 1;
        assert!(presses < 30, "Tab goes on to the document's card");
    }
    assert!(presses >= 3, "the video card's buttons come between: {presses}");
    assert!(!ringed(cx, 0));
    press(cx, "enter");
    assert_eq!(cx.opened_url().as_deref(), Some(shares[1].as_str()), "Enter opens it");
    // Shift+Tab goes back the same way
    for _ in 0..presses {
        press(cx, "shift-tab");
    }
    assert!(
        ringed(cx, 0),
        "back on the video's card ({to_video} presses to it first)"
    );
}

#[test]
fn space_on_an_images_preview_opens_it_and_shows_more_options() {
    let rig = rig_with(vec![image_message()]);
    rig.core
        .pictures
        .lock()
        .unwrap()
        .insert(PREVIEW_SRC.to_owned(), thumbnail());
    let shares = share_urls();
    let (written_trace, _) = trace();
    let (view, cx) = chat_window(&rig, written_trace);
    draw(cx);
    tab_to_card(&view, cx, 0);
    assert!(cx.opened_url().is_none());
    press(cx, "space");
    assert_eq!(cx.opened_url().as_deref(), Some(shares[0].as_str()));
    press(cx, "tab");
    assert!(!ringed(cx, 0), "its More options has the keys");
    press(cx, "enter");
    assert!(
        bounds(cx, &format!("file-menu-copy-{ID}-0")).is_some(),
        "Enter on More options opens it"
    );
}

#[test]
fn enter_plays_a_video_in_its_card_and_escape_gives_the_keys_back_to_the_card() {
    let (rig, view, cx) = shown_with_thumbnail();
    tab_to_card(&view, cx, 0);
    assert!(bounds(cx, &format!("video-controls-{ID}-0 on-hover")).is_some());
    press(cx, "enter");
    assert!(!ringed(cx, 0), "the keys went into the player");
    assert!(
        bounds(cx, &format!("video-controls-{ID}-0 shown")).is_some(),
        "its controls show while it has the keys"
    );
    // the player asks the engine for the file's bytes: it was started
    let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !rig.core.asked().contains(&"file_stream") {
        assert!(std::time::Instant::now() < until, "the video was never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
        cx.run_until_parked();
    }
    press(cx, "escape");
    assert!(ringed(cx, 0), "Escape gives the keys back to the card");
    assert!(bounds(cx, &format!("video-controls-{ID}-0 on-hover")).is_some());
    // the ring stays while the pointer moves, as a browser's does
    cx.simulate_mouse_move(gpui::point(px(5.0), px(5.0)), None, Modifiers::none());
    assert!(ringed(cx, 0), "the pointer's move leaves the ring");
    // a second Escape, on the card, leaves it for the list, as from any control in it
    press(cx, "escape");
    assert!(!ringed(cx, 0));
    assert!(cx.update(|window, cx| view.read(cx).focus_handle().is_focused(window)));
}

#[test]
fn a_cards_screen_reader_name_is_the_files_name_and_kind() {
    let files = shared_files(Some(
        &serde_json::from_str::<Value>(FIXTURE).unwrap()["read_back"]["properties"]["files"],
    ))
    .files;
    let spoken: Vec<String> = files.iter().map(teams_ui::files::spoken_name).collect();
    assert_eq!(spoken.len(), 2);
    assert!(
        spoken[0].starts_with(&files[0].name) && spoken[0].ends_with(", Video"),
        "{spoken:?}"
    );
    assert!(spoken[1].starts_with(&files[1].name), "{spoken:?}");
}
