//! The app's icons (Lucide's, ISC: `assets/icons/LICENSE-LUCIDE`), compiled in as SVG, and the icon-only button the
//! toolbars use: one icon on a quiet square, named by its tooltip (with its key, when it has one).

use gpui::{
    Action, ElementId, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _, div,
};
use gpui_component::Icon;
use gpui_component::button::{Button, ButtonVariants as _};

/// One of the app's icons.
#[derive(Clone, Copy)]
pub struct Glyph(&'static [u8]);

impl Glyph {
    pub const WORKFLOW: Self = Self(include_bytes!("../assets/icons/workflow.svg"));
    pub const ACCESSIBILITY: Self = Self(include_bytes!("../assets/icons/accessibility.svg"));
    pub const ALARM: Self = Self(include_bytes!("../assets/icons/alarm-clock.svg"));
    pub const BELL_OFF: Self = Self(include_bytes!("../assets/icons/bell-off.svg"));
    pub const TIMER: Self = Self(include_bytes!("../assets/icons/timer.svg"));
    pub const ACTIVITY: Self = Self(include_bytes!("../assets/icons/activity.svg"));
    pub const ARROW_DOWN: Self = Self(include_bytes!("../assets/icons/arrow-down.svg"));
    /// Back (a profile card's "Back to {name}").
    pub const ARROW_LEFT: Self = Self(include_bytes!("../assets/icons/arrow-left.svg"));
    pub const ARROW_RIGHT: Self = Self(include_bytes!("../assets/icons/arrow-right.svg"));
    pub const ARROW_UP: Self = Self(include_bytes!("../assets/icons/arrow-up.svg"));
    pub const BAN: Self = Self(include_bytes!("../assets/icons/ban.svg"));
    /// A message a policy flagged.
    pub const FLAG: Self = Self(include_bytes!("../assets/icons/flag.svg"));
    /// A message a policy warns of (a harmful link, phishing).
    pub const WARNING: Self = Self(include_bytes!("../assets/icons/triangle-alert.svg"));
    pub const STAR: Self = Self(include_bytes!("../assets/icons/star.svg"));
    pub const MAIL: Self = Self(include_bytes!("../assets/icons/mail.svg"));
    pub const BELL: Self = Self(include_bytes!("../assets/icons/bell.svg"));
    pub const BOT: Self = Self(include_bytes!("../assets/icons/bot.svg"));
    pub const BUG: Self = Self(include_bytes!("../assets/icons/bug.svg"));
    pub const CALENDAR: Self = Self(include_bytes!("../assets/icons/calendar.svg"));
    pub const CALENDAR_ADD: Self = Self(include_bytes!("../assets/icons/calendar-plus.svg"));
    pub const CALENDAR_CANCEL: Self = Self(include_bytes!("../assets/icons/calendar-x.svg"));
    pub const CALENDAR_CLOCK: Self = Self(include_bytes!("../assets/icons/calendar-clock.svg"));
    pub const CALENDAR_DAYS: Self = Self(include_bytes!("../assets/icons/calendar-days.svg"));
    pub const CALENDAR_RANGE: Self = Self(include_bytes!("../assets/icons/calendar-range.svg"));
    pub const CALENDAR_SEARCH: Self = Self(include_bytes!("../assets/icons/calendar-search.svg"));
    pub const CALENDAR_TODAY: Self = Self(include_bytes!("../assets/icons/calendar-1.svg"));
    pub const ADD_CHANNEL: Self = Self(include_bytes!("../assets/icons/folder-plus.svg"));
    pub const ARCHIVE: Self = Self(include_bytes!("../assets/icons/archive.svg"));
    pub const ARCHIVE_RESTORE: Self = Self(include_bytes!("../assets/icons/archive-restore.svg"));
    pub const ELLIPSIS: Self = Self(include_bytes!("../assets/icons/ellipsis.svg"));
    pub const EYE: Self = Self(include_bytes!("../assets/icons/eye.svg"));
    pub const EYE_OFF: Self = Self(include_bytes!("../assets/icons/eye-off.svg"));
    pub const GLOBE: Self = Self(include_bytes!("../assets/icons/globe.svg"));
    /// A chat's translation (Teams' "aあ" mark beside a translated message's time).
    pub const LANGUAGES: Self = Self(include_bytes!("../assets/icons/languages.svg"));
    /// The general channel in a team's list of channels.
    pub const HOUSE: Self = Self(include_bytes!("../assets/icons/house.svg"));
    pub const UPLOAD: Self = Self(include_bytes!("../assets/icons/arrow-up-from-line.svg"));
    pub const USER_PLUS: Self = Self(include_bytes!("../assets/icons/user-round-plus.svg"));
    pub const CAPTIONS: Self = Self(include_bytes!("../assets/icons/captions.svg"));
    pub const CHECK: Self = Self(include_bytes!("../assets/icons/check.svg"));
    pub const IMAGE: Self = Self(include_bytes!("../assets/icons/image.svg"));
    pub const IMAGE_PLUS: Self = Self(include_bytes!("../assets/icons/image-plus.svg"));
    pub const LOCK: Self = Self(include_bytes!("../assets/icons/lock.svg"));
    pub const LOCK_OPEN: Self = Self(include_bytes!("../assets/icons/lock-open.svg"));
    pub const NOTES: Self = Self(include_bytes!("../assets/icons/notebook-text.svg"));
    /// A call's transcript (Show transcript, a transcription's notices).
    pub const CALL_TRANSCRIPT: Self = Self(include_bytes!("../assets/icons/square-text.svg"));
    /// Start transcription, and the call's header while it transcribes.
    pub const TRANSCRIBE: Self = Self(include_bytes!("../assets/icons/file-pen-line.svg"));
    pub const PAPERCLIP: Self = Self(include_bytes!("../assets/icons/paperclip.svg"));
    pub const PRINTER: Self = Self(include_bytes!("../assets/icons/printer.svg"));
    pub const SAVE: Self = Self(include_bytes!("../assets/icons/save.svg"));
    pub const SUN: Self = Self(include_bytes!("../assets/icons/sun.svg"));
    pub const TAG: Self = Self(include_bytes!("../assets/icons/tag.svg"));
    pub const TEXT_FORMAT: Self = Self(include_bytes!("../assets/icons/type.svg"));
    pub const USER_COG: Self = Self(include_bytes!("../assets/icons/user-cog.svg"));
    // the message box's formats
    pub const BOLD: Self = Self(include_bytes!("../assets/icons/bold.svg"));
    pub const ITALIC: Self = Self(include_bytes!("../assets/icons/italic.svg"));
    pub const UNDERLINE: Self = Self(include_bytes!("../assets/icons/underline.svg"));
    pub const STRIKETHROUGH: Self = Self(include_bytes!("../assets/icons/strikethrough.svg"));
    pub const LIST: Self = Self(include_bytes!("../assets/icons/list.svg"));
    pub const FOLDER: Self = Self(include_bytes!("../assets/icons/folder.svg"));
    pub const OPEN_OUT: Self = Self(include_bytes!("../assets/icons/square-arrow-out-up-right.svg"));
    pub const LIST_ORDERED: Self = Self(include_bytes!("../assets/icons/list-ordered.svg"));
    pub const HIGHLIGHTER: Self = Self(include_bytes!("../assets/icons/highlighter.svg"));
    pub const FONT_COLOUR: Self = Self(include_bytes!("../assets/icons/baseline.svg"));
    pub const FONT_SIZE: Self = Self(include_bytes!("../assets/icons/a-large-small.svg"));
    pub const PILCROW: Self = Self(include_bytes!("../assets/icons/pilcrow.svg"));
    pub const HEADING_1: Self = Self(include_bytes!("../assets/icons/heading-1.svg"));
    pub const HEADING_2: Self = Self(include_bytes!("../assets/icons/heading-2.svg"));
    pub const HEADING_3: Self = Self(include_bytes!("../assets/icons/heading-3.svg"));
    pub const INDENT: Self = Self(include_bytes!("../assets/icons/list-indent-increase.svg"));
    pub const OUTDENT: Self = Self(include_bytes!("../assets/icons/list-indent-decrease.svg"));
    pub const REMOVE_FORMATTING: Self = Self(include_bytes!("../assets/icons/remove-formatting.svg"));
    pub const UNDO: Self = Self(include_bytes!("../assets/icons/undo-2.svg"));
    pub const REDO: Self = Self(include_bytes!("../assets/icons/redo-2.svg"));
    pub const TABLE: Self = Self(include_bytes!("../assets/icons/table.svg"));
    /// A ticked box (a calendar shown in the calendar's list).
    pub const CHECKBOX: Self = Self(include_bytes!("../assets/icons/square-check.svg"));
    pub const CHEVRON_DOWN: Self = Self(include_bytes!("../assets/icons/chevron-down.svg"));
    pub const CHEVRON_LEFT: Self = Self(include_bytes!("../assets/icons/chevron-left.svg"));
    pub const CHEVRON_RIGHT: Self = Self(include_bytes!("../assets/icons/chevron-right.svg"));
    pub const CHEVRON_UP: Self = Self(include_bytes!("../assets/icons/chevron-up.svg"));
    pub const CIRCLE_ALERT: Self = Self(include_bytes!("../assets/icons/circle-alert.svg"));
    pub const CIRCLE_CHECK: Self = Self(include_bytes!("../assets/icons/circle-check.svg"));
    pub const CIRCLE_MINUS: Self = Self(include_bytes!("../assets/icons/circle-minus.svg"));
    pub const RECORD: Self = Self(include_bytes!("../assets/icons/circle-dot.svg"));
    pub const RECORD_STOP: Self = Self(include_bytes!("../assets/icons/circle-stop.svg"));
    pub const CIRCLE_QUESTION: Self = Self(include_bytes!("../assets/icons/circle-question-mark.svg"));
    pub const CIRCLE_X: Self = Self(include_bytes!("../assets/icons/circle-x.svg"));
    pub const LIST_FILTER: Self = Self(include_bytes!("../assets/icons/list-filter.svg"));
    pub const CLOCK: Self = Self(include_bytes!("../assets/icons/clock.svg"));
    pub const CLOUD_UPLOAD: Self = Self(include_bytes!("../assets/icons/cloud-upload.svg"));
    pub const CODE: Self = Self(include_bytes!("../assets/icons/square-code.svg"));
    pub const COPY: Self = Self(include_bytes!("../assets/icons/copy.svg"));
    pub const COLUMNS: Self = Self(include_bytes!("../assets/icons/columns-2.svg"));
    pub const DOWNLOAD: Self = Self(include_bytes!("../assets/icons/download.svg"));
    pub const EMOJI: Self = Self(include_bytes!("../assets/icons/face-slightly-smiling.svg"));
    /// A picture at its own size (the picture viewer's "Actual size").
    pub const EXPAND: Self = Self(include_bytes!("../assets/icons/expand.svg"));
    pub const EXTERNAL_LINK: Self = Self(include_bytes!("../assets/icons/external-link.svg"));
    pub const FULLSCREEN: Self = Self(include_bytes!("../assets/icons/scan.svg"));
    pub const FILE: Self = Self(include_bytes!("../assets/icons/file.svg"));
    pub const FILE_ARCHIVE: Self = Self(include_bytes!("../assets/icons/file-archive.svg"));
    pub const FILE_AUDIO: Self = Self(include_bytes!("../assets/icons/file-headphone.svg"));
    pub const FILE_CHART: Self = Self(include_bytes!("../assets/icons/file-chart-column.svg"));
    pub const FILE_IMAGE: Self = Self(include_bytes!("../assets/icons/file-image.svg"));
    pub const FILE_SPREADSHEET: Self = Self(include_bytes!("../assets/icons/file-spreadsheet.svg"));
    pub const FILE_VIDEO: Self = Self(include_bytes!("../assets/icons/file-play.svg"));
    pub const FORWARD: Self = Self(include_bytes!("../assets/icons/forward.svg"));
    pub const HASH: Self = Self(include_bytes!("../assets/icons/hash.svg"));
    pub const HEADPHONES: Self = Self(include_bytes!("../assets/icons/headphones.svg"));
    pub const INFO: Self = Self(include_bytes!("../assets/icons/info.svg"));
    pub const LINK: Self = Self(include_bytes!("../assets/icons/link.svg"));
    pub const LOADING: Self = Self(include_bytes!("../assets/icons/loader-circle.svg"));
    pub const LOG_OUT: Self = Self(include_bytes!("../assets/icons/log-out.svg"));
    pub const MAP_PIN: Self = Self(include_bytes!("../assets/icons/map-pin.svg"));
    /// A profile card's Chat, Teams' round chat bubble.
    pub const CHAT: Self = Self(include_bytes!("../assets/icons/message-circle-more.svg"));
    /// A profile card's org chart.
    pub const ORG_CHART: Self = Self(include_bytes!("../assets/icons/network.svg"));
    /// A job title.
    pub const BRIEFCASE: Self = Self(include_bytes!("../assets/icons/briefcase.svg"));
    pub const MESSAGE: Self = Self(include_bytes!("../assets/icons/message-square-text.svg"));
    /// Teams' two speech bubbles: a channel post's "Reply in thread".
    pub const THREAD: Self = Self(include_bytes!("../assets/icons/messages-square.svg"));
    pub const MIC: Self = Self(include_bytes!("../assets/icons/mic.svg"));
    pub const MIC_OFF: Self = Self(include_bytes!("../assets/icons/mic-off.svg"));
    pub const MINUS: Self = Self(include_bytes!("../assets/icons/minus.svg"));
    pub const MORE: Self = Self(include_bytes!("../assets/icons/ellipsis.svg"));
    /// The app launcher's grid of dots (the title bar's left corner).
    pub const GRIP: Self = Self(include_bytes!("../assets/icons/grip.svg"));
    /// A call's Raise hand, and a raised hand on a tile or a row.
    pub const HAND: Self = Self(include_bytes!("../assets/icons/hand.svg"));
    /// A call's gallery view.
    pub const GALLERY: Self = Self(include_bytes!("../assets/icons/layout-grid.svg"));
    /// A call's speaker view.
    pub const SPEAKER_VIEW: Self = Self(include_bytes!("../assets/icons/layout-panel-top.svg"));
    /// View's Focus on content: the share alone.
    pub const FOCUS_CONTENT: Self = Self(include_bytes!("../assets/icons/layout-template.svg"));
    /// A call's Share (Teams' box with its arrow up).
    pub const SCREEN_SHARE: Self = Self(include_bytes!("../assets/icons/square-arrow-up.svg"));
    /// A screen, window or tab to share.
    pub const MONITOR: Self = Self(include_bytes!("../assets/icons/monitor.svg"));
    pub const PIN: Self = Self(include_bytes!("../assets/icons/pin.svg"));
    pub const PIN_OFF: Self = Self(include_bytes!("../assets/icons/pin-off.svg"));
    pub const SEARCH: Self = Self(include_bytes!("../assets/icons/search.svg"));
    pub const PALETTE: Self = Self(include_bytes!("../assets/icons/palette.svg"));
    pub const PANEL_LEFT: Self = Self(include_bytes!("../assets/icons/panel-left.svg"));
    pub const PAUSE: Self = Self(include_bytes!("../assets/icons/pause.svg"));
    pub const PENCIL: Self = Self(include_bytes!("../assets/icons/pencil.svg"));
    /// Teams' New message: a pen over a page.
    pub const NEW_MESSAGE: Self = Self(include_bytes!("../assets/icons/square-pen.svg"));
    pub const PHONE: Self = Self(include_bytes!("../assets/icons/phone.svg"));
    pub const PHONE_FORWARDED: Self = Self(include_bytes!("../assets/icons/phone-forwarded.svg"));
    pub const PHONE_OFF: Self = Self(include_bytes!("../assets/icons/phone-off.svg"));
    pub const PHONE_MISSED: Self = Self(include_bytes!("../assets/icons/phone-missed.svg"));
    pub const PLAY: Self = Self(include_bytes!("../assets/icons/circle-play.svg"));
    pub const PLAY_MEDIA: Self = Self(include_bytes!("../assets/icons/play.svg"));
    pub const PLUS: Self = Self(include_bytes!("../assets/icons/plus.svg"));
    pub const POWER: Self = Self(include_bytes!("../assets/icons/power.svg"));
    pub const POP_OUT: Self = Self(include_bytes!("../assets/icons/square-arrow-out-up-right.svg"));
    pub const QUOTE: Self = Self(include_bytes!("../assets/icons/quote.svg"));
    pub const REACT: Self = Self(include_bytes!("../assets/icons/smile-plus.svg"));
    pub const REFRESH: Self = Self(include_bytes!("../assets/icons/refresh-cw.svg"));
    /// A recurring event; [`Self::REPEAT_OFF`] a cancelled one of a series.
    pub const REPEAT: Self = Self(include_bytes!("../assets/icons/repeat.svg"));
    pub const REPEAT_OFF: Self = Self(include_bytes!("../assets/icons/repeat-off.svg"));
    pub const REPLY: Self = Self(include_bytes!("../assets/icons/reply.svg"));
    pub const RESET: Self = Self(include_bytes!("../assets/icons/rotate-ccw.svg"));
    pub const SEND: Self = Self(include_bytes!("../assets/icons/send-horizontal.svg"));
    pub const SETTINGS: Self = Self(include_bytes!("../assets/icons/settings.svg"));
    pub const SHARE: Self = Self(include_bytes!("../assets/icons/share.svg"));
    pub const SPARKLES: Self = Self(include_bytes!("../assets/icons/sparkles.svg"));
    pub const SPELL_CHECK: Self = Self(include_bytes!("../assets/icons/spell-check.svg"));
    pub const SHIELD: Self = Self(include_bytes!("../assets/icons/shield.svg"));
    pub const SHRINK: Self = Self(include_bytes!("../assets/icons/shrink.svg"));
    pub const SKIP_BACK: Self = Self(include_bytes!("../assets/icons/rotate-ccw.svg"));
    pub const SKIP_FORWARD: Self = Self(include_bytes!("../assets/icons/rotate-cw.svg"));
    pub const TRANSCRIPT: Self = Self(include_bytes!("../assets/icons/file-text.svg"));
    pub const TRASH: Self = Self(include_bytes!("../assets/icons/trash.svg"));
    pub const USER: Self = Self(include_bytes!("../assets/icons/user-round.svg"));
    pub const USER_LEFT: Self = Self(include_bytes!("../assets/icons/user-round-arrow-left.svg"));
    pub const USER_MINUS: Self = Self(include_bytes!("../assets/icons/user-round-minus.svg"));
    pub const UNVERIFIED: Self = Self(include_bytes!("../assets/icons/badge-question-mark.svg"));
    /// Everyone in a chat (the mention picker's Everyone).
    pub const USERS: Self = Self(include_bytes!("../assets/icons/users-round.svg"));
    pub const VIDEO: Self = Self(include_bytes!("../assets/icons/video.svg"));
    pub const VIDEO_OFF: Self = Self(include_bytes!("../assets/icons/video-off.svg"));
    pub const VOLUME: Self = Self(include_bytes!("../assets/icons/volume-2.svg"));
    pub const VOLUME_OFF: Self = Self(include_bytes!("../assets/icons/volume-x.svg"));
    pub const WIFI: Self = Self(include_bytes!("../assets/icons/wifi.svg"));
    pub const X: Self = Self(include_bytes!("../assets/icons/x.svg"));
    pub const ZOOM_IN: Self = Self(include_bytes!("../assets/icons/zoom-in.svg"));
    pub const ZOOM_OUT: Self = Self(include_bytes!("../assets/icons/zoom-out.svg"));

    /// The icon, in the text colour around it.
    #[must_use]
    pub fn icon(self) -> Icon {
        Icon::default().data(self.0)
    }
}

/// An icon-only button: `glyph` on a ghost square, `tip` its tooltip (and what it is called). The caller adds its
/// click, its variant when it isn't a quiet one, and its size.
#[must_use]
pub fn icon_button(id: impl Into<ElementId>, glyph: Glyph, tip: impl Into<SharedString>) -> Button {
    let tip = tip.into();
    Button::new(id)
        .icon(glyph.icon())
        .ghost()
        .accessibility_label(tip.clone())
        .tooltip(tip)
}

/// [`icon_button`] whose tooltip also shows the key bound to `action`.
#[must_use]
pub fn icon_button_with_key(
    id: impl Into<ElementId>,
    glyph: Glyph,
    tip: impl Into<SharedString>,
    action: &dyn Action,
) -> Button {
    let tip = tip.into();
    Button::new(id)
        .icon(glyph.icon())
        .ghost()
        .accessibility_label(tip.clone())
        .tooltip_with_action(tip, action, None)
}

/// A button that shows its work: the kit draws a loading button's spinner only in its icon's place, so a button with
/// a label alone would only dim.
pub trait Spinning {
    /// `loading`, with the kit's spinner (Teams' ring, `AppAssets`) before the label while it lasts.
    #[must_use]
    fn spinning(self, loading: bool) -> Self;
}

impl Spinning for Button {
    fn spinning(self, loading: bool) -> Self {
        let button = if loading {
            self.icon(Glyph::LOADING.icon())
        } else {
            self
        };
        button.loading(loading)
    }
}

/// `element` with `selector` as its test selector: the kit's buttons take none of their own.
pub fn selected_by(selector: impl Into<String>, element: impl IntoElement) -> gpui::Div {
    let selector = selector.into();
    div()
        .debug_selector(move || selector.clone())
        .flex_none()
        .child(element)
}
