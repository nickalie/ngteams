//! The TLS client config (`teams_auth::tls`): an own server's certificate authority is trusted only by the cloud that
//! names it, over TLS 1.2 and 1.3, through a plain handshake and through reqwest ([`Cloud::trust`]).

#![cfg(test)]

use std::sync::Arc;

use rustls::pki_types::{PrivateKeyDer, ServerName};
use rustls::{ServerConfig, SupportedProtocolVersion};
use teams_auth::cloud::Cloud;
use teams_auth::tls::client_config;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Authority {
    pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

fn authority() -> Authority {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "made-up certificate authority");
    let pem = params.self_signed(&key).unwrap().pem();
    Authority {
        pem,
        issuer: rcgen::Issuer::new(params, key),
    }
}

/// A loopback TLS server for `localhost` with a certificate that `authority` signed. It answers one HTTP/1.1 request.
async fn serve(
    authority: &Authority,
    versions: &[&'static SupportedProtocolVersion],
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    let cert = leaf.signed_by(&key, &authority.issuer).unwrap();
    let mut server = ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.der().clone()],
            PrivateKeyDer::try_from(key.serialize_der()).unwrap(),
        )
        .unwrap();
    server.alpn_protocols = vec![b"http/1.1".to_vec()];
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(mut tls) = tokio_rustls::TlsAcceptor::from(Arc::new(server)).accept(stream).await else {
            return;
        };
        let mut request = [0; 1024];
        if tls.read(&mut request).await.is_ok_and(|read| read > 0) {
            let _ = tls
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await;
            let _ = tls.shutdown().await;
        }
    });
    (address, serving)
}

async fn handshake(cloud: &Cloud, authority: &Authority, versions: &[&'static SupportedProtocolVersion]) -> bool {
    let (address, serving) = serve(authority, versions).await;
    let stream = tokio::net::TcpStream::connect(address).await.unwrap();
    let connected = tokio_rustls::TlsConnector::from(Arc::new(client_config(cloud).unwrap()))
        .connect(ServerName::try_from("localhost").unwrap(), stream)
        .await
        .is_ok();
    serving.abort();
    connected
}

#[tokio::test]
async fn only_the_cloud_that_names_the_authority_trusts_it_over_tls_12_and_13() {
    let authority = authority();
    let own = Cloud::own("https://localhost").unwrap();
    let trusting = own.clone().trusting(authority.pem.as_bytes()).unwrap();
    for versions in [&[&rustls::version::TLS12][..], &[&rustls::version::TLS13][..]] {
        assert!(handshake(&trusting, &authority, versions).await, "{versions:?}");
        assert!(!handshake(&own, &authority, versions).await, "{versions:?}");
        assert!(
            !handshake(&Cloud::Microsoft, &authority, versions).await,
            "{versions:?}"
        );
    }
}

#[test]
fn the_config_has_no_alpn_and_names_no_root_in_debug() {
    let authority = authority();
    let trusting = Cloud::own("https://localhost")
        .unwrap()
        .trusting(authority.pem.as_bytes())
        .unwrap();
    for cloud in [Cloud::Microsoft, trusting] {
        let config = client_config(&cloud).unwrap();
        assert!(config.alpn_protocols.is_empty(), "{cloud:?}");
        assert!(!format!("{config:?}").contains("BEGIN CERTIFICATE"));
    }
}

#[tokio::test]
async fn reqwest_reaches_an_own_server_through_its_authority() {
    let authority = authority();
    let own = Cloud::own("https://localhost").unwrap();
    let trusting = own.clone().trusting(authority.pem.as_bytes()).unwrap();

    let (address, serving) = serve(&authority, &[&rustls::version::TLS13]).await;
    let url = format!("https://localhost:{}/", address.port());
    let client = trusting
        .trust(reqwest::Client::builder())
        .resolve("localhost", address)
        .no_proxy()
        .build()
        .unwrap();
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
    serving.await.unwrap();

    let (address, serving) = serve(&authority, &[&rustls::version::TLS13]).await;
    let url = format!("https://localhost:{}/", address.port());
    let client = own
        .trust(reqwest::Client::builder())
        .resolve("localhost", address)
        .no_proxy()
        .build()
        .unwrap();
    assert!(
        client.get(&url).send().await.is_err(),
        "the system's roots alone refuse the server"
    );
    serving.abort();
}
