//! The real `ngteams-server` binary deployed as `docs/server.md` ("Scaling") runs it: two API nodes
//! (`serve --role api`) and a media node (`serve --role media`) on one Postgres database of this run's own
//! (`NGTEAMS_SERVER_PG_URL`), one Redis (`NGTEAMS_SERVER_REDIS_URL`), one store for pictures and files (the S3 bucket
//! `NGTEAMS_SERVER_S3_URL` names, under a prefix of this run's own; else a directory both nodes share), coturn as the
//! calls' relay, TLS on both API nodes from one made-up certificate authority (an account's `ca_file`).
//!
//! **One origin, a front before the nodes.** Every node of a deployment shares one `public_origin`: the bases authz
//! hands out, the tokens' audience and the file links are under it, and the client takes nothing outside its
//! account's origin (`teams_auth::cloud`, the port included). So, as the deployment's reverse proxy does, a front
//! listens on the origin's port and passes each TLS connection through to a node. It sends the connections of the app
//! process pinned to a node ([`Nodes::pin`]) there while that node takes them, else to the other node, and counts
//! where each went: two apps signed in to one address are each served by a node of their own, and one whose node is
//! killed reconnects to the other. The process of a connection is found from its socket in `/proc`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{fs, thread};

use super::own_server_binary::{PASSWORD, free_port, write_certificates};

pub const PG_ENV: &str = "NGTEAMS_SERVER_PG_URL";
pub const REDIS_ENV: &str = "NGTEAMS_SERVER_REDIS_URL";
pub const S3_ENV: &str = "NGTEAMS_SERVER_S3_URL";
pub const COTURN_ENV: &str = "NGTS_TEST_COTURN";
pub const COTURN_SECRET_ENV: &str = "NGTS_TEST_COTURN_SECRET";

/// How long a node has to serve (the host may be loaded).
const READY: Duration = Duration::from_secs(90);
/// How long the front waits for a node to take a connection before it tries the other.
const CONNECT: Duration = Duration::from_millis(500);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// What the deployment needs from the run: Postgres, Redis and coturn (`None`, said, without one of them).
pub struct Needs {
    pub postgres: String,
    pub redis: String,
    pub s3: Option<String>,
    pub relay: String,
    pub secret: String,
}

impl Needs {
    /// The run's services; `Err` names the first one missing.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).map_err(|_| format!("{name} isn't set"));
        Ok(Self {
            postgres: var(PG_ENV)?,
            redis: var(REDIS_ENV)?,
            s3: std::env::var(S3_ENV).ok(),
            relay: var(COTURN_ENV)?,
            secret: var(COTURN_SECRET_ENV)?,
        })
    }
}

/// A node's process: its configuration, its log, and its `serve` while it runs.
struct Node {
    name: &'static str,
    config: PathBuf,
    log: PathBuf,
    /// Where it is ready: the public listener of an API node, the internal API of a media node.
    ready_at: SocketAddr,
    role: &'static str,
    serving: Option<Child>,
}

impl Node {
    fn command(&self, binary: &Path) -> Command {
        let mut command = Command::new(binary);
        // the run's own variables (`NGTEAMS_SERVER_PG_URL`, …) would be read as configuration keys
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("NGTEAMS_SERVER_") {
                command.env_remove(key);
            }
        }
        command
            .arg("--config")
            .arg(&self.config)
            .env("NGTEAMS_SERVER_LOG", "info")
            .env("TOKIO_WORKER_THREADS", "2");
        command
    }

    fn start(&mut self, binary: &Path) -> TestResult {
        let log = fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        let before = log.metadata()?.len();
        let serving = self
            .command(binary)
            .args(["serve", "--role", self.role])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        self.serving = Some(serving);
        self.wait_ready(usize::try_from(before)?)
    }

    /// Waits until it takes connections and, for an API node, its log since `from` says it serves.
    fn wait_ready(&mut self, from: usize) -> TestResult {
        let start = Instant::now();
        loop {
            let log = fs::read(&self.log).unwrap_or_default();
            let said = String::from_utf8_lossy(log.get(from..).unwrap_or_default());
            let listens = TcpStream::connect_timeout(&self.ready_at, Duration::from_millis(200)).is_ok();
            if listens && (self.role == "media" || said.contains("serving")) {
                return Ok(());
            }
            if let Some(status) = self.serving.as_mut().map(Child::try_wait).transpose()?.flatten() {
                return Err(format!("node {} exited {status}: {}", self.name, tail(&said)).into());
            }
            if start.elapsed() > READY {
                return Err(format!("node {} isn't ready at {}: {}", self.name, self.ready_at, tail(&said)).into());
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// `SIGKILL`, as a crash or a lost machine ends it.
    fn kill(&mut self) -> TestResult {
        if let Some(mut serving) = self.serving.take() {
            serving.kill()?;
            serving.wait()?;
        }
        Ok(())
    }

    /// `SIGTERM`, then `SIGKILL` if it doesn't end in time.
    fn stop(&mut self) {
        let Some(mut serving) = self.serving.take() else {
            return;
        };
        let _ = Command::new("kill").args(["-TERM", &serving.id().to_string()]).status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if serving.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = serving.kill();
        let _ = serving.wait();
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut serving) = self.serving.take() {
            let _ = serving.kill();
            let _ = serving.wait();
        }
    }
}

/// The last lines of a log, for an error.
fn tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(15)..].join("\n")
}

/// The running deployment; every process stopped and its database dropped when it ends ([`Nodes::stop`]).
pub struct Nodes {
    /// `https://127.0.0.1:<front>`: the deployment's public origin, what an account's server is.
    pub origin: String,
    /// The certificate authority that signed the API nodes' certificate (PEM): an account's `ca_file`.
    pub ca_file: PathBuf,
    binary: PathBuf,
    api: Vec<Node>,
    media: Node,
    front: Front,
    postgres: String,
    database: String,
    _dir: tempfile::TempDir,
}

impl Nodes {
    /// Makes the database, writes the certificates and the nodes' configuration, migrates, and starts the media node
    /// and both API nodes behind the front; their logs go to `logs`.
    pub fn start(binary: &Path, needs: &Needs, runtime: &tokio::runtime::Runtime, logs: &Path) -> TestResult<Self> {
        Self::start_with(binary, needs, runtime, logs, "")
    }

    /// [`Nodes::start`] with `tables` added to every node's configuration (a `[bots]` table, say).
    pub fn start_with(
        binary: &Path,
        needs: &Needs,
        runtime: &tokio::runtime::Runtime,
        logs: &Path,
        tables: &str,
    ) -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let ca_file = write_certificates(dir.path())?;
        let front = Front::open()?;
        let origin = format!("https://127.0.0.1:{}", front.port);
        let (database, database_url) = runtime.block_on(fresh_database(&needs.postgres))?;
        let storage = storage(needs.s3.as_deref(), dir.path())?;
        let shared = Shared {
            origin: &origin,
            database_url: &database_url,
            redis: &needs.redis,
            storage: &storage,
            calls: &calls(&needs.relay, &needs.secret),
            tables,
        };
        fs::create_dir_all(logs)?;
        let api = ["api-a", "api-b"]
            .into_iter()
            .map(|name| api_node(name, dir.path(), logs, &shared))
            .collect::<TestResult<Vec<_>>>()?;
        let media = media_node(dir.path(), logs, &shared)?;
        front.route_to(api.iter().map(|node| node.ready_at).collect());
        let mut nodes = Self {
            origin,
            ca_file,
            binary: binary.to_owned(),
            api,
            media,
            front,
            postgres: needs.postgres.clone(),
            database,
            _dir: dir,
        };
        nodes.cli(&["migrate"], None)?;
        nodes.media.start(&nodes.binary)?;
        for index in 0..nodes.api.len() {
            nodes.start_api(index)?;
        }
        Ok(nodes)
    }

    /// Starts the API node `index` (again).
    pub fn start_api(&mut self, index: usize) -> TestResult {
        let node = self.api.get_mut(index).ok_or("no such API node")?;
        node.start(&self.binary)
    }

    /// Kills the API node `index` at once (`SIGKILL`).
    pub fn kill_api(&mut self, index: usize) -> TestResult {
        self.api.get_mut(index).ok_or("no such API node")?.kill()
    }

    /// The front sends `pid`'s connections to the API node `node` first; they are counted under `name`.
    pub fn pin(&self, pid: u32, node: usize, name: &str) {
        self.front.pin(pid, node, name);
    }

    /// How many connections of `name`'s went to each API node so far.
    pub fn connections(&self, name: &str) -> Vec<usize> {
        self.front.counts(name, self.api.len())
    }

    /// The server's command line with `args` (and `input` on standard input), as the operator runs it: what it printed.
    pub fn cli(&self, args: &[&str], input: Option<&str>) -> TestResult<String> {
        let node = self.api.first().ok_or("no API node")?;
        let mut running = node
            .command(&self.binary)
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

    /// `user add <upn> --name <name>` (`--admin` when `admin`) with [`PASSWORD`]: the account's MRI.
    pub fn add_user(&self, upn: &str, name: &str, admin: bool) -> TestResult<String> {
        let mut args = vec!["user", "add", upn, "--name", name];
        if admin {
            args.push("--admin");
        }
        let said = self.cli(&args, Some(PASSWORD))?;
        let id = said
            .trim()
            .rsplit_once('(')
            .and_then(|(_, id)| id.strip_suffix(')'))
            .ok_or_else(|| format!("user add said: {said}"))?;
        Ok(format!("8:orgid:{id}"))
    }

    /// The nodes' logs, for an error.
    pub fn logs(&self) -> String {
        let mut said = String::new();
        for node in self.api.iter().chain([&self.media]) {
            let log = fs::read_to_string(&node.log).unwrap_or_default();
            let _ = write!(said, "\n--- {}:\n{}", node.name, tail(&log));
        }
        said
    }

    /// Stops every node and the front and drops the run's database.
    pub fn stop(mut self, runtime: &tokio::runtime::Runtime) -> TestResult {
        for node in &mut self.api {
            node.stop();
        }
        self.media.stop();
        self.front.close();
        runtime.block_on(drop_database(&self.postgres, &self.database))?;
        Ok(())
    }
}

/// What every node of the deployment is configured with alike.
struct Shared<'a> {
    origin: &'a str,
    database_url: &'a str,
    redis: &'a str,
    /// `storage_url`, then its `[storage_options]` table.
    storage: &'a (String, String),
    /// The calls' and the media node's tables.
    calls: &'a str,
    /// The run's own tables, after each node's.
    tables: &'a str,
}

impl Shared<'_> {
    /// The configuration's root keys for a node listening at `listen`, then its tables.
    fn config(&self, listen: SocketAddr, tables: &str) -> String {
        format!(
            "listen = \"{listen}\"\npublic_origin = {:?}\ndatabase_url = {:?}\nredis_url = {:?}\nstorage_url = {:?}\n\
             {}{}{tables}{}",
            self.origin, self.database_url, self.redis, self.storage.0, self.storage.1, self.calls, self.tables,
        )
    }
}

fn api_node(name: &'static str, dir: &Path, logs: &Path, shared: &Shared<'_>) -> TestResult<Node> {
    let listen = SocketAddr::from(([127, 0, 0, 1], free_port()?));
    let tls = format!(
        "[tls]\ncert = \"{}\"\nkey = \"{}\"\n",
        dir.join("cert.pem").display(),
        dir.join("key.pem").display()
    );
    let config = dir.join(format!("{name}.toml"));
    fs::write(&config, shared.config(listen, &tls))?;
    Ok(Node {
        name,
        config,
        log: fresh(logs.join(format!("own-server-nodes-{name}.log"))),
        ready_at: listen,
        role: "api",
        serving: None,
    })
}

fn media_node(dir: &Path, logs: &Path, shared: &Shared<'_>) -> TestResult<Node> {
    let listen = SocketAddr::from(([127, 0, 0, 1], free_port()?));
    let internal = SocketAddr::from(([127, 0, 0, 1], free_port()?));
    let config = dir.join("media.toml");
    // the calls' tables end with `[mixer]`: its internal listener goes there
    fs::write(
        &config,
        shared.config(listen, &format!("internal_listen = \"{internal}\"\n")),
    )?;
    Ok(Node {
        name: "media",
        config,
        log: fresh(logs.join("own-server-nodes-media.log")),
        ready_at: internal,
        role: "media",
        serving: None,
    })
}

/// `log` with an earlier run's file of that name gone.
fn fresh(log: PathBuf) -> PathBuf {
    let _ = fs::remove_file(&log);
    log
}

/// The calls through the run's coturn, a ring that ends soon, and the media node's tables (its socket on loopback,
/// where the loopback coturn relays from; a killed node noticed within seconds). `[mixer]` comes last.
fn calls(relay: &str, secret: &str) -> String {
    format!(
        "[calling]\nring_timeout = 45\n[calling.turn]\nsecret = {secret:?}\nurls = [\"turn:{relay}?transport=udp\"]\n\
         [mixer]\nudp_listen = \"127.0.0.1:0\"\nheartbeat = 1\ndead_after = 3\ndrain_timeout = 3\n"
    )
}

/// A name of this run's own.
fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{}_{nanos}", std::process::id())
}

/// A new database on the Postgres at `base`: its name and URL.
async fn fresh_database(base: &str) -> TestResult<(String, String)> {
    use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
    let name = format!("ngts_ui_{}", unique());
    let mut admin = AsyncPgConnection::establish(base).await?;
    admin.batch_execute(&format!("CREATE DATABASE {name}")).await?;
    let mut url = reqwest::Url::parse(base)?;
    url.set_path(&format!("/{name}"));
    Ok((name, url.to_string()))
}

async fn drop_database(base: &str, name: &str) -> TestResult {
    use diesel_async::{AsyncConnection as _, AsyncPgConnection, SimpleAsyncConnection as _};
    let mut admin = AsyncPgConnection::establish(base).await?;
    admin
        .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .await?;
    Ok(())
}

/// `storage_url` and its `[storage_options]` table: the S3 bucket `s3` names (`http://<key>:<secret>@<host>:<port>/
/// <bucket>`) under a prefix of this run's own, else a directory in `dir` both nodes share.
fn storage(s3: Option<&str>, dir: &Path) -> TestResult<(String, String)> {
    let Some(s3) = s3 else {
        let blobs = dir.join("blobs");
        fs::create_dir_all(&blobs)?;
        return Ok((format!("file://{}", blobs.display()), String::new()));
    };
    let s3 = reqwest::Url::parse(s3)?;
    let bucket = s3.path().trim_matches('/').to_owned();
    let host = s3.host_str().ok_or("no S3 host")?;
    let endpoint = format!("{}://{host}:{}", s3.scheme(), s3.port().ok_or("no S3 port")?);
    let secret = s3.password().ok_or("no S3 secret")?;
    make_bucket(&endpoint, s3.username(), secret, &bucket)?;
    let mut options = "[storage_options]\n".to_owned();
    for (key, value) in [
        ("aws_endpoint", endpoint.as_str()),
        ("aws_region", "us-east-1"),
        ("aws_access_key_id", s3.username()),
        ("aws_secret_access_key", secret),
        ("aws_allow_http", "true"),
        ("aws_virtual_hosted_style_request", "false"),
    ] {
        writeln!(options, "{key} = {value:?}")?;
    }
    Ok((format!("s3://{bucket}/ui-{}", unique()), options))
}

/// Makes `bucket` on the S3 service at `endpoint` unless it is there, as the server's own suite does: a signed `PUT`
/// of the bucket (`curl --aws-sigv4`).
fn make_bucket(endpoint: &str, key: &str, secret: &str, bucket: &str) -> TestResult {
    let answer = Command::new("curl")
        .args(["-sS", "-o", "/dev/null", "-w", "%{http_code}", "-X", "PUT"])
        .args(["--aws-sigv4", "aws:amz:us-east-1:s3", "--user"])
        .arg(format!("{key}:{secret}"))
        .arg(format!("{endpoint}/{bucket}"))
        .stdin(Stdio::null())
        .output()?;
    let code = String::from_utf8_lossy(&answer.stdout).into_owned();
    if matches!(code.as_str(), "200" | "409") {
        return Ok(());
    }
    Err(format!(
        "the bucket {bucket}: HTTP {code} {}",
        String::from_utf8_lossy(&answer.stderr)
    )
    .into())
}

/// The process each pinned connection's node is chosen by, and where each one's connections went.
#[derive(Default)]
struct Routes {
    nodes: Vec<SocketAddr>,
    /// `(pid, node, name)`.
    pins: Vec<(u32, usize, String)>,
    /// `(name, node)` → connections sent there.
    sent: BTreeMap<(String, usize), usize>,
}

/// The TLS pass-through front on the origin's port (see the module docs).
struct Front {
    port: u16,
    routes: Arc<Mutex<Routes>>,
    closed: Arc<AtomicBool>,
    accepting: Option<thread::JoinHandle<()>>,
}

impl Front {
    fn open() -> TestResult<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let routes = Arc::new(Mutex::new(Routes::default()));
        let closed = Arc::new(AtomicBool::new(false));
        let (routes_in, closed_in) = (Arc::clone(&routes), Arc::clone(&closed));
        let accepting = thread::Builder::new()
            .name("own-server-front".to_owned())
            .spawn(move || accept(&listener, port, &routes_in, &closed_in))?;
        Ok(Self {
            port,
            routes,
            closed,
            accepting: Some(accepting),
        })
    }

    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn route_to(&self, nodes: Vec<SocketAddr>) {
        self.routes().nodes = nodes;
    }

    fn pin(&self, pid: u32, node: usize, name: &str) {
        self.routes().pins.push((pid, node, name.to_owned()));
    }

    fn counts(&self, name: &str, nodes: usize) -> Vec<usize> {
        let routes = self.routes();
        (0..nodes)
            .map(|node| routes.sent.get(&(name.to_owned(), node)).copied().unwrap_or_default())
            .collect()
    }

    fn close(&mut self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.close();
    }
}

fn accept(listener: &TcpListener, port: u16, routes: &Arc<Mutex<Routes>>, closed: &AtomicBool) {
    while !closed.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((client, peer)) => {
                let routes = Arc::clone(routes);
                let _ = thread::Builder::new()
                    .name("own-server-front-conn".to_owned())
                    .spawn(move || forward(client, peer, port, &routes));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
            Err(_) => return,
        }
    }
}

/// The nodes to try for the connection from `peer`, in order, and the name it is counted under.
fn order_for(peer: SocketAddr, port: u16, routes: &Mutex<Routes>) -> (Vec<(usize, SocketAddr)>, String) {
    let inode = socket_inode(peer.port(), port);
    let routes = routes.lock().unwrap_or_else(PoisonError::into_inner);
    let pinned = inode.and_then(|inode| {
        routes
            .pins
            .iter()
            .find(|(pid, _, _)| holds(*pid, inode))
            .map(|(_, node, name)| (*node, name.clone()))
    });
    let (first, name) = pinned.unwrap_or_else(|| (0, "other".to_owned()));
    let count = routes.nodes.len();
    let order = (0..count)
        .map(|step| (first + step) % count)
        .filter_map(|node| Some((node, *routes.nodes.get(node)?)))
        .collect();
    (order, name)
}

fn forward(client: TcpStream, peer: SocketAddr, port: u16, routes: &Mutex<Routes>) {
    let (order, name) = order_for(peer, port, routes);
    for (node, address) in order {
        let Ok(upstream) = TcpStream::connect_timeout(&address, CONNECT) else {
            continue;
        };
        *routes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sent
            .entry((name, node))
            .or_default() += 1;
        pipe(client, upstream);
        return;
    }
}

/// Copies both ways until either side ends.
fn pipe(client: TcpStream, upstream: TcpStream) {
    let (Ok(mut client_in), Ok(mut upstream_out)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let up = thread::spawn(move || {
        let _ = io::copy(&mut client_in, &mut upstream_out);
        let _ = upstream_out.shutdown(Shutdown::Write);
    });
    let (mut upstream_in, mut client_out) = (upstream, client);
    let _ = io::copy(&mut upstream_in, &mut client_out);
    let _ = client_out.shutdown(Shutdown::Both);
    let _ = upstream_in.shutdown(Shutdown::Both);
    let _ = up.join();
}

/// The inode of the client's socket of a loopback connection from `client_port` to the front's `front_port`.
fn socket_inode(client_port: u16, front_port: u16) -> Option<u64> {
    let table = fs::read_to_string("/proc/net/tcp").ok()?;
    let (local, remote) = (
        format!("0100007F:{client_port:04X}"),
        format!("0100007F:{front_port:04X}"),
    );
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) == Some(&local.as_str()) && fields.get(2) == Some(&remote.as_str()) {
            fields.get(9)?.parse().ok()
        } else {
            None
        }
    })
}

/// Whether process `pid` holds the socket `inode`.
fn holds(pid: u32, inode: u64) -> bool {
    let wanted = format!("socket:[{inode}]");
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    fds.flatten()
        .any(|fd| fs::read_link(fd.path()).is_ok_and(|link| link.as_os_str() == wanted.as_str()))
}
