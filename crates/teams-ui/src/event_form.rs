//! The scheduling form ("New event - Calendar"), in a window of its own as Teams opens it: a new event from the
//! calendar's New (or its menu, or a double click on an empty slot of the grid), or an event of the account's opened
//! to change (its card's Edit, or a double click on it).
//!
//! - **Its toolbar**: Event and Series (a new event repeats while Series is on; a change of an occurrence edits that
//!   occurrence, or the whole series once Series is picked: its master read and shown), Show as, the reminder, the
//!   categories, private, Print ([`print`]: the event's page in the desktop's browser, which prints it) and Save (Send
//!   while it invites anyone, Send update for a meeting's change); on a change also Cancel meeting (occurrence, series:
//!   the organizer's meeting with attendees, with a note for them) or Delete.
//! - **Its form**: the title; the required attendees and, by the attendee options, the optional ones (people the
//!   account knows by name and people the directory finds, picked from the suggestions, or any address typed in
//!   full); the date and times (a start and an end every half hour, all day), the suggested times and the Scheduler;
//!   for a series its recurrence ([`repeat`]: Teams' presets, a custom one, its end, its summary); the location; the
//!   Microsoft Teams meeting switch; the text, in the app's rich-text editor ([`crate::editor`]) with Teams' format bar
//!   (Format) and emoji, sent as the HTML the editor writes (an event's own text goes back as it came while it isn't
//!   edited).
//! - **Its day**: the date's hours beside the form, the new event's block in green over the day's events, moved by a
//!   drag (its top and bottom handles change its start and end) or a click on an empty half hour. The Scheduler
//!   ([`scheduler`]) takes its place: everyone's busy times in a column each, and the times all of them are free.
//! - **Save** goes through the engine (`teams_core::calendar`): one typed write through the outbox; the window closes once it
//!   went through and the calendar reads its days again, and stays open saying why when it didn't.
//! - **Read-only** (`Core::read_only`): the form shows the event and offers no Save, Cancel or Delete.
//! - **Traced**: `event-form opened` (`new` or `change`), `event-form saved` / `not-saved` with the write's kind.

mod body;
mod channel;
mod day;
pub(crate) mod model;
pub(crate) mod people;
pub mod print;
pub mod repeat;
mod scheduler;
pub mod zones;

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate};
use gpui::{
    AnyElement, App, AppContext as _, AsyncApp, Bounds, Context, Entity, FocusHandle, Focusable as _, Global,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, TitlebarOptions, WeakEntity, Window,
    WindowBounds, WindowHandle, WindowOptions, div, prelude::FluentBuilder as _, px, rgb, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::date_picker::{DatePickerEvent, DatePickerState};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::switch::Switch;
use gpui_component::{Disableable as _, Selectable as _, Sizable as _};
use serde_json::{Map, Value};
use teams_core::{CalendarEvent, EventAttendee, EventKind, EventShowAs, EventWrite, FullEvent, Invitable};

use crate::bridge::Core;
use crate::calendar::{self, Shown};
use crate::calendar_view::mix;
use crate::clock;
use crate::editor::toolbar::FormatBar;
use crate::editor::{Editor, EditorEvent, EnterMode};
use crate::icons::{Glyph, icon_button, selected_by};
use crate::reaction_picker::ReactionPicker;
use crate::theme::{self, Palette};
use crate::trace::{TraceLine, ViewTrace};
use crate::window::KitRoot;

pub use channel::MeetingPlace;
pub use model::Slot;
use model::{CATEGORIES, Fields, REMINDERS};
use repeat::{Custom, Repeat};

/// How the window opens, px.
const DEFAULT_SIZE: (f32, f32) = (1400.0, 900.0);
const MIN_SIZE: (f32, f32) = (900.0, 640.0);

/// What the form opens on.
#[derive(Clone, Debug)]
pub enum Seed {
    /// A new event in `slot`, a Teams meeting when `online`.
    New { slot: Slot, online: bool },
    /// An event of the account's, read whole, to change.
    Change(Box<FullEvent>),
    /// A new meeting in a channel, in `slot` (the channel header's Schedule a meeting): a Teams meeting on its team's
    /// calendar, posted to the channel.
    InChannel { slot: Slot, place: MeetingPlace },
    /// A channel's meeting, read whole from its team's calendar, to change (its card's Edit, its calendar card's Edit):
    /// Send update tells everyone on the team, Cancel meeting cancels it there.
    ChangeInChannel { full: Box<FullEvent>, place: MeetingPlace },
}

/// What a write that went through tells the calendar: what it touched ([`teams_core::Touched`]), so no copy of it is
/// shown as it was.
#[derive(Clone)]
pub struct Saved(Rc<OnSaved>);

/// What [`Saved`] calls.
type OnSaved = dyn Fn(&teams_core::Touched, &mut App);

impl Saved {
    #[must_use]
    pub fn new(saved: impl Fn(&teams_core::Touched, &mut App) + 'static) -> Self {
        Self(Rc::new(saved))
    }

    /// Nothing to tell (a test's form).
    #[must_use]
    pub fn nothing() -> Self {
        Self::new(|_, _| {})
    }
}

/// The app id the form's window opens with: the main window's ([`set_app_id`]).
struct AppId(String);

impl Global for AppId {}

/// Opens the form's windows with app id `app_id` (the main window's) from now on.
pub fn set_app_id(app_id: &str, cx: &mut App) {
    cx.set_global(AppId(app_id.to_owned()));
}

/// The form's window, while it is open.
struct Open {
    window: WindowHandle<KitRoot>,
    form: WeakEntity<EventForm>,
}

impl Global for Open {}

/// Opens the form on `seed` in a window of its own, once the update under way is over; an open form's window is
/// brought to the front instead (one form at a time, as one event is edited at a time).
pub fn open(core: Arc<dyn Core>, trace: Arc<ViewTrace>, seed: Seed, saved: Saved, cx: &mut App) {
    cx.defer(move |cx| {
        if let Some((window, _)) = alive(cx) {
            let _ = window.update(cx, |_, window, _| window.activate_window());
            return;
        }
        let title: SharedString = match &seed {
            Seed::New { .. } => "New event - Calendar".into(),
            Seed::InChannel { .. } => "New meeting - Calendar".into(),
            Seed::Change(full) | Seed::ChangeInChannel { full, .. } if !full.event.subject.trim().is_empty() => {
                format!("{} - Calendar", full.event.subject.trim()).into()
            }
            Seed::Change(_) | Seed::ChangeInChannel { .. } => "Event - Calendar".into(),
        };
        let bounds = Bounds::centered(None, size(px(DEFAULT_SIZE.0), px(DEFAULT_SIZE.1)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                ..TitlebarOptions::default()
            }),
            app_id: cx.try_global::<AppId>().map(|id| id.0.clone()),
            window_min_size: Some(size(px(MIN_SIZE.0), px(MIN_SIZE.1))),
            ..WindowOptions::default()
        };
        let mut made = None;
        let opened = cx.open_window(options, |window, cx| {
            let form = cx.new(|cx| EventForm::new(core, trace, seed, saved, window, cx));
            made = Some(form.downgrade());
            cx.new(|cx| KitRoot::new(form, window, cx))
        });
        if let (Ok(window), Some(form)) = (opened, made) {
            cx.set_global(Open { window, form });
        }
    });
}

/// The form, while its window is open.
#[must_use]
pub fn shown(cx: &App) -> Option<Entity<EventForm>> {
    alive(cx).map(|(_, form)| form)
}

/// The form's window, while it is open.
#[must_use]
pub fn window(cx: &App) -> Option<WindowHandle<KitRoot>> {
    alive(cx).map(|(window, _)| window)
}

/// Closes the form's window, if it is open (nothing is saved).
pub fn close(cx: &mut App) {
    let Some((window, _)) = alive(cx) else { return };
    cx.defer(move |cx| {
        let _ = window.update(cx, |_, window, _| window.remove_window());
    });
}

fn alive(cx: &App) -> Option<(WindowHandle<KitRoot>, Entity<EventForm>)> {
    let open = cx.try_global::<Open>()?;
    let id = open.window.window_id();
    if !cx.windows().iter().any(|window| window.window_id() == id) {
        return None;
    }
    Some((open.window, open.form.upgrade()?))
}

/// The event a change edits.
#[derive(Clone, Debug)]
struct Editing {
    id: String,
    /// The event as read: a change sends it back with the form's fields over it.
    raw: Map<String, Value>,
    event: CalendarEvent,
}

/// What the form's Event and Series stand for.
#[derive(Clone, Debug)]
enum Series {
    /// A new event: Event, or Series while it repeats ([`EventForm::repeat`]).
    New,
    /// A change of an event of its own, which doesn't repeat.
    Single,
    /// A change of an occurrence of the series `master_id`: the occurrence, or the whole series while `whole` (its
    /// master read into the form); the occurrence as read kept to go back to.
    Occurrence {
        occurrence: Box<FullEvent>,
        master_id: String,
        whole: bool,
    },
    /// A change of a series' master itself.
    Master,
}

/// Which list of attendees an attendee box adds to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Required,
    Optional,
}

/// What asks the user to confirm before it is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Cancel,
    Delete,
}

/// What shows beside the form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The event's day.
    Day,
    /// The Scheduler: everyone's busy times.
    Scheduler,
}

/// A drag of the new event's block in the day beside the form: what moves, from where, and the slot it began on.
#[derive(Clone, Copy, Debug)]
struct Drag {
    part: day::Part,
    from: Pixels,
    slot: Slot,
}

/// The scheduling form (see the module docs).
#[expect(
    clippy::struct_excessive_bools,
    reason = "the form's own switches (Format shown, text changed, optional shown) beside the event's online and private"
)]
pub struct EventForm {
    core: Arc<dyn Core>,
    trace: Arc<ViewTrace>,
    focus: FocusHandle,
    editing: Option<Editing>,
    /// The channel a new meeting is scheduled in, and whether its people get invitations of their own too.
    place: Option<MeetingPlace>,
    personal_invites: bool,
    series: Series,
    /// How the event repeats, while it is a series.
    repeat: Option<Repeat>,
    /// The custom recurrence dialog, while it is open.
    custom: Option<Custom>,
    /// The series' end beside the repeat drop-down, and the custom dialog's.
    until_picker: Entity<DatePickerState>,
    custom_until_picker: Entity<DatePickerState>,
    title: Entity<InputState>,
    required: Entity<InputState>,
    optional: Entity<InputState>,
    location: Entity<InputState>,
    /// The text, in the app's rich-text editor, and its format bar (shown by Format).
    body: Entity<Editor>,
    format_bar: Entity<FormatBar>,
    formatting: bool,
    /// The event's own text (HTML) as it was read: sent back as it came while the editor isn't changed.
    body_seed: Option<String>,
    body_changed: bool,
    /// The emoji picker over the text, while it is open, and where its button was last drawn.
    emoji: Option<(Entity<ReactionPicker>, Subscription)>,
    emoji_at: Rc<Cell<Option<Bounds<Pixels>>>>,
    cancel_note: Entity<InputState>,
    attendees: Vec<EventAttendee>,
    optional_shown: bool,
    slot: Slot,
    /// The start's and the end's time zones, while the globe shows them.
    zones: Option<zones::Zones>,
    /// The two zone pickers, once the globe has shown them.
    zone_pickers: Option<zones::ZonePickers>,
    online: bool,
    show_as: EventShowAs,
    reminder: Option<u32>,
    private: bool,
    categories: Vec<String>,
    /// The suggestions under an attendee box, for that box.
    suggestions: Option<(Need, Vec<Invitable>)>,
    /// What the box said when the suggestions were found: Enter takes the first only while it still says that.
    suggested_for: String,
    suggesting: Option<Task<()>>,
    /// The day beside the form, and its events once read.
    day: NaiveDate,
    day_events: Rc<Vec<Shown>>,
    day_read: Option<Task<()>>,
    day_scroll: ScrollHandle,
    /// The day or the Scheduler, and what the Scheduler read.
    side: Side,
    availability: scheduler::Read,
    availability_key: Option<scheduler::ReadKey>,
    availability_read: Option<Task<()>>,
    /// A series' master being read, as Series is picked on an occurrence.
    series_read: Option<Task<()>>,
    drag: Option<Drag>,
    confirm: Option<Confirm>,
    saving: Option<Task<()>>,
    problem: Option<SharedString>,
    saved: Saved,
    _subscriptions: Vec<Subscription>,
}

impl EventForm {
    /// The form on `seed`, its title focused.
    pub fn new(
        core: Arc<dyn Core>,
        trace: Arc<ViewTrace>,
        seed: Seed,
        saved: Saved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        let title = input("Add title", window, cx);
        let required = input("Invite required attendees", window, cx);
        let optional = input("Invite optional attendees", window, cx);
        let location = input("Add a room or location", window, cx);
        let cancel_note = input("Add a message for the attendees (optional)", window, cx);
        let body = Self::body_editor(core.read_only(), window, cx);
        let format_bar = cx.new(|cx| FormatBar::new(body.clone(), cx).without_discard());
        let (until_picker, custom_until_picker) = (Self::date_picker(window, cx), Self::date_picker(window, cx));
        let subscriptions = Self::subscriptions(
            [&required, &optional],
            &body,
            [&until_picker, &custom_until_picker],
            window,
            cx,
        );
        let now = calendar::now().naive_local();
        let mut form = Self {
            core,
            trace,
            focus: cx.focus_handle(),
            editing: None,
            place: None,
            personal_invites: false,
            series: Series::New,
            repeat: None,
            custom: None,
            until_picker,
            custom_until_picker,
            title,
            required,
            optional,
            location,
            body,
            format_bar,
            formatting: false,
            body_seed: None,
            body_changed: false,
            emoji: None,
            emoji_at: Rc::default(),
            cancel_note,
            attendees: Vec::new(),
            optional_shown: false,
            slot: Slot::next_half_hour(now),
            zones: None,
            zone_pickers: None,
            online: false,
            show_as: EventShowAs::Busy,
            reminder: Some(15),
            private: false,
            categories: Vec::new(),
            suggestions: None,
            suggested_for: String::new(),
            suggesting: None,
            day: now.date(),
            day_events: Rc::default(),
            day_read: None,
            day_scroll: ScrollHandle::new(),
            side: Side::Day,
            availability: scheduler::Read::None,
            availability_key: None,
            availability_read: None,
            series_read: None,
            drag: None,
            confirm: None,
            saving: None,
            problem: None,
            saved,
            _subscriptions: subscriptions,
        };
        let kind = form.seed(seed, window, cx);
        form.day = form.slot.start.date();
        form.day_scroll
            .set_offset(gpui::point(px(0.0), px(-day::scroll_to(form.slot.start.time()))));
        form.trace.record(&TraceLine::new("event-form", "opened").kind(kind));
        form.read_day(cx);
        form.title.update(cx, |title, cx| title.focus(window, cx));
        form
    }

    /// The event's text editor: Enter a new line, at least 160px high; read-only in the read-only mode.
    fn body_editor(read_only: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<Editor> {
        let body = cx.new(|cx| Editor::new(window, cx).placeholder("Type details for this new meeting"));
        body.update(cx, |editor, cx| {
            editor.set_enter_mode(EnterMode::NewLine, cx);
            editor.set_heights(Some(px(160.0)), None, cx);
            editor.set_read_only(read_only, cx);
        });
        body
    }

    /// A date picker of the form's (the series' end, the custom dialog's).
    fn date_picker(window: &mut Window, cx: &mut Context<Self>) -> Entity<DatePickerState> {
        cx.new(|cx| DatePickerState::new(window, cx).date_format("%a %-m/%-d/%Y"))
    }

    /// What the form listens to: the attendee boxes, the text, and the series' two end pickers.
    fn subscriptions(
        boxes: [&Entity<InputState>; 2],
        body: &Entity<Editor>,
        pickers: [&Entity<DatePickerState>; 2],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Subscription> {
        let [required, optional] = boxes;
        let [until, custom_until] = pickers;
        vec![
            cx.subscribe_in(
                required,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    this.attendee_input(Need::Required, event, window, cx);
                },
            ),
            cx.subscribe_in(
                optional,
                window,
                |this: &mut Self, _, event: &InputEvent, window, cx| {
                    this.attendee_input(Need::Optional, event, window, cx);
                },
            ),
            cx.subscribe_in(body, window, |this: &mut Self, _, event: &EditorEvent, _, cx| {
                if *event == EditorEvent::Changed {
                    this.body_changed = true;
                }
                cx.notify();
            }),
            cx.subscribe_in(until, window, |this: &mut Self, _, event: &DatePickerEvent, _, cx| {
                let DatePickerEvent::Change(picked) = event;
                let start = this.series_day();
                if let Some(repeat) = &mut this.repeat {
                    repeat.until = repeat::picked_until(picked.start().map(|time| time.date()), start);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                custom_until,
                window,
                |this: &mut Self, _, event: &DatePickerEvent, _, cx| {
                    let DatePickerEvent::Change(picked) = event;
                    let start = this.series_day();
                    if let Some(custom) = &mut this.custom {
                        custom.until = repeat::picked_until(picked.start().map(|time| time.date()), start);
                    }
                    cx.notify();
                },
            ),
        ]
    }

    /// Opens the form on `seed`; what it is, for the trace.
    fn seed(&mut self, seed: Seed, window: &mut Window, cx: &mut Context<Self>) -> &'static str {
        match seed {
            Seed::New { slot, online } => {
                self.slot = slot;
                self.online = online;
                "new"
            }
            Seed::InChannel { slot, place } => {
                self.slot = slot;
                self.online = true;
                self.place = Some(place);
                "channel"
            }
            Seed::ChangeInChannel { full, place } => {
                self.personal_invites = full.raw.get("inviteAllMembers").and_then(Value::as_bool) == Some(true);
                self.fill(*full, false, window, cx);
                self.online = true;
                self.place = Some(place);
                "channel-change"
            }
            Seed::Change(full) => {
                let event = &full.event;
                self.series = match (&event.kind, &event.series_master_id) {
                    (EventKind::Occurrence | EventKind::Exception, Some(master_id)) => Series::Occurrence {
                        occurrence: full.clone(),
                        master_id: master_id.clone(),
                        whole: false,
                    },
                    (EventKind::RecurringMaster, _) => Series::Master,
                    _ => Series::Single,
                };
                let repeats = matches!(self.series, Series::Master);
                self.fill(*full, repeats, window, cx);
                "change"
            }
        }
    }

    /// Fills the form from an event read whole; `repeats` takes its recurrence too (a series changed whole).
    fn fill(&mut self, full: FullEvent, repeats: bool, window: &mut Window, cx: &mut Context<Self>) {
        let event = full.event.clone();
        if let Some(shown) = Shown::of(Rc::new(event.clone())) {
            self.slot = Slot {
                start: shown.start,
                end: shown.end,
                all_day: event.all_day,
            };
        }
        // its own zone, as Outlook's form opens it: its times shown there, a series' weekday that zone's
        self.zones = zones::Zones::of_event(&full.raw, event.all_day);
        self.online = event.online;
        self.show_as = match &event.show_as {
            teams_core::ShowAs::Free => EventShowAs::Free,
            teams_core::ShowAs::Tentative => EventShowAs::Tentative,
            teams_core::ShowAs::Away => EventShowAs::Away,
            teams_core::ShowAs::WorkingElsewhere => EventShowAs::WorkingElsewhere,
            teams_core::ShowAs::Busy | teams_core::ShowAs::Other(_) => EventShowAs::Busy,
        };
        self.reminder = full.reminder_minutes;
        self.private = event.private;
        self.categories.clone_from(&full.categories);
        self.attendees = invited(&full);
        self.optional_shown = self.attendees.iter().any(|attendee| !attendee.required);
        self.repeat = full.recurrence.as_ref().filter(|_| repeats).map(Repeat::of);
        let until = self.repeat.as_ref().and_then(|repeat| repeat.until);
        self.until_picker.update(cx, |picker, cx| {
            picker.set_date(gpui_component::calendar::Date::Single(until), window, cx);
        });
        self.load_body(full.body_html.as_deref(), cx);
        let subject = event.subject.clone();
        let location = event.location.clone().unwrap_or_default();
        self.title.update(cx, |input, cx| input.set_value(subject, window, cx));
        self.location
            .update(cx, |input, cx| input.set_value(location, window, cx));
        self.editing = Some(Editing {
            id: event.id.clone(),
            raw: full.raw,
            event,
        });
    }

    /// The slot the event takes.
    #[must_use]
    pub fn slot(&self) -> Slot {
        self.slot
    }

    /// Whether it is a Teams meeting.
    #[must_use]
    pub fn online(&self) -> bool {
        self.online
    }

    /// The channel a new meeting is scheduled in, when it is.
    #[must_use]
    pub fn place(&self) -> Option<&MeetingPlace> {
        self.place.as_ref()
    }

    /// Whether everyone on the channel's team gets an invitation of their own too.
    #[must_use]
    pub fn personal_invites(&self) -> bool {
        self.personal_invites
    }

    /// Whom it invites.
    #[must_use]
    pub fn attendees(&self) -> &[EventAttendee] {
        &self.attendees
    }

    /// Whether it changes an event that exists.
    #[must_use]
    pub fn changes(&self) -> bool {
        self.editing.is_some()
    }

    /// What the form holds.
    fn fields(&self, cx: &App) -> Fields {
        let body_html = if self.body_changed {
            self.body_html(cx)
        } else {
            self.body_seed.clone().unwrap_or_else(|| self.body_html(cx))
        };
        let start = self.series_day();
        Fields {
            subject: self.title.read(cx).value().to_string(),
            slot: self.slot,
            // an all-day event's days are the desktop's (the globe is hidden then)
            zones: self.zones.filter(|_| !self.slot.all_day),
            attendees: self.attendees.clone(),
            location: self.location.read(cx).value().to_string(),
            body_html,
            online: self.online,
            show_as: self.show_as,
            reminder: self.reminder,
            private: self.private,
            categories: self.categories.clone(),
            recurrence: self
                .repeat
                .as_ref()
                .filter(|_| self.edits_series())
                .map(|repeat| repeat.recurrence(start, self.series_zone())),
        }
    }

    /// Moves the event to `slot`, and the day beside it to its date; a series' preset follows the new day (the start
    /// zone's).
    pub fn set_slot(&mut self, slot: Slot, cx: &mut Context<Self>) {
        let from = self.series_day();
        self.move_to(slot, from, cx);
    }

    /// Moves the event to `slot` as [`Self::set_slot`] does, a series having started on `from` before.
    fn move_to(&mut self, slot: Slot, from: NaiveDate, cx: &mut Context<Self>) {
        self.slot = slot;
        self.follow_series_day(from);
        if slot.start.date() != self.day {
            self.day = slot.start.date();
            self.read_day(cx);
        }
        self.read_side(cx);
        cx.notify();
    }

    /// Shows `date` beside the form (the event stays where it is).
    fn show_day(&mut self, date: NaiveDate, cx: &mut Context<Self>) {
        self.day = date;
        self.read_day(cx);
        self.read_side(cx);
        cx.notify();
    }

    /// Shows the day or the Scheduler beside the form.
    pub fn show_side(&mut self, side: Side, cx: &mut Context<Self>) {
        self.side = side;
        self.trace
            .record(&TraceLine::new("event-form", "side").kind(match side {
                Side::Day => "day",
                Side::Scheduler => "scheduler",
            }));
        self.read_side(cx);
        cx.notify();
    }

    /// What shows beside the form.
    #[must_use]
    pub fn side(&self) -> Side {
        self.side
    }

    /// The free and busy times are read again when what they are about changed, as the web client's form reads them
    /// while it names attendees (the suggested times) or shows the Scheduler.
    fn read_side(&mut self, cx: &mut Context<Self>) {
        if self.side == Side::Scheduler || !self.attendees.is_empty() {
            self.read_availability(cx);
        }
    }

    /// Reads the day beside the form's events.
    fn read_day(&mut self, cx: &mut Context<Self>) {
        let span = calendar::Span::of(self.day, calendar::Mode::Day);
        let (start, end) = span.window_ms();
        let reply = self.core.calendar(start, end);
        let day = self.day;
        self.day_read = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |form, cx| {
                if form.day != day {
                    return;
                }
                if let Ok(events) = answer {
                    let editing = form.editing.as_ref().map(|editing| editing.id.clone());
                    let mut shown: Vec<Shown> = events
                        .into_iter()
                        .filter(calendar::shows)
                        .filter(|event| Some(&event.id) != editing.as_ref())
                        .filter_map(|event| Shown::of(Rc::new(event)))
                        .collect();
                    shown.sort_by_key(|shown| shown.start);
                    form.day_events = Rc::new(shown);
                }
                cx.notify();
            });
        }));
    }

    /// An attendee box changed: suggest people whose names hold what it says; Enter takes the first suggestion or a
    /// whole address.
    fn attendee_input(&mut self, need: Need, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        let text = input.read(cx).value().to_string();
        match event {
            InputEvent::PressEnter { .. } => {
                // suggestions found for what the box said before belong to someone else
                let current = self.suggested_for == text.trim();
                let first = self
                    .suggestions
                    .as_ref()
                    .filter(|(for_need, _)| current && *for_need == need)
                    .and_then(|(_, found)| found.first().cloned());
                if let Some(person) = first {
                    self.invite_person(&person, need, window, cx);
                } else {
                    self.invite_typed(need, window, cx);
                }
            }
            InputEvent::Change => {
                let query = text.trim().to_owned();
                if query.chars().count() < 2 {
                    self.suggestions = None;
                    self.suggesting = None;
                    cx.notify();
                    return;
                }
                if query.ends_with([';', ',']) {
                    self.invite_typed(need, window, cx);
                    return;
                }
                self.suggest(need, query, cx);
            }
            _ => {}
        }
    }

    /// Invites the address an attendee box holds, when it is one; false keeps an invalid pending address visible.
    fn invite_typed(&mut self, need: Need, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        let text = input.read(cx).value().to_string();
        let accepted = match model::typed_attendee(&text, need == Need::Required) {
            Some(attendee) => {
                self.add_attendee(attendee, cx);
                input.update(cx, |input, cx| input.set_value("", window, cx));
                self.suggestions = None;
                self.problem = None;
                true
            }
            None if !text.trim().is_empty() => {
                self.problem = Some("Type a whole email address, or pick someone from the suggestions.".into());
                false
            }
            None => true,
        };
        cx.notify();
        accepted
    }

    /// Invites `person` from the suggestions.
    fn invite_person(&mut self, person: &Invitable, need: Need, window: &mut Window, cx: &mut Context<Self>) {
        self.add_attendee(
            EventAttendee {
                address: person.address.clone(),
                name: Some(person.name.clone()),
                mri: Some(person.mri.clone()).filter(|mri| !mri.is_empty()),
                required: need == Need::Required,
            },
            cx,
        );
        let input = match need {
            Need::Required => self.required.clone(),
            Need::Optional => self.optional.clone(),
        };
        input.update(cx, |input, cx| input.set_value("", window, cx));
        self.suggestions = None;
        self.problem = None;
        cx.notify();
    }

    fn add_attendee(&mut self, attendee: EventAttendee, cx: &mut Context<Self>) {
        let address = attendee.address.to_lowercase();
        self.attendees.retain(|known| known.address.to_lowercase() != address);
        self.attendees.push(attendee);
        self.read_side(cx);
    }

    /// Fills the form in as a person typing would: `subject`, each of `invited` (address, name, required), a Teams
    /// meeting with `text` (the UI tour's step).
    pub fn fill_in(
        &mut self,
        subject: &str,
        invited: &[(&str, &str, bool)],
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let subject = subject.to_owned();
        self.title.update(cx, |input, cx| input.set_value(subject, window, cx));
        for (address, name, required) in invited {
            self.add_attendee(
                EventAttendee {
                    address: (*address).to_owned(),
                    name: Some((*name).to_owned()),
                    mri: None,
                    required: *required,
                },
                cx,
            );
            self.optional_shown |= !required;
        }
        let doc = crate::editor::doc::Doc::new(text.lines().map(crate::editor::doc::Block::paragraph).collect());
        self.body.update(cx, |editor, cx| editor.set_doc(doc, cx));
        self.body_changed = true;
        self.online = true;
        cx.notify();
    }

    /// Takes the attendee `address` off the invitation.
    pub fn remove_attendee(&mut self, address: &str, cx: &mut Context<Self>) {
        let address = address.to_lowercase();
        self.attendees.retain(|known| known.address.to_lowercase() != address);
        self.read_side(cx);
        cx.notify();
    }

    /// Whether the form may send anything: not in the read-only mode, not while a save is under way, not while the
    /// series chosen is still being read (the form still holds the occurrence).
    fn may_write(&self) -> bool {
        !self.core.read_only() && self.saving.is_none() && self.series_read.is_none()
    }

    /// Save (or Send, Send update): the event created or changed, through the engine.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.may_write() {
            return;
        }
        // an address still in a box is invited, as Teams does on Send
        for need in [Need::Required, Need::Optional] {
            let input = match need {
                Need::Required => &self.required,
                Need::Optional => &self.optional,
            };
            // Saving must not omit someone whose pending address failed validation.
            if !input.read(cx).value().trim().is_empty() && !self.invite_typed(need, window, cx) {
                return;
            }
        }
        let draft = self.fields(cx).draft();
        let write = match (&self.editing, &self.place) {
            (Some(editing), Some(place)) => {
                let mut event = editing.raw.clone();
                event.insert("inviteAllMembers".to_owned(), Value::Bool(self.personal_invites));
                EventWrite::UpdateInChannel {
                    event,
                    draft,
                    channel: place.channel.clone(),
                }
            }
            (Some(editing), None) => EventWrite::Update {
                id: editing.id.clone(),
                event: editing.raw.clone(),
                draft,
            },
            (None, Some(place)) => EventWrite::CreateInChannel {
                draft,
                channel: place.channel.clone(),
                personal_invites: self.personal_invites,
            },
            (None, None) => EventWrite::Create(draft),
        };
        self.send(write, cx);
    }

    /// Cancel meeting or Delete, once confirmed.
    fn remove(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        let Some(editing) = &self.editing else { return };
        if !self.may_write() {
            return;
        }
        let note = Some(self.cancel_note.read(cx).value().to_string()).filter(|note| !note.trim().is_empty());
        let in_channel = self
            .place
            .as_ref()
            .zip(teams_core::ScheduledMeeting::of_event(&editing.event));
        let write = match (confirm, in_channel) {
            (Confirm::Cancel, Some((place, (meeting, post)))) => EventWrite::CancelInChannel {
                meeting: Box::new(meeting),
                channel: place.channel.clone(),
                post,
                message: note,
            },
            (Confirm::Cancel, None) => EventWrite::Cancel {
                id: editing.id.clone(),
                chat: editing.event.meeting_chat.clone(),
                message: note,
            },
            (Confirm::Delete, _) => EventWrite::Delete { id: editing.id.clone() },
        };
        self.confirm = None;
        self.send(write, cx);
    }

    fn send(&mut self, write: EventWrite, cx: &mut Context<Self>) {
        let kind = write.kind();
        let touched = write.touched();
        self.problem = None;
        let reply = self.core.write_event(write);
        self.saving = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let answer = reply.await;
            let _ = this.update(cx, |form, cx| {
                form.saving = None;
                match answer {
                    Ok(_) => {
                        form.trace.record(&TraceLine::new("event-form", "saved").kind(kind));
                        (form.saved.0)(&touched, cx);
                        form.close(cx);
                    }
                    Err(error) => {
                        form.trace.record(&TraceLine::new("event-form", "not-saved").kind(kind));
                        form.problem = Some(capitalized(&error.to_string()).into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Closes the form's window.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some((window, _)) = alive(cx) {
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        match keystroke.key.as_str() {
            "escape" if self.custom.is_some() => self.custom = None,
            "escape" if self.confirm.is_some() => self.confirm = None,
            "escape" if self.suggestions.is_some() => self.suggestions = None,
            "s" if keystroke.modifiers.secondary() && !keystroke.modifiers.alt => self.save(window, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The label of the form's Save: Send update for a change that tells attendees, Send for a new event inviting
    /// anyone, else Save.
    fn save_label(&self) -> &'static str {
        match (&self.editing, self.attendees.is_empty()) {
            // a channel's meeting tells everyone on the team
            (Some(_), _) if self.place.is_some() => "Send update",
            (Some(_), false) => "Send update",
            // a channel's meeting is posted to the channel: everyone on the team gets it
            (None, false) => "Send",
            (None, true) if self.place.is_some() => "Send",
            _ => "Save",
        }
    }

    // --- Event and Series ---

    /// Whether what the form saves is a whole series: a new one, a series' master, or an occurrence's series picked.
    fn edits_series(&self) -> bool {
        match &self.series {
            Series::New => self.repeat.is_some(),
            Series::Master => true,
            Series::Occurrence { whole, .. } => *whole,
            Series::Single => false,
        }
    }

    /// Whether the series being changed must stay one (its repeat offers no "Does not repeat").
    fn series_fixed(&self) -> bool {
        !matches!(self.series, Series::New)
    }

    /// Whether the form is on its Series tab.
    #[must_use]
    pub fn on_series(&self) -> bool {
        self.edits_series()
    }

    /// The toolbar's Event (`false`) or Series (`true`): a new event repeats weekly from its start's day or no longer
    /// repeats; a change of an occurrence edits that occurrence or reads its series' master and edits the series.
    pub fn choose_series(&mut self, series: bool, window: &mut Window, cx: &mut Context<Self>) {
        if series == self.edits_series() {
            return;
        }
        match &self.series {
            Series::New => {
                let start = self.series_day();
                let repeat = series.then(|| Repeat::preset(repeat::Preset::Weekly, start));
                self.set_repeat(repeat, window, cx);
            }
            Series::Occurrence {
                occurrence, master_id, ..
            } => {
                if series {
                    let master_id = master_id.clone();
                    self.read_series(master_id, window, cx);
                } else {
                    let occurrence = (**occurrence).clone();
                    self.fill(occurrence, false, window, cx);
                    self.set_whole(false);
                    self.after_refill(cx);
                }
            }
            Series::Single | Series::Master => {}
        }
        cx.notify();
    }

    /// Reads the series' master `master_id`, then changes the form over to the whole series.
    fn read_series(&mut self, master_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let reply = self.core.calendar_event(master_id);
        self.problem = None;
        self.series_read = Some(cx.spawn_in(window, async move |this, cx| {
            let answer = reply.await;
            let _ = this.update_in(cx, |form: &mut Self, window, cx| {
                form.series_read = None;
                match answer {
                    Ok(master) => {
                        form.fill(master, true, window, cx);
                        form.set_whole(true);
                        form.after_refill(cx);
                    }
                    Err(_) => form.problem = Some("The series couldn't be read. Try again.".into()),
                }
                cx.notify();
            });
        }));
    }

    fn set_whole(&mut self, to: bool) {
        if let Series::Occurrence { whole, .. } = &mut self.series {
            *whole = to;
        }
    }

    /// The day and the free/busy times follow an event filled in again.
    fn after_refill(&mut self, cx: &mut Context<Self>) {
        self.day = self.slot.start.date();
        self.read_day(cx);
        self.read_side(cx);
    }

    /// Sets how the event repeats (`None`: it doesn't), its end's picker with it.
    fn set_repeat(&mut self, repeat: Option<Repeat>, window: &mut Window, cx: &mut Context<Self>) {
        let until = repeat.as_ref().and_then(|repeat| repeat.until);
        self.repeat = repeat;
        self.until_picker.update(cx, |picker, cx| {
            picker.set_date(gpui_component::calendar::Date::Single(until), window, cx);
        });
        cx.notify();
    }

    /// Opens the custom recurrence dialog on the series' recurrence.
    pub fn open_custom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start = self.series_day();
        let custom = Custom::of(self.repeat.as_ref(), start);
        let until = custom.until;
        self.custom = Some(custom);
        self.custom_until_picker.update(cx, |picker, cx| {
            picker.set_date(gpui_component::calendar::Date::Single(until), window, cx);
        });
        cx.notify();
    }

    /// Closes the custom recurrence dialog, keeping the series' recurrence as it was (its Cancel).
    pub fn close_custom(&mut self, cx: &mut Context<Self>) {
        self.custom = None;
        cx.notify();
    }

    /// Changes the open custom dialog's choices.
    fn edit_custom(&mut self, change: impl FnOnce(&mut Custom), cx: &mut Context<Self>) {
        if let Some(custom) = &mut self.custom {
            change(custom);
            cx.notify();
        }
    }

    /// The custom dialog's Save: its recurrence becomes the series'.
    fn save_custom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(custom) = self.custom.take() else { return };
        let repeat = custom.repeat(self.series_day());
        self.set_repeat(Some(repeat), window, cx);
    }
}

/// Whom an event read whole invites: its attendees with an address, the organizer (the account itself, listed by the
/// service, never invited by the form) left out.
fn invited(full: &FullEvent) -> Vec<EventAttendee> {
    let organizer = full.event.organizer_address.as_deref().map(str::to_lowercase);
    full.event
        .attendees
        .iter()
        .filter_map(|attendee| {
            let address = attendee.address.clone()?;
            (organizer.as_deref() != Some(address.to_lowercase().as_str())).then(|| EventAttendee {
                required: !full.optional.contains(&address.to_lowercase()),
                address,
                name: attendee.name.clone(),
                mri: None,
            })
        })
        .collect()
}

/// `text` with its first letter capitalized.
/// The confirmation's title, text and button, by what is removed (`noun`: the occurrence, the series, or the event).
fn confirm_words(confirm: Confirm, noun: &str) -> (&'static str, &'static str, &'static str) {
    match (confirm, noun) {
        (Confirm::Cancel, "occurrence") => (
            "Cancel occurrence?",
            "The attendees get a cancellation of this occurrence, with your message if you add one.",
            "Cancel occurrence",
        ),
        (Confirm::Cancel, "series") => (
            "Cancel series?",
            "The attendees get a cancellation of every occurrence, with your message if you add one.",
            "Cancel series",
        ),
        (Confirm::Cancel, _) => (
            "Cancel meeting?",
            "The attendees get a cancellation, with your message if you add one.",
            "Cancel meeting",
        ),
        (Confirm::Delete, "occurrence") => (
            "Delete this occurrence?",
            "It's removed from your calendar; the rest of the series stays.",
            "Delete occurrence",
        ),
        (Confirm::Delete, "series") => (
            "Delete this series?",
            "Every occurrence is removed from your calendar.",
            "Delete series",
        ),
        (Confirm::Delete, _) => ("Delete this event?", "It's removed from your calendar.", "Delete"),
    }
}

/// A menu item `label` that moves the form's event by `change`.
fn slot_item(form: WeakEntity<EventForm>, label: String, change: impl Fn(Slot) -> Slot + 'static) -> PopupMenuItem {
    PopupMenuItem::new(label).on_click(move |_, _, cx: &mut App| {
        let _ = form.update(cx, |form, cx| form.set_slot(change(form.slot), cx));
    })
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// A raised box of the form.
fn card(palette: Palette) -> gpui::Div {
    div()
        .rounded_lg()
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.border))
        .shadow(theme::glow(palette, 0x0000_0000, 0.28, 24.0, 6.0))
}

/// One of the form's rows: its icon, and what it holds over a line.
fn field_row(palette: Palette, glyph: Glyph, content: impl IntoElement, focused: bool) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(px(40.0))
                .flex_none()
                .flex()
                .justify_center()
                .child(glyph.icon().size(px(18.0)).text_color(rgb(palette.muted_foreground))),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h(px(48.0))
                .flex()
                .items_center()
                .gap_2()
                .when(focused, |line| line.border_b_2().border_color(rgb(palette.brand)))
                .when(!focused, |line| line.border_b_1().border_color(rgb(palette.border)))
                .child(content),
        )
}

impl EventForm {
    fn toolbar(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        let writes = !self.core.read_only();
        let show_as_menu = self.show_as_menu(cx);
        let reminder_menu = self.reminder_menu(cx);
        let category_menu = self.category_menu(cx);
        let privacy_menu = self.privacy_menu(cx);
        let removing = self.removing(writes, cx);
        let save_label = self.save_label();
        let save = selected_by(
            "event-save",
            Button::new("event-save")
                .primary()
                .icon(Glyph::SAVE.icon())
                .label(if self.saving.is_some() { "Saving…" } else { save_label })
                .disabled(!self.may_write())
                .tooltip(if writes {
                    concat!("Save (", ctrl!(), "+S)")
                } else {
                    "Read-only: nothing is saved"
                })
                .on_click(cx.listener(|form, _, window, cx| form.save(window, cx))),
        );
        let separator = || div().w(px(1.0)).h(px(20.0)).mx_1().bg(rgb(palette.border));
        card(palette)
            .debug_selector(|| "event-toolbar".to_owned())
            .h(px(56.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .children(self.kind_tabs(cx))
            .child(separator())
            .child(selected_by("event-show-as", show_as_menu))
            .child(selected_by("event-reminder", reminder_menu))
            .child(selected_by("event-category", category_menu))
            .child(selected_by("event-privacy", privacy_menu))
            .child(separator())
            .child(selected_by(
                "event-print",
                icon_button("event-print", Glyph::PRINTER, "Print")
                    .small()
                    .on_click(cx.listener(|form, _, _, cx| form.print(cx))),
            ))
            .child(div().flex_1())
            .children(removing)
            .child(save)
    }

    /// The toolbar's drop-down button `id`: its `glyph`, what `label` it shows, its chevron.
    fn toolbar_menu(id: &'static str, glyph: Glyph, label: Option<&'static str>) -> Button {
        Button::new(id).ghost().small().child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(glyph.icon().size(px(16.0)))
                .children(label)
                .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
        )
    }

    /// Show as: Free, Working elsewhere, Tentative, Busy, Away; the event's checked.
    fn show_as_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        let show_as = self.show_as;
        Self::toolbar_menu(
            "event-show-as",
            Glyph::CALENDAR_CLOCK,
            Some(model::show_as_label(show_as)),
        )
        .tooltip("Show as")
        .dropdown_menu(move |menu, _, _| {
            EventShowAs::ALL.iter().fold(menu.min_w(px(200.0)), |menu, choice| {
                let (form, choice) = (form.clone(), *choice);
                menu.item(
                    PopupMenuItem::new(model::show_as_label(choice))
                        .checked(choice == show_as)
                        .on_click(move |_, _, cx: &mut App| {
                            let _ = form.update(cx, |form, cx| form.set_show_as(choice, cx));
                        }),
                )
            })
        })
    }

    /// The reminder's drop-down; the event's checked.
    fn reminder_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        let reminder = self.reminder;
        Self::toolbar_menu("event-reminder", Glyph::ALARM, None)
            .tooltip(format!("Reminder: {}", model::reminder_label(reminder)))
            .dropdown_menu(move |menu, _, _| {
                REMINDERS.iter().fold(menu.min_w(px(200.0)), |menu, (choice, label)| {
                    let (form, choice) = (form.clone(), *choice);
                    menu.item(PopupMenuItem::new(*label).checked(choice == reminder).on_click(
                        move |_, _, cx: &mut App| {
                            let _ = form.update(cx, |form, cx| form.set_reminder(choice, cx));
                        },
                    ))
                })
            })
    }

    /// Categorize: Outlook's categories and the event's own, each checked while the event has it.
    fn category_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        let categories = self.categories.clone();
        let tip = if categories.is_empty() {
            "Categorize".to_owned()
        } else {
            format!("Categories: {}", categories.join(", "))
        };
        Self::toolbar_menu("event-category", Glyph::TAG, None)
            .tooltip(tip)
            .dropdown_menu(move |menu, _, _| {
                let mut names: Vec<String> = CATEGORIES.iter().map(|name| (*name).to_owned()).collect();
                names.extend(
                    categories
                        .iter()
                        .filter(|name| !CATEGORIES.contains(&name.as_str()))
                        .cloned(),
                );
                names.into_iter().fold(menu.min_w(px(200.0)), |menu, name| {
                    let form = form.clone();
                    let chosen = categories.contains(&name);
                    menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(chosen)
                            .on_click(move |_, _, cx: &mut App| {
                                let _ = form.update(cx, |form, cx| form.toggle_category(&name, cx));
                            }),
                    )
                })
            })
    }

    /// Private, checked while the event is.
    fn privacy_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let form = cx.entity().downgrade();
        let private = self.private;
        let glyph = if private { Glyph::LOCK } else { Glyph::LOCK_OPEN };
        Self::toolbar_menu("event-privacy", glyph, None)
            .tooltip(if private { "Private" } else { "Not private" })
            .dropdown_menu(move |menu, _, _| {
                let form = form.clone();
                menu.min_w(px(200.0))
                    .item(
                        PopupMenuItem::new("Private")
                            .checked(private)
                            .on_click(move |_, _, cx: &mut App| {
                                let _ = form.update(cx, Self::flip_private);
                            }),
                    )
            })
    }

    fn set_show_as(&mut self, show_as: EventShowAs, cx: &mut Context<Self>) {
        self.show_as = show_as;
        cx.notify();
    }

    fn set_reminder(&mut self, reminder: Option<u32>, cx: &mut Context<Self>) {
        self.reminder = reminder;
        cx.notify();
    }

    /// Takes category `name` off the event when it has it, else puts it on.
    fn toggle_category(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.categories.iter().any(|known| known == name) {
            self.categories.retain(|known| known != name);
        } else {
            self.categories.push(name.to_owned());
        }
        cx.notify();
    }

    fn flip_private(&mut self, cx: &mut Context<Self>) {
        self.private = !self.private;
        cx.notify();
    }

    /// Shows the optional attendees' box, or hides it.
    fn flip_optional(&mut self, cx: &mut Context<Self>) {
        self.optional_shown = !self.optional_shown;
        cx.notify();
    }

    /// The toolbar's Event and Series, as Teams' form has them: a new event's (Series makes it repeat), an occurrence's
    /// (Occurrence, or the whole series), a series' own (Series alone); a single event's change offers Event alone.
    fn kind_tabs(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let on_series = self.edits_series();
        let (first, series_offered) = match &self.series {
            Series::New | Series::Master => ("Event", true),
            Series::Occurrence { .. } => ("Occurrence", true),
            Series::Single => ("Event", false),
        };
        let reading = self.series_read.is_some();
        let tab = |id: &'static str, glyph: Glyph, label: &'static str, chosen: bool, series: bool| {
            let button = Button::new(id)
                .small()
                .icon(glyph.icon())
                .label(label)
                .selected(chosen)
                .when(chosen, gpui_component::button::Button::outline)
                .when(!chosen, gpui_component::button::ButtonVariants::ghost)
                .disabled(reading || matches!(self.series, Series::Master) && !series)
                .on_click(cx.listener(move |form, _, window, cx| form.choose_series(series, window, cx)));
            selected_by(id, button).into_any_element()
        };
        let mut tabs = vec![tab("event-kind-event", Glyph::CALENDAR, first, !on_series, false)];
        if series_offered {
            tabs.push(tab("event-kind-series", Glyph::REPEAT, "Series", on_series, true));
        }
        tabs
    }

    /// Cancel meeting or Delete (of an occurrence, or of the series while the form changes it whole), on a change.
    fn removing(&self, writes: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let editing = self.editing.as_ref().filter(|_| writes)?;
        // a channel's meeting reaches the whole team, whomever else it invites
        let meeting = editing.event.is_organizer && (self.place.is_some() || !editing.event.attendees.is_empty());
        let what = self.removal_noun();
        let (id, label, glyph, confirm) = match (meeting, what) {
            (true, "occurrence") => (
                "event-cancel-meeting",
                "Cancel occurrence",
                Glyph::CALENDAR_CANCEL,
                Confirm::Cancel,
            ),
            (true, "series") => (
                "event-cancel-meeting",
                "Cancel series",
                Glyph::CALENDAR_CANCEL,
                Confirm::Cancel,
            ),
            (true, _) => (
                "event-cancel-meeting",
                "Cancel meeting",
                Glyph::CALENDAR_CANCEL,
                Confirm::Cancel,
            ),
            (false, "occurrence") => ("event-delete", "Delete occurrence", Glyph::TRASH, Confirm::Delete),
            (false, "series") => ("event-delete", "Delete series", Glyph::TRASH, Confirm::Delete),
            (false, _) => ("event-delete", "Delete", Glyph::TRASH, Confirm::Delete),
        };
        Some(
            selected_by(
                id,
                Button::new(id)
                    .ghost()
                    .small()
                    .icon(glyph.icon())
                    .label(label)
                    .text_color(rgb(theme::DANGER))
                    .disabled(self.saving.is_some() || self.series_read.is_some())
                    .on_click(cx.listener(move |form, _, _, cx| {
                        form.confirm = Some(confirm);
                        cx.notify();
                    })),
            )
            .into_any_element(),
        )
    }

    /// What a removal removes: `occurrence`, `series` or `event`.
    fn removal_noun(&self) -> &'static str {
        match &self.series {
            Series::Occurrence { whole: false, .. } => "occurrence",
            Series::Occurrence { whole: true, .. } | Series::Master => "series",
            Series::New | Series::Single => "event",
        }
    }

    /// Print: the event's page, written and opened in the desktop's browser, which prints it ([`print`]).
    pub fn print(&mut self, cx: &mut Context<Self>) {
        let page = print::page(&self.printable(cx));
        match print::write(&page) {
            Ok(link) => {
                self.trace.record(&TraceLine::new("event-form", "printed"));
                cx.open_url(&link);
            }
            Err(why) => self.problem = Some(capitalized(&why).into()),
        }
        cx.notify();
    }

    /// What the printed page shows.
    #[must_use]
    pub fn printable(&self, cx: &App) -> print::Printable {
        let cycle = clock::cycle(cx);
        let start = self.series_day();
        let names = |required: bool| {
            self.attendees
                .iter()
                .filter(|attendee| attendee.required == required)
                .map(|attendee| match &attendee.name {
                    Some(name) if name != &attendee.address => format!("{name} <{}>", attendee.address),
                    _ => attendee.address.clone(),
                })
                .collect()
        };
        let editor = self.body.read(cx);
        let body_html = crate::compose::message_of(editor.doc(), &[])
            .ok()
            .flatten()
            .map(|message| message.html().to_owned());
        print::Printable {
            title: self.title.read(cx).value().to_string(),
            when: self.slot.text(cycle),
            repeats: self
                .repeat
                .as_ref()
                .filter(|_| self.edits_series())
                .map(|repeat| repeat.summary(start)),
            location: self.location.read(cx).value().to_string(),
            online: self.online,
            required: names(true),
            optional: names(false),
            show_as: model::show_as_label(self.show_as).to_owned(),
            reminder: model::reminder_label(self.reminder),
            categories: self.categories.clone(),
            private: self.private,
            body_html,
            body_text: editor.text(),
        }
    }

    fn title_row(&self, palette: Palette, window: &Window, cx: &App) -> gpui::Div {
        let focused = self.title.read(cx).focus_handle(cx).is_focused(window);
        div()
            .debug_selector(|| "event-title".to_owned())
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(px(40.0))
                    .h(px(40.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(palette.border))
                    .child(Glyph::CALENDAR_ADD.icon().size(px(20.0)).text_color(rgb(palette.brand))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(48.0))
                    .flex()
                    .items_center()
                    .text_xl()
                    .when(focused, |line| line.border_b_2().border_color(rgb(palette.brand)))
                    .when(!focused, |line| line.border_b_1().border_color(rgb(palette.border)))
                    .child(
                        Input::new(&self.title)
                            .appearance(false)
                            .with_size(gpui_component::Size::Large),
                    ),
            )
    }

    fn attendees_row(&self, need: Need, palette: Palette, window: &Window, cx: &mut Context<Self>) -> gpui::Div {
        let input = match need {
            Need::Required => &self.required,
            Need::Optional => &self.optional,
        };
        let focused = input.read(cx).focus_handle(cx).is_focused(window);
        let chips: Vec<AnyElement> = self
            .attendees
            .iter()
            .filter(|attendee| attendee.required == (need == Need::Required))
            .map(|attendee| Self::chip(attendee, palette, cx))
            .collect();
        let options = (need == Need::Required).then(|| {
            let form = cx.entity().downgrade();
            let shown = self.optional_shown;
            selected_by(
                "event-attendee-options",
                Button::new("event-attendee-options")
                    .ghost()
                    .small()
                    .tooltip("Attendee options")
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(Glyph::USER_COG.icon().size(px(16.0)))
                            .child(Glyph::CHEVRON_DOWN.icon().size(px(12.0))),
                    )
                    .dropdown_menu(move |menu, _, _| {
                        let form = form.clone();
                        menu.min_w(px(220.0))
                            .item(PopupMenuItem::new("Optional attendees").checked(shown).on_click(
                                move |_, _, cx: &mut App| {
                                    let _ = form.update(cx, Self::flip_optional);
                                },
                            ))
                    }),
            )
        });
        let selector = match need {
            Need::Required => "event-attendees",
            Need::Optional => "event-optional-attendees",
        };
        let content = div()
            .debug_selector(move || selector.to_owned())
            .flex_1()
            .min_w_0()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .py_1()
            .children(chips)
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.0))
                    .child(Input::new(input).appearance(false)),
            )
            .children(options);
        let glyph = if need == Need::Required {
            Glyph::USERS
        } else {
            Glyph::USER
        };
        field_row(palette, glyph, content, focused)
    }

    fn chip(attendee: &EventAttendee, palette: Palette, cx: &mut Context<Self>) -> AnyElement {
        let address = attendee.address.clone();
        let label = attendee.name.clone().unwrap_or_else(|| attendee.address.clone());
        let id = SharedString::from(format!("event-attendee-{}", attendee.address.to_lowercase()));
        div()
            .id(id.clone())
            .h(px(28.0))
            .pl_2()
            .pr_1()
            .flex()
            .items_center()
            .gap_1()
            .rounded_full()
            .bg(rgb(palette.hover))
            .text_sm()
            .tooltip({
                let address = attendee.address.clone();
                move |window, cx| gpui_component::tooltip::Tooltip::new(address.clone()).build(window, cx)
            })
            .child(
                div()
                    .max_w(px(220.0))
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(
                icon_button(SharedString::from(format!("{id}-remove")), Glyph::X, "Remove")
                    .xsmall()
                    .on_click(cx.listener(move |form, _, _, cx| form.remove_attendee(&address, cx))),
            )
            .into_any_element()
    }

    /// The suggestions under the attendee box they are for.
    fn suggestions_view(&self, need: Need, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let (for_need, found) = self.suggestions.as_ref()?;
        if *for_need != need {
            return None;
        }
        let input = match need {
            Need::Required => &self.required,
            Need::Optional => &self.optional,
        };
        let typed = input.read(cx).value().to_string();
        let typed_row = model::typed_attendee(&typed, true).map(|attendee| {
            div()
                .id("event-suggestion-typed")
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .hover(move |row| row.bg(rgb(palette.hover)))
                .child(Glyph::PLUS.icon().size(px(14.0)))
                .child(format!("Invite {}", attendee.address))
                .on_click(cx.listener(move |form, _, window, cx| {
                    form.invite_typed(need, window, cx);
                }))
        });
        let rows: Vec<AnyElement> = found
            .iter()
            .map(|person| {
                let picked = person.clone();
                let selector = format!("event-suggestion-{}", person.address.to_lowercase());
                div()
                    .debug_selector(move || selector.clone())
                    .id(SharedString::from(format!(
                        "event-suggestion-{}",
                        person.address.to_lowercase()
                    )))
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    .hover(move |row| row.bg(rgb(palette.hover)))
                    .child(person.name.clone())
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(palette.muted_foreground))
                            .child(person.address.clone()),
                    )
                    .on_click(cx.listener(move |form, _, window, cx| form.invite_person(&picked, need, window, cx)))
                    .into_any_element()
            })
            .collect();
        if rows.is_empty() && typed_row.is_none() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "event-suggestions".to_owned())
                .ml(px(52.0))
                .w(px(360.0))
                .py_1()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .shadow_lg()
                .children(rows)
                .children(typed_row),
        )
    }

    fn when_row(&mut self, palette: Palette, window: &mut Window, cx: &mut Context<Self>) -> gpui::Div {
        let slot = self.slot;
        let zoned = self.zones.is_some() && !slot.all_day;
        let zoned_lines = zoned.then(|| self.zoned_lines(window, cx));
        let date_menu = (!zoned).then(|| self.date_menu(cx));
        let start_menu = (!slot.all_day && !zoned).then(|| self.start_menu(cx));
        let end_menu = (!slot.all_day && !zoned).then(|| self.end_menu(cx));
        let globe = (!slot.all_day).then(|| {
            let shown = self.zones.is_some();
            selected_by(
                "event-zones",
                icon_button(
                    "event-zones",
                    Glyph::GLOBE,
                    if shown { "Hide time zones" } else { "Show time zones" },
                )
                .small()
                .selected(shown)
                .on_click(cx.listener(move |form, _, _, cx| form.show_zones(!shown, cx))),
            )
        });
        let all_day = Switch::new("event-all-day")
            .checked(slot.all_day)
            .label("All day")
            .on_click(cx.listener(|form, checked: &bool, _, cx| {
                let slot = form.slot.with_all_day(*checked);
                form.set_slot(slot, cx);
            }));
        let content = div()
            .debug_selector(|| "event-when".to_owned())
            .flex_1()
            .flex()
            .items_center()
            .gap_1()
            .children(zoned_lines)
            .children(date_menu.map(|menu| selected_by("event-date", menu)))
            .children(start_menu.map(|menu| selected_by("event-start", menu)))
            .when(!slot.all_day && !zoned, |line| line.child("-"))
            .children(end_menu.map(|menu| selected_by("event-end", menu)))
            .children(globe)
            .child(div().flex_1())
            .child(selected_by("event-all-day", all_day))
            .child(self.suggested_times(cx))
            .child(selected_by(
                "event-scheduler",
                Button::new("event-scheduler")
                    .ghost()
                    .small()
                    .icon(Glyph::CALENDAR_RANGE.icon())
                    .label("Scheduler")
                    .selected(self.side == Side::Scheduler)
                    .tooltip(if self.side == Side::Scheduler {
                        "Show the day"
                    } else {
                        "See when everyone is free"
                    })
                    .on_click(cx.listener(|form, _, _, cx| {
                        let side = if form.side == Side::Scheduler {
                            Side::Day
                        } else {
                            Side::Scheduler
                        };
                        form.show_side(side, cx);
                    })),
            ));
        field_row(palette, Glyph::CLOCK, content, false)
    }

    /// The event's date: six weeks of days from a week ago.
    fn date_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let slot = self.slot;
        let form = cx.entity().downgrade();
        Button::new("event-date")
            .ghost()
            .small()
            .tooltip("Date")
            .label(slot.start.format("%a %-m/%-d/%Y").to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(220.0)).scrollable(true).max_h(px(420.0));
                (0..42).fold(menu, |menu, offset| {
                    let date = Local::now().date_naive() + Duration::days(offset - 7);
                    menu.item(
                        slot_item(form.clone(), date.format("%a %-m/%-d/%Y").to_string(), move |slot| {
                            slot.on(date)
                        })
                        .checked(date == slot.start.date()),
                    )
                })
            })
    }

    /// The event's start: the day's half hours.
    fn start_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cycle = clock::cycle(cx);
        let slot = self.slot;
        let form = cx.entity().downgrade();
        Button::new("event-start")
            .ghost()
            .small()
            .tooltip("Start time")
            .label(slot.start.format(cycle.time_of_day()).to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(160.0)).scrollable(true).max_h(px(420.0));
                model::day_times(slot.start.date())
                    .into_iter()
                    .fold(menu, |menu, time| {
                        let label = time.format(cycle.time_of_day()).to_string();
                        menu.item(
                            slot_item(form.clone(), label, move |slot| slot.starting(time)).checked(time == slot.start),
                        )
                    })
            })
    }

    /// The event's end: the times after its start, each with the length it makes.
    fn end_menu(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cycle = clock::cycle(cx);
        let slot = self.slot;
        let form = cx.entity().downgrade();
        Button::new("event-end")
            .ghost()
            .small()
            .tooltip("End time")
            .label(slot.end.format(cycle.time_of_day()).to_string())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(200.0)).scrollable(true).max_h(px(420.0));
                model::end_times(slot.start)
                    .into_iter()
                    .fold(menu, |menu, (time, length)| {
                        let label = format!("{} ({length})", time.format(cycle.time_of_day()));
                        menu.item(
                            slot_item(form.clone(), label, move |slot| slot.ending(time)).checked(time == slot.end),
                        )
                    })
            })
    }

    /// The suggested times beside the time: the starts on the day shown that every attendee is free at, each moving the
    /// event there; offered once there are attendees.
    fn suggested_times(&self, cx: &mut Context<Self>) -> AnyElement {
        let cycle = clock::cycle(cx);
        let suggestions = self.suggestions_on_day();
        let form = cx.entity().downgrade();
        let tip = if self.attendees.is_empty() {
            "Add attendees to see suggested times"
        } else {
            "Suggested times"
        };
        let button = Button::new("event-suggested-times")
            .ghost()
            .small()
            .icon(Glyph::CALENDAR_SEARCH.icon())
            .tooltip(tip)
            .disabled(self.attendees.is_empty())
            .dropdown_menu(move |menu, _, _| {
                let menu = menu.min_w(px(200.0)).scrollable(true).max_h(px(360.0));
                if suggestions.is_empty() {
                    return menu.item(PopupMenuItem::new("No suggested times on this day").disabled(true));
                }
                suggestions.iter().fold(menu, |menu, start| {
                    let start = *start;
                    let label = start.format(cycle.time_of_day()).to_string();
                    menu.item(slot_item(form.clone(), label, move |slot| slot.starting(start)))
                })
            });
        selected_by("event-suggested-times", button).into_any_element()
    }

    fn location_row(&self, palette: Palette, window: &Window, cx: &App) -> gpui::Div {
        let focused = self.location.read(cx).focus_handle(cx).is_focused(window);
        field_row(
            palette,
            Glyph::MAP_PIN,
            div()
                .debug_selector(|| "event-location".to_owned())
                .flex_1()
                .child(Input::new(&self.location).appearance(false)),
            focused,
        )
    }

    fn online_row(&self, palette: Palette, cx: &mut Context<Self>) -> gpui::Div {
        // a Teams meeting stays one: its join link and chat were made with it
        let fixed = self.editing.as_ref().is_some_and(|editing| editing.event.online);
        div()
            .flex()
            .items_center()
            .gap_3()
            .min_h(px(48.0))
            .child(
                div().w(px(40.0)).flex_none().flex().justify_center().child(
                    Glyph::VIDEO
                        .icon()
                        .size(px(18.0))
                        .text_color(rgb(palette.muted_foreground)),
                ),
            )
            .child(selected_by(
                "event-online",
                Switch::new("event-online")
                    .checked(self.online)
                    .label("Microsoft Teams meeting")
                    .disabled(fixed)
                    .on_click(cx.listener(|form, checked: &bool, _, cx| {
                        form.online = *checked;
                        cx.notify();
                    })),
            ))
    }

    fn problem_bar(&self, palette: Palette) -> Option<gpui::Div> {
        let problem = self.problem.clone()?;
        Some(
            div()
                .debug_selector(|| "event-problem".to_owned())
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(rgb(theme::DANGER))
                .bg(rgb(mix(palette.surface, theme::DANGER, 120)))
                .child(Glyph::CIRCLE_ALERT.icon().size(px(16.0)).text_color(rgb(theme::DANGER)))
                .child(div().flex_1().text_sm().child(problem)),
        )
    }

    /// The confirmation of Cancel meeting (with a note for the attendees) or Delete, over the form.
    fn confirm_view(&self, palette: Palette, cx: &mut Context<Self>) -> Option<gpui::Stateful<gpui::Div>> {
        let confirm = self.confirm?;
        let (title, text, action) = confirm_words(confirm, self.removal_noun());
        let dialog = div()
            .debug_selector(|| "event-confirm".to_owned())
            .id("event-confirm")
            .occlude()
            .w(px(440.0))
            .p_5()
            .flex()
            .flex_col()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .shadow_lg()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(div().text_lg().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(text))
            .when(confirm == Confirm::Cancel, |dialog| {
                dialog.child(Input::new(&self.cancel_note))
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by(
                        "event-confirm-keep",
                        Button::new("event-confirm-keep")
                            .outline()
                            .label("Don't")
                            .on_click(cx.listener(|form, _, _, cx| {
                                form.confirm = None;
                                cx.notify();
                            })),
                    ))
                    .child(selected_by(
                        "event-confirm-go",
                        Button::new("event-confirm-go")
                            .danger()
                            .label(action)
                            .on_click(cx.listener(move |form, _, _, cx| form.remove(confirm, cx))),
                    )),
            );
        Some(
            div()
                .id("event-confirm-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x0000_0066))
                .on_click(cx.listener(|form, _, _, cx| {
                    form.confirm = None;
                    cx.notify();
                }))
                .child(dialog),
        )
    }
}

impl Render for EventForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current(cx);
        let form_card = card(palette)
            .p_4()
            .pr_6()
            .flex()
            .flex_col()
            .gap_1()
            .child(self.title_row(palette, window, cx))
            .child(self.attendees_row(Need::Required, palette, window, cx))
            .children(self.suggestions_view(Need::Required, palette, cx))
            .when(self.optional_shown, |card| {
                card.child(self.attendees_row(Need::Optional, palette, window, cx))
            })
            .children(
                self.optional_shown
                    .then(|| self.suggestions_view(Need::Optional, palette, cx))
                    .flatten(),
            )
            .child(self.when_row(palette, window, cx))
            .children(self.repeat_row(palette, cx))
            .children(self.channel_row(palette, cx))
            .child(self.location_row(palette, window, cx))
            .when(self.place.is_none(), |card| card.child(self.online_row(palette, cx)));
        let left = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.problem_bar(palette))
            .child(form_card)
            .child(self.body_card(palette, cx));
        let side = match self.side {
            Side::Day => self.day_view(palette, cx),
            Side::Scheduler => self.scheduler_view(palette, cx),
        };
        let confirm = self.confirm_view(palette, cx);
        let custom = self.custom_view(palette, cx);
        div()
            .debug_selector(|| "event-form".to_owned())
            .key_context("EventForm")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .relative()
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            // its cards float on the frame, as the main window's panes do
            .bg(theme::frame(palette))
            .text_color(rgb(palette.foreground))
            .on_mouse_move(cx.listener(Self::dragged))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|form, _, _, cx| form.end_drag(cx)))
            .child(self.toolbar(palette, cx))
            .child(div().flex_1().min_h_0().flex().gap_4().child(left).child(side))
            .children(confirm)
            .children(custom)
            .children(self.emoji_overlay())
    }
}
