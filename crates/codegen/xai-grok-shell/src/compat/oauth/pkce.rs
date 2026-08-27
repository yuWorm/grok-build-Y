//! PKCE S256 (RFC 7636), same construction Pi uses.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};

pub(super) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub(super) fn generate_pkce() -> Pkce {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(hash);
    Pkce {
        verifier,
        challenge,
    }
}

#[cfg(test)]
mod tests {
    use super::generate_pkce;

    #[test]
    fn verifier_and_challenge_are_base64url() {
        let pkce = generate_pkce();
        assert!(pkce.verifier.len() >= 32);
        assert!(pkce.challenge.len() >= 32);
        assert!(!pkce.verifier.contains('+'));
        assert!(!pkce.verifier.contains('/'));
        assert_ne!(pkce.verifier, pkce.challenge);
    }
}
