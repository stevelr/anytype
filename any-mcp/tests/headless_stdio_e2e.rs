// any-mcp - bounded, workflow-oriented MCP server for Anytype
// SPDX-License-Identifier: Apache-2.0

//! Individually selectable production-stdio-to-headless acceptance cases.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use anytype::{
    error::AnytypeError,
    prelude::{AnytypeClient, ClientConfig},
    test_util::{
        DisposableRun, TestContext, TestError, TestResult, unique_suffix,
        with_disposable_space_context, with_test_context,
    },
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod support;

use support::{
    live_scenario::{
        McpDriver, ScenarioEvidence, ScenarioId, run_scenario, validate_live_ownership,
    },
    process::{ProcessOutput, ProtocolProcess},
};

#[derive(Clone, Copy)]
struct DriverOptions {
    profile: &'static str,
    read_only: bool,
    preview: bool,
}

impl DriverOptions {
    const STANDARD: Self = Self {
        profile: "standard",
        read_only: false,
        preview: false,
    };
    const COMPACT: Self = Self {
        profile: "compact",
        read_only: false,
        preview: false,
    };
    const READ_ONLY: Self = Self {
        profile: "standard",
        read_only: true,
        preview: false,
    };
    const PREVIEW: Self = Self {
        profile: "compact",
        read_only: false,
        preview: true,
    };

    fn metadata(self) -> String {
        format!(
            "protocol={} profile={} read_only={}",
            if self.preview {
                "2026-07-28"
            } else {
                "2025-11-25"
            },
            self.profile,
            self.read_only
        )
    }
}

fn preview_meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {
            "name": "any-mcp-headless-e2e",
            "version": "1"
        },
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

const PREVIEW_COMPACT_TOOLS: &[&str] = &[
    "object_edit",
    "object_get",
    "object_search",
    "server_status",
];

fn validate_preview_compact_catalog(tools: &[String]) -> Result<(), String> {
    let actual = tools.iter().map(String::as_str).collect::<Vec<_>>();
    (actual == PREVIEW_COMPACT_TOOLS)
        .then_some(())
        .ok_or_else(|| "preview compact catalog identity mismatch".to_owned())
}

struct StdioDriver {
    process: ProtocolProcess,
    next_id: u64,
    options: DriverOptions,
    _keystore: Option<TemporaryKeystore>,
}

#[derive(Debug)]
struct TemporaryKeystore {
    path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
struct FileKeystoreSpec {
    kind: String,
    modifiers: Vec<(String, String)>,
    path_index: usize,
}

impl FileKeystoreSpec {
    fn parse(specification: &str) -> Result<Option<Self>, String> {
        let trimmed = specification.trim().trim_end_matches(':');
        let (kind, remainder) = trimmed
            .split_once(':')
            .map_or((trimmed, None), |(kind, remainder)| (kind, Some(remainder)));
        if !matches!(kind, "file" | "sqlite") {
            return Ok(None);
        }
        let remainder = remainder.ok_or_else(|| {
            "live stdio tests require an explicit file:path=... keystore; plain file/sqlite defaults cannot be isolated safely".to_owned()
        })?;
        let modifiers = parse_keystore_modifiers(remainder)?;
        let paths = modifiers
            .iter()
            .enumerate()
            .filter_map(|(index, (key, _))| (key == "path").then_some(index))
            .collect::<Vec<_>>();
        let [path_index] = paths.as_slice() else {
            return Err(if paths.is_empty() {
                "live stdio tests require exactly one explicit file:path=... keystore".to_owned()
            } else {
                "live stdio tests reject duplicate keystore path modifiers".to_owned()
            });
        };
        let path = &modifiers[*path_index].1;
        if path.is_empty() {
            return Err("live stdio keystore path is empty".to_owned());
        }
        Ok(Some(Self {
            kind: kind.to_owned(),
            modifiers,
            path_index: *path_index,
        }))
    }

    fn source(&self) -> &str {
        &self.modifiers[self.path_index].1
    }

    fn with_path(mut self, path: &Path) -> String {
        self.modifiers[self.path_index].1 = path.display().to_string();
        let modifiers = self
            .modifiers
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(":");
        format!("{}:{modifiers}", self.kind)
    }
}

fn parse_keystore_modifiers(input: &str) -> Result<Vec<(String, String)>, String> {
    const SYNTAX: &str = "invalid keystore modifier syntax";
    let mut pairs = Vec::new();
    let mut remaining = input;
    while !remaining.is_empty() {
        let Some(equals) = remaining.find('=') else {
            return Err(SYNTAX.to_owned());
        };
        let key = &remaining[..equals];
        if !valid_modifier_key(key) {
            return Err(SYNTAX.to_owned());
        }
        let value_and_rest = &remaining[equals + 1..];
        let boundary = value_and_rest
            .char_indices()
            .find_map(|(index, character)| {
                if character != ':' {
                    return None;
                }
                let candidate = &value_and_rest[index + 1..];
                let candidate_equals = candidate.find('=')?;
                valid_modifier_key(&candidate[..candidate_equals]).then_some(index)
            });
        match boundary {
            Some(index) => {
                pairs.push((key.to_owned(), value_and_rest[..index].to_owned()));
                remaining = &value_and_rest[index + 1..];
            }
            None => {
                pairs.push((key.to_owned(), value_and_rest.to_owned()));
                remaining = "";
            }
        }
    }
    Ok(pairs)
}

fn valid_modifier_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

enum MemberFixtureReply {
    Json {
        status: &'static str,
        headers: &'static str,
        body: Value,
    },
    Raw(String),
    Hang(Duration),
}

struct MemberFixtureRequest {
    path: String,
    query: BTreeMap<String, String>,
    reply: MemberFixtureReply,
}

impl MemberFixtureRequest {
    fn json(path: impl Into<String>, query: &[(&str, &str)], body: Value) -> Self {
        Self {
            path: path.into(),
            query: query
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
            reply: MemberFixtureReply::Json {
                status: "200 OK",
                headers: "",
                body,
            },
        }
    }

    fn status(
        path: impl Into<String>,
        query: &[(&str, &str)],
        status: &'static str,
        headers: &'static str,
        body: Value,
    ) -> Self {
        let mut request = Self::json(path, query, body);
        request.reply = MemberFixtureReply::Json {
            status,
            headers,
            body: match request.reply {
                MemberFixtureReply::Json { body, .. } => body,
                _ => unreachable!("JSON member fixture request"),
            },
        };
        request
    }

    fn raw(path: impl Into<String>, query: &[(&str, &str)], body: impl Into<String>) -> Self {
        let mut request = Self::json(path, query, Value::Null);
        request.reply = MemberFixtureReply::Raw(body.into());
        request
    }

    fn hang(path: impl Into<String>, query: &[(&str, &str)], duration: Duration) -> Self {
        let mut request = Self::json(path, query, Value::Null);
        request.reply = MemberFixtureReply::Hang(duration);
        request
    }
}

struct SpawnedMemberFixture {
    endpoint: String,
    task: Option<std::thread::JoinHandle<usize>>,
    accepted: Arc<AtomicUsize>,
}

impl SpawnedMemberFixture {
    fn start(requests: Vec<MemberFixtureRequest>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind spawned members fixture");
        listener
            .set_nonblocking(true)
            .expect("nonblocking spawned members fixture");
        let endpoint = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("spawned members fixture address")
        );
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_count = Arc::clone(&accepted);
        let task = std::thread::Builder::new()
            .name("spawned-members-http-fixture".to_owned())
            .spawn(move || {
                let mut accepted = 0_usize;
                for expected in requests {
                    let mut socket = accept_member_fixture(&listener, Duration::from_secs(30));
                    accepted += 1;
                    accepted_count.store(accepted, Ordering::SeqCst);
                    let target = read_member_fixture_target(&mut socket);
                    let (path, raw_query) = target
                        .split_once('?')
                        .map_or((target.as_str(), ""), |(path, query)| (path, query));
                    assert_eq!(path, expected.path);
                    let query = url::form_urlencoded::parse(raw_query.as_bytes())
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect::<BTreeMap<_, _>>();
                    assert_eq!(query, expected.query, "spawned query for {path}");
                    match expected.reply {
                        MemberFixtureReply::Json {
                            status,
                            headers,
                            body,
                        } => write_member_fixture_response(
                            &mut socket,
                            status,
                            headers,
                            &body.to_string(),
                        ),
                        MemberFixtureReply::Raw(body) => {
                            write_member_fixture_response(&mut socket, "200 OK", "", &body)
                        }
                        MemberFixtureReply::Hang(duration) => std::thread::sleep(duration),
                    }
                }
                let deadline = std::time::Instant::now() + Duration::from_millis(500);
                loop {
                    match listener.accept() {
                        Ok(_) => panic!("spawned members fixture received an extra request"),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if std::time::Instant::now() >= deadline {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("accept extra spawned member request: {error}"),
                    }
                }
                accepted
            })
            .expect("spawn spawned-members fixture");
        Self {
            endpoint,
            task: Some(task),
            accepted,
        }
    }

    fn wait_until_accepted(&self, minimum: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while self.accepted.load(Ordering::SeqCst) < minimum {
            assert!(
                std::time::Instant::now() < deadline,
                "spawned members fixture did not accept request {minimum}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(mut self, expected_requests: usize) {
        let accepted = self
            .task
            .take()
            .expect("spawned members fixture task")
            .join()
            .expect("spawned members fixture thread");
        assert_eq!(accepted, expected_requests);
    }
}

impl Drop for SpawnedMemberFixture {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.join();
        }
    }
}

fn accept_member_fixture(listener: &TcpListener, timeout: Duration) -> TcpStream {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((socket, _)) => return socket,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "spawned members fixture accept timeout"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept spawned member request: {error}"),
        }
    }
}

fn read_member_fixture_target(socket: &mut TcpStream) -> String {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("spawned members fixture read timeout");
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 1024];
        let read = socket
            .read(&mut chunk)
            .expect("read spawned member request");
        assert!(read > 0, "spawned member request ended before headers");
        request.extend_from_slice(&chunk[..read]);
        assert!(request.len() <= 64 * 1024, "spawned request too large");
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&request).expect("ASCII spawned member request");
    request
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .expect("spawned member request target")
        .to_owned()
}

fn write_member_fixture_response(socket: &mut TcpStream, status: &str, headers: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket
        .write_all(response.as_bytes())
        .expect("write spawned member response");
    socket.flush().expect("flush spawned member response");
}

impl TemporaryKeystore {
    fn isolate_environment() -> Result<(Option<Self>, Option<String>), String> {
        let Some(specification) = std::env::var("ANYTYPE_KEYSTORE").ok() else {
            return Ok((None, None));
        };
        Self::isolate_specification(&specification)
    }

    fn isolate_specification(
        specification: &str,
    ) -> Result<(Option<Self>, Option<String>), String> {
        let Some(specification) = FileKeystoreSpec::parse(specification)? else {
            return Ok((None, None));
        };
        let source = PathBuf::from(specification.source());
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| "system clock precedes Unix epoch".to_owned())?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "any-mcp-headless-e2e-{}-{nonce}.db",
            std::process::id()
        ));
        copy_sqlite_snapshot(&source, &path)?;
        let temporary = Self { path };
        let rebuilt = specification.with_path(&temporary.path);
        Ok((Some(temporary), Some(rebuilt)))
    }
}

impl Drop for TemporaryKeystore {
    fn drop(&mut self) {
        remove_sqlite_snapshot(&self.path);
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn copy_sqlite_snapshot(source: &Path, destination: &Path) -> Result<(), String> {
    for _ in 0..3 {
        remove_sqlite_snapshot(destination);
        let before_main = file_fingerprint(source)?;
        let source_wal = sidecar(source, "-wal");
        let before_wal = optional_fingerprint(&source_wal)?;
        std::fs::copy(source, destination)
            .map_err(|_| "copy isolated live-test keystore main database".to_owned())?;
        if before_wal.is_some() {
            std::fs::copy(&source_wal, sidecar(destination, "-wal"))
                .map_err(|_| "copy isolated live-test keystore WAL".to_owned())?;
        }
        if file_fingerprint(source)? == before_main
            && optional_fingerprint(&source_wal)? == before_wal
        {
            return Ok(());
        }
    }
    remove_sqlite_snapshot(destination);
    Err("keystore changed while creating a consistent SQLite snapshot".to_owned())
}

fn file_fingerprint(path: &Path) -> Result<(u64, [u8; 32]), String> {
    let bytes = std::fs::read(path).map_err(|_| "read keystore snapshot source".to_owned())?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    Ok((bytes.len() as u64, digest))
}

fn optional_fingerprint(path: &Path) -> Result<Option<(u64, [u8; 32])>, String> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            Ok(Some((bytes.len() as u64, digest)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err("read keystore WAL snapshot source".to_owned()),
    }
}

fn remove_sqlite_snapshot(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(sidecar(path, suffix));
    }
}

impl StdioDriver {
    fn start(options: DriverOptions) -> Self {
        Self::start_with_toolsets(options, None)
    }

    fn start_with_toolsets(options: DriverOptions, toolsets: Option<&str>) -> Self {
        let (keystore, isolated_specification) = TemporaryKeystore::isolate_environment()
            .unwrap_or_else(|error| panic!("isolate live-test keystore: {error}"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        command
            .env("ANY_MCP_PROFILE", options.profile)
            .env(
                "ANY_MCP_READ_ONLY",
                if options.read_only { "1" } else { "0" },
            )
            .env("ANY_MCP_STARTUP_TIMEOUT_SECS", "15")
            .env("ANY_MCP_REQUEST_TIMEOUT_SECS", "30")
            .env("RUST_LOG", "any_mcp=info");
        if let Some(toolsets) = toolsets {
            command.env("ANY_MCP_TOOLSETS", toolsets);
        } else {
            command.env_remove("ANY_MCP_TOOLSETS");
        }
        if options.preview {
            command.env("ANY_MCP_PROTOCOL", "experimental-2026-07-28");
        } else {
            command.env_remove("ANY_MCP_PROTOCOL");
        }
        if let Some(specification) = isolated_specification {
            command.env("ANYTYPE_KEYSTORE", specification);
        }
        let mut driver = Self::spawn(command, options, keystore);
        driver.initialize();
        driver
    }

    fn start_members_fixture(endpoint: &str, request_timeout_secs: u64) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        command
            .env("ANY_MCP_PROFILE", "compact")
            .env("ANY_MCP_READ_ONLY", "0")
            .env("ANY_MCP_TOOLSETS", "members")
            .env("ANY_MCP_STARTUP_TIMEOUT_SECS", "10")
            .env(
                "ANY_MCP_REQUEST_TIMEOUT_SECS",
                request_timeout_secs.to_string(),
            )
            .env("ANYTYPE_URL", endpoint)
            .env("ANYTYPE_KEYSTORE", "env")
            .env("ANYTYPE_KEYSTORE_SERVICE", "spawned-members-fixture")
            .env("ANYTYPE_KEY_HTTP_TOKEN", "spawned-fixture-http-token")
            .env("ANYTYPE_RATE_LIMIT_MAX_RETRIES", "5")
            .env("RUST_LOG", "any_mcp=info")
            .env_remove("ANY_MCP_PROTOCOL")
            .env_remove("ANYTYPE_GRPC_ENDPOINT")
            .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
            .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
            .env_remove("ANYTYPE_KEY_SESSION_TOKEN");
        let mut driver = Self::spawn(command, DriverOptions::COMPACT, None);
        driver.initialize();
        driver
    }

    fn spawn(
        command: Command,
        options: DriverOptions,
        keystore: Option<TemporaryKeystore>,
    ) -> Self {
        let process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(30));
        Self {
            process,
            next_id: 1,
            options,
            _keystore: keystore,
        }
    }

    fn initialize(&mut self) {
        if self.options.preview {
            let discovered = self.request("server/discover", json!({}));
            assert_eq!(discovered["result"]["resultType"], "complete");
            assert_eq!(
                discovered["result"]["supportedVersions"],
                json!(["2026-07-28"])
            );
        } else {
            let initialized = self.request(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "any-mcp-headless-e2e", "version": "1"}
                }),
            );
            assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
            self.process
                .notification("notifications/initialized", json!({}));
        }
    }

    fn request(&mut self, method: &str, mut params: Value) -> Value {
        if self.options.preview {
            params
                .as_object_mut()
                .expect("preview params object")
                .insert("_meta".to_owned(), preview_meta());
        }
        let id = self.next_id;
        self.next_id += 1;
        self.process.request(id, method, params)
    }

    fn call_tool_sync(&mut self, name: &'static str, arguments: Value) -> Result<Value, String> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        tool_success(name, &response)
    }

    fn call_tool_error_sync(
        &mut self,
        name: &'static str,
        arguments: Value,
    ) -> Result<String, String> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        response
            .pointer("/result/structuredContent/code")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| response_summary(name, &response))
    }

    fn list_tools_sync(&mut self) -> Result<Vec<String>, String> {
        let response = self.request("tools/list", json!({}));
        response["result"]["tools"]
            .as_array()
            .ok_or_else(|| "tools/list omitted tools".to_owned())?
            .iter()
            .map(|tool| {
                tool["name"]
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "tools/list entry omitted name".to_owned())
            })
            .collect()
    }

    fn list_resources_sync(&mut self) -> Result<Value, String> {
        let response = self.request("resources/list", json!({}));
        response
            .get("result")
            .cloned()
            .ok_or_else(|| response_summary("resources/list", &response))
    }

    fn list_resource_templates_sync(&mut self) -> Result<Value, String> {
        let response = self.request("resources/templates/list", json!({}));
        response
            .get("result")
            .cloned()
            .ok_or_else(|| response_summary("resources/templates/list", &response))
    }

    fn read_resource_sync(&mut self, uri: &str) -> Result<Value, String> {
        let response = self.request("resources/read", json!({"uri": uri}));
        response
            .get("result")
            .cloned()
            .ok_or_else(|| response_summary("resources/read", &response))
    }

    fn finish(self) -> (String, ProcessOutput) {
        self.try_finish()
            .unwrap_or_else(|error| panic!("bounded stdio driver cleanup failed: {error}"))
    }

    fn try_finish(self) -> Result<(String, ProcessOutput), String> {
        let transcript = self.process.redacted_transcript();
        self.process.try_finish().map(|output| (transcript, output))
    }

    fn finish_after_panic(mut self) -> (String, ProcessOutput, &'static str) {
        if let Some(failure) = self.process.take_failure() {
            return (failure.transcript, failure.output, failure.category);
        }
        let transcript = self.process.redacted_transcript();
        let output = self.process.finish();
        (transcript, output, "scenario_panic")
    }
}

impl McpDriver for StdioDriver {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(self.call_tool_sync(name, arguments)))
    }

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>> {
        Box::pin(std::future::ready(
            self.call_tool_error_sync(name, arguments),
        ))
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        Box::pin(std::future::ready(self.list_tools_sync()))
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(self.list_resources_sync()))
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(self.list_resource_templates_sync()))
    }

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(self.read_resource_sync(uri)))
    }
}

struct OwnedStdioDriver {
    driver: Arc<Mutex<Option<StdioDriver>>>,
}

impl OwnedStdioDriver {
    fn with_driver<T>(&self, operation: impl FnOnce(&mut StdioDriver) -> T) -> T {
        let mut driver = lock_driver(&self.driver);
        operation(
            driver
                .as_mut()
                .expect("registered stdio child remains owned"),
        )
    }
}

impl McpDriver for OwnedStdioDriver {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let result = self.with_driver(|driver| driver.call_tool_sync(name, arguments));
        Box::pin(std::future::ready(result))
    }

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>> {
        let result = self.with_driver(|driver| driver.call_tool_error_sync(name, arguments));
        Box::pin(std::future::ready(result))
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        let result = self.with_driver(StdioDriver::list_tools_sync);
        Box::pin(std::future::ready(result))
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let result = self.with_driver(StdioDriver::list_resources_sync);
        Box::pin(std::future::ready(result))
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let result = self.with_driver(StdioDriver::list_resource_templates_sync);
        Box::pin(std::future::ready(result))
    }

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let result = self.with_driver(|driver| driver.read_resource_sync(uri));
        Box::pin(std::future::ready(result))
    }
}

fn tool_success(name: &str, response: &Value) -> Result<Value, String> {
    let result = response
        .get("result")
        .ok_or_else(|| response_summary(name, response))?;
    if result["isError"] == true {
        return Err(response_summary(name, response));
    }
    result
        .get("structuredContent")
        .cloned()
        .ok_or_else(|| format!("{name} success omitted structuredContent"))
}

fn response_summary(operation: &str, response: &Value) -> String {
    let jsonrpc = response.pointer("/error/code").and_then(Value::as_i64);
    let tool = response
        .pointer("/result/structuredContent/code")
        .and_then(Value::as_str);
    format!("{operation} failed (jsonrpc_category={jsonrpc:?}, tool_category={tool:?})")
}

#[derive(Default)]
struct CaseRecord {
    error: Option<String>,
    scenario: String,
    fixture_ids: Vec<String>,
    protocol: String,
    transcript: String,
    stderr: StderrMetrics,
    stdout_bytes: usize,
    request_count: usize,
    result_count: usize,
    tool_error_count: usize,
}

#[derive(Default, Debug)]
struct StderrMetrics {
    bytes: usize,
    lines: usize,
    runtime_ready: usize,
    operation_success: usize,
    operation_non_success: usize,
    other: usize,
    invalid_utf8: bool,
}

impl StderrMetrics {
    fn summary(&self) -> String {
        format!(
            "bytes={} lines={} runtime_ready={} operation_success={} operation_non_success={} other={} invalid_utf8={}",
            self.bytes,
            self.lines,
            self.runtime_ready,
            self.operation_success,
            self.operation_non_success,
            self.other,
            self.invalid_utf8
        )
    }
}

fn stderr_metrics(stderr: &[u8]) -> StderrMetrics {
    let mut metrics = StderrMetrics {
        bytes: stderr.len(),
        invalid_utf8: std::str::from_utf8(stderr).is_err(),
        ..StderrMetrics::default()
    };
    for line in stderr
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        metrics.lines += 1;
        if contains_bytes(line, b"authenticated Anytype runtime ready") {
            metrics.runtime_ready += 1;
        } else if contains_bytes(line, b"Anytype operation completed") {
            if contains_bytes(line, b"outcome=\"success\"") {
                metrics.operation_success += 1;
            } else {
                metrics.operation_non_success += 1;
            }
        } else {
            metrics.other += 1;
        }
    }
    metrics
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn process_metrics(transcript: &str) -> (usize, usize, usize) {
    (
        transcript
            .lines()
            .filter(|line| line.starts_with("-> id="))
            .count(),
        transcript
            .lines()
            .filter(|line| line.contains(" result"))
            .count(),
        transcript
            .lines()
            .filter(|line| line.contains(" tool-error:"))
            .count(),
    )
}

fn complete_case<E>(
    driver: StdioDriver,
    mut evidence: ScenarioEvidence,
    result: Result<Result<(), String>, E>,
    options: DriverOptions,
) -> CaseRecord {
    let (error, transcript, output) = match result {
        Ok(result) => {
            let (transcript, output) = driver.finish();
            (result.err(), transcript, output)
        }
        Err(_) => {
            let (transcript, output, category) = driver.finish_after_panic();
            (
                Some(format!("process_category={category}")),
                transcript,
                output,
            )
        }
    };
    let (request_count, result_count, tool_error_count) = process_metrics(&transcript);
    let stderr = stderr_metrics(&output.stderr);
    let fixture_ids = std::mem::take(&mut evidence.fixture_ids);
    CaseRecord {
        error: error.map(|error| evidence.sanitize(&error)),
        scenario: evidence.scenario.as_str().to_owned(),
        fixture_ids,
        protocol: options.metadata(),
        transcript,
        stderr,
        stdout_bytes: output.stdout.len(),
        request_count,
        result_count,
        tool_error_count,
    }
}

fn lock_driver(
    driver: &Arc<Mutex<Option<StdioDriver>>>,
) -> std::sync::MutexGuard<'_, Option<StdioDriver>> {
    driver
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ChildCleanupRecord {
    #[default]
    NotRun,
    Attempted,
    Stopped,
    Failed,
}

fn spawn_disposable_standard_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
) -> TestResult<Arc<Mutex<Option<StdioDriver>>>> {
    let child_environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| TestError::Assertion {
            message: "disposable callback omitted its child environment".to_owned(),
        })?
        .clone();
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
    child_environment.configure(&mut command)?;
    ctx.spawn_owned_child(move || {
        let driver = Arc::new(Mutex::new(Some(StdioDriver::spawn(
            command,
            DriverOptions::STANDARD,
            None,
        ))));
        let stopped = Arc::clone(&driver);
        (driver, move || {
            *cleanup_record.lock().expect("child cleanup record lock") =
                ChildCleanupRecord::Attempted;
            let result = lock_driver(&stopped)
                .take()
                .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
            match result {
                Ok(()) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Stopped;
                    Ok(())
                }
                Err(_) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Failed;
                    Err(TestError::Assertion {
                        message: "registered stdio child did not stop cleanly".to_owned(),
                    })
                }
            }
        })
    })
}

async fn run_spawned_standard_baseline(scenario: ScenarioId) {
    let record = Arc::new(Mutex::new(CaseRecord::default()));
    let captured = Arc::clone(&record);
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let cleanup = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-standard",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let child_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
                let driver = spawn_disposable_standard_driver(ctx.as_ref(), child_cleanup)?;

                let mut evidence = ScenarioEvidence::new(scenario);
                let result = AssertUnwindSafe(async {
                    lock_driver(&driver)
                        .as_mut()
                        .expect("registered stdio child remains owned")
                        .initialize();
                    let mut driver = OwnedStdioDriver {
                        driver: Arc::clone(&driver),
                    };
                    let tools = driver.list_tools().await?;
                    let borrowed = tools.iter().map(String::as_str).collect::<Vec<_>>();
                    validate_live_ownership(
                        &borrowed,
                        &[
                            "resources/list",
                            "resources/read",
                            "resources/templates/list",
                        ],
                    )?;
                    run_scenario(scenario, &mut driver, ctx.as_ref(), &mut evidence).await
                })
                .catch_unwind()
                .await;
                let driver = lock_driver(&driver)
                    .take()
                    .expect("registered stdio child remains available for shutdown");
                *captured.lock().expect("case record lock") =
                    complete_case(driver, evidence, result, DriverOptions::STANDARD);
                Ok(())
            })
        },
    ))
    .await;
    let cleanup_status = if cleanup.is_ok() { "success" } else { "failed" };
    let record = record.lock().expect("case record lock");
    if let Some(error) = &record.error {
        panic!(
            "scenario={} fixtures={:?} {} error={} requests={} results={} tool_errors={} stdout_bytes={} cleanup={}\ntranscript:\n{}\nstderr_metrics={}",
            record.scenario,
            record.fixture_ids,
            record.protocol,
            error,
            record.request_count,
            record.result_count,
            record.tool_error_count,
            record.stdout_bytes,
            cleanup_status,
            record.transcript,
            record.stderr.summary()
        );
    }
    drop(record);
    match cleanup.expect("cleanup-safe disposable spawned baseline scenario") {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable spawned baseline skipped before callback: {reason:?}");
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DisposableSentinelMode {
    Success,
    Panic,
}

#[derive(Default)]
struct DisposableSentinelIds {
    space_id: Option<String>,
    object_id: Option<String>,
}

fn sentinel_assertion(message: &str) -> TestError {
    TestError::Assertion {
        message: message.to_owned(),
    }
}

fn fresh_no_cache_client() -> Result<AnytypeClient, String> {
    let environment = |name: &str| {
        std::env::var(name).map_err(|_| format!("missing required sentinel selector {name}"))
    };
    AnytypeClient::with_config(ClientConfig {
        base_url: Some(environment("ANYTYPE_URL")?),
        app_name: "any-mcp-disposable-absence".to_owned(),
        rate_limit_max_retries: 5,
        disable_cache: true,
        keystore: Some("env".to_owned()),
        keystore_service: Some(environment("ANYTYPE_KEYSTORE_SERVICE")?),
        grpc_endpoint: Some(environment("ANYTYPE_GRPC_ENDPOINT")?),
        ..ClientConfig::default()
    })
    .map_err(|_| "construct fresh no-cache sentinel client".to_owned())
}

async fn assert_fresh_space_absence(space_id: &str) {
    let client = fresh_no_cache_client().expect("fresh no-cache sentinel client");
    client
        .get_config()
        .limits
        .validate_id(space_id, "sentinel exact space id")
        .expect("valid sentinel exact space id");
    match tokio::time::timeout(Duration::from_secs(30), client.space(space_id).get()).await {
        Ok(Err(AnytypeError::NotFound { .. } | AnytypeError::ApiError { code: 404, .. })) => {}
        Ok(Ok(space)) => {
            client
                .get_config()
                .limits
                .validate_id(&space.id, "sentinel returned space id")
                .expect("valid returned sentinel space id");
            client
                .get_config()
                .limits
                .validate_name(&space.name, "sentinel returned space name")
                .expect("valid returned sentinel space name");
            assert_eq!(space.id, space_id, "exact sentinel response identity");
            assert_eq!(space.object, anytype::spaces::SpaceModel::Space);
            assert!(
                !space.name.chars().any(char::is_control),
                "sentinel space name has no controls"
            );
            panic!("cleaned disposable sentinel space remains present");
        }
        Ok(Err(_)) => panic!("fresh exact sentinel absence request failed"),
        Err(_) => panic!("fresh exact sentinel absence request timed out"),
    }
}

async fn run_disposable_stdio_lifecycle_sentinel(mode: DisposableSentinelMode) {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let deliberate_panic = Arc::new(AtomicBool::new(false));
    let panic_flag = Arc::clone(&deliberate_panic);
    let ids = Arc::new(Mutex::new(DisposableSentinelIds::default()));
    let captured_ids = Arc::clone(&ids);
    let child_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let captured_cleanup = Arc::clone(&child_cleanup);

    let invocation = AssertUnwindSafe(with_disposable_space_context(
        match mode {
            DisposableSentinelMode::Success => "any-mcp-stdio-lifecycle",
            DisposableSentinelMode::Panic => "any-mcp-stdio-panic-lifecycle",
        },
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            captured_ids
                .lock()
                .expect("sentinel id record lock")
                .space_id = Some(ctx.space_id.clone());
            Box::pin(async move {
                let driver = spawn_disposable_standard_driver(ctx.as_ref(), captured_cleanup)?;
                lock_driver(&driver)
                    .as_mut()
                    .ok_or_else(|| sentinel_assertion("registered sentinel child disappeared"))?
                    .initialize();
                let mut driver = OwnedStdioDriver {
                    driver: Arc::clone(&driver),
                };
                let suffix = unique_suffix();
                let created = driver
                    .call_tool(
                        "object_create",
                        json!({
                            "space": ctx.space_id,
                            "type": "page",
                            "name": format!("MCP disposable sentinel {suffix}"),
                            "idempotency_key": format!("mcp-disposable-sentinel-{suffix}")
                        }),
                    )
                    .await
                    .map_err(|_| sentinel_assertion("stdio sentinel object_create failed"))?;
                let object_id = created
                    .pointer("/object/id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| sentinel_assertion("stdio sentinel create omitted object id"))?
                    .to_owned();
                ctx.register_object(&object_id);
                captured_ids
                    .lock()
                    .expect("sentinel id record lock")
                    .object_id = Some(object_id.clone());
                if created.pointer("/object/space_id").and_then(Value::as_str)
                    != Some(ctx.space_id.as_str())
                {
                    return Err(sentinel_assertion(
                        "stdio sentinel create returned the wrong space identity",
                    ));
                }

                let read = driver
                    .call_tool(
                        "object_get",
                        json!({"space": ctx.space_id, "object_id": object_id}),
                    )
                    .await
                    .map_err(|_| sentinel_assertion("stdio sentinel object_get failed"))?;
                if read.pointer("/object/summary/id").and_then(Value::as_str)
                    != Some(object_id.as_str())
                    || read
                        .pointer("/object/summary/space_id")
                        .and_then(Value::as_str)
                        != Some(ctx.space_id.as_str())
                {
                    return Err(sentinel_assertion(
                        "stdio sentinel read returned the wrong object identity",
                    ));
                }
                let independent = ctx.client.object(&ctx.space_id, &object_id).get().await?;
                if independent.id != object_id || independent.space_id != ctx.space_id {
                    return Err(sentinel_assertion(
                        "independent sentinel read returned the wrong identity",
                    ));
                }

                if mode == DisposableSentinelMode::Panic {
                    panic_flag.store(true, Ordering::SeqCst);
                    panic!("intentional disposable stdio sentinel panic");
                }
                Ok(())
            })
        },
    ))
    .catch_unwind()
    .await;

    if let Ok(Ok(DisposableRun::Skipped(reason))) = &invocation {
        assert!(!callback_ran.load(Ordering::SeqCst));
        assert_eq!(
            *child_cleanup.lock().expect("child cleanup record lock"),
            ChildCleanupRecord::NotRun
        );
        eprintln!("disposable stdio lifecycle sentinel skipped before callback: {reason:?}");
        return;
    }

    match mode {
        DisposableSentinelMode::Success => match invocation {
            Ok(Ok(DisposableRun::Completed(()))) => {
                assert!(callback_ran.load(Ordering::SeqCst));
                assert!(!deliberate_panic.load(Ordering::SeqCst));
            }
            Ok(Ok(DisposableRun::Skipped(_))) => unreachable!("skip handled above"),
            Ok(Err(error)) => panic!("disposable stdio lifecycle failed: {}", error.category()),
            Err(_) => panic!("disposable stdio lifecycle unexpectedly panicked"),
        },
        DisposableSentinelMode::Panic => {
            assert!(
                invocation.is_err(),
                "deliberate callback panic was not resumed"
            );
            assert!(callback_ran.load(Ordering::SeqCst));
            assert!(
                deliberate_panic.load(Ordering::SeqCst),
                "a panic occurred before the deliberate sentinel point"
            );
        }
    }

    assert_eq!(
        *child_cleanup.lock().expect("child cleanup record lock"),
        ChildCleanupRecord::Stopped,
        "registered stdio child cleanup completed before the sentinel returned"
    );
    let (space_id, object_id) = {
        let ids = ids.lock().expect("sentinel id record lock");
        let space_id = ids
            .space_id
            .as_deref()
            .expect("sentinel captured its exact space id")
            .to_owned();
        let object_id = ids
            .object_id
            .as_deref()
            .expect("sentinel captured its exact object id")
            .to_owned();
        (space_id, object_id)
    };
    assert_fresh_space_absence(&space_id).await;
    eprintln!(
        "disposable stdio sentinel cleanup verified: mode={mode:?} space_id={space_id} object_id={object_id} child=stopped exact_absence=verified"
    );
}

async fn run_spawned_baseline(scenario: ScenarioId, options: DriverOptions) {
    let mut driver = StdioDriver::start(options);
    if options.preview {
        let tools = driver.list_tools().await.expect("preview compact catalog");
        validate_preview_compact_catalog(&tools).expect("exact preview compact catalog identity");
    } else if options.profile == "standard" && !options.read_only {
        let tools = driver.list_tools().await.expect("standard catalog");
        let borrowed = tools.iter().map(String::as_str).collect::<Vec<_>>();
        validate_live_ownership(
            &borrowed,
            &[
                "resources/list",
                "resources/read",
                "resources/templates/list",
            ],
        )
        .expect("spawned catalog has complete executable ownership");
    }
    let record = Arc::new(Mutex::new(CaseRecord::default()));
    let captured = record.clone();
    let cleanup = Box::pin(with_test_context(move |ctx| {
        Box::pin(async move {
            ctx.client
                .ping_http()
                .await
                .map_err(anytype::test_util::TestError::from)?;
            ctx.client
                .ping_grpc()
                .await
                .map_err(anytype::test_util::TestError::from)?;
            let mut evidence = ScenarioEvidence::new(scenario);
            let result = AssertUnwindSafe(run_scenario(
                scenario,
                &mut driver,
                ctx.as_ref(),
                &mut evidence,
            ))
            .catch_unwind()
            .await;
            *captured.lock().expect("case record lock") =
                complete_case(driver, evidence, result, options);
            Ok(())
        })
    }))
    .await;
    let cleanup_status = if cleanup.is_ok() { "success" } else { "failed" };
    let record = record.lock().expect("case record lock");
    if let Some(error) = &record.error {
        panic!(
            "scenario={} fixtures={:?} {} error={} requests={} results={} tool_errors={} stdout_bytes={} cleanup={}\ntranscript:\n{}\nstderr_metrics={}",
            record.scenario,
            record.fixture_ids,
            record.protocol,
            error,
            record.request_count,
            record.result_count,
            record.tool_error_count,
            record.stdout_bytes,
            cleanup_status,
            record.transcript,
            record.stderr.summary()
        );
    }
    cleanup.expect("cleanup-safe spawned baseline scenario");
}

macro_rules! spawned_baseline_test {
    ($name:ident, $scenario:expr) => {
        #[tokio::test]
        #[serial_test::serial]
        #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
        async fn $name() {
            run_spawned_standard_baseline($scenario).await;
        }
    };
}

spawned_baseline_test!(headless_stdio_standard_discovery, ScenarioId::Discovery);
spawned_baseline_test!(headless_stdio_standard_documents, ScenarioId::Documents);
spawned_baseline_test!(headless_stdio_standard_views, ScenarioId::Views);
spawned_baseline_test!(headless_stdio_standard_mutations, ScenarioId::Mutations);
spawned_baseline_test!(
    headless_stdio_standard_markdown_noop,
    ScenarioId::MarkdownNoop
);
spawned_baseline_test!(headless_stdio_standard_archive, ScenarioId::Archive);

const SPAWNED_MEMBER_SPACE_ID: &str =
    "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
const SPAWNED_MEMBER_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a";
const SPAWNED_OTHER_MEMBER_ID: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4b";

fn member_http_page(items: Vec<Value>, offset: usize, limit: usize, total: usize) -> Value {
    json!({
        "data": items,
        "pagination": {
            "offset": offset,
            "limit": limit,
            "total": total,
            "has_more": offset + limit < total
        }
    })
}

fn spawned_member_value(id: &str, role: &str, status: &str) -> Value {
    json!({
        "id": id,
        "name": "Local member",
        "global_name": "SPAWNED-GLOBAL-NAME-SECRET",
        "identity": "SPAWNED-NETWORK-IDENTITY-SECRET",
        "icon": {"url": "SPAWNED-ICON-SECRET"},
        "role": role,
        "status": status
    })
}

fn startup_space_page() -> Value {
    member_http_page(Vec::new(), 0, 1, 0)
}

fn tool_result_code(response: &Value) -> Option<&str> {
    response
        .pointer("/result/structuredContent/code")
        .and_then(Value::as_str)
}

fn push_spawned_six_attempt_success(
    requests: &mut Vec<MemberFixtureRequest>,
    path: impl Into<String>,
    query: &[(&str, &str)],
    success: Value,
) {
    let path = path.into();
    for attempt in 0..5 {
        requests.push(if attempt == 1 {
            MemberFixtureRequest::status(
                &path,
                query,
                "504 Gateway Timeout",
                "",
                json!({"class": "retryable-status"}),
            )
        } else {
            MemberFixtureRequest::status(
                &path,
                query,
                "429 Too Many Requests",
                "RateLimit-Reset: 0\r\n",
                json!({"class": "rate-limit"}),
            )
        });
    }
    requests.push(MemberFixtureRequest::json(path, query, success));
}

#[test]
#[serial_test::serial]
fn headless_stdio_members_scripted_failure_matrix() {
    let member_path = format!("/v1/spaces/{SPAWNED_MEMBER_SPACE_ID}/members");
    let exact_path = format!("{member_path}/{SPAWNED_MEMBER_ID}");
    let secret = "SPAWNED-UPSTREAM-BODY-SECRET";
    let fixture = SpawnedMemberFixture::start(vec![
        MemberFixtureRequest::json("/v1/spaces", &[("limit", "1")], startup_space_page()),
        MemberFixtureRequest::json(
            &member_path,
            &[("limit", "1")],
            member_http_page(
                vec![spawned_member_value(SPAWNED_MEMBER_ID, "owner", "active")],
                0,
                1,
                2,
            ),
        ),
        MemberFixtureRequest::json(
            "/v1/spaces",
            &[("limit", "99")],
            member_http_page(
                vec![
                    json!({"id": "space-alpha", "name": "Shared", "object": "space"}),
                    json!({"id": "space-beta", "name": "shared", "object": "space"}),
                ],
                0,
                99,
                2,
            ),
        ),
        MemberFixtureRequest::status(
            &member_path,
            &[("limit", "20")],
            "401 Unauthorized",
            "",
            json!({"secret": secret}),
        ),
        MemberFixtureRequest::status(
            &member_path,
            &[("limit", "20")],
            "403 Forbidden",
            "",
            json!({"secret": secret}),
        ),
        MemberFixtureRequest::json(
            &exact_path,
            &[],
            json!({"member": spawned_member_value(SPAWNED_OTHER_MEMBER_ID, "owner", "active")}),
        ),
        MemberFixtureRequest::raw(&exact_path, &[], "{malformed-json"),
        MemberFixtureRequest::json(
            &exact_path,
            &[],
            json!({"member": spawned_member_value(SPAWNED_MEMBER_ID, "superuser", "active")}),
        ),
        MemberFixtureRequest::json(
            &exact_path,
            &[],
            json!({"member": spawned_member_value(SPAWNED_MEMBER_ID, "owner", "unknown")}),
        ),
        MemberFixtureRequest::status(
            &member_path,
            &[("limit", "20")],
            "503 Service Unavailable",
            "",
            json!({"secret": secret}),
        ),
        MemberFixtureRequest::hang(&member_path, &[("limit", "20")], Duration::from_secs(2)),
        MemberFixtureRequest::hang(&member_path, &[("limit", "20")], Duration::from_secs(2)),
    ]);
    let mut driver = StdioDriver::start_members_fixture(&fixture.endpoint, 1);

    let catalog = driver.request("tools/list", json!({}));
    let tools = catalog["result"]["tools"]
        .as_array()
        .expect("spawned members catalog");
    for expected in [
        any_mcp::member_toolset::member_get_tool()
            .expect("member_get contract")
            .into_tool(),
        any_mcp::member_toolset::member_list_tool()
            .expect("member_list contract")
            .into_tool(),
    ] {
        let actual = tools
            .iter()
            .find(|tool| tool["name"] == expected.name.as_ref())
            .expect("spawned members tool metadata");
        assert_eq!(
            *actual,
            serde_json::to_value(expected).expect("expected member contract")
        );
    }

    for arguments in [
        json!({"space": SPAWNED_MEMBER_SPACE_ID, "cursor": null}),
        json!({"space": SPAWNED_MEMBER_SPACE_ID, "limit": 101}),
        json!({"space": SPAWNED_MEMBER_SPACE_ID, "filter": "forbidden"}),
    ] {
        let response = driver.request(
            "tools/call",
            json!({"name": "member_list", "arguments": arguments}),
        );
        assert_eq!(
            response.pointer("/error/code").and_then(Value::as_i64),
            Some(-32602)
        );
    }

    let first = driver
        .call_tool_sync(
            "member_list",
            json!({"space": SPAWNED_MEMBER_SPACE_ID, "limit": 1}),
        )
        .expect("spawned first member page");
    let cursor = first["next_cursor"]
        .as_str()
        .expect("spawned member continuation")
        .to_owned();
    for arguments in [
        json!({"space": SPAWNED_MEMBER_SPACE_ID, "limit": 2, "cursor": cursor.clone()}),
        json!({
            "space": "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z.2tq5w93cr6oe7",
            "limit": 1,
            "cursor": cursor
        }),
    ] {
        assert_eq!(
            driver
                .call_tool_error_sync("member_list", arguments)
                .expect("spawned cursor rejection"),
            "validation"
        );
    }

    let ambiguity = driver.request(
        "tools/call",
        json!({"name": "member_list", "arguments": {"space": "Shared"}}),
    );
    assert_eq!(tool_result_code(&ambiguity), Some("ambiguous"));
    assert_eq!(
        ambiguity.pointer("/result/structuredContent/candidates"),
        Some(&json!([
            {"id": "space-alpha", "name": "Shared"},
            {"id": "space-beta", "name": "shared"}
        ]))
    );
    for expected in ["authentication", "authentication"] {
        assert_eq!(
            driver
                .call_tool_error_sync("member_list", json!({"space": SPAWNED_MEMBER_SPACE_ID}),)
                .expect("spawned authorization failure"),
            expected
        );
    }
    for _case in 0..4 {
        assert_eq!(
            driver
                .call_tool_error_sync(
                    "member_get",
                    json!({"space": SPAWNED_MEMBER_SPACE_ID, "member_id": SPAWNED_MEMBER_ID}),
                )
                .expect("spawned malformed member failure"),
            "upstream"
        );
    }
    assert_eq!(
        driver
            .call_tool_error_sync("member_list", json!({"space": SPAWNED_MEMBER_SPACE_ID}),)
            .expect("spawned 5xx failure"),
        "upstream"
    );

    let cancellation_id = driver.next_id;
    driver.next_id += 1;
    driver.process.send(json!({
        "jsonrpc": "2.0",
        "id": cancellation_id,
        "method": "tools/call",
        "params": {
            "name": "member_list",
            "arguments": {"space": SPAWNED_MEMBER_SPACE_ID}
        }
    }));
    fixture.wait_until_accepted(11);
    driver.process.notification(
        "notifications/cancelled",
        json!({"requestId": cancellation_id, "reason": "fixture cancellation"}),
    );

    assert_eq!(
        driver
            .call_tool_error_sync("member_list", json!({"space": SPAWNED_MEMBER_SPACE_ID}),)
            .expect("spawned timeout failure"),
        "upstream"
    );
    let (transcript, output) = driver.finish();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cancelled"),
        "fixed cancellation diagnostic"
    );
    for sensitive in [
        secret,
        "spawned-fixture-http-token",
        "SPAWNED-GLOBAL-NAME-SECRET",
        "SPAWNED-NETWORK-IDENTITY-SECRET",
        "SPAWNED-ICON-SECRET",
        fixture.endpoint.as_str(),
    ] {
        assert!(!transcript.contains(sensitive));
        assert!(!stdout.contains(sensitive));
        assert!(!stderr.contains(sensitive));
    }
    fixture.finish(12);
}

#[test]
#[serial_test::serial]
fn headless_stdio_members_asserts_logical_and_physical_work_ceilings() {
    for exact_get in [false, true] {
        let mut requests = vec![MemberFixtureRequest::json(
            "/v1/spaces",
            &[("limit", "1")],
            startup_space_page(),
        )];
        for page_index in 0..11_usize {
            let offset = page_index * 99;
            let count = if page_index == 10 { 10 } else { 99 };
            let mut spaces = (0..count)
                .map(|row| {
                    let ordinal = offset + row;
                    json!({
                        "id": format!("space-{ordinal:04}"),
                        "name": format!("Other {ordinal:04}"),
                        "object": "space"
                    })
                })
                .collect::<Vec<_>>();
            if page_index == 10 {
                *spaces.last_mut().expect("spawned terminal resolver row") =
                    json!({"id": SPAWNED_MEMBER_SPACE_ID, "name": "Target", "object": "space"});
            }
            let offset_string = offset.to_string();
            let mut query = vec![("limit", "99")];
            if page_index != 0 {
                query.push(("offset", offset_string.as_str()));
            }
            push_spawned_six_attempt_success(
                &mut requests,
                "/v1/spaces",
                &query,
                member_http_page(spaces, offset, 99, 1000),
            );
        }

        let (tool, arguments, final_path, final_query, final_success) = if exact_get {
            (
                "member_get",
                json!({"space": "Target", "member_id": SPAWNED_MEMBER_ID}),
                format!("/v1/spaces/{SPAWNED_MEMBER_SPACE_ID}/members/{SPAWNED_MEMBER_ID}"),
                Vec::new(),
                json!({
                    "member": spawned_member_value(SPAWNED_MEMBER_ID, "viewer", "active")
                }),
            )
        } else {
            (
                "member_list",
                json!({"space": "Target"}),
                format!("/v1/spaces/{SPAWNED_MEMBER_SPACE_ID}/members"),
                vec![("limit", "20")],
                member_http_page(Vec::new(), 0, 20, 0),
            )
        };
        push_spawned_six_attempt_success(&mut requests, final_path, &final_query, final_success);
        assert_eq!(requests.len(), 73, "startup plus 72 physical attempts");

        let fixture = SpawnedMemberFixture::start(requests);
        let mut driver = StdioDriver::start_members_fixture(&fixture.endpoint, 120);
        let result = driver
            .call_tool_sync(tool, arguments)
            .expect("spawned full physical-budget member result");
        if exact_get {
            assert_eq!(result["member"]["id"], SPAWNED_MEMBER_ID);
        } else {
            assert_eq!(result, json!({"items": []}));
        }
        let _ = driver.finish();
        fixture.finish(73);
    }
}

#[test]
#[serial_test::serial]
fn headless_stdio_members_mixed_retries_never_send_a_seventh_attempt() {
    let path = format!("/v1/spaces/{SPAWNED_MEMBER_SPACE_ID}/members");
    let mut requests = vec![MemberFixtureRequest::json(
        "/v1/spaces",
        &[("limit", "1")],
        startup_space_page(),
    )];
    for physical_attempt in 0..6 {
        requests.push(if physical_attempt % 2 == 0 {
            MemberFixtureRequest::status(
                &path,
                &[("limit", "20")],
                "429 Too Many Requests",
                "RateLimit-Reset: 0\r\n",
                json!({"secret": "SPAWNED-RETRY-SECRET"}),
            )
        } else {
            MemberFixtureRequest::status(
                &path,
                &[("limit", "20")],
                "504 Gateway Timeout",
                "",
                json!({"secret": "SPAWNED-RETRY-SECRET"}),
            )
        });
    }
    assert_eq!(requests.len(), 7, "one startup plus six physical attempts");
    let fixture = SpawnedMemberFixture::start(requests);
    let mut driver = StdioDriver::start_members_fixture(&fixture.endpoint, 20);
    assert_eq!(
        driver
            .call_tool_error_sync("member_list", json!({"space": SPAWNED_MEMBER_SPACE_ID}),)
            .expect("spawned mixed retry ceiling"),
        "upstream"
    );
    let (_, output) = driver.finish();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SPAWNED-RETRY-SECRET"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("SPAWNED-RETRY-SECRET"));
    fixture.finish(7);
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_members_minimizes_personal_data() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-members",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let mut driver =
                    StdioDriver::start_with_toolsets(DriverOptions::STANDARD, Some("members"));
                let tools = driver.list_tools_sync().expect("members tools/list");
                assert!(tools.iter().any(|name| name == "member_list"));
                assert!(tools.iter().any(|name| name == "member_get"));
                assert!(tools.iter().any(|name| name == "optional_toolset_status"));

                let status = driver
                    .call_tool_sync("optional_toolset_status", json!({}))
                    .expect("members status");
                assert_eq!(status["configured_toolsets"], json!(["members"]));
                assert_eq!(status["active_toolsets"], json!(["members"]));
                let page = driver
                    .call_tool_sync("member_list", json!({"space": ctx.space_id, "limit": 100}))
                    .expect("spawned member_list");
                let items = page["items"].as_array().expect("spawned members items");
                assert!(!items.is_empty(), "disposable space has an owner member");
                assert!(page.get("next_cursor").is_none());
                for item in items {
                    let wire = item.to_string();
                    for forbidden in ["identity", "global_name", "globalName", "icon"] {
                        assert!(!wire.contains(forbidden));
                    }
                    let exact = driver
                        .call_tool_sync(
                            "member_get",
                            json!({
                                "space": ctx.space_id,
                                "member_id": item["id"]
                            }),
                        )
                        .expect("spawned member_get");
                    assert_eq!(exact["member"], *item);
                }
                let (transcript, output) = driver.finish();
                assert!(!transcript.contains("network-secret"));
                let stderr = String::from_utf8_lossy(&output.stderr);
                for forbidden in ["identity", "global_name", "globalName", "icon"] {
                    assert!(!stderr.contains(forbidden));
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned members suite");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned members suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_disposable_lifecycle_sentinel() {
    run_disposable_stdio_lifecycle_sentinel(DisposableSentinelMode::Success).await;
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_disposable_panic_cleanup_sentinel() {
    run_disposable_stdio_lifecycle_sentinel(DisposableSentinelMode::Panic).await;
}

async fn run_spawned_read_sentinel(options: DriverOptions) {
    let mut driver = StdioDriver::start(options);
    let record = Arc::new(Mutex::new(CaseRecord::default()));
    let captured = record.clone();
    let cleanup = Box::pin(with_test_context(move |ctx| {
        Box::pin(async move {
            let mut evidence = ScenarioEvidence::new(ScenarioId::Documents);
            let result = AssertUnwindSafe(async {
                let name = format!("MCP profile sentinel {}", unique_suffix());
                evidence.sensitive(&name);
                let object = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(name)
                    .ensure_available()
                    .create()
                    .await
                    .map_err(|_| "create profile sentinel fixture".to_owned())?;
                ctx.register_object(&object.id);
                evidence.fixture(&object.id);
                let tools = driver.list_tools().await?;
                if options.profile == "compact" && !tools.iter().any(|name| name == "object_get") {
                    return Err("compact catalog omitted object_get".to_owned());
                }
                if options.read_only && tools.iter().any(|name| name == "object_edit") {
                    return Err("read-only catalog retained object_edit".to_owned());
                }
                driver
                    .call_tool(
                        "object_get",
                        json!({"space": ctx.space_id, "object_id": object.id}),
                    )
                    .await?;
                Ok::<(), String>(())
            })
            .catch_unwind()
            .await;
            let (error, transcript, output) = match result {
                Ok(result) => {
                    let (transcript, output) = driver.finish();
                    (result.err(), transcript, output)
                }
                Err(_) => {
                    let (transcript, output, category) = driver.finish_after_panic();
                    (
                        Some(format!("process_category={category}")),
                        transcript,
                        output,
                    )
                }
            };
            let (request_count, result_count, tool_error_count) = process_metrics(&transcript);
            let stderr = stderr_metrics(&output.stderr);
            let fixture_ids = std::mem::take(&mut evidence.fixture_ids);
            *captured.lock().expect("sentinel case record lock") = CaseRecord {
                error: error.map(|error| evidence.sanitize(&error)),
                scenario: format!("{}_read_sentinel", options.profile),
                fixture_ids,
                protocol: options.metadata(),
                transcript,
                stderr,
                stdout_bytes: output.stdout.len(),
                request_count,
                result_count,
                tool_error_count,
            };
            Ok(())
        })
    }))
    .await;
    let cleanup_status = if cleanup.is_ok() { "success" } else { "failed" };
    let record = record.lock().expect("sentinel case record lock");
    if let Some(error) = &record.error {
        panic!(
            "scenario={} fixtures={:?} {} error={} requests={} results={} tool_errors={} stdout_bytes={} cleanup={}\ntranscript:\n{}\nstderr_metrics={}",
            record.scenario,
            record.fixture_ids,
            record.protocol,
            error,
            record.request_count,
            record.result_count,
            record.tool_error_count,
            record.stdout_bytes,
            cleanup_status,
            record.transcript,
            record.stderr.summary()
        );
    }
    cleanup.expect("cleanup-safe spawned read sentinel");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_stdio_compact_sentinel() {
    run_spawned_read_sentinel(DriverOptions::COMPACT).await;
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_stdio_read_only_sentinel() {
    run_spawned_read_sentinel(DriverOptions::READ_ONLY).await;
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_stdio_preview_sentinel() {
    run_spawned_baseline(ScenarioId::Documents, DriverOptions::PREVIEW).await;
}

#[cfg(test)]
mod keystore_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn temporary_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("any-mcp-{name}-{}", unique_suffix()))
    }

    #[test]
    fn encrypted_file_spec_preserves_modifiers_and_wal_snapshot() {
        let source = temporary_path("source.db");
        std::fs::write(&source, b"main").unwrap();
        std::fs::write(sidecar(&source, "-wal"), b"wal").unwrap();
        let spec = format!("file:path={}:cipher=aes256:hexkey=0011", source.display());
        let (isolated, rebuilt) = TemporaryKeystore::isolate_specification(&spec).unwrap();
        let isolated = isolated.unwrap();
        let rebuilt = rebuilt.unwrap();
        let parsed = FileKeystoreSpec::parse(&rebuilt).unwrap().unwrap();
        assert_eq!(parsed.source(), isolated.path.display().to_string());
        assert_eq!(
            parsed
                .modifiers
                .iter()
                .filter(|(key, _)| key == "path")
                .count(),
            1
        );
        assert!(!rebuilt.contains(&source.display().to_string()));
        assert!(rebuilt.ends_with(":cipher=aes256:hexkey=0011"));
        assert_eq!(std::fs::read(&isolated.path).unwrap(), b"main");
        assert_eq!(
            std::fs::read(sidecar(&isolated.path, "-wal")).unwrap(),
            b"wal"
        );
        remove_sqlite_snapshot(&source);
    }

    #[test]
    fn parser_preserves_windows_drive_and_colon_bearing_paths() {
        for (specification, expected) in [
            (
                r"file:path=C:\Users\example\keys.db:cipher=aes256",
                r"C:\Users\example\keys.db",
            ),
            (
                "file:path=C:/Users/example/keys.db:cipher=aes256",
                "C:/Users/example/keys.db",
            ),
            (
                "sqlite:path=/var/lib/anytype/keys:primary.db:cipher=aes256",
                "/var/lib/anytype/keys:primary.db",
            ),
        ] {
            let parsed = FileKeystoreSpec::parse(specification).unwrap().unwrap();
            assert_eq!(parsed.source(), expected);
            assert_eq!(
                parsed.modifiers.last(),
                Some(&("cipher".to_owned(), "aes256".to_owned()))
            );
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn colon_bearing_source_is_isolated_to_one_snapshot_path() {
        let source = temporary_path("source:primary.db");
        std::fs::write(&source, b"main").unwrap();
        let specification = format!("file:path={}:cipher=aes256", source.display());
        let (isolated, rebuilt) = TemporaryKeystore::isolate_specification(&specification).unwrap();
        let isolated = isolated.unwrap();
        let rebuilt = FileKeystoreSpec::parse(&rebuilt.unwrap()).unwrap().unwrap();
        assert_eq!(rebuilt.source(), isolated.path.display().to_string());
        assert_eq!(
            rebuilt
                .modifiers
                .iter()
                .filter(|(key, _)| key == "path")
                .count(),
            1
        );
        remove_sqlite_snapshot(&source);
    }

    #[test]
    fn duplicate_missing_empty_and_plain_paths_are_rejected() {
        for specification in [
            "file",
            "sqlite",
            "file:cipher=aes256",
            "file:path=",
            "file:path=first.db:path=second.db",
        ] {
            assert!(
                FileKeystoreSpec::parse(specification).is_err(),
                "{specification}"
            );
        }
        assert!(
            TemporaryKeystore::isolate_specification("file")
                .unwrap_err()
                .contains("explicit file:path")
        );
        let (isolated, rebuilt) = TemporaryKeystore::isolate_specification("env").unwrap();
        assert!(isolated.is_none());
        assert!(rebuilt.is_none());
    }

    #[test]
    fn preview_driver_uses_the_namespaced_metadata_contract() {
        let metadata = preview_meta();
        assert_eq!(
            metadata["io.modelcontextprotocol/protocolVersion"],
            "2026-07-28"
        );
        assert!(metadata.get("protocolVersion").is_none());
        assert!(metadata["io.modelcontextprotocol/clientCapabilities"].is_object());
    }

    #[test]
    fn preview_live_catalog_validator_is_exact_and_ordered() {
        let exact = PREVIEW_COMPACT_TOOLS
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>();
        assert!(validate_preview_compact_catalog(&exact).is_ok());
        assert!(validate_preview_compact_catalog(&exact[..3]).is_err());
        let mut reordered = exact.clone();
        reordered.swap(0, 1);
        assert!(validate_preview_compact_catalog(&reordered).is_err());
        let mut extra = exact;
        extra.push("unexpected".to_owned());
        assert!(validate_preview_compact_catalog(&extra).is_err());
    }

    #[test]
    fn child_eof_stderr_is_structurally_classified_before_parent_report() {
        const HTTP_TOKEN: &str = "credential-like-http-token";
        const CIPHER: &str = "cipher-key-material-0011";
        const BODY: &str = "unregistered body and edit fragment";
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        command
            .env("ANY_MCP_PROFILE", "invalid-profile")
            .env("ANYTYPE_KEY_HTTP_TOKEN", HTTP_TOKEN)
            .env_remove("ANY_MCP_PROTOCOL");
        let mut process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(2));
        let panic = std::panic::catch_unwind(AssertUnwindSafe(|| process.read_frame()))
            .expect_err("startup exit becomes a fixed-category panic");
        let panic_text = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("non-string panic");
        assert_eq!(panic_text, "bounded protocol process failed: child_eof");
        for secret in [HTTP_TOKEN, CIPHER, BODY] {
            assert!(!panic_text.contains(secret));
        }

        let mut failure = process
            .take_failure()
            .expect("child EOF retains bounded evidence");
        failure.output.stderr.extend_from_slice(
            format!("\nunknown={HTTP_TOKEN} body={BODY} cipher={CIPHER}\n").as_bytes(),
        );
        let metrics = stderr_metrics(&failure.output.stderr);

        let cleanup_finalizer_ran = AtomicBool::new(false);
        cleanup_finalizer_ran.store(true, Ordering::SeqCst);
        let report = format!(
            "scenario=standard_discovery process_category={} cleanup={} stderr_metrics={}",
            failure.category,
            if cleanup_finalizer_ran.load(Ordering::SeqCst) {
                "success"
            } else {
                "failed"
            },
            metrics.summary()
        );
        assert!(report.contains("process_category=child_eof"));
        assert!(report.contains("cleanup=success"));
        assert!(report.contains("other="));
        for secret in [HTTP_TOKEN, CIPHER, BODY] {
            assert!(!report.contains(secret));
        }
    }
}
