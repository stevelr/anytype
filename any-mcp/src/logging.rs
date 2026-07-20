// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stderr-only diagnostic subscriber setup.

use std::fmt;

use tracing::{Metadata, Subscriber};
use tracing_subscriber::{
    EnvFilter, Layer, filter::Directive, fmt as tracing_fmt, fmt::writer::MakeWriter,
    layer::SubscriberExt,
};

const OPERATION_TARGET: &str = "any_mcp::operation";

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
    let configured = match std::env::var("RUST_LOG") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => return Err(LoggingError::InvalidFilter),
    };
    let filter = build_filter(configured.as_deref())?;
    tracing::subscriber::set_global_default(subscriber(filter, std::io::stderr))
        .map_err(|_| LoggingError::AlreadyInitialized)
}

fn build_filter(configured: Option<&str>) -> Result<EnvFilter, LoggingError> {
    let mut filter = EnvFilter::try_new(configured.unwrap_or("warn"))
        .map_err(|_| LoggingError::InvalidFilter)?;
    if !operator_configures_operation(configured.unwrap_or_default()) {
        filter = filter.add_directive(operation_info_directive());
    }
    Ok(filter)
}

fn operator_configures_operation(configured: &str) -> bool {
    configured.split(',').any(|directive| {
        let directive = directive.trim();
        if !directive.is_empty()
            && directive
                .parse::<tracing_subscriber::filter::LevelFilter>()
                .is_ok()
        {
            return true;
        }
        let Some((selector, _level)) = directive.rsplit_once('=') else {
            return false;
        };
        let target = selector
            .trim()
            .split_once('[')
            .map_or(selector.trim(), |(target, _)| target.trim());
        has_target_prefix(OPERATION_TARGET, target)
    })
}

fn operation_info_directive() -> Directive {
    format!("{OPERATION_TARGET}=info")
        .parse()
        .expect("static operation tracing directive is valid")
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
        sync::{Arc, Mutex, MutexGuard, Once},
    };

    use tracing::Dispatch;

    use super::*;

    static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());
    static TRACE_TEST_INTEREST: Once = Once::new();

    fn ensure_trace_interest() {
        TRACE_TEST_INTEREST.call_once(|| {
            let subscriber =
                tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE);
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

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
        ensure_trace_interest();
        let writer = Capture::default();
        let filter = build_filter(Some(filter)).expect("test filter");
        let dispatch = Dispatch::new(subscriber(filter, writer.clone()));
        (dispatch, writer)
    }

    pub(crate) fn capture_default() -> (Dispatch, Capture) {
        ensure_trace_interest();
        let writer = Capture::default();
        let filter = build_filter(None).expect("default test filter");
        let dispatch = Dispatch::new(subscriber(filter, writer.clone()));
        (dispatch, writer)
    }

    pub(crate) fn trace_test_guard() -> MutexGuard<'static, ()> {
        TRACE_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{OPERATION_TARGET, has_target_prefix, test_support::capture};

    #[test]
    fn dependency_payload_targets_cannot_override_metadata_deny_filter() {
        let _guard = super::test_support::trace_test_guard();
        let sentinel = "SECRET_SENTINEL_PAYLOAD";
        let (dispatch, output) = capture(
            "anytype::http=trace,anytype::http_json=trace,rmcp::transport=trace,any_mcp::logging_test=trace",
        );

        tracing::dispatcher::with_default(&dispatch, || {
            tracing::error!(target: "anytype::http", payload = sentinel, "upstream error");
            tracing::error!(target: "anytype::http_json", payload = sentinel, "upstream body");
            tracing::error!(target: "rmcp::transport", payload = sentinel, "protocol frame");
            tracing::info!(target: "any_mcp::logging_test", safe = true, "safe event");
        });

        let output = output.contents();
        assert!(output.contains("safe event"));
        assert!(!output.contains(sentinel));
        assert!(!output.contains("upstream error"));
        assert!(!output.contains("upstream body"));
        assert!(!output.contains("protocol frame"));
    }

    #[test]
    fn operation_diagnostics_are_enabled_by_default_unless_explicitly_overridden() {
        let _guard = super::test_support::trace_test_guard();
        let (default_dispatch, default_output) = super::test_support::capture_default();
        tracing::dispatcher::with_default(&default_dispatch, || {
            tracing::info!(target: OPERATION_TARGET, correlation_id = 1, "operation event");
        });
        assert!(default_output.contents().contains("operation event"));

        let (disabled_dispatch, disabled_output) = capture("error");
        tracing::dispatcher::with_default(&disabled_dispatch, || {
            tracing::info!(target: OPERATION_TARGET, correlation_id = 2, "disabled event");
        });
        assert!(!disabled_output.contents().contains("disabled event"));
    }

    #[test]
    fn deny_filter_matches_only_dependency_target_prefixes() {
        assert!(has_target_prefix("anytype::http_json", "anytype"));
        assert!(has_target_prefix("anytype", "anytype"));
        assert!(has_target_prefix("rmcp::transport", "rmcp"));
        assert!(!has_target_prefix("rmcp_safe", "rmcp"));
    }
}
