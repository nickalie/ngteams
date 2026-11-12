use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use gst::glib;
use gst::subclass::prelude::*;

use super::*;

mod synthetic {
    use super::*;
    #[derive(Default)]
    pub struct Device;
    #[glib::object_subclass]
    impl ObjectSubclass for Device {
        const NAME: &'static str = "NgteamsSyntheticCaptureDevice";
        type Type = super::SyntheticDevice;
        type ParentType = gst::Device;
    }
    impl ObjectImpl for Device {}
    impl GstObjectImpl for Device {}
    impl DeviceImpl for Device {
        fn create_element(&self, _name: Option<&str>) -> Result<gst::Element, gst::LoggableError> {
            let obj = self.obj();
            let label = obj.display_name();
            let element = match label.as_str() {
                "microphone-a" => gst::ElementFactory::make("audiotestsrc")
                    .property("is-live", true)
                    .property("freq", 440.0),
                "microphone-b" => gst::ElementFactory::make("audiotestsrc")
                    .property("is-live", true)
                    .property("freq", 880.0),
                "camera" => gst::ElementFactory::make("videotestsrc").property("is-live", true),
                "finite-camera" => gst::ElementFactory::make("videotestsrc")
                    .property("is-live", true)
                    .property("num-buffers", 1i32),
                _ => gst::ElementFactory::make("fakesink"),
            };
            element.build().map_err(|e| gst::loggable_error!(gst::CAT_RUST, "{e}"))
        }
    }
}
glib::wrapper! { pub struct SyntheticDevice(ObjectSubclass<synthetic::Device>) @extends gst::Device, gst::Object; }

fn device(kind: DeviceKind, name: &str, default: bool) -> Entry {
    gst::init().unwrap();
    let device: SyntheticDevice = glib::Object::builder()
        .property("display-name", name)
        .property("device-class", "Synthetic/Offline")
        .build();
    Entry {
        info: DeviceInfo {
            kind,
            name: name.into(),
            description: name.into(),
        },
        device: device.upcast(),
        default,
        actual_id: None,
    }
}

fn devices(entries: Vec<Entry>) -> WindowsDevices {
    let shared = Arc::new(Shared::default());
    shared.update(entries);
    WindowsDevices {
        shared,
        _monitor: Arc::new(Monitor {
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }),
    }
}

#[test]
fn windows_sandbox_reach_refuses_a_process_outside_the_authorized_console() {
    assert!(matches!(
        WindowsDevices::connect(Reach::Sandbox),
        Err(DeviceError::Refused(_))
    ));
}

#[test]
fn catalog_follows_defaults_retains_missing_choices_and_notifies_only_list_changes() {
    let a = device(DeviceKind::Microphone, "microphone-a", true);
    let b = device(DeviceKind::Microphone, "microphone-b", false);
    let devices = devices(vec![a.clone(), b.clone(), device(DeviceKind::Camera, "camera", false)]);
    let tells = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&tells);
    devices.watch(Box::new(move |_| {
        count.fetch_add(1, Ordering::Relaxed);
    }));
    assert_eq!(devices.devices().default_name(DeviceKind::Camera), Some("camera"));
    devices.choose(DeviceKind::Microphone, Some("microphone-b"));
    assert_eq!(
        devices.shared.selected(DeviceKind::Microphone).unwrap().info.name,
        "microphone-b"
    );
    devices.shared.update(vec![a.clone()]);
    assert_eq!(devices.chosen(DeviceKind::Microphone).as_deref(), Some("microphone-b"));
    assert_eq!(
        devices.shared.selected(DeviceKind::Microphone).unwrap().info.name,
        "microphone-a"
    );
    devices.shared.update(vec![a.clone()]);
    assert_eq!(tells.load(Ordering::Relaxed), 1);
    devices.shared.update(vec![a, b]);
    assert_eq!(
        devices.shared.selected(DeviceKind::Microphone).unwrap().info.name,
        "microphone-b"
    );
    devices.choose(DeviceKind::Microphone, None);
    assert_eq!(
        devices.shared.selected(DeviceKind::Microphone).unwrap().info.name,
        "microphone-a"
    );
}

fn frequency(microphone: &mut dyn AudioSource) -> u32 {
    let mut samples = Vec::new();
    for _ in 0..12 {
        let frame = microphone.next_frame(Duration::from_secs(2)).unwrap().unwrap();
        samples.extend_from_slice(frame.samples());
    }
    let a = crate::tone::amplitude_at(&samples, 440.0);
    let b = crate::tone::amplitude_at(&samples, 880.0);
    if a > b { 440 } else { 880 }
}

#[test]
fn a_live_microphone_switches_to_the_choice_then_falls_back_and_returns_on_hotplug() {
    let a = device(DeviceKind::Microphone, "microphone-a", true);
    let b = device(DeviceKind::Microphone, "microphone-b", false);
    let devices = devices(vec![a.clone(), b.clone()]);
    let mut microphone = devices.microphone().unwrap();
    assert_eq!(frequency(&mut *microphone), 440);
    for (entries, choice, hz) in [
        (vec![a.clone(), b.clone()], Some("microphone-b"), 880),
        (vec![a.clone()], Some("microphone-b"), 440),
        (vec![a, b], Some("microphone-b"), 880),
    ] {
        devices.choose(DeviceKind::Microphone, choice);
        devices.shared.update(entries);
        let deadline = Instant::now() + Duration::from_secs(5);
        while frequency(&mut *microphone) != hz {
            assert!(Instant::now() < deadline);
        }
    }
}

#[test]
fn camera_frames_and_speaker_queues_use_the_existing_device_contracts() {
    let devices = devices(vec![
        device(DeviceKind::Camera, "camera", true),
        device(DeviceKind::Speaker, "speaker", true),
    ]);
    let mut camera = devices.camera().unwrap();
    let frame = camera.next_frame(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(frame.format, super::super::PixelFormat::Bgra);
    assert_eq!(frame.stride, frame.width * 4);
    assert_eq!(frame.data.len(), (frame.stride * frame.height) as usize);
    let mut speaker = devices.speaker().unwrap();
    for _ in 0..100 {
        speaker.play(&super::super::AudioFrame::silence()).unwrap();
    }
}

#[test]
fn unsupported_provider_devices_are_excluded() {
    let entry = device(DeviceKind::Microphone, "microphone-a", false);
    assert!(super::entry(&entry.device).is_none());
}

#[test]
fn a_failed_open_reports_the_failure_and_an_ended_source_closes_its_queue() {
    let broken = devices(vec![device(DeviceKind::Camera, "broken", true)]);
    assert!(broken.camera().is_err());
    let finite = devices(vec![device(DeviceKind::Camera, "finite-camera", true)]);
    let mut source = finite.camera().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match source.next_frame(Duration::from_millis(50)) {
            Err(DeviceError::Closed) => break,
            Ok(_) => assert!(Instant::now() < deadline),
            Err(failure) => panic!("{failure}"),
        }
    }
}

#[test]
fn a_virtual_default_alias_is_reopened_when_its_actual_endpoint_changes() {
    let mut a = device(DeviceKind::Microphone, "microphone-a", true);
    a.actual_id = Some("endpoint-a".into());
    let devices = devices(vec![a]);
    let mut microphone = devices.microphone().unwrap();
    assert_eq!(frequency(&mut *microphone), 440);
    let mut b = device(DeviceKind::Microphone, "microphone-b", true);
    b.info.name = "microphone-a".into();
    b.actual_id = Some("endpoint-b".into());
    devices.shared.update(vec![b]);
    assert_eq!(
        devices.devices().default_name(DeviceKind::Microphone),
        Some("endpoint-b")
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while frequency(&mut *microphone) != 880 {
        assert!(Instant::now() < deadline);
    }
}

#[cfg(windows)]
#[test]
fn native_providers_are_installed_and_enumeration_never_needs_a_capture_stream() {
    gst::init().unwrap();
    for name in ["wasapi2src", "wasapi2sink", "mfvideosrc"] {
        assert!(
            gst::ElementFactory::find(name).is_some(),
            "missing native media plugin: {name}"
        );
    }
    let devices = WindowsDevices::connect(Reach::Desktop).unwrap();
    assert!(devices.devices().all().iter().all(|device| !device.name.is_empty()));
    devices.choose(DeviceKind::Microphone, Some("synthetic-missing-endpoint"));
    assert_eq!(
        devices.chosen(DeviceKind::Microphone).as_deref(),
        Some("synthetic-missing-endpoint")
    );
    devices.choose(DeviceKind::Microphone, None);
    assert!(devices.chosen(DeviceKind::Microphone).is_none());
}

#[test]
fn system_audio_loopbacks_cannot_be_selected_as_microphones() {
    gst::init().unwrap();
    let loopback = gst::Structure::builder("wasapi2-proplist")
        .field("wasapi2.device.loopback", true)
        .build();
    assert_eq!(device_kind("wasapi2src", Some(&loopback)), None);
    let microphone = gst::Structure::builder("wasapi2-proplist")
        .field("wasapi2.device.loopback", false)
        .build();
    assert_eq!(
        device_kind("wasapi2src", Some(&microphone)),
        Some((DeviceKind::Microphone, Some("device")))
    );
    assert_eq!(
        device_kind("wasapi2sink", None),
        Some((DeviceKind::Speaker, Some("device")))
    );
    assert_eq!(
        device_kind("osxaudiosrc", None),
        Some((DeviceKind::Microphone, Some("unique-id")))
    );
    assert_eq!(device_kind("avfvideosrc", None), Some((DeviceKind::Camera, None)));
    assert_eq!(device_kind("pulsesrc", None), None);
}
