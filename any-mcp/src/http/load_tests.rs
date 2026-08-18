// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Load and fault boundary tests for the Streamable HTTP transport.
//!
//! Each test drives one enforced transport boundary past its limit and proves
//! the fixed shed-and-recover behavior: the session ceiling under concurrent
//! initialize contention, the process-global request-rate budget under burst,
//! the 2 MiB body ceiling at and one byte over the edge including a streamed
//! body, the admitted-request concurrency cap, SSE client disconnect, a slow
//! reader of a flowing event stream, drain-then-cancel graceful shutdown under
//! in-flight load, and an abrupt client disconnect during an in-flight
//! mutation.
//!
//! Determinism comes from explicit gates rather than sleeps: services block on
//! a cancellation token, in-flight counts are awaited through a notification,
//! and the upstream fixture publishes received/served counters. No test
//! contacts a real Anytype server; the mutation test drives one scripted
//! loopback upstream and asserts its exact request tape.

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
use bytes::Bytes;
use http::{Method, Request, Response, StatusCode, header};
use http_body_util::{BodyExt, Full};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

use crate::{
    config::ApplicationProfile,
    http::{
        auth::{Authenticator, AuthorizedPrincipal},
        listener::{
            AdmittedRequest, HttpServeError, ListenerState, MAX_BODY_BYTES, McpService,
            fixed_response, handle_request, run_listener, tests::test_config,
        },
        session::StableBackend,
    },
    runtime::{RuntimeContext, StartupStatus},
};

/// Bounded deadline for every socket read in this module.
const SOCKET_DEADLINE: Duration = Duration::from_secs(10);
/// Bounded deadline for every awaited counter or in-flight condition.
const CONDITION_DEADLINE: Duration = Duration::from_secs(10);
/// Rate budget large enough that only the boundary under test can reject.
pub(super) const UNCONSTRAINED_RATE: &str = "600";
/// Negotiated revision used by every session in this module.
pub(super) const REVISION: &str = "2025-11-25";

// ---------------------------------------------------------------------------
// Shared fixtures
// ---------------------------------------------------------------------------

/// Builds an offline runtime whose upstream is unreachable.
///
/// Boundary tests reject before any handler runs, so no upstream traffic is
/// expected; an unroutable port makes an accidental call fail loudly.
pub(super) fn offline_runtime() -> RuntimeContext {
    upstream_runtime("http://127.0.0.1:1", ApplicationProfile::Compact, false)
}

/// Builds a runtime bound to one scripted loopback upstream.
pub(super) fn upstream_runtime(
    base_url: &str,
    profile: ApplicationProfile,
    grpc_available: bool,
) -> RuntimeContext {
    let client = AnytypeClient::with_config(ClientConfig {
        base_url: Some(base_url.to_owned()),
        keystore: Some("env".to_owned()),
        keystore_service: Some("any-mcp-http-load-test".to_owned()),
        app_name: "any-mcp-http-load-test".to_owned(),
        disable_cache: true,
        ..ClientConfig::default()
    })
    .expect("load test client");
    client.set_api_key(HttpCredentials::new("load-test-token"));
    RuntimeContext::from_parts_with_profile(
        client,
        4,
        Duration::from_secs(5),
        StartupStatus {
            http_available: true,
            grpc_available,
        },
        profile,
        false,
    )
}

fn principal(name: &str) -> AuthorizedPrincipal {
    AuthorizedPrincipal::from_identity_material("synthetic", name.as_bytes())
}

/// Builds one admitted request for direct session-backend drives.
fn admitted(
    method: Method,
    headers: &[(&str, &str)],
    body: &str,
    principal: &AuthorizedPrincipal,
) -> AdmittedRequest {
    let mut builder = Request::builder()
        .method(method)
        .uri("/mcp")
        .header("host", "localhost:8000")
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let (parts, ()) = builder.body(()).expect("admitted request").into_parts();
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

/// Builds one in-process listener request.
fn listener_request(bearer: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .method(Method::POST)
        .uri("/mcp")
        .header("host", "localhost:8000")
        .header("authorization", format!("Bearer {bearer}"))
        .body(Full::new(Bytes::new()))
        .expect("listener request")
}

fn initialize_body(id: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": REVISION,
            "capabilities": {},
            "clientInfo": {"name": "load-test", "version": "1.0.0"},
        },
    })
    .to_string()
}

/// A service that records every admitted body length and returns 200.
fn recording_service() -> (McpService, Arc<Mutex<Vec<usize>>>) {
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorded = bodies.clone();
    let service: McpService = Arc::new(move |admitted: AdmittedRequest| {
        let bodies = bodies.clone();
        Box::pin(async move {
            bodies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(admitted.body.len());
            fixed_response(StatusCode::OK, "ok")
        })
    });
    (service, recorded)
}

/// A service every request enters and none leaves until it is released.
struct Gate {
    in_flight: AtomicUsize,
    entered: Notify,
    release: CancellationToken,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            in_flight: AtomicUsize::new(0),
            entered: Notify::new(),
            release: CancellationToken::new(),
        })
    }

    fn service(self: &Arc<Self>) -> McpService {
        let gate = self.clone();
        Arc::new(move |_admitted| {
            let gate = gate.clone();
            Box::pin(async move {
                gate.in_flight.fetch_add(1, Ordering::SeqCst);
                gate.entered.notify_waiters();
                gate.release.cancelled().await;
                fixed_response(StatusCode::OK, "ok")
            })
        })
    }

    /// Awaits the exact number of requests inside the service.
    async fn wait_for_in_flight(&self, count: usize) {
        let deadline = Instant::now() + CONDITION_DEADLINE;
        loop {
            let notified = self.entered.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.in_flight.load(Ordering::SeqCst) >= count {
                return;
            }
            tokio::time::timeout_at(deadline, notified)
                .await
                .expect("requests reached the expected in-flight count");
        }
    }

    fn release(&self) {
        self.release.cancel();
    }
}

/// One listener bound to an OS-assigned loopback port.
pub(super) struct LoadServer {
    address: SocketAddr,
    shutdown: CancellationToken,
    task: JoinHandle<Result<(), HttpServeError>>,
}

impl LoadServer {
    pub(super) async fn start(state: Arc<ListenerState>, drain: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind load test listener");
        let address = listener.local_addr().expect("load test listener address");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(run_listener(listener, state, shutdown.clone(), drain));
        Self {
            address,
            shutdown,
            task,
        }
    }

    pub(super) fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.address.port())
    }

    pub(super) fn host(&self) -> String {
        format!("127.0.0.1:{}", self.address.port())
    }

    pub(super) async fn connect(&self) -> TcpStream {
        TcpStream::connect(self.address)
            .await
            .expect("connect to load test listener")
    }

    /// Shuts down and returns the drain duration actually taken.
    pub(super) async fn stop(self) -> Duration {
        let started = Instant::now();
        self.shutdown.cancel();
        let result = tokio::time::timeout(Duration::from_secs(30), self.task)
            .await
            .expect("listener shutdown deadline")
            .expect("listener join");
        assert_eq!(result, Ok(()));
        started.elapsed()
    }
}

pub(super) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .expect("load test client")
}

/// Extracts the last `data:` payload from one SSE body.
pub(super) fn last_sse_data(body: &str) -> Value {
    let data = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .next_back()
        .expect("SSE data event");
    serde_json::from_str(data).expect("SSE data JSON")
}

/// Reads one HTTP response head, tolerating an aborted connection.
async fn read_response_head(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    while let Ok(Ok(read)) = tokio::time::timeout(SOCKET_DEADLINE, stream.read(&mut chunk)).await {
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 64 * 1024 {
            break;
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Awaits a counter reaching a value under the shared condition deadline.
async fn wait_for_count(counter: &AtomicUsize, expected: usize, what: &str) {
    let deadline = Instant::now() + CONDITION_DEADLINE;
    while counter.load(Ordering::SeqCst) < expected {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

// ---------------------------------------------------------------------------
// Session ceiling
// ---------------------------------------------------------------------------

/// Concurrent initializes past the ceiling shed exactly, then recover.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_ceiling_is_exact_under_concurrent_initialize_contention() {
    const CEILING: u32 = 4;
    const CONTENDERS: usize = 24;

    let config = test_config(&[
        ("ANY_MCP_HTTP_MAX_SESSIONS", "4"),
        ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE),
    ]);
    let backend = Arc::new(StableBackend::new(
        offline_runtime(),
        &config,
        CancellationToken::new(),
    ));

    let mut contenders = Vec::with_capacity(CONTENDERS);
    for index in 0..CONTENDERS {
        let backend = backend.clone();
        let caller = principal(&format!("contender-{index}"));
        contenders.push(tokio::spawn(async move {
            let response = backend
                .call(admitted(Method::POST, &[], &initialize_body(1), &caller))
                .await;
            let session = response
                .headers()
                .get("mcp-session-id")
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

    let mut admitted_sessions = Vec::new();
    let mut rejected = 0_usize;
    for contender in contenders {
        let (status, session, retry_after, caller) = contender.await.expect("contender join");
        match status {
            StatusCode::OK => {
                admitted_sessions.push((session.expect("admitted session id"), caller));
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                assert_eq!(retry_after.as_deref(), Some("60"));
                assert!(session.is_none(), "a shed initialize must issue no session");
                rejected += 1;
            }
            other => panic!("unexpected initialize status {other}"),
        }
    }

    assert_eq!(admitted_sessions.len(), CEILING as usize);
    assert_eq!(rejected, CONTENDERS - CEILING as usize);
    assert_eq!(backend.available_session_slots(), 0);
    let mut identifiers = admitted_sessions
        .iter()
        .map(|(session, _)| session.clone())
        .collect::<Vec<_>>();
    identifiers.sort_unstable();
    identifiers.dedup();
    assert_eq!(identifiers.len(), CEILING as usize, "session ids collided");

    // Every reservation is released exactly once, concurrently.
    let mut releases = Vec::with_capacity(admitted_sessions.len());
    for (session, caller) in admitted_sessions {
        let backend = backend.clone();
        releases.push(tokio::spawn(async move {
            backend
                .call(admitted(
                    Method::DELETE,
                    &[
                        ("mcp-session-id", &session),
                        ("mcp-protocol-version", REVISION),
                    ],
                    "",
                    &caller,
                ))
                .await
                .status()
        }));
    }
    for release in releases {
        let status = release.await.expect("release join");
        assert!(status.is_success(), "delete status {status}");
    }
    assert_eq!(backend.available_session_slots(), CEILING as usize);

    // The recovered ceiling admits a fresh contender.
    let response = backend
        .clone()
        .call(admitted(
            Method::POST,
            &[],
            &initialize_body(1),
            &principal("late-arrival"),
        ))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(backend.available_session_slots(), CEILING as usize - 1);
}

// ---------------------------------------------------------------------------
// Request rate
// ---------------------------------------------------------------------------

/// A burst consumes exactly the window budget and the rest is shed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_window_admits_exactly_its_budget_under_burst() {
    const BUDGET: usize = 8;
    const BURST: usize = 64;

    let (service, bodies) = recording_service();
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "8")]),
        Authenticator::SyntheticAllow,
        None,
        service,
    ));

    let mut burst = Vec::with_capacity(BURST);
    for index in 0..BURST {
        let state = state.clone();
        burst.push(tokio::spawn(async move {
            let response =
                handle_request(state.as_ref(), listener_request(&format!("caller-{index}"))).await;
            let retry_after = response
                .headers()
                .get(header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            (response.status(), retry_after)
        }));
    }

    let mut admitted_requests = 0_usize;
    let mut shed = 0_usize;
    for request in burst {
        let (status, retry_after) = request.await.expect("burst join");
        match status {
            StatusCode::OK => admitted_requests += 1,
            StatusCode::TOO_MANY_REQUESTS => {
                assert_eq!(retry_after.as_deref(), Some("60"));
                shed += 1;
            }
            other => panic!("unexpected burst status {other}"),
        }
    }

    assert_eq!(admitted_requests, BUDGET);
    assert_eq!(shed, BURST - BUDGET);
    assert_eq!(
        bodies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        BUDGET,
        "shed requests must never reach the service"
    );
}

/// The window budget is one process-global counter, not a per-principal one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rate_budget_is_process_global_across_principals() {
    let (service, bodies) = recording_service();
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", "1")]),
        Authenticator::SyntheticAllow,
        None,
        service,
    ));

    let first = handle_request(state.as_ref(), listener_request("first-principal")).await;
    assert_eq!(first.status(), StatusCode::OK);

    // A different authenticated principal shares the same exhausted window:
    // this gate is coarse process self-protection, not tenant fairness.
    let second = handle_request(state.as_ref(), listener_request("second-principal")).await;
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        bodies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------
// Body ceiling
// ---------------------------------------------------------------------------

/// The 2 MiB ceiling holds over a real socket for framed and streamed bodies.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn body_ceiling_holds_at_the_edge_and_while_streaming() {
    let (service, bodies) = recording_service();
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE)]),
        Authenticator::SyntheticAllow,
        None,
        service,
    ));
    let server = LoadServer::start(state, Duration::from_secs(5)).await;
    let host = server.host();

    // Exactly at the ceiling: admitted whole.
    let mut stream = server.connect().await;
    let head = format!(
        "POST /mcp HTTP/1.1\r\nhost: {host}\r\nauthorization: Bearer edge\r\ncontent-length: {MAX_BODY_BYTES}\r\n\r\n"
    );
    stream
        .write_all(head.as_bytes())
        .await
        .expect("write exact-size head");
    stream
        .write_all(&vec![b'a'; MAX_BODY_BYTES])
        .await
        .expect("write exact-size body");
    let response = read_response_head(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    drop(stream);

    // One byte over the ceiling: rejected before the service.
    let over = MAX_BODY_BYTES + 1;
    let mut stream = server.connect().await;
    let head = format!(
        "POST /mcp HTTP/1.1\r\nhost: {host}\r\nauthorization: Bearer over\r\ncontent-length: {over}\r\n\r\n"
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&vec![b'a'; over]).await;
    let response = read_response_head(&mut stream).await;
    assert!(
        response.starts_with("HTTP/1.1 413 Payload Too Large"),
        "{response}"
    );
    drop(stream);

    // A chunked body that never declares its length is bounded mid-stream.
    let stream = server.connect().await;
    let (mut reader, mut writer) = stream.into_split();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nhost: {host}\r\nauthorization: Bearer chunked\r\ntransfer-encoding: chunked\r\n\r\n"
    );
    let writes = tokio::spawn(async move {
        if writer.write_all(head.as_bytes()).await.is_err() {
            return;
        }
        let chunk = vec![b'a'; 64 * 1024];
        let framed = format!("{:x}\r\n", chunk.len());
        // Thirty-three 64 KiB chunks exceed the ceiling before the terminator.
        for _ in 0..33_u32 {
            if writer.write_all(framed.as_bytes()).await.is_err()
                || writer.write_all(&chunk).await.is_err()
                || writer.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        let _ = writer.write_all(b"0\r\n\r\n").await;
    });
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    while let Ok(Ok(read)) = tokio::time::timeout(SOCKET_DEADLINE, reader.read(&mut chunk)).await {
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let response = String::from_utf8_lossy(&buffer).into_owned();
    assert!(
        response.starts_with("HTTP/1.1 413 Payload Too Large"),
        "{response}"
    );
    writes.abort();
    let _ = writes.await;

    let recorded = bodies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        recorded,
        vec![MAX_BODY_BYTES],
        "only the exact-size body reaches the service"
    );
    server.stop().await;
}

// ---------------------------------------------------------------------------
// Request concurrency
// ---------------------------------------------------------------------------

/// A saturated concurrency cap sheds excess load and recovers on release.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn request_concurrency_cap_sheds_excess_load_and_recovers() {
    const PERMITS: usize = 4;
    const EXCESS: usize = 16;

    let gate = Gate::new();
    let state = Arc::new(
        ListenerState::new(
            &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE)]),
            Authenticator::SyntheticAllow,
            None,
            gate.service(),
        )
        .with_admission_bounds(PERMITS, Duration::from_millis(50)),
    );

    let mut occupied = Vec::with_capacity(PERMITS);
    for index in 0..PERMITS {
        let state = state.clone();
        occupied.push(tokio::spawn(async move {
            handle_request(state.as_ref(), listener_request(&format!("holder-{index}")))
                .await
                .status()
        }));
    }
    gate.wait_for_in_flight(PERMITS).await;

    let mut shed = Vec::with_capacity(EXCESS);
    for index in 0..EXCESS {
        let state = state.clone();
        shed.push(tokio::spawn(async move {
            handle_request(state.as_ref(), listener_request(&format!("excess-{index}")))
                .await
                .status()
        }));
    }
    for request in shed {
        assert_eq!(
            request.await.expect("shed join"),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    assert_eq!(
        gate.in_flight.load(Ordering::SeqCst),
        PERMITS,
        "a shed request must never enter the service"
    );

    gate.release();
    for request in occupied {
        assert_eq!(request.await.expect("holder join"), StatusCode::OK);
    }

    // Released permits are reusable immediately.
    let recovered = handle_request(state.as_ref(), listener_request("recovered")).await;
    assert_eq!(recovered.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// SSE faults
// ---------------------------------------------------------------------------

/// An idle standalone SSE reader that stops reading and then disconnects
/// neither blocks the listener nor leaks its session slot.
///
/// This case covers the disconnect boundary only: the standalone stream is
/// opened but carries no server-initiated events during the test window.
/// Backpressure from a reader that is slow while events actually flow is
/// covered by `slow_sse_consumer_applies_backpressure_without_stalling`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_idle_reader_and_disconnect_neither_stall_nor_leak() {
    const BEARER: &str = "sse-principal";

    let config = test_config(&[
        ("ANY_MCP_HTTP_MAX_SESSIONS", "2"),
        ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE),
    ]);
    let backend = Arc::new(StableBackend::new(
        offline_runtime(),
        &config,
        CancellationToken::new(),
    ));
    let backend_service = backend.clone();
    let service: McpService =
        Arc::new(move |request| Box::pin(backend_service.clone().call(request)));
    let state = Arc::new(ListenerState::new(
        &config,
        Authenticator::SyntheticAllow,
        None,
        service,
    ));
    let server = LoadServer::start(state, Duration::from_secs(5)).await;
    let http = client();
    let base = server.base();
    let session = initialize_over_http(&http, &base, BEARER).await;
    assert_eq!(backend.available_session_slots(), 1);

    // A standalone SSE stream whose client reads the head and then stops.
    let mut stalled = server.connect().await;
    let request = format!(
        "GET /mcp HTTP/1.1\r\nhost: {}\r\nauthorization: Bearer {BEARER}\r\naccept: text/event-stream\r\nmcp-session-id: {session}\r\nmcp-protocol-version: {REVISION}\r\n\r\n",
        server.host()
    );
    stalled
        .write_all(request.as_bytes())
        .await
        .expect("write SSE request");
    let head = read_response_head(&mut stalled).await;
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("text/event-stream"),
        "{head}"
    );

    // The stalled reader must not delay unrelated work on the same session.
    let listed = tokio::time::timeout(
        Duration::from_secs(5),
        call_over_http(
            &http,
            &base,
            BEARER,
            &session,
            2,
            json!({"method": "tools/list"}),
        ),
    )
    .await
    .expect("a stalled SSE consumer must not stall the listener");
    assert!(listed["result"]["tools"].is_array(), "{listed}");

    // An abrupt disconnect leaves the session and the listener healthy.
    drop(stalled);
    let listed = tokio::time::timeout(
        Duration::from_secs(5),
        call_over_http(
            &http,
            &base,
            BEARER,
            &session,
            3,
            json!({"method": "tools/list"}),
        ),
    )
    .await
    .expect("the session survives an SSE disconnect");
    assert!(listed["result"]["tools"].is_array(), "{listed}");

    // The session slot is released exactly once, by the explicit DELETE.
    let response = http
        .delete(format!("{base}/mcp"))
        .bearer_auth(BEARER)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", REVISION)
        .send()
        .await
        .expect("session delete");
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(backend.available_session_slots(), 2);

    let drained = server.stop().await;
    assert!(
        drained < Duration::from_secs(5),
        "a disconnected SSE stream must not hold the drain: {drained:?}"
    );
}

/// Event-stream body larger than any plausible pair of socket buffers, so a
/// client that stops reading forces the server-side write to block.
const SLOW_CONSUMER_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Request header that selects the large event-stream body from the fixture.
const STREAM_HEADER: &str = "x-load-test-stream";

/// One SSE frame emitted by the incremental generator: a fixed-size event.
fn slow_consumer_frame() -> Bytes {
    Bytes::from(format!("event: message\ndata: {}\n\n", "x".repeat(4096)))
}

/// Number of generated frames; the total exceeds any plausible pair of
/// socket buffers so a stalled reader necessarily blocks the producer.
fn slow_consumer_frames() -> usize {
    SLOW_CONSUMER_BODY_BYTES.div_ceil(slow_consumer_frame().len())
}

/// A service that answers the stream marker with an application-generated
/// incremental event stream and every other request with a short fixed body.
///
/// Frames are produced one at a time, on demand, as the HTTP body is polled;
/// `produced` counts frames handed to the transport so a test can prove that
/// a stalled reader stops generation (backpressure reaches the application)
/// and that generation resumes and completes once the reader catches up. The
/// exact total length is declared up front so the reader can account for
/// every byte without chunked framing.
fn incremental_streaming_service(frames: usize, produced: Arc<AtomicUsize>) -> McpService {
    Arc::new(move |admitted: AdmittedRequest| {
        let produced = produced.clone();
        Box::pin(async move {
            if !admitted.parts.headers.contains_key(STREAM_HEADER) {
                return fixed_response(StatusCode::OK, "ok");
            }
            let frame = slow_consumer_frame();
            let total = frames * frame.len();
            let generator = futures::stream::unfold(0_usize, move |index| {
                let frame = frame.clone();
                let produced = produced.clone();
                async move {
                    if index >= frames {
                        return None;
                    }
                    produced.fetch_add(1, Ordering::SeqCst);
                    Some((
                        Ok::<_, std::convert::Infallible>(http_body::Frame::data(frame)),
                        index + 1,
                    ))
                }
            });
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header(header::CACHE_CONTROL, "no-store")
                .header(header::CONTENT_LENGTH, total)
                .body(http_body_util::StreamBody::new(generator).boxed())
                .expect("streamed load test response")
        })
    })
}

/// Reads one response head and reports how many body bytes arrived with it.
async fn read_head_and_body_prefix(stream: &mut TcpStream) -> (String, usize) {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    while let Ok(Ok(read)) = tokio::time::timeout(SOCKET_DEADLINE, stream.read(&mut chunk)).await {
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|start| start + 4)
        {
            let head = String::from_utf8_lossy(&buffer[..end]).into_owned();
            return (head, buffer.len() - end);
        }
        assert!(buffer.len() <= 64 * 1024, "response head exceeded 64 KiB");
    }
    panic!("the response head never completed");
}

/// Reads an exact remaining body length and returns the bytes actually read.
async fn read_body_remainder(stream: &mut TcpStream, mut remaining: usize) -> usize {
    let mut read_total = 0_usize;
    let mut chunk = vec![0_u8; 64 * 1024];
    while remaining > 0 {
        let Ok(Ok(read)) = tokio::time::timeout(SOCKET_DEADLINE, stream.read(&mut chunk)).await
        else {
            break;
        };
        if read == 0 {
            break;
        }
        read_total += read;
        remaining = remaining.saturating_sub(read);
    }
    read_total
}

/// Extracts the declared `content-length` from one response head.
fn declared_content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .expect("an exact content-length on the streamed response")
}

/// A reader that stops consuming a flowing event stream applies backpressure to
/// its own connection only: generation stalls at the application seam, the
/// listener keeps serving other clients, and the stalled response resumes and
/// completes exactly once the reader catches up.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slow_sse_consumer_applies_backpressure_without_stalling() {
    const BEARER: &str = "slow-consumer";

    let frames = slow_consumer_frames();
    let expected = frames * slow_consumer_frame().len();
    let produced = Arc::new(AtomicUsize::new(0));
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE)]),
        Authenticator::SyntheticAllow,
        None,
        incremental_streaming_service(frames, produced.clone()),
    ));
    let server = LoadServer::start(state, Duration::from_secs(5)).await;

    // A client that requests the flowing stream, reads only the head, and then
    // stops reading entirely.
    let mut slow = server.connect().await;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nhost: {}\r\nauthorization: Bearer {BEARER}\r\naccept: application/json, text/event-stream\r\ncontent-type: application/json\r\n{STREAM_HEADER}: 1\r\ncontent-length: 0\r\n\r\n",
        server.host()
    );
    slow.write_all(request.as_bytes())
        .await
        .expect("write streamed request");
    let (head, prefix) = read_head_and_body_prefix(&mut slow).await;
    assert!(head.starts_with("HTTP/1.1 200 OK"), "{head}");
    assert!(
        head.to_ascii_lowercase().contains("text/event-stream"),
        "{head}"
    );
    assert_eq!(declared_content_length(&head), expected, "{head}");
    // The socket buffers cannot hold the whole body, so the server write is
    // necessarily still pending on this connection.
    assert!(
        prefix < expected,
        "the stalled reader received the entire body: {prefix} of {expected}"
    );

    // Backpressure reaches the application: generation stops well short of
    // the total and stays stopped while the reader is stalled.
    let stalled_at = wait_for_stable_count(&produced).await;
    assert!(
        stalled_at > 0 && stalled_at < frames,
        "generation must stall between the first and last frame: {stalled_at} of {frames}"
    );

    // A different client is served promptly while the slow reader stalls.
    let http = client();
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        http.post(format!("{}/mcp", server.base()))
            .bearer_auth("other-principal")
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
            .body("{}")
            .send(),
    )
    .await
    .expect("a slow SSE consumer must not stall the listener")
    .expect("unrelated request");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.expect("unrelated body"), "ok");
    assert_eq!(
        produced.load(Ordering::SeqCst),
        stalled_at,
        "serving another client must not advance the stalled generator"
    );

    // Once the reader resumes, generation resumes and the stalled write
    // completes exactly, with no truncated or duplicated bytes or frames.
    let remainder = read_body_remainder(&mut slow, expected - prefix).await;
    assert_eq!(prefix + remainder, expected);
    assert_eq!(produced.load(Ordering::SeqCst), frames);

    let drained = server.stop().await;
    assert!(
        drained < Duration::from_secs(5),
        "a completed slow-consumer stream must not hold the drain: {drained:?}"
    );
}

/// Awaits the generator count becoming stable across two consecutive samples
/// and returns it, bounded by the shared condition deadline.
async fn wait_for_stable_count(counter: &AtomicUsize) -> usize {
    let deadline = Instant::now() + CONDITION_DEADLINE;
    let mut previous = counter.load(Ordering::SeqCst);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let current = counter.load(Ordering::SeqCst);
        if current == previous && current > 0 {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "the generator never stalled: {current} frames"
        );
        previous = current;
    }
}

// ---------------------------------------------------------------------------
// Graceful shutdown
// ---------------------------------------------------------------------------

/// Shutdown drains work that finishes inside the deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_drains_in_flight_work_within_the_deadline() {
    let gate = Gate::new();
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE)]),
        Authenticator::SyntheticAllow,
        None,
        gate.service(),
    ));
    let server = LoadServer::start(state, Duration::from_secs(20)).await;

    let mut stream = server.connect().await;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nhost: {}\r\nauthorization: Bearer drain\r\ncontent-length: 2\r\n\r\n{{}}",
        server.host()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write drained request");
    gate.wait_for_in_flight(1).await;

    server.shutdown.cancel();
    gate.release();
    let response = read_response_head(&mut stream).await;
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let drained = tokio::time::timeout(Duration::from_secs(20), server.task)
        .await
        .expect("drain deadline")
        .expect("listener join");
    assert_eq!(drained, Ok(()));
}

/// Shutdown cancels work that outlives the drain deadline, without hanging.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_work_that_outlives_the_drain_deadline() {
    let drain = Duration::from_millis(300);
    let gate = Gate::new();
    let state = Arc::new(ListenerState::new(
        &test_config(&[("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE)]),
        Authenticator::SyntheticAllow,
        None,
        gate.service(),
    ));
    let server = LoadServer::start(state, drain).await;

    let mut stream = server.connect().await;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nhost: {}\r\nauthorization: Bearer stuck\r\ncontent-length: 2\r\n\r\n{{}}",
        server.host()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write stuck request");
    gate.wait_for_in_flight(1).await;

    // The request is never released: the drain deadline must cancel it.
    let elapsed = server.stop().await;
    assert!(
        elapsed >= drain,
        "shutdown must wait for the full drain deadline: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "shutdown must cancel rather than hang: {elapsed:?}"
    );
    let response = read_response_head(&mut stream).await;
    assert!(
        !response.starts_with("HTTP/1.1 200 OK"),
        "cancelled work must not answer 200: {response}"
    );
    gate.release();
}

// ---------------------------------------------------------------------------
// Abrupt disconnect during a mutation
// ---------------------------------------------------------------------------

pub(super) const SPACE_ID: &str =
    "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
const TYPE_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y";
pub(super) const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";

fn type_document() -> Value {
    json!({
        "type": {
            "archived": false,
            "id": TYPE_ID,
            "key": "page",
            "layout": "basic",
            "name": "Page",
            "plural_name": "Pages",
            "properties": [
                {"id":"prop-description", "key":"description", "name":"Description", "format":"text"},
                {"id":"prop-done", "key":"done", "name":"Done", "format":"checkbox"}
            ]
        }
    })
}

pub(super) fn object_document() -> Value {
    json!({
        "object": {
            "archived": false,
            "icon": {"format":"emoji", "emoji":"📄"},
            "id": OBJECT_ID,
            "layout": "basic",
            "markdown": "# Plan",
            "name": "Roadmap",
            "object": "object",
            "properties": [
                {"id":"prop-description", "key":"description", "name":"Description", "format":"text", "text":"Q3"},
                {"id":"prop-done", "key":"done", "name":"Done", "format":"checkbox", "checkbox":true}
            ],
            "space_id": SPACE_ID,
            "type": {
                "archived": false,
                "id": TYPE_ID,
                "key":"page",
                "layout":"basic",
                "name":"Page",
                "plural_name":"Pages",
                "properties":[]
            }
        }
    })
}

fn create_arguments(key: &str) -> Value {
    json!({
        "space": SPACE_ID,
        "type": TYPE_ID,
        "name": "Roadmap",
        "body_markdown": "# Plan",
        "icon": {"format":"emoji", "emoji":"📄"},
        "properties": [
            {"format":"checkbox", "key":"done", "checkbox":true},
            {"format":"text", "key":"description", "text":"Q3"}
        ],
        "idempotency_key": key
    })
}

/// One scripted reply from the loopback Anytype upstream.
pub(super) struct UpstreamReply {
    body: String,
    delay: Duration,
    /// When set, the reply is written only after this token is cancelled, so
    /// a test can hold an upstream call in flight deterministically.
    held_until: Option<CancellationToken>,
}

impl UpstreamReply {
    pub(super) fn json(value: &Value) -> Self {
        Self {
            body: value.to_string(),
            delay: Duration::ZERO,
            held_until: None,
        }
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Holds the reply until `release` is cancelled.
    pub(super) fn held_until(mut self, release: CancellationToken) -> Self {
        self.held_until = Some(release);
        self
    }
}

/// A scripted loopback upstream that publishes its exact request tape.
pub(super) struct UpstreamFixture {
    pub(super) endpoint: String,
    pub(super) received: Arc<AtomicUsize>,
    pub(super) served: Arc<AtomicUsize>,
    shutdown: CancellationToken,
    task: JoinHandle<Vec<String>>,
}

impl UpstreamFixture {
    pub(super) async fn start(replies: Vec<UpstreamReply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind upstream fixture");
        let address = listener.local_addr().expect("upstream fixture address");
        let received = Arc::new(AtomicUsize::new(0));
        let served = Arc::new(AtomicUsize::new(0));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Self::serve(
            listener,
            replies,
            received.clone(),
            served.clone(),
            shutdown.clone(),
        ));
        Self {
            endpoint: format!("http://{address}"),
            received,
            served,
            shutdown,
            task,
        }
    }

    async fn serve(
        listener: TcpListener,
        replies: Vec<UpstreamReply>,
        received: Arc<AtomicUsize>,
        served: Arc<AtomicUsize>,
        shutdown: CancellationToken,
    ) -> Vec<String> {
        let mut tape = Vec::new();
        let mut index = 0_usize;
        loop {
            let accepted = tokio::select! {
                () = shutdown.cancelled() => break,
                accepted = listener.accept() => accepted,
            };
            let Ok((mut socket, _)) = accepted else { break };
            let Some(request) = read_upstream_request(&mut socket).await else {
                continue;
            };
            tape.push(
                request
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_end()
                    .to_owned(),
            );
            received.fetch_add(1, Ordering::SeqCst);
            let (status, body, delay, held_until) = match replies.get(index) {
                Some(reply) => (
                    "200 OK",
                    reply.body.clone(),
                    reply.delay,
                    reply.held_until.clone(),
                ),
                None => (
                    "500 Internal Server Error",
                    "{\"error\":\"unscripted\"}".to_owned(),
                    Duration::ZERO,
                    None,
                ),
            };
            index += 1;
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Some(release) = held_until {
                tokio::select! {
                    () = release.cancelled() => {}
                    () = shutdown.cancelled() => break,
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
            served.fetch_add(1, Ordering::SeqCst);
        }
        tape
    }

    pub(super) async fn finish(self) -> Vec<String> {
        self.shutdown.cancel();
        // Unblock the accept loop so the recorded tape is returned promptly.
        let _ = TcpStream::connect(
            self.endpoint
                .trim_start_matches("http://")
                .parse::<SocketAddr>()
                .expect("upstream fixture address"),
        )
        .await;
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("upstream fixture deadline")
            .expect("upstream fixture join")
    }
}

/// Reads one complete upstream request, or `None` if the peer went away.
async fn read_upstream_request(socket: &mut TcpStream) -> Option<String> {
    let mut request = Vec::new();
    let mut expected = None;
    loop {
        let mut buffer = [0_u8; 4096];
        let read = tokio::time::timeout(SOCKET_DEADLINE, socket.read(&mut buffer))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none()
            && let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n")
        {
            let headers = std::str::from_utf8(&request[..end]).ok()?;
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':').and_then(|(name, value)| {
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                    })
                })
                .flatten()
                .unwrap_or(0);
            expected = Some(end + 4 + length);
        }
        if expected.is_some_and(|length| request.len() >= length) {
            break;
        }
        if request.len() > 4 * 1024 * 1024 {
            return None;
        }
    }
    if request.is_empty() {
        return None;
    }
    String::from_utf8(request).ok()
}

/// Runs the initialize lifecycle and returns the issued session id.
pub(super) async fn initialize_over_http(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
) -> String {
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(bearer)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .body(initialize_body(1))
        .send()
        .await
        .expect("initialize request");
    assert_eq!(response.status(), 200);
    let session = response
        .headers()
        .get("mcp-session-id")
        .expect("session id header")
        .to_str()
        .expect("ascii session id")
        .to_owned();
    let _ = response.text().await.expect("initialize body");

    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(bearer)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", &session)
        .header("mcp-protocol-version", REVISION)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .expect("initialized notification");
    assert_eq!(response.status(), 202);
    session
}

/// Issues one JSON-RPC request on a session and returns the decoded message.
pub(super) async fn call_over_http(
    client: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    id: u64,
    request: Value,
) -> Value {
    let mut body = json!({"jsonrpc": "2.0", "id": id});
    if let (Some(body), Some(request)) = (body.as_object_mut(), request.as_object()) {
        for (name, value) in request {
            body.insert(name.clone(), value.clone());
        }
    }
    let response = client
        .post(format!("{base}/mcp"))
        .bearer_auth(bearer)
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", REVISION)
        .body(body.to_string())
        .send()
        .await
        .expect("session request");
    assert_eq!(response.status(), 200);
    let text = response.text().await.expect("session response body");
    last_sse_data(&text)
}

/// An abrupt disconnect mid-mutation keeps exactly one write, and the retry
/// on a fresh session replays the recorded outcome instead of writing again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abrupt_disconnect_during_a_mutation_keeps_one_write_and_a_safe_retry() {
    const BEARER: &str = "mutation-principal";
    const KEY: &str = "load-fault-disconnect";

    let upstream = UpstreamFixture::start(vec![
        UpstreamReply::json(&type_document()),
        UpstreamReply::json(&object_document()).delayed(Duration::from_millis(750)),
        UpstreamReply::json(&object_document()),
    ])
    .await;

    let config = test_config(&[
        ("ANY_MCP_HTTP_MAX_SESSIONS", "4"),
        ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE),
    ]);
    let runtime = upstream_runtime(&upstream.endpoint, ApplicationProfile::Standard, true);
    let backend = Arc::new(StableBackend::new(
        runtime,
        &config,
        CancellationToken::new(),
    ));
    let backend_service = backend.clone();
    let service: McpService =
        Arc::new(move |request| Box::pin(backend_service.clone().call(request)));
    let state = Arc::new(ListenerState::new(
        &config,
        Authenticator::SyntheticAllow,
        None,
        service,
    ));
    let server = LoadServer::start(state, Duration::from_secs(10)).await;
    let http = client();
    let base = server.base();

    // First session: dispatch the keyed mutation and abandon the connection
    // once the upstream write is confirmed in flight.
    let first_session = initialize_over_http(&http, &base, BEARER).await;
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "object_create", "arguments": create_arguments(KEY)},
    })
    .to_string();
    let mut abandoned = server.connect().await;
    let request = format!(
        "POST /mcp HTTP/1.1\r\nhost: {}\r\nauthorization: Bearer {BEARER}\r\naccept: application/json, text/event-stream\r\ncontent-type: application/json\r\nmcp-session-id: {first_session}\r\nmcp-protocol-version: {REVISION}\r\ncontent-length: {}\r\n\r\n{call}",
        server.host(),
        call.len()
    );
    abandoned
        .write_all(request.as_bytes())
        .await
        .expect("write mutation request");
    // Two upstream requests received means the non-idempotent POST is in
    // flight and still awaiting its delayed reply.
    wait_for_count(&upstream.received, 2, "the in-flight upstream write").await;
    drop(abandoned);

    // The detached mutation completes and records its outcome regardless.
    wait_for_count(&upstream.served, 3, "the completed mutation").await;

    // Second session, same principal: the keyed retry must not write again.
    let second_session = initialize_over_http(&http, &base, BEARER).await;
    assert_ne!(first_session, second_session);
    let replayed = call_over_http(
        &http,
        &base,
        BEARER,
        &second_session,
        3,
        json!({
            "method": "tools/call",
            "params": {"name": "object_create", "arguments": create_arguments(KEY)},
        }),
    )
    .await;
    assert_ne!(
        replayed["result"]["isError"],
        json!(true),
        "the retry must replay the recorded success: {replayed}"
    );
    assert_eq!(
        replayed["result"]["structuredContent"]["object"]["id"],
        json!(OBJECT_ID),
        "{replayed}"
    );

    server.stop().await;
    let tape = upstream.finish().await;
    let writes = tape
        .iter()
        .filter(|request| request.starts_with("POST "))
        .count();
    assert_eq!(writes, 1, "exactly one upstream write must exist: {tape:?}");
    assert_eq!(tape.len(), 3, "no unscripted upstream traffic: {tape:?}");
}
