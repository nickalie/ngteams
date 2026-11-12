//! Which devices there are and which one each kind uses: the list a backend sees (microphones, speakers and cameras,
//! with the system's default of each), and the user's choice of each kind, the system's default unless a device is
//! named.
//!
//! A [`DeviceDirectory`] is what the settings talk to: it lists the devices ([`DeviceDirectory::devices`]), tells every
//! change of that list ([`DeviceDirectory::watch`]: a device added or removed, another default), and takes the user's
//! choice ([`DeviceDirectory::choose`]), which the backend's devices follow at once, the open ones included: a call's
//! microphone, speaker and camera move to the device chosen without the call noticing. "Follow the system"
//! (`None`) leaves the choice to the desktop's session manager, so a device added or removed moves what follows it as
//! the system's default moves.

use std::sync::{Arc, Mutex, PoisonError};

/// What kind of device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceKind {
    Microphone,
    Speaker,
    Camera,
}

impl DeviceKind {
    pub const ALL: [Self; 3] = [Self::Microphone, Self::Speaker, Self::Camera];

    /// Its name, as a settings file or a probe's output gives it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Speaker => "speaker",
            Self::Camera => "camera",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Microphone => 0,
            Self::Speaker => 1,
            Self::Camera => 2,
        }
    }
}

/// One device as the backend names it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeviceInfo {
    pub kind: DeviceKind,
    /// The backend's stable identifier (a PipeWire node name or Windows endpoint/device path): what a choice keeps.
    pub name: String,
    /// What a person reads: the device's friendly description, else its identifier.
    pub description: String,
}

/// The devices there are, each kind's in a stable order (by description, then name), and the system's default of each
/// kind by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceList {
    devices: Vec<DeviceInfo>,
    defaults: [Option<String>; 3],
}

impl DeviceList {
    /// `devices`, sorted, with no default.
    #[must_use]
    pub fn new(mut devices: Vec<DeviceInfo>) -> Self {
        devices.sort_by(|a, b| (a.kind, &a.description, &a.name).cmp(&(b.kind, &b.description, &b.name)));
        devices.dedup();
        Self {
            devices,
            defaults: [None, None, None],
        }
    }

    /// The list with `name` as the system's default `kind` (`None`: no default).
    #[must_use]
    pub fn with_default(mut self, kind: DeviceKind, name: Option<&str>) -> Self {
        self.defaults[kind.index()] = name.map(str::to_owned);
        self
    }

    /// Every device, microphones first, then speakers, then cameras.
    #[must_use]
    pub fn all(&self) -> &[DeviceInfo] {
        &self.devices
    }

    /// The devices of `kind`.
    pub fn of(&self, kind: DeviceKind) -> impl Iterator<Item = &DeviceInfo> {
        self.devices.iter().filter(move |device| device.kind == kind)
    }

    /// The device of `kind` named `name`, if it is there.
    #[must_use]
    pub fn find(&self, kind: DeviceKind, name: &str) -> Option<&DeviceInfo> {
        self.of(kind).find(|device| device.name == name)
    }

    /// The name of the system's default `kind`, as the session manager says it (it may name a device not listed yet).
    #[must_use]
    pub fn default_name(&self, kind: DeviceKind) -> Option<&str> {
        self.defaults[kind.index()].as_deref()
    }

    /// The system's default `kind`, when it is listed.
    #[must_use]
    pub fn default_of(&self, kind: DeviceKind) -> Option<&DeviceInfo> {
        self.default_name(kind).and_then(|name| self.find(kind, name))
    }
}

/// Told every new list of devices, on the backend's own thread: it must return at once.
pub type DeviceWatcher = Box<dyn Fn(&DeviceList) + Send>;

/// Whether the device backend has finished discovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeviceStatus {
    Preparing,
    #[default]
    Ready,
    Unavailable,
}

/// The devices a backend sees and the user's choice of each kind (see the module docs).
pub trait DeviceDirectory: Send + Sync {
    /// Discovery's status; changes notify the directory watchers even if its list stays empty.
    fn status(&self) -> DeviceStatus {
        DeviceStatus::Ready
    }

    /// The devices there now.
    fn devices(&self) -> DeviceList;

    /// Tells `watcher` each new list from now on (not the one there now).
    fn watch(&self, watcher: DeviceWatcher);

    /// Uses the device named `name` for `kind` from now on, the open devices of that kind moved to it; `None` follows
    /// the system's default. A name that isn't listed is kept: the system's default stands in until it comes.
    fn choose(&self, kind: DeviceKind, name: Option<&str>);

    /// The device chosen for `kind`; `None` follows the system.
    fn chosen(&self, kind: DeviceKind) -> Option<String>;
}

/// The choices of each kind, as a backend keeps them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices([Option<String>; 3]);

impl Choices {
    #[must_use]
    pub fn get(&self, kind: DeviceKind) -> Option<&str> {
        self.0[kind.index()].as_deref()
    }

    /// Sets `kind`'s choice; whether it changed.
    pub fn set(&mut self, kind: DeviceKind, name: Option<&str>) -> bool {
        let slot = &mut self.0[kind.index()];
        if slot.as_deref() == name {
            return false;
        }
        *slot = name.map(str::to_owned);
        true
    }
}

/// A directory holding a list it is given, for the views' tests: it moves nothing, and every choice is kept in order
/// ([`FixedDirectory::choices_made`]). The file backend lists no devices (the app gives its settings no directory).
#[derive(Clone, Default)]
pub struct FixedDirectory {
    shared: Arc<Mutex<Fixed>>,
}

#[derive(Default)]
struct Fixed {
    status: DeviceStatus,
    list: DeviceList,
    choices: Choices,
    made: Vec<(DeviceKind, Option<String>)>,
    watchers: Vec<DeviceWatcher>,
}

impl std::fmt::Debug for FixedDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedDirectory").finish_non_exhaustive()
    }
}

impl FixedDirectory {
    #[must_use]
    pub fn new(list: DeviceList) -> Self {
        let directory = Self::default();
        directory.lock().list = list;
        directory
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Fixed> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the list (a device added or removed) and tells every watcher.
    pub fn set_devices(&self, list: DeviceList) {
        let mut fixed = self.lock();
        fixed.list = list;
        let Fixed { list, watchers, .. } = &*fixed;
        for watcher in watchers {
            watcher(list);
        }
    }

    /// Changes discovery's status and notifies watchers, for pending and failed view states.
    pub fn set_status(&self, status: DeviceStatus) {
        let mut fixed = self.lock();
        fixed.status = status;
        for watcher in &fixed.watchers {
            watcher(&fixed.list);
        }
    }

    /// Every choice made, in order, the ones that changed nothing included.
    #[must_use]
    pub fn choices_made(&self) -> Vec<(DeviceKind, Option<String>)> {
        self.lock().made.clone()
    }
}

impl DeviceDirectory for FixedDirectory {
    fn status(&self) -> DeviceStatus {
        self.lock().status
    }

    fn devices(&self) -> DeviceList {
        self.lock().list.clone()
    }

    fn watch(&self, watcher: DeviceWatcher) {
        self.lock().watchers.push(watcher);
    }

    fn choose(&self, kind: DeviceKind, name: Option<&str>) {
        let mut fixed = self.lock();
        fixed.choices.set(kind, name);
        fixed.made.push((kind, name.map(str::to_owned)));
    }

    fn chosen(&self, kind: DeviceKind) -> Option<String> {
        self.lock().choices.get(kind).map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(kind: DeviceKind, name: &str, description: &str) -> DeviceInfo {
        DeviceInfo {
            kind,
            name: name.to_owned(),
            description: description.to_owned(),
        }
    }

    #[test]
    fn a_list_is_ordered_by_kind_then_description_and_names_each_default() {
        let list = DeviceList::new(vec![
            device(DeviceKind::Camera, "cam", "Camera"),
            device(DeviceKind::Microphone, "usb", "USB headset"),
            device(DeviceKind::Speaker, "spk", "Speakers"),
            device(DeviceKind::Microphone, "built-in", "Built-in microphone"),
            device(DeviceKind::Microphone, "usb", "USB headset"),
        ])
        .with_default(DeviceKind::Microphone, Some("usb"))
        .with_default(DeviceKind::Camera, Some("gone"));
        let names: Vec<&str> = list.all().iter().map(|device| device.name.as_str()).collect();
        assert_eq!(names, ["built-in", "usb", "spk", "cam"], "a duplicate is listed once");
        assert_eq!(list.of(DeviceKind::Microphone).count(), 2);
        assert_eq!(
            list.default_of(DeviceKind::Microphone)
                .map(|device| device.description.as_str()),
            Some("USB headset")
        );
        assert_eq!(list.default_name(DeviceKind::Camera), Some("gone"));
        assert_eq!(list.default_of(DeviceKind::Camera), None, "a default not listed yet");
        assert_eq!(list.default_name(DeviceKind::Speaker), None);
    }

    #[test]
    fn a_fixed_directory_keeps_choices_and_tells_watchers_each_new_list() {
        let directory = FixedDirectory::new(DeviceList::default());
        let told = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&told);
        directory.watch(Box::new(move |list| {
            seen.lock().unwrap().push(list.all().len());
        }));
        directory.set_devices(DeviceList::new(vec![device(DeviceKind::Speaker, "spk", "Speakers")]));
        assert_eq!(*told.lock().unwrap(), [1]);
        assert_eq!(directory.chosen(DeviceKind::Speaker), None, "the system's by default");
        directory.choose(DeviceKind::Speaker, Some("spk"));
        directory.choose(DeviceKind::Speaker, None);
        assert_eq!(directory.chosen(DeviceKind::Speaker), None);
        assert_eq!(
            directory.choices_made(),
            [
                (DeviceKind::Speaker, Some("spk".to_owned())),
                (DeviceKind::Speaker, None)
            ]
        );
    }

    #[test]
    fn a_choice_says_whether_it_changed() {
        let mut choices = Choices::default();
        assert!(choices.set(DeviceKind::Camera, Some("cam")));
        assert!(!choices.set(DeviceKind::Camera, Some("cam")));
        assert_eq!(choices.get(DeviceKind::Camera), Some("cam"));
        assert_eq!(choices.get(DeviceKind::Microphone), None);
        assert!(choices.set(DeviceKind::Camera, None));
    }
}
