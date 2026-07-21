// any-mcp - bounded, workflow-oriented MCP server for Anytype
// SPDX-License-Identifier: Apache-2.0

//! Individually selectable production-stdio-to-headless acceptance cases.

use std::{
    ffi::OsString,
    future::Future,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anytype::test_util::{unique_suffix, with_test_context};
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
        if options.preview {
            command.env("ANY_MCP_PROTOCOL", "experimental-2026-07-28");
        } else {
            command.env_remove("ANY_MCP_PROTOCOL");
        }
        if let Some(specification) = isolated_specification {
            command.env("ANYTYPE_KEYSTORE", specification);
        }
        let process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(30));
        let mut driver = Self {
            process,
            next_id: 1,
            options,
            _keystore: keystore,
        };
        if options.preview {
            let discovered = driver.request("server/discover", json!({}));
            assert_eq!(discovered["result"]["resultType"], "complete");
            assert_eq!(
                discovered["result"]["supportedVersions"],
                json!(["2026-07-28"])
            );
        } else {
            let initialized = driver.request(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "any-mcp-headless-e2e", "version": "1"}
                }),
            );
            assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
            driver
                .process
                .notification("notifications/initialized", json!({}));
        }
        driver
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

    fn finish(self) -> (String, ProcessOutput) {
        let transcript = self.process.redacted_transcript();
        let output = self.process.finish();
        (transcript, output)
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
        Box::pin(async move {
            let response =
                self.request("tools/call", json!({"name": name, "arguments": arguments}));
            tool_success(name, &response)
        })
    }

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>> {
        Box::pin(async move {
            let response =
                self.request("tools/call", json!({"name": name, "arguments": arguments}));
            response
                .pointer("/result/structuredContent/code")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| response_summary(name, &response))
        })
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        Box::pin(async move {
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
        })
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let response = self.request("resources/list", json!({}));
            response
                .get("result")
                .cloned()
                .ok_or_else(|| response_summary("resources/list", &response))
        })
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let response = self.request("resources/templates/list", json!({}));
            response
                .get("result")
                .cloned()
                .ok_or_else(|| response_summary("resources/templates/list", &response))
        })
    }

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let response = self.request("resources/read", json!({"uri": uri}));
            response
                .get("result")
                .cloned()
                .ok_or_else(|| response_summary("resources/read", &response))
        })
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
            *captured.lock().expect("case record lock") = CaseRecord {
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
            };
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
        #[ignore = "requires source .test-env and an authenticated headless Anytype server"]
        async fn $name() {
            run_spawned_baseline($scenario, DriverOptions::STANDARD).await;
        }
    };
}

spawned_baseline_test!(headless_stdio_standard_discovery, ScenarioId::Discovery);
spawned_baseline_test!(headless_stdio_standard_documents, ScenarioId::Documents);
spawned_baseline_test!(headless_stdio_standard_views, ScenarioId::Views);
spawned_baseline_test!(headless_stdio_standard_mutations, ScenarioId::Mutations);
spawned_baseline_test!(headless_stdio_standard_archive, ScenarioId::Archive);

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
