//! Native Teams sign-in: the PKCE code exchange, token refresh and storage, and the skypetoken. The interactive step
//! always runs in a browser engine; this crate only handles the tokens.
//!
//! - [`authz`]: the skypetoken exchange and the account's region;
//! - [`broker`]: the credential broker, one account's secrets served over a socket to a sandboxed process, and the
//!   [`secrets::SecretStore`] that process uses;
//! - [`claims`]: reading a JWT's claims, and what may be shown of an access token;
//! - [`pkce`]: the PKCE verifier and challenge, and the OAuth `state`;
//! - [`token`]: the authorize URL, the scope rule, and the grants against Entra's token endpoint;
//! - [`id_token`]: the account an ID token names, checked before a code exchange stores anything;
//! - [`redirect`]: the registered redirect URI, and the code (or error) its redirect carries;
//! - [`refresh`]: the code exchange and refresh with the refresh token kept in a [`secrets::SecretStore`];
//! - [`secrets`]: the store itself;
//! - [`signin`]: the parent's side of the sign-in window (`teams-signin`): the request it is given, the redirect it
//!   hands back, and the code redeemed only for the account the ID token names;
//! - [`session`]: one signed-in account's tokens while a client runs: an access token per audience, refreshed before it
//!   runs out, and the skypetoken renewed after its Entra token.
//!
//! Filled in by M0 Tasks 2, 13 and 14 (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`), M1 Task 4 and M2
//! Task 4 (the broker) and Task 24 (the sign-in window).

pub mod authz;
pub mod broker;
pub mod claims;
pub mod cloud;
pub mod id_token;
pub mod pkce;
pub mod redirect;
pub mod refresh;
pub mod secrets;
pub mod session;
pub mod signin;
pub mod token;

mod wire;
