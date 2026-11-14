//! The listener: plain HTTP (for a reverse proxy in front, or tests on loopback) or TLS with rustls.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum_server::Handle;
use axum_server::tls_rustls::RustlsConfig;
use rustls::pki_types::pem::PemObject as _;

use crate::config::TlsConfig;

/// How long open connections get to finish once shutdown starts.
const GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("can't listen: {0}")]
    Listen(#[source] std::io::Error),
    #[error("can't load the TLS certificate or key: {0}")]
    Tls(#[source] std::io::Error),
}

/// A bound listener, ready to serve.
#[derive(Debug)]
pub struct Listener {
    listener: std::net::TcpListener,
}

impl Listener {
    /// Binds `address`.
    ///
    /// # Errors
    /// The address can't be bound.
    pub fn bind(address: SocketAddr) -> Result<Self, ServeError> {
        let listener = std::net::TcpListener::bind(address).map_err(ServeError::Listen)?;
        listener.set_nonblocking(true).map_err(ServeError::Listen)?;
        Ok(Self { listener })
    }

    /// The address it is bound to (the port, when bound to port 0).
    ///
    /// # Errors
    /// The socket can't say.
    pub fn local_addr(&self) -> Result<SocketAddr, ServeError> {
        self.listener.local_addr().map_err(ServeError::Listen)
    }

    /// Serves `router` until `shutdown` resolves, then lets open connections finish for a while.
    ///
    /// # Errors
    /// The TLS files can't be read, or the listener fails.
    pub async fn serve(
        self,
        router: Router,
        tls: Option<TlsConfig>,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), ServeError> {
        let handle: Handle<SocketAddr> = Handle::new();
        let watcher = handle.clone();
        tokio::spawn(async move {
            shutdown.await;
            watcher.graceful_shutdown(Some(GRACE));
        });
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        match tls {
            Some(tls) => {
                let config = Arc::new(tls_config(&tls).await?);
                axum_server::from_tcp_rustls(self.listener, RustlsConfig::from_config(config))
                    .map_err(ServeError::Listen)?
                    .handle(handle)
                    .serve(service)
                    .await
            }
            None => {
                axum_server::from_tcp(self.listener)
                    .map_err(ServeError::Listen)?
                    .handle(handle)
                    .serve(service)
                    .await
            }
        }
        .map_err(ServeError::Listen)
    }
}

/// The rustls server configuration of a PEM chain and key, on the aws-lc-rs provider, HTTP/2 and HTTP/1.1.
async fn tls_config(tls: &TlsConfig) -> Result<rustls::ServerConfig, ServeError> {
    let chain = tokio::fs::read(&tls.cert).await.map_err(ServeError::Tls)?;
    let key = tokio::fs::read(&tls.key).await.map_err(ServeError::Tls)?;
    let invalid = |what: &str| ServeError::Tls(std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_owned()));
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(&chain)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("unreadable certificate"))?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(&key).map_err(|_| invalid("unreadable key"))?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| invalid("no TLS versions"))?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map_err(|_| invalid("the key doesn't match the certificate"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}
