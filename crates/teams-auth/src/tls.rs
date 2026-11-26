//! The TLS client config of an account's connections, for the HTTP transport (reqwest, [`Cloud::trust`]) and the
//! Trouter socket alike: the system's certificate verifier on `aws-lc-rs`. An own server with a certificate authority
//! of its own ([`Cloud::trusting`]) is checked against that authority first and against the system's verifier after.
//!
//! The two verifiers stay separate because the system verifier has no form with extra roots on Android.

use std::sync::Arc;

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{self, CryptoProvider};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};

use crate::cloud::Cloud;

/// The client config of `cloud`'s connections, without ALPN: each caller sets the protocols it speaks.
///
/// # Errors
/// When the system verifier cannot be made, or the certificate authority that `cloud` names cannot be read.
pub fn client_config(cloud: &Cloud) -> Result<ClientConfig, Error> {
    let provider = Arc::new(crypto::aws_lc_rs::default_provider());
    let system = rustls_platform_verifier::Verifier::new(Arc::clone(&provider))?;
    let verifier: Arc<dyn ServerCertVerifier> = match cloud.extra_roots() {
        Some(pem) => Arc::new(OwnOrSystem {
            own: own_verifier(pem, Arc::clone(&provider))?,
            system,
        }),
        None => Arc::new(system),
    };
    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth())
}

/// Whether `pem` holds at least one certificate authority and every one of them can be read.
pub(crate) fn readable_authority(pem: &[u8]) -> bool {
    own_verifier(pem, Arc::new(crypto::aws_lc_rs::default_provider())).is_ok()
}

fn own_verifier(pem: &[u8], provider: Arc<CryptoProvider>) -> Result<Arc<WebPkiServerVerifier>, Error> {
    let unreadable = |error: &dyn std::fmt::Display| Error::General(format!("the certificate authority: {error}"));
    let mut roots = RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(pem) {
        roots
            .add(certificate.map_err(|error| unreadable(&error))?)
            .map_err(|error| unreadable(&error))?;
    }
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|error| unreadable(&error))
}

/// An own server's certificate authority, then the system's verifier.
#[derive(Debug)]
struct OwnOrSystem {
    own: Arc<WebPkiServerVerifier>,
    system: rustls_platform_verifier::Verifier,
}

impl ServerCertVerifier for OwnOrSystem {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.own
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .or_else(|own| {
                self.system
                    .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
                    .map_err(|system| more_telling(own, system))
            })
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.own.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.own.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.own.supported_verify_schemes()
    }
}

/// The own authority's error when the certificate chains to it (expired, wrong name), else the system's.
fn more_telling(own: Error, system: Error) -> Error {
    match own {
        Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer) => system,
        own => own,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_authority_is_an_error() {
        for pem in [
            "no certificate at all",
            "-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----\n",
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        ] {
            let error = own_verifier(pem.as_bytes(), Arc::new(crypto::aws_lc_rs::default_provider())).unwrap_err();
            assert!(
                error.to_string().contains("the certificate authority: "),
                "{pem:?}: {error}"
            );
            assert!(!readable_authority(pem.as_bytes()), "{pem:?}");
        }
    }

    #[test]
    fn the_own_authority_error_wins_unless_it_does_not_know_the_issuer() {
        use rustls::CertificateError::{Expired, UnknownIssuer};
        let system = || Error::InvalidCertificate(UnknownIssuer);
        assert_eq!(
            more_telling(Error::InvalidCertificate(Expired), system()),
            Error::InvalidCertificate(Expired)
        );
        assert_eq!(
            more_telling(
                Error::InvalidCertificate(UnknownIssuer),
                Error::General("system".to_owned())
            ),
            Error::General("system".to_owned())
        );
    }
}
