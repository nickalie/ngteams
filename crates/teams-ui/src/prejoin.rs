//! A meeting's pre-join ([`PrejoinView`]), as Teams shows one before it goes into a meeting: the meeting's title, the
//! account's camera preview with its switch, its device and its background filters, the audio to use (Teams' choices:
//! Computer audio with the microphone's switch and the microphone and the speaker, Phone audio, which Teams offers
//! greyed out to an account without dial-in, and Don't use audio, which joins muted), then Cancel and Join now.
//! Nothing has gone out while it shows: Join now is what goes into the meeting's call ([`calls::join_now`]), starting
//! it when nobody has. Opened by Accept on the meeting's Request to join, its Join now answers that ring while it rings,
//! and its Cancel leaves the ring to ring on.
//!
//! - **The camera** starts off, and only the switch (the user's click) turns it on, for the preview alone
//!   ([`CameraPreview`]): nothing is sent. Whatever the pre-join's switches say at Join now, the call turns them into
//!   the user's mute and camera once it can take them ([`calls::Calls::next_choice`]).
//! - **The background filters** are the call window's video effects: the picker beside the preview, its own preview
//!   taking over the camera while it is open.
//! - **The keys**: the call's own (Ctrl+Shift+M, Ctrl+Shift+O), Enter for Join now and Escape for Cancel.
//!
//! The view's trace has `prejoin shown` (with the chat), `prejoin camera` and `prejoin microphone` (on, off),
//! `prejoin audio` (computer, none), and
//! `prejoin closed` (cancel) or the `call asked` of its join, never a name.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, AsyncApp, Context, Entity, FocusHandle, InteractiveElement as _,
    IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px, rgb,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenu};
use gpui_component::switch::Switch;
use gpui_component::{Selectable as _, Sizable as _};
use teams_media::devices::catalog::DeviceKind;

use crate::bridge::Core;
use crate::call_stage::device_items;
use crate::calls::{self, Calls, Prejoin};
use crate::effects::{EffectsPicker, PickerPlace, PreviewSession};
use crate::icons::{Glyph, Spinning as _, icon_button, selected_by};
use crate::settings::SettingsModel;
use crate::settings_view;
use crate::shell::{ToggleCameraCall, ToggleMuteCall};
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::video::{Tile, Tiles, VideoFeed};

/// The view's key context.
pub const CONTEXT: &str = "Prejoin";

/// The preview's height, px.
const PREVIEW_HEIGHT: f32 = 300.0;
/// The audio box's width, px.
const AUDIO_WIDTH: f32 = 340.0;
/// The video effects' pane's width, px.
const EFFECTS_WIDTH: f32 = 280.0;

/// What the view is made of: the account's engine (its request side), its calls, the app's settings and the view trace.
pub struct PrejoinParts {
    pub core: Arc<dyn Core>,
    pub calls: Entity<Calls>,
    pub settings: Entity<SettingsModel>,
    pub trace: Arc<ViewTrace>,
}

/// The pre-join (see the module docs). A GPUI entity, the window's root view.
pub struct PrejoinView {
    core: Arc<dyn Core>,
    calls: Entity<Calls>,
    settings: Entity<SettingsModel>,
    trace: Arc<ViewTrace>,
    /// The tiles drawing the camera's preview, in a build with one.
    tiles: Option<Tiles>,
    /// The preview while the camera's switch is on and the filters' pane isn't taking the camera over.
    session: Option<PreviewSession>,
    /// The background filters' pane, while it is open.
    effects: Option<Entity<EffectsPicker>>,
    focus: FocusHandle,
    /// A Meet now's subject, editable as Teams' pre-join has it (made the first time the pre-join draws one).
    subject: Option<(Entity<InputState>, Subscription)>,
    _waking: Option<Task<()>>,
    _observing: [Subscription; 2],
}

impl PrejoinView {
    /// The pre-join over `parts`.
    pub fn new(parts: PrejoinParts, cx: &mut Context<Self>) -> Self {
        let PrejoinParts {
            core,
            calls,
            settings,
            trace,
        } = parts;
        let observing = [
            cx.observe(&calls, |_, _, cx| cx.notify()),
            cx.observe(&settings, |_, _, cx| cx.notify()),
        ];
        let feed = settings.read(cx).camera_preview().map(|preview| preview.feed().clone());
        let waking = feed.as_ref().map(|feed| wake_on_frames(feed, cx));
        let tiles = feed.map(|feed| Tiles::new(feed, Arc::new(ViewTrace::off())));
        let mut shown = TraceLine::new("prejoin", "shown");
        if let Some(chat) = calls.read(cx).prejoin().and_then(|prejoin| prejoin.to_join.chat()) {
            shown = shown.conversation(chat);
        }
        trace.record(&shown);
        Self {
            core,
            calls,
            settings,
            trace,
            tiles,
            session: None,
            effects: None,
            focus: cx.focus_handle(),
            subject: None,
            _waking: waking,
            _observing: observing,
        }
    }

    /// What takes the window's keyboard.
    #[must_use]
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Whether the camera shows its preview here.
    #[must_use]
    pub fn previewing(&self) -> bool {
        self.session.is_some()
    }

    /// The camera's switch: the preview follows it (the user's click, never by itself).
    pub fn set_camera(&mut self, on: bool, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("prejoin", "camera").kind(if on { "on" } else { "off" }));
        self.calls.update(cx, |calls, cx| calls.set_prejoin_camera(on, cx));
        self.follow_camera(cx);
        cx.notify();
    }

    /// The preview runs while the switch is on, unless the filters' pane took the camera over.
    fn follow_camera(&mut self, cx: &mut Context<Self>) {
        let wanted = self.calls.read(cx).prejoin().is_some_and(|prejoin| prejoin.camera);
        if wanted && self.effects.is_none() {
            if self.session.is_none() {
                self.session = self
                    .settings
                    .read(cx)
                    .camera_preview()
                    .map(crate::effects::CameraPreview::start);
            }
        } else {
            self.session = None;
        }
    }

    /// The microphone's switch.
    pub fn set_microphone(&mut self, on: bool, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("prejoin", "microphone").kind(if on { "on" } else { "off" }));
        self.calls.update(cx, |calls, cx| calls.set_prejoin_muted(!on, cx));
    }

    /// The audio to use: the computer's, or none ("Don't use audio": the call is joined muted).
    pub fn set_audio(&mut self, computer: bool, cx: &mut Context<Self>) {
        self.trace
            .record(&TraceLine::new("prejoin", "audio").kind(if computer { "computer" } else { "none" }));
        self.calls
            .update(cx, |calls, cx| calls.set_prejoin_no_audio(!computer, cx));
    }

    /// Background filters: the pane opens or closes.
    pub fn show_effects(&mut self, open: bool, cx: &mut Context<Self>) {
        self.effects = open.then(|| {
            let (settings, trace) = (self.settings.clone(), Arc::clone(&self.trace));
            cx.new(|cx| EffectsPicker::new(settings, trace, PickerPlace::CallPane, cx))
        });
        self.follow_camera(cx);
        cx.notify();
    }

    /// Join now: the choices stay with the call, the camera's preview gives way to it.
    pub fn join(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.effects = None;
        calls::join_now(&self.calls, &self.core, &self.trace, cx);
    }

    /// Cancel: nothing went out, and nothing will.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        self.session = None;
        self.trace.record(&TraceLine::new("prejoin", "closed"));
        self.calls.update(cx, Calls::cancel_prejoin);
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.join(cx),
            "escape" => self.cancel(cx),
            _ => {}
        }
    }

    fn toggle_microphone(&mut self, cx: &mut Context<Self>) {
        if let Some(muted) = self.calls.read(cx).prejoin().map(|prejoin| prejoin.muted) {
            self.set_microphone(muted, cx);
        }
    }

    fn toggle_camera(&mut self, cx: &mut Context<Self>) {
        if self.tiles.is_some()
            && let Some(camera) = self.calls.read(cx).prejoin().map(|prejoin| prejoin.camera)
        {
            self.set_camera(!camera, cx);
        }
    }

    /// The camera's preview, or that the camera is off.
    fn preview(&mut self, prejoin: &Prejoin, palette: Palette) -> AnyElement {
        let drawn = self.session.as_ref().and_then(|session| {
            session.seen();
            self.tiles.as_mut()?.fill(Tile::Own, 0)
        });
        let stopped = self.session.as_ref().is_some_and(|session| !session.running());
        let body = match drawn {
            Some(video) if !stopped => div().size_full().child(video).into_any_element(),
            _ => {
                let (glyph, words) = if stopped {
                    (Glyph::VIDEO_OFF, "Your camera isn't available")
                } else if prejoin.camera && self.effects.is_some() {
                    (Glyph::VIDEO, "Your camera is previewed in Video effects")
                } else if prejoin.camera {
                    (Glyph::VIDEO, "Starting your camera")
                } else {
                    (Glyph::VIDEO_OFF, "Your camera is turned off")
                };
                div()
                    .debug_selector(|| "prejoin-camera-off".to_owned())
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .text_color(rgb(palette.muted_foreground))
                    .child(glyph.icon().size(px(22.0)))
                    .child(words)
                    .into_any_element()
            }
        };
        div()
            .debug_selector(|| "prejoin-preview".to_owned())
            .h(px(PREVIEW_HEIGHT))
            .w_full()
            .overflow_hidden()
            .bg(rgb(palette.background))
            .child(body)
            .into_any_element()
    }

    /// Under the preview: the camera's switch and device, and the background filters.
    fn camera_row(&self, prejoin: &Prejoin, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let glyph = if prejoin.camera { Glyph::VIDEO } else { Glyph::VIDEO_OFF };
        let weak = cx.entity().downgrade();
        let switch =
            Switch::new("prejoin-camera")
                .checked(prejoin.camera)
                .on_click(move |on: &bool, _, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| this.set_camera(*on, cx));
                });
        let open = self.effects.is_some();
        div()
            .h(px(48.0))
            .px_3()
            .flex()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(glyph.icon().size(px(18.0)))
            .children(self.devices_menu("prejoin-camera-devices", CAMERAS, "Choose camera", cx))
            .child(selected_by("prejoin-camera", switch))
            .child(div().w(px(1.0)).h(px(24.0)).mx_2().bg(rgb(palette.border)))
            .child(selected_by(
                "prejoin-effects",
                Button::new("prejoin-effects")
                    .ghost()
                    .small()
                    .icon(Glyph::SPARKLES.icon())
                    .label("Background filters")
                    .selected(open)
                    .on_click(cx.listener(move |this, _, _, cx| this.show_effects(!open, cx))),
            ))
    }

    /// The audio's choices as Teams lists them: Computer audio (its devices while it is picked), Phone audio (greyed
    /// out: this app joins by no phone), Don't use audio.
    fn audio(&self, prejoin: &Prejoin, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let computer = prejoin.audio == calls::PrejoinAudio::Computer;
        let devices = computer.then(|| self.computer_audio(prejoin, palette, cx));
        let caption = (!computer).then(|| {
            div()
                .px_4()
                .pb_3()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("Join muted to avoid causing audio disruption.")
        });
        div()
            .debug_selector(|| "prejoin-audio".to_owned())
            .w(px(AUDIO_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                Self::audio_box(palette)
                    .child(Self::audio_choice(
                        "prejoin-audio-computer",
                        "Computer audio",
                        computer,
                        true,
                        palette,
                        cx,
                    ))
                    .children(devices),
            )
            .child(Self::audio_box(palette).child(Self::audio_choice(
                "prejoin-audio-phone",
                "Phone audio",
                false,
                false,
                palette,
                cx,
            )))
            .child(
                Self::audio_box(palette)
                    .child(Self::audio_choice(
                        "prejoin-audio-none",
                        "Don't use audio",
                        !computer,
                        true,
                        palette,
                        cx,
                    ))
                    .children(caption),
            )
    }

    /// `content` on the card everything sits on, floating on the frame (a sheet, as the main window's panes are),
    /// rising in as the window opens.
    fn card(content: gpui::Div, palette: Palette, cx: &App) -> gpui::Div {
        let card = div()
            .w_full()
            .max_w(px(948.0))
            .p_6()
            .rounded(px(theme::SHEET_RADIUS + 4.0))
            .bg(rgb(palette.background))
            .shadow(theme::glow(palette, 0x0000_0000, 0.35, 40.0, 12.0))
            .when(!palette.depth, |card| card.border_1().border_color(rgb(palette.border)))
            .child(content);
        div()
            .w_full()
            .flex()
            .justify_center()
            .child(crate::motion::rise(card, "prejoin-card", 16.0, cx))
    }

    /// A box of the audio's choices.
    fn audio_box(palette: Palette) -> gpui::Div {
        div()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .flex()
            .flex_col()
    }

    /// One of the audio's choices: a radio mark and its name; a choice not `enabled` is drawn greyed out and takes no
    /// click, as Teams draws Phone audio for an account without it.
    fn audio_choice(
        id: &'static str,
        name: &'static str,
        picked: bool,
        enabled: bool,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let mark = div()
            .size(px(16.0))
            .flex_none()
            .rounded_full()
            .border_1()
            .border_color(rgb(if picked {
                palette.brand
            } else {
                palette.muted_foreground
            }))
            .flex()
            .items_center()
            .justify_center()
            .children(picked.then(|| div().size(px(8.0)).rounded_full().bg(rgb(palette.brand))));
        let row = div()
            .debug_selector(move || id.to_owned())
            .id(id)
            .px_4()
            .py_3()
            .flex()
            .items_center()
            .gap_3()
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .child(mark)
            .child(name);
        if !enabled {
            return row.opacity(0.45).tooltip(|window, cx| {
                gpui_component::tooltip::Tooltip::new("Phone audio isn't available for your account").build(window, cx)
            });
        }
        let computer = id == "prejoin-audio-computer";
        row.cursor_pointer()
            .hover(|style| style.bg(rgb(palette.hover)))
            .on_click(cx.listener(move |this, _, _, cx| this.set_audio(computer, cx)))
    }

    /// The computer's audio: the microphone's device and switch, and the speaker's device.
    fn computer_audio(&self, prejoin: &Prejoin, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let weak = cx.entity().downgrade();
        let switch =
            Switch::new("prejoin-microphone")
                .checked(!prejoin.muted)
                .on_click(move |on: &bool, _, cx: &mut App| {
                    let _ = weak.update(cx, |this, cx| this.set_microphone(*on, cx));
                });
        let mic = if prejoin.muted { Glyph::MIC_OFF } else { Glyph::MIC };
        let named = |kind: DeviceKind, cx: &App| -> SharedString {
            let settings = self.settings.read(cx);
            settings
                .devices()
                .and_then(|list| {
                    settings_view::device_choices(kind, settings.settings(), list)
                        .into_iter()
                        .find(|choice| choice.chosen)
                })
                .map_or_else(|| SYSTEM_DEVICE.into(), |choice| choice.label)
        };
        let microphone = named(DeviceKind::Microphone, cx);
        let speaker = named(DeviceKind::Speaker, cx);
        div()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(rgb(palette.border))
            .child(
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(mic.icon().size(px(18.0)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(microphone))
                    .children(self.devices_menu("prejoin-microphone-devices", MICROPHONES, "Choose microphone", cx))
                    .child(selected_by("prejoin-microphone", switch)),
            )
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Glyph::VOLUME.icon().size(px(18.0)))
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(speaker))
                    .children(self.devices_menu("prejoin-speaker-devices", SPEAKERS, "Choose speaker", cx)),
            )
    }

    /// The menu of the devices of `kinds`, chosen as the settings choose them; none while the devices aren't listed.
    fn devices_menu(
        &self,
        id: &'static str,
        kinds: &'static [DeviceKind],
        tip: &'static str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        self.settings.read(cx).devices()?;
        let model = self.settings.clone();
        Some(
            selected_by(
                id,
                Button::new(id)
                    .ghost()
                    .xsmall()
                    .icon(Glyph::CHEVRON_DOWN.icon())
                    .tooltip(tip)
                    .dropdown_menu_with_anchor(Anchor::TopLeft, move |menu: PopupMenu, _, cx| {
                        device_items(menu, kinds, &model, cx)
                    }),
            )
            .into_any_element(),
        )
    }

    /// Cancel and Join now; while the join goes, that it does.
    fn actions(prejoin: &Prejoin, cx: &mut Context<Self>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_end()
            .gap_2()
            .child(selected_by(
                "prejoin-cancel",
                Button::new("prejoin-cancel")
                    .label("Cancel")
                    .outline()
                    .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
            ))
            .child(selected_by(
                "prejoin-join",
                Button::new("prejoin-join")
                    .label(if prejoin.joining { "Joining…" } else { "Join now" })
                    .primary()
                    .spinning(prejoin.joining)
                    .on_click(cx.listener(|this, _, _, cx| this.join(cx))),
            ))
    }

    /// The background filters' pane, while it is open.
    fn effects_pane(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let picker = self.effects.clone()?;
        Some(
            div()
                .debug_selector(|| "prejoin-effects-pane".to_owned())
                .w(px(EFFECTS_WIDTH))
                .flex_none()
                .h_full()
                .flex()
                .flex_col()
                .bg(rgb(palette.surface))
                .border_l_1()
                .border_color(rgb(palette.border))
                .child(
                    div()
                        .h(px(48.0))
                        .px_4()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_lg()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Video effects"),
                        )
                        .child(selected_by(
                            "prejoin-effects-close",
                            icon_button("prejoin-effects-close", Glyph::X, "Close video effects")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| this.show_effects(false, cx))),
                        )),
                )
                .child(
                    div()
                        .id("prejoin-effects-scroll")
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
}

impl PrejoinView {
    /// The pre-join's title: the meeting's, or for a Meet now Teams' heading over the new meeting's subject, which the
    /// user may change before Join now ([`Calls::set_prejoin_title`]).
    fn title(&mut self, prejoin: &Prejoin, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let heading = div()
            .debug_selector(|| "prejoin-title".to_owned())
            .text_center()
            .text_xl()
            .font_weight(gpui::FontWeight::SEMIBOLD);
        if prejoin.entry != calls::PrejoinEntry::MeetNow {
            return heading.child(SharedString::from(prejoin.to_join.title().to_owned()));
        }
        if self.subject.is_none() {
            let title = prejoin.to_join.title().to_owned();
            let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
            let calls = self.calls.clone();
            let changed = cx.subscribe(&input, move |_, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let title = input.read(cx).value().to_string();
                    calls.update(cx, |calls, cx| calls.set_prejoin_title(title, cx));
                }
            });
            self.subject = Some((input, changed));
        }
        let Some((input, _)) = &self.subject else {
            return heading.child(SharedString::from(prejoin.to_join.title().to_owned()));
        };
        let input = input.clone();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_2()
            .child(heading.child(MEET_NOW_HEADING))
            .child(
                div()
                    .debug_selector(|| "prejoin-subject".to_owned())
                    .w(px(320.0))
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .bg(rgb(palette.surface))
                    .child(Input::new(&input).appearance(false)),
            )
    }
}

/// What a Meet now's pre-join says over the new meeting's subject, as Teams' does.
const MEET_NOW_HEADING: &str = "Microsoft Teams meeting";

impl Render for PrejoinView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let Some(prejoin) = self.calls.read(cx).prejoin().cloned() else {
            return div().id("prejoin").size_full().bg(rgb(palette.background));
        };
        window.set_window_title(prejoin.to_join.title());
        let refused = self.calls.read(cx).refused().map(|refused| {
            div()
                .debug_selector(|| "prejoin-refused".to_owned())
                .px_3()
                .py_2()
                .rounded_md()
                .bg(rgb(crate::calendar_view::mix(palette.surface, theme::DANGER, 100)))
                .text_sm()
                .child(SharedString::from(refused.text.clone()))
        });
        let video = self.tiles.is_some();
        let camera = video.then(|| {
            div()
                .flex_1()
                .min_w_0()
                .rounded_lg()
                .overflow_hidden()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .child(self.preview(&prejoin, palette))
                .child(self.camera_row(&prejoin, palette, cx))
        });
        let actions = Self::actions(&prejoin, cx);
        let title = self.title(&prejoin, palette, window, cx);
        let audio = self.audio(&prejoin, palette, cx);
        let effects = self.effects_pane(palette, cx);
        div()
            .debug_selector(|| "prejoin".to_owned())
            .id("prejoin")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &ToggleMuteCall, _, cx| this.toggle_microphone(cx)))
            .on_action(cx.listener(|this, _: &ToggleCameraCall, _, cx| this.toggle_camera(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.key_down(event, cx)))
            .size_full()
            .flex()
            .bg(theme::frame(palette))
            .text_color(rgb(palette.foreground))
            .child(
                div()
                    .id("prejoin-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .p_6()
                    .child(Self::card(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(title)
                            .child(div().flex().items_start().gap_4().children(camera).child(audio))
                            .children(refused)
                            .child(actions),
                        palette,
                        cx,
                    )),
            )
            .children(effects)
    }
}

/// What names a device while the devices aren't listed.
const SYSTEM_DEVICE: &str = "The system's default";

/// The camera's menu.
const CAMERAS: &[DeviceKind] = &[DeviceKind::Camera];
/// The microphone's menu.
const MICROPHONES: &[DeviceKind] = &[DeviceKind::Microphone];
/// The speaker's menu.
const SPEAKERS: &[DeviceKind] = &[DeviceKind::Speaker];

/// A new frame wakes the view, which draws the newest one.
fn wake_on_frames(feed: &VideoFeed, cx: &mut Context<PrejoinView>) -> Task<()> {
    let woken = feed.woken();
    cx.spawn(async move |this, cx: &mut AsyncApp| {
        while woken.recv().await.is_ok() {
            if this.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
        }
    })
}
