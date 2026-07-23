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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

#[cfg(feature = "acceptance-harness")]
use any_mcp::body_toolset::BodyAcceptanceDirect;
#[cfg(feature = "acceptance-harness")]
use any_mcp::collection_member_toolset::{
    AcceptanceMetricsSnapshot, AcceptanceMutationMode, ViewsWriteAcceptanceDirect,
};
#[cfg(feature = "acceptance-harness")]
use anytype::keystore::KeyStore;
#[cfg(feature = "acceptance-harness")]
use anytype::test_util::retry_definitive_rate_limit;
#[cfg(feature = "acceptance-harness")]
use anytype::test_util::{DisposableCallbackStage, disposable_callback_error};
use anytype::{
    chats::MessageContent,
    error::AnytypeError,
    objects::Icon,
    prelude::{AnytypeClient, ClientConfig, Color, Tag},
    test_util::{
        DisposableRun, TestContext, TestError, TestResult, unique_suffix,
        with_disposable_space_context, with_test_context,
    },
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod support;

#[cfg(feature = "acceptance-harness")]
use support::live_scenario::BodyDriverMetrics;
#[cfg(feature = "acceptance-harness")]
use support::live_scenario::{
    BODY_DIAGNOSTIC_SECRET, BodyReadOnlyEvidence, BodyScenarioEvidence, BodyScenarioFailure,
    run_body_read_only_scenario, run_body_scenario,
};
use support::{
    live_scenario::{
        ChatsRegistryEvidence, ChatsRegistryFixture, McpDriver, ScenarioEvidence, ScenarioId,
        ToolErrorEvidence, run_chats_registry_scenario, run_live_scenario_on_large_stack,
        run_representative_layout_scenario, run_scenario, validate_live_ownership,
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
    const PREVIEW_STANDARD: Self = Self {
        profile: "standard",
        read_only: false,
        preview: true,
    };
    #[cfg(feature = "acceptance-harness")]
    const PREVIEW_READ_ONLY: Self = Self {
        profile: "standard",
        read_only: true,
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

#[cfg(feature = "acceptance-harness")]
const BODY_TOOL_NAMES: &[&str] = &[
    "body_block_create",
    "body_block_delete",
    "body_block_list",
    "body_block_move",
    "body_block_update",
    "rich_page_create",
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
    #[cfg(feature = "acceptance-harness")]
    body_tool_error_frames: Vec<Value>,
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

fn configure_stdio_command(command: &mut Command, options: DriverOptions, toolsets: Option<&str>) {
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
}

impl StdioDriver {
    fn start(options: DriverOptions) -> Self {
        Self::start_with_toolsets(options, None)
    }

    fn start_with_toolsets(options: DriverOptions, toolsets: Option<&str>) -> Self {
        let mut driver = Self::spawn_with_toolsets_uninitialized(options, toolsets);
        driver.initialize();
        driver
    }

    fn spawn_with_toolsets_uninitialized(options: DriverOptions, toolsets: Option<&str>) -> Self {
        let (keystore, isolated_specification) = TemporaryKeystore::isolate_environment()
            .unwrap_or_else(|error| panic!("isolate live-test keystore: {error}"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        configure_stdio_command(&mut command, options, toolsets);
        if let Some(specification) = isolated_specification {
            command.env("ANYTYPE_KEYSTORE", specification);
        }
        Self::spawn(command, options, keystore)
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
            #[cfg(feature = "acceptance-harness")]
            body_tool_error_frames: Vec::new(),
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

    #[cfg(feature = "acceptance-harness")]
    fn request_pair(
        &mut self,
        method: &str,
        mut first_params: Value,
        mut second_params: Value,
    ) -> [Value; 2] {
        if self.options.preview {
            for params in [&mut first_params, &mut second_params] {
                params
                    .as_object_mut()
                    .expect("preview params object")
                    .insert("_meta".to_owned(), preview_meta());
            }
        }
        let first_id = self.next_id;
        let second_id = first_id + 1;
        self.next_id += 2;
        for (id, params) in [(first_id, first_params), (second_id, second_params)] {
            self.process.send(json!({
                "jsonrpc":"2.0",
                "id":id,
                "method":method,
                "params":params
            }));
        }
        let first_response = self.process.read_frame();
        let second_response = self.process.read_frame();
        self.process.record_response(&first_response);
        self.process.record_response(&second_response);
        let response_id = |response: &Value| response["id"].as_u64();
        match (response_id(&first_response), response_id(&second_response)) {
            (Some(id), Some(other)) if id == first_id && other == second_id => {
                [first_response, second_response]
            }
            (Some(id), Some(other)) if id == second_id && other == first_id => {
                [second_response, first_response]
            }
            _ => panic!("paired response ids must match the two requests"),
        }
    }

    fn call_tool_sync(&mut self, name: &'static str, arguments: Value) -> Result<Value, String> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        tool_success(name, &response)
    }

    fn call_tool_error_sync(
        &mut self,
        name: &'static str,
        arguments: Value,
    ) -> Result<ToolErrorEvidence, String> {
        let response = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        let result = response
            .get("result")
            .ok_or_else(|| response_summary(name, &response))?;
        let evidence = ToolErrorEvidence::from_result(result, self.options.preview)?;
        #[cfg(feature = "acceptance-harness")]
        if name == "body_block_list" && evidence.code() == "conflict" {
            self.body_tool_error_frames.push(response);
        }
        Ok(evidence)
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

    #[cfg(feature = "acceptance-harness")]
    fn body_tool_descriptors_sync(&mut self) -> Result<Vec<Value>, String> {
        let response = self.request("tools/list", json!({}));
        Ok(response["result"]["tools"]
            .as_array()
            .ok_or_else(|| "tools/list omitted descriptors".to_owned())?
            .iter()
            .filter(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|name| BODY_TOOL_NAMES.contains(&name))
            })
            .cloned()
            .collect::<Vec<_>>())
    }

    #[cfg(feature = "acceptance-harness")]
    fn raw_body_parity_frames(&mut self, space_id: &str, object_id: &str) -> [Value; 2] {
        let success = self.request(
            "tools/call",
            json!({
                "name":"body_block_list",
                "arguments":{"space":space_id,"object_id":object_id,"limit":8}
            }),
        );
        let error = self.request(
            "tools/call",
            json!({
                "name":"body_block_list",
                "arguments":{"space":null,"object_id":object_id,"limit":8}
            }),
        );
        [success, error]
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
    ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>> {
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
    ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>> {
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

#[cfg(feature = "acceptance-harness")]
struct DirectBodyDriver {
    driver: BodyAcceptanceDirect,
}

#[cfg(feature = "acceptance-harness")]
impl McpDriver for DirectBodyDriver {
    fn body_acceptance_metrics(&self) -> Option<BodyDriverMetrics> {
        let metrics = self.driver.metrics();
        Some(BodyDriverMetrics {
            page_create_polls: metrics.page_create_polls,
            show_attempts: metrics.show_attempts,
            foreground_close_attempts: metrics.foreground_close_attempts,
            foreground_close_confirmed: metrics.foreground_close_confirmed,
            fallback_close_attempts: metrics.fallback_close_attempts,
            fallback_close_confirmed: metrics.fallback_close_confirmed,
            write_polls: metrics.write_polls,
            show_limit_rejections: metrics.show_limit_rejections,
            non_show_limit_rejections: metrics.non_show_limit_rejections,
            close_limit_rejections: metrics.close_limit_rejections,
            mutation_limit_rejections: metrics.mutation_limit_rejections,
        })
    }

    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let result = self.driver.call(name, arguments).await;
            if result.is_error == Some(true) {
                let code = result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value["code"].as_str())
                    .unwrap_or("missing");
                return Err(format!("{name} returned tool error {code}"));
            }
            result
                .structured_content
                .ok_or_else(|| format!("{name} success omitted structured content"))
        })
    }

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>> {
        Box::pin(async move {
            let result = self.driver.call(name, arguments).await;
            let value = serde_json::to_value(result)
                .map_err(|_| format!("{name} error result was not serializable"))?;
            ToolErrorEvidence::from_result(&value, false)
        })
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        Box::pin(std::future::ready(Ok(self.driver.tool_names())))
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "direct body scenario does not use resources/list".to_owned(),
        )))
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "direct body scenario does not use resources/templates/list".to_owned(),
        )))
    }

    fn read_resource<'a>(
        &'a mut self,
        _uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "direct body scenario does not use resources/read".to_owned(),
        )))
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

struct PropertyScopedTagReadback {
    space_id: String,
    property_id: String,
    tag: Tag,
}

async fn property_scoped_tag_readback(
    client: &AnytypeClient,
    space_id: &str,
    property_id: &str,
    tag_id: &str,
) -> TestResult<PropertyScopedTagReadback> {
    let page = client
        .tags(space_id, property_id)
        .limit(1_000)
        .offset(0)
        .list()
        .await?;
    if page.pagination.total != page.items.len() || page.items.len() > 1_000 {
        return Err(TestError::Assertion {
            message: "property-scoped tag readback was incomplete".to_owned(),
        });
    }
    let mut matches = page.items.iter().filter(|tag| tag.id == tag_id);
    let Some(tag) = matches.next().cloned() else {
        return Err(TestError::Assertion {
            message: "property-scoped tag readback omitted the exact tag".to_owned(),
        });
    };
    if matches.next().is_some() {
        return Err(TestError::Assertion {
            message: "property-scoped tag readback duplicated the exact tag".to_owned(),
        });
    }
    Ok(PropertyScopedTagReadback {
        space_id: space_id.to_owned(),
        property_id: property_id.to_owned(),
        tag,
    })
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
    stack_overflow: usize,
    panic: usize,
    fatal: usize,
    other: usize,
    invalid_utf8: bool,
}

impl StderrMetrics {
    fn summary(&self) -> String {
        format!(
            "bytes={} lines={} runtime_ready={} operation_success={} operation_non_success={} stack_overflow={} panic={} fatal={} other={} invalid_utf8={}",
            self.bytes,
            self.lines,
            self.runtime_ready,
            self.operation_success,
            self.operation_non_success,
            self.stack_overflow,
            self.panic,
            self.fatal,
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
        if contains_bytes(line, b"stack overflow") {
            metrics.stack_overflow += 1;
        } else if contains_bytes(line, b"panicked at") {
            metrics.panic += 1;
        } else if contains_bytes(line, b"fatal runtime error") {
            metrics.fatal += 1;
        } else if contains_bytes(line, b"authenticated Anytype runtime ready") {
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

#[cfg(feature = "acceptance-harness")]
fn require_body_diagnostics(
    stderr: &[u8],
    secret: &[u8],
    expect_operations: bool,
) -> TestResult<()> {
    let metrics = stderr_metrics(stderr);
    let categorized =
        metrics.runtime_ready + metrics.operation_success + metrics.operation_non_success;
    if stderr.len() > 524_288
        || contains_bytes(stderr, secret)
        || metrics.invalid_utf8
        || metrics.runtime_ready != 1
        || metrics.stack_overflow != 0
        || metrics.panic != 0
        || metrics.fatal != 0
        || metrics.other != 0
        || metrics.lines != categorized
        || (expect_operations && metrics.operation_success == 0)
        || (!expect_operations
            && (metrics.operation_success != 0 || metrics.operation_non_success != 0))
    {
        return Err(sentinel_assertion(
            "body child diagnostics violated fixed-category/redaction bounds",
        ));
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn inspect_reviewed_body_server_log(secrets: &[&[u8]]) -> TestResult<()> {
    let marker = std::env::var("ANY_MCP_HEADLESS_LOG_RUN_MARKER").map_err(|_| {
        sentinel_assertion("reviewed headless server-log marker was not configured")
    })?;
    let service = std::env::var("ANYTYPE_KEYSTORE_SERVICE")
        .map_err(|_| sentinel_assertion("reviewed log keystore service was absent"))?;
    let specification = std::env::var("ANYTYPE_KEYSTORE")
        .map_err(|_| sentinel_assertion("reviewed log keystore was absent"))?;
    let keystore = KeyStore::new(service, &specification)
        .map_err(|_| sentinel_assertion("reviewed log keystore could not be opened"))?;
    inspect_reviewed_body_server_log_at(
        std::env::var_os("ANY_MCP_HEADLESS_REDACTED_LOG_FILE"),
        Some(&marker),
        secrets,
        |log| {
            keystore
                .configured_credentials_absent_from(log)
                .unwrap_or(false)
        },
    )
}

fn inspect_reviewed_body_server_log_at(
    path: Option<OsString>,
    marker: Option<&str>,
    secrets: &[&[u8]],
    credentials_absent: impl FnOnce(&[u8]) -> bool,
) -> TestResult<()> {
    let Some(path) = path else {
        return Err(sentinel_assertion(
            "reviewed headless server-log evidence was not configured",
        ));
    };
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(sentinel_assertion(
            "reviewed headless server-log path was not absolute",
        ));
    }
    let marker = marker
        .filter(|value| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| sentinel_assertion("reviewed headless server-log marker was invalid"))?;
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|_| sentinel_assertion("reviewed headless server log metadata was unreadable"))?;
    if !metadata.file_type().is_file() {
        return Err(sentinel_assertion(
            "reviewed headless server log was not a regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // SAFETY: `geteuid` has no preconditions and does not dereference
        // pointers or mutate process state.
        let effective_uid = unsafe { libc::geteuid() };
        if metadata.permissions().mode() & 0o777 != 0o600 || metadata.uid() != effective_uid {
            return Err(sentinel_assertion(
                "reviewed headless server log ownership or permissions were unsafe",
            ));
        }
    }
    let log = std::fs::read(path)
        .map_err(|_| sentinel_assertion("reviewed headless server log was unreadable"))?;
    if log.is_empty()
        || log.len() > 524_288
        || std::str::from_utf8(&log).is_err()
        || secrets
            .iter()
            .any(|secret| !secret.is_empty() && contains_bytes(&log, secret))
        || !credentials_absent(&log)
    {
        return Err(sentinel_assertion(
            "reviewed headless server log violated size/UTF-8/redaction bounds",
        ));
    }
    let marker_line = format!("any-mcp-run-marker={marker}");
    let mut marker_count = 0usize;
    let mut event_count = 0usize;
    for line in std::str::from_utf8(&log)
        .map_err(|_| sentinel_assertion("reviewed headless server log was not UTF-8"))?
        .lines()
    {
        if line.is_empty() {
            continue;
        } else if line == marker_line {
            marker_count = marker_count.saturating_add(1);
        } else if reviewed_server_event_line(line) {
            event_count = event_count.saturating_add(1);
        } else {
            return Err(sentinel_assertion(
                "reviewed headless server log contained a non-allowlisted line",
            ));
        }
    }
    if marker_count != 1 || event_count == 0 {
        return Err(sentinel_assertion(
            "reviewed headless server log lacked current-run provenance or events",
        ));
    }
    Ok(())
}

fn reviewed_server_event_line(line: &str) -> bool {
    const KEYS: &[&str] = &[
        "timestamp",
        "severity",
        "component",
        "category",
        "fixture_id",
    ];
    let Ok(Value::Object(event)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    event.len() >= 2
        && event.keys().all(|key| KEYS.contains(&key.as_str()))
        && event
            .get("severity")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty() && value.len() <= 32)
        && ["component", "category"].iter().any(|key| {
            event
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty() && value.len() <= 128)
        })
        && event.values().all(|value| {
            value
                .as_str()
                .is_some_and(|value| !value.is_empty() && value.len() <= 256)
        })
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
    spawn_disposable_driver(ctx, cleanup_record, DriverOptions::STANDARD, None)
}

fn spawn_disposable_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    options: DriverOptions,
    toolsets: Option<&str>,
) -> TestResult<Arc<Mutex<Option<StdioDriver>>>> {
    let child_environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| TestError::Assertion {
            message: "disposable callback omitted its child environment".to_owned(),
        })?
        .clone();
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
    child_environment.configure(&mut command)?;
    // Only strict, non-secret MCP selectors are overlaid after the disposable
    // environment clears ambient state and installs environment credentials.
    configure_stdio_command(&mut command, options, toolsets);
    ctx.spawn_owned_child(move || {
        let driver = Arc::new(Mutex::new(Some(StdioDriver::spawn(command, options, None))));
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

#[cfg(feature = "acceptance-harness")]
fn spawn_disposable_views_write_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    mode: &str,
) -> TestResult<SpawnedViewsWriteDriver> {
    let child_environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| TestError::Assertion {
            message: "disposable callback omitted its child environment".to_owned(),
        })?
        .clone();
    let metrics_path =
        std::env::temp_dir().join(format!("any-mcp-views-write-metrics-{}", unique_suffix()));
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-views-write-acceptance"));
    command.arg(&metrics_path).arg(mode);
    child_environment.configure(&mut command)?;
    let options = if mode.starts_with("preview-") {
        DriverOptions::PREVIEW
    } else {
        DriverOptions::STANDARD
    };
    let cleanup_path = metrics_path.clone();
    let driver = ctx.spawn_owned_child(move || {
        let driver = Arc::new(Mutex::new(Some(StdioDriver::spawn(command, options, None))));
        let stopped = Arc::clone(&driver);
        (driver, move || {
            *cleanup_record.lock().expect("child cleanup record lock") =
                ChildCleanupRecord::Attempted;
            let result = lock_driver(&stopped)
                .take()
                .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
            let _ = std::fs::remove_file(&cleanup_path);
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
                        message: "registered views-write stdio child did not stop cleanly"
                            .to_owned(),
                    })
                }
            }
        })
    })?;
    Ok(SpawnedViewsWriteDriver {
        driver,
        metrics_path,
    })
}

#[cfg(feature = "acceptance-harness")]
struct SpawnedViewsWriteDriver {
    driver: Arc<Mutex<Option<StdioDriver>>>,
    metrics_path: PathBuf,
}

#[cfg(feature = "acceptance-harness")]
impl SpawnedViewsWriteDriver {
    fn offline_classification(mode: &str) -> TestResult<Self> {
        let metrics_path = std::env::temp_dir().join(format!(
            "any-mcp-views-write-offline-metrics-{}",
            unique_suffix()
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-views-write-acceptance"));
        command.env_clear().arg(&metrics_path).arg(mode);
        #[cfg(windows)]
        if let Some(system_root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", system_root);
        }
        let options = if mode.starts_with("preview-") {
            DriverOptions::PREVIEW
        } else {
            DriverOptions::STANDARD
        };
        let driver = Arc::new(Mutex::new(Some(StdioDriver::spawn(command, options, None))));
        let value = Self {
            driver,
            metrics_path,
        };
        value.initialize()?;
        Ok(value)
    }

    fn initialize(&self) -> TestResult<()> {
        let mut driver = lock_driver(&self.driver);
        let initialized = std::panic::catch_unwind(AssertUnwindSafe(|| {
            driver
                .as_mut()
                .ok_or_else(|| TestError::Assertion {
                    message: "registered views-write child disappeared".to_owned(),
                })?
                .initialize();
            Ok(())
        }));
        match initialized {
            Ok(result) => result,
            Err(_) => {
                if let Some(driver) = driver.take() {
                    let (_, output, category) = driver.finish_after_panic();
                    eprintln!(
                        "views-write acceptance child initialization failed: {category} {}",
                        stderr_metrics(&output.stderr).summary()
                    );
                }
                Err(TestError::Assertion {
                    message: "views-write child initialization failed".to_owned(),
                })
            }
        }
    }

    fn call(&self, name: &'static str, arguments: Value) -> TestResult<AcceptanceCall> {
        let mut driver = lock_driver(&self.driver);
        let response = std::panic::catch_unwind(AssertUnwindSafe(|| {
            driver
                .as_mut()
                .ok_or_else(|| TestError::Assertion {
                    message: "registered views-write child disappeared".to_owned(),
                })
                .map(|driver| {
                    driver.request("tools/call", json!({"name":name,"arguments":arguments}))
                })
        }));
        let response = match response {
            Ok(result) => result?,
            Err(_) => {
                if let Some(driver) = driver.take() {
                    let (_, output, category) = driver.finish_after_panic();
                    eprintln!(
                        "views-write acceptance child {category} during {name}: {}",
                        stderr_metrics(&output.stderr).summary()
                    );
                }
                return Err(TestError::Assertion {
                    message: "views-write child call failed".to_owned(),
                });
            }
        };
        acceptance_call_from_response(&response)
    }

    fn call_pair(
        &self,
        name: &'static str,
        first: Value,
        second: Value,
    ) -> TestResult<[AcceptanceCall; 2]> {
        let mut driver = lock_driver(&self.driver);
        let responses = std::panic::catch_unwind(AssertUnwindSafe(|| {
            driver
                .as_mut()
                .ok_or_else(|| TestError::Assertion {
                    message: "registered views-write child disappeared".to_owned(),
                })
                .map(|driver| {
                    driver.request_pair(
                        "tools/call",
                        json!({"name":name,"arguments":first}),
                        json!({"name":name,"arguments":second}),
                    )
                })
        }));
        let responses = match responses {
            Ok(result) => result?,
            Err(_) => {
                if let Some(driver) = driver.take() {
                    let (_, output, category) = driver.finish_after_panic();
                    eprintln!(
                        "views-write acceptance child {category} during paired {name}: {}",
                        stderr_metrics(&output.stderr).summary()
                    );
                }
                return Err(TestError::Assertion {
                    message: "views-write child paired call failed".to_owned(),
                });
            }
        };
        Ok([
            acceptance_call_from_response(&responses[0])?,
            acceptance_call_from_response(&responses[1])?,
        ])
    }

    fn metrics(&self) -> TestResult<AcceptanceMetricsSnapshot> {
        let encoded =
            std::fs::read_to_string(&self.metrics_path).map_err(|_| TestError::Assertion {
                message: "read views-write child metrics".to_owned(),
            })?;
        let line = encoded
            .lines()
            .next_back()
            .ok_or_else(|| TestError::Assertion {
                message: "views-write child metrics are empty".to_owned(),
            })?;
        serde_json::from_str(line).map_err(|_| TestError::Assertion {
            message: "decode views-write child metrics".to_owned(),
        })
    }

    fn finish(&self) -> TestResult<()> {
        if let Some(driver) = lock_driver(&self.driver).take() {
            driver.try_finish().map_err(|_| TestError::Assertion {
                message: "views-write child did not stop cleanly".to_owned(),
            })?;
        }
        let _ = std::fs::remove_file(&self.metrics_path);
        Ok(())
    }
}

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Debug, PartialEq, Eq)]
struct AcceptanceCall {
    is_error: bool,
    structured: Value,
}

#[cfg(feature = "acceptance-harness")]
fn acceptance_call_from_response(response: &Value) -> TestResult<AcceptanceCall> {
    let result = response.get("result").ok_or_else(|| TestError::Assertion {
        message: "views-write child omitted tool result".to_owned(),
    })?;
    Ok(AcceptanceCall {
        is_error: result["isError"].as_bool().unwrap_or(false),
        structured: result["structuredContent"].clone(),
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

fn chats_process_failure(
    label: &str,
    stage: &str,
    driver: StdioDriver,
) -> anytype::test_util::TestError {
    let (transcript, output, category) = driver.finish_after_panic();
    let stderr = stderr_metrics(&output.stderr);
    let (requests, results, tool_errors) = process_metrics(&transcript);
    eprintln!(
        "spawned chats registry process failed: transport={label} stage={stage} category={category} requests={requests} results={results} tool_errors={tool_errors} stderr={}",
        stderr.summary()
    );
    TestError::Assertion {
        message: "spawned chats registry process failed".to_owned(),
    }
}

async fn run_spawned_chats_registry_transport(
    label: &str,
    options: DriverOptions,
    fixture: ChatsRegistryFixture<'_>,
) -> Result<ChatsRegistryEvidence, TestError> {
    let mut driver = StdioDriver::spawn_with_toolsets_uninitialized(options, Some("chats"));
    if std::panic::catch_unwind(AssertUnwindSafe(|| driver.initialize())).is_err() {
        return Err(chats_process_failure(label, "initialize", driver));
    }
    let result = AssertUnwindSafe(run_chats_registry_scenario(&mut driver, fixture))
        .catch_unwind()
        .await;
    match result {
        Ok(Ok(evidence)) => {
            let (transcript, output) = driver.finish();
            let stderr = stderr_metrics(&output.stderr);
            if stderr.stack_overflow != 0 || stderr.panic != 0 || stderr.fatal != 0 {
                eprintln!(
                    "spawned chats registry emitted fatal diagnostics: transport={label} stderr={}",
                    stderr.summary()
                );
                return Err(TestError::Assertion {
                    message: "spawned chats registry emitted fatal diagnostics".to_owned(),
                });
            }
            if transcript.contains("private-content-sentinel") {
                return Err(TestError::Assertion {
                    message: "spawned chats registry transcript exposed content".to_owned(),
                });
            }
            Ok(evidence)
        }
        Ok(Err(message)) => {
            let _ = driver.finish();
            eprintln!("spawned chats registry scenario failed: transport={label} stage={message}");
            Err(TestError::Assertion { message })
        }
        Err(_) => Err(chats_process_failure(label, "scenario", driver)),
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_chats_registry_runs_stable_and_preview_workflows() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-chats-registry",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let suffix = unique_suffix();
                let query = format!("mcpstdiochats{suffix}");
                let chat = ctx
                    .client
                    .chats()
                    .in_space(&ctx.space_id)
                    .create(
                        format!("MCP stdio chats registry {suffix}"),
                        Icon::Emoji {
                            emoji: "💬".to_owned(),
                        },
                    )
                    .create()
                    .await?;
                ctx.register_object(&chat.id);
                let seed_id = ctx
                    .client
                    .chats()
                    .in_space(&ctx.space_id)
                    .add_message(
                        &chat.id,
                        MessageContent::new().text(format!("{query} cleanup-owned seed")),
                    )
                    .send()
                    .await?;
                ctx.register_chat_message(&chat.id, &seed_id)?;

                for (label, options) in [
                    ("stable", DriverOptions::COMPACT),
                    ("preview", DriverOptions::PREVIEW),
                ] {
                    let add_text = format!("{label} chats registry {suffix}");
                    let idempotency_key = format!("{label}-chats-registry-{suffix}");
                    let evidence = Box::pin(run_spawned_chats_registry_transport(
                        label,
                        options,
                        ChatsRegistryFixture {
                            space_id: &ctx.space_id,
                            chat_id: &chat.id,
                            seed_message_id: &seed_id,
                            search_query: &query,
                            add_text: &add_text,
                            idempotency_key: &idempotency_key,
                        },
                    ))
                    .await?;
                    assert_eq!(evidence.chat_id, chat.id);
                    assert_eq!(evidence.seed_message_id, seed_id);
                    assert!(evidence.deleted);
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned chats registry suite");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned chats registry suite skipped before callback: {reason:?}");
        }
    }
}

fn layouts_process_failure(
    label: &str,
    stage: &str,
    driver: StdioDriver,
) -> anytype::test_util::TestError {
    let (transcript, output, category) = driver.finish_after_panic();
    let stderr = stderr_metrics(&output.stderr);
    let (requests, results, tool_errors) = process_metrics(&transcript);
    eprintln!(
        "spawned representative-layout process failed: transport={label} stage={stage} category={category} requests={requests} results={results} tool_errors={tool_errors} stderr={}",
        stderr.summary()
    );
    TestError::Assertion {
        message: "spawned representative-layout process failed".to_owned(),
    }
}

fn take_registered_layout_driver(
    driver: &Arc<Mutex<Option<StdioDriver>>>,
) -> TestResult<StdioDriver> {
    lock_driver(driver)
        .take()
        .ok_or_else(|| TestError::Assertion {
            message: "registered representative-layout child disappeared".to_owned(),
        })
}

async fn run_spawned_layout_transport(
    label: &str,
    options: DriverOptions,
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
) -> TestResult<()> {
    let driver = spawn_disposable_driver(ctx, cleanup_record, options, Some("views-write"))?;
    if std::panic::catch_unwind(AssertUnwindSafe(|| {
        lock_driver(&driver)
            .as_mut()
            .expect("registered representative-layout child remains owned")
            .initialize();
    }))
    .is_err()
    {
        let driver = take_registered_layout_driver(&driver)?;
        return Err(layouts_process_failure(label, "initialize", driver));
    }
    let mut owned = OwnedStdioDriver {
        driver: Arc::clone(&driver),
    };
    let result = AssertUnwindSafe(run_representative_layout_scenario(&mut owned, ctx))
        .catch_unwind()
        .await;
    drop(owned);
    let driver = take_registered_layout_driver(&driver)?;
    match result {
        Ok(Ok(evidence)) => {
            if evidence.member_ids.len() != 3 || evidence.kanban_view_id == evidence.grid_view_id {
                let _ = driver.try_finish();
                return Err(TestError::Assertion {
                    message: "spawned representative-layout evidence mismatch".to_owned(),
                });
            }
            let (transcript, output) = driver.try_finish().map_err(|_| TestError::Assertion {
                message: "registered representative-layout child did not stop cleanly".to_owned(),
            })?;
            let stderr = stderr_metrics(&output.stderr);
            if stderr.stack_overflow != 0 || stderr.panic != 0 || stderr.fatal != 0 {
                eprintln!(
                    "spawned representative-layout emitted fatal diagnostics: transport={label} stderr={}",
                    stderr.summary()
                );
                return Err(TestError::Assertion {
                    message: "spawned representative-layout emitted fatal diagnostics".to_owned(),
                });
            }
            if transcript.contains("MCP representative layout") {
                return Err(TestError::Assertion {
                    message: "spawned representative-layout transcript exposed fixture text"
                        .to_owned(),
                });
            }
            Ok(())
        }
        Ok(Err(_)) => {
            let _ = driver.try_finish();
            eprintln!("spawned representative-layout scenario failed: transport={label}");
            Err(TestError::Assertion {
                message: "spawned representative-layout scenario failed".to_owned(),
            })
        }
        Err(_) => Err(layouts_process_failure(label, "scenario", driver)),
    }
}

#[test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
fn headless_stdio_ordinary_tools_cover_representative_layouts() {
    run_live_scenario_on_large_stack("stdio-representative-layouts", || async {
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&callback_ran);
        let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
        let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
        let stable_callback_cleanup = Arc::clone(&stable_cleanup);
        let preview_callback_cleanup = Arc::clone(&preview_cleanup);
        let outcome = Box::pin(with_disposable_space_context(
            "any-mcp-stdio-layouts",
            move |ctx| {
                callback_flag.store(true, Ordering::SeqCst);
                Box::pin(async move {
                    for (label, options, cleanup_record) in [
                        ("stable", DriverOptions::STANDARD, stable_callback_cleanup),
                        (
                            "preview",
                            DriverOptions::PREVIEW_STANDARD,
                            preview_callback_cleanup,
                        ),
                    ] {
                        Box::pin(run_spawned_layout_transport(
                            label,
                            options,
                            ctx.as_ref(),
                            cleanup_record,
                        ))
                        .await?;
                    }
                    Ok(())
                })
            },
        ))
        .await
        .expect("cleanup-safe spawned representative-layout suite");
        match outcome {
            DisposableRun::Completed(()) => {
                assert!(callback_ran.load(Ordering::SeqCst));
                assert_eq!(
                    *stable_cleanup.lock().expect("stable child cleanup record"),
                    ChildCleanupRecord::Stopped
                );
                assert_eq!(
                    *preview_cleanup
                        .lock()
                        .expect("preview child cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            DisposableRun::Skipped(reason) => {
                assert!(!callback_ran.load(Ordering::SeqCst));
                assert_eq!(
                    *stable_cleanup.lock().expect("stable child cleanup record"),
                    ChildCleanupRecord::NotRun
                );
                assert_eq!(
                    *preview_cleanup
                        .lock()
                        .expect("preview child cleanup record"),
                    ChildCleanupRecord::NotRun
                );
                eprintln!(
                    "spawned representative-layout suite skipped before callback: {reason:?}"
                );
            }
        }
    });
}

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Copy, Debug)]
enum ViewsWriteTransport {
    Direct,
    Stable,
    Preview,
}

#[cfg(feature = "acceptance-harness")]
enum ViewsWriteDriver {
    Direct(Box<ViewsWriteAcceptanceDirect>),
    Spawned(Box<SpawnedViewsWriteDriver>),
}

#[cfg(feature = "acceptance-harness")]
impl ViewsWriteDriver {
    fn call(
        &mut self,
        name: &'static str,
        arguments: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TestResult<AcceptanceCall>> + Send + '_>>
    {
        Box::pin(async move {
            match self {
                Self::Direct(driver) => {
                    let result = driver.call(name, arguments).await;
                    Ok(AcceptanceCall {
                        is_error: result.is_error.unwrap_or(false),
                        structured: result.structured_content.unwrap_or(Value::Null),
                    })
                }
                Self::Spawned(driver) => driver.call(name, arguments),
            }
        })
    }

    fn metrics(&self) -> TestResult<AcceptanceMetricsSnapshot> {
        match self {
            Self::Direct(driver) => Ok(driver.metrics()),
            Self::Spawned(driver) => driver.metrics(),
        }
    }

    fn call_pair(
        &mut self,
        name: &'static str,
        first: Value,
        second: Value,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = TestResult<[AcceptanceCall; 2]>> + Send + '_>,
    > {
        Box::pin(async move {
            match self {
                Self::Direct(driver) => {
                    Ok(driver
                        .call_pair(name, first, second)
                        .await
                        .map(|result| AcceptanceCall {
                            is_error: result.is_error.unwrap_or(false),
                            structured: result.structured_content.unwrap_or(Value::Null),
                        }))
                }
                Self::Spawned(driver) => driver.call_pair(name, first, second),
            }
        })
    }

    fn finish(&self) -> TestResult<()> {
        if let Self::Spawned(driver) = self {
            driver.finish()?;
        }
        Ok(())
    }
}

#[cfg(feature = "acceptance-harness")]
fn acceptance_mode_name(
    transport: ViewsWriteTransport,
    mode: AcceptanceMutationMode,
    read_only: bool,
) -> TestResult<String> {
    let protocol = match transport {
        ViewsWriteTransport::Stable => "stable",
        ViewsWriteTransport::Preview => "preview",
        ViewsWriteTransport::Direct => {
            return Err(TestError::Assertion {
                message: "direct acceptance has no process mode".to_owned(),
            });
        }
    };
    let stage = if read_only {
        "read-only"
    } else {
        match mode {
            AcceptanceMutationMode::Normal => "normal",
            AcceptanceMutationMode::CancelAddBeforeMark => "add-before",
            AcceptanceMutationMode::CancelAddAfterMark => "add-after",
            AcceptanceMutationMode::CancelRemoveBeforeMark => "remove-before",
            AcceptanceMutationMode::CancelRemoveAfterMark => "remove-after",
            AcceptanceMutationMode::ClassifyAdd403 => "classify-403",
            AcceptanceMutationMode::ConcurrentAdd => "concurrent-add",
        }
    };
    Ok(format!("{protocol}-{stage}"))
}

#[cfg(feature = "acceptance-harness")]
fn views_write_driver(
    ctx: &TestContext,
    transport: ViewsWriteTransport,
    mode: AcceptanceMutationMode,
    read_only: bool,
) -> TestResult<ViewsWriteDriver> {
    match transport {
        ViewsWriteTransport::Direct => {
            ViewsWriteAcceptanceDirect::new(ctx.client.clone(), read_only, mode)
                .map(Box::new)
                .map(ViewsWriteDriver::Direct)
                .map_err(|_| TestError::Assertion {
                    message: "construct direct views-write acceptance driver".to_owned(),
                })
        }
        ViewsWriteTransport::Stable | ViewsWriteTransport::Preview => {
            let mode_name = acceptance_mode_name(transport, mode, read_only)?;
            let cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
            let driver = spawn_disposable_views_write_driver(ctx, cleanup, &mode_name)?;
            driver.initialize()?;
            Ok(ViewsWriteDriver::Spawned(Box::new(driver)))
        }
    }
}

#[cfg(feature = "acceptance-harness")]
fn metrics_delta(
    before: AcceptanceMetricsSnapshot,
    after: AcceptanceMetricsSnapshot,
) -> AcceptanceMetricsSnapshot {
    AcceptanceMetricsSnapshot {
        http_logical: after.http_logical - before.http_logical,
        http_physical: after.http_physical - before.http_physical,
        observer_attempts: after.observer_attempts - before.observer_attempts,
        query_rounds: after.query_rounds - before.query_rounds,
        subscribe_attempts: after.subscribe_attempts - before.subscribe_attempts,
        foreground_close_attempts: after.foreground_close_attempts
            - before.foreground_close_attempts,
        foreground_close_successes: after.foreground_close_successes
            - before.foreground_close_successes,
        fallback_close_attempts: after.fallback_close_attempts - before.fallback_close_attempts,
        add_dispatches: after.add_dispatches - before.add_dispatches,
        remove_dispatches: after.remove_dispatches - before.remove_dispatches,
    }
}

#[cfg(feature = "acceptance-harness")]
fn expected_metrics(
    http: u64,
    observers: u64,
    queries: u64,
    adds: u64,
    removes: u64,
) -> AcceptanceMetricsSnapshot {
    AcceptanceMetricsSnapshot {
        http_logical: http,
        http_physical: http,
        observer_attempts: observers,
        query_rounds: queries,
        subscribe_attempts: queries,
        foreground_close_attempts: queries,
        foreground_close_successes: queries,
        fallback_close_attempts: 0,
        add_dispatches: adds,
        remove_dispatches: removes,
    }
}

#[cfg(feature = "acceptance-harness")]
async fn acceptance_call_with_metrics(
    driver: &mut ViewsWriteDriver,
    name: &'static str,
    arguments: Value,
    expected: AcceptanceMetricsSnapshot,
) -> TestResult<AcceptanceCall> {
    let before = driver.metrics()?;
    let result = driver.call(name, arguments).await?;
    let after = driver.metrics()?;
    let actual = metrics_delta(before, after);
    if actual != expected {
        eprintln!(
            "views-write acceptance metrics mismatch for {name}: actual={actual:?} expected={expected:?}"
        );
        return Err(TestError::Assertion {
            message: "views-write acceptance metrics mismatch".to_owned(),
        });
    }
    Ok(result)
}

#[cfg(feature = "acceptance-harness")]
fn require_membership_result(
    result: &AcceptanceCall,
    collection_id: &str,
    object_id: &str,
    membership: &str,
) -> TestResult<()> {
    if result.is_error
        || result.structured
            != json!({
                "collection_id":collection_id,
                "object_id":object_id,
                "membership":membership
            })
    {
        return Err(TestError::Assertion {
            message: "views-write acceptance returned wrong membership identity".to_owned(),
        });
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn require_tool_error(
    result: &AcceptanceCall,
    code: &str,
    context: &'static str,
) -> TestResult<()> {
    if !result.is_error || result.structured["code"] != code {
        return Err(TestError::Assertion {
            message: context.to_owned(),
        });
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
async fn require_exact_canonical_members(
    ctx: &TestContext,
    collection_id: &str,
    expected: &[&str],
) -> TestResult<Vec<String>> {
    let page = ctx
        .client
        .collection_membership_page(&ctx.space_id, collection_id, 61, None)
        .await?;
    let mut actual_members = page
        .object_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut expected_members = expected.to_vec();
    actual_members.sort_unstable();
    expected_members.sort_unstable();
    if page.continuation.is_some() || actual_members != expected_members {
        return Err(TestError::Assertion {
            message: "canonical collection members changed outside the target scope".to_owned(),
        });
    }
    Ok(page.object_ids)
}

#[cfg(feature = "acceptance-harness")]
async fn require_exact_fixture_states(
    ctx: &TestContext,
    collection_id: &str,
    seed_id: &str,
    target_id: &str,
    target_state: anytype::views::CollectionMembershipState,
    control_id: &str,
) -> TestResult<()> {
    for (object_id, expected) in [
        (seed_id, anytype::views::CollectionMembershipState::Present),
        (target_id, target_state),
        (
            control_id,
            anytype::views::CollectionMembershipState::Absent,
        ),
    ] {
        let observed = ctx
            .client
            .observe_collection_membership(&ctx.space_id, collection_id, object_id)
            .await?;
        if observed.state != expected {
            return Err(TestError::Assertion {
                message: "collection membership escaped the A/B/C test scope".to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Copy)]
struct ViewsWriteFixture<'a> {
    collection_id: &'a str,
    query_id: &'a str,
    seed_id: &'a str,
    target_id: &'a str,
    control_id: &'a str,
    saved_view_id: &'a str,
}

#[cfg(feature = "acceptance-harness")]
async fn run_views_write_transport_scenario(
    ctx: &TestContext,
    transport: ViewsWriteTransport,
    fixture: ViewsWriteFixture<'_>,
) -> TestResult<Vec<AcceptanceCall>> {
    let ViewsWriteFixture {
        collection_id,
        query_id,
        seed_id,
        target_id,
        control_id,
        saved_view_id,
    } = fixture;
    let args = json!({
        "space":ctx.space_id,
        "collection_id":collection_id,
        "object_id":target_id
    });
    let mut transcript = Vec::new();
    let mut driver = views_write_driver(ctx, transport, AcceptanceMutationMode::Normal, false)?;

    require_exact_canonical_members(ctx, collection_id, &[seed_id]).await?;
    require_exact_fixture_states(
        ctx,
        collection_id,
        seed_id,
        target_id,
        anytype::views::CollectionMembershipState::Absent,
        control_id,
    )
    .await?;

    for (name, arguments) in [
        (
            "collection_member_list",
            json!({
                "space":ctx.space_id,
                "collection_id":query_id,
                "limit":1
            }),
        ),
        (
            "collection_member_add",
            json!({
                "space":ctx.space_id,
                "collection_id":query_id,
                "object_id":target_id
            }),
        ),
        (
            "collection_member_remove",
            json!({
                "space":ctx.space_id,
                "collection_id":query_id,
                "object_id":target_id
            }),
        ),
    ] {
        let rejection = acceptance_call_with_metrics(
            &mut driver,
            name,
            arguments,
            expected_metrics(1, 0, 0, 0, 0),
        )
        .await?;
        require_tool_error(&rejection, "upstream", "Set/query rejection was not exact")?;
        transcript.push(rejection);
    }

    for (mode, code) in [
        (AcceptanceMutationMode::CancelAddBeforeMark, "upstream"),
        (AcceptanceMutationMode::CancelAddAfterMark, "conflict"),
    ] {
        let mut cancellation = views_write_driver(ctx, transport, mode, false)?;
        let result = acceptance_call_with_metrics(
            &mut cancellation,
            "collection_member_add",
            args.clone(),
            expected_metrics(2, 1, 3, 0, 0),
        )
        .await?;
        if !result.is_error || result.structured["code"] != code {
            return Err(TestError::Assertion {
                message: "add cancellation boundary was not exact".to_owned(),
            });
        }
        cancellation.finish()?;
        transcript.push(result);
    }
    require_exact_canonical_members(ctx, collection_id, &[seed_id]).await?;

    let added = acceptance_call_with_metrics(
        &mut driver,
        "collection_member_add",
        args.clone(),
        expected_metrics(5, 2, 5, 1, 0),
    )
    .await?;
    require_membership_result(&added, collection_id, target_id, "present")?;
    transcript.push(added);
    require_exact_canonical_members(ctx, collection_id, &[seed_id, target_id]).await?;

    for (mode, code) in [
        (AcceptanceMutationMode::CancelRemoveBeforeMark, "upstream"),
        (AcceptanceMutationMode::CancelRemoveAfterMark, "conflict"),
    ] {
        let mut cancellation = views_write_driver(ctx, transport, mode, false)?;
        let result = acceptance_call_with_metrics(
            &mut cancellation,
            "collection_member_remove",
            args.clone(),
            expected_metrics(2, 1, 2, 0, 0),
        )
        .await?;
        if !result.is_error || result.structured["code"] != code {
            return Err(TestError::Assertion {
                message: "remove cancellation boundary was not exact".to_owned(),
            });
        }
        cancellation.finish()?;
        transcript.push(result);
    }
    require_exact_fixture_states(
        ctx,
        collection_id,
        seed_id,
        target_id,
        anytype::views::CollectionMembershipState::Present,
        control_id,
    )
    .await?;

    let add_noop = acceptance_call_with_metrics(
        &mut driver,
        "collection_member_add",
        args.clone(),
        expected_metrics(2, 1, 2, 0, 0),
    )
    .await?;
    require_membership_result(&add_noop, collection_id, target_id, "present")?;
    transcript.push(add_noop);
    let canonical_order =
        require_exact_canonical_members(ctx, collection_id, &[seed_id, target_id]).await?;

    let first = acceptance_call_with_metrics(
        &mut driver,
        "collection_member_list",
        json!({
            "space":ctx.space_id,
            "collection_id":collection_id,
            "limit":1
        }),
        expected_metrics(1, 0, 1, 0, 0),
    )
    .await?;
    if first.is_error {
        return Err(TestError::Assertion {
            message: "canonical first membership page failed".to_owned(),
        });
    }
    let first_cursor = first.structured["next_cursor"]
        .as_str()
        .ok_or_else(|| TestError::Assertion {
            message: "two-member fixture omitted its continuation cursor".to_owned(),
        })?
        .to_owned();
    for mismatch_input in [
        json!({
            "space":ctx.space_id,
            "collection_id":collection_id,
            "limit":2,
            "cursor":first_cursor.clone()
        }),
        json!({
            "space":ctx.space_id,
            "collection_id":query_id,
            "limit":1,
            "cursor":first_cursor.clone()
        }),
    ] {
        let mismatch = acceptance_call_with_metrics(
            &mut driver,
            "collection_member_list",
            mismatch_input,
            expected_metrics(0, 0, 0, 0, 0),
        )
        .await?;
        require_tool_error(
            &mismatch,
            "validation",
            "cursor mismatch was not rejected before membership I/O",
        )?;
        transcript.push(mismatch);
    }

    let mut cursor = Some(first_cursor);
    let mut canonical_ids = first.structured["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| item["object_id"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    loop {
        let mut input = json!({
            "space":ctx.space_id,
            "collection_id":collection_id,
            "limit":1
        });
        if let Some(token) = cursor.take() {
            input["cursor"] = Value::String(token);
        }
        let page = acceptance_call_with_metrics(
            &mut driver,
            "collection_member_list",
            input,
            expected_metrics(1, 0, 1, 0, 0),
        )
        .await?;
        if page.is_error {
            return Err(TestError::Assertion {
                message: "canonical membership page failed".to_owned(),
            });
        }
        canonical_ids.extend(
            page.structured["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item["object_id"].as_str().map(str::to_owned)),
        );
        cursor = page.structured["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    if canonical_ids != canonical_order {
        return Err(TestError::Assertion {
            message: "canonical pagination did not preserve exact A/B membership".to_owned(),
        });
    }
    let presentation = ctx
        .client
        .view_list_objects(&ctx.space_id, collection_id)
        .view(saved_view_id)
        .limit(61)
        .list()
        .await?;
    if presentation.items.iter().any(|item| item.id == target_id) {
        return Err(TestError::Assertion {
            message: "saved-view filtering altered canonical membership".to_owned(),
        });
    }

    let removed = acceptance_call_with_metrics(
        &mut driver,
        "collection_member_remove",
        args.clone(),
        expected_metrics(5, 2, 5, 0, 1),
    )
    .await?;
    require_membership_result(&removed, collection_id, target_id, "absent")?;
    transcript.push(removed);

    let remove_noop = acceptance_call_with_metrics(
        &mut driver,
        "collection_member_remove",
        args.clone(),
        expected_metrics(2, 1, 3, 0, 0),
    )
    .await?;
    require_membership_result(&remove_noop, collection_id, target_id, "absent")?;
    transcript.push(remove_noop);
    driver.finish()?;

    require_exact_canonical_members(ctx, collection_id, &[seed_id]).await?;
    require_exact_fixture_states(
        ctx,
        collection_id,
        seed_id,
        target_id,
        anytype::views::CollectionMembershipState::Absent,
        control_id,
    )
    .await?;

    let mut concurrent =
        views_write_driver(ctx, transport, AcceptanceMutationMode::ConcurrentAdd, false)?;
    let before = concurrent.metrics()?;
    let concurrent_results = concurrent
        .call_pair("collection_member_add", args.clone(), args.clone())
        .await?;
    let delta = metrics_delta(before, concurrent.metrics()?);
    let verification_observers =
        delta
            .observer_attempts
            .checked_sub(2)
            .ok_or_else(|| TestError::Assertion {
                message: "concurrent add omitted one of its two preflight observers".to_owned(),
            })?;
    if delta.add_dispatches != 2
        || delta.remove_dispatches != 0
        || delta.http_logical != 6 + 2 * verification_observers
        || delta.http_physical != delta.http_logical
        || delta.query_rounds != 6 + 2 * verification_observers
        || delta.subscribe_attempts != delta.query_rounds
        || delta.foreground_close_attempts != delta.query_rounds
        || delta.foreground_close_successes != delta.query_rounds
        || delta.fallback_close_attempts != 0
    {
        return Err(TestError::Assertion {
            message: "concurrent add aggregate work was not exact".to_owned(),
        });
    }
    let mut successful = 0;
    for result in concurrent_results {
        if result.is_error {
            require_tool_error(
                &result,
                "conflict",
                "concurrent add returned an unsafe failure category",
            )?;
        } else {
            require_membership_result(&result, collection_id, target_id, "present")?;
            successful += 1;
        }
    }
    if successful == 0 {
        return Err(TestError::Assertion {
            message: "concurrent add produced no verified success".to_owned(),
        });
    }
    require_exact_canonical_members(ctx, collection_id, &[seed_id, target_id]).await?;
    require_exact_fixture_states(
        ctx,
        collection_id,
        seed_id,
        target_id,
        anytype::views::CollectionMembershipState::Present,
        control_id,
    )
    .await?;
    let concurrent_cleanup = acceptance_call_with_metrics(
        &mut concurrent,
        "collection_member_remove",
        args.clone(),
        expected_metrics(5, 2, 5, 0, 1),
    )
    .await?;
    require_membership_result(&concurrent_cleanup, collection_id, target_id, "absent")?;
    transcript.push(concurrent_cleanup);
    concurrent.finish()?;
    require_exact_canonical_members(ctx, collection_id, &[seed_id]).await?;
    require_exact_fixture_states(
        ctx,
        collection_id,
        seed_id,
        target_id,
        anytype::views::CollectionMembershipState::Absent,
        control_id,
    )
    .await?;

    let mut read_only = views_write_driver(ctx, transport, AcceptanceMutationMode::Normal, true)?;
    for name in ["collection_member_add", "collection_member_remove"] {
        let read_only_result = acceptance_call_with_metrics(
            &mut read_only,
            name,
            args.clone(),
            expected_metrics(0, 0, 0, 0, 0),
        )
        .await?;
        require_tool_error(
            &read_only_result,
            "validation",
            "read-only mutation gate was not exact",
        )?;
        transcript.push(read_only_result);
    }
    read_only.finish()?;

    for object_id in [seed_id, target_id, control_id] {
        let survived = ctx.client.object(&ctx.space_id, object_id).get().await?;
        if survived.id != object_id || survived.space_id != ctx.space_id {
            return Err(TestError::Assertion {
                message: "collection workflow changed an A/B/C object".to_owned(),
            });
        }
    }
    Ok(transcript)
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
async fn offline_direct_stable_preview_403_mapping_is_exact_and_io_free() {
    use anytype::prelude::{ClientConfig, HttpCredentials};

    let client = AnytypeClient::with_config(ClientConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        keystore: Some("env".to_owned()),
        keystore_service: Some("views-write-offline-direct".to_owned()),
        app_name: "views-write-offline-direct".to_owned(),
        disable_cache: true,
        ..ClientConfig::default()
    })
    .expect("offline direct classifier client");
    client.set_api_key(HttpCredentials::new("offline-direct-token"));
    let direct =
        ViewsWriteAcceptanceDirect::new(client, false, AcceptanceMutationMode::ClassifyAdd403)
            .expect("offline direct classifier");
    let stable = SpawnedViewsWriteDriver::offline_classification("stable-classify-403")
        .expect("offline stable classifier child");
    let preview = SpawnedViewsWriteDriver::offline_classification("preview-classify-403")
        .expect("offline preview classifier child");
    let mut drivers = [
        ViewsWriteDriver::Direct(Box::new(direct)),
        ViewsWriteDriver::Spawned(Box::new(stable)),
        ViewsWriteDriver::Spawned(Box::new(preview)),
    ];
    let arguments = json!({
        "space":"bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "collection_id":"bafyreicccccccccccccccccccccccccccccccccccccccccccccccccccc",
        "object_id":"bafyreiooooooooooooooooooooooooooooooooooooooooooooooooooo"
    });
    let expected_rejection = json!({
        "code":"authentication",
        "message":"Anytype authentication failed. Verify the configured credentials and retry."
    });
    let mut baseline = None;
    for driver in &mut drivers {
        let mut results = Vec::new();
        for _ in 0..2 {
            let result = acceptance_call_with_metrics(
                driver,
                "collection_member_add",
                arguments.clone(),
                expected_metrics(0, 0, 0, 0, 0),
            )
            .await
            .expect("offline 403 classification call");
            assert!(result.is_error);
            assert_eq!(result.structured, expected_rejection);
            results.push(result);
        }
        if let Some(expected) = baseline.as_ref() {
            assert_eq!(&results, expected);
        } else {
            baseline = Some(results);
        }
        driver.finish().expect("offline classifier shutdown");
    }
}

#[cfg(feature = "acceptance-harness")]
struct OwnedViewsWriteFixture {
    collection_id: String,
    query_id: String,
    seed_id: String,
    target_id: String,
    control_id: String,
    saved_view_id: String,
}

#[cfg(feature = "acceptance-harness")]
impl OwnedViewsWriteFixture {
    fn borrowed(&self) -> ViewsWriteFixture<'_> {
        ViewsWriteFixture {
            collection_id: &self.collection_id,
            query_id: &self.query_id,
            seed_id: &self.seed_id,
            target_id: &self.target_id,
            control_id: &self.control_id,
            saved_view_id: &self.saved_view_id,
        }
    }
}

#[cfg(feature = "acceptance-harness")]
async fn create_views_write_fixture(ctx: &TestContext) -> TestResult<OwnedViewsWriteFixture> {
    let suffix = unique_suffix();
    let collection_type = ctx
        .create_collection_type_fixture(format!("MCP stdio collection {suffix}"))
        .await?;
    let collection_id = ctx
        .create_collection_fixture(&collection_type, format!("MCP stdio members {suffix}"))
        .await?
        .id;
    let name_a = format!("MCP stdio member A {suffix}");
    let seed_id = retry_definitive_rate_limit("stdio member A", || async {
        ctx.client
            .new_object(&ctx.space_id, "page")
            .name(&name_a)
            .create()
            .await
    })
    .await?
    .id;
    ctx.register_object(&seed_id);
    let target_id = retry_definitive_rate_limit("stdio member B", || async {
        ctx.client
            .new_object(&ctx.space_id, "page")
            .name(format!("MCP stdio member B {suffix}"))
            .create()
            .await
    })
    .await?
    .id;
    ctx.register_object(&target_id);
    let control_id = retry_definitive_rate_limit("stdio member C", || async {
        ctx.client
            .new_object(&ctx.space_id, "page")
            .name(format!("MCP stdio member C {suffix}"))
            .create()
            .await
    })
    .await?
    .id;
    ctx.register_object(&control_id);
    let set_type_key = ctx
        .client
        .types(&ctx.space_id)
        .list()
        .await?
        .items
        .iter()
        .find(|typ| typ.layout == anytype::objects::ObjectLayout::Set)
        .map(|typ| typ.key.clone())
        .ok_or_else(|| TestError::Assertion {
            message: "disposable space has no Set-layout type".to_owned(),
        })?;
    let query_id = retry_definitive_rate_limit("stdio query", || async {
        ctx.client
            .new_object(&ctx.space_id, &set_type_key)
            .name(format!("MCP stdio query {suffix}"))
            .create()
            .await
    })
    .await?
    .id;
    ctx.register_object(&query_id);
    ctx.client
        .view_add_objects(&ctx.space_id, &collection_id, [&seed_id])
        .await?;
    let saved_view_id = ctx
        .create_collection_view_fixture(&collection_id, format!("MCP stdio only A {suffix}"))
        .await?
        .id;
    ctx.add_collection_name_filter_fixture(&collection_id, &saved_view_id, &name_a)
        .await?;
    Ok(OwnedViewsWriteFixture {
        collection_id,
        query_id,
        seed_id,
        target_id,
        control_id,
        saved_view_id,
    })
}

#[cfg(feature = "acceptance-harness")]
async fn run_shared_views_write_acceptance(ctx: Arc<TestContext>) -> TestResult<()> {
    let fixture = Box::pin(create_views_write_fixture(ctx.as_ref())).await?;
    let direct = Box::pin(run_views_write_transport_scenario(
        ctx.as_ref(),
        ViewsWriteTransport::Direct,
        fixture.borrowed(),
    ))
    .await
    .map_err(|_| {
        eprintln!("views-write direct acceptance stage failed");
        TestError::Assertion {
            message: "direct views-write acceptance stage".to_owned(),
        }
    })?;
    let stable = Box::pin(run_views_write_transport_scenario(
        ctx.as_ref(),
        ViewsWriteTransport::Stable,
        fixture.borrowed(),
    ))
    .await
    .map_err(|_| {
        eprintln!("views-write stable acceptance stage failed");
        TestError::Assertion {
            message: "stable views-write acceptance stage".to_owned(),
        }
    })?;
    let preview = Box::pin(run_views_write_transport_scenario(
        ctx.as_ref(),
        ViewsWriteTransport::Preview,
        fixture.borrowed(),
    ))
    .await
    .map_err(|_| {
        eprintln!("views-write preview acceptance stage failed");
        TestError::Assertion {
            message: "preview views-write acceptance stage".to_owned(),
        }
    })?;
    if direct != stable || direct != preview {
        return Err(TestError::Assertion {
            message: "direct, stable, and preview results diverged".to_owned(),
        });
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn shared_direct_stable_preview_views_write_acceptance_is_exact() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-views-write-stdio",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(run_shared_views_write_acceptance(ctx))
        },
    ))
    .await
    .expect("cleanup-safe spawned views-write acceptance");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned views-write acceptance skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_schema_registry_runs_all_nine_workflows() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-schema",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let mut driver =
                    StdioDriver::start_with_toolsets(DriverOptions::STANDARD, Some("schema"));
                let tools = driver.list_tools_sync().expect("schema tools/list");
                let schema_names = [
                    "property_create",
                    "property_update",
                    "space_create",
                    "space_update",
                    "tag_create",
                    "tag_update",
                    "type_create",
                    "type_get",
                    "type_update",
                ];
                for name in schema_names {
                    assert!(
                        tools.iter().any(|candidate| candidate == name),
                        "missing {name}"
                    );
                }
                assert_eq!(
                    tools
                        .iter()
                        .filter(|name| name.as_str() == "optional_toolset_status")
                        .count(),
                    1
                );
                let status = driver
                    .call_tool_sync("optional_toolset_status", json!({}))
                    .expect("schema status");
                assert_eq!(status["configured_toolsets"], json!(["schema"]));
                assert_eq!(status["active_toolsets"], json!(["schema"]));

                let created_space_name = format!("MCP schema registry space {}", unique_suffix());
                let created_space_claim =
                    Arc::new(ctx.prepare_space_fixture_claim(&created_space_name).await?);
                let created_space = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    driver.call_tool_sync(
                        "space_create",
                        json!({
                            "name":created_space_name,
                            "description":"schema registry create",
                            "idempotency_key":format!("space-{}", unique_suffix())
                        }),
                    )
                })) {
                    Ok(result) => result.expect("spawned space_create"),
                    Err(_) => {
                        let (_, output, process_category) = driver.finish_after_panic();
                        let stderr = stderr_metrics(&output.stderr);
                        panic!(
                            "spawned schema call failed: process={process_category} status={} stderr={}",
                            output.exit_category,
                            stderr.summary()
                        );
                    }
                };
                let created_space_id = created_space
                    .pointer("/space/id")
                    .and_then(Value::as_str)
                    .expect("created space id")
                    .to_owned();
                let created_space_readback =
                    ctx.client.space(&created_space_id).get_direct().await?;
                ctx.claim_prepared_space_fixture(
                    created_space_claim.as_ref(),
                    &created_space_readback,
                )?;
                let updated_space = driver
                    .call_tool_sync(
                        "space_update",
                        json!({
                            "space":created_space_id,
                            "description":"schema registry updated"
                        }),
                    )
                    .expect("spawned space_update");
                assert_eq!(
                    updated_space
                        .pointer("/space/description")
                        .and_then(Value::as_str),
                    Some("schema registry updated")
                );
                assert_eq!(
                    ctx.client
                        .space(&created_space_id)
                        .get_direct()
                        .await?
                        .description
                        .as_deref(),
                    Some("schema registry updated")
                );

                let type_name = format!("MCP schema type {}", unique_suffix());
                let created_type = driver
                    .call_tool_sync(
                        "type_create",
                        json!({
                            "space":ctx.space_id,
                            "name":type_name,
                            "layout":"basic",
                            "idempotency_key":format!("type-{}", unique_suffix())
                        }),
                    )
                    .expect("spawned type_create");
                let type_id = created_type
                    .pointer("/type/id")
                    .and_then(Value::as_str)
                    .expect("created type id")
                    .to_owned();
                ctx.register_type(&type_id);
                let fetched_type = driver
                    .call_tool_sync("type_get", json!({"space":ctx.space_id,"type":type_id}))
                    .expect("spawned type_get");
                assert_eq!(
                    fetched_type.pointer("/type/id").and_then(Value::as_str),
                    Some(type_id.as_str())
                );
                let updated_type_name = format!("MCP schema updated type {}", unique_suffix());
                let updated_type = driver
                    .call_tool_sync(
                        "type_update",
                        json!({
                            "space":ctx.space_id,
                            "type":type_id,
                            "name":updated_type_name
                        }),
                    )
                    .expect("spawned type_update");
                assert_eq!(
                    updated_type.pointer("/type/name").and_then(Value::as_str),
                    Some(updated_type_name.as_str())
                );
                assert_eq!(
                    ctx.client
                        .get_type(&ctx.space_id, &type_id)
                        .get_direct()
                        .await?
                        .name
                        .as_deref(),
                    Some(updated_type_name.as_str())
                );

                let property_name = format!("MCP schema property {}", unique_suffix());
                let created_property = driver
                    .call_tool_sync(
                        "property_create",
                        json!({
                            "space":ctx.space_id,
                            "name":property_name,
                            "format":"select",
                            "idempotency_key":format!("property-{}", unique_suffix())
                        }),
                    )
                    .expect("spawned property_create");
                let property_id = created_property
                    .pointer("/property/id")
                    .and_then(Value::as_str)
                    .expect("created property id")
                    .to_owned();
                ctx.register_property(&property_id);
                assert_eq!(created_property["tags"], json!([]));
                let updated_property_name =
                    format!("MCP schema updated property {}", unique_suffix());
                let updated_property = driver
                    .call_tool_sync(
                        "property_update",
                        json!({
                            "space":ctx.space_id,
                            "property":property_id,
                            "name":updated_property_name
                        }),
                    )
                    .expect("spawned property_update");
                assert_eq!(
                    updated_property
                        .pointer("/property/name")
                        .and_then(Value::as_str),
                    Some(updated_property_name.as_str())
                );
                assert_eq!(
                    ctx.client
                        .property(&ctx.space_id, &property_id)
                        .get_direct()
                        .await?
                        .name,
                    updated_property_name
                );

                let tag_name = format!("MCP schema tag {}", unique_suffix());
                let created_tag = driver
                    .call_tool_sync(
                        "tag_create",
                        json!({
                            "space":ctx.space_id,
                            "property":property_id,
                            "name":tag_name,
                            "color":"grey",
                            "idempotency_key":format!("tag-{}", unique_suffix())
                        }),
                    )
                    .expect("spawned tag_create");
                let tag_id = created_tag
                    .pointer("/tag/id")
                    .and_then(Value::as_str)
                    .expect("created tag id")
                    .to_owned();
                let created_tag_readback = property_scoped_tag_readback(
                    &ctx.client,
                    &ctx.space_id,
                    &property_id,
                    &tag_id,
                )
                .await?;
                assert_eq!(created_tag_readback.space_id, ctx.space_id);
                assert_eq!(created_tag_readback.property_id, property_id);
                assert_eq!(created_tag_readback.tag.id, tag_id);
                assert_eq!(created_tag_readback.tag.name, tag_name);
                assert_eq!(created_tag_readback.tag.color, Color::Grey);
                let updated_tag_name = format!("MCP schema updated tag {}", unique_suffix());
                let updated_tag = driver
                    .call_tool_sync(
                        "tag_update",
                        json!({
                            "space":ctx.space_id,
                            "property":property_id,
                            "tag_id":tag_id,
                            "name":updated_tag_name,
                            "color":"teal"
                        }),
                    )
                    .expect("spawned tag_update");
                assert_eq!(
                    updated_tag.pointer("/tag/name").and_then(Value::as_str),
                    Some(updated_tag_name.as_str())
                );
                assert_eq!(
                    updated_tag.pointer("/tag/color").and_then(Value::as_str),
                    Some("teal")
                );
                let updated_tag_readback = property_scoped_tag_readback(
                    &ctx.client,
                    &ctx.space_id,
                    &property_id,
                    &tag_id,
                )
                .await?;
                assert_eq!(updated_tag_readback.space_id, ctx.space_id);
                assert_eq!(updated_tag_readback.property_id, property_id);
                assert_eq!(updated_tag_readback.tag.id, tag_id);
                assert_eq!(updated_tag_readback.tag.name, updated_tag_name);
                assert_eq!(updated_tag_readback.tag.color, Color::Teal);

                let (transcript, output) = driver.finish();
                assert!(!transcript.contains(&created_space_name));
                let stderr = String::from_utf8_lossy(&output.stderr);
                for sensitive in [
                    created_space_name.as_str(),
                    type_name.as_str(),
                    updated_type_name.as_str(),
                    property_name.as_str(),
                    updated_property_name.as_str(),
                    tag_name.as_str(),
                    updated_tag_name.as_str(),
                ] {
                    assert!(!stderr.contains(sensitive));
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned schema registry suite");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned schema registry suite skipped before callback: {reason:?}");
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

fn take_registered_body_driver(
    driver: &Arc<Mutex<Option<StdioDriver>>>,
) -> TestResult<StdioDriver> {
    lock_driver(driver)
        .take()
        .ok_or_else(|| sentinel_assertion("registered body-block child disappeared"))
}

fn body_tool_value(
    driver: &mut StdioDriver,
    name: &'static str,
    arguments: Value,
) -> TestResult<Value> {
    driver
        .call_tool_sync(name, arguments)
        .map_err(|_| sentinel_assertion("spawned body-block call failed"))
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_body_blocks_direct_stable_preview_and_object_show() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_callback_cleanup = Arc::clone(&stable_cleanup);
    let preview_callback_cleanup = Arc::clone(&preview_cleanup);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-body-blocks",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let suffix = unique_suffix();
                let object = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(format!("MCP body blocks {suffix}"))
                    .body("# Seed heading\n\nSeed paragraph")
                    .create()
                    .await?;
                ctx.register_object(&object.id);

                let stable = spawn_disposable_driver(
                    ctx.as_ref(),
                    stable_callback_cleanup,
                    DriverOptions::STANDARD,
                    Some("body-blocks"),
                )?;
                let (final_snapshot_hash, rich_id, first_id, second_id) = {
                    let mut guard = lock_driver(&stable);
                    let driver = guard
                        .as_mut()
                        .ok_or_else(|| sentinel_assertion("stable body child missing"))?;
                    driver.initialize();
                    let tools = driver
                        .list_tools_sync()
                        .map_err(|_| sentinel_assertion("stable body catalog failed"))?;
                    for name in [
                        "body_block_list",
                        "body_block_create",
                        "body_block_update",
                        "body_block_delete",
                        "body_block_move",
                        "rich_page_create",
                    ] {
                        if !tools.iter().any(|candidate| candidate == name) {
                            return Err(sentinel_assertion("stable body catalog omitted a tool"));
                        }
                    }

                    let initial = body_tool_value(
                        driver,
                        "body_block_list",
                        json!({"space":ctx.space_id,"object_id":object.id,"limit":12}),
                    )?;
                    let root_id = initial["root_id"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("stable body list omitted root ID"))?
                        .to_owned();
                    let mut snapshot_hash = initial["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("stable body list omitted snapshot hash"))?
                        .to_owned();

                    let first = body_tool_value(
                        driver,
                        "body_block_create",
                        json!({
                            "space":ctx.space_id,
                            "object_id":object.id,
                            "expected_snapshot_hash":snapshot_hash,
                            "target_block_id":root_id,
                            "position":"last_child",
                            "block":{"kind":"text","style":"paragraph","text":"first body block","marks":[]},
                            "idempotency_key":format!("body-first-{suffix}")
                        }),
                    )?;
                    let first_id = first["block"]["id"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("body create omitted block ID"))?
                        .to_owned();
                    snapshot_hash = first["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("body create omitted snapshot hash"))?
                        .to_owned();

                    let updated = body_tool_value(
                        driver,
                        "body_block_update",
                        json!({
                            "space":ctx.space_id,
                            "object_id":object.id,
                            "expected_snapshot_hash":snapshot_hash,
                            "block_id":first_id,
                            "change":{"kind":"set_text","text":"updated body block","marks":[]}
                        }),
                    )?;
                    snapshot_hash = updated["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("body update omitted snapshot hash"))?
                        .to_owned();

                    let second = body_tool_value(
                        driver,
                        "body_block_create",
                        json!({
                            "space":ctx.space_id,
                            "object_id":object.id,
                            "expected_snapshot_hash":snapshot_hash,
                            "target_block_id":root_id,
                            "position":"last_child",
                            "block":{"kind":"relation","key":"tag"},
                            "idempotency_key":format!("body-second-{suffix}")
                        }),
                    )?;
                    let second_id = second["block"]["id"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("second create omitted block ID"))?
                        .to_owned();
                    snapshot_hash = second["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("second create omitted snapshot hash"))?
                        .to_owned();

                    let moved = body_tool_value(
                        driver,
                        "body_block_move",
                        json!({
                            "space":ctx.space_id,
                            "object_id":object.id,
                            "expected_snapshot_hash":snapshot_hash,
                            "block_id":first_id,
                            "target_block_id":second_id,
                            "position":"after"
                        }),
                    )?;
                    snapshot_hash = moved["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("body move omitted snapshot hash"))?
                        .to_owned();

                    let deleted = body_tool_value(
                        driver,
                        "body_block_delete",
                        json!({
                            "space":ctx.space_id,
                            "object_id":object.id,
                            "expected_snapshot_hash":snapshot_hash,
                            "block_id":second_id,
                            "expected_subtree_blocks":1,
                            "confirm_delete":"delete_subtree"
                        }),
                    )?;
                    let final_snapshot_hash = deleted["snapshot_hash"]
                        .as_str()
                        .ok_or_else(|| sentinel_assertion("body delete omitted snapshot hash"))?
                        .to_owned();

                    let rich = body_tool_value(
                        driver,
                        "rich_page_create",
                        json!({
                            "space":ctx.space_id,
                            "name":format!("MCP rich page {suffix}"),
                            "idempotency_key":format!("rich-page-{suffix}"),
                            "blocks":[
                                {"local_key":"heading","block":{"kind":"text","style":"heading_1","text":"Rich heading","marks":[]}},
                                {"local_key":"body","parent_key":"heading","block":{"kind":"embed","processor":"mermaid","source":"graph TD; A-->B"}}
                            ]
                        }),
                    )?;
                    let rich_id = rich["object_id"].as_str().map(str::to_owned);
                    if let Some(rich_id) = rich_id.as_deref() {
                        ctx.register_object(rich_id);
                    }
                    if rich["status"] != "complete" {
                        return Err(sentinel_assertion("rich page did not complete"));
                    }
                    let rich_id = rich_id
                        .ok_or_else(|| sentinel_assertion("rich page omitted object ID"))?
                        .to_owned();
                    (final_snapshot_hash, rich_id, first_id, second_id)
                };
                let rich_object = ctx.client.object(&ctx.space_id, &rich_id).get().await?;
                if rich_object.id != rich_id {
                    return Err(sentinel_assertion("rich page exact GET identity mismatch"));
                }
                let rich_snapshot = ctx
                    .client
                    .blocks()
                    .body(&ctx.space_id, &rich_id)
                    .fetch()
                    .await?;
                if rich_snapshot.object_id != rich_id || rich_snapshot.len() < 3 {
                    return Err(sentinel_assertion("rich page ObjectShow verification failed"));
                }

                let final_snapshot = ctx
                    .client
                    .blocks()
                    .body(&ctx.space_id, &object.id)
                    .fetch()
                    .await?;
                if final_snapshot.object_id != object.id
                    || final_snapshot
                        .iter()
                        .all(|block| block.id.as_str() != first_id)
                    || final_snapshot
                        .iter()
                        .any(|block| block.id.as_str() == second_id)
                {
                    return Err(sentinel_assertion("primitive ObjectShow verification failed"));
                }
                let stable_driver = take_registered_body_driver(&stable)?;
                let (_, stable_output) = stable_driver
                    .try_finish()
                    .map_err(|_| sentinel_assertion("stable body child did not stop"))?;
                if !stable_output.stderr.is_empty()
                    && stderr_metrics(&stable_output.stderr).panic != 0
                {
                    return Err(sentinel_assertion("stable body child emitted a panic"));
                }

                let preview = spawn_disposable_driver(
                    ctx.as_ref(),
                    preview_callback_cleanup,
                    DriverOptions::PREVIEW_STANDARD,
                    Some("body-blocks"),
                )?;
                {
                    let mut guard = lock_driver(&preview);
                    let driver = guard
                        .as_mut()
                        .ok_or_else(|| sentinel_assertion("preview body child missing"))?;
                    driver.initialize();
                    let preview_list = body_tool_value(
                        driver,
                        "body_block_list",
                        json!({"space":ctx.space_id,"object_id":object.id,"limit":12}),
                    )?;
                    if preview_list["snapshot_hash"] != final_snapshot_hash {
                        return Err(sentinel_assertion("stable and preview body hashes diverged"));
                    }
                }
                let preview_driver = take_registered_body_driver(&preview)?;
                preview_driver
                    .try_finish()
                    .map_err(|_| sentinel_assertion("preview body child did not stop"))?;
                Ok(ctx.space_id.clone())
            })
        },
    ))
    .await
    .expect("cleanup-safe body-block direct/stdio acceptance");
    match outcome {
        DisposableRun::Completed(space_id) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *stable_cleanup.lock().expect("stable cleanup record"),
                ChildCleanupRecord::Stopped
            );
            assert_fresh_space_absence(&space_id).await;
            assert_eq!(
                *preview_cleanup.lock().expect("preview cleanup record"),
                ChildCleanupRecord::Stopped
            );
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("body-block acceptance skipped before callback: {reason:?}");
        }
    }
}

#[cfg(feature = "acceptance-harness")]
type BodyAcceptancePhaseFuture<'a, T> = Pin<Box<dyn Future<Output = TestResult<T>> + 'a>>;

#[cfg(feature = "acceptance-harness")]
type SharedBodyCallbackFuture = Pin<Box<dyn Future<Output = TestResult<String>>>>;

#[cfg(feature = "acceptance-harness")]
struct SpawnedBodyEvidence {
    scenario: BodyScenarioEvidence,
    descriptors: Vec<Value>,
    frames: [Value; 2],
    stale_cursor_frame: Value,
}

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyFrameParityFailure {
    StableResultType,
    PreviewResultType,
    EnvelopeVersion,
    EnvelopeId,
    EnvelopeShape,
    OutcomeShape,
    ResultShape,
    TextDuplicate,
    ToolErrorShape,
    ErrorShape,
    ErrorCode,
    ErrorMessage,
    ErrorData,
    Payload,
}

#[cfg(feature = "acceptance-harness")]
fn compare_body_protocol_frame(
    stable: &Value,
    preview: &Value,
) -> Result<(), BodyFrameParityFailure> {
    if stable.pointer("/result/resultType").is_some() {
        return Err(BodyFrameParityFailure::StableResultType);
    }
    if stable.get("jsonrpc") != preview.get("jsonrpc") {
        return Err(BodyFrameParityFailure::EnvelopeVersion);
    }
    if stable.get("id") != preview.get("id") {
        return Err(BodyFrameParityFailure::EnvelopeId);
    }
    let Some(stable_object) = stable.as_object() else {
        return Err(BodyFrameParityFailure::EnvelopeShape);
    };
    let Some(preview_object) = preview.as_object() else {
        return Err(BodyFrameParityFailure::EnvelopeShape);
    };
    if stable_object.keys().collect::<Vec<_>>() != preview_object.keys().collect::<Vec<_>>() {
        return Err(BodyFrameParityFailure::EnvelopeShape);
    }
    match (
        stable.get("result"),
        stable.get("error"),
        preview.get("result"),
        preview.get("error"),
    ) {
        (Some(_), None, Some(_), None) => compare_body_result_frame(stable, preview),
        (None, Some(_), None, Some(_)) => compare_body_error_frame(stable, preview),
        _ => Err(BodyFrameParityFailure::OutcomeShape),
    }
}

#[cfg(feature = "acceptance-harness")]
fn compare_body_result_frame(
    stable: &Value,
    preview: &Value,
) -> Result<(), BodyFrameParityFailure> {
    let mut normalized_preview = preview.clone();
    let Some(preview_result) = normalized_preview
        .get_mut("result")
        .and_then(Value::as_object_mut)
    else {
        return Err(BodyFrameParityFailure::ResultShape);
    };
    if preview_result.remove("resultType") != Some(json!("complete")) {
        return Err(BodyFrameParityFailure::PreviewResultType);
    }
    validate_body_result_semantics(stable)?;
    validate_body_result_semantics(&normalized_preview)?;
    validate_body_frame_text_duplicate(stable)?;
    validate_body_frame_text_duplicate(&normalized_preview)?;
    let Some(stable_result) = stable.get("result").and_then(Value::as_object) else {
        return Err(BodyFrameParityFailure::ResultShape);
    };
    let Some(preview_result) = normalized_preview.get("result").and_then(Value::as_object) else {
        return Err(BodyFrameParityFailure::ResultShape);
    };
    if stable_result.keys().collect::<Vec<_>>() != preview_result.keys().collect::<Vec<_>>() {
        return Err(BodyFrameParityFailure::ResultShape);
    }
    if stable != &normalized_preview {
        return Err(BodyFrameParityFailure::Payload);
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn validate_body_result_semantics(frame: &Value) -> Result<(), BodyFrameParityFailure> {
    let result = frame
        .get("result")
        .and_then(Value::as_object)
        .ok_or(BodyFrameParityFailure::ResultShape)?;
    let expected_keys = ["content", "isError", "structuredContent"];
    if result.len() != expected_keys.len()
        || !expected_keys.iter().all(|key| result.contains_key(*key))
    {
        return Err(BodyFrameParityFailure::ResultShape);
    }
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .ok_or(BodyFrameParityFailure::ResultShape)?;
    if !is_error {
        return Ok(());
    }
    let structured = result
        .get("structuredContent")
        .and_then(Value::as_object)
        .ok_or(BodyFrameParityFailure::ToolErrorShape)?;
    if !(structured.len() == 2 || structured.len() == 3)
        || !structured.contains_key("code")
        || !structured.contains_key("message")
        || (structured.len() == 3 && !structured.contains_key("candidates"))
        || structured
            .get("code")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        || structured
            .get("message")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(BodyFrameParityFailure::ToolErrorShape);
    }
    if let Some(candidates) = structured.get("candidates") {
        let candidates = candidates
            .as_array()
            .filter(|values| !values.is_empty() && values.len() <= 8)
            .ok_or(BodyFrameParityFailure::ToolErrorShape)?;
        if candidates.iter().any(|candidate| {
            let Some(candidate) = candidate.as_object() else {
                return true;
            };
            candidate.len() != 2
                || candidate
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.is_empty() || value.len() > 256)
                || candidate
                    .get("name")
                    .and_then(Value::as_str)
                    .is_none_or(|value| value.is_empty() || value.len() > 256)
        }) {
            return Err(BodyFrameParityFailure::ToolErrorShape);
        }
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn compare_body_error_frame(stable: &Value, preview: &Value) -> Result<(), BodyFrameParityFailure> {
    let Some(stable_error) = stable.get("error").and_then(Value::as_object) else {
        return Err(BodyFrameParityFailure::ErrorShape);
    };
    let Some(preview_error) = preview.get("error").and_then(Value::as_object) else {
        return Err(BodyFrameParityFailure::ErrorShape);
    };
    if stable_error.keys().collect::<Vec<_>>() != preview_error.keys().collect::<Vec<_>>() {
        return Err(BodyFrameParityFailure::ErrorShape);
    }
    if stable_error.get("code") != preview_error.get("code") {
        return Err(BodyFrameParityFailure::ErrorCode);
    }
    if stable_error.get("message") != preview_error.get("message") {
        return Err(BodyFrameParityFailure::ErrorMessage);
    }
    if stable_error.get("data") != preview_error.get("data") {
        return Err(BodyFrameParityFailure::ErrorData);
    }
    if stable != preview {
        return Err(BodyFrameParityFailure::Payload);
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn validate_body_frame_text_duplicate(frame: &Value) -> Result<(), BodyFrameParityFailure> {
    let result = frame
        .get("result")
        .and_then(Value::as_object)
        .ok_or(BodyFrameParityFailure::EnvelopeShape)?;
    let structured = result
        .get("structuredContent")
        .ok_or(BodyFrameParityFailure::TextDuplicate)?;
    let content = result
        .get("content")
        .and_then(Value::as_array)
        .ok_or(BodyFrameParityFailure::TextDuplicate)?;
    let [item] = content.as_slice() else {
        return Err(BodyFrameParityFailure::TextDuplicate);
    };
    let item = item
        .as_object()
        .ok_or(BodyFrameParityFailure::TextDuplicate)?;
    if item.len() != 2 || item.get("type") != Some(&json!("text")) {
        return Err(BodyFrameParityFailure::TextDuplicate);
    }
    let text = item
        .get("text")
        .and_then(Value::as_str)
        .ok_or(BodyFrameParityFailure::TextDuplicate)?;
    let duplicate: Value =
        serde_json::from_str(text).map_err(|_| BodyFrameParityFailure::TextDuplicate)?;
    let canonical =
        serde_json::to_string(structured).map_err(|_| BodyFrameParityFailure::TextDuplicate)?;
    if duplicate != *structured || text != canonical {
        return Err(BodyFrameParityFailure::TextDuplicate);
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
fn body_protocol_frames_match(stable: &[Value; 2], preview: &[Value; 2]) -> bool {
    stable
        .iter()
        .zip(preview)
        .all(|(stable, preview)| compare_body_protocol_frame(stable, preview).is_ok())
}

#[cfg(feature = "acceptance-harness")]
fn body_scenario_callback_error(
    stage: DisposableCallbackStage,
    failure: BodyScenarioFailure,
) -> TestError {
    TestError::DisposableCallback {
        stage,
        category: failure.category(),
    }
}

#[cfg(feature = "acceptance-harness")]
fn run_direct_body_phase(
    ctx: &TestContext,
) -> BodyAcceptancePhaseFuture<'_, (BodyScenarioEvidence, Vec<Value>)> {
    Box::pin(async move {
        let direct = BodyAcceptanceDirect::new(ctx.client.clone(), false)
            .map_err(|_| sentinel_assertion("direct body driver construction failed"))?;
        let descriptors = direct
            .tool_descriptors()
            .map_err(|_| sentinel_assertion("direct body descriptors were not serializable"))?;
        let mut direct_driver = DirectBodyDriver { driver: direct };
        let evidence = run_body_scenario(&mut direct_driver, ctx, "direct")
            .await
            .map_err(|failure| {
                body_scenario_callback_error(DisposableCallbackStage::BodyDirect, failure)
            })?;
        Ok((evidence, descriptors))
    })
}

#[cfg(feature = "acceptance-harness")]
fn run_spawned_body_phase<'a>(
    ctx: &'a TestContext,
    cleanup: Arc<Mutex<ChildCleanupRecord>>,
    options: DriverOptions,
    transport: &'static str,
    parity_page_id: &'a str,
) -> BodyAcceptancePhaseFuture<'a, SpawnedBodyEvidence> {
    Box::pin(async move {
        let child = spawn_disposable_driver(ctx, cleanup, options, Some("body-blocks"))
            .map_err(|_| sentinel_assertion("spawned body child failed"))?;
        let (descriptors, frames) = {
            let mut guard = lock_driver(&child);
            let process = guard
                .as_mut()
                .ok_or_else(|| sentinel_assertion("spawned body child missing"))?;
            process.initialize();
            let descriptors = process
                .body_tool_descriptors_sync()
                .map_err(|_| sentinel_assertion("spawned body descriptors failed"))?;
            let frames = process.raw_body_parity_frames(&ctx.space_id, parity_page_id);
            (descriptors, frames)
        };
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        let callback_stage = if options.preview {
            DisposableCallbackStage::BodyStdioPreview
        } else {
            DisposableCallbackStage::BodyStdioStable
        };
        let scenario = run_body_scenario(&mut driver, ctx, transport)
            .await
            .map_err(|failure| body_scenario_callback_error(callback_stage, failure))?;
        let stale_cursor_frame = {
            let mut guard = lock_driver(&child);
            let process = guard
                .as_mut()
                .ok_or_else(|| sentinel_assertion("spawned body child missing after scenario"))?;
            let [frame] = std::mem::take(&mut process.body_tool_error_frames)
                .try_into()
                .map_err(|_| {
                    sentinel_assertion(
                        "spawned body scenario did not retain one stale-cursor frame",
                    )
                })?;
            frame
        };
        let (_, output) = take_registered_body_driver(&child)?
            .try_finish()
            .map_err(|_| sentinel_assertion("spawned shared body child did not stop"))?;
        require_body_diagnostics(&output.stderr, BODY_DIAGNOSTIC_SECRET.as_bytes(), true)?;
        Ok(SpawnedBodyEvidence {
            scenario,
            descriptors,
            frames,
            stale_cursor_frame,
        })
    })
}

#[cfg(feature = "acceptance-harness")]
fn run_spawned_read_only_body_phase<'a>(
    ctx: &'a TestContext,
    cleanup: Arc<Mutex<ChildCleanupRecord>>,
    options: DriverOptions,
    space_id: &'a str,
    object_id: &'a str,
) -> BodyAcceptancePhaseFuture<'a, BodyReadOnlyEvidence> {
    Box::pin(async move {
        let child = spawn_disposable_driver(ctx, cleanup, options, Some("body-blocks"))
            .map_err(|_| sentinel_assertion("read-only body child failed"))?;
        lock_driver(&child)
            .as_mut()
            .ok_or_else(|| sentinel_assertion("read-only body child missing"))?
            .initialize();
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        let callback_stage = if options.preview {
            DisposableCallbackStage::BodyReadOnlyPreview
        } else {
            DisposableCallbackStage::BodyReadOnlyStable
        };
        let evidence = run_body_read_only_scenario(&mut driver, space_id, object_id)
            .await
            .map_err(|_| {
                disposable_callback_error(
                    callback_stage,
                    sentinel_assertion("read-only body scenario failed"),
                )
            })?;
        let (_, output) = take_registered_body_driver(&child)?
            .try_finish()
            .map_err(|_| sentinel_assertion("read-only body child did not stop"))?;
        require_body_diagnostics(&output.stderr, b"SECRET_UNPARSED_BODY_VALUE", false)?;
        Ok(evidence)
    })
}

#[cfg(feature = "acceptance-harness")]
fn run_shared_body_callback(
    ctx: Arc<TestContext>,
    stable_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    preview_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    stable_read_only_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    preview_read_only_cleanup: Arc<Mutex<ChildCleanupRecord>>,
) -> SharedBodyCallbackFuture {
    Box::pin(async move {
        let parity_page = ctx
            .client
            .new_object(&ctx.space_id, "page")
            .name("Body protocol parity fixture")
            .body("Protocol parity body")
            .create()
            .await
            .map_err(|_| sentinel_assertion("body parity fixture page creation failed"))?;
        ctx.register_object(&parity_page.id);

        let (direct_evidence, direct_descriptors) = run_direct_body_phase(&ctx).await?;
        let stable = run_spawned_body_phase(
            &ctx,
            stable_cleanup,
            DriverOptions::STANDARD,
            "stable",
            &parity_page.id,
        )
        .await?;
        let preview = run_spawned_body_phase(
            &ctx,
            preview_cleanup,
            DriverOptions::PREVIEW_STANDARD,
            "preview",
            &parity_page.id,
        )
        .await?;
        let stable_read_only = run_spawned_read_only_body_phase(
            &ctx,
            stable_read_only_cleanup,
            DriverOptions::READ_ONLY,
            &ctx.space_id,
            &parity_page.id,
        )
        .await?;
        let preview_read_only = run_spawned_read_only_body_phase(
            &ctx,
            preview_read_only_cleanup,
            DriverOptions::PREVIEW_READ_ONLY,
            &ctx.space_id,
            &parity_page.id,
        )
        .await?;

        inspect_reviewed_body_server_log(&[
            BODY_DIAGNOSTIC_SECRET.as_bytes(),
            b"SECRET_UNPARSED_BODY_VALUE",
        ])?;
        if stable.descriptors != preview.descriptors || direct_descriptors != stable.descriptors {
            return Err(sentinel_assertion(
                "direct/stable/preview body descriptors, schemas, or annotations diverged",
            ));
        }
        if !body_protocol_frames_match(&stable.frames, &preview.frames)
            || compare_body_protocol_frame(&stable.stale_cursor_frame, &preview.stale_cursor_frame)
                .is_err()
            || stable.stale_cursor_frame.pointer("/result/isError") != Some(&Value::Bool(true))
            || stable
                .stale_cursor_frame
                .pointer("/result/structuredContent/code")
                .and_then(Value::as_str)
                != Some("conflict")
            || stable.frames[0]
                .pointer("/result/structuredContent/items")
                .and_then(Value::as_array)
                .is_none()
            || stable.frames[0]
                .pointer("/result/structuredContent/next_cursor")
                .is_some()
            || stable.frames[0].pointer("/result/isError") != Some(&Value::Bool(false))
            || stable.frames[1]
                .pointer("/error/code")
                .and_then(Value::as_i64)
                != Some(-32602)
            || stable.frames[1]
                .pointer("/error/message")
                .and_then(Value::as_str)
                != Some("Tool arguments do not match the declared schema.")
            || stable.frames[1]
                .pointer("/error/data/code")
                .and_then(Value::as_str)
                != Some("validation")
        {
            return Err(sentinel_assertion(
                "stable/preview raw body success or error JSON-RPC frames diverged",
            ));
        }
        if stable.scenario != preview.scenario {
            return Err(sentinel_assertion(
                "stable and preview normalized body result shapes diverged",
            ));
        }
        if direct_evidence != stable.scenario {
            return Err(sentinel_assertion(
                "direct and stdio normalized body result shapes diverged",
            ));
        }
        if stable_read_only != preview_read_only {
            return Err(sentinel_assertion(
                "stable and preview read-only body evidence diverged",
            ));
        }
        Ok(ctx.space_id.clone())
    })
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn shared_body_acceptance_futures_keep_only_heap_handles_inline() {
    let word = std::mem::size_of::<usize>();
    assert!(std::mem::size_of::<BodyAcceptancePhaseFuture<'static, ()>>() <= 2 * word);
    assert!(std::mem::size_of::<SharedBodyCallbackFuture>() <= 2 * word);
}

#[cfg(feature = "acceptance-harness")]
fn body_protocol_test_frame(id: u64, is_error: bool, structured: Value) -> Value {
    let text = serde_json::to_string(&structured).expect("test structured result");
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "result":{
            "content":[{"type":"text","text":text}],
            "structuredContent":structured,
            "isError":is_error
        }
    })
}

#[cfg(feature = "acceptance-harness")]
fn preview_body_protocol_test_frame(stable: &Value) -> Value {
    let mut preview = stable.clone();
    preview["result"]
        .as_object_mut()
        .expect("test result")
        .insert("resultType".to_owned(), json!("complete"));
    preview
}

#[cfg(feature = "acceptance-harness")]
fn body_protocol_error_frame(id: u64, code: i64, message: &str, data: Value) -> Value {
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{"code":code,"message":message,"data":data}
    })
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn body_raw_frame_parity_allows_only_preview_complete_result_type() {
    let success = body_protocol_test_frame(
        3,
        false,
        json!({
            "space_id":"space-exact",
            "object_id":"object-exact",
            "root_id":"root-exact",
            "snapshot_hash":"hash-exact",
            "items":[{
                "id":"root-exact",
                "content":{
                    "kind":"unsupported",
                    "opaque_kind":"page",
                    "child_count":1,
                    "approx_bytes":917
                }
            }]
        }),
    );
    let error = body_protocol_error_frame(
        4,
        -32602,
        "Tool arguments do not match the declared schema.",
        json!({"code":"validation"}),
    );
    let preview_success = preview_body_protocol_test_frame(&success);
    let preview_error = error.clone();
    let tool_error = body_protocol_test_frame(
        5,
        true,
        json!({"code":"conflict","message":"The requested state changed. Refresh and retry."}),
    );
    let preview_tool_error = preview_body_protocol_test_frame(&tool_error);

    assert_eq!(
        compare_body_protocol_frame(&success, &preview_success),
        Ok(())
    );
    assert_eq!(compare_body_protocol_frame(&error, &preview_error), Ok(()));
    assert_eq!(
        compare_body_protocol_frame(&tool_error, &preview_tool_error),
        Ok(())
    );
    assert!(body_protocol_frames_match(
        &[success, error],
        &[preview_success, preview_error]
    ));
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn body_raw_frame_parity_rejects_protocol_shape_and_payload_drift() {
    let success = body_protocol_test_frame(
        3,
        false,
        json!({
            "space_id":"space-exact",
            "object_id":"object-exact",
            "root_id":"root-exact",
            "snapshot_hash":"hash-exact",
            "items":[{
                "id":"root-exact",
                "content":{
                    "kind":"unsupported",
                    "opaque_kind":"page",
                    "child_count":1,
                    "approx_bytes":917
                }
            }]
        }),
    );
    let preview = preview_body_protocol_test_frame(&success);
    let mutate = |path: &str, value: Value| {
        let mut candidate = preview.clone();
        *candidate.pointer_mut(path).expect("test mutation path") = value;
        candidate
    };
    let payload_candidate = |path: &str, value: Value| {
        let mut structured = success["result"]["structuredContent"].clone();
        *structured.pointer_mut(path).expect("test payload path") = value;
        preview_body_protocol_test_frame(&body_protocol_test_frame(3, false, structured))
    };

    let mut missing_type = preview.clone();
    missing_type["result"]
        .as_object_mut()
        .expect("test result")
        .remove("resultType");
    assert_eq!(
        compare_body_protocol_frame(&success, &missing_type),
        Err(BodyFrameParityFailure::PreviewResultType)
    );
    for value in [json!("partial"), Value::Null] {
        assert_eq!(
            compare_body_protocol_frame(&success, &mutate("/result/resultType", value)),
            Err(BodyFrameParityFailure::PreviewResultType)
        );
    }

    let mut stable_with_type = success.clone();
    stable_with_type["result"]
        .as_object_mut()
        .expect("test result")
        .insert("resultType".to_owned(), json!("complete"));
    assert_eq!(
        compare_body_protocol_frame(&stable_with_type, &preview),
        Err(BodyFrameParityFailure::StableResultType)
    );
    assert_eq!(
        compare_body_protocol_frame(&success, &mutate("/id", json!(9))),
        Err(BodyFrameParityFailure::EnvelopeId)
    );
    assert_eq!(
        compare_body_protocol_frame(&success, &mutate("/jsonrpc", json!("1.0"))),
        Err(BodyFrameParityFailure::EnvelopeVersion)
    );

    let mut envelope_drift = preview.clone();
    envelope_drift
        .as_object_mut()
        .expect("test envelope")
        .insert("extra".to_owned(), json!(true));
    assert_eq!(
        compare_body_protocol_frame(&success, &envelope_drift),
        Err(BodyFrameParityFailure::EnvelopeShape)
    );
    let mut result_drift = preview.clone();
    result_drift["result"]
        .as_object_mut()
        .expect("test result")
        .insert("extra".to_owned(), json!(true));
    assert_eq!(
        compare_body_protocol_frame(&success, &result_drift),
        Err(BodyFrameParityFailure::ResultShape)
    );

    let invalid_duplicate = mutate(
        "/result/content/0/text",
        json!("{\"snapshot_hash\":\"different\"}"),
    );
    assert_eq!(
        compare_body_protocol_frame(&success, &invalid_duplicate),
        Err(BodyFrameParityFailure::TextDuplicate)
    );
    let mut invalid_stable_duplicate = success.clone();
    invalid_stable_duplicate["result"]["content"][0]["text"] = json!("{}");
    let invalid_preview_duplicate = preview_body_protocol_test_frame(&invalid_stable_duplicate);
    assert_eq!(
        compare_body_protocol_frame(&invalid_stable_duplicate, &invalid_preview_duplicate),
        Err(BodyFrameParityFailure::TextDuplicate)
    );
    let mut cursor_structured = success["result"]["structuredContent"].clone();
    cursor_structured
        .as_object_mut()
        .expect("test structured result")
        .insert("next_cursor".to_owned(), json!("cursor-must-remain-exact"));
    let cursor_drift =
        preview_body_protocol_test_frame(&body_protocol_test_frame(3, false, cursor_structured));

    for candidate in [
        payload_candidate("/snapshot_hash", json!("different")),
        payload_candidate("/object_id", json!("different")),
        payload_candidate("/root_id", json!("different")),
        payload_candidate("/items/0/id", json!("different")),
        payload_candidate("/items/0/content/approx_bytes", json!(918)),
        cursor_drift,
    ] {
        assert_eq!(
            compare_body_protocol_frame(&success, &candidate),
            Err(BodyFrameParityFailure::Payload)
        );
    }
    assert_eq!(
        compare_body_protocol_frame(&success, &mutate("/result/isError", json!(true))),
        Err(BodyFrameParityFailure::ToolErrorShape)
    );

    let error = body_protocol_error_frame(
        4,
        -32602,
        "Tool arguments do not match the declared schema.",
        json!({"code":"validation"}),
    );
    let preview_error = error.clone();
    let error_code_drift = body_protocol_error_frame(
        4,
        -32603,
        "Tool arguments do not match the declared schema.",
        json!({"code":"validation"}),
    );
    assert_eq!(
        compare_body_protocol_frame(&error, &error_code_drift),
        Err(BodyFrameParityFailure::ErrorCode)
    );
    let error_message_drift =
        body_protocol_error_frame(4, -32602, "Invalid params", json!({"code":"validation"}));
    assert_eq!(
        compare_body_protocol_frame(&error, &error_message_drift),
        Err(BodyFrameParityFailure::ErrorMessage)
    );
    let error_data_drift = body_protocol_error_frame(
        4,
        -32602,
        "Tool arguments do not match the declared schema.",
        json!({"code":"upstream"}),
    );
    assert_eq!(
        compare_body_protocol_frame(&error, &error_data_drift),
        Err(BodyFrameParityFailure::ErrorData)
    );
    let mut error_shape_drift = preview_error.clone();
    error_shape_drift["error"]
        .as_object_mut()
        .expect("test error")
        .insert("extra".to_owned(), json!(true));
    assert_eq!(
        compare_body_protocol_frame(&error, &error_shape_drift),
        Err(BodyFrameParityFailure::ErrorShape)
    );
    let mut both_stable = success.clone();
    both_stable
        .as_object_mut()
        .expect("test envelope")
        .insert("error".to_owned(), error["error"].clone());
    let both_preview = preview_body_protocol_test_frame(&both_stable);
    assert_eq!(
        compare_body_protocol_frame(&both_stable, &both_preview),
        Err(BodyFrameParityFailure::OutcomeShape)
    );
    let neither = json!({"jsonrpc":"2.0","id":3});
    assert_eq!(
        compare_body_protocol_frame(&neither, &neither),
        Err(BodyFrameParityFailure::OutcomeShape)
    );
    assert!(compare_body_protocol_frame(&success, &preview_error).is_err());
    assert!(!body_protocol_frames_match(
        &[success, error],
        &[preview_error, preview]
    ));

    let tool_error = body_protocol_test_frame(
        5,
        true,
        json!({
            "code":"conflict",
            "message":"The requested state changed. Refresh and retry.",
            "candidates":[{"id":"candidate-1","name":"Candidate"}]
        }),
    );
    let preview_tool_error = preview_body_protocol_test_frame(&tool_error);
    assert_eq!(
        compare_body_protocol_frame(&tool_error, &preview_tool_error),
        Ok(())
    );
    let tool_error_mutate = |path: &str, value: Value| {
        let mut candidate = preview_tool_error.clone();
        *candidate
            .pointer_mut(path)
            .expect("tool-error mutation path") = value;
        candidate
    };
    for candidate in [
        tool_error_mutate("/result/isError", json!(false)),
        tool_error_mutate("/result/structuredContent/code", json!("upstream")),
        tool_error_mutate("/result/structuredContent/message", json!("different")),
        tool_error_mutate(
            "/result/structuredContent/candidates/0/id",
            json!("candidate-2"),
        ),
        tool_error_mutate(
            "/result/content/0/text",
            json!("{\"code\":\"conflict\",\"message\":\"noncanonical\"}"),
        ),
    ] {
        assert!(compare_body_protocol_frame(&tool_error, &candidate).is_err());
    }

    for malformed in [
        body_protocol_test_frame(5, true, json!({"code":"","message":"message"})),
        body_protocol_test_frame(5, true, json!({"code":"conflict","message":""})),
        body_protocol_test_frame(
            5,
            true,
            json!({"code":"conflict","message":"message","candidates":[]}),
        ),
        body_protocol_test_frame(
            5,
            true,
            json!({
                "code":"conflict",
                "message":"message",
                "candidates":[{"id":"x".repeat(257),"name":"Candidate"}]
            }),
        ),
        body_protocol_test_frame(
            5,
            true,
            json!({
                "code":"conflict",
                "message":"message",
                "candidates":[{"id":"candidate-1","name":"x".repeat(257)}]
            }),
        ),
        body_protocol_test_frame(
            5,
            true,
            json!({"code":"conflict","message":"message","extra":true}),
        ),
        body_protocol_test_frame(
            5,
            true,
            json!({
                "code":"conflict",
                "message":"message",
                "candidates":[{"id":"","name":"Candidate"}]
            }),
        ),
    ] {
        let malformed_preview = preview_body_protocol_test_frame(&malformed);
        assert_eq!(
            compare_body_protocol_frame(&malformed, &malformed_preview),
            Err(BodyFrameParityFailure::ToolErrorShape)
        );
    }
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_body_blocks_shared_direct_stable_preview_scenarios() {
    let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_read_only_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_read_only_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_callback_cleanup = Arc::clone(&stable_cleanup);
    let preview_callback_cleanup = Arc::clone(&preview_cleanup);
    let stable_read_only_callback_cleanup = Arc::clone(&stable_read_only_cleanup);
    let preview_read_only_callback_cleanup = Arc::clone(&preview_read_only_cleanup);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-body-shared-stdio",
        move |ctx| {
            run_shared_body_callback(
                ctx,
                stable_callback_cleanup,
                preview_callback_cleanup,
                stable_read_only_callback_cleanup,
                preview_read_only_callback_cleanup,
            )
        },
    ))
    .await
    .expect("cleanup-safe shared stable/preview body scenario");
    if let DisposableRun::Completed(space_id) = outcome {
        assert_eq!(
            *stable_cleanup.lock().expect("stable cleanup record"),
            ChildCleanupRecord::Stopped
        );
        assert_eq!(
            *preview_cleanup.lock().expect("preview cleanup record"),
            ChildCleanupRecord::Stopped
        );
        assert_eq!(
            *stable_read_only_cleanup
                .lock()
                .expect("stable read-only cleanup record"),
            ChildCleanupRecord::Stopped
        );
        assert_eq!(
            *preview_read_only_cleanup
                .lock()
                .expect("preview read-only cleanup record"),
            ChildCleanupRecord::Stopped
        );
        assert_fresh_space_absence(&space_id).await;
    }
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

    const RUN_MARKER: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const REVIEWED_EVENT: &str = "{\"timestamp\":\"2026-07-23T00:00:00Z\",\"severity\":\"info\",\"component\":\"anytype\",\"category\":\"body_acceptance\"}";

    fn write_reviewed_log(name: &str, contents: &[u8]) -> PathBuf {
        let path = temporary_path(name);
        std::fs::write(&path, contents).expect("write reviewed log fixture");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .expect("set private reviewed log permissions");
        }
        path
    }

    fn inspect_log(path: &Path, marker: Option<&str>, credentials_absent: bool) -> TestResult<()> {
        inspect_reviewed_body_server_log_at(Some(path.as_os_str().to_owned()), marker, &[], |_| {
            credentials_absent
        })
    }

    #[test]
    fn body_server_log_inspection_fails_closed_when_path_is_missing() {
        assert!(
            inspect_reviewed_body_server_log_at(None, Some(RUN_MARKER), &[], |_| true).is_err()
        );
    }

    #[test]
    fn body_server_log_requires_private_current_allowlisted_evidence() {
        let valid = write_reviewed_log(
            "reviewed-valid.log",
            format!("{REVIEWED_EVENT}\nany-mcp-run-marker={RUN_MARKER}\n").as_bytes(),
        );
        assert!(inspect_log(&valid, Some(RUN_MARKER), true).is_ok());
        assert!(
            inspect_reviewed_body_server_log_at(
                Some(valid.as_os_str().to_owned()),
                Some(RUN_MARKER),
                &[b"".as_slice()],
                |_| true,
            )
            .is_ok()
        );
        assert!(
            inspect_reviewed_body_server_log_at(
                Some(valid.as_os_str().to_owned()),
                Some(RUN_MARKER),
                &[b"body_acceptance".as_slice()],
                |_| true,
            )
            .is_err()
        );
        assert!(inspect_log(&valid, None, true).is_err());
        assert!(inspect_log(&valid, Some(&"a".repeat(63)), true).is_err());
        assert!(inspect_log(&valid, Some(RUN_MARKER), false).is_err());

        for (name, contents) in [
            ("reviewed-empty.log", "".to_owned()),
            ("reviewed-arbitrary.log", "arbitrary\n".to_owned()),
            ("reviewed-no-marker.log", format!("{REVIEWED_EVENT}\n")),
            (
                "reviewed-no-event.log",
                format!("any-mcp-run-marker={RUN_MARKER}\n"),
            ),
            (
                "reviewed-duplicate-marker.log",
                format!(
                    "{REVIEWED_EVENT}\nany-mcp-run-marker={RUN_MARKER}\nany-mcp-run-marker={RUN_MARKER}\n"
                ),
            ),
            (
                "reviewed-unknown-field.log",
                format!(
                    "{{\"severity\":\"info\",\"component\":\"anytype\",\"body\":\"forbidden\"}}\nany-mcp-run-marker={RUN_MARKER}\n"
                ),
            ),
        ] {
            let path = write_reviewed_log(name, contents.as_bytes());
            assert!(
                inspect_log(&path, Some(RUN_MARKER), true).is_err(),
                "accepted {name}"
            );
            let _ = std::fs::remove_file(path);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            std::fs::set_permissions(&valid, std::fs::Permissions::from_mode(0o640))
                .expect("set unsafe reviewed log permissions");
            assert!(inspect_log(&valid, Some(RUN_MARKER), true).is_err());
        }
        let _ = std::fs::remove_file(valid);

        let directory = temporary_path("reviewed-directory");
        std::fs::create_dir(&directory).expect("create non-file fixture");
        assert!(inspect_log(&directory, Some(RUN_MARKER), true).is_err());
        let _ = std::fs::remove_dir(directory);
    }

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
            "scenario=standard_discovery process_category={} status={} cleanup={} stderr_metrics={}",
            failure.category,
            failure.output.exit_category,
            if cleanup_finalizer_ran.load(Ordering::SeqCst) {
                "success"
            } else {
                "failed"
            },
            metrics.summary()
        );
        assert!(report.contains("process_category=child_eof"));
        assert!(report.contains("status=exit_code"));
        assert!(report.contains("cleanup=success"));
        assert!(report.contains("other="));
        for secret in [HTTP_TOKEN, CIPHER, BODY] {
            assert!(!report.contains(secret));
        }
    }

    #[test]
    fn shipped_binary_accepts_views_write_selector_before_authentication() {
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp"));
        command
            .env("ANY_MCP_TOOLSETS", "views-write")
            .env("ANYTYPE_KEYSTORE", "env")
            .env("ANYTYPE_KEYSTORE_SERVICE", "views-write-link-process-test")
            .env_remove("ANYTYPE_KEY_HTTP_TOKEN")
            .env_remove("ANYTYPE_KEY_ACCOUNT_ID")
            .env_remove("ANYTYPE_KEY_ACCOUNT_KEY")
            .env_remove("ANYTYPE_KEY_SESSION_TOKEN")
            .env_remove("ANY_MCP_PROTOCOL");
        let output = command.output().expect("run shipped any-mcp binary");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("HTTP credentials are missing"));
        assert!(!stderr.contains("views-write"));
        assert!(!stderr.contains("unsupported optional toolset selector"));
    }
}
