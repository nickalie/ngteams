//! The desktop's devices through `GStreamer`'s native providers: WASAPI2 and Media Foundation on Windows, Core Audio
//! (`osxaudio`) and `AVFoundation` (`avfvideosrc`) on macOS. Device monitor events refresh the catalog; each active
//! stream follows the current choice (or the system default) on its own thread. Opening a backend only enumerates
//! devices: it never starts a microphone or camera. Screen sharing separately invokes the Windows picker and captures
//! only its result. Queues remain bounded across a device switch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use gst::prelude::*;
use gstreamer as gst;

use super::catalog::{Choices, DeviceDirectory, DeviceInfo, DeviceKind, DeviceList, DeviceWatcher};
use super::{AudioSink, AudioSource, DeviceError, Devices, Reach, VideoSource};

#[cfg(windows)]
mod screen;
mod streams;
#[cfg(test)]
mod tests;

const POLL: Duration = Duration::from_millis(50);

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

fn error(value: impl std::fmt::Display) -> DeviceError {
    DeviceError::Native(value.to_string())
}

#[derive(Clone)]
struct Entry {
    info: DeviceInfo,
    device: gst::Device,
    default: bool,
    // WASAPI2's virtual default keeps its name when the physical endpoint changes. The actual ID both names
    // the catalog's default and makes an open stream move even when only that virtual entry is listed.
    actual_id: Option<String>,
}

#[derive(PartialEq, Eq)]
struct Key {
    name: String,
    endpoint: Option<String>,
}
impl Entry {
    fn key(&self) -> Key {
        Key {
            name: self.info.name.clone(),
            endpoint: self.actual_id.clone(),
        }
    }
}

#[derive(Default)]
struct Catalog {
    entries: Vec<Entry>,
    list: DeviceList,
    choices: Choices,
}

#[derive(Default)]
struct Shared {
    catalog: Mutex<Catalog>,
    watchers: Mutex<Vec<DeviceWatcher>>,
}

impl Shared {
    fn update(&self, entries: Vec<Entry>) {
        let mut list = DeviceList::new(entries.iter().map(|entry| entry.info.clone()).collect());
        for kind in DeviceKind::ALL {
            let default = entries.iter().find(|entry| entry.info.kind == kind && entry.default);
            // Media Foundation has no default-camera concept. The first stable device stands in.
            let default = default.or_else(|| {
                (kind == DeviceKind::Camera)
                    .then(|| {
                        list.of(kind)
                            .next()
                            .and_then(|info| entries.iter().find(|e| e.info == *info))
                    })
                    .flatten()
            });
            list = list.with_default(
                kind,
                default.map(|entry| entry.actual_id.as_deref().unwrap_or(&entry.info.name)),
            );
        }
        let mut catalog = lock(&self.catalog);
        catalog.entries = entries;
        if catalog.list == list {
            return;
        }
        catalog.list = list.clone();
        drop(catalog);
        for watcher in lock(&self.watchers).iter() {
            watcher(&list);
        }
    }

    fn selected(&self, kind: DeviceKind) -> Option<Entry> {
        let catalog = lock(&self.catalog);
        let chosen = catalog.choices.get(kind).and_then(|name| catalog.list.find(kind, name));
        let info = chosen.or_else(|| catalog.list.default_of(kind)).or_else(|| {
            catalog
                .entries
                .iter()
                .find(|entry| entry.info.kind == kind && entry.default)
                .map(|entry| &entry.info)
        });
        info.and_then(|info| catalog.entries.iter().find(|entry| entry.info == *info))
            .cloned()
    }
}

struct Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The current desktop's devices. Clones and open streams retain the monitor; the last drop stops it.
#[derive(Clone)]
pub struct WindowsDevices {
    shared: Arc<Shared>,
    _monitor: Arc<Monitor>,
}

impl WindowsDevices {
    /// Enumerates devices without opening a capture stream. Sandbox reach requires the authorized VM console.
    ///
    /// # Errors
    /// The console guard refuses the launch, or GStreamer/device enumeration cannot start.
    pub fn connect(reach: Reach) -> Result<Self, DeviceError> {
        if reach == Reach::Sandbox {
            sandbox_allowed()?;
        }
        gst::init().map_err(error)?;
        let monitor = gst::DeviceMonitor::new();
        for class in ["Audio/Source", "Audio/Sink", "Video/Source"] {
            monitor.add_filter(Some(class), None);
        }
        monitor.start().map_err(error)?;
        let shared = Arc::new(Shared::default());
        shared.update(entries(&monitor));
        let directory = Arc::clone(&shared);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::Builder::new().name("windows-devices".into()).spawn(move || {
            let bus = monitor.bus();
            while !stopped.load(Ordering::Acquire) {
                if bus.timed_pop(gst::ClockTime::from_mseconds(50)).is_some() {
                    directory.update(entries(&monitor));
                }
            }
            monitor.stop();
        })?;
        Ok(Self {
            shared,
            _monitor: Arc::new(Monitor {
                stop,
                thread: Some(thread),
            }),
        })
    }
}

#[cfg(windows)]
fn sandbox_allowed() -> Result<(), DeviceError> {
    teams_platform::windows_sandbox::window_allowed_here().map_err(|e| DeviceError::Refused(e.to_string()))
}

#[cfg(target_os = "macos")]
fn sandbox_allowed() -> Result<(), DeviceError> {
    Err(DeviceError::Refused(
        "macOS has no device sandbox: lab devices run in the Linux UI sandbox or the Windows VM console".into(),
    ))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn sandbox_allowed() -> Result<(), DeviceError> {
    Err(DeviceError::Refused(
        "Windows devices require the Windows VM console".into(),
    ))
}

/// The element a device of `kind` is opened with when the system's default is wanted.
fn factory(kind: DeviceKind) -> &'static str {
    match (cfg!(target_os = "macos"), kind) {
        (false, DeviceKind::Microphone) => "wasapi2src",
        (false, DeviceKind::Speaker) => "wasapi2sink",
        (false, DeviceKind::Camera) => "mfvideosrc",
        (true, DeviceKind::Microphone) => "osxaudiosrc",
        (true, DeviceKind::Speaker) => "osxaudiosink",
        (true, DeviceKind::Camera) => "avfvideosrc",
    }
}

fn entries(monitor: &gst::DeviceMonitor) -> Vec<Entry> {
    monitor.devices().iter().filter_map(entry).collect()
}

fn entry(device: &gst::Device) -> Option<Entry> {
    let element = device.create_element(None).ok()?;
    let factory = element.factory()?.name();
    let properties = device.properties();
    let (kind, property) = device_kind(factory.as_str(), properties.as_deref())?;
    // `AVFoundation`'s cameras have no persistent id property on their element: their name stands in
    let name = match property {
        Some(property) => element.property::<Option<String>>(property)?,
        None => device.display_name().to_string(),
    };
    let default = device
        .properties()
        .and_then(|p| p.get::<bool>("device.default").ok())
        .unwrap_or(false);
    let actual_id = device
        .properties()
        .and_then(|p| p.get::<String>("device.actual-id").ok());
    Some(Entry {
        actual_id,
        info: DeviceInfo {
            kind,
            name,
            description: device.display_name().to_string(),
        },
        device: device.clone(),
        default,
    })
}

// WASAPI2 also exposes render endpoints as loopback sources. They are system audio, never microphones; admitting
// their virtual default could capture the speaker instead of the user's microphone.
fn device_kind(factory: &str, properties: Option<&gst::StructureRef>) -> Option<(DeviceKind, Option<&'static str>)> {
    if properties
        .and_then(|p| p.get::<bool>("wasapi2.device.loopback").ok())
        .unwrap_or(false)
    {
        return None;
    }
    match factory {
        "wasapi2src" => Some((DeviceKind::Microphone, Some("device"))),
        "wasapi2sink" => Some((DeviceKind::Speaker, Some("device"))),
        "mfvideosrc" => Some((DeviceKind::Camera, Some("device-path"))),
        "osxaudiosrc" => Some((DeviceKind::Microphone, Some("unique-id"))),
        "osxaudiosink" => Some((DeviceKind::Speaker, Some("unique-id"))),
        "avfvideosrc" => Some((DeviceKind::Camera, None)),
        _ => None,
    }
}

impl DeviceDirectory for WindowsDevices {
    fn devices(&self) -> DeviceList {
        lock(&self.shared.catalog).list.clone()
    }
    fn watch(&self, watcher: DeviceWatcher) {
        lock(&self.shared.watchers).push(watcher);
    }
    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        lock(&self.shared.catalog).choices.set(kind, name);
    }
    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        lock(&self.shared.catalog).choices.get(kind).map(str::to_owned)
    }
}

impl Devices for WindowsDevices {
    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        streams::microphone(self.clone())
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        streams::speaker(self.clone())
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        streams::camera(self.clone())
    }
    #[cfg(windows)]
    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        screen::open()
    }
}
