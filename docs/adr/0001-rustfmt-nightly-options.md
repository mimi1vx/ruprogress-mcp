# ADR 0001: rustfmt nightly import options

## Context

The upstream `rmcp` rust-sdk uses nightly-only rustfmt options
(`group_imports = "StdExternalCrate"`, `imports_granularity = "Crate"`) which
produce a tidier import style than stable rustfmt defaults. Enabling them
requires a `cargo +nightly fmt` CI job, since stable `rustfmt` silently
ignores nightly-only keys (or errors, depending on version).

## Options

- (a) Stable-only `rustfmt.toml`, defaults + `edition = "2024"`.
- (b) Add the nightly options and a corresponding nightly `fmt` CI job.

## Decision

**(a) — stable-only.** A nightly toolchain dependency in CI buys style and
costs breakage (nightly rustfmt behaviour is not guaranteed stable across
releases). Revisit if import churn becomes a recurring review friction point.
