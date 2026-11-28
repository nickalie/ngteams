//! The sign-in bound (`teams_app::signin_bound`): the host list, and the host gate that opens a tunnel only to a host
//! on it (a local server stands in for Entra, through `SignInHosts::for_tests`), every other connection refused and
//! logged. Every host and value is made up.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use teams_app::signin_bound::egress::{GateLog, HostGate, request_line};
use teams_app::signin_bound::hosts::{ENTRA_HOSTS, SignInHosts};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// What the gate logged, in order: `refuse <method> <target>` or `tunnel <host>:<port>`.
#[derive(Default)]
struct Recorded(Mutex<Vec<String>>);

impl Recorded {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn push(&self, line: String) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(line);
    }
}

impl GateLog for Recorded {
    fn refuse(&self, method: &str, target: &str) {
        self.push(format!("refuse {method} {target}"));
    }

    fn tunnel(&self, host: &str, port: u16) {
        self.push(format!("tunnel {host}:{port}"));
    }
}

#[test]
fn the_bound_is_entra_and_the_identity_provider_s_hosts_never_a_microsoft_service() -> TestResult {
    let hosts = SignInHosts::entra_with(&["sso.example.test".to_owned()])?;
    for host in ENTRA_HOSTS.iter().copied().chain(["sso.example.test"]) {
        assert!(hosts.admits(host, 443), "{host}");
        assert!(!hosts.admits(host, 80), "{host} on 80");
    }
    for host in [
        "teams.microsoft.com",
        "graph.microsoft.com",
        "evil.example.test",
        "127.0.0.1",
    ] {
        assert!(!hosts.admits(host, 443), "{host}");
    }
    for refused in [
        "teams.microsoft.com",
        "login.microsoftonline.com",
        "tenant.sharepoint.com",
        "x.cloud.microsoft",
        "Upper.example.test",
        "localhost",
        "-bad.example.test",
    ] {
        assert!(SignInHosts::entra_with(&[refused.to_owned()]).is_err(), "{refused}");
    }
    Ok(())
}

#[test]
fn a_refused_navigation_names_only_a_host_the_user_could_add_to_idp_hosts() -> TestResult {
    // the account names no identity provider yet: its organization's sign-in page is refused, and its host (alone,
    // never the path or the query) is the one the window's answer names
    let hosts = SignInHosts::entra_with(&[])?;
    assert_eq!(
        hosts
            .idp_candidate("https://sso.example.test/adfs/ls/?SAMLRequest=made-up&RelayState=made-up")
            .as_deref(),
        Some("sso.example.test")
    );
    assert_eq!(
        hosts.idp_candidate("https://SSO.Example.test/").as_deref(),
        Some("sso.example.test"),
        "a URL's host is lowercase"
    );
    // nothing to add: a Microsoft service, a host on the bound, an address, a page that isn't https
    for url in [
        "https://go.microsoft.com/fwlink/?LinkId=made-up",
        "https://www.office.com/",
        "https://login.microsoftonline.com:8443/x",
        "https://192.0.2.1/x",
        "http://sso.example.test/adfs/ls/",
        "https://localhost/x",
        "about:blank",
        "not a url",
    ] {
        assert_eq!(hosts.idp_candidate(url), None, "{url}");
    }
    let named = SignInHosts::entra_with(&["sso.example.test".to_owned()])?;
    assert_eq!(
        named.idp_candidate("https://sso.example.test:8443/adfs/ls/"),
        None,
        "on the bound already"
    );
    Ok(())
}

#[test]
fn this_test_build_has_the_offline_bounds() {
    assert!(teams_app::signin_bound::test_support_enabled());
}

#[test]
fn a_request_line_without_its_fields_gives_question_marks() {
    assert_eq!(
        request_line("CONNECT a.example.test:443 HTTP/1.1"),
        ("CONNECT", "a.example.test:443")
    );
    assert_eq!(request_line("GET"), ("GET", "?"));
    assert_eq!(request_line(""), ("?", "?"));
}

/// A local server that answers each connection's first line with `echo: <line>` and closes.
fn echo_server() -> TestResult<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for stream in listener.incoming().map_while(Result::ok) {
            let mut line = String::new();
            let mut reader = BufReader::new(&stream);
            if reader.read_line(&mut line).is_ok() {
                let mut stream = &stream;
                let _ = stream.write_all(format!("echo: {line}").as_bytes());
            }
        }
    });
    Ok(port)
}

/// What the gate answers `request`, read to the end.
fn ask(gate: &HostGate, request: &str) -> TestResult<String> {
    let address = gate.uri().trim_start_matches("http://").to_owned();
    let mut stream = TcpStream::connect(address)?;
    stream.write_all(request.as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    Ok(answer)
}

#[test]
fn the_gate_opens_a_tunnel_only_to_a_host_on_the_bound_and_logs_every_connection() -> TestResult {
    let port = echo_server()?;
    let log = Arc::new(Recorded::default());
    let gate = HostGate::start(Arc::clone(&log), Arc::new(SignInHosts::for_tests(&["127.0.0.1"], port)))?;

    let answer = ask(
        &gate,
        &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\nhello through\n"),
    )?;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with("echo: hello through\n"), "{answer}");
    assert_eq!(log.lines(), [format!("tunnel 127.0.0.1:{port}")]);

    let other = port.wrapping_add(1);
    for (refused, logged) in [
        (
            format!("CONNECT 127.0.0.1:{other} HTTP/1.1\r\n\r\n"),
            format!("refuse CONNECT 127.0.0.1:{other}"),
        ),
        (
            "CONNECT teams.microsoft.com:443 HTTP/1.1\r\n\r\n".to_owned(),
            "refuse CONNECT teams.microsoft.com:443".to_owned(),
        ),
        (
            format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n\r\n"),
            format!("refuse GET http://127.0.0.1:{port}/"),
        ),
        ("nonsense\r\n\r\n".to_owned(), "refuse nonsense ?".to_owned()),
    ] {
        let answer = ask(&gate, &refused)?;
        assert!(answer.starts_with("HTTP/1.1 403"), "{refused:?}: {answer}");
        assert_eq!(log.lines().last(), Some(&logged));
    }
    Ok(())
}

#[test]
fn the_host_gate_rejects_invalid_authorities_and_reports_unavailable_allowed_services() -> TestResult {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    // the port stays reserved while the refusals run: none of them may connect upstream
    let log = Arc::new(Recorded::default());
    let gate = HostGate::start(Arc::clone(&log), Arc::new(SignInHosts::for_tests(&["127.0.0.1"], port)))?;
    for authority in [
        "missing-port",
        ":443",
        "user@127.0.0.1:443",
        "[::1]:443",
        "127.0.0.1:invalid",
    ] {
        assert!(ask(&gate, &format!("CONNECT {authority} HTTP/1.1\r\n\r\n"))?.starts_with("HTTP/1.1 403"));
    }
    // an end of file before the blank line never starts an upstream connection
    assert!(ask(&gate, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n"))?.is_empty());
    drop(listener);
    assert!(ask(&gate, &format!("CONNECT 127.0.0.1:{port} HTTP/1.1\r\n\r\n"))?.starts_with("HTTP/1.1 502"));
    assert_eq!(log.lines().iter().filter(|line| line.starts_with("refuse")).count(), 5);
    assert_eq!(log.lines().iter().filter(|line| line.starts_with("tunnel")).count(), 1);
    Ok(())
}

#[test]
fn a_page_on_the_bound_is_https_on_a_host_of_it_without_user_info() -> TestResult {
    let hosts = SignInHosts::entra_with(&["sso.example.test".to_owned()])?;
    assert_eq!(hosts.idp_hosts(), ["sso.example.test"]);
    assert!(hosts.admits_url("https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize"));
    assert!(hosts.admits_url("https://sso.example.test/adfs/ls/"));
    for refused in [
        "http://login.microsoftonline.com/x",
        "https://login.microsoftonline.com:8443/x",
        "https://user@login.microsoftonline.com/x",
        "https://teams.microsoft.com/v2/",
        "not a url",
    ] {
        assert!(!hosts.admits_url(refused), "{refused}");
    }
    // a test's bound on another port takes the plain scheme its port implies
    let local = SignInHosts::for_tests(&["127.0.0.1"], 8080);
    assert!(local.admits_url("http://127.0.0.1:8080/authorize"));
    assert!(!local.admits_url("http://127.0.0.1:8081/authorize"));
    Ok(())
}

#[test]
fn the_personal_rules_take_the_identity_provider_s_posts_and_entra_s_return_paths() -> TestResult {
    use teams_app::signin_bound::classify::{Classification, Classifier, OwnKind};
    let classifier = Classifier::personal(&SignInHosts::entra_with(&["sso.example.test".to_owned()])?);
    assert_eq!(
        classifier.classify("POST", "https://sso.example.test/any/path?x=1"),
        Classification::Own(OwnKind::IdentityProvider)
    );
    assert_eq!(
        classifier.classify(
            "POST",
            "https://login.microsoftonline.com/common/SAS/BeginAuth?any=query"
        ),
        Classification::Own(OwnKind::EntraMultiFactor)
    );
    assert_eq!(
        classifier.classify("POST", "https://other.example.test/any/path"),
        Classification::Unknown
    );
    assert_eq!(
        Classifier::sign_in().classify("POST", "https://login.microsoftonline.com/common/SAS/EndAuth"),
        Classification::Unknown
    );
    Ok(())
}
