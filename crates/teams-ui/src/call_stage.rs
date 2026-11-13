//! What a call's own window shows ([`CallStage`]), as Teams' call window does: the main window's chats stay usable
//! beside it ([`crate::call_window`] opens and closes it).
//!
//! - **The header** ([`toolbar`]): who the call is with (a chat's name, an invitation's caller, "Echo" for the test
//!   call) and its state (calling, ringing, connecting, connected with its duration, reconnecting, taken by
//!   voicemail); on its right Teams' call controls in Teams' order: Chat (the call's chat), People (the participants'
//!   pane, with how many are in the call), Raise and React (a group call's and a meeting's, as Teams offers them), View
//!   (Gallery or Speaker, Focus on content while a share shows, Show videos first, Max gallery size: 4, 9, 16 or 49;
//!   More options: Turn off other videos, Hide me, Full screen), More (Language and speech, Video effects and settings, Audio settings: the main window's
//!   settings on their Calls page), the camera (a build that shows video) and the microphone, each with a menu of the
//!   devices the settings list, Share (a 1:1 call's, [`share`]: "Share content", its screen picked at the desktop;
//!   a red Stop sharing while it shares), and Leave in Teams' red (a meeting's organizer's with End meeting beside it,
//!   which asks first). Each control's tooltip names it and its key.
//! - **The stage** ([`gallery`]): before anyone is in the call, the callee's avatar (a 1:1 call's other one's picture)
//!   with the call's state, as Teams shows it while it rings; once the call's roster told who is in it, a tile per
//!   participant (its video when a line shows it, else its person's picture, or their initials, centred and sized to
//!   the tile: [`faces`]; its name with its microphone muted and its hand raised; a border in the brand colour
//!   while it speaks, Teams' gold while its hand is up) in the gallery, as many to a page as Max gallery size and the
//!   stage allow, or the one who speaks (or the one pinned) large over a strip of the others; more than fit go on
//!   further pages, turned with the `<` `>` under the gallery or at the strip's end ([`pages`]); each tile's More
//!   options pins it for the account alone. The account's own tile (its own picture while its camera is off) is the
//!   last tile on each gallery page in a group call; in a 1:1 call and the speaker view it floats at the stage's
//!   bottom right, as Teams' self view does (the last tile of the speaker view's strip in a stage too narrow for it
//!   beside the strip); View can hide it. Live reactions float up from the stage's bottom. The
//!   far end's shared screen takes the stage, the people's tiles (the account's own last) in a strip on its right
//!   (View's Focus on content: the share alone). Nothing on the stage covers the controls above it.
//! - **The People pane** ([`people_pane`]): a meeting's Share invite (its link copied), who waits in its lobby (Admit
//!   all, Admit and Deny each), and who is in the call, the account first, with whether they speak, are muted or have
//!   their hand up (numbered in the order the hands went up, the avatar ringed in gold), and "Organizer" under the
//!   meeting's organizer. People's red badge counts the hands up and the people waiting in the lobby.
//! - **The Chat pane** ([`chat_pane`]): the call's chat beside the stage, as the main window shows it.
//! - **The video effects' pane** (More's or the camera menu's Video effects; one pane at a time, as Teams has it):
//!   the self-preview over the background effects ([`crate::effects::EffectsPicker`]): what the call sends while the
//!   camera is on, else a preview only the user's Preview turns on. A background chosen applies to the call's camera
//!   at once and is kept for the next calls.
//! - **The Call health pane** ([`health`], More → Settings → Call health): the call's network, audio, video and screen
//!   sharing as its media measures them, refreshed while it is open; reading them sends nothing.
//! - **How a call ended**, when the user didn't end it (the far end hung up, it dropped, never connected, went to
//!   voicemail): who it was with (their avatar and name), why it ended in plain words ([`calls::Ended::reason`]: "Lab
//!   Bee ended the call", "No one else joined the call", never a code the service sent), how long it lasted, and
//!   Call back (a 1:1 or a group chat's call, Ctrl+Shift+C) or Rejoin (a meeting's call, or a group call that runs on
//!   without the account, Ctrl+Shift+J), each a new start or join the user asks for, and Close. Left alone it closes by itself
//!   ([`AUTO_CLOSE`]) unless Call back or Rejoin was asked.
//! - **A test call's results** once it ended, as Teams shows them in its call window: the microphone, the speaker and
//!   the camera (each naming the device the call used) and the network, each checked by what the call showed
//!   ([`calls::DeviceCheck`]), with "Device settings" and Close.
//!
//! Every control asks through [`calls::ask`], as the main window's do; a device chosen goes to the settings, as the
//! settings' own pickers do it. What it shows is traced (`call-participant shown` per grid cell when the grid changed,
//! with the call's number, the participant's MRI and kinds for its camera, its line and whether it speaks, and
//! `call-participant self` for the account's own; the tiles' `video-tile` lines; `call-window ended` with the call's
//! number, why it ended and what it offers: `call-back`, `rejoin` or `none`), never a name.

mod captions;
mod chat_pane;
mod controls;
mod faces;
mod gallery;
mod health;
pub use health::HEALTH_REFRESH;
mod invite;
mod meeting;
mod pages;
mod people_pane;
mod recording;
mod share;
mod toolbar;
mod transcript;
mod transfer;
pub use gallery::Layout;
pub use meeting::{ATTENDANCE_FILE, TimerChange, attendance_list, timer_after, timer_text};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Anchor, AnyElement, App, AppContext as _, AsyncApp, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px, relative, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::Size;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::tooltip::Tooltip;
use teams_core::{CallId, CallParticipant, CallStatus, CallView};
use teams_media::devices::catalog::DeviceKind;
use teams_proto::Mri;

use crate::bridge::Core;
use crate::call_view::{self, CallPeople, Control, Look, control};
use crate::calls::{self, ActiveCall, Ask, CallKind, Calls, DeviceCheck, EndReason, Ended, Verdict};
use crate::effects::{EffectsPicker, PickerPlace};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::notify::SOMEONE;
use crate::people;
use crate::settings::SettingsModel;
use crate::settings_view::{self, DeviceChoice, device_label};
use crate::shell::{
    CallChat, HangUpCall, JoinCall, OpenShareCall, ShellParts, ToggleCameraCall, ToggleCaptionsCall, ToggleHandCall,
    ToggleMuteCall,
};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::translate::CaptionTranslation;
use crate::video::{Tile, Tiles, VideoFeed};

/// The call window's key context.
pub const CONTEXT: &str = "CallStage";

/// The header's height, px.
const HEADER_HEIGHT: f32 = 56.0;
/// The shortest the stage gets beside captions, px: room for the timer, its padding, two strip tiles and the pager.
const MIN_STAGE_HEIGHT: f32 = 180.0;
/// The self view's size, px.
const SELF_VIEW: (f32, f32) = (224.0, 126.0);
/// A side pane's width (People, Video effects), px.
const PANE_WIDTH: f32 = 320.0;
/// The Chat pane's width, px.
const CHAT_WIDTH: f32 = 360.0;
/// The window's width under which the toolbar's controls show their icons alone, px.
const COMPACT_BELOW: f32 = 860.0;
/// The avatar of the one the call rings, holds or ended with: the kit's large one (80 px), whose initials it sizes and
/// centres (a custom size leaves them small, at its top left). A tile without video sizes its face to the tile
/// ([`faces::face_side`], `CallStage::tile_face`).
const BIG_AVATAR: Size = Size::Large;
/// [`BIG_AVATAR`]'s side, px (the kit's large avatar).
const BIG_AVATAR_PX: f32 = 80.0;
/// How long a call's end stays shown when the user does nothing.
pub const AUTO_CLOSE: Duration = Duration::from_secs(8);

/// What names a group chat's call in its window: the main window's chat list's row for it, when the list is there.
pub type ChatNamer = Rc<dyn Fn(&str, &App) -> Option<SharedString>>;

/// What the call window asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallStageEvent {
    /// More's (or a test call's results') "Device settings": the main window's settings on their Calls page.
    DeviceSettings,
    /// Close on a call's end or a test call's results, or the end left alone for [`AUTO_CLOSE`].
    Close,
}

/// What a call's end offers: a new start to its chat, or a join of its group call that runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FollowUp {
    ask: Ask,
    rejoin: bool,
}

/// What the stage is made of: the account's engine (its request side), its calls, the app's settings, the view
/// trace, and what names a group chat.
pub struct StageParts {
    pub core: Arc<dyn Core>,
    pub calls: Entity<Calls>,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
    pub namer: Option<ChatNamer>,
    /// The app's other accounts' calls: Call back and Rejoin wait while one of them is busy (one call at a time across
    /// the accounts, as the shell's call buttons do).
    pub others: Vec<Entity<Calls>>,
    /// What the call's chat is drawn with in the Chat pane (the account's shell parts); none, no Chat.
    pub shell: Option<ShellParts>,
}

/// One tile of the stage: its test selector, who (by MRI, when known), its name, whether it speaks, its microphone
/// muted and its hand raised, and the video line showing it.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a sign of its own the tile shows of its person"
)]
struct StageTile {
    selector: String,
    mri: Option<String>,
    name: SharedString,
    speaking: bool,
    muted: bool,
    hand: bool,
    /// Its camera is on (View's Show videos first leads with these).
    video_on: bool,
    video: Option<Tile>,
}

/// The meeting a call is, as its chat's thread names it: for Share invite.
struct MeetingLink {
    call: CallId,
    /// Its details once read (its join link among them).
    details: Option<teams_proto::MeetingDetails>,
    /// Copy meeting link put the link on the clipboard.
    copied: bool,
}

/// The side pane open beside the stage: one at a time, as Teams has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    None,
    People,
    Chat,
    Effects,
    /// More → Meeting info.
    MeetingInfo,
    /// More → Settings → Call health ([`health`]).
    Health,
    /// More → Record and transcribe → Show transcript.
    Transcript,
}

/// What the call window shows (see the module docs). A GPUI entity, the window's root view.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a choice of its own in the window (View's options, a pane, a confirmation), set by its own control"
)]
pub struct CallStage {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    namer: Option<ChatNamer>,
    /// The other accounts' calls ([`StageParts::others`]).
    others: Vec<Entity<Calls>>,
    /// The call the window is for.
    call: CallId,
    people: CallPeople,
    /// The video tiles, in a build that shows video.
    tiles: Option<Tiles>,
    /// The side pane open.
    pane: Pane,
    share_menu: Option<Entity<PopupMenu>>,
    share_dismiss: Option<Subscription>,
    /// The video effects' pane's picker, while it is open.
    effects: Option<Entity<EffectsPicker>>,
    /// The call's chat, once the Chat pane showed it.
    chat: Option<Entity<crate::chat_view::ChatView>>,
    /// The Call health pane's reading, while it is open.
    health: Option<health::HealthPane>,
    /// What the call's chat is drawn with ([`StageParts::shell`]).
    shell: Option<ShellParts>,
    /// How the stage lays its tiles out (View).
    layout: Layout,
    /// The participant pinned for the account alone, by MRI.
    pinned: Option<String>,
    /// Who spoke last (the speaker view shows them while nobody speaks), by MRI.
    last_speaker: Option<String>,
    /// View's Hide me: the self view hidden.
    self_hidden: bool,
    /// View's Show videos first: the tiles whose camera is on lead.
    videos_first: bool,
    /// View's Max gallery size, the pages of the gallery and the strips shown, and the window's and the stage's size.
    paging: pages::Paging,
    /// View's Turn off other videos: the others' tiles show their pictures (or initials), not their video.
    others_off: bool,
    /// Who has a hand up, in the order they raised it (the account by its own MRI): the People pane numbers them.
    hands: Vec<String>,
    /// End meeting asks first: its confirmation shows.
    confirm_end: bool,
    /// People's "Invite someone or dial a number" box, while the pane shows it.
    inviting: Option<invite::Finder>,
    /// Transfer's dialog, while it is open.
    dialog: Option<transfer::Dialog>,
    /// The meeting the call is, as its chat's thread names it (its join link), read once per call.
    meeting: Option<MeetingLink>,
    /// More → Timer opened the timer's bar before the meeting runs one: five minutes, paused, until it is sent.
    timer_draft: bool,
    /// The timer's editor while it is open: its minutes and seconds.
    timer_edit: Option<(u64, u64)>,
    /// "Lock the meeting?" is asked.
    confirm_lock: bool,
    /// The meeting's lock as last seen, and the notice of its change while it shows (locked or not).
    lock_seen: Option<bool>,
    lock_notice: Option<bool>,
    /// A recording ran when last seen, and its notice shows.
    recording_seen: bool,
    recording_notice: bool,
    /// Record and transcribe's dialogs and notices.
    record: recording::RecordUi,
    /// The organizer's Controls and Mute all, and the organizer's mute's card.
    controls: controls::ControlsUi,
    /// Meeting info's Copy join info put the join info on the clipboard.
    join_info_copied: bool,
    /// The live reactions' floating while some show.
    reacting: Option<Task<()>>,
    /// The devices a test call used, named as it ended.
    results: Option<[String; 3]>,
    /// The grid as last traced.
    grid_traced: Option<CallView>,
    /// The window's title as last set.
    titled: Option<SharedString>,
    caption_scroll: gpui::ScrollHandle,
    caption_font: f32,
    caption_top: bool,
    caption_language: Option<String>,
    /// The captions translated on this computer, when the user picked a language ([`crate::translate`]).
    translation: Entity<CaptionTranslation>,
    /// Who the call was with, as last said while it was up: its end still names them.
    with: Option<SharedString>,
    /// The account's own name, as its profile gives it: People names it so, as Teams does.
    own_name: Option<SharedString>,
    /// The call's people's pictures ([`faces`]).
    faces: faces::Faces,
    /// The call's end was shown (and traced).
    end_shown: bool,
    /// The end closing by itself, while it is due.
    closing: Option<Task<()>>,
    focus: FocusHandle,
    ticking: Option<Task<()>>,
    _waking: Option<Task<()>>,
    _observing: [Subscription; 4],
    /// Profiles read in the background (the account's chats' model, when the window has its shell's parts): the
    /// faces read again.
    _profiles: Option<Subscription>,
}

impl EventEmitter<CallStageEvent> for CallStage {}

impl CallStage {
    /// What the stage follows: the calls, the captions' translation, the settings, and its own release.
    fn observing(
        (calls, translation, settings): (&Entity<Calls>, &Entity<CaptionTranslation>, &Entity<SettingsModel>),
        cx: &mut Context<Self>,
    ) -> [Subscription; 4] {
        [
            cx.observe(calls, |this: &mut Self, _, cx| this.changed(cx)),
            // a caption's translation came, or translating stopped
            cx.observe(translation, |_, _, cx| cx.notify()),
            // the devices listed or chosen: their menus follow
            cx.observe(settings, |_, _, cx| cx.notify()),
            // the window gone (its call ended, or closed): the call's frames go with it
            cx.on_release(|this: &mut Self, _| {
                if let Some(tiles) = &mut this.tiles {
                    tiles.clear(this.call.get());
                }
            }),
        ]
    }
}

impl CallStage {
    fn observe_profiles(shell: Option<&ShellParts>, cx: &mut Context<Self>) -> Option<Subscription> {
        shell.map(|shell| {
            cx.observe(&shell.models.chats, |this: &mut Self, chats, cx| {
                this.profiles_changed(&chats, cx);
            })
        })
    }

    /// The window of call `call` over `parts`.
    pub fn new(parts: StageParts, call: CallId, cx: &mut Context<Self>) -> Self {
        let StageParts {
            core,
            calls,
            settings,
            trace,
            namer,
            others,
            shell,
        } = parts;
        let translation = cx.new(|cx| CaptionTranslation::new(settings.clone(), cx));
        let observing = Self::observing((&calls, &translation, &settings), cx);
        let feed = calls.read(cx).video().cloned();
        let waking = feed.as_ref().map(|feed| wake_on_frames(feed, cx));
        let tiles = feed.map(|feed| Tiles::new(feed, Arc::clone(&trace)));
        // profile changes before the stage asked about anyone change nothing it read
        let seen = shell.as_ref().map_or(0, |shell| shell.models.chats.read(cx).revision());
        let profiles = Self::observe_profiles(shell.as_ref(), cx);
        let mut stage = Self {
            core,
            calls,
            settings,
            trace,
            namer,
            others,
            call,
            people: CallPeople::default(),
            tiles,
            pane: Pane::None,
            share_menu: None,
            share_dismiss: None,
            effects: None,
            chat: None,
            health: None,
            shell,
            layout: Layout::default(),
            pinned: None,
            last_speaker: None,
            self_hidden: false,
            videos_first: false,
            paging: pages::Paging::default(),
            others_off: false,
            hands: Vec::new(),
            confirm_end: false,
            inviting: None,
            dialog: None,
            meeting: None,
            timer_draft: false,
            timer_edit: None,
            confirm_lock: false,
            lock_seen: None,
            lock_notice: None,
            recording_seen: false,
            recording_notice: false,
            record: recording::RecordUi::default(),
            controls: controls::ControlsUi::default(),
            join_info_copied: false,
            reacting: None,
            results: None,
            grid_traced: None,
            titled: None,
            caption_scroll: gpui::ScrollHandle::new(),
            caption_font: 18.0,
            caption_top: false,
            caption_language: None,
            translation,
            with: None,
            own_name: None,
            faces: faces::Faces::seen_at(seen),
            end_shown: false,
            closing: None,
            focus: cx.focus_handle(),
            ticking: None,
            _waking: waking,
            _observing: observing,
            _profiles: profiles,
        };
        stage.read_own_name(cx);
        stage.changed(cx);
        stage
    }

    /// Reads the account's own name from its profile, once: People shows it in "You"'s place.
    fn read_own_name(&self, cx: &mut Context<Self>) {
        let reply = self.core.person(self.core.me().to_owned());
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let name = reply
                .await
                .ok()
                .and_then(|person| person.name)
                .filter(|name| !name.trim().is_empty());
            // a stage that is gone shows nobody
            // none leaves a name the faces' later read gave
            let Some(name) = name else { return };
            let _ = this.update(cx, |this, cx| {
                this.own_name = Some(SharedString::from(name));
                cx.notify();
            });
        })
        .detach();
    }

    /// The picture of the one person the call is with (`call`'s, else the call's read last), when it is with one
    /// person who has one: the ringing and the ended screens show it, as Teams does in a 1:1 call.
    fn partner_picture(&self, call: Option<CallId>) -> Option<&people::Picture> {
        let people = call.map_or_else(|| self.people.last(), |call| self.people.of(call));
        match people {
            [(mri, _)] => self.faces.picture(mri),
            _ => None,
        }
    }

    /// Opens or closes the participants' pane, as People does.
    pub fn show_roster(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.pane = Pane::People;
            self.effects = None;
            self.read_meeting(cx);
        } else if self.pane == Pane::People {
            self.pane = Pane::None;
        }
        cx.notify();
    }

    /// Whether the People pane is open.
    #[must_use]
    pub fn roster_shown(&self) -> bool {
        self.pane == Pane::People
    }

    /// Reads the meeting the call is (its join link, for Share invite), once per call; nothing for any other call.
    fn read_meeting(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.live(cx).filter(|active| active.kind == CallKind::Meeting) else {
            return;
        };
        let Some(chat) = active.chat.clone() else { return };
        if self.meeting.as_ref().is_some_and(|meeting| meeting.call == active.id) {
            return;
        }
        self.meeting = Some(MeetingLink {
            call: active.id,
            details: None,
            copied: false,
        });
        let reply = self.core.meeting(chat);
        let call = active.id;
        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let details = reply.await.ok().flatten();
            // a stage that is gone has no pane to fill
            let _ = this.update(cx, |this, cx| {
                if let Some(meeting) = this.meeting.as_mut().filter(|meeting| meeting.call == call) {
                    meeting.details = details;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Opens or closes the video effects' pane, as More's Video effects does (a build that shows video).
    pub fn show_effects(&mut self, open: bool, cx: &mut Context<Self>) {
        if !open {
            self.effects = None;
        } else if self.effects.is_none()
            && let Some(feed) = self.calls.read(cx).video().cloned()
        {
            let (settings, trace, call) = (self.settings.clone(), Arc::clone(&self.trace), self.call.get());
            self.effects =
                Some(cx.new(|cx| EffectsPicker::new(settings, trace, PickerPlace::CallPane, cx).in_call(feed, call)));
            self.pane = Pane::Effects;
        }
        if !open && self.pane == Pane::Effects {
            self.pane = Pane::None;
        }
        cx.notify();
    }

    /// Whether the video effects' pane is open.
    #[must_use]
    pub fn effects_shown(&self) -> bool {
        self.effects.is_some()
    }

    /// What takes the window's keyboard, so the call keys reach the window's controls.
    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The call while it is up, if it is this window's.
    fn live(&self, cx: &App) -> Option<ActiveCall> {
        self.calls
            .read(cx)
            .call()
            .filter(|active| active.id == self.call)
            .cloned()
    }

    /// How this window's call ended, once it did.
    fn ended(&self, cx: &App) -> Option<Ended> {
        self.calls
            .read(cx)
            .ended()
            .filter(|ended| ended.id == self.call)
            .cloned()
    }

    /// The calls changed: the participants read, the duration ticking while connected; once the call ended its frames
    /// go and a test call's devices are named as they were.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.caption_scroll.scroll_to_bottom();
        let live = self.live(cx);
        self.translate(live.as_ref(), cx);
        if let Some(active) = &live {
            let (core, echo) = (Arc::clone(&self.core), self.calls.read(cx).echo().cloned());
            self.people.follow(
                active,
                (&core, echo.as_ref()),
                cx,
                (
                    |stage| &mut stage.people,
                    |stage, cx| {
                        stage.ask_faces(cx);
                        cx.notify();
                    },
                ),
            );
            self.ask_faces(cx);
            self.float_reactions(active, cx);
            self.order_hands(active);
            self.meeting_seen(active);
            self.controls_follow(active, cx);
            self.record_problem_seen(cx);
        } else {
            self.confirm_end = false;
            self.confirm_lock = false;
            self.record.dialog = None;
            self.record.confirm_stop = false;
            self.record.confirm_stop_transcription = false;
            // the call's Mute notifications held them back only while it ran
            crate::notify::mute_in_call(false, cx);
            if let Some(tiles) = &mut self.tiles {
                tiles.clear(self.call.get());
            }
            if self.results.is_none() && self.ended(cx).is_some_and(|ended| ended.check.is_some()) {
                self.results = Some(self.devices_in_use(cx));
            }
            self.end_seen(cx);
        }
        if let Some(ask) = self.calls.update(cx, Calls::next_choice) {
            self.ask(ask, cx);
        }
        let connected = live
            .as_ref()
            .is_some_and(|active| active.status == CallStatus::Connected);
        if connected && self.ticking.is_none() {
            self.ticking = Some(call_view::ticking(cx));
        } else if !connected {
            self.ticking = None;
        }
        cx.notify();
    }

    /// The call's end, shown for the first time (one the user didn't choose, not a test call's): traced, and due to
    /// close by itself.
    fn end_seen(&mut self, cx: &mut Context<Self>) {
        if self.end_shown {
            return;
        }
        let Some(ended) = self.end_card(cx) else { return };
        self.end_shown = true;
        let offers = match self.follow_up(&ended, cx) {
            Some(FollowUp { rejoin: true, .. }) => "rejoin",
            Some(_) => "call-back",
            None => "none",
        };
        self.trace.record(
            &TraceLine::new("call-window", "ended")
                .call(self.call.get())
                .kind(ended.reason(self.core.me()).name())
                .kind(offers),
        );
        self.close_later(cx);
    }

    /// The end closes by itself after [`AUTO_CLOSE`].
    fn close_later(&mut self, cx: &mut Context<Self>) {
        self.closing = Some(cx.spawn(async move |this, cx: &mut AsyncApp| {
            cx.background_executor().timer(AUTO_CLOSE).await;
            // a stage that is gone has nothing left to close
            let _ = this.update(cx, |this, cx| {
                this.closing = None;
                cx.emit(CallStageEvent::Close);
            });
        }));
    }

    /// How this window's call ended, when the window shows it as its end: one the user didn't choose, not a test
    /// call's (whose results it shows).
    fn end_card(&self, cx: &App) -> Option<Ended> {
        self.ended(cx)
            .filter(|ended| ended.check.is_none() && ended.stays_in_window())
    }

    /// What the end offers: Rejoin for a meeting's call, whatever ended it (a meeting never offers Call back), and for
    /// a group call that runs on without the account (it dropped, never got in, or the chat's call was running already)
    /// and that an invitation named; else Call back to its chat, or for a 1:1 invitation to the caller's chat; nothing
    /// in the read-only mode or for a test call.
    fn follow_up(&self, ended: &Ended, cx: &App) -> Option<FollowUp> {
        if self.core.read_only() || ended.kind == CallKind::TestCall {
            return None;
        }
        let reason = ended.reason(self.core.me());
        if reason == EndReason::Own {
            return None;
        }
        if ended.kind == CallKind::Meeting {
            // a meeting can be gone into at any time, whatever became of its call
            let ask = self.calls.read(cx).rejoin_of(ended)?;
            return Some(FollowUp { ask, rejoin: true });
        }
        if let Some(chat) = ended.group_chat() {
            let runs_on = matches!(
                reason,
                EndReason::Dropped | EndReason::NotConnected | EndReason::ChatHasCall
            );
            if runs_on && self.calls.read(cx).joinable(chat) {
                let ask = Ask::Join { chat: chat.to_owned() };
                return Some(FollowUp { ask, rejoin: true });
            }
            if reason == EndReason::ChatHasCall {
                return None;
            }
        }
        let chat = ended.call_back_chat(self.core.me())?;
        Some(FollowUp {
            ask: Ask::Call { chat },
            rejoin: false,
        })
    }

    /// Call back or Rejoin, as the end offers it (its button or its key); nothing while the call is up or another
    /// rings or sets up.
    fn act_on_end(&mut self, rejoin: bool, cx: &mut Context<Self>) {
        if self.calls.read(cx).busy() || self.others.iter().any(|calls| calls.read(cx).busy()) {
            return;
        }
        let Some(ended) = self.end_card(cx) else { return };
        let Some(follow_up) = self
            .follow_up(&ended, cx)
            .filter(|follow_up| follow_up.rejoin == rejoin)
        else {
            return;
        };
        // the user acted: the end no longer closes by itself
        self.closing = None;
        self.ask(follow_up.ask, cx);
    }

    /// The names of the devices the calls use now, in [`RESULT_DEVICES`]' order.
    fn devices_in_use(&self, cx: &App) -> [String; 3] {
        let model = self.settings.read(cx);
        RESULT_DEVICES.map(|kind| settings_view::device_in_use(kind, model).unwrap_or_else(|| SYSTEM_DEVICE.to_owned()))
    }

    /// Asks `ask` of the calls, as a button does.
    fn ask(&self, ask: Ask, cx: &mut Context<Self>) {
        calls::ask(&self.calls, &self.core, &self.trace, ask, cx);
    }

    /// Asks `ask` of the calls from a part of the stage drawn elsewhere (the lobby's card).
    pub(crate) fn ask_call(&self, ask: Ask, cx: &mut Context<Self>) {
        self.ask(ask, cx);
    }

    /// Hands the captions shown, and the call's title and people, to the translation.
    fn translate(&self, live: Option<&ActiveCall>, cx: &mut Context<Self>) {
        let calls = self.calls.read(cx);
        let captions = calls.captions();
        // captions that run only for the saved transcript (their pane hidden) aren't translated
        let showing = live.is_some()
            && calls.captions_shown()
            && matches!(
                captions.phase,
                teams_core::CaptionPhase::On | teams_core::CaptionPhase::Starting
            );
        let lines = captions.lines.clone();
        let brief = live.map(|active| self.brief(active, cx)).unwrap_or_default();
        self.translation
            .update(cx, |translation, cx| translation.follow(&lines, showing, brief, cx));
    }

    /// The call's title and who is in it, for the translation's context.
    fn brief(&self, active: &ActiveCall, cx: &App) -> String {
        let mut brief = vec![format!("Title: {}", self.title(active, cx))];
        if let Some(title) = active.title.as_deref().filter(|title| !title.trim().is_empty()) {
            brief.push(format!("Meeting: {title}"));
        }
        let names = call_view::names_in_call(active, self.people.of(active.id));
        if !names.is_empty() {
            let names: Vec<&str> = names.iter().map(SharedString::as_ref).collect();
            brief.push(format!("In the call: {}", names.join(", ")));
        }
        brief.join("\n")
    }

    /// Who the call is with, as the window's title says it.
    fn title(&self, active: &ActiveCall, cx: &App) -> SharedString {
        let names = call_view::names_in_call(active, self.people.of(active.id));
        let joined = || {
            let names: Vec<&str> = names.iter().map(SharedString::as_ref).collect();
            if names.is_empty() {
                SharedString::from("Group call")
            } else {
                SharedString::from(names.join(", "))
            }
        };
        call_view::call_title(active, |chat| {
            self.namer
                .as_ref()
                .and_then(|namer| namer(chat, cx))
                .unwrap_or_else(joined)
        })
    }

    /// Sets the window's title to `title`, when it changed.
    fn retitle(&mut self, title: &SharedString, window: &mut Window) {
        if self.titled.as_ref() != Some(title) {
            window.set_window_title(title.as_ref());
            self.titled = Some(title.clone());
        }
    }
}

impl CallStage {
    /// Traces the grid's cells when the grid of `call` changed: each participant's MRI, camera, line and speaking, and
    /// the account's own speaking and whether its microphone hears anything (a closing `self` line).
    fn trace_grid(&mut self, call: CallId, view: &CallView) {
        if self.grid_traced.as_ref() == Some(view) {
            return;
        }
        for participant in &view.participants {
            let line = TraceLine::new("call-participant", "shown")
                .call(call.get())
                .people([participant.mri.as_str()])
                .kind(if participant.video_on {
                    "camera-on"
                } else {
                    "camera-off"
                })
                .kind(participant.line.map_or("no-line", |line| Tile::Line(line).name()))
                .kind(if participant.speaking { "speaking" } else { "quiet" });
            self.trace.record(&line);
        }
        self.trace.record(
            &TraceLine::new("call-participant", "self")
                .call(call.get())
                .kind(if view.speaking { "speaking" } else { "quiet" })
                .kind(if view.hearing { "mic-sound" } else { "mic-silent" }),
        );
        self.grid_traced = Some(view.clone());
    }

    /// The video effects' pane while it is open: its title and Close over the picker, which learns whether the call's
    /// camera is on.
    fn effects_pane(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.effects.clone().filter(|_| self.pane == Pane::Effects)?;
        picker.update(cx, |picker, cx| picker.set_camera_on(active.camera, cx));
        let close = selected_by(
            "call-effects-close",
            icon_button("call-effects-close", Glyph::X, "Close video effects")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_effects(false, cx))),
        );
        Some(
            people_pane::side_pane("call-effects-pane", "Video effects", close, palette)
                .child(
                    div()
                        .id("call-effects-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_3()
                        .pb_3()
                        .child(picker),
                )
                .into_any_element(),
        )
    }

    /// A test call's results, as Teams shows them in its call window once it ended: a row per check (the microphone,
    /// the speaker and the camera, each naming the device the call used, and the network), "Device settings" and
    /// Close.
    fn results_card(&self, ended: &Ended, check: DeviceCheck, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        // the devices as they were when the call ended: a device chosen since wasn't checked
        let named = self.results.clone().unwrap_or_else(|| self.devices_in_use(cx));
        let verdicts = [check.microphone(), check.speaker(), check.camera()];
        let mut checks: Vec<AnyElement> = [Glyph::MIC, Glyph::VOLUME, Glyph::VIDEO]
            .into_iter()
            .zip(RESULT_DEVICES.into_iter().zip(named))
            .zip(verdicts)
            .map(|((glyph, (kind, device)), verdict)| check_row((glyph, device_label(kind), device), verdict, palette))
            .collect();
        checks.push(check_row(
            (Glyph::WIFI, "Network", "Your connection".to_owned()),
            check.network(),
            palette,
        ));
        let passed = check.passed();
        let summary = if passed {
            "Your devices are working.".to_owned()
        } else {
            calls::end_text(&ended.end, ended.voicemail)
        };
        let verdict = if passed { Verdict::Fine } else { Verdict::Problem };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .bg(rgb(palette.background))
            .child(
                div()
                    .w(px(RESULTS_WIDTH))
                    .max_w_full()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_6()
                    .rounded_lg()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .child(results_header(verdict, summary, palette))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(palette.border))
                            .children(checks),
                    )
                    .child(Self::results_buttons(cx)),
            )
            .into_any_element()
    }

    /// The results' "Device settings" and Close.
    fn results_buttons(cx: &mut Context<Self>) -> gpui::Div {
        let device_settings = selected_by(
            "test-call-device-settings",
            Button::new("test-call-device-settings")
                .label("Device settings")
                .outline()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::DeviceSettings))),
        );
        let close = selected_by(
            "test-call-close",
            Button::new("test-call-close")
                .label("Close")
                .primary()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::Close))),
        );
        div().flex().justify_end().gap_2().child(device_settings).child(close)
    }

    /// Why the last ask went nowhere while the call is up.
    fn refused(&self, palette: Palette, cx: &App) -> Option<gpui::Div> {
        let refused = self.calls.read(cx).refused()?;
        Some(
            div()
                .debug_selector(|| "call-window-refused".to_owned())
                .flex_none()
                .px_4()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .bg(rgb(palette.surface))
                .border_b_1()
                .border_color(rgb(palette.border))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(14.0)).text_color(rgb(theme::DANGER)))
                .child(SharedString::from(call_view::refusal_text(refused))),
        )
    }

    /// What the window shows under its header: the stage (and the participants' pane), a test call's results, or that
    /// the call ended.
    fn body(
        &mut self,
        (live, ended): (Option<&ActiveCall>, Option<&Ended>),
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(active) = live {
            let stage = self.stage(active, title, palette, cx);
            let stage = self.with_meeting(stage, active, palette, cx);
            let roster = (self.pane == Pane::People).then(|| self.roster_pane(active, palette, cx));
            let chat = self.chat_pane(active, palette, cx);
            let effects = self.effects_pane(active, palette, cx);
            let info = self.meeting_info_pane(active, title, palette, cx);
            let health = self.health_pane(palette, cx);
            let transcript = self.transcript_pane(active, palette, cx);
            return div()
                .size_full()
                .flex()
                .child(stage)
                .children(roster)
                .children(chat)
                .children(effects)
                .children(info)
                .children(health)
                .children(transcript)
                .into_any_element();
        }
        match (ended, ended.and_then(|ended| ended.check)) {
            (Some(ended), Some(check)) => self.results_card(ended, check, palette, cx),
            (Some(ended), None) => self.end_view(ended, title, palette, cx),
            (None, _) => calling((title, self.partner_picture(None)), "Call ended", palette, Vec::new()),
        }
    }

    /// How the call ended, as the window shows it once it did: who it was with, why it ended, how long it lasted,
    /// Call back or Rejoin, and Close.
    fn end_view(&self, ended: &Ended, title: &SharedString, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let group = ended.group_chat().is_some();
        let reason = match ended.reason(self.core.me()) {
            // a 1:1 call's far end goes by the name the app shows them by (the window's title), not the one its
            // client sent with the end
            EndReason::EndedBy(_) if !group => EndReason::EndedBy(None),
            reason => reason,
        };
        // a group call's end that names nobody isn't the chat's doing
        let other = if group { SOMEONE } else { title.as_ref() };
        let why = reason.text(other).map(|why| {
            div()
                .debug_selector(|| "call-end-reason".to_owned())
                .text_color(rgb(palette.foreground))
                .child(SharedString::from(why))
        });
        let lasted = ended.duration.map(|lasted| {
            div()
                .debug_selector(|| "call-end-duration".to_owned())
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(format!(
                    "Duration {}",
                    calls::duration_text(lasted.as_secs())
                )))
        });
        let follow_up = self
            .follow_up(ended, cx)
            .filter(|_| !self.calls.read(cx).busy())
            .map(|follow_up| follow_up_button(follow_up.rejoin, cx));
        let close = selected_by(
            "call-end-close",
            Button::new("call-end-close")
                .label("Close")
                .outline()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CallStageEvent::Close))),
        );
        div()
            .debug_selector(|| "call-end".to_owned())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .p_4()
            .bg(rgb(palette.background))
            .child(ended_avatar(title, self.partner_picture(None), palette))
            .child(
                div()
                    .mt_2()
                    .max_w_full()
                    .truncate()
                    .text_xl()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.clone()),
            )
            .child(
                div()
                    .debug_selector(|| "call-end-heading".to_owned())
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(reason.heading()),
            )
            .children(why)
            .children(lasted)
            .child(
                div()
                    .mt_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .children(follow_up)
                    .child(close),
            )
            .into_any_element()
    }

    /// Who the call was with, as its end names them: as last said while it was up, else as the end knows them.
    fn ended_title(&self, ended: &Ended, cx: &App) -> SharedString {
        if let Some(with) = &self.with {
            return with.clone();
        }
        let chat_name = |chat: &str| self.namer.as_ref().and_then(|namer| namer(chat, cx));
        match (ended.kind, &ended.chat) {
            (CallKind::Incoming, None) => ended.caller_name.clone().map(SharedString::from),
            (_, Some(chat)) => chat_name(chat),
            _ => None,
        }
        .or_else(|| {
            let names: Vec<&str> = self.people.last().iter().map(|(_, name)| name.as_ref()).collect();
            (!names.is_empty()).then(|| SharedString::from(names.join(", ")))
        })
        .unwrap_or_else(|| SharedString::from("Call"))
    }
}

impl Render for CallStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let live = self.live(cx);
        let ended = self.ended(cx);
        let title = match (&live, &ended) {
            (Some(active), _) => {
                let title = self.title(active, cx);
                self.with = Some(title.clone());
                title
            }
            (None, Some(ended)) if ended.check.is_some() => SharedString::from("Test call"),
            (None, Some(ended)) => self.ended_title(ended, cx),
            (None, None) => SharedString::from("Call"),
        };
        self.retitle(&title, window);
        self.follow_invite_box(window, cx);
        let viewport = window.viewport_size();
        self.paging.window = (f32::from(viewport.width), f32::from(viewport.height));
        let body = self.body((live.as_ref(), ended.as_ref()), &title, palette, cx);
        let header = self.header(live.as_ref(), title, palette, cx);
        let mut captions = live.as_ref().and_then(|_| self.caption_pane(palette, cx));
        // a refusal while the call is up, or of a call back from its end
        let refused = (live.is_some() || ended.as_ref().is_some_and(|ended| ended.check.is_none()))
            .then(|| self.refused(palette, cx))
            .flatten();
        // the stage and what goes with it on a sheet floating on the frame, under the header
        let panes = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(palette.background))
            .children(refused)
            .children(
                live.as_ref()
                    .and_then(|active| gallery::connection_banner(active, palette)),
            )
            .children(if self.caption_top { captions.take() } else { None })
            .child(div().flex_1().min_h(px(MIN_STAGE_HEIGHT)).overflow_hidden().child(body))
            .children(captions);
        let confirmation = self.end_confirmation(palette, cx);
        let lock = self.lock_confirmation(palette, cx);
        let record = live.as_ref().and_then(|_| self.record_dialog_view(palette, cx));
        let stop_record = live
            .as_ref()
            .and_then(|_| self.stop_recording_confirmation(palette, cx));
        let dialog = live.as_ref().and_then(|_| self.transfer_dialog(palette, cx));
        let mute_all = live.as_ref().and_then(|_| self.mute_all_confirmation(palette, cx));
        let sheet = crate::sheet::float(
            div().flex_1().min_h_0().w_full(),
            panes,
            palette,
            window,
            (0.0, HEADER_HEIGHT),
            true,
        );
        div()
            .debug_selector(|| "call-window".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleMuteCall, _, cx| this.ask(Ask::ToggleMute, cx)))
            .on_action(cx.listener(|this, _: &ToggleCaptionsCall, _, cx| this.ask(Ask::ToggleCaptions, cx)))
            .on_action(cx.listener(|this, _: &ToggleCameraCall, _, cx| this.ask(Ask::ToggleCamera, cx)))
            .on_action(cx.listener(|this, _: &HangUpCall, _, cx| this.ask(Ask::HangUp, cx)))
            .on_action(cx.listener(|this, _: &ToggleHandCall, _, cx| this.toggle_hand(cx)))
            .on_action(cx.listener(|this, _: &OpenShareCall, window, cx| this.set_share_open(true, window, cx)))
            .on_action(cx.listener(|this, _: &CallChat, _, cx| this.act_on_end(false, cx)))
            .on_action(cx.listener(|this, _: &JoinCall, _, cx| this.act_on_end(true, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::frame(palette))
            .text_color(rgb(palette.foreground))
            .child(header)
            .child(sheet)
            .children(confirmation)
            .children(lock)
            .children(record)
            .children(stop_record)
            .children(dialog)
            .children(mute_all)
    }
}

/// What a test call's results name a device by while the devices aren't listed (or the system has no default).
const SYSTEM_DEVICE: &str = "The system's default";

/// The devices a test call's results name, in their rows' order.
const RESULT_DEVICES: [DeviceKind; 3] = [DeviceKind::Microphone, DeviceKind::Speaker, DeviceKind::Camera];

/// The results' card's width, px.
const RESULTS_WIDTH: f32 = 480.0;

/// The microphone's menu, in Teams' order: the speakers, then the microphones.
const AUDIO: &[DeviceKind] = &[DeviceKind::Speaker, DeviceKind::Microphone];

/// The camera's menu.
const CAMERAS: &[DeviceKind] = &[DeviceKind::Camera];

/// A new frame wakes the stage, which draws the newest one.
fn wake_on_frames(feed: &VideoFeed, cx: &mut Context<CallStage>) -> Task<()> {
    let woken = feed.woken();
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        while woken.recv().await.is_ok() {
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                break;
            }
        }
    })
}

/// The menus' Video effects: opens the video effects' pane.
fn effects_item(selector: &'static str, stage: WeakEntity<CallStage>) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .items_center()
            .gap_2()
            .child(Glyph::SPARKLES.icon().size(px(14.0)))
            .child("Video effects and settings")
    })
    .on_click(move |_, _, cx| {
        if let Some(stage) = stage.upgrade() {
            stage.update(cx, |stage, cx| stage.show_effects(true, cx));
        }
    })
}

/// Tells the stage's listeners `event`, while the stage is there.
fn emit(stage: &WeakEntity<CallStage>, event: CallStageEvent, cx: &mut App) {
    let _ = stage.update(cx, |_, cx| cx.emit(event));
}

/// The menu's rows of the devices of `kinds`: each kind's name, then its choices (a check on the one chosen), each
/// row's test selector `call-<kind>-<key>`; a choice goes to the settings as their own pickers set it.
pub(crate) fn device_items(
    mut menu: PopupMenu,
    kinds: &[DeviceKind],
    model: &Entity<SettingsModel>,
    cx: &App,
) -> PopupMenu {
    let settings = model.read(cx);
    let Some(list) = settings.devices() else {
        return menu;
    };
    for (index, kind) in kinds.iter().copied().enumerate() {
        if index > 0 {
            menu = menu.separator();
        }
        menu = menu.label(device_label(kind));
        for choice in settings_view::device_choices(kind, settings.settings(), list) {
            menu = menu.item(device_item(kind, choice, model));
        }
    }
    menu.scrollable(true).max_h(px(MENU_HEIGHT)).min_w(px(260.0))
}

/// The tallest a device menu gets before it scrolls, px.
const MENU_HEIGHT: f32 = 360.0;

/// One device's row of a menu, its test selector `call-<kind>-<key>`.
fn device_item(kind: DeviceKind, choice: DeviceChoice, model: &Entity<SettingsModel>) -> PopupMenuItem {
    let selector = format!("call-{}-{}", kind.key(), choice.key);
    let DeviceChoice {
        label, chosen, name, ..
    } = choice;
    let model = model.clone();
    PopupMenuItem::element(move |_, _| {
        let selector = selector.clone();
        div().debug_selector(move || selector.clone()).child(label.clone())
    })
    .checked(chosen)
    .on_click(move |_, _, cx| {
        let name = name.clone();
        model.update(cx, |model, cx| {
            model.change(|settings| settings.set_device(kind, name.clone()), cx);
        });
    })
}

/// The screen while the call rings (or once it ended): its avatar (the picture of the one it is with, when it has one),
/// who it is with and its state, centred.
fn calling(
    (title, picture): (&SharedString, Option<&people::Picture>),
    state: &str,
    palette: Palette,
    rings: Vec<AnyElement>,
) -> AnyElement {
    div()
        .debug_selector(|| "call-calling".to_owned())
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_3()
        // `rings` spreading from who is called, while it rings or waits
        .child(
            div()
                .relative()
                .children(rings)
                .child(
                    people::avatar(Some(title), picture, BIG_AVATAR).debug_selector(move || {
                        let shown = if picture.is_some() { "picture" } else { "initials" };
                        format!("call-calling-{shown}")
                    }),
                ),
        )
        .child(
            div()
                .text_xl()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.clone()),
        )
        .child(
            div()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(state.to_owned())),
        )
        .into_any_element()
}

/// The avatar of who an ended call was with (their picture, when it was with one person who has one), a red hung-up
/// mark on it.
fn ended_avatar(title: &SharedString, picture: Option<&people::Picture>, palette: Palette) -> gpui::Div {
    div()
        .relative()
        .child(
            people::avatar(Some(title), picture, BIG_AVATAR).debug_selector(move || {
                let shown = if picture.is_some() { "picture" } else { "initials" };
                format!("call-end-{shown}")
            }),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .right_0()
                .size(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_2()
                .border_color(rgb(palette.background))
                .bg(rgb(theme::DANGER))
                .text_color(rgb(0x00ff_ffff))
                .child(Glyph::PHONE_OFF.icon().size(px(16.0))),
        )
}

/// Call back (Teams' green) or Rejoin, with its key.
fn follow_up_button(rejoin: bool, cx: &mut Context<CallStage>) -> AnyElement {
    if rejoin {
        control(
            "call-end-rejoin",
            Control::Labelled(Glyph::PHONE, "Rejoin"),
            ("Rejoin the call", &JoinCall),
            Look::Success,
            cx,
            |this, cx| this.act_on_end(true, cx),
        )
    } else {
        control(
            "call-end-call-back",
            Control::Labelled(Glyph::PHONE, "Call back"),
            ("Call back", &CallChat),
            Look::Success,
            cx,
            |this, cx| this.act_on_end(false, cx),
        )
    }
}

/// A participant's name: the roster's, else the one the call's people give its MRI, else "Someone".
fn participant_name(participant: &CallParticipant, people: &[(String, SharedString)]) -> SharedString {
    participant
        .name
        .clone()
        .map(SharedString::from)
        .or_else(|| {
            people
                .iter()
                .find(|(mri, _)| *mri == participant.mri)
                .map(|(_, name)| name.clone())
        })
        .unwrap_or_else(|| SharedString::from(SOMEONE))
}

/// The results' heading: the verdict's mark, "Test call results" and what they say.
fn results_header(verdict: Verdict, summary: String, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "test-call-results".to_owned())
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(verdict_mark(verdict, 24.0, palette))
                .child(
                    div()
                        .text_xl()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child("Test call results"),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child(SharedString::from(summary)),
        )
}

/// A verdict's mark: Teams' green check, its red alert, or a muted dash.
fn verdict_mark(verdict: Verdict, size: f32, palette: Palette) -> AnyElement {
    let (glyph, color) = match verdict {
        Verdict::Fine => (Glyph::CIRCLE_CHECK, theme::SUCCESS),
        Verdict::Problem => (Glyph::CIRCLE_ALERT, theme::DANGER),
        Verdict::Untried => (Glyph::CIRCLE_MINUS, palette.muted_foreground),
    };
    glyph.icon().size(px(size)).text_color(rgb(color)).into_any_element()
}

/// One check of a test call's results, its test selector `test-call-check-<name>`: its device's glyph, its name and
/// the device, and on its right the verdict with its words.
fn check_row(
    (glyph, name, device): (Glyph, &'static str, String),
    (verdict, said): (Verdict, &'static str),
    palette: Palette,
) -> AnyElement {
    let selector = format!("test-call-check-{}", name.to_lowercase());
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .py_2p5()
        .border_b_1()
        .border_color(rgb(palette.border))
        .child(glyph.icon().size(px(18.0)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).child(name))
                .child(
                    div()
                        .truncate()
                        .text_xs()
                        .text_color(rgb(palette.muted_foreground))
                        .child(SharedString::from(device)),
                ),
        )
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .child(verdict_mark(verdict, 14.0, palette))
                .child(said),
        )
        .into_any_element()
}
