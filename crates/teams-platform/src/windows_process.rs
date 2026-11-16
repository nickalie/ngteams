//! Explicit Windows handle transfer for isolated runners and the app's final restart. Ordinary children inherit
//! neither broker nor trace. A short-lived transfer set owns inheritable duplicates and launches with an exact
//! handle list, so even unrelated inheritable handles are excluded. No command shell interprets arguments.
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

/// A transfer set. Build the child's arguments with [`Self::value`], since duplicates have their own handle values.
/// Drop this set immediately after spawning. Use it only at the runner boundary or after the GUI has stopped.
#[derive(Debug)]
pub struct Transfer {
    passed: Vec<OwnedHandle>,
    standard: Vec<OwnedHandle>,
}

impl Transfer {
    /// Duplicate only the passed handles and the three standard streams. Null standard streams use the null device.
    ///
    /// # Errors
    /// A native duplication or null-device open fails.
    pub fn new(handles: &[BorrowedHandle<'_>]) -> io::Result<Self> {
        let passed = handles
            .iter()
            .map(|handle| duplicate(handle.as_raw_handle()))
            .collect::<io::Result<_>>()?;
        let standard = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
            .into_iter()
            .map(|kind| {
                // SAFETY: GetStdHandle only queries a process standard handle, and this function does not close it.
                let handle = unsafe { GetStdHandle(kind) };
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    let null_file = std::fs::OpenOptions::new().read(true).write(true).open("NUL")?;
                    duplicate(null_file.as_raw_handle())
                } else {
                    duplicate(handle)
                }
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { passed, standard })
    }

    /// Duplicate an explicit set of standard streams, without inheriting any other process handles.
    /// Workers use private pipes; this retains the same atomic job assignment as the runners.
    ///
    /// # Errors
    /// Duplicating a supplied handle fails.
    pub fn with_standard_streams(standard: [BorrowedHandle<'_>; 3]) -> io::Result<Self> {
        Ok(Self {
            passed: Vec::new(),
            standard: standard
                .into_iter()
                .map(|handle| duplicate(handle.as_raw_handle()))
                .collect::<io::Result<_>>()?,
        })
    }

    /// The child's numeric handle for the passed entry at `index`.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<usize> {
        self.passed.get(index).map(|handle| handle.as_raw_handle() as usize)
    }

    /// Launch an absolute native executable with this exact transfer set, the current directory and environment.
    /// Standard streams are the duplicates selected by the constructor. Shell scripts are refused.
    ///
    /// # Errors
    /// Invalid executable/arguments, attribute-list allocation, or process creation failure.
    pub fn spawn(&self, program: &Path, args: &[OsString]) -> io::Result<Child> {
        self.spawn_with_job(program, args, None, None)
    }

    /// As [`Self::spawn`], in `directory` when one is given (an update's installer starts in its own folder).
    ///
    /// # Errors
    /// Invalid executable/arguments/directory, attribute-list allocation, or process creation failure.
    pub fn spawn_in(&self, program: &Path, args: &[OsString], directory: Option<&Path>) -> io::Result<Child> {
        self.spawn_with_job(program, args, None, directory)
    }

    /// Launch with atomic job assignment, before the child's first instruction. The job handle is never inherited.
    ///
    /// # Errors
    /// Invalid executable/arguments, attribute-list allocation, job assignment or process creation failure.
    pub fn spawn_in_job(&self, program: &Path, args: &[OsString], job: &super::windows_job::Job) -> io::Result<Child> {
        self.spawn_with_job(program, args, Some(job.handle()), None)
    }

    fn spawn_with_job(
        &self,
        program: &Path,
        args: &[OsString],
        job: Option<BorrowedHandle<'_>>,
        directory: Option<&Path>,
    ) -> io::Result<Child> {
        if !program.is_absolute()
            || !program
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "handle transfer requires an absolute native executable",
            ));
        }
        let application = wide(program.as_os_str())?;
        let directory = directory.map(|directory| wide(directory.as_os_str())).transpose()?;
        let mut command_line = command_line(program, args)?;
        let handles: Vec<HANDLE> = self
            .passed
            .iter()
            .chain(&self.standard)
            .map(AsRawHandle::as_raw_handle)
            .collect();
        let job_handles: Vec<HANDLE> = job.iter().map(AsRawHandle::as_raw_handle).collect();
        let attributes = Attributes::new(&handles, &job_handles)?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = self.standard[0].as_raw_handle();
        startup.StartupInfo.hStdOutput = self.standard[1].as_raw_handle();
        startup.StartupInfo.hStdError = self.standard[2].as_raw_handle();
        startup.lpAttributeList = attributes.pointer;
        let mut information = PROCESS_INFORMATION::default();
        // SAFETY: all strings are NUL-terminated, command_line is writable, and the attribute list, its handle
        // array and the owning transfer set outlive this call. Only those handles are inherited. A null environment
        // or directory inherits this process's value. The child process and primary thread outputs are fresh.
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                directory.as_ref().map_or(null(), Vec::as_ptr),
                &raw const startup.StartupInfo,
                &raw mut information,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful CreateProcessW returned a fresh, uniquely owned thread handle.
        let thread = unsafe { OwnedHandle::from_raw_handle(information.hThread) };
        drop(thread);
        // SAFETY: successful CreateProcessW returned a fresh, uniquely owned process handle.
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess) };
        Ok(Child {
            process,
            id: information.dwProcessId,
        })
    }
}

fn duplicate(handle: HANDLE) -> io::Result<OwnedHandle> {
    // SAFETY: GetCurrentProcess returns a pseudo-handle, not owned by this function.
    let current = unsafe { GetCurrentProcess() };
    let mut copied = null_mut();
    // SAFETY: the source handle is borrowed through this call; the result is a fresh inheritable duplicate.
    if unsafe { DuplicateHandle(current, handle, current, &raw mut copied, 0, 1, DUPLICATE_SAME_ACCESS) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle returned a fresh owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(copied) })
}

struct Attributes {
    // usize alignment satisfies the opaque native attribute list's pointer alignment.
    _buffer: Vec<usize>,
    pointer: LPPROC_THREAD_ATTRIBUTE_LIST,
}
impl Attributes {
    fn new(handles: &[HANDLE], jobs: &[HANDLE]) -> io::Result<Self> {
        let count = 1 + u32::from(!jobs.is_empty());
        let mut size = 0;
        // SAFETY: this first sizing query deliberately supplies a null list, with size as its output.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &raw mut size) };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = vec![0_usize; size.div_ceil(size_of::<usize>())];
        let pointer = buffer.as_mut_ptr().cast();
        // SAFETY: buffer is aligned and has at least size bytes; it stays allocated through Attributes' lifetime.
        if unsafe { InitializeProcThreadAttributeList(pointer, count, 0, &raw mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let attributes = Self {
            _buffer: buffer,
            pointer,
        };
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, handles)?;
        if !jobs.is_empty() {
            attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST as usize, jobs)?;
        }
        Ok(attributes)
    }

    fn set(&self, kind: usize, values: &[HANDLE]) -> io::Result<()> {
        // SAFETY: values remains alive until CreateProcessW returns. The initialized list has room for this kind.
        if unsafe {
            UpdateProcThreadAttribute(
                self.pointer,
                0,
                kind,
                values.as_ptr().cast(),
                size_of_val(values),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the initialized attribute list is still owned by _buffer, and is deleted once before deallocation.
        unsafe { DeleteProcThreadAttributeList(self.pointer) };
    }
}

/// A native child handle, independent of its PID's possible later reuse. Dropping it closes the observation handle.
#[derive(Debug)]
pub struct Child {
    process: OwnedHandle,
    id: u32,
}
impl Child {
    #[must_use]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Wait up to `timeout` for an exit code. `None` means still running.
    ///
    /// # Errors
    /// A native wait or exit-code query failed.
    pub fn wait(&self, timeout: Duration) -> io::Result<Option<u32>> {
        let millis = u32::try_from(timeout.as_millis())
            .unwrap_or(u32::MAX - 1)
            .min(u32::MAX - 1);
        // SAFETY: process is owned through the wait; timeout is finite and never the INFINITE sentinel.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), millis) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                // SAFETY: process is valid and code is a writable output.
                if unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Some(code))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<u16> = value.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "an argument contains NUL"));
    }
    units.push(0);
    Ok(units)
}

// The Windows C runtime's quoting rules: backslashes escape a quote or a closing quote, and are literal otherwise.
// Work on UTF-16 units directly so unpaired surrogates survive. No shell, percent expansion or PowerShell is used.
fn command_line(program: &Path, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for part in std::iter::once(program.as_os_str()).chain(args.iter().map(OsString::as_os_str)) {
        if !line.is_empty() {
            line.push(32);
        }
        line.push(34);
        let mut slashes = 0;
        for unit in part.encode_wide() {
            match unit {
                0 => return Err(io::Error::new(io::ErrorKind::InvalidInput, "an argument contains NUL")),
                92 => slashes += 1,
                _ => {
                    line.extend(std::iter::repeat_n(
                        92,
                        if unit == 34 { slashes * 2 + 1 } else { slashes },
                    ));
                    slashes = 0;
                    line.push(unit);
                }
            }
        }
        line.extend(std::iter::repeat_n(92, slashes * 2));
        line.push(34);
    }
    line.push(0);
    if line.len() > 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the command line exceeds Windows' limit",
        ));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt as _;
    #[test]
    fn raw_utf16_arguments_survive_without_lossy_unicode_conversion() {
        let line = command_line(Path::new(r"C:\app.exe"), &[OsString::from_wide(&[0xd800, 120])]).unwrap();
        assert_eq!(&line[line.len() - 5..], &[34, 0xd800, 120, 34, 0]);
    }
}
