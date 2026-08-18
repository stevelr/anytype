// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Explicitly selected Streamable HTTP transport.
//!
//! Stdio remains the production default. `ANY_MCP_TRANSPORT=streamable-http`
//! opts one process into the authenticated loopback HTTP listener described in
//! the approved transport design. This module owns every HTTP concern:
//! configuration, secret handling, request admission, authentication,
//! sessions, and the listener. Domain handlers and tool schemas are shared
//! with stdio unchanged and never observe raw headers or bearer values.

use std::{fmt, sync::Arc, time::Duration};

use tokio_util::sync::CancellationToken;

pub(crate) mod admission;
pub(crate) mod auth;
pub mod config;
pub(crate) mod listener;
#[cfg(test)]
mod load_tests;
pub mod oauth;
pub(crate) mod preview;
#[cfg(test)]
mod process_tests;
pub mod secret;
pub(crate) mod session;
#[cfg(test)]
mod stream_tests;

pub use config::{HttpAuthConfig, HttpConfig, HttpConfigError, TransportSelection};
pub use secret::{StaticToken, StaticTokenError};

use crate::{RuntimeContext, config::ProtocolMode};

/// Validated authentication material loaded before Anytype startup probes.
///
/// The startup order is fixed: configuration parsing, then this credential
/// material (token file safety checks or OAuth metadata/JWKS retrieval),
/// then the authenticated Anytype runtime, then the listener bind.
pub enum HttpAuthMaterial {
    /// One loaded and validated static bearer token.
    StaticToken(StaticToken),
    /// One started OAuth resource-server validator with a warm JWKS.
    OAuth(Box<oauth::OAuthValidator>),
}

/// A safe HTTP transport failure without secrets, values, or addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpTransportError {
    /// The static token file failed a safety or grammar check.
    StaticToken(StaticTokenError),
    /// OAuth metadata/JWKS retrieval or validation failed.
    Jwks(oauth::JwksError),
    /// The fixed catalog could not be constructed.
    Catalog,
    /// The loopback listener could not be bound.
    Bind,
    /// The listener service failed fatally.
    Listener,
}

impl fmt::Display for HttpTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaticToken(error) => error.fmt(formatter),
            Self::Jwks(error) => error.fmt(formatter),
            Self::Catalog => formatter.write_str("MCP static catalog construction failed"),
            Self::Bind => formatter.write_str("HTTP listener bind failed"),
            Self::Listener => formatter.write_str("HTTP listener service failed"),
        }
    }
}

impl std::error::Error for HttpTransportError {}

/// Loads and validates authentication material for one HTTP process.
///
/// Runs before Anytype credential access or probes so an unauthenticated or
/// misconfigured HTTP deployment fails without touching the keystore.
///
/// # Errors
///
/// Returns a fixed [`HttpTransportError`] category without echoing paths,
/// URIs, or file content.
pub async fn prepare_http_auth(
    config: &HttpConfig,
    fetch_timeout: Duration,
) -> Result<HttpAuthMaterial, HttpTransportError> {
    match &config.auth {
        HttpAuthConfig::StaticToken { token_file } => StaticToken::load(token_file)
            .map(HttpAuthMaterial::StaticToken)
            .map_err(HttpTransportError::StaticToken),
        HttpAuthConfig::OAuthResourceServer(oauth_config) => {
            oauth::OAuthValidator::start((**oauth_config).clone(), fetch_timeout)
                .await
                .map(|validator| HttpAuthMaterial::OAuth(Box::new(validator)))
                .map_err(HttpTransportError::Jwks)
        }
    }
}

/// Serves the authenticated Streamable HTTP listener until shutdown.
///
/// SIGINT, SIGTERM, or a fatal listener failure begins one idempotent
/// shutdown: stop accepting, drain in-flight work until the configured
/// deadline, cancel the remainder, and release Anytype operation permits.
///
/// # Errors
///
/// Returns a fixed [`HttpTransportError`] category; a drained or
/// deadline-cancelled shutdown is success.
pub async fn serve_http(
    runtime: RuntimeContext,
    protocol_mode: ProtocolMode,
    config: HttpConfig,
    auth: HttpAuthMaterial,
) -> Result<(), HttpTransportError> {
    let (authenticator, metadata) = match auth {
        HttpAuthMaterial::StaticToken(token) => (auth::Authenticator::StaticToken(token), None),
        HttpAuthMaterial::OAuth(validator) => {
            let metadata = validator.metadata_document();
            (
                auth::Authenticator::OAuthResourceServer(validator),
                Some(metadata),
            )
        }
    };

    let session_cancel = CancellationToken::new();
    let service: listener::McpService = match protocol_mode {
        ProtocolMode::Stable => {
            let backend = Arc::new(session::StableBackend::new(
                runtime.clone(),
                &config,
                session_cancel.clone(),
            ));
            let ingress_runtime = runtime.clone();
            Arc::new(move |admitted| {
                let backend = Arc::clone(&backend);
                let runtime = ingress_runtime.clone();
                Box::pin(async move {
                    let invocation = admitted.invocation.clone();
                    runtime
                        .scope_ingress(invocation, backend.call(admitted))
                        .await
                })
            })
        }
        ProtocolMode::Experimental20260728 => {
            let backend = Arc::new(preview::PreviewBackend::new(runtime.clone()));
            let ingress_runtime = runtime.clone();
            Arc::new(move |admitted| {
                let backend = Arc::clone(&backend);
                let runtime = ingress_runtime.clone();
                Box::pin(async move {
                    let invocation = admitted.invocation.clone();
                    runtime
                        .scope_ingress(invocation, backend.call(admitted))
                        .await
                })
            })
        }
    };

    tracing::info!(
        target: "any_mcp::http",
        protocol = match protocol_mode {
            ProtocolMode::Stable => "stable",
            ProtocolMode::Experimental20260728 => "experimental-2026-07-28",
        },
        auth = match &authenticator {
            auth::Authenticator::StaticToken(_) => "static-token",
            auth::Authenticator::OAuthResourceServer(_) => "oauth-resource-server",
            #[cfg(test)]
            auth::Authenticator::SyntheticAllow => "synthetic",
        },
        max_sessions = config.max_sessions,
        requests_per_minute = config.requests_per_minute,
        shutdown_secs = config.shutdown.as_secs(),
        "http_transport_starting"
    );

    let state = Arc::new(listener::ListenerState::new_with_runtime(
        &config,
        authenticator,
        metadata,
        service,
        &runtime,
    ));
    let shutdown = CancellationToken::new();
    let signal_shutdown = shutdown.clone();
    let signals = tokio::spawn(async move {
        crate::runtime::wait_for_shutdown_signal().await;
        signal_shutdown.cancel();
    });
    let bound = match tokio::net::TcpListener::bind(config.bind).await {
        Ok(bound) => bound,
        Err(_) => {
            signals.abort();
            session_cancel.cancel();
            shutdown_runtime(&runtime).await;
            return Err(HttpTransportError::Bind);
        }
    };
    tracing::info!(target: "any_mcp::http", "http_transport_ready");

    let runtime_shutdown = runtime.shutdown_token();
    let staging_shutdown = shutdown.clone();
    let staging_failure = tokio::spawn(async move {
        runtime_shutdown.cancelled().await;
        staging_shutdown.cancel();
    });

    let result = listener::run_listener(bound, state, shutdown.clone(), config.shutdown).await;
    signals.abort();
    staging_failure.abort();
    // Cancel remaining sessions and SSE streams, then stop admitting any
    // Anytype operation work.
    session_cancel.cancel();
    shutdown_runtime(&runtime).await;
    tracing::info!(target: "any_mcp::http", "http_transport_stopping");
    result.map_err(|listener::HttpServeError::Listener| HttpTransportError::Listener)
}

/// Stops runtime admission and waits for owned artifact settlement.
async fn shutdown_runtime(runtime: &RuntimeContext) {
    runtime.begin_shutdown();
    runtime.drain_artifact_cleanup().await;
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use anytype::prelude::{AnytypeClient, ClientConfig};

    use super::*;
    use crate::{
        artifact_toolset::ImportIdempotency,
        runtime::{RuntimeContext, StartupStatus},
    };

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn test_runtime() -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("any-mcp-http-shutdown-test".to_owned()),
            app_name: "any-mcp-http-shutdown-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("test client");
        RuntimeContext::from_parts(
            client,
            1,
            Duration::from_secs(1),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
        )
    }

    async fn pending_settlement(runtime: &RuntimeContext) -> Arc<AtomicBool> {
        let key = [41; 32];
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import(key, [42; 32])
                .await,
            Ok(ImportIdempotency::Dispatch)
        ));
        let permit = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("settlement permit");
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let cancellation = CancellationToken::new();
        let capability = runtime
            .admit_invocation("file_import", &cancellation)
            .await
            .expect("invocation admission");
        let _receiver = runtime
            .run_invocation(
                capability,
                &cancellation,
                Box::pin(async {
                    Some(
                        runtime.supervise_import_settlement(key, permit, async move {
                            let _marker = DropMarker(marker);
                            std::future::pending().await
                        }),
                    )
                }),
            )
            .await
            .expect("start settlement")
            .expect("settlement receiver");
        tokio::task::yield_now().await;
        dropped
    }

    #[tokio::test]
    async fn http_shutdown_waits_for_owned_artifact_settlement() {
        let runtime = test_runtime();
        let dropped = pending_settlement(&runtime).await;

        shutdown_runtime(&runtime).await;

        assert!(dropped.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn bind_failure_runs_the_shared_artifact_cleanup() {
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").expect("occupy address");
        let address = occupied.local_addr().expect("occupied address");
        let runtime = test_runtime();
        let dropped = pending_settlement(&runtime).await;
        let mut config = listener::tests::test_config(&[]);
        config.bind = address;

        let result = serve_http(
            runtime,
            ProtocolMode::Stable,
            config,
            HttpAuthMaterial::StaticToken(StaticToken::for_test()),
        )
        .await;

        assert_eq!(result, Err(HttpTransportError::Bind));
        assert!(dropped.load(Ordering::Acquire));
        drop(occupied);
    }
}
