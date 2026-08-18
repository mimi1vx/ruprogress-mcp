//! `oauth-proxy` redirect-URI allowlisting (P7): component matching, never
//! raw-string globbing.
//!
//! A pattern is parsed once at boot into [`RedirectPattern`]'s components
//! (scheme, host, port, path prefix); matching a candidate redirect URI is
//! then a comparison over those components, never a glob over the raw
//! string. `RedirectPolicy::permits` additionally runs the unconditional
//! floor checks from [`validate_floor`] before consulting any pattern —
//! those checks apply even under [`RedirectPolicy::Any`].

use std::net::IpAddr;

use url::Url;

/// A single label's host match: `Exact` for a literal hostname, `Suffix`
/// for a pattern's single leading `*.` label. `Suffix` matches any number of
/// labels above the suffix (`sub.sub.app.example.com` matches `*.example.com`)
/// — the same unbounded-depth semantics as a browser's CORS `Access-Control-
/// Allow-Origin` wildcard, and a deliberate, documented choice rather than an
/// oversight.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    Exact(String),
    Suffix(String),
}

/// A pattern's port match: a literal `*` in the pattern, or a specific port
/// — either given explicitly or defaulted from the pattern's scheme
/// (`http` → 80, `https` → 443) when absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Port {
    Any,
    Exact(u16),
}

/// A pattern's path match: a trailing `*` in the pattern means "this prefix
/// or anything under it"; otherwise the candidate's path must equal the
/// pattern's exactly. A pattern with no path at all (e.g. `http://localhost:*`)
/// is `Prefix(String::new())`, matching every path — a redirect allowlist
/// that named a scheme/host/port but no path is asking to allow that origin
/// outright, not to lock every client onto `/`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathMatch {
    Exact(String),
    Prefix(String),
}

/// One parsed entry of `REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` (C4).
/// `pub`, not `pub(crate)`: it appears in a field of the `pub`
/// `OAuthProxyConfig` (`oauth`'s own module is private, so this is not
/// actually nameable from outside the crate either way — `pub` here just
/// keeps rustc's public-interface lint satisfied).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectPattern {
    scheme: &'static str,
    host: Host,
    port: Port,
    path: PathMatch,
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "https" { 443 } else { 80 }
}

impl RedirectPattern {
    /// Parses one pattern entry. Rejects anything that cannot be
    /// represented as scheme + host (optionally one leading `*.` label) +
    /// port (a literal `*` or a number) + path (optionally trailing `*`):
    /// a wildcard scheme, a bare `*` host, a `*` embedded mid-label,
    /// userinfo, a query string, or a fragment.
    ///
    /// # Errors
    ///
    /// Returns a message naming what about `raw` could not be parsed.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let Some((scheme, rest)) = raw.split_once("://") else {
            return Err(format!("{raw:?} has no \"scheme://\" prefix"));
        };
        let scheme: &'static str = match scheme {
            "http" => "http",
            "https" => "https",
            other => return Err(format!("scheme {other:?} must be \"http\" or \"https\"")),
        };
        if rest.contains('@') {
            return Err(format!(
                "{raw:?} contains userinfo, which a pattern may not"
            ));
        }
        if rest.contains('?') {
            return Err(format!(
                "{raw:?} contains a query string, which a pattern may not"
            ));
        }
        if rest.contains('#') {
            return Err(format!(
                "{raw:?} contains a fragment, which a pattern may not"
            ));
        }

        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, String::new()),
        };
        if authority.is_empty() {
            return Err(format!("{raw:?} has no host"));
        }
        let (host_part, port_part) = match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };

        let host = if let Some(label) = host_part.strip_prefix("*.") {
            if label.is_empty() || label.contains('*') {
                return Err(format!(
                    "{raw:?} has a wildcard host that is not a single leading \"*.\" label"
                ));
            }
            Host::Suffix(label.to_ascii_lowercase())
        } else if host_part.contains('*') {
            return Err(format!(
                "{raw:?} has a wildcard host that is not a single leading \"*.\" label"
            ));
        } else {
            Host::Exact(host_part.to_ascii_lowercase())
        };

        let port = match port_part {
            Some("*") => Port::Any,
            Some(digits) => Port::Exact(
                digits
                    .parse()
                    .map_err(|_| format!("{raw:?} has an invalid port {digits:?}"))?,
            ),
            None => Port::Exact(default_port(scheme)),
        };

        let path = path.strip_suffix('*').map_or_else(
            || {
                if path.is_empty() {
                    PathMatch::Prefix(String::new())
                } else {
                    PathMatch::Exact(path.clone())
                }
            },
            |prefix| PathMatch::Prefix(prefix.to_string()),
        );

        Ok(Self {
            scheme,
            host,
            port,
            path,
        })
    }

    fn matches(&self, url: &Url) -> bool {
        if url.scheme() != self.scheme {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host.to_ascii_lowercase();
        let host_matches = match &self.host {
            Host::Exact(exact) => host == *exact,
            // `strip_suffix` + a leftover ending in `.` both confirms the
            // suffix matched at a label boundary and rejects the bare
            // apex (an empty leftover does not end with `.`), with no
            // indexing or length arithmetic.
            Host::Suffix(suffix) => host
                .strip_suffix(suffix.as_str())
                .is_some_and(|prefix| prefix.ends_with('.')),
        };
        if !host_matches {
            return false;
        }
        let port_matches = match self.port {
            Port::Any => true,
            Port::Exact(port) => url.port_or_known_default() == Some(port),
        };
        if !port_matches {
            return false;
        }
        match &self.path {
            PathMatch::Exact(exact) => url.path() == exact,
            PathMatch::Prefix(prefix) => url.path().starts_with(prefix.as_str()),
        }
    }
}

/// `REDMINE_MCP_ALLOWED_CLIENT_REDIRECT_URIS` (C3): parsed once at boot.
#[derive(Debug, Clone)]
pub enum RedirectPolicy {
    /// Default: `http://localhost:*` and `http://127.0.0.1:*`, expressed as
    /// [`RedirectPattern`]s rather than a second, hand-rolled check — one
    /// matcher, not two.
    Loopback,
    /// The literal `*`: no pattern restriction beyond the unconditional
    /// floor in [`validate_floor`].
    Any,
    Patterns(Vec<RedirectPattern>),
}

fn loopback_patterns() -> [RedirectPattern; 2] {
    [
        RedirectPattern {
            scheme: "http",
            host: Host::Exact("localhost".to_string()),
            port: Port::Any,
            path: PathMatch::Prefix(String::new()),
        },
        RedirectPattern {
            scheme: "http",
            host: Host::Exact("127.0.0.1".to_string()),
            port: Port::Any,
            path: PathMatch::Prefix(String::new()),
        },
    ]
}

/// Whether `host` is a loopback host for the purposes of [`validate_floor`]'s
/// `http`-only-on-loopback rule: `localhost` by name, or a literal IP that
/// parses as loopback (covers `127.0.0.1` and `::1`). `Url::host_str` wraps
/// an IPv6 literal in brackets (`"[::1]"`), which `IpAddr::parse` rejects, so
/// they are stripped first — same normalization as
/// `transport::http::normalize_host`.
fn is_loopback_host(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost")
        || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// The unconditional floor (C5), checked before any pattern and regardless
/// of [`RedirectPolicy`]: absolute, `http`/`https` only, no userinfo, no
/// fragment, and `http` only when the host is loopback. Returns the parsed
/// URL on success so callers never re-parse.
fn validate_floor(candidate: &str) -> Option<Url> {
    let url = Url::parse(candidate).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if url.fragment().is_some() {
        return None;
    }
    let host = url.host_str()?;
    if url.scheme() == "http" && !is_loopback_host(host) {
        return None;
    }
    Some(url)
}

impl RedirectPolicy {
    /// A secret-free summary for `Config::redacted_summary`/`--print-config`:
    /// enough to see which policy is active without dumping every pattern.
    pub(crate) fn summary_label(&self) -> String {
        match self {
            Self::Loopback => "loopback".to_string(),
            Self::Any => "any".to_string(),
            Self::Patterns(patterns) => format!("{} pattern(s)", patterns.len()),
        }
    }

    /// Whether `candidate` (a client-supplied redirect URI, at registration
    /// or at `/authorize`) is allowed. Never panics on malformed input — an
    /// unparseable or floor-violating candidate is simply not permitted.
    pub(crate) fn permits(&self, candidate: &str) -> bool {
        let Some(url) = validate_floor(candidate) else {
            return false;
        };
        match self {
            Self::Loopback => loopback_patterns().iter().any(|p| p.matches(&url)),
            Self::Any => true,
            Self::Patterns(patterns) => patterns.iter().any(|p| p.matches(&url)),
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod tests {
    use super::*;

    fn patterns(raw: &[&str]) -> RedirectPolicy {
        RedirectPolicy::Patterns(
            raw.iter()
                .map(|p| RedirectPattern::parse(p).expect("valid pattern"))
                .collect(),
        )
    }

    // --- exact-host patterns ---------------------------------------------

    #[test]
    fn exact_host_pattern_accepts_the_named_path() {
        let policy = patterns(&["https://app.example.com/*"]);
        assert!(policy.permits("https://app.example.com/cb"));
    }

    #[test]
    fn exact_host_pattern_rejects_a_different_host() {
        let policy = patterns(&["https://app.example.com/*"]);
        assert!(!policy.permits("https://other.example.com/cb"));
    }

    #[test]
    fn exact_path_pattern_rejects_a_different_path() {
        let policy = patterns(&["https://app.example.com/cb"]);
        assert!(policy.permits("https://app.example.com/cb"));
        assert!(!policy.permits("https://app.example.com/cb/extra"));
        assert!(!policy.permits("https://app.example.com/other"));
    }

    // --- wildcard-host patterns -------------------------------------------

    #[test]
    fn wildcard_host_pattern_accepts_a_direct_subdomain() {
        let policy = patterns(&["https://*.example.com/*"]);
        assert!(policy.permits("https://app.example.com/cb"));
    }

    #[test]
    fn wildcard_host_pattern_rejects_the_bare_apex() {
        let policy = patterns(&["https://*.example.com/*"]);
        assert!(!policy.permits("https://example.com/cb"));
    }

    #[test]
    fn wildcard_host_pattern_rejects_a_query_smuggled_host() {
        let policy = patterns(&["https://*.example.com/*"]);
        assert!(!policy.permits("https://evil.com/?x=.example.com/"));
    }

    #[test]
    fn wildcard_host_pattern_rejects_a_suffix_confusable_host() {
        let policy = patterns(&["https://*.example.com/*"]);
        assert!(!policy.permits("https://app.example.com.evil.com/cb"));
    }

    #[test]
    fn wildcard_host_pattern_accepts_multi_level_subdomains() {
        // Intended answer: a single leading "*." label matches any number of
        // labels above the suffix, same as CORS wildcard semantics.
        let policy = patterns(&["https://*.example.com/*"]);
        assert!(policy.permits("https://sub.sub.app.example.com/cb"));
    }

    // --- loopback policy ----------------------------------------------------

    #[test]
    fn loopback_rejects_a_non_loopback_ip() {
        assert!(!RedirectPolicy::Loopback.permits("http://10.0.0.5/cb"));
    }

    #[test]
    fn loopback_accepts_127_0_0_1_with_any_port() {
        assert!(RedirectPolicy::Loopback.permits("http://127.0.0.1:51234/cb"));
    }

    #[test]
    fn loopback_accepts_localhost_with_no_port() {
        assert!(RedirectPolicy::Loopback.permits("http://localhost/cb"));
    }

    #[test]
    fn loopback_accepts_localhost_with_any_path() {
        assert!(RedirectPolicy::Loopback.permits("http://localhost:9000/"));
        assert!(RedirectPolicy::Loopback.permits("http://localhost:9000/callback/deep"));
    }

    #[test]
    fn loopback_rejects_https_localhost_by_pattern_mismatch() {
        // Not a floor failure (https is always allowed) — the default
        // loopback patterns are http-only, matching the reference's literal
        // defaults.
        assert!(!RedirectPolicy::Loopback.permits("https://localhost/cb"));
    }

    #[test]
    fn loopback_rejects_ipv6_loopback_by_pattern_mismatch() {
        // ::1 passes the floor (it is loopback, so http is allowed) but is
        // not one of the two literal default patterns.
        assert!(!RedirectPolicy::Loopback.permits("http://[::1]/cb"));
    }

    // --- Any policy still enforces the floor -------------------------------

    #[test]
    fn any_rejects_userinfo() {
        assert!(!RedirectPolicy::Any.permits("https://user:pw@app.example.com/cb"));
    }

    #[test]
    fn any_rejects_a_fragment() {
        assert!(!RedirectPolicy::Any.permits("https://app.example.com/cb#frag"));
    }

    #[test]
    fn any_accepts_a_query_string() {
        assert!(RedirectPolicy::Any.permits("https://app.example.com/cb?x=1"));
    }

    #[test]
    fn any_rejects_non_http_schemes() {
        assert!(!RedirectPolicy::Any.permits("javascript:alert(1)"));
        assert!(!RedirectPolicy::Any.permits("data:text/html,hi"));
    }

    #[test]
    fn any_rejects_http_on_a_non_loopback_host() {
        assert!(!RedirectPolicy::Any.permits("http://app.example.com/cb"));
    }

    #[test]
    fn any_accepts_https_on_any_host() {
        assert!(RedirectPolicy::Any.permits("https://app.example.com/cb"));
        assert!(RedirectPolicy::Any.permits("https://anything.example.org/x/y"));
    }

    #[test]
    fn any_accepts_http_on_loopback() {
        assert!(RedirectPolicy::Any.permits("http://127.0.0.1:4000/cb"));
    }

    #[test]
    fn any_rejects_a_malformed_uri() {
        assert!(!RedirectPolicy::Any.permits("not a url"));
        assert!(!RedirectPolicy::Any.permits("//no-scheme.example.com/cb"));
    }

    // --- port matching ------------------------------------------------------

    #[test]
    fn explicit_port_pattern_requires_an_exact_match() {
        let policy = patterns(&["https://app.example.com:8443/*"]);
        assert!(policy.permits("https://app.example.com:8443/cb"));
        assert!(!policy.permits("https://app.example.com:9443/cb"));
    }

    #[test]
    fn pattern_with_no_port_defaults_to_the_scheme_port() {
        let secure_policy = patterns(&["https://app.example.com/*"]);
        assert!(secure_policy.permits("https://app.example.com:443/cb"));
        assert!(!secure_policy.permits("https://app.example.com:8443/cb"));

        let plain_policy = patterns(&["http://localhost/*"]);
        assert!(plain_policy.permits("http://localhost:80/cb"));
    }

    #[test]
    fn wildcard_port_pattern_accepts_any_port() {
        let policy = patterns(&["http://localhost:*/*"]);
        assert!(policy.permits("http://localhost:1/cb"));
        assert!(policy.permits("http://localhost:65535/cb"));
    }

    // --- pattern parse rejections (boot-time) --------------------------------

    #[test]
    fn wildcard_scheme_is_rejected() {
        assert!(RedirectPattern::parse("*://x").is_err());
    }

    #[test]
    fn bare_wildcard_host_is_rejected() {
        assert!(RedirectPattern::parse("https://*/*").is_err());
    }

    #[test]
    fn mid_label_wildcard_host_is_rejected() {
        assert!(RedirectPattern::parse("https://ap*.example.com/*").is_err());
    }

    #[test]
    fn userinfo_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("https://user:pw@example.com/*").is_err());
    }

    #[test]
    fn query_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("https://example.com/*?x=1").is_err());
    }

    #[test]
    fn fragment_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("https://example.com/*#frag").is_err());
    }

    #[test]
    fn non_http_scheme_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("ftp://example.com/*").is_err());
    }

    #[test]
    fn non_numeric_port_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("https://example.com:abc/*").is_err());
    }

    #[test]
    fn empty_host_in_a_pattern_is_rejected() {
        assert!(RedirectPattern::parse("https:///*").is_err());
    }

    #[test]
    fn a_pattern_with_no_scheme_separator_is_rejected() {
        assert!(RedirectPattern::parse("example.com/*").is_err());
    }
}
