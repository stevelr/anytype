//! Errors returned by `AnytypeClient`
//!
use std::{fmt, path::PathBuf};

use anytype_rpc::error::AnytypeGrpcError;
use snafu::prelude::*;

use crate::resolve::ResolveCandidate;

/// Errors returned by the Anytype crate.
///
/// `Display`, `Debug`, and the standard error source chain intentionally omit
/// every free-form string and typed upstream source that could contain request
/// or document content. Match the public variants and fields when an
/// application explicitly needs raw values; use [`AnytypeError::diagnostic`]
/// for ordinary logs and telemetry. Error text is therefore
/// classification-oriented and is not a stable parsing contract.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum AnytypeError {
    // Http connection or timeout error
    #[snafu(display(
        "HTTP transport error {} path:{}",
        diagnostic_method(method),
        crate::http_client::diagnostic_path(url)
    ))]
    Http {
        method: String,
        /// Original request target retained for programmatic inspection.
        /// Use [`AnytypeError::diagnostic`] before logging it.
        url: String,
        /// Raw transport source retained for explicit programmatic matching.
        /// It is omitted from standard formatting and the error source chain.
        #[snafu(source(false))]
        source: reqwest::Error,
    },

    /// Anytype Server responded with error.
    /// This error usually means the request was invalid, or there was an internal server error.
    #[snafu(display(
        "Anytype API error status={code} {} path:{} (upstream payload redacted)",
        diagnostic_method(method),
        crate::http_client::diagnostic_path(url)
    ))]
    ApiError {
        code: u16,
        method: String,
        /// Original request target retained for programmatic inspection.
        /// Use [`AnytypeError::diagnostic`] before logging it.
        url: String,
        /// Bounded upstream response text for explicit programmatic handling.
        ///
        /// This may contain document data or credentials supplied by an
        /// untrusted server. It is deliberately omitted from `Display`,
        /// `Debug`, and [`AnytypeError::diagnostic`].
        message: String,
    },

    /// A buffered HTTP response exceeded its configured byte ceiling.
    ///
    /// The error intentionally contains no response body, URL, request body,
    /// or credential-bearing value, so callers may classify it safely.
    #[snafu(display("HTTP response exceeds the configured {limit}-byte limit"))]
    ResponseTooLarge {
        /// Maximum response bytes permitted for this operation.
        limit: u64,
        /// Server-declared length when one was available.
        declared: Option<u64>,
    },

    /// Encountered server error on "retryable" request, but all retry attempts failed.
    #[snafu(display("server api request: failed {n} times"))]
    TooManyRetries { n: u32 },

    /// Authorization error.
    ///
    /// The raw message remains available for explicit programmatic matching,
    /// but standard formatting omits it because it may contain request data.
    #[snafu(display("Authentication failed (details redacted)"))]
    Auth { message: String },

    /// Deserialization error. This means we didn't deserialize a server response correctly.
    /// If you see this error, please report it as a bug.
    #[snafu(display(
        "Deserialization error at line {} column {}",
        source.line(),
        source.column()
    ))]
    Deserialization {
        #[snafu(source(false))]
        source: serde_json::Error,
    },

    /// Serialization error. unlikely to occur. If you see this error, please report it as a bug.
    #[snafu(display(
        "Serialization error at line {} column {}",
        source.line(),
        source.column()
    ))]
    Serialization {
        #[snafu(source(false))]
        source: serde_json::Error,
    },

    /// Expected item was not found. Returned for any object get by id,
    /// or property or type lookup by unique key, or tag lookup by property and name.
    #[snafu(display("Requested Anytype item was not found (identity redacted)"))]
    NotFound { obj_type: String, key: String },

    /// A name matched more than one item. Returned by the `resolve_*` helpers
    /// (see the [`resolve`](crate::resolve) module) when a space, type, chat,
    /// or view name is not unique in its scope. Use the id (or, for types,
    /// the `@key` form) to disambiguate.
    #[snafu(display("Anytype item name is ambiguous (identity redacted)"))]
    Ambiguous {
        obj_type: String,
        key: String,
        /// Deterministically ordered, deduplicated alternatives that callers
        /// can present when asking the user to disambiguate.
        candidates: Vec<ResolveCandidate>,
    },

    /// A resolver could not prove a unique or missing result within its hard
    /// upstream scan bound. Retry with an id or an explicit unique key.
    #[snafu(display(
        "Anytype item resolution exceeded the {limit}-item scan limit (identity redacted)"
    ))]
    ResolutionLimitExceeded {
        obj_type: String,
        key: String,
        limit: usize,
    },

    /// Client is not authenticated.
    #[snafu(display("Client is not authenticated. Log in first."))]
    Unauthorized,

    /// Client is authenticated, but user does not have proper authorization
    #[snafu(display("Permission denied: User does not have permission to access the object(s)"))]
    Forbidden,

    /// Too many requests occurred. See the anytype rate limit documentation.
    ///
    /// When the Anytype server responds with HTTP 429, the HTTP client
    /// throttles and retries only replay-safe methods
    /// until the server stops returning errors, or up to `rate_limit_max_retries` times
    /// before giving up and returning this error to the client. The config setting
    /// `rate_limit_max_retries` can be increased to handle arbitrary-sized
    /// bursts, with the result that the app may spend more time waiting.
    /// If `rate_limit_max_retries` is zero, replay-safe requests wait and retry
    /// without a retry-count cap. Non-idempotent mutation requests are never
    /// replayed automatically and instead return their original 429 failure.
    #[snafu(display(
        "Rate limit exceeded (parsed wait_time: {} secs; upstream header redacted)",
        duration.as_secs()
    ))]
    RateLimitExceeded {
        /// Raw bounded header value retained for explicit programmatic use.
        /// It is omitted from all standard diagnostics.
        header: String,
        duration: std::time::Duration,
    },

    /// Validation error: an internal parameter validation check failed.
    ///
    /// The raw message remains available for explicit programmatic matching,
    /// but standard formatting omits it because validation context can contain
    /// request or document data.
    #[snafu(display("Validation error (details redacted)"))]
    Validation { message: String },

    /// A `KeyStore` has not been configured.
    /// This is an `AnytypeError` rather than a `KeyStoreError`, because it is a client configuration error
    #[snafu(display("No configured keystore"))]
    NoKeyStore,

    /// gRPC auth or transport error.
    ///
    /// The typed source remains available for explicit programmatic matching,
    /// but standard formatting and the error source chain omit it because
    /// upstream statuses can contain response or request data.
    #[snafu(display("gRPC error (details redacted)"))]
    Grpc {
        #[snafu(source(false))]
        source: anytype_rpc::error::AnytypeGrpcError,
    },

    /// gRPC auth is unavailable (missing config or account key).
    #[snafu(display("gRPC service unavailable (details redacted)"))]
    GrpcUnavailable { message: String },

    /// Error encountered by the configured `KeyStore`.
    ///
    /// The typed source remains available for explicit programmatic matching,
    /// but standard formatting and the error source chain omit it because it
    /// can contain paths, environment names, or backend error text.
    #[snafu(display("KeyStore error (details redacted)"))]
    KeyStore {
        #[snafu(source(false))]
        source: KeyStoreError,
    },

    /// A function requiring the cache failed because the cache is disabled.
    #[snafu(display("Operation requires cache to be enabled"))]
    CacheDisabled,

    /// The previous operation could not be confirmed within the expected time interval.
    /// For more information, see the notes about eventual consistency in the project [README](../README.md).
    #[snafu(display(
        "Verify timeout after {attempts} attempts in {timeout:?} (identity and last error redacted)"
    ))]
    VerifyTimeout {
        obj_type: String,
        key: String,
        attempts: usize,
        timeout: std::time::Duration,
        last_error: Option<String>,
    },

    /// Some other error occurred.
    ///
    /// The raw message remains available for explicit programmatic matching,
    /// but standard formatting omits it because callers may have included
    /// request or document data.
    #[snafu(display("Anytype error (details redacted)"))]
    Other { message: String },
}

fn diagnostic_method(method: &str) -> &str {
    if !method.is_empty()
        && method.len() <= 16
        && method
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
    {
        method
    } else {
        "unknown"
    }
}

/// Structured, secret-safe classification of an [`AnytypeError`].
///
/// This value intentionally excludes upstream response text, headers,
/// credential-bearing URLs, request bodies, document bodies, and underlying
/// error strings. It is safe to pass to ordinary application diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnytypeDiagnostic {
    /// Stable error variant name.
    pub variant: &'static str,
    /// HTTP response status when the variant carries one.
    pub status: Option<u16>,
    /// Validated HTTP method when available.
    pub method: Option<String>,
    /// Bounded path-only request context when available.
    pub path: Option<String>,
}

impl fmt::Display for AnytypeDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "variant={}", self.variant)?;
        if let Some(status) = self.status {
            write!(formatter, " status={status}")?;
        }
        if let Some(method) = self.method.as_deref() {
            write!(formatter, " method={method}")?;
        }
        if let Some(path) = self.path.as_deref() {
            write!(formatter, " path={path}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for AnytypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("AnytypeError")
            .field(&self.diagnostic())
            .finish()
    }
}

impl AnytypeError {
    /// Returns structured diagnostic context with all payload-bearing fields
    /// removed and every request target reduced to a bounded path.
    #[must_use]
    pub fn diagnostic(&self) -> AnytypeDiagnostic {
        let (variant, status, method, path) = match self {
            Self::Http { method, url, .. } => (
                "http_transport",
                None,
                Some(diagnostic_method(method).to_owned()),
                Some(crate::http_client::diagnostic_path(url)),
            ),
            Self::ApiError {
                code, method, url, ..
            } => (
                "api_error",
                Some(*code),
                Some(diagnostic_method(method).to_owned()),
                Some(crate::http_client::diagnostic_path(url)),
            ),
            Self::ResponseTooLarge { .. } => ("response_too_large", None, None, None),
            Self::TooManyRetries { .. } => ("too_many_retries", None, None, None),
            Self::Auth { .. } => ("auth", None, None, None),
            Self::Deserialization { .. } => ("deserialization", None, None, None),
            Self::Serialization { .. } => ("serialization", None, None, None),
            Self::NotFound { .. } => ("not_found", None, None, None),
            Self::Ambiguous { .. } => ("ambiguous", None, None, None),
            Self::ResolutionLimitExceeded { .. } => ("resolution_limit_exceeded", None, None, None),
            Self::Unauthorized => ("unauthorized", Some(401), None, None),
            Self::Forbidden => ("forbidden", Some(403), None, None),
            Self::RateLimitExceeded { .. } => ("rate_limit", Some(429), None, None),
            Self::Validation { .. } => ("validation", None, None, None),
            Self::NoKeyStore => ("no_keystore", None, None, None),
            Self::Grpc { .. } => ("grpc", None, None, None),
            Self::GrpcUnavailable { .. } => ("grpc_unavailable", None, None, None),
            Self::KeyStore { .. } => ("keystore", None, None, None),
            Self::CacheDisabled => ("cache_disabled", None, None, None),
            Self::VerifyTimeout { .. } => ("verify_timeout", None, None, None),
            Self::Other { .. } => ("other", None, None, None),
        };
        AnytypeDiagnostic {
            variant,
            status,
            method,
            path,
        }
    }

    /// Returns bounded alternatives for an ambiguous resolver lookup.
    ///
    /// The slice contains at most
    /// [`MAX_RESOLVE_CANDIDATES`](crate::resolve::MAX_RESOLVE_CANDIDATES)
    /// entries. Other error variants return `None`.
    #[must_use]
    pub fn resolve_candidates(&self) -> Option<&[ResolveCandidate]> {
        match self {
            Self::Ambiguous { candidates, .. } => Some(candidates),
            _ => None,
        }
    }
}

/// Errors arising from `KeyStore`
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum KeyStoreError {
    /// Problem accessing the key file
    #[snafu(display("keystore file {path:?} {source}"))]
    File {
        //message: String,
        path: PathBuf,
        source: std::io::Error,
    },

    /// Problem accessing OS keyring
    #[snafu(display("keyring error {source}"))]
    Keyring {
        //service: Option<String>,
        //user: Option<String>,
        source: keyring_core::Error,
    },

    /// Required environment variable undefined
    #[snafu(display("file keystore expects environment variable {var}"))]
    FileEnv {
        var: String,
        source: std::env::VarError,
    },

    #[snafu(display("keystore configuration error"))]
    Config { message: String },

    /// Other error type - can be used by external implementations
    #[snafu(display("keystore {message}"))]
    External { message: String },
}

impl From<keyring_core::Error> for KeyStoreError {
    fn from(source: keyring_core::Error) -> Self {
        Self::Keyring { source }
    }
}

impl From<KeyStoreError> for AnytypeError {
    fn from(source: KeyStoreError) -> Self {
        Self::KeyStore { source }
    }
}

impl From<AnytypeGrpcError> for AnytypeError {
    fn from(source: AnytypeGrpcError) -> Self {
        Self::Grpc { source }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anytype_rpc::error::{AnytypeGrpcError, AuthError};

    use super::{AnytypeError, KeyStoreError};
    use crate::resolve::ResolveCandidate;

    const SECRET: &str = "STANDARD_DISPLAY_DOCUMENT_SECRET";

    #[test]
    fn all_raw_bearing_error_variants_keep_fields_but_redact_standard_diagnostics() {
        let errors = vec![
            AnytypeError::Auth {
                message: SECRET.to_owned(),
            },
            AnytypeError::NotFound {
                obj_type: SECRET.to_owned(),
                key: SECRET.to_owned(),
            },
            AnytypeError::Ambiguous {
                obj_type: SECRET.to_owned(),
                key: SECRET.to_owned(),
                candidates: vec![ResolveCandidate::new(SECRET, SECRET)],
            },
            AnytypeError::ResolutionLimitExceeded {
                obj_type: SECRET.to_owned(),
                key: SECRET.to_owned(),
                limit: 37,
            },
            AnytypeError::Validation {
                message: SECRET.to_owned(),
            },
            AnytypeError::Grpc {
                source: AnytypeGrpcError::Auth {
                    source: AuthError::Api {
                        code: 500,
                        description: SECRET.to_owned(),
                    },
                },
            },
            AnytypeError::GrpcUnavailable {
                message: SECRET.to_owned(),
            },
            AnytypeError::KeyStore {
                source: KeyStoreError::External {
                    message: SECRET.to_owned(),
                },
            },
            AnytypeError::VerifyTimeout {
                obj_type: SECRET.to_owned(),
                key: SECRET.to_owned(),
                attempts: 4,
                timeout: Duration::from_secs(2),
                last_error: Some(SECRET.to_owned()),
            },
            AnytypeError::Other {
                message: SECRET.to_owned(),
            },
        ];

        let AnytypeError::Auth { message } = &errors[0] else {
            panic!("raw auth fixture changed variant");
        };
        assert_eq!(
            message, SECRET,
            "raw fields remain programmatically available"
        );

        for error in errors {
            let mut diagnostics = format!("{error} {error:?} {}", error.diagnostic());
            let mut source = std::error::Error::source(&error);
            while let Some(current) = source {
                diagnostics.push_str(&format!(" {current} {current:?}"));
                source = current.source();
            }
            assert!(
                !diagnostics.contains(SECRET),
                "standard diagnostics exposed raw variant data: {diagnostics}"
            );
        }
    }
}
