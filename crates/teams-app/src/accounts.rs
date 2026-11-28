//! The accounts file (`accounts.toml`), the accounts' identity metadata, their paths and atomic persistence. The host
//! gives the directories ([`PersonalPaths::new`]): the desktop resolves its own, the phone app gets them from Android.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use teams_proto::{Cloud, Mri};

pub use files::AccountFiles;
mod files;

/// The personal build's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonalPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
}

impl PersonalPaths {
    /// Paths in these directories: the host's own (tests use temporary ones).
    #[must_use]
    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self { config_dir, data_dir }
    }

    #[must_use]
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// `accounts.toml`: the accounts the user added, in the order they were added ([`PersonalAccount`]).
    #[must_use]
    pub fn accounts_file(&self) -> PathBuf {
        self.config_dir.join("accounts.toml")
    }

    /// `account.toml`: the one account a build before several accounts kept ([`PersonalPaths::migrate_single_account`]
    /// moves it into `accounts.toml`).
    #[must_use]
    pub fn legacy_account_file(&self) -> PathBuf {
        self.config_dir.join("account.toml")
    }

    /// `account`'s store (`teams-store`): `store/<oid>.sqlite`, one per account, so a store kept after
    /// `ngteams --forget` is never another account's; `None` before a sign-in named its object id (or for one that
    /// isn't a GUID).
    #[must_use]
    pub fn store_file(&self, account: &PersonalAccount) -> Option<PathBuf> {
        let oid = account.oid.as_deref().filter(|oid| is_guid(oid))?;
        Some(self.data_dir.join("store").join(format!("{oid}.sqlite")))
    }

    /// `account`'s region as authz gave it (never a token): `regions/<oid>.json`; `None` before a sign-in named its
    /// object id.
    #[must_use]
    pub fn region_file(&self, account: &PersonalAccount) -> Option<PathBuf> {
        let oid = account.oid.as_deref().filter(|oid| is_guid(oid))?;
        Some(self.data_dir.join("regions").join(format!("{oid}.json")))
    }

    /// `region.json`: the one account's region a build before several accounts kept.
    #[must_use]
    pub fn legacy_region_file(&self) -> PathBuf {
        self.data_dir.join("region.json")
    }

    /// Moves a build's single account into the files of several: its `region.json` to `regions/<oid>.json`, then
    /// `account.toml` into `accounts.toml` (as its one account), which marks the move done. Nothing when `accounts.toml` exists already or there is
    /// no `account.toml`; a region of an account no sign-in named is left where it is (the next sign-in writes one).
    ///
    /// # Errors
    /// When a file can't be read, parsed or moved; whatever moved before stays moved, and the next start goes on.
    pub fn migrate_single_account(&self) -> Result<(), PersonalError> {
        let legacy = self.legacy_account_file();
        if self.accounts_file().exists() || !legacy.exists() {
            return Ok(());
        }
        let text = fs::read_to_string(&legacy).map_err(|source| PersonalError::Read {
            path: legacy.clone(),
            source,
        })?;
        let account = PersonalAccount::parse(&text)?;
        // the region first: `accounts.toml` written is what says the move is done
        if let Some(region) = self.region_file(&account) {
            let old = self.legacy_region_file();
            if old.is_file() {
                let moved = region
                    .parent()
                    .map_or(Ok(()), teams_platform::private::create_dir_all)
                    .and_then(|()| fs::rename(&old, &region));
                moved.map_err(|source| PersonalError::Write { path: region, source })?;
            }
        }
        write_accounts(self, std::slice::from_ref(&account))?;
        fs::remove_file(&legacy).map_err(|source| PersonalError::Write { path: legacy, source })
    }

    /// The app's settings (`teams-ui`).
    #[must_use]
    pub fn settings_file(&self) -> PathBuf {
        self.data_dir.join("settings.toml")
    }

    /// The app's diagnostic log directory (`teams-log`).
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Makes the data dir (and the config dir), `0700`.
    ///
    /// # Errors
    /// When they can't be made or made private.
    pub fn make_dirs(&self) -> io::Result<()> {
        for dir in [&self.data_dir, &self.config_dir] {
            teams_platform::private::create_dir_all(dir)?;
            teams_platform::private::restrict_dir(dir)?;
        }
        Ok(())
    }
}

/// One account the personal build signs in (an `[[account]]` of `accounts.toml`): the UPN the user gave (the first
/// sign-in screen, the rail's "+" or `ngteams --account`), and once the
/// first sign-in went through, the object id and tenant its ID token named, which every later sign-in must match, and
/// the display name it gave (`name`, refreshed by every sign-in; what the account's posts carry as their sender's
/// name). Never a token.
///
/// ```toml
/// upn = "someone@example.com"
/// oid = "<the account's object id>"   # written by the first sign-in
/// tid = "<its tenant id>"             # written by the first sign-in
/// name = "Someone Example"            # written by a sign-in whose ID token gave one
/// idp_hosts = ["sso.example.com"]     # optional: the federated identity provider's hosts the sign-in window may reach
/// server = "https://chat.example.com" # an own server's account; absent means Microsoft 365
/// ca_file = "/etc/ssl/own-ca.pem"     # optional, with `server`: the certificate authority of a self-signed deployment
/// ```
///
/// The server is the account's own: accounts of Microsoft 365 and of own servers sit side by side, each one's tokens,
/// store and region its own. An account is told from the others by its server and UPN together ([`Self::id`]).
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAccount {
    pub upn: String,
    #[serde(default)]
    pub oid: Option<String>,
    #[serde(default)]
    pub tid: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub idp_hosts: Vec<String>,
    /// The own server's HTTPS origin; `None` is Microsoft 365.
    #[serde(default)]
    pub server: Option<String>,
    /// The certificate authority (a PEM file) the own server's certificate is checked against beside the system's.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
}

/// Where an own-server account signs in: its server's origin and, for a self-signed deployment, its CA file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnServer {
    pub origin: String,
    pub ca_file: Option<PathBuf>,
}

impl OwnServer {
    /// The server at `url` as the user wrote it (a trailing `/` and spaces around it dropped, `https://` put before a
    /// bare host), with the CA file `ca_file` names (empty is none).
    ///
    /// # Errors
    /// [`PersonalError::Account`] unless it is an HTTPS origin and the CA file's path is absolute.
    pub fn parse(url: &str, ca_file: &str) -> Result<Self, PersonalError> {
        let url = url.trim().trim_end_matches('/');
        let origin = if url.contains("://") {
            url.to_owned()
        } else {
            format!("https://{url}")
        };
        let ca_file = Some(ca_file.trim()).filter(|path| !path.is_empty()).map(PathBuf::from);
        let server = Self { origin, ca_file };
        server.validate()?;
        Ok(server)
    }

    /// The server's cloud, trusting the certificate authority its CA file holds.
    ///
    /// # Errors
    /// When the server isn't an HTTPS origin, or the CA file can't be read or holds no certificate.
    pub fn cloud(&self) -> Result<Cloud, PersonalError> {
        let cloud =
            Cloud::own(&self.origin).map_err(|error| PersonalError::Account(format!("{}: {error}", self.origin)))?;
        let Some(path) = &self.ca_file else {
            return Ok(cloud);
        };
        let pem = fs::read(path).map_err(|source| PersonalError::Read {
            path: path.clone(),
            source,
        })?;
        cloud
            .trusting(&pem)
            .map_err(|error| PersonalError::Account(format!("{}: {error}", path.display())))
    }

    fn validate(&self) -> Result<(), PersonalError> {
        if Cloud::own(&self.origin).is_err() {
            return Err(PersonalError::Account(
                "server isn't an HTTPS address like https://chat.example.com".to_owned(),
            ));
        }
        if self.ca_file.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(PersonalError::Account("ca_file isn't an absolute path".to_owned()));
        }
        Ok(())
    }
}

impl fmt::Debug for PersonalAccount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersonalAccount")
            .field("upn", &self.upn)
            .field("server", &self.server)
            .field("idp_hosts", &self.idp_hosts.len())
            .finish_non_exhaustive()
    }
}

impl PersonalAccount {
    /// The accounts `paths`' `accounts.toml` holds, in the order they were added; none before the first one was given.
    /// Without `accounts.toml`, a single account's `account.toml` (not migrated yet) is read as the one account.
    ///
    /// # Errors
    /// When it can't be read or isn't valid ([`PersonalAccount::parse`]; an account given twice, by UPN or object id).
    pub fn load_all(paths: &PersonalPaths) -> Result<Vec<Self>, PersonalError> {
        let path = paths.accounts_file();
        match fs::read_to_string(&path) {
            Ok(text) => parse_accounts(&text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let legacy = paths.legacy_account_file();
                match fs::read_to_string(&legacy) {
                    Ok(text) => Ok(vec![Self::parse(&text)?]),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(source) => Err(PersonalError::Read { path: legacy, source }),
                }
            }
            Err(error) => Err(PersonalError::Read { path, source: error }),
        }
    }

    /// The account named `name` (case aside) of `paths`' accounts ([`PersonalAccount::load_all`]): its [`Self::id`],
    /// or a bare UPN, which names the Microsoft 365 account of that UPN, else the one own-server account of it;
    /// `None` when it names none, or several own-server accounts alike.
    ///
    /// # Errors
    /// As [`PersonalAccount::load_all`].
    pub fn find(paths: &PersonalPaths, name: &str) -> Result<Option<Self>, PersonalError> {
        let accounts = Self::load_all(paths)?;
        Ok(position(&accounts, name).map(|index| accounts[index].clone()))
    }

    /// The account named `name` among `accounts`, as [`Self::find`] reads a name.
    #[must_use]
    pub fn named<'a>(accounts: &'a [Self], name: &str) -> Option<&'a Self> {
        position(accounts, name).map(|index| &accounts[index])
    }

    /// Takes the account named `name` (as [`Self::find`] reads it) out of `paths`' `accounts.toml`, the others kept in
    /// their order; the account taken, or `None` when it wasn't one.
    ///
    /// # Errors
    /// When the accounts can't be read or written.
    pub fn remove(paths: &PersonalPaths, name: &str) -> Result<Option<Self>, PersonalError> {
        let _changing = changing();
        let mut accounts = Self::load_all(paths)?;
        let Some(index) = position(&accounts, name) else {
            return Ok(None);
        };
        let removed = accounts.remove(index);
        write_accounts(paths, &accounts)?;
        Ok(Some(removed))
    }

    /// Parses one account (a single account's `account.toml`, or one `[[account]]` table of `accounts.toml`): a UPN
    /// (`user@domain`), the object id and tenant id as GUIDs (stored lowercase), both or neither, the identity provider
    /// hosts, each a lowercase DNS name, and an own server's origin (kept without a trailing `/`).
    ///
    /// # Errors
    /// [`PersonalError::Account`] saying what is wrong.
    pub fn parse(text: &str) -> Result<Self, PersonalError> {
        let mut account: Self = toml::from_str(text).map_err(|error| PersonalError::Account(error.message().into()))?;
        account.oid = account.oid.map(|oid| oid.to_ascii_lowercase());
        account.tid = account.tid.map(|tid| tid.to_ascii_lowercase());
        // one spelling of a server, as the sign-in screen's ([`OwnServer::parse`]): the account's id, and so its
        // entry and its tokens, never depend on a trailing `/`
        account.server = account.server.map(|server| match server.strip_suffix('/') {
            Some(origin) => origin.to_owned(),
            None => server,
        });
        account.validate()?;
        Ok(account)
    }

    /// The account `upn` before its first sign-in (no object id or tenant yet).
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn pending(upn: &str) -> Result<Self, PersonalError> {
        let account = Self {
            upn: upn.to_owned(),
            oid: None,
            tid: None,
            name: None,
            idp_hosts: Vec::new(),
            server: None,
            ca_file: None,
        };
        account.validate()?;
        Ok(account)
    }

    /// The own server `server`'s account `upn` before its first sign-in.
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn pending_on(upn: &str, server: &OwnServer) -> Result<Self, PersonalError> {
        let account = Self {
            server: Some(server.origin.clone()),
            ca_file: server.ca_file.clone(),
            ..Self::pending(upn)?
        };
        account.validate()?;
        Ok(account)
    }

    /// The own server this account is of; `None` for a Microsoft 365 account.
    #[must_use]
    pub fn own_server(&self) -> Option<OwnServer> {
        self.server.as_ref().map(|origin| OwnServer {
            origin: origin.clone(),
            ca_file: self.ca_file.clone(),
        })
    }

    /// What tells the account from every other, and what its tokens are stored under: its UPN for a Microsoft 365
    /// account (as before own servers), and for an own server's its server before it (`https://chat.example.com/upn`),
    /// so the same user name on two servers, or on a server and Microsoft 365, never shares a token.
    #[must_use]
    pub fn id(&self) -> String {
        match &self.server {
            Some(server) => format!("{server}/{}", self.upn),
            None => self.upn.clone(),
        }
    }

    /// The own server as the window names it: its host, with a port that isn't HTTPS's own.
    #[must_use]
    pub fn server_host(&self) -> Option<String> {
        let cloud = Cloud::own(self.server.as_deref()?).ok()?;
        let url = cloud.origin()?;
        let host = url.host_str()?;
        Some(
            url.port()
                .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}")),
        )
    }

    /// Whether `other` is this account: the same server and UPN, case aside.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        self.id().eq_ignore_ascii_case(&other.id())
    }

    /// The services the account belongs to: Microsoft's, or its own server's with the certificate authority its
    /// `ca_file` holds.
    ///
    /// # Errors
    /// When the server isn't an HTTPS origin, or the CA file can't be read or holds no certificate.
    pub fn cloud(&self) -> Result<Cloud, PersonalError> {
        match self.own_server() {
            Some(server) => server.cloud(),
            None => Ok(Cloud::Microsoft),
        }
    }

    /// The account `upn` as a sign-in named it: its object id and tenant.
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn new(upn: &str, oid: &str, tid: &str) -> Result<Self, PersonalError> {
        let account = Self {
            oid: Some(oid.to_ascii_lowercase()),
            tid: Some(tid.to_ascii_lowercase()),
            ..Self::pending(upn)?
        };
        account.validate()?;
        Ok(account)
    }

    /// The same account with the object id and tenant a sign-in named, and the display name its ID token gave (kept as
    /// it was when the token gave none, or one a post can't carry).
    ///
    /// # Errors
    /// As [`PersonalAccount::parse`].
    pub fn signed_in_as(&self, oid: &str, tid: &str, name: Option<&str>) -> Result<Self, PersonalError> {
        let name = name.map(str::trim).filter(|name| is_display_name(name));
        let account = Self {
            oid: Some(oid.to_ascii_lowercase()),
            tid: Some(tid.to_ascii_lowercase()),
            name: name.map(str::to_owned).or_else(|| self.name.clone()),
            ..self.clone()
        };
        account.validate()?;
        Ok(account)
    }

    /// Whether a sign-in has named its object id and tenant.
    #[must_use]
    pub fn is_signed_in(&self) -> bool {
        self.oid.is_some() && self.tid.is_some()
    }

    fn validate(&self) -> Result<(), PersonalError> {
        let bad = |why: &str| Err(PersonalError::Account(why.to_owned()));
        let upn_ok = self
            .upn
            .split_once('@')
            .is_some_and(|(user, domain)| !user.is_empty() && !domain.is_empty() && !domain.contains('@'))
            && !self.upn.chars().any(char::is_whitespace);
        if !upn_ok {
            return bad("upn isn't a UPN (user@domain)");
        }
        match (&self.oid, &self.tid) {
            (Some(oid), Some(tid)) if is_guid(oid) && is_guid(tid) => {}
            (None, None) => {}
            _ => return bad("oid and tid must be GUIDs, both or neither"),
        }
        if !self.name.as_deref().is_none_or(is_display_name) {
            return bad("name is empty, has control characters or is padded with spaces");
        }
        if !self.idp_hosts.iter().all(|host| is_host(host)) {
            return bad("an idp_hosts entry isn't a lowercase DNS name");
        }
        match self.own_server() {
            Some(server) => {
                server.validate()?;
                if !self.idp_hosts.is_empty() {
                    return bad("idp_hosts is for a Microsoft 365 account, not an own server's");
                }
            }
            None if self.ca_file.is_some() => return bad("ca_file needs a server"),
            None => {}
        }
        Ok(())
    }

    /// The account's MRI, `8:orgid:<oid>`, once a sign-in named its object id.
    #[must_use]
    pub fn mri(&self) -> Option<Mri> {
        self.oid.as_ref().map(|oid| Mri::new(format!("8:orgid:{oid}")))
    }

    /// Writes the account into `paths`' `accounts.toml` (`0600`, its directory `0700`): in the place of the account of
    /// the same server and UPN (case aside), else after the others; the file whole, through a new file renamed over it.
    /// A single account's files not moved yet ([`PersonalPaths::migrate_single_account`]) are moved first.
    ///
    /// # Errors
    /// When the accounts can't be read or written, or another account has the same object id.
    pub fn save(&self, paths: &PersonalPaths) -> Result<(), PersonalError> {
        let _changing = changing();
        // a single account's files not moved yet go first, so they are never left behind `accounts.toml`
        paths.migrate_single_account()?;
        let mut accounts = Self::load_all(paths)?;
        match accounts.iter_mut().find(|account| account.is(self)) {
            Some(held) => *held = self.clone(),
            None => accounts.push(self.clone()),
        }
        check_distinct(&accounts)?;
        write_accounts(paths, &accounts)
    }

    /// The account as one `[[account]]` table of `accounts.toml`.
    fn to_toml(&self) -> String {
        let hosts: Vec<toml::Value> = self.idp_hosts.iter().cloned().map(toml::Value::String).collect();
        let ids = match (&self.oid, &self.tid) {
            (Some(oid), Some(tid)) => format!(
                "oid = {}\ntid = {}\n",
                toml::Value::String(oid.clone()),
                toml::Value::String(tid.clone())
            ),
            _ => String::new(),
        };
        let name = self
            .name
            .as_ref()
            .map(|name| format!("name = {}\n", toml::Value::String(name.clone())))
            .unwrap_or_default();
        let server = self
            .server
            .as_ref()
            .map(|server| format!("server = {}\n", toml::Value::String(server.clone())))
            .unwrap_or_default();
        let ca_file = self
            .ca_file
            .as_ref()
            .map(|path| format!("ca_file = {}\n", toml::Value::String(path.display().to_string())))
            .unwrap_or_default();
        format!(
            "[[account]]\nupn = {}\n{ids}{name}idp_hosts = {}\n{server}{ca_file}",
            toml::Value::String(self.upn.clone()),
            toml::Value::Array(hosts),
        )
    }
}

/// Held while `accounts.toml` is read, changed and written again: an account saved by a sign-in and one removed at the
/// same moment (each on a task of its own) never write over each other's change.
static CHANGING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The next staging file's number: two writes in one process never share a staging file.
static STAGING: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn changing() -> std::sync::MutexGuard<'static, ()> {
    CHANGING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `accounts.toml` as written: its `[[account]]` tables.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountsFile {
    #[serde(default)]
    account: Vec<toml::Table>,
}

/// Parses `accounts.toml`: each `[[account]]` as [`PersonalAccount::parse`] takes `account.toml`, no account twice.
fn parse_accounts(text: &str) -> Result<Vec<PersonalAccount>, PersonalError> {
    let file: AccountsFile = toml::from_str(text).map_err(|error| PersonalError::Account(error.message().into()))?;
    let accounts = file
        .account
        .into_iter()
        .map(|table| PersonalAccount::parse(&table.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    check_distinct(&accounts)?;
    Ok(accounts)
}

/// The account `name` names among `accounts` ([`PersonalAccount::find`]).
fn position(accounts: &[PersonalAccount], name: &str) -> Option<usize> {
    if let Some(index) = accounts
        .iter()
        .position(|account| account.id().eq_ignore_ascii_case(name))
    {
        return Some(index);
    }
    let mut own = accounts
        .iter()
        .enumerate()
        .filter(|(_, account)| account.server.is_some() && account.upn.eq_ignore_ascii_case(name));
    match (own.next(), own.next()) {
        (Some((index, _)), None) => Some(index),
        _ => None,
    }
}

/// Refuses an account given twice, by its server and UPN (case aside) or object id.
fn check_distinct(accounts: &[PersonalAccount]) -> Result<(), PersonalError> {
    for (index, account) in accounts.iter().enumerate() {
        let twice = accounts[..index]
            .iter()
            .any(|earlier| earlier.is(account) || (earlier.oid.is_some() && earlier.oid == account.oid));
        if twice {
            return Err(PersonalError::Account(format!("{} is given twice", account.id())));
        }
    }
    Ok(())
}

/// Writes `accounts` to `paths`' `accounts.toml` (`0600`, its directory `0700`), whole, through a new file renamed over
/// it.
fn write_accounts(paths: &PersonalPaths, accounts: &[PersonalAccount]) -> Result<(), PersonalError> {
    let path = paths.accounts_file();
    let write = || -> io::Result<()> {
        teams_platform::private::create_dir_all(paths.config_dir())?;
        let mut text = String::from(
            "# The accounts the personal build signs in, in the order they were added (a first sign-in adds oid and\n\
             # tid). Never a token.\n",
        );
        for account in accounts {
            text.push('\n');
            text.push_str(&account.to_toml());
        }
        let mut staging = path.as_os_str().to_owned();
        let serial = STAGING.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        staging.push(format!(".{}.{serial}.new", std::process::id()));
        let staging = PathBuf::from(staging);
        let written = (|| {
            let mut file = teams_platform::private::open(&staging, teams_platform::private::Open::NewReadWrite)?;
            io::Write::write_all(&mut file, text.as_bytes())?;
            file.sync_all()?;
            fs::rename(&staging, &path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&staging);
        }
        written
    };
    write().map_err(|source| PersonalError::Write {
        path: path.clone(),
        source,
    })
}

/// Why the account files could not be used.
#[derive(Debug, thiserror::Error)]
pub enum PersonalError {
    #[cfg_attr(
        windows,
        error(
            "LOCALAPPDATA and USERPROFILE are unset or relative, so the personal build's directories can't be found"
        )
    )]
    #[cfg_attr(
        not(windows),
        error("HOME is unset or relative, so the personal build's directories can't be found")
    )]
    NoHome,
    #[error("can't read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("can't write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    #[error("accounts.toml: {0}")]
    Account(String),
    #[error("the account isn't one signed in yet (accounts.toml names no object id for it): sign in first")]
    NoAccount,
}

/// A GUID as `8-4-4-4-12` lowercase hex digits.
fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, len)| {
            group.len() == len
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

/// A display name as a post may carry it: not empty, no control character, no leading or trailing space, at most 256
/// characters.
fn is_display_name(text: &str) -> bool {
    !text.is_empty() && text.trim() == text && text.chars().count() <= 256 && !text.chars().any(char::is_control)
}

/// A lowercase DNS name: labels of letters, digits and `-`, at least two of them, none empty.
fn is_host(text: &str) -> bool {
    let labels: Vec<&str> = text.split('.').collect();
    labels.len() >= 2
        && text.len() <= 253
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}
