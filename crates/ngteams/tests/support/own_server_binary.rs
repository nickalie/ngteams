//! The real `ngteams-server` binary on loopback, run as an operator runs it: a configuration file, `serve` as a
//! process of its own, users made with `user add` (the password on standard input). TLS with a certificate a made-up
//! certificate authority signed (the CA's PEM is what an account's `ca_file` names), SQLite in a temporary directory.
//! Nothing leaves loopback.

use std::ffi::OsStr;
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};

/// The variable naming the binary; without it, `ngteams-server` beside the test's own target directory.
pub const BIN_ENV: &str = "NGTEAMS_QA_SERVER_BIN";

/// The password of every user [`BinaryServer::add_user`] adds.
pub const PASSWORD: &str = "correct horse battery";

/// How long `serve` has to listen (the host may be loaded).
const READY: Duration = Duration::from_secs(60);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The binary to run: [`BIN_ENV`]'s, which must exist, else the one `cargo build -p ngteams-server` left beside this
/// test's target directory, if it is there.
pub fn binary() -> TestResult<Option<PathBuf>> {
    if let Some(named) = std::env::var_os(BIN_ENV) {
        let named = PathBuf::from(named);
        if !named.is_file() {
            return Err(format!("{BIN_ENV} names no file: {}", named.display()).into());
        }
        return Ok(Some(named));
    }
    let exe = std::env::current_exe()?;
    // target/<profile>/deps/<test> -> target/<profile>/ngteams-server
    let beside = exe
        .parent()
        .and_then(Path::parent)
        .map(|profile| profile.join("ngteams-server"));
    Ok(beside.filter(|path| path.is_file()))
}

/// A running server process; killed when dropped.
pub struct BinaryServer {
    /// `https://127.0.0.1:<port>`.
    pub origin: String,
    /// The certificate authority that signed its certificate (PEM).
    pub ca_file: PathBuf,
    binary: PathBuf,
    config: PathBuf,
    serving: Child,
    dir: PathBuf,
    _temporary: Option<tempfile::TempDir>,
}

impl BinaryServer {
    /// Writes the certificates and the configuration, then starts `serve` and waits until it listens.
    pub fn start(binary: &Path) -> TestResult<Self> {
        Self::start_with(binary, "")
    }

    /// [`Self::start`] with `tables` added to its configuration (a group call's relay and media node, say).
    pub fn start_with(binary: &Path, tables: &str) -> TestResult<Self> {
        Self::start_in(binary, None, tables, &[])
    }

    /// [`Self::start_with`] in `dir` (which stays after the server, for a test's evidence), else in a temporary
    /// directory, with `env` added to the environment of `serve`.
    pub fn start_in(binary: &Path, dir: Option<&Path>, tables: &str, env: &[(&str, &OsStr)]) -> TestResult<Self> {
        let (dir, temporary) = if let Some(dir) = dir {
            std::fs::create_dir_all(dir)?;
            (dir.to_owned(), None)
        } else {
            let temporary = tempfile::tempdir()?;
            (temporary.path().to_owned(), Some(temporary))
        };
        let ca_file = write_certificates(&dir)?;
        let port = free_port()?;
        let origin = format!("https://127.0.0.1:{port}");
        let config = dir.join("server.toml");
        std::fs::write(
            &config,
            format!(
                "listen = \"127.0.0.1:{port}\"\npublic_origin = \"{origin}\"\ndatabase_url = \"sqlite://{}\"\n\n\
                 [tls]\ncert = \"{}\"\nkey = \"{}\"\n{tables}",
                dir.join("server.sqlite").display(),
                dir.join("cert.pem").display(),
                dir.join("key.pem").display(),
            ),
        )?;
        let log = std::fs::File::create(dir.join("server.log"))?;
        let serving = Command::new(binary)
            .arg("--config")
            .arg(&config)
            .arg("serve")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        let mut server = Self {
            origin,
            ca_file,
            binary: binary.to_owned(),
            config,
            serving,
            dir,
            _temporary: temporary,
        };
        server.wait_until_listening(port)?;
        Ok(server)
    }

    /// Waits for `serve` to say it serves (its database is ready by then) and to take a connection.
    fn wait_until_listening(&mut self, port: u16) -> TestResult {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let start = Instant::now();
        loop {
            if self.log().contains("serving")
                && TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
            {
                return Ok(());
            }
            if let Some(status) = self.serving.try_wait()? {
                return Err(format!("ngteams-server serve exited {status}: {}", self.log()).into());
            }
            if start.elapsed() > READY {
                return Err(format!("ngteams-server serve doesn't listen on {address}: {}", self.log()).into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `ngteams-server user add <upn> --name <name>` with [`PASSWORD`] on standard input; the user's object id.
    pub fn add_user(&self, upn: &str, name: &str) -> TestResult<String> {
        self.add(upn, name, false)
    }

    /// The same with `--admin`.
    pub fn add_admin(&self, upn: &str, name: &str) -> TestResult<String> {
        self.add(upn, name, true)
    }

    fn add(&self, upn: &str, name: &str, admin: bool) -> TestResult<String> {
        let mut args = vec!["user", "add", upn, "--name", name];
        if admin {
            args.push("--admin");
        }
        let said = self.run(&args, Some(PASSWORD))?;
        // "added <upn> (<id>)"
        let id = said
            .trim()
            .rsplit_once('(')
            .and_then(|(_, id)| id.strip_suffix(')'))
            .ok_or_else(|| format!("user add said: {said}"))?;
        Ok(id.to_owned())
    }

    /// `ngteams-server app add --name <name> --endpoint <url> --description <text>`: what it printed.
    pub fn add_app(&self, name: &str, endpoint: &str, description: &str) -> TestResult<String> {
        self.run(
            &[
                "app",
                "add",
                "--name",
                name,
                "--endpoint",
                endpoint,
                "--description",
                description,
            ],
            None,
        )
    }

    /// `ngteams-server setup-code`:the new one-time setup code (its first line).
    pub fn setup_code(&self) -> TestResult<String> {
        let said = self.run(&["setup-code"], None)?;
        Ok(said.lines().next().ok_or("setup-code said nothing")?.trim().to_owned())
    }

    /// `ngteams-server user list`: `(user name, id, enabled, admin)` of each account.
    pub fn users(&self) -> TestResult<Vec<(String, String, bool, bool)>> {
        let said = self.run(&["user", "list"], None)?;
        said.lines()
            .map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                match fields.as_slice() {
                    [upn, id, state, _, role] => Ok((
                        (*upn).to_owned(),
                        (*id).to_owned(),
                        *state == "enabled",
                        *role == "admin",
                    )),
                    _ => Err(format!("user list said: {line}").into()),
                }
            })
            .collect()
    }

    /// Runs the server's command line with `args` (and `input` on standard input); what it printed.
    pub fn run(&self, args: &[&str], input: Option<&str>) -> TestResult<String> {
        let mut running = Command::new(&self.binary)
            .arg("--config")
            .arg(&self.config)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = running.stdin.take().ok_or("the command's standard input")?;
        if let Some(input) = input {
            stdin.write_all(format!("{input}\n").as_bytes())?;
        }
        drop(stdin);
        let output = running.wait_with_output()?;
        if !output.status.success() {
            return Err(format!("{args:?}: {}", String::from_utf8_lossy(&output.stderr)).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// The `serve` process's id.
    pub fn pid(&self) -> u32 {
        self.serving.id()
    }

    /// What `serve` wrote to standard error so far.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.dir.join("server.log")).unwrap_or_default()
    }
}

impl Drop for BinaryServer {
    fn drop(&mut self) {
        let _ = self.serving.kill();
        let _ = self.serving.wait();
    }
}

/// A port nothing listens on now.
pub fn free_port() -> TestResult<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

/// The server's certificate and key (`cert.pem`, `key.pem`) and the certificate authority's PEM (`ca.pem`) in `dir`; the
/// CA file's path.
pub fn write_certificates(dir: &Path) -> TestResult<PathBuf> {
    let ca_key = KeyPair::generate()?;
    let mut ca = CertificateParams::new(Vec::new())?;
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "ngteams test certificate authority");
    let ca_cert = ca.self_signed(&ca_key)?;
    let key = KeyPair::generate()?;
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned()])?;
    let cert = leaf.signed_by(&key, &Issuer::new(ca, ca_key))?;
    std::fs::write(dir.join("cert.pem"), cert.pem())?;
    std::fs::write(dir.join("key.pem"), key.serialize_pem())?;
    let ca_file = dir.join("ca.pem");
    std::fs::write(&ca_file, ca_cert.pem())?;
    Ok(ca_file)
}
