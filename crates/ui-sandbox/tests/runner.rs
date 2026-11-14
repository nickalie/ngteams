//! The runner's command line and private control socket, using synthetic cat processes only.

#![cfg(target_os = "linux")]

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixListener;
    use std::path::Path;
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    use ui_sandbox::{Tools, required};

    const RUNNER: &str = env!("CARGO_BIN_EXE_ui-sandbox-run");

    struct Runner(Child);

    impl Drop for Runner {
        fn drop(&mut self) {
            if self.0.try_wait().unwrap().is_some() {
                return;
            }
            if let Ok(pid) = libc::pid_t::try_from(self.0.id()) {
                // SAFETY: sends a termination request to our still-owned child, whose handler cleans up its sandbox.
                unsafe { libc::kill(pid, libc::SIGTERM) };
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if self.0.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn ask(dir: &Path, words: &[&str]) -> Output {
        Command::new(RUNNER).args(words).arg("--dir").arg(dir).output().unwrap()
    }

    fn okay(output: &Output) {
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.stdout.starts_with(b"ok ") || output.stdout == b"ok\n",
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    fn start(dir: &Path) -> Runner {
        let log = File::create(dir.join("runner.log")).unwrap();
        let child = Command::new(RUNNER)
            .arg("serve")
            .arg("--dir")
            .arg(dir)
            .args([
                "--broker",
                "/bin/sh",
                "--broker-arg",
                "-c",
                "--broker-arg",
                "exec /bin/cat <&{broker-fd}",
                "--app",
                "/bin/sh",
                "--app-arg",
                "-c",
                "--app-arg",
                "printf 'synthetic runner trace\\n' >&{trace-fd}; exec /bin/cat <&{secrets-fd}",
            ])
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut runner = Runner(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let text = fs::read_to_string(dir.join("runner.log")).unwrap_or_default();
            if text.lines().any(|line| line.starts_with("ready sandbox="))
                && fs::read_to_string(dir.join("trace.jsonl")).is_ok_and(|trace| trace == "synthetic runner trace\n")
            {
                break;
            }
            assert!(
                runner.0.try_wait().unwrap().is_none(),
                "runner ended before readiness: {text}"
            );
            assert!(Instant::now() < deadline, "runner never became ready: {text}");
            std::thread::sleep(Duration::from_millis(20));
        }
        runner
    }

    fn tools_available() -> bool {
        match Tools::find() {
            Ok(_) => true,
            Err(error) => {
                assert!(!required(), "{error}");
                eprintln!("skipping runner: {error}");
                false
            }
        }
    }

    #[test]
    fn the_runner_drives_only_its_private_sandbox_and_stops_both_synthetic_peers() {
        if !tools_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut runner = start(dir.path());
        let holder = fs::read_to_string(dir.path().join("holder.pid")).unwrap();
        assert_eq!(holder.trim(), runner.0.id().to_string());
        assert_eq!(
            fs::read_to_string(dir.path().join("trace.jsonl")).unwrap(),
            "synthetic runner trace\n"
        );
        let pasted = dir.path().join("synthetic-clipboard.txt");
        fs::write(&pasted, "synthetic clipboard").unwrap();
        let pasted = pasted.to_str().unwrap();
        for command in [
            vec!["clipboard", "text/plain", pasted, "text/html", pasted],
            vec!["screen", "synthetic_screen"],
            vec!["windows"],
            vec!["key", "ctrl+shift+Tab"],
            vec!["type", "synthetic text"],
            vec!["click", "10", "20"],
            vec!["scroll", "10", "20", "2"],
            vec!["scroll", "10", "20", "-2"],
            vec!["scroll", "10", "20", "0"],
            vec!["screen", "Synthetic-2"],
        ] {
            okay(&ask(dir.path(), &command));
        }
        assert!(image::open(dir.path().join("shots/synthetic_screen.png")).is_ok());
        for command in [
            vec!["shot", "../outside"],
            vec!["screen", ""],
            vec!["key", "invalid-key"],
            vec!["click", "bad", "20"],
            vec!["click", "1", "2", "3"],
            vec!["scroll", "bad", "20", "1"],
            vec!["scroll", "10", "bad", "1"],
            vec!["scroll", "10", "20", "bad"],
            vec!["scroll", "10", "20"],
            vec!["scroll", "10", "20", "1", "extra"],
            vec!["type", "💻"],
            vec!["clipboard"],
            vec!["clipboard", "text/plain"],
            vec!["clipboard", "text/plain", "synthetic-clipboard.txt"],
            vec!["unknown"],
        ] {
            let output = ask(dir.path(), &command);
            assert!(!output.status.success(), "{command:?}");
            assert!(
                output.stdout.starts_with(b"error "),
                "{command:?}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
        }
        okay(&ask(dir.path(), &["stop"]));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = runner.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "runner did not stop");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!dir.path().join("control.sock").exists());
        assert!(
            fs::read_to_string(dir.path().join("runner.log"))
                .unwrap()
                .contains("stopped: stop asked")
        );
    }

    #[test]
    fn missing_and_unknown_arguments_fail_before_starting_a_sandbox() {
        for args in [
            vec![],
            vec!["serve"],
            vec!["serve", "--dir"],
            vec!["serve", "--unknown", "value"],
            vec!["serve", "--dir", "/unused", "--app", "/bin/cat"],
            vec!["windows"],
        ] {
            let output = Command::new(RUNNER).args(&args).output().unwrap();
            assert!(!output.status.success(), "{args:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
        }
        let dir = tempfile::tempdir().unwrap();
        let output = ask(dir.path(), &["windows"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no ui-sandbox-run serve"));
    }

    #[test]
    fn an_unstartable_broker_or_app_reports_failure_without_claiming_readiness() {
        if !tools_available() {
            return;
        }
        for missing_broker in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let missing = dir.path().join("synthetic-missing-peer");
            let cat = Path::new("/bin/cat");
            let (broker, app) = if missing_broker {
                (missing.as_path(), cat)
            } else {
                (cat, missing.as_path())
            };
            let output = Command::new(RUNNER)
                .arg("serve")
                .arg("--dir")
                .arg(dir.path())
                .arg("--broker")
                .arg(broker)
                .arg("--app")
                .arg(app)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{missing_broker}: {output:?}");
            assert!(!String::from_utf8_lossy(&output.stdout).contains("ready sandbox="));
            assert!(String::from_utf8_lossy(&output.stderr).contains("No such file"));
        }
    }

    #[test]
    fn the_client_sends_one_line_and_uses_the_servers_success_or_failure() {
        for (reply, success) in [("ok synthetic answer\n", true), ("error synthetic refusal\n", false)] {
            let dir = tempfile::tempdir().unwrap();
            let listener = UnixListener::bind(dir.path().join("control.sock")).unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&stream).read_line(&mut line).unwrap();
                assert_eq!(line, "type synthetic words\n");
                stream.write_all(reply.as_bytes()).unwrap();
            });
            let output = ask(dir.path(), &["type", "synthetic", "words"]);
            assert_eq!(output.status.success(), success);
            assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), reply.trim_end());
            server.join().unwrap();
        }
    }

    #[test]
    fn the_client_refuses_a_multiline_command_without_writing_to_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("control.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            assert_eq!(BufReader::new(stream).read_line(&mut line).unwrap(), 0);
        });
        let output = ask(dir.path(), &["type", "synthetic\nstop"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("a command is one line"));
        server.join().unwrap();
    }
}
