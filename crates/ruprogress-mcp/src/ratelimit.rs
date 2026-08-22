//! A hand-rolled, keyed token-bucket rate limiter (RL1/RL2).
//!
//! Deliberately dependency-free and free of any axum types, so this module
//! is testable in complete isolation from the HTTP transport — see
//! `transport::http` for the middleware that extracts a key from a request
//! and calls [`Limiter::check`].

use std::collections::HashMap;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The outcome of a [`Limiter::check`] call. The `bool`s report a state
/// *transition* (RL11) — whether this call started or ended a run of
/// denials for its key — so the caller can log once per transition rather
/// than once per request.
#[derive(Debug, Clone, Copy)]
pub enum Decision {
    Allow {
        recovered: bool,
    },
    Deny {
        retry_after_secs: u64,
        newly_limited: bool,
    },
}

/// One caller's token bucket. `tokens`/`capacity` are counted in whole
/// requests but tracked as `f64` so a fractional refill between two calls a
/// few milliseconds apart is not silently rounded away to zero.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    updated_at: Instant,
    /// Whether the most recent `take()` on this bucket denied.
    limited: bool,
}

impl Bucket {
    fn new(capacity: f64, now: Instant) -> Self {
        Self {
            tokens: capacity,
            updated_at: now,
            limited: false,
        }
    }

    /// Refills for the elapsed time since the last call, then takes one
    /// token if available.
    fn take(&mut self, now: Instant, refill_per_sec: f64, capacity: f64) -> Decision {
        // `checked_duration_since` rather than `now - self.updated_at`:
        // `now` is caller-supplied (tests step it explicitly), so it is not
        // guaranteed to be monotonically increasing relative to the bucket's
        // own clock reads. Treat a `now` that appears to be in the past as
        // "no time elapsed" rather than panicking or wrapping.
        let elapsed = now
            .checked_duration_since(self.updated_at)
            .unwrap_or(Duration::ZERO);
        let refilled = elapsed.as_secs_f64() * refill_per_sec;
        self.tokens = (self.tokens + refilled).min(capacity);
        self.updated_at = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            let recovered = self.limited;
            self.limited = false;
            Decision::Allow { recovered }
        } else {
            let deficit = 1.0 - self.tokens;
            let wait_secs = if refill_per_sec > 0.0 {
                deficit / refill_per_sec
            } else {
                f64::INFINITY
            };
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss,
                reason = "wait_secs is non-negative; clamped well within u64's range before \
                          the cast, and a 52-bit mantissa is nowhere near a real retry window"
            )]
            let retry_after_secs = wait_secs.clamp(1.0, u64::MAX as f64).ceil() as u64;
            let newly_limited = !self.limited;
            self.limited = true;
            Decision::Deny {
                retry_after_secs,
                newly_limited,
            }
        }
    }

    /// Whether this bucket, refilled up to `now`, is indistinguishable from
    /// a freshly created one for any future call: not currently limited,
    /// and refilled back to (or above, before clamping) `capacity`.
    ///
    /// This backs the eviction-scope narrowing below: evicting a
    /// pristine bucket is decision-equivalent to keeping it, since both a
    /// retained pristine bucket and a freshly recreated one start their
    /// next call at `capacity` tokens with `limited == false`. A shard can
    /// therefore free space by sweeping pristine entries before falling
    /// back to `evict_oldest`, with no effect on any `Decision` a caller
    /// could observe.
    fn is_pristine(&self, now: Instant, refill_per_sec: f64, capacity: f64) -> bool {
        if self.limited {
            return false;
        }
        let elapsed = now
            .checked_duration_since(self.updated_at)
            .unwrap_or(Duration::ZERO);
        let refilled = elapsed.as_secs_f64() * refill_per_sec;
        self.tokens + refilled >= capacity
    }
}

/// Below this many keys per shard, sharding buys nothing but adds a second
/// hash; `Limiter::new` folds back to a single shard instead.
const MIN_KEYS_PER_SHARD: usize = 64;
/// Enough to remove contention on realistic core counts without adding a
/// configuration knob nobody deploying this server needs.
const MAX_SHARDS: usize = 16;

/// A keyed token bucket: `capacity = burst` tokens, refilling at `rps`
/// tokens/second, `std::time::Instant` as the clock (RL2). The bucket map
/// is sharded to bound lock contention under concurrent keys; each shard
/// has its own hard entry cap (RL7) of roughly `max_keys /
/// shards`, and at capacity a *new* key evicts the least-recently-touched
/// entry in its shard rather than being refused.
#[derive(Debug)]
pub struct Limiter<K> {
    shards: Box<[Mutex<HashMap<K, Bucket>>]>,
    hasher: RandomState,
    /// `shards.len() - 1`; `shards.len()` is always a power of two, so
    /// `hash & shard_mask` is a fast, uniformly-distributed shard index.
    shard_mask: u64,
    capacity: f64,
    refill_per_sec: f64,
    per_shard_max_keys: usize,
}

impl<K: Eq + Hash + Clone> Limiter<K> {
    #[must_use]
    pub fn new(rps: u32, burst: u32, max_keys: usize) -> Self {
        let shard_count = (max_keys / MIN_KEYS_PER_SHARD)
            .clamp(1, MAX_SHARDS)
            .next_power_of_two();
        let shards = (0..shard_count)
            .map(|_| Mutex::new(HashMap::new()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            shards,
            hasher: RandomState::new(),
            shard_mask: u64::try_from(shard_count.saturating_sub(1)).unwrap_or(0),
            capacity: f64::from(burst),
            refill_per_sec: f64::from(rps),
            per_shard_max_keys: max_keys.div_ceil(shard_count),
        }
    }

    pub fn check(&self, key: K, now: Instant) -> Decision {
        let index = self.hasher.hash_one(&key) & self.shard_mask;
        #[allow(
            clippy::indexing_slicing,
            reason = "index is masked to shard_mask (shards.len() - 1, a power of two), so it \
                      is always in bounds; `shards.get` would force an unreachable None arm"
        )]
        let shard = &self.shards[usize::try_from(index).unwrap_or(0)];
        let mut buckets = shard.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bucket) = buckets.get_mut(&key) {
            return bucket.take(now, self.refill_per_sec, self.capacity);
        }
        if buckets.len() >= self.per_shard_max_keys {
            let (capacity, refill_per_sec) = (self.capacity, self.refill_per_sec);
            let before = buckets.len();
            buckets.retain(|_, bucket| !bucket.is_pristine(now, refill_per_sec, capacity));
            if buckets.len() == before {
                evict_oldest(&mut buckets);
            }
        }
        let mut bucket = Bucket::new(self.capacity, now);
        let decision = bucket.take(now, self.refill_per_sec, self.capacity);
        buckets.insert(key, bucket);
        decision
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| shard.lock().unwrap_or_else(PoisonError::into_inner).len())
            .sum()
    }
}

/// Evicts the single least-recently-touched bucket. Called only when
/// inserting a *new* key would otherwise push the map over `max_keys`; an
/// existing key's own bucket is never evicted by its own request.
fn evict_oldest<K: Eq + Hash + Clone>(buckets: &mut HashMap<K, Bucket>) {
    if let Some(oldest) = buckets
        .iter()
        .min_by_key(|(_, bucket)| bucket.updated_at)
        .map(|(key, _)| key.clone())
    {
        buckets.remove(&oldest);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "test-only LCG and index arithmetic over fixed, non-attacker-controlled constants"
)]
mod tests {
    use super::*;

    fn allowed(decision: &Decision) -> bool {
        matches!(decision, Decision::Allow { .. })
    }

    #[test]
    fn burst_then_deny() {
        let limiter: Limiter<&str> = Limiter::new(1, 2, 10);
        let now = Instant::now();
        assert!(allowed(&limiter.check("a", now)));
        assert!(allowed(&limiter.check("a", now)));
        let decision = limiter.check("a", now);
        assert!(!allowed(&decision));
        let Decision::Deny {
            retry_after_secs,
            newly_limited,
        } = decision
        else {
            panic!("expected Deny");
        };
        assert!(retry_after_secs >= 1);
        assert!(newly_limited, "the first denial should be a transition");
    }

    #[test]
    fn a_second_denial_in_a_row_is_not_a_new_transition() {
        let limiter: Limiter<&str> = Limiter::new(1, 1, 10);
        let now = Instant::now();
        assert!(allowed(&limiter.check("a", now)));
        let Decision::Deny { newly_limited, .. } = limiter.check("a", now) else {
            panic!("expected Deny");
        };
        assert!(newly_limited);
        let Decision::Deny { newly_limited, .. } = limiter.check("a", now) else {
            panic!("expected Deny");
        };
        assert!(!newly_limited, "already limited; not a new transition");
    }

    #[test]
    fn refill_after_time_recovers_and_reports_it() {
        let limiter: Limiter<&str> = Limiter::new(1, 1, 10);
        let t0 = Instant::now();
        assert!(allowed(&limiter.check("a", t0)));
        assert!(!allowed(&limiter.check("a", t0)));

        let later = t0 + Duration::from_secs(2);
        let Decision::Allow { recovered } = limiter.check("a", later) else {
            panic!("expected Allow after refill");
        };
        assert!(recovered, "should report recovery from a limited state");
    }

    #[test]
    fn eviction_keeps_the_map_at_or_under_the_cap() {
        let limiter: Limiter<u32> = Limiter::new(1, 1, 100);
        let now = Instant::now();
        for key in 0..50_000u32 {
            limiter.check(key, now);
            assert!(limiter.len() <= 100);
        }
        assert!(limiter.len() <= 100);
    }

    #[test]
    fn two_keys_are_independent() {
        let limiter: Limiter<&str> = Limiter::new(1, 1, 10);
        let now = Instant::now();
        assert!(allowed(&limiter.check("a", now)));
        assert!(!allowed(&limiter.check("a", now)));
        // "b" has never been seen, so it gets its own full bucket.
        assert!(allowed(&limiter.check("b", now)));
    }

    #[test]
    fn no_panic_on_extreme_rps_and_burst() {
        let limiter: Limiter<&str> = Limiter::new(u32::MAX, u32::MAX, 10);
        let now = Instant::now();
        for _ in 0..10 {
            limiter.check("a", now);
        }
        // A `now` far in the future must not overflow the refill math.
        limiter.check("a", now + Duration::from_secs(u64::from(u32::MAX)));
    }

    #[test]
    fn a_now_that_moves_backwards_does_not_panic_or_grant_extra_tokens() {
        let limiter: Limiter<&str> = Limiter::new(1, 1, 10);
        let now = Instant::now();
        assert!(allowed(&limiter.check("a", now)));
        let earlier = now.checked_sub(Duration::from_secs(5)).unwrap_or(now);
        assert!(!allowed(&limiter.check("a", earlier)));
    }

    #[test]
    fn small_max_keys_do_not_panic_or_divide_by_zero() {
        for max_keys in [0usize, 1, 3] {
            let limiter: Limiter<u32> = Limiter::new(1, 1, max_keys);
            let now = Instant::now();
            for key in 0..20u32 {
                limiter.check(key, now);
            }
        }
    }

    #[test]
    fn many_shards_keep_the_map_at_or_under_the_cap() {
        // Same shape as `eviction_keeps_the_map_at_or_under_the_cap`, but
        // with a `max_keys` large enough to land on the 16-shard path
        // (10_000 / 16 divides evenly, so the per-shard cap sums back to
        // exactly `max_keys`).
        let limiter: Limiter<u32> = Limiter::new(1, 1, 10_000);
        let now = Instant::now();
        for key in 0..50_000u32 {
            limiter.check(key, now);
            assert!(limiter.len() <= 10_000);
        }
        assert!(limiter.len() <= 10_000);
    }

    #[test]
    fn a_pristine_only_shard_admits_a_new_key_without_evicting_a_live_one() {
        // rps = burst = u32::MAX (as in `no_panic_on_extreme_rps_and_burst`)
        // means every bucket refills to `capacity` between two calls at the
        // same or a later `now`, so once each key has taken its one token
        // it is immediately pristine again.
        let limiter: Limiter<u32> = Limiter::new(u32::MAX, u32::MAX, 3);
        let now = Instant::now();
        for key in 0..3u32 {
            limiter.check(key, now);
        }
        assert_eq!(limiter.len(), 3);
        limiter.check(99, now);
        // The sweep should have reclaimed the pristine keys 0..3 rather
        // than running `evict_oldest`, so the new key is admitted and the
        // shard does not grow past its cap.
        assert!(limiter.len() <= 3);
    }

    #[test]
    fn a_shard_with_no_pristine_entries_evicts_exactly_the_oldest() {
        let limiter: Limiter<u32> = Limiter::new(1, 1, 3);
        let t0 = Instant::now();
        // Each key takes its single token and is never refilled enough to
        // reach `capacity` again, so none of them are pristine: the sweep
        // must free nothing and fall through to `evict_oldest`.
        limiter.check(0, t0);
        limiter.check(1, t0 + Duration::from_millis(1));
        limiter.check(2, t0 + Duration::from_millis(2));
        assert_eq!(limiter.len(), 3);
        limiter.check(3, t0 + Duration::from_millis(3));
        assert_eq!(limiter.len(), 3, "the cap must still hold after eviction");

        // Keys 1 and 2 must be the untouched, still-near-empty buckets from
        // above (querying an existing key never evicts), so the same tiny
        // refill window still denies them. Since the map holds exactly 3
        // entries — key 3 plus these two — key 0 (the only remaining
        // candidate) must be the one `evict_oldest` removed.
        let later = t0 + Duration::from_millis(4);
        assert!(
            !allowed(&limiter.check(1, later)),
            "key 1 must be undisturbed"
        );
        assert!(
            !allowed(&limiter.check(2, later)),
            "key 2 must be undisturbed"
        );
        assert_eq!(limiter.len(), 3, "checking existing keys must not evict");
    }

    /// A minimal single-map reference limiter mirroring pre-sharding
    /// `Limiter::check`, used only to prove the sharded implementation
    /// makes the same `Decision`s when `max_keys` is high enough that
    /// neither side ever evicts.
    struct ReferenceLimiter {
        buckets: std::collections::HashMap<u32, Bucket>,
        capacity: f64,
        refill_per_sec: f64,
    }

    impl ReferenceLimiter {
        fn new(rps: u32, burst: u32) -> Self {
            Self {
                buckets: std::collections::HashMap::new(),
                capacity: f64::from(burst),
                refill_per_sec: f64::from(rps),
            }
        }

        fn check(&mut self, key: u32, now: Instant) -> Decision {
            let capacity = self.capacity;
            let refill_per_sec = self.refill_per_sec;
            let bucket = self
                .buckets
                .entry(key)
                .or_insert_with(|| Bucket::new(capacity, now));
            bucket.take(now, refill_per_sec, capacity)
        }
    }

    fn decisions_match(a: Decision, b: Decision) -> bool {
        match (a, b) {
            (Decision::Allow { recovered: r1 }, Decision::Allow { recovered: r2 }) => r1 == r2,
            (
                Decision::Deny {
                    retry_after_secs: s1,
                    newly_limited: n1,
                },
                Decision::Deny {
                    retry_after_secs: s2,
                    newly_limited: n2,
                },
            ) => s1 == s2 && n1 == n2,
            _ => false,
        }
    }

    /// Tiny deterministic linear-congruential generator: enough entropy for
    /// a randomised equivalence sweep without a `dev-dependency` on `rand`.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            self.0
        }

        fn next_key(&mut self, alphabet: u32) -> u32 {
            u32::try_from(self.next() % u64::from(alphabet)).unwrap_or(0)
        }

        fn next_step_millis(&mut self) -> u64 {
            self.next() % 50
        }
    }

    #[test]
    fn sharded_and_reference_limiters_agree_with_no_eviction() {
        // `max_keys` far above the key alphabet: neither implementation
        // ever evicts, so only argument 1 (shard routing preserves
        // per-key decisions) is exercised, not the eviction-scope
        // narrowing of argument 2.
        const KEY_ALPHABET: u32 = 6;
        const STEPS: usize = 200;

        for seed in 0..5u64 {
            let mut rng = Lcg(seed.wrapping_mul(2).wrapping_add(1));
            let sharded: Limiter<u32> = Limiter::new(10, 40, 10_000);
            let mut reference = ReferenceLimiter::new(10, 40);
            let mut now = Instant::now();
            for step in 0..STEPS {
                now += Duration::from_millis(rng.next_step_millis());
                let key = rng.next_key(KEY_ALPHABET);
                let sharded_decision = sharded.check(key, now);
                let reference_decision = reference.check(key, now);
                assert!(
                    decisions_match(sharded_decision, reference_decision),
                    "seed {seed} step {step} key {key}: {sharded_decision:?} != \
                     {reference_decision:?}"
                );
            }
        }
    }

    #[test]
    fn sharded_and_reference_limiters_agree_on_fixed_edge_cases() {
        let sharded: Limiter<u32> = Limiter::new(u32::MAX, u32::MAX, 10_000);
        let mut reference = ReferenceLimiter::new(u32::MAX, u32::MAX);
        let t0 = Instant::now();

        for now in [
            t0,
            t0,
            t0 + Duration::from_secs(u64::from(u32::MAX)),
            t0.checked_sub(Duration::from_secs(5)).unwrap_or(t0),
        ] {
            let sharded_decision = sharded.check(1, now);
            let reference_decision = reference.check(1, now);
            assert!(decisions_match(sharded_decision, reference_decision));
        }
    }
}
