//! PKCE (RFC 7636): the S256 challenge against the RFC's own test vector, verifier validation, generation, and the
//! OAuth `state`.

use teams_auth::pkce::{Pkce, PkceError, random_state, s256_challenge};

/// RFC 7636 Appendix B: the 32 octets, the verifier they encode to, and its S256 challenge.
const RFC_OCTETS: [u8; 32] = [
    116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37, 77, 105, 214, 191, 240,
    91, 88, 5, 88, 83, 132, 141, 121,
];
const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn is_unreserved(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

#[test]
fn rfc_7636_octets_give_the_rfc_verifier_and_challenge() {
    let pkce = Pkce::from_bytes(&RFC_OCTETS);
    assert_eq!(pkce.verifier().expose(), RFC_VERIFIER);
    assert_eq!(pkce.challenge(), RFC_CHALLENGE);
    assert_eq!(Pkce::METHOD, "S256");
}

#[test]
fn rfc_7636_verifier_gives_the_rfc_challenge() {
    assert_eq!(s256_challenge(RFC_VERIFIER), RFC_CHALLENGE);
    let pkce = Pkce::from_verifier(RFC_VERIFIER).expect("the RFC verifier is valid");
    assert_eq!(pkce.challenge(), RFC_CHALLENGE);
}

#[test]
fn verifier_length_bounds_are_43_to_128() {
    for length in [43, 64, 128] {
        assert!(
            Pkce::from_verifier("a".repeat(length)).is_ok(),
            "length {length} is allowed"
        );
    }
    for length in [0, 42, 129] {
        assert!(
            matches!(Pkce::from_verifier("a".repeat(length)), Err(PkceError::InvalidVerifier)),
            "length {length} is refused"
        );
    }
}

#[test]
fn verifier_takes_only_unreserved_characters() {
    let base = "a".repeat(42);
    for good in ['-', '.', '_', '~', 'Z', '9'] {
        assert!(
            Pkce::from_verifier(format!("{base}{good}")).is_ok(),
            "{good:?} is unreserved"
        );
    }
    for bad in ['+', '/', '=', ' ', '%', '<', 'é'] {
        assert!(
            Pkce::from_verifier(format!("{base}{bad}")).is_err(),
            "{bad:?} is refused"
        );
    }
}

#[test]
fn a_refused_verifier_is_not_echoed() {
    let planted = format!("{}+planted", "x".repeat(40));
    let error = Pkce::from_verifier(planted.clone()).expect_err("'+' is refused");
    assert!(!error.to_string().contains(&planted));
    assert!(!format!("{error:?}").contains(&planted));
}

#[test]
fn generated_verifiers_are_43_unreserved_characters_and_differ() {
    let first = Pkce::generate().expect("random source");
    let second = Pkce::generate().expect("random source");
    for pkce in [&first, &second] {
        assert_eq!(pkce.verifier().expose().len(), 43);
        assert!(is_unreserved(pkce.verifier().expose()));
        assert_eq!(pkce.challenge(), s256_challenge(pkce.verifier().expose()));
    }
    assert_ne!(first.verifier().expose(), second.verifier().expose());
}

#[test]
fn debug_hides_the_verifier() {
    let pkce = Pkce::from_bytes(&RFC_OCTETS);
    let debug = format!("{pkce:?}");
    assert!(!debug.contains(RFC_VERIFIER), "{debug}");
    assert!(debug.contains(RFC_CHALLENGE), "the challenge is public: {debug}");
}

#[test]
fn state_is_url_safe_and_fresh() {
    let first = random_state().expect("random source");
    let second = random_state().expect("random source");
    assert!(first.len() >= 22, "{first}");
    assert!(is_unreserved(&first));
    assert_ne!(first, second);
}
