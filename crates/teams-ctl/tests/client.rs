//! The client against an in-test server on a temporary socket: `hello`, requests, errors, timeouts and a stream.

// test code throughout (its helpers unwrap like the tests do)
#![cfg(test)]

use std::io::{BufReader, Write as _};
#[cfg(unix)]
use std::os::unix::net::UnixListener;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
#[cfg(windows)]
use teams_ctl::transport::Listener as UnixListener;
use teams_ctl::transport::Stream as UnixStream;

#[cfg(unix)]
use rustix::net::{AddressFamily, SocketAddrUnix, SocketType};
use teams_ctl::client::{Client, ClientError, Timeouts};
use teams_ctl::codec::{self, LineReader, decode_request};
use teams_ctl::protocol::{
    CtlError, CtlEvent, ErrorCode, EventFilter, EventLine, Hello, Outcome, PROTOCOL_VERSION, Request, RequestLine,
    Response, ResponseLine,
};

fn accept(listener: &UnixListener) -> UnixStream {
    #[cfg(unix)]
    {
        listener.accept().unwrap().0
    }
    #[cfg(windows)]
    loop {
        match listener.accept() {
            Ok(stream) => return stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(5)),
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn quick() -> Timeouts {
    Timeouts {
        connect: Duration::from_millis(300),
        read: Duration::from_millis(300),
        write: Duration::from_secs(5),
    }
}

/// What the server does with a request: lines to send back, then whether to go on.
type Handler = dyn FnMut(&RequestLine) -> (Vec<String>, bool) + Send;

fn ok(id: u64, response: Response) -> String {
    serde_json::to_string(&ResponseLine {
        id,
        outcome: Outcome::Ok(response),
    })
    .unwrap()
}

fn err(id: u64, code: ErrorCode, message: &str) -> String {
    serde_json::to_string(&ResponseLine {
        id,
        outcome: Outcome::Error(CtlError::new(code, message)),
    })
    .unwrap()
}

fn hello(id: u64, protocol: u32) -> String {
    ok(
        id,
        Response::Hello(Hello {
            protocol,
            app: "9.9.9".to_owned(),
        }),
    )
}

/// A server on its own socket in a fresh directory, answering one connection with `handler` after the `hello`.
struct Server {
    path: PathBuf,
    thread: JoinHandle<Vec<RequestLine>>,
    _dir: tempfile::TempDir,
}

fn server(mut handler: Box<Handler>) -> Server {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let thread = thread::spawn(move || {
        let stream = accept(&listener);
        let mut writer = stream.try_clone().unwrap();
        let mut lines = LineReader::new(BufReader::new(stream));
        let mut seen = Vec::new();
        while let Ok(Some(line)) = lines.read_line() {
            let request = decode_request(&line).unwrap();
            let (answers, go_on) = handler(&request);
            seen.push(request);
            for answer in answers {
                writer.write_all(answer.as_bytes()).unwrap();
                writer.write_all(b"\n").unwrap();
            }
            if !go_on {
                break;
            }
        }
        seen
    });
    Server {
        path,
        thread,
        _dir: dir,
    }
}

/// A server that answers `hello` with this protocol, then `rest` for the requests after it.
fn with_hello(protocol: u32, mut rest: impl FnMut(&RequestLine) -> (Vec<String>, bool) + Send + 'static) -> Server {
    server(Box::new(move |request| match request.request {
        Request::Hello { .. } => (vec![hello(request.id, protocol)], true),
        _ => rest(request),
    }))
}

#[test]
fn hello_then_requests_with_their_ids_and_accounts() {
    let server = with_hello(PROTOCOL_VERSION, |request| match &request.request {
        Request::Status => (vec![ok(request.id, Response::Done)], true),
        Request::Chats { .. } => (vec![err(request.id, ErrorCode::UnknownAccount, "no account 7")], true),
        _ => (vec![ok(request.id, Response::Subscribed)], false),
    });
    let mut client = Client::connect(&server.path, quick()).unwrap();
    assert_eq!(client.hello().app, "9.9.9");
    assert_eq!(client.request(None, Request::Status).unwrap(), Response::Done);
    match client.request(
        Some("7"),
        Request::Chats {
            unread_only: false,
            limit: None,
        },
    ) {
        Err(ClientError::Server(error)) => {
            assert_eq!(error.code, ErrorCode::UnknownAccount);
            assert_eq!(error.message, "no account 7");
        }
        other => panic!("{other:?}"),
    }
    let line = client.request_line(None, Request::Unread).unwrap();
    assert_eq!((line.id, line.outcome), (4, Outcome::Ok(Response::Subscribed)));
    drop(client);

    let seen = server.thread.join().unwrap();
    assert_eq!(seen.iter().map(|line| line.id).collect::<Vec<_>>(), [1, 2, 3, 4]);
    assert!(matches!(
        seen[0].request,
        Request::Hello {
            protocol: PROTOCOL_VERSION,
            ..
        }
    ));
    assert_eq!(seen[2].account.as_deref(), Some("7"));
    assert_eq!(seen[1].account, None);
}

#[test]
fn an_error_line_is_handed_back_as_it_came() {
    let server = with_hello(PROTOCOL_VERSION, |request| {
        (vec![err(request.id, ErrorCode::Stopped, "stopped")], false)
    });
    let mut client = Client::connect(&server.path, quick()).unwrap();
    let line = client.request_line(Some("a@lab.invalid"), Request::Sync).unwrap();
    assert_eq!(
        line.outcome,
        Outcome::Error(CtlError::new(ErrorCode::Stopped, "stopped"))
    );
    server.thread.join().unwrap();
}

#[test]
fn a_refused_subscription_is_a_server_error_or_its_line() {
    let refusing = || {
        with_hello(PROTOCOL_VERSION, |request| {
            (vec![err(request.id, ErrorCode::UnknownChat, "no chat is Nope")], false)
        })
    };
    let refused = CtlError::new(ErrorCode::UnknownChat, "no chat is Nope");

    let server = refusing();
    let client = Client::connect(&server.path, quick()).unwrap();
    match client.subscribe(None, EventFilter::default()) {
        Err(ClientError::Server(error)) => assert_eq!(error, refused),
        other => panic!("{other:?}"),
    }
    server.thread.join().unwrap();

    let server = refusing();
    let client = Client::connect(&server.path, quick()).unwrap();
    let Ok(Err(refusal)) = client.try_subscribe(None, EventFilter::default()) else {
        panic!("not refused");
    };
    assert_eq!(
        ResponseLine::from(refusal),
        ResponseLine {
            id: 2,
            outcome: Outcome::Error(refused),
        }
    );
    server.thread.join().unwrap();
}

#[test]
fn a_server_of_another_version_is_refused() {
    let server = with_hello(PROTOCOL_VERSION + 1, |_| (Vec::new(), false));
    match Client::connect(&server.path, quick()) {
        Err(ClientError::VersionMismatch { ours, theirs }) => {
            assert_eq!((ours, theirs), (PROTOCOL_VERSION, PROTOCOL_VERSION + 1));
        }
        other => panic!("{other:?}"),
    }
    server.thread.join().unwrap();
}

#[test]
fn a_server_refusing_the_clients_version_says_so() {
    let server = server(Box::new(|request| {
        (
            vec![err(request.id, ErrorCode::VersionMismatch, "the app speaks 2")],
            false,
        )
    }));
    match Client::connect(&server.path, quick()) {
        Err(ClientError::Server(error)) => assert_eq!(error.code, ErrorCode::VersionMismatch),
        other => panic!("{other:?}"),
    }
    server.thread.join().unwrap();
}

#[test]
fn an_answer_of_another_kind_or_id_is_a_protocol_error() {
    let server = server(Box::new(|request| (vec![ok(request.id, Response::Done)], false)));
    assert!(matches!(
        Client::connect(&server.path, quick()),
        Err(ClientError::Protocol(_))
    ));
    server.thread.join().unwrap();

    let server = with_hello(PROTOCOL_VERSION, |request| {
        (vec![ok(request.id + 5, Response::Done)], true)
    });
    let mut client = Client::connect(&server.path, quick()).unwrap();
    assert!(matches!(
        client.request(None, Request::Status),
        Err(ClientError::Protocol(_))
    ));
    assert!(matches!(
        client.request_line(None, Request::Status),
        Err(ClientError::Protocol(_))
    ));
    drop(client);
    server.thread.join().unwrap();

    let server = with_hello(PROTOCOL_VERSION, |request| {
        (vec![ok(request.id, Response::Done)], false)
    });
    let client = Client::connect(&server.path, quick()).unwrap();
    assert!(matches!(
        client.subscribe(None, EventFilter::default()),
        Err(ClientError::Protocol(_))
    ));
    server.thread.join().unwrap();
}

#[test]
fn a_connection_closed_before_the_answer_is_closed() {
    // the hello answered, the server closes
    let server = server(Box::new(|request| (vec![hello(request.id, PROTOCOL_VERSION)], false)));
    let mut client = Client::connect(&server.path, quick()).unwrap();
    server.thread.join().unwrap();
    assert!(matches!(
        client.request(None, Request::Status),
        Err(ClientError::Closed | ClientError::Codec(_))
    ));
}

#[test]
fn an_answer_that_does_not_come_in_time_is_a_timeout() {
    let server = with_hello(PROTOCOL_VERSION, |_| (Vec::new(), true));
    let mut client = Client::connect(&server.path, quick()).unwrap();
    let started = Instant::now();
    assert!(matches!(
        client.request(None, Request::Status),
        Err(ClientError::Timeout)
    ));
    assert!(started.elapsed() >= Duration::from_millis(250));
    drop(client);
    server.thread.join().unwrap();
}

#[test]
fn a_late_answer_to_a_timed_out_request_is_skipped() {
    let mut statuses = 0;
    let server = with_hello(PROTOCOL_VERSION, move |request| {
        statuses += 1;
        match statuses {
            // the first status isn't answered in time
            1 => (Vec::new(), true),
            // its late answer comes ahead of the second's
            2 => (
                vec![
                    err(request.id - 1, ErrorCode::Busy, "late"),
                    ok(request.id, Response::Done),
                ],
                true,
            ),
            _ => (vec![ok(request.id, Response::Done)], false),
        }
    });
    let mut client = Client::connect(&server.path, quick()).unwrap();
    assert!(matches!(
        client.request(None, Request::Status),
        Err(ClientError::Timeout)
    ));
    assert!(matches!(client.request(None, Request::Status), Ok(Response::Done)));
    assert_eq!(client.request_line(None, Request::Status).unwrap().id, 4);
    server.thread.join().unwrap();
}

#[test]
fn a_line_that_is_not_an_answer_is_a_codec_error() {
    let server = server(Box::new(|_| (vec!["{\"neither\":true}".to_owned()], false)));
    assert!(matches!(
        Client::connect(&server.path, quick()),
        Err(ClientError::Codec(_))
    ));
    server.thread.join().unwrap();
}

#[test]
fn nothing_listening_is_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("ctl.sock");
    match Client::connect(&missing, quick()) {
        Err(ClientError::NotRunning { path, .. }) => assert_eq!(path, missing),
        other => panic!("{other:?}"),
    }
    #[cfg(unix)]
    {
        // a stale socket file nobody listens on
        drop(UnixListener::bind(&missing).unwrap());
        assert!(missing.exists());
        let error = Client::connect(&missing, quick()).unwrap_err();
        assert!(matches!(error, ClientError::NotRunning { .. }), "{error:?}");
        assert!(error.to_string().starts_with("no ngteams is running on "), "{error}");
        // a path too long for a socket address
        let long = dir.path().join("x".repeat(200));
        let expected = teams_ctl::transport::connect(&long, quick().connect).unwrap_err();
        match Client::connect(&long, quick()) {
            Err(ClientError::Codec(codec::CodecError::Io(error))) => {
                assert_eq!(error.kind(), expected.kind());
                assert_eq!(error.raw_os_error(), expected.raw_os_error());
            }
            other => panic!("invalid socket address lost its native error: {other:?}"),
        }
    }
}

/// A listener with a backlog of nothing, which never accepts: the first connect fills its queue.
#[cfg(unix)]
fn full_listener(path: &Path) -> rustix::fd::OwnedFd {
    let socket = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None).unwrap();
    rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC).unwrap();
    rustix::net::bind(&socket, &SocketAddrUnix::new(path).unwrap()).unwrap();
    rustix::net::listen(&socket, 0).unwrap();
    socket
}

#[cfg(unix)]
#[test]
fn a_connect_that_does_not_get_through_in_time_is_a_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let _listener = full_listener(&path);
    let _queued = UnixStream::connect(&path).unwrap();
    let started = Instant::now();
    let error = Client::connect(&path, quick()).unwrap_err();
    assert!(matches!(error, ClientError::Timeout), "{error:?}");
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(started.elapsed() < Duration::from_secs(5));
}

fn event_line(event: CtlEvent) -> String {
    serde_json::to_string(&EventLine {
        account: Some("a@lab.invalid".to_owned()),
        at: 1,
        event,
    })
    .unwrap()
}

#[test]
fn a_subscription_streams_events_until_the_app_closes_it() {
    let server = with_hello(PROTOCOL_VERSION, |request| match &request.request {
        Request::Subscribe { filter } => {
            assert_eq!(filter.kinds, ["unread"]);
            let mut lines = vec![ok(request.id, Response::Subscribed)];
            lines.push(event_line(CtlEvent::TeamsChanged));
            lines.push(event_line(CtlEvent::Lagged));
            (lines, false)
        }
        other => panic!("{other:?}"),
    });
    let client = Client::connect(&server.path, quick()).unwrap();
    let filter = EventFilter {
        kinds: vec!["unread".to_owned()],
        ..EventFilter::default()
    };
    let events = client.subscribe(Some("0"), filter).unwrap();
    let kinds: Vec<_> = events.map(|line| line.unwrap().event.kind()).collect();
    assert_eq!(kinds, ["teams_changed", "lagged"]);
    let seen = server.thread.join().unwrap();
    assert_eq!(seen[1].account.as_deref(), Some("0"));
}

#[test]
fn a_subscription_waits_for_events_as_long_as_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (go, gate) = std::sync::mpsc::channel::<()>();
    let thread = thread::spawn(move || {
        let stream = accept(&listener);
        let mut writer = stream.try_clone().unwrap();
        let mut lines = LineReader::new(BufReader::new(stream));
        for answer in [hello(1, PROTOCOL_VERSION), ok(2, Response::Subscribed)] {
            let _: RequestLine = lines.read().unwrap().unwrap();
            writer.write_all(format!("{answer}\n").as_bytes()).unwrap();
        }
        // hold the stream open, silent, until the test is done waiting
        assert!(gate.recv().is_err());
    });
    let client = Client::connect(&path, quick()).unwrap();
    let mut events = client.subscribe(None, EventFilter::default()).unwrap();
    // past the client's read timeout, with no bound set: still waiting
    let waiting = thread::spawn(move || {
        let next = events.next_event();
        (events, next)
    });
    thread::sleep(Duration::from_millis(500));
    assert!(!waiting.is_finished());
    drop(go);
    let (_events, next) = waiting.join().unwrap();
    assert!(next.unwrap().is_none());
    thread.join().unwrap();
}

#[test]
fn a_bound_on_a_subscription_times_out_a_silent_stream() {
    let server = with_hello(PROTOCOL_VERSION, |request| {
        (vec![ok(request.id, Response::Subscribed)], true)
    });
    let client = Client::connect(&server.path, quick()).unwrap();
    let mut events = client.subscribe(None, EventFilter::default()).unwrap();
    events.set_read_timeout(Some(Duration::from_millis(100))).unwrap();
    assert!(matches!(events.next_event(), Err(ClientError::Timeout)));
    assert!(matches!(events.next(), Some(Err(ClientError::Timeout))));
    drop(events);
    server.thread.join().unwrap();
}

#[test]
fn client_errors_read_plainly() {
    assert_eq!(ClientError::Timeout.to_string(), "the app didn't answer in time");
    assert_eq!(ClientError::Closed.to_string(), "the app closed the connection");
    assert_eq!(
        ClientError::VersionMismatch { ours: 1, theirs: 2 }.to_string(),
        "the app speaks protocol 2, this client 1"
    );
    assert_eq!(
        ClientError::from(CtlError::new(ErrorCode::Busy, "in a call")).to_string(),
        "busy: in a call"
    );
    let timed_out = ClientError::from(codec::CodecError::Io(std::io::Error::from(
        std::io::ErrorKind::TimedOut,
    )));
    assert!(matches!(timed_out, ClientError::Timeout));
    assert!(matches!(
        ClientError::from(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
        ClientError::Timeout
    ));
    let broken = ClientError::from(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
    assert!(matches!(broken, ClientError::Codec(_)));
    assert_eq!(Timeouts::default().connect, Duration::from_secs(1));
}

#[test]
fn zero_io_timeouts_are_refused_before_hello_and_when_changed_later() {
    for timeouts in [
        Timeouts {
            read: Duration::ZERO,
            ..quick()
        },
        Timeouts {
            write: Duration::ZERO,
            ..quick()
        },
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ctl.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        assert!(matches!(Client::connect(&path, timeouts), Err(ClientError::Codec(_))));
    }
    let server = with_hello(PROTOCOL_VERSION, |request| {
        (vec![ok(request.id, Response::Subscribed)], true)
    });
    let client = Client::connect(&server.path, quick()).unwrap();
    assert!(matches!(
        client.set_read_timeout(Duration::ZERO),
        Err(ClientError::Codec(_))
    ));
    let events = client.subscribe(None, EventFilter::default()).unwrap();
    assert!(matches!(
        events.set_read_timeout(Some(Duration::ZERO)),
        Err(ClientError::Codec(_))
    ));
    drop(events);
    server.thread.join().unwrap();
}

#[cfg(unix)]
#[test]
fn permission_errors_remain_errors_instead_of_implying_no_running_app() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ctl.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = teams_ctl::client::connect_stream(&path, quick().connect);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(result, Err(ClientError::Codec(codec::CodecError::Io(error))) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
}
