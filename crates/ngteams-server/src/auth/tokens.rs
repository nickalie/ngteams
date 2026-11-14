//! The tokens the server mints, in the shapes the client reads from Entra and authz (`docs/protocol/auth.md`):
//!
//! - an access token per audience: `aud` is the resource the client asked for (`<resource>/.default` mapped to
//!   `<resource>`), with `oid`, `tid`, `upn`, `name`, `scp` and `exp`;
//! - an ID token for the client (`aud` its client id) naming the account;
//! - the skypetoken: `skypeid` (`orgid:<oid>`), `rgn` and the `exp` of the access token it was exchanged for.
//!
//! All three are EdDSA JWTs from the [`KeyRing`], with this server's issuer; the skypetoken's audience is its own, so
//! no access token passes for a skypetoken, nor the other way round. Refresh tokens are opaque (`crate::db::refresh`).

use std::time::{Duration, SystemTime};

use jsonwebtoken::{Algorithm, Validation};
use serde::{Deserialize, Serialize};

use super::keys::{Invalid, KeyError, KeyRing};
use crate::db::users::User;
use crate::db::{now, seconds, unix};

/// The audience of a skypetoken, which no resource may be named.
pub const SKYPETOKEN_AUDIENCE: &str = "urn:ngteams-server:skypetoken";
/// Clock skew allowed when checking a token's times.
const LEEWAY: u64 = 60;
/// Scopes asked with every resource, which name no resource.
const SIGN_IN_SCOPES: [&str; 4] = ["openid", "profile", "offline_access", "email"];

/// An access token's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub oid: String,
    pub sub: String,
    pub tid: String,
    pub upn: String,
    pub preferred_username: String,
    pub name: String,
    pub scp: String,
    pub azp: String,
    pub ver: String,
    pub uti: String,
}

/// An ID token's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub oid: String,
    pub sub: String,
    pub tid: String,
    pub preferred_username: String,
    pub name: String,
    pub ver: String,
}

/// A skypetoken's claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkypeClaims {
    pub aud: String,
    pub iss: String,
    pub iat: i64,
    pub exp: i64,
    /// `orgid:<oid>`: the account's MRI is `8:` and this.
    pub skypeid: String,
    pub rgn: String,
    pub tid: String,
    pub oid: String,
    pub jti: String,
}

impl SkypeClaims {
    /// The account's MRI.
    #[must_use]
    pub fn mri(&self) -> String {
        format!("8:{}", self.skypeid)
    }
}

/// A requested resource, as a token request's `scope` names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requested {
    /// The token's `aud`.
    pub audience: String,
    /// The token's `scp`.
    pub scope: String,
}

/// The resource a `scope` asks for, when it is exactly one the server mints tokens for: `<resource>/.default` (a
/// resource ending in `/` is asked as `<resource>//.default`) or a named scope `<resource>/<name>`, beside any of
/// `openid profile offline_access email`.
#[must_use]
pub fn requested(scope: &str, allowed: &[String]) -> Option<Requested> {
    let mut resources = scope.split_whitespace().filter(|part| !SIGN_IN_SCOPES.contains(part));
    let resource = resources.next()?;
    if resources.next().is_some() {
        return None;
    }
    let (audience, name) = match resource.strip_suffix("/.default") {
        Some(audience) => (audience, "user_impersonation"),
        None => resource.rsplit_once('/').filter(|(_, name)| !name.is_empty())?,
    };
    allowed.iter().any(|allowed| allowed == audience).then(|| Requested {
        audience: audience.to_owned(),
        scope: name.to_owned(),
    })
}

/// Mints and checks the server's JWTs.
#[derive(Debug)]
pub struct Issuer {
    keys: KeyRing,
    issuer: String,
    tenant: String,
    region: String,
    access_lifetime: Duration,
}

/// A minted token and when it runs out.
#[derive(Clone, Debug)]
pub struct Minted {
    pub token: String,
    pub expires_at: i64,
}

impl Issuer {
    /// An issuer for `tenant` at `origin` (`iss` is `<origin>/<tenant>/v2.0`, as Entra's v2 issuers are).
    #[must_use]
    pub fn new(keys: KeyRing, origin: &str, tenant: &str, region: &str, access_lifetime: Duration) -> Self {
        Self {
            keys,
            issuer: format!("{origin}/{tenant}/v2.0"),
            tenant: tenant.to_owned(),
            region: region.to_owned(),
            access_lifetime,
        }
    }

    #[must_use]
    pub fn keys(&self) -> &KeyRing {
        &self.keys
    }

    #[must_use]
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// An access token for `user` and `requested`, to `client_id`.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn access(&self, user: &User, requested: &Requested, client_id: &str) -> Result<Minted, KeyError> {
        let iat = now();
        let exp = iat + seconds(self.access_lifetime);
        let claims = AccessClaims {
            aud: requested.audience.clone(),
            iss: self.issuer.clone(),
            iat,
            nbf: iat,
            exp,
            oid: user.id.clone(),
            sub: user.id.clone(),
            tid: self.tenant.clone(),
            upn: user.upn.clone(),
            preferred_username: user.upn.clone(),
            name: user.display_name.clone(),
            scp: requested.scope.clone(),
            azp: client_id.to_owned(),
            ver: "2.0".to_owned(),
            uti: uuid::Uuid::new_v4().simple().to_string(),
        };
        Ok(Minted {
            token: self.keys.sign(&claims)?,
            expires_at: exp,
        })
    }

    /// An ID token naming `user`, for `client_id`.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn id_token(&self, user: &User, client_id: &str) -> Result<String, KeyError> {
        let iat = now();
        self.keys.sign(&IdClaims {
            aud: client_id.to_owned(),
            iss: self.issuer.clone(),
            iat,
            nbf: iat,
            exp: iat + seconds(self.access_lifetime),
            oid: user.id.clone(),
            sub: user.id.clone(),
            tid: self.tenant.clone(),
            preferred_username: user.upn.clone(),
            name: user.display_name.clone(),
            ver: "2.0".to_owned(),
        })
    }

    /// A skypetoken for the account an access token names, running out when that token does.
    ///
    /// # Errors
    /// No signing key, or signing fails.
    pub fn skypetoken(&self, access: &AccessClaims) -> Result<Minted, KeyError> {
        let claims = SkypeClaims {
            aud: SKYPETOKEN_AUDIENCE.to_owned(),
            iss: self.issuer.clone(),
            iat: now(),
            exp: access.exp,
            skypeid: format!("orgid:{}", access.oid),
            rgn: self.region.clone(),
            tid: self.tenant.clone(),
            oid: access.oid.clone(),
            jti: uuid::Uuid::new_v4().simple().to_string(),
        };
        Ok(Minted {
            token: self.keys.sign(&claims)?,
            expires_at: claims.exp,
        })
    }

    /// The claims of an access token for `audience`.
    ///
    /// # Errors
    /// [`Invalid`]: not ours, another audience, or expired.
    pub async fn verify_access(&self, token: &str, audience: &str) -> Result<AccessClaims, Invalid> {
        self.keys.verify(token, &self.validation(audience)).await
    }

    /// The claims of a skypetoken.
    ///
    /// # Errors
    /// [`Invalid`]: not ours, not a skypetoken, or expired.
    pub async fn verify_skypetoken(&self, token: &str) -> Result<SkypeClaims, Invalid> {
        self.keys.verify(token, &self.validation(SKYPETOKEN_AUDIENCE)).await
    }

    fn validation(&self, audience: &str) -> Validation {
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.leeway = LEEWAY;
        validation.set_audience(&[audience]);
        validation.set_issuer(&[&self.issuer]);
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        validation
    }
}

/// Seconds from now until `at` (none when it has passed).
#[must_use]
pub fn seconds_until(at: i64) -> u64 {
    u64::try_from(at - unix(SystemTime::now())).unwrap_or(0)
}
