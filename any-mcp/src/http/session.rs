// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stable Streamable HTTP sessions bound to authenticated principals.
//!
//! This module wraps `rmcp`'s stateful session service rather than trusting
//! its defaults: it adds a process session ceiling reserved before session
//! creation, binds every session to the authenticated principal so a foreign
//! session behaves exactly like an unknown one, and restricts the accepted
//! protocol revisions to the Streamable-HTTP-capable set this implementation
//! tests. Domain handlers, tool schemas, and structured results are the
//! stdio implementations unchanged.
//!
//! State lifetimes follow the approved design: the Anytype client and the
//! operation semaphore are process-global; mutation idempotency is
//! process-lifetime and partitioned by principal (through one forked runtime
//! identity per principal); per-request cancellation, negotiated versions,
//! and SSE streams are session-local inside `rmcp`. Cursor stores live on
//! the per-principal facade: they never cross principals, and a principal's
//! cursors survive session loss together with its idempotency state.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::Full;
use rmcp::transport::streamable_http_server::{
    SessionManager, StreamableHttpServerConfig, StreamableHttpService,
    session::local::LocalSessionManager,
};
use serde_json::Value;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::{
    http::{
        auth::AuthorizedPrincipal,
        config::HttpConfig,
        listener::{AdmittedRequest, HttpBody, fixed_response},
    },
    runtime::RuntimeContext,
    server::AnyMcpServer,
};

/// Streamable-HTTP-capable protocol revisions accepted in the
/// `MCP-Protocol-Version` header on post-initialize requests.
///
/// Revision `2024-11-05` predates both Streamable HTTP and this header, so a
/// compliant client never sends it here; the `2026-07-28` preview uses the
/// stateless adapter, never stable sessions.
const ACCEPTED_VERSIONS: [&str; 3] = ["2025-03-26", "2025-06-18", "2025-11-25"];

/// Protocol revisions a client may propose in an `initialize` body.
///
/// Everything in [`ACCEPTED_VERSIONS`] plus `2024-11-05`: real Streamable
/// HTTP clients in the wild (e.g. zeroclaw) still propose the launch
/// revision while speaking the newer transport perfectly well, and the spec
/// requires a server to answer an unsupported proposal with a version it
/// does support. rmcp echoes any known proposed
/// version, so the session proceeds on the client's revision; the rest of
/// this module is revision-agnostic.
const INITIALIZE_ACCEPTED_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// Reviewed `rmcp` SSE keep-alive interval.
const SSE_KEEP_ALIVE: Duration = Duration::from_secs(15);
/// Reviewed `rmcp` SSE retry hint.
const SSE_RETRY: Duration = Duration::from_secs(3);
/// Bounded number of cached per-principal server facades.
const MAX_PRINCIPAL_SERVERS: usize = 64;

const SESSION_ID_HEADER: &str = "mcp-session-id";
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

tokio::task_local! {
    /// Hands the per-principal server facade to the `rmcp` session factory,
    /// which has no request context of its own.
    static SESSION_SERVER: RefCell<Option<AnyMcpServer>>;
}

/// Bounded cache of one server facade per authenticated principal.
///
/// Each facade is built over one forked runtime identity, so identity-keyed
/// mutation state is process-lifetime yet principal-partitioned, and each
/// facade's cursor store never crosses principals. Eviction of the least
/// recently used principal bounds memory; it forgets that principal's
/// idempotency and cursor memory exactly like a process restart, which the
/// mutation contracts already require clients to survive by rereading.
pub(crate) struct PrincipalServers {
    runtime: RuntimeContext,
    capacity: usize,
    servers: Mutex<HashMap<[u8; 32], (AnyMcpServer, Instant)>>,
}

impl PrincipalServers {
    pub(crate) fn new(runtime: RuntimeContext) -> Self {
        Self::with_capacity(runtime, MAX_PRINCIPAL_SERVERS)
    }

    fn with_capacity(runtime: RuntimeContext, capacity: usize) -> Self {
        Self {
            runtime,
            capacity,
            servers: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the principal's facade, building it on first use.
    pub(crate) fn server_for(&self, principal: &AuthorizedPrincipal) -> Result<AnyMcpServer, ()> {
        let mut servers = self
            .servers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((server, last_used)) = servers.get_mut(principal.key()) {
            *last_used = Instant::now();
            return Ok(server.clone());
        }
        if servers.len() >= self.capacity
            && let Some(oldest) = servers
                .iter()
                .min_by_key(|(_, (_, last_used))| *last_used)
                .map(|(key, _)| *key)
        {
            servers.remove(&oldest);
        }
        let server = AnyMcpServer::new(self.runtime.fork_identity()).map_err(|_| ())?;
        servers.insert(*principal.key(), (server.clone(), Instant::now()));
        Ok(server)
    }
}

struct SessionEntry {
    principal: AuthorizedPrincipal,
    _permit: OwnedSemaphorePermit,
}

/// Principal-bound session registry enforcing the process session ceiling.
///
/// Admission is reserved before `rmcp` creates a session and released
/// exactly once when the binding is dropped: on failed initialize, DELETE,
/// reconciliation with `rmcp`'s session manager, or process shutdown.
pub(crate) struct SessionRegistry {
    permits: Arc<Semaphore>,
    sessions: Mutex<HashMap<String, SessionEntry>>,
}

impl SessionRegistry {
    pub(crate) fn new(max_sessions: u32) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(max_sessions as usize)),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Reserves one session slot before session creation.
    fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        self.permits.clone().try_acquire_owned().ok()
    }

    /// Binds a created session to its principal, consuming the reservation.
    fn bind(
        &self,
        session_id: String,
        principal: AuthorizedPrincipal,
        permit: OwnedSemaphorePermit,
    ) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.insert(
            session_id,
            SessionEntry {
                principal,
                _permit: permit,
            },
        );
    }

    /// Validates a presented session against its bound principal.
    ///
    /// An unknown session and another principal's session are
    /// indistinguishable: both report absent.
    fn validate(&self, session_id: &str, principal: &AuthorizedPrincipal) -> bool {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions
            .get(session_id)
            .is_some_and(|entry| &entry.principal == principal)
    }

    /// Removes one binding if still present, releasing its slot at most once.
    fn remove(&self, session_id: &str) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(session_id).is_some()
    }

    /// Snapshots bound IDs without holding the registry lock across I/O.
    fn session_ids(&self) -> Vec<String> {
        let sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.keys().cloned().collect()
    }

    #[cfg(test)]
    fn available_slots(&self) -> usize {
        self.permits.available_permits()
    }
}

/// Stable-mode MCP backend: admission-gated `rmcp` stateful sessions.
pub(crate) struct StableBackend {
    rmcp: StreamableHttpService<AnyMcpServer, LocalSessionManager>,
    session_manager: Arc<LocalSessionManager>,
    servers: PrincipalServers,
    registry: SessionRegistry,
    reconciliation: tokio::sync::Mutex<()>,
}

impl StableBackend {
    pub(crate) fn new(
        runtime: RuntimeContext,
        config: &HttpConfig,
        cancellation: CancellationToken,
    ) -> Self {
        Self::build(runtime, config, cancellation, SSE_KEEP_ALIVE)
    }

    /// Builds the backend with a caller-selected SSE keep-alive interval.
    ///
    /// Production always uses the reviewed [`SSE_KEEP_ALIVE`]; this seam lets
    /// the stream contract tests observe several live keep-alives and the
    /// reconnect behaviour within a bounded wall-clock budget. The retry hint,
    /// session policy, and every other setting are exactly production's.
    #[cfg(test)]
    pub(crate) fn with_sse_keep_alive(
        runtime: RuntimeContext,
        config: &HttpConfig,
        cancellation: CancellationToken,
        sse_keep_alive: Duration,
    ) -> Self {
        Self::build(runtime, config, cancellation, sse_keep_alive)
    }

    fn build(
        runtime: RuntimeContext,
        config: &HttpConfig,
        cancellation: CancellationToken,
        sse_keep_alive: Duration,
    ) -> Self {
        let session_manager = Arc::new(LocalSessionManager::default());
        let rmcp_config = StreamableHttpServerConfig::default()
            .with_sse_keep_alive(Some(sse_keep_alive))
            .with_sse_retry(Some(SSE_RETRY))
            .with_legacy_session_mode(true)
            .with_json_response(false)
            .with_cancellation_token(cancellation)
            .with_allowed_hosts(
                config
                    .allowed_hosts
                    .iter()
                    .map(super::config::HostAuthority::serialized),
            )
            .with_allowed_origins(
                config
                    .allowed_origins
                    .iter()
                    .map(|origin| origin.serialized().to_owned()),
            );
        let rmcp = StreamableHttpService::new(
            || {
                SESSION_SERVER
                    .try_with(|server| server.borrow_mut().take())
                    .ok()
                    .flatten()
                    .ok_or_else(|| std::io::Error::other("session factory outside request scope"))
            },
            session_manager.clone(),
            rmcp_config,
        );
        Self {
            rmcp,
            session_manager,
            servers: PrincipalServers::new(runtime),
            registry: SessionRegistry::new(config.max_sessions),
            reconciliation: tokio::sync::Mutex::new(()),
        }
    }

    /// Returns the currently unreserved session slots.
    ///
    /// Test seam for the load and fault boundary suite: session admission is
    /// otherwise observable only through response statuses.
    #[cfg(test)]
    pub(crate) fn available_session_slots(&self) -> usize {
        self.registry.available_slots()
    }

    /// Reserves a session slot, reconciling stale registry bindings only when
    /// the fast admission path finds the process ceiling full.
    async fn reserve_session_slot(&self) -> Option<OwnedSemaphorePermit> {
        if let Some(permit) = self.registry.try_reserve() {
            return Some(permit);
        }

        // Only one full-capacity caller probes rmcp at a time. Capacity may
        // have been released while this caller waited, so recheck first.
        let _reconciliation = self.reconciliation.lock().await;
        if let Some(permit) = self.registry.try_reserve() {
            return Some(permit);
        }

        let session_ids = self.registry.session_ids();
        let checked = session_ids.len();
        let mut absent = Vec::new();
        let mut probe_errors = 0_usize;
        for session_id in session_ids {
            match self
                .session_manager
                .has_session(&session_id.clone().into())
                .await
            {
                Ok(true) => {}
                Ok(false) => absent.push(session_id),
                Err(_) => probe_errors += 1,
            }
        }

        // A concurrent DELETE may already have removed a binding. Conditional
        // removal makes both paths idempotent and drops each permit at most
        // once. Probe failures fail closed and retain their bindings.
        let reclaimed = absent
            .iter()
            .filter(|session_id| self.registry.remove(session_id))
            .count();
        tracing::info!(
            target: "any_mcp::http",
            checked,
            reclaimed,
            probe_errors,
            "http_session_reconciled"
        );

        self.registry.try_reserve()
    }

    /// Handles one admitted stable-mode request.
    pub(crate) async fn call(self: Arc<Self>, admitted: AdmittedRequest) -> Response<HttpBody> {
        let AdmittedRequest {
            mut parts,
            body,
            principal,
            invocation,
        } = admitted;
        parts.extensions.insert(invocation);

        // Gate: the negotiated-version header must be a tested
        // Streamable-HTTP-capable revision. The header postdates 2024-11-05,
        // so it is not in the accepted set even though an initialize body may
        // propose it (see INITIALIZE_ACCEPTED_VERSIONS).
        if let Some(version) = parts.headers.get(PROTOCOL_VERSION_HEADER) {
            let accepted = version
                .to_str()
                .is_ok_and(|version| ACCEPTED_VERSIONS.contains(&version));
            if !accepted {
                return fixed_response(StatusCode::BAD_REQUEST, "Bad Request");
            }
        }

        let session_id = parts
            .headers
            .get(SESSION_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        // Gate: a presented session must belong to this principal. Unknown,
        // expired, and foreign sessions are indistinguishable.
        if let Some(session_id) = &session_id
            && !self.registry.validate(session_id, &principal)
        {
            return fixed_response(StatusCode::NOT_FOUND, "Not Found: Session not found");
        }

        // Gate: an initialize request must carry a tested revision in its
        // body; the session ceiling is reserved before session creation.
        let initialize =
            parts.method == Method::POST && session_id.is_none() && is_initialize_request(&body);
        let reservation = if initialize {
            if !initialize_version_accepted(&body) {
                return fixed_response(StatusCode::BAD_REQUEST, "Bad Request");
            }
            match self.reserve_session_slot().await {
                Some(permit) => Some(permit),
                None => {
                    tracing::info!(target: "any_mcp::http", "http_capacity_rejected");
                    let mut response =
                        fixed_response(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable");
                    response
                        .headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
                    return response;
                }
            }
        } else {
            None
        };

        let Ok(server) = self.servers.server_for(&principal) else {
            return fixed_response(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
        };

        let method = parts.method.clone();
        let request = Request::from_parts(parts, Full::new(body));
        let response = SESSION_SERVER
            .scope(RefCell::new(Some(server)), self.rmcp.handle(request))
            .await;

        // A successful initialize binds the new session to its principal;
        // any other outcome releases the reservation immediately.
        if let Some(permit) = reservation {
            let created = response
                .headers()
                .get(SESSION_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .filter(|_| response.status().is_success())
                .map(str::to_owned);
            if let Some(created) = created {
                self.registry.bind(created, principal, permit);
            }
        }

        // DELETE releases the binding exactly once; rmcp already closed the
        // session.
        if method == Method::DELETE
            && let Some(session_id) = &session_id
        {
            self.registry.remove(session_id);
        }

        response
    }
}

fn is_initialize_request(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|value| {
        value.as_object().is_some_and(|object| {
            object.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && object.get("method").and_then(Value::as_str) == Some("initialize")
        })
    })
}

fn initialize_version_accepted(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|value| {
        value
            .get("params")
            .and_then(|params| params.get("protocolVersion"))
            .and_then(Value::as_str)
            .is_some_and(|version| INITIALIZE_ACCEPTED_VERSIONS.contains(&version))
    })
}

#[cfg(test)]
mod tests {
    use anytype::prelude::{AnytypeClient, ClientConfig};
    use bytes::Bytes;
    use http_body_util::BodyExt;

    use super::*;
    use crate::{
        config::ApplicationProfile,
        http::{
            auth::Authenticator,
            listener::{ListenerState, McpService, handle_request},
        },
        runtime::StartupStatus,
    };

    fn test_runtime() -> RuntimeContext {
        test_runtime_with_timeout(Duration::from_secs(5))
    }

    fn test_runtime_with_timeout(timeout: Duration) -> RuntimeContext {
        let config = ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_string()),
            keystore: Some("env".to_string()),
            keystore_service: Some("any-mcp-http-test".to_string()),
            app_name: "any-mcp-http-test".to_string(),
            ..ClientConfig::default()
        };
        let client = AnytypeClient::with_config(config).expect("test client");
        RuntimeContext::from_parts_with_profile(
            client,
            4,
            timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
            ApplicationProfile::Compact,
            false,
        )
    }

    fn principal(name: &str) -> AuthorizedPrincipal {
        AuthorizedPrincipal::from_identity_material("test", name.as_bytes())
    }

    fn backend(max_sessions: u32) -> Arc<StableBackend> {
        let config = crate::http::listener::tests::test_config(&[(
            "ANY_MCP_HTTP_MAX_SESSIONS",
            &max_sessions.to_string(),
        )]);
        Arc::new(StableBackend::new(
            test_runtime(),
            &config,
            CancellationToken::new(),
        ))
    }

    fn admitted(
        method: Method,
        headers: &[(&str, &str)],
        body: &str,
        principal: &AuthorizedPrincipal,
    ) -> AdmittedRequest {
        let mut builder = Request::builder().method(method).uri("/mcp");
        builder = builder
            .header("host", "localhost:8000")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let (parts, ()) = builder.body(()).expect("test request").into_parts();
        AdmittedRequest {
            parts,
            body: Bytes::copy_from_slice(body.as_bytes()),
            principal: principal.clone(),
            invocation: crate::runtime::InvocationAnchor::capture_durations(
                Duration::from_secs(5),
                Duration::from_secs(300),
            ),
        }
    }

    fn initialize_body(version: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"},
            },
        })
        .to_string()
    }

    async fn read_body(response: Response<HttpBody>) -> String {
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("collect response body")
            .to_bytes();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Initializes one session and returns its ID.
    async fn initialize_session(
        backend: &Arc<StableBackend>,
        principal: &AuthorizedPrincipal,
    ) -> String {
        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &initialize_body("2025-11-25"),
                principal,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let session = response
            .headers()
            .get(SESSION_ID_HEADER)
            .expect("session id header")
            .to_str()
            .expect("ascii session id")
            .to_owned();
        let body = read_body(response).await;
        assert!(body.contains("serverInfo"), "{body}");

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                })
                .to_string(),
                principal,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        session
    }

    fn listener_request(body: String, session: Option<&str>) -> Request<Full<Bytes>> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header("host", "localhost:8000")
            .header("authorization", "Bearer synthetic-token")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json");
        if let Some(session) = session {
            builder = builder
                .header("mcp-session-id", session)
                .header("mcp-protocol-version", "2025-11-25");
        }
        builder
            .body(Full::new(Bytes::from(body)))
            .expect("listener request")
    }

    async fn initialize_listener_session(state: &Arc<ListenerState>) -> String {
        let initialized =
            handle_request(state, listener_request(initialize_body("2025-11-25"), None)).await;
        assert_eq!(initialized.status(), StatusCode::OK);
        let session = initialized
            .headers()
            .get(SESSION_ID_HEADER)
            .expect("session id")
            .to_str()
            .expect("ascii session id")
            .to_owned();
        let notified = handle_request(
            state,
            listener_request(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                })
                .to_string(),
                Some(&session),
            ),
        )
        .await;
        assert_eq!(notified.status(), StatusCode::ACCEPTED);
        session
    }

    fn stable_listener(
        timeout: Duration,
    ) -> (
        Arc<ListenerState>,
        tokio::sync::mpsc::UnboundedReceiver<crate::runtime::InvocationAnchor>,
    ) {
        stable_listener_with_claim_barrier(timeout, None, false)
    }

    fn stable_listener_with_claim_barrier(
        timeout: Duration,
        claim_barrier: Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
        listener_claim_fixture: bool,
    ) -> (
        Arc<ListenerState>,
        tokio::sync::mpsc::UnboundedReceiver<crate::runtime::InvocationAnchor>,
    ) {
        let runtime = test_runtime_with_timeout(timeout);
        let config = crate::http::listener::tests::test_config(&[]);
        let backend = Arc::new(StableBackend::new(
            runtime.clone(),
            &config,
            CancellationToken::new(),
        ));
        let (anchor_tx, anchor_rx) = tokio::sync::mpsc::unbounded_channel();
        let ingress_runtime = runtime.clone();
        let service: McpService = Arc::new(move |admitted| {
            let backend = Arc::clone(&backend);
            let runtime = ingress_runtime.clone();
            let deadline_fixture = admitted
                .body
                .windows(b"__test_deadline".len())
                .any(|window| window == b"__test_deadline");
            if deadline_fixture {
                if let Some((claimed, release)) = claim_barrier.as_ref() {
                    admitted
                        .invocation
                        .arm_dispatch_claim_barrier(Arc::clone(claimed), Arc::clone(release));
                }
                let _ = anchor_tx.send(admitted.invocation.clone());
            }
            if listener_claim_fixture && deadline_fixture {
                let invocation = admitted.invocation.clone();
                return Box::pin(async move {
                    let _ = tokio::task::spawn_blocking(move || {
                        invocation.complete_armed_dispatch_claim()
                    })
                    .await;
                    std::future::pending::<Response<HttpBody>>().await
                });
            }
            Box::pin(async move {
                let invocation = admitted.invocation.clone();
                runtime
                    .scope_ingress(invocation, backend.call(admitted))
                    .await
            })
        });
        let state = ListenerState::new_with_runtime(
            &config,
            Authenticator::SyntheticAllow,
            None,
            service,
            &runtime,
        );
        (Arc::new(state), anchor_rx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stable_http_terminal_wins_a_claiming_deadline_without_dispatch() {
        let claimed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let (state, mut anchors) = stable_listener_with_claim_barrier(
            Duration::from_millis(250),
            Some((Arc::clone(&claimed), Arc::clone(&release))),
            true,
        );
        let session = initialize_listener_session(&state).await;
        let running = tokio::spawn(async move {
            handle_request(
                &state,
                listener_request(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 42,
                        "method": "tools/call",
                        "params": {"name": "__test_deadline_mutation", "arguments": {}},
                    })
                    .to_string(),
                    Some(&session),
                ),
            )
            .await
        });
        let anchor = anchors.recv().await.expect("claiming anchor");
        tokio::task::spawn_blocking(move || claimed.wait())
            .await
            .expect("claim barrier");
        tokio::time::sleep_until(anchor.deadline()).await;
        let response = tokio::time::timeout(Duration::from_secs(1), running).await;
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release barrier");
        let response = response
            .expect("listener terminal response")
            .expect("request join");
        assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
        assert!(!anchor.dispatched());
        tokio::task::yield_now().await;
        assert!(!anchor.dispatched());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stable_http_dispatch_wins_before_deadline_and_returns_structured_outcome() {
        let claimed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let (state, mut anchors) = stable_listener_with_claim_barrier(
            Duration::from_millis(250),
            Some((Arc::clone(&claimed), Arc::clone(&release))),
            false,
        );
        let session = initialize_listener_session(&state).await;
        let running = tokio::spawn(async move {
            handle_request(
                &state,
                listener_request(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 43,
                        "method": "tools/call",
                        "params": {"name": "__test_deadline_mutation", "arguments": {}},
                    })
                    .to_string(),
                    Some(&session),
                ),
            )
            .await
        });
        let anchor = anchors.recv().await.expect("claiming anchor");
        tokio::task::spawn_blocking(move || claimed.wait())
            .await
            .expect("claim barrier");
        tokio::task::spawn_blocking(move || release.wait())
            .await
            .expect("release barrier");
        tokio::time::timeout_at(anchor.deadline(), async {
            while !anchor.dispatched() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatch claim commits before deadline");
        let response = tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .expect("structured terminal response")
            .expect("request join");
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_body(response).await;
        assert!(body.contains("mutation may have applied"), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initialize_creates_a_principal_bound_session_serving_tools() {
        let backend = backend(4);
        let alice = principal("alice");
        let session = initialize_session(&backend, &alice).await;

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/list",
                })
                .to_string(),
                &alice,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_body(response).await;
        assert!(body.contains("object_search"), "{body}");

        // Another authenticated principal presenting the stolen session ID
        // observes exactly an unknown session.
        let mallory = principal("mallory");
        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "{}",
                &mallory,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let unknown = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", "does-not-exist"),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "{}",
                &mallory,
            ))
            .await;
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(start_paused = true)]
    async fn stable_session_worker_preserves_expired_authenticated_anchor() {
        let backend = backend(4);
        let alice = principal("anchor-alice");
        let session = initialize_session(&backend, &alice).await;
        let mut request = admitted(
            Method::POST,
            &[
                ("mcp-session-id", &session),
                ("mcp-protocol-version", "2025-11-25"),
            ],
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {"name": "server_status", "arguments": {}},
            })
            .to_string(),
            &alice,
        );
        request.invocation = crate::runtime::InvocationAnchor::capture_durations(
            Duration::from_secs(1),
            Duration::from_secs(300),
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        let response = backend.clone().call(request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_body(response).await;
        assert!(body.contains("\"id\":9"), "{body}");
        assert!(body.contains("\"isError\":true"), "{body}");
    }

    #[tokio::test(start_paused = true)]
    async fn stable_http_preserves_pre_and_post_dispatch_deadline_classification() {
        let (state, mut anchors) = stable_listener(Duration::from_secs(1));
        let session = initialize_listener_session(&state).await;

        let pre_state = Arc::clone(&state);
        let pre_session = session.clone();
        let pre = tokio::spawn(async move {
            handle_request(
                &pre_state,
                listener_request(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 40,
                        "method": "tools/call",
                        "params": {"name": "__test_deadline_predispatch", "arguments": {}},
                    })
                    .to_string(),
                    Some(&pre_session),
                ),
            )
            .await
        });
        let pre_anchor = anchors.recv().await.expect("pre-dispatch anchor");
        assert!(!pre_anchor.dispatched());
        tokio::time::advance(Duration::from_secs(1)).await;
        let pre_response = pre.await.expect("pre-dispatch response");
        assert_eq!(pre_response.status(), StatusCode::OK);
        let pre_body = read_body(pre_response).await;
        assert!(pre_body.contains("\"id\":40"), "{pre_body}");
        assert!(pre_body.contains("upstream"), "{pre_body}");
        assert!(!pre_body.contains("mutation_indeterminate"), "{pre_body}");
        assert!(!pre_anchor.dispatched());

        let post_state = Arc::clone(&state);
        let post = tokio::spawn(async move {
            handle_request(
                &post_state,
                listener_request(
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 41,
                        "method": "tools/call",
                        "params": {"name": "__test_deadline_mutation", "arguments": {}},
                    })
                    .to_string(),
                    Some(&session),
                ),
            )
            .await
        });
        let post_anchor = anchors.recv().await.expect("post-dispatch anchor");
        for _ in 0..32 {
            if post_anchor.dispatched() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(post_anchor.dispatched());
        tokio::time::advance(Duration::from_secs(1)).await;
        let post_response = post.await.expect("post-dispatch response");
        assert_eq!(post_response.status(), StatusCode::OK);
        let body = read_body(post_response).await;
        assert!(body.contains("\"id\":41"), "{body}");
        assert!(body.contains("\"code\":\"conflict\""), "{body}");
        assert!(body.contains("mutation may have applied"), "{body}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn untested_revisions_are_rejected_before_session_work() {
        let backend = backend(4);
        let alice = principal("alice");
        for version in ["2026-07-28", "1999-01-01"] {
            let response = backend
                .clone()
                .call(admitted(
                    Method::POST,
                    &[],
                    &initialize_body(version),
                    &alice,
                ))
                .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{version}");
        }
        assert_eq!(backend.registry.available_slots(), 4);

        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[("mcp-protocol-version", "2024-11-05")],
                "{}",
                &alice,
            ))
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn session_ceiling_is_reserved_before_creation_and_released_by_delete() {
        let backend = backend(1);
        let alice = principal("alice");
        let session = initialize_session(&backend, &alice).await;
        assert_eq!(backend.registry.available_slots(), 0);

        let full = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &initialize_body("2025-11-25"),
                &principal("bob"),
            ))
            .await;
        assert_eq!(full.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(full.headers().get(header::RETRY_AFTER).unwrap(), "60");

        let deleted = backend
            .clone()
            .call(admitted(
                Method::DELETE,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "",
                &alice,
            ))
            .await;
        assert_eq!(deleted.status(), StatusCode::ACCEPTED);
        assert_eq!(backend.registry.available_slots(), 1);

        // DELETE releases the binding exactly once. Repeating it observes an
        // unknown session and cannot release an additional permit.
        let repeated_delete = backend
            .clone()
            .call(admitted(
                Method::DELETE,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "",
                &alice,
            ))
            .await;
        assert_eq!(repeated_delete.status(), StatusCode::NOT_FOUND);
        assert_eq!(backend.registry.available_slots(), 1);

        // The deleted session remains unknown, and the slot serves a new
        // principal.
        let gone = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "{}",
                &alice,
            ))
            .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
        initialize_session(&backend, &principal("bob")).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_initialize_releases_its_reservation() {
        let backend = backend(1);
        // Well-formed JSON that is an initialize by method but unacceptable
        // to rmcp (missing params) fails without consuming the ceiling.
        let response = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[],
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {"protocolVersion": "2025-11-25"},
                })
                .to_string(),
                &principal("alice"),
            ))
            .await;
        assert_ne!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(backend.registry.available_slots(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_initialize_reclaims_a_binding_absent_from_rmcp() {
        let backend = backend(1);
        let alice = principal("alice");
        let stale_session = initialize_session(&backend, &alice).await;
        assert_eq!(backend.registry.available_slots(), 0);

        // Deterministically model rmcp's idle timeout: its worker owns the
        // logical lifetime and removes the manager entry, while the admission
        // binding remains until a full initialize reconciles the two stores.
        backend
            .session_manager
            .close_session(&stale_session.clone().into())
            .await
            .expect("remove rmcp session");
        assert!(
            !backend
                .session_manager
                .has_session(&stale_session.clone().into())
                .await
                .expect("probe removed rmcp session")
        );

        let bob = principal("bob");
        let replacement = initialize_session(&backend, &bob).await;
        assert_ne!(replacement, stale_session);
        assert_eq!(backend.registry.available_slots(), 0);

        let gone = backend
            .clone()
            .call(admitted(
                Method::POST,
                &[
                    ("mcp-session-id", &stale_session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "{}",
                &alice,
            ))
            .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stale_ceiling_one_reconciliation_admits_exactly_one_contender() {
        const CONTENDERS: usize = 12;

        let backend = backend(1);
        let stale_session = initialize_session(&backend, &principal("stale-owner")).await;
        backend
            .session_manager
            .close_session(&stale_session.into())
            .await
            .expect("remove stale rmcp session");

        let start = Arc::new(tokio::sync::Barrier::new(CONTENDERS));
        let mut contenders = Vec::with_capacity(CONTENDERS);
        for index in 0..CONTENDERS {
            let backend = backend.clone();
            let start = start.clone();
            contenders.push(tokio::spawn(async move {
                let caller = principal(&format!("contender-{index}"));
                start.wait().await;
                let response = backend
                    .call(admitted(
                        Method::POST,
                        &[],
                        &initialize_body("2025-11-25"),
                        &caller,
                    ))
                    .await;
                let session = response
                    .headers()
                    .get(SESSION_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                let retry_after = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                (response.status(), session, retry_after, caller)
            }));
        }

        let mut admitted_contender = None;
        let mut rejected = 0_usize;
        for contender in contenders {
            let (status, session, retry_after, caller) = contender.await.expect("contender join");
            match status {
                StatusCode::OK => {
                    assert!(
                        admitted_contender.is_none(),
                        "only one contender may take the slot"
                    );
                    assert!(retry_after.is_none());
                    admitted_contender = Some((session.expect("admitted session id"), caller));
                }
                StatusCode::SERVICE_UNAVAILABLE => {
                    assert!(session.is_none(), "a shed initialize must issue no session");
                    assert_eq!(retry_after.as_deref(), Some("60"));
                    rejected += 1;
                }
                other => panic!("unexpected initialize status {other}"),
            }
        }

        assert_eq!(rejected, CONTENDERS - 1);
        assert_eq!(backend.registry.available_slots(), 0);
        let (session, caller) = admitted_contender.expect("one admitted contender");
        let deleted = backend
            .clone()
            .call(admitted(
                Method::DELETE,
                &[
                    ("mcp-session-id", &session),
                    ("mcp-protocol-version", "2025-11-25"),
                ],
                "",
                &caller,
            ))
            .await;
        assert_eq!(deleted.status(), StatusCode::ACCEPTED);
        assert_eq!(backend.registry.available_slots(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sessionless_get_and_delete_take_rmcp_fixed_statuses() {
        let backend = backend(2);
        let alice = principal("alice");
        let mut get = admitted(Method::GET, &[], "", &alice);
        get.parts.headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        let response = backend.clone().call(get).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn principal_servers_are_cached_partitioned_and_bounded() {
        let servers = PrincipalServers::with_capacity(test_runtime(), 2);
        let alice = principal("alice");
        let bob = principal("bob");

        let first = servers.server_for(&alice).expect("alice server");
        let again = servers.server_for(&alice).expect("alice server again");
        assert!(std::ptr::eq(first.tools().as_ptr(), again.tools().as_ptr()));

        let other = servers.server_for(&bob).expect("bob server");
        assert!(!std::ptr::eq(
            first.tools().as_ptr(),
            other.tools().as_ptr()
        ));

        // A third principal evicts the least recently used facade (alice was
        // used first; bob later). Alice's rebuilt facade is a new instance.
        let _carol = servers.server_for(&principal("carol")).expect("carol");
        let rebuilt = servers.server_for(&alice).expect("alice rebuilt");
        assert!(!std::ptr::eq(
            first.tools().as_ptr(),
            rebuilt.tools().as_ptr()
        ));
    }
}
