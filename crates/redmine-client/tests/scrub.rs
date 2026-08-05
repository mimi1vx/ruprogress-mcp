//! Asserts every fixture is free of secrets, real emails, and IP addresses.
//! See `tests/fixtures/README.md` for the policy this enforces.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

fn contains_hostile_pattern(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("api_key") || lower.contains("api-key") || lower.contains("apikey") {
        return Some("api key marker");
    }
    if lower.contains("bearer ") {
        return Some("bearer token marker");
    }
    for token in text
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_')
    {
        if is_email_shaped(token) {
            return Some("email address");
        }
        if is_ipv4_shaped(token) {
            return Some("IPv4 address");
        }
    }
    None
}

fn is_email_shaped(token: &str) -> bool {
    let Some((local, domain)) = token.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    let Some((_, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    domain.contains('.') && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_ipv4_shaped(token: &str) -> bool {
    let parts: Vec<&str> = token.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.parse::<u8>().is_ok())
}

#[test]
fn fixtures_are_scrubbed() {
    let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut checked = 0;
    for entry in std::fs::read_dir(&fixtures_dir).expect("fixtures dir should exist") {
        let entry = entry.expect("dir entry should be readable");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("fixture should be readable");
        if let Some(why) = contains_hostile_pattern(&content) {
            panic!("fixture {path:?} contains a {why}; scrub it before committing");
        }
        checked += 1;
    }
    assert!(checked > 0, "expected at least one fixture to check");
}

#[test]
fn scrub_detector_flags_known_hostile_patterns() {
    assert_eq!(
        contains_hostile_pattern("\"api_key\": \"abc\""),
        Some("api key marker")
    );
    assert_eq!(
        contains_hostile_pattern("Authorization: Bearer abc123"),
        Some("bearer token marker")
    );
    assert_eq!(
        contains_hostile_pattern("contact alice@example.com"),
        Some("email address")
    );
    assert_eq!(
        contains_hostile_pattern("host 10.0.0.5 is up"),
        Some("IPv4 address")
    );
    assert_eq!(
        contains_hostile_pattern("\"name\": \"Example Project\""),
        None
    );
}
