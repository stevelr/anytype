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
            Arc::new(move |admitted| Box::pin(backend.clone().call(admitted)))
        }
        ProtocolMode::Experimental20260728 => {
            let backend = Arc::new(preview::PreviewBackend::new(runtime.clone()));
            Arc::new(move |admitted| Box::pin(backend.clone().call(admitted)))
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

    let state = Arc::new(listener::ListenerState::new(
        &config,
        authenticator,
        metadata,
        service,
    ));
    let bound = tokio::net::TcpListener::bind(config.bind)
        .await
        .map_err(|_| HttpTransportError::Bind)?;
    tracing::info!(target: "any_mcp::http", "http_transport_ready");

    let shutdown = CancellationToken::new();
    let signal_shutdown = shutdown.clone();
    let signals = tokio::spawn(async move {
        let interrupt = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            let mut terminate =
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(terminate) => terminate,
                    Err(_) => {
                        let _ = interrupt.await;
                        signal_shutdown.cancel();
                        return;
                    }
                };
            tokio::select! {
                _ = interrupt => {}
                _ = terminate.recv() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = interrupt.await;
        }
        signal_shutdown.cancel();
    });

    let result = listener::run_listener(bound, state, shutdown.clone(), config.shutdown).await;
    signals.abort();
    // Cancel remaining sessions and SSE streams, then stop admitting any
    // Anytype operation work.
    session_cancel.cancel();
    runtime.begin_shutdown();
    tracing::info!(target: "any_mcp::http", "http_transport_stopping");
    result.map_err(|listener::HttpServeError::Listener| HttpTransportError::Listener)
}
