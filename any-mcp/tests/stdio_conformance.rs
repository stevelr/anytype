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
//! Tests cover both the current stateless MCP 2026-07-28 wire contract and the
//! exact legacy lifecycle used by current Codex, Claude Code, and Inspector
//! releases.

use std::{
    any::Any,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const FRAME_QUEUE_CAPACITY: usize = 32;
const MAX_STDOUT_LINE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_LINE_BYTES: usize = 64 * 1024;
const MAX_STDERR_BYTES: usize = 1024 * 1024;
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
const NORMAL_CATALOG_SNAPSHOT: &str = include_str!("snapshots/catalog-normal.json");
const READ_ONLY_CATALOG_SNAPSHOT: &str = include_str!("snapshots/catalog-read-only.json");
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

struct ProtocolProcess {
    child: Option<Child>,
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
        let (frame_tx, frames) = mpsc::sync_channel(FRAME_QUEUE_CAPACITY);
        let stdout_thread = thread::spawn(move || read_stdout(stdout, &frame_tx));
        let stderr_thread = thread::spawn(move || {
            read_bounded_stream(stderr, MAX_STDERR_LINE_BYTES, MAX_STDERR_BYTES)
        });
        Self {
            child: Some(child),
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
        self.send_modern_request(id, method, request)
    }

    fn send_modern_request(&mut self, id: Value, method: &str, request: Value) -> Value {
        self.send(request);
        let response = self.read_frame();
        assert_eq!(response["id"], id, "response id for {method}");
        assert_official_modern_response(&response);
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

    fn shutdown(&mut self, graceful: bool, require_success: bool) -> Result<ProcessOutput, String> {
        drop(self.stdin.take());
        let mut errors = Vec::new();
        let status = self.child.take().and_then(|mut child| {
            if graceful {
                let deadline = Instant::now() + DEADLINE;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => break Some(status),
                        Ok(None) if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                        Ok(None) => {
                            errors.push("any-mcp did not exit after clean stdin EOF".to_owned());
                            if let Err(error) = child.kill() {
                                errors.push(format!("kill hung any-mcp child: {error}"));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(error) => {
                                    errors.push(format!("wait for killed any-mcp child: {error}"));
                                    None
                                }
                            };
                        }
                        Err(error) => {
                            errors.push(format!("poll any-mcp child status: {error}"));
                            if let Err(kill_error) = child.kill() {
                                errors.push(format!(
                                    "kill any-mcp child after poll error: {kill_error}"
                                ));
                            }
                            break match child.wait() {
                                Ok(status) => Some(status),
                                Err(wait_error) => {
                                    errors.push(format!("wait for any-mcp child: {wait_error}"));
                                    None
                                }
                            };
                        }
                    }
                }
            } else {
                if let Err(error) = child.kill() {
                    errors.push(format!("kill dropped any-mcp child: {error}"));
                }
                match child.wait() {
                    Ok(status) => Some(status),
                    Err(error) => {
                        errors.push(format!("wait for dropped any-mcp child: {error}"));
                        None
                    }
                }
            }
        });
        if require_success
            && let Some(status) = status
            && !status.success()
        {
            errors.push(format!(
                "any-mcp exited unsuccessfully after stdin EOF: {status}"
            ));
        }

        let stdout = join_reader(self.stdout_thread.take(), "stdout", &mut errors);
        let stderr = join_reader(self.stderr_thread.take(), "stderr", &mut errors);
        if errors.is_empty() {
            Ok(ProcessOutput { stdout, stderr })
        } else {
            Err(errors.join("; "))
        }
    }

    fn finish(mut self) -> ProcessOutput {
        self.shutdown(true, true)
            .unwrap_or_else(|error| panic!("bounded protocol process cleanup failed: {error}"))
    }
}

impl Drop for ProtocolProcess {
    fn drop(&mut self) {
        if (self.child.is_some()
            || self.stdin.is_some()
            || self.stdout_thread.is_some()
            || self.stderr_thread.is_some())
            && let Err(error) = self.shutdown(false, false)
            && !thread::panicking()
        {
            panic!("bounded dropped protocol process cleanup failed: {error}");
        }
    }
}

fn read_stdout(stdout: impl Read, frames: &mpsc::SyncSender<Vec<u8>>) -> std::io::Result<Vec<u8>> {
    let mut reader = BufReader::with_capacity(8 * 1024, stdout);
    let mut all = Vec::new();
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !line.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "stdout ended with an unterminated protocol frame",
                ));
            }
            return Ok(all);
        }
        let chunk_len = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        let next_line_len = line
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| std::io::Error::other("stdout line length overflow"))?;
        if next_line_len > MAX_STDOUT_LINE_BYTES {
            return Err(std::io::Error::other(
                "stdout protocol frame exceeds byte cap",
            ));
        }
        let next_total = all
            .len()
            .checked_add(chunk_len)
            .ok_or_else(|| std::io::Error::other("stdout aggregate length overflow"))?;
        if next_total > MAX_STDOUT_BYTES {
            return Err(std::io::Error::other("stdout exceeds aggregate byte cap"));
        }
        let complete = available[chunk_len - 1] == b'\n';
        line.extend_from_slice(&available[..chunk_len]);
        all.extend_from_slice(&available[..chunk_len]);
        reader.consume(chunk_len);
        if complete {
            frames
                .try_send(std::mem::take(&mut line))
                .map_err(|error| {
                    std::io::Error::other(format!(
                        "bounded stdout frame queue unavailable: {error}"
                    ))
                })?;
        }
    }
}

fn read_bounded_stream(
    mut stream: impl Read,
    max_line_bytes: usize,
    max_bytes: usize,
) -> std::io::Result<Vec<u8>> {
    let mut all = Vec::new();
    let mut line_bytes = 0_usize;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(all);
        }
        let next_total = all
            .len()
            .checked_add(read)
            .ok_or_else(|| std::io::Error::other("diagnostic aggregate length overflow"))?;
        if next_total > max_bytes {
            return Err(std::io::Error::other(
                "diagnostic stream exceeds aggregate byte cap",
            ));
        }
        for byte in &buffer[..read] {
            line_bytes = line_bytes
                .checked_add(1)
                .ok_or_else(|| std::io::Error::other("diagnostic line length overflow"))?;
            if line_bytes > max_line_bytes {
                return Err(std::io::Error::other("diagnostic line exceeds byte cap"));
            }
            if *byte == b'\n' {
                line_bytes = 0;
            }
        }
        all.extend_from_slice(&buffer[..read]);
    }
}

fn join_reader(
    handle: Option<thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    name: &str,
    errors: &mut Vec<String>,
) -> Vec<u8> {
    let Some(handle) = handle else {
        return Vec::new();
    };
    match handle.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            errors.push(format!("read bounded {name}: {error}"));
            Vec::new()
        }
        Err(payload) => {
            errors.push(format!(
                "{name} reader panicked: {}",
                panic_payload(&payload)
            ));
            Vec::new()
        }
    }
}

fn panic_payload(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
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
        NORMAL_CATALOG_SNAPSHOT
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
    assert_exchange_depth(&output.stdout, if read_only { 19 } else { 23 });
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

fn run_modern_stdio_acceptance(read_only: bool) {
    let fixture = HttpFixture::start();
    let mut process = ProtocolProcess::start(&fixture, read_only);
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
    assert_exchange_depth(&output.stdout, if read_only { 21 } else { 25 });
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
    fixture.finish();
    assert_stdout_purity(&output.stdout);
    assert_exchange_depth(&output.stdout, 3);
}
