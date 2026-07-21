// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Authenticated client ownership and MCP service lifecycle.

use std::{
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anytype::prelude::{AnytypeClient, AnytypeError};
use rmcp::{
    RoleServer, ServiceExt,
    service::{QuitReason, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{IntoTransport, Transport},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    config::{ApplicationProfile, ProtocolMode, RuntimeConfig},
    server::AnyMcpServer,
};

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
    shutdown: CancellationToken,
    next_correlation_id: Arc<AtomicU64>,
    request_timeout: Duration,
    startup_status: StartupStatus,
    profile: ApplicationProfile,
    read_only: bool,
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeContext")
            .field("request_timeout", &self.request_timeout)
            .field("startup_status", &self.startup_status)
            .field("profile", &self.profile)
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl RuntimeContext {
    /// Builds the long-lived client, loads existing credentials, and performs
    /// the mandatory startup health checks exactly once.
    ///
    /// HTTP credentials and ping success are required. gRPC is checked when
    /// gRPC credentials are configured, and is additionally required when the
    /// selected profile/access catalog cannot fulfill its contracts over HTTP.
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

        let startup_status = verify_startup_probes(
            auth.http.is_authenticated(),
            auth.grpc.is_authenticated(),
            config.profile.requires_grpc(config.read_only),
            config.startup_timeout,
            || client.ping_http(),
            || client.ping_grpc(),
        )
        .await?;

        Ok(Self::from_parts_with_profile(
            client,
            config.max_concurrency,
            config.request_timeout,
            startup_status,
            config.profile,
            config.read_only,
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

    /// Returns whether this process must omit and reject mutating workflows.
    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the startup-selected application catalog profile.
    #[must_use]
    pub const fn profile(&self) -> ApplicationProfile {
        self.profile
    }

    /// Starts process shutdown, rejects new work, and cancels running or
    /// permit-waiting operations.
    ///
    /// This operation is idempotent. The stdio transport invokes it as soon as
    /// EOF is observed, before rmcp performs its bounded in-flight drain.
    pub fn begin_shutdown(&self) {
        self.permits.close();
        self.shutdown.cancel();
    }

    /// Returns whether process shutdown has started.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    /// Executes one upstream operation with concurrency, timeout, and MCP
    /// cancellation controls.
    ///
    /// The timeout covers both waiting for a permit and the upstream future.
    /// A handler should pass the cancellation token from its rmcp
    /// `RequestContext`, which rmcp cancels on `notifications/cancelled`.
    /// Production handlers pass their request token through this explicit seam.
    /// Diagnostics use a server-generated correlation ID and never the
    /// peer-controlled raw MCP request ID.
    pub async fn execute<F, T>(
        &self,
        context: OperationContext,
        cancellation: &CancellationToken,
        operation: F,
    ) -> Result<T, RuntimeError>
    where
        F: Future<Output = Result<T, AnytypeError>>,
    {
        self.execute_classified(
            context,
            cancellation,
            operation,
            OperationFailureDiagnostic::from_anytype,
        )
        .await
        .map_err(|error| match error {
            ControlledOperationError::Cancelled => RuntimeError::Cancelled,
            ControlledOperationError::TimedOut => RuntimeError::TimedOut,
            ControlledOperationError::ShuttingDown => RuntimeError::ShuttingDown,
            ControlledOperationError::Operation(error) => RuntimeError::Upstream(error),
        })
    }

    pub(crate) async fn execute_classified<F, T, E, C>(
        &self,
        context: OperationContext,
        cancellation: &CancellationToken,
        operation: F,
        classify: C,
    ) -> Result<T, ControlledOperationError<E>>
    where
        F: Future<Output = Result<T, E>>,
        C: Fn(&E) -> OperationFailureDiagnostic,
    {
        self.execute_classified_with_control(
            context,
            cancellation,
            operation,
            classify,
            default_control_failure_diagnostic,
        )
        .await
    }

    pub(crate) async fn execute_classified_with_control<F, T, E, C, D>(
        &self,
        context: OperationContext,
        cancellation: &CancellationToken,
        operation: F,
        classify: C,
        classify_control: D,
    ) -> Result<T, ControlledOperationError<E>>
    where
        F: Future<Output = Result<T, E>>,
        C: Fn(&E) -> OperationFailureDiagnostic,
        D: Fn(ControlledFailureKind) -> OperationFailureDiagnostic,
    {
        let started = Instant::now();
        let correlation_id = self
            .next_correlation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .expect("correlation update always returns a value");
        let controlled = async {
            let permit = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => {
                    return Err(ControlledOperationError::ShuttingDown);
                },
                () = cancellation.cancelled() => {
                    return Err(ControlledOperationError::Cancelled);
                },
                permit = self.permits.acquire() => {
                    permit.map_err(|_| ControlledOperationError::ShuttingDown)?
                }
            };

            let result = tokio::select! {
                biased;
                () = self.shutdown.cancelled() => Err(ControlledOperationError::ShuttingDown),
                () = cancellation.cancelled() => Err(ControlledOperationError::Cancelled),
                result = operation => result.map_err(ControlledOperationError::Operation),
            };
            drop(permit);
            result
        };

        let result = tokio::time::timeout(self.request_timeout, controlled)
            .await
            .unwrap_or(Err(ControlledOperationError::TimedOut));
        log_classified_operation(
            context,
            correlation_id,
            started.elapsed(),
            &result,
            &classify,
            &classify_control,
        );
        result
    }

    #[cfg(test)]
    pub(crate) fn from_parts(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
    ) -> Self {
        Self::from_parts_with_read_only(
            client,
            max_concurrency,
            request_timeout,
            startup_status,
            false,
        )
    }

    #[cfg(test)]
    pub(crate) fn from_parts_with_read_only(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
        read_only: bool,
    ) -> Self {
        Self::from_parts_with_profile(
            client,
            max_concurrency,
            request_timeout,
            startup_status,
            ApplicationProfile::Standard,
            read_only,
        )
    }

    pub(crate) fn from_parts_with_profile(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
        profile: ApplicationProfile,
        read_only: bool,
    ) -> Self {
        Self {
            client: Arc::new(client),
            permits: Arc::new(Semaphore::new(max_concurrency)),
            shutdown: CancellationToken::new(),
            next_correlation_id: Arc::new(AtomicU64::new(1)),
            request_timeout,
            startup_status,
            profile,
            read_only,
        }
    }
}

async fn verify_startup_probes<FH, FG, HH, HG, EH, EG>(
    http_configured: bool,
    grpc_configured: bool,
    grpc_required: bool,
    timeout: Duration,
    http_probe: FH,
    grpc_probe: FG,
) -> Result<StartupStatus, StartupError>
where
    FH: FnOnce() -> HH,
    FG: FnOnce() -> HG,
    HH: Future<Output = Result<(), EH>>,
    HG: Future<Output = Result<(), EG>>,
{
    if !http_configured {
        return Err(StartupError::MissingHttpCredentials);
    }
    startup_check(timeout, http_probe())
        .await
        .map_err(|error| match error {
            StartupCheckError::Timeout => StartupError::HttpTimeout,
            StartupCheckError::Unavailable => StartupError::HttpUnavailable,
        })?;

    if grpc_required && !grpc_configured {
        return Err(StartupError::MissingRequiredGrpcCredentials);
    }

    let grpc_available = if grpc_configured {
        startup_check(timeout, grpc_probe())
            .await
            .map_err(|error| match error {
                StartupCheckError::Timeout => StartupError::GrpcTimeout,
                StartupCheckError::Unavailable => StartupError::GrpcUnavailable,
            })?;
        true
    } else {
        false
    };

    Ok(StartupStatus {
        http_available: true,
        grpc_available,
    })
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

/// Static diagnostic context for one bounded Anytype operation.
///
/// Operation names should be short lowercase identifiers such as
/// `object_get`. Invalid names are replaced with `invalid_operation` rather
/// than logged. The runtime adds a monotonic server correlation ID; it never
/// records the peer-controlled raw MCP request ID, which may contain sensitive
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationContext {
    operation: &'static str,
}

impl OperationContext {
    /// Creates operation context from a compile-time name.
    #[must_use]
    pub const fn new(operation: &'static str) -> Self {
        Self { operation }
    }

    fn safe_operation(self) -> &'static str {
        const MAX_OPERATION_LEN: usize = 64;
        if !self.operation.is_empty()
            && self.operation.len() <= MAX_OPERATION_LEN
            && self
                .operation
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            self.operation
        } else {
            "invalid_operation"
        }
    }
}

pub(crate) enum ControlledOperationError<E> {
    Cancelled,
    TimedOut,
    ShuttingDown,
    Operation(E),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlledFailureKind {
    Cancelled,
    TimedOut,
    ShuttingDown,
}

#[derive(Clone, Copy)]
pub(crate) struct OperationFailureDiagnostic {
    outcome: &'static str,
    status: UpstreamDiagnostic,
}

impl OperationFailureDiagnostic {
    pub(crate) const fn classified(outcome: &'static str, category: &'static str) -> Self {
        Self {
            outcome,
            status: UpstreamDiagnostic::new(category),
        }
    }

    pub(crate) fn from_anytype(error: &AnytypeError) -> Self {
        Self {
            outcome: "upstream_error",
            status: UpstreamDiagnostic::from_error(error),
        }
    }
}

fn log_classified_operation<T, E, C>(
    context: OperationContext,
    correlation_id: u64,
    duration: Duration,
    result: &Result<T, ControlledOperationError<E>>,
    classify: &C,
    classify_control: &impl Fn(ControlledFailureKind) -> OperationFailureDiagnostic,
) where
    C: Fn(&E) -> OperationFailureDiagnostic,
{
    let diagnostic = match result {
        Ok(_) => OperationFailureDiagnostic::classified("success", "success"),
        Err(ControlledOperationError::Cancelled) => {
            classify_control(ControlledFailureKind::Cancelled)
        }
        Err(ControlledOperationError::TimedOut) => {
            classify_control(ControlledFailureKind::TimedOut)
        }
        Err(ControlledOperationError::ShuttingDown) => {
            classify_control(ControlledFailureKind::ShuttingDown)
        }
        Err(ControlledOperationError::Operation(error)) => classify(error),
    };
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        target: "any_mcp::operation",
        operation = context.safe_operation(),
        correlation_id,
        duration_ms,
        outcome = diagnostic.outcome,
        upstream_status = diagnostic.status.category,
        upstream_http_status = diagnostic.status.http_status.unwrap_or_default(),
        upstream_http_status_present = diagnostic.status.http_status.is_some(),
        "Anytype operation completed"
    );
}

const fn default_control_failure_diagnostic(
    failure: ControlledFailureKind,
) -> OperationFailureDiagnostic {
    match failure {
        ControlledFailureKind::Cancelled => {
            OperationFailureDiagnostic::classified("cancelled", "not_observed")
        }
        ControlledFailureKind::TimedOut => {
            OperationFailureDiagnostic::classified("timeout", "not_observed")
        }
        ControlledFailureKind::ShuttingDown => {
            OperationFailureDiagnostic::classified("shutdown", "not_observed")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UpstreamDiagnostic {
    category: &'static str,
    http_status: Option<u16>,
}

impl UpstreamDiagnostic {
    const fn new(category: &'static str) -> Self {
        Self {
            category,
            http_status: None,
        }
    }

    fn from_error(error: &AnytypeError) -> Self {
        match error {
            AnytypeError::Http { .. } => Self::new("http_transport"),
            AnytypeError::ApiError { code, .. } => Self {
                category: "http_status",
                http_status: Some(*code),
            },
            AnytypeError::TooManyRetries { .. } => Self::new("http_retries"),
            AnytypeError::Auth { .. } | AnytypeError::Unauthorized => Self::new("auth"),
            AnytypeError::Forbidden => Self::new("permission"),
            AnytypeError::Deserialization { .. } | AnytypeError::Serialization { .. } => {
                Self::new("codec")
            }
            AnytypeError::NotFound { .. } => Self::new("not_found"),
            AnytypeError::Ambiguous { .. } => Self::new("ambiguous"),
            AnytypeError::ResolutionLimitExceeded { .. } => Self::new("resolution_limit"),
            AnytypeError::ResponseTooLarge { .. } => Self::new("response_too_large"),
            AnytypeError::FileHeaderEvidenceTooLarge { status, .. } => Self {
                category: "file_header_evidence_too_large",
                http_status: Some(*status),
            },
            AnytypeError::InvalidFileResponseHeader { status, .. } => Self {
                category: "invalid_file_response_header",
                http_status: Some(*status),
            },
            AnytypeError::ChatSseEventTooLarge { .. } => Self::new("chat_sse_event_too_large"),
            AnytypeError::ChatSseTransport { .. } => Self::new("chat_sse_transport"),
            AnytypeError::RateLimitExceeded { .. } => Self::new("rate_limit"),
            AnytypeError::Validation { .. } => Self::new("validation"),
            AnytypeError::NoKeyStore | AnytypeError::KeyStore { .. } => Self::new("keystore"),
            AnytypeError::Grpc { .. } | AnytypeError::GrpcUnavailable { .. } => Self::new("grpc"),
            AnytypeError::CacheDisabled => Self::new("cache"),
            AnytypeError::BodyGraph { .. } => Self::new("body_graph"),
            AnytypeError::VerifyTimeout { .. } => Self::new("verification"),
            AnytypeError::Other { .. } => Self::new("other"),
        }
    }
}

/// A controlled upstream operation failure.
pub enum RuntimeError {
    /// The MCP client cancelled this request.
    Cancelled,
    /// The end-to-end operation timeout elapsed.
    TimedOut,
    /// The server is shutting down and no more permits can be acquired.
    ShuttingDown,
    /// The upstream Anytype operation failed.
    Upstream(AnytypeError),
}

impl fmt::Debug for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("RuntimeError::Cancelled"),
            Self::TimedOut => formatter.write_str("RuntimeError::TimedOut"),
            Self::ShuttingDown => formatter.write_str("RuntimeError::ShuttingDown"),
            Self::Upstream(_) => formatter.write_str("RuntimeError::Upstream(<redacted>)"),
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("request cancelled"),
            Self::TimedOut => formatter.write_str("Anytype request timed out"),
            Self::ShuttingDown => formatter.write_str("server is shutting down"),
            Self::Upstream(_) => formatter.write_str("Anytype request failed"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// A redacted authenticated-startup failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupError {
    /// Client or keystore initialization failed.
    ClientInitialization,
    /// Credentials could not be loaded from the configured keystore.
    CredentialLookup,
    /// No HTTP token was present in the configured keystore.
    MissingHttpCredentials,
    /// The selected catalog requires gRPC, but no gRPC credentials were present.
    MissingRequiredGrpcCredentials,
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
            Self::MissingRequiredGrpcCredentials => formatter.write_str(
                "selected Anytype MCP catalog requires configured gRPC credentials",
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
/// The selected bounded framing adapter owns stdout, which is never used for
/// diagnostics. Stable mode delegates typed lifecycle dispatch to rmcp;
/// experimental preview mode uses the stateless adapter.
///
/// # Errors
///
/// Returns a redacted [`ServeError`] for protocol initialization or service
/// task failures. EOF, including EOF before initialization, is a clean exit.
pub async fn serve_stdio(
    runtime: RuntimeContext,
    protocol_mode: ProtocolMode,
) -> Result<(), ServeError> {
    let server = AnyMcpServer::new(runtime).map_err(|_| ServeError::Catalog)?;
    crate::stdio::serve_stdio(server, protocol_mode).await
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
    let runtime = server.runtime().clone();
    let transport = ShutdownTransport {
        inner: transport.into_transport(),
        runtime: runtime.clone(),
    };
    let running = match server.serve(transport).await {
        Ok(running) => running,
        Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => {
            runtime.begin_shutdown();
            return Ok(());
        }
        Err(_) => {
            runtime.begin_shutdown();
            return Err(ServeError::Initialization);
        }
    };

    let result = match running.waiting().await {
        Ok(QuitReason::Closed | QuitReason::Cancelled) => Ok(()),
        Ok(QuitReason::JoinError(_)) | Ok(_) | Err(_) => Err(ServeError::ServiceTask),
    };
    runtime.begin_shutdown();
    result
}

struct ShutdownTransport<T> {
    inner: T,
    runtime: RuntimeContext,
}

impl<T> Transport<RoleServer> for ShutdownTransport<T>
where
    T: Transport<RoleServer>,
{
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        let message = self.inner.receive().await;
        if message.is_none() {
            self.runtime.begin_shutdown();
        }
        message
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.runtime.begin_shutdown();
        self.inner.close()
    }
}

/// A safe stdio service failure which omits protocol payloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeError {
    /// The fixed Phase 1 catalog could not be constructed safely.
    Catalog,
    /// MCP initialization failed.
    Initialization,
    /// The rmcp service task failed.
    ServiceTask,
    /// The bounded stdio adapter could not read or write a frame.
    StdioTransport,
}

impl fmt::Display for ServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog => formatter.write_str("MCP static catalog construction failed"),
            Self::Initialization => formatter.write_str("MCP stdio initialization failed"),
            Self::ServiceTask => formatter.write_str("MCP stdio service task failed"),
            Self::StdioTransport => formatter.write_str("MCP stdio transport failed"),
        }
    }
}

impl std::error::Error for ServeError {}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Duration,
    };

    use anytype::prelude::ClientConfig;
    use rmcp::{
        ErrorData as McpError, ServerHandler,
        model::{CallToolRequestParams, CallToolResult, ServerCapabilities, ServerInfo},
        service::RequestContext,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split};
    use tracing::instrument::WithSubscriber;

    use super::*;

    #[derive(Clone)]
    struct CancellationToolServer {
        runtime: RuntimeContext,
        started: Arc<tokio::sync::Notify>,
        cancelled: Arc<tokio::sync::Notify>,
    }

    impl ServerHandler for CancellationToolServer {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }

        async fn call_tool(
            &self,
            _request: CallToolRequestParams,
            context: RequestContext<RoleServer>,
        ) -> Result<CallToolResult, McpError> {
            let started = self.started.clone();
            let result = self
                .runtime
                .execute(
                    OperationContext::new("cancellation_probe"),
                    &context.ct,
                    async move {
                        started.notify_one();
                        std::future::pending::<Result<(), AnytypeError>>().await
                    },
                )
                .await;
            assert!(matches!(result, Err(RuntimeError::Cancelled)));
            self.cancelled.notify_one();
            Ok(CallToolResult::success(Vec::new()))
        }
    }

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
                grpc_available: true,
            },
        )
    }

    fn run_trace_test<F>(future: F) -> F::Output
    where
        F: Future,
    {
        let _guard = crate::logging::test_support::trace_test_guard();
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("trace test runtime")
            .block_on(future)
    }

    #[tokio::test]
    async fn startup_requires_http_without_running_probes() {
        let http_calls = AtomicUsize::new(0);
        let grpc_calls = AtomicUsize::new(0);
        let result = verify_startup_probes(
            false,
            true,
            false,
            Duration::from_secs(1),
            || async {
                http_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            || async {
                grpc_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
        )
        .await;

        assert_eq!(result, Err(StartupError::MissingHttpCredentials));
        assert_eq!(http_calls.load(Ordering::SeqCst), 0);
        assert_eq!(grpc_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn startup_runs_http_and_only_configured_grpc_probe_once() {
        let http_calls = AtomicUsize::new(0);
        let grpc_calls = AtomicUsize::new(0);
        let http_only = verify_startup_probes(
            true,
            false,
            false,
            Duration::from_secs(1),
            || async {
                http_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            || async {
                grpc_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
        )
        .await
        .expect("HTTP-only startup");
        assert_eq!(
            http_only,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            }
        );
        assert_eq!(http_calls.load(Ordering::SeqCst), 1);
        assert_eq!(grpc_calls.load(Ordering::SeqCst), 0);

        let both = verify_startup_probes(
            true,
            true,
            false,
            Duration::from_secs(1),
            || async {
                http_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            || async {
                grpc_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
        )
        .await
        .expect("HTTP and gRPC startup");
        assert_eq!(
            both,
            StartupStatus {
                http_available: true,
                grpc_available: true,
            }
        );
        assert_eq!(http_calls.load(Ordering::SeqCst), 2);
        assert_eq!(grpc_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn startup_rejects_http_only_when_selected_catalog_requires_grpc() {
        let http_calls = AtomicUsize::new(0);
        let grpc_calls = AtomicUsize::new(0);
        let result = verify_startup_probes(
            true,
            false,
            true,
            Duration::from_secs(1),
            || async {
                http_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            || async {
                grpc_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
        )
        .await;

        assert_eq!(result, Err(StartupError::MissingRequiredGrpcCredentials));
        assert_eq!(http_calls.load(Ordering::SeqCst), 1);
        assert_eq!(grpc_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn startup_rejects_neither_transport_without_running_probes() {
        let http_calls = AtomicUsize::new(0);
        let grpc_calls = AtomicUsize::new(0);
        let result = verify_startup_probes(
            false,
            false,
            true,
            Duration::from_secs(1),
            || async {
                http_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
            || async {
                grpc_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, ()>(())
            },
        )
        .await;

        assert_eq!(result, Err(StartupError::MissingHttpCredentials));
        assert_eq!(http_calls.load(Ordering::SeqCst), 0);
        assert_eq!(grpc_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn startup_fails_when_a_mandatory_configured_probe_fails() {
        let http_failure = verify_startup_probes(
            true,
            false,
            false,
            Duration::from_secs(1),
            || async { Err::<(), _>(()) },
            || async { Ok::<_, ()>(()) },
        )
        .await;
        assert_eq!(http_failure, Err(StartupError::HttpUnavailable));

        let grpc_failure = verify_startup_probes(
            true,
            true,
            false,
            Duration::from_secs(1),
            || async { Ok::<_, ()>(()) },
            || async { Err::<(), _>(()) },
        )
        .await;
        assert_eq!(grpc_failure, Err(StartupError::GrpcUnavailable));
    }

    #[tokio::test]
    async fn execute_honors_request_cancellation() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let result = runtime
            .execute(OperationContext::new("test_cancel"), &cancellation, async {
                std::future::pending::<Result<(), AnytypeError>>().await
            })
            .await;

        assert!(matches!(result, Err(RuntimeError::Cancelled)));
    }

    #[tokio::test]
    async fn rmcp_cancel_notification_cancels_upstream_and_releases_permit() {
        let runtime = runtime(1, Duration::from_secs(2));
        let started = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(tokio::sync::Notify::new());
        let handler = CancellationToolServer {
            runtime: runtime.clone(),
            started: started.clone(),
            cancelled: cancelled.clone(),
        };
        let (client_transport, server_transport) = duplex(4096);
        let server = tokio::spawn(async move {
            handler
                .serve(server_transport)
                .await
                .expect("test server initialize")
                .waiting()
                .await
                .expect("test server task")
        });
        let (reader, mut writer) = split(client_transport);
        let mut reader = BufReader::new(reader);

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"cancel-test","version":"0.0.0"}}}
"#,
            )
            .await
            .expect("write initialize");
        writer.flush().await.expect("flush initialize");
        let mut initialize = String::new();
        reader
            .read_line(&mut initialize)
            .await
            .expect("read initialize");
        assert!(initialize.contains("\"id\":1"));

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"cancel-test","arguments":{}}}
"#,
            )
            .await
            .expect("write tool call");
        writer.flush().await.expect("flush tool call");
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("tool operation started");

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2,"reason":"test cancellation"}}
"#,
            )
            .await
            .expect("write cancellation");
        writer.flush().await.expect("flush cancellation");
        tokio::time::timeout(Duration::from_secs(1), cancelled.notified())
            .await
            .expect("tool operation cancelled");

        runtime
            .execute(
                OperationContext::new("after_rmcp_cancel"),
                &CancellationToken::new(),
                async { Ok::<_, AnytypeError>(()) },
            )
            .await
            .expect("permit released after rmcp cancellation");

        writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":3,"method":"ping"}
"#,
            )
            .await
            .expect("write ping");
        writer.flush().await.expect("flush ping");
        let mut saw_cancelled_response = false;
        let ping_seen = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let mut response = String::new();
                if reader
                    .read_line(&mut response)
                    .await
                    .expect("read response")
                    == 0
                {
                    return false;
                }
                let value: rmcp::serde_json::Value =
                    rmcp::serde_json::from_str(&response).expect("protocol JSON");
                if value["id"] == 2 {
                    saw_cancelled_response = true;
                }
                if value["id"] == 3 {
                    return true;
                }
            }
        })
        .await
        .expect("ping response deadline");
        assert!(ping_seen);
        assert!(!saw_cancelled_response);

        drop(writer);
        drop(reader);
        assert!(matches!(
            server.await.expect("test server join"),
            QuitReason::Closed | QuitReason::Cancelled
        ));
    }

    #[tokio::test]
    async fn execute_applies_end_to_end_timeout() {
        let runtime = runtime(1, Duration::from_millis(20));
        let cancellation = CancellationToken::new();

        let result = runtime
            .execute(
                OperationContext::new("test_timeout"),
                &cancellation,
                async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    Ok::<_, AnytypeError>(())
                },
            )
            .await;

        assert!(matches!(result, Err(RuntimeError::TimedOut)));
    }

    #[tokio::test]
    async fn cancellation_releases_permit_for_next_operation() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let operation_started = started.clone();
        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .execute(
                    OperationContext::new("cancel_release"),
                    &cancellation,
                    async move {
                        operation_started.notify_one();
                        std::future::pending::<Result<(), AnytypeError>>().await
                    },
                )
                .await
        });
        started.notified().await;
        cancel.cancel();
        assert!(matches!(
            first.await.expect("cancelled operation"),
            Err(RuntimeError::Cancelled)
        ));

        let next = runtime
            .execute(
                OperationContext::new("after_cancel"),
                &CancellationToken::new(),
                async { Ok::<_, AnytypeError>(()) },
            )
            .await;
        assert!(next.is_ok());
    }

    #[tokio::test]
    async fn timeout_releases_permit_for_next_operation() {
        let runtime = runtime(1, Duration::from_millis(20));
        let timed_out = runtime
            .execute(
                OperationContext::new("timeout_release"),
                &CancellationToken::new(),
                std::future::pending::<Result<(), AnytypeError>>(),
            )
            .await;
        assert!(matches!(timed_out, Err(RuntimeError::TimedOut)));

        let next = runtime
            .execute(
                OperationContext::new("after_timeout"),
                &CancellationToken::new(),
                async { Ok::<_, AnytypeError>(()) },
            )
            .await;
        assert!(next.is_ok());
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
                .execute(
                    OperationContext::new("test_first"),
                    &CancellationToken::new(),
                    async move {
                        started.notify_one();
                        release.notified().await;
                        Ok::<_, AnytypeError>(())
                    },
                )
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
                .execute(
                    OperationContext::new("test_second"),
                    &second_cancellation,
                    async move {
                        executed.store(true, Ordering::SeqCst);
                        Ok::<_, AnytypeError>(())
                    },
                )
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
    async fn upstream_error_display_debug_and_tool_result_surfaces_are_redacted() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        let secrets = [
            "SECRET_CREDENTIAL",
            "SECRET_URL_TOKEN",
            "SECRET_RESPONSE_BODY",
            "localhost",
        ];
        let runtime_error = runtime
            .execute(OperationContext::new("test_error"), &cancellation, async {
                Err::<(), _>(AnytypeError::ApiError {
                    code: 500,
                    method: "Bearer SECRET_CREDENTIAL".to_owned(),
                    url: "http://localhost/private?token=SECRET_URL_TOKEN".to_owned(),
                    message: "SECRET_RESPONSE_BODY".to_owned(),
                })
            })
            .await
            .expect_err("upstream failure");

        assert_eq!(runtime_error.to_string(), "Anytype request failed");
        let RuntimeError::Upstream(source) = &runtime_error else {
            panic!("fixture must remain an upstream runtime error");
        };
        let crate::error::AnytypeErrorMapping::Ready(tool_error) =
            crate::error::ToolError::from_anytype(source)
        else {
            panic!("HTTP errors map directly to a tool error");
        };
        let result = crate::result::tool_error(&tool_error);
        let surfaces = format!(
            "{} {runtime_error:?} {} {} {result:?}",
            runtime_error,
            result
                .structured_content
                .as_ref()
                .expect("structured tool error"),
            result.content[0].as_text().expect("text tool error").text,
        );
        for secret in secrets {
            assert!(!surfaces.contains(secret), "leaked {secret}");
        }
    }

    #[test]
    fn operation_diagnostic_contains_only_safe_bounded_context() {
        run_trace_test(async {
            let runtime = runtime(1, Duration::from_secs(1));
            let secret = "secret-token-and-upstream-body";
            let (dispatch, output) =
                crate::logging::test_support::capture("any_mcp::operation=trace");

            let result = runtime
                .execute(
                    OperationContext::new("diagnostic_test"),
                    &CancellationToken::new(),
                    async {
                        Err::<(), _>(AnytypeError::ApiError {
                            code: 503,
                            method: secret.to_string(),
                            url: secret.to_string(),
                            message: secret.to_string(),
                        })
                    },
                )
                .with_subscriber(dispatch)
                .await;
            assert!(matches!(result, Err(RuntimeError::Upstream(_))));

            let output = output.contents();
            assert!(output.contains("operation=\"diagnostic_test\""));
            assert!(output.contains("correlation_id=1"));
            assert!(!output.contains("request_id="));
            assert!(output.contains("outcome=\"upstream_error\""));
            assert!(output.contains("upstream_status=\"http_status\""));
            assert!(output.contains("upstream_http_status=503"));
            assert!(output.contains("upstream_http_status_present=true"));
            assert!(output.contains("duration_ms="));
            assert!(!output.contains(secret));
        });
    }

    #[test]
    fn operation_diagnostic_classifies_every_outcome() {
        run_trace_test(async {
            let (dispatch, output) =
                crate::logging::test_support::capture("any_mcp::operation=trace");
            let active_runtime = runtime(1, Duration::from_millis(20));

            active_runtime
                .execute(
                    OperationContext::new("outcome_success"),
                    &CancellationToken::new(),
                    async { Ok::<_, AnytypeError>(()) },
                )
                .with_subscriber(dispatch.clone())
                .await
                .expect("successful operation");

            let cancelled = CancellationToken::new();
            cancelled.cancel();
            let _ = active_runtime
                .execute(
                    OperationContext::new("outcome_cancel"),
                    &cancelled,
                    std::future::pending::<Result<(), AnytypeError>>(),
                )
                .with_subscriber(dispatch.clone())
                .await;
            let _ = active_runtime
                .execute(
                    OperationContext::new("outcome_timeout"),
                    &CancellationToken::new(),
                    std::future::pending::<Result<(), AnytypeError>>(),
                )
                .with_subscriber(dispatch.clone())
                .await;
            let _ = active_runtime
                .execute(
                    OperationContext::new("outcome_upstream"),
                    &CancellationToken::new(),
                    async {
                        Err::<(), _>(AnytypeError::Other {
                            message: "SECRET_UPSTREAM_ERROR".to_string(),
                        })
                    },
                )
                .with_subscriber(dispatch.clone())
                .await;

            let shutdown_runtime = runtime(1, Duration::from_secs(1));
            shutdown_runtime.begin_shutdown();
            let _ = shutdown_runtime
                .execute(
                    OperationContext::new("outcome_shutdown"),
                    &CancellationToken::new(),
                    std::future::pending::<Result<(), AnytypeError>>(),
                )
                .with_subscriber(dispatch)
                .await;

            let output = output.contents();
            for outcome in [
                "success",
                "cancelled",
                "timeout",
                "upstream_error",
                "shutdown",
            ] {
                assert!(output.contains(&format!("outcome=\"{outcome}\"")));
            }
            assert!(output.contains("correlation_id=1"));
            assert!(output.contains("correlation_id=4"));
            assert!(!output.contains("SECRET_UPSTREAM_ERROR"));
        });
    }

    #[test]
    fn unsafe_operation_name_is_omitted_from_diagnostics() {
        let unsafe_name = OperationContext::new("secret/value");
        assert_eq!(unsafe_name.safe_operation(), "invalid_operation");
    }

    #[test]
    fn upstream_diagnostic_uses_variants_and_status_code_only() {
        let secret = "SECRET_ERROR_TEXT";
        let api = UpstreamDiagnostic::from_error(&AnytypeError::ApiError {
            code: 502,
            method: secret.to_string(),
            url: secret.to_string(),
            message: secret.to_string(),
        });
        assert_eq!(api.category, "http_status");
        assert_eq!(api.http_status, Some(502));

        let auth = UpstreamDiagnostic::from_error(&AnytypeError::Auth {
            message: secret.to_string(),
        });
        assert_eq!(auth, UpstreamDiagnostic::new("auth"));

        let grpc = UpstreamDiagnostic::from_error(&AnytypeError::GrpcUnavailable {
            message: secret.to_string(),
        });
        assert_eq!(grpc, UpstreamDiagnostic::new("grpc"));
        let file_headers =
            UpstreamDiagnostic::from_error(&AnytypeError::FileHeaderEvidenceTooLarge {
                limit: 4_096,
                status: 429,
            });
        assert_eq!(file_headers.category, "file_header_evidence_too_large");
        assert_eq!(file_headers.http_status, Some(429));
        let malformed_file =
            UpstreamDiagnostic::from_error(&AnytypeError::InvalidFileResponseHeader {
                status: 206,
                header: "content-range",
                issue: "malformed",
            });
        assert_eq!(malformed_file.category, "invalid_file_response_header");
        assert_eq!(malformed_file.http_status, Some(206));
        assert!(
            !format!("{api:?}{auth:?}{grpc:?}{file_headers:?}{malformed_file:?}").contains(secret)
        );
    }

    #[tokio::test]
    async fn process_shutdown_cancels_in_flight_operation_and_permit_waiter() {
        let runtime = runtime(1, Duration::from_secs(1));
        let first_started = Arc::new(tokio::sync::Notify::new());
        let started = first_started.clone();
        let first_runtime = runtime.clone();
        let first = tokio::spawn(async move {
            first_runtime
                .execute(
                    OperationContext::new("shutdown_active"),
                    &CancellationToken::new(),
                    async move {
                        started.notify_one();
                        std::future::pending::<Result<(), AnytypeError>>().await
                    },
                )
                .await
        });
        first_started.notified().await;

        let waiter_executed = Arc::new(AtomicBool::new(false));
        let executed = waiter_executed.clone();
        let second_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            second_runtime
                .execute(
                    OperationContext::new("shutdown_waiter"),
                    &CancellationToken::new(),
                    async move {
                        executed.store(true, Ordering::SeqCst);
                        Ok::<_, AnytypeError>(())
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        runtime.begin_shutdown();

        assert!(matches!(
            first.await.expect("active operation"),
            Err(RuntimeError::ShuttingDown)
        ));
        assert!(matches!(
            second.await.expect("permit waiter"),
            Err(RuntimeError::ShuttingDown)
        ));
        assert!(!waiter_executed.load(Ordering::SeqCst));
        assert!(runtime.is_shutting_down());
        assert!(runtime.permits.is_closed());
    }

    #[tokio::test]
    async fn eof_before_initialize_is_a_clean_shutdown() {
        let (client_transport, server_transport) = duplex(1024);
        drop(client_transport);

        let result = serve_transport(
            AnyMcpServer::new(runtime(1, Duration::from_secs(1))).expect("static test catalog"),
            server_transport,
        )
        .await;

        assert_eq!(result, Ok(()));
    }

    #[tokio::test]
    async fn initialized_transport_shuts_down_cleanly_on_eof() {
        let (client_transport, server_transport) = duplex(4096);
        let runtime = runtime(1, Duration::from_secs(2));
        let operation_started = Arc::new(tokio::sync::Notify::new());
        let started = operation_started.clone();
        let operation_runtime = runtime.clone();
        let operation = tokio::spawn(async move {
            operation_runtime
                .execute(
                    OperationContext::new("eof_active"),
                    &CancellationToken::new(),
                    async move {
                        started.notify_one();
                        std::future::pending::<Result<(), AnytypeError>>().await
                    },
                )
                .await
        });
        operation_started.notified().await;
        let waiter_executed = Arc::new(AtomicBool::new(false));
        let executed = waiter_executed.clone();
        let waiter_runtime = runtime.clone();
        let waiter = tokio::spawn(async move {
            waiter_runtime
                .execute(
                    OperationContext::new("eof_waiter"),
                    &CancellationToken::new(),
                    async move {
                        executed.store(true, Ordering::SeqCst);
                        Ok::<_, AnytypeError>(())
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        let server = tokio::spawn(serve_transport(
            AnyMcpServer::new(runtime.clone()).expect("static test catalog"),
            server_transport,
        ));
        let (reader, mut writer) = split(client_transport);
        let mut reader = BufReader::new(reader);
        writer
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"runtime-test","version":"0.0.0"}}}
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
        assert!(response.contains("\"protocolVersion\":\"2025-11-25\""));
        assert_eq!(
            response.lines().count(),
            1,
            "stdout contains one JSON frame"
        );
        let frame: rmcp::serde_json::Value =
            rmcp::serde_json::from_str(&response).expect("valid protocol JSON");
        assert_eq!(frame["jsonrpc"], "2.0");

        drop(writer);
        drop(reader);
        assert_eq!(server.await.expect("server task"), Ok(()));
        assert!(matches!(
            operation.await.expect("EOF operation"),
            Err(RuntimeError::ShuttingDown)
        ));
        assert!(matches!(
            waiter.await.expect("EOF permit waiter"),
            Err(RuntimeError::ShuttingDown)
        ));
        assert!(!waiter_executed.load(Ordering::SeqCst));
        assert!(runtime.is_shutting_down());
    }
}
