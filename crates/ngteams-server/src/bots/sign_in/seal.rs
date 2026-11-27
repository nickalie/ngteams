//! Sealing what a bot's sign-in keeps (a person's tokens from an identity provider, a connection's client secret,
//! a sign-in's PKCE verifier): AES-256-GCM under `bots.token_key`, a random nonce before each sealed value, and the
//! row the value belongs to as its additional data, so a sealed value copied to another row doesn't open.

use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

/// `bots.token_key`: 32 bytes in base64. Never shown: its `Debug` names no byte of it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SealKey(String);

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey(…)")
    }
}

impl SealKey {
    /// A key from its base64.
    #[must_use]
    pub fn new(base64: impl Into<String>) -> Self {
        Self(base64.into())
    }

    /// Whether it is 32 bytes in base64.
    #[must_use]
    pub fn usable(&self) -> bool {
        self.bytes().is_some()
    }

    fn bytes(&self) -> Option<[u8; 32]> {
        STANDARD.decode(self.0.trim()).ok()?.try_into().ok()
    }
}

/// Why a sealed value didn't open, or a value wasn't sealed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("a sealed value didn't open or seal")]
pub struct SealError;

/// The sealer of one key.
pub(crate) struct Sealer(LessSafeKey);

impl Sealer {
    /// The sealer of `key`, when it is usable.
    pub(crate) fn new(key: &SealKey) -> Option<Self> {
        let bytes = key.bytes()?;
        let key = UnboundKey::new(&AES_256_GCM, &bytes).ok()?;
        Some(Self(LessSafeKey::new(key)))
    }

    /// `plain` sealed for `context`: its nonce, then the ciphertext and tag.
    pub(crate) fn seal(&self, context: &str, plain: &[u8]) -> Result<Vec<u8>, SealError> {
        let mut nonce = [0_u8; NONCE_LEN];
        aws_lc_rs::rand::fill(&mut nonce).map_err(|_| SealError)?;
        let mut sealed = plain.to_vec();
        self.0
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(context.as_bytes()),
                &mut sealed,
            )
            .map_err(|_| SealError)?;
        let mut out = nonce.to_vec();
        out.append(&mut sealed);
        Ok(out)
    }

    /// What `sealed` holds, when it was sealed for `context` under this key.
    pub(crate) fn open(&self, context: &str, sealed: &[u8]) -> Result<Vec<u8>, SealError> {
        if sealed.len() < NONCE_LEN {
            return Err(SealError);
        }
        let (nonce, rest) = sealed.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SealError)?;
        let mut rest = rest.to_vec();
        let plain = self
            .0
            .open_in_place(nonce, Aad::from(context.as_bytes()), &mut rest)
            .map_err(|_| SealError)?;
        Ok(plain.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> SealKey {
        SealKey::new(STANDARD.encode([byte; 32]))
    }

    #[test]
    fn a_sealed_value_opens_only_with_its_key_and_its_context() {
        let sealer = Sealer::new(&key(7)).unwrap();
        let closed = sealer.seal("token|bot|github|ada", b"synthetic-idp-token").unwrap();
        assert!(!closed.windows(9).any(|window| window == b"synthetic"));
        assert_eq!(
            sealer.open("token|bot|github|ada", &closed).unwrap(),
            b"synthetic-idp-token"
        );
        assert_eq!(sealer.open("token|bot|github|bob", &closed), Err(SealError));
        assert_eq!(
            Sealer::new(&key(8)).unwrap().open("token|bot|github|ada", &closed),
            Err(SealError)
        );
        assert_eq!(sealer.open("token|bot|github|ada", &closed[..5]), Err(SealError));
        let again = sealer.seal("token|bot|github|ada", b"synthetic-idp-token").unwrap();
        assert_ne!(again, closed, "each seal takes a new nonce");
    }

    #[test]
    fn only_32_bytes_in_base64_make_a_key_and_none_shows_in_debug() {
        assert!(key(1).usable());
        assert!(!SealKey::new(STANDARD.encode([1_u8; 16])).usable());
        assert!(!SealKey::new("not base64!").usable());
        assert!(Sealer::new(&SealKey::new("")).is_none());
        assert_eq!(format!("{:?}", key(1)), "SealKey(…)");
    }
}
