// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! End-to-end stdio protocol regression and acceptance tests for the
//! production binary.
//!
//! The harness deliberately uses only portable Rust process, TCP, thread, and
//! channel APIs. It starts a bounded local Anytype HTTP fixture, drives the
//! real `any-mcp` executable one JSON-RPC line at a time, and retains every
//! stdout byte so protocol-channel purity is checked after clean EOF. Passing
//! tests cover the server's current legacy-shaped lifecycle; ignored tests
//! freeze requirements that the advertised MCP 2026-07-28 revision does not
//! yet satisfy.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const HTTP_TOKEN: &str = "conformance-http-token-must-never-be-logged";
const INPUT_SECRET: &str = "conformance-input-secret-must-never-be-logged";
const DOCUMENT_BODY: &str = "# conformance document body must stay off stderr";
const SPACE_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
const OBJECT_ID: &str = "bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
const HANG_OBJECT_ID: &str = "bafyreihangaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreie6n5l5nkbjal37su54cha4coy7qzuhrnajluzv5qd5jvtsrxkequ";
const HANG_RESOURCE_URI: &str = "anytype://spaces/bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7/objects/bafyreihangaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESOURCE_TEMPLATE: &str = "anytype://spaces/{space_id}/objects/{object_id}";

const NORMAL_TOOLS: [&str; 14] = [
    "object_archive",
    "object_create",
    "object_edit",
    "object_get",
    "object_search",
    "object_update",
    "property_list",
    "server_status",
    "space_list",
    "tag_list",
    "template_list",
    "type_list",
    "view_list",
    "view_object_list",
];

const READ_ONLY_TOOLS: [&str; 10] = [
    "object_get",
    "object_search",
    "property_list",
    "server_status",
    "space_list",
    "tag_list",
    "template_list",
    "type_list",
    "view_list",
    "view_object_list",
];

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
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let stop = thread_stop.clone();
                        let arm = thread_arm.clone();
                        let release = thread_release.clone();
                        let claimed = hang_claimed.clone();
                        let started = hang_tx.clone();
                        thread::spawn(move || {
                            handle_http_connection(
                                stream, &stop, &arm, &release, &claimed, &started,
                            );
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(error) => panic!("HTTP fixture accept failed: {error}"),
                }
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
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.release_hangs.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take() {
            handle.join().expect("join HTTP fixture accept thread");
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
    while request.len() <= 64 * 1024 && !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let read = stream.read(&mut buffer).expect("read HTTP fixture request");
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    assert!(
        request.len() <= 64 * 1024,
        "HTTP fixture request is bounded"
    );
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

struct ProtocolProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: mpsc::Receiver<Vec<u8>>,
    stdout_thread: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    stderr_thread: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
}

struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ProtocolProcess {
    fn start(fixture: &HttpFixture, read_only: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let mut child = command.spawn().expect("spawn production any-mcp binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        let (frame_tx, frames) = mpsc::channel();
        let stdout_thread = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut all = Vec::new();
            loop {
                let mut line = Vec::new();
                let read = reader.read_until(b'\n', &mut line)?;
                if read == 0 {
                    break;
                }
                all.extend_from_slice(&line);
                let _ = frame_tx.send(line);
            }
            Ok(all)
        });
        let stderr_thread = thread::spawn(move || {
            let mut stderr = stderr;
            let mut all = Vec::new();
            stderr.read_to_end(&mut all)?;
            Ok(all)
        });
        Self {
            child,
            stdin: Some(stdin),
            frames,
            stdout_thread: Some(stdout_thread),
            stderr_thread: Some(stderr_thread),
        }
    }

    fn send(&mut self, frame: Value) {
        self.send_bytes(&serde_json::to_vec(&frame).expect("encode JSON-RPC frame"));
    }

    fn send_bytes(&mut self, frame: &[u8]) {
        let stdin = self.stdin.as_mut().expect("open child stdin");
        stdin.write_all(frame).expect("write JSON-RPC frame");
        stdin.write_all(b"\n").expect("terminate JSON-RPC frame");
        stdin.flush().expect("flush JSON-RPC frame");
    }

    fn notification(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let response = self.read_frame();
        assert_eq!(response["id"], id, "response id for {method}");
        response
    }

    fn read_frame(&self) -> Value {
        let bytes = self
            .frames
            .recv_timeout(DEADLINE)
            .expect("protocol response before deadline");
        assert_eq!(bytes.last(), Some(&b'\n'), "one LF-delimited stdout frame");
        assert_ne!(bytes.first(), Some(&b'\n'), "no blank stdout frame");
        serde_json::from_slice(&bytes[..bytes.len() - 1]).expect("stdout line is one JSON frame")
    }

    fn finish(mut self) -> ProcessOutput {
        drop(self.stdin.take());
        let deadline = Instant::now() + DEADLINE;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll child status") {
                break status;
            }
            if Instant::now() >= deadline {
                self.child.kill().expect("kill hung conformance child");
                panic!("any-mcp did not exit after clean stdin EOF");
            }
            thread::sleep(POLL_INTERVAL);
        };
        assert!(
            status.success(),
            "any-mcp exits successfully after stdin EOF"
        );
        let stdout = self
            .stdout_thread
            .take()
            .expect("stdout reader thread")
            .join()
            .expect("join stdout reader")
            .expect("read stdout");
        let stderr = self
            .stderr_thread
            .take()
            .expect("stderr reader thread")
            .join()
            .expect("join stderr reader")
            .expect("read stderr");
        ProcessOutput { stdout, stderr }
    }
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
    assert!(!stdout.is_empty(), "protocol run emitted responses");
    let mut frame_count = 0;
    for line in stdout.split_inclusive(|byte| *byte == b'\n') {
        frame_count += 1;
        assert_eq!(line.last(), Some(&b'\n'), "final frame is LF terminated");
        assert_eq!(line.first(), Some(&b'{'), "stdout has no diagnostic prefix");
        assert_ne!(line.get(line.len().saturating_sub(2)), Some(&b'\r'));
        serde_json::from_slice::<Value>(&line[..line.len() - 1])
            .expect("every stdout byte belongs to a JSON-RPC frame");
    }
    assert!(frame_count >= 10, "substantial protocol exchange completed");
}

fn initialize_legacy_session(process: &mut ProtocolProcess) {
    let initialized = process.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": {"name": "any-mcp-conformance", "version": "1.0.0"}
        }),
    );
    assert_eq!(initialized["result"]["protocolVersion"], "2026-07-28");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "any-mcp");
    assert_eq!(initialized["result"]["capabilities"]["tools"], json!({}));
    assert_eq!(
        initialized["result"]["capabilities"]["resources"],
        json!({})
    );
    process.notification("notifications/initialized", json!({}));
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
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    let expected = if read_only {
        READ_ONLY_TOOLS.as_slice()
    } else {
        NORMAL_TOOLS.as_slice()
    };
    assert_eq!(names, expected);
    assert!(
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| {
                tool["inputSchema"]["additionalProperties"] == false
                    && tool["outputSchema"]["additionalProperties"] == false
                    && tool["annotations"].is_object()
            })
    );

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
    for name in expected {
        if matches!(*name, "server_status" | "object_search") {
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
    assert_stdout_purity(&output.stdout);
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
#[ignore = "blocked by any-iur: rmcp 2.2.0 implements the legacy handshake, not MCP 2026-07-28"]
fn advertised_2026_revision_supports_stateless_server_discovery() {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, true);
    let discovered = process.request(1, "server/discover", json!({"_meta": modern_meta()}));
    assert_eq!(discovered["result"]["resultType"], "complete");
    assert_eq!(
        discovered["result"]["supportedVersions"],
        json!(["2026-07-28"])
    );
    assert_eq!(
        discovered["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "any-mcp"
    );

    let listed = process.request(2, "tools/list", json!({"_meta": modern_meta()}));
    assert_eq!(listed["result"]["resultType"], "complete");
    assert_eq!(
        listed["result"]["tools"]
            .as_array()
            .expect("modern tools array")
            .len(),
        READ_ONLY_TOOLS.len()
    );
    let output = process.finish();
    assert_stdout_purity(&output.stdout);
}

#[test]
#[ignore = "blocked by any-m2u: rmcp stdio currently drops syntactically malformed JSON"]
fn malformed_json_returns_parse_error_and_preserves_the_stream() {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, true);
    initialize_legacy_session(&mut process);

    process.send_bytes(b"{malformed-json");
    let parse_error = process.read_frame();
    assert_eq!(parse_error["id"], Value::Null);
    assert_eq!(parse_error["error"]["code"], -32700);

    let ping = process.request(2, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));
    let output = process.finish();
    assert_stdout_purity(&output.stdout);
}
