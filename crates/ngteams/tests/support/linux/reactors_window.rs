//! Who reacted to a message, as a user reaches it: the real app in the UI sandbox, on the mock, driven by the
//! sandbox's own pointer and keyboard (`harness = false`: GPUI runs on the process's main thread).
//!
//! The test runs itself again inside the sandbox as the app of lab account a, its engine on the mock chat service and
//! its Trouter. Before the engine starts, b's message in the a–b chat (its oldest: the mock's clock is older than its
//! fixtures') gets four reactions: a like from c, b and a (given in store order a, c, b, so only the sort by time lists
//! them c, b, a), a heart from b alone, a key Teams doesn't offer, from b, and a laugh from more people than the card
//! shows at once (the pills in that order, by who reacted first). The child opens the chat and
//! answers the parent's questions about the chat view (the card shown, the pill under the keys, the selection, each
//! pill's count and whether it is a's) through a file in the sandbox dir; everything a user does comes from the
//! sandbox's virtual pointer and keyboard.
//!
//! - **Keys**: Escape in the compose box hands the focus to the messages, Up walks to b's message; Right moves through
//!   its pills (the pill under the keys drawn in the selection colour, which is how the parent finds each pill on the
//!   screen, and its card shown at once), stops at the last, Left goes back and leaves the pills from the first;
//!   Escape closes the card first and clears the selection next; another message selected leaves the pills; Space sets and takes
//!   back a's own heart.
//! - **Pointer**: a pill rested on for [`DELAY`] shows its card, not before; passing quickly over one shows none; the
//!   card stays while the pointer is on the pill or the card and closes [`GRACE`] after it left both; with a card
//!   shown, another pill shows its own at once; the wheel over a long card scrolls the card, not the messages; a click
//!   sets and takes back a's own reaction, only for a reaction Teams offers.
//! - **A pill gone**: b takes back the heart under the keys: its pill and card go, Space sets nothing, and the next
//!   Escape clears the selection.
//!
//! Screenshots of the cards, by pointer and by keys, are kept under `target/ui-shots/reactors-*.png`.
//!
//! Without weston 15, niri or `dbus-daemon` it says so and passes, unless `GPUI_SMOKE_REQUIRE=1`.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use std::{fs, future, thread};

use gpui::{App, AsyncApp, Entity, Window, WindowHandle, WindowingModes};
use ngteams::app::{self, Launch};
#[path = "../window.rs"]
mod support;
use support::Alias;
use support::display_name;
use teams_auth::secrets::Secret;
use teams_auth::session::{AccessToken, Renewal};
use teams_auth::token::TokenError;
use teams_core::Outbox;
use teams_core::engine::{self, EngineHandle, EngineOptions, Parts, Push};
use teams_mock::{
    AB_CHAT, AMS_BASE, CHAT_BASE, CSA_BASE, ChatEvent, ChatServiceMock, EventKind, LAB_A, LAB_B, LAB_C,
    MIDDLE_TIER_BASE, PRESENCE_BASE, PROVISIONING_BASE, TrouterMock, USER_PROFILE_BASE,
};
use teams_proto::Mri;
use teams_proto::{Author, Client, Credentials, HttpTransport};
use teams_store::Store;
use teams_trouter::{Options, Trouter, TrouterCredentials};
use teams_ui::chat_view::ChatView;
use teams_ui::chat_view::reactors::{DELAY, GRACE};
use teams_ui::shell::Shell;
use teams_ui::view::Root;
use teams_ui::window::KitRoot;
use tokio::sync::broadcast;
use ui_sandbox::input::{VirtualInput, keys};
use ui_sandbox::{NIRI_FULL_WINDOW, Running, Sandbox, SandboxOptions, SandboxResult, sandbox_or_skip};

/// Which mode the child runs in.
const CHILD_ENV: &str = "NGTEAMS_REACTORS_TEST";
/// The file in the sandbox dir the parent writes a question to (`<n> <verb>`); the child answers `re <n> <answer>`.
const ASK: &str = "reactors-ask";
/// From the child's start to its `ready` (the mock started, the window drawn).
const STARTUP: Duration = Duration::from_secs(90);
/// Anything else the parent waits for.
const WAIT: Duration = Duration::from_secs(20);
/// How often the parent looks again while it waits for a state.
const POLL: Duration = Duration::from_millis(40);
/// How long the views get to settle where nothing is awaited (names looked up, a frame drawn).
const SETTLE: Duration = Duration::from_millis(1500);
/// How long the parent watches for something that must not happen.
const QUIET: Duration = Duration::from_millis(1200);

/// The reactions on b's message, in the order its pills show.
const LIKE: &str = "like";
const HEART: &str = "heart";
const LAUGH: &str = "laugh";
/// A key Teams doesn't offer: its pill only shows.
const OTHER: &str = "made-up-key";
/// How many people gave the laugh: more than the card shows at once.
const MANY: usize = 16;
/// The most room a card takes beside its pill, either way: a little over its 320 px.
const CARD_REACH: u32 = 340;

pub(super) fn main() -> ExitCode {
    let outcome = match std::env::var(CHILD_ENV).as_deref() {
        Ok("app") => child(),
        Ok(other) => Err(format!("unknown mode {other}").into()),
        Err(_) => parent(),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reactors window test: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Prints `line` to stdout at once.
fn say(line: &str) {
    let mut stdout = io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

// --- the parent: the sandbox, the user's pointer and keys, and the checks ---

fn parent() -> SandboxResult {
    let options = SandboxOptions {
        niri_extra: NIRI_FULL_WINDOW,
        ..SandboxOptions::default()
    };
    let Some(sandbox) = sandbox_or_skip("ngteams reactors window", &options)? else {
        return Ok(());
    };
    let child = sandbox.spawn(&std::env::current_exe()?, &[], &[(CHILD_ENV, "app")])?;
    let running = Running::new(child)?;
    let mut user = User::start(&sandbox, running)?;
    match who_reacted(&mut user) {
        Ok(()) => {
            user.ask("quit")?;
            let (status, stderr) = user.running.finish(WAIT)?;
            if !status.success() {
                return Err(format!("the app exited {status}: {stderr}").into());
            }
            say("ngteams reactors window: ok");
            Ok(())
        }
        Err(error) => {
            let shot = user
                .shot("reactors-failed.png")
                .map_or_else(|_| String::new(), |path| path.display().to_string());
            Err(user
                .running
                .fail(&format!("{error} (last screen: {shot}){}", sandbox.logs())))
        }
    }
}

/// The whole walk: the keys first (they find where each pill is drawn), then the pointer, then a pill gone.
fn who_reacted(user: &mut User) -> SandboxResult {
    let pills = keys_go_through_the_pills(user)?;
    the_pointer_rests_on_a_pill(user, &pills)?;
    the_card_stays_while_the_pointer_is_on_it(user, &pills)?;
    another_pill_shows_its_card_at_once(user, &pills)?;
    passing_over_a_pill_shows_no_card(user, &pills)?;
    a_long_card_scrolls_and_the_messages_stay(user, &pills)?;
    a_click_sets_and_takes_back_the_account_s_own(user, &pills)?;
    space_sets_and_takes_back_the_account_s_own(user)?;
    a_pill_gone_under_the_keys_takes_nothing(user)
}

/// A rectangle on the screen, in output pixels: `x0..x1`, `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn center(self) -> (u32, u32) {
        (u32::midpoint(self.x0, self.x1), u32::midpoint(self.y0, self.y1))
    }

    fn contains(self, (x, y): (u32, u32)) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }

    fn overlaps(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    fn grown(self, by: u32) -> Self {
        Self {
            x0: self.x0.saturating_sub(by),
            y0: self.y0.saturating_sub(by),
            x1: self.x1 + by,
            y1: self.y1 + by,
        }
    }
}

/// Where a pill and its card were drawn, found from the keys' highlight.
#[derive(Clone, Copy, Debug)]
struct Pill {
    pill: Rect,
    card: Rect,
}

/// The pills of b's message, by key.
struct Pills {
    like: Pill,
    heart: Pill,
    laugh: Pill,
    other: Pill,
}

/// The chat view as the child reads it.
#[derive(Debug, Default, PartialEq, Eq)]
struct State {
    /// The pill (key) whose card is shown, on b's message.
    shown: Option<String>,
    /// The pill (key) under the keys, on b's message.
    focus: Option<String>,
    /// Whether b's message is the one selected; `None` when none is.
    selected: Option<bool>,
    /// Each pill of b's message: its key, count and whether it holds a's own, in the order they show.
    pills: Vec<(String, usize, bool)>,
    /// Who gave the like, in the order the card lists them.
    likers: Vec<String>,
    /// The message list's scroll position.
    scroll: String,
}

impl State {
    fn parse(answer: &str) -> SandboxResult<Self> {
        let mut state = Self::default();
        for field in answer.split(' ') {
            let (name, value) = field
                .split_once('=')
                .ok_or_else(|| format!("odd state field {field:?}"))?;
            let some = (value != "-").then(|| value.to_owned());
            match name {
                "shown" => state.shown = some,
                "focus" => state.focus = some,
                "selected" => state.selected = some.map(|value| value == "b"),
                "pills" => {
                    for pill in value.split(',').filter(|pill| !pill.is_empty()) {
                        let mut parts = pill.split(':');
                        let key = parts.next().ok_or("a pill without a key")?.to_owned();
                        let count = parts.next().ok_or("a pill without a count")?.parse()?;
                        let mine = parts.next() == Some("mine");
                        state.pills.push((key, count, mine));
                    }
                }
                "likers" => {
                    state.likers = value
                        .split(',')
                        .filter(|mri| !mri.is_empty())
                        .map(str::to_owned)
                        .collect();
                }
                "scroll" => value.clone_into(&mut state.scroll),
                _ => return Err(format!("odd state field {field:?}").into()),
            }
        }
        Ok(state)
    }

    fn pill(&self, key: &str) -> Option<(usize, bool)> {
        self.pills
            .iter()
            .find(|(shown, _, _)| shown == key)
            .map(|(_, count, mine)| (*count, *mine))
    }

    fn shows(&self, key: &str) -> bool {
        self.shown.as_deref() == Some(key)
    }
}

/// The user at the sandbox: its pointer and keyboard, its screenshots, and the child it asks about the views.
struct User<'a> {
    sandbox: &'a Sandbox,
    running: Running,
    input: VirtualInput,
    asked: u32,
}

impl<'a> User<'a> {
    fn start(sandbox: &'a Sandbox, running: Running) -> SandboxResult<Self> {
        running
            .line_where(STARTUP, |line| line == "ready")
            .map_err(|error| format!("no ready line: {error:?}"))?;
        let mut user = Self {
            sandbox,
            running,
            input: sandbox.input()?,
            asked: 0,
        };
        user.park()?;
        user.ask("open")?;
        // b's message and its four pills, then the names looked up
        user.until("b's message with its pills", |state| state.pills.len() == 4)?;
        thread::sleep(SETTLE);
        Ok(user)
    }

    /// Asks the child `verb`; its answer.
    fn ask(&mut self, verb: &str) -> SandboxResult<String> {
        self.asked += 1;
        let staged = self.sandbox.path(&format!("{ASK}.staged"));
        fs::write(&staged, format!("{} {verb}", self.asked))?;
        fs::rename(&staged, self.sandbox.path(ASK))?;
        let prefix = format!("re {} ", self.asked);
        let line = self
            .running
            .line_where(WAIT, |line| line.starts_with(&prefix))
            .map_err(|error| format!("no answer to {verb}: {error:?}"))?;
        Ok(line[prefix.len()..].to_owned())
    }

    fn state(&mut self) -> SandboxResult<State> {
        State::parse(&self.ask("state")?)
    }

    /// Waits up to [`WAIT`] for a state `wanted` takes, named `what` in the error; that state.
    fn until(&mut self, what: &str, wanted: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if wanted(&state) {
                return Ok(state);
            }
            if start.elapsed() > WAIT {
                return Err(format!("{what}: never came; last {state:?}").into());
            }
            thread::sleep(POLL);
        }
    }

    /// Watches for [`QUIET`] that the state stays as `kept` wants, named `what` in the error.
    fn stays(&mut self, what: &str, kept: impl Fn(&State) -> bool) -> SandboxResult<State> {
        let start = Instant::now();
        loop {
            let state = self.state()?;
            if !kept(&state) {
                return Err(format!("{what}: changed after {:?}: {state:?}", start.elapsed()).into());
            }
            if start.elapsed() > QUIET {
                return Ok(state);
            }
            thread::sleep(POLL);
        }
    }

    fn key(&mut self, key: u32) -> SandboxResult {
        self.input.key(key)
    }

    fn move_to(&mut self, (x, y): (u32, u32)) -> SandboxResult {
        self.input.move_to(x, y)
    }

    /// Moves the pointer from `from` to `to` in steps of a few pixels, as a hand moves a mouse.
    fn glide(&mut self, from: (u32, u32), to: (u32, u32)) -> SandboxResult {
        let steps = from.0.abs_diff(to.0).max(from.1.abs_diff(to.1)).div_ceil(4).max(1);
        for step in 1..=steps {
            let along = |a: u32, b: u32| {
                let (a, b) = (i64::from(a), i64::from(b));
                u32::try_from(a + (b - a) * i64::from(step) / i64::from(steps)).unwrap_or_default()
            };
            self.input.move_to(along(from.0, to.0), along(from.1, to.1))?;
        }
        Ok(())
    }

    /// The pointer on the chat's header, away from every message and pill.
    fn park(&mut self) -> SandboxResult {
        self.input.move_to(900, 24)
    }

    /// Screenshots the screen into the sandbox dir and keeps a copy under `target/ui-shots/`.
    fn shot(&self, name: &str) -> SandboxResult<PathBuf> {
        let png = self.sandbox.screenshot(name, false)?;
        fs::copy(&png, shots_dir()?.join(name))?;
        Ok(png)
    }
}

/// Where the screenshots are kept: `target/ui-shots/`, never committed.
fn shots_dir() -> io::Result<PathBuf> {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .parent()
        .map_or_else(|| PathBuf::from("target"), Path::to_owned)
        .join("ui-shots");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The pixels of `after` drawn in the palette's selection colour that weren't in `before`: the pill under the keys.
fn highlighted(before: &Path, after: &Path) -> SandboxResult<Rect> {
    let wanted = teams_ui::theme::LIGHT_PALETTE.selection.to_be_bytes();
    let near = |pixel: &image::Rgb<u8>| {
        pixel
            .0
            .iter()
            .zip(&wanted[1..])
            .all(|(&got, &want)| got.abs_diff(want) <= 6)
    };
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    bounds_of(&after, |x, y, pixel| near(pixel) && !near(before.get_pixel(x, y)))
        .ok_or_else(|| "no pill drawn in the selection colour".into())
}

/// The pixels of `after` that differ from `before` around `pill` but not on it: the card shown. Only where a card of
/// that pill can be (centred on it, at most [`CARD_REACH`] wide and tall, under or over it), so nothing else that
/// changed on the screen meanwhile counts.
fn card_drawn(before: &Path, after: &Path, pill: Rect) -> SandboxResult<Rect> {
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    let around = pill.grown(3);
    let (middle, _) = pill.center();
    let reach = Rect {
        x0: middle.saturating_sub(CARD_REACH / 2),
        y0: pill.y0.saturating_sub(CARD_REACH),
        x1: middle + CARD_REACH / 2,
        y1: pill.y1 + CARD_REACH,
    };
    bounds_of(&after, |x, y, pixel| {
        reach.contains((x, y))
            && !around.contains((x, y))
            && pixel
                .0
                .iter()
                .zip(before.get_pixel(x, y).0)
                .any(|(&got, was)| got.abs_diff(was) > 48)
    })
    .ok_or_else(|| "no card drawn".into())
}

/// The bounds of the pixels `taken` takes, if any.
fn bounds_of(shot: &image::RgbImage, taken: impl Fn(u32, u32, &image::Rgb<u8>) -> bool) -> Option<Rect> {
    let mut found: Option<Rect> = None;
    for (x, y, pixel) in shot.enumerate_pixels() {
        if !taken(x, y, pixel) {
            continue;
        }
        found = Some(found.map_or(
            Rect {
                x0: x,
                y0: y,
                x1: x + 1,
                y1: y + 1,
            },
            |rect| Rect {
                x0: rect.x0.min(x),
                y0: rect.y0.min(y),
                x1: rect.x1.max(x + 1),
                y1: rect.y1.max(y + 1),
            },
        ));
    }
    found
}

/// Whether the pixels of `rect` differ between two screenshots.
fn changed_within(before: &Path, after: &Path, rect: Rect) -> SandboxResult<bool> {
    let (before, after) = (image::open(before)?.to_rgb8(), image::open(after)?.to_rgb8());
    Ok(bounds_of(&after, |x, y, pixel| {
        rect.contains((x, y))
            && pixel
                .0
                .iter()
                .zip(before.get_pixel(x, y).0)
                .any(|(&got, was)| got.abs_diff(was) > 48)
    })
    .is_some())
}

/// With b's message selected by the keys: Right reaches each pill in turn (found on the screen by its highlight), its
/// card shown at once and never on the pill, and stops at the last; Left goes back and leaves the pills from the
/// first; Escape closes the card first, then clears the selection; another message selected leaves the pills.
fn keys_go_through_the_pills(user: &mut User) -> SandboxResult<Pills> {
    // the compose box hands the focus to the messages
    user.key(keys::ESC)?;
    select_b_s_message(user)?;
    thread::sleep(SETTLE);
    let none = user.shot("reactors-keys-none.png")?;
    let mut found = Vec::new();
    for key in [LIKE, HEART, OTHER, LAUGH] {
        let pressed = Instant::now();
        user.key(keys::RIGHT)?;
        let state = user.until(&format!("Right reaches the {key}"), |state| {
            state.focus.as_deref() == Some(key)
        })?;
        if !state.shows(key) || pressed.elapsed() >= DELAY {
            return Err(format!("the {key}'s card isn't shown at once by the keys: {state:?}").into());
        }
        thread::sleep(SETTLE);
        let shot = user.shot(&format!("reactors-keys-{key}.png"))?;
        let pill = highlighted(&none, &shot)?;
        let card = card_drawn(&none, &shot, pill)?;
        if card.overlaps(pill) {
            return Err(format!("the {key}'s card {card:?} is on its pill {pill:?}").into());
        }
        if !(120..=330).contains(&(card.x1 - card.x0)) || card.y0 < pill.y1 {
            return Err(format!("the {key}'s card {card:?} isn't a card under its pill {pill:?}").into());
        }
        found.push(Pill { pill, card });
    }
    let [like, heart, other, laugh] = found[..] else {
        return Err("four pills".into());
    };
    if !(like.pill.x1 <= heart.pill.x0 && heart.pill.x1 <= other.pill.x0 && other.pill.x1 <= laugh.pill.x0) {
        return Err(format!("the pills aren't in a row: {found:?}").into());
    }
    user.key(keys::RIGHT)?;
    user.stays("Right stops at the last pill", |state| {
        state.focus.as_deref() == Some(LAUGH) && state.shows(LAUGH)
    })?;
    for key in [OTHER, HEART, LIKE] {
        user.key(keys::LEFT)?;
        user.until(&format!("Left back to the {key}"), |state| {
            state.focus.as_deref() == Some(key) && state.shows(key)
        })?;
    }
    user.key(keys::LEFT)?;
    user.until("Left on the first pill leaves the pills", |state| {
        state.focus.is_none() && state.shown.is_none() && state.selected == Some(true)
    })?;

    user.key(keys::RIGHT)?;
    user.until("Right on the first pill again", |state| {
        state.focus.as_deref() == Some(LIKE)
    })?;
    user.key(keys::ESC)?;
    user.until("Escape closes the card, the message still selected", |state| {
        state.focus.is_none() && state.shown.is_none() && state.selected == Some(true)
    })?;
    user.key(keys::ESC)?;
    user.until("a second Escape clears the selection", |state| state.selected.is_none())?;

    select_b_s_message(user)?;
    user.key(keys::RIGHT)?;
    user.until("Right on the like", |state| state.focus.as_deref() == Some(LIKE))?;
    user.key(keys::DOWN)?;
    user.until("Down selects the message after and leaves the pills", |state| {
        state.selected == Some(false) && state.focus.is_none() && state.shown.is_none()
    })?;
    user.key(keys::UP)?;
    user.until("Up selects b's message, no pill under the keys", |state| {
        state.selected == Some(true) && state.focus.is_none()
    })?;
    user.key(keys::ESC)?;
    user.until("Escape clears the selection", |state| state.selected.is_none())?;
    Ok(Pills {
        like,
        heart,
        laugh,
        other,
    })
}

/// Up from the newest message, as often as it takes, to b's (the chat's oldest: the mock's clock is older than its
/// fixtures').
fn select_b_s_message(user: &mut User) -> SandboxResult {
    for _ in 0..12 {
        user.key(keys::UP)?;
        thread::sleep(POLL);
        if user.state()?.selected == Some(true) {
            return Ok(());
        }
    }
    Err(format!("Up never reached b's message: {:?}", user.state()?).into())
}

/// The pointer resting on the like: no card before [`DELAY`], then the card listing c, b and a in the order they
/// reacted; it closes no sooner than [`GRACE`] after the pointer left.
fn the_pointer_rests_on_a_pill(user: &mut User, pills: &Pills) -> SandboxResult {
    user.park()?;
    let moved = Instant::now();
    user.move_to(pills.like.pill.center())?;
    let state = user.state()?;
    if state.shown.is_some() && moved.elapsed() < DELAY {
        return Err(format!("a card before the pointer rested: {state:?}").into());
    }
    let state = user.until("the like's card after the pointer rested", |state| state.shows(LIKE))?;
    if moved.elapsed() < DELAY {
        return Err(format!(
            "the card came {:?} after the pointer, before {DELAY:?}",
            moved.elapsed()
        )
        .into());
    }
    if state.likers != [LAB_C, LAB_B, LAB_A] {
        return Err(format!(
            "the likers aren't in the order they reacted (c, b, a): {:?}",
            state.likers
        )
        .into());
    }
    thread::sleep(SETTLE);
    let shot = user.shot("reactors-pointer-like.png")?;
    let none = user.sandbox.path("reactors-keys-none.png");
    if !changed_within(&none, &shot, pills.like.card)? {
        return Err("the pointer's card isn't where the keys' was".into());
    }
    let left = Instant::now();
    user.park()?;
    user.until("the card closes once the pointer left", |state| state.shown.is_none())?;
    if left.elapsed() < GRACE {
        return Err(format!(
            "the card closed {:?} after the pointer left, before {GRACE:?}",
            left.elapsed()
        )
        .into());
    }
    Ok(())
}

/// The card stays while the pointer goes from the pill onto it and back, and closes once it left both.
fn the_card_stays_while_the_pointer_is_on_it(user: &mut User, pills: &Pills) -> SandboxResult {
    user.move_to(pills.like.pill.center())?;
    user.until("the like's card", |state| state.shows(LIKE))?;
    let (pill, card) = (pills.like.pill.center(), pills.like.card.center());
    user.glide(pill, card)?;
    user.stays("the card stays while the pointer moves onto it", |state| {
        state.shows(LIKE)
    })?;
    user.glide(card, pill)?;
    user.stays("the card stays with the pointer back on its pill", |state| {
        state.shows(LIKE)
    })?;
    user.move_to(pills.like.card.center())?;
    user.stays("the card stays while the pointer jumps onto it", |state| {
        state.shows(LIKE)
    })?;
    user.move_to(pills.like.pill.center())?;
    user.stays("the card stays with the pointer back on its pill", |state| {
        state.shows(LIKE)
    })?;
    user.move_to(pills.like.card.center())?;
    user.park()?;
    user.until("the card closes once the pointer left the card", |state| {
        state.shown.is_none()
    })?;
    Ok(())
}

/// With a card shown, the pointer on another pill shows that pill's card at once: the heart's, b alone.
fn another_pill_shows_its_card_at_once(user: &mut User, pills: &Pills) -> SandboxResult {
    user.move_to(pills.like.pill.center())?;
    user.until("the like's card", |state| state.shows(LIKE))?;
    let moved = Instant::now();
    user.move_to(pills.heart.pill.center())?;
    user.until("the heart's card", |state| state.shows(HEART))?;
    if moved.elapsed() >= DELAY {
        return Err(format!("the heart's card waited {:?}, not shown at once", moved.elapsed()).into());
    }
    thread::sleep(SETTLE);
    user.shot("reactors-pointer-heart.png")?;
    user.park()?;
    user.until("the heart's card closes", |state| state.shown.is_none())?;
    Ok(())
}

/// The pointer passing over a pill without resting shows no card.
fn passing_over_a_pill_shows_no_card(user: &mut User, pills: &Pills) -> SandboxResult {
    user.move_to(pills.like.pill.center())?;
    user.park()?;
    user.stays("no card after the pointer passed over the like", |state| {
        state.shown.is_none()
    })?;
    Ok(())
}

/// The laugh's card lists more people than it shows: the wheel over it scrolls the card, the messages stay.
fn a_long_card_scrolls_and_the_messages_stay(user: &mut User, pills: &Pills) -> SandboxResult {
    user.move_to(pills.laugh.pill.center())?;
    let before = user.until("the laugh's card", |state| state.shows(LAUGH))?;
    thread::sleep(SETTLE);
    let top = user.shot("reactors-many-top.png")?;
    let card = pills.laugh.card;
    let height = card.y1 - card.y0;
    if !(200..=340).contains(&height) {
        return Err(format!("the long card isn't capped at its height: {card:?}").into());
    }
    user.move_to(card.center())?;
    let (x, y) = card.center();
    user.input.scroll(x, y, 12)?;
    thread::sleep(SETTLE);
    let scrolled = user.shot("reactors-many-scrolled.png")?;
    let after = user.state()?;
    if !after.shows(LAUGH) {
        return Err(format!("the card closed under the wheel: {after:?}").into());
    }
    if after.scroll != before.scroll {
        return Err(format!("the messages scrolled: {} to {}", before.scroll, after.scroll).into());
    }
    if !changed_within(&top, &scrolled, card)? {
        return Err("the card's list didn't scroll".into());
    }
    user.park()?;
    user.until("the laugh's card closes", |state| state.shown.is_none())?;
    Ok(())
}

/// A click on the heart sets a's own (2, a's), a second takes it back (1, not a's); a click on the key Teams doesn't
/// offer sets nothing.
fn a_click_sets_and_takes_back_the_account_s_own(user: &mut User, pills: &Pills) -> SandboxResult {
    let heart = pills.heart.pill.center();
    user.input.click(heart.0, heart.1, keys::BTN_LEFT)?;
    user.until("a click sets a's heart", |state| state.pill(HEART) == Some((2, true)))?;
    thread::sleep(SETTLE);
    user.shot("reactors-heart-mine.png")?;
    user.input.click(heart.0, heart.1, keys::BTN_LEFT)?;
    user.until("a second click takes it back", |state| {
        state.pill(HEART) == Some((1, false))
    })?;
    let other = pills.other.pill.center();
    user.input.click(other.0, other.1, keys::BTN_LEFT)?;
    user.stays("a click on a key Teams doesn't offer sets nothing", |state| {
        state.pill(OTHER) == Some((1, false))
    })?;
    user.park()?;
    user.until("no card", |state| state.shown.is_none())?;
    Ok(())
}

/// Space on the heart under the keys sets a's own, and again takes it back.
fn space_sets_and_takes_back_the_account_s_own(user: &mut User) -> SandboxResult {
    select_b_s_message(user)?;
    user.key(keys::RIGHT)?;
    user.key(keys::RIGHT)?;
    user.until("the heart under the keys", |state| {
        state.focus.as_deref() == Some(HEART)
    })?;
    user.key(keys::SPACE)?;
    user.until("Space sets a's heart", |state| state.pill(HEART) == Some((2, true)))?;
    user.key(keys::SPACE)?;
    user.until("Space takes it back", |state| {
        state.pill(HEART) == Some((1, false)) && state.focus.as_deref() == Some(HEART)
    })?;
    Ok(())
}

/// b takes back the heart under the keys: its pill and card go, Space sets nothing, and Escape clears the selection.
fn a_pill_gone_under_the_keys_takes_nothing(user: &mut User) -> SandboxResult {
    user.ask("b-takes-back-heart")?;
    user.until("the heart's pill goes, the keys leave it", |state| {
        state.pill(HEART).is_none() && state.focus.is_none() && state.shown.is_none()
    })?;
    user.key(keys::SPACE)?;
    user.stays("Space on a pill gone sets nothing", |state| state.pill(HEART).is_none())?;
    user.key(keys::ESC)?;
    user.until(
        "Escape isn't spent on the card gone: it clears the selection",
        |state| state.selected.is_none(),
    )?;
    Ok(())
}

// --- the child: the app on the mock, answering the parent ---

/// One account's mock token, for the transport and the push channel alike; it never runs out in a test.
struct Tokens {
    token: String,
    renewals: broadcast::Sender<Renewal>,
}

impl Credentials for Tokens {
    fn bearer(&self, _audience: &str) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }

    fn bearer_rejected(
        &self,
        _audience: &str,
        _rejected: &Secret,
    ) -> impl Future<Output = Result<Secret, TokenError>> + Send {
        future::ready(Ok(Secret::new(self.token.clone())))
    }
}

impl TrouterCredentials for Tokens {
    fn ic3(&self) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn ic3_after(&self, _old: &Secret) -> impl Future<Output = Result<AccessToken, TokenError>> + Send {
        future::ready(Ok(self.access()))
    }

    fn renewals(&self) -> broadcast::Receiver<Renewal> {
        self.renewals.subscribe()
    }
}

impl Tokens {
    fn access(&self) -> AccessToken {
        AccessToken {
            token: Secret::new(self.token.clone()),
            expires_at: std::time::SystemTime::now() + Duration::from_secs(7_200),
            scopes: Vec::new(),
        }
    }
}

/// The mock, with b's message and its reactions, and a's engine on it.
struct MockLab {
    _runtime: tokio::runtime::Runtime,
    mock: Arc<ChatServiceMock>,
    _trouter: TrouterMock,
    dir: tempfile::TempDir,
    /// b's message: its id and client message id.
    message: (String, String),
}

impl MockLab {
    fn start() -> SandboxResult<(Self, EngineHandle)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let mock = runtime.block_on(ChatServiceMock::start())?;
        let trouter = mock.start_trouter()?;
        let message = seed_reactions(&mock)?;
        let dir = tempfile::tempdir()?;
        let tokens = Arc::new(Tokens {
            token: mock.token(LAB_A).ok_or("the mock's token for a")?,
            renewals: broadcast::channel(4).0,
        });
        let origin = mock.origin();
        let http = || HttpTransport::with_mock_origin(Arc::clone(&tokens), CHAT_BASE, &origin);
        let transport = Arc::new(http()?);
        let trouter_client = Trouter::with_mock_origin(Arc::clone(&transport), Arc::clone(&tokens), trouter.origin())?
            .with_options(Options {
                backoff_base: Duration::from_millis(50),
                backoff_cap: Duration::from_millis(50),
                backoff_jitter_percent: 0,
                ..Options::default()
            });
        let author = Author::new(Mri::new(LAB_A), display_name("bot-a@lab.example.test"));
        let engine = engine::start(Parts {
            calls: None,
            store: Store::open(&support::store_file(dir.path(), Alias::A))?,
            // the reactors' names and pictures come from the profile services, as the senders' do
            client: Arc::new(Client::new(
                http()?
                    .with_ams(AMS_BASE)?
                    .with_presence(PRESENCE_BASE)?
                    .with_csa(CSA_BASE)?
                    .with_user_profile(USER_PROFILE_BASE)?
                    .with_provisioning(PROVISIONING_BASE)?
                    .with_middle_tier(MIDDLE_TIER_BASE)?,
            )),
            outbox: Outbox::new(transport, author),
            push: Some(Push::new(trouter_client)),
            options: EngineOptions::default(),
            // the mock serves neither Graph nor the profile lookup: c and the made-up people are named as the lab
            // build names its accounts (`lab.toml`), the engine's last source for a name
            known_names: known_names(),
        })?;
        let lab = Self {
            _runtime: runtime,
            mock: Arc::new(mock),
            _trouter: trouter,
            dir,
            message,
        };
        Ok((lab, engine))
    }
}

/// A reaction entry as the store gives it: `mri` reacted at `time` (ms).
fn given(mri: &str, time: i64) -> serde_json::Value {
    serde_json::json!({"mri": mri, "time": time, "value": time.to_string()})
}

/// The names the engine knows already: c's, and the made-up people's.
fn known_names() -> Vec<(String, String)> {
    let mut names = vec![(LAB_C.to_owned(), "Lab C".to_owned())];
    names.extend((0..MANY).map(|n| (laugher(n), format!("Made-up Person {n:02}"))));
    names
}

/// One of the made-up people who gave the laugh.
fn laugher(n: usize) -> String {
    format!("8:orgid:00000000-0000-4000-8000-0000000002{n:02}")
}

/// b's message in the a–b chat, the chat's oldest, with its reactions (see the module docs); its id and client
/// message id.
fn seed_reactions(mock: &ChatServiceMock) -> SandboxResult<(String, String)> {
    let id = mock
        .post_as(LAB_B, AB_CHAT, "<p>The made-up release notes are up for a look</p>")
        .ok_or("b's post refused")?;
    let client_message_id = (9_000_000_000_000_000_000_u64 + id.parse::<u64>()?).to_string();
    // before the mock's clock, so a reaction a sets now comes after them, as it would
    let base = 1_759_000_000_000_i64;
    let laughers: Vec<serde_json::Value> = (0..MANY)
        .map(|n| given(&laugher(n), base + 100 + i64::try_from(n).unwrap_or_default()))
        .collect();
    let changed = mock.change_message(AB_CHAT, &client_message_id, |message| {
        message["properties"]["emotions"] = serde_json::json!([
            // in store order, not by time: a reacted last
            {"key": LIKE, "users": [given(LAB_A, base + 30), given(LAB_C, base + 10), given(LAB_B, base + 20)]},
            {"key": HEART, "users": [given(LAB_B, base + 40)]},
            {"key": LAUGH, "users": laughers},
            {"key": OTHER, "users": [given(LAB_B, base + 50)]},
        ]);
    });
    if !changed {
        return Err("b's post isn't in the mock".into());
    }
    Ok((id, client_message_id))
}

/// b takes back its heart on its message, as a's push channel then tells it.
fn b_takes_back_heart(mock: &ChatServiceMock, client_message_id: &str) -> bool {
    let mut changed = None;
    let found = mock.change_message(AB_CHAT, client_message_id, |message| {
        if let Some(emotions) = message["properties"]["emotions"].as_array_mut() {
            for emotion in emotions.iter_mut().filter(|emotion| emotion["key"] == HEART) {
                emotion["users"] = serde_json::json!([]);
            }
        }
        message["version"] = serde_json::json!("1767268900000");
        changed = Some(message.clone());
    });
    let Some(resource) = changed.filter(|_| found) else {
        return false;
    };
    mock.queue_event(
        LAB_A,
        ChatEvent {
            conversation: AB_CHAT.to_owned(),
            from: LAB_B.to_owned(),
            kind: EventKind::Reaction,
            resource,
        },
    );
    true
}

fn child() -> SandboxResult {
    ngteams::read_only::window_allowed_here()?;
    let (lab, engine) = MockLab::start()?;
    let first = support::started(Alias::A, engine)?;
    let settings = teams_ui::SettingsFile::new(lab.dir.path().join("ui").join("a.toml"));
    let runner = support::runner(lab.dir.path())?;
    let failed = Arc::new(Mutex::new(None));
    let failed_in_app = Arc::clone(&failed);
    let mock = Arc::clone(&lab.mock);
    let message = lab.message.clone();
    let application = gpui_platform::linux(WindowingModes::WAYLAND).with_assets(teams_ui::AppAssets);
    application.run(move |cx: &mut App| {
        let launched = app::launch(
            cx,
            Launch {
                first,
                others: Vec::new(),
                trace: Arc::new(teams_ui::ViewTrace::off()),
                ctl: None,
                settings: teams_ui::SettingsModel::load(settings),
                bus: teams_ui::bus::address_from_env(),
                sign_in_again: std::rc::Rc::new(|_| std::rc::Rc::new(|_| {})),
                runner,
                read_only: false,
            },
        );
        match launched {
            Ok(launched) => {
                let window = launched.window;
                let _ = window.update(cx, |_, window, _| {
                    after_frames(window, 3, |_, _| say("ready"));
                });
                answer(window, mock, message, cx);
            }
            Err(failed) => {
                *failed_in_app.lock().unwrap_or_else(PoisonError::into_inner) = Some(failed.error);
                app::quit_after(failed.stopped, cx);
            }
        }
    });
    if let Some(error) = failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
        return Err(error.into());
    }
    drop(lab);
    Ok(())
}

/// Runs `done` once `frames` more frames of `window` have been presented.
fn after_frames(window: &Window, frames: usize, done: impl FnOnce(&mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if frames <= 1 {
            done(window, cx);
        } else {
            after_frames(window, frames - 1, done);
        }
    });
}

/// Answers the parent's questions as they come, until it asks to quit.
fn answer(window: WindowHandle<KitRoot>, mock: Arc<ChatServiceMock>, message: (String, String), cx: &mut App) {
    let Some(dir) = std::env::var_os(ui_sandbox::SANDBOX_ENV) else {
        say("no sandbox dir");
        return;
    };
    let ask = PathBuf::from(dir).join(ASK);
    cx.spawn(async move |cx: &mut AsyncApp| {
        loop {
            cx.background_executor().timer(Duration::from_millis(10)).await;
            let Ok(question) = fs::read_to_string(&ask) else {
                continue;
            };
            let _ = fs::remove_file(&ask);
            let (seq, verb) = question.split_once(' ').unwrap_or((question.as_str(), ""));
            let reply = match verb {
                "quit" => {
                    say(&format!("re {seq} ok"));
                    cx.update(|cx| cx.quit());
                    return;
                }
                "b-takes-back-heart" => b_takes_back_heart(&mock, &message.1).to_string(),
                _ => window
                    .update(cx, |kit, window, cx| {
                        let shell = kit
                            .view()
                            .clone()
                            .downcast::<Root>()
                            .ok()?
                            .read(cx)
                            .shell_in(cx)
                            .cloned()?;
                        Some(on_shell(&shell, verb, &message.0, window, cx))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "no shell".to_owned()),
            };
            say(&format!("re {seq} {reply}"));
        }
    })
    .detach();
}

fn on_shell(shell: &Entity<Shell>, verb: &str, message: &str, window: &mut Window, cx: &mut App) -> String {
    match verb {
        "open" => {
            shell.update(cx, |shell, cx| shell.open_chat(AB_CHAT.to_owned(), window, cx));
            "ok".to_owned()
        }
        "state" => shell
            .read(cx)
            .chat()
            .map_or_else(|| "no chat".to_owned(), |chat| state_of(chat.read(cx), message)),
        other => format!("unknown {other}"),
    }
}

/// The chat view's state as [`State::parse`] reads it.
fn state_of(chat: &ChatView, message: &str) -> String {
    let on_message = |pill: Option<(&str, &str)>| {
        pill.filter(|(id, _)| *id == message)
            .map_or("-", |(_, key)| key)
            .to_owned()
    };
    let item = chat.message_items().find(|item| item.id == message);
    let pills = item.map_or_else(String::new, |item| {
        item.reactions
            .iter()
            .filter(|reaction| reaction.count() > 0)
            .map(|reaction| {
                format!(
                    "{}:{}:{}",
                    reaction.key,
                    reaction.count(),
                    if reaction.mine { "mine" } else { "theirs" }
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    });
    let likers = item
        .and_then(|item| item.reactions.iter().find(|reaction| reaction.key == LIKE))
        .map_or_else(String::new, |like| like.users.join(","));
    let selected = match chat.selected() {
        None => "-",
        Some(id) if id == message => "b",
        Some(_) => "other",
    };
    let top = chat.list_state().logical_scroll_top();
    format!(
        "shown={} focus={} selected={selected} pills={pills} likers={likers} scroll={}:{}",
        on_message(chat.reactors_shown()),
        on_message(chat.reaction_focus()),
        top.item_ix,
        top.offset_in_item.as_f32(),
    )
}
