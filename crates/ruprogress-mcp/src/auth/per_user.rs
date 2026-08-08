//! `AuthMode::LegacyPerUser`: each HTTP request carries its own credential in
//! the `X-Redmine-API-Key` header. See `docs/legacy-per-user-auth.md` for the
//! threat model.
//!
//! No ambient fallback and no cross-request reuse: the credential this module
//! produces lives only as long as the request that carried it.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};
use std::sync::OnceLock;

use http::request::Parts;
use redmine_client::{Credential, RedmineClient, Scoped};
use rmcp::ErrorData as McpError;
use rmcp::RoleServer;
use rmcp::model::Extensions;
use rmcp::service::RequestContext;
use secrecy::{ExposeSecret as _, SecretString};

/// Only this header is accepted: `Authorization: Bearer`/`Basic` are ignored,
/// not forwarded — validating a bearer token belongs to a future OAuth mode,
/// and forwarding one unvalidated here would be a strictly worse version of
/// this mode.
const HEADER_NAME: &str = "x-redmine-api-key";

/// Redmine's own keys are 40 hex characters; 512 is far above that and far
/// below anything worth forwarding.
const MAX_HEADER_LEN: usize = 512;

/// Extract the per-request Redmine credential from `ctx`'s inbound HTTP
/// headers. A missing/empty/malformed/duplicated header is an [`McpError`]
/// protocol error: no Redmine request is attempted, so the in-band
/// `{error, code, retryable, hint}` tool-error envelope does not apply.
///
/// # Errors
///
/// Returns an [`McpError`] naming the problem (never the header's value) if
/// the request did not arrive over HTTP, or the header is absent, empty,
/// malformed, too long, or sent more than once.
pub(crate) fn credential(ctx: &RequestContext<RoleServer>) -> Result<Credential, McpError> {
    credential_from_extensions(&ctx.extensions)
}

fn credential_from_extensions(extensions: &Extensions) -> Result<Credential, McpError> {
    let parts = extensions.get::<Parts>().ok_or_else(|| {
        McpError::invalid_request(
            "this auth mode requires the HTTP transport (no request headers available)",
            None,
        )
    })?;
    credential_from_parts(parts)
}

fn credential_from_parts(parts: &Parts) -> Result<Credential, McpError> {
    let mut values = parts.headers.get_all(HEADER_NAME).iter();

    let Some(value) = values.next() else {
        return Err(McpError::invalid_request(
            "missing required X-Redmine-API-Key header",
            None,
        ));
    };
    if values.next().is_some() {
        return Err(McpError::invalid_request(
            "X-Redmine-API-Key header must be sent exactly once",
            None,
        ));
    }
    if value.len() > MAX_HEADER_LEN {
        return Err(McpError::invalid_request(
            "X-Redmine-API-Key header exceeds the maximum accepted length",
            None,
        ));
    }
    let text = value.to_str().map_err(|_| {
        McpError::invalid_request(
            "X-Redmine-API-Key header contains invalid (non-visible-ASCII) characters",
            None,
        )
    })?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(McpError::invalid_request(
            "X-Redmine-API-Key header must not be empty",
            None,
        ));
    }

    Ok(Credential::ApiKey(SecretString::from(trimmed.to_string())))
}

/// The `AuthMode::LegacyPerUser` arm of `RedmineMcp::scoped`: extracts the
/// request's credential and hands back a `Scoped` owning it — nothing else
/// touches `client`'s pool with this identity, and nothing here retains the
/// credential beyond this call.
///
/// # Errors
///
/// See [`credential`].
pub(crate) fn scoped<'c>(
    client: &'c RedmineClient,
    ctx: &RequestContext<RoleServer>,
    audit_identity: bool,
) -> Result<Scoped<'c>, McpError> {
    let cred = credential(ctx)?;
    if audit_identity {
        log_caller(&cred, ctx);
    }
    Ok(client.as_user_owned(cred))
}

/// `REDMINE_PER_USER_AUDIT_IDENTITY=true`: logs a fingerprint of the
/// inbound key, never the key or a resolved Redmine identity. Silently a
/// no-op for the non-`ApiKey` variants `credential` never actually returns —
/// defensive only, since [`Credential`] is a shared type with other auth
/// modes.
fn log_caller(cred: &Credential, ctx: &RequestContext<RoleServer>) {
    let Credential::ApiKey(key) = cred else {
        return;
    };
    let fingerprint = KeyFingerprint::of(key.expose_secret());
    tracing::info!(caller = %fingerprint, request_id = ?ctx.id, "per-user request");
}

/// A per-process, non-reversible correlation id for an inbound API key:
/// `SipHash` (via `RandomState`, keyed once per process at first use) over
/// the key bytes, rendered as 16 hex chars. Deliberately does not survive a
/// restart and cannot be rainbow-tabled back to the key — the
/// privacy-correct shape for an audit breadcrumb. Holds only the computed
/// hash, never the key, so deriving `Debug` cannot leak it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyFingerprint(u64);

impl KeyFingerprint {
    fn of(key: &str) -> Self {
        static HASHER_KEY: OnceLock<RandomState> = OnceLock::new();
        let state = HASHER_KEY.get_or_init(RandomState::new);
        let mut hasher = state.build_hasher();
        hasher.write(key.as_bytes());
        Self(hasher.finish())
    }
}

impl std::fmt::Display for KeyFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn parts_with_headers(headers: &[(&str, &str)]) -> Parts {
        let mut builder = http::Request::builder();
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let (parts, ()) = builder.body(()).expect("request should build").into_parts();
        parts
    }

    fn extensions_with(parts: Parts) -> Extensions {
        let mut extensions = Extensions::new();
        extensions.insert(parts);
        extensions
    }

    fn key_of(credential: &Credential) -> String {
        match credential {
            Credential::ApiKey(key) => {
                use secrecy::ExposeSecret as _;
                key.expose_secret().to_string()
            }
            other => panic!("expected Credential::ApiKey, got {other:?}"),
        }
    }

    #[test]
    fn no_http_parts_is_rejected() {
        let extensions = Extensions::new();
        let error = credential_from_extensions(&extensions).unwrap_err();
        assert!(format!("{error}").contains("HTTP transport"));
    }

    #[test]
    fn absent_header_is_rejected() {
        let extensions = extensions_with(parts_with_headers(&[]));
        let error = credential_from_extensions(&extensions).unwrap_err();
        assert!(format!("{error}").contains("missing required"));
    }

    #[test]
    fn empty_header_is_rejected() {
        let parts = parts_with_headers(&[("x-redmine-api-key", "")]);
        let error = credential_from_parts(&parts).unwrap_err();
        assert!(format!("{error}").contains("must not be empty"));
    }

    #[test]
    fn whitespace_only_header_is_rejected() {
        let parts = parts_with_headers(&[("x-redmine-api-key", "   ")]);
        let error = credential_from_parts(&parts).unwrap_err();
        assert!(format!("{error}").contains("must not be empty"));
    }

    #[test]
    fn duplicated_header_is_rejected() {
        let parts = parts_with_headers(&[
            ("x-redmine-api-key", "key-one"),
            ("x-redmine-api-key", "key-two"),
        ]);
        let error = credential_from_parts(&parts).unwrap_err();
        assert!(format!("{error}").contains("exactly once"));
    }

    #[test]
    fn oversized_header_is_rejected() {
        let value = "a".repeat(MAX_HEADER_LEN + 1);
        let parts = parts_with_headers(&[("x-redmine-api-key", &value)]);
        let error = credential_from_parts(&parts).unwrap_err();
        assert!(format!("{error}").contains("maximum accepted length"));
    }

    #[test]
    fn non_ascii_header_is_rejected() {
        // A byte >= 0x80 is a legal (opaque) `HeaderValue` but fails
        // `to_str()`'s visible-ASCII check — the realistic "non-ASCII
        // header" shape, since raw control bytes like CR/LF are rejected by
        // `HeaderValue` construction itself.
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "x-redmine-api-key",
            http::HeaderValue::from_bytes(b"abc\xC3\xA9def")
                .expect("opaque bytes should construct"),
        );
        let (mut parts, ()) = http::Request::builder()
            .body(())
            .expect("request should build")
            .into_parts();
        parts.headers = headers;
        let error = credential_from_parts(&parts).unwrap_err();
        assert!(format!("{error}").contains("invalid"));
    }

    #[test]
    fn happy_path_trims_and_returns_the_key() {
        let parts = parts_with_headers(&[("x-redmine-api-key", "  abc123  ")]);
        let credential = credential_from_parts(&parts).expect("should accept a valid key");
        assert_eq!(key_of(&credential), "abc123");
    }

    #[test]
    fn fingerprint_is_stable_for_the_same_key_within_a_process() {
        let a = KeyFingerprint::of("same-key-value");
        let b = KeyFingerprint::of("same-key-value");
        assert_eq!(a, b);
    }

    #[test]
    fn fingerprint_differs_between_keys() {
        let a = KeyFingerprint::of("key-one-value");
        let b = KeyFingerprint::of("key-two-value");
        assert_ne!(a, b);
    }

    #[test]
    fn fingerprint_shares_no_8_char_substring_with_the_key() {
        let key = "0123456789abcdef0123456789abcdef";
        let rendered = KeyFingerprint::of(key).to_string();
        for window in key.as_bytes().windows(8) {
            let substring = std::str::from_utf8(window).expect("ASCII input");
            assert!(
                !rendered.contains(substring),
                "fingerprint {rendered} leaked substring {substring} of the key"
            );
        }
    }
}
