//! Secrets that must not leak through logs or errors.
//!
//! `Debug` prints a fixed redaction and there is no `Display`, so a secret
//! reaches the outside only through an explicit call to `expose`.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::{Entropy, Error};

/// A session token: what a client presents to prove it holds a session.
/// `server.db` keeps only its [hash](Self::hash).
#[derive(Clone)]
pub struct SessionToken(String);

impl SessionToken {
    /// Wraps a token read from somewhere, such as the credential file.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// A new token: 32 random bytes, written as 64 hex digits.
    pub(crate) fn generate(entropy: &dyn Entropy) -> Result<Self, Error> {
        let mut bytes = [0; 32];
        entropy.fill(&mut bytes)?;
        Ok(Self(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    /// The token itself. Take care what you do with it.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// What `server.db` keeps: the SHA-256 of the token's text. A token is
    /// 256 random bits, so a plain hash is enough and nothing slower is
    /// needed.
    pub fn hash(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// A server's Ed25519 signing key, as its 32-byte seed. It is made once, on
/// first start, and stored in `server.db`, which it never leaves.
pub(crate) struct SigningKey([u8; 32]);

impl SigningKey {
    pub(crate) fn generate(entropy: &dyn Entropy) -> Result<Self, Error> {
        let mut seed = [0; 32];
        entropy.fill(&mut seed)?;
        Ok(Self(seed))
    }

    /// The key itself. Take care what you do with it.
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// The public half: what clients and peers pin.
    pub(crate) fn public_key(&self) -> [u8; 32] {
        ed25519_dalek::SigningKey::from_bytes(&self.0)
            .verifying_key()
            .to_bytes()
    }
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SigningKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::CountingEntropy;

    #[test]
    fn debug_is_a_fixed_redaction() {
        const SENTINEL: &str = "SENTINEL_SESSION_TOKEN_0001";
        assert_eq!(
            format!("{:?}", SessionToken::new(SENTINEL)),
            "SessionToken(<redacted>)"
        );
        assert_eq!(
            format!("{:#?}", Some(vec![SessionToken::new(SENTINEL)])),
            "Some(\n    [\n        SessionToken(<redacted>),\n    ],\n)"
        );
        assert_eq!(
            format!("{:?}", SigningKey([0x5a; 32])),
            "SigningKey(<redacted>)"
        );
    }

    #[test]
    fn a_new_token_is_64_hex_digits() {
        let token = SessionToken::generate(&CountingEntropy::default()).unwrap();
        assert_eq!(
            token.expose(),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
    }

    #[test]
    fn the_hash_is_the_sha_256_of_the_text() {
        // The digest of "abc" from FIPS 180-4.
        assert_eq!(
            SessionToken::new("abc").hash(),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad
            ]
        );
    }

    #[test]
    fn the_public_key_is_the_one_that_verifies_what_the_key_signs() {
        use ed25519_dalek::{Signer, Verifier, VerifyingKey};

        let key = SigningKey::generate(&CountingEntropy::default()).unwrap();
        let signature = ed25519_dalek::SigningKey::from_bytes(key.expose()).sign(b"nonce");
        let public = VerifyingKey::from_bytes(&key.public_key()).unwrap();
        public.verify(b"nonce", &signature).unwrap();
    }
}
