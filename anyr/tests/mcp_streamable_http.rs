/*
 * anyr - list, search, and manipulate anytype objects
 * github.com/stevelr/anytype
 *
 * SPDX-FileCopyrightText: 2026 Steve Schoettler
 * SPDX-License-Identifier: Apache-2.0
 */

//! Streamable HTTP conformance across the shipped `anyr mcp` command boundary.
//!
//! The any-mcp crate proves its HTTP listener in-process and through a private
//! process wrapper. These tests instead spawn the user-facing `anyr` binary in
//! `ANY_MCP_TRANSPORT=streamable-http` mode against a bounded scripted Anytype
//! upstream, wait for the real loopback listener, and drive a complete
//! authenticated MCP exchange with only portable Rust process, TCP, and thread
//! APIs: initialize and initialized, `tools/list` over an SSE response, the
//! standalone GET stream, session DELETE, and the stateless preview JSON
//! sentinel. Every wait is bounded, the child is always reaped, stdout must
//! stay empty, and stderr must carry the fixed transport diagnostics without
//! disclosing the listener token, the upstream token, the session ID, or any
//! request or response body.

#![cfg(feature = "mcp")]

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// A hang bound, not a performance assertion: a debug-build child reaching
/// the fixture on a loaded or emulated CI runner has exceeded two minutes.
const DEADLINE: Duration = Duration::from_secs(120);
/// Bounded deadline for one HTTP exchange against the spawned listener.
const EXCHANGE_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_UPSTREAM_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

/// Bearer presented by the child to the scripted upstream.
const UPSTREAM_TOKEN: &str = "spawned-http-upstream-token-must-never-be-logged";
/// Static listener token loaded from the private token file (base64url grammar).
const LISTENER_TOKEN: &str = "spawned-http-listener-token-must-never-be-logged";
/// Client name carried in request bodies; its absence from stderr proves that
/// request bodies stay out of diagnostics.
const CLIENT_NAME: &str = "spawned-http-client-name-must-never-be-logged";
/// Stable revision negotiated over the spawned listener.
const REVISION: &str = "2025-11-25";
/// Preview revision selected through the `_meta` handshake.
const PREVIEW_REVISION: &str = "2026-07-28";

static TOKEN_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Scripted Anytype upstream
// ---------------------------------------------------------------------------

/// A bounded loopback Anytype HTTP upstream that authenticates every request
/// and answers with an empty list, which satisfies the startup probes.
struct UpstreamFixture {
    url: String,
    stop: Arc<AtomicBool>,
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl UpstreamFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind upstream fixture");
        listener
            .set_nonblocking(true)
            .expect("nonblocking upstream fixture");
        let url = format!("http://{}", listener.local_addr().expect("fixture address"));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let accept_thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        workers.push(thread::spawn(move || answer_upstream_request(stream)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(error) => panic!("upstream fixture accept failed: {error}"),
                }
            }
            for worker in workers {
                if let Err(payload) = worker.join() {
                    std::panic::resume_unwind(payload);
                }
            }
        });
        Self {
            url,
            stop,
            accept_thread: Some(accept_thread),
        }
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take()
            && let Err(payload) = handle.join()
        {
            std::panic::resume_unwind(payload);
        }
    }
}

impl Drop for UpstreamFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take()
            && let Err(payload) = handle.join()
            && !thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}

fn answer_upstream_request(mut stream: TcpStream) {
    stream
        .set_nonblocking(false)
        .expect("blocking upstream fixture connection");
    stream
        .set_read_timeout(Some(DEADLINE))
        .expect("upstream fixture read timeout");
    stream
        .set_write_timeout(Some(DEADLINE))
        .expect("upstream fixture write timeout");
    let mut request = Vec::new();
    let mut buffer = [0_u8; 2048];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        let remaining = MAX_UPSTREAM_REQUEST_BYTES
            .checked_sub(request.len())
            .filter(|remaining| *remaining > 0)
            .expect("upstream fixture request is bounded");
        let read_limit = remaining.min(buffer.len());
        let read = stream
            .read(&mut buffer[..read_limit])
            .expect("read upstream fixture request");
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    let request = String::from_utf8_lossy(&request);
    let expected_authorization = format!("authorization: Bearer {UPSTREAM_TOKEN}");
    assert!(
        request
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&expected_authorization)),
        "the spawned process authenticates every upstream request"
    );
    let body = json!({
        "items": [],
        "pagination": {"has_more": false, "limit": 1, "offset": 0, "total": 0}
    })
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write upstream fixture response");
}

// ---------------------------------------------------------------------------
// Private static token file
// ---------------------------------------------------------------------------

/// One owner-only regular file holding the listener token, removed on drop.
struct TokenFile {
    path: std::path::PathBuf,
}

impl TokenFile {
    fn create() -> Self {
        // The token loader opens every ancestor without following symlinks,
        // so the path must not run through a symlinked temp dir (macOS /var).
        let sequence = TOKEN_FILE_SEQUENCE.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let path = std::env::temp_dir()
            .canonicalize()
            .expect("canonical temporary directory")
            .join(format!(
                "anyr-mcp-http-token-{}-{sequence}-{nanos:x}",
                std::process::id()
            ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows_sys::Win32::{Foundation::GENERIC_WRITE, Storage::FileSystem::WRITE_DAC};

            options.access_mode(GENERIC_WRITE | WRITE_DAC);
        }
        let mut file = options.open(&path).expect("create private token file");
        #[cfg(windows)]
        anytype::test_util::protect_private_windows_file(&file, false)
            .expect("protect private token file");
        file.write_all(LISTENER_TOKEN.as_bytes())
            .expect("write listener token");
        file.sync_all().expect("sync listener token");
        Self { path }
    }
}

impl Drop for TokenFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// Spawned `anyr mcp` process
// ---------------------------------------------------------------------------

/// Bounded output captured from the finished child.
struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// Fixed classification of the child status without platform detail.
    exit_category: &'static str,
}

/// The shipped `anyr mcp` command running the HTTP listener, with bounded
/// stdout/stderr capture and unconditional teardown.
struct ServerProcess {
    child: Option<Child>,
    stdout_thread: Option<thread::JoinHandle<Vec<u8>>>,
    stderr_thread: Option<thread::JoinHandle<Vec<u8>>>,
}

impl ServerProcess {
    fn spawn(mut command: Command) -> Self {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().expect("spawn the anyr binary under test");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");
        Self {
            child: Some(child),
            stdout_thread: Some(thread::spawn(move || read_bounded(stdout))),
            stderr_thread: Some(thread::spawn(move || read_bounded(stderr))),
        }
    }

    /// Requests a graceful stop and returns the captured output.
    ///
    /// On Unix this sends `SIGINT` and requires a successful bounded exit. On
    /// Windows a console control event cannot be delivered to a piped child
    /// without sharing a console, so the child is terminated instead and only
    /// the exchange, stdout purity, and redaction assertions apply.
    fn stop(mut self) -> ProcessOutput {
        let mut child = self.child.take().expect("child present");
        #[cfg(unix)]
        let terminated_by_driver = {
            let process_id = i32::try_from(child.id()).expect("child pid fits i32");
            // SAFETY: `kill` receives a positive process identifier owned by
            // this harness and a fixed platform signal number.
            let signalled = unsafe { libc::kill(process_id, libc::SIGINT) } == 0;
            assert!(
                signalled,
                "signal spawned anyr child: {}",
                std::io::Error::last_os_error()
            );
            let deadline = Instant::now() + DEADLINE;
            loop {
                match child.try_wait().expect("poll spawned anyr child") {
                    Some(_) => break false,
                    None if Instant::now() < deadline => thread::sleep(POLL_INTERVAL),
                    None => {
                        let _ = child.kill();
                        break true;
                    }
                }
            }
        };
        #[cfg(not(unix))]
        let terminated_by_driver = {
            let _ = child.kill();
            true
        };
        let status = child.wait().expect("wait for spawned anyr child");
        let stdout = self
            .stdout_thread
            .take()
            .map_or_else(Vec::new, |thread| thread.join().expect("stdout reader"));
        let stderr = self
            .stderr_thread
            .take()
            .map_or_else(Vec::new, |thread| thread.join().expect("stderr reader"));
        let exit_category = if terminated_by_driver {
            "terminated"
        } else if status.success() {
            "success"
        } else if status.code().is_some() {
            "exit_code"
        } else {
            "signal"
        };
        ProcessOutput {
            stdout,
            stderr,
            exit_category,
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for thread in [self.stdout_thread.take(), self.stderr_thread.take()]
            .into_iter()
            .flatten()
        {
            let _ = thread.join();
        }
    }
}

fn read_bounded(mut reader: impl Read) -> Vec<u8> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                let room = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
                captured.extend_from_slice(&buffer[..read.min(room)]);
            }
        }
    }
    captured
}

fn reserve_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve loopback address");
    listener.local_addr().expect("reserved loopback address")
}

/// Starts `anyr mcp` in Streamable HTTP mode and waits for the real listener.
fn start_anyr_mcp_http(
    fixture: &UpstreamFixture,
    token_file: &TokenFile,
    protocol: Option<&str>,
) -> (ServerProcess, SocketAddr) {
    let address = reserve_loopback_address();
    let mut command = Command::new(env!("CARGO_BIN_EXE_anyr"));
    command
        .arg("mcp")
        .env("ANYTYPE_URL", &fixture.url)
        .env("ANYTYPE_KEYSTORE", "env")
        .env("ANYTYPE_KEYSTORE_SERVICE", "anyr-mcp-http-conformance")
        .env("ANYTYPE_KEY_HTTP_TOKEN", UPSTREAM_TOKEN)
        .env("ANY_MCP_PROFILE", "compact")
        .env("ANY_MCP_READ_ONLY", "0")
        .env("ANY_MCP_MAX_CONCURRENCY", "1")
        .env("ANY_MCP_STARTUP_TIMEOUT_SECS", "5")
        .env("ANY_MCP_REQUEST_TIMEOUT_SECS", "5")
        .env("ANY_MCP_TRANSPORT", "streamable-http")
        .env("ANY_MCP_HTTP_BIND", address.to_string())
        .env("ANY_MCP_HTTP_ALLOWED_HOSTS", address.to_string())
        .env("ANY_MCP_HTTP_AUTH", "static-token")
        .env("ANY_MCP_HTTP_TOKEN_FILE", &token_file.path)
        .env("ANY_MCP_HTTP_SHUTDOWN_SECS", "2")
        .env("RUST_LOG", "any_mcp=info")
        .env_remove("ANYTYPE_GRPC_ENDPOINT")
        .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
        .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
        .env_remove("ANYTYPE_KEY_SESSION_TOKEN")
        .env_remove("ANY_MCP_TOOLSETS")
        .env_remove("ANY_MCP_HTTP_ALLOWED_ORIGINS");
    match protocol {
        Some(protocol) => {
            command.env("ANY_MCP_PROTOCOL", protocol);
        }
        None => {
            command.env_remove("ANY_MCP_PROTOCOL");
        }
    }
    let process = ServerProcess::spawn(command);
    wait_for_listener(address);
    (process, address)
}

/// Waits until the real loopback listener answers an unauthenticated probe
/// with the fixed 401 challenge, bounded by the startup deadline.
fn wait_for_listener(address: SocketAddr) {
    let deadline = Instant::now() + DEADLINE;
    let mut last_evidence = "listener did not accept a connection".to_owned();
    while Instant::now() < deadline {
        match exchange(
            address,
            "GET",
            &[("accept", "text/event-stream")],
            None,
            |_| false,
        ) {
            Ok(response) if response.status == 401 => return,
            Ok(response) => {
                last_evidence = format!("readiness probe returned status {}", response.status);
            }
            Err(error) => last_evidence = format!("readiness probe failed: {error}"),
        }
        thread::sleep(POLL_INTERVAL);
    }
    panic!("spawned anyr mcp listener did not become ready: {last_evidence}");
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 client
// ---------------------------------------------------------------------------

/// One decoded HTTP/1.1 response from the spawned listener.
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    /// Decoded body bytes (de-chunked); truncated when the stop predicate
    /// ended an open stream early.
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Performs one bounded HTTP/1.1 exchange on a fresh connection.
///
/// `stop` is evaluated on the decoded body after every chunk; returning true
/// closes the socket immediately, which is how the standalone SSE stream is
/// consumed and then abandoned.
fn exchange(
    address: SocketAddr,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    stop: impl Fn(&[u8]) -> bool,
) -> Result<HttpResponse, String> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(1))
        .map_err(|error| format!("connect: {error}"))?;
    stream
        .set_read_timeout(Some(EXCHANGE_DEADLINE))
        .map_err(|error| format!("read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(EXCHANGE_DEADLINE))
        .map_err(|error| format!("write timeout: {error}"))?;
    let mut request = format!("{method} /mcp HTTP/1.1\r\nhost: {address}\r\nconnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = body.unwrap_or_default();
    if method == "POST" {
        request.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write request: {error}"))?;

    let mut reader = BoundedReader::new(stream);
    let head_end = reader.read_until(b"\r\n\r\n")?;
    let head = String::from_utf8_lossy(&reader.buffer[..head_end]).into_owned();
    reader.buffer.drain(..head_end + 4);
    let mut lines = head.lines();
    let status_line = lines.next().ok_or("empty status line")?;
    let status = status_line
        .split_ascii_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| format!("bad status line: {status_line}"))?;
    let response_headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect::<Vec<_>>();
    let header = |name: &str| {
        response_headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };

    let mut decoded = Vec::new();
    if header("transfer-encoding").is_some_and(|value| value.eq_ignore_ascii_case("chunked")) {
        loop {
            let size_end = reader.read_until(b"\r\n")?;
            let size_line = String::from_utf8_lossy(&reader.buffer[..size_end]).into_owned();
            reader.buffer.drain(..size_end + 2);
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16)
                .map_err(|_| format!("bad chunk size line: {size_line:?}"))?;
            if size == 0 {
                break;
            }
            reader.ensure(size + 2)?;
            decoded.extend_from_slice(&reader.buffer[..size]);
            reader.buffer.drain(..size + 2);
            if decoded.len() > MAX_RESPONSE_BYTES {
                return Err("response body exceeded the bound".to_owned());
            }
            if stop(&decoded) {
                break;
            }
        }
    } else if let Some(length) = header("content-length").and_then(|value| value.parse().ok()) {
        reader.ensure(length)?;
        decoded.extend_from_slice(&reader.buffer[..length]);
    } else {
        reader.read_to_end()?;
        decoded.append(&mut reader.buffer);
    }
    Ok(HttpResponse {
        status,
        headers: response_headers,
        body: decoded,
    })
}

/// A socket reader with a bounded buffer and per-read timeouts.
struct BoundedReader {
    stream: TcpStream,
    buffer: Vec<u8>,
    eof: bool,
}

impl BoundedReader {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            eof: false,
        }
    }

    fn fill(&mut self) -> Result<(), String> {
        if self.eof {
            return Err("unexpected end of response".to_owned());
        }
        let mut chunk = [0_u8; 8192];
        let read = self
            .stream
            .read(&mut chunk)
            .map_err(|error| format!("read response: {error}"))?;
        if read == 0 {
            self.eof = true;
        }
        self.buffer.extend_from_slice(&chunk[..read]);
        if self.buffer.len() > MAX_RESPONSE_BYTES {
            return Err("response exceeded the bound".to_owned());
        }
        Ok(())
    }

    /// Reads until `needle` is buffered and returns its offset.
    fn read_until(&mut self, needle: &[u8]) -> Result<usize, String> {
        loop {
            if let Some(position) = self
                .buffer
                .windows(needle.len())
                .position(|window| window == needle)
            {
                return Ok(position);
            }
            self.fill()?;
        }
    }

    fn ensure(&mut self, length: usize) -> Result<(), String> {
        while self.buffer.len() < length {
            self.fill()?;
        }
        Ok(())
    }

    fn read_to_end(&mut self) -> Result<(), String> {
        while !self.eof {
            self.fill()?;
        }
        Ok(())
    }
}

/// One parsed SSE event; comment-only frames are keepalives.
#[derive(Debug, Default, PartialEq, Eq)]
struct SseEvent {
    id: Option<String>,
    retry: Option<u64>,
    data: Vec<String>,
    comments: Vec<String>,
}

/// Parses complete (blank-line terminated) SSE events from a decoded body.
fn sse_events(body: &[u8]) -> Vec<SseEvent> {
    let text = String::from_utf8_lossy(body);
    let mut events = Vec::new();
    let mut current = SseEvent::default();
    let mut pending = false;
    for line in text.split_inclusive('\n') {
        let Some(line) = line.strip_suffix('\n') else {
            break;
        };
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if pending {
                events.push(std::mem::take(&mut current));
                pending = false;
            }
            continue;
        }
        pending = true;
        if let Some(comment) = line.strip_prefix(':') {
            current.comments.push(comment.trim_start().to_owned());
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "id" => current.id = Some(value.to_owned()),
            "retry" => current.retry = value.parse().ok(),
            "data" => current.data.push(value.to_owned()),
            _ => {}
        }
    }
    events
}

/// Extracts the last JSON `data` payload from an SSE body.
fn last_sse_json(body: &[u8]) -> Value {
    let events = sse_events(body);
    let data = events
        .iter()
        .rev()
        .find(|event| !event.data.is_empty())
        .map(|event| event.data.join("\n"))
        .expect("an SSE data event");
    serde_json::from_str(&data).expect("SSE data is JSON")
}

fn stable_headers(session: Option<&str>) -> Vec<(&str, &str)> {
    let mut headers = vec![
        ("authorization", bearer_header()),
        ("accept", "application/json, text/event-stream"),
        ("content-type", "application/json"),
        ("mcp-protocol-version", REVISION),
    ];
    if let Some(session) = session {
        headers.push(("mcp-session-id", session));
    }
    headers
}

fn bearer_header() -> &'static str {
    static BEARER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BEARER
        .get_or_init(|| format!("Bearer {LISTENER_TOKEN}"))
        .as_str()
}

fn assert_redacted(stderr: &str, session: Option<&str>) {
    for secret in [LISTENER_TOKEN, UPSTREAM_TOKEN, CLIENT_NAME] {
        assert!(
            !stderr.contains(secret),
            "stderr disclosed a secret or body marker: {stderr}"
        );
    }
    if let Some(session) = session {
        assert!(
            !stderr.contains(session),
            "stderr disclosed the session identifier: {stderr}"
        );
    }
    assert!(
        !stderr.contains("\"tools\""),
        "stderr disclosed a response body: {stderr}"
    );
}

fn assert_shutdown(output: &ProcessOutput, session: Option<&str>) {
    assert!(
        output.stdout.is_empty(),
        "HTTP transport reserves stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("http_transport_ready"),
        "fixed readiness diagnostic: {stderr}"
    );
    if cfg!(unix) {
        assert_eq!(output.exit_category, "success", "{stderr}");
        assert!(
            stderr.contains("http_transport_stopping"),
            "fixed stopping diagnostic: {stderr}"
        );
    }
    assert_redacted(&stderr, session);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The shipped `anyr mcp` command serves the complete stable Streamable HTTP
/// lifecycle over a real loopback socket and stops cleanly.
#[test]
fn spawned_anyr_mcp_serves_the_stable_streamable_http_lifecycle() {
    let fixture = UpstreamFixture::start();
    let token_file = TokenFile::create();
    let (process, address) = start_anyr_mcp_http(&fixture, &token_file, None);

    // Authentication is required on every route and challenged exactly.
    let response = exchange(
        address,
        "POST",
        &[
            ("accept", "application/json, text/event-stream"),
            ("content-type", "application/json"),
        ],
        Some("{}"),
        |_| false,
    )
    .expect("unauthenticated request");
    assert_eq!(response.status, 401);
    assert_eq!(response.header("www-authenticate"), Some("Bearer"));

    // Initialize opens one principal-bound session over an SSE response.
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": REVISION,
            "capabilities": {},
            "clientInfo": {"name": CLIENT_NAME, "version": "1.0.0"},
        },
    })
    .to_string();
    let response = exchange(
        address,
        "POST",
        &stable_headers(None),
        Some(&initialize),
        |_| false,
    )
    .expect("initialize");
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert!(
        response
            .header("content-type")
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "{:?}",
        response.headers
    );
    let session = response
        .header("mcp-session-id")
        .expect("session id header")
        .to_owned();
    assert!(!session.is_empty());
    let message = last_sse_json(&response.body);
    assert_eq!(message["id"], 1, "{message}");
    assert_eq!(message["result"]["protocolVersion"], REVISION, "{message}");
    assert!(
        message["result"]["serverInfo"]["name"].is_string(),
        "{message}"
    );

    let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string();
    let response = exchange(
        address,
        "POST",
        &stable_headers(Some(&session)),
        Some(&initialized),
        |_| false,
    )
    .expect("initialized notification");
    assert_eq!(response.status, 202, "{}", response.body_text());

    // tools/list answers over SSE with the compact catalog.
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}).to_string();
    let response = exchange(
        address,
        "POST",
        &stable_headers(Some(&session)),
        Some(&list),
        |_| false,
    )
    .expect("tools/list");
    assert_eq!(response.status, 200, "{}", response.body_text());
    let events = sse_events(&response.body);
    let priming = events.first().expect("priming event");
    assert!(
        priming.id.as_deref().is_some_and(|id| id.starts_with("0/")),
        "request-scoped priming id: {priming:?}"
    );
    assert_eq!(priming.retry, Some(3000), "{priming:?}");
    let message = last_sse_json(&response.body);
    assert_eq!(message["id"], 2, "{message}");
    let tools = message["result"]["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "{message}");
    assert!(
        tools.iter().all(|tool| tool["name"].is_string()),
        "{message}"
    );

    // The standalone GET stream is live: consume its priming event, then
    // abandon the connection.
    let response = exchange(
        address,
        "GET",
        &[
            ("authorization", bearer_header()),
            ("accept", "text/event-stream"),
            ("mcp-protocol-version", REVISION),
            ("mcp-session-id", &session),
        ],
        None,
        |body| !sse_events(body).is_empty(),
    )
    .expect("standalone stream");
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert!(
        response
            .header("content-type")
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "{:?}",
        response.headers
    );
    let events = sse_events(&response.body);
    assert_eq!(events[0].id.as_deref(), Some("0"), "{events:?}");
    assert_eq!(events[0].retry, Some(3000), "{events:?}");

    // The abandoned stream leaves the session usable; DELETE terminates it.
    let list = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}).to_string();
    let response = exchange(
        address,
        "POST",
        &stable_headers(Some(&session)),
        Some(&list),
        |_| false,
    )
    .expect("tools/list after stream disconnect");
    assert_eq!(response.status, 200, "{}", response.body_text());

    let response = exchange(
        address,
        "DELETE",
        &stable_headers(Some(&session)),
        None,
        |_| false,
    )
    .expect("session delete");
    assert!(
        (200..300).contains(&response.status),
        "{} {}",
        response.status,
        response.body_text()
    );
    let response = exchange(
        address,
        "POST",
        &stable_headers(Some(&session)),
        Some(&list),
        |_| false,
    )
    .expect("post-delete request");
    assert_eq!(response.status, 404);

    let output = process.stop();
    fixture.finish();
    assert_shutdown(&output, Some(&session));
}

/// The preview protocol over the shipped command is stateless JSON: one POST
/// yields one `application/json` sentinel and GET is rejected.
#[test]
fn spawned_anyr_mcp_preview_http_answers_the_stateless_json_sentinel() {
    let fixture = UpstreamFixture::start();
    let token_file = TokenFile::create();
    let (process, address) =
        start_anyr_mcp_http(&fixture, &token_file, Some("experimental-2026-07-28"));

    let discover = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": PREVIEW_REVISION,
                "io.modelcontextprotocol/clientInfo": {"name": CLIENT_NAME, "version": "1.0.0"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        },
    })
    .to_string();
    let response = exchange(
        address,
        "POST",
        &[
            ("authorization", bearer_header()),
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ],
        Some(&discover),
        |_| false,
    )
    .expect("preview discover");
    assert_eq!(response.status, 200, "{}", response.body_text());
    assert_eq!(response.header("content-type"), Some("application/json"));
    let message: Value = serde_json::from_slice(&response.body).expect("preview JSON body");
    assert_eq!(message["result"]["resultType"], "complete", "{message}");
    assert_eq!(
        message["result"]["supportedVersions"],
        json!([PREVIEW_REVISION]),
        "{message}"
    );

    let response = exchange(
        address,
        "GET",
        &[
            ("authorization", bearer_header()),
            ("accept", "text/event-stream"),
        ],
        None,
        |_| false,
    )
    .expect("preview GET");
    assert_eq!(response.status, 405, "{}", response.body_text());

    let output = process.stop();
    fixture.finish();
    assert_shutdown(&output, None);
}
