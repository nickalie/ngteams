//! Device discovery must not hold up account sync or the first window. Opens wait off the UI thread; directory
//! reads and choices stay immediate, and discovery's completion wakes both async callers and directory watchers.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use super::catalog::{Choices, DeviceDirectory, DeviceKind, DeviceList, DeviceStatus, DeviceWatcher};
use super::{AudioSink, AudioSource, DeviceError, DeviceReady, Devices, VideoSource};

/// A backend whose discovery can be deferred.
pub trait Backend: Devices + DeviceDirectory {}
impl<T: Devices + DeviceDirectory> Backend for T {}

type Connected = Result<Arc<dyn Backend>, String>;

struct State {
    connected: Option<Connected>,
    choices: Choices,
}

struct Shared {
    state: Mutex<State>,
    opened: Condvar,
    ready: watch::Sender<bool>,
    watchers: Mutex<Watchers>,
}

#[derive(Default)]
struct Watchers {
    revision: u64,
    callbacks: Vec<DeviceWatcher>,
}

impl Watchers {
    fn tell(&mut self, list: &DeviceList) {
        self.revision = self.revision.wrapping_add(1);
        for watcher in &self.callbacks {
            watcher(list);
        }
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn tell(&self, list: &DeviceList) {
        self.watchers.lock().unwrap_or_else(PoisonError::into_inner).tell(list);
    }

    fn finish(self: &Arc<Self>, connected: Connected) {
        if let Err(error) = &connected {
            log::warn!("no calls: {error}");
            eprintln!("teams: no calls: {error}");
        }
        if let Ok(backend) = &connected {
            let weak = Arc::downgrade(self);
            backend.watch(Box::new(move |list| {
                if let Some(shared) = weak.upgrade() {
                    shared.tell(list);
                }
            }));
        }
        let mut state = self.lock();
        if let Ok(backend) = &connected {
            for kind in DeviceKind::ALL {
                backend.choose(kind, state.choices.get(kind));
            }
        }
        state.connected = Some(connected);
        drop(state);
        self.opened.notify_all();
        self.ready.send_replace(true);
        let revision = self.watchers.lock().unwrap_or_else(PoisonError::into_inner).revision;
        let backend = self.lock().connected.clone();
        let list = backend
            .and_then(Result::ok)
            .map(|backend| backend.devices())
            .unwrap_or_default();
        let mut watchers = self.watchers.lock().unwrap_or_else(PoisonError::into_inner);
        // Reading outside this lock avoids inverting a backend's directory and watcher locks.
        if watchers.revision == revision {
            watchers.tell(&list);
        }
    }
}

/// Shared call devices that connect on their own thread. A failed connection is permanent for this app run.
#[derive(Clone)]
pub struct DeferredDevices {
    shared: Arc<Shared>,
}

impl DeferredDevices {
    /// Starts discovery and returns immediately, including when discovery fails or its worker cannot start.
    #[must_use]
    pub fn connect<T: Backend + 'static>(connect: impl FnOnce() -> Result<T, DeviceError> + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                connected: None,
                choices: Choices::default(),
            }),
            opened: Condvar::new(),
            ready: watch::channel(false).0,
            watchers: Mutex::new(Watchers::default()),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("call-devices".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(connect))
                    .map_err(|_| "media setup stopped unexpectedly".to_owned())
                    .and_then(|result| {
                        result
                            .map(|backend| Arc::new(backend) as Arc<dyn Backend>)
                            .map_err(|error| error.to_string())
                    });
                worker.finish(result);
            });
        if let Err(error) = spawned {
            shared.finish(Err(error.to_string()));
        }
        Self { shared }
    }

    fn backend(&self) -> Result<Arc<dyn Backend>, DeviceError> {
        let mut state = self.shared.lock();
        loop {
            if let Some(result) = &state.connected {
                return result.clone().map_err(DeviceError::Refused);
            }
            state = self.shared.opened.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl Devices for DeferredDevices {
    fn ready(&self) -> DeviceReady<'_> {
        Box::pin(async move {
            let mut ready = self.shared.ready.subscribe();
            ready.wait_for(|ready| *ready).await.map_err(|_| DeviceError::Closed)?;
            self.backend().map(|_| ())
        })
    }

    fn preparing(&self) -> bool {
        self.status() == DeviceStatus::Preparing
    }

    fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
        self.backend()?.microphone()
    }
    fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
        self.backend()?.speaker()
    }
    fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.backend()?.camera()
    }
    fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
        self.backend()?.screen()
    }
}

impl DeviceDirectory for DeferredDevices {
    fn status(&self) -> DeviceStatus {
        match &self.shared.lock().connected {
            None => DeviceStatus::Preparing,
            Some(Ok(_)) => DeviceStatus::Ready,
            Some(Err(_)) => DeviceStatus::Unavailable,
        }
    }

    fn devices(&self) -> DeviceList {
        let backend = self.shared.lock().connected.clone();
        backend
            .and_then(Result::ok)
            .map(|backend| backend.devices())
            .unwrap_or_default()
    }

    fn watch(&self, watcher: DeviceWatcher) {
        self.shared
            .watchers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .callbacks
            .push(watcher);
    }

    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        let mut state = self.shared.lock();
        state.choices.set(kind, name);
        if let Some(Ok(backend)) = &state.connected {
            backend.choose(kind, name);
        }
    }

    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        self.shared.lock().choices.get(kind).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::super::catalog::{DeviceInfo, FixedDirectory};
    use super::super::file::{AudioInput, FileDevices, Pacing, PatternCamera};
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    struct Silent {
        directory: FixedDirectory,
        files: FileDevices,
        snapshot_pause: Mutex<Option<SnapshotPause>>,
    }

    struct SnapshotPause {
        taken: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }

    impl Silent {
        fn new(directory: FixedDirectory) -> Self {
            Self {
                directory,
                snapshot_pause: Mutex::new(None),
                files: FileDevices {
                    input: AudioInput::Silence,
                    recording: None,
                    camera: PatternCamera::default(),
                    pacing: Pacing::Unpaced,
                },
            }
        }
    }
    impl Devices for Silent {
        fn microphone(&self) -> Result<Box<dyn AudioSource>, DeviceError> {
            self.files.microphone()
        }
        fn speaker(&self) -> Result<Box<dyn AudioSink>, DeviceError> {
            self.files.speaker()
        }
        fn camera(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
            self.files.camera()
        }
        fn screen(&self) -> Result<Box<dyn VideoSource>, DeviceError> {
            Err(DeviceError::Closed)
        }
    }
    impl DeviceDirectory for Silent {
        fn devices(&self) -> DeviceList {
            let list = self.directory.devices();
            if let Some(pause) = self.snapshot_pause.lock().unwrap().take() {
                pause.taken.send(()).unwrap();
                pause.resume.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            list
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
    fn discovery_returns_while_blocked_and_opens_wait_then_use_the_remembered_choices() {
        let directory = FixedDirectory::default();
        let backend = Silent::new(directory.clone());
        let (release, held) = mpsc::channel();
        let devices = DeferredDevices::connect(move || {
            held.recv().unwrap();
            Ok(backend)
        });
        assert_eq!(devices.status(), DeviceStatus::Preparing);
        assert!(devices.devices().all().is_empty());
        devices.choose(DeviceKind::Camera, Some("camera"));
        assert_eq!(devices.chosen(DeviceKind::Camera).as_deref(), Some("camera"));
        let (told, changes) = mpsc::channel();
        devices.watch(Box::new(move |list| {
            told.send(list.clone()).unwrap();
        }));
        let opener = devices.clone();
        let (opened, result) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = opener
                .microphone()
                .and_then(|_| opener.speaker())
                .and_then(|_| opener.camera());
            opened.send(result.is_ok()).unwrap();
        });
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        assert!(result.recv_timeout(Duration::from_secs(5)).unwrap());
        worker.join().unwrap();
        assert_eq!(devices.status(), DeviceStatus::Ready);
        assert_eq!(directory.chosen(DeviceKind::Camera).as_deref(), Some("camera"));
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        directory.set_devices(DeviceList::new(vec![DeviceInfo {
            kind: DeviceKind::Camera,
            name: "camera".to_owned(),
            description: "Camera".to_owned(),
        }]));
        assert_eq!(changes.recv_timeout(Duration::from_secs(5)).unwrap().all().len(), 1);
        assert_eq!(devices.devices().all().len(), 1);
        devices.choose(DeviceKind::Camera, None);
        assert_eq!(directory.chosen(DeviceKind::Camera), None);
        assert!(matches!(devices.screen(), Err(DeviceError::Closed)));
        futures_executor::block_on(devices.ready()).unwrap();
    }

    #[test]
    fn a_hotplug_update_supersedes_the_discovery_snapshot_before_it_is_published() {
        let directory = FixedDirectory::default();
        let backend = Silent::new(directory.clone());
        let (taken, snapshot) = mpsc::channel();
        let (resume, held) = mpsc::channel();
        *backend.snapshot_pause.lock().unwrap() = Some(SnapshotPause { taken, resume: held });
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                connected: None,
                choices: Choices::default(),
            }),
            opened: Condvar::new(),
            ready: watch::channel(false).0,
            watchers: Mutex::new(Watchers::default()),
        });
        let devices = DeferredDevices { shared };
        let (told, changes) = mpsc::channel();
        devices.watch(Box::new(move |list| told.send(list.clone()).unwrap()));
        let discovery = Arc::clone(&devices.shared);
        let worker = std::thread::spawn(move || discovery.finish(Ok(Arc::new(backend))));
        snapshot.recv_timeout(Duration::from_secs(5)).unwrap();
        let plugged = DeviceList::new(vec![DeviceInfo {
            kind: DeviceKind::Camera,
            name: "camera".to_owned(),
            description: "Camera".to_owned(),
        }]);
        directory.set_devices(plugged.clone());
        assert_eq!(changes.recv_timeout(Duration::from_secs(5)).unwrap(), plugged);
        resume.send(()).unwrap();
        worker.join().unwrap();
        assert!(matches!(changes.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(devices.devices(), plugged);
    }

    #[test]
    fn failure_wakes_opens_and_async_waiters_and_notifies_even_with_an_empty_list() {
        let (release, held) = mpsc::channel();
        let devices = DeferredDevices::connect::<Silent>(move || {
            held.recv().unwrap();
            Err(DeviceError::Refused("synthetic unavailable devices".to_owned()))
        });
        let (told, changes) = mpsc::channel();
        devices.watch(Box::new(move |list| {
            told.send(list.clone()).unwrap();
        }));
        let waiting = devices.clone();
        let worker = std::thread::spawn(move || futures_executor::block_on(waiting.ready()));
        release.send(()).unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(worker.join().unwrap().is_err());
        assert_eq!(devices.status(), DeviceStatus::Unavailable);
        assert!(devices.microphone().is_err());
        assert!(devices.speaker().is_err());
        assert!(devices.camera().is_err());
        assert!(devices.screen().is_err());
        assert!(devices.devices().all().is_empty());
        devices.choose(DeviceKind::Speaker, Some("saved"));
        assert_eq!(devices.chosen(DeviceKind::Speaker).as_deref(), Some("saved"));
    }
}
