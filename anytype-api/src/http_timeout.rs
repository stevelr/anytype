//! Logical HTTP deadline configuration and classifications.

use std::{ffi::OsString, fmt, time::Duration};

use crate::{Result, error::AnytypeError};

/// Process environment variable that overrides inherited HTTP deadlines.
pub const ANYTYPE_HTTP_TIMEOUT_SECS: &str = "ANYTYPE_HTTP_TIMEOUT_SECS";
/// Largest supported finite logical HTTP deadline.
pub const MAX_HTTP_TIMEOUT: Duration = Duration::from_secs(3_600);
/// Default deadline for buffered HTTP operations.
pub const DEFAULT_STANDARD_HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// Default deadline for file and multipart HTTP operations.
pub const DEFAULT_LONG_HTTP_TIMEOUT: Duration = Duration::from_secs(600);
/// Default deadline for opening a successful Server-Sent Events response.
pub const DEFAULT_SSE_OPEN_TIMEOUT: Duration = Duration::from_secs(120);
/// Default deadline for buffering a non-success Server-Sent Events response.
pub const DEFAULT_SSE_ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(120);

/// Logical HTTP deadlines applied by the Anytype client.
///
/// A finite value must be between one and 3,600 seconds inclusive. `None`
/// disables that boundary. Supplying this policy through
/// [`ClientConfig::http_timeouts`](crate::ClientConfig::http_timeouts) ignores
/// [`ANYTYPE_HTTP_TIMEOUT_SECS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpTimeoutPolicy {
    /// Deadline for ordinary buffered requests.
    pub standard_operation: Option<Duration>,
    /// Deadline for file and multipart requests.
    pub long_operation: Option<Duration>,
    /// Deadline through successful SSE response headers.
    pub sse_open: Option<Duration>,
    /// Deadline for a non-success SSE response body.
    pub sse_error_body: Option<Duration>,
    /// Optional no-progress deadline for an established SSE stream.
    pub sse_idle: Option<Duration>,
    /// Optional total lifetime for an established SSE stream.
    pub sse_total_lifetime: Option<Duration>,
}

impl Default for HttpTimeoutPolicy {
    fn default() -> Self {
        Self {
            standard_operation: Some(DEFAULT_STANDARD_HTTP_TIMEOUT),
            long_operation: Some(DEFAULT_LONG_HTTP_TIMEOUT),
            sse_open: Some(DEFAULT_SSE_OPEN_TIMEOUT),
            sse_error_body: Some(DEFAULT_SSE_ERROR_BODY_TIMEOUT),
            sse_idle: None,
            sse_total_lifetime: None,
        }
    }
}

impl HttpTimeoutPolicy {
    pub(crate) fn validate(self) -> Result<Self> {
        for (field, value) in [
            ("standard_operation", self.standard_operation),
            ("long_operation", self.long_operation),
            ("sse_open", self.sse_open),
            ("sse_error_body", self.sse_error_body),
            ("sse_idle", self.sse_idle),
            ("sse_total_lifetime", self.sse_total_lifetime),
        ] {
            if let Some(duration) = value
                && !(Duration::from_secs(1)..=MAX_HTTP_TIMEOUT).contains(&duration)
            {
                return Err(AnytypeError::Validation {
                    message: format!(
                        "http_timeouts.{field} must be disabled or between 1 and 3600 seconds"
                    ),
                });
            }
        }
        Ok(self)
    }

    pub(crate) fn resolve(explicit: Option<Self>) -> Result<Self> {
        if let Some(policy) = explicit {
            return policy.validate();
        }
        Self::from_environment(std::env::var_os(ANYTYPE_HTTP_TIMEOUT_SECS))
    }

    fn from_environment(value: Option<OsString>) -> Result<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let value = value.into_string().map_err(|_| AnytypeError::Validation {
            message: format!("{ANYTYPE_HTTP_TIMEOUT_SECS} must be Unicode ASCII decimal"),
        })?;
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(AnytypeError::Validation {
                message: format!(
                    "{ANYTYPE_HTTP_TIMEOUT_SECS} must be an ASCII decimal from 0 through 3600"
                ),
            });
        }
        let seconds = value.parse::<u64>().map_err(|_| AnytypeError::Validation {
            message: format!(
                "{ANYTYPE_HTTP_TIMEOUT_SECS} must be an ASCII decimal from 0 through 3600"
            ),
        })?;
        if seconds > MAX_HTTP_TIMEOUT.as_secs() {
            return Err(AnytypeError::Validation {
                message: format!("{ANYTYPE_HTTP_TIMEOUT_SECS} must not exceed 3600 seconds"),
            });
        }
        let inherited = (seconds != 0).then(|| Duration::from_secs(seconds));
        Ok(Self {
            standard_operation: inherited,
            long_operation: inherited,
            sse_open: inherited,
            sse_error_body: inherited,
            sse_idle: None,
            sse_total_lifetime: None,
        })
    }

    pub(crate) const fn duration(self, class: HttpTimeoutClass) -> Option<Duration> {
        match class {
            HttpTimeoutClass::StandardOperation => self.standard_operation,
            HttpTimeoutClass::LongOperation => self.long_operation,
            HttpTimeoutClass::SseOpen => self.sse_open,
            HttpTimeoutClass::SseErrorBody => self.sse_error_body,
            HttpTimeoutClass::SseIdle => self.sse_idle,
            HttpTimeoutClass::SseLifetime => self.sse_total_lifetime,
        }
    }
}

/// Library logical deadline that expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpTimeoutClass {
    /// Ordinary buffered request deadline.
    StandardOperation,
    /// File or multipart request deadline.
    LongOperation,
    /// Successful SSE response-open deadline.
    SseOpen,
    /// Non-success SSE response-body deadline.
    SseErrorBody,
    /// Established SSE no-progress deadline.
    SseIdle,
    /// Established SSE total-lifetime deadline.
    SseLifetime,
}

impl HttpTimeoutClass {
    pub(crate) const COUNT: usize = 6;

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::StandardOperation => 0,
            Self::LongOperation => 1,
            Self::SseOpen => 2,
            Self::SseErrorBody => 3,
            Self::SseIdle => 4,
            Self::SseLifetime => 5,
        }
    }
}

impl fmt::Display for HttpTimeoutClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::StandardOperation => "standard_operation",
            Self::LongOperation => "long_operation",
            Self::SseOpen => "sse_open",
            Self::SseErrorBody => "sse_error_body",
            Self::SseIdle => "sse_idle",
            Self::SseLifetime => "sse_lifetime",
        })
    }
}

/// Effect of an HTTP timeout on the logical operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeoutOutcome {
    /// A read did not complete.
    ReadAborted,
    /// A mutation may have reached the server and requires fresh observation.
    MutationIndeterminate,
    /// An established stream was terminated.
    StreamTerminated,
}

impl TimeoutOutcome {
    pub(crate) const COUNT: usize = 3;

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::ReadAborted => 0,
            Self::MutationIndeterminate => 1,
            Self::StreamTerminated => 2,
        }
    }
}

impl fmt::Display for TimeoutOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadAborted => "read_aborted",
            Self::MutationIndeterminate => "mutation_indeterminate",
            Self::StreamTerminated => "stream_terminated",
        })
    }
}

pub(crate) fn timeout_outcome(method: &reqwest::Method) -> TimeoutOutcome {
    if matches!(
        *method,
        reqwest::Method::GET | reqwest::Method::HEAD | reqwest::Method::OPTIONS
    ) {
        TimeoutOutcome::ReadAborted
    } else {
        TimeoutOutcome::MutationIndeterminate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_environment_contract_is_exact() {
        assert_eq!(
            HttpTimeoutPolicy::from_environment(None).expect("defaults"),
            HttpTimeoutPolicy::default()
        );
        let disabled = HttpTimeoutPolicy::from_environment(Some(OsString::from("0")))
            .expect("disabled policy");
        assert_eq!(disabled.standard_operation, None);
        assert_eq!(disabled.long_operation, None);
        assert_eq!(disabled.sse_open, None);
        assert_eq!(disabled.sse_error_body, None);
        assert_eq!(disabled.sse_idle, None);
        assert_eq!(disabled.sse_total_lifetime, None);
        let finite = HttpTimeoutPolicy::from_environment(Some(OsString::from("17")))
            .expect("finite policy");
        assert_eq!(finite.standard_operation, Some(Duration::from_secs(17)));
        assert_eq!(finite.long_operation, Some(Duration::from_secs(17)));
    }

    #[test]
    fn inherited_environment_rejects_non_decimal_and_out_of_range_values() {
        for value in ["", " 1", "+1", "-1", "1.0", "3601", "18446744073709551616"] {
            assert!(HttpTimeoutPolicy::from_environment(Some(OsString::from(value))).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn inherited_environment_rejects_non_unicode() {
        use std::os::unix::ffi::OsStringExt;

        assert!(
            HttpTimeoutPolicy::from_environment(Some(OsString::from_vec(vec![0xff]))).is_err()
        );
    }

    #[test]
    fn explicit_policy_rejects_zero_subsecond_and_over_maximum_values() {
        for duration in [
            Duration::ZERO,
            Duration::from_millis(999),
            Duration::from_secs(3_601),
        ] {
            let policy = HttpTimeoutPolicy {
                standard_operation: Some(duration),
                ..HttpTimeoutPolicy::default()
            };
            assert!(policy.validate().is_err());
        }
        let disabled = HttpTimeoutPolicy {
            standard_operation: None,
            ..HttpTimeoutPolicy::default()
        };
        assert!(disabled.validate().is_ok());
    }
}
