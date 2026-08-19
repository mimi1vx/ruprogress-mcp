//! Builds the shipped `tracing_subscriber::EnvFilter`: whatever the operator
//! requested (`--log-level`/`RUST_LOG`) plus a payload-safety floor on a
//! fixed list of dependency targets that log wire-level bodies at
//! `DEBUG`/`TRACE` — tool arguments, HTTP headers, the whole JSON-RPC
//! envelope. Our own two crates (`ruprogress_mcp`, `redmine_client`) are
//! never floored: `trace` on them stays fully useful.
//!
//! The floor is a target ceiling, not output scrubbing: nothing here can
//! redact a third-party crate's `Debug` impl after the fact, so the only
//! real control is not turning that logging on in the first place. An
//! operator who names a floored target explicitly (`RUST_LOG=rmcp=trace`)
//! gets exactly that — [`env_filter`] reports the override so the caller can
//! warn about it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing_subscriber::EnvFilter;

/// `(target, level)`: crates whose default `DEBUG`/`TRACE` output can
/// include a request/response body, wire frame, or header rather than pure
/// control-flow metadata. `tower_http::trace` is capped at `debug`, not
/// `info`, because its `debug` output is request/response *metadata* only
/// (method, path, status, latency) — no body.
const FLOORS: &[(&str, &str)] = &[
    ("rmcp", "info"),
    ("hyper", "info"),
    ("hyper_util", "info"),
    ("h2", "info"),
    ("reqwest", "info"),
    ("tower_http::trace", "debug"),
    ("rustls", "info"),
    ("wiremock", "info"),
];

/// Builds the filter from `requested` (the raw `--log-level`/`RUST_LOG`
/// string) plus the floor. Returns the filter and the subset of the floored
/// targets that `requested` already names explicitly — those are left
/// alone rather than floored, and the caller should warn about them.
#[must_use]
pub fn env_filter(requested: &str) -> (EnvFilter, Vec<&'static str>) {
    let named: Vec<&str> = requested
        .split(',')
        .map(|directive| directive.split('=').next().unwrap_or_default().trim())
        .collect();

    let mut overridden = Vec::new();
    let mut spec = requested.to_string();
    for (target, level) in FLOORS {
        if named.contains(target) {
            overridden.push(*target);
        } else {
            spec.push(',');
            spec.push_str(target);
            spec.push('=');
            spec.push_str(level);
        }
    }

    (EnvFilter::new(spec), overridden)
}

/// Correlates the tracing lines of one `tools/call` (`server.rs`'s
/// `call_tool`, which opens the `tool_call` span this formats into):
/// an 8-hex-digit random prefix, stable for the process's life, plus a
/// 16-hex-digit call counter. **Not** a client-supplied or W3C trace id
/// (OB2) — nothing propagates it over the wire, and it exists only to tie
/// one call's lines together and to tell two processes' logs apart in an
/// aggregator.
#[derive(Debug, Clone, Copy)]
pub struct RequestId {
    counter: u64,
}

impl RequestId {
    /// Atomically advances `counter` (owned by the caller — `RedmineMcp`'s
    /// inner state, so every call through one server shares a sequence) and
    /// returns the id for the value it held before the increment.
    #[must_use]
    pub fn next(counter: &AtomicU64) -> Self {
        Self {
            counter: counter.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{:016x}", process_prefix(), self.counter)
    }
}

fn process_prefix() -> &'static str {
    static PREFIX: OnceLock<String> = OnceLock::new();
    PREFIX.get_or_init(|| format!("{:08x}", rand::random::<u32>()))
}

/// `REDMINE_MCP_LOG_FORMAT`: cosmetic only, does not change what is logged
/// (OB8). `Json` is one `tracing_subscriber` JSON object per line, for a log
/// aggregator; `Text` (default) is the existing human-readable format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl LogFormat {
    /// Parses `REDMINE_MCP_LOG_FORMAT`'s two accepted values. `None` for
    /// anything else, including an empty string — the caller decides what
    /// that means (an unset var vs. a rejected one).
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::{FLOORS, LogFormat, RequestId, env_filter};

    fn rendered(requested: &str) -> String {
        env_filter(requested).0.to_string()
    }

    #[test]
    fn two_ids_share_the_process_prefix_and_differ_in_the_counter() {
        let counter = std::sync::atomic::AtomicU64::new(0);
        let a = RequestId::next(&counter).to_string();
        let b = RequestId::next(&counter).to_string();
        let (prefix_a, suffix_a) = a.split_once('-').expect("id has a prefix-counter shape");
        let (prefix_b, suffix_b) = b.split_once('-').expect("id has a prefix-counter shape");
        assert_eq!(prefix_a, prefix_b, "same process must share a prefix");
        assert_ne!(suffix_a, suffix_b, "the counter must differ");
        assert_eq!(prefix_a.len(), 8);
        assert_eq!(suffix_a.len(), 16);
    }

    #[test]
    fn log_format_parses_its_two_values_and_rejects_anything_else() {
        assert_eq!(LogFormat::parse("text"), Some(LogFormat::Text));
        assert_eq!(LogFormat::parse("json"), Some(LogFormat::Json));
        assert_eq!(LogFormat::parse("JSON"), None);
        assert_eq!(LogFormat::parse(""), None);
        assert_eq!(LogFormat::default(), LogFormat::Text);
    }

    #[test]
    fn info_gets_every_floor() {
        let out = rendered("info");
        for (target, level) in FLOORS {
            assert!(
                out.contains(&format!("{target}={level}")),
                "missing floor {target}={level} in {out}"
            );
        }
    }

    #[test]
    fn trace_gets_every_floor_and_leaves_our_own_crates_at_trace() {
        let out = rendered("trace");
        for (target, level) in FLOORS {
            assert!(
                out.contains(&format!("{target}={level}")),
                "missing floor {target}={level} in {out}"
            );
        }
        // Neither of our own crates is named as an explicit floor, so the
        // bare "trace" directive still governs them.
        assert!(!out.contains("ruprogress_mcp="));
        assert!(!out.contains("redmine_client="));
    }

    #[test]
    fn an_explicit_floor_override_wins_and_is_reported() {
        let (filter, overridden) = env_filter("trace,rmcp=trace");
        assert_eq!(overridden, vec!["rmcp"]);
        let out = filter.to_string();
        assert!(out.contains("rmcp=trace"), "{out}");
        assert!(!out.contains("rmcp=info"), "{out}");
    }

    #[test]
    fn floors_apply_on_top_of_a_bare_crate_directive() {
        let out = rendered("ruprogress_mcp=debug");
        assert!(out.contains("ruprogress_mcp=debug"), "{out}");
        for (target, level) in FLOORS {
            assert!(
                out.contains(&format!("{target}={level}")),
                "missing floor {target}={level} in {out}"
            );
        }
    }

    #[test]
    fn a_malformed_directive_is_ignored_not_rejected() {
        // `EnvFilter::new` already parses leniently (an invalid directive is
        // dropped, not an error) — the floor must not change that.
        let (_filter, overridden) = env_filter("not a valid directive!!!");
        assert!(overridden.is_empty());
    }
}
