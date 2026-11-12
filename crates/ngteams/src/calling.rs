//! The personal build's calls (`docs/personal-build.md`, "Calls"): the account's engine gets its calling
//! ([`account_calls`]: the calls' media on the devices the app opened, relayed through Teams'
//! relay, the microphone processed as a call's is, the account describing its client as the web client does), and the
//! window shows calls
//! ([`PersonalCalls`]: the ringtone on the same devices, the test call to the echo service, a call's video, the
//! devices the settings choose among).
//!
//! - **The devices** are the native backend selected by [`Reach`]: PipeWire and the camera portal on Linux,
//!   WASAPI/Media Foundation and the system screen picker on Windows. Sandbox reach selects Linux's private server
//!   or validates the explicitly authorized Windows VM console. Read-only startup opens no devices.
//!   Discovery runs in the background; calls wait for it and stay unavailable if it fails.
//! - **The test call** goes to the echo service by its MRI from the web bundle ([`echo`],
//!   `teams_proto::TEAMS_ECHO_BOT_MRI`), never one taken from the network or the UI.
//! - **Video**: a call takes video on the OpenH264 built into the app; the settings' switch turns it off and on.
//!
//! The engine's call state and shared call floor control call admission; nothing here sends requests.

use std::sync::Arc;

use teams_core::account::{AccountCalls, account_calls, call_video};
use teams_media::devices::Devices;
use teams_media::devices::Reach;
#[cfg(not(test))]
use teams_media::devices::SystemDevices;
use teams_media::devices::catalog::DeviceDirectory;
use teams_media::devices::deferred::DeferredDevices;
use teams_media::player::PlayerSetup;
use teams_proto::Mri;
use teams_ui::sounds::SoundBoard;
use teams_ui::{Ringer, VideoFeed};

#[cfg(test)]
#[path = "../tests/support/calling_devices.rs"]
mod tests;
#[cfg(test)]
use tests::SystemDevices;

/// What the window's calls are made of.
#[derive(Clone)]
pub struct PersonalCalls {
    /// What plays an incoming call's ringtone; `None` plays none.
    pub ringer: Option<Arc<dyn Ringer>>,
    /// What plays the app's sounds, the ringtone among them, as the settings say; `None` plays none.
    pub sounds: Option<Arc<SoundBoard>>,
    /// The echo service the test call goes to ([`echo`]).
    pub echo: Mri,
    /// Where a call's video is shown; `None`: the calls carry no video.
    pub video: Option<VideoFeed>,
    /// The camera the video effects' preview opens (the settings' camera); `None`: no preview.
    pub camera: Option<Arc<dyn Devices + Send + Sync>>,
    /// The devices the settings list and choose among; `None` lists none.
    pub devices: Option<Arc<dyn DeviceDirectory>>,
}

impl std::fmt::Debug for PersonalCalls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalCalls")
            .field("ringer", &self.ringer.is_some())
            .field("sounds", &self.sounds.is_some())
            .field("video", &self.video.is_some())
            .field("camera", &self.camera.is_some())
            .field("devices", &self.devices.is_some())
            .finish_non_exhaustive()
    }
}

/// The echo service the personal build's test call goes to: the web bundle's `teamsEchoBotMri`.
#[must_use]
pub fn echo() -> Mri {
    Mri::new(teams_proto::TEAMS_ECHO_BOT_MRI)
}

/// The engine's calling and the window's calls on `devices`, the calls taking video when `video` (none without).
#[must_use]
pub fn on_devices(devices: Arc<dyn Devices + Send + Sync>, video: bool) -> (AccountCalls, PersonalCalls) {
    let sounds = Arc::new(SoundBoard::new(Arc::clone(&devices)));
    let ringer: Arc<dyn Ringer> = sounds.clone();
    let feed = video.then(VideoFeed::new);
    let camera = video.then(|| Arc::clone(&devices));
    let video = feed.clone().map(|feed| call_video(Arc::new(feed)));
    (
        account_calls(devices, video),
        PersonalCalls {
            ringer: Some(ringer),
            sounds: Some(sounds),
            echo: echo(),
            video: feed,
            camera,
            devices: None,
        },
    )
}

/// The calls' devices of the app, opened once and shared by every account (`docs/personal-build.md`, "Several
/// accounts"): the native devices selected by the caller.
#[derive(Clone)]
pub struct CallDevices {
    devices: Arc<DeferredDevices>,
    /// The app's sounds, one board for every account: one setting, one ringtone at a time.
    sounds: Arc<SoundBoard>,
}

impl std::fmt::Debug for CallDevices {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallDevices").finish_non_exhaustive()
    }
}

impl CallDevices {
    /// Starts native device discovery without delaying account engines or the first window.
    #[must_use]
    pub fn open(reach: Reach) -> Self {
        let devices = Arc::new(DeferredDevices::connect(move || SystemDevices::connect(reach)));
        let shared: Arc<dyn Devices + Send + Sync> = devices.clone();
        let sounds = Arc::new(SoundBoard::new(shared));
        Self { devices, sounds }
    }

    /// What a video a message carries is played with: its sound on these devices.
    #[must_use]
    pub fn playback(&self) -> PlayerSetup {
        PlayerSetup {
            speaker: Some(self.devices.clone()),
        }
    }

    /// One account's engine calling and window calls on these devices (its own video feed).
    #[must_use]
    pub fn for_account(&self) -> (AccountCalls, PersonalCalls) {
        let devices: Arc<dyn Devices + Send + Sync> = self.devices.clone();
        let (engine, mut window) = on_devices(devices, true);
        window.devices = Some(self.devices.clone());
        let ringer: Arc<dyn Ringer> = self.sounds.clone();
        window.ringer = Some(ringer);
        window.sounds = Some(self.sounds.clone());
        (engine, window)
    }
}

/// The engine's calling and the window's calls on the native devices selected by `reach` (see the module docs).
/// Discovery runs in the background; a call waits for it before starting.
#[must_use]
pub fn open(reach: Reach) -> (AccountCalls, PersonalCalls) {
    CallDevices::open(reach).for_account()
}
