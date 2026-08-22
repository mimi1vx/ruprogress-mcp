//! Finding 10 baseline: `Limiter::check`'s O(n) eviction scan under a
//! global mutex, and whether it becomes a throughput ceiling under
//! contention. Cases A-C are single-threaded; case D measures 1/2/4/8-thread
//! throughput at two key cardinalities. See
//! `plans/finding-09-11-performance-baselines.md`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use ruprogress_mcp::ratelimit::Limiter;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const MAX_KEYS: usize = 10_000;

/// A limiter with effectively-unbounded capacity/refill, so every call is an
/// `Allow` and the measured cost is purely the lock + bucket-update path,
/// not refill arithmetic edge cases.
fn unlimited(max_keys: usize) -> Limiter<u64> {
    Limiter::new(u32::MAX, u32::MAX, max_keys)
}

/// A limiter using the shipped defaults (`rps = 10`, `burst = 40`). A
/// pre-filled bucket sits at 39/40 tokens after its first hit — not
/// pristine — so this is the pessimistic case for a pristine-bucket sweep
/// before eviction: `unlimited` above makes every pre-filled bucket
/// pristine and would flatter such a sweep.
fn realistic(max_keys: usize) -> Limiter<u64> {
    Limiter::new(10, 40, max_keys)
}

fn case_a_warm_key(c: &mut Criterion) {
    let limiter = unlimited(MAX_KEYS);
    let mut now = Instant::now();
    limiter.check(0, now); // warm the one key this case hits.
    c.bench_function("ratelimit/a_warm_key", |b| {
        b.iter(|| {
            now += Duration::from_micros(1);
            limiter.check(0, now)
        });
    });
}

fn case_b_new_key_below_capacity(c: &mut Criterion) {
    let mut next_key: u64 = 0;
    c.bench_function("ratelimit/b_new_key_below_capacity", |b| {
        b.iter_batched(
            || {
                // A fresh, near-empty map every iteration: "far from
                // max_keys" per the plan, so insertion never evicts.
                let limiter = unlimited(MAX_KEYS);
                next_key += 1;
                (limiter, next_key)
            },
            |(limiter, key)| limiter.check(key, Instant::now()),
            BatchSize::SmallInput,
        );
    });
}

fn case_c_new_key_at_capacity(c: &mut Criterion) {
    let mut next_key: u64 = u64::try_from(MAX_KEYS).unwrap();
    let mut group = c.benchmark_group("ratelimit");
    // Setup (excluded from timing) pre-fills 10,000 entries every
    // iteration, so keep the sample count modest.
    group.sample_size(20);
    group.bench_function("c_new_key_at_capacity", |b| {
        b.iter_batched(
            || {
                let limiter = unlimited(MAX_KEYS);
                let now = Instant::now();
                for k in 0..u64::try_from(MAX_KEYS).unwrap() {
                    limiter.check(k, now);
                }
                next_key += 1;
                (limiter, next_key)
            },
            |(limiter, key)| limiter.check(key, Instant::now()),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

/// Same as `case_c_new_key_at_capacity`, but against `realistic()` so the
/// pre-filled buckets are not pristine.
fn case_c_new_key_at_capacity_realistic(c: &mut Criterion) {
    let mut next_key: u64 = u64::try_from(MAX_KEYS).unwrap();
    let mut group = c.benchmark_group("ratelimit");
    group.sample_size(20);
    group.bench_function("c_new_key_at_capacity_realistic", |b| {
        b.iter_batched(
            || {
                let limiter = realistic(MAX_KEYS);
                let now = Instant::now();
                for k in 0..u64::try_from(MAX_KEYS).unwrap() {
                    limiter.check(k, now);
                }
                next_key += 1;
                (limiter, next_key)
            },
            |(limiter, key)| limiter.check(key, Instant::now()),
            BatchSize::LargeInput,
        );
    });
    group.finish();
}

/// Runs `threads` workers, each issuing `per_thread` `check()` calls against
/// one shared `limiter`, and returns the wall-clock time for the whole
/// batch. `key_for` picks each call's key given (thread index, call index).
fn run_parallel(
    limiter: &Arc<Limiter<u64>>,
    threads: usize,
    per_thread: u64,
    key_for: impl Fn(usize, u64) -> u64 + Sync,
) -> Duration {
    let start = Instant::now();
    thread::scope(|scope| {
        for t in 0..threads {
            let limiter = Arc::clone(limiter);
            let key_for = &key_for;
            scope.spawn(move || {
                let now = Instant::now();
                for i in 0..per_thread {
                    limiter.check(key_for(t, i), now);
                }
            });
        }
    });
    start.elapsed()
}

/// Case D: the finding's actual claim ("rate limiting itself becomes a
/// `DoS` amplifier"). "Warm" cardinality has each thread hammer its own
/// already-existing key (contention on the mutex only); "saturated"
/// cardinality has every call mint a brand-new key against a map already at
/// `MAX_KEYS`, so every call also pays case C's full eviction scan.
fn case_d_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("ratelimit/d_contention");
    for threads in [1usize, 2, 4, 8] {
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("{threads}_threads_warm"), |b| {
            b.iter_custom(|iters| {
                let limiter = Arc::new(unlimited(MAX_KEYS));
                let now = Instant::now();
                for t in 0..threads {
                    limiter.check(u64::try_from(t).unwrap(), now);
                }
                let per_thread = (iters / u64::try_from(threads).unwrap()).max(1);
                run_parallel(&limiter, threads, per_thread, |t, _| {
                    u64::try_from(t).unwrap()
                })
            });
        });

        group.bench_function(format!("{threads}_threads_saturated"), |b| {
            b.iter_custom(|iters| {
                let limiter = Arc::new(unlimited(MAX_KEYS));
                let now = Instant::now();
                for k in 0..u64::try_from(MAX_KEYS).unwrap() {
                    limiter.check(k, now);
                }
                let next = Arc::new(AtomicU64::new(u64::try_from(MAX_KEYS).unwrap()));
                let per_thread = (iters / u64::try_from(threads).unwrap()).max(1);
                run_parallel(&limiter, threads, per_thread, move |_, _| {
                    next.fetch_add(1, Ordering::Relaxed)
                })
            });
        });
    }
    group.finish();
}

/// Same as `case_d_contention`'s saturated legs, but against `realistic()`.
/// The warm legs are not duplicated: they never evict, so pristine-sweep
/// cost cannot show up in them regardless of limiter parameters.
fn case_d_contention_saturated_realistic(c: &mut Criterion) {
    let mut group = c.benchmark_group("ratelimit/d_contention_realistic");
    for threads in [1usize, 2, 4, 8] {
        group.throughput(Throughput::Elements(1));
        group.bench_function(format!("{threads}_threads_saturated"), |b| {
            b.iter_custom(|iters| {
                let limiter = Arc::new(realistic(MAX_KEYS));
                let now = Instant::now();
                for k in 0..u64::try_from(MAX_KEYS).unwrap() {
                    limiter.check(k, now);
                }
                let next = Arc::new(AtomicU64::new(u64::try_from(MAX_KEYS).unwrap()));
                let per_thread = (iters / u64::try_from(threads).unwrap()).max(1);
                run_parallel(&limiter, threads, per_thread, move |_, _| {
                    next.fetch_add(1, Ordering::Relaxed)
                })
            });
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    case_a_warm_key,
    case_b_new_key_below_capacity,
    case_c_new_key_at_capacity,
    case_c_new_key_at_capacity_realistic,
    case_d_contention,
    case_d_contention_saturated_realistic,
);
criterion_main!(benches);
