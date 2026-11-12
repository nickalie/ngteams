//! Synthetic guard replies on private Unix sockets; no `WebKit` process or tenant requests.

#![cfg(test)]
#![cfg(target_os = "linux")]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::thread;

use teams_signin_extension::client::GuardClient;
use teams_signin_extension::protocol::{LINE_LIMIT, Query};

#[test]
fn incomplete_oversized_or_non_utf8_answers_disconnect_and_deny() {
    for answer in [Vec::new(), b"allow".to_vec(), vec![b'a'; LINE_LIMIT], vec![0xff, b'\n']] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guard.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "request 7 GET https://synthetic.test/\n");
            stream.write_all(&answer).unwrap();
        });
        let mut client = GuardClient::connect(&path);
        assert!(client.is_connected());
        assert!(!client.allows(7, None, "https://synthetic.test/"));
        assert!(!client.is_connected());
        assert!(!client.armed(7, 42));
        assert!(!client.allows(7, Some("POST"), "https://synthetic.test/again"));
        peer.join().unwrap();
    }
}

#[test]
fn the_line_limit_counts_bytes_and_accepts_the_exact_boundary() {
    let prefix = "request 7 GET ";
    let query = Query::Request {
        page: 7,
        method: "GET".to_owned(),
        url: "x".repeat(LINE_LIMIT - prefix.len()),
    };
    let line = query.to_line().unwrap();
    assert_eq!(line.len(), LINE_LIMIT);
    assert_eq!(Query::parse(&line), Some(query));
    assert!(Query::parse(&(line + "x")).is_none());
    let unicode = Query::Request {
        page: 7,
        method: "GET".to_owned(),
        url: "é".repeat(LINE_LIMIT / 2),
    };
    assert!(unicode.to_line().is_none());
}

#[test]
fn numeric_overflow_and_control_characters_are_refused() {
    for line in [
        "armed 18446744073709551616 1",
        "armed 1 4294967296",
        "armed  1",
        "request 18446744073709551616 GET https://synthetic.test/",
        "request 1 GE\tT https://synthetic.test/",
        "request 1 GET https://synthetic.test/\0",
        "request 1 GET https://synthetic.test/\n",
    ] {
        assert!(Query::parse(line).is_none(), "{line:?}");
    }
    assert_eq!(
        Query::parse("armed 18446744073709551615 4294967295"),
        Some(Query::Armed {
            page: u64::MAX,
            pid: u32::MAX,
        })
    );
    for (method, url) in [("", "https://synthetic.test/"), ("GET", ""), ("GET", "x\0")] {
        assert!(
            Query::Request {
                page: 7,
                method: method.to_owned(),
                url: url.to_owned(),
            }
            .to_line()
            .is_none()
        );
    }
}
