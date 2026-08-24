//! RFC 7636 PKCE primitives (P5, F3): S256 only, used for both the
//! downstream (client-facing) and upstream (Redmine-facing) legs of
//! `oauth-proxy`'s authorization-code flow — always as two independent
//! verifier/challenge pairs, never the same one reused across legs.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRng as _;
use rand::rngs::SysRng;
use sha2::{Digest as _, Sha256};

/// Generates a fresh `code_verifier`: 32 bytes of `SysRng`, base64url-encoded
/// (no padding) to the 43-character form RFC 7636 §4.1 recommends. `None`
/// only if the OS RNG itself is unavailable (C8's same failure mode as
/// `store::ClientRegistry::mint_client_id`) — never a caller-input failure.
pub(crate) fn generate_verifier() -> Option<String> {
    let mut bytes = [0u8; 32];
    if let Err(error) = SysRng.try_fill_bytes(&mut bytes) {
        tracing::error!(%error, "OS RNG unavailable; cannot generate a PKCE verifier");
        return None;
    }
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

/// Derives the S256 `code_challenge` for `verifier` (RFC 7636 §4.2):
/// `BASE64URL-ENCODE(SHA256(ASCII(verifier)))`.
pub(crate) fn challenge_for(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

/// Constant-time byte comparison: no early return on the first mismatching
/// byte, so a `code_verifier`-guessing attacker cannot use response timing
/// to recover the challenge one byte at a time. A length mismatch is not
/// itself timed against `a`'s length — comparing against a zero result is
/// enough here since both sides are short, fixed-shape base64url strings,
/// not secrets whose *length* must also be hidden.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Verifies `verifier` against a previously stored `code_challenge`
/// (S256 only, per P5).
pub(crate) fn verify(code_challenge: &str, verifier: &str) -> bool {
    constant_time_eq(
        challenge_for(verifier).as_bytes(),
        code_challenge.as_bytes(),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B's published test vector.
    #[test]
    fn matches_the_rfc_7636_test_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert_eq!(challenge_for(verifier), expected_challenge);
        assert!(verify(expected_challenge, verifier));
    }

    #[test]
    fn generate_verifier_produces_a_43_char_url_safe_string() {
        let verifier = generate_verifier().expect("OS RNG should be available");
        assert_eq!(verifier.len(), 43);
        assert!(
            verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }

    #[test]
    fn two_generated_verifiers_never_collide() {
        let a = generate_verifier().expect("should generate");
        let b = generate_verifier().expect("should generate");
        assert_ne!(a, b);
    }

    #[test]
    fn verify_rejects_a_wrong_verifier() {
        let challenge = challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert!(!verify(&challenge, "a-completely-different-verifier"));
    }

    #[test]
    fn verify_rejects_a_challenge_of_different_length() {
        assert!(!verify(
            "short",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        ));
    }
}
