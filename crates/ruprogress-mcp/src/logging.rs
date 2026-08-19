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

#[cfg(test)]
mod tests {
    use super::{FLOORS, env_filter};

    fn rendered(requested: &str) -> String {
        env_filter(requested).0.to_string()
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
