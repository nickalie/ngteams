//! QA of task 706 (round 1), in the same window: files in a channel's posts and replies, as a user adds them.
//!
//! - **Attach** on the post box: the file chooser, the card while the file uploads, then the ready card; Send; the
//!   post names the file, and the channel's folder on the mock's team site holds it.
//! - **A drop and a paste**: a drop on the post box while the thread shows lights the post box alone and the file goes
//!   there; a paste of a file manager's copy into the reply box goes to the reply box alone.
//! - **A reply** with a file, from Attach on the reply box, to the post's thread.
//! - **The same name**: the copy gets a number and Teams' yellow notice, which its close button takes away.
//! - **Remove** on an uploaded file before Send: the file goes to the site's recycle bin.
//! - **A failed step** (`addStub`, `StartUpload`, `FinishUpload`, each a 500 once): the card says the upload failed,
//!   and no file that is not in the recycle bin stays in the folder.
//!
//! The places the pointer clicks (Attach, Remove, the notice's close) are measured from the box's bounds, the way the
//! box lays them out at 1280 px with a font of 14.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use ui_sandbox::{SandboxResult, wait_until};

use super::portal::Answer;
use super::{
    CALM, CHANNEL_POST, DOLPHIN, SETTLE, User, WAIT, box_centre, ensure, file_lists, problem_of, px, ready, say,
    staged, text_line,
};

/// The mock's channel folder, as a server-relative path.
const FOLDER: &str = "/sites/mockteam/Shared Documents/Channel";
/// Attach on a box's bottom row, up from the box's bottom edge: from its left edge where the box is narrow and puts its
/// tools under the text, else in from its right edge (the tools at the right of the text line).
const ATTACH_FROM_LEFT: u32 = 86;
const ATTACH_FROM_RIGHT: u32 = 125;
const ROW_FROM_BOTTOM: u32 = 27;
/// The narrowest box that keeps its tools at the right of its text line, at 1280 px.
const TOOLS_BESIDE_TEXT: u32 = 400;
/// The reply box's text line, down from its top edge: "Send to: thread only" is over it.
const REPLY_TEXT_FROM_TOP: u32 = 46;
/// A card's Remove: from the box's left edge, and down from its top edge while the box holds no text.
const REMOVE_FROM_LEFT: u32 = 307;
const REMOVE_FROM_TOP: u32 = 57;
/// The copy notice's close button: in from the box's right edge, and up from its top edge (the notice takes one line
/// over a wide box, two over a narrow one).
const NOTICE_CLOSE_FROM_RIGHT: u32 = 20;
const NOTICE_CLOSE_ABOVE_WIDE: u32 = 19;
const NOTICE_CLOSE_ABOVE_NARROW: u32 = 33;
/// How long the mock holds the `addStub` answer, so that the user sees the card while the file uploads.
const SLOW_STUB: Duration = Duration::from_millis(2500);

pub(super) const CHECKS: [super::Check; 9] = [
    ("qa706_attach_post", attach_post),
    ("qa706_drop_and_paste", drop_and_paste),
    ("qa706_reply", reply),
    ("qa706_same_name", same_name),
    ("qa706_remove", remove),
    ("qa706_fail_add_stub", fail_add_stub),
    ("qa706_fail_start_upload", fail_start_upload),
    ("qa706_fail_finish_upload", fail_finish_upload),
    ("qa706_dark", dark),
];

impl User<'_> {
    /// The channel shown, its boxes empty, Teams Light, font 14; the thread of the post open when `thread`.
    fn fresh_channel(&mut self, thread: bool) -> SandboxResult<Value> {
        self.ask("theme-light")?;
        self.ask("font 14")?;
        self.ask("open-channel")?;
        self.until("the channel", |state| state["post"]["bounds"].is_array())?;
        self.ask("open-thread")?;
        self.until("the thread", |state| state["thread"]["bounds"].is_array())?;
        self.ask("clear")?;
        if !thread {
            self.ask("close-thread")?;
            // the post box widens a frame after the thread goes: Attach is found by its bounds
            return self.until("the thread closed", |state| {
                state["thread"].is_null()
                    && staged(&state["post"]).is_empty()
                    && state["post"]["bounds"][2]
                        .as_f64()
                        .is_some_and(|width| width >= f64::from(TOOLS_BESIDE_TEXT))
            });
        }
        self.until("both boxes empty", |state| {
            staged(&state["post"]).is_empty() && staged(&state["thread"]).is_empty()
        })
    }

    /// Clicks Attach on the box `key` and picks `path` in the file chooser.
    pub(super) fn attach_by_button(&mut self, key: &str, path: &Path) -> SandboxResult {
        self.chooser.clear();
        self.chooser.queue(Answer::Pick(path.to_owned()));
        let state = self.state()?;
        let bounds = &state[key]["bounds"];
        let (left, width) = (px(&bounds[0]), px(&bounds[2]));
        let x = if width >= TOOLS_BESIDE_TEXT {
            left + width - ATTACH_FROM_RIGHT
        } else {
            left + ATTACH_FROM_LEFT
        };
        let attach = (x, px(&bounds[1]) + px(&bounds[3]) - ROW_FROM_BOTTOM);
        let asked = self.chooser.asked().len();
        let (x, y) = self.on_screen(attach);
        self.input.move_to(x, y)?;
        thread::sleep(super::STEP);
        self.click((x, y))?;
        wait_until(WAIT, || (self.chooser.asked().len() > asked).then_some(()))
            .ok_or_else(|| format!("Attach on the {key} box opened no file chooser"))?;
        Ok(())
    }

    /// The `POST`s of a message the mock got that hold `text`: the path and the body of each.
    pub(super) fn messages_with(&self, text: &str) -> Vec<(String, String)> {
        self.runtime
            .block_on(self.mock.received_requests())
            .iter()
            .filter(|request| request.method.as_str() == "POST" && request.url.path().ends_with("/messages"))
            .map(|request| {
                (
                    request.url.path().to_owned(),
                    String::from_utf8_lossy(&request.body).into_owned(),
                )
            })
            .filter(|(_, body)| body.contains(text))
            .collect()
    }

    /// Types `text` into the box `key` and sends it; the message the mock got.
    fn send_from(&mut self, key: &str, text: &str) -> SandboxResult<(String, String)> {
        let state = self.state()?;
        let at = self.on_screen(text_line_of(key, &state[key])?);
        self.click(at)?;
        self.type_text(text)?;
        self.press("enter")?;
        let sent = wait_until(WAIT, || self.messages_with(text).into_iter().next());
        sent.ok_or_else(|| format!("the mock got no message with {text}").into())
    }

    /// Both shots of the window, Teams Light and Teams Dark.
    pub(super) fn shots_light_and_dark(&mut self, name: &str) -> SandboxResult {
        thread::sleep(SETTLE);
        self.shot(name)?;
        self.ask("theme-dark")?;
        thread::sleep(SETTLE);
        self.shot(&format!("{name}-dark"))?;
        self.ask("theme-light")?;
        thread::sleep(SETTLE);
        Ok(())
    }
}

/// The folder's files of `name` that are not in the recycle bin, and those that are.
fn in_folder(user: &User<'_>, name: &str) -> (usize, usize) {
    let path = format!("{FOLDER}/{name}");
    user.mock
        .channel_files()
        .into_iter()
        .filter(|file| file.path == path)
        .fold((0, 0), |(kept, recycled), file| {
            if file.recycled {
                (kept, recycled + 1)
            } else {
                (kept + 1, recycled)
            }
        })
}

/// The files' states in `boxed`.
fn states(boxed: &Value) -> Vec<String> {
    boxed["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["state"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The sent message names the channel file as the web client does: `serviceName` "teams", and the file's name.
fn names_channel_file(body: &str, name: &str) -> SandboxResult {
    ensure(
        body.contains(r#"\"serviceName\":\"teams\""#) && body.contains(name),
        || format!("the message does not name the channel file {name}: {body}"),
    )
}

/// Attach on the post box: the chooser, the card while the file uploads, the ready card, the post with the file and
/// the file in the channel's folder.
fn attach_post(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up attached to the channel.txt";
    user.fresh_channel(false)?;
    let paths = user.made_up(&[NAME])?;
    user.runtime
        .block_on(user.mock.answer_next_after("POST", "addStub(@fn)", SLOW_STUB));
    user.attach_by_button("post", &paths[0])?;
    let asked = user.chooser.asked();
    say(&format!("  the file chooser: {:?}", asked.last()));
    user.until("the card while the file uploads", |state| {
        staged(&state["post"]) == [NAME] && states(&state["post"]) == ["uploading"]
    })?;
    user.shot("qa706-attach-uploading")?;
    let state = user.until("the ready card", |state| ready(&state["post"], &[NAME]))?;
    ensure(problem_of(&state["post"]).is_empty(), || {
        format!("the post box said: {state}")
    })?;
    user.shots_light_and_dark("qa706-attach-ready")?;
    ensure(in_folder(user, NAME) == (1, 0), || {
        format!("the folder holds {:?}", user.mock.channel_files())
    })?;
    let mark = user.mark();
    let (path, body) = user.send_from("post", &format!("A made-up post with a file {mark}"))?;
    names_channel_file(&body, NAME)?;
    ensure(!path.contains(";messageid="), || {
        format!("the post went to a thread: {path}")
    })?;
    user.until("the box empty after Send", |state| staged(&state["post"]).is_empty())?;
    user.shots_light_and_dark("qa706-attach-posted")?;
    ensure(in_folder(user, NAME) == (1, 0), || {
        format!("after Send the folder holds {:?}", user.mock.channel_files())
    })
}

/// A drop on the post box while the thread shows: the post box alone lights up and takes the file. A paste of a
/// file manager's copy into the reply box: the reply box alone takes it.
fn drop_and_paste(user: &mut User<'_>) -> SandboxResult {
    const DROPPED: &str = "made-up dropped on the post box.txt";
    const PASTED: &str = "made-up pasted into the reply box.txt";
    let state = user.fresh_channel(true)?;
    let dropped = user.made_up(&[DROPPED])?;
    let held = user.hover(DOLPHIN, &dropped, box_centre(&state["post"])?, CALM)?;
    let hover = user.until("the post box's highlight", |state| state["post"]["drop_active"] == true)?;
    ensure(hover["thread"]["drop_active"] != true, || {
        format!("the reply box lit up for the post box: {hover}")
    })?;
    user.shot("qa706-drop-post-hover")?;
    user.release(held)?;
    let state = user.until("the post box's file", |state| ready(&state["post"], &[DROPPED]))?;
    ensure(staged(&state["thread"]).is_empty(), || {
        format!("the reply box took the post box's file: {state}")
    })?;
    let pasted = user.made_up(&[PASTED])?;
    let (uris, _) = file_lists(&pasted);
    user.sandbox
        .set_clipboard(vec![("text/uri-list".to_owned(), uris.into_bytes())])?;
    thread::sleep(super::STEP);
    let at = user.on_screen(text_line_of("thread", &state["thread"])?);
    user.click(at)?;
    user.press("ctrl+v")?;
    let state = user.until("the reply box's pasted file", |state| {
        ready(&state["thread"], &[PASTED])
    })?;
    ensure(staged(&state["post"]) == [DROPPED], || {
        format!("the post box changed for the paste: {state}")
    })?;
    ensure(text_of_box(&state["thread"]).is_empty(), || {
        format!("the paste put text in the reply box: {state}")
    })?;
    user.shot("qa706-drop-and-paste")?;
    ensure(
        in_folder(user, DROPPED) == (1, 0) && in_folder(user, PASTED) == (1, 0),
        || format!("the folder holds {:?}", user.mock.channel_files()),
    )?;
    user.ask("clear")?;
    Ok(())
}

/// Where the user clicks to type in the box `key`.
fn text_line_of(key: &str, boxed: &Value) -> SandboxResult<(u32, u32)> {
    if key == "thread" {
        let bounds = &boxed["bounds"];
        return Ok((px(&bounds[0]) + 60, px(&bounds[1]) + REPLY_TEXT_FROM_TOP));
    }
    text_line(boxed)
}

fn text_of_box(boxed: &Value) -> String {
    boxed["text"].as_str().unwrap_or_default().trim().to_owned()
}

/// Attach on the reply box, then Send: the reply goes to the post's thread and names the file.
fn reply(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up for the reply.txt";
    user.fresh_channel(true)?;
    let paths = user.made_up(&[NAME])?;
    user.attach_by_button("thread", &paths[0])?;
    let state = user.until("the reply box's card", |state| ready(&state["thread"], &[NAME]))?;
    ensure(staged(&state["post"]).is_empty(), || {
        format!("the post box took the reply's file: {state}")
    })?;
    user.shots_light_and_dark("qa706-reply-attached")?;
    let mark = user.mark();
    let (path, body) = user.send_from("thread", &format!("A made-up reply with a file {mark}"))?;
    names_channel_file(&body, NAME)?;
    ensure(path.contains(&format!(";messageid={CHANNEL_POST}")), || {
        format!("the reply did not go to the post's thread: {path}")
    })?;
    user.until("the reply box empty after Send", |state| {
        staged(&state["thread"]).is_empty()
    })?;
    user.shots_light_and_dark("qa706-reply-sent")?;
    user.ask("close-thread")?;
    Ok(())
}

/// The same name twice in the post box: the copy gets a number and the yellow notice, which closes.
fn same_name(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up twice.txt";
    const COPY: &str = "made-up twice 1.txt";
    let state = user.fresh_channel(false)?;
    let paths = user.made_up(&[NAME])?;
    user.drop_files(DOLPHIN, &paths, box_centre(&state["post"])?, CALM)?;
    let first = user.until("the first file", |state| ready(&state["post"], &[NAME]))?;
    let before = px(&first["post"]["bounds"][1]);
    // another file of the same name, from another folder
    let again = user.made_up(&[NAME])?;
    user.drop_files(DOLPHIN, &again, box_centre(&first["post"])?, CALM)?;
    let state = user.until("the numbered copy", |state| ready(&state["post"], &[COPY, NAME]))?;
    say(&format!(
        "  the box's top: {before} with one file, {} with the copy",
        px(&state["post"]["bounds"][1])
    ));
    user.shots_light_and_dark("qa706-same-name-notice")?;
    ensure(
        in_folder(user, NAME) == (1, 0) && in_folder(user, COPY) == (1, 0),
        || format!("the folder holds {:?}", user.mock.channel_files()),
    )?;
    let bounds = &state["post"]["bounds"];
    let close = (
        px(&bounds[0]) + px(&bounds[2]) - NOTICE_CLOSE_FROM_RIGHT,
        px(&bounds[1]).saturating_sub(if px(&bounds[2]) >= TOOLS_BESIDE_TEXT {
            NOTICE_CLOSE_ABOVE_WIDE
        } else {
            NOTICE_CLOSE_ABOVE_NARROW
        }),
    );
    let shown = user.shot("qa706-same-name-before-close")?;
    user.click(user.on_screen(close))?;
    user.park()?;
    thread::sleep(SETTLE);
    let closed = user.shot("qa706-same-name-closed")?;
    ensure(notice_gone(&shown, &closed, close)?, || {
        format!("the yellow notice still shows after its close at {close:?}")
    })?;
    let state = user.state()?;
    ensure(ready(&state["post"], &[COPY, NAME]), || {
        format!("the close of the notice changed the files: {state}")
    })?;
    user.ask("clear")?;
    Ok(())
}

/// Whether the notice's yellow is at `close`'s row in `before` and not in `after`.
fn notice_gone(before: &Path, after: &Path, close: (u32, u32)) -> SandboxResult<bool> {
    let yellow_at = |shot: &Path| -> SandboxResult<bool> {
        let image = image::open(shot)?.to_rgb8();
        let [red, green, blue] = image.get_pixel(close.0.saturating_sub(40), close.1).0;
        Ok(red > 200 && green > 200 && u16::from(red) > u16::from(blue) + 25)
    };
    let (shown, still) = (yellow_at(before)?, yellow_at(after)?);
    say(&format!(
        "  the notice's yellow: {shown} before the close, {still} after"
    ));
    Ok(shown && !still)
}

/// An uploaded file removed before Send goes to the site's recycle bin.
fn remove(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up removed before send.txt";
    let state = user.fresh_channel(false)?;
    let paths = user.made_up(&[NAME])?;
    user.drop_files(DOLPHIN, &paths, box_centre(&state["post"])?, CALM)?;
    let state = user.until("the card", |state| ready(&state["post"], &[NAME]))?;
    let bounds = &state["post"]["bounds"];
    let at = (px(&bounds[0]) + REMOVE_FROM_LEFT, px(&bounds[1]) + REMOVE_FROM_TOP);
    user.shot("qa706-remove-before")?;
    user.click(user.on_screen(at))?;
    user.park()?;
    user.until("the card gone", |state| staged(&state["post"]).is_empty())?;
    wait_until(WAIT, || (in_folder(user, NAME) == (0, 1)).then_some(())).ok_or_else(|| {
        format!(
            "the removed file is not in the recycle bin: {:?}",
            user.mock.channel_files()
        )
    })?;
    user.shot("qa706-removed")?;
    Ok(())
}

/// A step of the upload that fails: the card says so, and the folder keeps no file outside the recycle bin.
fn fail_at(user: &mut User<'_>, step: &str, name: &str) -> SandboxResult {
    let state = user.fresh_channel(false)?;
    let paths = user.made_up(&[name])?;
    let error =
        json!({"odata.error": {"code": "-1, Made-up", "message": {"lang": "en-US", "value": "Made-up failure"}}});
    user.runtime
        .block_on(user.mock.answer_next_containing("POST", step, 500, &error));
    user.drop_files(DOLPHIN, &paths, box_centre(&state["post"])?, CALM)?;
    let state = user.until("the failed card", |state| {
        staged(&state["post"]) == [name] && states(&state["post"]) == ["failed"]
    })?;
    say(&format!("  {step} failed: {}", state["post"]));
    thread::sleep(SETTLE);
    let shot = format!("qa706-fail-{}", step.trim_matches(|c: char| !c.is_ascii_alphanumeric()));
    user.shot(&shot)?;
    let (kept, _) = in_folder(user, name);
    let settled = wait_until(WAIT, || (in_folder(user, name).0 == 0).then_some(()));
    ensure(settled.is_some(), || {
        format!(
            "after a failed {step}, {kept} file(s) stay in the folder: {:?}",
            user.mock.channel_files()
        )
    })?;
    user.ask("clear")?;
    Ok(())
}

fn fail_add_stub(user: &mut User<'_>) -> SandboxResult {
    fail_at(user, "addStub(", "made-up stub fails.txt")
}

fn fail_start_upload(user: &mut User<'_>) -> SandboxResult {
    fail_at(user, "/StartUpload(", "made-up start fails.txt")
}

fn fail_finish_upload(user: &mut User<'_>) -> SandboxResult {
    fail_at(user, "/FinishUpload(", "made-up finish fails.txt")
}

/// Teams Dark: a drop on each box, the cards in both boxes.
fn dark(user: &mut User<'_>) -> SandboxResult {
    const NAME: &str = "made-up in the dark.txt";
    let state = user.fresh_channel(true)?;
    user.ask("theme-dark")?;
    let paths: Vec<PathBuf> = user.made_up(&[NAME])?;
    let held = user.hover(DOLPHIN, &paths, box_centre(&state["thread"])?, CALM)?;
    user.until("the reply box's highlight", |state| {
        state["thread"]["drop_active"] == true
    })?;
    thread::sleep(SETTLE);
    user.shot("qa706-dark-reply-hover")?;
    user.release(held)?;
    let state = user.until("the reply box's file", |state| ready(&state["thread"], &[NAME]))?;
    user.drop_files(DOLPHIN, &paths, box_centre(&state["post"])?, CALM)?;
    user.until("the post box's copy", |state| {
        ready(&state["post"], &["made-up in the dark 1.txt"])
    })?;
    thread::sleep(SETTLE);
    user.shot("qa706-dark-both")?;
    user.ask("clear")?;
    user.ask("theme-light")?;
    user.ask("close-thread")?;
    Ok(())
}
