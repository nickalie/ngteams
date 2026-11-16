//! Real native subprocesses, synthetic files only: handle whitelisting, exact argument transfer and bounded waits.
#![cfg(all(test, windows))]
use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, FromRawHandle as _};
use std::path::Path;
use std::time::Duration;
use teams_platform::windows_process::Transfer;
use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT, SetHandleInformation};

const MARKER: &str = "NGTEAMS_PROCESS_SYNTHETIC";
fn arguments(mode: &str) -> Vec<OsString> {
    [
        "--exact",
        "transfer_child",
        "--nocapture",
        "--test-threads=1",
        "--",
        MARKER,
        mode,
    ]
    .map(OsString::from)
    .to_vec()
}

#[test]
fn only_explicit_handles_and_uninterpreted_arguments_reach_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let trace = dir.path().join("trace");
    let file = File::create(&trace).unwrap();
    let unrelated_path = dir.path().join("unrelated");
    let unrelated = File::create(&unrelated_path).unwrap();
    // SAFETY: this live synthetic file intentionally stays inheritable, to test the launcher's whitelist.
    let updated = unsafe { SetHandleInformation(unrelated.as_raw_handle(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(updated, 0);
    let transfer = Transfer::new(&[file.as_handle()]).unwrap();
    let passed = transfer.value(0).unwrap();
    assert!(transfer.value(1).is_none());
    let mut args = arguments("write");
    args.extend([
        passed.to_string().into(),
        (unrelated.as_raw_handle() as usize).to_string().into(),
        unrelated_path.canonicalize().unwrap().into_os_string(),
    ]);
    let payload: Vec<OsString> = vec![
        "".into(),
        "hello world".into(),
        r#"a\"b\\"#.into(),
        "quotes\"\"".into(),
        r"ends in slash\".into(),
        "日本語 🎉".into(),
        "%PATH% $env:PATH $(bad) `bad` & | >".into(),
    ];
    args.extend(payload.clone());
    let child = transfer.spawn(&std::env::current_exe().unwrap(), &args).unwrap();
    assert_ne!(child.id(), 0);
    drop(transfer);
    assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
    let actual = std::fs::read(&trace).unwrap();
    let expected: Vec<u8> = payload
        .iter()
        .flat_map(|part| part.encode_wide().chain([0]).flat_map(u16::to_le_bytes))
        .collect();
    assert_eq!(actual, expected);
    let mut flags = 0;
    // SAFETY: file is still owned in this process, and flags is a live output.
    let valid = unsafe { GetHandleInformation(file.as_raw_handle(), &raw mut flags) };
    assert_ne!(valid, 0);
    assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
}

#[test]
fn private_standard_streams_are_transferred_without_changing_the_parents_streams() {
    use std::io::Read as _;
    let (input, mut writing) = std::io::pipe().unwrap();
    let (mut reading, output) = std::io::pipe().unwrap();
    let null = std::fs::OpenOptions::new().write(true).open("NUL").unwrap();
    let transfer = Transfer::with_standard_streams([input.as_handle(), output.as_handle(), null.as_handle()]).unwrap();
    let job = teams_platform::windows_job::Job::new().unwrap();
    let child = transfer
        .spawn_in_job(&std::env::current_exe().unwrap(), &arguments("pipes"), &job)
        .unwrap();
    drop((transfer, input, output));
    writing.write_all(b"synthetic private request").unwrap();
    drop(writing);
    assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
    let mut text = String::new();
    reading.read_to_string(&mut text).unwrap();
    assert!(text.contains("synthetic private request"));
}

#[test]
fn waits_are_bounded_and_job_termination_stops_the_owned_process() {
    let transfer = Transfer::new(&[]).unwrap();
    let job = teams_platform::windows_job::Job::new().unwrap();
    let child = transfer
        .spawn_in_job(&std::env::current_exe().unwrap(), &arguments("sleep"), &job)
        .unwrap();
    assert_eq!(child.wait(Duration::ZERO).unwrap(), None);
    job.terminate(1).unwrap();
    assert_eq!(child.wait(Duration::from_secs(10)).unwrap(), Some(1));
}

/// An update's installer starts in its own folder (`spawn_in`), not in the app's: Windows refuses to move a folder that
/// a process works in, and the upgrade moves the app's `bin`, where its Start menu shortcut starts it.
#[test]
fn a_child_started_in_a_folder_works_there_and_that_folder_cannot_be_moved_meanwhile() {
    let dir = tempfile::tempdir().unwrap();
    let (stage, bin) = (dir.path().join("stage"), dir.path().join("bin"));
    std::fs::create_dir_all(&stage).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let report = dir.path().join("directory");
    let transfer = Transfer::new(&[]).unwrap();
    let mut args = arguments("directory");
    args.push(report.clone().into_os_string());
    let held = stage.join("held");
    std::fs::create_dir_all(&held).unwrap();
    let child = transfer
        .spawn_in(&std::env::current_exe().unwrap(), &args, Some(&held))
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !report.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::canonicalize(std::fs::read_to_string(&report).unwrap()).unwrap(),
        std::fs::canonicalize(&held).unwrap()
    );
    // the folder the child works in can't be moved while it runs; any other folder can
    assert!(std::fs::rename(&held, dir.path().join("moved")).is_err());
    std::fs::rename(&bin, dir.path().join("bin-moved")).unwrap();
    assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
    std::fs::rename(&held, dir.path().join("moved")).unwrap();
}

#[test]
fn invalid_launches_fail_before_creating_a_child() {
    let transfer = Transfer::new(&[]).unwrap();
    for program in [
        Path::new("relative.exe"),
        Path::new(r"C:\script.cmd"),
        Path::new(r"C:\script.ps1"),
    ] {
        assert_eq!(
            transfer.spawn(program, &[]).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
    assert!(
        transfer
            .spawn(&std::env::current_exe().unwrap(), &[OsString::from_wide(&[0])])
            .is_err()
    );
    assert!(
        transfer
            .spawn(&std::env::current_exe().unwrap(), &["a".repeat(32768).into()])
            .is_err()
    );
    assert_eq!(
        transfer
            .spawn(Path::new(r"C:\ngteams-synthetic-missing.exe"), &[])
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn job_retains_restart_descendants_and_drop_kills_them() {
    for explicit_stop in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let job = teams_platform::windows_job::Job::new().unwrap();
        assert_eq!(job.active_processes().unwrap(), 0);
        let transfer = Transfer::new(&[]).unwrap();
        let mut args = arguments("restart");
        args.push(ready.clone().into_os_string());
        let child = transfer
            .spawn_in_job(&std::env::current_exe().unwrap(), &args, &job)
            .unwrap();
        assert_eq!(child.wait(Duration::from_secs(20)).unwrap(), Some(0));
        // The process handle can signal before job accounting removes that exiting image. Wait for accounting to
        // settle; its successor must remain supervised after the original is gone.
        let accounting_deadline = std::time::Instant::now() + Duration::from_secs(10);
        while job.active_processes().unwrap() != 1 {
            assert!(std::time::Instant::now() < accounting_deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(&ready).unwrap().parse::<u32>().unwrap();
        // SAFETY: opens an observation-only handle to this synthetic child's reported PID.
        let process = unsafe { windows_sys::Win32::System::Threading::OpenProcess(0x0010_0000, 0, pid) };
        assert!(!process.is_null());
        // SAFETY: OpenProcess returned a fresh unique handle, used only for a bounded exit wait.
        let process = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process) };
        if explicit_stop {
            job.terminate(124).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while job.active_processes().unwrap() != 0 {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        drop(job); // Also works when the runner exits without an explicit stop.
        // SAFETY: the process observation handle is valid through this bounded wait.
        let waited =
            unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(process.as_raw_handle(), 10000) };
        assert_eq!(waited, 0);
    }
}

#[test]
fn transfer_child() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let Some(start) = args.iter().position(|value| value == MARKER) else {
        return;
    };
    if args[start + 1] == "pipes" {
        use std::io::Read as _;
        let mut text = String::new();
        std::io::stdin().read_to_string(&mut text).unwrap();
        std::io::stdout().write_all(text.as_bytes()).unwrap();
        return;
    }
    if args[start + 1] == "restart" {
        let child = Transfer::new(&[])
            .unwrap()
            .spawn(&std::env::current_exe().unwrap(), &arguments("sleep"))
            .unwrap();
        std::fs::write(&args[start + 2], child.id().to_string()).unwrap();
        return;
    }
    if args[start + 1] == "directory" {
        let directory = std::env::current_dir().unwrap();
        let written = args[start + 2].to_str().unwrap().to_owned() + ".tmp";
        std::fs::write(&written, directory.to_str().unwrap()).unwrap();
        std::fs::rename(&written, &args[start + 2]).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        return;
    }
    if args[start + 1] == "sleep" {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let passed = args[start + 2].to_str().unwrap().parse::<usize>().unwrap();
    let unrelated = args[start + 3].to_str().unwrap().parse::<usize>().unwrap();
    let mut name = [0_u16; 4096];
    // SAFETY: a numeric handle is only queried; invalid/reused handles return failure or a different path. The
    // output buffer is valid. No ownership is taken and no potentially reused object is changed or closed.
    let length = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW(unrelated as _, name.as_mut_ptr(), 4096, 0)
    };
    assert_ne!(
        OsString::from_wide(&name[..usize::try_from(length).unwrap().min(4096)]),
        args[start + 4]
    );
    // SAFETY: the test runner transferred this fresh file handle once, solely for this helper to own.
    let mut file = unsafe { File::from_raw_handle(passed as _) };
    for part in &args[start + 5..] {
        for unit in part.encode_wide().chain([0]) {
            file.write_all(&unit.to_le_bytes()).unwrap();
        }
    }
}
