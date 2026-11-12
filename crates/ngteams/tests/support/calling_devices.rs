//! Personal call-device controllers over silent file devices; the real backend's sandbox reach is checked separately.

use std::process::Command;
use std::sync::Arc;

use teams_media::devices::Reach;
use teams_media::devices::catalog::{DeviceInfo, DeviceKind, DeviceList, DeviceWatcher, FixedDirectory};
use teams_media::devices::file::{AudioInput, FileDevices, Pacing, PatternCamera};
use teams_media::devices::{AudioSink, AudioSource, DeviceError, Devices, VideoSource};

pub(super) struct SystemDevices {
    files: FileDevices,
    directory: FixedDirectory,
}

impl SystemDevices {
    pub(super) fn connect(reach: Reach) -> Result<Self, DeviceError> {
        Ok(Self {
            files: FileDevices {
                input: AudioInput::Silence,
                recording: None,
                camera: PatternCamera::default(),
                pacing: Pacing::Unpaced,
            },
            directory: FixedDirectory::new(DeviceList::new(vec![DeviceInfo {
                kind: DeviceKind::Microphone,
                name: format!("{reach:?}"),
                description: "Synthetic microphone".to_owned(),
            }])),
        })
    }
}

impl Devices for SystemDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.files.microphone()
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.files.speaker()
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.files.camera()
    }
}

impl teams_media::devices::catalog::DeviceDirectory for SystemDevices {
    fn devices(&self) -> DeviceList {
        self.directory.devices()
    }
    fn watch(&self, watcher: DeviceWatcher) {
        self.directory.watch(watcher);
    }
    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        self.directory.choose(kind, name);
    }
    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        self.directory.chosen(kind)
    }
}

#[test]
fn account_devices_share_the_catalog_and_playback_but_keep_their_video_feeds_separate() {
    let devices = super::CallDevices::open(Reach::Sandbox);
    let (first_engine, first) = devices.for_account();
    let (_, second) = devices.for_account();
    assert_eq!(first.echo, super::echo());
    assert!(first.ringer.is_some() && first.camera.is_some());
    first
        .devices
        .as_ref()
        .unwrap()
        .choose(DeviceKind::Camera, Some("synthetic-camera"));
    assert_eq!(
        second.devices.as_ref().unwrap().chosen(DeviceKind::Camera).as_deref(),
        Some("synthetic-camera")
    );
    assert!(Arc::ptr_eq(
        first.devices.as_ref().unwrap(),
        second.devices.as_ref().unwrap()
    ));
    assert!(format!("{second:?}").contains("camera: true"));
    first.video.unwrap().set_enabled(false);
    assert!(!teams_core::VideoOutput::enabled(
        &*first_engine.media.video.unwrap().output
    ));
    assert!(teams_core::VideoOutput::enabled(&second.video.unwrap()));
    let mut speaker = devices.playback().speaker.unwrap().speaker().unwrap();
    speaker.play(&teams_media::devices::AudioFrame::silence()).unwrap();
    assert!(format!("{devices:?}").contains("CallDevices"));
    let (opened, _) = super::open(Reach::Sandbox);
    assert!(opened.media.video.is_some());
}

#[test]
fn the_real_backend_with_sandbox_reach_refuses_the_desktop() {
    let directory = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", "calling::tests::real_guard_child", "--nocapture"])
        .env_clear()
        .env("NGTEAMS_CALL_DEVICE_GUARD", "1")
        .env("HOME", directory.path());
    for key in ["PATH", "LANG", "LD_LIBRARY_PATH", "LLVM_PROFILE_FILE"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    let output = child.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn real_guard_child() {
    if std::env::var_os("NGTEAMS_CALL_DEVICE_GUARD").is_none() {
        return;
    }
    let actual = teams_media::devices::SystemDevices::connect(Reach::Sandbox);
    assert!(
        matches!(actual, Err(DeviceError::Refused(_))),
        "sandbox reach never opens the desktop device backend"
    );
}

#[test]
fn the_call_controller_passes_the_selected_reach_to_its_background_backend() {
    for reach in [Reach::Desktop, Reach::Sandbox] {
        let devices = super::CallDevices::open(reach);
        devices.playback().speaker.unwrap().speaker().unwrap();
        let (_, calls) = devices.for_account();
        let list = calls.devices.unwrap().devices();
        assert_eq!(list.all()[0].name, format!("{reach:?}"));
        let (engine, calls) = super::open(reach);
        engine.media.devices.speaker().unwrap();
        assert_eq!(calls.devices.unwrap().devices().all()[0].name, format!("{reach:?}"));
    }
}
