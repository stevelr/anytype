// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stderr-only diagnostic subscriber setup.

use std::fmt;

use tracing::{Metadata, Subscriber};
use tracing_subscriber::{
    EnvFilter, Layer, fmt as tracing_fmt, fmt::writer::MakeWriter, layer::SubscriberExt,
};

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
    tracing::subscriber::set_global_default(subscriber(filter, std::io::stderr))
        .map_err(|_| LoggingError::AlreadyInitialized)
}

fn subscriber<W>(filter: EnvFilter, writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let format = tracing_fmt::layer()
        .with_writer(writer)
        .with_target(false)
        .with_ansi(false)
        .with_filter(filter)
        .with_filter(tracing_subscriber::filter::filter_fn(metadata_is_safe));
    tracing_subscriber::registry().with(format)
}

fn metadata_is_safe(metadata: &Metadata<'_>) -> bool {
    !has_target_prefix(metadata.target(), "anytype")
        && !has_target_prefix(metadata.target(), "rmcp")
}

fn has_target_prefix(target: &str, prefix: &str) -> bool {
    target == prefix
        || target
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with("::"))
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use tracing::Dispatch;

    use super::*;

    #[derive(Clone, Default)]
    pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Capture {
        pub(crate) fn contents(&self) -> String {
            String::from_utf8(self.0.lock().expect("capture lock").clone())
                .expect("diagnostics are UTF-8")
        }
    }

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("capture lock")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Capture {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self::Writer {
            self.clone()
        }
    }

    pub(crate) fn capture(filter: &str) -> (Dispatch, Capture) {
        let writer = Capture::default();
        let filter = EnvFilter::try_new(filter).expect("test filter");
        let dispatch = Dispatch::new(subscriber(filter, writer.clone()));
        (dispatch, writer)
    }
}

#[cfg(test)]
mod tests {
    use super::{has_target_prefix, test_support::capture};

    #[test]
    fn dependency_payload_targets_cannot_override_metadata_deny_filter() {
        let sentinel = "SECRET_SENTINEL_PAYLOAD";
        let (dispatch, output) =
            capture("anytype::http_json=trace,rmcp::transport=trace,any_mcp::logging_test=trace");

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::error!(target: "anytype::http_json", payload = sentinel, "upstream body");
            tracing::error!(target: "rmcp::transport", payload = sentinel, "protocol frame");
            tracing::info!(target: "any_mcp::logging_test", safe = true, "safe event");
        });

        let output = output.contents();
        assert!(output.contains("safe event"));
        assert!(!output.contains(sentinel));
        assert!(!output.contains("upstream body"));
        assert!(!output.contains("protocol frame"));
    }

    #[test]
    fn deny_filter_matches_only_dependency_target_prefixes() {
        assert!(has_target_prefix("anytype::http_json", "anytype"));
        assert!(has_target_prefix("anytype", "anytype"));
        assert!(has_target_prefix("rmcp::transport", "rmcp"));
        assert!(!has_target_prefix("rmcp_safe", "rmcp"));
    }
}
