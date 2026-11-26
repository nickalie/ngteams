//! An own server's certificate authority: trusted by that account's cloud alone, beside the system's.

use teams_auth::authz::AuthzClient;
use teams_auth::cloud::{Cloud, InvalidRoots};
use teams_auth::token::TokenClient;

#[test]
fn only_an_own_server_trusts_a_certificate_authority_of_its_own() {
    let authority = rcgen::generate_simple_self_signed(vec!["chat.example".to_owned()]).unwrap();
    let pem = authority.cert.pem();
    let own = Cloud::own("https://chat.example").unwrap();
    assert!(own.extra_roots().is_none());
    let trusting = own.clone().trusting(pem.as_bytes()).unwrap();
    assert_eq!(trusting.extra_roots(), Some(pem.as_bytes()));
    assert_eq!(trusting.origin(), own.origin());
    assert_ne!(trusting, own, "an account's cloud is its origin and what it trusts");
    assert!(!format!("{trusting:?}").contains("BEGIN CERTIFICATE"));
    assert!(trusting.trust(reqwest::Client::builder()).build().is_ok());
    assert!(TokenClient::for_cloud(&trusting).is_ok());
    assert!(AuthzClient::for_cloud(&trusting).is_ok());
    for bad in [
        "",
        "not a certificate",
        "-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----\n",
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        assert_eq!(own.clone().trusting(bad.as_bytes()), Err(InvalidRoots), "{bad:?}");
    }
    assert_eq!(Cloud::Microsoft.trusting(pem.as_bytes()), Err(InvalidRoots));
    assert!(Cloud::Microsoft.extra_roots().is_none());
    assert!(TokenClient::for_cloud(&Cloud::Microsoft).is_ok());
}
