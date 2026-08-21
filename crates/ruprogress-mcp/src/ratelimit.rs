//! A hand-rolled, keyed token-bucket rate limiter (RL1/RL2).
//!
//! Deliberately dependency-free and free of any axum types, so this module
//! is testable in complete isolation from the HTTP transport — see
//! `transport::http` for the middleware that extracts a key from a request
//! and calls [`Limiter::check`].

use std::collections::HashMap;
use std::hash::Hash;
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
}

/// A keyed token bucket: `capacity = burst` tokens, refilling at `rps`
/// tokens/second, `std::time::Instant` as the clock (RL2). The map has a
/// hard entry cap (RL7): at capacity, a *new* key evicts the
/// least-recently-touched entry rather than being refused.
#[derive(Debug)]
pub struct Limiter<K> {
    buckets: Mutex<HashMap<K, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
    max_keys: usize,
}

impl<K: Eq + Hash + Clone> Limiter<K> {
    #[must_use]
    pub fn new(rps: u32, burst: u32, max_keys: usize) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: f64::from(burst),
            refill_per_sec: f64::from(rps),
            max_keys,
        }
    }

    pub fn check(&self, key: K, now: Instant) -> Decision {
        let mut buckets = self.buckets.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(bucket) = buckets.get_mut(&key) {
            return bucket.take(now, self.refill_per_sec, self.capacity);
        }
        if buckets.len() >= self.max_keys {
            evict_oldest(&mut buckets);
        }
        let mut bucket = Bucket::new(self.capacity, now);
        let decision = bucket.take(now, self.refill_per_sec, self.capacity);
        buckets.insert(key, bucket);
        decision
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buckets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
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
}
