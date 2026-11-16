//! Windows has no exec. Start the next image in the same runner job, then exit; the runner retains the broker and
//! timeout until every app image has ended. Originals and working clones remain noninheritable throughout.
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::io::AsHandle as _;
use std::path::Path;
use std::process::Command;
use teams_auth::broker::BrokerStream;
use teams_platform::windows_process::Transfer;

impl super::RestartDescriptors {
    /// Start the successor with only fresh broker/trace duplicates, then exit. Failed launches close all temporary
    /// duplicates and keep the originals private and usable.
    pub fn exec(&self, command: &mut Command) -> io::Error {
        if let Err(error) = validate(command) {
            return error;
        }
        let BrokerStream::NamedPipe(socket) = &self.socket;
        let mut handles = vec![socket.inner().as_handle()];
        if let Some(trace) = &self.trace {
            handles.push(trace.as_handle());
        }
        let transfer = match Transfer::new(&handles) {
            Ok(transfer) => transfer,
            Err(error) => return error,
        };
        let args: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
        let args = match rewritten(&args, &transfer, self.trace.is_some()) {
            Ok(args) => args,
            Err(error) => return error,
        };
        // Only the arguments are changed, using a fresh command so the old descriptor numbers cannot slip through.
        let mut next = Command::new(command.get_program());
        next.args(args);
        exec(&next, &transfer)
    }
}

pub(super) fn validate(command: &Command) -> io::Result<()> {
    validate_environment(command)?;
    if command.get_current_dir().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a restart inherits its directory",
        ));
    }
    Ok(())
}

/// An update's restart may name its folder (the installer's own), never its environment.
pub(super) fn validate_environment(command: &Command) -> io::Result<()> {
    if command.get_envs().next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a restart inherits its environment",
        ));
    }
    Ok(())
}

pub(super) fn exec(command: &Command, transfer: &Transfer) -> io::Error {
    let args: Vec<OsString> = command.get_args().map(OsStr::to_os_string).collect();
    match transfer.spawn_in(Path::new(command.get_program()), &args, command.get_current_dir()) {
        Ok(_) => std::process::exit(0),
        Err(error) => error,
    }
}

fn rewritten(args: &[OsString], transfer: &Transfer, trace: bool) -> io::Result<Vec<OsString>> {
    let mut next = Vec::with_capacity(args.len());
    let mut source = args.iter();
    let mut socket_seen = false;
    let mut trace_seen = !trace;
    while let Some(arg) = source.next() {
        let replacement = if arg == "--secrets-fd" {
            socket_seen = true;
            Some((0, "--secrets-fd"))
        } else if arg == "--trace-fd" {
            trace_seen = true;
            Some((1, "--trace-fd"))
        } else {
            None
        };
        if let Some((index, flag)) = replacement {
            source
                .next()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a restart descriptor has no value"))?;
            next.push(flag.into());
            next.push(
                transfer
                    .value(index)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "a restart descriptor was not transferred")
                    })?
                    .to_string()
                    .into(),
            );
        } else if let Some(value) = arg.to_str().and_then(|arg| {
            arg.strip_prefix("--secrets-fd=")
                .map(|_| 0)
                .or_else(|| arg.strip_prefix("--trace-fd=").map(|_| 1))
        }) {
            if value == 0 {
                socket_seen = true;
            } else {
                trace_seen = true;
            }
            let flag = if value == 0 { "--secrets-fd" } else { "--trace-fd" };
            next.push(flag.into());
            next.push(
                transfer
                    .value(value)
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "a restart descriptor was not transferred")
                    })?
                    .to_string()
                    .into(),
            );
        } else {
            next.push(arg.clone());
        }
    }
    if !socket_seen || !trace_seen {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a restart needs all retained descriptors",
        ));
    }
    Ok(next)
}
