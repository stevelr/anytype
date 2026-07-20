//! Errors returned by `AnytypeClient`
//!
use std::path::PathBuf;

use anytype_rpc::error::AnytypeGrpcError;
use snafu::prelude::*;

use crate::resolve::ResolveCandidate;

/// Errors returned by anytype crate
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum AnytypeError {
    // Http connection or timeout error
    #[snafu(display("HTTP error {method} url:{url}"))]
    Http {
        method: String,
        url: String,
        source: reqwest::Error,
    },

    /// Anytype Server responded with error.
    /// This error usually means the request was invalid, or there was an internal server error.
    #[snafu(display("Api Server reported error ({code}) {method} {url}: {message}"))]
    ApiError {
        code: u16,
        method: String,
        url: String,
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

    /// Authorization error
    #[snafu(display("Authentication failed: {message}"))]
    Auth { message: String },

    /// Deserialization error. This means we didn't deserialize a server response correctly.
    /// If you see this error, please report it as a bug.
    #[snafu(display("Deserialization: {source}"))]
    Deserialization { source: serde_json::Error },

    /// Serialization error. unlikely to occur. If you see this error, please report it as a bug.
    #[snafu(display("Serialization: {source}"))]
    Serialization { source: serde_json::Error },

    /// Expected item was not found. Returned for any object get by id,
    /// or property or type lookup by unique key, or tag lookup by property and name.
    #[snafu(display("{obj_type} {key} not found"))]
    NotFound { obj_type: String, key: String },

    /// A name matched more than one item. Returned by the `resolve_*` helpers
    /// (see the [`resolve`](crate::resolve) module) when a space, type, chat,
    /// or view name is not unique in its scope. Use the id (or, for types,
    /// the `@key` form) to disambiguate.
    #[snafu(display("{obj_type} name is ambiguous: {key}"))]
    Ambiguous {
        obj_type: String,
        key: String,
        /// Deterministically ordered, deduplicated alternatives that callers
        /// can present when asking the user to disambiguate.
        candidates: Vec<ResolveCandidate>,
    },

    /// A resolver could not prove a unique or missing result within its hard
    /// upstream scan bound. Retry with an id or an explicit unique key.
    #[snafu(display("{obj_type} resolution exceeded the {limit}-item scan limit: {key}"))]
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
    #[snafu(display("Rate limit exceeded: \"{header}\" (parsed wait_time: {} secs)"))]
    RateLimitExceeded {
        header: String,
        duration: std::time::Duration,
    },

    /// Validation error: an internal parameter validation check failed.
    #[snafu(display("Validation error: {message}"))]
    Validation { message: String },

    /// A `KeyStore` has not been configured.
    /// This is an `AnytypeError` rather than a `KeyStoreError`, because it is a client configuration error
    #[snafu(display("No configured keystore"))]
    NoKeyStore,

    /// gRPC auth or transport error.
    #[snafu(display("gRPC error: {source}"))]
    Grpc {
        source: anytype_rpc::error::AnytypeGrpcError,
    },

    /// gRPC auth is unavailable (missing config or account key).
    #[snafu(display("gRPC service unavailable: {message}"))]
    GrpcUnavailable { message: String },

    /// Error encountered by the configured `KeyStore`.
    #[snafu(display("KeyStore: {source}"))]
    KeyStore { source: KeyStoreError },

    /// A function requiring the cache failed because the cache is disabled.
    #[snafu(display("Operation requires cache to be enabled"))]
    CacheDisabled,

    /// The previous operation could not be confirmed within the expected time interval.
    /// For more information, see the notes about eventual consistency in the project [README](../README.md).
    #[snafu(display(
        "Verify timeout for {obj_type} {key} after {attempts} attempts in {timeout:?}"
    ))]
    VerifyTimeout {
        obj_type: String,
        key: String,
        attempts: usize,
        timeout: std::time::Duration,
        last_error: Option<String>,
    },

    /// Some other error occurred
    #[snafu(display("{message}"))]
    Other { message: String },
}

impl AnytypeError {
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
