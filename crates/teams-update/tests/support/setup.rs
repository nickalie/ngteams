//! The Windows installer's update: verified like any download, checked again before it runs, and handed back as the
//! program a restart runs with its unattended-upgrade arguments. The installation is built by hand where Linux runs
//! these (its detection is Windows'); on Windows it is detected.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use super::{places, version};
use crate::fakes::{Feed, publish, tree};
use crate::{Format, IDENTITY, Installation, Prepared, Source};

/// The installer's installation of `version` at `<home>/Programs/ngteams`.
fn setup_installation(home: &Path, version: &str) -> Installation {
    let root = home.join("Programs/ngteams");
    tree(&root, version, Format::WindowsSetup).unwrap();
    detected(&root, home)
}

#[cfg(windows)]
fn detected(root: &Path, home: &Path) -> Installation {
    Installation::detect(&places(root.join("bin/ngteams.exe"), home)).unwrap()
}

#[cfg(unix)]
fn detected(root: &Path, home: &Path) -> Installation {
    let places = places(root.join("bin/ngteams.exe"), home);
    let installation = Installation {
        root: root.to_path_buf(),
        executable: places.executable,
        format: Format::WindowsSetup,
        stage_dir: places.stage_dir,
        protected: places.protected,
        prompter: "pkexec".into(),
    };
    installation.validate().unwrap();
    installation
}

/// Release 0.1.0.6's installer, `bytes`, downloaded and verified for `installation`.
fn prepared(installation: &Installation, bytes: &[u8]) -> (Prepared, Feed) {
    let feed = Feed::start().unwrap();
    let asset = Format::WindowsSetup.asset(version("0.1.0.6"));
    publish(&feed, "v0.1.0.6", &asset, bytes);
    let source = Source::local(&feed.base);
    let release = source.check(installation, version("0.1.0.5")).unwrap().unwrap();
    assert_eq!(release.asset(), asset);
    (source.prepare(installation, &release, |_, _| {}).unwrap(), feed)
}

#[test]
fn the_installer_is_handed_back_to_run_once_the_app_has_quit() {
    let home = tempfile::tempdir().unwrap();
    let installation = setup_installation(home.path(), "0.1.0.5");
    assert_eq!(installation.format(), Format::WindowsSetup);
    let (mut prepared, _feed) = prepared(&installation, b"installer 0.1.0.6");
    // its download waits in the app's own folder, not beside the installation
    assert!(prepared.candidate.starts_with(home.path().join("data/updates")));
    let installer = prepared.candidate.clone();
    let restart = prepared.install().unwrap();
    assert_eq!(restart.program, installer);
    // The installer starts in its own folder: Windows can't move a folder that a process works in, so an installer
    // that inherits the app's folder (`bin`, where the Start menu starts it) can't move it aside to upgrade it.
    let directory = restart.directory.clone().unwrap();
    assert_eq!(directory, installer.parent().unwrap());
    assert!(!directory.starts_with(installation.root()), "{}", directory.display());
    let arguments = restart.arguments.unwrap();
    let mut directory = OsString::from("/DIR=");
    directory.push(installation.root());
    for expected in [
        OsString::from("/VERYSILENT"),
        OsString::from("/SUPPRESSMSGBOXES"),
        OsString::from("/NORESTART"),
        OsString::from(format!("/UPDATEPID={}", std::process::id())),
        directory,
    ] {
        assert!(arguments.contains(&expected), "{expected:?} in {arguments:?}");
    }
    let log = arguments
        .iter()
        .find_map(|argument| argument.to_str()?.strip_prefix("/LOG="))
        .unwrap();
    assert!(Path::new(log).starts_with(installer.parent().unwrap()), "{log}");
    // nothing is installed before the app quits, and the installer stays to be run
    assert_eq!(
        fs::read_to_string(installation.root().join(IDENTITY)).unwrap(),
        "0.1.0.5\n"
    );
    assert_eq!(fs::read(&installer).unwrap(), b"installer 0.1.0.6");
    assert!(prepared.install().is_err(), "an update goes in once");
}

#[test]
fn an_installer_changed_after_its_check_is_not_run() {
    let home = tempfile::tempdir().unwrap();
    let installation = setup_installation(home.path(), "0.1.0.5");
    let (mut prepared, _feed) = prepared(&installation, b"installer 0.1.0.6");
    fs::write(&prepared.candidate, b"something else").unwrap();
    let error = prepared.install().unwrap_err().to_string();
    assert!(error.contains("changed after it was verified"), "{error}");
    // updated by something else meanwhile: refused before the installer is read
    let (mut again, _feed) = prepared_again(&installation);
    fs::write(installation.root().join(IDENTITY), "0.1.0.6\n").unwrap();
    let error = again.install().unwrap_err().to_string();
    assert!(error.contains("updated by something else"), "{error}");
}

fn prepared_again(installation: &Installation) -> (Prepared, Feed) {
    prepared(installation, b"installer 0.1.0.6")
}

#[test]
fn a_stage_whose_file_is_held_keeps_its_owner_for_a_later_start() {
    let parent = tempfile::tempdir().unwrap();
    let work = parent.path().join(format!("{}held", crate::stage::PREFIX));
    fs::create_dir(&work).unwrap();
    // the owner exited (a finished child's id); written first and named before the held folder, so a removal in
    // listing order (creation order on tmpfs, name order on NTFS) reaches it before the held file
    fs::write(work.join("owner"), exited().to_string()).unwrap();
    let held = work.join("running");
    fs::create_dir(&held).unwrap();
    fs::write(held.join("installer.exe"), "running").unwrap();
    let release = hold(&held);
    assert_eq!(crate::stage::cleanup(parent.path()), 0);
    assert!(work.join("owner").exists(), "the owner goes last");
    release();
    assert_eq!(crate::stage::cleanup(parent.path()), 1);
    assert!(!work.exists());
}

/// A process that ran and exited: its id.
fn exited() -> u32 {
    #[cfg(unix)]
    let mut child = std::process::Command::new("true").spawn().unwrap();
    #[cfg(windows)]
    let mut child = std::process::Command::new("cmd").args(["/C", "exit"]).spawn().unwrap();
    let id = child.id();
    child.wait().unwrap();
    id
}

/// Makes the files in `folder` impossible to remove until the returned call: a folder without write permission on
/// Linux, an open handle that doesn't share deletion on Windows (as a running installer's).
#[cfg(unix)]
fn hold(folder: &Path) -> impl FnOnce() {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(folder, fs::Permissions::from_mode(0o555)).unwrap();
    let folder = folder.to_path_buf();
    move || fs::set_permissions(&folder, fs::Permissions::from_mode(0o755)).unwrap()
}

#[cfg(windows)]
fn hold(folder: &Path) -> impl FnOnce() {
    use std::os::windows::fs::OpenOptionsExt as _;
    // FILE_SHARE_READ only: deleting it fails while it is open
    let file = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(folder.join("installer.exe"))
        .unwrap();
    move || drop(file)
}

#[cfg(windows)]
#[test]
fn only_a_marked_installer_folder_with_bin_is_an_installation() {
    let home = tempfile::tempdir().unwrap();
    let installation = setup_installation(home.path(), "0.1.0.5");
    // canonicalize's `\\?\` prefix is left out: the installer and the user are shown a plain path
    assert!(!installation.root().to_string_lossy().starts_with(r"\\?\"));
    assert!(!installation.executable().to_string_lossy().starts_with(r"\\?\"));
    assert_eq!(
        installation.executable(),
        installation.root().join("bin").join("ngteams.exe")
    );
    // a zip's folder (Install.ps1) has no marker
    let zip = home.path().join("zip");
    tree(&zip, "0.1.0.5", Format::WindowsSetup).unwrap();
    fs::remove_file(zip.join(crate::MARKER)).unwrap();
    assert!(Installation::detect(&places(zip.join("bin/ngteams.exe"), home.path())).is_err());
    // a Linux marker is no Windows installation
    fs::write(zip.join(crate::MARKER), "linux-archive\n").unwrap();
    assert!(Installation::detect(&places(zip.join("bin/ngteams.exe"), home.path())).is_err());
    // the program outside bin
    fs::write(zip.join(crate::MARKER), "windows-setup\n").unwrap();
    fs::write(zip.join("ngteams.exe"), "").unwrap();
    assert!(Installation::detect(&places(zip.join("ngteams.exe"), home.path())).is_err());
    // the user's data inside it
    let mut inside = places(zip.join("bin/ngteams.exe"), home.path());
    inside.protected.push(zip.join("data"));
    assert!(Installation::detect(&inside).is_err());
    assert!(crate::stage::alive(std::process::id()));
    assert!(crate::stage::alive(0));
    assert!(!crate::stage::alive(exited()));
}
