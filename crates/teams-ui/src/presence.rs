//! Others' presence as the views show it (M2 Task 17): a dot beside a person in the chat list (a 1:1 chat's other
//! member, on the avatar), the chat's header (its other member) and the mention picker (each member offered); a
//! message's sender's badge on their avatar ([`crate::presence_badge`]).
//!
//! - **From the engine only.** A view asks the engine about everyone it shows ([`watch`]: `Core::presence`, which makes
//!   the engine read them from then on), and draws what the [`Presences`] model holds: the engine's
//!   `Event::Presence`s as the bridge applied them. A person no event told about yet is
//!   [`Availability::Unknown`](teams_core::Availability::Unknown).
//! - **Within the engine's watch** ([`teams_core::MAX_WATCHED`] people, the one asked about longest ago dropped and
//!   told unknown): a view asks again each time it shows its people anew, and the views together ask about no more
//!   than it holds: the chat list its top [`LIST_WATCHED`](crate::chat_list::LIST_WATCHED) 1:1 chats' other members,
//!   the header its chat's, the chat view the newest [`SENDERS_WATCHED`](crate::chat_view::SENDERS_WATCHED) senders
//!   it shows after each load, the mention picker the [`PICKER_MAX`](crate::compose::PICKER_MAX) people it offers.
//!   The chat view and the picker show the open chat's people and share one share of the watch: while the picker
//!   offers people the view doesn't show, the ones asked about longest ago may drop until their views ask again (the
//!   next load of the chat or the list). The profile card's Organization tab and `ngteamsctl presence` (and
//!   `person`) ask outside these shares, with the same effect. A chat left isn't unwatched: its senders give way to whoever is asked about
//!   next, as anyone does.
//! - **Unknown** is drawn as a hollow ring: before the first read, for someone this build can't read (in the lab,
//!   anyone but the lab accounts), when a read failed, and for someone no longer watched. A word UPS gave that this build doesn't name is drawn the
//!   same way, its word as the dot's label.

use std::collections::HashMap;

use gpui::{Div, InteractiveElement as _, ParentElement as _, SharedString, Styled as _, div, px, rgb};
use teams_core::Availability;

use crate::bridge::Core;
use crate::theme::Palette;

/// A dot's diameter, px.
pub const DOT: f32 = 10.0;

/// Available's green.
const GREEN: u32 = 0x006b_b700;
/// Busy's and Do not disturb's red: Teams' one red.
const RED: u32 = crate::theme::DANGER;
/// Away's and Be right back's amber.
const AMBER: u32 = 0x00ff_aa44;
/// Offline's grey.
const GREY: u32 = 0x008a_8886;

/// Everyone's presence as the engine last told it ([`Models::presence`](crate::Models::presence)).
#[derive(Debug, Default)]
pub struct Presences {
    known: HashMap<String, Availability>,
}

impl Presences {
    /// `mri`'s presence as last told; unknown while none was.
    #[must_use]
    pub fn of(&self, mri: &str) -> Availability {
        self.known.get(mri).cloned().unwrap_or(Availability::Unknown)
    }

    /// Keeps `mri`'s presence; whether it changed.
    pub(crate) fn set(&mut self, mri: String, availability: Availability) -> bool {
        self.known.insert(mri, availability.clone()) != Some(availability)
    }
}

/// Asks the engine about everyone in `mris`, so it reads their presence from now on; the answers come as events.
pub fn watch<'a>(core: &dyn Core, mris: impl IntoIterator<Item = &'a String>) {
    for mri in mris {
        // the request goes out as it is made; its answer is the model's to show, as the events tell it
        drop(core.presence(mri.clone()));
    }
}

/// What a dot says, for a tooltip, the trace and the tests.
#[must_use]
pub fn label(availability: &Availability) -> SharedString {
    match availability {
        Availability::Available => "Available".into(),
        Availability::AvailableIdle => "Available, idle".into(),
        Availability::Away => "Away".into(),
        Availability::BeRightBack => "Be right back".into(),
        Availability::Busy => "Busy".into(),
        Availability::BusyIdle => "Busy, idle".into(),
        Availability::DoNotDisturb => "Do not disturb".into(),
        Availability::Offline => "Offline".into(),
        Availability::Other(word) => word.clone().into(),
        Availability::Unknown => "Unknown".into(),
    }
}

/// The trace's name for `availability` (`available`, `busy`, …; `other` for a word this build doesn't name).
#[must_use]
pub fn key(availability: &Availability) -> &'static str {
    match availability {
        Availability::Available => "available",
        Availability::AvailableIdle => "available-idle",
        Availability::Away => "away",
        Availability::BeRightBack => "be-right-back",
        Availability::Busy => "busy",
        Availability::BusyIdle => "busy-idle",
        Availability::DoNotDisturb => "do-not-disturb",
        Availability::Offline => "offline",
        Availability::Other(_) => "other",
        Availability::Unknown => "unknown",
    }
}

/// The dot's fill; `None` for a hollow ring (unknown, or a word this build doesn't name).
#[must_use]
pub fn fill(availability: &Availability) -> Option<u32> {
    match availability {
        Availability::Available | Availability::AvailableIdle => Some(GREEN),
        Availability::Busy | Availability::BusyIdle | Availability::DoNotDisturb => Some(RED),
        Availability::Away | Availability::BeRightBack => Some(AMBER),
        Availability::Offline => Some(GREY),
        Availability::Other(_) | Availability::Unknown => None,
    }
}

/// The dot of `availability`, its debug selector `presence-<what>` (the label's words joined by `-`, lowercase).
#[must_use]
pub fn dot(availability: &Availability, palette: Palette) -> Div {
    let selector = format!(
        "presence-{}",
        label(availability)
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    );
    let dot = div()
        .debug_selector(move || selector.clone())
        .size(px(DOT))
        .flex_none()
        .rounded_full()
        .border_1()
        .border_color(rgb(palette.background));
    match fill(availability) {
        Some(color) => dot.bg(rgb(color)),
        None => dot
            .border_color(rgb(palette.muted_foreground))
            .bg(rgb(palette.background)),
    }
}

/// The dot with its label beside it, for the chat's header.
#[must_use]
pub fn labelled(availability: &Availability, palette: Palette) -> Div {
    div()
        .flex()
        .items_center()
        .gap_1()
        .text_xs()
        .font_weight(gpui::FontWeight::NORMAL)
        .text_color(rgb(palette.muted_foreground))
        .child(dot(availability, palette))
        .child(label(availability))
}
