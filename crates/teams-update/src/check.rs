//! GitHub's latest release, and the one asset of it that updates this installation.

use std::io::Read as _;
use std::time::Duration;

use serde::Deserialize;

use crate::{Error, Installation, Version, error};

const API: &str = "https://api.github.com/repos/nickalie/ngteams/releases/latest";
const DOWNLOADS: &str = "https://github.com/nickalie/ngteams/releases/download/";
/// The largest release description read: GitHub's is a few kilobytes per asset.
const METADATA_LIMIT: u64 = 2 * 1024 * 1024;
/// The largest asset taken: a package is under 100 MB.
pub(crate) const ASSET_LIMIT: u64 = 1024 * 1024 * 1024;
/// The largest `SHA256SUMS` taken: a line per asset.
pub(crate) const CHECKSUMS_LIMIT: u64 = 64 * 1024;
const USER_AGENT: &str = concat!("ngteams/", env!("CARGO_PKG_VERSION"));

#[derive(Deserialize)]
pub(crate) struct Metadata {
    pub tag_name: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

#[derive(Deserialize)]
pub(crate) struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub size: u64,
}

/// A newer release, with the asset that updates this installation and the release's checksums.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub(crate) asset: String,
    pub(crate) url: String,
    pub(crate) checksums_url: String,
    pub(crate) size: u64,
}

impl Release {
    /// The asset's file name.
    #[must_use]
    pub fn asset(&self) -> &str {
        &self.asset
    }

    /// The asset's size, bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Where releases are read and downloaded from: GitHub's API and release downloads ([`Source::github`]), or a test's
/// feed on the loopback ([`Source::local`]).
#[derive(Clone)]
pub struct Source {
    api: String,
    pub(crate) downloads: String,
    pub(crate) agent: ureq::Agent,
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Source").field("api", &self.api).finish_non_exhaustive()
    }
}

fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        // as the app's own transport: HTTPS only, no proxy; no credential is ever set on a request
        .https_only(https_only)
        .proxy(None)
        // the status read here: a rate limit isn't an error to show
        .http_status_as_error(false)
        // a release asset is served from GitHub's storage host; the checksum is what's trusted
        .max_redirects(5)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_per_read(Some(Duration::from_secs(30)))
        .build()
        .into()
}

impl Source {
    /// GitHub's releases of ngteams.
    #[must_use]
    pub fn github() -> Self {
        Self {
            api: API.into(),
            downloads: DOWNLOADS.into(),
            agent: agent(true),
        }
    }

    /// A test's release feed at `base` (`http://127.0.0.1:<port>`): its latest release at `<base>/latest`, its
    /// assets under `<base>/download/<tag>/`. Never in a shipped build.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn local(base: &str) -> Self {
        Self {
            api: format!("{base}/latest"),
            downloads: format!("{base}/download/"),
            agent: agent(false),
        }
    }

    /// The newest release when it is newer than `current` and than what the installation holds, with the asset that
    /// updates it; `None` when there is none.
    ///
    /// # Errors
    /// [`Error::Offline`] and [`Error::RateLimited`] for a later try; a release without its asset or checksums, or an
    /// answer that can't be read, says so.
    pub fn check(&self, installation: &Installation, current: Version) -> Result<Option<Release>, Error> {
        let mut response = self
            .agent
            .get(&self.api)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call()?;
        match response.status().as_u16() {
            200 => {}
            // no release published yet
            404 => return Ok(None),
            403 | 429 => return Err(Error::RateLimited),
            status => return Err(error(format!("GitHub answered {status}. Try again later."))),
        }
        let bytes = bounded(
            response.body_mut().as_reader(),
            METADATA_LIMIT,
            "The release description",
        )?;
        let metadata: Metadata = serde_json::from_slice(&bytes)
            .map_err(|_| error("GitHub's release description couldn't be read. Try again later."))?;
        select(&metadata, &self.downloads, installation, current)
    }
}

/// The release `metadata` describes, when it is a newer stable one, with exactly its asset for `installation`'s format
/// and its checksums, each at its release download URL under `downloads`.
pub(crate) fn select(
    metadata: &Metadata,
    downloads: &str,
    installation: &Installation,
    current: Version,
) -> Result<Option<Release>, Error> {
    let Some(version) = metadata.tag_name.strip_prefix('v').and_then(Version::parse) else {
        // a tag of another kind (the runtime's `web-engine-<key>` is never "latest"): nothing to offer
        return Ok(None);
    };
    if metadata.draft
        || metadata.prerelease
        || version <= current
        || installation
            .installed_version()
            .is_some_and(|installed| version <= installed)
    {
        return Ok(None);
    }
    let find = |name: &str| -> Result<&Asset, Error> {
        let found: Vec<_> = metadata.assets.iter().filter(|asset| asset.name == name).collect();
        let [asset] = found.as_slice() else {
            return Err(error(format!(
                "Release {version} has no {name} to update this copy with."
            )));
        };
        if asset.browser_download_url != format!("{downloads}v{version}/{name}") {
            return Err(error(format!(
                "Release {version}'s {name} isn't where GitHub keeps release files."
            )));
        }
        Ok(*asset)
    };
    let name = installation.format.asset(version);
    let asset = find(&name)?;
    let checksums = find("SHA256SUMS")?;
    if asset.size == 0 || asset.size > ASSET_LIMIT || checksums.size == 0 || checksums.size > CHECKSUMS_LIMIT {
        return Err(error(format!("Release {version}'s files have sizes no release has.")));
    }
    Ok(Some(Release {
        version,
        asset: name,
        url: asset.browser_download_url.clone(),
        checksums_url: checksums.browser_download_url.clone(),
        size: asset.size,
    }))
}

/// At most `limit` bytes of `reader`; more is an error naming `what`.
pub(crate) fn bounded(reader: impl std::io::Read, limit: u64, what: &str) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(crate::received)?;
    if bytes.len() as u64 > limit {
        return Err(error(format!("{what} is larger than any release's.")));
    }
    Ok(bytes)
}

/// `name`'s SHA-256 in `SHA256SUMS` (`sha256sum`'s lines), lowercase.
pub(crate) fn checksum(manifest: &str, name: &str) -> Result<String, Error> {
    let hashes: Vec<_> = manifest
        .lines()
        .filter_map(|line| {
            let (hash, file) = line.split_once("  ").or_else(|| line.split_once(" *"))?;
            (file == name).then_some(hash)
        })
        .collect();
    let [hash] = hashes.as_slice() else {
        return Err(error(format!(
            "The release's SHA256SUMS has no single line for {name}."
        )));
    };
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(error(format!(
            "The release's SHA256SUMS line for {name} isn't a SHA-256."
        )));
    }
    Ok(hash.to_ascii_lowercase())
}
