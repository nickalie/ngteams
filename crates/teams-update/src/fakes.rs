//! A local fake release feed for tests (feature `test-support`): GitHub's latest release and its downloads over plain
//! HTTP on the loopback ([`crate::Source::local`]), and release trees and archives as the release workflow packs them.
//! Never in a shipped build.

use std::collections::HashMap;
use std::fs;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::{Format, IDENTITY, MARKER};

/// A reply: its status line, extra headers and body; `cut`, only the first half of the body is sent before the
/// connection closes, as when the network goes during a download.
#[derive(Clone)]
pub struct Reply {
    pub status: &'static str,
    pub headers: String,
    pub body: Vec<u8>,
    pub cut: bool,
}

impl Reply {
    #[must_use]
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Self {
            status: "200 OK",
            headers: String::new(),
            body: body.into(),
            cut: false,
        }
    }

    /// `body`'s length announced, but only its first half sent before the connection closes.
    #[must_use]
    pub fn cut(body: impl Into<Vec<u8>>) -> Self {
        Self {
            cut: true,
            ..Self::ok(body)
        }
    }

    #[must_use]
    pub fn status(status: &'static str) -> Self {
        Self {
            status,
            headers: String::new(),
            body: Vec::new(),
            cut: false,
        }
    }
}

/// The feed, serving `routes` by path until dropped; what it was asked is kept.
pub struct Feed {
    pub base: String,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    pub asked: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

/// The lock's value, whether or not a test thread panicked holding it.
fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Feed {
    /// A feed on a free loopback port, serving nothing yet.
    ///
    /// # Errors
    /// The loopback can't be listened on.
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let base = format!("http://{}", listener.local_addr()?);
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let asked: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (served, heard, stopping) = (Arc::clone(&routes), Arc::clone(&asked), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => answer(socket, &served, &heard),
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Ok(Self {
            base,
            routes,
            asked,
            stop,
        })
    }

    /// Serves `reply` at `path` from now on.
    pub fn serve(&self, path: &str, reply: Reply) {
        locked(&self.routes).insert(path.to_owned(), reply);
    }

    /// The request lines and headers asked, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        locked(&self.asked).clone()
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn answer(mut socket: TcpStream, routes: &Mutex<HashMap<String, Reply>>, asked: &Mutex<Vec<String>>) {
    if socket.set_nonblocking(false).is_err() || socket.set_read_timeout(Some(Duration::from_secs(5))).is_err() {
        return;
    }
    let mut request = Vec::new();
    let mut byte = [0];
    while !request.ends_with(b"\r\n\r\n") {
        if socket.read_exact(&mut byte).is_err() {
            return;
        }
        request.push(byte[0]);
    }
    let request = String::from_utf8_lossy(&request).into_owned();
    let path = request.split_whitespace().nth(1).unwrap_or_default().to_owned();
    locked(asked).push(request);
    let reply = locked(routes)
        .get(&path)
        .cloned()
        .unwrap_or_else(|| Reply::status("404 Not Found"));
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
        reply.status,
        reply.body.len(),
        reply.headers
    );
    let _ = socket.write_all(head.as_bytes());
    let sent = if reply.cut {
        reply.body.len() / 2
    } else {
        reply.body.len()
    };
    let _ = socket.write_all(&reply.body[..sent]);
}

/// A release's description as GitHub's API gives it, its assets served under `base`'s downloads.
#[must_use]
pub fn release_json(base: &str, tag: &str, assets: &[(&str, usize)]) -> String {
    let assets: Vec<String> = assets
        .iter()
        .map(|(name, size)| {
            format!(r#"{{"name":"{name}","size":{size},"browser_download_url":"{base}/download/{tag}/{name}"}}"#)
        })
        .collect();
    format!(
        r#"{{"tag_name":"{tag}","draft":false,"prerelease":false,"assets":[{}]}}"#,
        assets.join(",")
    )
}

/// Publishes `asset` (its bytes) and its `SHA256SUMS` as the latest release `tag` of `feed`.
pub fn publish(feed: &Feed, tag: &str, asset: &str, bytes: &[u8]) {
    let sums = format!("{}  {asset}\n{}  other-file\n", sha256(bytes), "0".repeat(64));
    feed.serve(
        "/latest",
        Reply::ok(release_json(
            &feed.base,
            tag,
            &[(asset, bytes.len()), ("SHA256SUMS", sums.len())],
        )),
    );
    feed.serve(&format!("/download/{tag}/{asset}"), Reply::ok(bytes.to_vec()));
    feed.serve(&format!("/download/{tag}/SHA256SUMS"), Reply::ok(sums));
}

/// A release tree as `scripts/package-release.sh` lays one out (or, for [`Format::WindowsSetup`], as the Windows
/// installer does), at `root`, made as `version` and marked `format`.
///
/// # Errors
/// The files can't be written.
pub fn tree(root: &Path, version: &str, format: Format) -> std::io::Result<()> {
    let programs: &[&str] = if format == Format::WindowsSetup {
        &["bin/ngteams.exe", "bin/ngteams-webview.exe", "bin/ngteamsctl.exe"]
    } else {
        &[
            "AppRun",
            "usr/bin/ngteams",
            "usr/bin/ngteams-web",
            "usr/bin/ngteamsctl",
            "usr/bin/libteams_signin_extension.so",
        ]
    };
    for program in programs {
        let file = root.join(program);
        fs::create_dir_all(file.parent().unwrap_or(root))?;
        fs::write(&file, format!("{program} {version}"))?;
    }
    for path in [MARKER, IDENTITY] {
        fs::create_dir_all(root.join(path).parent().unwrap_or(root))?;
    }
    fs::write(root.join(MARKER), format!("{}\n", format.marker()))?;
    fs::write(root.join(IDENTITY), format!("{version}\n"))
}

/// The `.tar.gz` of a release tree of `version`, its tree under `ngteams/`; `edit` changes the tree before packing.
///
/// # Errors
/// The tree can't be written or packed.
pub fn archive_bytes(version: &str, edit: impl FnOnce(&Path)) -> std::io::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let root = dir.path().join(crate::FOLDER);
    tree(&root, version, Format::Archive)?;
    edit(&root);
    let mut packed = Vec::new();
    {
        let encoder = flate2::write::GzEncoder::new(&mut packed, flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        // no modification times or owners: the same release gives the same bytes, so its published size and
        // checksum hold for a copy made again
        builder.mode(tar::HeaderMode::Deterministic);
        builder.append_dir_all(crate::FOLDER, &root)?;
        builder.into_inner()?.finish()?;
    }
    Ok(packed)
}

/// `bytes`' SHA-256, lowercase hex.
#[must_use]
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    crate::prepare::hex(&sha2::Sha256::digest(bytes))
}

/// Writes `body` as an executable script at `path`, through a short-lived `sh` of its own: a file this process wrote
/// can't be run while another thread's fork still holds it open for writing ("Text file busy").
///
/// # Errors
/// `sh` can't run, or it couldn't write the file.
pub fn program(path: &Path, body: &str) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut writer = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = writer.stdin.take() {
        stdin.write_all(body.as_bytes())?;
    }
    if writer.wait()?.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{} couldn't be written", path.display())))
    }
}
