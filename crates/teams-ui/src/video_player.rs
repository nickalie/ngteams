//! A video a message carries, played in its card as Teams plays one (`crate::files` draws the card's header): a black
//! box of the card's width at 16:9 holding the video's picture, with a seek bar and the controls over its bottom.
//!
//! - **Before it plays**: the file's thumbnail alone (Graph's, as the card shows it, `crate::inline::thumbnail`), as
//!   Teams shows it; a video with none (an own server's) shows the large video icon on the picture's shade
//!   (`crate::files::no_picture`), never a black box. A click on the picture or Space starts it: a [`Player`] (`teams-media`) on a thread of its
//!   own, streaming the file from the engine ([`Core::file_stream`](crate::bridge::Core::file_stream), through
//!   the card's [`PictureSource`]: no view builds a request), played by `GStreamer`, each frame shown here through
//!   GPUI's external surface, its sound on the speaker of the devices the app gave ([`set_playback`]).
//! - **The controls**, as Teams' player has them: play/pause, back 10 s, forward 10 s, mute (its volume slider beside
//!   it while the pointer is on it), the time (`0:10 / 1:52`), "More options" (open in the browser, download, copy
//!   link), full screen (the video over the whole window, the window full screen; Esc or the button leaves it) and
//!   "Open in browser" (Stream's player, where Teams pops the video out). The seek bar seeks
//!   where it is clicked or dragged to, its tooltip the time. In a card the controls show only while the pointer is on
//!   the video (or its seek bar is dragged, or a menu of it is open), whether it plays or not, as in Teams; a recap's
//!   player shows them while it doesn't play too. Space plays or pauses, Left and Right go 10 s back or forward, M
//!   mutes. In a card, Escape pauses it and gives the keys back to its card ([`VideoPlayer::return_to`]), which a
//!   card's Enter or Space moved into it ([`VideoPlayer::play_from_keys`]); the controls show while the keyboard has the
//!   keys in it, until the pointer presses in it. Tab goes through each control in the order of the bar, ringed as the
//!   kit's buttons are; Enter or Space presses the control that has the keys, and the keys stay on it (in full screen
//!   too), where Tab stays in the video. A press of the pointer on a control leaves the keys on the control that has
//!   them; full screen and a recap's menus bring them into the player from another place, for its Escape.
//! - **Its states**: loading and buffering show a spinner over the picture; a video that can't be played here (a
//!   format no installed `GStreamer` plugin plays, a file the account can't read, the network) says so and offers the
//!   browser.
//! - The player lives as long as its card is drawn: a card scrolled away stops it. One plays at a time: starting one
//!   pauses the others, as in Teams.
//! - **In a meeting's recap** ([`crate::recap_view`]) the same player fills its column at 16:9 ([`VideoPlayer::filling`])
//!   and is told where to play from ([`VideoPlayer::play_from`]: a transcript's paragraph clicked), the recap reading its
//!   position to follow the transcript ([`VideoPlayer::position`]). It streams the recording as Teams' Recap player
//!   does ([`VideoPlayer::streaming`]): its DASH presentation and parts from the engine
//!   ([`Core::recording_presentation`](crate::bridge::Core::recording_presentation)), played by the same `GStreamer`
//!   player ([`Player::open_stream`]). That plays a recording whose download is blocked (an attendee's view of
//!   another's meeting), which the file's download link refuses; a recording that can't be streamed (not processed
//!   yet) is played from its download link, as any shared video, before the player says it can't be opened.
//!   Its bar is Teams' Recap player's: in place of "More options", captions (CC: the transcript's words over the
//!   picture, [`VideoPlayer::set_captions`]), the playback speed ("1x": 0.8x to 2x, its sound's pitch kept) and the
//!   settings (caption settings, quality, keyboard shortcuts), each a menu over the picture (`menus`). C turns the
//!   captions on or off, Z and X slow it down or speed it up.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::video_surface::{self, Frame as PixelFrame, Surface as PixelSurface};
use gpui::prelude::FluentBuilder as _;

use gpui::{
    AnyElement, App, Bounds, ClipboardItem, Context, Div, Entity, FocusHandle, FontWeight, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, ParentElement as _, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Task, WeakEntity, Window, anchored, canvas, deferred, div, hsla, img, point, px, relative,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use gpui_component::{FocusTrapElement as _, ThemeStyled as _};
use teams_core::{EngineError, FileStream};
use teams_media::devices::{PixelFormat, VideoFrame};
use teams_media::player::{
    Abandon, ByteSource, ByteStream, Command, Failure, Part, Player, PlayerOutput, PlayerSetup, RATES, SegmentSource,
    SourceError, Span, State, Status, Track, TrackKind,
};
use teams_proto::chat::{FileKind, SharedFile};

use crate::files::{self, COPY_LINK, DOWNLOAD, OPEN_IN_BROWSER, PICTURE_RATIO};
use crate::icons::{Glyph, selected_by};
use crate::inline::{PictureSource, PictureState, thumbnail};
use crate::keyboard::Handles;
use crate::theme;
use crate::video::Picture;

mod captions;
mod menus;

pub use captions::{Caption, CaptionColor, CaptionSize, CaptionStyle, Captions, language_name};
pub use menus::{KEYS, Menu, Page, rate_label};

/// The player's key context: its Escape is taken before the list's or the recap's around it.
pub const CONTEXT: &str = "VideoPlayer";

gpui::actions!(teams_ui, [LeavePlayer, SkipBack, SkipForward]);

/// Marks the player's key bindings as installed.
struct Bound;

impl Global for Bound {}

/// Binds the player's Escape ([`LeavePlayer`]), Left and Right ([`SkipBack`], [`SkipForward`]), once per app: as
/// actions, so that the keys of the list around it do not take them first.
pub fn bind(cx: &mut App) {
    if cx.has_global::<Bound>() {
        return;
    }
    cx.set_global(Bound);
    cx.bind_keys([
        gpui::KeyBinding::new("escape", LeavePlayer, Some(CONTEXT)),
        gpui::KeyBinding::new("left", SkipBack, Some(CONTEXT)),
        gpui::KeyBinding::new("right", SkipForward, Some(CONTEXT)),
    ]);
}

/// The skip buttons' names.
const BACK: &str = "Back 10 seconds";
const FORWARD: &str = "Forward 10 seconds";

/// How far back and forward the skip buttons go.
pub const SKIP: Duration = Duration::from_secs(10);

/// The video's controls' colour, on the black of the player in every theme, as Teams draws it.
const ON_VIDEO: u32 = 0x00ff_ffff;

/// What every video player is made with: the devices whose speaker plays the sound. The app sets it once
/// ([`set_playback`]); without it a video plays silently.
#[derive(Clone, Debug, Default)]
pub struct Playback(pub PlayerSetup);

impl Global for Playback {}

/// Sets what every video player is made with (see [`Playback`]).
pub fn set_playback(setup: PlayerSetup, cx: &mut App) {
    cx.set_global(Playback(setup));
}

/// Whether `file` is played in its card: a video with a sharing link (one whose format can't be played says so there).
#[must_use]
pub fn plays_inline(file: &SharedFile) -> bool {
    file.kind() == FileKind::Video && file.share_url.is_some()
}

/// A file's bytes as the engine streams them, for the player's threads.
struct EngineBytes {
    source: PictureSource,
    share_url: String,
}

impl ByteSource for EngineBytes {
    fn open(&self, offset: u64, len: u64, abandon: &Abandon) -> Result<Box<dyn ByteStream>, SourceError> {
        let asked = self.source.engine().file_stream(self.share_url.clone(), offset, len);
        // let go before it was answered, the reply dropped stops its read
        match abandon.wait_for(asked) {
            None => Err(SourceError::Stopped),
            Some(Ok(stream)) => Ok(Box::new(EngineStream(stream))),
            Some(Err(error)) => Err(source_error(&error)),
        }
    }
}

/// A stream the engine answered; dropped, its read stops.
struct EngineStream(FileStream);

impl ByteStream for EngineStream {
    fn total(&self) -> u64 {
        self.0.total
    }

    fn next(&mut self, abandon: &Abandon) -> Result<Option<Vec<u8>>, SourceError> {
        match abandon.wait_for(self.0.next()) {
            None => Err(SourceError::Stopped),
            Some(None) => Ok(None),
            Some(Some(Ok(bytes))) => Ok(Some(bytes)),
            Some(Some(Err(error))) => Err(source_error(&error)),
        }
    }
}

/// A meeting recording, where it is: the drive and the item that hold it (a recap part's), streamed by them; both empty
/// when the recap names neither (the engine finds them by the recording's sharing link).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recording {
    pub drive: String,
    pub item: String,
}

/// A meeting recording's presentation and parts as the engine streams them, for the player's threads.
struct EngineParts {
    source: PictureSource,
    /// The recording's sharing link: its personal site.
    link: String,
    recording: Recording,
    /// The presentation as the engine read it: its parts' addresses.
    read: Mutex<Option<Arc<teams_proto::Presentation>>>,
}

impl SegmentSource for EngineParts {
    fn presentation(&self, abandon: &Abandon) -> Result<teams_media::player::Presentation, SourceError> {
        let presentation = self.ask(abandon)?;
        let played = played(&presentation);
        *self.read.lock().unwrap_or_else(PoisonError::into_inner) = Some(presentation);
        Ok(played)
    }

    /// A part refused (its address's signed `docId` expired: a recording played or left paused long enough) is asked
    /// for once more at its address in the presentation read again, unless another part's read did that meanwhile.
    fn part(&self, track: usize, part: Part, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
        let (read, link) = self.link(track, part)?;
        match self.segment(link, abandon) {
            Err(SourceError::Unavailable) => {
                let current = self.read.lock().unwrap_or_else(PoisonError::into_inner).clone();
                if current.is_some_and(|current| Arc::ptr_eq(&current, &read)) {
                    self.read_again(&read, abandon)?;
                }
                let (_, link) = self.link(track, part)?;
                self.segment(link, abandon)
            }
            answer => answer,
        }
    }
}

impl EngineParts {
    /// The presentation, as the engine reads it now.
    fn ask(&self, abandon: &Abandon) -> Result<Arc<teams_proto::Presentation>, SourceError> {
        let asked = self.source.engine().recording_presentation(
            self.link.clone(),
            self.recording.drive.clone(),
            self.recording.item.clone(),
        );
        match abandon.wait_for(asked) {
            None => Err(SourceError::Stopped),
            Some(Ok(presentation)) => Ok(presentation),
            Some(Err(error)) => Err(source_error(&error)),
        }
    }

    /// The presentation as last read, and the address of `part` of its track `track` in it.
    fn link(
        &self,
        track: usize,
        part: Part,
    ) -> Result<(Arc<teams_proto::Presentation>, teams_proto::MediaSegmentLink), SourceError> {
        let read = self
            .read
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(SourceError::Unavailable)?;
        let link = match (read.tracks.get(track), part) {
            (Some(track), Part::Init) => Some(track.init().clone()),
            (Some(track), Part::Media(index)) => track.segment(index),
            (None, _) => None,
        };
        let link = link.ok_or(SourceError::Unavailable)?;
        Ok((read, link))
    }

    /// The bytes at `link`.
    fn segment(&self, link: teams_proto::MediaSegmentLink, abandon: &Abandon) -> Result<Vec<u8>, SourceError> {
        match abandon.wait_for(self.source.engine().recording_segment(link)) {
            None => Err(SourceError::Stopped),
            Some(Ok(bytes)) => Ok(bytes),
            Some(Err(error)) => Err(source_error(&error)),
        }
    }

    /// The presentation read again, its parts' addresses fresh; [`SourceError::Unavailable`] when it no longer plays
    /// as `was` did (its tracks or their times changed: the player's places would be another presentation's).
    fn read_again(&self, was: &teams_proto::Presentation, abandon: &Abandon) -> Result<(), SourceError> {
        let presentation = self.ask(abandon)?;
        if played(&presentation) != played(was) {
            return Err(SourceError::Unavailable);
        }
        *self.read.lock().unwrap_or_else(PoisonError::into_inner) = Some(presentation);
        Ok(())
    }
}

/// The presentation as the player plays it: its length and its tracks' times.
fn played(presentation: &teams_proto::Presentation) -> teams_media::player::Presentation {
    teams_media::player::Presentation {
        duration: Duration::from_millis(presentation.duration_ms),
        tracks: presentation
            .tracks
            .iter()
            .map(|track| Track {
                kind: match track.kind {
                    teams_proto::TrackKind::Audio => TrackKind::Audio,
                    teams_proto::TrackKind::Video => TrackKind::Video,
                },
                timescale: track.timescale,
                segments: track
                    .segments
                    .iter()
                    .map(|segment| Span {
                        start: segment.start,
                        duration: segment.duration,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn source_error(error: &EngineError) -> SourceError {
    match error {
        EngineError::FileUnavailable => SourceError::Unavailable,
        EngineError::Stopped => SourceError::Stopped,
        _ => SourceError::Failed,
    }
}

/// What the player's thread hands the view: its newest frame and status, and a wake.
struct Screen {
    picture: Mutex<Option<Arc<Picture>>>,
    status: Mutex<Option<Status>>,
    serial: AtomicU64,
    wake: async_channel::Sender<()>,
}

impl PlayerOutput for Screen {
    /// Takes a frame as the player hands it over: BGRA, its rows packed (anything else isn't drawn).
    fn frame(&self, frame: VideoFrame) {
        let (width, height) = (frame.width, frame.height);
        if frame.format != PixelFormat::Bgra || frame.stride != width * 4 {
            return;
        }
        let bgra = frame.data;
        let serial = self.serial.fetch_add(1, Ordering::Relaxed) + 1;
        *self.picture.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(Picture {
            width,
            height,
            bgra: Arc::new(bgra),
            serial,
            at: Instant::now(),
        }));
        // full means a wake is already waiting
        let _ = self.wake.try_send(());
    }

    fn status(&self, status: Status) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
        let _ = self.wake.try_send(());
    }
}

/// Whether a player's video fills the window, and what it did to the window for it.
struct FullScreen {
    on: bool,
    /// The window was made full screen by this player (and is put back when it leaves).
    made: bool,
    /// The window is put back from full screen when this player goes (registered at its first full screen).
    hooked: bool,
    /// The video over the whole window, which keeps Tab in itself.
    trap: FocusHandle,
}

/// The keys of a card's player.
#[derive(Default)]
struct Keys {
    /// The card that holds it: where Escape gives the keys back.
    card: Option<FocusHandle>,
    /// The keyboard brought the keys into it: its controls stay until the keys leave or the pointer presses in it.
    from_keyboard: bool,
}

/// What a player was last asked: to play (or to play once loaded), or to pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    Play,
    Pause,
}

/// How loud it plays, and its slider.
struct Volume {
    /// From 0 (silent) to 1 (as recorded).
    level: f32,
    /// The slider is dragged.
    adjusting: bool,
    /// Where the slider was drawn, for a click's place on it.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Default for Volume {
    fn default() -> Self {
        Self {
            level: 1.0,
            adjusting: false,
            bar: Rc::new(Cell::new(None)),
        }
    }
}

/// One card's player (see the module docs).
pub struct VideoPlayer {
    /// The card's id: the message's and the file's place.
    id: String,
    file: SharedFile,
    source: PictureSource,
    player: Option<Player>,
    screen: Arc<Screen>,
    status: Option<Status>,
    surface: Arc<PixelSurface>,
    drawn: Option<(u64, Arc<PixelFrame>)>,
    muted: bool,
    volume: Volume,
    full_screen: FullScreen,
    /// Where the seek bar was drawn, for a click's place on it.
    bar: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The seek bar's place while it is dragged, as a fraction.
    dragging: Option<f32>,
    /// A recap's player: its controls show while it doesn't play, and its bar has the recap's buttons.
    fill: bool,
    /// Where to go once the file opened: asked before the player had a place to seek in.
    pending_seek: Option<Duration>,
    /// The meeting recording it streams (a recap's player), rather than its file's bytes.
    recording: Option<Recording>,
    /// The recording couldn't be streamed: it plays from its file's download link instead.
    streamed_failed: bool,
    /// What it was last asked: what a player opened again in its place does.
    asked: Asked,
    /// How fast it plays (one of `teams_media::player::RATES`).
    rate: f64,
    /// Its transcript's words, shown over the picture when captions are on (a recap's player).
    captions: Option<Captions>,
    /// The menu open over the picture.
    menu: Option<Menu>,
    focus: FocusHandle,
    keys: Keys,
    /// Each control's focus handle, by its name: the keyboard reaches every one of them.
    controls: Handles,
    _woken: Task<()>,
}

impl VideoPlayer {
    /// The player of `file`, the card `id`'s, reading through `source`'s engine; nothing plays until asked, but the
    /// engine reads the file's download link now (its size asked for), so Play doesn't wait for Graph.
    pub fn new(id: String, file: SharedFile, source: PictureSource, cx: &mut Context<Self>) -> Self {
        if let Some(share_url) = &file.share_url {
            // the engine keeps the link whether or not the answer is heard
            drop(source.engine().file_size(share_url.clone()));
        }
        let me = cx.weak_entity();
        let shown = &mut cx.default_global::<Players>().0;
        shown.retain(|player| player.upgrade().is_some());
        shown.push(me);
        let (wake, woken) = async_channel::bounded(1);
        let task = cx.spawn(async move |this, cx| {
            while woken.recv().await.is_ok() {
                let alive = this.update(cx, |this, cx| {
                    this.status = *this.screen.status.lock().unwrap_or_else(PoisonError::into_inner);
                    this.fall_back(cx);
                    this.seek_when_open(cx);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        Self {
            id,
            file,
            source,
            player: None,
            screen: Arc::new(Screen {
                picture: Mutex::new(None),
                status: Mutex::new(None),
                serial: AtomicU64::new(0),
                wake,
            }),
            status: None,
            surface: PixelSurface::new(),
            drawn: None,
            muted: false,
            volume: Volume::default(),
            full_screen: FullScreen {
                on: false,
                made: false,
                hooked: false,
                trap: cx.focus_handle(),
            },
            bar: Rc::new(Cell::new(None)),
            dragging: None,
            fill: false,
            pending_seek: None,
            recording: None,
            streamed_failed: false,
            asked: Asked::Pause,
            rate: 1.0,
            captions: None,
            menu: None,
            focus: cx.focus_handle(),
            keys: Keys::default(),
            controls: Handles::default(),
            _woken: task,
        }
    }

    /// Makes Escape in it give the keys back to its card's `card`.
    pub fn return_to(&mut self, card: &FocusHandle) {
        if self.keys.card.as_ref() != Some(card) {
            self.keys.card = Some(card.clone());
        }
    }

    /// Enter or Space on its card: the keys go into it and it plays (it goes on playing if it does).
    pub fn play_from_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.keys.from_keyboard = true;
        if !self.playing() {
            self.toggle(cx);
        }
        cx.notify();
    }

    /// Whether the keyboard has the keys in it, on its picture or on one of its controls: not when the pointer gave
    /// them.
    fn has_keys(&mut self, window: &Window, cx: &App) -> bool {
        self.keys.from_keyboard =
            self.focus.contains_focused(window, cx) && (self.keys.from_keyboard || window.last_input_was_keyboard());
        self.keys.from_keyboard
    }

    /// Escape: an open menu closes, then full screen ends, then a card's player pauses and gives the keys back to its
    /// card; else it goes on to the view around it.
    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.is_some() {
            self.show_menu(None, cx);
        } else if self.full_screen.on {
            self.toggle_fullscreen(window, cx);
        } else if let Some(card) = self.keys.card.clone() {
            self.pause(cx);
            window.focus(&card, cx);
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    /// The player of a recap: its controls show while it doesn't play, and its bar has the recap's buttons.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill = true;
        self
    }

    /// The player of a meeting recording: it streams `recording` as Teams' Recap player does (see the module docs).
    #[must_use]
    pub fn streaming(mut self, recording: Recording) -> Self {
        self.recording = Some(recording);
        self
    }

    /// The meeting recording it streams, when it streams one.
    #[must_use]
    pub fn recording(&self) -> Option<&Recording> {
        self.recording.as_ref()
    }

    /// Its captions (a recap's player: its recording's transcript), or none.
    pub fn set_captions(&mut self, captions: Option<Captions>, cx: &mut Context<Self>) {
        if self.captions != captions {
            self.captions = captions;
            cx.notify();
        }
    }

    /// Its captions, when it has them.
    #[must_use]
    pub fn captions(&self) -> Option<&Captions> {
        self.captions.as_ref()
    }

    /// Its card's id (a recap part's player: `recap-<part>`).
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The caption shown now: captions on and a cue spoken where it plays.
    #[must_use]
    pub fn caption(&self, cx: &App) -> Option<&str> {
        let at = self.position()?;
        self.captions.as_ref().filter(|_| CaptionStyle::current(cx).on)?.at(at)
    }

    /// Turns captions on or off, for every player (see `captions`).
    pub fn show_captions(&mut self, shown: bool, cx: &mut Context<Self>) {
        CaptionStyle::change(cx, |style| style.on = shown);
        cx.notify();
    }

    /// How fast it plays.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Plays at `rate` (one of `RATES`), its picture and sound together; kept when it is opened later.
    pub fn set_rate(&mut self, rate: f64, cx: &mut Context<Self>) {
        self.rate = rate;
        if let Some(player) = &self.player {
            player.send(Command::Rate(rate));
        }
        cx.notify();
    }

    /// One speed `faster` or slower among `RATES` (none past the fastest or the slowest).
    fn step_rate(&mut self, faster: bool, cx: &mut Context<Self>) {
        let at = RATES
            .iter()
            .position(|rate| (rate - self.rate).abs() < f64::EPSILON)
            .unwrap_or(1);
        let next = if faster { at + 1 } else { at.wrapping_sub(1) };
        if let Some(&rate) = RATES.get(next) {
            self.set_rate(rate, cx);
        }
    }

    /// The menu open over the picture.
    #[must_use]
    pub fn menu(&self) -> Option<Menu> {
        self.menu
    }

    /// Opens `menu` over the picture (closing any other), or closes it.
    pub fn show_menu(&mut self, menu: Option<Menu>, cx: &mut Context<Self>) {
        self.menu = menu;
        cx.notify();
    }

    /// Opens `menu`, or closes it when it is open (its button clicked again).
    fn toggle_menu(&mut self, menu: Menu, window: &mut Window, cx: &mut Context<Self>) {
        let open = match (self.menu, menu) {
            (Some(Menu::Settings(_)), Menu::Settings(_)) => true,
            (Some(open), menu) => open == menu,
            (None, _) => false,
        };
        self.menu = (!open).then_some(menu);
        self.take_keys(window, cx);
        cx.notify();
    }

    /// The height of the picture played (the recording's quality, "1080p"), once a frame came.
    fn quality(&self) -> Option<u32> {
        self.screen
            .picture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|picture| picture.height)
    }

    /// Where it plays, as last told; `None` before it was started.
    #[must_use]
    pub fn position(&self) -> Option<Duration> {
        self.status.map(|status| status.position)
    }

    /// Plays from `at`: the file opened first when it wasn't (the place kept until it can be gone to), then played.
    pub fn play_from(&mut self, at: Duration, cx: &mut Context<Self>) {
        if self.player.is_none() {
            self.pending_seek = Some(at);
            self.toggle(cx);
            return;
        }
        self.seek(Command::Seek(at), cx);
        if !self.playing() {
            self.toggle(cx);
        }
    }

    /// Goes to the place asked for before the file was open, once it is.
    fn seek_when_open(&mut self, cx: &mut Context<Self>) {
        if matches!(self.state(), Some(State::Playing | State::Paused | State::Buffering))
            && let Some(at) = self.pending_seek.take()
        {
            self.seek(Command::Seek(at), cx);
        }
    }

    /// The player's state as last told, `None` before it was started.
    #[must_use]
    pub fn state(&self) -> Option<State> {
        self.status.map(|status| status.state)
    }

    /// Whether it plays, or will once it has loaded: what Pause stops and what another player's Play pauses.
    fn playing(&self) -> bool {
        self.player.is_some()
            && matches!(
                self.state(),
                None | Some(State::Loading | State::Playing | State::Buffering)
            )
    }

    /// Pauses it while it plays or is loading to play (a recap's player its tab or part put away); nothing otherwise.
    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if self.playing() && self.player.is_some() {
            self.ask(Asked::Pause);
            cx.notify();
        }
    }

    /// Plays, or pauses while it plays; the first play opens the file.
    pub fn toggle(&mut self, cx: &mut Context<Self>) {
        if !self.playing() {
            pause_others(cx);
        }
        if self.player.is_none() {
            self.open(Asked::Play, cx);
        } else if self.playing() {
            self.ask(Asked::Pause);
        } else {
            self.ask(Asked::Play);
        }
        cx.notify();
    }

    /// Tells its player to play or to pause, and keeps what it was asked.
    fn ask(&mut self, asked: Asked) {
        if let Some(player) = &self.player {
            player.send(match asked {
                Asked::Play => Command::Play,
                Asked::Pause => Command::Pause,
            });
        }
        self.asked = asked;
    }

    /// Opens it, played once loaded or paused on its first frame as `asked`: a recording streamed, else (or once its
    /// stream couldn't be had) the file's bytes.
    fn open(&mut self, asked: Asked, cx: &mut Context<Self>) {
        let play = asked == Asked::Play;
        let Some(share_url) = self.file.share_url.clone() else {
            return;
        };
        let setup = cx
            .try_global::<Playback>()
            .map(|playback| playback.0.clone())
            .unwrap_or_default();
        let output: Arc<dyn PlayerOutput> = self.screen.clone();
        // a player whose thread couldn't be made told its failure to the screen already
        self.player = if let Some(recording) = self.recording.clone().filter(|_| !self.streamed_failed) {
            let parts = Arc::new(EngineParts {
                source: self.source.clone(),
                link: share_url,
                recording,
                read: Mutex::new(None),
            });
            Player::open_stream(parts, setup, output, play)
        } else {
            let bytes = Arc::new(EngineBytes {
                source: self.source.clone(),
                share_url,
            });
            Player::open(bytes, setup, output, play)
        };
        self.asked = asked;
        if let Some(player) = &self.player {
            if self.muted {
                player.send(Command::Muted(true));
            }
            if self.volume.level < 1.0 {
                player.send(Command::Volume(f64::from(self.volume.level)));
            }
            if (self.rate - 1.0).abs() >= f64::EPSILON {
                player.send(Command::Rate(self.rate));
            }
        }
    }

    /// A recording whose stream can't be had (not processed yet, or refused) is played from its file's download link
    /// instead, once, from where the stream had got to, played or paused as it was last asked; only when that fails too
    /// does the player say it can't be opened.
    fn fall_back(&mut self, cx: &mut Context<Self>) {
        if self.recording.is_none() || self.streamed_failed || self.state() != Some(State::Failed(Failure::Unavailable))
        {
            return;
        }
        self.streamed_failed = true;
        if self.pending_seek.is_none() {
            self.pending_seek = self.position().filter(|at| !at.is_zero());
        }
        self.player = None;
        self.status = None;
        *self.screen.status.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.open(self.asked, cx);
    }

    /// Goes `back` or forward by [`SKIP`].
    fn skip(&mut self, back: bool, cx: &mut Context<Self>) {
        let Some(status) = self.status else { return };
        let to = if back {
            status.position.saturating_sub(SKIP)
        } else {
            let ahead = status.position + SKIP;
            // a length not known yet holds nothing back
            if status.duration.is_zero() {
                ahead
            } else {
                ahead.min(status.duration)
            }
        };
        self.seek(Command::Seek(to), cx);
    }

    /// Seeks (`seek` is [`Command::Seek`] or [`Command::SeekNear`]), the bar showing its time at once.
    fn seek(&mut self, seek: Command, cx: &mut Context<Self>) {
        let (Command::Seek(to) | Command::SeekNear(to)) = seek else {
            return;
        };
        if let Some(player) = &self.player {
            player.send(seek);
            if let Some(status) = self.status.as_mut() {
                status.position = to;
            }
            cx.notify();
        }
    }

    /// Sets how loud it plays from the place `position` is on the volume slider; a sound turned up is unmuted.
    fn set_volume_at(&mut self, position: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(bar) = self.volume.bar.get() else {
            return;
        };
        let width = f32::from(bar.size.width).max(1.0);
        let level = (f32::from(position.x - bar.origin.x) / width).clamp(0.0, 1.0);
        self.volume.level = level;
        if let Some(player) = &self.player {
            player.send(Command::Volume(f64::from(level)));
        }
        if self.muted && level > 0.0 {
            self.toggle_mute(cx);
        }
        cx.notify();
    }

    fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        self.muted = !self.muted;
        if let Some(player) = &self.player {
            player.send(Command::Muted(self.muted));
        }
        cx.notify();
    }

    fn toggle_fullscreen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let full = &mut self.full_screen;
        full.on = !full.on;
        if full.on && !window.is_fullscreen() {
            window.toggle_fullscreen();
            full.made = true;
            self.hook_release(window, cx);
        } else if !full.on && full.made {
            if window.is_fullscreen() {
                window.toggle_fullscreen();
            }
            full.made = false;
        }
        self.take_keys(window, cx);
        cx.notify();
    }

    /// Brings the keys into it, for its Escape, unless one of its controls has them: full screen draws each of them
    /// again. Another place in it may not be drawn again where it is now (the kit's "More options" as the video goes
    /// full screen), so the keys move to its picture from there.
    fn take_keys(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controls.focused(window).is_none() {
            window.focus(&self.focus, cx);
        }
    }

    /// Whether the keys are in it: on its picture or on one of its controls.
    #[must_use]
    pub fn holds_keys(&self, window: &Window, cx: &App) -> bool {
        self.focus.contains_focused(window, cx)
    }

    /// The focus handle of its control `what` (a [`control`]'s name).
    #[must_use]
    pub fn control_focus(&self, what: &str, cx: &App) -> FocusHandle {
        self.controls.get(what, cx)
    }

    /// `control` as the keyboard reaches it: a Tab stop named `tip` for a screen reader, ringed as the kit's buttons
    /// are while it has the keys (its test selector `video-ring-<card> <what>: <tip>` then). Enter or Space presses it
    /// (GPUI's keyboard click).
    fn reached(
        &self,
        what: &str,
        tip: &'static str,
        control: gpui::Stateful<Div>,
        window: &Window,
        cx: &App,
    ) -> AnyElement {
        let focus = self.controls.get(what, cx).tab_stop(true);
        let ringed = focus.is_focused(window);
        control
            .track_focus(&focus)
            .role(gpui::accesskit::Role::Button)
            .aria_label(tip)
            .when(ringed, |control| {
                let selector = format!("video-ring-{} {what}: {tip}", self.id);
                control
                    .focus_ring_style(window, cx)
                    .child(div().debug_selector(move || selector.clone()))
            })
            .into_any_element()
    }

    /// A card that goes while its video fills the screen (the chat switched, the list moved) puts the window back as it
    /// found it: registered once, at its first full screen.
    fn hook_release(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.full_screen.hooked {
            return;
        }
        self.full_screen.hooked = true;
        cx.on_release_in(window, |this, window, _| {
            if this.full_screen.made && window.is_fullscreen() {
                window.toggle_fullscreen();
            }
        })
        .detach();
    }

    /// The seek bar's fraction under `position`, when the bar was drawn.
    fn fraction_at(&self, position: gpui::Point<Pixels>) -> Option<f32> {
        let bar = self.bar.get()?;
        let width = f32::from(bar.size.width).max(1.0);
        Some((f32::from(position.x - bar.origin.x) / width).clamp(0.0, 1.0))
    }

    fn seek_to_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
        let Some(status) = self.status else { return };
        self.seek(Command::SeekNear(status.duration.mul_f32(fraction)), cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            // Space on one of its controls presses that control
            "space" if self.focus.is_focused(window) => self.toggle(cx),
            "m" => self.toggle_mute(cx),
            "c" if self.captions.is_some() => {
                let on = CaptionStyle::current(cx).on;
                self.show_captions(!on, cx);
            }
            "z" if self.fill => self.step_rate(false, cx),
            "x" if self.fill => self.step_rate(true, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The frame to draw: the one made for the newest picture already, or a new one.
    fn frame(&mut self) -> Option<(Arc<PixelFrame>, (u32, u32))> {
        let picture = self
            .screen
            .picture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let shape = (picture.width, picture.height);
        if let Some((serial, frame)) = &self.drawn
            && *serial == picture.serial
        {
            return Some((Arc::clone(frame), shape));
        }
        let frame = video_surface::frame(&self.surface, picture.serial, shape, &picture.bgra, |_| {});
        self.drawn = Some((picture.serial, Arc::clone(&frame)));
        Some((frame, shape))
    }
}

impl Render for VideoPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let id = self.id.clone();
        let inline = if self.full_screen.on {
            // the video is drawn over the whole window; the card keeps its place, black
            div().size_full().bg(gpui::black()).into_any_element()
        } else {
            self.screen_box(window, cx)
        };
        let overlay = self.full_screen.on.then(|| {
            let viewport = window.viewport_size();
            let screen = self.screen_box(window, cx);
            deferred(
                anchored()
                    .position_mode(gpui::AnchoredPositionMode::Window)
                    .position(point(px(0.0), px(0.0)))
                    // it takes every click, wheel and hover over the window: none reaches the page under it
                    .child(
                        div()
                            .w(viewport.width)
                            .h(viewport.height)
                            .occlude()
                            .child(screen)
                            // Tab stays on the video: the page under it is hidden
                            .focus_trap(
                                SharedString::from(format!("video-full-screen-{id}")),
                                &self.full_screen.trap,
                            ),
                    ),
            )
            .with_priority(100)
        });
        // 16:9 of its box's width, in a card as narrow as it gets too
        div()
            .debug_selector(move || format!("video-player-{id}"))
            .w_full()
            .aspect_ratio(PICTURE_RATIO)
            .child(inline)
            .children(overlay)
    }
}

/// The parts of the player's box.
impl VideoPlayer {
    /// The black box with the picture, what is over it and the controls.
    fn screen_box(&mut self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let id = self.id.clone();
        let group = SharedString::from(format!("video-screen-{id}"));
        let picture = self.picture(cx);
        let middle = self.middle();
        let caption = self
            .caption(cx)
            .map(|text| captions::caption(&id, text, CaptionStyle::current(cx)));
        let keys = self.has_keys(window, cx);
        let controls = self.controls(&group, keys, window, cx);
        let shift = if self.file.open_link().is_some() { 0.0 } else { 32.0 };
        let menu = self.menu_panel(shift, cx);
        div()
            .id(SharedString::from(format!("video-screen-{id}")))
            .debug_selector(move || format!("video-screen-{id}"))
            .group(group)
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &LeavePlayer, window, cx| this.escape(window, cx)))
            .on_action(cx.listener(|this, _: &SkipBack, _, cx| this.skip(true, cx)))
            .on_action(cx.listener(|this, _: &SkipForward, _, cx| this.skip(false, cx)))
            .capture_any_mouse_down(cx.listener(|this, _, _, _| this.keys.from_keyboard = false))
            .on_key_down(cx.listener(Self::key))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(gpui::black())
            .child(
                div()
                    .id("video-picture")
                    .absolute()
                    .inset_0()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        // a click outside an open menu closes it, as in Teams
                        if this.menu.is_some() {
                            this.show_menu(None, cx);
                        } else {
                            this.toggle(cx);
                        }
                    }))
                    .child(picture),
            )
            .children(middle)
            .children(caption)
            .child(controls)
            .children(menu)
            .into_any_element()
    }

    /// The video's frame fitted into the box, else the file's thumbnail, else in a card the picture's shade (with the
    /// large video icon when there is no thumbnail to wait for and no error to show in its place), else black.
    fn picture(&mut self, cx: &mut App) -> AnyElement {
        if let Some((frame, (width, height))) = self.frame() {
            return canvas(
                |_, _, _| (),
                move |bounds, (), window, cx| {
                    video_surface::paint(&frame, fitted(bounds, width, height), window, cx);
                },
            )
            .size_full()
            .into_any_element();
        }
        let poster = self
            .file
            .share_url
            .as_deref()
            .map_or(PictureState::Missing, |share_url| {
                thumbnail(share_url, &self.source, cx)
            });
        match poster {
            PictureState::Shown(image, _) => {
                let id = self.id.clone();
                div()
                    .debug_selector(move || format!("video-poster-{id}"))
                    .size_full()
                    .child(img(image).size_full().object_fit(ObjectFit::Contain))
                    .into_any_element()
            }
            // a recording's player stays black, as Teams' Recap player is
            _ if self.recording.is_some() => div().size_full().into_any_element(),
            PictureState::Reading => div()
                .size_full()
                .bg(gpui::rgb(files::picture_colour(theme::current(cx))))
                .into_any_element(),
            PictureState::Missing => {
                let id = self.id.clone();
                let failed = matches!(self.state(), Some(State::Failed(_)));
                div()
                    .debug_selector(move || format!("video-no-thumbnail-{id}"))
                    .size_full()
                    .bg(gpui::rgb(files::picture_colour(theme::current(cx))))
                    .when(!failed, |shade| {
                        shade.child(files::no_picture(FileKind::Video, &format!("video-icon-{}", self.id)))
                    })
                    .into_any_element()
            }
        }
    }

    /// What shows in the middle: a spinner while it loads or waits, why it can't be played.
    fn middle(&self) -> Option<AnyElement> {
        let id = self.id.clone();
        match self.state()? {
            State::Loading | State::Buffering => Some(
                div()
                    .debug_selector(move || format!("video-loading-{id}"))
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Glyph::LOADING.icon().size(px(32.0)).text_color(gpui::rgb(ON_VIDEO)))
                    .into_any_element(),
            ),
            State::Failed(failure) => Some(self.failure(failure)),
            State::Paused | State::Playing | State::Ended => None,
        }
    }

    fn failure(&self, failure: Failure) -> AnyElement {
        let why = match failure {
            Failure::Unavailable => "This video can't be opened here.",
            Failure::Network => "The video couldn't be loaded. Check your connection.",
            Failure::Unsupported => "This video's format can't be played here.",
            Failure::NoCodec => "This video needs a codec that isn't installed (GStreamer's plugins for its format).",
            Failure::Internal => "The video couldn't be started.",
        };
        let id = self.id.clone();
        let open = self.file.open_link().map(str::to_owned);
        div()
            .debug_selector(move || format!("video-failed-{id}"))
            .absolute()
            .inset_0()
            .bg(hsla(0.0, 0.0, 0.0, 0.75))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_4()
            .text_sm()
            .text_color(gpui::rgb(ON_VIDEO))
            .child(div().text_center().child(why))
            .children(open.map(|link| {
                div()
                    .id("video-open-instead")
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui::rgb(ON_VIDEO))
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
                    .on_click(move |_, _, cx| cx.open_url(&link))
                    .child(OPEN_IN_BROWSER)
            }))
            .into_any_element()
    }

    /// The seek bar and the buttons over the box's bottom, on a shade: while the pointer is on the video, or always
    /// while they are used (see the module docs). Its test selector says which: `video-controls-<id> shown` or
    /// `video-controls-<id> on-hover`.
    fn controls(&mut self, group: &SharedString, keys: bool, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let always = (self.fill && !self.playing()) || self.dragging.is_some() || self.menu.is_some() || keys;
        let bar = self.seek_bar(cx);
        let buttons = self.buttons(window, cx);
        let id = self.id.clone();
        let shown = if always { "shown" } else { "on-hover" };
        div()
            .debug_selector(move || format!("video-controls-{id} {shown}"))
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .px_3()
            .pb_1()
            .pt_6()
            .bg(gpui::linear_gradient(
                0.0,
                gpui::linear_color_stop(hsla(0.0, 0.0, 0.0, 0.7), 0.0),
                gpui::linear_color_stop(hsla(0.0, 0.0, 0.0, 0.0), 1.0),
            ))
            .when(!always, |controls| {
                controls
                    .opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
            })
            .flex()
            .flex_col()
            .gap_1()
            .child(bar)
            .child(buttons)
            .into_any_element()
    }

    fn seek_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let (position, duration) = self.status.map_or((Duration::ZERO, Duration::ZERO), |status| {
            (status.position, status.duration)
        });
        let played = if duration.is_zero() {
            0.0
        } else {
            (position.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        let fraction = self.dragging.unwrap_or(played);
        let tip = SharedString::from(format!("{} / {}", clock(duration.mul_f32(fraction)), clock(duration)));
        let bar = Rc::clone(&self.bar);
        let id = self.id.clone();
        div()
            .id("video-seek")
            .debug_selector(move || format!("video-seek-{id}"))
            .relative()
            .h(px(14.0))
            .w_full()
            .cursor_pointer()
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.dragging = this.fraction_at(event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.dragging.is_some() && event.pressed_button == Some(MouseButton::Left) {
                    this.dragging = this.fraction_at(event.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_drag))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_drag))
            .child(
                canvas(move |bounds, _, _| bar.set(Some(bounds)), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(5.5))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 1.0, 0.35))
                    .child(
                        div()
                            .h_full()
                            .w(relative(fraction))
                            .rounded_full()
                            .bg(gpui::rgb(ON_VIDEO)),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .top(px(1.0))
                    .left(relative(fraction))
                    .ml(px(-6.0))
                    .size(px(12.0))
                    .rounded_full()
                    .bg(gpui::rgb(ON_VIDEO)),
            )
            .into_any_element()
    }

    fn end_drag(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            let fraction = self.fraction_at(event.position).unwrap_or(0.0);
            self.seek_to_fraction(fraction, cx);
        }
    }

    /// Play, back, forward and mute on the left; "More options", full screen and the browser on the right.
    fn buttons(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .justify_between()
            .child(self.left_buttons(window, cx))
            .child(self.right_buttons(window, cx))
            .into_any_element()
    }

    /// Play, back, forward and mute.
    fn left_buttons(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let id = &self.id;
        let (play_glyph, play_tip) = if self.playing() {
            (Glyph::PAUSE, "Pause")
        } else {
            (Glyph::PLAY_MEDIA, "Play")
        };
        // a player that plays with no speaker says so on its sound's button
        let silent = self.status.is_some_and(|status| self.playing() && !status.sound);
        let (volume_glyph, volume_tip) = if silent {
            (Glyph::VOLUME_OFF, "No sound device")
        } else if self.muted {
            (Glyph::VOLUME_OFF, "Unmute")
        } else {
            (Glyph::VOLUME, "Mute")
        };
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.reached(
                "play",
                play_tip,
                control(
                    id,
                    "play",
                    play_glyph,
                    play_tip,
                    None,
                    cx.listener(|this, _, _, cx| this.toggle(cx)),
                ),
                window,
                cx,
            ))
            .child(self.reached(
                "back",
                BACK,
                control(
                    id,
                    "back",
                    Glyph::SKIP_BACK,
                    BACK,
                    Some("10"),
                    cx.listener(|this, _, _, cx| this.skip(true, cx)),
                ),
                window,
                cx,
            ))
            .child(self.reached(
                "forward",
                FORWARD,
                control(
                    id,
                    "forward",
                    Glyph::SKIP_FORWARD,
                    FORWARD,
                    Some("10"),
                    cx.listener(|this, _, _, cx| this.skip(false, cx)),
                ),
                window,
                cx,
            ))
            .child(self.sound(volume_glyph, volume_tip, window, cx))
            .children(self.times())
    }

    /// The sound's button and, while the pointer is on it (or it is dragged), its volume slider, as Teams' player has
    /// them.
    fn sound(&self, glyph: Glyph, tip: &'static str, window: &Window, cx: &mut Context<Self>) -> Div {
        let group = SharedString::from(format!("video-sound-{}", self.id));
        let slider = self.volume_slider(cx);
        let shown = self.volume.adjusting;
        div()
            .group(group.clone())
            .flex()
            .items_center()
            .child(self.reached(
                "mute",
                tip,
                control(
                    &self.id,
                    "mute",
                    glyph,
                    tip,
                    None,
                    cx.listener(|this, _, _, cx| this.toggle_mute(cx)),
                ),
                window,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .w(px(if shown { 72.0 } else { 0.0 }))
                    .group_hover(group, |style| style.w(px(72.0)))
                    .child(slider),
            )
    }

    /// The sound's volume slider, beside its button.
    fn volume_slider(&self, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let level = if self.muted { 0.0 } else { self.volume.level };
        let bar = Rc::clone(&self.volume.bar);
        let id = self.id.clone();
        div()
            .id("video-volume")
            .debug_selector(move || format!("video-volume-{id}"))
            .relative()
            .h(px(14.0))
            .w(px(64.0))
            .ml_1()
            .cursor_pointer()
            .tooltip(|window, cx| Tooltip::new("Volume").build(window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.volume.adjusting = true;
                    this.set_volume_at(event.position, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.volume.adjusting && event.pressed_button == Some(MouseButton::Left) {
                    this.set_volume_at(event.position, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_volume))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_volume))
            .child(
                canvas(move |bounds, _, _| bar.set(Some(bounds)), |_, (), _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(5.5))
                    .h(px(3.0))
                    .rounded_full()
                    .bg(hsla(0.0, 0.0, 1.0, 0.35))
                    .child(div().h_full().w(relative(level)).rounded_full().bg(gpui::rgb(ON_VIDEO))),
            )
            .child(
                div()
                    .absolute()
                    .top(px(2.0))
                    .left(relative(level))
                    .ml(px(-5.0))
                    .size(px(10.0))
                    .rounded_full()
                    .bg(gpui::rgb(ON_VIDEO)),
            )
    }

    fn end_volume(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.volume.adjusting {
            self.volume.adjusting = false;
            cx.notify();
        }
    }

    /// Where it plays and its length (`0:10 / 1:52`), as Teams' player shows them beside its sound's button, once its
    /// length is known.
    fn times(&self) -> Option<Div> {
        let status = self.status.filter(|status| !status.duration.is_zero())?;
        let id = self.id.clone();
        Some(
            div()
                .debug_selector(move || format!("video-time-{id}"))
                .pl_2()
                .text_xs()
                .text_color(gpui::rgb(ON_VIDEO))
                .whitespace_nowrap()
                .child(format!(
                    "{} / {}",
                    clock(status.position.min(status.duration)),
                    clock(status.duration)
                )),
        )
    }

    /// "More options" (a recap's: captions, the speed and the settings), full screen and the browser.
    fn right_buttons(&self, window: &Window, cx: &mut Context<Self>) -> Div {
        let id = &self.id;
        let (screen_glyph, screen_tip) = if self.full_screen.on {
            (Glyph::SHRINK, "Exit full screen")
        } else {
            (Glyph::FULLSCREEN, "Full screen")
        };
        let open = self.file.open_link().map(str::to_owned);
        div()
            .flex()
            .items_center()
            .gap_1()
            .when(!self.fill, |buttons| buttons.child(self.more()))
            .when(self.fill, |buttons| buttons.children(self.recap_buttons(window, cx)))
            .child(self.reached(
                "fullscreen",
                screen_tip,
                control(
                    id,
                    "fullscreen",
                    screen_glyph,
                    screen_tip,
                    None,
                    cx.listener(|this, _, window, cx| this.toggle_fullscreen(window, cx)),
                ),
                window,
                cx,
            ))
            .children(open.map(|link| {
                let open = control(
                    id,
                    "open",
                    Glyph::POP_OUT,
                    OPEN_IN_BROWSER,
                    None,
                    move |_, _, cx: &mut App| {
                        cx.open_url(&link);
                    },
                );
                self.reached("open", OPEN_IN_BROWSER, open, window, cx)
            }))
    }

    /// A recap's captions (when it has them), speed and settings buttons, as Teams' Recap player has them.
    fn recap_buttons(&self, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let id = &self.id;
        let open = self.menu;
        let mut buttons = Vec::new();
        if self.captions.is_some() {
            let captions = menu_control(
                id,
                "captions",
                Glyph::CAPTIONS.icon().size(px(18.0)).into_any_element(),
                "Captions",
                open == Some(Menu::Captions),
                cx.listener(|this, _, window, cx| this.toggle_menu(Menu::Captions, window, cx)),
            );
            buttons.push(self.reached("captions", "Captions", captions, window, cx));
        }
        let speed = menu_control(
            id,
            "speed",
            div()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .child(rate_label(self.rate))
                .into_any_element(),
            "Playback speed",
            open == Some(Menu::Speed),
            cx.listener(|this, _, window, cx| this.toggle_menu(Menu::Speed, window, cx)),
        );
        buttons.push(self.reached("speed", "Playback speed", speed, window, cx));
        let settings = menu_control(
            id,
            "settings",
            Glyph::SETTINGS.icon().size(px(18.0)).into_any_element(),
            "Settings",
            matches!(open, Some(Menu::Settings(_))),
            cx.listener(|this, _, window, cx| this.toggle_menu(Menu::Settings(Page::Main), window, cx)),
        );
        buttons.push(self.reached("settings", "Settings", settings, window, cx));
        buttons
    }

    /// "More options" on the video: the file's own actions, as the card's header offers them.
    fn more(&self) -> AnyElement {
        let id = self.id.clone();
        let open = self.file.open_link().map(str::to_owned);
        let download = self.file.download_link();
        let button = Button::new(SharedString::from(format!("video-more-button-{id}")))
            .ghost()
            .icon(Glyph::MORE.icon().size(px(18.0)).text_color(gpui::rgb(ON_VIDEO)))
            .tooltip("More options")
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu;
                if let Some(open) = open.clone() {
                    let copied = open.clone();
                    menu = menu
                        .item(
                            menu_item("open", Glyph::EXTERNAL_LINK, OPEN_IN_BROWSER).on_click(move |_, _, cx| {
                                cx.open_url(&open);
                            }),
                        )
                        .item(menu_item("copy", Glyph::LINK, COPY_LINK).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()));
                        }));
                }
                if let Some(download) = download.clone() {
                    menu = menu.item(
                        menu_item("download", Glyph::DOWNLOAD, DOWNLOAD)
                            .on_click(move |_, _, cx| cx.open_url(&download)),
                    );
                }
                menu
            });
        selected_by(format!("video-more-{}", self.id), button).into_any_element()
    }
}

/// A control on the video: its icon in white (with a small mark under it, the skip buttons' "10"), its tooltip, its
/// click; its test selector `video-<what>-<card>`.
fn control(
    card: &str,
    what: &'static str,
    glyph: Glyph,
    tip: &'static str,
    mark: Option<&'static str>,
    click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let selector = format!("video-{what}-{card}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .relative()
        .size(px(28.0))
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        // a press of the pointer leaves the keys where they are, as on the kit's buttons
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            click(event, window, cx);
        })
        .child(glyph.icon().size(px(18.0)).text_color(gpui::rgb(ON_VIDEO)))
        .children(mark.map(|mark| {
            div()
                .absolute()
                .bottom(px(-1.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .text_size(px(8.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui::rgb(ON_VIDEO))
                .child(mark)
        }))
}

/// A button that opens a menu over the picture (a recap's captions, speed and settings): its face in white, underlined
/// while its menu is open, as Teams draws it; its test selector `video-<what>-<card>`.
fn menu_control(
    card: &str,
    what: &'static str,
    face: AnyElement,
    tip: &'static str,
    open: bool,
    click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let selector = format!("video-{what}-{card}");
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector.clone())
        .relative()
        .h(px(28.0))
        .min_w(px(28.0))
        .px_1()
        .rounded_md()
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(gpui::rgb(ON_VIDEO))
        .hover(|style| style.bg(hsla(0.0, 0.0, 1.0, 0.15)))
        .when(!open, |button| {
            button.tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
        })
        // a press of the pointer leaves the keys where they are, as on the kit's buttons
        .on_mouse_down(MouseButton::Left, |_, window, cx| {
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            click(event, window, cx);
        })
        .child(face)
        .when(open, |button| {
            button.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-3.0))
                    .h(px(2.0))
                    .bg(gpui::rgb(ON_VIDEO)),
            )
        })
}

/// A "More options" row: its icon and words.
fn menu_item(what: &'static str, glyph: Glyph, label: &'static str) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || format!("video-menu-{what}"))
            .flex()
            .items_center()
            .gap_2()
            .child(glyph.icon().size(px(14.0)))
            .child(label)
    })
}

/// `bounds` narrowed to a picture of `width` by `height` fitted inside it, centred, its aspect ratio kept.
pub(crate) use crate::video_surface::fitted;
/// A time as the player writes it: `m:ss`, or `h:mm:ss` from an hour.
#[must_use]
pub fn clock(at: Duration) -> String {
    let seconds = at.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Every player drawn this run (the ones whose card went are let go): one plays at a time, as in Teams.
#[derive(Default)]
struct Players(Vec<WeakEntity<VideoPlayer>>);

impl Global for Players {}

/// Pauses every player but the one being updated.
fn pause_others(cx: &mut Context<VideoPlayer>) {
    let me = cx.entity_id();
    for other in players(cx) {
        if other.entity_id() == me {
            continue;
        }
        other.update(cx, |other, cx| {
            if other.playing() && other.player.is_some() {
                other.ask(Asked::Pause);
                cx.notify();
            }
        });
    }
}

/// The players drawn now, the first drawn first.
#[must_use]
pub fn players(cx: &App) -> Vec<Entity<VideoPlayer>> {
    cx.try_global::<Players>()
        .map(|players| players.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default()
}

/// The player of the card `id` showing `file`, kept while the card is drawn.
pub fn player_of(
    id: &str,
    file: &SharedFile,
    source: &PictureSource,
    window: &mut Window,
    cx: &mut App,
) -> Entity<VideoPlayer> {
    let (id, file, source) = (id.to_owned(), file.clone(), source.clone());
    window.use_keyed_state(SharedString::from(format!("video-player-{id}")), cx, move |_, cx| {
        VideoPlayer::new(id, file, source, cx)
    })
}

#[cfg(test)]
#[path = "../tests/support/video_player_units.rs"]
mod tests;
