//! The descriptors the read-only mode is handed at its start (`crate::read_only`), taken over before anything else in
//! the process opens one: the credential broker's socket (`ngteams --read-only --secrets-fd <n>`, and the broker's own
//! end, `ngteams secrets --fd <n>`) and the view trace (`--trace-fd <n>`). Each must be 3 or above, open and of the
//! right kind, and is made close-on-exec.

use std::fs::File;
use std::io;
use std::os::fd::{FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use rustix::fs::{OFlags, fcntl_getfl};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::net::{AddressFamily, SocketType, sockopt};

/// The command-line representation of an inherited native descriptor.
pub type RawDescriptor = RawFd;

/// The first descriptor past stdin, stdout and stderr.
pub const FIRST_PASSED_FD: RawFd = 3;

/// Why a descriptor handed in wasn't taken over.
#[derive(Debug, thiserror::Error)]
pub enum AdoptError {
    #[error("descriptor {0} is stdin, stdout or stderr, or not a descriptor (it is 3 or above)")]
    Reserved(RawFd),
    #[error("descriptor {fd} isn't open: {source}")]
    NotOpen { fd: RawFd, source: io::Error },
    #[error("descriptor {fd} isn't a Unix stream socket")]
    NotAUnixStream { fd: RawFd },
    #[error("descriptor {fd} isn't open for writing")]
    NotWritable { fd: RawFd },
    #[error("descriptor {fd}: {source}")]
    Io { fd: RawFd, source: io::Error },
}

/// Descriptor `fd`, 3 or above and open, owned from now on.
fn adopt(fd: RawFd) -> Result<OwnedFd, AdoptError> {
    if fd < FIRST_PASSED_FD {
        return Err(AdoptError::Reserved(fd));
    }
    // SAFETY: `F_GETFD` only reads the descriptor's flags; a number that isn't an open descriptor fails with `EBADF`.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(AdoptError::NotOpen {
            fd,
            source: io::Error::last_os_error(),
        });
    }
    // SAFETY: the descriptor is open (checked above), and the command line hands it to this process to own: nothing
    // else in the process uses it, since it is taken over before anything else opens a descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn cloexec(owned: &OwnedFd, fd: RawFd) -> Result<(), AdoptError> {
    fcntl_setfd(owned, FdFlags::CLOEXEC).map_err(|error| AdoptError::Io {
        fd,
        source: error.into(),
    })
}

/// Takes over descriptor `fd` as a credential broker's socket: 3 or above, open, a Unix stream socket; made
/// close-on-exec. One that is open but not such a socket is closed; one below 3 or not open is left alone.
///
/// # Errors
/// See [`AdoptError`].
pub fn adopt_socket(fd: RawFd) -> Result<UnixStream, AdoptError> {
    let owned = adopt(fd)?;
    #[cfg(not(target_vendor = "apple"))]
    let domain = sockopt::socket_domain(&owned).map_err(|_| AdoptError::NotAUnixStream { fd })?;
    // macOS has no `SO_DOMAIN`: the socket's own address says its family
    #[cfg(target_vendor = "apple")]
    let domain = rustix::net::getsockname(&owned)
        .map_err(|_| AdoptError::NotAUnixStream { fd })?
        .address_family();
    let kind = sockopt::socket_type(&owned).map_err(|_| AdoptError::NotAUnixStream { fd })?;
    if domain != AddressFamily::UNIX || kind != SocketType::STREAM {
        return Err(AdoptError::NotAUnixStream { fd });
    }
    cloexec(&owned, fd)?;
    Ok(UnixStream::from(owned))
}

/// Takes over descriptor `fd` as the view trace's file: 3 or above, open, open for writing; made close-on-exec. One
/// that is open but not writable is closed; one below 3 or not open is left alone.
///
/// # Errors
/// See [`AdoptError`].
pub fn adopt_trace(fd: RawFd) -> Result<File, AdoptError> {
    let owned = adopt(fd)?;
    let flags = fcntl_getfl(&owned).map_err(|error| AdoptError::Io {
        fd,
        source: error.into(),
    })?;
    if !(flags.contains(OFlags::WRONLY) || flags.contains(OFlags::RDWR)) {
        return Err(AdoptError::NotWritable { fd });
    }
    cloexec(&owned, fd)?;
    Ok(File::from(owned))
}
