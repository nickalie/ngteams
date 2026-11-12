//! `proto-lab chrome install`: the pinned Chrome for Testing build under `~/.local/share/teams-lab/chrome/`.
//!
//! The build is pinned by version, URL and the SHA-256 of its zip. The Chrome for Testing JSON endpoints carry no
//! checksums, so the hash was taken when the build was pinned, from a download whose MD5 matched the `x-goog-hash`
//! Google Cloud Storage serves for the object. A download that doesn't match is deleted and nothing is installed.
//!
//! The pin is no older than the Chromium that created the lab profiles (Playwright 1.62.1's Chrome for Testing
//! 151.0.7922.34): an older Chrome can't open a profile written by a newer one.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// The pinned build: Chrome for Testing Stable 155.0.8059.84 (2026-10-19), for linux64 and for mac-arm64 (the macOS
/// port's lab sign-in on Apple silicon).
pub const PINNED_VERSION: &str = "155.0.8059.84";
pub const PINNED_URL: &str =
    "https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.84/linux64/chrome-linux64.zip";
pub const PINNED_SHA256: &str = "cf0f049892dea22e6a6db26d47ed0d7776b7158edcbec4634086633df8bafcbc";
pub const PINNED_MAC_ARM64_URL: &str =
    "https://storage.googleapis.com/chrome-for-testing-public/155.0.8059.84/mac-arm64/chrome-mac-arm64.zip";
pub const PINNED_MAC_ARM64_SHA256: &str = "13b9e98ce56f5ce18e100a009f455c9285f399e7c9dc8a76fdf2f3a9755bb5f2";

/// The browser binary inside each zip.
const LINUX64_BINARY: &str = "chrome-linux64/chrome";
const MAC_ARM64_BINARY: &str =
    "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing";
/// Written last into a finished install; holds the zip's SHA-256.
const MARKER: &str = ".verified";

/// One Chrome for Testing build to install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeBuild {
    pub version: String,
    pub url: String,
    /// The SHA-256 of the zip, in hex.
    pub sha256: String,
    /// The browser binary's path inside the zip, `/`-separated.
    pub binary: String,
}

impl ChromeBuild {
    /// The pinned build for this computer: mac-arm64 on macOS, linux64 elsewhere.
    #[must_use]
    pub fn pinned() -> Self {
        if cfg!(target_os = "macos") {
            Self::mac_arm64()
        } else {
            Self::linux64()
        }
    }

    #[must_use]
    pub fn linux64() -> Self {
        Self {
            version: PINNED_VERSION.to_owned(),
            url: PINNED_URL.to_owned(),
            sha256: PINNED_SHA256.to_owned(),
            binary: LINUX64_BINARY.to_owned(),
        }
    }

    #[must_use]
    pub fn mac_arm64() -> Self {
        Self {
            version: PINNED_VERSION.to_owned(),
            url: PINNED_MAC_ARM64_URL.to_owned(),
            sha256: PINNED_MAC_ARM64_SHA256.to_owned(),
            binary: MAC_ARM64_BINARY.to_owned(),
        }
    }

    /// Where this build is unpacked under `chrome_dir`.
    #[must_use]
    pub fn install_dir(&self, chrome_dir: &Path) -> PathBuf {
        chrome_dir.join(&self.version)
    }

    /// The browser binary of this build under `chrome_dir`.
    #[must_use]
    pub fn executable(&self, chrome_dir: &Path) -> PathBuf {
        self.install_dir(chrome_dir).join(&self.binary)
    }
}

/// Why a file didn't pass its checksum.
#[derive(Debug, thiserror::Error)]
pub enum ChecksumError {
    #[error("not a SHA-256 in hex: {0:?}")]
    BadExpected(String),
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("checksum mismatch: expected sha256 {expected}, got {actual}")]
    Mismatch { expected: String, actual: String },
}

/// Checks that the SHA-256 of the file at `path` is `expected` (hex, any case).
///
/// # Errors
/// When `expected` isn't 64 hex digits, the file can't be read, or the hashes differ.
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), ChecksumError> {
    let expected = expected.to_ascii_lowercase();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ChecksumError::BadExpected(expected));
    }
    let read_error = |source| ChecksumError::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut file = std::fs::File::open(path).map_err(read_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = file.read(&mut buffer).map_err(read_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // writing into a String can't fail
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    if actual == expected {
        Ok(())
    } else {
        Err(ChecksumError::Mismatch { expected, actual })
    }
}

/// Why the install failed.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("download failed: {0}")]
    Download(#[from] reqwest::Error),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Checksum(#[from] ChecksumError),
    #[error("can't unpack the zip: {0}")]
    Unzip(#[from] zip::result::ZipError),
    #[error("the zip has no browser binary at {0}")]
    MissingExecutable(String),
    #[error("the unpack task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// What [`install`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// Downloaded, verified and unpacked now.
    Fresh(PathBuf),
    /// A verified install of this build was already there; nothing was downloaded.
    Present(PathBuf),
}

impl Installed {
    /// The browser binary.
    #[must_use]
    pub fn executable(&self) -> &Path {
        match self {
            Self::Fresh(path) | Self::Present(path) => path,
        }
    }
}

/// Installs `build` into `chrome_dir/<version>/`: download, check the SHA-256, unpack, mark verified. A verified
/// install is left alone. The download and the unpacking go through `<version>.zip.part` and `<version>.staging`
/// next to it, and are removed on failure, so a half-done install never looks finished.
///
/// # Errors
/// When the download fails or returns an error status, the checksum doesn't match, the zip can't be unpacked or
/// holds no browser binary, or a file operation fails.
pub async fn install(
    build: &ChromeBuild,
    chrome_dir: &Path,
    client: &reqwest::Client,
) -> Result<Installed, InstallError> {
    let executable = build.executable(chrome_dir);
    if is_installed(build, chrome_dir).await {
        return Ok(Installed::Present(executable));
    }
    tokio::fs::create_dir_all(chrome_dir).await.map_err(io_at(chrome_dir))?;
    let zip = chrome_dir.join(format!("{}.zip.part", build.version));
    let result = fetch_and_unpack(build, chrome_dir, &zip, client).await;
    remove_file_if_present(&zip).await?;
    result?;
    Ok(Installed::Fresh(executable))
}

async fn is_installed(build: &ChromeBuild, chrome_dir: &Path) -> bool {
    let marker = build.install_dir(chrome_dir).join(MARKER);
    match tokio::fs::read_to_string(&marker).await {
        Ok(hash) if hash.trim().eq_ignore_ascii_case(&build.sha256) => {
            tokio::fs::metadata(build.executable(chrome_dir))
                .await
                .is_ok_and(|metadata| metadata.is_file())
        }
        _ => false,
    }
}

async fn fetch_and_unpack(
    build: &ChromeBuild,
    chrome_dir: &Path,
    zip: &Path,
    client: &reqwest::Client,
) -> Result<(), InstallError> {
    download(&build.url, zip, client).await?;
    let (zip, sha256, binary) = (zip.to_path_buf(), build.sha256.clone(), build.binary.clone());
    let staging = chrome_dir.join(format!("{}.staging", build.version));
    let unpacked = staging.clone();
    let checked = tokio::task::spawn_blocking(move || -> Result<(), InstallError> {
        verify_sha256(&zip, &sha256)?;
        unpack(&zip, &unpacked, &binary)
    })
    .await?;
    if let Err(error) = checked {
        remove_dir_if_present(&staging).await?;
        return Err(error);
    }
    let target = build.install_dir(chrome_dir);
    remove_dir_if_present(&target).await?;
    tokio::fs::rename(&staging, &target).await.map_err(io_at(&target))?;
    let marker = target.join(MARKER);
    tokio::fs::write(&marker, format!("{}\n", build.sha256.to_ascii_lowercase()))
        .await
        .map_err(io_at(&marker))
}

async fn download(url: &str, path: &Path, client: &reqwest::Client) -> Result<(), InstallError> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut file = tokio::fs::File::create(path).await.map_err(io_at(path))?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await.map_err(io_at(path))?;
    }
    file.flush().await.map_err(io_at(path))
}

/// Unpacks the zip into a fresh `staging` dir; it must hold the browser binary.
fn unpack(zip: &Path, staging: &Path, binary: &str) -> Result<(), InstallError> {
    match std::fs::remove_dir_all(staging) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(io_at(staging)(error)),
        _ => {}
    }
    let file = std::fs::File::open(zip).map_err(io_at(zip))?;
    zip::ZipArchive::new(file)?.extract(staging)?;
    if staging.join(binary).is_file() {
        Ok(())
    } else {
        Err(InstallError::MissingExecutable(binary.to_owned()))
    }
}

fn io_at(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError {
    let path = path.to_path_buf();
    move |source| InstallError::Io { path, source }
}

async fn remove_file_if_present(path: &Path) -> Result<(), InstallError> {
    match tokio::fs::remove_file(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}

async fn remove_dir_if_present(path: &Path) -> Result<(), InstallError> {
    match tokio::fs::remove_dir_all(path).await {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}
