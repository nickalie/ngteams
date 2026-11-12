//! Native lab account files. The format is ngteams's public account/region layout, without depending on its UI.

use std::ffi::OsString;
use std::fs;
use std::io;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use teams_auth::authz::Region;
use teams_auth::id_token::Account;

use crate::config::{Alias, overlapping};

use super::KEYRING_APPLICATION;

const PERSONAL_DIR: &str = "ngteams";

/// The isolated account/config/data directory consumed by `ngteams --lab <alias>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeTarget {
    root: PathBuf,
}

impl NativeTarget {
    /// Resolves the native target from this process's environment: XDG on Linux, `~/Library/Application Support` on
    /// macOS, Local `AppData` on Windows.
    ///
    /// # Errors
    /// The environment cannot resolve absolute paths, or the target overlaps personal app files.
    pub fn from_env(alias: Alias) -> io::Result<Self> {
        #[cfg(windows)]
        return Self::windows_alias(alias.as_str());
        #[cfg(target_os = "macos")]
        return Self::macos(alias, std::env::var_os("HOME"));
        #[cfg(all(unix, not(target_os = "macos")))]
        Self::resolve(
            alias,
            std::env::var_os("XDG_DATA_HOME"),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// Resolves a Windows runner alias, always in the isolated native lab namespace.
    ///
    /// # Errors
    /// The alias or LOCALAPPDATA is invalid, or a link points into personal data.
    #[cfg(windows)]
    pub fn windows_alias(alias: &str) -> io::Result<Self> {
        if alias.is_empty()
            || alias.len() > 64
            || !alias.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(invalid("invalid native lab alias"));
        }
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid("LOCALAPPDATA must be absolute"))?;
        let root = local.join(KEYRING_APPLICATION).join(alias);
        if overlapping(&root, &local.join(PERSONAL_DIR)) {
            return Err(invalid("native lab target overlaps personal Teams files"));
        }
        Ok(Self { root })
    }

    /// Verifies an existing Windows alias names the requested lab account before tokens or files change.
    ///
    /// # Errors
    /// Existing account configuration is malformed or belongs to another identity.
    #[cfg(windows)]
    pub fn expect_account(&self, upn: &str) -> io::Result<()> {
        let path = self.root.join("accounts.toml");
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let config: toml::Table =
            toml::from_str(&text).map_err(|_| invalid("invalid native lab account configuration"))?;
        let matches = config
            .get("account")
            .and_then(toml::Value::as_array)
            .filter(|entries| entries.len() == 1)
            .and_then(|entries| entries[0].get("upn"))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(upn));
        if !matches {
            return Err(invalid("native lab alias belongs to another account"));
        }
        Ok(())
    }

    /// Reads a previously verified native identity, without opening its store or credentials.
    ///
    /// # Errors
    /// The alias is not provisioned for this account or lacks valid identity IDs.
    #[cfg(windows)]
    pub fn account(&self, upn: &str) -> io::Result<Account> {
        self.expect_account(upn)?;
        let text = fs::read_to_string(self.root.join("accounts.toml"))?;
        let config: toml::Table = toml::from_str(&text).map_err(|_| invalid("invalid native lab account"))?;
        let entry = config
            .get("account")
            .and_then(toml::Value::as_array)
            .and_then(|entries| entries.first())
            .ok_or_else(|| invalid("missing native account"))?;
        let id = |key| {
            entry
                .get(key)
                .and_then(toml::Value::as_str)
                .filter(|value| guid(&value.to_lowercase()))
                .map(str::to_owned)
                .ok_or_else(|| invalid("native alias is not provisioned"))
        };
        Ok(Account {
            username: upn.to_owned(),
            object_id: id("oid")?,
            tenant: id("tid")?,
            display_name: entry.get("name").and_then(toml::Value::as_str).map(str::to_owned),
        })
    }

    /// The macOS alias, where the Mac app's lab build reads it: `~/Library/Application Support/ngteams-lab/<alias>`,
    /// beside the personal app's `ngteams` directory there.
    ///
    /// # Errors
    /// No absolute home, or the native target overlaps the personal app's directory after symlinks resolve.
    pub fn macos(alias: Alias, home: Option<OsString>) -> io::Result<Self> {
        let support = home
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| invalid("HOME must be absolute"))?
            .join("Library/Application Support");
        let root = support.join(KEYRING_APPLICATION).join(alias.as_str());
        if overlapping(&root, &support.join(PERSONAL_DIR)) {
            return Err(invalid("native lab target overlaps personal Teams files"));
        }
        Ok(Self { root })
    }

    /// The XDG resolution on explicit values, for offline provisioning tests.
    ///
    /// # Errors
    /// No absolute XDG/home fallback, or the native target overlaps personal data/config after symlinks resolve.
    pub fn resolve(
        alias: Alias,
        data_home: Option<OsString>,
        config_home: Option<OsString>,
        home: Option<OsString>,
    ) -> io::Result<Self> {
        let home = home.map(PathBuf::from).filter(|path| path.is_absolute());
        let base = |value: Option<OsString>, fallback: &str| {
            value
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| home.as_ref().map(|home| home.join(fallback)))
                .ok_or_else(|| invalid("HOME/XDG directories cannot be resolved"))
        };
        let data = base(data_home, ".local/share")?;
        let config = base(config_home, ".config")?;
        let root = data.join(KEYRING_APPLICATION).join(alias.as_str());
        if [data.join(PERSONAL_DIR), config.join(PERSONAL_DIR)]
            .iter()
            .any(|personal| overlapping(&root, personal))
        {
            return Err(invalid("native lab target overlaps personal Teams files"));
        }
        Ok(Self { root })
    }

    /// The native alias's directory, containing both account config and app data.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Saves the verified identity and, when authz succeeded, region in ngteams's layout.
    ///
    /// Existing same-account identity-provider hosts and a name absent from the new ID token are retained.
    /// Region is saved first, then the account file, so a newly ready entry always has its region.
    ///
    /// # Errors
    /// Invalid identity IDs, an unreadable existing account file, or a private/atomic file write failure.
    pub fn save(&self, account: &Account, region: Option<&Region>) -> io::Result<PathBuf> {
        let oid = account.object_id.to_ascii_lowercase();
        let tid = account.tenant.to_ascii_lowercase();
        if !guid(&oid) || !guid(&tid) {
            return Err(invalid("verified identity has invalid object/tenant GUIDs"));
        }
        private_dir(&self.root)?;
        let path = self.root.join("accounts.toml");
        let mut entry = existing(&path, &account.username)?;
        entry.insert("upn".into(), account.username.clone().into());
        entry.insert("oid".into(), oid.clone().into());
        entry.insert("tid".into(), tid.into());
        if let Some(name) = account
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| valid_name(name))
        {
            entry.insert("name".into(), name.into());
        }
        entry
            .entry("idp_hosts")
            .or_insert_with(|| toml::Value::Array(Vec::new()));
        if let Some(region) = region {
            let regions = self.root.join("regions");
            private_dir(&regions)?;
            let text = serde_json::to_vec_pretty(&region.to_json()).map_err(io::Error::other)?;
            atomic(&regions.join(format!("{oid}.json")), &text)?;
        }
        let config = toml::Table::from_iter([("account".into(), toml::Value::Array(vec![entry.into()]))]);
        let text = toml::to_string(&config).map_err(io::Error::other)?;
        atomic(&path, text.as_bytes())?;
        Ok(path)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(unix)]
fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(unix)]
fn atomic(path: &Path, text: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| invalid("native target has no parent"))?;
    let mut staging = tempfile::Builder::new()
        .prefix(".native-auth-")
        .permissions(fs::Permissions::from_mode(0o600))
        .tempfile_in(dir)?;
    staging.write_all(text)?;
    staging.as_file().sync_all()?;
    staging.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn existing(path: &Path, upn: &str) -> io::Result<toml::Table> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(toml::Table::new()),
        Err(error) => return Err(error),
    };
    let value: toml::Table = toml::from_str(&text).map_err(io::Error::other)?;
    let entry = value
        .get("account")
        .and_then(toml::Value::as_array)
        .filter(|entries| entries.len() == 1)
        .and_then(|entries| entries[0].as_table());
    Ok(entry
        .filter(|entry| {
            entry
                .get("upn")
                .and_then(toml::Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(upn))
        })
        .map(|entry| {
            ["name", "idp_hosts"]
                .into_iter()
                .filter_map(|key| entry.get(key).map(|value| (key.into(), value.clone())))
                .collect()
        })
        .unwrap_or_default())
}

fn guid(text: &str) -> bool {
    let groups: Vec<_> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, size)| {
            group.len() == size
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 256 && !name.chars().any(char::is_control)
}

#[cfg(windows)]
fn private_dir(path: &Path) -> io::Result<()> {
    teams_platform::private::create_dir_all(path)?;
    teams_platform::private::restrict_dir(path)
}

#[cfg(windows)]
fn atomic(path: &Path, text: &[u8]) -> io::Result<()> {
    crate::private::atomic(path, text)
}
