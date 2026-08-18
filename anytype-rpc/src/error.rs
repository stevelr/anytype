//! Errors returned by anytype-rpc gRPC operations.

use std::fmt;

use snafu::prelude::*;

/// Local stream boundary that interrupted a control mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcControlBoundaryKind {
    /// The decoded event queue reached its configured capacity.
    QueueSaturated,
    /// The session event stream closed before a terminal mutation result.
    StreamClosed,
    /// The session event transport failed before a terminal mutation result.
    TransportLost,
}

impl fmt::Display for GrpcControlBoundaryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::QueueSaturated => "queue_saturated",
            Self::StreamClosed => "stream_closed",
            Self::TransportLost => "transport_lost",
        })
    }
}

/// Unified error type for anytype-rpc gRPC operations.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum AnytypeGrpcError {
    /// Authentication error.
    #[snafu(display("gRPC authentication failed (details redacted)"))]
    Auth { source: AuthError },

    /// Configuration error.
    #[snafu(display("gRPC configuration failed (details redacted)"))]
    Config { source: ConfigError },

    /// View operation error.
    #[snafu(display("gRPC view operation failed (details redacted)"))]
    View { source: ViewError },

    /// Space backup operation error.
    #[snafu(display("gRPC backup operation failed (details redacted)"))]
    Backup { source: BackupError },

    /// gRPC transport connection error.
    #[snafu(display("gRPC transport failed (details redacted)"))]
    Transport {
        #[snafu(source(false))]
        source: tonic::transport::Error,
    },

    /// Logical gRPC deadline configuration error.
    #[snafu(
        context(suffix(GrpcTimeoutConfigSnafu)),
        display("gRPC deadline configuration error: {source}")
    )]
    TimeoutConfig {
        source: crate::deadline::GrpcTimeoutConfigError,
    },

    /// Stable, payload-free logical gRPC deadline expiration.
    #[snafu(context(suffix(GrpcDeadlineSnafu)), display("{source}"))]
    Deadline {
        source: crate::deadline::GrpcDeadlineError,
    },

    /// A local session-stream boundary interrupted a control mutation.
    #[snafu(display("gRPC control boundary kind={kind} outcome={outcome}"))]
    ControlBoundary {
        /// Closed boundary classification without peer-provided text.
        kind: GrpcControlBoundaryKind,
        /// Whether the control future could have dispatched before interruption.
        outcome: crate::deadline::GrpcTimeoutOutcome,
    },
}

/// Errors from authentication operations.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum AuthError {
    /// gRPC status error from a request.
    #[snafu(display("gRPC auth request failed with code {}", source.code()))]
    Status {
        #[snafu(source(false))]
        source: tonic::Status,
    },

    /// Anytype API returned an error response.
    #[snafu(display("Anytype auth API error ({code}; description redacted)"))]
    Api { code: i32, description: String },

    /// Create session returned an empty token.
    #[snafu(display("Create session returned empty token"))]
    EmptyToken,

    /// Invalid metadata value for auth token.
    #[snafu(display("invalid authentication metadata (details redacted)"))]
    InvalidMetadata {
        #[snafu(source(false))]
        source: tonic::metadata::errors::InvalidMetadataValue,
    },

    /// Logical deadline policy could not be resolved.
    #[snafu(
        context(suffix(AuthTimeoutConfigSnafu)),
        display("gRPC deadline configuration error: {source}")
    )]
    TimeoutConfig {
        source: crate::deadline::GrpcTimeoutConfigError,
    },

    /// Credential setup reached its logical deadline.
    #[snafu(context(suffix(AuthDeadlineSnafu)), display("{source}"))]
    Deadline {
        source: crate::deadline::GrpcDeadlineError,
    },
}

/// Errors from configuration operations.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum ConfigError {
    /// Config file I/O error.
    #[snafu(display("configuration I/O failed (details redacted)"))]
    Io {
        #[snafu(source(false))]
        source: std::io::Error,
    },

    /// Config file parse error.
    #[snafu(display("configuration parsing failed (details redacted)"))]
    Parse {
        #[snafu(source(false))]
        source: serde_json::Error,
    },

    /// Home-directory environment variables are unavailable.
    #[snafu(display("home directory environment variable not set"))]
    MissingHome,
}

/// Errors from view operations.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum ViewError {
    /// Authentication token attachment failed before request dispatch.
    #[snafu(
        context(suffix(ViewSnafu)),
        display("view authentication failed (details redacted)")
    )]
    Auth { source: AuthError },

    /// gRPC status error from a request.
    #[snafu(display("view gRPC request failed with code {}", source.code()))]
    Rpc {
        #[snafu(source(false))]
        source: tonic::Status,
    },

    /// Anytype API returned an error response.
    #[snafu(display("Anytype view API error ({code}; description redacted)"))]
    ApiResponse { code: i32, description: String },

    /// Object view missing in response.
    #[snafu(display("Object view missing in response"))]
    MissingObjectView,

    /// Dataview block not found for view id.
    #[snafu(display("dataview block not found (view id redacted)"))]
    MissingDataviewBlock { view_id: String },

    /// View id not found.
    #[snafu(display("view not found (id redacted)"))]
    MissingView { view_id: String },

    /// View type not supported.
    #[snafu(display("view is not supported (id redacted; type {actual})"))]
    NotSupportedView { view_id: String, actual: i32 },
}

/// Errors from space backup operations.
#[derive(Snafu)]
#[snafu(visibility(pub))]
pub enum BackupError {
    /// gRPC status error from a request.
    #[snafu(display("backup gRPC request failed with code {}", source.code()))]
    BackupRpc {
        #[snafu(source(false))]
        source: tonic::Status,
    },

    /// Anytype API returned an error response.
    #[snafu(display("Anytype backup API error ({code}; description redacted)"))]
    BackupApiResponse { code: i32, description: String },

    /// Authentication token metadata was invalid.
    #[snafu(display("backup authentication failed (details redacted)"))]
    BackupAuth { source: AuthError },

    /// Backup options were invalid.
    #[snafu(display("invalid backup options (details redacted)"))]
    InvalidOptions { message: String },

    /// Failed to resolve the friendly name for a space.
    #[snafu(display("failed to resolve backup space name (details redacted)"))]
    SpaceNameLookup { space_id: String, message: String },

    /// Server response did not include an export path.
    #[snafu(display("Backup response missing export path"))]
    MissingExportPath,

    /// Failed to create or access a local path.
    #[snafu(display("backup I/O failed (details redacted)"))]
    BackupIo {
        path: std::path::PathBuf,
        #[snafu(source(false))]
        source: std::io::Error,
    },

    /// Failed to move generated backup to its final target path.
    #[snafu(display("moving backup output failed (details redacted)"))]
    BackupMove {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
        #[snafu(source(false))]
        source: std::io::Error,
    },

    /// A backup gRPC operation reached its logical deadline.
    #[snafu(context(suffix(BackupDeadlineSnafu)), display("{source}"))]
    Deadline {
        source: crate::deadline::GrpcDeadlineError,
    },
}

macro_rules! impl_redacted_debug {
    ($($error:ty),+ $(,)?) => {
        $(
            impl fmt::Debug for $error {
                fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                    fmt::Display::fmt(self, formatter)
                }
            }
        )+
    };
}

impl_redacted_debug!(
    AnytypeGrpcError,
    AuthError,
    ConfigError,
    ViewError,
    BackupError,
);

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

impl From<crate::deadline::GrpcTimeoutConfigError> for AuthError {
    fn from(source: crate::deadline::GrpcTimeoutConfigError) -> Self {
        AuthError::TimeoutConfig { source }
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
impl From<AuthError> for ViewError {
    fn from(source: AuthError) -> Self {
        ViewError::Auth { source }
    }
}

impl From<tonic::Status> for ViewError {
    fn from(source: tonic::Status) -> Self {
        ViewError::Rpc { source }
    }
}

// From impls for BackupError
impl From<tonic::Status> for BackupError {
    fn from(source: tonic::Status) -> Self {
        BackupError::BackupRpc { source }
    }
}

impl From<AuthError> for BackupError {
    fn from(source: AuthError) -> Self {
        BackupError::BackupAuth { source }
    }
}

// From impls for AnytypeGrpcError
impl From<AuthError> for AnytypeGrpcError {
    fn from(source: AuthError) -> Self {
        match source {
            AuthError::TimeoutConfig { source } => AnytypeGrpcError::TimeoutConfig { source },
            AuthError::Deadline { source } => AnytypeGrpcError::Deadline { source },
            source => AnytypeGrpcError::Auth { source },
        }
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

impl From<BackupError> for AnytypeGrpcError {
    fn from(source: BackupError) -> Self {
        match source {
            BackupError::Deadline { source } => AnytypeGrpcError::Deadline { source },
            source => AnytypeGrpcError::Backup { source },
        }
    }
}

impl From<tonic::transport::Error> for AnytypeGrpcError {
    fn from(source: tonic::transport::Error) -> Self {
        AnytypeGrpcError::Transport { source }
    }
}

impl From<crate::deadline::GrpcTimeoutConfigError> for AnytypeGrpcError {
    fn from(source: crate::deadline::GrpcTimeoutConfigError) -> Self {
        AnytypeGrpcError::TimeoutConfig { source }
    }
}

impl From<crate::deadline::GrpcDeadlineError> for AnytypeGrpcError {
    fn from(source: crate::deadline::GrpcDeadlineError) -> Self {
        AnytypeGrpcError::Deadline { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "HOSTILE_RPC_PEER_SECRET";

    fn assert_redacted<E>(error: &E)
    where
        E: std::error::Error,
    {
        assert!(!error.to_string().contains(SECRET));
        assert!(!format!("{error:?}").contains(SECRET));
        let mut source = error.source();
        while let Some(current) = source {
            assert!(!current.to_string().contains(SECRET));
            assert!(!format!("{current:?}").contains(SECRET));
            source = current.source();
        }
    }

    #[test]
    fn public_rpc_error_families_redact_peer_controlled_details_and_sources() {
        let auth_status = AuthError::Status {
            source: tonic::Status::internal(SECRET),
        };
        assert_redacted(&auth_status);
        assert_redacted(&AuthError::Api {
            code: 500,
            description: SECRET.to_owned(),
        });

        let config = ConfigError::Io {
            source: std::io::Error::other(SECRET),
        };
        assert_redacted(&config);

        let view_status = ViewError::Rpc {
            source: tonic::Status::invalid_argument(SECRET),
        };
        assert_redacted(&view_status);
        assert_redacted(&ViewError::ApiResponse {
            code: 400,
            description: SECRET.to_owned(),
        });
        assert_redacted(&ViewError::MissingView {
            view_id: SECRET.to_owned(),
        });

        let backup_status = BackupError::BackupRpc {
            source: tonic::Status::unavailable(SECRET),
        };
        assert_redacted(&backup_status);
        assert_redacted(&BackupError::BackupApiResponse {
            code: 503,
            description: SECRET.to_owned(),
        });
        assert_redacted(&BackupError::SpaceNameLookup {
            space_id: SECRET.to_owned(),
            message: SECRET.to_owned(),
        });

        assert_redacted(&AnytypeGrpcError::Auth {
            source: AuthError::Api {
                code: 500,
                description: SECRET.to_owned(),
            },
        });
        let transport = tonic::transport::Endpoint::from_shared(format!("not {SECRET}"))
            .expect_err("hostile endpoint is invalid");
        assert_redacted(&AnytypeGrpcError::Transport { source: transport });
        assert_redacted(&AnytypeGrpcError::ControlBoundary {
            kind: GrpcControlBoundaryKind::QueueSaturated,
            outcome: crate::deadline::GrpcTimeoutOutcome::MutationIndeterminate,
        });
    }
}
