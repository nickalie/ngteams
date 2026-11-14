//! The signing keys every node shares through the database: Ed25519 (`EdDSA` JWTs), each named by a `kid`.
//!
//! The unretired key signs. A key is retired when its replacement is created, and deleted
//! once nothing it signed can still be valid. A node that meets a `kid` it doesn't hold reloads the keys (another node
//! rotated), at most once per [`RELOAD_PAUSE`]: tokens with made-up key ids cost the database little.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::db::keys::{self, SigningKey};
use crate::db::{Db, DbError, now, seconds};

/// The shortest time between two reloads for unknown key ids.
pub const RELOAD_PAUSE: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("a signing key can't be made")]
    Generate,
    #[error("no signing key")]
    NoKey,
    #[error("the token can't be signed")]
    Sign,
}

/// Why a token isn't accepted. Says nothing about the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the token isn't valid here")]
pub struct Invalid;

struct Signer {
    kid: String,
    key: EncodingKey,
}

#[derive(Default)]
struct Keys {
    signer: Option<Arc<Signer>>,
    verifiers: HashMap<String, DecodingKey>,
}

/// The node's view of the shared keys.
pub struct KeyRing {
    db: Db,
    keys: RwLock<Keys>,
    last_reload: Mutex<Option<Instant>>,
    rotation: Duration,
    /// How long after its retirement a key may still have to verify a token.
    keep: Duration,
}

impl std::fmt::Debug for KeyRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys = self.keys.read().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("KeyRing")
            .field("signing", &keys.signer.as_ref().map(|signer| signer.kid.clone()))
            .field("verifying", &keys.verifiers.len())
            .finish_non_exhaustive()
    }
}

impl KeyRing {
    /// Loads the keys, making the first one or rotating as due.
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn load(db: Db, rotation: Duration, keep: Duration) -> Result<Self, KeyError> {
        let ring = Self {
            db,
            keys: RwLock::default(),
            last_reload: Mutex::new(None),
            rotation,
            keep,
        };
        ring.rotate_if_due().await?;
        Ok(ring)
    }

    /// Makes a new signing key when the newest is older than the rotation period (or there is none), retires the
    /// others, deletes keys retired longer ago than any token they signed can live, and reloads.
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn rotate_if_due(&self) -> Result<(), KeyError> {
        let at = now();
        let stored = keys::all(&self.db).await?;
        let fresh = stored
            .iter()
            .find(|key| key.retired_at.is_none())
            .is_some_and(|key| at - key.created_at < seconds(self.rotation));
        if !fresh {
            self.rotate().await?;
        }
        keys::delete_retired(&self.db, at - seconds(self.keep)).await?;
        self.reload().await
    }

    /// Makes a new signing key now and retires the others (they still verify).
    ///
    /// # Errors
    /// A database failure, or a key that can't be made.
    pub async fn rotate(&self) -> Result<String, KeyError> {
        let key = generate()?;
        let kid = key.kid.clone();
        keys::rotate(&self.db, &key).await?;
        self.reload().await?;
        Ok(kid)
    }

    /// Reads every key again.
    ///
    /// # Errors
    /// A database failure.
    pub async fn reload(&self) -> Result<(), KeyError> {
        let stored = keys::all(&self.db).await?;
        let signer = stored.iter().find(|key| key.retired_at.is_none()).map(|key| {
            Arc::new(Signer {
                kid: key.kid.clone(),
                key: EncodingKey::from_ed_der(&key.private_der),
            })
        });
        let verifiers = stored
            .iter()
            .map(|key| (key.kid.clone(), DecodingKey::from_ed_der(&key.public_der)))
            .collect();
        *self.keys.write().unwrap_or_else(PoisonError::into_inner) = Keys { signer, verifiers };
        Ok(())
    }

    /// `claims` as a JWT signed with the current key.
    ///
    /// # Errors
    /// No key, or signing fails.
    pub fn sign<T: Serialize>(&self, claims: &T) -> Result<String, KeyError> {
        let signer = self
            .keys
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .signer
            .clone()
            .ok_or(KeyError::NoKey)?;
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some(signer.kid.clone());
        jsonwebtoken::encode(&header, claims, &signer.key).map_err(|_| KeyError::Sign)
    }

    /// The claims of `token` when one of the keys signed it and `validation` (audience, issuer, expiry) holds.
    ///
    /// # Errors
    /// [`Invalid`] for any token that isn't one of ours, valid now.
    pub async fn verify<T: DeserializeOwned>(&self, token: &str, validation: &Validation) -> Result<T, Invalid> {
        let kid = jsonwebtoken::decode_header(token)
            .map_err(|_| Invalid)?
            .kid
            .ok_or(Invalid)?;
        if !self.knows(&kid) && self.may_reload() {
            self.reload().await.map_err(|_| Invalid)?;
        }
        let keys = self.keys.read().unwrap_or_else(PoisonError::into_inner);
        let key = keys.verifiers.get(&kid).ok_or(Invalid)?;
        jsonwebtoken::decode::<T>(token, key, validation)
            .map(|data| data.claims)
            .map_err(|_| Invalid)
    }

    fn knows(&self, kid: &str) -> bool {
        self.keys
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .verifiers
            .contains_key(kid)
    }

    /// Whether an unknown `kid` may reload the keys now; if so, the pause starts.
    fn may_reload(&self) -> bool {
        let mut last = self.last_reload.lock().unwrap_or_else(PoisonError::into_inner);
        let due = last.is_none_or(|last| last.elapsed() >= RELOAD_PAUSE);
        if due {
            *last = Some(Instant::now());
        }
        due
    }
}

/// A fresh Ed25519 key with a random `kid`.
fn generate() -> Result<SigningKey, KeyError> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| KeyError::Generate)?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| KeyError::Generate)?;
    Ok(SigningKey {
        kid: uuid::Uuid::new_v4().simple().to_string(),
        private_der: pkcs8.as_ref().to_vec(),
        public_der: pair.public_key().as_ref().to_vec(),
        created_at: now(),
        retired_at: None,
    })
}
