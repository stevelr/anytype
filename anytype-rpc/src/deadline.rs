//! Logical gRPC deadline configuration, propagation, and local enforcement.

use std::{
    error::Error as StdError,
    ffi::OsString,
    fmt,
    future::Future,
    marker::PhantomData,
    pin::Pin,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};

use http_body::{Body as HttpBody, Frame, SizeHint};
use prost::bytes::Buf;
use tonic::{
    Code, GrpcMethod, Request, Status, TimeoutExpired,
    body::Body,
    codegen::{Service, http},
    metadata::{Ascii, MetadataMap, MetadataValue},
};

tokio::task_local! {
    static ENCLOSING_DEADLINE: GrpcEnclosingDeadline;
}

/// Process environment variable that overrides inherited gRPC deadlines.
pub const ANYTYPE_GRPC_TIMEOUT_SECS: &str = "ANYTYPE_GRPC_TIMEOUT_SECS";
/// Largest credential, ordinary, setup, idle, or lifetime deadline.
pub const MAX_GRPC_TIMEOUT: Duration = Duration::from_secs(3_600);
/// Largest long-operation deadline.
pub const MAX_LONG_GRPC_TIMEOUT: Duration = Duration::from_secs(7_200);
/// Largest cleanup deadline.
pub const MAX_CLEANUP_GRPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Default credential-session setup deadline.
pub const DEFAULT_CREDENTIAL_GRPC_TIMEOUT: Duration = Duration::from_secs(120);
/// Default ordinary unary RPC deadline.
pub const DEFAULT_ORDINARY_GRPC_TIMEOUT: Duration = Duration::from_secs(120);
/// Default long unary RPC deadline.
pub const DEFAULT_LONG_GRPC_TIMEOUT: Duration = Duration::from_secs(1_800);
/// Default stream response-header deadline.
pub const DEFAULT_STREAM_SETUP_GRPC_TIMEOUT: Duration = Duration::from_secs(120);
/// Default cleanup RPC deadline.
pub const DEFAULT_CLEANUP_GRPC_TIMEOUT: Duration = Duration::from_secs(5);

const DEADLINE_SOURCE_METADATA: &str = "x-anytype-deadline-source";
const DEADLINE_CLASS_METADATA: &str = "x-anytype-deadline-class";
const DEADLINE_OUTCOME_METADATA: &str = "x-anytype-deadline-outcome";
const GRPC_TIMEOUT_HEADER: &str = "grpc-timeout";

/// Logical gRPC deadlines applied by an Anytype gRPC client.
///
/// `None` disables a boundary. Credential, ordinary, setup, idle, and stream
/// lifetime values must be between one and 3,600 seconds. Long unary values
/// may be as large as 7,200 seconds, while cleanup values may be at most 30
/// seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrpcTimeoutPolicy {
    /// Deadline for account-key or app-key session creation.
    pub credential_setup: Option<Duration>,
    /// Deadline for ordinary unary reads and mutations.
    pub ordinary_unary: Option<Duration>,
    /// Deadline for export, import, and equivalent long unary operations.
    pub long_unary: Option<Duration>,
    /// Deadline through successful streaming response headers.
    pub stream_setup: Option<Duration>,
    /// Optional no-progress deadline for an established stream.
    pub stream_idle: Option<Duration>,
    /// Optional total lifetime for an established stream.
    pub stream_total_lifetime: Option<Duration>,
    /// Deadline for a cleanup RPC.
    pub cleanup: Option<Duration>,
}

impl Default for GrpcTimeoutPolicy {
    fn default() -> Self {
        Self {
            credential_setup: Some(DEFAULT_CREDENTIAL_GRPC_TIMEOUT),
            ordinary_unary: Some(DEFAULT_ORDINARY_GRPC_TIMEOUT),
            long_unary: Some(DEFAULT_LONG_GRPC_TIMEOUT),
            stream_setup: Some(DEFAULT_STREAM_SETUP_GRPC_TIMEOUT),
            stream_idle: None,
            stream_total_lifetime: None,
            cleanup: Some(DEFAULT_CLEANUP_GRPC_TIMEOUT),
        }
    }
}

impl GrpcTimeoutPolicy {
    /// Resolves an explicit policy or the process environment and defaults.
    ///
    /// An explicit policy ignores [`ANYTYPE_GRPC_TIMEOUT_SECS`].
    pub fn resolve(explicit: Option<Self>) -> Result<Self, GrpcTimeoutConfigError> {
        if let Some(policy) = explicit {
            return policy.validate();
        }
        Self::from_environment(std::env::var_os(ANYTYPE_GRPC_TIMEOUT_SECS))
    }

    /// Validates every finite boundary and returns the unchanged policy.
    pub fn validate(self) -> Result<Self, GrpcTimeoutConfigError> {
        for (field, value, maximum) in [
            ("credential_setup", self.credential_setup, MAX_GRPC_TIMEOUT),
            ("ordinary_unary", self.ordinary_unary, MAX_GRPC_TIMEOUT),
            ("long_unary", self.long_unary, MAX_LONG_GRPC_TIMEOUT),
            ("stream_setup", self.stream_setup, MAX_GRPC_TIMEOUT),
            ("stream_idle", self.stream_idle, MAX_GRPC_TIMEOUT),
            (
                "stream_total_lifetime",
                self.stream_total_lifetime,
                MAX_GRPC_TIMEOUT,
            ),
            ("cleanup", self.cleanup, MAX_CLEANUP_GRPC_TIMEOUT),
        ] {
            if let Some(duration) = value
                && !(Duration::from_secs(1)..=maximum).contains(&duration)
            {
                return Err(GrpcTimeoutConfigError::InvalidField {
                    field,
                    maximum_seconds: maximum.as_secs(),
                });
            }
        }
        Ok(self)
    }

    /// Returns the configured duration for one deadline class.
    #[must_use]
    pub const fn duration(self, class: GrpcTimeoutClass) -> Option<Duration> {
        match class {
            GrpcTimeoutClass::CredentialSetup => self.credential_setup,
            GrpcTimeoutClass::OrdinaryUnary => self.ordinary_unary,
            GrpcTimeoutClass::LongUnary => self.long_unary,
            GrpcTimeoutClass::StreamSetup => self.stream_setup,
            GrpcTimeoutClass::StreamIdle => self.stream_idle,
            GrpcTimeoutClass::StreamLifetime => self.stream_total_lifetime,
            GrpcTimeoutClass::Cleanup => self.cleanup,
        }
    }

    fn from_environment(value: Option<OsString>) -> Result<Self, GrpcTimeoutConfigError> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let value = value
            .into_string()
            .map_err(|_| GrpcTimeoutConfigError::InvalidEnvironment)?;
        if value.is_empty()
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(GrpcTimeoutConfigError::InvalidEnvironment);
        }
        let seconds = value
            .parse::<u64>()
            .map_err(|_| GrpcTimeoutConfigError::InvalidEnvironment)?;
        if seconds > MAX_GRPC_TIMEOUT.as_secs() {
            return Err(GrpcTimeoutConfigError::EnvironmentOutOfRange);
        }
        let inherited = (seconds != 0).then(|| Duration::from_secs(seconds));
        Ok(Self {
            credential_setup: inherited,
            ordinary_unary: inherited,
            long_unary: inherited,
            stream_setup: inherited,
            stream_idle: None,
            stream_total_lifetime: None,
            cleanup: Some(DEFAULT_CLEANUP_GRPC_TIMEOUT),
        })
    }
}

/// Invalid logical gRPC deadline configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcTimeoutConfigError {
    /// A programmatic field was zero, subsecond, or above its finite maximum.
    InvalidField {
        /// Stable field name.
        field: &'static str,
        /// Largest supported whole-second value.
        maximum_seconds: u64,
    },
    /// The process override was not an exact supported ASCII decimal.
    InvalidEnvironment,
    /// The process override exceeded 3,600 seconds.
    EnvironmentOutOfRange,
    /// Absolute deadline arithmetic was not representable.
    UnrepresentableDeadline,
}

impl fmt::Display for GrpcTimeoutConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField {
                field,
                maximum_seconds,
            } => write!(
                formatter,
                "grpc_timeouts.{field} must be disabled or between 1 and {maximum_seconds} seconds"
            ),
            Self::InvalidEnvironment => write!(
                formatter,
                "{ANYTYPE_GRPC_TIMEOUT_SECS} must be an ASCII decimal from 0 through 3600"
            ),
            Self::EnvironmentOutOfRange => write!(
                formatter,
                "{ANYTYPE_GRPC_TIMEOUT_SECS} must not exceed 3600 seconds"
            ),
            Self::UnrepresentableDeadline => {
                formatter.write_str("gRPC absolute deadline is not representable")
            }
        }
    }
}

impl StdError for GrpcTimeoutConfigError {}

/// Closed logical gRPC deadline taxonomy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcTimeoutClass {
    /// Credential-session setup.
    CredentialSetup,
    /// Ordinary unary RPC.
    OrdinaryUnary,
    /// Long unary RPC.
    LongUnary,
    /// Streaming response setup.
    StreamSetup,
    /// Established stream no-progress boundary.
    StreamIdle,
    /// Established stream total lifetime.
    StreamLifetime,
    /// Cleanup RPC.
    Cleanup,
}

impl GrpcTimeoutClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CredentialSetup => "credential_setup",
            Self::OrdinaryUnary => "ordinary_unary",
            Self::LongUnary => "long_unary",
            Self::StreamSetup => "stream_setup",
            Self::StreamIdle => "stream_idle",
            Self::StreamLifetime => "stream_lifetime",
            Self::Cleanup => "cleanup",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "credential_setup" => Some(Self::CredentialSetup),
            "ordinary_unary" => Some(Self::OrdinaryUnary),
            "long_unary" => Some(Self::LongUnary),
            "stream_setup" => Some(Self::StreamSetup),
            "stream_idle" => Some(Self::StreamIdle),
            "stream_lifetime" => Some(Self::StreamLifetime),
            "cleanup" => Some(Self::Cleanup),
            _ => None,
        }
    }
}

impl fmt::Display for GrpcTimeoutClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Effect of a gRPC timeout on an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcTimeoutOutcome {
    /// A read did not complete, or dispatch had not begun when time expired.
    ReadAborted,
    /// A mutation may have reached the server.
    MutationIndeterminate,
    /// An established stream was terminated.
    StreamTerminated,
}

impl GrpcTimeoutOutcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReadAborted => "read_aborted",
            Self::MutationIndeterminate => "mutation_indeterminate",
            Self::StreamTerminated => "stream_terminated",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "read_aborted" => Some(Self::ReadAborted),
            "mutation_indeterminate" => Some(Self::MutationIndeterminate),
            "stream_terminated" => Some(Self::StreamTerminated),
            _ => None,
        }
    }
}

impl fmt::Display for GrpcTimeoutOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Origin of an observed gRPC deadline expiration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrpcTimeoutSource {
    /// The local Tokio absolute deadline expired.
    Local,
    /// The peer returned `DeadlineExceeded`.
    Server,
}

impl fmt::Display for GrpcTimeoutSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Local => "local",
            Self::Server => "server",
        })
    }
}

/// Stable, payload-free gRPC deadline classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrpcDeadlineError {
    /// Expired deadline class.
    pub class: GrpcTimeoutClass,
    /// Safety outcome of the interrupted operation.
    pub outcome: GrpcTimeoutOutcome,
    /// Whether expiration was enforced locally or reported by the peer.
    pub source: GrpcTimeoutSource,
    /// Elapsed local operation time when known.
    pub elapsed: Duration,
}

impl GrpcDeadlineError {
    /// Classifies a deadline status without retaining its message or metadata.
    #[must_use]
    pub fn from_status(
        status: &Status,
        fallback_class: GrpcTimeoutClass,
        fallback_outcome: GrpcTimeoutOutcome,
        elapsed: Duration,
    ) -> Option<Self> {
        if status.code() != Code::DeadlineExceeded {
            return None;
        }
        let class = metadata_text(status.metadata(), DEADLINE_CLASS_METADATA)
            .and_then(GrpcTimeoutClass::from_str)
            .unwrap_or(fallback_class);
        let outcome = metadata_text(status.metadata(), DEADLINE_OUTCOME_METADATA)
            .and_then(GrpcTimeoutOutcome::from_str)
            .unwrap_or(fallback_outcome);
        let source = if metadata_text(status.metadata(), DEADLINE_SOURCE_METADATA) == Some("local")
        {
            GrpcTimeoutSource::Local
        } else {
            GrpcTimeoutSource::Server
        };
        Some(Self {
            class,
            outcome,
            source,
            elapsed,
        })
    }
}

impl fmt::Display for GrpcDeadlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "gRPC deadline expired class={} outcome={} source={} elapsed_ms={}",
            self.class,
            self.outcome,
            self.source,
            self.elapsed.as_millis()
        )
    }
}

impl StdError for GrpcDeadlineError {}

/// An optional absolute budget supplied by an enclosing workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrpcEnclosingDeadline(tokio::time::Instant);

impl GrpcEnclosingDeadline {
    /// Captures an absolute deadline relative to the current Tokio clock.
    pub fn from_now(duration: Duration) -> Result<Self, GrpcTimeoutConfigError> {
        tokio::time::Instant::now()
            .checked_add(duration)
            .map(Self)
            .ok_or(GrpcTimeoutConfigError::UnrepresentableDeadline)
    }

    /// Wraps an already captured Tokio instant.
    #[must_use]
    pub const fn from_instant(deadline: tokio::time::Instant) -> Self {
        Self(deadline)
    }

    /// Returns the captured absolute Tokio instant.
    #[must_use]
    pub const fn instant(self) -> tokio::time::Instant {
        self.0
    }
}

/// Runs gRPC work under one caller-owned absolute deadline.
///
/// The deadline is applied by the transport layer to every generated call,
/// including credential setup and calls whose request options are inferred.
pub async fn scope_grpc_enclosing_deadline<F, T>(deadline: GrpcEnclosingDeadline, operation: F) -> T
where
    F: Future<Output = T>,
{
    ENCLOSING_DEADLINE.scope(deadline, operation).await
}

/// Per-request profile, outcome, and optional enclosing budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrpcCallOptions {
    /// Selected logical profile.
    pub class: GrpcTimeoutClass,
    /// Safety outcome if the call expires after possible dispatch.
    pub outcome: GrpcTimeoutOutcome,
    /// Optional smaller enclosing absolute deadline.
    pub enclosing: Option<GrpcEnclosingDeadline>,
}

impl GrpcCallOptions {
    /// Creates request options for a deadline class and safety outcome.
    #[must_use]
    pub const fn new(class: GrpcTimeoutClass, outcome: GrpcTimeoutOutcome) -> Self {
        Self {
            class,
            outcome,
            enclosing: None,
        }
    }

    /// Adds an enclosing absolute deadline.
    #[must_use]
    pub const fn enclosing(mut self, deadline: GrpcEnclosingDeadline) -> Self {
        self.enclosing = Some(deadline);
        self
    }

    /// Ordinary read options.
    #[must_use]
    pub const fn ordinary_read() -> Self {
        Self::new(
            GrpcTimeoutClass::OrdinaryUnary,
            GrpcTimeoutOutcome::ReadAborted,
        )
    }

    /// Ordinary mutation options.
    #[must_use]
    pub const fn ordinary_mutation() -> Self {
        Self::new(
            GrpcTimeoutClass::OrdinaryUnary,
            GrpcTimeoutOutcome::MutationIndeterminate,
        )
    }

    /// Long read options.
    #[must_use]
    pub const fn long_read() -> Self {
        Self::new(GrpcTimeoutClass::LongUnary, GrpcTimeoutOutcome::ReadAborted)
    }

    /// Stream setup options.
    #[must_use]
    pub const fn stream_setup() -> Self {
        Self::new(
            GrpcTimeoutClass::StreamSetup,
            GrpcTimeoutOutcome::StreamTerminated,
        )
    }

    /// Cleanup mutation options.
    #[must_use]
    pub const fn cleanup() -> Self {
        Self::new(
            GrpcTimeoutClass::Cleanup,
            GrpcTimeoutOutcome::MutationIndeterminate,
        )
    }
}

impl Default for GrpcCallOptions {
    fn default() -> Self {
        // Unknown RPCs are classified conservatively until their owner marks
        // read semantics explicitly.
        Self::ordinary_mutation()
    }
}

/// Adds explicit deadline semantics to a tonic request.
pub fn with_grpc_call_options<T>(mut request: Request<T>, options: GrpcCallOptions) -> Request<T> {
    request.extensions_mut().insert(options);
    request
}

/// Raw response-body progress shared with an established stream controller.
///
/// Tonic preserves this value in the streaming response extensions. Each data
/// frame advances its generation before tonic attempts to decode a message.
#[derive(Clone)]
pub struct GrpcTransportProgress {
    inner: Arc<GrpcTransportProgressInner>,
}

struct GrpcTransportProgressInner {
    generation: AtomicU64,
    notify: tokio::sync::Notify,
}

impl GrpcTransportProgress {
    fn new() -> Self {
        Self {
            inner: Arc::new(GrpcTransportProgressInner {
                generation: AtomicU64::new(0),
                notify: tokio::sync::Notify::new(),
            }),
        }
    }

    fn record(&self) {
        self.inner.generation.fetch_add(1, Ordering::Relaxed);
        self.inner.notify.notify_waiters();
    }

    fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::Relaxed)
    }

    async fn changed_after(&self, generation: u64) -> u64 {
        loop {
            let notified = self.inner.notify.notified();
            let current = self.generation();
            if current != generation {
                return current;
            }
            notified.await;
        }
    }
}

impl fmt::Debug for GrpcTransportProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrpcTransportProgress")
            .field("generation", &self.generation())
            .finish()
    }
}

/// A service wrapper that propagates and locally enforces logical deadlines.
///
/// Generated tonic clients can use this wrapper exactly as they use a channel.
/// Existing shorter `grpc-timeout` metadata is preserved as the winning bound.
#[derive(Clone)]
pub struct GrpcDeadlineService<S> {
    inner: S,
    policy: GrpcTimeoutPolicy,
}

impl<S> fmt::Debug for GrpcDeadlineService<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrpcDeadlineService")
            .field("policy", &self.policy)
            .field("inner", &"redacted")
            .finish()
    }
}

impl<S> GrpcDeadlineService<S> {
    /// Wraps a tonic service after validating its timeout policy.
    pub fn try_new(inner: S, policy: GrpcTimeoutPolicy) -> Result<Self, GrpcTimeoutConfigError> {
        Ok(Self {
            inner,
            policy: policy.validate()?,
        })
    }

    pub(crate) const fn new_resolved(inner: S, policy: GrpcTimeoutPolicy) -> Self {
        Self { inner, policy }
    }

    /// Returns the resolved policy used by this service.
    #[must_use]
    pub const fn policy(&self) -> GrpcTimeoutPolicy {
        self.policy
    }

    /// Returns a reference to the wrapped service.
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.inner
    }
}

/// Service error used by [`GrpcDeadlineService`].
pub enum GrpcDeadlineServiceError<E> {
    /// The wrapped transport failed.
    Transport {
        /// Closed gRPC status classification captured before redaction.
        code: Code,
        /// Retains the transport error type without retaining its payload.
        error_type: PhantomData<fn() -> E>,
    },
    /// A local deadline expired or was exhausted before dispatch.
    Deadline(Status),
    /// Existing `grpc-timeout` metadata was malformed.
    InvalidTimeout(Status),
}

impl<E> fmt::Debug for GrpcDeadlineServiceError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { code, .. } => formatter
                .debug_struct("GrpcDeadlineServiceError::Transport")
                .field("code", code)
                .field("source", &"redacted")
                .finish(),
            Self::Deadline(_) => {
                formatter.write_str("GrpcDeadlineServiceError::Deadline(redacted)")
            }
            Self::InvalidTimeout(_) => {
                formatter.write_str("GrpcDeadlineServiceError::InvalidTimeout(redacted)")
            }
        }
    }
}

impl<E> fmt::Display for GrpcDeadlineServiceError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Transport { .. } => "gRPC transport failed",
            Self::Deadline(_) => "gRPC local deadline expired",
            Self::InvalidTimeout(_) => "invalid gRPC timeout metadata",
        })
    }
}

impl<E> StdError for GrpcDeadlineServiceError<E> {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Transport { code, .. } => Some(redacted_transport_status_source(*code)),
            Self::Deadline(status) => Some(redacted_deadline_status_source(status)),
            Self::InvalidTimeout(_) => Some(redacted_invalid_timeout_source()),
        }
    }
}

impl<E> GrpcDeadlineServiceError<E>
where
    E: StdError + Send + Sync + 'static,
{
    fn transport(source: E) -> Self {
        let code = closed_grpc_error_code(source);
        Self::Transport {
            code,
            error_type: PhantomData,
        }
    }
}

fn closed_grpc_error_code<E>(error: E) -> Code
where
    E: StdError + Send + Sync + 'static,
{
    Status::from_error(Box::new(error)).code()
}

fn redacted_transport_status_source(code: Code) -> &'static Status {
    static SOURCES: [OnceLock<Status>; 17] = [const { OnceLock::new() }; 17];
    let index = match code {
        Code::Ok => 0,
        Code::Cancelled => 1,
        Code::Unknown => 2,
        Code::InvalidArgument => 3,
        Code::DeadlineExceeded => 4,
        Code::NotFound => 5,
        Code::AlreadyExists => 6,
        Code::PermissionDenied => 7,
        Code::ResourceExhausted => 8,
        Code::FailedPrecondition => 9,
        Code::Aborted => 10,
        Code::OutOfRange => 11,
        Code::Unimplemented => 12,
        Code::Internal => 13,
        Code::Unavailable => 14,
        Code::DataLoss => 15,
        Code::Unauthenticated => 16,
    };
    SOURCES[index].get_or_init(|| Status::new(code, "gRPC transport failed (details redacted)"))
}

fn redacted_deadline_status_source(status: &Status) -> &'static Status {
    static SOURCES: [OnceLock<Status>; 21] = [const { OnceLock::new() }; 21];
    let class = metadata_text(status.metadata(), DEADLINE_CLASS_METADATA)
        .and_then(GrpcTimeoutClass::from_str)
        .unwrap_or(GrpcTimeoutClass::OrdinaryUnary);
    let outcome = metadata_text(status.metadata(), DEADLINE_OUTCOME_METADATA)
        .and_then(GrpcTimeoutOutcome::from_str)
        .unwrap_or(GrpcTimeoutOutcome::MutationIndeterminate);
    let class_index = match class {
        GrpcTimeoutClass::CredentialSetup => 0,
        GrpcTimeoutClass::OrdinaryUnary => 1,
        GrpcTimeoutClass::LongUnary => 2,
        GrpcTimeoutClass::StreamSetup => 3,
        GrpcTimeoutClass::StreamIdle => 4,
        GrpcTimeoutClass::StreamLifetime => 5,
        GrpcTimeoutClass::Cleanup => 6,
    };
    let outcome_index = match outcome {
        GrpcTimeoutOutcome::ReadAborted => 0,
        GrpcTimeoutOutcome::MutationIndeterminate => 1,
        GrpcTimeoutOutcome::StreamTerminated => 2,
    };
    SOURCES[class_index * 3 + outcome_index]
        .get_or_init(|| local_deadline_status(GrpcCallOptions::new(class, outcome), Duration::ZERO))
}

fn redacted_invalid_timeout_source() -> &'static Status {
    static SOURCE: OnceLock<Status> = OnceLock::new();
    SOURCE.get_or_init(|| Status::invalid_argument("invalid gRPC timeout metadata"))
}

type BoxServiceFuture<T, E> = Pin<
    Box<
        dyn Future<
                Output = Result<http::Response<GrpcDeadlineBody<T>>, GrpcDeadlineServiceError<E>>,
            > + Send
            + 'static,
    >,
>;

/// Response body that retains a unary RPC's absolute deadline through trailers.
pub struct GrpcDeadlineBody<B> {
    inner: Pin<Box<B>>,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    progress: Option<GrpcTransportProgress>,
    options: GrpcCallOptions,
    started: tokio::time::Instant,
    finished: bool,
}

impl<B> fmt::Debug for GrpcDeadlineBody<B> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrpcDeadlineBody")
            .field("class", &self.options.class)
            .field("outcome", &self.options.outcome)
            .field("deadline_enabled", &self.sleep.is_some())
            .field("progress_enabled", &self.progress.is_some())
            .field("finished", &self.finished)
            .field("inner", &"redacted")
            .finish()
    }
}

impl<B> GrpcDeadlineBody<B> {
    fn new(
        inner: B,
        deadline: Option<tokio::time::Instant>,
        progress: Option<GrpcTransportProgress>,
        options: GrpcCallOptions,
        started: tokio::time::Instant,
    ) -> Self {
        Self {
            inner: Box::pin(inner),
            sleep: deadline.map(|deadline| Box::pin(tokio::time::sleep_until(deadline))),
            progress,
            options,
            started,
            finished: false,
        }
    }
}

impl<B> HttpBody for GrpcDeadlineBody<B>
where
    B: HttpBody,
    B::Error: StdError + Send + Sync + 'static,
{
    type Data = B::Data;
    type Error = GrpcDeadlineServiceError<B::Error>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.as_mut().get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        if let Some(sleep) = this.sleep.as_mut()
            && sleep.as_mut().poll(context).is_ready()
        {
            this.finished = true;
            return Poll::Ready(Some(Err(GrpcDeadlineServiceError::Deadline(
                local_deadline_status(this.options, this.started.elapsed()),
            ))));
        }

        match this.inner.as_mut().poll_frame(context) {
            Poll::Ready(Some(Ok(mut frame))) => {
                if frame.data_ref().is_some_and(|data| data.remaining() > 0)
                    && let Some(progress) = this.progress.as_ref()
                {
                    progress.record();
                }
                if let Some(trailers) = frame.trailers_mut() {
                    annotate_server_deadline_headers(trailers, this.options);
                }
                return Poll::Ready(Some(Ok(frame)));
            }
            Poll::Ready(Some(Err(source))) => {
                this.finished = true;
                return Poll::Ready(Some(Err(GrpcDeadlineServiceError::transport(source))));
            }
            Poll::Ready(None) => {
                this.finished = true;
                return Poll::Ready(None);
            }
            Poll::Pending => {}
        }
        Poll::Pending
    }

    fn is_end_stream(&self) -> bool {
        self.finished || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl<S, ResponseBody> Service<http::Request<Body>> for GrpcDeadlineService<S>
where
    S: Service<http::Request<Body>, Response = http::Response<ResponseBody>>
        + Clone
        + Send
        + 'static,
    S::Error: StdError + Send + Sync + 'static,
    S::Future: Send + 'static,
    ResponseBody: Send + 'static,
{
    type Response = http::Response<GrpcDeadlineBody<ResponseBody>>;
    type Error = GrpcDeadlineServiceError<S::Error>;
    type Future = BoxServiceFuture<ResponseBody, S::Error>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let _ = context;
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: http::Request<Body>) -> Self::Future {
        let mut options = request
            .extensions()
            .get::<GrpcCallOptions>()
            .copied()
            .unwrap_or_else(|| inferred_call_options(request.extensions().get::<GrpcMethod>()));
        if let Ok(scoped) = ENCLOSING_DEADLINE.try_with(|deadline| *deadline) {
            options.enclosing = Some(match options.enclosing {
                Some(explicit) => {
                    GrpcEnclosingDeadline::from_instant(explicit.instant().min(scoped.instant()))
                }
                None => scoped,
            });
        }
        let started = tokio::time::Instant::now();
        let caller_deadline = match request.headers().get(GRPC_TIMEOUT_HEADER) {
            Some(value) => match parse_grpc_timeout(value) {
                Ok(Some(duration)) => match started.checked_add(duration) {
                    Some(deadline) => Some(deadline),
                    None => {
                        let status =
                            Status::invalid_argument("unrepresentable grpc-timeout metadata");
                        return Box::pin(async move {
                            Err(GrpcDeadlineServiceError::InvalidTimeout(status))
                        });
                    }
                },
                Ok(None) => None,
                Err(_) => {
                    let status = Status::invalid_argument("invalid grpc-timeout metadata");
                    return Box::pin(async move {
                        Err(GrpcDeadlineServiceError::InvalidTimeout(status))
                    });
                }
            },
            None => None,
        };
        let selected = select_deadline(
            self.policy.duration(options.class),
            options.enclosing,
            request.headers().get(GRPC_TIMEOUT_HEADER),
            started,
        );
        let duration = match selected {
            Ok(duration) => duration,
            Err(SelectDeadlineError::Expired) => {
                let status = local_deadline_status(
                    GrpcCallOptions {
                        outcome: GrpcTimeoutOutcome::ReadAborted,
                        ..options
                    },
                    Duration::ZERO,
                );
                return Box::pin(async move { Err(GrpcDeadlineServiceError::Deadline(status)) });
            }
            Err(SelectDeadlineError::InvalidHeader) => {
                let status = Status::invalid_argument("invalid grpc-timeout metadata");
                return Box::pin(
                    async move { Err(GrpcDeadlineServiceError::InvalidTimeout(status)) },
                );
            }
        };

        let absolute_deadline = match duration {
            Some(duration) => match started.checked_add(duration) {
                Some(deadline) => Some(deadline),
                None => {
                    let status = Status::invalid_argument("unrepresentable gRPC deadline");
                    return Box::pin(async move {
                        Err(GrpcDeadlineServiceError::InvalidTimeout(status))
                    });
                }
            },
            None => None,
        };
        let mut inner = self.inner.clone();
        Box::pin(async move {
            let ready = async { std::future::poll_fn(|context| inner.poll_ready(context)).await };
            let ready_result = if let Some(deadline) = absolute_deadline {
                tokio::pin!(ready);
                tokio::select! {
                    biased;
                    result = &mut ready => Some(result),
                    () = tokio::time::sleep_until(deadline) => None,
                }
            } else {
                Some(ready.await)
            };
            match ready_result {
                Some(Ok(())) => {}
                Some(Err(source)) if error_chain_contains_timeout(&source) => {
                    let status = local_deadline_status(
                        GrpcCallOptions {
                            outcome: GrpcTimeoutOutcome::ReadAborted,
                            ..options
                        },
                        started.elapsed(),
                    );
                    return Err(GrpcDeadlineServiceError::Deadline(status));
                }
                Some(Err(source)) => return Err(GrpcDeadlineServiceError::transport(source)),
                None => {
                    let status = local_deadline_status(
                        GrpcCallOptions {
                            outcome: GrpcTimeoutOutcome::ReadAborted,
                            ..options
                        },
                        started.elapsed(),
                    );
                    return Err(GrpcDeadlineServiceError::Deadline(status));
                }
            }

            if let Some(deadline) = absolute_deadline {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    let status = local_deadline_status(
                        GrpcCallOptions {
                            outcome: GrpcTimeoutOutcome::ReadAborted,
                            ..options
                        },
                        started.elapsed(),
                    );
                    return Err(GrpcDeadlineServiceError::Deadline(status));
                }
                let propagated = if options.class == GrpcTimeoutClass::StreamSetup {
                    caller_deadline
                        .map(|deadline| {
                            deadline.saturating_duration_since(tokio::time::Instant::now())
                        })
                        .filter(|duration| !duration.is_zero())
                } else {
                    Some(remaining)
                };
                if let Some(propagated) = propagated {
                    let value = grpc_timeout_value(propagated).ok_or_else(|| {
                        GrpcDeadlineServiceError::InvalidTimeout(Status::invalid_argument(
                            "unrepresentable grpc-timeout metadata",
                        ))
                    })?;
                    let header = http::HeaderValue::from_str(&value).map_err(|_| {
                        GrpcDeadlineServiceError::InvalidTimeout(Status::invalid_argument(
                            "invalid grpc-timeout metadata",
                        ))
                    })?;
                    request.headers_mut().insert(GRPC_TIMEOUT_HEADER, header);
                }
            }

            let future = inner.call(request);
            let result = if let Some(deadline) = absolute_deadline {
                tokio::pin!(future);
                tokio::select! {
                    biased;
                    result = &mut future => Some(result),
                    () = tokio::time::sleep_until(deadline) => None,
                }
            } else {
                Some(future.await)
            };

            match result {
                Some(Ok(mut response)) => {
                    sanitize_response_deadline_headers(response.headers_mut(), options);
                    let body_deadline = (options.class != GrpcTimeoutClass::StreamSetup)
                        .then_some(absolute_deadline)
                        .flatten();
                    let progress = (options.class == GrpcTimeoutClass::StreamSetup)
                        .then(GrpcTransportProgress::new);
                    if let Some(progress) = progress.as_ref() {
                        response.extensions_mut().insert(progress.clone());
                    }
                    Ok(response.map(|body| {
                        GrpcDeadlineBody::new(body, body_deadline, progress, options, started)
                    }))
                }
                Some(Err(source)) if error_chain_contains_timeout(&source) => {
                    Err(GrpcDeadlineServiceError::Deadline(local_deadline_status(
                        options,
                        started.elapsed(),
                    )))
                }
                Some(Err(source)) => Err(GrpcDeadlineServiceError::transport(source)),
                None => Err(GrpcDeadlineServiceError::Deadline(local_deadline_status(
                    options,
                    started.elapsed(),
                ))),
            }
        })
    }
}

/// Established-stream idle and lifetime controller.
#[derive(Debug)]
pub struct GrpcStreamDeadline {
    idle: Option<Duration>,
    idle_deadline: Option<tokio::time::Instant>,
    lifetime_deadline: Option<tokio::time::Instant>,
    enclosing: Option<GrpcEnclosingDeadline>,
    transport_progress: Option<GrpcTransportProgress>,
    observed_transport_generation: u64,
    started: tokio::time::Instant,
}

/// Error returned while waiting for established-stream progress.
pub enum GrpcStreamError {
    /// An idle, lifetime, enclosing, or peer deadline expired.
    Deadline(GrpcDeadlineError),
    /// The stream failed for a reason other than deadline expiration.
    Status(Status),
}

impl fmt::Debug for GrpcStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline(source) => formatter
                .debug_tuple("GrpcStreamError::Deadline")
                .field(source)
                .finish(),
            Self::Status(_) => formatter.write_str("GrpcStreamError::Status(redacted)"),
        }
    }
}

impl fmt::Display for GrpcStreamError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadline(source) => source.fmt(formatter),
            Self::Status(_) => formatter.write_str("gRPC stream failed (details redacted)"),
        }
    }
}

impl StdError for GrpcStreamError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Deadline(source) => Some(source),
            Self::Status(_) => None,
        }
    }
}

impl GrpcStreamDeadline {
    /// Starts established-stream boundaries after successful response headers.
    pub fn new(
        policy: GrpcTimeoutPolicy,
        enclosing: Option<GrpcEnclosingDeadline>,
    ) -> Result<Self, GrpcTimeoutConfigError> {
        let started = tokio::time::Instant::now();
        let idle = policy.stream_idle;
        let idle_deadline = checked_deadline(started, idle)?;
        let lifetime_deadline = checked_deadline(started, policy.stream_total_lifetime)?;
        Ok(Self {
            idle,
            idle_deadline,
            lifetime_deadline,
            enclosing,
            transport_progress: None,
            observed_transport_generation: 0,
            started,
        })
    }

    /// Connects raw body progress supplied in a tonic streaming response.
    #[must_use]
    pub fn with_transport_progress(mut self, progress: Option<GrpcTransportProgress>) -> Self {
        self.set_transport_progress(progress);
        self
    }

    /// Replaces the raw body progress source after a successful stream reopen.
    pub fn set_transport_progress(&mut self, progress: Option<GrpcTransportProgress>) {
        self.observed_transport_generation = progress
            .as_ref()
            .map_or(0, GrpcTransportProgress::generation);
        self.transport_progress = progress;
    }

    /// Restarts only the idle boundary after a successful stream reopen.
    ///
    /// The total lifetime and enclosing absolute deadline remain unchanged.
    pub fn reset_idle(&mut self) -> Result<(), GrpcTimeoutConfigError> {
        self.idle_deadline = checked_deadline(tokio::time::Instant::now(), self.idle)?;
        if let Some(progress) = self.transport_progress.as_ref() {
            self.observed_transport_generation = progress.generation();
        }
        Ok(())
    }

    /// Records delivery of a decoded message when raw body progress is unavailable.
    ///
    /// When transport progress is attached, its DATA frame already advanced the
    /// idle boundary and decoding must not grant a second idle window.
    pub fn observe_decoded_message(&mut self) -> Result<(), GrpcTimeoutConfigError> {
        if self.transport_progress.is_some() {
            Ok(())
        } else {
            self.reset_idle()
        }
    }

    /// Returns the earliest absolute lifetime or enclosing workflow boundary.
    #[must_use]
    pub fn workflow_deadline(&self) -> Option<GrpcEnclosingDeadline> {
        [
            self.lifetime_deadline,
            self.enclosing.map(GrpcEnclosingDeadline::instant),
        ]
        .into_iter()
        .flatten()
        .min()
        .map(GrpcEnclosingDeadline::from_instant)
    }

    /// Runs reconnect or replay work under the retained lifetime/workflow bound.
    ///
    /// This boundary does not reset the idle timer because reconnect work is not
    /// established-stream transport progress.
    pub async fn phase<T, F>(&self, future: F) -> Result<T, GrpcStreamError>
    where
        F: Future<Output = T>,
    {
        let Some(deadline) = self.workflow_deadline().map(GrpcEnclosingDeadline::instant) else {
            return Ok(future.await);
        };
        if deadline <= tokio::time::Instant::now() {
            return Err(GrpcStreamError::Deadline(self.stream_error(
                GrpcTimeoutClass::StreamLifetime,
                GrpcTimeoutSource::Local,
            )));
        }
        tokio::pin!(future);
        tokio::select! {
            biased;
            () = tokio::time::sleep_until(deadline) => Err(GrpcStreamError::Deadline(
                self.stream_error(GrpcTimeoutClass::StreamLifetime, GrpcTimeoutSource::Local),
            )),
            value = &mut future => Ok(value),
        }
    }

    /// Runs established-stream work under idle, lifetime, and enclosing bounds.
    ///
    /// Raw nonempty DATA progress resets idle while the work is pending. Work
    /// completion itself does not count as stream progress.
    pub async fn established_phase<T, F>(&mut self, future: F) -> Result<T, GrpcStreamError>
    where
        F: Future<Output = T>,
    {
        tokio::pin!(future);
        loop {
            self.observe_transport_progress()?;
            let now = tokio::time::Instant::now();
            let boundary = earliest_stream_boundary(
                self.idle_deadline,
                self.lifetime_deadline,
                self.enclosing.map(GrpcEnclosingDeadline::instant),
            );
            if let Some((deadline, class)) = boundary
                && deadline <= now
            {
                return Err(GrpcStreamError::Deadline(
                    self.stream_error(class, GrpcTimeoutSource::Local),
                ));
            }

            let progress = self.transport_progress.clone();
            let observed = self.observed_transport_generation;
            let progress_wait = async move {
                match progress {
                    Some(progress) => progress.changed_after(observed).await,
                    None => std::future::pending().await,
                }
            };
            let result = if let Some((deadline, class)) = boundary {
                tokio::select! {
                    biased;
                    () = tokio::time::sleep_until(deadline) => {
                        return Err(GrpcStreamError::Deadline(
                            self.stream_error(class, GrpcTimeoutSource::Local),
                        ));
                    }
                    value = &mut future => return Ok(value),
                    generation = progress_wait => generation,
                }
            } else {
                tokio::select! {
                    biased;
                    value = &mut future => return Ok(value),
                    generation = progress_wait => generation,
                }
            };
            self.observed_transport_generation = result;
            self.idle_deadline =
                checked_deadline(tokio::time::Instant::now(), self.idle).map_err(|_| {
                    GrpcStreamError::Deadline(
                        self.stream_error(GrpcTimeoutClass::StreamIdle, GrpcTimeoutSource::Local),
                    )
                })?;
        }
    }

    /// Waits for the next transport-progress future and resets idle on success.
    pub async fn next<T, F>(&mut self, future: F) -> Result<T, GrpcStreamError>
    where
        F: Future<Output = Result<T, Status>>,
    {
        tokio::pin!(future);
        loop {
            self.observe_transport_progress()?;
            let now = tokio::time::Instant::now();
            let boundary = earliest_stream_boundary(
                self.idle_deadline,
                self.lifetime_deadline,
                self.enclosing.map(GrpcEnclosingDeadline::instant),
            );
            if let Some((deadline, class)) = boundary
                && deadline <= now
            {
                return Err(GrpcStreamError::Deadline(
                    self.stream_error(class, GrpcTimeoutSource::Local),
                ));
            }

            let progress = self.transport_progress.clone();
            let observed = self.observed_transport_generation;
            let progress_wait = async move {
                match progress {
                    Some(progress) => progress.changed_after(observed).await,
                    None => std::future::pending().await,
                }
            };
            let result = if let Some((deadline, class)) = boundary {
                tokio::select! {
                    biased;
                    () = tokio::time::sleep_until(deadline) => None,
                    result = &mut future => Some(StreamWait::Result(result, class)),
                    generation = progress_wait => Some(StreamWait::Progress(generation)),
                }
            } else {
                tokio::select! {
                    biased;
                    result = &mut future => Some(StreamWait::Result(
                        result,
                        GrpcTimeoutClass::StreamLifetime,
                    )),
                    generation = progress_wait => Some(StreamWait::Progress(generation)),
                }
            };

            match result {
                Some(StreamWait::Progress(generation)) => {
                    self.observed_transport_generation = generation;
                    self.idle_deadline =
                        checked_deadline(tokio::time::Instant::now(), self.idle).map_err(|_| {
                            GrpcStreamError::Deadline(self.stream_error(
                                GrpcTimeoutClass::StreamIdle,
                                GrpcTimeoutSource::Local,
                            ))
                        })?;
                }
                Some(StreamWait::Result(Ok(value), _)) => {
                    self.idle_deadline =
                        checked_deadline(tokio::time::Instant::now(), self.idle).map_err(|_| {
                            GrpcStreamError::Deadline(self.stream_error(
                                GrpcTimeoutClass::StreamIdle,
                                GrpcTimeoutSource::Local,
                            ))
                        })?;
                    return Ok(value);
                }
                Some(StreamWait::Result(Err(status), class)) => {
                    return if let Some(error) = GrpcDeadlineError::from_status(
                        &status,
                        class,
                        GrpcTimeoutOutcome::StreamTerminated,
                        self.started.elapsed(),
                    ) {
                        Err(GrpcStreamError::Deadline(error))
                    } else {
                        Err(GrpcStreamError::Status(status))
                    };
                }
                None => {
                    let (_, class) = boundary.unwrap_or((now, GrpcTimeoutClass::StreamLifetime));
                    return Err(GrpcStreamError::Deadline(
                        self.stream_error(class, GrpcTimeoutSource::Local),
                    ));
                }
            }
        }
    }

    fn observe_transport_progress(&mut self) -> Result<(), GrpcStreamError> {
        let Some(progress) = self.transport_progress.as_ref() else {
            return Ok(());
        };
        let generation = progress.generation();
        if generation == self.observed_transport_generation {
            return Ok(());
        }
        self.observed_transport_generation = generation;
        self.idle_deadline =
            checked_deadline(tokio::time::Instant::now(), self.idle).map_err(|_| {
                GrpcStreamError::Deadline(
                    self.stream_error(GrpcTimeoutClass::StreamIdle, GrpcTimeoutSource::Local),
                )
            })?;
        Ok(())
    }

    fn stream_error(
        &self,
        class: GrpcTimeoutClass,
        source: GrpcTimeoutSource,
    ) -> GrpcDeadlineError {
        GrpcDeadlineError {
            class,
            outcome: GrpcTimeoutOutcome::StreamTerminated,
            source,
            elapsed: self.started.elapsed(),
        }
    }
}

enum StreamWait<T> {
    Result(Result<T, Status>, GrpcTimeoutClass),
    Progress(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectDeadlineError {
    Expired,
    InvalidHeader,
}

fn select_deadline(
    profile: Option<Duration>,
    enclosing: Option<GrpcEnclosingDeadline>,
    existing: Option<&http::HeaderValue>,
    now: tokio::time::Instant,
) -> Result<Option<Duration>, SelectDeadlineError> {
    let existing = existing.map(parse_grpc_timeout).transpose()?.flatten();
    let enclosing = enclosing
        .map(GrpcEnclosingDeadline::instant)
        .map(|deadline| {
            (deadline > now)
                .then(|| deadline.saturating_duration_since(now))
                .ok_or(SelectDeadlineError::Expired)
        })
        .transpose()?;
    let selected = [profile, enclosing, existing].into_iter().flatten().min();
    if selected == Some(Duration::ZERO) {
        return Err(SelectDeadlineError::Expired);
    }
    Ok(selected)
}

fn parse_grpc_timeout(value: &http::HeaderValue) -> Result<Option<Duration>, SelectDeadlineError> {
    let value = value
        .to_str()
        .map_err(|_| SelectDeadlineError::InvalidHeader)?;
    if value.is_empty() || value.len() > 9 {
        return Err(SelectDeadlineError::InvalidHeader);
    }
    let (digits, unit) = value.split_at(value.len() - 1);
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SelectDeadlineError::InvalidHeader);
    }
    let amount = digits
        .parse::<u64>()
        .map_err(|_| SelectDeadlineError::InvalidHeader)?;
    let duration = match unit {
        "H" => Duration::from_secs(amount.saturating_mul(3_600)),
        "M" => Duration::from_secs(amount.saturating_mul(60)),
        "S" => Duration::from_secs(amount),
        "m" => Duration::from_millis(amount),
        "u" => Duration::from_micros(amount),
        "n" => Duration::from_nanos(amount),
        _ => return Err(SelectDeadlineError::InvalidHeader),
    };
    Ok(Some(duration))
}

fn grpc_timeout_value(duration: Duration) -> Option<String> {
    fn format_unit(value: u128, unit: char) -> Option<String> {
        (value <= 99_999_999).then(|| format!("{value}{unit}"))
    }

    format_unit(duration.as_nanos(), 'n')
        .or_else(|| format_unit(duration.as_micros(), 'u'))
        .or_else(|| format_unit(duration.as_millis(), 'm'))
        .or_else(|| format_unit(duration.as_secs().into(), 'S'))
        .or_else(|| format_unit((duration.as_secs() / 60).into(), 'M'))
        .or_else(|| format_unit((duration.as_secs() / 3_600).into(), 'H'))
}

fn local_deadline_status(options: GrpcCallOptions, _elapsed: Duration) -> Status {
    let mut metadata = MetadataMap::new();
    metadata.insert(
        DEADLINE_SOURCE_METADATA,
        MetadataValue::<Ascii>::from_static("local"),
    );
    if let Ok(value) = options.class.as_str().parse() {
        metadata.insert(DEADLINE_CLASS_METADATA, value);
    }
    if let Ok(value) = options.outcome.as_str().parse() {
        metadata.insert(DEADLINE_OUTCOME_METADATA, value);
    }
    Status::with_metadata(Code::DeadlineExceeded, "gRPC deadline exceeded", metadata)
}

fn annotate_server_deadline_headers(headers: &mut http::HeaderMap, options: GrpcCallOptions) {
    headers.insert(
        DEADLINE_SOURCE_METADATA,
        http::HeaderValue::from_static("server"),
    );
    if let Ok(value) = http::HeaderValue::from_str(options.class.as_str()) {
        headers.insert(DEADLINE_CLASS_METADATA, value);
    }
    if let Ok(value) = http::HeaderValue::from_str(options.outcome.as_str()) {
        headers.insert(DEADLINE_OUTCOME_METADATA, value);
    }
}

fn sanitize_response_deadline_headers(headers: &mut http::HeaderMap, options: GrpcCallOptions) {
    let has_terminal_status = headers.contains_key("grpc-status");
    headers.remove(DEADLINE_SOURCE_METADATA);
    headers.remove(DEADLINE_CLASS_METADATA);
    headers.remove(DEADLINE_OUTCOME_METADATA);
    if has_terminal_status {
        annotate_server_deadline_headers(headers, options);
    }
}

macro_rules! generated_method_profiles {
    (
        credential: [$($credential:literal),* $(,)?],
        stream: [$($stream:literal),* $(,)?],
        cleanup: [$($cleanup:literal),* $(,)?],
        long_read: [$($long_read:literal),* $(,)?],
        long_mutation: [$($long_mutation:literal),* $(,)?],
        ordinary_read: [$($ordinary_read:literal),* $(,)?],
        ordinary_mutation: [$($ordinary_mutation:literal),* $(,)?],
    ) => {
        fn reviewed_generated_call_options(method: &str) -> Option<GrpcCallOptions> {
            Some(match method {
                $($credential => GrpcCallOptions::new(
                    GrpcTimeoutClass::CredentialSetup,
                    GrpcTimeoutOutcome::MutationIndeterminate,
                ),)*
                $($stream => GrpcCallOptions::stream_setup(),)*
                $($cleanup => GrpcCallOptions::cleanup(),)*
                $($long_read => GrpcCallOptions::long_read(),)*
                $($long_mutation => GrpcCallOptions::new(
                    GrpcTimeoutClass::LongUnary,
                    GrpcTimeoutOutcome::MutationIndeterminate,
                ),)*
                $($ordinary_read => GrpcCallOptions::ordinary_read(),)*
                $($ordinary_mutation => GrpcCallOptions::ordinary_mutation(),)*
                _ => return None,
            })
        }

        #[cfg(test)]
        const REVIEWED_GENERATED_METHODS: &[&str] = &[
            $($credential,)*
            $($stream,)*
            $($cleanup,)*
            $($long_read,)*
            $($long_mutation,)*
            $($ordinary_read,)*
            $($ordinary_mutation,)*
        ];
    };
}

generated_method_profiles! {
    credential: [
        "AccountLocalLinkNewChallenge",
        "AccountLocalLinkSolveChallenge",
        "WalletCreateSession",
    ],
    stream: [
        "ListenSessionEvents",
    ],
    cleanup: [
        "WalletCloseSession",
        "AccountMigrateCancel",
        "SpaceJoinCancel",
        "ObjectClose",
        "ObjectCrossSpaceSearchUnsubscribe",
        "ObjectSearchUnsubscribe",
        "FileDiscardPreload",
        "FileCacheCancelDownload",
        "ProcessCancel",
        "ProcessUnsubscribe",
        "ChatUnsubscribe",
        "ChatUnsubscribeFromMessagePreviews",
    ],
    long_read: [
        "WorkspaceExport",
        "ObjectListExport",
        "ObjectExport",
        "TemplateExportAll",
        "BlockExport",
        "DebugExportLocalstore",
        "DebugExportReport",
        "FileDownload",
    ],
    long_mutation: [
        "AccountRecoverFromLegacyExport",
        "ObjectImport",
        "ObjectImportUseCase",
        "ObjectImportExperience",
        "BlockUpload",
        "FileUpload",
    ],
    ordinary_read: [
        "AppGetVersion",
        "AccountLocalLinkListApps",
        "WorkspaceGetCurrent",
        "WorkspaceGetAll",
        "SpaceInviteGetCurrent",
        "SpaceInviteGetGuest",
        "SpaceInviteView",
        "PublishingList",
        "PublishingResolveUri",
        "PublishingGetStatus",
        "ObjectShow",
        "ObjectGraph",
        "ObjectSearch",
        "ObjectSearchWithMeta",
        "ObjectCleanupSuggestions",
        "ObjectImportList",
        "ObjectImportNotionValidateToken",
        "ObjectDateByTimestamp",
        "RelationOptions",
        "RelationListWithValue",
        "ObjectRelationListAvailable",
        "ObjectTypeListConflictingRelations",
        "HistoryShowVersion",
        "HistoryGetVersions",
        "HistoryDiffVersions",
        "FileSpaceUsage",
        "FileNodeUsage",
        "NavigationListObjects",
        "NavigationGetObjectInfoWithLinks",
        "TemplateGetPlaceholders",
        "LinkPreview",
        "UnsplashSearch",
        "UnsplashDownload",
        "GalleryDownloadManifest",
        "GalleryDownloadIndex",
        "BlockPreview",
        "DebugStat",
        "DebugTree",
        "DebugTreeHeads",
        "DebugSpaceSummary",
        "DebugStackGoroutines",
        "DebugPing",
        "DebugSubscriptions",
        "DebugOpenedObjects",
        "DebugAccountSelectTrace",
        "DebugAnystoreObjectChanges",
        "DebugNetCheck",
        "NotificationList",
        "MembershipGetStatus",
        "MembershipIsNameValid",
        "MembershipGetPortalLinkUrl",
        "MembershipGetVerificationEmailStatus",
        "MembershipGetTiers",
        "MembershipCodeGetInfo",
        "MembershipV2GetProducts",
        "MembershipV2GetStatus",
        "MembershipV2GetPortalLink",
        "MembershipV2AnyNameIsValid",
        "MembershipV2CartGet",
        "NameServiceUserAccountGet",
        "NameServiceResolveName",
        "NameServiceResolveAnyId",
        "DeviceList",
        "ChatGetMessages",
        "ChatGetMessagesByIds",
        "ChatUnreadMessages",
        "ChatReadReactions",
        "ChatSearch",
        "ChatGetPinnedMessages",
        "AIWritingTools",
        "AIAutofill",
        "AIListSummary",
    ],
    ordinary_mutation: [
        "AppSetDeviceState",
        "AppShutdown",
        "WalletCreate",
        "WalletRecover",
        "WalletConvert",
        "AccountLocalLinkCreateApp",
        "AccountLocalLinkRevokeApp",
        "WorkspaceCreate",
        "WorkspaceOpen",
        "WorkspaceObjectAdd",
        "WorkspaceObjectListAdd",
        "WorkspaceObjectListRemove",
        "WorkspaceSelect",
        "WorkspaceSetInfo",
        "WorkspaceSetHomepage",
        "AccountRecover",
        "AccountMigrate",
        "AccountCreate",
        "AccountDelete",
        "AccountPreloadRemainingSpaces",
        "AccountRevertDeletion",
        "AccountSelect",
        "AccountEnableLocalNetworkSync",
        "AccountChangeJsonApiAddr",
        "AccountStop",
        "AccountMove",
        "AccountConfigUpdate",
        "AccountChangeNetworkConfigAndRestart",
        "SpaceDelete",
        "SpaceInviteGenerate",
        "SpaceInviteChange",
        "SpaceInviteRevoke",
        "SpaceJoin",
        "SpaceStopSharing",
        "SpaceRequestApprove",
        "SpaceRequestDecline",
        "SpaceLeaveApprove",
        "SpaceMakeShareable",
        "SpaceParticipantRemove",
        "SpaceParticipantPermissionsChange",
        "SpaceSetOrder",
        "SpaceUnsetOrder",
        "SpaceChangeOwnership",
        "SpaceDeleteCorruptedBackup",
        "SpaceParticipantsAddList",
        "PublishingCreate",
        "PublishingRemove",
        "ObjectOpen",
        "ObjectRefresh",
        "ObjectCreate",
        "ObjectCreateBookmark",
        "ObjectCreateFromUrl",
        "ObjectCreateSet",
        "ObjectSearchSubscribe",
        "ObjectCrossSpaceSearchSubscribe",
        "ObjectSubscribeIds",
        "ObjectGroupsSubscribe",
        "ObjectSetDetails",
        "ObjectDuplicate",
        "ObjectSetObjectType",
        "ObjectSetLayout",
        "ObjectSetInternalFlags",
        "ObjectSetIsFavorite",
        "ObjectSetIsArchived",
        "ObjectSetSource",
        "ObjectListDuplicate",
        "ObjectListDelete",
        "ObjectListSetIsArchived",
        "ObjectCleanupSuggestionIgnore",
        "ObjectListSetIsFavorite",
        "ObjectListSetObjectType",
        "ObjectListSetDetails",
        "ObjectListModifyDetailValues",
        "ObjectApplyTemplate",
        "ObjectToSet",
        "ObjectToCollection",
        "ObjectShareByLink",
        "ObjectUndo",
        "ObjectRedo",
        "ObjectBookmarkFetch",
        "ObjectCollectionAdd",
        "ObjectCollectionRemove",
        "ObjectCollectionSort",
        "ObjectCreateRelation",
        "ObjectCreateRelationOption",
        "RelationListRemoveOption",
        "RelationOptionSetOrder",
        "ObjectRelationAdd",
        "ObjectRelationDelete",
        "ObjectRelationAddFeatured",
        "ObjectRelationRemoveFeatured",
        "ObjectCreateObjectType",
        "ObjectTypeRelationAdd",
        "ObjectTypeRelationRemove",
        "ObjectTypeRecommendedRelationsSet",
        "ObjectTypeRecommendedFeaturedRelationsSet",
        "ObjectTypeResolveLayoutConflicts",
        "ObjectTypeSetOrder",
        "HistorySetVersion",
        "FileSpaceOffload",
        "FileReconcile",
        "FileListOffload",
        "FileDrop",
        "FileSetAutoDownload",
        "FileCacheDownload",
        "FileAutoDownloadSetLimit",
        "TemplateCreateFromObject",
        "TemplateClone",
        "TemplateSetPlaceholders",
        "TemplateDeletePlaceholders",
        "BlockReplace",
        "BlockCreate",
        "BlockSplit",
        "BlockMerge",
        "BlockCopy",
        "BlockPaste",
        "BlockCut",
        "BlockSetFields",
        "BlockSetCarriage",
        "BlockListDelete",
        "BlockListMoveToExistingObject",
        "BlockListMoveToNewObject",
        "BlockListConvertToObjects",
        "BlockListSetFields",
        "BlockListDuplicate",
        "BlockListSetBackgroundColor",
        "BlockListSetAlign",
        "BlockListSetVerticalAlign",
        "BlockListTurnInto",
        "BlockTextSetText",
        "BlockTextSetColor",
        "BlockTextSetStyle",
        "BlockTextSetChecked",
        "BlockTextSetIcon",
        "BlockTextListSetColor",
        "BlockTextListSetMark",
        "BlockTextListSetStyle",
        "BlockTextListClearStyle",
        "BlockTextListClearContent",
        "BlockFileSetName",
        "BlockFileSetTargetObjectId",
        "BlockImageSetName",
        "BlockVideoSetName",
        "BlockFileCreateAndUpload",
        "BlockFileListSetStyle",
        "BlockDataviewViewCreate",
        "BlockDataviewViewDelete",
        "BlockDataviewViewUpdate",
        "BlockDataviewViewSetActive",
        "BlockDataviewViewSetPosition",
        "BlockDataviewSetSource",
        "BlockDataviewRelationSet",
        "BlockDataviewRelationAdd",
        "BlockDataviewRelationDelete",
        "BlockDataviewGroupOrderUpdate",
        "BlockDataviewObjectOrderUpdate",
        "BlockDataviewObjectOrderMove",
        "BlockDataviewCreateFromExistingObject",
        "BlockDataviewFilterAdd",
        "BlockDataviewFilterRemove",
        "BlockDataviewFilterReplace",
        "BlockDataviewFilterSort",
        "BlockDataviewSortAdd",
        "BlockDataviewSortRemove",
        "BlockDataviewSortReplace",
        "BlockDataviewSortSort",
        "BlockDataviewViewRelationAdd",
        "BlockDataviewViewRelationRemove",
        "BlockDataviewViewRelationReplace",
        "BlockDataviewViewRelationSort",
        "BlockTableCreate",
        "BlockTableExpand",
        "BlockTableRowCreate",
        "BlockTableRowDelete",
        "BlockTableRowDuplicate",
        "BlockTableRowSetHeader",
        "BlockTableColumnCreate",
        "BlockTableColumnMove",
        "BlockTableColumnDelete",
        "BlockTableColumnDuplicate",
        "BlockTableRowListFill",
        "BlockTableRowListClean",
        "BlockTableColumnListFill",
        "BlockTableSort",
        "BlockCreateWidget",
        "BlockWidgetSetTargetId",
        "BlockWidgetSetLayout",
        "BlockWidgetSetLimit",
        "BlockWidgetSetViewId",
        "BlockLinkCreateWithObject",
        "BlockLinkListSetAppearance",
        "BlockBookmarkFetch",
        "BlockBookmarkCreateAndFetch",
        "BlockRelationSetKey",
        "BlockRelationAdd",
        "BlockDivListSetStyle",
        "BlockLatexSetText",
        "ProcessSubscribe",
        "LogSend",
        "DebugRunProfiler",
        "DebugCleanupReport",
        "InitialSetParameters",
        "NotificationReply",
        "NotificationTest",
        "MembershipRegisterPaymentRequest",
        "MembershipGetVerificationEmail",
        "MembershipVerifyEmailCode",
        "MembershipFinalize",
        "MembershipVerifyAppStoreReceipt",
        "MembershipCodeRedeem",
        "MembershipV2AnyNameAllocate",
        "MembershipV2CartUpdate",
        "MembershipV2SubscribeToUpdates",
        "BroadcastPayloadEvent",
        "DeviceSetName",
        "DeviceNetworkStateSet",
        "ChatAddMessage",
        "ChatEditMessageContent",
        "ChatToggleMessageReaction",
        "ChatDeleteMessage",
        "ChatSubscribeLastMessages",
        "ChatReadMessages",
        "ChatSubscribeToMessagePreviews",
        "ObjectChatAdd",
        "ObjectAddDiscussion",
        "ChatReadAll",
        "ChatSetPinnedMessages",
        "ChatAddNotificationSubscriber",
        "ChatRemoveNotificationSubscriber",
        "AIObjectCreateFromUrl",
        "PushNotificationRegisterToken",
        "PushNotificationSetSpaceMode",
        "PushNotificationSetForceModeIds",
        "PushNotificationResetIds",
    ],
}

fn inferred_call_options(method: Option<&GrpcMethod<'_>>) -> GrpcCallOptions {
    let Some(method) = method else {
        return GrpcCallOptions::default();
    };
    if method.service() != "anytype.ClientCommands" {
        return GrpcCallOptions::default();
    }
    reviewed_generated_call_options(method.method()).unwrap_or_default()
}

fn metadata_text<'a>(metadata: &'a MetadataMap, key: &'static str) -> Option<&'a str> {
    metadata.get(key).and_then(|value| value.to_str().ok())
}

fn error_chain_contains_timeout(error: &(dyn StdError + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(current) = source {
        if current.downcast_ref::<TimeoutExpired>().is_some() {
            return true;
        }
        source = current.source();
    }
    false
}

fn checked_deadline(
    start: tokio::time::Instant,
    duration: Option<Duration>,
) -> Result<Option<tokio::time::Instant>, GrpcTimeoutConfigError> {
    duration
        .map(|duration| {
            start
                .checked_add(duration)
                .ok_or(GrpcTimeoutConfigError::UnrepresentableDeadline)
        })
        .transpose()
}

fn earliest_stream_boundary(
    idle: Option<tokio::time::Instant>,
    lifetime: Option<tokio::time::Instant>,
    enclosing: Option<tokio::time::Instant>,
) -> Option<(tokio::time::Instant, GrpcTimeoutClass)> {
    [
        idle.map(|deadline| (deadline, GrpcTimeoutClass::StreamIdle)),
        lifetime.map(|deadline| (deadline, GrpcTimeoutClass::StreamLifetime)),
        enclosing.map(|deadline| (deadline, GrpcTimeoutClass::StreamLifetime)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(deadline, _)| *deadline)
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use tokio::sync::oneshot;

    use super::*;

    #[derive(Clone)]
    struct ScriptService {
        calls: Arc<AtomicUsize>,
        header: Arc<Mutex<Option<oneshot::Sender<Option<String>>>>>,
    }

    #[derive(Debug)]
    struct PendingBody;

    impl HttpBody for PendingBody {
        type Data = tonic::codegen::Bytes;
        type Error = Infallible;

        fn poll_frame(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Pending
        }

        fn is_end_stream(&self) -> bool {
            false
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    struct StatusErrorBody {
        status: Option<Status>,
    }

    #[derive(Debug)]
    struct HostileTransportError(&'static str);

    impl fmt::Display for HostileTransportError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl StdError for HostileTransportError {}

    impl HttpBody for StatusErrorBody {
        type Data = tonic::codegen::Bytes;
        type Error = Status;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(self.status.take().map(Err))
        }
    }

    #[derive(Clone)]
    struct StatusReadyFailureService;

    impl Service<http::Request<Body>> for StatusReadyFailureService {
        type Response = http::Response<Body>;
        type Error = Status;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(Status::unavailable("HOSTILE_READINESS_SECRET")))
        }

        fn call(&mut self, _request: http::Request<Body>) -> Self::Future {
            std::future::ready(Ok(http::Response::new(Body::empty())))
        }
    }

    #[derive(Debug)]
    struct TrailerBody {
        frame: Option<Frame<tonic::codegen::Bytes>>,
    }

    impl HttpBody for TrailerBody {
        type Data = tonic::codegen::Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            Poll::Ready(self.frame.take().map(Ok))
        }

        fn is_end_stream(&self) -> bool {
            self.frame.is_none()
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    #[derive(Debug)]
    struct SignaledDataBody {
        ready: oneshot::Receiver<()>,
        data: tonic::codegen::Bytes,
        emitted: bool,
    }

    impl HttpBody for SignaledDataBody {
        type Data = tonic::codegen::Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            if self.emitted {
                return Poll::Pending;
            }
            match Pin::new(&mut self.ready).poll(context) {
                Poll::Ready(_) => {
                    self.emitted = true;
                    let data = std::mem::take(&mut self.data);
                    Poll::Ready(Some(Ok(Frame::data(data))))
                }
                Poll::Pending => Poll::Pending,
            }
        }

        fn is_end_stream(&self) -> bool {
            false
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    #[derive(Clone)]
    struct PendingReadyService {
        calls: Arc<AtomicUsize>,
    }

    impl Service<http::Request<Body>> for PendingReadyService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn call(&mut self, _request: http::Request<Body>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Ok(http::Response::new(Body::empty())))
        }
    }

    #[derive(Clone)]
    struct ReadyAtDeadlineService {
        calls: Arc<AtomicUsize>,
        ready: Arc<Mutex<Pin<Box<tokio::time::Sleep>>>>,
        header: Arc<Mutex<Option<oneshot::Sender<Option<String>>>>>,
    }

    impl Service<http::Request<Body>> for ReadyAtDeadlineService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let Ok(mut ready) = self.ready.lock() else {
                return Poll::Pending;
            };
            ready.as_mut().poll(context).map(|()| Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut slot) = self.header.lock()
                && let Some(sender) = slot.take()
            {
                let value = request
                    .headers()
                    .get(GRPC_TIMEOUT_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let _ = sender.send(value);
            }
            std::future::ready(Ok(http::Response::new(Body::empty())))
        }
    }

    struct DelayedGrpcBody {
        delay: Pin<Box<tokio::time::Sleep>>,
        state: u8,
    }

    impl HttpBody for DelayedGrpcBody {
        type Data = tonic::codegen::Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            match self.state {
                0 => {
                    if self.delay.as_mut().poll(context).is_pending() {
                        return Poll::Pending;
                    }
                    self.state = 1;
                    Poll::Ready(Some(Ok(Frame::data(tonic::codegen::Bytes::from_static(
                        &[0, 0, 0, 0, 0],
                    )))))
                }
                1 => {
                    self.state = 2;
                    let mut trailers = http::HeaderMap::new();
                    trailers.insert("grpc-status", http::HeaderValue::from_static("0"));
                    Poll::Ready(Some(Ok(Frame::trailers(trailers))))
                }
                _ => Poll::Ready(None),
            }
        }

        fn is_end_stream(&self) -> bool {
            self.state >= 2
        }

        fn size_hint(&self) -> SizeHint {
            SizeHint::default()
        }
    }

    #[derive(Clone)]
    struct DelayedStreamService {
        header: Arc<Mutex<Option<oneshot::Sender<Option<String>>>>>,
    }

    impl tonic::server::NamedService for DelayedStreamService {
        const NAME: &'static str = "anytype.ClientCommands";
    }

    impl Service<http::Request<Body>> for DelayedStreamService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            if let Ok(mut slot) = self.header.lock()
                && let Some(sender) = slot.take()
            {
                let header = request
                    .headers()
                    .get(GRPC_TIMEOUT_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let _ = sender.send(header);
            }
            let body = DelayedGrpcBody {
                delay: Box::pin(tokio::time::sleep(Duration::from_millis(1_100))),
                state: 0,
            };
            let response = http::Response::builder()
                .status(200)
                .header("content-type", "application/grpc")
                .body(Body::new(body))
                .unwrap_or_else(|_| http::Response::new(Body::empty()));
            std::future::ready(Ok(response))
        }
    }

    struct ListenerIncoming {
        listener: tokio::net::TcpListener,
    }

    impl tonic::codegen::tokio_stream::Stream for ListenerIncoming {
        type Item = std::io::Result<tokio::net::TcpStream>;

        fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            self.listener
                .poll_accept(context)
                .map(|result| Some(result.map(|(stream, _)| stream)))
        }
    }

    #[derive(Clone)]
    struct ServerDeadlineService;

    impl Service<http::Request<Body>> for ServerDeadlineService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: http::Request<Body>) -> Self::Future {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("grpc-status", http::HeaderValue::from_static("4"));
            trailers.insert(
                "grpc-message",
                http::HeaderValue::from_static("UNTRUSTED_SERVER_SECRET"),
            );
            trailers.insert(
                DEADLINE_SOURCE_METADATA,
                http::HeaderValue::from_static("local"),
            );
            trailers.insert(
                DEADLINE_OUTCOME_METADATA,
                http::HeaderValue::from_static("mutation_indeterminate"),
            );
            let body = Body::new(TrailerBody {
                frame: Some(Frame::trailers(trailers)),
            });
            let response = http::Response::builder()
                .status(200)
                .header("content-type", "application/grpc")
                .body(body)
                .unwrap_or_else(|_| http::Response::new(Body::empty()));
            std::future::ready(Ok(response))
        }
    }

    #[derive(Clone)]
    struct HeaderThenStallService {
        header: Arc<Mutex<Option<oneshot::Sender<Option<String>>>>>,
    }

    impl Service<http::Request<Body>> for HeaderThenStallService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            if let Ok(mut slot) = self.header.lock()
                && let Some(sender) = slot.take()
            {
                let value = request
                    .headers()
                    .get(GRPC_TIMEOUT_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let _ = sender.send(value);
            }
            let response = http::Response::builder()
                .status(200)
                .header("content-type", "application/grpc")
                .body(Body::new(PendingBody))
                .unwrap_or_else(|_| http::Response::new(Body::empty()));
            std::future::ready(Ok(response))
        }
    }

    fn header_then_stall_service() -> (HeaderThenStallService, oneshot::Receiver<Option<String>>) {
        let (sender, receiver) = oneshot::channel();
        (
            HeaderThenStallService {
                header: Arc::new(Mutex::new(Some(sender))),
            },
            receiver,
        )
    }

    impl Service<http::Request<Body>> for ScriptService {
        type Response = http::Response<Body>;
        type Error = Infallible;
        type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

        fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: http::Request<Body>) -> Self::Future {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if let Ok(mut slot) = self.header.lock()
                && let Some(sender) = slot.take()
            {
                let value = request
                    .headers()
                    .get(GRPC_TIMEOUT_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let _ = sender.send(value);
            }
            Box::pin(std::future::pending())
        }
    }

    fn scripted_service() -> (
        ScriptService,
        Arc<AtomicUsize>,
        oneshot::Receiver<Option<String>>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let (sender, receiver) = oneshot::channel();
        (
            ScriptService {
                calls: calls.clone(),
                header: Arc::new(Mutex::new(Some(sender))),
            },
            calls,
            receiver,
        )
    }

    #[test]
    fn defaults_and_finite_ranges_match_policy() {
        let policy = GrpcTimeoutPolicy::default();
        assert_eq!(policy.credential_setup, Some(Duration::from_secs(120)));
        assert_eq!(policy.ordinary_unary, Some(Duration::from_secs(120)));
        assert_eq!(policy.long_unary, Some(Duration::from_secs(1_800)));
        assert_eq!(policy.stream_setup, Some(Duration::from_secs(120)));
        assert_eq!(policy.stream_idle, None);
        assert_eq!(policy.stream_total_lifetime, None);
        assert_eq!(policy.cleanup, Some(Duration::from_secs(5)));

        for duration in [Duration::ZERO, Duration::from_millis(999)] {
            assert!(
                GrpcTimeoutPolicy {
                    ordinary_unary: Some(duration),
                    ..policy
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            GrpcTimeoutPolicy {
                long_unary: Some(Duration::from_secs(7_201)),
                ..policy
            }
            .validate()
            .is_err()
        );
        assert!(
            GrpcTimeoutPolicy {
                cleanup: Some(Duration::from_secs(31)),
                ..policy
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn environment_override_has_exact_grammar_and_limited_scope() {
        let disabled = GrpcTimeoutPolicy::from_environment(Some(OsString::from("0")))
            .expect("zero disables inherited generic profiles");
        assert_eq!(disabled.credential_setup, None);
        assert_eq!(disabled.ordinary_unary, None);
        assert_eq!(disabled.long_unary, None);
        assert_eq!(disabled.stream_setup, None);
        assert_eq!(disabled.cleanup, Some(Duration::from_secs(5)));

        let finite = GrpcTimeoutPolicy::from_environment(Some(OsString::from("17")))
            .expect("finite override");
        assert_eq!(finite.credential_setup, Some(Duration::from_secs(17)));
        assert_eq!(finite.long_unary, Some(Duration::from_secs(17)));
        assert_eq!(finite.stream_idle, None);

        for malformed in [
            "",
            "00",
            "01",
            " 1",
            "+1",
            "-1",
            "1.0",
            "3601",
            "18446744073709551616",
        ] {
            assert!(
                GrpcTimeoutPolicy::from_environment(Some(OsString::from(malformed))).is_err(),
                "accepted {malformed:?}"
            );
        }
    }

    #[test]
    fn explicit_policy_resolution_returns_the_validated_policy() {
        let explicit = GrpcTimeoutPolicy {
            credential_setup: None,
            ordinary_unary: Some(Duration::from_secs(11)),
            long_unary: Some(Duration::from_secs(7_200)),
            stream_setup: None,
            stream_idle: Some(Duration::from_secs(12)),
            stream_total_lifetime: None,
            cleanup: Some(Duration::from_secs(30)),
        };
        assert_eq!(
            GrpcTimeoutPolicy::resolve(Some(explicit)).expect("explicit policy"),
            explicit
        );
        assert!(
            GrpcDeadlineService::try_new(
                (),
                GrpcTimeoutPolicy {
                    cleanup: Some(Duration::from_secs(31)),
                    ..explicit
                }
            )
            .is_err()
        );
    }

    #[test]
    fn generated_method_profiles_cover_the_checked_in_client_inventory() {
        let generated_source = include_str!("gen/anytype.rs");
        let mut generated = generated_source
            .split("GrpcMethod::new(")
            .skip(1)
            .filter_map(|tail| {
                let mut quoted = tail.split('"');
                let _ = quoted.next()?;
                let service = quoted.next()?;
                let _ = quoted.next()?;
                let method = quoted.next()?;
                (service == "anytype.ClientCommands").then_some(method)
            })
            .collect::<Vec<_>>();
        generated.sort_unstable();
        generated.dedup();

        let reviewed_count = REVIEWED_GENERATED_METHODS.len();
        let mut reviewed = REVIEWED_GENERATED_METHODS.to_vec();
        reviewed.sort_unstable();
        reviewed.dedup();
        assert_eq!(
            reviewed.len(),
            reviewed_count,
            "profile authority has duplicates"
        );
        let generated_without_profile = generated
            .iter()
            .copied()
            .filter(|method| reviewed.binary_search(method).is_err())
            .collect::<Vec<_>>();
        assert!(
            generated_without_profile.is_empty(),
            "generated RPCs missing reviewed profiles: {generated_without_profile:?}"
        );
        let reviewed_without_generated = reviewed
            .iter()
            .copied()
            .filter(|method| generated.binary_search(method).is_err())
            .collect::<Vec<_>>();
        // A reviewed forward profile may land before its generated snapshot;
        // only explicitly named transition methods may be absent locally.
        let mut expected_forward_profiles =
            ["ObjectCleanupSuggestions", "ObjectCleanupSuggestionIgnore"]
                .into_iter()
                .filter(|method| generated.binary_search(method).is_err())
                .collect::<Vec<_>>();
        expected_forward_profiles.sort_unstable();
        assert_eq!(
            reviewed_without_generated, expected_forward_profiles,
            "profile authority contains an unexpected method absent from generated RPCs"
        );
    }

    #[test]
    fn generated_method_defaults_are_closed_and_conservative() {
        let read = GrpcMethod::new("anytype.ClientCommands", "ObjectShow");
        assert_eq!(
            inferred_call_options(Some(&read)).outcome,
            GrpcTimeoutOutcome::ReadAborted
        );
        let stream = GrpcMethod::new("anytype.ClientCommands", "ListenSessionEvents");
        assert_eq!(
            inferred_call_options(Some(&stream)).class,
            GrpcTimeoutClass::StreamSetup
        );
        let import = GrpcMethod::new("anytype.ClientCommands", "ObjectImport");
        assert_eq!(
            inferred_call_options(Some(&import)),
            GrpcCallOptions::new(
                GrpcTimeoutClass::LongUnary,
                GrpcTimeoutOutcome::MutationIndeterminate,
            )
        );
        let close = GrpcMethod::new("anytype.ClientCommands", "ObjectClose");
        assert_eq!(
            inferred_call_options(Some(&close)).class,
            GrpcTimeoutClass::Cleanup
        );
        let unknown = GrpcMethod::new("anytype.ClientCommands", "FutureMutation");
        assert_eq!(
            inferred_call_options(Some(&unknown)).outcome,
            GrpcTimeoutOutcome::MutationIndeterminate
        );
    }

    #[test]
    fn cleanup_suggestion_methods_have_explicit_read_and_mutation_outcomes() {
        let suggestions = GrpcMethod::new("anytype.ClientCommands", "ObjectCleanupSuggestions");
        assert_eq!(
            inferred_call_options(Some(&suggestions)),
            GrpcCallOptions::ordinary_read()
        );
        let ignore = GrpcMethod::new("anytype.ClientCommands", "ObjectCleanupSuggestionIgnore");
        assert_eq!(
            inferred_call_options(Some(&ignore)),
            GrpcCallOptions::ordinary_mutation()
        );
    }

    #[cfg(unix)]
    #[test]
    fn environment_rejects_non_unicode() {
        use std::os::unix::ffi::OsStringExt;

        assert!(GrpcTimeoutPolicy::from_environment(Some(OsString::from_vec(vec![0xff]))).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn request_header_and_local_deadline_use_the_same_budget() {
        let (inner, calls, header) = scripted_service();
        let mut service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                ordinary_unary: Some(Duration::from_secs(10)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let request = http::Request::new(Body::empty());
        let call = tokio::spawn(async move { service.call(request).await });

        assert_eq!(
            header.await.expect("captured header").as_deref(),
            Some("10000000u")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(10)).await;
        let error = call
            .await
            .expect("service task")
            .expect_err("local timeout");
        assert!(matches!(
            error,
            GrpcDeadlineServiceError::Deadline(ref status)
                if status.code() == Code::DeadlineExceeded
        ));
        let source = error.source().expect("payload-free deadline source");
        assert!(!format!("{source:?}").contains("SECRET"));
    }

    #[tokio::test(start_paused = true)]
    async fn scoped_enclosing_deadline_caps_generated_header_and_local_wait() {
        let (inner, calls, header) = scripted_service();
        let mut service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                ordinary_unary: Some(Duration::from_secs(120)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let enclosing = GrpcEnclosingDeadline::from_now(Duration::from_secs(3))
            .expect("representable enclosing deadline");
        let call = tokio::spawn(async move {
            scope_grpc_enclosing_deadline(enclosing, async move {
                service.call(http::Request::new(Body::empty())).await
            })
            .await
        });

        assert_eq!(
            header.await.expect("captured header").as_deref(),
            Some("3000000u")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(matches!(
            call.await.expect("service task").expect_err("local timeout"),
            GrpcDeadlineServiceError::Deadline(ref status)
                if status.code() == Code::DeadlineExceeded
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn generated_tonic_client_preserves_local_timeout_classification() {
        use crate::{
            anytype::{ClientCommandsClient, rpc::account::local_link::list_apps},
            deadline::with_grpc_call_options,
        };

        let (inner, calls, header) = scripted_service();
        let service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                ordinary_unary: Some(Duration::from_secs(4)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut client = ClientCommandsClient::new(service);
        let request = with_grpc_call_options(
            Request::new(list_apps::Request {}),
            GrpcCallOptions::ordinary_read(),
        );
        let call = tokio::spawn(async move { client.account_local_link_list_apps(request).await });
        assert_eq!(
            header.await.expect("captured header").as_deref(),
            Some("4000000u")
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_secs(4)).await;
        let status = call
            .await
            .expect("generated client task")
            .expect_err("local timeout");
        let error = GrpcDeadlineError::from_status(
            &status,
            GrpcTimeoutClass::LongUnary,
            GrpcTimeoutOutcome::MutationIndeterminate,
            Duration::from_secs(4),
        )
        .expect("classified deadline");
        assert_eq!(error.class, GrpcTimeoutClass::OrdinaryUnary);
        assert_eq!(error.outcome, GrpcTimeoutOutcome::ReadAborted);
        assert_eq!(error.source, GrpcTimeoutSource::Local);
    }

    #[tokio::test(start_paused = true)]
    async fn unary_deadline_remains_active_after_response_headers() {
        use crate::anytype::{ClientCommandsClient, rpc::account::local_link::list_apps};

        let (inner, header) = header_then_stall_service();
        let service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                ordinary_unary: Some(Duration::from_secs(6)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut client = ClientCommandsClient::new(service);
        let request = with_grpc_call_options(
            Request::new(list_apps::Request {}),
            GrpcCallOptions::ordinary_read(),
        );
        let call = tokio::spawn(async move { client.account_local_link_list_apps(request).await });
        assert_eq!(
            header.await.expect("captured header").as_deref(),
            Some("6000000u")
        );
        tokio::time::advance(Duration::from_secs(6)).await;
        let status = call
            .await
            .expect("generated client task")
            .expect_err("stalled unary body must expire");
        let error = GrpcDeadlineError::from_status(
            &status,
            GrpcTimeoutClass::OrdinaryUnary,
            GrpcTimeoutOutcome::ReadAborted,
            Duration::from_secs(6),
        )
        .expect("classified deadline");
        assert_eq!(error.source, GrpcTimeoutSource::Local);
    }

    #[tokio::test(start_paused = true)]
    async fn successful_stream_headers_disarm_the_setup_deadline() {
        use crate::anytype::{ClientCommandsClient, StreamRequest};

        let (inner, header) = header_then_stall_service();
        let service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                stream_setup: Some(Duration::from_secs(5)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut client = ClientCommandsClient::new(service);
        let request = Request::new(StreamRequest {
            token: String::new(),
        });
        let request = with_grpc_call_options(request, GrpcCallOptions::stream_setup());
        let response = client
            .listen_session_events(request)
            .await
            .expect("successful response headers");
        assert_eq!(header.await.expect("captured header").as_deref(), None);
        let mut stream = response.into_inner();
        tokio::time::advance(Duration::from_secs(6)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), stream.message())
                .await
                .is_err(),
            "setup deadline must not terminate an established stream body"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn stream_setup_preserves_a_caller_supplied_whole_call_timeout() {
        use crate::anytype::{ClientCommandsClient, StreamRequest};

        let (inner, header) = header_then_stall_service();
        let service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                stream_setup: Some(Duration::from_secs(5)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut client = ClientCommandsClient::new(service);
        let mut request = Request::new(StreamRequest {
            token: String::new(),
        });
        request.set_timeout(Duration::from_secs(2));
        let request = with_grpc_call_options(request, GrpcCallOptions::stream_setup());
        let _response = client
            .listen_session_events(request)
            .await
            .expect("successful response headers");
        assert_eq!(
            header.await.expect("captured caller timeout").as_deref(),
            Some("2000000u")
        );
    }

    #[tokio::test]
    async fn tonic_server_stream_remains_established_beyond_setup_budget() {
        use crate::anytype::{ClientCommandsClient, StreamRequest};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind tonic test server");
        let address = listener.local_addr().expect("tonic test address");
        let (header_sender, header_receiver) = oneshot::channel();
        let service = DelayedStreamService {
            header: Arc::new(Mutex::new(Some(header_sender))),
        };
        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        let server = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(ListenerIncoming { listener }, async move {
                    let _ = shutdown_receiver.await;
                })
                .await
        });
        let channel = tonic::transport::Endpoint::from_shared(format!("http://{address}"))
            .expect("valid tonic test endpoint")
            .connect()
            .await
            .expect("connect tonic test client");
        let deadline_service = GrpcDeadlineService::try_new(
            channel,
            GrpcTimeoutPolicy {
                stream_setup: Some(Duration::from_secs(1)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid stream policy");
        let mut client = ClientCommandsClient::new(deadline_service);
        let request = with_grpc_call_options(
            Request::new(StreamRequest {
                token: String::new(),
            }),
            GrpcCallOptions::stream_setup(),
        );
        let response = client
            .listen_session_events(request)
            .await
            .expect("stream response headers");
        assert_eq!(
            header_receiver.await.expect("captured server header"),
            None,
            "stream setup must remain a local-only header boundary"
        );
        assert!(
            response
                .extensions()
                .get::<GrpcTransportProgress>()
                .is_some(),
            "tonic response extensions must carry raw transport progress"
        );

        let mut stream = response.into_inner();
        let message = tokio::time::timeout(Duration::from_secs(2), stream.message())
            .await
            .expect("established stream wait")
            .expect("stream transport")
            .expect("delayed event");
        assert!(message.context_id.is_empty());

        let _ = shutdown_sender.send(());
        server
            .await
            .expect("tonic server task")
            .expect("tonic server shutdown");
    }

    #[tokio::test]
    async fn server_deadline_uses_request_classification_and_cannot_spoof_local_source() {
        use crate::anytype::{ClientCommandsClient, rpc::account::local_link::list_apps};

        let service =
            GrpcDeadlineService::try_new(ServerDeadlineService, GrpcTimeoutPolicy::default())
                .expect("valid policy");
        let mut client = ClientCommandsClient::new(service);
        let request = with_grpc_call_options(
            Request::new(list_apps::Request {}),
            GrpcCallOptions::ordinary_read(),
        );
        let status = client
            .account_local_link_list_apps(request)
            .await
            .expect_err("server deadline");
        let error = GrpcDeadlineError::from_status(
            &status,
            GrpcTimeoutClass::LongUnary,
            GrpcTimeoutOutcome::MutationIndeterminate,
            Duration::from_millis(1),
        )
        .expect("classified deadline");
        assert_eq!(error.class, GrpcTimeoutClass::OrdinaryUnary);
        assert_eq!(error.outcome, GrpcTimeoutOutcome::ReadAborted);
        assert_eq!(error.source, GrpcTimeoutSource::Server);
        assert!(!error.to_string().contains("UNTRUSTED_SERVER_SECRET"));
    }

    #[tokio::test(start_paused = true)]
    async fn enclosing_deadline_and_existing_tighter_timeout_win() {
        let now = tokio::time::Instant::now();
        let enclosing = GrpcEnclosingDeadline::from_instant(now + Duration::from_secs(7));
        let selected = select_deadline(Some(Duration::from_secs(120)), Some(enclosing), None, now)
            .expect("valid deadline");
        assert_eq!(selected, Some(Duration::from_secs(7)));

        let header = http::HeaderValue::from_static("3000000u");
        let selected = select_deadline(
            Some(Duration::from_secs(120)),
            Some(enclosing),
            Some(&header),
            now,
        )
        .expect("valid deadline");
        assert_eq!(selected, Some(Duration::from_secs(3)));

        let exhausted = http::HeaderValue::from_static("0n");
        assert_eq!(
            select_deadline(None, None, Some(&exhausted), now),
            Err(SelectDeadlineError::Expired)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_enclosing_deadline_prevents_dispatch() {
        let (inner, calls, _header) = scripted_service();
        let mut service = GrpcDeadlineService::try_new(inner, GrpcTimeoutPolicy::default())
            .expect("valid policy");
        let mut request = http::Request::new(Body::empty());
        request
            .extensions_mut()
            .insert(GrpcCallOptions::ordinary_mutation().enclosing(
                GrpcEnclosingDeadline::from_instant(tokio::time::Instant::now()),
            ));
        let error = service.call(request).await.expect_err("expired deadline");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        let status = match &error {
            GrpcDeadlineServiceError::Deadline(status) => status,
            other => panic!("unexpected service error: {other:?}"),
        };
        assert_eq!(status.code(), Code::DeadlineExceeded);
        let classified = GrpcDeadlineError::from_status(
            status,
            GrpcTimeoutClass::OrdinaryUnary,
            GrpcTimeoutOutcome::MutationIndeterminate,
            Duration::ZERO,
        );
        assert_eq!(
            classified.map(|error| error.outcome),
            Some(GrpcTimeoutOutcome::ReadAborted)
        );
        assert!(error.source().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn readiness_wait_consumes_cleanup_and_enclosing_budget() {
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = PendingReadyService {
            calls: calls.clone(),
        };
        let mut service = GrpcDeadlineService::try_new(inner, GrpcTimeoutPolicy::default())
            .expect("valid policy");
        let enclosing = GrpcEnclosingDeadline::from_now(Duration::from_secs(2))
            .expect("valid enclosing deadline");
        let options = GrpcCallOptions::cleanup().enclosing(enclosing);
        let mut request = http::Request::new(Body::empty());
        request.extensions_mut().insert(options);
        let call = tokio::spawn(async move { service.call(request).await });

        tokio::time::advance(Duration::from_secs(2)).await;
        let error = call
            .await
            .expect("service task")
            .expect_err("pending readiness must expire");
        let status = match error {
            GrpcDeadlineServiceError::Deadline(status) => status,
            other => panic!("unexpected service error: {other:?}"),
        };
        let classified = GrpcDeadlineError::from_status(
            &status,
            GrpcTimeoutClass::Cleanup,
            GrpcTimeoutOutcome::MutationIndeterminate,
            Duration::from_secs(2),
        )
        .expect("classified readiness deadline");
        assert_eq!(classified.class, GrpcTimeoutClass::Cleanup);
        assert_eq!(classified.outcome, GrpcTimeoutOutcome::ReadAborted);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_setup_ready_at_exact_deadline_does_not_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ready_at = tokio::time::Instant::now() + Duration::from_secs(1);
        let inner = ReadyAtDeadlineService {
            calls: calls.clone(),
            ready: Arc::new(Mutex::new(Box::pin(tokio::time::sleep_until(ready_at)))),
            header: Arc::new(Mutex::new(None)),
        };
        let mut service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                stream_setup: Some(Duration::from_secs(1)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut request = http::Request::new(Body::empty());
        request
            .extensions_mut()
            .insert(GrpcCallOptions::stream_setup());
        let call = tokio::spawn(async move { service.call(request).await });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = call
            .await
            .expect("service task")
            .expect_err("exhausted setup deadline");
        assert!(matches!(error, GrpcDeadlineServiceError::Deadline(_)));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn caller_stream_timeout_propagates_only_its_remaining_absolute_budget() {
        let calls = Arc::new(AtomicUsize::new(0));
        let ready_at = tokio::time::Instant::now() + Duration::from_millis(1_900);
        let (header_sender, header_receiver) = oneshot::channel();
        let inner = ReadyAtDeadlineService {
            calls: calls.clone(),
            ready: Arc::new(Mutex::new(Box::pin(tokio::time::sleep_until(ready_at)))),
            header: Arc::new(Mutex::new(Some(header_sender))),
        };
        let mut service = GrpcDeadlineService::try_new(
            inner,
            GrpcTimeoutPolicy {
                stream_setup: Some(Duration::from_secs(5)),
                ..GrpcTimeoutPolicy::default()
            },
        )
        .expect("valid policy");
        let mut request = http::Request::new(Body::empty());
        request
            .headers_mut()
            .insert(GRPC_TIMEOUT_HEADER, http::HeaderValue::from_static("2S"));
        request
            .extensions_mut()
            .insert(GrpcCallOptions::stream_setup());
        let call = tokio::spawn(async move { service.call(request).await });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1_900)).await;
        call.await
            .expect("service task")
            .expect("remaining caller timeout dispatches");
        let header = header_receiver
            .await
            .expect("captured remaining caller timeout")
            .expect("caller timeout header preserved");
        let remaining = parse_grpc_timeout(
            &http::HeaderValue::from_str(&header).expect("valid propagated header"),
        )
        .expect("parse propagated header")
        .expect("finite propagated header");
        assert!((Duration::from_millis(99)..=Duration::from_millis(100)).contains(&remaining));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn expired_body_deadline_precedes_queued_data_and_trailers() {
        let options = GrpcCallOptions::ordinary_read();
        for frame in [
            Frame::data(tonic::codegen::Bytes::from_static(b"queued")),
            Frame::trailers(http::HeaderMap::new()),
        ] {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
            let mut body = GrpcDeadlineBody::new(
                TrailerBody { frame: Some(frame) },
                Some(deadline),
                None,
                options,
                tokio::time::Instant::now(),
            );
            tokio::time::advance(Duration::from_secs(1)).await;
            let frame = std::future::poll_fn(|context| Pin::new(&mut body).poll_frame(context))
                .await
                .expect("deadline frame");
            assert!(matches!(frame, Err(GrpcDeadlineServiceError::Deadline(_))));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn raw_body_progress_resets_idle_before_a_message_decodes() {
        let progress = GrpcTransportProgress::new();
        let (ready, receiver) = oneshot::channel();
        let body = SignaledDataBody {
            ready: receiver,
            data: tonic::codegen::Bytes::from_static(b"partial"),
            emitted: false,
        };
        let mut body = GrpcDeadlineBody::new(
            body,
            None,
            Some(progress.clone()),
            GrpcCallOptions::stream_setup(),
            tokio::time::Instant::now(),
        );
        let policy = GrpcTimeoutPolicy {
            stream_idle: Some(Duration::from_secs(5)),
            ..GrpcTimeoutPolicy::default()
        };
        let mut deadlines = GrpcStreamDeadline::new(policy, None)
            .expect("stream deadlines")
            .with_transport_progress(Some(progress));
        let waiting = tokio::spawn(async move {
            let undecoded = std::future::poll_fn(move |context| {
                let _ = Pin::new(&mut body).poll_frame(context);
                Poll::<Result<(), Status>>::Pending
            });
            deadlines.next(undecoded).await
        });

        tokio::time::advance(Duration::from_secs(4)).await;
        let _ = ready.send(());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(
            !waiting.is_finished(),
            "raw progress must reset stream idle"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = waiting
            .await
            .expect("stream task")
            .expect_err("idle expires after progress window");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamIdle,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn decoded_delivery_does_not_grant_a_second_idle_window_after_raw_progress() {
        let progress = GrpcTransportProgress::new();
        let mut deadlines = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_idle: Some(Duration::from_secs(5)),
                ..GrpcTimeoutPolicy::default()
            },
            None,
        )
        .expect("stream deadlines")
        .with_transport_progress(Some(progress.clone()));
        let control = tokio::spawn(async move {
            let result = deadlines
                .established_phase(tokio::time::sleep(Duration::from_secs(8)))
                .await;
            (deadlines, result)
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        progress.record();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        let (mut deadlines, result) = control.await.expect("control phase task");
        result.expect("raw progress keeps long control work alive");
        deadlines
            .observe_decoded_message()
            .expect("decoded delivery observation");

        let waiting = tokio::spawn(async move {
            deadlines
                .established_phase(std::future::pending::<()>())
                .await
        });
        tokio::time::advance(Duration::from_millis(999)).await;
        assert!(!waiting.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        let error = waiting
            .await
            .expect("idle task")
            .expect_err("idle remains anchored to raw DATA arrival");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamIdle,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn empty_raw_data_does_not_reset_stream_idle() {
        let progress = GrpcTransportProgress::new();
        let (ready, receiver) = oneshot::channel();
        let body = SignaledDataBody {
            ready: receiver,
            data: tonic::codegen::Bytes::new(),
            emitted: false,
        };
        let mut body = GrpcDeadlineBody::new(
            body,
            None,
            Some(progress.clone()),
            GrpcCallOptions::stream_setup(),
            tokio::time::Instant::now(),
        );
        let policy = GrpcTimeoutPolicy {
            stream_idle: Some(Duration::from_secs(5)),
            ..GrpcTimeoutPolicy::default()
        };
        let mut deadlines = GrpcStreamDeadline::new(policy, None)
            .expect("stream deadlines")
            .with_transport_progress(Some(progress));
        let waiting = tokio::spawn(async move {
            let undecoded = std::future::poll_fn(move |context| {
                let _ = Pin::new(&mut body).poll_frame(context);
                Poll::<Result<(), Status>>::Pending
            });
            deadlines.next(undecoded).await
        });

        tokio::time::advance(Duration::from_secs(4)).await;
        let _ = ready.send(());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = waiting
            .await
            .expect("stream task")
            .expect_err("empty DATA must not reset idle");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamIdle,
                ..
            })
        ));
    }

    async fn assert_dedicated_reader_progresses_during_stalled_work() {
        let progress = GrpcTransportProgress::new();
        let (ready, receiver) = oneshot::channel();
        let mut body = GrpcDeadlineBody::new(
            SignaledDataBody {
                ready: receiver,
                data: tonic::codegen::Bytes::from_static(b"partial"),
                emitted: false,
            },
            None,
            Some(progress.clone()),
            GrpcCallOptions::stream_setup(),
            tokio::time::Instant::now(),
        );
        let reader = tokio::spawn(async move {
            std::future::poll_fn(move |context| {
                let _ = Pin::new(&mut body).poll_frame(context);
                Poll::<()>::Pending
            })
            .await;
        });
        let mut deadlines = GrpcStreamDeadline::new(
            GrpcTimeoutPolicy {
                stream_idle: Some(Duration::from_secs(5)),
                ..GrpcTimeoutPolicy::default()
            },
            None,
        )
        .expect("stream deadlines")
        .with_transport_progress(Some(progress));
        let work = tokio::spawn(async move {
            deadlines
                .established_phase(std::future::pending::<()>())
                .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        let _ = ready.send(());
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(!work.is_finished(), "raw progress must reset stream idle");
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = work
            .await
            .expect("established work task")
            .expect_err("idle expires after the raw-progress window");
        assert!(matches!(
            error,
            GrpcStreamError::Deadline(GrpcDeadlineError {
                class: GrpcTimeoutClass::StreamIdle,
                ..
            })
        ));
        reader.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn raw_progress_remains_observable_during_stalled_control_work() {
        assert_dedicated_reader_progresses_during_stalled_work().await;
    }

    #[tokio::test(start_paused = true)]
    async fn raw_progress_remains_observable_during_stalled_output_work() {
        assert_dedicated_reader_progresses_during_stalled_work().await;
    }

    #[tokio::test(start_paused = true)]
    async fn raw_progress_remains_observable_during_stalled_resubscribe_work() {
        assert_dedicated_reader_progresses_during_stalled_work().await;
    }

    #[test]
    fn debug_output_redacts_inner_errors_statuses_and_bodies() {
        let transport =
            GrpcDeadlineServiceError::<Status>::transport(Status::internal("TRANSPORT_SECRET"));
        assert!(!format!("{transport:?}").contains("TRANSPORT_SECRET"));
        let source = transport.source().expect("payload-free transport source");
        assert!(!source.to_string().contains("TRANSPORT_SECRET"));
        assert!(!format!("{source:?}").contains("TRANSPORT_SECRET"));
        assert_eq!(
            Status::from_error(Box::new(transport)).code(),
            Code::Internal
        );
        let deadline = GrpcDeadlineServiceError::<Status>::Deadline(Status::deadline_exceeded(
            "DEADLINE_SECRET",
        ));
        assert!(!format!("{deadline:?}").contains("DEADLINE_SECRET"));
        let source = deadline.source().expect("payload-free deadline source");
        assert!(!source.to_string().contains("DEADLINE_SECRET"));
        assert!(!format!("{source:?}").contains("DEADLINE_SECRET"));
        let invalid = GrpcDeadlineServiceError::<Status>::InvalidTimeout(Status::invalid_argument(
            "INVALID_SECRET",
        ));
        assert!(!format!("{invalid:?}").contains("INVALID_SECRET"));
        let source = invalid.source().expect("payload-free invalid source");
        assert!(!source.to_string().contains("INVALID_SECRET"));
        assert!(!format!("{source:?}").contains("INVALID_SECRET"));
        let stream = GrpcStreamError::Status(Status::internal("SERVER_SECRET"));
        assert!(!format!("{stream:?}").contains("SERVER_SECRET"));
        let body = GrpcDeadlineBody::new(
            PendingBody,
            None,
            None,
            GrpcCallOptions::ordinary_read(),
            tokio::time::Instant::now(),
        );
        assert!(!format!("{body:?}").contains("PendingBody"));
        let service =
            GrpcDeadlineService::new_resolved("INNER_SECRET", GrpcTimeoutPolicy::default());
        assert!(!format!("{service:?}").contains("INNER_SECRET"));
    }

    #[tokio::test]
    async fn redacted_transport_errors_preserve_readiness_and_body_status_codes() {
        let mut service = GrpcDeadlineService::new_resolved(
            StatusReadyFailureService,
            GrpcTimeoutPolicy::default(),
        );
        let readiness = service
            .call(http::Request::new(Body::empty()))
            .await
            .expect_err("readiness failure");
        let readiness_text = format!("{readiness:?} {readiness}");
        assert!(!readiness_text.contains("HOSTILE_READINESS_SECRET"));
        let readiness_status = Status::from_error(Box::new(readiness));
        assert_eq!(readiness_status.code(), Code::Unavailable);
        assert!(!format!("{readiness_status:?}").contains("HOSTILE_READINESS_SECRET"));

        let mut body = GrpcDeadlineBody::new(
            StatusErrorBody {
                status: Some(Status::cancelled("HOSTILE_BODY_SECRET")),
            },
            None,
            None,
            GrpcCallOptions::ordinary_read(),
            tokio::time::Instant::now(),
        );
        let body_error = std::future::poll_fn(|context| Pin::new(&mut body).poll_frame(context))
            .await
            .expect("body failure frame")
            .expect_err("body status failure");
        let body_text = format!("{body_error:?} {body_error}");
        assert!(!body_text.contains("HOSTILE_BODY_SECRET"));
        let body_status = Status::from_error(Box::new(body_error));
        assert_eq!(body_status.code(), Code::Cancelled);
        assert!(!format!("{body_status:?}").contains("HOSTILE_BODY_SECRET"));
    }

    #[test]
    fn redacted_transport_errors_preserve_non_status_tonic_classification() {
        let timeout = GrpcDeadlineServiceError::<TimeoutExpired>::transport(TimeoutExpired(()));
        assert_eq!(
            Status::from_error(Box::new(timeout)).code(),
            Code::Cancelled
        );

        let connect =
            GrpcDeadlineServiceError::<tonic::ConnectError>::transport(tonic::ConnectError(
                Box::new(HostileTransportError("HOSTILE_CONNECT_SECRET\ncontrol")),
            ));
        let rendered = format!("{connect:?} {connect}");
        assert!(!rendered.contains("HOSTILE_CONNECT_SECRET"));
        let status = Status::from_error(Box::new(connect));
        assert_eq!(status.code(), Code::Unavailable);
        assert!(!format!("{status:?}").contains("HOSTILE_CONNECT_SECRET"));
    }

    #[tokio::test(start_paused = true)]
    async fn stream_idle_resets_but_lifetime_does_not() {
        let policy = GrpcTimeoutPolicy {
            stream_idle: Some(Duration::from_secs(3)),
            stream_total_lifetime: Some(Duration::from_secs(8)),
            ..GrpcTimeoutPolicy::default()
        };
        let mut deadlines = GrpcStreamDeadline::new(policy, None).expect("stream deadlines");
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            deadlines.next(async { Ok::<_, Status>(1) }).await,
            Ok(1)
        ));
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(matches!(
            deadlines.next(async { Ok::<_, Status>(2) }).await,
            Ok(2)
        ));
        tokio::time::advance(Duration::from_secs(3)).await;
        let error = deadlines
            .next(std::future::pending::<Result<(), Status>>())
            .await
            .expect_err("idle timeout");
        let GrpcStreamError::Deadline(error) = error else {
            panic!("expected deadline error");
        };
        assert_eq!(error.class, GrpcTimeoutClass::StreamIdle);
        assert_eq!(error.outcome, GrpcTimeoutOutcome::StreamTerminated);
    }

    #[tokio::test(start_paused = true)]
    async fn stream_progress_and_reopen_do_not_reset_total_lifetime() {
        let policy = GrpcTimeoutPolicy {
            stream_idle: Some(Duration::from_secs(4)),
            stream_total_lifetime: Some(Duration::from_secs(6)),
            ..GrpcTimeoutPolicy::default()
        };
        let mut deadlines = GrpcStreamDeadline::new(policy, None).expect("stream deadlines");
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(matches!(
            deadlines.next(async { Ok::<_, Status>(()) }).await,
            Ok(())
        ));
        tokio::time::advance(Duration::from_secs(2)).await;
        deadlines.reset_idle().expect("reopen resets idle only");
        tokio::time::advance(Duration::from_secs(1)).await;
        let error = deadlines
            .next(std::future::pending::<Result<(), Status>>())
            .await
            .expect_err("lifetime timeout");
        let GrpcStreamError::Deadline(error) = error else {
            panic!("expected deadline error");
        };
        assert_eq!(error.class, GrpcTimeoutClass::StreamLifetime);
    }

    #[test]
    fn server_and_local_statuses_share_classification_but_retain_source() {
        let options = GrpcCallOptions::ordinary_mutation();
        let local = local_deadline_status(options, Duration::from_secs(1));
        let local = GrpcDeadlineError::from_status(
            &local,
            GrpcTimeoutClass::LongUnary,
            GrpcTimeoutOutcome::ReadAborted,
            Duration::from_secs(1),
        )
        .expect("local deadline");
        assert_eq!(local.class, GrpcTimeoutClass::OrdinaryUnary);
        assert_eq!(local.outcome, GrpcTimeoutOutcome::MutationIndeterminate);
        assert_eq!(local.source, GrpcTimeoutSource::Local);

        let server = Status::deadline_exceeded("untrusted upstream detail");
        let server = GrpcDeadlineError::from_status(
            &server,
            GrpcTimeoutClass::LongUnary,
            GrpcTimeoutOutcome::ReadAborted,
            Duration::from_secs(1),
        )
        .expect("server deadline");
        assert_eq!(server.class, GrpcTimeoutClass::LongUnary);
        assert_eq!(server.source, GrpcTimeoutSource::Server);
        assert!(!server.to_string().contains("untrusted upstream detail"));
    }
}
