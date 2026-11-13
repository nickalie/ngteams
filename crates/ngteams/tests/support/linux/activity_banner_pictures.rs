//! Desktop banners' pictures (#114) as a user meets them, in a run of the app of its own on a data dir of its own.
//! The mock's Graph gives b a picture (one colour), c one that isn't a picture, and holds its answer for the outsider's
//! (a synthetic account in a 1:1 chat with a) past the banner; the apps' definitions name Made-up Square's tile (one
//! colour to its edges) and Made-up Slow's, whose read the mock holds too. With another chat on screen, as the
//! notification server on the sandbox's bus records them:
//!
//! - b's message and b's mention of a carry b's picture, 96×96 and round (its corners clear); the banner's click
//!   still opens the chat;
//! - four of b's messages in the 1:1 chat collapse into one banner with b's picture; four in the group, one without;
//! - c's message (a picture that doesn't decode) and the outsider's (a picture read still running) come as promptly as
//!   b's, with no picture;
//! - Made-up Square's first item, its app never read this run, comes with no picture and has its tile read after; the
//!   next one carries the tile, square to its corners; Made-up Slow's two items come promptly while its tile read
//!   hangs, with no picture;
//! - no window toast says anything went wrong, and on Linux no banner picture file is ever written (the pixels go in
//!   the `image-data` hint), during the run nor after it.
//!
//! Each banner's picture is kept as `activity-banner-picture-*.png` beside the screenshots.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use std::{fs, io};

use serde_json::{Value, json};
use teams_mock::{AB_CHAT, ABC_CHAT, AC_CHAT, AX_CHAT, CHAT_BASE, ChatServiceMock, LAB_A, LAB_B, LAB_C, OUTSIDER};
use teams_proto::chat::{Marks, MessageText, Node, RichText};
use teams_proto::{Author, HttpTransport, Mri, SendMessage, Transport as _};
use tokio::sync::broadcast;
use ui_sandbox::bus::Notification;
use ui_sandbox::{Sandbox, SandboxResult, wait_until};

use super::turn_off::APP;
use super::{CTL_SOCKET, Check, EVIDENCE_ENV, Probe, Qa, Tokens, app_note, ensure, say, shots_dir};

/// The child's mode for this run.
pub(super) const MODE: &str = "pictures";
/// The data dir of this run, passed to the app's child.
const DATA_ENV: &str = "NGTEAMS_ACTIVITY_PICTURES_DATA";
/// The side Graph's picture has, and a banner's.
const SIDE: u32 = 96;
/// b's picture, and Made-up Square's tile.
const B_COLOUR: [u8; 3] = [0xd0, 0x40, 0x90];
const SQUARE_COLOUR: [u8; 3] = [0xf0, 0x80, 0x10];
/// The apps of this run besides the feed's own (Workflows, [`APP`]).
const SQUARE_APP: &str = "00000000-0000-4000-8000-00000000a201";
const SLOW_APP: &str = "00000000-0000-4000-8000-00000000a202";
/// The CDN's app folder, and the same on the mock.
const CDN: &str = "https://res.cdn.office.net/teamsappdata/evergreen-assets/apps";
const CDN_PATH: &str = "/teamsappdata/evergreen-assets/apps";
/// More than the definition reads a run makes.
const DEFINITION_READS: usize = 24;
/// How long the mock holds the outsider's picture and Made-up Slow's tile before it answers (404): the app's request
/// timeout, so a read is held until it gives up.
const HANG: Duration = Duration::from_secs(30);
/// The longest a banner may take from the post that brings it: far less than [`HANG`].
const PROMPT: Duration = Duration::from_secs(4);
/// How long a collapsed banner gets to settle (later messages replace it).
const SETTLE_BANNERS: Duration = Duration::from_secs(3);

/// The flows (each leaves the window as the next expects it).
const CHECKS: [Check; 9] = [
    // first: the outsider's read is held from about the app's start, for HANG only
    ("hanging_picture_read_doesnt_hold_the_banner", hanging_person),
    ("message_banner_shows_the_sender_s_round_picture", message),
    ("mention_banner_shows_the_sender_s_round_picture", mention),
    (
        "reading_the_group_chat_closes_its_mention_banner",
        reading_closes_the_mention,
    ),
    (
        "collapsed_one_to_one_banner_shows_the_other_person_s_picture",
        collapsed_one_to_one,
    ),
    ("collapsed_group_banner_shows_no_picture", collapsed_group),
    (
        "picture_that_doesnt_decode_gives_a_prompt_banner_without_one",
        undecodable,
    ),
    (
        "first_app_banner_without_its_tile_the_next_with_it_square",
        app_first_then_tile,
    ),
    ("hanging_tile_read_doesnt_hold_the_app_s_banners", hanging_tile),
];

/// The flows that passed, and those that failed with why.
type Tally = (Vec<&'static str>, Vec<(&'static str, String)>);

/// The run (see the module docs).
pub(super) fn run(sandbox: &Sandbox) -> SandboxResult<Tally> {
    let data = tempfile::tempdir()?;
    let dir = data.path().to_string_lossy().into_owned();
    let _ = fs::remove_file(sandbox.path(CTL_SOCKET));
    let mut qa = Qa::start_in(sandbox, MODE, &[(DATA_ENV, &dir)])?;
    people_read(&qa)?;
    for (name, check) in CHECKS {
        qa.check(name, check);
    }
    let (mut passed, mut failed) = (qa.passed.clone(), qa.failed.clone());
    let during = no_picture_files(data.path());
    qa.quit()?;
    let name = "linux_banners_write_no_picture_file";
    match during.and_then(|()| no_picture_files(data.path())) {
        Ok(()) => {
            say(&format!("ok {name}"));
            passed.push(name);
        }
        Err(error) => {
            say(&format!("FAILED {name}: {error}"));
            failed.push((name, error.to_string()));
        }
    }
    Ok((passed, failed))
}

/// The data dir of the run of `mode`, or `None` for another mode.
pub(super) fn data_dir(mode: &str) -> SandboxResult<Option<PathBuf>> {
    if mode != MODE {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(
        std::env::var_os(DATA_ENV).ok_or("no pictures data dir")?,
    )))
}

/// The object id of the account `mri`.
fn guid(mri: &str) -> &str {
    mri.trim_start_matches("8:orgid:")
}

/// Graph's path of `mri`'s picture.
fn picture_path(mri: &str) -> String {
    format!("/v1.0/users/{}/photos/96x96/$value", guid(mri))
}

fn tile_path(app: &str) -> String {
    format!("{CDN_PATH}/{app}_largeImage.png")
}

/// The mock for this run: the people's pictures, the apps' definitions and tiles.
pub(super) async fn seed(mock: &ChatServiceMock) -> SandboxResult {
    mock.serve_bytes(&picture_path(LAB_B), "image/png", one_colour(SIDE, B_COLOUR)?)
        .await;
    mock.serve_bytes(
        &picture_path(LAB_C),
        "image/jpeg",
        b"made-up bytes, no picture".to_vec(),
    )
    .await;
    mock.refuse_next_after("GET", &picture_path(OUTSIDER), 404, HANG).await;
    let octets = "application/octet-stream";
    mock.serve_bytes(&tile_path("made-up-square"), octets, one_colour(192, SQUARE_COLOUR)?)
        .await;
    for _ in 0..3 {
        mock.refuse_next_after("GET", &tile_path("made-up-slow"), 404, HANG)
            .await;
    }
    let definition = |id: &str, name: &str, file: &str| {
        json!({
            "id": id, "name": name, "developerName": "Made-up Maker", "shortDescription": "Made up",
            "accentColor": "#5B5FC7", "largeImageUrl": format!("{CDN}/{file}_largeImage.png"),
        })
    };
    let mut updates = definition(APP, "Made-up Updates", "made-up-updates");
    updates["activities"] = json!({"activityTypes": [{"type": "messageNotify"}, {"type": "customNotification"}]});
    let answer = json!([
        updates,
        definition(SQUARE_APP, "Made-up Square", "made-up-square"),
        definition(SLOW_APP, "Made-up Slow", "made-up-slow"),
    ]);
    for _ in 0..DEFINITION_READS {
        mock.answer_next("POST", "users/apps/batchedDefinitions", 200, &answer)
            .await;
    }
    Ok(())
}

/// A PNG `side` px square of `colour` to its edges.
fn one_colour(side: u32, [r, g, b]: [u8; 3]) -> SandboxResult<Vec<u8>> {
    let picture = image::RgbaImage::from_pixel(side, side, image::Rgba([r, g, b, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(picture).write_to(&mut io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    Ok(bytes)
}

// --- the child's answers ---

/// The child's answer to `pic <rest>`.
pub(super) fn reply(rest: &str, probe: &Probe) -> Value {
    let (what, count) = rest.split_once(' ').unwrap_or((rest, "1"));
    let count: usize = count.parse().unwrap_or(1);
    let mock = &probe.mock;
    match what {
        "reads" => reads(probe),
        "b-says" => posts(mock, LAB_B, AB_CHAT, count),
        "b-says-group" => posts(mock, LAB_B, ABC_CHAT, count),
        "c-says" => posts(mock, LAB_C, AC_CHAT, count),
        "outsider-says" => posts(mock, OUTSIDER, AX_CHAT, count),
        "b-mentions" => b_mentions(probe),
        "square" => app_item(mock, SQUARE_APP, "Made-up Square", count),
        "slow" => app_item(mock, SLOW_APP, "Made-up Slow", count),
        _ => json!({"error": format!("pic what: {rest}")}),
    }
}

/// Every `GET` of a picture the mock got (people's and tiles), by path: how many.
fn reads(probe: &Probe) -> Value {
    let requests = probe.block(probe.mock.received_requests()).unwrap_or_default();
    let mut reads: BTreeMap<String, usize> = BTreeMap::new();
    for request in requests.iter().filter(|request| request.method.as_str() == "GET") {
        let path = request.url.path();
        if path.contains("/photos/") || path.starts_with(CDN_PATH) {
            *reads.entry(path.to_owned()).or_default() += 1;
        }
    }
    json!(reads)
}

/// `from` posts `count` made-up messages to `chat`, as its members are then told.
fn posts(mock: &ChatServiceMock, from: &str, chat: &str, count: usize) -> Value {
    let ids: Option<Vec<String>> = (0..count)
        .map(|number| mock.post_as(from, chat, &format!("<p>A made-up note {number} for a banner</p>")))
        .collect();
    ids.map_or_else(|| json!({"error": "a post refused"}), |ids| json!({"ids": ids}))
}

/// b mentions a in the group chat, posted as b's client posts it (its mention named in the message's properties).
fn b_mentions(probe: &Probe) -> Value {
    let Some(token) = probe.mock.token(LAB_B) else {
        return json!({"error": "no token for b"});
    };
    let tokens = Arc::new(Tokens {
        token,
        renewals: broadcast::channel(4).0,
    });
    let Ok(transport) = HttpTransport::with_mock_origin(tokens, CHAT_BASE, &probe.mock.origin()) else {
        return json!({"error": "no transport for b"});
    };
    let nodes = vec![Node::Paragraph(vec![
        Node::Mention {
            item_id: "m".to_owned(),
            text: "Lab A".to_owned(),
        },
        Node::Text {
            text: "\u{a0}a made-up question for the banner".to_owned(),
            marks: Marks::NONE,
        },
    ])];
    let text = MessageText::new(
        RichText {
            nodes,
            truncated: false,
        },
        [("m".to_owned(), Mri::new(LAB_A))],
    );
    let Ok(text) = text else {
        return json!({"error": "the mention isn't a message"});
    };
    // a client message id, as every client's post carries one (a read receipt names the message by it)
    let send = SendMessage::new(ABC_CHAT, Author::new(Mri::new(LAB_B), "Lab B"), text)
        .with_client_message_id("9100000000000000777");
    match probe.block(transport.deliver(send)) {
        Some(Ok(answer)) => json!({"status": answer.status()}),
        Some(Err(error)) => json!({"error": error.to_string()}),
        None => json!({"error": "no answer"}),
    }
}

/// `count` new items of app `id` (`name`), unread, as the feed then tells a.
fn app_item(mock: &ChatServiceMock, id: &str, name: &str, count: usize) -> Value {
    let mut ids = Vec::new();
    for _ in 0..count {
        let mut item = app_note();
        let number = 400 + ids.len() as u64 + u64::from(id == SLOW_APP) * 100;
        item["activityId"] = json!(number);
        item["sourceAppDisplayName"] = json!(name);
        item["sourceUserImDisplayName"] = json!(name);
        item["messagePreview"] = json!(format!("Made-up note {number} from {name}"));
        item["activityContext"]["teamsAppId"] = json!(id);
        let Some(added) = mock.add_activity(LAB_A, &item, false) else {
            return json!({"error": "no feed"});
        };
        ids.push(added);
    }
    json!({"ids": ids})
}

// --- the parent's flows ---

/// The picture reads the mock got so far, by path.
fn picture_reads(qa: &Qa<'_>) -> SandboxResult<BTreeMap<String, usize>> {
    Ok(serde_json::from_value(qa.ask("pic reads")?)?)
}

/// Waits for the app's reads of b's, c's and the outsider's pictures (the last one held), started as the chat list
/// shows them, and gives the store a moment to keep them.
fn people_read(qa: &Qa<'_>) -> SandboxResult {
    let wanted = [LAB_B, LAB_C, OUTSIDER].map(picture_path);
    wait_until(super::WAIT, || {
        let reads = picture_reads(qa).ok()?;
        wanted.iter().all(|path| reads.contains_key(path)).then_some(())
    })
    .ok_or_else(|| format!("the people's pictures never read: {:?}", picture_reads(qa)))?;
    thread::sleep(Duration::from_secs(1));
    Ok(())
}

/// Another chat on screen (the a–c chat), so the chats the flows post to notify.
fn park(qa: &mut Qa<'_>) -> SandboxResult {
    show(qa, AC_CHAT)
}

/// `chat` on screen.
fn show(qa: &mut Qa<'_>, chat: &str) -> SandboxResult {
    qa.ctl_json(&["open", "chat", chat])?;
    qa.state_until("the chat on screen", |state| state["chat"] == chat)?;
    Ok(())
}

/// `chat` opened (read, its banners closed and their count started again), then another chat on screen.
fn read_then_park(qa: &mut Qa<'_>, chat: &str) -> SandboxResult {
    qa.ctl_json(&["open", "chat", chat])?;
    qa.state_until("the chat opened", |state| state["chat"] == chat)?;
    // its messages loaded and its read receipt's debounce (800 ms) past, as a user's look at it takes
    thread::sleep(Duration::from_secs(3));
    park(qa)
}

/// Asks the child `question` (a post) and waits for the first banner after it: the banner and how long it took.
fn banner_for(qa: &Qa<'_>, question: &str) -> SandboxResult<(Notification, Duration)> {
    let shown = qa.sandbox.bus().notifications().len();
    let asked = Instant::now();
    let posted = qa.ask(question)?;
    ensure(posted.get("error").is_none(), || format!("{question}: {posted}"))?;
    let banner = wait_until(super::WAIT, || qa.sandbox.bus().notifications().get(shown).cloned())
        .ok_or_else(|| format!("no banner after {question}"))?;
    let took = asked.elapsed();
    say(&format!(
        "banner after {question} in {} ms: {:?}: {:?}; hints {:?}; picture {:?}",
        took.as_millis(),
        banner.summary,
        banner.body,
        banner.hints,
        banner.image.as_ref().map(|(width, height, _)| (width, height))
    ));
    Ok((banner, took))
}

/// The last collapsed banner ("N new messages") shown after the first `shown`, once later ones had time to replace it.
fn collapsed_after(qa: &Qa<'_>, shown: usize) -> SandboxResult<Notification> {
    let collapsed = |all: Vec<Notification>| {
        all.into_iter()
            .skip(shown)
            .rfind(|banner| banner.body.contains(" new messages"))
    };
    wait_until(super::WAIT, || collapsed(qa.sandbox.bus().notifications())).ok_or("no collapsed banner")?;
    thread::sleep(SETTLE_BANNERS);
    let banner = collapsed(qa.sandbox.bus().notifications()).ok_or("the collapsed banner went")?;
    say(&format!(
        "collapsed banner: {:?}: {:?}; hints {:?}; picture {:?}",
        banner.summary,
        banner.body,
        banner.hints,
        banner.image.as_ref().map(|(width, height, _)| (width, height))
    ));
    Ok(banner)
}

/// The banner's picture: 96×96 pixels, RGBA.
fn pixels(banner: &Notification) -> Result<&[u8], String> {
    let (width, height, pixels) = banner.image.as_ref().ok_or_else(|| {
        format!(
            "no picture on {:?}: {:?} (hints {:?})",
            banner.summary, banner.body, banner.hints
        )
    })?;
    let side = i32::try_from(SIDE).unwrap_or_default();
    if (*width, *height) != (side, side) || pixels.len() != (SIDE * SIDE * 4) as usize {
        return Err(format!("a picture {width}×{height} of {} bytes", pixels.len()));
    }
    if !banner.hints.iter().any(|hint| hint == "image-data") {
        return Err(format!("no image-data hint: {:?}", banner.hints));
    }
    Ok(pixels)
}

/// The pixel at `(x, y)`.
fn at(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let start = ((y * SIDE + x) * 4) as usize;
    [pixels[start], pixels[start + 1], pixels[start + 2], pixels[start + 3]]
}

fn near(pixel: [u8; 4], [r, g, b]: [u8; 3]) -> bool {
    pixel[3] == 255 && pixel[0].abs_diff(r) <= 8 && pixel[1].abs_diff(g) <= 8 && pixel[2].abs_diff(b) <= 8
}

const CORNERS: [(u32, u32); 4] = [(0, 0), (SIDE - 1, 0), (0, SIDE - 1), (SIDE - 1, SIDE - 1)];

/// How many pixels are `colour`, opaque.
fn of_colour(pixels: &[u8], colour: [u8; 3]) -> usize {
    pixels
        .chunks(4)
        .filter(|pixel| near([pixel[0], pixel[1], pixel[2], pixel[3]], colour))
        .count()
}

/// The banner carries `colour` cropped to a circle: its corners clear, its middle `colour`, about a circle's area of
/// it (π·48² ≈ 7238 of 9216).
fn round_of(banner: &Notification, colour: [u8; 3]) -> Result<(), String> {
    let pixels = pixels(banner)?;
    let corners: Vec<[u8; 4]> = CORNERS.iter().map(|&(x, y)| at(pixels, x, y)).collect();
    if corners.iter().any(|pixel| pixel[3] != 0) {
        return Err(format!("not round: its corners {corners:?}"));
    }
    let middle = at(pixels, SIDE / 2, SIDE / 2);
    if !near(middle, colour) {
        return Err(format!("not the person's picture: its middle {middle:?}"));
    }
    let filled = of_colour(pixels, colour);
    if !(6800..=7400).contains(&filled) {
        return Err(format!("{filled} pixels of the picture, not a circle's"));
    }
    say(&format!("round picture: {filled} opaque pixels, corners {corners:?}"));
    Ok(())
}

/// The banner carries `colour` to its corners: square, not cropped.
fn square_of(banner: &Notification, colour: [u8; 3]) -> Result<(), String> {
    let pixels = pixels(banner)?;
    let corners: Vec<[u8; 4]> = CORNERS.iter().map(|&(x, y)| at(pixels, x, y)).collect();
    if !corners.iter().all(|&pixel| near(pixel, colour)) {
        return Err(format!("not square: its corners {corners:?}"));
    }
    let filled = of_colour(pixels, colour);
    if filled < 9000 {
        return Err(format!("only {filled} pixels of the tile"));
    }
    say(&format!("square picture: {filled} opaque pixels, corners {corners:?}"));
    Ok(())
}

fn no_picture(banner: &Notification) -> Result<(), String> {
    if banner.image.is_some() || banner.hints.iter().any(|hint| hint == "image-data") {
        return Err(format!("a picture on {:?}: {:?}", banner.summary, banner.body));
    }
    Ok(())
}

fn prompt(took: Duration, what: &str) -> Result<(), String> {
    if took > PROMPT {
        return Err(format!("{what} took {} ms", took.as_millis()));
    }
    Ok(())
}

/// Keeps the banner's picture as `activity-banner-picture-<name>.png`, with the screenshots and in the evidence dir.
fn keep(banner: &Notification, name: &str) -> SandboxResult {
    let Some((width, height, pixels)) = &banner.image else {
        return Ok(());
    };
    let picture = image::RgbaImage::from_raw(u32::try_from(*width)?, u32::try_from(*height)?, pixels.clone())
        .ok_or("the picture's bytes don't fill it")?;
    let file = format!("activity-banner-picture-{name}.png");
    picture.save(shots_dir()?.join(&file))?;
    if let Some(dir) = std::env::var_os(EVIDENCE_ENV) {
        let dir = PathBuf::from(dir);
        fs::create_dir_all(&dir)?;
        picture.save(dir.join(&file))?;
    }
    Ok(())
}

/// No window toast (nothing said went wrong).
fn quiet_window(qa: &Qa<'_>) -> SandboxResult {
    let state = qa.state()?;
    ensure(state["toast"].is_null(), || format!("a toast: {}", state["toast"]))
}

/// b's message in the 1:1 chat: the banner carries b's picture, round; its click opens the chat.
fn message(qa: &mut Qa<'_>) -> SandboxResult {
    park(qa)?;
    let (banner, took) = banner_for(qa, "pic b-says")?;
    keep(&banner, "message")?;
    round_of(&banner, B_COLOUR)?;
    prompt(took, "b's banner")?;
    qa.sandbox.bus().invoke_action(banner.id, "default")?;
    qa.state_until("the banner's click opened the a–b chat", |state| {
        state["chat"] == AB_CHAT
    })?;
    quiet_window(qa)?;
    qa.hover(super::AWAY)?;
    qa.shot("banner-picture-message-opened-light")?;
    Ok(())
}

/// b's mention of a in the group chat: "… mentioned you", b's picture, round.
fn mention(qa: &mut Qa<'_>) -> SandboxResult {
    park(qa)?;
    let (banner, took) = banner_for(qa, "pic b-mentions")?;
    keep(&banner, "mention")?;
    ensure(banner.summary.contains("mentioned you"), || {
        format!("not a mention's banner: {:?}: {:?}", banner.summary, banner.body)
    })?;
    round_of(&banner, B_COLOUR)?;
    prompt(took, "b's mention")?;
    Ok(())
}

/// The group chat opened (read) after b's mention: the mention's banner closes and the chat's count starts again, as
/// `notify.rs`' docs say (not #114's; the next flow's count depends on it).
fn reading_closes_the_mention(qa: &mut Qa<'_>) -> SandboxResult {
    let banner = qa
        .sandbox
        .bus()
        .notifications()
        .into_iter()
        .rfind(|banner| banner.summary.contains("mentioned you"))
        .ok_or("no mention's banner")?;
    // the chat stays on screen until its banner closes, as a user reading it
    show(qa, ABC_CHAT)?;
    let closed = wait_until(super::WAIT, || {
        qa.sandbox
            .bus()
            .closed_notifications()
            .contains(&banner.id)
            .then_some(())
    });
    say(&format!(
        "the mention's banner closed once the group chat was read: {}",
        closed.is_some()
    ));
    ensure(closed.is_some(), || {
        "the group chat opened and read, its mention's banner stays".to_owned()
    })?;
    park(qa)
}

/// Four of b's messages in the 1:1 chat: one "4 new messages" banner, b's picture on it.
fn collapsed_one_to_one(qa: &mut Qa<'_>) -> SandboxResult {
    read_then_park(qa, AB_CHAT)?;
    let shown = qa.sandbox.bus().notifications().len();
    let posted = qa.ask("pic b-says 4")?;
    ensure(posted.get("error").is_none(), || format!("b's posts: {posted}"))?;
    let banner = collapsed_after(qa, shown)?;
    keep(&banner, "collapsed-one-to-one")?;
    round_of(&banner, B_COLOUR)?;
    read_then_park(qa, AB_CHAT)
}

/// Four of b's messages in the group chat: one "4 new messages" banner about the chat, no picture.
fn collapsed_group(qa: &mut Qa<'_>) -> SandboxResult {
    read_then_park(qa, ABC_CHAT)?;
    let shown = qa.sandbox.bus().notifications().len();
    let posted = qa.ask("pic b-says-group 4")?;
    ensure(posted.get("error").is_none(), || format!("b's posts: {posted}"))?;
    let banner = collapsed_after(qa, shown)?;
    no_picture(&banner)?;
    read_then_park(qa, ABC_CHAT)
}

/// c's message in the a–c chat: c's picture was read but doesn't decode; the banner comes as promptly, without one.
fn undecodable(qa: &mut Qa<'_>) -> SandboxResult {
    show(qa, AB_CHAT)?;
    let (banner, took) = banner_for(qa, "pic c-says")?;
    no_picture(&banner)?;
    prompt(took, "c's banner")?;
    let reads = picture_reads(qa)?;
    ensure(reads.contains_key(&picture_path(LAB_C)), || {
        format!("c's picture never read: {reads:?}")
    })?;
    quiet_window(qa)?;
    park(qa)
}

/// The outsider's message while the mock still holds their picture's read: the banner comes promptly, no picture.
fn hanging_person(qa: &mut Qa<'_>) -> SandboxResult {
    park(qa)?;
    let (banner, took) = banner_for(qa, "pic outsider-says")?;
    let since_launch = qa.launched.elapsed();
    say(&format!(
        "the outsider's banner {} ms after the app's start",
        since_launch.as_millis()
    ));
    ensure(since_launch < HANG, || {
        format!(
            "the outsider's banner came {} ms after the app's start: their picture's read may have ended",
            since_launch.as_millis()
        )
    })?;
    no_picture(&banner)?;
    prompt(took, "the outsider's banner")?;
    let reads = picture_reads(qa)?;
    ensure(reads.contains_key(&picture_path(OUTSIDER)), || {
        format!("the outsider's picture never asked for: {reads:?}")
    })?;
    quiet_window(qa)?;
    read_then_park(qa, AX_CHAT)
}

/// Made-up Square's first item, the app's tile never read this run: a banner without it, promptly, and the tile read
/// after; its second item's banner carries the tile, square.
fn app_first_then_tile(qa: &mut Qa<'_>) -> SandboxResult {
    park(qa)?;
    let path = tile_path("made-up-square");
    let before = picture_reads(qa)?;
    ensure(!before.contains_key(&path), || {
        format!("Made-up Square's tile read before its first banner: {before:?}")
    })?;
    let (first, took) = banner_for(qa, "pic square")?;
    no_picture(&first)?;
    prompt(took, "the app's first banner")?;
    wait_until(super::WAIT, || {
        picture_reads(qa).ok()?.contains_key(&path).then_some(())
    })
    .ok_or("Made-up Square's tile never read after its first banner")?;
    thread::sleep(Duration::from_secs(1));
    let (next, took) = banner_for(qa, "pic square")?;
    keep(&next, "app-tile")?;
    square_of(&next, SQUARE_COLOUR)?;
    prompt(took, "the app's next banner")?;
    let reads = picture_reads(qa)?;
    ensure(reads.get(&path) == Some(&1), || {
        format!("Made-up Square's tile read {:?} times", reads.get(&path))
    })?;
    quiet_window(qa)
}

/// Made-up Slow's items while the mock holds its tile's read: each banner comes promptly, without a picture.
fn hanging_tile(qa: &mut Qa<'_>) -> SandboxResult {
    park(qa)?;
    let path = tile_path("made-up-slow");
    let (first, took) = banner_for(qa, "pic slow")?;
    no_picture(&first)?;
    prompt(took, "Made-up Slow's first banner")?;
    wait_until(super::WAIT, || {
        picture_reads(qa).ok()?.contains_key(&path).then_some(())
    })
    .ok_or("Made-up Slow's tile never asked for")?;
    let (next, took) = banner_for(qa, "pic slow")?;
    no_picture(&next)?;
    prompt(took, "Made-up Slow's next banner")?;
    quiet_window(qa)
}

/// The data dir holds no banner picture file: no `banner-pictures/` in the app's data, nor a file of that name
/// anywhere under it.
fn no_picture_files(root: &Path) -> SandboxResult {
    let folder = root.join("ngteams-data").join("banner-pictures");
    ensure(!folder.exists(), || format!("{} exists", folder.display()))?;
    let mut found = Vec::new();
    let mut dirs = vec![root.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir)?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path.clone());
            }
            if path.to_string_lossy().contains("banner") {
                found.push(path);
            }
        }
    }
    ensure(found.is_empty(), || format!("banner files: {found:?}"))?;
    say(&format!("no banner picture file under {}", root.display()));
    Ok(())
}
