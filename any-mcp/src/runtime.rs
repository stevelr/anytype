// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Authenticated client ownership and MCP service lifecycle.

use std::{fmt, future::Future, sync::Arc, time::Duration};

use anytype::prelude::AnytypeClient;
use rmcp::{RoleServer, ServiceExt, service::QuitReason, transport::IntoTransport};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{config::RuntimeConfig, server::AnyMcpServer};

/// Availability established once during authenticated startup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupStatus {
    /// Whether the mandatory authenticated HTTP ping succeeded.
    pub http_available: bool,
    /// Whether configured gRPC credentials were present and its ping succeeded.
    pub grpc_available: bool,
}

/// Shared state for all MCP workflow handlers in one process.
///
/// The client is immutable and internally shareable. A semaphore, rather than
/// a client mutex, bounds concurrent work, so no shared lock is held across an
/// upstream await.
#[derive(Clone)]
pub struct RuntimeContext {
    client: Arc<AnytypeClient>,
    permits: Arc<Semaphore>,
    request_timeout: Duration,
    startup_status: StartupStatus,
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeContext")
            .field("request_timeout", &self.request_timeout)
            .field("startup_status", &self.startup_status)
            .finish_non_exhaustive()
    }
}

impl RuntimeContext {
    /// Builds the long-lived client, loads existing credentials, and performs
    /// the mandatory startup health checks exactly once.
    ///
    /// HTTP credentials and ping success are required. gRPC is checked when
    /// gRPC credentials are configured; absent gRPC credentials do not prevent
    /// the REST-backed default toolset from starting.
    ///
    /// # Errors
    ///
    /// Returns a concise [`StartupError`] without embedding credential values
    /// or upstream response bodies.
    pub async fn start(config: &RuntimeConfig) -> Result<Self, StartupError> {
        let client = AnytypeClient::with_config(config.client_config())
            .map_err(|_| StartupError::ClientInitialization)?;
        let auth = client
            .auth_status()
            .map_err(|_| StartupError::CredentialLookup)?;

        if !auth.http.is_authenticated() {
            return Err(StartupError::MissingHttpCredentials);
        }
        startup_check(config.startup_timeout, client.ping_http())
            .await
            .map_err(|error| match error {
                StartupCheckError::Timeout => StartupError::HttpTimeout,
                StartupCheckError::Unavailable => StartupError::HttpUnavailable,
            })?;

        let grpc_available = if auth.grpc.is_authenticated() {
            startup_check(config.startup_timeout, client.ping_grpc())
                .await
                .map_err(|error| match error {
                    StartupCheckError::Timeout => StartupError::GrpcTimeout,
                    StartupCheckError::Unavailable => StartupError::GrpcUnavailable,
                })?;
            true
        } else {
            false
        };

        Ok(Self::from_parts(
            client,
            config.max_concurrency,
            config.request_timeout,
            StartupStatus {
                http_available: true,
                grpc_available,
            },
        ))
    }

    /// Returns the one long-lived Anytype client.
    #[must_use]
    pub fn client(&self) -> &AnytypeClient {
        self.client.as_ref()
    }

    /// Returns the startup availability snapshot without repeating pings.
    #[must_use]
    pub const fn startup_status(&self) -> StartupStatus {
        self.startup_status
    }

    /// Executes one upstream operation with concurrency, timeout, and MCP
    /// cancellation controls.
    ///
    /// The timeout covers both waiting for a permit and the upstream future.
    /// A handler should pass the cancellation token from its rmcp request
    /// context, which rmcp cancels on `notifications/cancelled`.
    pub async fn execute<F, T, E>(
        &self,
        cancellation: &CancellationToken,
        operation: F,
    ) -> Result<T, RuntimeError<E>>
    where
        F: Future<Output = Result<T, E>>,
    {
        let controlled = async {
            let permit = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(RuntimeError::Cancelled),
                permit = self.permits.acquire() => {
                    permit.map_err(|_| RuntimeError::ShuttingDown)?
                }
            };

            let result = tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(RuntimeError::Cancelled),
                result = operation => result.map_err(RuntimeError::Upstream),
            };
            drop(permit);
            result
        };

        tokio::time::timeout(self.request_timeout, controlled)
            .await
            .unwrap_or(Err(RuntimeError::TimedOut))
    }

    pub(crate) fn from_parts(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
    ) -> Self {
        Self {
            client: Arc::new(client),
            permits: Arc::new(Semaphore::new(max_concurrency)),
            request_timeout,
            startup_status,
        }
    }
}

async fn startup_check<F, T, E>(timeout: Duration, operation: F) -> Result<(), StartupCheckError>
where
    F: Future<Output = Result<T, E>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(_)) => Err(StartupCheckError::Unavailable),
        Err(_) => Err(StartupCheckError::Timeout),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupCheckError {
    Timeout,
    Unavailable,
}

/// A controlled upstream operation failure.
pub enum RuntimeError<E> {
    /// The MCP client cancelled this request.
    Cancelled,
    /// The end-to-end operation timeout elapsed.
    TimedOut,
    /// The server is shutting down and no more permits can be acquired.
    ShuttingDown,
    /// The upstream Anytype operation failed.
    Upstream(E),
}

impl<E> fmt::Debug for RuntimeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("RuntimeError::Cancelled"),
            Self::TimedOut => formatter.write_str("RuntimeError::TimedOut"),
            Self::ShuttingDown => formatter.write_str("RuntimeError::ShuttingDown"),
            Self::Upstream(_) => formatter.write_str("RuntimeError::Upstream(<redacted>)"),
        }
    }
}

impl<E> fmt::Display for RuntimeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("request cancelled"),
            Self::TimedOut => formatter.write_str("Anytype request timed out"),
            Self::ShuttingDown => formatter.write_str("server is shutting down"),
            Self::Upstream(_) => formatter.write_str("Anytype request failed"),
        }
    }
}

impl<E> std::error::Error for RuntimeError<E> {}

/// A redacted authenticated-startup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupError {
    /// Client or keystore initialization failed.
    ClientInitialization,
    /// Credentials could not be loaded from the configured keystore.
    CredentialLookup,
    /// No HTTP token was present in the configured keystore.
    MissingHttpCredentials,
    /// The authenticated HTTP ping failed.
    HttpUnavailable,
    /// The authenticated HTTP ping exceeded its deadline.
    HttpTimeout,
    /// Configured gRPC credentials failed their ping.
    GrpcUnavailable,
    /// The authenticated gRPC ping exceeded its deadline.
    GrpcTimeout,
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientInitialization => formatter.write_str(
                "unable to initialize Anytype client from the configured endpoint and keystore",
            ),
            Self::CredentialLookup => {
                formatter.write_str("unable to read configured Anytype credentials")
            }
            Self::MissingHttpCredentials => formatter.write_str(
                "Anytype HTTP credentials are missing; configure the existing anyr keystore or env keystore",
            ),
            Self::HttpUnavailable => formatter.write_str("authenticated Anytype HTTP ping failed"),
            Self::HttpTimeout => formatter.write_str("authenticated Anytype HTTP ping timed out"),
            Self::GrpcUnavailable => formatter.write_str("authenticated Anytype gRPC ping failed"),
            Self::GrpcTimeout => formatter.write_str("authenticated Anytype gRPC ping timed out"),
        }
    }
}

impl std::error::Error for StartupError {}

/// Runs the authenticated server over stdin/stdout until EOF.
///
/// Stdout is passed directly to rmcp and is never used for diagnostics.
///
/// # Errors
///
/// Returns a redacted [`ServeError`] for protocol initialization or service
/// task failures. EOF, including EOF before initialization, is a clean exit.
pub async fn serve_stdio(runtime: RuntimeContext) -> Result<(), ServeError> {
    serve_transport(AnyMcpServer::new(runtime), rmcp::transport::stdio()).await
}

/// Runs an initialized handler over an arbitrary rmcp transport.
///
/// This seam supports protocol lifecycle tests without redirecting process
/// stdout.
///
/// # Errors
///
/// Returns a redacted [`ServeError`] if initialization or the service task
/// fails. Transport EOF is successful shutdown.
pub async fn serve_transport<T, E, A>(server: AnyMcpServer, transport: T) -> Result<(), ServeError>
where
    T: IntoTransport<RoleServer, E, A>,
    E: std::error::Error + Send + Sync + 'static,
{
    let running = match server.serve(transport).await {
        Ok(running) => running,
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
        Err(_) => return Err(ServeError::Initialization),
    };

    match running.waiting().await {
        Ok(QuitReason::Closed | QuitReason::Cancelled) => Ok(()),
        Ok(QuitReason::JoinError(_)) | Ok(_) | Err(_) => Err(ServeError::ServiceTask),
    }
}

/// A safe stdio service failure which omits protocol payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeError {
    /// MCP initialization failed.
    Initialization,
    /// The rmcp service task failed.
    ServiceTask,
}

impl fmt::Display for ServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initialization => formatter.write_str("MCP stdio initialization failed"),
            Self::ServiceTask => formatter.write_str("MCP stdio service task failed"),
        }
    }
}

impl std::error::Error for ServeError {}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use anytype::prelude::ClientConfig;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};

    use super::*;

    fn runtime(max_concurrency: usize, timeout: Duration) -> RuntimeContext {
        let config = ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_string()),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-test".to_string()),
            app_name: "any-mcp-test".to_string(),
            ..ClientConfig::default()
        };
        let client = AnytypeClient::with_config(config).expect("in-memory test client");
        RuntimeContext::from_parts(
            client,
            max_concurrency,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
        )
    }

    #[tokio::test]
    async fn execute_honors_request_cancellation() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = runtime
            .execute(&cancellation, async {
                std::future::pending::<Result<(), Infallible>>().await
            })
            .await;

        assert!(matches!(result, Err(RuntimeError::Cancelled)));
    }

    #[tokio::test]
    async fn execute_applies_end_to_end_timeout() {
        let runtime = runtime(1, Duration::from_millis(20));
        let cancellation = CancellationToken::new();

        let result = runtime
            .execute(&cancellation, async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<_, Infallible>(())
            })
            .await;

        assert!(matches!(result, Err(RuntimeError::TimedOut)));
    }

    #[tokio::test]
    async fn concurrency_limit_bounds_waiting_operations() {
        let runtime = runtime(1, Duration::from_secs(1));
        let first_runtime = runtime.clone();
        let first_started = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());
        let started = first_started.clone();
        let release = release_first.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .execute(&CancellationToken::new(), async move {
                    started.notify_one();
                    release.notified().await;
                    Ok::<_, Infallible>(())
                })
                .await
        });
        first_started.notified().await;

        let second_cancellation = CancellationToken::new();
        let cancel_second = second_cancellation.clone();
        let second_executed = Arc::new(AtomicBool::new(false));
        let executed = second_executed.clone();
        let second_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .execute(&second_cancellation, async move {
                    executed.store(true, Ordering::SeqCst);
                    Ok::<_, Infallible>(())
                })
                .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!second_executed.load(Ordering::SeqCst));
        cancel_second.cancel();
        assert!(matches!(
            second.await.expect("second task"),
            Err(RuntimeError::Cancelled)
        ));

        release_first.notify_one();
        assert!(first.await.expect("first task").is_ok());
    }

    #[tokio::test]
    async fn upstream_error_formatting_is_redacted() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        let result = runtime
            .execute(&cancellation, async {
                Err::<(), _>("secret-token-and-upstream-body")
            })
            .await
            .expect_err("upstream failure");

        assert_eq!(result.to_string(), "Anytype request failed");
        assert!(!format!("{result:?}").contains("secret-token"));
    }

    #[tokio::test]
    async fn eof_before_initialize_is_a_clean_shutdown() {
        let (client_transport, server_transport) = duplex(1024);
        drop(client_transport);

        let result = serve_transport(
            AnyMcpServer::new(runtime(1, Duration::from_secs(1))),
            server_transport,
        )
        .await;

        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn initialized_transport_shuts_down_cleanly_on_eof() {
        let (client_transport, server_transport) = duplex(4096);
        let server = tokio::spawn(serve_transport(
            AnyMcpServer::new(runtime(1, Duration::from_secs(1))),
            server_transport,
        ));
        let (reader, mut writer) = split(client_transport);
        let mut reader = BufReader::new(reader);
        writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"runtime-test","version":"0.0.0"}}}
"#,
            )
            .await
            .expect("write initialize request");
        writer.flush().await.expect("flush initialize request");

        let mut response = String::new();
        tokio::time::timeout(Duration::from_secs(1), reader.read_line(&mut response))
            .await
            .expect("initialize response deadline")
            .expect("read initialize response");
        assert!(response.contains("\"id\":1"));
        assert!(response.contains("\"protocolVersion\":\"2026-07-28\""));

        drop(writer);
        drop(reader);
        assert_eq!(server.await.expect("server task"), Ok(()));
    }
}
