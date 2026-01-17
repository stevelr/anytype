//! Errors returned by anytype-rpc gRPC operations.

use snafu::prelude::*;

/// Unified error type for anytype-rpc gRPC operations.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum AnytypeGrpcError {
    /// Authentication error.
    #[snafu(display("Auth error: {source}"))]
    Auth { source: AuthError },

    /// Configuration error.
    #[snafu(display("Config error: {source}"))]
    Config { source: ConfigError },

    /// View operation error.
    #[snafu(display("View error: {source}"))]
    View { source: ViewError },

    /// gRPC transport connection error.
    #[snafu(display("Transport error: {source}"))]
    Transport { source: tonic::transport::Error },
}

/// Errors from authentication operations.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum AuthError {
    /// gRPC status error from a request.
    #[snafu(display("Transport error: {source}"))]
    Status { source: tonic::Status },

    /// Anytype API returned an error response.
    #[snafu(display("API error ({code}): {description}"))]
    Api { code: i32, description: String },

    /// Create session returned an empty token.
    #[snafu(display("Create session returned empty token"))]
    EmptyToken,

    /// Invalid metadata value for auth token.
    #[snafu(display("Invalid metadata value: {source}"))]
    InvalidMetadata {
        source: tonic::metadata::errors::InvalidMetadataValue,
    },
}

/// Errors from configuration operations.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ConfigError {
    /// Config file I/O error.
    #[snafu(display("I/O error: {source}"))]
    Io { source: std::io::Error },

    /// Config file parse error.
    #[snafu(display("Parse error: {source}"))]
    Parse { source: serde_json::Error },

    /// HOME environment variable not set.
    #[snafu(display("HOME environment variable not set"))]
    MissingHome,
}

/// Errors from view operations.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub))]
pub enum ViewError {
    /// gRPC status error from a request.
    #[snafu(display("Transport error: {source}"))]
    Rpc { source: tonic::Status },

    /// Anytype API returned an error response.
    #[snafu(display("API error ({code}): {description}"))]
    ApiResponse { code: i32, description: String },

    /// Object view missing in response.
    #[snafu(display("Object view missing in response"))]
    MissingObjectView,

    /// Dataview block not found for view id.
    #[snafu(display("Dataview block not found for view id {view_id}"))]
    MissingDataviewBlock { view_id: String },

    /// View id not found.
    #[snafu(display("View id {view_id} not found"))]
    MissingView { view_id: String },

    /// View type not supported.
    #[snafu(display("View id {view_id} is not a supported view (type {actual})"))]
    NotSupportedView { view_id: String, actual: i32 },
}

// From impls for AuthError
impl From<tonic::Status> for AuthError {
    fn from(source: tonic::Status) -> Self {
        AuthError::Status { source }
    }
}

impl From<tonic::metadata::errors::InvalidMetadataValue> for AuthError {
    fn from(source: tonic::metadata::errors::InvalidMetadataValue) -> Self {
        AuthError::InvalidMetadata { source }
    }
}

// From impls for ConfigError
impl From<std::io::Error> for ConfigError {
    fn from(source: std::io::Error) -> Self {
        ConfigError::Io { source }
    }
}

impl From<serde_json::Error> for ConfigError {
    fn from(source: serde_json::Error) -> Self {
        ConfigError::Parse { source }
    }
}

// From impls for ViewError
impl From<tonic::Status> for ViewError {
    fn from(source: tonic::Status) -> Self {
        ViewError::Rpc { source }
    }
}

// From impls for AnytypeGrpcError
impl From<AuthError> for AnytypeGrpcError {
    fn from(source: AuthError) -> Self {
        AnytypeGrpcError::Auth { source }
    }
}

impl From<ConfigError> for AnytypeGrpcError {
    fn from(source: ConfigError) -> Self {
        AnytypeGrpcError::Config { source }
    }
}

impl From<ViewError> for AnytypeGrpcError {
    fn from(source: ViewError) -> Self {
        AnytypeGrpcError::View { source }
    }
}

impl From<tonic::transport::Error> for AnytypeGrpcError {
    fn from(source: tonic::transport::Error) -> Self {
        AnytypeGrpcError::Transport { source }
    }
}
