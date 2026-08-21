//! Finding 9 baseline: `apply_caps`' worst case is a per-item `pop` +
//! full-payload re-serialise loop when the byte cap fires. Four payload
//! shapes at the default caps (200 items / 256 KiB) — see
//! `plans/finding-09-11-performance-baselines.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ruprogress_mcp::tools::output::apply_caps_bench;
use serde_json::{Value, json};

const DEFAULT_MAX_ITEMS: usize = 200;
const DEFAULT_MAX_BYTES: usize = 256 * 1024;

/// A payload shaped like a real list-tool response: one top-level array
/// (`apply_caps` finds the first one) plus a sibling object field, matching
/// `IssuesOutput`/`ProjectsOutput`'s `{items, pagination}` shape closely
/// enough for the benchmark (the exact sibling fields don't matter — only
/// that there is exactly one top-level array).
fn payload(n_items: usize, item_bytes: usize) -> Value {
    let items: Vec<Value> = (0..n_items)
        .map(|id| json!({ "id": id, "payload": "x".repeat(item_bytes) }))
        .collect();
    json!({
        "items": items,
        "pagination": { "total": n_items, "truncated": false },
    })
}

/// `apply_caps` mutates its input in place (truncate/pop), so a naive
/// `iter()` would measure a progressively-emptier payload after the first
/// iteration. `iter_batched` with `LargeInput` clones the base payload fresh
/// for every iteration and excludes that clone from the timed region.
fn bench_shape(c: &mut Criterion, name: &str, n_items: usize, item_bytes: usize) {
    let base = std::hint::black_box(payload(n_items, item_bytes));
    c.bench_function(name, |b| {
        b.iter_batched(
            || base.clone(),
            |mut value| apply_caps_bench(&mut value, DEFAULT_MAX_ITEMS, DEFAULT_MAX_BYTES),
            BatchSize::LargeInput,
        );
    });
}

fn shape_a_under_both_caps(c: &mut Criterion) {
    bench_shape(c, "output_caps/a_under_both_caps", 50, 1024);
}

fn shape_b_item_cap_only(c: &mut Criterion) {
    bench_shape(c, "output_caps/b_item_cap_only", 500, 1024);
}

fn shape_c_byte_cap_moderate(c: &mut Criterion) {
    bench_shape(c, "output_caps/c_byte_cap_moderate", 200, 4 * 1024);
}

fn shape_d_byte_cap_worst_reachable(c: &mut Criterion) {
    let mut group = c.benchmark_group("output_caps");
    // Shape D allocates ~64 MiB per iteration; a handful of samples is
    // enough to get a stable median without an excessive total run time.
    group.sample_size(10);
    let base = std::hint::black_box(payload(200, 320 * 1024));
    group.bench_function("d_byte_cap_worst_reachable", |b| {
        b.iter_batched(
            || base.clone(),
            |mut value| apply_caps_bench(&mut value, DEFAULT_MAX_ITEMS, DEFAULT_MAX_BYTES),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

criterion_group!(
    benches,
    shape_a_under_both_caps,
    shape_b_item_cap_only,
    shape_c_byte_cap_moderate,
    shape_d_byte_cap_worst_reachable,
);
criterion_main!(benches);
