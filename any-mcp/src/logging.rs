// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stderr-only diagnostic subscriber setup.

use std::fmt;

use tracing_subscriber::{EnvFilter, filter::Directive};

/// Installs the process-wide diagnostic subscriber with stderr as its writer.
///
/// `RUST_LOG` controls filtering and defaults to `warn`. Callers must continue
/// to reserve stdout for MCP protocol frames.
///
/// # Errors
///
/// Returns a redacted [`LoggingError`] if the filter is invalid or another
/// subscriber has already been installed.
pub fn init() -> Result<(), LoggingError> {
    let filter = match std::env::var("RUST_LOG") {
        Ok(value) => EnvFilter::try_new(value).map_err(|_| LoggingError::InvalidFilter)?,
        Err(std::env::VarError::NotPresent) => EnvFilter::new("warn"),
        Err(std::env::VarError::NotUnicode(_)) => return Err(LoggingError::InvalidFilter),
    };
    // Those dependencies have opt-in debugging paths that can include request
    // or response payloads. Protocol and upstream payloads are never suitable
    // process diagnostics for this stdio server, even when RUST_LOG is broad.
    let filter = filter
        .add_directive(disabled("anytype"))
        .add_directive(disabled("rmcp"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(false)
        .try_init()
        .map_err(|_| LoggingError::AlreadyInitialized)
}

fn disabled(target: &'static str) -> Directive {
    format!("{target}=off")
        .parse()
        .expect("static tracing directive is valid")
}

/// Safe logging initialization failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoggingError {
    /// `RUST_LOG` was not a valid tracing filter.
    InvalidFilter,
    /// A process-wide tracing subscriber was already installed.
    AlreadyInitialized,
}

impl fmt::Display for LoggingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFilter => formatter.write_str("invalid RUST_LOG filter"),
            Self::AlreadyInitialized => {
                formatter.write_str("diagnostic subscriber is already initialized")
            }
        }
    }
}

impl std::error::Error for LoggingError {}
