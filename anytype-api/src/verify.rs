//! Verification helpers for eventual consistency.

use std::{
    future::Future,
    time::{Duration, Instant},
};

use tracing::{debug, warn};

use crate::{Result, error::AnytypeError};

/// Configuration for verifying read-after-write availability.
#[derive(Debug, Clone)]
pub struct VerifyConfig {
    /// Upper bound for total verification time (wall clock).
    pub timeout: Duration,
    /// Delay before the first verification attempt.
    pub initial_delay: Duration,
    /// Maximum delay between attempts.
    pub max_delay: Duration,
    /// Maximum number of verification attempts (0 disables the cap).
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

pub(crate) async fn verify_available<T, Fut, F>(
    config: &VerifyConfig,
    obj_type: &str,
    key: &str,
    mut fetch: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let start = Instant::now();
    let mut attempt = 0usize;
    let mut delay = config.initial_delay;
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }

    loop {
        attempt += 1;
        match fetch().await {
            Ok(result) => return Ok(result),
            Err(err) => {
                let retryable = matches!(
                    err,
                    AnytypeError::NotFound { .. }
                        | AnytypeError::Http { .. }
                        | AnytypeError::TooManyRetries { .. }
                ) || matches!(err, AnytypeError::ApiError { code, .. } if code >= 500);

                if !retryable {
                    return Err(err);
                }

                let err_string = err.to_string();

                let elapsed = start.elapsed();
                let attempts_exhausted = config.max_attempts > 0 && attempt >= config.max_attempts;
                let timeout_exhausted = elapsed >= config.timeout;
                if attempts_exhausted || timeout_exhausted {
                    warn!(
                        obj_type,
                        key,
                        attempt,
                        elapsed_ms = elapsed.as_millis(),
                        "verify giving up after retryable error"
                    );
                    return Err(AnytypeError::VerifyTimeout {
                        obj_type: obj_type.to_string(),
                        key: key.to_string(),
                        attempts: attempt,
                        timeout: config.timeout,
                        last_error: Some(err_string),
                    });
                }

                match &err {
                    AnytypeError::ApiError { code, .. } if *code >= 500 => {
                        warn!(
                            obj_type,
                            key, attempt, code, "verify saw transient server error, retrying"
                        );
                    }
                    AnytypeError::Http { .. } => {
                        warn!(obj_type, key, attempt, "verify saw http error, retrying");
                    }
                    AnytypeError::NotFound { .. } => {
                        debug!(obj_type, key, attempt, "verify not found, retrying");
                    }
                    AnytypeError::TooManyRetries { .. } => {
                        warn!(obj_type, key, attempt, "verify retry limit hit, retrying");
                    }
                    _ => {}
                }

                let next_delay = if delay.is_zero() {
                    Duration::from_millis(0)
                } else {
                    let doubled = delay.mul_f64(2.0);
                    if doubled > config.max_delay {
                        config.max_delay
                    } else {
                        doubled
                    }
                };

                if !next_delay.is_zero() {
                    tokio::time::sleep(next_delay).await;
                }
                delay = next_delay;
            }
        }
    }
}
