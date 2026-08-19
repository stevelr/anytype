// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Session-scoped MCP client-root narrowing for local artifact operations.
//!
//! Stable stdio serves exactly one initialized session per process, so the
//! server may take one bounded `roots/list` snapshot and use it as a
//! session-specific narrowing layer over the static TOML root policy. The
//! snapshot can only remove static authority; it never adds any.
//!
//! A client that advertises no roots capability keeps the configured static
//! policy. An unusable snapshot (transport failure, timeout, oversize,
//! duplicate, or unparsable entry) freezes local root operations as disabled
//! for the whole session and never falls back to broader static roots.
//!
//! Client root URIs and decoded paths never enter diagnostics or receipts.
//!
//! rmcp marks the whole roots wire model deprecated ahead of SEP-2577, but it
//! is the only released mechanism for client-supplied filesystem roots, so
//! this module opts out of that deprecation. The
//! opt-out is item-scoped to the declarations that name the deprecated wire
//! types, so unrelated deprecations in this file still warn.

use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
};

#[allow(deprecated)]
use rmcp::model::Root;
use rmcp::{RoleServer, service::Peer};
use tokio::sync::Notify;
use url::Url;

use crate::{
    artifact_config::AbsoluteNativePath,
    artifact_roots::{EffectiveRootRegistry, RootAccessError, RootRegistry},
    runtime::{InvocationCapability, RuntimeContext},
};

/// Maximum client roots accepted in one snapshot.
pub(crate) const MAX_CLIENT_ROOTS: usize = 64;

/// Maximum accepted length, in bytes, of one client root URI.
const MAX_ROOT_URI_BYTES: usize = 4096;

/// Boxed future returned by a client-root snapshot source.
#[allow(deprecated)]
type SnapshotFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Root>, ()>> + Send + 'a>>;

/// One session's terminal source of MCP client roots.
///
/// The production implementation wraps the initialized rmcp peer. Tests
/// substitute a scripted source so the whole narrowing decision is verifiable
/// without a transport.
pub(crate) trait ClientRootsSource: Send + Sync {
    /// Returns whether the initialized client advertised the roots capability.
    fn advertises_roots(&self) -> bool;

    /// Requests exactly one `roots/list` snapshot.
    fn list_roots(&self) -> SnapshotFuture<'_>;
}

type IntersectionHandle = tokio::task::JoinHandle<Result<EffectiveRootRegistry, RootAccessError>>;

/// Starts the bounded blocking client/static-root intersection.
trait ClientRootsIntersection: Send + Sync {
    fn spawn(
        &self,
        registry: RootRegistry,
        paths: Vec<AbsoluteNativePath>,
    ) -> Result<IntersectionHandle, ()>;
}

#[derive(Debug)]
struct TokioClientRootsIntersection;

impl ClientRootsIntersection for TokioClientRootsIntersection {
    fn spawn(
        &self,
        registry: RootRegistry,
        paths: Vec<AbsoluteNativePath>,
    ) -> Result<IntersectionHandle, ()> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| ())?;
        Ok(runtime.spawn_blocking(move || registry.intersect_client_roots(&paths)))
    }
}

/// Production snapshot source backed by the initialized rmcp peer.
pub(crate) struct PeerRootsSource(Peer<RoleServer>);

impl PeerRootsSource {
    /// Wraps one initialized peer as a snapshot source.
    pub(crate) const fn new(peer: Peer<RoleServer>) -> Self {
        Self(peer)
    }
}

#[allow(deprecated)]
impl ClientRootsSource for PeerRootsSource {
    fn advertises_roots(&self) -> bool {
        self.0
            .peer_info()
            .is_some_and(|info| info.capabilities.roots.is_some())
    }

    fn list_roots(&self) -> SnapshotFuture<'_> {
        Box::pin(async move {
            self.0
                .list_roots()
                .await
                .map(|result| result.roots)
                .map_err(|_| ())
        })
    }
}

/// Session-scoped client-root narrowing gate shared by artifact handlers.
///
/// The gate is inert unless a transport enables it, so transports that do not
/// carry a single terminal client session keep the static policy unchanged.
pub(crate) struct ClientRootsGate {
    enabled: AtomicBool,
    source: OnceLock<Arc<dyn ClientRootsSource>>,
    decision: OnceLock<Arc<DecisionSlot>>,
    intersection: Arc<dyn ClientRootsIntersection>,
    #[cfg(test)]
    slot_install_barrier: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
}

impl Default for ClientRootsGate {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            source: OnceLock::new(),
            decision: OnceLock::new(),
            intersection: Arc::new(TokioClientRootsIntersection),
            #[cfg(test)]
            slot_install_barrier: Mutex::new(None),
        }
    }
}

/// Path-free local-root authority reported by `artifact_status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocalRootAuthority {
    /// Local roots cannot be used because none were activated.
    Unavailable,
    /// The complete configured root policy is effective.
    Configured,
    /// A valid client snapshot narrowed the configured root policy.
    Narrowed,
    /// Client-root resolution failed closed for this session.
    Disabled,
}

/// One terminal root-authority decision shared by status and operations.
#[derive(Clone, Debug)]
pub(crate) struct RootAuthorityDecision {
    authority: LocalRootAuthority,
    effective: Option<EffectiveRootRegistry>,
}

impl RootAuthorityDecision {
    fn unavailable(registry: &RootRegistry) -> Self {
        Self {
            authority: LocalRootAuthority::Unavailable,
            effective: Some(registry.static_policy()),
        }
    }

    fn configured(registry: &RootRegistry) -> Self {
        Self {
            authority: LocalRootAuthority::Configured,
            effective: Some(registry.static_policy()),
        }
    }

    fn narrowed(effective: EffectiveRootRegistry) -> Self {
        Self {
            authority: LocalRootAuthority::Narrowed,
            effective: Some(effective),
        }
    }

    fn disabled() -> Self {
        Self {
            authority: LocalRootAuthority::Disabled,
            effective: None,
        }
    }

    /// Returns the closed, path-free authority category.
    pub(crate) const fn authority(&self) -> LocalRootAuthority {
        self.authority
    }

    /// Returns the effective import-root count without revealing identities.
    pub(crate) fn import_root_count(&self) -> usize {
        self.effective
            .as_ref()
            .map_or(0, EffectiveRootRegistry::import_root_count)
    }

    /// Returns the effective export-root count without revealing identities.
    pub(crate) fn export_root_count(&self) -> usize {
        self.effective
            .as_ref()
            .map_or(0, EffectiveRootRegistry::export_root_count)
    }

    fn effective(&self) -> Result<EffectiveRootRegistry, RootAccessError> {
        self.effective
            .clone()
            .ok_or_else(RootAccessError::client_roots)
    }
}

/// Cancellation-independent publication slot for one session decision.
struct DecisionSlot {
    source: Option<Arc<dyn ClientRootsSource>>,
    registry: RootRegistry,
    deadline: tokio::time::Instant,
    intersection: Arc<dyn ClientRootsIntersection>,
    supervisor_started: AtomicBool,
    publication: AtomicU8,
    decision: OnceLock<RootAuthorityDecision>,
    invocation: Mutex<Option<InvocationCapability>>,
    notify: Notify,
}

impl DecisionSlot {
    const PENDING: u8 = 0;
    const PUBLISHING: u8 = 1;
    const PUBLISHED: u8 = 2;

    fn new(
        source: Option<Arc<dyn ClientRootsSource>>,
        registry: RootRegistry,
        deadline: tokio::time::Instant,
        intersection: Arc<dyn ClientRootsIntersection>,
        invocation: Option<InvocationCapability>,
    ) -> Self {
        Self {
            source,
            registry,
            deadline,
            intersection,
            supervisor_started: AtomicBool::new(false),
            publication: AtomicU8::new(Self::PENDING),
            decision: OnceLock::new(),
            invocation: Mutex::new(invocation),
            notify: Notify::new(),
        }
    }

    fn start_supervisor(&self) -> bool {
        self.supervisor_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn publish(&self, decision: RootAuthorityDecision) {
        if self
            .publication
            .compare_exchange(
                Self::PENDING,
                Self::PUBLISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        let _ = self.decision.set(decision);
        self.publication.store(Self::PUBLISHED, Ordering::Release);
        drop(
            self.invocation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> RootAuthorityDecision {
        loop {
            // Register before checking the terminal state so publication
            // cannot land between the check and waiter registration.
            let notified = self.notify.notified();
            if self.publication.load(Ordering::Acquire) == Self::PUBLISHED
                && let Some(decision) = self.decision.get()
            {
                return decision.clone();
            }
            notified.await;
        }
    }
}

/// Publishes fail-closed authority when a detached supervisor is dropped.
struct SupervisorPublicationGuard {
    slot: Arc<DecisionSlot>,
    armed: bool,
}

impl SupervisorPublicationGuard {
    fn new(slot: Arc<DecisionSlot>) -> Self {
        Self { slot, armed: true }
    }

    fn publish(mut self, decision: RootAuthorityDecision) {
        self.slot.publish(decision);
        self.armed = false;
    }
}

impl Drop for SupervisorPublicationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.slot.publish(RootAuthorityDecision::disabled());
        }
    }
}

impl fmt::Debug for ClientRootsGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientRootsGate")
            .field("enabled", &self.is_enabled())
            .field("source_installed", &self.source.get().is_some())
            .field(
                "decision_frozen",
                &self.decision.get().is_some_and(|slot| {
                    slot.publication.load(Ordering::Acquire) == DecisionSlot::PUBLISHED
                }),
            )
            .finish()
    }
}

impl ClientRootsGate {
    #[cfg(test)]
    fn with_intersection(intersection: Arc<dyn ClientRootsIntersection>) -> Self {
        Self {
            intersection,
            ..Self::default()
        }
    }

    /// Enables client-root narrowing for this session.
    ///
    /// Only a transport that serves exactly one terminal client session may
    /// call this. The call is idempotent.
    pub(crate) fn enable(&self) {
        self.enabled.store(true, Ordering::Release);
    }

    /// Returns whether client-root narrowing is active for this session.
    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// Installs the terminal snapshot source exactly once.
    ///
    /// Later installs are ignored, so a session cannot be re-pointed at a
    /// different peer after its snapshot decision is taken.
    pub(crate) fn install_source(&self, source: Arc<dyn ClientRootsSource>) {
        if self.is_enabled() {
            let _ = self.source.set(source);
        }
    }

    /// Installs the initialized rmcp peer as this session's snapshot source.
    pub(crate) fn install_peer(&self, peer: &Peer<RoleServer>) {
        if self.is_enabled() && self.source.get().is_none() {
            self.install_source(Arc::new(PeerRootsSource::new(peer.clone())));
        }
    }

    /// Resolves the effective root authority for one local artifact operation.
    ///
    /// The snapshot is taken at most once per session and its outcome is
    /// frozen, so repeated operations neither re-query the client nor observe
    /// a widened authority.
    ///
    /// # Errors
    ///
    /// Returns the fixed client-root failure when the snapshot could not be
    /// securely frozen. The caller must not fall back to static policy.
    #[cfg(test)]
    pub(crate) async fn effective(
        &self,
        registry: &RootRegistry,
        timeout: std::time::Duration,
    ) -> Result<EffectiveRootRegistry, RootAccessError> {
        self.authority_at(
            registry,
            tokio::time::Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(tokio::time::Instant::now),
            None,
        )
        .await
        .effective()
    }

    pub(crate) async fn effective_scoped(
        &self,
        runtime: &RuntimeContext,
        registry: &RootRegistry,
        control_deadline: tokio::time::Instant,
    ) -> Result<EffectiveRootRegistry, RootAccessError> {
        self.authority_scoped(runtime, registry, control_deadline)
            .await
            .effective()
    }

    /// Resolves the shared path-free status and operation authority decision.
    #[cfg(test)]
    pub(crate) async fn authority(
        &self,
        registry: &RootRegistry,
        timeout: std::time::Duration,
    ) -> RootAuthorityDecision {
        self.authority_at(
            registry,
            tokio::time::Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(tokio::time::Instant::now),
            None,
        )
        .await
    }

    pub(crate) async fn authority_scoped(
        &self,
        runtime: &RuntimeContext,
        registry: &RootRegistry,
        control_deadline: tokio::time::Instant,
    ) -> RootAuthorityDecision {
        if !std::ptr::eq(self, runtime.client_roots()) {
            return RootAuthorityDecision::disabled();
        }
        let Some(invocation) = runtime.active_invocation_capability() else {
            return RootAuthorityDecision::disabled();
        };
        let deadline = invocation.deadline().min(control_deadline);
        #[cfg(test)]
        #[cfg(test)]
        let barrier = {
            self.slot_install_barrier
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        };
        #[cfg(test)]
        if let Some((entered, release)) = barrier {
            entered.notify_one();
            release.notified().await;
        }
        self.authority_at(registry, deadline, Some(invocation))
            .await
    }

    async fn authority_at(
        &self,
        registry: &RootRegistry,
        deadline: tokio::time::Instant,
        invocation: Option<InvocationCapability>,
    ) -> RootAuthorityDecision {
        if registry.import_root_count() == 0 && registry.export_root_count() == 0 {
            return RootAuthorityDecision::unavailable(registry);
        }
        if !self.is_enabled() {
            return RootAuthorityDecision::configured(registry);
        }

        let slot = self.decision_slot(registry, deadline, invocation);
        if slot.start_supervisor() {
            let supervisor_slot = Arc::clone(&slot);
            match tokio::runtime::Handle::try_current() {
                Ok(runtime) => {
                    let guard = SupervisorPublicationGuard::new(Arc::clone(&supervisor_slot));
                    let supervisor = runtime.spawn(async move {
                        let mut decision = resolve_authority(
                            supervisor_slot.source.clone(),
                            supervisor_slot.registry.clone(),
                            supervisor_slot.deadline,
                            Arc::clone(&supervisor_slot.intersection),
                        )
                        .await;
                        // Expiry wins at equality, including when the final operation
                        // became ready on the same scheduler tick.
                        if tokio::time::Instant::now() >= supervisor_slot.deadline {
                            decision = RootAuthorityDecision::disabled();
                        }
                        guard.publish(decision);
                    });
                    // DecisionSlot owns the completion signal, so the task
                    // handle is deliberately detached from every waiter.
                    drop(supervisor);
                }
                Err(_) => supervisor_slot.publish(RootAuthorityDecision::disabled()),
            }
        }
        slot.wait().await
    }

    fn decision_slot(
        &self,
        registry: &RootRegistry,
        deadline: tokio::time::Instant,
        invocation: Option<InvocationCapability>,
    ) -> Arc<DecisionSlot> {
        Arc::clone(self.decision.get_or_init(|| {
            // The set-once initializer is the linearization point for source
            // availability and budget. Every possible supervisor uses these
            // frozen values even when another caller wins the start claim.
            Arc::new(DecisionSlot::new(
                self.source.get().cloned(),
                registry.clone(),
                deadline,
                Arc::clone(&self.intersection),
                invocation,
            ))
        }))
    }
}

async fn resolve_authority(
    source: Option<Arc<dyn ClientRootsSource>>,
    registry: RootRegistry,
    deadline: tokio::time::Instant,
    intersection: Arc<dyn ClientRootsIntersection>,
) -> RootAuthorityDecision {
    if tokio::time::Instant::now() >= deadline {
        return RootAuthorityDecision::disabled();
    }
    let Some(source) = source else {
        return RootAuthorityDecision::disabled();
    };
    if tokio::time::Instant::now() >= deadline {
        return RootAuthorityDecision::disabled();
    }
    if !source.advertises_roots() {
        return RootAuthorityDecision::configured(&registry);
    }
    if tokio::time::Instant::now() >= deadline {
        return RootAuthorityDecision::disabled();
    }
    let roots = match tokio::time::timeout_at(deadline, source.list_roots()).await {
        Ok(Ok(roots)) => roots,
        Ok(Err(())) | Err(_) => return RootAuthorityDecision::disabled(),
    };
    if tokio::time::Instant::now() >= deadline {
        return RootAuthorityDecision::disabled();
    }
    let paths = match parse_client_root_snapshot(&roots) {
        Ok(paths) => paths,
        Err(_) => return RootAuthorityDecision::disabled(),
    };
    if tokio::time::Instant::now() >= deadline {
        return RootAuthorityDecision::disabled();
    }
    // The blocking closure owns only the bounded intersection inputs. It has
    // no decision slot, gate, or publication authority, so a timed-out result
    // is inert even if an operating-system filesystem call remains blocked.
    let intersection = match intersection.spawn(registry, paths) {
        Ok(intersection) => intersection,
        Err(()) => return RootAuthorityDecision::disabled(),
    };
    let effective = match tokio::time::timeout_at(deadline, intersection).await {
        Ok(Ok(Ok(effective))) => effective,
        Ok(Ok(Err(_)) | Err(_)) | Err(_) => return RootAuthorityDecision::disabled(),
    };
    if tokio::time::Instant::now() >= deadline {
        RootAuthorityDecision::disabled()
    } else {
        RootAuthorityDecision::narrowed(effective)
    }
}

/// Fixed, path-free client-root snapshot rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientRootsSnapshotError;

/// Validates one complete `roots/list` snapshot into native directory paths.
///
/// Rejects an oversize snapshot, any unusable URI, and duplicate aliases that
/// would make the intersection ambiguous.
///
/// # Errors
///
/// Returns the fixed snapshot rejection. The caller must disable local root
/// operations for the session rather than fall back to static policy.
#[allow(deprecated)]
pub(crate) fn parse_client_root_snapshot(
    roots: &[Root],
) -> Result<Vec<AbsoluteNativePath>, ClientRootsSnapshotError> {
    if roots.len() > MAX_CLIENT_ROOTS {
        return Err(ClientRootsSnapshotError);
    }
    let mut parsed = Vec::with_capacity(roots.len());
    for root in roots {
        let path = parse_client_root_uri(&root.uri)?;
        if parsed.contains(&path) {
            return Err(ClientRootsSnapshotError);
        }
        parsed.push(path);
    }
    Ok(parsed)
}

/// Decodes one canonical `file:` URI into a validated absolute native path.
///
/// # Errors
///
/// Returns the fixed snapshot rejection for a non-`file` scheme, a non-local
/// host, userinfo, a port, a query, a fragment, an encoded separator or NUL,
/// an overlong value, or a path this platform cannot represent.
pub(crate) fn parse_client_root_uri(
    uri: &str,
) -> Result<AbsoluteNativePath, ClientRootsSnapshotError> {
    if uri.is_empty() || uri.len() > MAX_ROOT_URI_BYTES {
        return Err(ClientRootsSnapshotError);
    }
    if !has_canonical_local_prefix(uri) || contains_encoded_separator(uri) {
        return Err(ClientRootsSnapshotError);
    }
    let url = Url::parse(uri).map_err(|_| ClientRootsSnapshotError)?;
    if url.scheme() != "file"
        || url.host().is_some()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ClientRootsSnapshotError);
    }
    // `Path::components` drops the trailing separator that canonical directory
    // URIs carry, and collapses repeated separators, without resolving `..`.
    let path = file_url_path(&url)?
        .components()
        .collect::<std::path::PathBuf>();
    AbsoluteNativePath::from_os_str(path.as_os_str()).map_err(|_| ClientRootsSnapshotError)
}

/// Reports whether the raw URI uses the canonical local `file:` authority form.
///
/// Only the empty and `localhost` authorities are local, and only the explicit
/// `file://.../` form is canonical, so scheme-relative and host-bearing
/// spellings are rejected before any normalization can rewrite them.
fn has_canonical_local_prefix(uri: &str) -> bool {
    let lowered = uri.to_ascii_lowercase();
    lowered.starts_with("file:///") || lowered.starts_with("file://localhost/")
}

/// Reports whether the raw URI encodes a separator or NUL.
///
/// An encoded separator would let one URI address a path the same client could
/// not name literally, so it is rejected before any decoding.
fn contains_encoded_separator(uri: &str) -> bool {
    let bytes = uri.as_bytes();
    bytes.windows(3).any(|window| {
        window[0] == b'%'
            && matches!(
                (
                    window[1].to_ascii_lowercase(),
                    window[2].to_ascii_lowercase()
                ),
                (b'2', b'f') | (b'5', b'c') | (b'0', b'0')
            )
    })
}

#[cfg(any(unix, windows))]
fn file_url_path(url: &Url) -> Result<std::path::PathBuf, ClientRootsSnapshotError> {
    url.to_file_path().map_err(|()| ClientRootsSnapshotError)
}

#[cfg(not(any(unix, windows)))]
fn file_url_path(_: &Url) -> Result<std::path::PathBuf, ClientRootsSnapshotError> {
    Err(ClientRootsSnapshotError)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use std::{
        fs,
        path::Path,
        sync::{Condvar, Mutex, atomic::AtomicUsize},
        time::Duration,
    };

    use anytype::prelude::{AnytypeClient, ClientConfig};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        artifact_config::{ArtifactConfig, RelativeNativePath},
        artifact_roots::RootAccessErrorKind,
        runtime::StartupStatus,
    };

    fn invocation_runtime(max_concurrency: usize, timeout: Duration) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("any-mcp-client-roots-test".to_owned()),
            app_name: "any-mcp-client-roots-test".to_owned(),
            ..ClientConfig::default()
        })
        .expect("test client");
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

    struct ScriptedRoots {
        advertises: bool,
        calls: AtomicUsize,
        answer: Option<Vec<Root>>,
        stall: bool,
    }

    impl ScriptedRoots {
        fn advertised(roots: Vec<Root>) -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: Some(roots),
                stall: false,
            }
        }

        fn silent() -> Self {
            Self {
                advertises: false,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: false,
            }
        }

        fn failing() -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: false,
            }
        }

        fn stalled() -> Self {
            Self {
                advertises: true,
                calls: AtomicUsize::new(0),
                answer: None,
                stall: true,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    impl ClientRootsSource for ScriptedRoots {
        fn advertises_roots(&self) -> bool {
            self.advertises
        }

        fn list_roots(&self) -> SnapshotFuture<'_> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                if self.stall {
                    std::future::pending::<()>().await;
                }
                self.answer.clone().ok_or(())
            })
        }
    }

    struct DeferredRoots {
        calls: AtomicUsize,
        answer: Vec<Root>,
        release: Notify,
    }

    impl DeferredRoots {
        fn new(answer: Vec<Root>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                answer,
                release: Notify::new(),
            }
        }

        async fn wait_until_called(&self) {
            while self.calls.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        }
    }

    impl ClientRootsSource for DeferredRoots {
        fn advertises_roots(&self) -> bool {
            true
        }

        fn list_roots(&self) -> SnapshotFuture<'_> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                self.release.notified().await;
                Ok(self.answer.clone())
            })
        }
    }

    #[derive(Clone, Copy)]
    enum IntersectionMode {
        Blocking,
        Panic,
        SpawnFailure,
    }

    struct ScriptedIntersection {
        mode: IntersectionMode,
        calls: AtomicUsize,
        entered: AtomicBool,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl ScriptedIntersection {
        fn new(mode: IntersectionMode) -> Self {
            Self {
                mode,
                calls: AtomicUsize::new(0),
                entered: AtomicBool::new(false),
                release: Arc::new((Mutex::new(false), Condvar::new())),
            }
        }

        async fn wait_until_entered(&self) {
            while !self.entered.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        }

        fn release(&self) {
            let (lock, wake) = self.release.as_ref();
            let mut released = lock.lock().expect("intersection release lock");
            *released = true;
            wake.notify_all();
        }
    }

    impl ClientRootsIntersection for ScriptedIntersection {
        fn spawn(
            &self,
            registry: RootRegistry,
            paths: Vec<AbsoluteNativePath>,
        ) -> Result<IntersectionHandle, ()> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            if matches!(self.mode, IntersectionMode::SpawnFailure) {
                return Err(());
            }
            let mode = self.mode;
            let release = Arc::clone(&self.release);
            self.entered.store(true, Ordering::Release);
            Ok(tokio::task::spawn_blocking(move || {
                if matches!(mode, IntersectionMode::Panic) {
                    panic!("scripted intersection panic");
                }
                if matches!(mode, IntersectionMode::Blocking) {
                    let (lock, wake) = release.as_ref();
                    let mut released = lock.lock().expect("intersection release lock");
                    while !*released {
                        released = wake.wait(released).expect("intersection release wait");
                    }
                }
                registry.intersect_client_roots(&paths)
            }))
        }
    }

    fn config(import: &Path, export: &Path) -> ArtifactConfig {
        ArtifactConfig::from_toml(&format!(
            "schema_version = 1\n[spaces]\nread_only = false\n\
             [[roots.import]]\nid = \"inbox\"\npath = {import:?}\n\
             [[roots.export]]\nid = \"outbox\"\npath = {export:?}\n"
        ))
        .expect("root config")
    }

    fn temporary_tree() -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!(
                "any-mcp-client-roots-{}-{}",
                std::process::id(),
                getrandom::u64().unwrap_or(0)
            ));
        let import = base.join("import");
        let export = base.join("export");
        fs::create_dir_all(&import).expect("import directory");
        fs::create_dir_all(&export).expect("export directory");
        crate::artifact_roots::prepare_test_private_directory(&base)
            .expect("private base directory");
        crate::artifact_roots::prepare_test_private_directory(&import)
            .expect("private import directory");
        crate::artifact_roots::prepare_test_private_directory(&export)
            .expect("private export directory");
        (base, import, export)
    }

    /// Removes a fixture tree once every capability handle has closed.
    ///
    /// A blocking intersection task, or a decision that still owns a registry
    /// clone, may release its directory handles slightly after the test's own
    /// drops. Windows refuses to delete a directory with an open handle, so
    /// removal retries briefly instead of racing that release.
    fn remove_temporary_tree(base: std::path::PathBuf) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match fs::remove_dir_all(&base) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(error) => {
                    assert!(std::time::Instant::now() < deadline, "cleanup: {error}");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }

    fn relative(value: &str) -> RelativeNativePath {
        RelativeNativePath::from_utf8(value).expect("relative path")
    }

    fn directory_uri(path: &Path) -> String {
        Url::from_directory_path(path)
            .expect("directory URI")
            .to_string()
    }

    #[test]
    fn client_root_uris_admit_only_local_absolute_directories() {
        let inbox = directory_uri(&std::env::temp_dir().join("inbox"));
        let local_inbox = inbox.replacen("file:///", "file://localhost/", 1);
        let with_space = directory_uri(&std::env::temp_dir().join("with space"));
        assert!(parse_client_root_uri(&inbox).is_ok());
        assert!(parse_client_root_uri(&local_inbox).is_ok());
        assert!(parse_client_root_uri(&with_space).is_ok());

        for rejected in [
            "",
            "http://127.0.0.1/tmp",
            "file://example.com/tmp",
            "file://user@localhost/tmp",
            "file:///tmp?query=1",
            "file:///tmp#fragment",
            "file:///tmp/a%2Fb",
            "file:///tmp/a%00b",
            "file:relative/path",
            "file:/tmp/inbox",
            "FILE://EXAMPLE.COM/tmp",
            "not a uri",
        ] {
            assert!(
                parse_client_root_uri(rejected).is_err(),
                "expected rejection: {rejected}"
            );
        }
    }

    #[test]
    fn a_canonical_directory_uri_ignores_its_trailing_separator() {
        let directory = directory_uri(&std::env::temp_dir().join("inbox"));
        let without_separator = directory.trim_end_matches('/');
        assert_eq!(
            parse_client_root_uri(&directory).expect("directory URI"),
            parse_client_root_uri(without_separator).expect("plain URI")
        );
    }

    #[test]
    fn overlong_client_root_uris_are_rejected_before_parsing() {
        let long = format!("file:///{}", "a".repeat(MAX_ROOT_URI_BYTES));
        assert_eq!(parse_client_root_uri(&long), Err(ClientRootsSnapshotError));
    }

    #[test]
    fn snapshots_reject_oversize_and_duplicate_entries() {
        let oversize = (0..=MAX_CLIENT_ROOTS)
            .map(|index| Root::new(format!("file:///tmp/root-{index}")))
            .collect::<Vec<_>>();
        assert!(parse_client_root_snapshot(&oversize).is_err());

        let inbox = directory_uri(&std::env::temp_dir().join("inbox"));
        let inbox_with_dot = format!("{}/./", inbox.trim_end_matches('/'));
        let duplicates = vec![Root::new(inbox.clone()), Root::new(inbox_with_dot)];
        assert!(parse_client_root_snapshot(&duplicates).is_err());

        let accepted = vec![
            Root::new(inbox),
            Root::new(directory_uri(&std::env::temp_dir().join("outbox"))),
        ];
        assert_eq!(
            parse_client_root_snapshot(&accepted)
                .expect("accepted snapshot")
                .len(),
            2
        );
    }

    #[test]
    fn unavailable_runtime_publishes_one_disabled_decision_without_retry() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(source.clone());

        let first = futures::executor::block_on(gate.authority(&registry, Duration::from_secs(1)));
        let repeated =
            futures::executor::block_on(gate.authority(&registry, Duration::from_secs(1)));

        assert_eq!(first.authority(), LocalRootAuthority::Disabled);
        assert_eq!(repeated.authority(), LocalRootAuthority::Disabled);
        assert_eq!(source.calls(), 0);
        assert!(gate.decision.get().is_some_and(
            |slot| slot.publication.load(Ordering::Acquire) == DecisionSlot::PUBLISHED
        ));

        drop(repeated);
        drop(first);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn slot_initialization_freezes_source_before_a_later_caller_claims_start() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let frozen_deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        // This models the initializer being preempted before it can claim the
        // supervisor. The later caller may claim start but must consume the
        // missing source frozen by this first initialization.
        let initialized = gate.decision_slot(&registry, frozen_deadline, None);
        let late = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(late.clone());
        let decision = gate.authority(&registry, Duration::from_secs(60)).await;

        assert!(initialized.source.is_none());
        assert_eq!(initialized.deadline, frozen_deadline);
        assert!(initialized.supervisor_started.load(Ordering::Acquire));
        assert_eq!(decision.authority(), LocalRootAuthority::Disabled);
        assert_eq!(late.calls(), 0);

        drop(decision);
        drop(initialized);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn later_caller_cannot_extend_the_frozen_deadline() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = Arc::new(ClientRootsGate::default());
        gate.enable();
        let source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        gate.install_source(source.clone());

        let first_gate = Arc::clone(&gate);
        let first_registry = registry.clone();
        let first = tokio::spawn(async move {
            first_gate
                .authority(&first_registry, Duration::from_secs(5))
                .await
        });
        source.wait_until_called().await;
        tokio::time::advance(Duration::from_secs(4)).await;

        let later_gate = Arc::clone(&gate);
        let later_registry = registry.clone();
        let later = tokio::spawn(async move {
            later_gate
                .authority(&later_registry, Duration::from_secs(60))
                .await
        });
        tokio::time::advance(Duration::from_secs(1)).await;

        assert_eq!(
            first.await.expect("first deadline waiter").authority(),
            LocalRootAuthority::Disabled
        );
        assert_eq!(
            later.await.expect("later deadline waiter").authority(),
            LocalRootAuthority::Disabled
        );
        assert_eq!(source.calls.load(Ordering::Acquire), 1);

        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn delayed_first_status_cannot_reset_or_extend_the_ingress_deadline() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(source.clone());
        let ingress_deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        tokio::time::advance(Duration::from_secs(5)).await;
        let first_status = gate.authority_at(&registry, ingress_deadline, None).await;
        let later_status = gate.authority(&registry, Duration::from_secs(60)).await;

        assert_eq!(first_status.authority(), LocalRootAuthority::Disabled);
        assert_eq!(later_status.authority(), LocalRootAuthority::Disabled);
        assert_eq!(source.calls(), 0);
        assert!(gate.decision.get().is_some_and(|slot| {
            slot.deadline == ingress_deadline
                && slot.publication.load(Ordering::Acquire) == DecisionSlot::PUBLISHED
        }));

        drop(later_status);
        drop(first_status);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn decision_slot_retains_capacity_after_all_waiters_cancel() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let runtime = invocation_runtime(1, Duration::from_secs(5));
        runtime.client_roots().enable();
        let source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        runtime.client_roots().install_source(source.clone());
        let cancellation = CancellationToken::new();
        let capability = runtime
            .admit_invocation("artifact_status", &cancellation)
            .await
            .expect("first admission");
        let operation_runtime = runtime.clone();
        let first_registry = registry.clone();
        let second_registry = registry.clone();
        let operation_cancellation = cancellation.clone();
        let running_runtime = runtime.clone();
        let running = tokio::spawn(async move {
            running_runtime
                .run_invocation(
                    capability,
                    &operation_cancellation,
                    Box::pin(async move {
                        tokio::join!(
                            operation_runtime.client_roots().authority_scoped(
                                &operation_runtime,
                                &first_registry,
                                tokio::time::Instant::now() + Duration::from_secs(5),
                            ),
                            operation_runtime.client_roots().authority_scoped(
                                &operation_runtime,
                                &second_registry,
                                tokio::time::Instant::now() + Duration::from_secs(5),
                            ),
                        )
                    }),
                )
                .await
        });
        source.wait_until_called().await;
        cancellation.cancel();
        let failure = running
            .await
            .expect("first invocation join")
            .expect_err("all authority waiters cancel");
        assert_eq!(
            failure.kind,
            crate::runtime::ControlledFailureKind::Cancelled
        );

        let waiter_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            waiter_runtime
                .admit_invocation("artifact_status", &CancellationToken::new())
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "decision slot must retain the permit"
        );

        source.release.notify_one();
        let second_capability = tokio::time::timeout(Duration::from_secs(1), second)
            .await
            .expect("capacity release deadline")
            .expect("second admission join")
            .expect("second admission");
        drop(second_capability);
        assert_eq!(source.calls.load(Ordering::Acquire), 1);
        let slot = runtime
            .client_roots()
            .decision
            .get()
            .expect("frozen decision slot");
        assert_eq!(
            slot.publication.load(Ordering::Acquire),
            DecisionSlot::PUBLISHED
        );
        assert!(
            slot.invocation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        );

        drop(runtime);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn preemption_before_slot_install_keeps_the_original_ingress_deadline() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let runtime = invocation_runtime(1, Duration::from_secs(5));
        runtime.client_roots().enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        runtime.client_roots().install_source(source.clone());
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *runtime
            .client_roots()
            .slot_install_barrier
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((Arc::clone(&entered), Arc::clone(&release)));
        let cancellation = CancellationToken::new();
        let capability = runtime
            .admit_invocation("artifact_status", &cancellation)
            .await
            .expect("first admission");
        let ingress_deadline = capability.deadline();
        let control_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let supervisor_runtime = runtime.clone();
        let operation_registry = registry.clone();
        let operation_cancellation = cancellation.clone();
        let running_runtime = runtime.clone();
        let (supervisor_sender, supervisor_receiver) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(async move {
            running_runtime
                .run_invocation(
                    capability,
                    &operation_cancellation,
                    Box::pin(async move {
                        let authority_runtime = supervisor_runtime.clone();
                        let supervisor =
                            supervisor_runtime.spawn_invocation_supervisor(async move {
                                authority_runtime
                                    .client_roots()
                                    .authority_scoped(
                                        &authority_runtime,
                                        &operation_registry,
                                        control_deadline,
                                    )
                                    .await
                            });
                        let _ = supervisor_sender.send(supervisor);
                        std::future::pending::<()>().await;
                    }),
                )
                .await
        });
        let supervisor = supervisor_receiver.await.expect("supervisor handle");
        entered.notified().await;
        assert!(runtime.client_roots().decision.get().is_none());

        tokio::time::advance(Duration::from_secs(5)).await;
        let failure = running
            .await
            .expect("first invocation join")
            .expect_err("ingress deadline");
        assert_eq!(
            failure.kind,
            crate::runtime::ControlledFailureKind::TimedOut
        );
        let waiter_runtime = runtime.clone();
        let second = tokio::spawn(async move {
            waiter_runtime
                .admit_invocation("artifact_status", &CancellationToken::new())
                .await
        });
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "supervisor retains the invocation lease"
        );

        release.notify_one();
        let decision = supervisor.await.expect("supervisor join");
        assert_eq!(decision.authority(), LocalRootAuthority::Disabled);
        assert_eq!(source.calls(), 0, "expired slot must not start source work");
        let slot = runtime
            .client_roots()
            .decision
            .get()
            .expect("terminal decision slot");
        assert_eq!(slot.deadline, ingress_deadline);
        assert_eq!(
            slot.publication.load(Ordering::Acquire),
            DecisionSlot::PUBLISHED
        );
        assert!(
            slot.invocation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none()
        );
        let second_capability = second
            .await
            .expect("second admission join")
            .expect("capacity releases after publication");
        drop(second_capability);

        drop(runtime);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn scoped_authority_rejects_absent_and_foreign_capabilities() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let primary = invocation_runtime(1, Duration::from_secs(1));
        primary.client_roots().enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        primary.client_roots().install_source(source.clone());

        let absent = primary
            .client_roots()
            .authority_scoped(
                &primary,
                &registry,
                tokio::time::Instant::now() + Duration::from_secs(1),
            )
            .await;
        assert_eq!(absent.authority(), LocalRootAuthority::Disabled);
        assert!(primary.client_roots().decision.get().is_none());

        let foreign = invocation_runtime(1, Duration::from_secs(1));
        let cancellation = CancellationToken::new();
        let capability = primary
            .admit_invocation("artifact_status", &cancellation)
            .await
            .expect("primary capability");
        let foreign_runtime = foreign.clone();
        let primary_runtime = primary.clone();
        let foreign_registry = registry.clone();
        let decision = primary
            .run_invocation(
                capability,
                &cancellation,
                Box::pin(async move {
                    primary_runtime
                        .client_roots()
                        .authority_scoped(
                            &foreign_runtime,
                            &foreign_registry,
                            tokio::time::Instant::now() + Duration::from_secs(1),
                        )
                        .await
                }),
            )
            .await
            .expect("foreign probe result");
        assert_eq!(decision.authority(), LocalRootAuthority::Disabled);
        assert!(primary.client_roots().decision.get().is_none());
        assert_eq!(source.calls(), 0);

        drop(primary);
        drop(foreign);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[test]
    fn runtime_teardown_drops_the_supervisor_into_one_disabled_publication() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = Arc::new(ClientRootsGate::default());
        gate.enable();
        let source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        gate.install_source(source.clone());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let waiter_gate = Arc::clone(&gate);
            let waiter_registry = registry.clone();
            let waiter = tokio::spawn(async move {
                waiter_gate
                    .authority(&waiter_registry, Duration::from_secs(60))
                    .await
            });
            source.wait_until_called().await;
            drop(waiter);
        });
        drop(runtime);

        let decision =
            futures::executor::block_on(gate.authority(&registry, Duration::from_secs(60)));
        assert_eq!(decision.authority(), LocalRootAuthority::Disabled);
        assert_eq!(source.calls.load(Ordering::Acquire), 1);
        assert!(gate.decision.get().is_some_and(
            |slot| slot.publication.load(Ordering::Acquire) == DecisionSlot::PUBLISHED
        ));

        drop(decision);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[test]
    fn production_intersection_spawn_without_a_runtime_fails_closed() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");

        assert!(
            TokioClientRootsIntersection
                .spawn(registry.clone(), Vec::new())
                .is_err()
        );

        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn a_disabled_gate_keeps_static_policy_without_a_snapshot() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        let source = Arc::new(ScriptedRoots::advertised(Vec::new()));
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("static policy");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_ok()
        );
        assert_eq!(source.calls(), 0);
        drop(effective);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn a_client_without_roots_capability_keeps_static_policy() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::silent());
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("static fallback");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_ok()
        );
        assert_eq!(source.calls(), 0);
        drop(effective);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn one_snapshot_narrows_every_later_operation() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(source.clone());
        fs::write(import.join("source.bin"), b"source").expect("source");

        for _ in 0..3 {
            let effective = gate
                .effective(&registry, Duration::from_secs(1))
                .await
                .expect("narrowed policy");
            assert!(
                effective
                    .open_import("inbox", &relative("source.bin"), 64)
                    .is_ok()
            );
            assert!(
                effective
                    .begin_atomic_export("outbox", &relative("denied.bin"), 64)
                    .is_err()
            );
        }

        assert_eq!(source.calls(), 1);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn an_empty_snapshot_denies_every_local_root() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(Vec::new())));
        fs::write(import.join("source.bin"), b"source").expect("source");

        let effective = gate
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("empty intersection");

        assert!(
            effective
                .open_import("inbox", &relative("source.bin"), 64)
                .is_err()
        );
        drop(effective);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn a_failed_snapshot_freezes_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::failing());
        gate.install_source(source.clone());

        for _ in 0..2 {
            let error = gate
                .effective(&registry, Duration::from_secs(1))
                .await
                .expect_err("frozen disabled");
            assert_eq!(error.kind(), RootAccessErrorKind::ClientRoots);
        }

        assert_eq!(source.calls(), 1);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn an_unusable_snapshot_uri_freezes_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            "http://example.com/tmp",
        )])));

        assert!(
            gate.effective(&registry, Duration::from_secs(1))
                .await
                .is_err()
        );
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_snapshot_times_out_into_disabled_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::stalled()));

        assert!(
            gate.effective(&registry, Duration::from_secs(5))
                .await
                .is_err()
        );
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn an_enabled_gate_without_a_source_denies_local_roots() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();

        assert!(
            gate.effective(&registry, Duration::from_secs(1))
                .await
                .is_err()
        );
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn configured_narrowed_empty_and_disabled_decisions_report_effective_counts() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");

        let direct = ClientRootsGate::default()
            .authority(&registry, Duration::from_secs(1))
            .await;
        assert_eq!(direct.authority(), LocalRootAuthority::Configured);
        assert_eq!(
            (direct.import_root_count(), direct.export_root_count()),
            (1, 1)
        );

        let narrowed_gate = ClientRootsGate::default();
        narrowed_gate.enable();
        narrowed_gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            directory_uri(&import),
        )])));
        let narrowed = narrowed_gate
            .authority(&registry, Duration::from_secs(1))
            .await;
        assert_eq!(narrowed.authority(), LocalRootAuthority::Narrowed);
        assert_eq!(
            (narrowed.import_root_count(), narrowed.export_root_count()),
            (1, 0)
        );

        let empty_gate = ClientRootsGate::default();
        empty_gate.enable();
        empty_gate.install_source(Arc::new(ScriptedRoots::advertised(Vec::new())));
        let empty = empty_gate
            .authority(&registry, Duration::from_secs(1))
            .await;
        assert_eq!(empty.authority(), LocalRootAuthority::Narrowed);
        assert_eq!(
            (empty.import_root_count(), empty.export_root_count()),
            (0, 0)
        );

        let disabled_gate = ClientRootsGate::default();
        disabled_gate.enable();
        disabled_gate.install_source(Arc::new(ScriptedRoots::failing()));
        let disabled = disabled_gate
            .authority(&registry, Duration::from_secs(1))
            .await;
        assert_eq!(disabled.authority(), LocalRootAuthority::Disabled);
        assert_eq!(
            (disabled.import_root_count(), disabled.export_root_count()),
            (0, 0)
        );

        drop((direct, narrowed, empty, disabled));
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn rootless_policy_is_unavailable_without_client_work() {
        let config = ArtifactConfig::from_toml("schema_version = 1\n[spaces]\nread_only = false\n")
            .expect("rootless config");
        let registry = RootRegistry::activate(&config).expect("activate rootless config");
        let gate = ClientRootsGate::default();
        gate.enable();
        let source = Arc::new(ScriptedRoots::failing());
        gate.install_source(source.clone());

        let decision = gate.authority(&registry, Duration::from_secs(1)).await;

        assert_eq!(decision.authority(), LocalRootAuthority::Unavailable);
        assert_eq!(
            (decision.import_root_count(), decision.export_root_count()),
            (0, 0)
        );
        assert_eq!(source.calls(), 0);
        assert_eq!(
            decision
                .effective()
                .expect("empty static policy")
                .open_import("inbox", &relative("source.bin"), 64)
                .expect_err("rootless refusal")
                .kind(),
            RootAccessErrorKind::Missing
        );
    }

    #[tokio::test]
    async fn cancelling_every_waiter_does_not_cancel_the_single_supervisor() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = Arc::new(ClientRootsGate::default());
        gate.enable();
        let source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        gate.install_source(source.clone());

        let mut waiters = Vec::new();
        for _ in 0..32 {
            let gate = Arc::clone(&gate);
            let registry = registry.clone();
            waiters.push(tokio::spawn(async move {
                gate.authority(&registry, Duration::from_secs(5)).await
            }));
        }
        source.wait_until_called().await;
        for waiter in waiters {
            waiter.abort();
        }
        source.release.notify_one();

        let decision = gate.authority(&registry, Duration::from_secs(5)).await;
        assert_eq!(decision.authority(), LocalRootAuthority::Narrowed);
        assert_eq!(source.calls.load(Ordering::Acquire), 1);

        drop(decision);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn cancelling_during_intersection_keeps_one_shared_supervisor() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let intersection = Arc::new(ScriptedIntersection::new(IntersectionMode::Blocking));
        let gate = Arc::new(ClientRootsGate::with_intersection(intersection.clone()));
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            directory_uri(&import),
        )])));

        let waiter_gate = Arc::clone(&gate);
        let waiter_registry = registry.clone();
        let waiter = tokio::spawn(async move {
            waiter_gate
                .authority(&waiter_registry, Duration::from_secs(5))
                .await
        });
        intersection.wait_until_entered().await;
        waiter.abort();
        intersection.release();

        let decision = gate.authority(&registry, Duration::from_secs(5)).await;
        assert_eq!(decision.authority(), LocalRootAuthority::Narrowed);
        assert_eq!(intersection.calls.load(Ordering::Acquire), 1);

        drop(decision);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn resolution_before_source_install_is_terminally_disabled() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let gate = ClientRootsGate::default();
        gate.enable();

        let first = gate.authority(&registry, Duration::from_secs(1)).await;
        assert_eq!(first.authority(), LocalRootAuthority::Disabled);

        let late = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        gate.install_source(late.clone());
        let repeated = gate.authority(&registry, Duration::from_secs(1)).await;
        assert_eq!(repeated.authority(), LocalRootAuthority::Disabled);
        assert_eq!(late.calls(), 0);

        drop(repeated);
        drop(first);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn status_and_operation_order_share_the_same_terminal_decision() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");

        let status_first = ClientRootsGate::default();
        status_first.enable();
        let status_source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        status_first.install_source(status_source.clone());
        let status_decision = status_first
            .authority(&registry, Duration::from_secs(1))
            .await;
        let status_effective = status_first
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("status-first effective roots");

        let operation_first = ClientRootsGate::default();
        operation_first.enable();
        let operation_source = Arc::new(ScriptedRoots::advertised(vec![Root::new(directory_uri(
            &import,
        ))]));
        operation_first.install_source(operation_source.clone());
        let operation_effective = operation_first
            .effective(&registry, Duration::from_secs(1))
            .await
            .expect("operation-first effective roots");
        let operation_decision = operation_first
            .authority(&registry, Duration::from_secs(1))
            .await;

        assert_eq!(status_decision.authority(), operation_decision.authority());
        assert_eq!(
            (
                status_decision.import_root_count(),
                status_decision.export_root_count()
            ),
            (
                operation_decision.import_root_count(),
                operation_decision.export_root_count()
            )
        );
        assert_eq!(
            (
                status_effective.import_root_count(),
                status_effective.export_root_count()
            ),
            (
                operation_effective.import_root_count(),
                operation_effective.export_root_count()
            )
        );
        assert_eq!(status_source.calls(), 1);
        assert_eq!(operation_source.calls(), 1);

        drop(operation_effective);
        drop(status_effective);
        drop(operation_first);
        drop(status_first);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn completion_one_tick_before_deadline_wins_but_equality_disables() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");

        let before_gate = Arc::new(ClientRootsGate::default());
        before_gate.enable();
        let before_source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        before_gate.install_source(before_source.clone());
        let waiter_gate = Arc::clone(&before_gate);
        let waiter_registry = registry.clone();
        let before = tokio::spawn(async move {
            waiter_gate
                .authority(&waiter_registry, Duration::from_secs(5))
                .await
        });
        before_source.wait_until_called().await;
        tokio::time::advance(Duration::from_secs(5) - Duration::from_nanos(1)).await;
        before_source.release.notify_one();
        let before = before.await.expect("one-tick-before waiter");
        assert_eq!(before.authority(), LocalRootAuthority::Narrowed);

        let equal_gate = Arc::new(ClientRootsGate::default());
        equal_gate.enable();
        let equal_source = Arc::new(DeferredRoots::new(vec![Root::new(directory_uri(&import))]));
        equal_gate.install_source(equal_source.clone());
        let waiter_gate = Arc::clone(&equal_gate);
        let waiter_registry = registry.clone();
        let equal = tokio::spawn(async move {
            waiter_gate
                .authority(&waiter_registry, Duration::from_secs(5))
                .await
        });
        equal_source.wait_until_called().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        equal_source.release.notify_one();
        let equal = equal.await.expect("deadline-equality waiter");
        assert_eq!(equal.authority(), LocalRootAuthority::Disabled);

        drop(equal);
        drop(before);
        drop(equal_gate);
        drop(before_gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_blocking_intersection_cannot_publish_late() {
        let (base, import, export) = temporary_tree();
        let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
        let intersection = Arc::new(ScriptedIntersection::new(IntersectionMode::Blocking));
        let gate = Arc::new(ClientRootsGate::with_intersection(intersection.clone()));
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            directory_uri(&import),
        )])));
        let waiter_gate = Arc::clone(&gate);
        let waiter_registry = registry.clone();
        let waiter = tokio::spawn(async move {
            waiter_gate
                .authority(&waiter_registry, Duration::from_secs(5))
                .await
        });
        intersection.wait_until_entered().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        let timed_out = waiter.await.expect("timed-out waiter");
        assert_eq!(timed_out.authority(), LocalRootAuthority::Disabled);

        intersection.release();
        tokio::task::yield_now().await;
        let repeated = gate.authority(&registry, Duration::from_secs(5)).await;
        assert_eq!(repeated.authority(), LocalRootAuthority::Disabled);
        assert_eq!(intersection.calls.load(Ordering::Acquire), 1);

        drop(repeated);
        drop(timed_out);
        drop(gate);
        drop(registry);
        remove_temporary_tree(base);
    }

    #[tokio::test]
    async fn intersection_spawn_failure_and_join_panic_disable_without_panicking() {
        for mode in [IntersectionMode::SpawnFailure, IntersectionMode::Panic] {
            let (base, import, export) = temporary_tree();
            let registry = RootRegistry::activate(&config(&import, &export)).expect("activate");
            let intersection = Arc::new(ScriptedIntersection::new(mode));
            let gate = ClientRootsGate::with_intersection(intersection);
            gate.enable();
            gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
                directory_uri(&import),
            )])));

            let decision = gate.authority(&registry, Duration::from_secs(1)).await;
            assert_eq!(decision.authority(), LocalRootAuthority::Disabled);

            drop(decision);
            drop(gate);
            drop(registry);
            remove_temporary_tree(base);
        }
    }

    #[test]
    fn gate_debug_output_carries_no_client_paths() {
        let gate = ClientRootsGate::default();
        gate.enable();
        gate.install_source(Arc::new(ScriptedRoots::advertised(vec![Root::new(
            "file:///tmp/secret-inbox",
        )])));

        let rendered = format!("{gate:?}");
        assert!(!rendered.contains("secret-inbox"));
        assert!(rendered.contains("enabled: true"));
    }
}
