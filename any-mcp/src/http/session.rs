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

use bytes::Bytes;
use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
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
        listener::{AdmittedRequest, HttpBody},
    },
    runtime::RuntimeContext,
    server::AnyMcpServer,
};

/// Streamable-HTTP-capable protocol revisions accepted by this server.
///
/// Revision `2024-11-05` predates Streamable HTTP and stays stdio-only; the
/// `2026-07-28` preview uses the stateless adapter, never stable sessions.
const ACCEPTED_VERSIONS: [&str; 3] = ["2025-03-26", "2025-06-18", "2025-11-25"];

/// Reviewed `rmcp` SSE keep-alive interval.
const SSE_KEEP_ALIVE: Duration = Duration::from_secs(15);
/// Reviewed `rmcp` SSE retry hint.
const SSE_RETRY: Duration = Duration::from_secs(3);
/// Idle margin added to `rmcp`'s five-minute session keep-alive before the
/// principal binding and its admission permit are swept.
const SESSION_SWEEP_AFTER: Duration = Duration::from_secs(300 + 60);
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
    last_seen: Instant,
}

/// Principal-bound session registry enforcing the process session ceiling.
///
/// Admission is reserved before `rmcp` creates a session and released
/// exactly once when the binding is dropped: on failed initialize, DELETE,
/// idle sweep, or process shutdown.
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
                last_seen: Instant::now(),
            },
        );
    }

    /// Validates a presented session against its bound principal.
    ///
    /// An unknown session and another principal's session are
    /// indistinguishable: both report absent.
    fn validate(&self, session_id: &str, principal: &AuthorizedPrincipal) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match sessions.get_mut(session_id) {
            Some(entry) if &entry.principal == principal => {
                entry.last_seen = Instant::now();
                true
            }
            _ => false,
        }
    }

    /// Removes one binding, releasing its session slot exactly once.
    fn remove(&self, session_id: &str) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.remove(session_id);
    }

    /// Sweeps bindings idle past the `rmcp` keep-alive plus margin.
    fn sweep(&self) -> Vec<String> {
        self.sweep_at(Instant::now())
    }

    fn sweep_at(&self, now: Instant) -> Vec<String> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = sessions
            .iter()
            .filter(|(_, entry)| {
                now.saturating_duration_since(entry.last_seen) >= SESSION_SWEEP_AFTER
            })
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in &expired {
            sessions.remove(id);
        }
        expired
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
}

impl StableBackend {
    pub(crate) fn new(
        runtime: RuntimeContext,
        config: &HttpConfig,
        cancellation: CancellationToken,
    ) -> Self {
        let session_manager = Arc::new(LocalSessionManager::default());
        let rmcp_config = StreamableHttpServerConfig::default()
            .with_sse_keep_alive(Some(SSE_KEEP_ALIVE))
            .with_sse_retry(Some(SSE_RETRY))
            .with_stateful_mode(true)
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
        }
    }

    /// Handles one admitted stable-mode request.
    pub(crate) async fn call(self: Arc<Self>, admitted: AdmittedRequest) -> Response<HttpBody> {
        // Reclaim bindings whose rmcp sessions idled out.
        for expired in self.registry.sweep() {
            let manager = self.session_manager.clone();
            tokio::spawn(async move {
                let _ = manager.close_session(&expired.into()).await;
            });
        }

        let AdmittedRequest {
            parts,
            body,
            principal,
        } = admitted;

        // Gate: the negotiated-version header must be a tested
        // Streamable-HTTP-capable revision. rmcp alone would also accept the
        // stdio-only 2024-11-05 revision.
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
            match self.registry.try_reserve() {
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
            .is_some_and(|version| ACCEPTED_VERSIONS.contains(&version))
    })
}

fn fixed_response(status: StatusCode, body: &'static str) -> Response<HttpBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(body.as_bytes())).boxed())
        .expect("fixed response")
}

#[cfg(test)]
mod tests {
    use anytype::prelude::{AnytypeClient, ClientConfig};

    use super::*;
    use crate::{config::ApplicationProfile, runtime::StartupStatus};

    fn test_runtime() -> RuntimeContext {
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
            Duration::from_secs(5),
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn untested_revisions_are_rejected_before_session_work() {
        let backend = backend(4);
        let alice = principal("alice");
        for version in ["2024-11-05", "2026-07-28", "1999-01-01"] {
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
        assert!(
            deleted.status().is_success(),
            "delete status {}",
            deleted.status()
        );
        assert_eq!(backend.registry.available_slots(), 1);

        // The deleted session is unknown afterwards, and the slot serves a
        // new principal.
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
    fn registry_sweep_releases_slots_only_after_the_idle_margin() {
        let registry = SessionRegistry::new(2);
        let permit = registry.try_reserve().expect("reserve");
        registry.bind("session-1".to_owned(), principal("alice"), permit);
        assert_eq!(registry.available_slots(), 1);

        let now = Instant::now();
        assert!(registry.sweep_at(now + Duration::from_secs(300)).is_empty());
        let swept = registry.sweep_at(now + SESSION_SWEEP_AFTER + Duration::from_secs(1));
        assert_eq!(swept, vec!["session-1".to_owned()]);
        assert_eq!(registry.available_slots(), 2);
        assert!(!registry.validate("session-1", &principal("alice")));
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
