//! Retry policy for idempotent requests.
//!
//! Retries are restricted to `GET`/`HEAD`: Redmine has no idempotency keys,
//! so retrying a `POST`/`PUT`/`DELETE` risks creating a duplicate issue or
//! time entry.

use std::time::Duration;

/// Retry budget and backoff shape for a single logical request.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts after the first try.
    pub max_retries: u32,
    /// Base delay for exponential backoff.
    pub base: Duration,
    /// Backoff (and `Retry-After`) is never allowed to exceed this.
    pub max_backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base: Duration::from_millis(200),
            max_backoff: Duration::from_secs(5),
        }
    }
}

/// Whether `method` is safe to retry at all. Redmine has no idempotency
/// keys, so only the two verbs with no side effects qualify.
pub(crate) fn method_is_retryable(method: &http::Method) -> bool {
    matches!(*method, http::Method::GET | http::Method::HEAD)
}

/// Full-jitter exponential backoff for retry attempt `attempt` (0-based:
/// the delay before the *first* retry, i.e. after the initial failed try).
pub(crate) fn backoff_duration(policy: &RetryPolicy, attempt: u32) -> Duration {
    let factor = 2u32.checked_pow(attempt).unwrap_or(u32::MAX);
    let cap = policy.base.saturating_mul(factor).min(policy.max_backoff);
    let cap_millis = u64::try_from(cap.as_millis()).unwrap_or(u64::MAX);
    Duration::from_millis(rand::random_range(0..=cap_millis))
}

/// Parse a `Retry-After` header value: either delta-seconds (`"120"`) or an
/// HTTP-date (`"Sun, 06 Nov 1994 08:49:37 GMT"`).
pub(crate) fn retry_after(headers: &http::HeaderMap) -> Option<Duration> {
    let value = headers.get(http::header::RETRY_AFTER)?.to_str().ok()?;
    let value = value.trim();

    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }

    let when = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    let now = chrono::Utc::now();
    let delta_secs = when
        .with_timezone(&chrono::Utc)
        .signed_duration_since(now)
        .num_seconds();
    let secs = u64::try_from(delta_secs).unwrap_or(0);
    Some(Duration::from_secs(secs))
}

/// Clamp a caller- or server-supplied delay to `max_backoff`, so a
/// hostile/broken server sending `Retry-After: 86400` cannot park a request
/// for a day.
pub(crate) fn clamp_retry_after(delay: Duration, max_backoff: Duration) -> Duration {
    delay.min(max_backoff)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn only_get_and_head_are_retryable() {
        assert!(method_is_retryable(&http::Method::GET));
        assert!(method_is_retryable(&http::Method::HEAD));
        assert!(!method_is_retryable(&http::Method::POST));
        assert!(!method_is_retryable(&http::Method::PUT));
        assert!(!method_is_retryable(&http::Method::DELETE));
        assert!(!method_is_retryable(&http::Method::PATCH));
    }

    #[test]
    fn backoff_never_exceeds_cap() {
        let policy = RetryPolicy {
            max_retries: 3,
            base: Duration::from_millis(200),
            max_backoff: Duration::from_secs(5),
        };
        for attempt in 0..10 {
            let d = backoff_duration(&policy, attempt);
            assert!(d <= policy.max_backoff, "attempt {attempt}: {d:?} > cap");
        }
    }

    #[test]
    fn retry_after_parses_delta_seconds() {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::RETRY_AFTER,
            http::HeaderValue::from_static("2"),
        );
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(2)));
    }

    #[test]
    fn retry_after_parses_http_date() {
        let future = chrono::Utc::now() + chrono::Duration::seconds(10);
        let value = future.to_rfc2822();
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::RETRY_AFTER,
            http::HeaderValue::from_str(&value).unwrap(),
        );
        let parsed = retry_after(&headers).expect("should parse HTTP-date");
        // Allow a couple seconds of test-execution slack either side.
        assert!(parsed.as_secs() <= 12, "got {parsed:?}");
    }

    #[test]
    fn clamp_retry_after_caps_a_hostile_value() {
        let hostile = Duration::from_hours(24);
        assert_eq!(
            clamp_retry_after(hostile, Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }
}
