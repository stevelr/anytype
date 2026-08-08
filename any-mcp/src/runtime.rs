// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Authenticated client ownership and MCP service lifecycle.

use std::{
    fmt,
    future::Future,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anytype::prelude::{AnytypeClient, AnytypeError};
use rmcp::{
    RoleServer, ServiceExt,
    service::{QuitReason, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{IntoTransport, Transport},
};
use sha2::{Digest as _, Sha256};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    artifact_acceptance_gates::ArtifactAcceptanceGates,
    artifact_client_roots::ClientRootsGate,
    artifact_config::ArtifactConfig,
    artifact_roots::RootRegistry,
    artifact_staging::{ArtifactStaging, StagingError},
    artifact_toolset::{
        ArtifactOperationState, ArtifactToolError, FileImportOutput, ImportIdempotency,
    },
    artifact_validators::ValidatorRunner,
    config::{ApplicationProfile, ProtocolMode, RuntimeConfig},
    optional_toolsets::OptionalToolsetSelection,
    server::AnyMcpServer,
    space_policy::{PolicyClient, SpaceAuthority, SpacePolicy},
};

fn hash_generation_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    hasher.update(bytes);
}

fn runtime_artifact_policy_digest(
    profile: ApplicationProfile,
    read_only: bool,
    optional_toolsets: &OptionalToolsetSelection,
    artifact: &ArtifactConfig,
    authority: &SpaceAuthority,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"any-mcp/configuration-policy/v1\0");
    hasher.update(1_u64.to_be_bytes());
    hash_generation_part(&mut hasher, profile.as_str().as_bytes());
    hasher.update([u8::from(read_only)]);
    for name in optional_toolsets.names() {
        hash_generation_part(&mut hasher, name.as_bytes());
    }
    match authority.policy() {
        SpacePolicy::AllReadWrite => hasher.update([0]),
        SpacePolicy::None => hasher.update([1]),
        SpacePolicy::OnlyReadWrite(spaces) => {
            hasher.update([2]);
            for space in spaces {
                hash_generation_part(&mut hasher, space.as_str().as_bytes());
            }
        }
    }
    for validator in artifact.validators() {
        hash_generation_part(&mut hasher, validator.id.as_str().as_bytes());
        hash_generation_part(&mut hasher, b"file-mime/v1");
        hash_generation_part(&mut hasher, validator.sha256.as_bytes());
        hasher.update([u8::from(validator.required)]);
        for media_type in &validator.mime {
            hash_generation_part(&mut hasher, media_type.as_bytes());
        }
        for value in [
            validator.timeout.as_nanos(),
            u128::from(validator.memory_bytes),
            u128::from(validator.input_bytes),
            u128::try_from(validator.stdout_bytes).unwrap_or(u128::MAX),
            u128::try_from(validator.stderr_bytes).unwrap_or(u128::MAX),
            u128::try_from(validator.fields).unwrap_or(u128::MAX),
            u128::try_from(validator.field_bytes).unwrap_or(u128::MAX),
        ] {
            hasher.update(value.to_be_bytes());
        }
    }
    hasher.finalize().into()
}

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
    identity: Arc<()>,
    client: PolicyClient,
    permits: Arc<Semaphore>,
    shutdown: CancellationToken,
    next_correlation_id: Arc<AtomicU64>,
    request_timeout: Duration,
    startup_status: StartupStatus,
    profile: ApplicationProfile,
    read_only: bool,
    optional_toolsets: OptionalToolsetSelection,
    artifact_config: Arc<ArtifactConfig>,
    artifact_roots: Option<RootRegistry>,
    artifact_staging: Option<ArtifactStaging>,
    artifact_validators: Option<ValidatorRunner>,
    artifact_operations: ArtifactOperationState,
    settlement_permits: Arc<Semaphore>,
    settlement_active: Arc<AtomicUsize>,
    settlement_notify: Arc<Notify>,
    settlement_gate: Arc<Mutex<SettlementAdmissionGate>>,
    artifact_acceptance_gates: ArtifactAcceptanceGates,
    client_roots: Arc<ClientRootsGate>,
}

struct RuntimeParts {
    max_concurrency: usize,
    request_timeout: Duration,
    startup_status: StartupStatus,
    profile: ApplicationProfile,
    read_only: bool,
    optional_toolsets: OptionalToolsetSelection,
    space_authority: SpaceAuthority,
}

#[derive(Debug)]
struct SettlementAdmissionGate {
    accepting: bool,
}

/// One bounded settlement slot registered with shutdown drain ownership.
///
/// A reserved import key is terminalized synchronously if this admission is
/// dropped before or after supervision, closing the reservation/supervision
/// cancellation gap without an unbounded cleanup task.
pub(crate) struct ImportSettlementAdmission {
    _permit: tokio::sync::OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    gate: Arc<Mutex<SettlementAdmissionGate>>,
    terminalize: Option<(ArtifactOperationState, [u8; 32])>,
}

impl ImportSettlementAdmission {
    fn gate(&self) -> MutexGuard<'_, SettlementAdmissionGate> {
        match self.gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn reserve_import(
        &mut self,
        operations: &ArtifactOperationState,
        key: [u8; 32],
        fingerprint: [u8; 32],
    ) -> Result<ImportIdempotency, ArtifactToolError> {
        let reservation = operations.reserve_import_now(key, fingerprint)?;
        if matches!(
            reservation,
            ImportIdempotency::Dispatch | ImportIdempotency::VerifyCandidate { .. }
        ) {
            self.terminalize = Some((operations.clone(), key));
        }
        Ok(reservation)
    }
}

impl Drop for ImportSettlementAdmission {
    fn drop(&mut self) {
        if let Some((operations, key)) = self.terminalize.take() {
            operations.settle_import_timeout_now(key);
        }
        {
            let _gate = self.gate();
            let _ = self
                .active
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_sub(1)
                });
        }
        self.notify.notify_waiters();
    }
}

impl fmt::Debug for RuntimeContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeContext")
            .field("request_timeout", &self.request_timeout)
            .field("startup_status", &self.startup_status)
            .field("profile", &self.profile)
            .field("read_only", &self.read_only)
            .field(
                "optional_toolset_count",
                &self.optional_toolsets.names().len(),
            )
            .field("artifact_config", &self.artifact_config)
            .field("artifact_staging_active", &self.artifact_staging.is_some())
            .field(
                "artifact_validator_count",
                &self
                    .artifact_validators
                    .as_ref()
                    .map_or(0, ValidatorRunner::configured_count),
            )
            .field("artifact_operations", &"<redacted>")
            .field(
                "active_artifact_settlements",
                &self.settlement_active.load(Ordering::Acquire),
            )
            .field("client_roots", &self.client_roots)
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
        // Validate and retain all filesystem authority before credential or
        // network activity. Staging is activated only after these validators
        // and the canonical space policy have succeeded, so no listener or
        // cleanup task can outlive a later startup failure.
        let artifact_roots = if config.optional_toolsets.contains("artifacts") && !config.read_only
        {
            Some(
                RootRegistry::activate(&config.artifact)
                    .map_err(|_| StartupError::ArtifactRoots)?,
            )
        } else {
            None
        };
        let artifact_validators = if artifact_roots.is_some()
            && !config.artifact.validators().is_empty()
        {
            Some(
                ValidatorRunner::activate(config.artifact.validators(), &config.artifact.limits)
                    .await
                    .map_err(|_| StartupError::ArtifactValidators)?,
            )
        } else {
            None
        };
        let client = AnytypeClient::with_config(config.client_config())
            .map_err(|_| StartupError::ClientInitialization)?;
        let auth = client
            .auth_status()
            .map_err(|_| StartupError::CredentialLookup)?;

        let startup_status = verify_startup_probes(
            auth.http.is_authenticated(),
            auth.grpc.is_authenticated(),
            config.profile.requires_grpc(config.read_only)
                || config.optional_toolsets.requires_grpc(),
            config.startup_timeout,
            || client.ping_http(),
            || client.ping_grpc(),
        )
        .await?;

        let authority = SpaceAuthority::initialize(&client, &config.artifact.spaces)
            .await
            .map_err(|_| StartupError::SpacePolicy)?;
        let artifact_policy_digest = runtime_artifact_policy_digest(
            config.profile,
            config.read_only,
            &config.optional_toolsets,
            &config.artifact,
            &authority,
        );
        let mut runtime = Self::from_parts_with_authority(
            client,
            RuntimeParts {
                max_concurrency: config.max_concurrency,
                request_timeout: config.request_timeout,
                startup_status,
                profile: config.profile,
                read_only: config.read_only,
                optional_toolsets: config.optional_toolsets.clone(),
                space_authority: authority,
            },
        );
        let artifact_staging = match (
            artifact_roots.as_ref(),
            config.artifact.staging().filter(|staging| staging.enabled),
        ) {
            (Some(roots), Some(staging)) => Some(
                ArtifactStaging::activate_with_policy_digest(
                    staging,
                    &config.artifact.limits,
                    roots,
                    artifact_policy_digest,
                    runtime.shutdown.clone(),
                )
                .await
                .map_err(classify_staging_startup_error)?,
            ),
            _ => None,
        };
        runtime.artifact_config = Arc::new(config.artifact.clone());
        runtime.artifact_roots = artifact_roots;
        runtime.artifact_staging = artifact_staging;
        runtime.artifact_validators = artifact_validators;
        Ok(runtime)
    }

    /// Returns the one long-lived Anytype client.
    #[must_use]
    pub const fn client(&self) -> &PolicyClient {
        &self.client
    }

    /// Returns the frozen central Anytype-space authorization gate.
    #[must_use]
    pub fn space_authority(&self) -> &SpaceAuthority {
        self.client.space_authority()
    }

    /// Returns the process-local identity shared by clones of this runtime.
    pub(crate) fn identity(&self) -> &Arc<()> {
        &self.identity
    }

    /// Returns a clone sharing every process-global resource — client,
    /// operation permits, shutdown, correlation — under a fresh handler-state
    /// identity.
    ///
    /// Identity-keyed handler state (idempotency registries, metrics) built
    /// over the fork is isolated from every other fork. The HTTP transport
    /// forks one runtime per authenticated principal so mutation idempotency
    /// is process-lifetime yet principal-partitioned.
    pub(crate) fn fork_identity(&self) -> Self {
        Self {
            identity: Arc::new(()),
            // A fork serves a different client session, so it must never
            // inherit another session's client-root narrowing decision.
            client_roots: Arc::new(ClientRootsGate::default()),
            ..self.clone()
        }
    }

    /// Returns the configured per-invocation upstream timeout.
    pub(crate) const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Returns this session's MCP client-root narrowing gate.
    pub(crate) fn client_roots(&self) -> &ClientRootsGate {
        &self.client_roots
    }

    /// Returns the one absolute deadline for a newly admitted invocation.
    pub(crate) fn request_deadline(&self) -> Instant {
        let now = Instant::now();
        now.checked_add(self.request_timeout).unwrap_or(now)
    }

    fn next_operation_correlation_id(&self) -> u64 {
        self.next_correlation_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1))
            })
            .unwrap_or(u64::MAX)
    }

    /// Records a locally controlled failure for an operation bounded outside
    /// the shared upstream executor.
    pub(crate) fn record_controlled_failure(
        &self,
        context: OperationContext,
        duration: Duration,
        failure: ControlledFailureKind,
    ) {
        log_operation_diagnostic(
            context,
            self.next_operation_correlation_id(),
            duration,
            default_control_failure_diagnostic(failure),
        );
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

    /// Returns the canonical startup-selected optional registry set.
    #[must_use]
    pub const fn optional_toolsets(&self) -> &OptionalToolsetSelection {
        &self.optional_toolsets
    }

    /// Returns the immutable startup artifact and space policy.
    #[must_use]
    pub fn artifact_config(&self) -> &ArtifactConfig {
        self.artifact_config.as_ref()
    }

    /// Returns activated local artifact roots for the selected writable registry.
    #[must_use]
    pub fn artifact_roots(&self) -> Option<&RootRegistry> {
        self.artifact_roots.as_ref()
    }

    /// Returns activated remote staging authority for this process generation.
    #[must_use]
    pub(crate) fn artifact_staging(&self) -> Option<&ArtifactStaging> {
        self.artifact_staging.as_ref()
    }

    /// Returns startup-pinned artifact validator authority.
    #[must_use]
    pub(crate) fn artifact_validators(&self) -> Option<&ValidatorRunner> {
        self.artifact_validators.as_ref()
    }

    /// Returns the process-generation artifact mutation ledger.
    #[must_use]
    pub(crate) fn artifact_operations(&self) -> &ArtifactOperationState {
        &self.artifact_operations
    }

    /// Returns this runtime's private acceptance synchronization facility.
    #[cfg(any(test, feature = "acceptance-harness"))]
    pub fn artifact_acceptance_gates(&self) -> &ArtifactAcceptanceGates {
        &self.artifact_acceptance_gates
    }

    /// Enables private in-process acceptance synchronization for this runtime.
    #[cfg(any(test, feature = "acceptance-harness"))]
    pub fn enable_artifact_acceptance_gates(&mut self) {
        self.artifact_acceptance_gates = ArtifactAcceptanceGates::enabled();
    }

    /// Starts process shutdown, rejects new work, and cancels running or
    /// permit-waiting operations.
    ///
    /// This operation is idempotent. The stdio transport invokes it as soon as
    /// EOF is observed, before rmcp performs its bounded in-flight drain.
    pub fn begin_shutdown(&self) {
        {
            let mut gate = match self.settlement_gate.lock() {
                Ok(gate) => gate,
                Err(poisoned) => poisoned.into_inner(),
            };
            gate.accepting = false;
        }
        self.permits.close();
        self.settlement_permits.close();
        self.shutdown.cancel();
    }

    /// Runs one post-reservation import settlement under runtime ownership.
    /// Request cancellation never reaches this task; shutdown and the
    /// operation's independent deadline do.  A failed child is converted into
    /// an idempotency terminal state before the caller observes failure.
    pub(crate) fn supervise_import_settlement<F>(
        &self,
        key: [u8; 32],
        admission: ImportSettlementAdmission,
        operation: F,
    ) -> tokio::sync::oneshot::Receiver<Result<FileImportOutput, ArtifactToolError>>
    where
        F: Future<Output = Result<FileImportOutput, ArtifactToolError>> + Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let shutdown = self.shutdown.clone();
        let operations = self.artifact_operations.clone();
        tokio::spawn(async move {
            let mut child = tokio::spawn(operation);
            let result = tokio::select! {
                joined = &mut child => match joined {
                    Ok(result) => result,
                    Err(_) => {
                        operations.settle_import_timeout(key).await;
                        Err(ArtifactToolError::Indeterminate)
                    }
                },
                () = shutdown.cancelled() => {
                    child.abort();
                    let _ = child.await;
                    operations.settle_import_timeout(key).await;
                    Err(ArtifactToolError::Indeterminate)
                },
            };
            drop(admission);
            let _ = sender.send(result);
        });
        receiver
    }

    /// Owns one blocking local artifact operation through completion even if
    /// its request waiter is cancelled. Shutdown drain observes the same task
    /// counter used by import settlement, and a vanished waiter leaves a
    /// terminal indeterminate ledger entry rather than an unowned commit.
    pub(crate) fn supervise_artifact_blocking<T, F>(
        &self,
        key: [u8; 32],
        operation: F,
    ) -> tokio::sync::oneshot::Receiver<Result<T, ArtifactToolError>>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, ArtifactToolError> + Send + 'static,
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let gate = match self.settlement_gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !gate.accepting {
            let _ = sender.send(Err(ArtifactToolError::Indeterminate));
            return receiver;
        }
        self.settlement_active.fetch_add(1, Ordering::AcqRel);
        drop(gate);
        let active = Arc::clone(&self.settlement_active);
        let notify = Arc::clone(&self.settlement_notify);
        let operations = self.artifact_operations.clone();
        tokio::spawn(async move {
            let result = match tokio::task::spawn_blocking(operation).await {
                Ok(result) => result,
                Err(_) => Err(ArtifactToolError::Indeterminate),
            };
            if sender.send(result).is_err() {
                operations.mark_indeterminate(key);
            }
            let _ = active.fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
            notify.notify_waiters();
        });
        receiver
    }

    /// Admits a settlement before an idempotency reservation is created.
    /// This prevents reserved operations from accumulating behind an
    /// unbounded internal task queue.
    pub(crate) async fn admit_import_settlement(
        &self,
        deadline: Instant,
    ) -> Result<ImportSettlementAdmission, ArtifactToolError> {
        let acquire = Arc::clone(&self.settlement_permits).acquire_owned();
        let permit = tokio::select! {
            result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), acquire) => {
                result.ok().and_then(Result::ok).ok_or(ArtifactToolError::Bounded)
            }
            () = self.shutdown.cancelled() => Err(ArtifactToolError::Indeterminate),
        }?;
        let gate = match self.settlement_gate.lock() {
            Ok(gate) => gate,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !gate.accepting {
            return Err(ArtifactToolError::Indeterminate);
        }
        self.settlement_active.fetch_add(1, Ordering::AcqRel);
        let admission = ImportSettlementAdmission {
            _permit: permit,
            active: Arc::clone(&self.settlement_active),
            notify: Arc::clone(&self.settlement_notify),
            gate: Arc::clone(&self.settlement_gate),
            terminalize: None,
        };
        Ok(admission)
    }

    /// Waits only a bounded interval for owned artifact settlement tasks.
    pub(crate) async fn drain_artifact_settlements(&self, timeout: Duration) {
        let drained = async {
            loop {
                let notified = self.settlement_notify.notified();
                let gate = match self.settlement_gate.lock() {
                    Ok(gate) => gate,
                    Err(poisoned) => poisoned.into_inner(),
                };
                if self.settlement_active.load(Ordering::Acquire) == 0 {
                    return;
                }
                drop(gate);
                notified.await;
            }
        };
        let _ = tokio::time::timeout(timeout, drained).await;
    }

    /// Waits a bounded interval for staging listener, connection, cleanup,
    /// and publication work to release the retained instance authority.
    pub(crate) async fn drain_artifact_staging(&self, timeout: Duration) {
        if let Some(staging) = &self.artifact_staging {
            let _ = staging.drain(timeout).await;
        }
    }

    /// Returns whether process shutdown has started.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.shutdown.is_cancelled()
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
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

    pub(crate) async fn execute_classified_until<F, T, E, C>(
        &self,
        deadline: Instant,
        context: OperationContext,
        cancellation: &CancellationToken,
        operation: F,
        classify: C,
    ) -> Result<T, ControlledOperationError<E>>
    where
        F: Future<Output = Result<T, E>>,
        C: Fn(&E) -> OperationFailureDiagnostic,
    {
        self.execute_classified_with_control_until(
            deadline,
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
        self.execute_classified_with_control_until(
            self.request_deadline(),
            context,
            cancellation,
            operation,
            classify,
            classify_control,
        )
        .await
    }

    pub(crate) async fn execute_classified_with_control_until<F, T, E, C, D>(
        &self,
        deadline: Instant,
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
        let correlation_id = self.next_operation_correlation_id();
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

        let result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), controlled)
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
    pub(crate) fn from_parts_with_space_authority(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
        space_authority: SpaceAuthority,
    ) -> Self {
        Self::from_parts_with_authority(
            client,
            RuntimeParts {
                max_concurrency,
                request_timeout,
                startup_status,
                profile: ApplicationProfile::Standard,
                read_only: false,
                optional_toolsets: OptionalToolsetSelection::default(),
                space_authority,
            },
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

    #[cfg(test)]
    pub(crate) fn from_parts_with_profile(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
        profile: ApplicationProfile,
        read_only: bool,
    ) -> Self {
        Self::from_parts_with_profile_and_optional_toolsets(
            client,
            max_concurrency,
            request_timeout,
            startup_status,
            profile,
            read_only,
            OptionalToolsetSelection::default(),
        )
    }

    #[cfg(any(test, feature = "acceptance-harness"))]
    pub(crate) fn from_parts_with_profile_and_optional_toolsets(
        client: AnytypeClient,
        max_concurrency: usize,
        request_timeout: Duration,
        startup_status: StartupStatus,
        profile: ApplicationProfile,
        read_only: bool,
        optional_toolsets: OptionalToolsetSelection,
    ) -> Self {
        Self::from_parts_with_authority(
            client,
            RuntimeParts {
                max_concurrency,
                request_timeout,
                startup_status,
                profile,
                read_only,
                optional_toolsets,
                space_authority: SpaceAuthority::allow_all_for_fixtures(),
            },
        )
    }

    /// Builds a live runtime that owns activated artifact roots, staging, and
    /// validators from an already parsed strict artifact policy.
    ///
    /// This is the direct-router acceptance seam. A production stdio child
    /// reaches the same state through [`RuntimeContext::start`], which
    /// additionally performs credential lookup and the startup probes an
    /// acceptance fixture has already proven with its own client. The fixture
    /// shape is otherwise fixed: standard profile, two concurrent requests, and
    /// a 30-second request timeout.
    ///
    /// `read_only` mirrors [`RuntimeContext::start`] exactly: a read-only
    /// server activates no artifact roots, and therefore no staging service and
    /// no validators, so the direct router and a spawned read-only child report
    /// the same artifact status.
    ///
    /// # Errors
    ///
    /// Returns the same concise [`StartupError`] values as
    /// [`RuntimeContext::start`] when the space policy, roots, staging service,
    /// or validators cannot be activated.
    #[cfg(test)]
    pub(crate) async fn from_parts_with_artifact_policy(
        client: AnytypeClient,
        startup_status: StartupStatus,
        optional_toolsets: OptionalToolsetSelection,
        artifact: &ArtifactConfig,
        read_only: bool,
    ) -> Result<Self, StartupError> {
        let artifact_roots = if optional_toolsets.contains("artifacts") && !read_only {
            Some(RootRegistry::activate(artifact).map_err(|_| StartupError::ArtifactRoots)?)
        } else {
            None
        };
        let artifact_validators = if artifact_roots.is_some() && !artifact.validators().is_empty() {
            Some(
                ValidatorRunner::activate(artifact.validators(), &artifact.limits)
                    .await
                    .map_err(|_| StartupError::ArtifactValidators)?,
            )
        } else {
            None
        };
        let authority = SpaceAuthority::initialize(&client, &artifact.spaces)
            .await
            .map_err(|_| StartupError::SpacePolicy)?;
        let artifact_policy_digest = runtime_artifact_policy_digest(
            ApplicationProfile::Standard,
            read_only,
            &optional_toolsets,
            artifact,
            &authority,
        );
        let mut runtime = Self::from_parts_with_authority(
            client,
            RuntimeParts {
                max_concurrency: 2,
                request_timeout: Duration::from_secs(30),
                startup_status,
                profile: ApplicationProfile::Standard,
                read_only,
                optional_toolsets,
                space_authority: authority,
            },
        );
        let artifact_staging = match (
            artifact_roots.as_ref(),
            artifact.staging().filter(|staging| staging.enabled),
        ) {
            (Some(roots), Some(staging)) => Some(
                ArtifactStaging::activate_with_policy_digest(
                    staging,
                    &artifact.limits,
                    roots,
                    artifact_policy_digest,
                    runtime.shutdown.clone(),
                )
                .await
                .map_err(classify_staging_startup_error)?,
            ),
            _ => None,
        };
        runtime.artifact_config = Arc::new(artifact.clone());
        runtime.artifact_roots = artifact_roots;
        runtime.artifact_staging = artifact_staging;
        runtime.artifact_validators = artifact_validators;
        Ok(runtime)
    }

    fn from_parts_with_authority(client: AnytypeClient, parts: RuntimeParts) -> Self {
        Self {
            identity: Arc::new(()),
            client: PolicyClient::new(client, parts.space_authority),
            permits: Arc::new(Semaphore::new(parts.max_concurrency)),
            shutdown: CancellationToken::new(),
            next_correlation_id: Arc::new(AtomicU64::new(1)),
            request_timeout: parts.request_timeout,
            startup_status: parts.startup_status,
            profile: parts.profile,
            read_only: parts.read_only,
            optional_toolsets: parts.optional_toolsets,
            artifact_config: Arc::new(ArtifactConfig::default()),
            artifact_roots: None,
            artifact_staging: None,
            artifact_validators: None,
            artifact_operations: ArtifactOperationState::default(),
            settlement_permits: Arc::new(Semaphore::new(parts.max_concurrency)),
            settlement_active: Arc::new(AtomicUsize::new(0)),
            settlement_notify: Arc::new(Notify::new()),
            settlement_gate: Arc::new(Mutex::new(SettlementAdmissionGate { accepting: true })),
            artifact_acceptance_gates: ArtifactAcceptanceGates::disabled(),
            client_roots: Arc::new(ClientRootsGate::default()),
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
    log_operation_diagnostic(context, correlation_id, duration, diagnostic);
}

fn log_operation_diagnostic(
    context: OperationContext,
    correlation_id: u64,
    duration: Duration,
    diagnostic: OperationFailureDiagnostic,
) {
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
            AnytypeError::ChatTimestamp { .. } => Self::new("chat_timestamp"),
            AnytypeError::ChatHistoryEvidence { .. } => Self::new("chat_history_evidence"),
            AnytypeError::ChatEditTimestampNotAdvanced => {
                Self::new("chat_edit_timestamp_not_advanced")
            }
            AnytypeError::RateLimitExceeded { .. } => Self::new("rate_limit"),
            AnytypeError::Validation { .. } => Self::new("validation"),
            AnytypeError::NoKeyStore | AnytypeError::KeyStore { .. } => Self::new("keystore"),
            AnytypeError::Grpc { .. } | AnytypeError::GrpcUnavailable { .. } => Self::new("grpc"),
            AnytypeError::CacheDisabled => Self::new("cache"),
            AnytypeError::BodyGraph { .. } => Self::new("body_graph"),
            AnytypeError::CollectionMembershipEvidence { .. } => {
                Self::new("collection_membership_evidence")
            }
            AnytypeError::TypePropertyClassification { .. } => {
                Self::new("type_property_classification")
            }
            AnytypeError::AttachedDiscussion { .. } => Self::new("attached_discussion"),
            AnytypeError::BodyRpcLifecycle { kind } => {
                Self::new(body_rpc_diagnostic_category(*kind))
            }
            AnytypeError::BodyMutationIndeterminate { .. } => {
                Self::new("body_mutation_indeterminate")
            }
            AnytypeError::VerifyTimeout { .. } => Self::new("verification"),
            AnytypeError::Other { .. } => Self::new("other"),
        }
    }
}

fn body_rpc_diagnostic_category(
    kind: anytype::body_rpc::BodyRpcLifecycleErrorKind,
) -> &'static str {
    use anytype::body_rpc::BodyRpcLifecycleErrorKind;

    if kind == BodyRpcLifecycleErrorKind::ShowDeadline {
        "body_show_deadline"
    } else if kind == BodyRpcLifecycleErrorKind::ShowResponseTooLarge {
        "body_show_response_too_large"
    } else if kind == BodyRpcLifecycleErrorKind::CleanupFailed {
        "body_cleanup_failed"
    } else if kind == BodyRpcLifecycleErrorKind::AbsoluteDeadlineExhausted {
        "body_absolute_deadline_exhausted"
    } else {
        // Future non-exhaustive lifecycle kinds remain payload-free and
        // fail closed instead of inheriting an unrelated diagnostic category.
        "body_rpc_lifecycle"
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
    /// Configured Anytype space authority could not be frozen safely.
    SpacePolicy,
    /// Configured artifact roots could not be activated safely.
    ArtifactRoots,
    /// Configured private staging authority could not be activated safely.
    ArtifactStaging,
    /// Configured private staging policy or instance ownership was invalid.
    ArtifactStagingPolicy,
    /// Durable private staging state could not be reconciled safely.
    ArtifactStateReconciliation,
    /// Configured validator executables could not be pinned safely.
    ArtifactValidators,
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
            Self::SpacePolicy => {
                formatter.write_str("unable to initialize configured Anytype space policy")
            }
            Self::ArtifactRoots => {
                formatter.write_str("unable to initialize configured artifact roots")
            }
            Self::ArtifactStaging => {
                formatter.write_str("unable to initialize configured artifact staging")
            }
            Self::ArtifactStagingPolicy => formatter.write_str("invalid staging policy"),
            Self::ArtifactStateReconciliation => {
                formatter.write_str("artifact state reconciliation failed")
            }
            Self::ArtifactValidators => {
                formatter.write_str("unable to initialize configured artifact validators")
            }
        }
    }
}

fn classify_staging_startup_error(error: StagingError) -> StartupError {
    match error {
        StagingError::InvalidPolicy => StartupError::ArtifactStagingPolicy,
        StagingError::Reconciliation => StartupError::ArtifactStateReconciliation,
        _ => StartupError::ArtifactStaging,
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
            runtime
                .drain_artifact_settlements(runtime.artifact_config().limits.operation_timeout)
                .await;
            runtime
                .drain_artifact_staging(runtime.artifact_config().limits.operation_timeout)
                .await;
            return Ok(());
        }
        Err(_) => {
            runtime.begin_shutdown();
            runtime
                .drain_artifact_settlements(runtime.artifact_config().limits.operation_timeout)
                .await;
            return Err(ServeError::Initialization);
        }
    };

    let result = match running.waiting().await {
        Ok(QuitReason::Closed | QuitReason::Cancelled) => Ok(()),
        Ok(QuitReason::JoinError(_)) | Ok(_) | Err(_) => Err(ServeError::ServiceTask),
    };
    runtime.begin_shutdown();
    runtime
        .drain_artifact_settlements(runtime.artifact_config().limits.operation_timeout)
        .await;
    runtime
        .drain_artifact_staging(runtime.artifact_config().limits.operation_timeout)
        .await;
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
        let shutdown = self.runtime.shutdown_token();
        let message = tokio::select! {
            biased;
            () = shutdown.cancelled() => None,
            message = self.inner.receive() => message,
        };
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

    #[test]
    fn artifact_generation_evidence_binds_canonical_space_policy() {
        let artifact = ArtifactConfig::default();
        let optional = OptionalToolsetSelection::default();
        let all = SpaceAuthority::from_policy_for_tests(SpacePolicy::AllReadWrite);
        let only = SpaceAuthority::from_policy_for_tests(SpacePolicy::OnlyReadWrite(
            [crate::domain::SpaceId::new("space-1").expect("space ID")]
                .into_iter()
                .collect(),
        ));

        assert_ne!(
            runtime_artifact_policy_digest(
                ApplicationProfile::Standard,
                false,
                &optional,
                &artifact,
                &all,
            ),
            runtime_artifact_policy_digest(
                ApplicationProfile::Standard,
                false,
                &optional,
                &artifact,
                &only,
            )
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_local_publication_waiter_remains_owned_and_indeterminate() {
        let runtime = runtime(1, Duration::from_secs(1));
        let key = [51; 32];
        let fingerprint = [52; 32];
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import_now(key, fingerprint),
            Ok(crate::artifact_toolset::ImportIdempotency::Dispatch)
        ));
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let receiver = runtime.supervise_artifact_blocking(key, move || {
            worker_entered.wait();
            worker_release.wait();
            Ok::<_, ArtifactToolError>(())
        });
        entered.wait();
        drop(receiver);
        release.wait();
        runtime
            .drain_artifact_settlements(Duration::from_secs(1))
            .await;

        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import_now(key, fingerprint),
            Err(ArtifactToolError::Indeterminate)
        ));
    }

    #[tokio::test]
    async fn settlement_panic_is_terminal_and_drainable() {
        let runtime = runtime(1, Duration::from_secs(1));
        let key = [7; 32];
        let fingerprint = [9; 32];
        let mut admission = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("settlement permit");
        assert!(matches!(
            admission.reserve_import(runtime.artifact_operations(), key, fingerprint),
            Ok(crate::artifact_toolset::ImportIdempotency::Dispatch)
        ));
        let receiver = runtime.supervise_import_settlement(key, admission, async move {
            panic!("test settlement panic");
            #[allow(unreachable_code)]
            Ok::<_, ArtifactToolError>(unreachable!())
        });
        assert!(matches!(
            receiver.await,
            Ok(Err(ArtifactToolError::Indeterminate))
        ));
        runtime
            .drain_artifact_settlements(Duration::from_millis(100))
            .await;
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import(key, fingerprint)
                .await,
            Err(ArtifactToolError::Indeterminate)
        ));
    }

    #[tokio::test]
    async fn settlement_admission_deadline_has_no_invisible_queue() {
        let runtime = runtime(1, Duration::from_secs(1));
        let held = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("first permit");
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(20))
            .expect("bounded deadline");
        assert!(matches!(
            runtime.admit_import_settlement(deadline).await,
            Err(ArtifactToolError::Bounded)
        ));
        assert_eq!(runtime.settlement_active.load(Ordering::Acquire), 1);
        drop(held);
        assert_eq!(runtime.settlement_active.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn shutdown_between_admission_and_supervision_waits_and_terminalizes() {
        let runtime = runtime(1, Duration::from_secs(1));
        let key = [5; 32];
        let fingerprint = [6; 32];
        let mut admission = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("settlement admission");
        assert!(matches!(
            admission.reserve_import(runtime.artifact_operations(), key, fingerprint),
            Ok(crate::artifact_toolset::ImportIdempotency::Dispatch)
        ));

        runtime.begin_shutdown();
        let draining_runtime = runtime.clone();
        let drain = tokio::spawn(async move {
            draining_runtime
                .drain_artifact_settlements(Duration::from_secs(1))
                .await;
        });
        tokio::task::yield_now().await;
        assert!(!drain.is_finished(), "drain must retain admitted ownership");

        drop(admission);
        drain.await.expect("drain task");
        assert_eq!(runtime.settlement_active.load(Ordering::Acquire), 0);
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import(key, fingerprint)
                .await,
            Err(ArtifactToolError::Indeterminate)
        ));
    }

    #[tokio::test]
    async fn shutdown_terminalizes_every_admitted_settlement_before_drain() {
        let runtime = runtime(1, Duration::from_secs(1));
        let key = [3; 32];
        let fingerprint = [4; 32];
        let mut admission = runtime
            .admit_import_settlement(runtime.request_deadline())
            .await
            .expect("settlement permit");
        assert!(matches!(
            admission.reserve_import(runtime.artifact_operations(), key, fingerprint),
            Ok(crate::artifact_toolset::ImportIdempotency::Dispatch)
        ));
        let receiver = runtime.supervise_import_settlement(key, admission, async {
            std::future::pending::<Result<FileImportOutput, ArtifactToolError>>().await
        });
        tokio::task::yield_now().await;
        runtime.begin_shutdown();
        assert!(matches!(
            receiver.await,
            Ok(Err(ArtifactToolError::Indeterminate))
        ));
        runtime
            .drain_artifact_settlements(Duration::from_millis(100))
            .await;
        assert_eq!(runtime.settlement_active.load(Ordering::Acquire), 0);
        assert!(matches!(
            runtime
                .artifact_operations()
                .reserve_import(key, fingerprint)
                .await,
            Err(ArtifactToolError::Indeterminate)
        ));
    }

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
        let body_mutation =
            UpstreamDiagnostic::from_error(&AnytypeError::BodyMutationIndeterminate {
                object_id: secret.to_owned(),
                block_id: None,
                attempts: 1,
                timeout: Duration::from_secs(1),
                observed: None,
            });
        assert_eq!(body_mutation.category, "body_mutation_indeterminate");
        let attached_discussion =
            UpstreamDiagnostic::from_error(&AnytypeError::AttachedDiscussion {
                kind: anytype::attached_discussions::AttachedDiscussionErrorKind::MalformedEvidence,
            });
        assert_eq!(attached_discussion.category, "attached_discussion");
        let body_rpc_cases = [
            (
                anytype::body_rpc::BodyRpcLifecycleErrorKind::ShowDeadline,
                "body_show_deadline",
            ),
            (
                anytype::body_rpc::BodyRpcLifecycleErrorKind::ShowResponseTooLarge,
                "body_show_response_too_large",
            ),
            (
                anytype::body_rpc::BodyRpcLifecycleErrorKind::CleanupFailed,
                "body_cleanup_failed",
            ),
            (
                anytype::body_rpc::BodyRpcLifecycleErrorKind::AbsoluteDeadlineExhausted,
                "body_absolute_deadline_exhausted",
            ),
        ];
        for (kind, expected) in body_rpc_cases {
            let diagnostic =
                UpstreamDiagnostic::from_error(&AnytypeError::BodyRpcLifecycle { kind });
            assert_eq!(diagnostic, UpstreamDiagnostic::new(expected));
        }
        assert!(
            !format!(
                "{api:?}{auth:?}{grpc:?}{file_headers:?}{malformed_file:?}{body_mutation:?}{attached_discussion:?}"
            )
                .contains(secret)
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
