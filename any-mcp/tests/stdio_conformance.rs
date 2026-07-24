// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! End-to-end stdio protocol regression and acceptance tests for the
//! production binary.
//!
//! The harness deliberately uses only portable Rust process, TCP, thread, and
//! channel APIs. It starts a bounded local Anytype HTTP fixture, drives the
//! private process-test wrapper for the real `anyr mcp` entrypoint one JSON-RPC
//! line at a time, and retains every
//! stdout byte so protocol-channel purity is checked after clean EOF. Passing
//! Tests cover both the current stateless MCP 2026-07-28 wire contract and the
//! exact legacy lifecycle used by current Codex, Claude Code, and Inspector
//! releases.

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serde_json::{Value, json};

#[path = "support/process.rs"]
mod process_support;

use process_support::{
    FRAME_QUEUE_CAPACITY, MAX_STDERR_BYTES, MAX_STDERR_LINE_BYTES, MAX_STDOUT_LINE_BYTES,
    ProtocolProcess, read_bounded_stream, read_stdout,
};

const DEADLINE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_HTTP_REQUEST_BYTES: usize = 64 * 1024;
const HTTP_TOKEN: &str = "conformance-http-token-must-never-be-logged";
const INPUT_SECRET: &str = "conformance-input-secret-must-never-be-logged";
const DOCUMENT_BODY: &str = "# conformance document body must stay off stderr";
const SPACE_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
const HANG_OBJECT_ID: &str = "bafyreihangaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
const HANG_RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreihangaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESOURCE_TEMPLATE: &str = "anytype://spaces/{space_id}/objects/{object_id}";
const READ_ONLY_CATALOG_SNAPSHOT: &str = include_str!("snapshots/catalog-read-only.snap");
const COMPACT_CATALOG_SNAPSHOT: &str = include_str!("snapshots/catalog-compact.snap");
const MCP_DRAFT_SCHEMA: &str = include_str!("schema/mcp-2026-07-28.json");

static MODERN_REQUEST_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    official_validator(&[
        "DiscoverRequest",
        "ListToolsRequest",
        "CallToolRequest",
        "ListResourcesRequest",
        "ListResourceTemplatesRequest",
        "ReadResourceRequest",
    ])
});

static MODERN_RESPONSE_SCHEMA: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    official_validator(&[
        "DiscoverResultResponse",
        "ListToolsResultResponse",
        "CallToolResultResponse",
        "ListResourcesResultResponse",
        "ListResourceTemplatesResultResponse",
        "ReadResourceResultResponse",
    ])
});

static ERROR_RESPONSE_SCHEMA: LazyLock<jsonschema::Validator> =
    LazyLock::new(|| official_validator(&["JSONRPCErrorResponse"]));

fn official_validator(definitions: &[&str]) -> jsonschema::Validator {
    let official: Value = serde_json::from_str(MCP_DRAFT_SCHEMA).expect("official draft schema");
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": official["$defs"].clone(),
        "anyOf": definitions
            .iter()
            .map(|definition| json!({"$ref": format!("#/$defs/{definition}")}))
            .collect::<Vec<_>>()
    });
    jsonschema::draft202012::options()
        .build(&schema)
        .expect("compile official draft schema subset")
}

fn assert_official_modern_request(request: &Value) {
    assert!(
        MODERN_REQUEST_SCHEMA.is_valid(request),
        "modern request matches the official draft schema: {request}"
    );
}

fn assert_official_modern_response(response: &Value) {
    let validator = if response.get("result").is_some() {
        &*MODERN_RESPONSE_SCHEMA
    } else {
        &*ERROR_RESPONSE_SCHEMA
    };
    assert!(
        validator.is_valid(response),
        "modern response matches the official draft schema: {response}"
    );
}

struct HttpFixture {
    address: String,
    stop: Arc<AtomicBool>,
    arm_hang: Arc<AtomicBool>,
    release_hangs: Arc<AtomicBool>,
    hang_started: mpsc::Receiver<()>,
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP fixture");
        listener
            .set_nonblocking(true)
            .expect("configure nonblocking HTTP fixture");
        let address = format!("http://{}", listener.local_addr().expect("fixture address"));
        let stop = Arc::new(AtomicBool::new(false));
        let arm_hang = Arc::new(AtomicBool::new(false));
        let release_hangs = Arc::new(AtomicBool::new(false));
        let hang_claimed = Arc::new(AtomicBool::new(false));
        let (hang_tx, hang_started) = mpsc::channel();
        let thread_stop = stop.clone();
        let thread_arm = arm_hang.clone();
        let thread_release = release_hangs.clone();
        let accept_thread = thread::spawn(move || {
            let mut workers = Vec::new();
            let mut accept_error = None;
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stop = thread_stop.clone();
                        let arm = thread_arm.clone();
                        let release = thread_release.clone();
                        let claimed = hang_claimed.clone();
                        let started = hang_tx.clone();
                        workers.push(thread::spawn(move || {
                            handle_http_connection(
                                stream, &stop, &arm, &release, &claimed, &started,
                            );
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(error) => {
                        accept_error = Some(error);
                        break;
                    }
                }
            }
            let mut first_panic = None;
            for worker in workers {
                if let Err(payload) = worker.join()
                    && first_panic.is_none()
                {
                    first_panic = Some(payload);
                }
            }
            if let Some(payload) = first_panic {
                std::panic::resume_unwind(payload);
            }
            if let Some(error) = accept_error {
                panic!("HTTP fixture accept failed: {error}");
            }
        });
        Self {
            address,
            stop,
            arm_hang,
            release_hangs,
            hang_started,
            accept_thread: Some(accept_thread),
        }
    }

    fn wait_for_hanging_request(&self) {
        self.hang_started
            .recv_timeout(DEADLINE)
            .expect("space_list reached the hanging HTTP fixture");
    }

    fn arm_hanging_request(&self) {
        self.arm_hang.store(true, Ordering::SeqCst);
    }

    fn release_hanging_requests(&self) {
        self.release_hangs.store(true, Ordering::SeqCst);
    }

    fn shutdown(&mut self) -> std::thread::Result<()> {
        self.release_hangs.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        self.accept_thread
            .take()
            .map_or(Ok(()), |handle| handle.join())
    }

    fn finish(mut self) {
        if let Err(payload) = self.shutdown() {
            std::panic::resume_unwind(payload);
        }
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        if let Err(payload) = self.shutdown()
            && !thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}

fn handle_http_connection(
    mut stream: TcpStream,
    stop: &AtomicBool,
    arm_hang: &AtomicBool,
    release_hangs: &AtomicBool,
    hang_claimed: &AtomicBool,
    hang_started: &mpsc::Sender<()>,
) {
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("HTTP fixture read timeout");
    stream
        .set_write_timeout(Some(DEADLINE))
        .expect("HTTP fixture write timeout");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let remaining = MAX_HTTP_REQUEST_BYTES
            .checked_sub(request.len())
            .expect("HTTP fixture request is bounded");
        assert!(remaining > 0, "HTTP fixture request is bounded");
        let read_limit = remaining.min(buffer.len());
        let read = stream
            .read(&mut buffer[..read_limit])
            .expect("read HTTP fixture request");
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let request = String::from_utf8(request).expect("HTTP fixture request UTF-8");
    let expected_authorization = format!("authorization: Bearer {HTTP_TOKEN}");
    assert!(
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&expected_authorization)),
        "production process authenticated fixture request"
    );
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .expect("HTTP request target");

    let hanging_target = format!("/v1/spaces/{SPACE_ID}/objects/{HANG_OBJECT_ID}");
    if target == hanging_target
        && arm_hang.load(Ordering::SeqCst)
        && !hang_claimed.swap(true, Ordering::SeqCst)
    {
        let _ = hang_started.send(());
        while !stop.load(Ordering::SeqCst) && !release_hangs.load(Ordering::SeqCst) {
            thread::sleep(POLL_INTERVAL);
        }
        return;
    }

    let resource_target = format!("/v1/spaces/{SPACE_ID}/objects/{OBJECT_ID}");
    if target == resource_target {
        respond_json(
            &mut stream,
            "200 OK",
            json!({
                "object": {
                    "archived": false,
                    "id": OBJECT_ID,
                    "space_id": SPACE_ID,
                    "name": "Conformance document",
                    "markdown": DOCUMENT_BODY,
                    "type": {
                        "archived": false,
                        "id": "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                        "key": "page"
                    }
                }
            }),
        );
    } else {
        respond_json(
            &mut stream,
            "200 OK",
            json!({
                "items": [],
                "pagination": {"has_more": false, "limit": 1, "offset": 0, "total": 0}
            }),
        );
    }
}

fn respond_json(stream: &mut TcpStream, status: &str, body: Value) {
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write HTTP fixture response");
}

trait ConformanceProcessExt: Sized {
    fn start(fixture: &HttpFixture, read_only: bool) -> Self {
        let profile = if read_only { "standard" } else { "compact" };
        Self::start_with_options(fixture, Some(profile), read_only, None)
    }

    fn start_preview(fixture: &HttpFixture, read_only: bool) -> Self {
        let profile = if read_only { "standard" } else { "compact" };
        Self::start_with_options(
            fixture,
            Some(profile),
            read_only,
            Some("experimental-2026-07-28"),
        )
    }

    fn start_with_default_profile(fixture: &HttpFixture, read_only: bool) -> Self {
        Self::start_with_options(fixture, None, read_only, None)
    }

    fn start_preview_with_profile(fixture: &HttpFixture, profile: &str, read_only: bool) -> Self {
        Self::start_with_options(
            fixture,
            Some(profile),
            read_only,
            Some("experimental-2026-07-28"),
        )
    }

    fn start_with_options(
        fixture: &HttpFixture,
        profile: Option<&str>,
        read_only: bool,
        protocol: Option<&str>,
    ) -> Self;

    fn modern_request(&mut self, id: u64, method: &str, params: Value) -> Value;

    fn modern_request_with_id(&mut self, id: Value, method: &str, params: Value) -> Value;
}

impl ConformanceProcessExt for ProtocolProcess {
    fn start_with_options(
        fixture: &HttpFixture,
        profile: Option<&str>,
        read_only: bool,
        protocol: Option<&str>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
        command
            .env("ANYTYPE_URL", &fixture.address)
            .env("ANYTYPE_KEYSTORE", "env")
            .env("ANYTYPE_KEYSTORE_SERVICE", "any-mcp-conformance")
            .env("ANYTYPE_KEY_HTTP_TOKEN", HTTP_TOKEN)
            .env("ANY_MCP_READ_ONLY", if read_only { "1" } else { "0" })
            .env("ANY_MCP_STARTUP_TIMEOUT_SECS", "5")
            .env("ANY_MCP_REQUEST_TIMEOUT_SECS", "5")
            .env("RUST_LOG", "any_mcp=info")
            .env_remove("ANYTYPE_GRPC_ENDPOINT")
            .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
            .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
            .env_remove("ANYTYPE_KEY_SESSION_TOKEN");
        if let Some(profile) = profile {
            command.env("ANY_MCP_PROFILE", profile);
        } else {
            command.env_remove("ANY_MCP_PROFILE");
        }
        if let Some(protocol) = protocol {
            command.env("ANY_MCP_PROTOCOL", protocol);
        } else {
            command.env_remove("ANY_MCP_PROTOCOL");
        }
        ProtocolProcess::spawn(command)
    }

    fn modern_request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.modern_request_with_id(json!(id), method, params)
    }

    fn modern_request_with_id(&mut self, id: Value, method: &str, params: Value) -> Value {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "method": method,
            "params": params
        });
        if matches!(
            method,
            "server/discover"
                | "tools/list"
                | "tools/call"
                | "resources/list"
                | "resources/templates/list"
                | "resources/read"
        ) {
            assert_official_modern_request(&request);
        }
        self.send(request);
        let response = self.read_frame();
        assert_eq!(response["id"], id, "response id for {method}");
        assert_official_modern_response(&response);
        self.record_response(&response);
        response
    }
}

#[test]
fn standard_read_write_http_only_fails_before_protocol_output() {
    for protocol in [None, Some("experimental-2026-07-28")] {
        let fixture = HttpFixture::start();
        let fixture_address = fixture.address.clone();
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
        command
            .env("ANYTYPE_URL", &fixture.address)
            .env("ANYTYPE_KEYSTORE", "env")
            .env("ANYTYPE_KEYSTORE_SERVICE", "any-mcp-http-only-rejection")
            .env("ANYTYPE_KEY_HTTP_TOKEN", HTTP_TOKEN)
            .env("ANY_MCP_PROFILE", "standard")
            .env("ANY_MCP_READ_ONLY", "0")
            .env("ANY_MCP_STARTUP_TIMEOUT_SECS", "5")
            .env("RUST_LOG", "any_mcp=info")
            .env_remove("ANYTYPE_GRPC_ENDPOINT")
            .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
            .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
            .env_remove("ANYTYPE_KEY_SESSION_TOKEN");
        if let Some(protocol) = protocol {
            command.env("ANY_MCP_PROTOCOL", protocol);
        } else {
            command.env_remove("ANY_MCP_PROTOCOL");
        }
        let output = command
            .output()
            .expect("run HTTP-only standard read-write process");
        fixture.finish();

        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "startup failure keeps stdout empty"
        );
        let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8 diagnostics");
        assert!(
            stderr.contains("selected Anytype MCP catalog requires configured gRPC credentials")
        );
        for secret in [
            HTTP_TOKEN,
            INPUT_SECRET,
            DOCUMENT_BODY,
            fixture_address.as_str(),
        ] {
            assert!(!stderr.contains(secret), "startup diagnostic is redacted");
        }
    }
}

fn assert_compact_wire_catalog(result: &Value) {
    let actual = canonical_json(json!({
        "read_only": false,
        "tools": result["tools"].clone(),
    }));
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&actual).expect("serialize compact wire catalog")
    );
    assert_eq!(actual, COMPACT_CATALOG_SNAPSHOT);
}

#[test]
fn capture_helpers_enforce_line_and_aggregate_limits() {
    let line = vec![b'x'; MAX_STDERR_LINE_BYTES + 1];
    let line_error = read_bounded_stream(
        std::io::Cursor::new(line),
        MAX_STDERR_LINE_BYTES,
        MAX_STDERR_BYTES,
    )
    .expect_err("oversized diagnostic line is rejected");
    assert!(line_error.to_string().contains("line exceeds byte cap"));

    let aggregate = vec![b'\n'; MAX_STDERR_BYTES + 1];
    let aggregate_error = read_bounded_stream(
        std::io::Cursor::new(aggregate),
        MAX_STDERR_LINE_BYTES,
        MAX_STDERR_BYTES,
    )
    .expect_err("oversized diagnostic aggregate is rejected");
    assert!(aggregate_error.to_string().contains("aggregate byte cap"));

    let (frame_tx, _frames) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
    let oversized_frame = vec![b'x'; MAX_STDOUT_LINE_BYTES + 1];
    let frame_error = read_stdout(std::io::Cursor::new(oversized_frame), &frame_tx)
        .expect_err("oversized stdout frame is rejected before allocation past its cap");
    assert!(frame_error.to_string().contains("frame exceeds byte cap"));
}

fn assert_structured_result(result: &Value, is_error: bool) {
    assert_eq!(result["isError"], is_error);
    let structured = &result["structuredContent"];
    assert!(structured.is_object(), "structuredContent is present");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("compact JSON fallback text");
    assert_eq!(
        serde_json::from_str::<Value>(text).expect("fallback text is JSON"),
        *structured
    );
}

fn assert_stdout_purity(stdout: &[u8]) {
    for line in stdout.split_inclusive(|byte| *byte == b'\n') {
        assert_eq!(line.last(), Some(&b'\n'), "final frame is LF terminated");
        assert_eq!(line.first(), Some(&b'{'), "stdout has no diagnostic prefix");
        assert_ne!(line.get(line.len().saturating_sub(2)), Some(&b'\r'));
        let frame = serde_json::from_slice::<Value>(&line[..line.len() - 1])
            .expect("every stdout byte belongs to a JSON-RPC frame");
        assert!(frame.is_object(), "every stdout frame is a JSON object");
        assert_eq!(
            frame.get("jsonrpc"),
            Some(&json!("2.0")),
            "every stdout object declares JSON-RPC 2.0"
        );
    }
}

fn assert_exchange_depth(stdout: &[u8], expected_frames: usize) {
    assert_eq!(
        stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .count(),
        expected_frames,
        "protocol exchange emitted the exact expected response count"
    );
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(object) => {
            let mut entries = object.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

fn assert_exact_wire_catalog(result: &Value, read_only: bool) -> Vec<String> {
    let actual = canonical_json(json!({
        "read_only": read_only,
        "tools": result["tools"].clone(),
    }));
    let actual = format!(
        "{}\n",
        serde_json::to_string_pretty(&actual).expect("serialize canonical wire catalog")
    );
    let expected = if read_only {
        READ_ONLY_CATALOG_SNAPSHOT
    } else {
        COMPACT_CATALOG_SNAPSHOT
    };
    assert_eq!(
        actual, expected,
        "real tools/list descriptions, nested schemas, and annotations match the reviewed snapshot"
    );
    result["tools"]
        .as_array()
        .expect("wire catalog tools array")
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("wire catalog tool name")
                .to_owned()
        })
        .collect()
}

fn initialize_legacy_session(process: &mut ProtocolProcess) {
    initialize_stable_session(process, "any-mcp-conformance", "1.0.0", "2025-11-25");
}

fn initialize_stable_session(
    process: &mut ProtocolProcess,
    client_name: &str,
    client_version: &str,
    requested_version: &str,
) {
    let initialized = process.request(
        1,
        "initialize",
        json!({
            "protocolVersion": requested_version,
            "capabilities": {},
            "clientInfo": {"name": client_name, "version": client_version}
        }),
    );
    assert_eq!(initialized["result"]["protocolVersion"], requested_version);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "any-mcp");
    assert_eq!(initialized["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        initialized["result"]["capabilities"]["resources"],
        json!({})
    );
    process.notification("notifications/initialized", json!({}));
}

#[test]
fn production_stable_negotiates_exact_pinned_host_requests() {
    let captures = [
        ("codex-mcp-client", "0.144.6", "2025-06-18"),
        ("claude-code", "2.1.214", "2025-11-25"),
        ("inspector", "0.22.0", "2025-11-25"),
    ];
    for (client_name, client_version, requested_version) in captures {
        let fixture = HttpFixture::start();
        let mut process = ProtocolProcess::start(&fixture, true);
        initialize_stable_session(&mut process, client_name, client_version, requested_version);
        let ping = process.request(2, "ping", json!({}));
        assert_eq!(ping["result"], json!({}));
        let output = process.finish();
        fixture.finish();
        assert_stdout_purity(&output.stdout);
        assert_exchange_depth(&output.stdout, 2);
    }
}

fn modern_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "any-mcp-conformance",
            "version": "1.0.0"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

fn run_legacy_stdio_regression(read_only: bool) {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, read_only);
    initialize_legacy_session(&mut process);

    let listed = process.request(2, "tools/list", json!({}));
    let expected = assert_exact_wire_catalog(&listed["result"], read_only);

    let resources = process.request(3, "resources/list", json!({}));
    assert_eq!(resources["result"]["resources"], json!([]));
    let templates = process.request(4, "resources/templates/list", json!({}));
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        RESOURCE_TEMPLATE
    );
    let read = process.request(5, "resources/read", json!({"uri": RESOURCE_URI}));
    assert_eq!(read["result"]["contents"][0]["uri"], RESOURCE_URI);
    assert_eq!(read["result"]["contents"][0]["mimeType"], "text/markdown");
    assert_eq!(read["result"]["contents"][0]["text"], DOCUMENT_BODY);

    let success = process.request(
        6,
        "tools/call",
        json!({"name": "server_status", "arguments": {}}),
    );
    assert_structured_result(&success["result"], false);
    assert_eq!(
        success["result"]["structuredContent"]["http_available"],
        true
    );

    let execution_error = process.request(
        7,
        "tools/call",
        json!({
            "name": "object_search",
            "arguments": {
                "filters": {
                    "operator": "and",
                    "conditions": [{
                        "format": "select",
                        "property_key": "tag",
                        "condition": "in",
                        "values": []
                    }],
                    "filters": []
                }
            }
        }),
    );
    assert_structured_result(&execution_error["result"], true);
    assert_eq!(
        execution_error["result"]["structuredContent"]["code"],
        "validation"
    );

    let mut id = 20;
    for name in &expected {
        if matches!(name.as_str(), "server_status" | "object_search") {
            continue;
        }
        let invalid = process.request(
            id,
            "tools/call",
            json!({"name": name, "arguments": {"unknown": INPUT_SECRET}}),
        );
        assert_eq!(invalid["error"]["code"], -32602, "invalid {name} input");
        assert_eq!(invalid["error"]["data"]["code"], "validation");
        id += 1;
    }

    fixture.arm_hanging_request();
    process.send(json!({
        "jsonrpc": "2.0",
        "id": 80,
        "method": "resources/read",
        "params": {"uri": HANG_RESOURCE_URI}
    }));
    fixture.wait_for_hanging_request();
    process.notification(
        "notifications/cancelled",
        json!({"requestId": 80, "reason": "bounded conformance cancellation"}),
    );
    let ping = process.request(81, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));
    fixture.release_hanging_requests();

    let unknown_tool = process.request(
        82,
        "tools/call",
        json!({"name": "unknown_tool", "arguments": {}}),
    );
    assert_eq!(unknown_tool["error"]["code"], -32601);
    let unknown_method = process.request(83, "conformance/unknown", json!({}));
    assert_eq!(unknown_method["error"]["code"], -32601);

    process.send_bytes(br#"{"jsonrpc":"2.0","id":84,"params":{}}"#);
    let invalid_request = process.read_frame();
    assert_eq!(invalid_request["error"]["code"], -32600);

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, if read_only { 19 } else { 13 });
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("\"id\":80"),
        "rmcp cancellation suppresses the cancelled request response"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8 diagnostics");
    assert!(stderr.contains("authenticated Anytype runtime ready"));
    for secret in [HTTP_TOKEN, INPUT_SECRET, DOCUMENT_BODY] {
        assert!(
            !stderr.contains(secret),
            "stderr redacts protocol and Anytype data"
        );
    }
}

#[test]
fn production_stdio_normal_mode_legacy_regression_is_bounded_and_pure() {
    run_legacy_stdio_regression(false);
}

#[test]
fn production_stdio_read_only_mode_legacy_regression_is_bounded_and_pure() {
    run_legacy_stdio_regression(true);
}

#[test]
fn repeated_production_tool_dispatch_remains_stack_bounded() {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, false);
    initialize_legacy_session(&mut process);

    for id in 2..=129 {
        let status = process.request(
            id,
            "tools/call",
            json!({"name": "server_status", "arguments": {}}),
        );
        assert_structured_result(&status["result"], false);
    }

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, 129);
}

fn run_modern_stdio_acceptance(read_only: bool) {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start_preview(&fixture, read_only);
    let discovered = process.modern_request(1, "server/discover", json!({"_meta": modern_meta()}));
    assert_eq!(discovered["result"]["resultType"], "complete");
    assert_eq!(
        discovered["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
    assert_eq!(
        discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "any-mcp"
    );
    assert_eq!(discovered["result"]["cacheScope"], "public");
    assert!(discovered["result"]["ttlMs"].as_u64().unwrap() > 0);

    let listed = process.modern_request(2, "tools/list", json!({"_meta": modern_meta()}));
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(listed["result"]["cacheScope"], "public");
    assert!(listed["result"]["ttlMs"].as_u64().unwrap() > 0);
    let expected = assert_exact_wire_catalog(&listed["result"], read_only);

    let resources = process.modern_request(3, "resources/list", json!({"_meta": modern_meta()}));
    assert_eq!(resources["result"]["resources"], json!([]));
    assert_eq!(resources["result"]["cacheScope"], "public");
    let templates = process.modern_request(
        4,
        "resources/templates/list",
        json!({"_meta": modern_meta()}),
    );
    assert_eq!(
        templates["result"]["resourceTemplates"][0]["uriTemplate"],
        RESOURCE_TEMPLATE
    );
    assert_eq!(templates["result"]["cacheScope"], "public");
    let read = process.modern_request(
        5,
        "resources/read",
        json!({"uri": RESOURCE_URI, "_meta": modern_meta()}),
    );
    assert_eq!(read["result"]["contents"][0]["text"], DOCUMENT_BODY);
    assert_eq!(read["result"]["ttlMs"], 0);
    assert_eq!(read["result"]["cacheScope"], "private");

    let success = process.modern_request(
        6,
        "tools/call",
        json!({"name": "server_status", "arguments": {}, "_meta": modern_meta()}),
    );
    assert_eq!(success["result"]["resultType"], "complete");
    assert_structured_result(&success["result"], false);
    let execution_error = process.modern_request(
        7,
        "tools/call",
        json!({
            "name": "object_search",
            "arguments": {
                "filters": {
                    "operator": "and",
                    "conditions": [{
                        "format": "select",
                        "property_key": "tag",
                        "condition": "in",
                        "values": []
                    }],
                    "filters": []
                }
            },
            "_meta": modern_meta()
        }),
    );
    assert_structured_result(&execution_error["result"], true);

    let mut id = 20;
    for name in &expected {
        if matches!(name.as_str(), "server_status" | "object_search") {
            continue;
        }
        let invalid = process.modern_request(
            id,
            "tools/call",
            json!({
                "name": name,
                "arguments": {"unknown": INPUT_SECRET},
                "_meta": modern_meta()
            }),
        );
        assert_eq!(invalid["error"]["code"], -32602, "invalid {name} input");
        id += 1;
    }

    let mut unsupported_meta = modern_meta();
    unsupported_meta["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    let unsupported = process.modern_request(70, "tools/list", json!({"_meta": unsupported_meta}));
    assert_eq!(unsupported["error"]["code"], -32022);
    assert_eq!(unsupported["error"]["data"]["requested"], "1900-01-01");
    assert_eq!(
        unsupported["error"]["data"]["supported"],
        json!(["2026-07-28"])
    );

    let mut anonymous_meta = modern_meta();
    anonymous_meta
        .as_object_mut()
        .unwrap()
        .remove("io.modelcontextprotocol/clientInfo");
    let anonymous_discover = process.modern_request_with_id(
        json!(""),
        "server/discover",
        json!({"_meta": anonymous_meta.clone()}),
    );
    assert_eq!(anonymous_discover["id"], "");
    assert_eq!(anonymous_discover["result"]["resultType"], "complete");
    let anonymous_list =
        process.modern_request(71, "tools/list", json!({"_meta": anonymous_meta.clone()}));
    assert_eq!(anonymous_list["result"]["resultType"], "complete");
    let anonymous_tool = process.modern_request(
        72,
        "tools/call",
        json!({
            "name": "server_status",
            "arguments": {},
            "_meta": anonymous_meta
        }),
    );
    assert_structured_result(&anonymous_tool["result"], false);

    let initialize = process.modern_request(73, "initialize", json!({"_meta": modern_meta()}));
    assert_eq!(initialize["error"]["code"], -32601);

    fixture.arm_hanging_request();
    process.send(json!({
        "jsonrpc": "2.0",
        "id": 80,
        "method": "resources/read",
        "params": {"uri": HANG_RESOURCE_URI, "_meta": modern_meta()}
    }));
    fixture.wait_for_hanging_request();
    process.notification(
        "notifications/cancelled",
        json!({"requestId": 80, "reason": "bounded modern cancellation"}),
    );
    let after_cancel =
        process.modern_request(81, "server/discover", json!({"_meta": modern_meta()}));
    assert_eq!(after_cancel["result"]["resultType"], "complete");
    fixture.release_hanging_requests();

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, if read_only { 21 } else { 15 });
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("\"id\":80"),
        "modern cancellation suppresses the cancelled request response"
    );
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8 diagnostics");
    for secret in [HTTP_TOKEN, INPUT_SECRET, DOCUMENT_BODY] {
        assert!(
            !stderr.contains(secret),
            "stderr redacts modern request data"
        );
    }
}

#[test]
fn production_stdio_normal_mode_supports_stateless_2026_revision() {
    run_modern_stdio_acceptance(false);
}

#[test]
fn production_stdio_read_only_mode_supports_stateless_2026_revision() {
    run_modern_stdio_acceptance(true);
}

#[test]
fn compact_catalog_is_identical_on_session_and_stateless_transports() {
    let fixture = HttpFixture::start();

    let mut session = ProtocolProcess::start_with_default_profile(&fixture, false);
    initialize_legacy_session(&mut session);
    let session_list = session.request(2, "tools/list", json!({}));
    assert_compact_wire_catalog(&session_list["result"]);
    let session_tools = session_list["result"]["tools"].clone();
    let session_output = session.finish();
    assert_stdout_purity(&session_output.stdout);

    let mut stateless = ProtocolProcess::start_preview_with_profile(&fixture, "compact", false);
    let stateless_list = stateless.modern_request(1, "tools/list", json!({"_meta": modern_meta()}));
    assert_compact_wire_catalog(&stateless_list["result"]);
    assert_eq!(
        stateless_list["result"]["tools"], session_tools,
        "the selected application catalog is independent of protocol transport"
    );
    let stateless_output = stateless.finish();
    assert_stdout_purity(&stateless_output.stdout);

    fixture.finish();
}

fn assert_exact_decoder_error(response: &Value, code: i64, message: &str) {
    assert_eq!(response.get("id"), Some(&Value::Null));
    assert_eq!(
        response,
        &json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": code, "message": message}
        })
    );
}

fn run_legacy_malformed_recovery(read_only: bool) {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, read_only);
    initialize_legacy_session(&mut process);

    process.send_bytes(b"{malformed-json");
    let first_parse_error = process.read_frame();
    assert_exact_decoder_error(&first_parse_error, -32700, "Parse error");

    process.send_bytes(b"[");
    let second_parse_error = process.read_frame();
    assert_exact_decoder_error(&second_parse_error, -32700, "Parse error");

    let oversized = vec![b'x'; MAX_STDOUT_LINE_BYTES + 1];
    process.send_bytes(&oversized);
    let oversized_error = process.read_frame();
    assert_exact_decoder_error(&oversized_error, -32600, "Invalid request");

    process.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
        "params": "invalid-notification-params"
    }));
    process.send(json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": {"invalid": true}}
    }));
    process.send(json!({
        "jsonrpc": "2.0",
        "method": "ping",
        "params": {"invalid": true}
    }));
    process.notification("$/setTrace", json!({"value": "off"}));

    process.send(json!({
        "jsonrpc": "1.0",
        "method": "notifications/initialized",
        "params": {}
    }));
    let bad_version = process.read_frame();
    assert_exact_decoder_error(&bad_version, -32600, "Invalid request");

    process.send(json!({"jsonrpc": "2.0", "params": {}}));
    let missing_method = process.read_frame();
    assert_exact_decoder_error(&missing_method, -32600, "Invalid request");

    let ping = process.request(2, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));
    let status = process.request(
        3,
        "tools/call",
        json!({"name": "server_status", "arguments": {}}),
    );
    assert_structured_result(&status["result"], false);

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, 8);
    let stderr = String::from_utf8(output.stderr).expect("stderr is UTF-8 diagnostics");
    for secret in [HTTP_TOKEN, INPUT_SECRET, DOCUMENT_BODY] {
        assert!(
            !stderr.contains(secret),
            "stderr redacts malformed input data"
        );
    }
}

#[test]
fn malformed_json_returns_parse_error_and_preserves_the_stream() {
    run_legacy_malformed_recovery(true);
}

#[test]
fn malformed_json_recovery_is_identical_in_normal_mode() {
    run_legacy_malformed_recovery(false);
}

#[test]
fn malformed_first_frame_returns_parse_error_without_selecting_preview() {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, true);

    process.send_bytes(b"{malformed-json");
    let parse_error = process.read_frame();
    assert_exact_decoder_error(&parse_error, -32700, "Parse error");

    process.send(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "server/discover",
        "params": {"_meta": modern_meta()}
    }));
    let rejected = process.read_frame();
    assert_exact_decoder_error(&rejected, -32600, "Invalid request");
    process.send(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "initialize",
        "params": {}
    }));
    let malformed_initialize = process.read_frame();
    assert_exact_decoder_error(&malformed_initialize, -32600, "Invalid request");
    process.send(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "initialize",
        "params": {
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": {"name": "preview-client", "version": "1.0.0"}
        }
    }));
    let preview_initialize = process.read_frame();
    assert_exact_decoder_error(&preview_initialize, -32600, "Invalid request");
    initialize_legacy_session(&mut process);
    let ping = process.request(5, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, 6);
}

#[test]
fn preview_mode_remains_directly_testable_after_malformed_first_frame() {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start_preview(&fixture, true);

    process.send_bytes(b"{malformed-json");
    let parse_error = process.read_frame();
    assert_exact_decoder_error(&parse_error, -32700, "Parse error");
    let discovered = process.modern_request(2, "server/discover", json!({"_meta": modern_meta()}));
    assert_eq!(discovered["result"]["resultType"], "complete");

    let output = process.finish();
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, 2);
}
