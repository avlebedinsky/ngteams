//! PKCE (RFC 7636) with the S256 method, and the OAuth `state` value.
//!
//! The verifier is a [`Secret`]: it never shows in `Debug`, and it leaves the process only in the code exchange's
//! form body. The challenge goes into the authorize URL and is public.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::secrets::Secret;

/// Random bytes in a generated verifier: 32 bytes give the RFC's recommended 43-character verifier.
const VERIFIER_BYTES: usize = 32;
/// Random bytes in a generated `state`: 128 bits, 22 characters.
const STATE_BYTES: usize = 16;
const VERIFIER_LENGTH: std::ops::RangeInclusive<usize> = 43..=128;

/// Why a PKCE value couldn't be made. Never carries the verifier.
#[derive(Debug, thiserror::Error)]
pub enum PkceError {
    #[error("a PKCE verifier is 43 to 128 characters from A-Z a-z 0-9 - . _ ~")]
    InvalidVerifier,
    #[error("the system random source failed: {0}")]
    Random(getrandom::Error),
}

/// A PKCE verifier and its S256 challenge. `Debug` shows the challenge only.
#[derive(Clone, Debug)]
pub struct Pkce {
    verifier: Secret,
    challenge: String,
}

impl Pkce {
    /// The only method we use: the challenge is the unpadded base64url SHA-256 of the verifier.
    pub const METHOD: &'static str = "S256";

    /// A fresh verifier from the system random source.
    ///
    /// # Errors
    /// When the random source fails.
    pub fn generate() -> Result<Self, PkceError> {
        let mut bytes = Zeroizing::new([0_u8; VERIFIER_BYTES]);
        getrandom::fill(bytes.as_mut_slice()).map_err(PkceError::Random)?;
        Ok(Self::from_bytes(&bytes))
    }

    /// The verifier that encodes these 32 bytes (unpadded base64url), as in RFC 7636 Appendix B.
    #[must_use]
    pub fn from_bytes(bytes: &[u8; VERIFIER_BYTES]) -> Self {
        Self::from_valid(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// A given verifier, checked against RFC 7636 section 4.1.
    ///
    /// # Errors
    /// When the verifier is too short or too long, or has a character outside the unreserved set.
    pub fn from_verifier(verifier: impl Into<String>) -> Result<Self, PkceError> {
        // Zeroizing until it moves into the Secret, so a refused verifier is wiped too.
        let mut verifier = Zeroizing::new(verifier.into());
        let unreserved = verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'));
        if !unreserved || !VERIFIER_LENGTH.contains(&verifier.len()) {
            return Err(PkceError::InvalidVerifier);
        }
        Ok(Self::from_valid(std::mem::take(&mut *verifier)))
    }

    fn from_valid(verifier: String) -> Self {
        let challenge = s256_challenge(&verifier);
        Self {
            verifier: Secret::from(verifier),
            challenge,
        }
    }

    /// The verifier, for the code exchange only.
    #[must_use]
    pub fn verifier(&self) -> &Secret {
        &self.verifier
    }

    /// The S256 challenge, for the authorize URL.
    #[must_use]
    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

/// The S256 challenge of a verifier: `BASE64URL(SHA256(ASCII(verifier)))`, unpadded.
#[must_use]
pub fn s256_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A fresh OAuth `state`: 128 random bits, unpadded base64url.
///
/// # Errors
/// When the random source fails.
pub fn random_state() -> Result<String, PkceError> {
    let mut bytes = [0_u8; STATE_BYTES];
    getrandom::fill(&mut bytes).map_err(PkceError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
