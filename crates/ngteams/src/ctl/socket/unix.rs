//! Unix socket binding, stale-file recovery and inode-checked cleanup.
use super::super::BindError;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
#[cfg(target_vendor = "apple")]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use teams_ctl::{ClientError, CodecError, connect_stream};
const PROBE: Duration = Duration::from_millis(500);
/// The Unix socket and the filesystem identity held until shutdown.
#[derive(Debug)]
pub struct Socket {
    listener: UnixListener,
    pub path: PathBuf,
    /// The socket file's device and inode: the stop removes the file only while it is this one.
    file: (u64, u64),
}

/// Bind the socket at `path` (see the module docs).
///
/// # Errors
/// [`BindError::AlreadyRunning`] when an app answers there, [`BindError::NotASocket`] when something else is there, or
/// the directory, the stale file's removal or the bind failed.
pub fn bind(path: &Path) -> Result<Socket, BindError> {
    let failed = |source: io::Error| BindError::Io {
        path: path.to_owned(),
        source,
    };
    teams_ctl::prepare_dir(path).map_err(failed)?;
    match fs::symlink_metadata(path) {
        Ok(found) if !found.file_type().is_socket() => return Err(BindError::NotASocket(path.to_owned())),
        Ok(_) => remove_stale(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    let listener = UnixListener::bind(path).map_err(failed)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(failed)?;
    let bound = fs::symlink_metadata(path).map_err(failed)?;
    Ok(Socket {
        listener,
        path: path.to_owned(),
        file: (bound.dev(), bound.ino()),
    })
}

/// The socket file at `path`, asked whether an app is behind it: removed when nobody is.
fn remove_stale(path: &Path) -> Result<(), BindError> {
    let failed = |source: io::Error| BindError::Io {
        path: path.to_owned(),
        source,
    };
    match connect_stream(path, PROBE) {
        // a busy app (its backlog full) is running all the same
        Ok(_) | Err(ClientError::Timeout) => Err(BindError::AlreadyRunning(path.to_owned())),
        Err(ClientError::NotRunning { source, .. }) if source.kind() == io::ErrorKind::ConnectionRefused => {
            log::info!("the control socket was left by an app that didn't quit: it is replaced");
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(failed(error)),
            }
        }
        // `connect_stream`'s only other `NotRunning`: the file went meanwhile
        Err(ClientError::NotRunning { .. }) => Ok(()),
        // the probe's own failure (permission, a path too long, …), its kind and OS error kept
        Err(ClientError::Codec(CodecError::Io(source))) => Err(failed(source)),
        Err(error) => Err(failed(io::Error::other(error.to_string()))),
    }
}

impl Socket {
    pub fn accept(&self) -> io::Result<teams_ctl::transport::Stream> {
        self.listener.accept().map(|(stream, _)| stream)
    }

    pub fn wake(&self) {
        let _ = rustix::net::shutdown(&self.listener, rustix::net::Shutdown::Both);
        // macOS doesn't let a listening socket be shut down, so its blocked `accept` stays: a connection of this
        // process's own returns it, and the stopped loop closes that connection and ends
        #[cfg(target_vendor = "apple")]
        let _ = UnixStream::connect(&self.path);
    }

    pub fn cleanup(&self) {
        let ours = fs::symlink_metadata(&self.path).is_ok_and(|found| (found.dev(), found.ino()) == self.file);
        if ours && let Err(error) = fs::remove_file(&self.path) {
            log::warn!("the control socket's file wasn't removed: {error}");
        }
    }
}
