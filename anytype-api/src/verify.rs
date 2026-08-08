//! Verification helpers for eventual consistency.

use std::{future::Future, time::Duration};

use tokio::time::Instant;
use tracing::{debug, warn};

use crate::{Result, error::AnytypeError};

/// Hard safety ceiling for semantic verification attempts.
///
/// This bound also makes zero-delay verification finite and prevents an
/// accidentally enormous caller-supplied attempt count from monopolizing an
/// async executor.
pub const MAX_VERIFY_ATTEMPTS: usize = 10_000;

/// Configuration for bounded read-after-write verification.
#[derive(Debug, Clone)]
pub struct VerifyConfig {
    /// Upper bound for total verification time (wall clock), including delays
    /// and in-flight fetches.
    pub timeout: Duration,
    /// Delay before the first verification attempt.
    pub initial_delay: Duration,
    /// Maximum delay between attempts.
    pub max_delay: Duration,
    /// Maximum number of verification attempts.
    ///
    /// Values above [`MAX_VERIFY_ATTEMPTS`] are clamped to that hard ceiling.
    /// The legacy value zero, which previously disabled the attempt cap, is
    /// also normalized to the hard ceiling so existing configurations remain
    /// useful without permitting an unbounded zero-delay loop.
    pub max_attempts: usize,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(3),
            initial_delay: Duration::from_millis(50),
            max_delay: Duration::from_millis(300),
            max_attempts: 10,
        }
    }
}

impl VerifyConfig {
    /// Returns the validated, nonzero finite attempt cap used by verification.
    #[must_use]
    pub const fn effective_max_attempts(&self) -> usize {
        if self.max_attempts == 0 || self.max_attempts > MAX_VERIFY_ATTEMPTS {
            MAX_VERIFY_ATTEMPTS
        } else {
            self.max_attempts
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum VerifyPolicy {
    Default,
    Enabled,
    Disabled,
}

pub(crate) fn resolve_verify(
    policy: VerifyPolicy,
    config: Option<&VerifyConfig>,
) -> Option<VerifyConfig> {
    match policy {
        VerifyPolicy::Disabled => None,
        VerifyPolicy::Default => config.cloned(),
        VerifyPolicy::Enabled => Some(config.cloned().unwrap_or_default()),
    }
}

/// Repeatedly fetches a value until it satisfies a semantic predicate.
///
/// Both successful-but-stale values and transient fetch failures are retried.
/// Every started fetch counts as an attempt, and verification is bounded by
/// both [`VerifyConfig::timeout`] and [`VerifyConfig::max_attempts`]. The
/// timeout includes the initial delay, retry backoff, and time spent awaiting a
/// fetch. Backoff doubles up to `max_delay` and remains cancellation-safe.
///
/// The verifier never formats, logs, or stores fetched values. Retry failures
/// are represented in the terminal error only by a fixed, secret-safe
/// classification rather than by an upstream error string.
///
/// # Errors
///
/// Returns a nonretryable fetch error immediately or
/// [`AnytypeError::VerifyTimeout`] when either finite verification bound is
/// exhausted.
pub async fn verify_semantic<T, Fut, Fetch, Ready>(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    fetch: Fetch,
    ready: Ready,
) -> Result<T>
where
    Fetch: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
    Ready: FnMut(&T) -> bool,
{
    let mut fetch = fetch;
    verify_with_retry_policy(
        config,
        obj_type,
        key,
        move |_| fetch(),
        ready,
        semantic_retryable,
    )
    .await
}

/// Repeatedly fetches a value while exposing the current remaining wall-clock
/// budget to each attempt.
///
/// This is the deadline-aware form of [`verify_semantic`]. It is intended for
/// compound reads whose inner RPC must carry a deadline no greater than the
/// verifier's remaining timeout. The supplied duration is always nonzero and
/// already includes delays and work spent by earlier attempts.
pub async fn verify_semantic_with_remaining<T, Fut, Fetch, Ready>(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    fetch: Fetch,
    ready: Ready,
) -> Result<T>
where
    Fetch: FnMut(Duration) -> Fut,
    Fut: Future<Output = Result<T>>,
    Ready: FnMut(&T) -> bool,
{
    verify_with_retry_policy(config, obj_type, key, fetch, ready, semantic_retryable).await
}

async fn verify_with_retry_policy<T, Fut, Fetch, Ready, Retryable>(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    mut fetch: Fetch,
    mut ready: Ready,
    retryable: Retryable,
) -> Result<T>
where
    Fetch: FnMut(Duration) -> Fut,
    Fut: Future<Output = Result<T>>,
    Ready: FnMut(&T) -> bool,
    Retryable: Fn(&AnytypeError) -> bool,
{
    let start = Instant::now();
    let mut attempts = 0usize;
    let max_attempts = config.effective_max_attempts();
    let mut delay = config.initial_delay;
    if !delay.is_zero() && !sleep_within(start, config.timeout, delay).await {
        return Err(verify_timeout(config, obj_type, key, attempts, None));
    }

    loop {
        let Some(remaining) = config.timeout.checked_sub(start.elapsed()) else {
            return Err(verify_timeout(config, obj_type, key, attempts, None));
        };
        if remaining.is_zero() {
            return Err(verify_timeout(config, obj_type, key, attempts, None));
        }

        attempts += 1;
        let fetched = match tokio::time::timeout(remaining, fetch(remaining)).await {
            Ok(result) => result,
            Err(_) => {
                return Err(verify_timeout(
                    config,
                    obj_type,
                    key,
                    attempts,
                    Some("fetch_timeout"),
                ));
            }
        };

        let retry_class = match fetched {
            Ok(value) => {
                if ready(&value) {
                    return Ok(value);
                }
                drop(value);
                debug!(
                    obj_type,
                    key,
                    attempt = attempts,
                    "verify value is stale, retrying"
                );
                "stale_value"
            }
            Err(error) if retryable(&error) => {
                let class = retry_classification(&error);
                match &error {
                    AnytypeError::ApiError { code, .. } if *code >= 500 => warn!(
                        obj_type,
                        key,
                        attempt = attempts,
                        code,
                        "verify saw transient server error, retrying"
                    ),
                    AnytypeError::ApiError { code: 404, .. } | AnytypeError::NotFound { .. } => {
                        debug!(
                            obj_type,
                            key,
                            attempt = attempts,
                            "verify not found, retrying"
                        )
                    }
                    AnytypeError::Http { .. } => warn!(
                        obj_type,
                        key,
                        attempt = attempts,
                        "verify saw http error, retrying"
                    ),
                    AnytypeError::TooManyRetries { .. } => warn!(
                        obj_type,
                        key,
                        attempt = attempts,
                        "verify retry limit hit, retrying"
                    ),
                    _ => {}
                }
                class
            }
            Err(error) => return Err(error),
        };

        if attempts >= max_attempts || start.elapsed() >= config.timeout {
            return Err(verify_timeout(
                config,
                obj_type,
                key,
                attempts,
                Some(retry_class),
            ));
        }

        delay = next_delay(delay, config.max_delay);
        if delay.is_zero() {
            // A ready fetch plus zero backoff would otherwise monopolize the
            // executor until the attempt cap. Yielding also makes cancellation
            // of the verifier future prompt.
            tokio::task::yield_now().await;
        } else if !sleep_within(start, config.timeout, delay).await {
            return Err(verify_timeout(
                config,
                obj_type,
                key,
                attempts,
                Some(retry_class),
            ));
        }
    }
}

pub(crate) async fn verify_available<T, Fut, Fetch>(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    fetch: Fetch,
) -> Result<T>
where
    Fetch: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut fetch = fetch;
    verify_with_retry_policy(
        config,
        obj_type,
        key,
        move |_| fetch(),
        |_| true,
        legacy_retryable,
    )
    .await
}

fn legacy_retryable(error: &AnytypeError) -> bool {
    matches!(
        error,
        AnytypeError::NotFound { .. }
            | AnytypeError::Http { .. }
            | AnytypeError::TooManyRetries { .. }
    ) || matches!(error, AnytypeError::ApiError { code, .. } if *code >= 500)
}

fn semantic_retryable(error: &AnytypeError) -> bool {
    legacy_retryable(error) || matches!(error, AnytypeError::ApiError { code: 404, .. })
}

const fn retry_classification(error: &AnytypeError) -> &'static str {
    match error {
        AnytypeError::NotFound { .. } | AnytypeError::ApiError { code: 404, .. } => "not_found",
        AnytypeError::Http { .. } => "http_transport",
        AnytypeError::TooManyRetries { .. } => "retry_limit",
        AnytypeError::ApiError { code, .. } if *code >= 500 => "server_error",
        _ => "retryable_error",
    }
}

fn verify_timeout(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    attempts: usize,
    last_classification: Option<&'static str>,
) -> AnytypeError {
    warn!(
        obj_type,
        key,
        attempts,
        elapsed_limit_ms = config.timeout.as_millis(),
        last_classification = last_classification.unwrap_or("none"),
        "verify exhausted its finite bounds"
    );
    AnytypeError::VerifyTimeout {
        obj_type: obj_type.to_owned(),
        key: key.to_owned(),
        attempts,
        timeout: config.timeout,
        last_error: last_classification.map(str::to_owned),
    }
}

async fn sleep_within(start: Instant, timeout: Duration, delay: Duration) -> bool {
    let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
        return false;
    };
    if remaining.is_zero() {
        return false;
    }
    tokio::time::timeout(remaining, tokio::time::sleep(delay))
        .await
        .is_ok()
}

fn next_delay(current: Duration, maximum: Duration) -> Duration {
    if current.is_zero() {
        Duration::ZERO
    } else {
        current.saturating_mul(2).min(maximum)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use super::*;

    fn config(max_attempts: usize) -> VerifyConfig {
        VerifyConfig {
            timeout: Duration::from_secs(1),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts,
        }
    }

    fn api_error(code: u16, secret: &str) -> AnytypeError {
        AnytypeError::ApiError {
            code,
            method: "GET".to_owned(),
            url: format!("https://secret.invalid/{secret}"),
            message: secret.to_owned(),
        }
    }

    fn timeout_parts(error: AnytypeError) -> (usize, Option<String>) {
        let AnytypeError::VerifyTimeout {
            attempts,
            last_error,
            ..
        } = error
        else {
            panic!("expected verify timeout");
        };
        (attempts, last_error)
    }

    #[tokio::test]
    async fn stale_values_converge_false_false_true() {
        let attempt = AtomicUsize::new(0);
        let result = verify_semantic(
            &config(3),
            "Object",
            "object-id",
            || async { Ok(attempt.fetch_add(1, Ordering::SeqCst) + 1) },
            |value| *value == 3,
        )
        .await
        .unwrap();
        assert_eq!(result, 3);
        assert_eq!(attempt.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn exact_attempt_cap_is_enforced_for_zero_delay() {
        let attempts = AtomicUsize::new(0);
        let error = verify_semantic(
            &config(4),
            "Object",
            "object-id",
            || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            },
            |_| false,
        )
        .await
        .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
        assert_eq!(timeout_parts(error), (4, Some("stale_value".to_owned())));
    }

    #[tokio::test]
    async fn transient_404_and_5xx_then_stale_then_success_converge() {
        let attempts = AtomicUsize::new(0);
        let error_secret = "BEARER_PRIVATE_BODY";
        let result = verify_semantic(
            &config(4),
            "Object",
            "object-id",
            || async {
                match attempts.fetch_add(1, Ordering::SeqCst) {
                    0 => Err(api_error(404, error_secret)),
                    1 => Err(api_error(503, error_secret)),
                    2 => Ok(1),
                    _ => Ok(2),
                }
            },
            |value| *value == 2,
        )
        .await
        .unwrap();
        assert_eq!(result, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn nonretryable_error_stops_immediately() {
        let attempts = AtomicUsize::new(0);
        let error = verify_semantic(
            &config(5),
            "Object",
            "object-id",
            || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<(), _>(api_error(400, "PRIVATE_BAD_REQUEST"))
            },
            |_| true,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AnytypeError::ApiError { code: 400, .. }));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_bounds_backoff_and_in_flight_fetches() {
        let backoff = VerifyConfig {
            timeout: Duration::from_millis(25),
            initial_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(20),
            max_attempts: 10,
        };
        let attempts = AtomicUsize::new(0);
        let error = verify_semantic(
            &backoff,
            "Object",
            "object-id",
            || async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(0)
            },
            |_| false,
        )
        .await
        .unwrap_err();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(timeout_parts(error), (1, Some("stale_value".to_owned())));

        let fetch_timeout = VerifyConfig {
            timeout: Duration::from_millis(10),
            initial_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            max_attempts: 2,
        };
        let error = verify_semantic(
            &fetch_timeout,
            "Object",
            "object-id",
            std::future::pending::<Result<()>>,
            |_| true,
        )
        .await
        .unwrap_err();
        assert_eq!(timeout_parts(error), (1, Some("fetch_timeout".to_owned())));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_and_usize_max_attempt_bounds_exhaust_the_clamped_loop() {
        for max_attempts in [0, usize::MAX] {
            let calls = AtomicUsize::new(0);
            let bounded = VerifyConfig {
                timeout: Duration::from_secs(60 * 60),
                ..config(max_attempts)
            };
            let error = verify_semantic(
                &bounded,
                "Object",
                "object-id",
                || async {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
                |_| false,
            )
            .await
            .unwrap_err();
            assert_eq!(calls.load(Ordering::SeqCst), MAX_VERIFY_ATTEMPTS);
            assert_eq!(
                timeout_parts(error),
                (MAX_VERIFY_ATTEMPTS, Some("stale_value".to_owned()))
            );
        }
    }

    #[tokio::test]
    async fn legacy_availability_retry_classifier_preserves_exact_parity() {
        let http_source = reqwest::Client::new()
            .get("not a valid URL")
            .build()
            .unwrap_err();
        let cases = [
            (
                "not_found",
                AnytypeError::NotFound {
                    obj_type: "Object".to_owned(),
                    key: "id".to_owned(),
                },
                true,
            ),
            (
                "http",
                AnytypeError::Http {
                    method: "GET".to_owned(),
                    url: "https://redacted.invalid".to_owned(),
                    source: http_source,
                    outcome: None,
                    elapsed: None,
                    attempts: None,
                },
                true,
            ),
            (
                "too_many_retries",
                AnytypeError::TooManyRetries { n: 3 },
                true,
            ),
            ("api_500", api_error(500, "PRIVATE_500"), true),
            ("api_599", api_error(599, "PRIVATE_599"), true),
            ("api_404", api_error(404, "PRIVATE_404"), false),
            ("api_400", api_error(400, "PRIVATE_400"), false),
            ("unauthorized", AnytypeError::Unauthorized, false),
            ("forbidden", AnytypeError::Forbidden, false),
            (
                "validation",
                AnytypeError::Validation {
                    message: "PRIVATE_VALIDATION".to_owned(),
                },
                false,
            ),
        ];

        for (name, error, expected) in cases {
            assert_eq!(legacy_retryable(&error), expected, "{name}");
        }

        let calls = AtomicUsize::new(0);
        let error = verify_available(&config(2), "Object", "id", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(api_error(404, "PRIVATE_404"))
        })
        .await
        .unwrap_err();
        assert!(matches!(error, AnytypeError::ApiError { code: 404, .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn availability_wrapper_preserves_first_success_behavior() {
        let calls = AtomicUsize::new(0);
        let value = verify_available(&config(2), "Object", "id", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("available")
        })
        .await
        .unwrap();
        assert_eq!(value, "available");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropped_verifier_drops_in_flight_fetch_without_retaining_value() {
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn({
            let dropped = dropped.clone();
            let entered = entered.clone();
            async move {
                let _ = verify_semantic(
                    &config(2),
                    "Object",
                    "id",
                    || {
                        let guard = DropFlag(dropped.clone());
                        entered.notify_one();
                        async move {
                            let _guard = guard;
                            std::future::pending::<Result<()>>().await
                        }
                    },
                    |_| true,
                )
                .await;
            }
        });
        entered.notified().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn retry_classifications_are_fixed_and_never_contain_error_values() {
        let secret = "SUPER_SECRET_VALUE";
        let error = api_error(503, secret);
        let class = retry_classification(&error);
        assert_eq!(class, "server_error");
        assert!(!class.contains(secret));
    }

    #[test]
    fn backoff_doubles_and_clamps_exactly() {
        assert_eq!(
            next_delay(Duration::from_millis(10), Duration::from_millis(25)),
            Duration::from_millis(20)
        );
        assert_eq!(
            next_delay(Duration::from_millis(20), Duration::from_millis(25)),
            Duration::from_millis(25)
        );
        assert_eq!(
            next_delay(Duration::ZERO, Duration::from_secs(1)),
            Duration::ZERO
        );
        assert_eq!(next_delay(Duration::MAX, Duration::MAX), Duration::MAX);
        assert_eq!(
            next_delay(Duration::MAX, Duration::from_secs(7)),
            Duration::from_secs(7)
        );
    }
}
