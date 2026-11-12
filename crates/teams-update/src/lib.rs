//! The installed app's own updates from its GitHub releases, after ngterm's updater (MIT, the same author's), adapted
//! to the packages `.github/workflows/release.yml` publishes.
//!
//! - **Which copies update**: only a release's own, which knows its release version ([`Version::current`], compiled in
//!   by the release workflow) and carries the marker that says how it was installed ([`MARKER`], written by
//!   `scripts/package-release.sh` into each package). A source build (`scripts/install.sh`), a lab build and a
//!   `test-support` build have no release version, so [`Installation::detect`] is never reached for them: the app
//!   doesn't even ask (`ngteams::update`).
//! - **The check** ([`Source::check`]) reads GitHub's latest release, anonymously: no credential, token or account data
//!   goes to GitHub, and the request carries only a `User-Agent` naming the app and its version. A draft, a prerelease
//!   (the `web-engine-<key>` runtimes'), the running version and anything older are never offered. Offline and
//!   GitHub's rate limit come back as [`Error::Offline`] and [`Error::RateLimited`], for the caller to try again later
//!   without bothering the user.
//! - **The download** ([`Source::prepare`]) takes exactly the asset matching how this copy was installed
//!   ([`Format::asset`]) and the release's `SHA256SUMS`, both at the URLs GitHub's release download serves them from,
//!   over HTTPS only, with no proxy. Redirects are followed (GitHub serves an asset from its storage host); the
//!   checksum is what's trusted. The asset is checked against its size and its line in `SHA256SUMS` before anything
//!   of it is unpacked, then unpacked and checked again (its marker, its release version, its programs) in a stage
//!   beside the installation. Every failure until then leaves the installation as it was, and drops the stage.
//! - **Provenance**: the release workflow attests every asset and `SHA256SUMS` (GitHub build provenance). The app
//!   doesn't verify those attestations itself (that takes a Sigstore verifier and its trust root); it trusts
//!   `SHA256SUMS` from the same release over HTTPS, as the README's manual install does. `gh attestation verify
//!   <file> --repo nickalie/ngteams` checks a download by hand.
//! - **The install** ([`Prepared::install`]): an unpacked archive's whole folder is swapped for the new one by two
//!   renames on one filesystem, put back if the second fails; an AppImage file is replaced by one rename; a `.deb`,
//!   `.rpm` or Arch package goes to the system's package manager behind the desktop's password prompt (`pkexec`). Each
//!   first checks that nothing else updated the installation since the download started. It gives back what a
//!   restart runs ([`Restart`]): the app quits as it always does (its engines shut down, its stores closed) and runs
//!   that.
//! - **macOS** ([`Format::MacApp`]): the zip holds `ngteams.app`, marker and identity under `Contents/Resources/ngteams/`.
//!   Its update is unpacked with `ditto` (which keeps the bundle's signature intact) beside the installed bundle,
//!   checked as an archive is and its code signature verified (`codesign --verify --deep --strict`), and swapped for
//!   the installed bundle by the same two renames.
//! - **Windows** ([`Format::WindowsSetup`]): the per-user installer (`packaging/windows/ngteams.iss`) writes the marker
//!   and identity under `share/ngteams/` beside `bin/`. Its update downloads the next installer into the app's own
//!   folder and checks it the same way; [`Prepared::install`] checks it again and hands back the installer, with its
//!   silent-update arguments, as the program a restart runs. A running program's files can't be replaced on Windows,
//!   so the installer waits for this process to exit, upgrades the folder in place and starts the new version.
//!
//! Every message is fit to show the user: no URL with a query, header or environment value is in one.

mod check;
#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
mod install;
mod installation;
mod prepare;
#[cfg(unix)]
mod privileged;
mod stage;

pub use check::{Release, Source};
pub use install::{Prepared, Restart};
pub use installation::{Installation, Places};

/// The release marker, relative to an installed tree's root (the folder holding `usr/`): how this copy was installed,
/// one of [`Format::marker`]'s words.
#[cfg(all(unix, not(target_os = "macos")))]
pub const MARKER: &str = "usr/share/ngteams/update-format";
/// The release version an installed tree holds, relative to its root: the one its programs were built as.
#[cfg(all(unix, not(target_os = "macos")))]
pub const IDENTITY: &str = "usr/share/ngteams/release";
/// The release marker, relative to the app bundle (`ngteams.app`), written by `scripts/package-macos.sh`.
#[cfg(target_os = "macos")]
pub const MARKER: &str = "Contents/Resources/ngteams/update-format";
/// The release version the app bundle holds, relative to it, written by `scripts/package-macos.sh`.
#[cfg(target_os = "macos")]
pub const IDENTITY: &str = "Contents/Resources/ngteams/release";
/// The release marker, relative to the installed folder (the one holding `bin/`), written by the installer.
#[cfg(windows)]
pub const MARKER: &str = "share/ngteams/update-format";
/// The release version the installed folder holds, relative to it, written by the installer.
#[cfg(windows)]
pub const IDENTITY: &str = "share/ngteams/release";
/// The folder an archive unpacks into, and must stay in to update itself.
pub const FOLDER: &str = "ngteams";
/// The release page, for a manual download when the app can't install the update itself.
pub const RELEASES_URL: &str = "https://github.com/nickalie/ngteams/releases/latest";
/// The release version this build was made as, set by the release workflow; `None` in every other build.
pub const CURRENT: Option<&str> = option_env!("NGTEAMS_RELEASE_VERSION");

/// Whether this build carries the plain-HTTP release feed of tests ([`Source::local`]).
#[must_use]
pub const fn test_support_enabled() -> bool {
    cfg!(feature = "test-support")
}

/// A release's version, `<workspace version>.<run number>` (`0.1.0.123`, its tag `v0.1.0.123`): four numbers,
/// compared in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version([u64; 4]);

impl Version {
    /// `text` as a version: exactly four dot-separated decimal numbers.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = [0; 4];
        let mut numbers = text.split('.');
        for part in &mut parts {
            let number = numbers.next()?;
            if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            *part = number.parse().ok()?;
        }
        numbers.next().is_none().then_some(Self(parts))
    }

    /// The release version this build was made as ([`CURRENT`]); `None` for any other build.
    #[must_use]
    pub fn current() -> Option<Self> {
        CURRENT.and_then(Self::parse)
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [major, minor, patch, run] = self.0;
        write!(f, "{major}.{minor}.{patch}.{run}")
    }
}

/// An update that didn't go through, fit to show the user.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(String),
    /// GitHub couldn't be reached: the next check tries again.
    #[error("GitHub can't be reached right now. Check your connection and try again.")]
    Offline,
    /// GitHub turned the request down for its rate limit: the next check tries again.
    #[error("GitHub is limiting requests right now. Try again later.")]
    RateLimited,
    /// The password prompt for the package manager was closed: nothing changed.
    #[error("The update wasn't installed: the password prompt was closed.")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) fn error(message: impl Into<String>) -> Error {
    Error::Message(message.into())
}

impl From<ureq::Error> for Error {
    fn from(error: ureq::Error) -> Self {
        match error {
            ureq::Error::HostNotFound | ureq::Error::ConnectionFailed | ureq::Error::Timeout(_) => Self::Offline,
            ureq::Error::Io(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused
                        | std::io::ErrorKind::ConnectionReset
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::NotConnected
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::HostUnreachable
                        | std::io::ErrorKind::NetworkUnreachable
                        | std::io::ErrorKind::NetworkDown
                ) =>
            {
                Self::Offline
            }
            error => Self::Message(format!("GitHub's answer couldn't be read ({error}). Try again later.")),
        }
    }
}

/// How this copy was installed, which picks the release asset that updates it and how it is installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// The `.tar.gz` unpacked into a folder named `ngteams` (an AppImage extracted the same way counts as one).
    Archive,
    /// The `.AppImage` file, run as it is.
    AppImage,
    /// The `.deb`, installed under `/opt/ngteams`.
    Deb,
    /// The `.rpm`, installed under `/opt/ngteams`.
    Rpm,
    /// The Arch package, installed under `/opt/ngteams`.
    Pacman,
    /// The Windows per-user installer (`-setup.exe`), installed under `%LOCALAPPDATA%\Programs\ngteams`.
    WindowsSetup,
    /// The macOS app bundle (`ngteams.app`, from the `-macos-arm64.zip`), wherever the user put it.
    MacApp,
}

impl Format {
    /// The release asset that updates this format to `version`, as `scripts/package-release.sh` (and nfpm) name it.
    #[must_use]
    pub fn asset(self, version: Version) -> String {
        match self {
            Self::Archive => format!("ngteams-{version}-linux-x86_64.tar.gz"),
            Self::AppImage => format!("ngteams-{version}-linux-x86_64.AppImage"),
            Self::Deb => format!("ngteams_{version}-1_amd64.deb"),
            Self::Rpm => format!("ngteams-{version}-1.x86_64.rpm"),
            Self::Pacman => format!("ngteams-{version}-1-x86_64.pkg.tar.zst"),
            Self::WindowsSetup => format!("ngteams-{version}-windows-x86_64-setup.exe"),
            Self::MacApp => format!("ngteams-{version}-macos-arm64.zip"),
        }
    }

    /// The word [`MARKER`] holds for this format.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Archive => "linux-archive",
            Self::AppImage => "linux-appimage",
            Self::Deb => "linux-deb",
            Self::Rpm => "linux-rpm",
            Self::Pacman => "linux-pacman",
            Self::WindowsSetup => "windows-setup",
            Self::MacApp => "macos-app",
        }
    }

    /// The format a tree's marker names. An AppImage's tree found unpacked (no AppImage running it) is an archive.
    pub(crate) fn from_marker(text: &str) -> Option<Self> {
        match text.trim() {
            "linux-archive" | "linux-appimage" => Some(Self::Archive),
            "linux-deb" => Some(Self::Deb),
            "linux-rpm" => Some(Self::Rpm),
            "linux-pacman" => Some(Self::Pacman),
            "windows-setup" => Some(Self::WindowsSetup),
            "macos-app" => Some(Self::MacApp),
            _ => None,
        }
    }

    /// The package manager's kind, for a package the system owns; `None` for a copy the user owns.
    pub(crate) fn system_package(self) -> Option<&'static str> {
        match self {
            Self::Deb => Some("deb"),
            Self::Rpm => Some("rpm"),
            Self::Pacman => Some("pacman"),
            Self::Archive | Self::AppImage | Self::WindowsSetup | Self::MacApp => None,
        }
    }

    /// Whether an update's stage goes into the app's own folder rather than beside the installation: a system
    /// package's folder isn't the user's to write, and an installer needs no rename beside what it replaces.
    pub(crate) fn stages_in_app_folder(self) -> bool {
        self.system_package().is_some() || self == Self::WindowsSetup
    }
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod tests;
