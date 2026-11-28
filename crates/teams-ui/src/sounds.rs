//! The app's sounds: which one plays for each event, how loud, and the packs they come from, as Teams' Settings has
//! them ("Sound" under Notifications and activity, "Choose a ringtone for you" under Calls), the packs an extension
//! of those.
//!
//! - **An event** ([`SoundEvent`]) is something the app makes a sound for, a wider list than Teams': Teams sets one
//!   notification sound, one urgent sound and a ringtone. Each has a switch, a volume and, optionally, its own pick of
//!   a pack's sound or of the user's own file ([`EventSound`]); left alone it plays the sound of the pack the user
//!   chose as a whole ([`SoundSettings::pack`]). A master volume sits over them all.
//! - **A pack** ([`SoundSource`]) is the app's ([`synth`]: made by code, but for one recorded for the app,
//!   [`recorded`]) or one the user imported ([`pack`]). A sound that can't be had falls back: the event's own pick, the chosen pack's sound, the
//!   default pack's, which can't fail ([`board`]).
//! - **Playing them** is the [`board::SoundBoard`]'s, on the speaker the calls use.
//! - **Quiet while busy**: "Play sounds when I'm busy or in a meeting" off keeps [notification](SoundEvent::notifies)
//!   sounds and the ringtone quiet in those states; a call's own cues still play.

pub mod board;
pub mod pack;
mod recorded;
pub mod synth;

pub use board::{SoundBoard, SoundPlayer};
pub use pack::{ImportKind, PackError, PackInfo};
pub use teams_app::settings::sounds::{
    EventSound, LARGE_CALL, MAX_VOLUME, SoundEvent, SoundSettings, SoundSource, VOLUME_STEP, allowed,
};
