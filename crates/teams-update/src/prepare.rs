//! A release's asset downloaded into a stage, verified against `SHA256SUMS` and readied for its install.

use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};

use crate::check::{CHECKSUMS_LIMIT, bounded, checksum};
use crate::{Error, Format, IDENTITY, Installation, MARKER, Prepared, Release, Source, Version, error};

/// The files an archive's tree must hold, beside its marker and identity.
const PROGRAMS: [&str; 5] = [
    "AppRun",
    "usr/bin/ngteams",
    "usr/bin/ngteams-web",
    "usr/bin/ngteamsctl",
    "usr/bin/libteams_signin_extension.so",
];

impl Source {
    /// Downloads `release`'s asset and checksums into a new stage beside the installation, verifies the asset, and
    /// readies it: an archive unpacked and checked, an AppImage made executable, a package kept as it is. `progress`
    /// hears the bytes received of the total.
    ///
    /// # Errors
    /// The stage can't be written (no permission, no space), the download fails, or the asset doesn't match its
    /// checksum or isn't the release it should be. The installation is untouched and the stage removed.
    pub fn prepare(
        &self,
        installation: &Installation,
        release: &Release,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<Prepared, Error> {
        installation.validate()?;
        if let Some(installed) = installation.installed_version()
            && installed >= release.version
        {
            return Err(error(format!(
                "This copy is release {installed} already, as new as {} or newer.",
                release.version
            )));
        }
        let identity = installation.identity()?;
        let parent = installation.stage_parent();
        let work = crate::stage::create(&parent)?;
        progress(0, release.size);
        let manifest = self.fetch(&release.checksums_url)?;
        let manifest = String::from_utf8(manifest).map_err(|_| error("The release's SHA256SUMS isn't text."))?;
        let expected = checksum(&manifest, &release.asset)?;
        let asset = work.path().join(&release.asset);
        self.download(release, &asset, &expected, &mut progress)
            .map_err(|error| short_of_space(error, release.size, &parent))?;
        let candidate = ready(installation.format, &asset, work.path(), release.version)
            .map_err(|error| short_of_space(error, release.size * 3, &parent))?;
        Ok(Prepared {
            installation: installation.clone(),
            work: Some(work),
            candidate,
            sha256: expected,
            identity,
            version: release.version,
        })
    }

    /// A small file of the release (its checksums).
    fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        let mut response = self.agent.get(url).call()?;
        expect_ok(response.status().as_u16())?;
        bounded(
            response.body_mut().as_reader(),
            CHECKSUMS_LIMIT,
            "The release's SHA256SUMS",
        )
    }

    /// The asset streamed into `target`, hashed as it comes; refused unless its size and SHA-256 are `expected`.
    fn download(
        &self,
        release: &Release,
        target: &Path,
        expected: &str,
        progress: &mut impl FnMut(u64, u64),
    ) -> Result<(), Error> {
        let mut response = self.agent.get(&release.url).call()?;
        expect_ok(response.status().as_u16())?;
        let mut reader = response.body_mut().as_reader();
        let mut file = File::create(target)?;
        let mut hash = Sha256::new();
        let mut received = 0;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            received += read as u64;
            if received > release.size {
                return Err(error(
                    "The download is larger than the release says. Nothing was changed.",
                ));
            }
            file.write_all(&buffer[..read])?;
            hash.update(&buffer[..read]);
            progress(received, release.size);
        }
        file.sync_all()?;
        if received != release.size || hex(&hash.finalize()) != expected {
            return Err(error(
                "The download doesn't match the release's SHA256SUMS, so it wasn't installed. Nothing was changed.",
            ));
        }
        Ok(())
    }
}

fn expect_ok(status: u16) -> Result<(), Error> {
    match status {
        200 => Ok(()),
        403 | 429 => Err(Error::RateLimited),
        status => Err(error(format!(
            "GitHub answered {status} for the release's files. Try again later."
        ))),
    }
}

/// The asset readied for its install, checked to be `version` of `format`.
pub(crate) fn ready(format: Format, asset: &Path, work: &Path, version: Version) -> Result<PathBuf, Error> {
    match format {
        Format::Archive => {
            let unpacked = work.join("unpacked");
            fs::create_dir(&unpacked)?;
            let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(asset)?));
            archive.set_preserve_permissions(true);
            archive.set_overwrite(false);
            archive.unpack(&unpacked)?;
            fs::remove_file(asset)?;
            let candidate = unpacked.join(crate::FOLDER);
            validate_tree(&candidate, version)?;
            Ok(candidate)
        }
        Format::AppImage => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(asset, fs::Permissions::from_mode(0o755))?;
            }
            Ok(asset.to_path_buf())
        }
        Format::MacApp => {
            let unpacked = work.join("unpacked");
            fs::create_dir(&unpacked)?;
            unpack_zip(asset, &unpacked)?;
            fs::remove_file(asset)?;
            let candidate = unpacked.join(APP_BUNDLE);
            validate_app(&candidate, version)?;
            Ok(candidate)
        }
        // kept as it is: its checksum was verified, and the installer checks the files it holds as it writes them
        Format::Deb | Format::Rpm | Format::Pacman | Format::WindowsSetup => Ok(asset.to_path_buf()),
    }
}

/// The app bundle a macOS release's zip holds.
const APP_BUNDLE: &str = "ngteams.app";
/// The programs a macOS release's bundle holds.
const APP_PROGRAMS: [&str; 3] = [
    "Contents/MacOS/ngteams",
    "Contents/MacOS/ngteams-webview",
    "Contents/MacOS/ngteamsctl",
];

/// Unpacks the zip `asset` into `into` with the system's `ditto`, as Finder does (permissions, links and the
/// bundle's signature kept).
fn unpack_zip(asset: &Path, into: &Path) -> Result<(), Error> {
    let unpacked = std::process::Command::new("/usr/bin/ditto")
        .arg("-x")
        .arg("-k")
        .arg(asset)
        .arg(into)
        .status()
        .map_err(|failed| {
            error(format!(
                "The download couldn't be unpacked ({failed}). Nothing was changed."
            ))
        })?;
    if !unpacked.success() {
        return Err(error("The download couldn't be unpacked. Nothing was changed."));
    }
    Ok(())
}

/// An unpacked app bundle is the release it should be: the macOS app's marker, `version` as its identity, its
/// programs, and a code signature that verifies over the whole bundle.
fn validate_app(candidate: &Path, version: Version) -> Result<(), Error> {
    let marker = fs::read_to_string(candidate.join(MARKER)).unwrap_or_default();
    let identity = fs::read_to_string(candidate.join(IDENTITY)).unwrap_or_default();
    if marker.trim() != Format::MacApp.marker()
        || identity.lines().next().map(str::trim) != Some(version.to_string().as_str())
    {
        return Err(error(format!(
            "The download isn't release {version}'s app. Nothing was changed."
        )));
    }
    if let Some(missing) = APP_PROGRAMS.iter().find(|program| !candidate.join(program).is_file()) {
        return Err(error(format!(
            "The downloaded app has no {missing}. Nothing was changed."
        )));
    }
    let verified = std::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(candidate)
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !verified {
        return Err(error(
            "The downloaded app's code signature doesn't verify, so it wasn't installed. Nothing was changed.",
        ));
    }
    Ok(())
}

/// An unpacked archive is the release it should be: an archive's marker, `version` as its identity, and its programs.
pub(crate) fn validate_tree(candidate: &Path, version: Version) -> Result<(), Error> {
    let marker = fs::read_to_string(candidate.join(MARKER)).unwrap_or_default();
    let identity = fs::read_to_string(candidate.join(IDENTITY)).unwrap_or_default();
    if marker.trim() != Format::Archive.marker()
        || identity.lines().next().map(str::trim) != Some(version.to_string().as_str())
    {
        return Err(error(format!(
            "The downloaded archive isn't release {version}'s archive. Nothing was changed."
        )));
    }
    if let Some(missing) = PROGRAMS.iter().find(|program| !candidate.join(program).is_file()) {
        return Err(error(format!(
            "The downloaded archive has no {missing}. Nothing was changed."
        )));
    }
    Ok(())
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            use std::fmt::Write as _;
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// A full disk said in words: how much the update needs where.
pub(crate) fn short_of_space(error: Error, needed: u64, parent: &Path) -> Error {
    match &error {
        Error::Io(io) if full(io) => crate::error(format!(
            "There isn't enough free space in {} for the update (it needs about {} MB). Nothing was changed.",
            parent.display(),
            needed.div_ceil(1024 * 1024)
        )),
        _ => error,
    }
}

/// A stage that can't be made in `parent`, said in words: no permission or a read-only place means this copy can't
/// update itself there.
pub(crate) fn unwritable(error: std::io::Error, parent: &Path) -> Error {
    use std::io::ErrorKind;
    // by kind: the temporary folder's errors carry their path, not the OS's code
    match error.kind() {
        ErrorKind::PermissionDenied | ErrorKind::ReadOnlyFilesystem => crate::error(format!(
            "ngteams can't write to {}, so it can't update itself there. Download the update from {} instead.",
            parent.display(),
            crate::RELEASES_URL
        )),
        _ if full(&error) => crate::error(format!(
            "There isn't enough free space in {} for the update. Nothing was changed.",
            parent.display()
        )),
        _ => Error::Io(error),
    }
}

/// Whether `error` says the disk is full.
fn full(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    let codes = [libc::ENOSPC];
    #[cfg(windows)]
    let codes = {
        use windows_sys::Win32::Foundation::{ERROR_DISK_FULL, ERROR_HANDLE_DISK_FULL};
        [ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL].map(u32::cast_signed)
    };
    error.kind() == std::io::ErrorKind::StorageFull || error.raw_os_error().is_some_and(|code| codes.contains(&code))
}
