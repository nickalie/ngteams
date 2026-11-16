//! The title bar's update button: shown while a newer release downloads, once it is ready, and for as long as
//! installing it goes on.
//!
//! The app's updater (`ngteams::update`) checks for releases, downloads a newer one by itself and installs it at the
//! press; this is only what the window shows of it ([`set_status`]) and the press it hands back ([`on_press`]). One
//! status is shared by every account's title bar.
//!
//! - **Downloading update… n%**, **Installing update…**: a spinner, no press.
//! - **Restart to update** (downloaded and verified): the press installs and restarts, unless something holds the
//!   restart up (a call, or a message not sent yet, [`Waiting`]): then it says what does.
//! - **Update failed**: why in its tooltip; the press tries again, as the next check does.
//!
//! Nothing is shown otherwise: no update, or a build that doesn't update itself.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Global, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::Sizable as _;
use gpui_component::spinner::Spinner;
use gpui_component::tooltip::Tooltip;

use crate::icons::Glyph;
use crate::theme::{self, Palette};

/// Where the app's update stands, as the title bar shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Nothing to show.
    #[default]
    None,
    /// Downloading and verifying a newer release: how far, in percent.
    Downloading { version: SharedString, percent: u8 },
    /// Downloaded and verified; its install and restart wait for the press.
    Ready(SharedString),
    /// Installing it; the app restarts once it is in.
    Installing(SharedString),
    /// It didn't go through: why. The press tries again.
    Failed {
        version: SharedString,
        reason: SharedString,
    },
}

/// What holds an update's restart up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waiting {
    /// A call is up or ringing (any account's).
    Call,
    /// A message is typed and not sent yet (any account's box): a restart would lose it.
    Unsent,
}

impl Waiting {
    /// What the press says while this holds the restart up.
    #[must_use]
    pub fn says(self) -> &'static str {
        match self {
            Self::Call => "The update installs after your call: end it, then select Restart to update",
            Self::Unsent => "Send or discard your unsent messages, then select Restart to update",
        }
    }
}

/// What a press asks of the app.
pub type Press = Rc<dyn Fn(&mut App)>;
/// What holds the restart up now, asked of the app.
pub type Holds = Rc<dyn Fn(&App) -> Option<Waiting>>;

#[derive(Default)]
struct Updates {
    status: UpdateStatus,
    press: Option<Press>,
    holds: Option<Holds>,
}

impl Global for Updates {}

fn updates(cx: &mut App) -> &mut Updates {
    cx.default_global::<Updates>()
}

/// Shows `status` in every title bar.
pub fn set_status(status: UpdateStatus, cx: &mut App) {
    if updates(cx).status != status {
        updates(cx).status = status;
        cx.refresh_windows();
    }
}

/// The status the title bars show.
#[must_use]
pub fn status(cx: &App) -> UpdateStatus {
    cx.try_global::<Updates>()
        .map(|updates| updates.status.clone())
        .unwrap_or_default()
}

/// What a press of the button does.
pub fn on_press(press: Press, cx: &mut App) {
    updates(cx).press = Some(press);
}

/// What tells whether something holds the restart up.
pub fn holds_with(holds: Holds, cx: &mut App) {
    updates(cx).holds = Some(holds);
}

/// What holds an update's restart up now; nothing when the app said nothing does.
#[must_use]
pub fn waiting(cx: &App) -> Option<Waiting> {
    let holds = cx.try_global::<Updates>()?.holds.clone()?;
    holds(cx)
}

/// The press, as the button takes it: the app's press when the status takes one, else nothing. A press of Restart to
/// update while something holds the restart up gives back what does, for the window to say.
pub fn press(cx: &mut App) -> Option<Waiting> {
    let status = status(cx);
    match status {
        UpdateStatus::Ready(_) if let Some(waiting) = waiting(cx) => return Some(waiting),
        UpdateStatus::Ready(_) | UpdateStatus::Failed { .. } => {}
        UpdateStatus::None | UpdateStatus::Downloading { .. } | UpdateStatus::Installing(_) => return None,
    }
    if let Some(press) = cx.try_global::<Updates>().and_then(|updates| updates.press.clone()) {
        press(cx);
    }
    None
}

/// The button's face for `status`: its glyph (none while busy), its words and its tooltip.
fn face(status: &UpdateStatus, waiting: Option<Waiting>) -> Option<(Option<Glyph>, SharedString, SharedString)> {
    Some(match status {
        UpdateStatus::None => return None,
        UpdateStatus::Downloading { version, percent } => (
            None,
            format!("Downloading update… {percent}%").into(),
            format!("Downloading ngteams {version}").into(),
        ),
        UpdateStatus::Ready(version) => (
            Some(Glyph::REFRESH),
            "Restart to update".into(),
            waiting.map_or_else(
                || format!("ngteams {version} is downloaded: restart to install it").into(),
                |waiting| waiting.says().into(),
            ),
        ),
        UpdateStatus::Installing(version) => (
            None,
            "Installing update…".into(),
            format!("Installing ngteams {version}; ngteams restarts once it's in").into(),
        ),
        UpdateStatus::Failed { version, reason } => (
            Some(Glyph::CIRCLE_ALERT),
            "Update failed".into(),
            format!("ngteams {version} wasn't installed. {reason}\nSelect to try again").into(),
        ),
    })
}

/// The button as the title bar shows it, on the window's frame; `pressed` runs on a press (none while busy).
pub fn button(
    palette: Palette,
    pressed: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Option<AnyElement> {
    let status = status(cx);
    let (glyph, words, tip) = face(&status, None)?;
    let busy = glyph.is_none();
    let failed = matches!(status, UpdateStatus::Failed { .. });
    let frame = rgb(palette.on_frame);
    let mark: AnyElement = match glyph {
        Some(glyph) => glyph.icon().size(px(14.0)).text_color(frame).into_any_element(),
        None => Spinner::new().small().color(frame.into()).into_any_element(),
    };
    Some(
        div()
            .debug_selector(|| "title-update".to_owned())
            .id("title-update")
            .flex_none()
            .h(px(28.0))
            .px_2p5()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded_md()
            .border_1()
            .border_color(if failed {
                rgb(theme::DANGER)
            } else {
                frame.opacity(0.45)
            })
            // a failure is a solid red chip: Teams' danger red reads on every theme's frame, a red outline doesn't
            .when(failed, |button| button.bg(rgb(theme::DANGER)))
            .text_color(frame)
            .text_sm()
            .whitespace_nowrap()
            .when(!busy, |button| {
                button
                    .cursor_pointer()
                    .hover(move |button| {
                        if failed {
                            button.bg(rgb(theme::DANGER_HOVER))
                        } else {
                            button.bg(frame.opacity(0.15))
                        }
                    })
                    .on_click(pressed)
            })
            // A shell is leased during render. Ask the desktop about its shells only when the
            // tooltip opens, after rendering, just as a press checks the current holds.
            .tooltip(move |window, cx| {
                let tip = if matches!(status, UpdateStatus::Ready(_)) {
                    face(&status, waiting(cx)).map_or_else(|| tip.clone(), |(_, _, tip)| tip)
                } else {
                    tip.clone()
                };
                Tooltip::new(tip).build(window, cx)
            })
            .child(mark)
            .child(words)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[gpui::test]
    fn the_status_is_shared_and_a_press_goes_to_the_app_only_when_it_takes_one(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            assert_eq!(status(cx), UpdateStatus::None);
            assert_eq!(waiting(cx), None);
            assert_eq!(press(cx), None, "no press is set: nothing happens");
            let pressed = Rc::new(Cell::new(0));
            let counted = Rc::clone(&pressed);
            on_press(Rc::new(move |_| counted.set(counted.get() + 1)), cx);
            let version = SharedString::from("0.1.0.7");
            // only the restart and a retry of a failure take a press: the download starts by itself
            let takes = [
                (UpdateStatus::None, false),
                (
                    UpdateStatus::Downloading {
                        version: version.clone(),
                        percent: 40,
                    },
                    false,
                ),
                (UpdateStatus::Ready(version.clone()), true),
                (UpdateStatus::Installing(version.clone()), false),
                (
                    UpdateStatus::Failed {
                        version: version.clone(),
                        reason: "Offline.".into(),
                    },
                    true,
                ),
            ];
            for (shown, taken) in takes {
                set_status(shown.clone(), cx);
                assert_eq!(status(cx), shown);
                let before = pressed.get();
                assert_eq!(press(cx), None);
                assert_eq!(pressed.get() - before, usize::from(taken), "{shown:?}");
            }
            // a call holds the restart up: the press says so and asks nothing of the app
            holds_with(Rc::new(|_| Some(Waiting::Call)), cx);
            set_status(UpdateStatus::Ready(version.clone()), cx);
            let before = pressed.get();
            assert_eq!(press(cx), Some(Waiting::Call));
            assert_eq!(pressed.get(), before);
            // ... but not a new try of a failed download, which may go on meanwhile
            set_status(
                UpdateStatus::Failed {
                    version,
                    reason: "Offline.".into(),
                },
                cx,
            );
            assert_eq!(press(cx), None);
            assert_eq!(pressed.get(), before + 1);
        });
    }

    #[test]
    fn each_status_says_what_it_is_and_what_a_press_does() {
        let version = SharedString::from("0.1.0.7");
        assert!(face(&UpdateStatus::None, None).is_none());
        let words = |status: UpdateStatus, waiting| {
            let (glyph, words, tip) = face(&status, waiting).unwrap();
            (glyph.is_some(), words.to_string(), tip.to_string())
        };
        assert_eq!(
            words(
                UpdateStatus::Downloading {
                    version: version.clone(),
                    percent: 42
                },
                None
            ),
            (
                false,
                "Downloading update… 42%".into(),
                "Downloading ngteams 0.1.0.7".into()
            )
        );
        assert_eq!(
            words(UpdateStatus::Ready(version.clone()), None),
            (
                true,
                "Restart to update".into(),
                "ngteams 0.1.0.7 is downloaded: restart to install it".into()
            )
        );
        assert_eq!(
            words(UpdateStatus::Ready(version.clone()), Some(Waiting::Unsent)).2,
            Waiting::Unsent.says()
        );
        assert!(!words(UpdateStatus::Installing(version.clone()), None).0);
        let failed = words(
            UpdateStatus::Failed {
                version,
                reason: "Not enough space.".into(),
            },
            None,
        );
        assert_eq!(failed.1, "Update failed");
        assert!(failed.2.contains("Not enough space.") && failed.2.ends_with("Select to try again"));
        assert!(Waiting::Call.says().contains("after your call"));
    }
}
