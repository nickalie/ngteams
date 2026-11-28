//! The noise suppression level of each account's calls (Settings > Calls > Devices, and the call's audio menu), as
//! Teams keeps it for each user: Auto until the account chooses another. The file keeps a table of level names under
//! the account's MRI, only for the accounts that chose another level than Auto.
//!
//! Each account's calls read their level from a [`NoiseControl`] ([`follow`]), so a change applies to a call at once.

use gpui::{App, Entity};
use teams_media::processing::NoiseControl;

use super::SettingsModel;
pub use teams_app::settings::noise::{description, label};

/// Keeps `control` at `account`'s level: now, and at each change of the settings.
pub fn follow(model: &Entity<SettingsModel>, account: String, control: NoiseControl, cx: &mut App) {
    control.set(model.read(cx).settings().noise_suppression_of(&account));
    cx.observe(model, move |model, cx| {
        control.set(model.read(cx).settings().noise_suppression_of(&account));
    })
    .detach();
}
