//! What a meeting shares with everyone in it, as Teams' call window shows it (`docs/protocol/calling.md`, "More of a
//! running call"):
//!
//! - **The timer** (More → Timer in a group call or a meeting): a bar over the stage with the time left in the brand's
//!   colour, Play or Pause, Reset and a bar of what is left; its time opens the editor (minutes and seconds with their
//!   steppers, +1, +5 and +10 min, Cancel timer and Start). Each change goes to everyone in the call
//!   ([`Ask::Timer`]); the bar shows anyone's timer as the conversation tells it, counting down between its updates.
//!   More → Timer before anyone set one shows the bar at five minutes, paused, until Play or Start sends it.
//! - **The lock** (People's "…" and Controls, the meeting's organizer's): "Lock the meeting?" asks first; the notice
//!   "This meeting is locked." (or unlocked) shows once the meeting tells it changed.
//! - **A recording or a transcription someone runs**: its notice, and the header's mark meanwhile ([`super::transcript`]).
//! - **Meeting info** (More → Meeting info in a meeting): a pane with the meeting's title, Copy join info, and how to
//!   join as its invitation says (the join link, the meeting id, the passcode, Need help? and System reference).
//! - **The attendance list** (People's "…" → Download attendance list): made here from who is in the call and when
//!   each joined, as Teams' is (a UTF-16 tab-separated file), saved where the user picks.

use super::*;
use gpui_component::Disableable as _;
use std::time::{SystemTime, UNIX_EPOCH};
use teams_proto::{DEFAULT_TIMER_MS, MeetingTimer, TimerState};

/// The timer editor's largest minutes.
const MAX_MINUTES: u64 = 99;

/// The timer as the bar shows it: one the meeting runs, or the five minutes More → Timer opened before anyone set
/// one (its draft).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TimerShown {
    Live,
    Draft,
}

/// What the account changes the timer to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerChange {
    /// Play, or Pause while it runs.
    Toggle,
    Reset,
    /// The editor's Start, at its time.
    Start,
    Cancel,
}

/// Now, in Unix milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|now| i64::try_from(now.as_millis()).ok())
        .unwrap_or_default()
}

/// `ms` as Teams' timer writes it: `MM:SS`.
#[must_use]
pub fn timer_text(ms: u64) -> String {
    let seconds = ms.div_ceil(1000);
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// The timer `change` makes of `timer` (the one the meeting runs, if any), the editor's time `edited` (ms) for Start,
/// at `now` (Unix ms); `None` when the change doesn't apply (Reset of a draft).
#[must_use]
pub fn timer_after(timer: Option<&MeetingTimer>, change: TimerChange, edited: u64, now: i64) -> Option<MeetingTimer> {
    let current = timer.map(|timer| timer.state);
    let whole = timer.map_or(DEFAULT_TIMER_MS, |timer| timer.original_ms);
    let made = |state, remaining_ms, original_ms| MeetingTimer {
        state,
        remaining_ms,
        original_ms,
        edited_ms: None,
        previous: current,
    };
    Some(match (change, timer) {
        (TimerChange::Toggle, Some(timer)) if timer.state == TimerState::InProgress => {
            made(TimerState::Paused, timer.left_at(now), whole)
        }
        (TimerChange::Toggle, Some(timer)) => made(TimerState::InProgress, timer.remaining_ms, whole),
        (TimerChange::Toggle, None) => made(TimerState::InProgress, DEFAULT_TIMER_MS, DEFAULT_TIMER_MS),
        // as captured: a reset leaves it paused at its whole time
        (TimerChange::Reset, Some(_)) => made(TimerState::Paused, whole, whole),
        (TimerChange::Reset | TimerChange::Cancel, None) => return None,
        (TimerChange::Start, _) => made(TimerState::InProgress, edited, edited),
        (TimerChange::Cancel, Some(_)) => made(TimerState::Cancelled, whole, whole),
    })
}

/// The attendance list Teams' Download attendance list makes: `Full Name`, `User Action` and `Timestamp`, tab-separated,
/// a `Joined` row per person with their join time as `M/D/YY, h:mm:ss AM`, in UTF-16 (little-endian, with its BOM).
#[must_use]
pub fn attendance_list(people: &[(String, chrono::DateTime<chrono::Local>)]) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut text = String::from("Full Name\tUser Action\tTimestamp\r\n");
    for (name, joined) in people {
        let at = joined.format("%-m/%-d/%y, %-I:%M:%S %p");
        let _ = write!(text, "{name}\tJoined\t{at}\r\n");
    }
    let mut bytes = vec![0xff, 0xfe];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

/// The name Teams gives the attendance list's file.
pub const ATTENDANCE_FILE: &str = "meetingAttendanceList.csv";

/// Whether `active` is a meeting's call: one gone into as a meeting, or one in a meeting's chat (an invitation into
/// it).
#[must_use]
pub fn is_meeting(active: &ActiveCall) -> bool {
    active.kind == CallKind::Meeting || active.chat.as_deref().is_some_and(teams_proto::is_meeting_chat_id)
}

impl CallStage {
    /// Whether the call takes the meeting's tools (Timer, Controls): a group call's or a meeting's that is up, never
    /// the test call's or in the read-only mode.
    pub(super) fn takes_meeting_tools(&self, active: &ActiveCall) -> bool {
        let group = is_meeting(active)
            || active.view.participants.len() > 1
            || active
                .chat
                .as_deref()
                .is_some_and(|chat| teams_proto::one_to_one_pair(chat).is_none());
        group && active.kind != CallKind::TestCall && active.status == CallStatus::Connected && !self.core.read_only()
    }

    /// More → Timer: the bar shows (the meeting's timer, or a draft of five minutes until it is sent).
    pub fn open_timer(&mut self, cx: &mut Context<Self>) {
        self.timer_draft = true;
        cx.notify();
    }

    /// The timer the bar shows, if any: the meeting's, else the draft More → Timer opened.
    fn timer_shown(&self, active: &ActiveCall) -> Option<(TimerShown, Option<MeetingTimer>)> {
        match (&active.meeting.timer, self.timer_draft) {
            (Some(timer), _) => Some((TimerShown::Live, Some(timer.clone()))),
            (None, true) => Some((TimerShown::Draft, None)),
            (None, false) => None,
        }
    }

    /// Changes the timer as `change` says (the editor's time for Start), sent to everyone in the call.
    pub(super) fn change_timer(&mut self, change: TimerChange, cx: &mut Context<Self>) {
        let Some(active) = self.live(cx) else { return };
        let edited = self
            .timer_edit
            .map_or(DEFAULT_TIMER_MS, |(minutes, seconds)| (minutes * 60 + seconds) * 1000);
        self.timer_edit = None;
        if change == TimerChange::Cancel && active.meeting.timer.is_none() {
            // a draft nobody sent: it goes away here alone
            self.timer_draft = false;
            cx.notify();
            return;
        }
        if change == TimerChange::Cancel {
            self.timer_draft = false;
        }
        if let Some(timer) = timer_after(active.meeting.timer.as_ref(), change, edited, now_ms()) {
            self.ask(Ask::Timer(timer), cx);
        }
        cx.notify();
    }

    /// Opens the timer's editor at the time the bar shows (or closes it).
    pub fn edit_timer(&mut self, open: bool, cx: &mut Context<Self>) {
        self.timer_edit = open.then(|| {
            let ms = self
                .live(cx)
                .and_then(|active| active.meeting.timer)
                .map_or(DEFAULT_TIMER_MS, |timer| timer.original_ms);
            let seconds = ms / 1000;
            ((seconds / 60).min(MAX_MINUTES), seconds % 60)
        });
        cx.notify();
    }

    /// Steps the editor's minutes (`minutes`) or seconds by `by`, or adds `add` minutes (+1, +5, +10 min).
    fn step_timer(&mut self, (minutes, by, add): (bool, i64, u64), cx: &mut Context<Self>) {
        let Some((m, s)) = self.timer_edit.as_mut() else { return };
        if add > 0 {
            *m = (*m + add).min(MAX_MINUTES);
        } else if minutes {
            *m = m.saturating_add_signed(by).min(MAX_MINUTES);
        } else {
            *s = u64::try_from((i64::try_from(*s).unwrap_or(0) + by).rem_euclid(60)).unwrap_or(0);
        }
        cx.notify();
    }

    /// The stage with what the meeting shares over it: the timer's bar on top (and its editor), the notices.
    pub(super) fn with_meeting(
        &self,
        stage: AnyElement,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bar = self.timer_bar(active, palette, cx);
        let editor = bar.is_some().then(|| self.timer_editor(active, palette, cx)).flatten();
        let notices = self.notices(active, palette, cx);
        if bar.is_none() && notices.is_none() {
            return stage;
        }
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .children(bar)
            .child(div().flex_1().min_h_0().flex().child(stage))
            .children(notices.map(|notices| {
                div()
                    .absolute()
                    .top(px(if self.timer_shown(active).is_some() { 40.0 } else { 0.0 }))
                    .left_0()
                    .right_0()
                    .child(notices)
            }))
            .children(editor)
            .into_any_element()
    }

    /// The timer's bar over the stage, while one shows (see the module docs).
    pub(super) fn timer_bar(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (shown, timer) = self.timer_shown(active)?;
        let now = now_ms();
        let (left, whole, running) = match &timer {
            Some(timer) => (
                timer.left_at(now),
                timer.original_ms.max(1),
                timer.state == TimerState::InProgress,
            ),
            None => (DEFAULT_TIMER_MS, DEFAULT_TIMER_MS, false),
        };
        let controls = !self.core.read_only();
        let (glyph, tip) = if running {
            (Glyph::PAUSE, "Pause timer")
        } else {
            (Glyph::PLAY, "Start timer")
        };
        let toggle = selected_by(
            "call-timer-toggle",
            icon_button("call-timer-toggle", glyph, tip)
                .small()
                .disabled(!controls)
                .on_click(cx.listener(|this, _, _, cx| this.change_timer(TimerChange::Toggle, cx))),
        );
        let reset = selected_by(
            "call-timer-reset",
            icon_button("call-timer-reset", Glyph::RESET, "Reset timer")
                .small()
                .disabled(!controls || shown == TimerShown::Draft)
                .on_click(cx.listener(|this, _, _, cx| this.change_timer(TimerChange::Reset, cx))),
        );
        let fraction = f32::from(
            u16::try_from(left.saturating_mul(1000) / whole)
                .unwrap_or(1000)
                .min(1000),
        ) / 1000.0;
        let time = div()
            .debug_selector(|| "call-timer-time".to_owned())
            .id("call-timer-time")
            .px_1()
            .rounded_md()
            .text_2xl()
            .font_weight(gpui::FontWeight::BOLD)
            .text_color(rgb(palette.brand))
            .when(self.timer_edit.is_some(), |time| time.bg(rgb(palette.selected)))
            .when(controls, |time| {
                time.cursor_pointer()
                    .hover(|time| time.bg(rgb(palette.hover)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let open = this.timer_edit.is_none();
                        this.edit_timer(open, cx);
                    }))
            })
            .child(SharedString::from(timer_text(left)));
        let progress = progress_bar(fraction, palette);
        Some(
            div()
                .debug_selector(|| "call-timer".to_owned())
                .flex_none()
                .h(px(40.0))
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .bg(rgb(palette.background))
                .child(time)
                .child(toggle)
                .child(reset)
                .child(progress)
                .into_any_element(),
        )
    }

    /// The timer's editor under its time while it is open: minutes and seconds with their steppers, +1, +5 and +10 min,
    /// Cancel timer and Start.
    pub(super) fn timer_editor(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (minutes, seconds) = self.timer_edit?;
        let cancel = selected_by(
            "call-timer-cancel",
            Button::new("call-timer-cancel")
                .label("Cancel timer")
                .outline()
                .small()
                .disabled(active.meeting.timer.is_none() && !self.timer_draft)
                .on_click(cx.listener(|this, _, _, cx| this.change_timer(TimerChange::Cancel, cx))),
        );
        let start = selected_by(
            "call-timer-start",
            Button::new("call-timer-start")
                .label("Start")
                .primary()
                .small()
                .disabled(minutes == 0 && seconds == 0)
                .on_click(cx.listener(|this, _, _, cx| this.change_timer(TimerChange::Start, cx))),
        );
        Some(
            div()
                .debug_selector(|| "call-timer-editor".to_owned())
                .id("call-timer-editor")
                .occlude()
                .absolute()
                .top(px(40.0))
                .left(px(12.0))
                .w(px(216.0))
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .rounded_lg()
                .border_1()
                .border_color(rgb(palette.border))
                .bg(rgb(palette.surface))
                .text_color(rgb(palette.foreground))
                .shadow_lg()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .child(stepper(true, minutes, cx))
                        .child(div().text_3xl().font_weight(gpui::FontWeight::BOLD).child(":"))
                        .child(stepper(false, seconds, cx)),
                )
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(add_minutes(1, cx))
                        .child(add_minutes(5, cx))
                        .child(add_minutes(10, cx)),
                )
                .child(div().flex().justify_between().gap_2().child(cancel).child(start))
                .into_any_element(),
        )
    }

    /// What the meeting told since last seen: a lock changed or a recording started shows its notice.
    pub(super) fn meeting_seen(&mut self, active: &ActiveCall) {
        let state = &active.meeting;
        if let Some(was) = self.lock_seen
            && was != state.locked
        {
            self.lock_notice = Some(state.locked);
        }
        self.lock_seen = Some(state.locked);
        self.recorder_seen(active);
        if state.timer.is_some() {
            // the meeting runs a timer now: no draft of the account's own
            self.timer_draft = false;
        }
    }

    /// The notices over the stage's top while they show: the lock's and a recording's (see the module docs).
    pub(super) fn notices(&self, active: &ActiveCall, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        let lock = self.lock_notice.map(|locked| {
            let (heading, line) = if locked {
                ("This meeting is locked.", "No one else can join.")
            } else {
                ("This meeting is unlocked.", "People can join.")
            };
            notice(
                ("call-lock-notice", Glyph::LOCK, heading),
                SharedString::from(line),
                palette,
                cx.listener(|this, _, _, cx| {
                    this.lock_notice = None;
                    cx.notify();
                }),
            )
        });
        let recording = self.recording_notice_view(active, palette, cx);
        let others = self.recording_notices(active, palette, cx);
        let muted = self.muted_toast(active, palette, cx);
        if lock.is_none() && recording.is_none() && others.is_empty() && muted.is_none() {
            return None;
        }
        Some(
            div()
                .absolute()
                .top(px(8.0))
                .left_0()
                .right_0()
                .px_2()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .children(muted)
                .children(others)
                .children(recording)
                .children(lock)
                .into_any_element(),
        )
    }

    /// The name of `mri` in the call: the account's own, or a participant's as the call names them.
    pub(super) fn name_of(&self, active: &ActiveCall, mri: &Mri) -> SharedString {
        if mri.as_str() == self.core.me() {
            return self.own_name.clone().unwrap_or_else(|| SharedString::from("You"));
        }
        let people = self.people.of(active.id);
        active
            .view
            .participants
            .iter()
            .find(|participant| participant.mri == mri.as_str())
            .map_or_else(
                || SharedString::from(SOMEONE),
                |participant| participant_name(participant, people),
            )
    }

    /// "Lock the meeting?" shown (`true`, as Lock asks first) or put away (Cancel).
    pub fn confirm_locking(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.confirm_lock = shown;
        cx.notify();
    }

    /// Lock the meeting (`true`) or Unlock it: Lock asks first ("Lock the meeting?"), Unlock goes at once.
    pub fn ask_lock(&mut self, lock: bool, cx: &mut Context<Self>) {
        if lock {
            self.confirm_lock = true;
        } else {
            self.ask(Ask::LockMeeting(false), cx);
        }
        cx.notify();
    }

    /// "Lock the meeting?" over the whole window while it is asked: Lock locks it, Cancel leaves it as it is.
    pub(super) fn lock_confirmation(&self, palette: Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.confirm_lock {
            return None;
        }
        let cancel = Button::new("call-lock-cancel")
            .label("Cancel")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm_lock = false;
                cx.notify();
            }));
        let lock = Button::new("call-lock-confirm")
            .label("Lock")
            .primary()
            .on_click(cx.listener(|this, _, _, cx| {
                this.confirm_lock = false;
                this.ask(Ask::LockMeeting(true), cx);
                cx.notify();
            }));
        let dialog = div()
            .debug_selector(|| "call-lock-dialog".to_owned())
            .occlude()
            .w(px(430.0))
            .max_w_full()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .rounded_lg()
            .border_1()
            .border_color(rgb(palette.border))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.foreground))
            .shadow_lg()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::BOLD)
                    .child("Lock the meeting?"),
            )
            .child(div().text_sm().child(
                "No one else will be able to join, but invitees can still access the meeting chat, the recording, \
                 and other meeting info.",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(selected_by("call-lock-cancel", cancel))
                    .child(selected_by("call-lock-confirm", lock)),
            );
        Some(
            div()
                .id("call-lock-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .p_6()
                .bg(gpui::black().opacity(0.4))
                .occlude()
                .child(dialog)
                .into_any_element(),
        )
    }

    /// Opens or closes the Meeting info pane, as More → Meeting info does.
    pub fn show_meeting_info(&mut self, open: bool, cx: &mut Context<Self>) {
        if open {
            self.pane = Pane::MeetingInfo;
            self.effects = None;
        } else if self.pane == Pane::MeetingInfo {
            self.pane = Pane::None;
        }
        cx.notify();
    }

    /// The Meeting info pane (see the module docs).
    pub(super) fn meeting_info_pane(
        &self,
        active: &ActiveCall,
        title: &SharedString,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.pane != Pane::MeetingInfo {
            return None;
        }
        let close = selected_by(
            "call-meeting-info-close",
            icon_button("call-meeting-info-close", Glyph::X, "Close")
                .small()
                .on_click(cx.listener(|this, _, _, cx| this.show_meeting_info(false, cx))),
        );
        let join = active.meeting.join.clone();
        let heading = active
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .map_or_else(|| title.clone(), SharedString::from);
        let copy_text = join.as_ref().map(teams_proto::MeetingJoinInfo::text);
        let copy = selected_by(
            "call-copy-join-info",
            Button::new("call-copy-join-info")
                .outline()
                .label(if self.join_info_copied {
                    "Join info copied"
                } else {
                    "Copy join info"
                })
                .disabled(copy_text.is_none())
                .w(px(240.0))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(text) = copy_text.clone() {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        this.join_info_copied = true;
                        cx.notify();
                    }
                })),
        );
        let details = join.map(|join| join_details(&join, palette));
        let missing = details.is_none().then(|| {
            div()
                .text_sm()
                .text_color(rgb(palette.muted_foreground))
                .child("The meeting's join info isn't known yet.")
        });
        Some(
            people_pane::side_pane("call-meeting-info-pane", "Meeting info", close, palette)
                .child(
                    div()
                        .id("call-meeting-info-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .px_4()
                        .pb_4()
                        .flex()
                        .flex_col()
                        .child(div().pt_2().text_lg().child(heading))
                        .child(
                            div()
                                .mt_8()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(Glyph::LINK.icon().size(px(18.0)))
                                .child(copy),
                        )
                        .child(div().mt_8().mb_4().h(px(1.0)).bg(rgb(palette.foreground)))
                        .children(details)
                        .children(missing),
                )
                .into_any_element(),
        )
    }

    /// People's "…" → Download attendance list: who is in the call and when each joined, saved where the user picks
    /// (the downloads folder when there is no file chooser).
    pub(super) fn download_attendance(&mut self, cx: &mut Context<Self>) {
        let Some(active) = self.live(cx) else { return };
        let people = self.attendance(&active);
        let bytes = attendance_list(&people);
        let Ok(folder) = crate::picture_viewer::downloads() else {
            self.trace
                .record(&TraceLine::new("call-attendance", "saved").kind("failed"));
            return;
        };
        let chosen = cx.prompt_for_new_path(&folder, Some(ATTENDANCE_FILE));
        let trace = Arc::clone(&self.trace);
        cx.spawn(async move |_, cx: &mut AsyncApp| {
            let target = match chosen.await {
                Ok(Ok(Some(path))) => path,
                Ok(Ok(None)) => return,
                Ok(Err(_)) | Err(_) => folder.join(ATTENDANCE_FILE),
            };
            let written = cx
                .background_executor()
                .spawn(async move { std::fs::write(&target, &bytes) })
                .await;
            trace.record(&TraceLine::new("call-attendance", "saved").kind(if written.is_ok() {
                "written"
            } else {
                "failed"
            }));
        })
        .detach();
    }

    /// Who is in the call and when each joined: the account first (since its media connected), then each participant
    /// as the roster gives their join.
    fn attendance(&self, active: &ActiveCall) -> Vec<(String, chrono::DateTime<chrono::Local>)> {
        let now = chrono::Local::now();
        let own_join = active
            .connected_at
            .and_then(|at| chrono::Duration::from_std(at.elapsed()).ok())
            .map_or(now, |since| now - since);
        let own = self
            .own_name
            .clone()
            .map_or_else(|| self.core.me().to_owned(), |name| name.to_string());
        let people = self.people.of(active.id);
        std::iter::once((own, own_join))
            .chain(active.view.participants.iter().map(|participant| {
                let joined = participant
                    .joined
                    .as_deref()
                    .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                    .map_or(now, |at| at.with_timezone(&chrono::Local));
                (participant_name(participant, people).to_string(), joined)
            }))
            .collect()
    }
}

/// What the call's More offers besides its own entries, as Teams' per call kind: Meeting info (a meeting's), Timer (a
/// group call's or a meeting's), and under Settings Call health (any call once it is up) and Mute notifications (a
/// group call's or a meeting's).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says whether More offers an entry of its own"
)]
pub(super) struct MoreTools {
    meeting_info: bool,
    timer: bool,
    call_health: bool,
    mute_notifications: bool,
    /// The notifications are held back now (the entry reads Unmute notifications).
    notifications_muted: bool,
}

impl CallStage {
    /// What More offers for the call as it stands (see [`MoreTools`]).
    pub(super) fn more_tools(&self, cx: &App) -> MoreTools {
        let Some(active) = self.live(cx) else {
            return MoreTools::default();
        };
        let group = self.takes_meeting_tools(&active);
        MoreTools {
            meeting_info: is_meeting(&active) && active.status == CallStatus::Connected,
            timer: group,
            call_health: active.status == CallStatus::Connected,
            mute_notifications: group,
            notifications_muted: crate::notify::muted_in_call(cx),
        }
    }

    /// Controls, as Teams' toolbar offers it to a meeting's organizer: Lock meeting (Unlock meeting while it is
    /// locked) and View more options (the meeting's options page in the browser).
    pub(super) fn controls_menu(
        &self,
        active: &ActiveCall,
        palette: Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // a group call's organizer has them as a meeting's has (`controls`)
        self.controls_tool(active, palette, cx)
    }

    /// The meeting's options page (`docs/protocol/calling.md`, "The lobby"), when its organizer and tenant are known
    /// (the meeting was gone into from its pre-join).
    pub(super) fn meeting_options_url(&self, active: &ActiveCall, cx: &App) -> Option<String> {
        let chat = active.chat.as_deref()?;
        let to_join = self
            .calls
            .read(cx)
            .meeting_to_join()
            .filter(|to_join| to_join.chat() == Some(chat))?;
        let meeting = to_join.meeting()?;
        Some(format!(
            "https://teams.microsoft.com/meetingOptions/?organizerId={}&tenantId={}&threadId={}&messageId=0",
            meeting.organizer_id,
            meeting.tenant_id,
            chat.replace(':', "_")
        ))
    }

    /// People's "…" in a meeting: Download attendance list, and for its organizer Lock the meeting (Unlock the
    /// meeting while it is locked).
    pub(super) fn roster_more(&self, active: &ActiveCall, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !is_meeting(active) || active.status != CallStatus::Connected {
            return None;
        }
        let stage = cx.weak_entity();
        let lock = (active.organizer && !self.core.read_only()).then_some(active.meeting.locked);
        Some(
            selected_by(
                "call-roster-more",
                icon_button("call-roster-more", Glyph::ELLIPSIS, "More options")
                    .small()
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |menu: PopupMenu, _, _| {
                        let download = stage.clone();
                        let menu = menu.min_w(px(220.0)).item(
                            toolbar::menu_item(
                                "call-roster-attendance",
                                Some(Glyph::DOWNLOAD),
                                "Download attendance list",
                            )
                            .on_click(move |_, _, cx| {
                                let _ = download.update(cx, CallStage::download_attendance);
                            }),
                        );
                        match lock {
                            Some(locked) => {
                                let stage = stage.clone();
                                let (glyph, label) = if locked {
                                    (Glyph::LOCK_OPEN, "Unlock the meeting")
                                } else {
                                    (Glyph::LOCK, "Lock the meeting")
                                };
                                menu.item(toolbar::menu_item("call-roster-lock", Some(glyph), label).on_click(
                                    move |_, _, cx| {
                                        let _ = stage.update(cx, |stage, cx| stage.ask_lock(!locked, cx));
                                    },
                                ))
                            }
                            None => menu,
                        }
                    }),
            )
            .into_any_element(),
        )
    }
}

/// More's first entries per call kind (see [`MoreTools`]): Meeting info, Timer.
pub(super) fn more_items(menu: PopupMenu, stage: &WeakEntity<CallStage>, tools: MoreTools) -> PopupMenu {
    let menu = if tools.meeting_info {
        let stage = stage.clone();
        menu.item(
            toolbar::menu_item("call-more-meeting-info", Some(Glyph::INFO), "Meeting info").on_click(
                move |_, _, cx| {
                    let _ = stage.update(cx, |stage, cx| stage.show_meeting_info(true, cx));
                },
            ),
        )
    } else {
        menu
    };
    if tools.timer {
        let stage = stage.clone();
        menu.item(
            toolbar::menu_item("call-more-timer", Some(Glyph::TIMER), "Timer").on_click(move |_, _, cx| {
                let _ = stage.update(cx, CallStage::open_timer);
            }),
        )
    } else {
        menu
    }
}

/// What More's Help opens in a group call or a meeting, as Teams' does (seen 2026-11-06): Microsoft's help for Teams
/// calls and meetings, in the browser.
pub(super) const HELP_URL: &str = "https://go.microsoft.com/fwlink/?linkid=2163160";

/// More's last entry in a group call or a meeting, after a separator: Help ([`HELP_URL`]). A 1:1 call's More has none.
pub(super) fn help_item(menu: PopupMenu, tools: MoreTools) -> PopupMenu {
    if !tools.timer {
        return menu;
    }
    menu.separator().item(
        toolbar::menu_item("call-more-help", Some(Glyph::CIRCLE_QUESTION), "Help")
            .on_click(|_, _, cx| cx.open_url(HELP_URL)),
    )
}

/// More's Settings, where it has an entry: Call health once the call is up, Mute notifications (or Unmute) in a group
/// call or a meeting.
pub(super) fn settings_submenu(
    menu: PopupMenu,
    stage: &WeakEntity<CallStage>,
    tools: MoreTools,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    if !tools.mute_notifications && !tools.call_health {
        return menu;
    }
    let stage = stage.clone();
    // no icon, as Record and transcribe and Language and speech: the kit draws a submenu's in the gutter, out of line
    // with the items' own (seen live, 2026-11-06)
    menu.submenu("Settings", window, cx, move |menu, _, _| {
        let menu = if tools.call_health {
            menu.item(super::health::menu_item(stage.clone()))
        } else {
            menu
        };
        if !tools.mute_notifications {
            return menu.min_w(px(220.0));
        }
        let stage = stage.clone();
        let muted = tools.notifications_muted;
        let (glyph, label) = if muted {
            (Glyph::BELL, "Unmute notifications")
        } else {
            (Glyph::BELL_OFF, "Mute notifications")
        };
        menu.min_w(px(220.0)).item(
            toolbar::menu_item("call-more-mute-notifications", Some(glyph), label).on_click(move |_, _, cx| {
                // held back while the call runs: the stage takes it back when the call ends
                crate::notify::mute_in_call(!muted, cx);
                let _ = stage.update(cx, |_, cx| cx.notify());
            }),
        )
    })
}

/// The timer's bar of what is left (`fraction` of its whole time), filled from its right as Teams' is.
fn progress_bar(fraction: f32, palette: Palette) -> gpui::Div {
    div()
        .debug_selector(|| "call-timer-progress".to_owned())
        .flex_1()
        .h(px(22.0))
        .rounded_full()
        .bg(rgb(palette.hover))
        .flex()
        .justify_end()
        .child(
            div()
                .h_full()
                .w(relative(fraction))
                .rounded_full()
                .bg(rgb(palette.brand).opacity(0.3)),
        )
}

/// The timer editor's minutes (`minutes_part`) or seconds, `value`, with their Up and Down steppers.
fn stepper(minutes_part: bool, value: u64, cx: &mut Context<CallStage>) -> gpui::Div {
    let part = if minutes_part { "minutes" } else { "seconds" };
    let step = |by: i64, glyph: Glyph, cx: &mut Context<CallStage>| {
        let id = format!("call-timer-{part}-{}", if by > 0 { "up" } else { "down" });
        selected_by(
            id.clone(),
            icon_button(SharedString::from(id), glyph, if by > 0 { "Up" } else { "Down" })
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| this.step_timer((minutes_part, by, 0), cx))),
        )
    };
    div()
        .flex()
        .items_center()
        .child(
            div()
                .debug_selector(move || format!("call-timer-{part}"))
                .text_3xl()
                .font_weight(gpui::FontWeight::BOLD)
                .child(SharedString::from(format!("{value:02}"))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(step(1, Glyph::CHEVRON_UP, cx))
                .child(step(-1, Glyph::CHEVRON_DOWN, cx)),
        )
}

/// The timer editor's "+N min".
fn add_minutes(minutes: u64, cx: &mut Context<CallStage>) -> AnyElement {
    let id = format!("call-timer-add-{minutes}");
    selected_by(
        id.clone(),
        Button::new(SharedString::from(id))
            .ghost()
            .xsmall()
            .label(SharedString::from(format!("+{minutes} min")))
            .on_click(cx.listener(move |this, _, _, cx| this.step_timer((true, 0, minutes), cx))),
    )
    .into_any_element()
}

/// How to join, as Meeting info shows it: "Microsoft Teams meeting", "Join:" and the link, the meeting id and the
/// passcode, then Need help? | System reference between rules.
fn join_details(join: &teams_proto::MeetingJoinInfo, palette: Palette) -> gpui::Div {
    let rule = || div().my_4().h(px(1.0)).bg(rgb(palette.foreground));
    let line = |label: &'static str, value: Option<String>, selector: &'static str| {
        value.map(|value| {
            div()
                .debug_selector(move || selector.to_owned())
                .text_sm()
                .child(SharedString::from(format!("{label} {value}")))
        })
    };
    let links = div()
        .flex()
        .items_center()
        .gap_1()
        .text_sm()
        .children(
            join.help_link
                .clone()
                .map(|link| link_text("call-meeting-help", "Need help?".into(), link, palette)),
        )
        .when(join.help_link.is_some() && join.reference_link.is_some(), |row| {
            row.child(div().text_color(rgb(palette.muted_foreground)).child("|"))
        })
        .children(
            join.reference_link
                .clone()
                .map(|link| link_text("call-meeting-reference", "System reference".into(), link, palette)),
        );
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .child("Microsoft Teams meeting"),
        )
        .child(
            div()
                .mt_2()
                .text_lg()
                .font_weight(gpui::FontWeight::BOLD)
                .child("Join:"),
        )
        .children(
            join.join_link
                .clone()
                .map(|link| link_text("call-meeting-join-link", link.clone().into(), link, palette).text_lg()),
        )
        .child(div().h_2())
        .children(line("Meeting ID:", join.meeting_id.clone(), "call-meeting-id"))
        .children(line("Passcode:", join.passcode.clone(), "call-meeting-passcode"))
        .child(rule())
        .child(links)
        .child(rule())
}

/// A notice over the stage, as Teams' (the lock's, a recording's): its icon and heading with a Close, its line under.
pub(super) fn notice(
    (selector, glyph, heading): (&'static str, Glyph, &'static str),
    line: impl IntoElement,
    palette: Palette,
    close: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let close_id = format!("{selector}-close");
    div()
        .debug_selector(move || selector.to_owned())
        .occlude()
        .w(px(320.0))
        .max_w_full()
        .px_3()
        .py_2()
        .flex()
        .flex_col()
        .gap_1()
        .rounded_lg()
        .border_1()
        .border_color(rgb(palette.border))
        .bg(rgb(palette.surface))
        .text_color(rgb(palette.foreground))
        .shadow_md()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(glyph.icon().size(px(16.0)))
                // a text's narrowest is its whole line in GPUI: without `min_w_0` a long heading pushes Close out
                .child(div().flex_1().min_w_0().text_sm().child(heading))
                .child(selected_by(
                    close_id.clone(),
                    icon_button(SharedString::from(close_id), Glyph::X, "Close")
                        .xsmall()
                        .on_click(close),
                )),
        )
        .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(line))
        .into_any_element()
}

/// `text` and then the link `label` in one run of text, as Teams' notices end with their privacy policy: the link in the
/// brand's colour, underlined, opens `url` in the browser. One run wraps as a paragraph inside its card. A row of the
/// text and the link apart overflowed it: GPUI measures a text's narrowest as its whole line, so a flex row never
/// wraps it, however long a name in it makes it.
pub(super) fn text_with_link(
    selector: &'static str,
    text: &str,
    label: &str,
    url: &'static str,
    palette: Palette,
) -> AnyElement {
    let whole = format!("{text} {label}");
    let link = whole.len() - label.len()..whole.len();
    let style = gpui::HighlightStyle {
        color: Some(rgb(palette.brand).into()),
        underline: Some(gpui::UnderlineStyle {
            thickness: px(1.0),
            ..gpui::UnderlineStyle::default()
        }),
        ..gpui::HighlightStyle::default()
    };
    div()
        .debug_selector(move || selector.to_owned())
        .w_full()
        .child(
            gpui::InteractiveText::new(
                selector,
                gpui::StyledText::new(whole).with_highlights([(link.clone(), style)]),
            )
            .on_click(vec![link], move |_, _, cx| cx.open_url(url)),
        )
        .into_any_element()
}

/// A link in the brand's colour, underlined, that opens `url` in the browser.
pub(super) fn link_text(
    selector: &'static str,
    text: SharedString,
    url: String,
    palette: Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .debug_selector(move || selector.to_owned())
        .id(selector)
        .text_color(rgb(palette.brand))
        .underline()
        .cursor_pointer()
        .on_click(move |_, _, cx| cx.open_url(&url))
        .child(text)
}
