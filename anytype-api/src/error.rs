//! Errors returned by `AnytypeClient`
//!
use std::path::PathBuf;

#[cfg(feature = "grpc")]
use anytype_rpc::error::AnytypeGrpcError;
use snafu::prelude::*;

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

    /// Client is not authenticated.
    #[snafu(display("Client is not authenticated. Log in first."))]
    Unauthorized,

    /// Client is authenticated, but user does not have proper authorization
    #[snafu(display("Permission denied: User does not have permission to access the object(s)"))]
    Forbidden,

    /// Too many requests occurred. See the anytype rate limit documentation.
    ///
    /// When the anytype server rate limit is exceeded and responds with http 429 status,
    /// the http client in this library throttles requests (to 1 per second)
    /// until the server stops returning errors, or up to `rate_limit_max_retries` times
    /// before giving up and returning this error to the client. The config setting
    /// `rate_limit_max_retries` can be increased to handle arbitrary-sized
    /// bursts, with the result that the app may spend more time waiting.
    /// If `rate_limit_max_retries` is zero, the http client will always wait and retry.
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
    #[cfg(feature = "grpc")]
    #[snafu(display("gRPC error: {source}"))]
    Grpc {
        source: anytype_rpc::error::AnytypeGrpcError,
    },

    /// gRPC auth is unavailable (missing config or account key).
    #[cfg(feature = "grpc")]
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

#[cfg(feature = "grpc")]
impl From<AnytypeGrpcError> for AnytypeError {
    fn from(source: AnytypeGrpcError) -> Self {
        Self::Grpc { source }
    }
}
