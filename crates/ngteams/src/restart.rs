//! Lab broker/trace originals retained across the explicit restart after sign-in.
//!
//! Unix keeps their descriptor numbers and close-on-exec flags, clearing the flags only for self-exec and restoring
//! them on failure. Windows leaves originals noninheritable, transfers fresh duplicates with an exact handle list,
//! and rewrites the successor's descriptor arguments. Ordinary children, including sign-in workers, get neither.

use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd as _, BorrowedFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
use std::process::Command;
use teams_auth::broker::BrokerStream;

#[cfg(unix)]
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};

/// The adopted broker/trace originals, held until the app either quits or executes itself again.
#[derive(Debug)]
pub struct RestartDescriptors {
    socket: BrokerStream,
    trace: Option<File>,
}

impl RestartDescriptors {
    /// Keeps originals already made close-on-exec by [`crate::descriptors`].
    #[must_use]
    pub fn new(socket: BrokerStream, trace: Option<File>) -> Self {
        Self { socket, trace }
    }

    /// A close-on-exec clone for the app's credential store.
    ///
    /// # Errors
    /// The socket cannot be duplicated.
    pub fn socket(&self) -> io::Result<BrokerStream> {
        #[cfg(windows)]
        use interprocess::TryClone as _;
        self.socket.try_clone()
    }

    /// A close-on-exec clone for the app's optional view trace.
    ///
    /// # Errors
    /// The file cannot be duplicated.
    pub fn trace(&self) -> io::Result<Option<File>> {
        self.trace.as_ref().map(File::try_clone).transpose()
    }

    /// Executes the app again, inheriting these originals under their existing descriptor numbers.
    ///
    /// Does not return on success. The returned error leaves both descriptors' original flags restored.
    #[cfg(unix)]
    pub fn exec(&self, command: &mut Command) -> io::Error {
        let inherited = match Inherited::new(self) {
            Ok(inherited) => inherited,
            Err(error) => return error,
        };
        let error = command.exec();
        drop(inherited);
        error
    }
}

/// Restores even a partly prepared descriptor set if a later flag operation fails.
#[cfg(unix)]
struct Inherited<'a> {
    flags: Vec<(BorrowedFd<'a>, FdFlags)>,
}

#[cfg(unix)]
impl<'a> Inherited<'a> {
    fn new(descriptors: &'a RestartDescriptors) -> io::Result<Self> {
        let mut inherited = Self {
            flags: Vec::with_capacity(2),
        };
        for fd in std::iter::once(descriptors.socket.as_fd()).chain(descriptors.trace.iter().map(File::as_fd)) {
            let flags = fcntl_getfd(fd)?;
            inherited.flags.push((fd, flags));
            fcntl_setfd(fd, flags & !FdFlags::CLOEXEC)?;
        }
        Ok(inherited)
    }
}

#[cfg(unix)]
impl Drop for Inherited<'_> {
    fn drop(&mut self) {
        for (fd, flags) in &self.flags {
            // The owning RestartDescriptors remains borrowed through restoration, so these descriptors stay open.
            let _ = fcntl_setfd(fd, *flags);
        }
    }
}

#[cfg(windows)]
#[path = "restart/windows.rs"]
mod windows;

/// Replace this process after its UI and engines have stopped. Windows starts the successor in the same runner
/// job before exiting; Unix exec replaces the image. No lab handles are transferred by this personal restart. The
/// successor starts in `command`'s folder when it names one (an update's installer, `teams_update::Restart`).
/// Returns only a launch error.
pub fn exec(command: &mut Command) -> io::Error {
    #[cfg(unix)]
    {
        command.exec()
    }
    #[cfg(windows)]
    {
        if let Err(error) = windows::validate_environment(command) {
            return error;
        }
        match teams_platform::windows_process::Transfer::new(&[]) {
            Ok(transfer) => windows::exec(command, &transfer),
            Err(error) => error,
        }
    }
}
