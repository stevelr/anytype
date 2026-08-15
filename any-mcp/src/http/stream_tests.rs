// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Stable Streamable HTTP event-stream contract over real loopback sockets.
//!
//! These tests consume live SSE frames from the production listener and
//! stable session backend rather than inspecting response heads: the
//! standalone GET stream's priming event and repeated keep-alives, session
//! survival across an abrupt stream disconnect, the exact `Last-Event-ID`
//! reconnect contract implemented by `rmcp` 2.2.0 for both the standalone
//! stream and request-scoped POST response streams, and stream termination
//! when the session is deleted. Every socket wait is bounded.
//!
//! The reconnect contract proved here is the one documented for operators:
//! a POST response stream can be resumed with `Last-Event-ID` only while its
//! request is still in flight, and the response then arrives on the resumed
//! stream; once the response has been emitted the request-scoped stream is
//! gone and reconnecting yields an empty successful stream. The standalone
//! stream resumes as a live keep-alive stream, and any-mcp emits no
//! server-initiated messages, so there is nothing to replay on it. Unknown or
//! malformed IDs also yield an empty successful stream, never an error that
//! would send a browser `EventSource` into a retry loop.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::load_tests::{
    LoadServer, OBJECT_ID, REVISION, SPACE_ID, UNCONSTRAINED_RATE, UpstreamFixture, UpstreamReply,
    call_over_http, client, initialize_over_http, object_document, offline_runtime,
    upstream_runtime,
};
use crate::{
    config::ApplicationProfile,
    http::{
        auth::Authenticator,
        listener::{ListenerState, McpService, tests::test_config},
        session::StableBackend,
    },
};

/// Bounded deadline for one SSE frame or stream end.
const FRAME_DEADLINE: Duration = Duration::from_secs(10);
/// Keep-alive interval selected through the backend seam so several live
/// keep-alives arrive inside the frame deadline; production uses 15 seconds.
const TEST_KEEP_ALIVE: Duration = Duration::from_millis(200);
/// Reviewed retry hint carried by every priming event, in milliseconds.
const RETRY_MILLIS: u64 = 3000;

/// One parsed SSE frame; a comment-only frame is a keep-alive.
#[derive(Debug, Default, PartialEq, Eq)]
struct SseFrame {
    id: Option<String>,
    retry: Option<u64>,
    data: Option<String>,
    comment: bool,
}

impl SseFrame {
    fn is_keep_alive(&self) -> bool {
        self.comment && self.id.is_none() && self.retry.is_none() && self.data.is_none()
    }

    fn json(&self) -> Value {
        serde_json::from_str(self.data.as_deref().expect("SSE data")).expect("SSE data JSON")
    }
}

/// Parses one complete frame (terminated by a blank line).
fn parse_frame(text: &str) -> SseFrame {
    let mut frame = SseFrame::default();
    for line in text.lines() {
        if line.starts_with(':') {
            frame.comment = true;
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => frame.id = Some(value.to_owned()),
            "retry" => frame.retry = value.parse().ok(),
            "data" => {
                let data = frame.data.get_or_insert_with(String::new);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
            _ => {}
        }
    }
    frame
}

/// Incremental reader over a live `reqwest` byte stream.
struct SseReader {
    stream: std::pin::Pin<Box<dyn futures::Stream<Item = reqwest::Result<bytes::Bytes>> + Send>>,
    buffer: Vec<u8>,
    ended: bool,
}

impl SseReader {
    fn new(response: reqwest::Response) -> Self {
        Self {
            stream: Box::pin(response.bytes_stream()),
            buffer: Vec::new(),
            ended: false,
        }
    }

    /// Returns the next complete frame, or `None` once the stream has ended.
    async fn next_frame(&mut self) -> Option<SseFrame> {
        let deadline = Instant::now() + FRAME_DEADLINE;
        loop {
            if let Some(end) = self.buffer.windows(2).position(|window| window == b"\n\n") {
                let text = String::from_utf8_lossy(&self.buffer[..end]).into_owned();
                self.buffer.drain(..end + 2);
                return Some(parse_frame(&text));
            }
            if self.ended {
                assert!(
                    self.buffer.iter().all(u8::is_ascii_whitespace),
                    "stream ended mid-frame: {:?}",
                    String::from_utf8_lossy(&self.buffer)
                );
                return None;
            }
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(chunk))) => self.buffer.extend_from_slice(&chunk),
                Ok(Some(Err(error))) => panic!("SSE stream failed: {error}"),
                Ok(None) => self.ended = true,
                Err(_) => panic!(
                    "no SSE frame within {FRAME_DEADLINE:?}; buffered {:?}",
                    String::from_utf8_lossy(&self.buffer)
                ),
            }
        }
    }

    /// Reads to the end of the stream and returns every remaining frame.
    async fn drain(&mut self) -> Vec<SseFrame> {
        let mut frames = Vec::new();
        while let Some(frame) = self.next_frame().await {
            frames.push(frame);
        }
        frames
    }
}

fn stable_server(keep_alive: Duration) -> (Arc<StableBackend>, Arc<ListenerState>) {
    let config = test_config(&[
        ("ANY_MCP_HTTP_MAX_SESSIONS", "2"),
        ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE),
    ]);
    let backend = Arc::new(StableBackend::with_sse_keep_alive(
        offline_runtime(),
        &config,
        CancellationToken::new(),
        keep_alive,
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
    (backend, state)
}

/// Opens one authenticated GET stream, optionally resuming from an event ID.
async fn open_stream(
    http: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    last_event_id: Option<&str>,
) -> reqwest::Response {
    let mut request = http
        .get(format!("{base}/mcp"))
        .bearer_auth(bearer)
        .header("accept", "text/event-stream")
        .header("mcp-session-id", session)
        .header("mcp-protocol-version", REVISION);
    if let Some(last_event_id) = last_event_id {
        request = request.header("last-event-id", last_event_id);
    }
    tokio::time::timeout(FRAME_DEADLINE, request.send())
        .await
        .expect("stream response head within the deadline")
        .expect("stream request")
}

fn assert_event_stream(response: &reqwest::Response) {
    assert_eq!(response.status(), 200);
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "{:?}",
        response.headers()
    );
}

/// The standalone GET stream delivers its priming event and then live
/// keep-alives; an abrupt disconnect leaves the session usable; a reconnect
/// with `Last-Event-ID` resumes as a live stream without replaying anything
/// (any-mcp emits no server-initiated messages); and deleting the session
/// ends the resumed stream and makes further resumes unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standalone_stream_delivers_live_keep_alives_and_resumes_after_reconnect() {
    const BEARER: &str = "stream-principal";

    let (backend, state) = stable_server(TEST_KEEP_ALIVE);
    let server = LoadServer::start(state, Duration::from_secs(5)).await;
    let http = client();
    let base = server.base();
    let session = initialize_over_http(&http, &base, BEARER).await;

    // Priming first: event ID 0 with the reviewed retry hint and empty data.
    let response = open_stream(&http, &base, BEARER, &session, None).await;
    assert_event_stream(&response);
    let mut reader = SseReader::new(response);
    let priming = reader.next_frame().await.expect("priming event");
    assert_eq!(priming.id.as_deref(), Some("0"), "{priming:?}");
    assert_eq!(priming.retry, Some(RETRY_MILLIS), "{priming:?}");
    assert_eq!(priming.data.as_deref(), Some(""), "{priming:?}");

    // Then live keep-alives, spaced by at least the configured interval.
    let started = Instant::now();
    for index in 0..3 {
        let frame = reader.next_frame().await.expect("live keep-alive");
        assert!(frame.is_keep_alive(), "keep-alive {index}: {frame:?}");
    }
    assert!(
        started.elapsed() >= TEST_KEEP_ALIVE * 2,
        "three keep-alives cannot arrive faster than two intervals: {:?}",
        started.elapsed()
    );

    // Abrupt disconnect: the session and the listener stay healthy.
    drop(reader);
    let listed = call_over_http(
        &http,
        &base,
        BEARER,
        &session,
        2,
        json!({"method": "tools/list"}),
    )
    .await;
    assert!(listed["result"]["tools"].is_array(), "{listed}");

    // Reconnect from the last seen event: a live stream, no priming, no
    // replay, keep-alives continue.
    let response = open_stream(&http, &base, BEARER, &session, Some("0")).await;
    assert_event_stream(&response);
    let mut resumed = SseReader::new(response);
    let frame = resumed.next_frame().await.expect("resumed keep-alive");
    assert!(
        frame.is_keep_alive(),
        "a resumed standalone stream carries no priming or replay: {frame:?}"
    );
    let frame = resumed
        .next_frame()
        .await
        .expect("second resumed keep-alive");
    assert!(frame.is_keep_alive(), "{frame:?}");

    // Session work is unaffected by the resumed stream.
    let listed = call_over_http(
        &http,
        &base,
        BEARER,
        &session,
        3,
        json!({"method": "tools/list"}),
    )
    .await;
    assert!(listed["result"]["tools"].is_array(), "{listed}");

    // DELETE terminates the session: the resumed stream ends within the
    // bound (only keep-alives may precede the end), the slot is released,
    // and a further resume is an unknown session.
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
    let tail = resumed.drain().await;
    assert!(
        tail.iter().all(SseFrame::is_keep_alive),
        "termination delivers no data frames: {tail:?}"
    );
    assert_eq!(backend.available_session_slots(), 2);
    let response = open_stream(&http, &base, BEARER, &session, Some("0")).await;
    assert_eq!(response.status(), 404);

    let drained = server.stop().await;
    assert!(
        drained < Duration::from_secs(5),
        "closed streams must not hold the drain: {drained:?}"
    );
}

/// Extracts `(index, request)` from a request-scoped event ID `index/request`.
fn split_event_id(id: &str) -> (u64, u64) {
    let (index, request) = id.split_once('/').expect("request-scoped event id");
    (
        index.parse().expect("event index"),
        request.parse().expect("http request id"),
    )
}

/// Sends one session POST whose response is a request-scoped SSE stream.
async fn post_stream(
    http: &reqwest::Client,
    base: &str,
    bearer: &str,
    session: &str,
    body: Value,
) -> reqwest::Response {
    let response = http
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
    assert_event_stream(&response);
    response
}

/// Frames that carry protocol content: everything except keep-alives.
fn content_frames(frames: Vec<SseFrame>) -> Vec<SseFrame> {
    frames
        .into_iter()
        .filter(|frame| !frame.is_keep_alive())
        .collect()
}

/// POST response streams are request-scoped. `rmcp` 2.2.0 lets a client that
/// lost the connection while its request was still in flight reconnect with
/// the priming event ID and receive the response on the resumed stream, which
/// then ends. Once the response has been emitted the request-scoped stream is
/// gone: reconnecting yields an empty successful stream, so a client that
/// never read a delivered response must retry the request (mutations stay
/// safe through process-lifetime idempotency). Unknown and malformed IDs also
/// yield an empty successful stream rather than an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_response_stream_resumes_in_flight_and_never_replays_after_completion() {
    const BEARER: &str = "replay-principal";

    // One held upstream reply keeps the tool call in flight until the test
    // releases it; any further upstream traffic is unscripted and fails.
    let release = CancellationToken::new();
    let upstream = UpstreamFixture::start(vec![
        UpstreamReply::json(&object_document()).held_until(release.clone()),
    ])
    .await;
    let config = test_config(&[
        ("ANY_MCP_HTTP_MAX_SESSIONS", "2"),
        ("ANY_MCP_HTTP_REQUESTS_PER_MINUTE", UNCONSTRAINED_RATE),
    ]);
    let backend = Arc::new(StableBackend::new(
        upstream_runtime(&upstream.endpoint, ApplicationProfile::Compact, false),
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

    // In-flight recovery: abandon the response stream after its priming
    // event while the upstream call is still held, reconnect from that ID,
    // then release the upstream. The response arrives exactly once on the
    // resumed stream, which ends afterwards, and the upstream saw one call.
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": "object_get", "arguments": {"space": SPACE_ID, "object_id": OBJECT_ID}}
    });
    let mut abandoned = SseReader::new(post_stream(&http, &base, BEARER, &session, call).await);
    let priming = abandoned.next_frame().await.expect("priming event");
    let (priming_index, held_request) = split_event_id(priming.id.as_deref().expect("priming id"));
    assert_eq!(priming_index, 0, "{priming:?}");
    assert_eq!(priming.retry, Some(RETRY_MILLIS), "{priming:?}");
    assert_eq!(priming.data.as_deref(), Some(""), "{priming:?}");
    drop(abandoned);
    let response = open_stream(
        &http,
        &base,
        BEARER,
        &session,
        Some(&format!("0/{held_request}")),
    )
    .await;
    assert_event_stream(&response);
    let mut resumed = SseReader::new(response);
    // Wait until the upstream is actually holding the call, then release it.
    let deadline = Instant::now() + FRAME_DEADLINE;
    while upstream.received.load(std::sync::atomic::Ordering::SeqCst) < 1 {
        assert!(
            Instant::now() < deadline,
            "upstream never received the held call"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    release.cancel();
    let recovered = content_frames(resumed.drain().await);
    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert_eq!(
        recovered[0].id.as_deref(),
        Some(format!("1/{held_request}").as_str()),
        "{recovered:?}"
    );
    assert_eq!(recovered[0].retry, None);
    let message = recovered[0].json();
    assert_eq!(message["id"], 2, "{message}");
    assert_eq!(message["result"]["isError"], false, "{message}");
    assert_eq!(
        upstream.received.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "resuming never redispatches the upstream call"
    );

    // Completed streams: a fully consumed response has priming `0/N` and the
    // result `1/N`; reconnecting from either ID afterwards replays nothing.
    let list = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"});
    let frames = content_frames(
        SseReader::new(post_stream(&http, &base, BEARER, &session, list).await)
            .drain()
            .await,
    );
    assert_eq!(frames.len(), 2, "{frames:?}");
    let (priming_index, request_id) = split_event_id(frames[0].id.as_deref().expect("priming id"));
    assert_eq!(priming_index, 0, "{frames:?}");
    let result_id = frames[1].id.clone().expect("result event id");
    assert_eq!(split_event_id(&result_id), (1, request_id), "{frames:?}");
    let listed = frames[1].json();
    assert!(listed["result"]["tools"].is_array(), "{listed}");
    for last_event_id in [
        format!("0/{request_id}"),
        result_id,
        format!("0/{}", request_id + 1000),
        "not-an-event-id".to_owned(),
    ] {
        let response = open_stream(&http, &base, BEARER, &session, Some(&last_event_id)).await;
        assert_event_stream(&response);
        let frames = SseReader::new(response).drain().await;
        assert!(
            frames.is_empty(),
            "{last_event_id}: no replay after completion, unknown, or malformed: {frames:?}"
        );
    }

    // The session is intact throughout and released exactly once.
    let listed_again = call_over_http(
        &http,
        &base,
        BEARER,
        &session,
        4,
        json!({"method": "tools/list"}),
    )
    .await;
    assert_eq!(listed_again["result"], listed["result"], "{listed_again}");
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
        "completed request streams must not hold the drain: {drained:?}"
    );
    let tape = upstream.finish().await;
    assert_eq!(tape.len(), 1, "{tape:?}");
}
