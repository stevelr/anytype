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
        with_disposable_space_context,
    },
};
#[cfg(feature = "acceptance-harness")]
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod support;

#[cfg(feature = "acceptance-harness")]
use support::live_scenario::{
    ACCEPTANCE_TRANSFER_CHUNK_BYTES, ADVERSARIAL_DYNAMIC_STDIO_IMPLEMENTED_IDS,
    ADVERSARIAL_STDIO_SENTINEL_IDS, ARTIFACT_FILE_MEDIA_TYPE, ARTIFACT_FILE_PAYLOAD,
    ARTIFACT_FRAME_CEILING_BYTES, ARTIFACT_TOOL_NAMES, AdversarialCaseId, AdversarialExecution,
    ArtifactAdversarialRun, ArtifactContentEvidence, ArtifactContentRun, ArtifactContentScenario,
    ArtifactControlPlane, ArtifactDataPlane, ArtifactFrameMeasurement, ArtifactGateHooks,
    ArtifactGateLease, ArtifactLifecycleScenario, ArtifactPolicyEvidence, ArtifactPolicyFixture,
    ArtifactPolicyOptions, ArtifactPolicyRun, ArtifactPolicyScenario, ArtifactServerLogAudit,
    ArtifactServerLogBaseline, ArtifactSmokeFixture, ArtifactStageAllocation,
    ArtifactStartupCaseOutcome, ArtifactSymlinkStartupTarget, ArtifactTransport, ExpectedOutcome,
    FixtureValidatorPolicy, ObservedOutcome, allocate_stage_upload, artifact_catalog_snapshot,
    artifact_sha256, assert_artifact_content_parity, assert_artifact_parity,
    assert_artifact_policy_parity, assert_payload_frame_independence, audit_server_log,
    classify_collision_frames, measure_artifact_frame, prepare_artifact_symlink_startup_case,
    record_artifact_dynamic_filesystem_startup_cases, reject_oversized_stage_chunk,
    release_stage_upload, require_completed, run_artifact_adversarial_stdio_sentinels,
    run_artifact_content_scenario, run_artifact_dynamic_filesystem_stdio_sentinels,
    run_artifact_policy_scenario, run_artifact_race01, run_artifact_race04,
    run_artifact_smoke_scenario, server_log_baseline, stage_head_status, upload_stage_bytes,
    validate_tool_frame, wait_for_stage_reaped,
};
#[cfg(feature = "acceptance-harness")]
use support::live_scenario::{
    BODY_DIAGNOSTIC_SECRET, BodyReadOnlyEvidence, BodyScenarioEvidence, BodyScenarioFailure,
    run_body_read_only_scenario, run_body_scenario,
};
#[cfg(feature = "acceptance-harness")]
use support::live_scenario::{
    BodyDriverMetrics, OPTIONAL_LIVE_OWNERSHIP, OptionalEvidenceTier, OptionalExecutableWorkflow,
    OptionalFastWorkflow, OptionalOperation, OptionalRealWorkflow, OptionalRegistry,
};
#[cfg(feature = "acceptance-harness")]
use support::process::{MAX_STDOUT_BYTES, MidFramePause};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionalRealWorkflowRun {
    Executed,
    Skipped,
}

#[cfg(feature = "acceptance-harness")]
fn require_optional_workflow_executed(
    outcome: OptionalRealWorkflowRun,
) -> Result<(), &'static str> {
    match outcome {
        OptionalRealWorkflowRun::Executed => Ok(()),
        OptionalRealWorkflowRun::Skipped => Err("required real-headless workflow was skipped"),
    }
}

#[cfg(feature = "acceptance-harness")]
type OptionalRealWorkflowFuture = Pin<Box<dyn Future<Output = OptionalRealWorkflowRun> + 'static>>;

#[cfg(feature = "acceptance-harness")]
type OptionalRealWorkflowRunner = fn() -> OptionalRealWorkflowFuture;

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Copy)]
struct OptionalRealWorkflowRegistration {
    workflow: OptionalRealWorkflow,
    runner: OptionalRealWorkflowRunner,
}

#[cfg(feature = "acceptance-harness")]
impl OptionalRealWorkflowRegistration {
    async fn run(self) -> OptionalRealWorkflowRun {
        (self.runner)().await
    }
}

#[cfg(feature = "acceptance-harness")]
fn artifacts_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_artifacts_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn body_blocks_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_body_blocks_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn chats_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_chats_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn files_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_files_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn members_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_members_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn schema_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_schema_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
fn views_write_real_runner() -> OptionalRealWorkflowFuture {
    Box::pin(run_views_write_real_workflow())
}

#[cfg(feature = "acceptance-harness")]
const OPTIONAL_REAL_WORKFLOWS: [OptionalRealWorkflowRegistration; 7] = [
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::Artifacts,
        runner: artifacts_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::BodyBlocks,
        runner: body_blocks_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::Chats,
        runner: chats_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::Files,
        runner: files_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::Members,
        runner: members_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::Schema,
        runner: schema_real_runner,
    },
    OptionalRealWorkflowRegistration {
        workflow: OptionalRealWorkflow::ViewsWrite,
        runner: views_write_real_runner,
    },
];

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
    "rich_page_resume",
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

    #[cfg(feature = "acceptance-harness")]
    fn spawn_paused_in_second_frame(
        command: Command,
        options: DriverOptions,
    ) -> (Self, MidFramePause) {
        let (process, pause) =
            ProtocolProcess::spawn_paused_in_second_frame(command, Duration::from_secs(30));
        (
            Self {
                process,
                next_id: 1,
                options,
                body_tool_error_frames: Vec::new(),
                _keystore: None,
            },
            pause,
        )
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

    /// Issues one `tools/call` frame and validates its complete envelope.
    #[cfg(feature = "acceptance-harness")]
    fn scripted_tool_frame(&mut self, name: &str, arguments: Value) -> Result<Value, String> {
        let id = self.next_id;
        let frame = self.request("tools/call", json!({"name": name, "arguments": arguments}));
        validate_tool_frame(name, id, &frame)
    }

    /// Issues and measures one complete artifact `tools/call` response frame.
    #[cfg(feature = "acceptance-harness")]
    fn measured_tool_frame(
        &mut self,
        name: &'static str,
        arguments: Value,
    ) -> Result<ArtifactFrameMeasurement, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut params = json!({"name": name, "arguments": arguments});
        if self.options.preview {
            params
                .as_object_mut()
                .ok_or_else(|| "preview measured-tool params were not an object".to_owned())?
                .insert("_meta".to_owned(), preview_meta());
        }
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":params
        }));
        let frame = self.process.read_frame_bytes();
        let parsed: Value = serde_json::from_slice(&frame[..frame.len().saturating_sub(1)])
            .map_err(|_| "measured artifact response was not JSON".to_owned())?;
        if parsed["id"].as_u64() != Some(id) {
            return Err("measured artifact response carried a mismatched identifier".to_owned());
        }
        self.process.record_response(&parsed);
        measure_artifact_frame(name, id, &frame)
    }

    /// Dispatches two concurrent calls and returns their request identifiers and frames.
    #[cfg(feature = "acceptance-harness")]
    fn collision_tool_frames(
        &mut self,
        name: &'static str,
        first: Value,
        second: Value,
    ) -> ([u64; 2], [Value; 2]) {
        let first_id = self.next_id;
        let ids = [first_id, first_id.saturating_add(1)];
        let frames = self.request_pair(
            "tools/call",
            json!({"name": name, "arguments": first}),
            json!({"name": name, "arguments": second}),
        );
        (ids, frames)
    }

    /// Cancels one in-flight tool call and proves the server remains responsive.
    #[cfg(feature = "acceptance-harness")]
    fn cancel_tool_call(
        &mut self,
        name: &'static str,
        arguments: Value,
        gate: &ChildArtifactGate,
    ) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut params = json!({"name": name, "arguments": arguments});
        if self.options.preview {
            params
                .as_object_mut()
                .expect("preview tool params object")
                .insert("_meta".to_owned(), preview_meta());
        }
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":params
        }));
        gate.wait_ready()
            .map_err(|_| "artifact cancellation never reached its gate".to_owned())?;
        self.process.notification(
            "notifications/cancelled",
            json!({"requestId": id, "reason": "artifact acceptance cancellation"}),
        );
        gate.release()
            .map_err(|_| "artifact cancellation did not release the paused operation".to_owned())?;
        gate.wait_done()
            .map_err(|_| "artifact cancellation did not settle the paused operation".to_owned())?;
        let ping_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":ping_id,
            "method":"ping",
            "params":{}
        }));
        let first = self.process.read_frame();
        self.process.record_response(&first);
        let ping = if first["id"].as_u64() == Some(ping_id) {
            first
        } else {
            let second = self.process.read_frame();
            self.process.record_response(&second);
            let [cancelled, ping] = correlate_response_pair([id, ping_id], [first, second])?;
            if cancelled["result"]["isError"].as_bool() != Some(true)
                || cancelled
                    .pointer("/result/structuredContent/code")
                    .and_then(Value::as_str)
                    != Some("conflict")
            {
                return Err(
                    "artifact cancellation did not return the fixed conflict result".to_owned(),
                );
            }
            ping
        };
        if ping["result"] != json!({}) {
            return Err("artifact child did not respond after cancellation".to_owned());
        }
        Ok(id)
    }

    /// Sends one `tools/call` frame without reading a response, for crash
    /// scenarios that kill the child while the call is paused at a gate.
    #[cfg(feature = "acceptance-harness")]
    fn send_tool_call_only(&mut self, name: &'static str, arguments: Value) {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        }));
    }

    /// Cancels one gated call and proves the server remains responsive.
    ///
    /// Per the MCP cancellation contract the cancelled request itself should
    /// receive no response frame; when production does answer anyway (the
    /// cancellation raced completion) the response must be the fixed conflict
    /// result. The case invariants are asserted separately by each owner.
    #[cfg(feature = "acceptance-harness")]
    fn cancel_tool_call_exact(
        &mut self,
        name: &'static str,
        arguments: Value,
        gate: &ChildArtifactGate,
    ) -> Result<(), String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        }));
        gate.wait_ready()
            .map_err(|_| "exact artifact cancellation never reached its gate".to_owned())?;
        self.process.notification(
            "notifications/cancelled",
            json!({"requestId": id, "reason": "exact artifact acceptance cancellation"}),
        );
        gate.release()
            .map_err(|_| "exact artifact cancellation did not release its gate".to_owned())?;
        gate.wait_done()
            .map_err(|_| "exact artifact cancellation did not settle its gate".to_owned())?;
        let ping_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.process.send(json!({
            "jsonrpc":"2.0",
            "id":ping_id,
            "method":"ping",
            "params":{}
        }));
        let first = self.process.read_frame();
        self.process.record_response(&first);
        let ping = if first["id"].as_u64() == Some(ping_id) {
            first
        } else {
            let second = self.process.read_frame();
            self.process.record_response(&second);
            let [cancelled, ping] = correlate_response_pair([id, ping_id], [first, second])?;
            let result = cancelled
                .get("result")
                .ok_or_else(|| "exact artifact cancellation omitted its tool result".to_owned())?;
            let evidence = ToolErrorEvidence::from_result(result, false)?;
            if evidence.code() != "conflict" {
                return Err("exact artifact cancellation did not return conflict".to_owned());
            }
            ping
        };
        if ping["result"] != json!({}) {
            return Err("artifact child did not respond after exact cancellation".to_owned());
        }
        Ok(())
    }

    fn list_tool_descriptors_sync(&mut self) -> Result<Vec<Value>, String> {
        let response = self.request("tools/list", json!({}));
        response["result"]["tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| "tools/list omitted tools".to_owned())
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

    #[cfg(feature = "acceptance-harness")]
    fn terminate(self) -> Result<(String, ProcessOutput), String> {
        let transcript = self.process.redacted_transcript();
        self.process.terminate().map(|output| (transcript, output))
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

#[cfg(feature = "acceptance-harness")]
fn correlate_response_pair(ids: [u64; 2], responses: [Value; 2]) -> Result<[Value; 2], String> {
    let [first, second] = responses;
    match (first["id"].as_u64(), second["id"].as_u64()) {
        (Some(id), Some(other)) if id == ids[0] && other == ids[1] => Ok([first, second]),
        (Some(id), Some(other)) if id == ids[1] && other == ids[0] => Ok([second, first]),
        _ => Err("paired responses did not match the two outstanding requests".to_owned()),
    }
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn cancellation_response_pair_is_correlated_in_either_arrival_order() {
    let cancelled = json!({
        "jsonrpc": "2.0",
        "id": 41,
        "result": {"isError": true, "structuredContent": {"code": "conflict"}}
    });
    let ping = json!({"jsonrpc": "2.0", "id": 42, "result": {}});

    let ordered = correlate_response_pair([41, 42], [ping.clone(), cancelled.clone()])
        .expect("reverse arrival order is correlated");
    assert_eq!(ordered, [cancelled, ping]);
    assert!(
        correlate_response_pair(
            [41, 42],
            [
                json!({"jsonrpc": "2.0", "id": 41, "result": {}}),
                json!({"jsonrpc": "2.0", "id": 43, "result": {}}),
            ],
        )
        .is_err(),
        "an unrelated response must not be left queued"
    );
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

    fn list_tool_descriptors<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, String>> + 'a>> {
        Box::pin(std::future::ready(self.list_tool_descriptors_sync()))
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

    /// Runs one blocking driver transaction on the blocking pool so awaiting
    /// callers (gate-race `select!` arms in particular) never stall the async
    /// executor. A driver panic (transport deadline) resumes on the awaiting
    /// task to preserve the pre-existing panic contract.
    fn drive<'a, T: Send + 'static>(
        &'a mut self,
        operation: impl FnOnce(&mut StdioDriver) -> T + Send + 'static,
    ) -> Pin<Box<dyn Future<Output = T> + 'a>> {
        let driver = Arc::clone(&self.driver);
        Box::pin(async move {
            let joined = tokio::task::spawn_blocking(move || {
                let mut driver = lock_driver(&driver);
                operation(
                    driver
                        .as_mut()
                        .expect("registered stdio child remains owned"),
                )
            })
            .await;
            match joined {
                Ok(result) => result,
                Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
                Err(_) => panic!("stdio driver task cancelled"),
            }
        })
    }
}

impl McpDriver for OwnedStdioDriver {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        self.drive(move |driver| driver.call_tool_sync(name, arguments))
    }

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolErrorEvidence, String>> + 'a>> {
        self.drive(move |driver| driver.call_tool_error_sync(name, arguments))
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        self.drive(StdioDriver::list_tools_sync)
    }

    fn list_tool_descriptors<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, String>> + 'a>> {
        self.drive(StdioDriver::list_tool_descriptors_sync)
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        self.drive(StdioDriver::list_resources_sync)
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        self.drive(StdioDriver::list_resource_templates_sync)
    }

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let uri = uri.to_owned();
        self.drive(move |driver| driver.read_resource_sync(&uri))
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
        std::env::var_os("ANY_MCP_HEADLESS_EVIDENCE_CONTEXT"),
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
    context_path: Option<OsString>,
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
    let Some(context_path) = context_path.map(PathBuf::from) else {
        return Err(sentinel_assertion(
            "reviewed headless server-log context was not configured",
        ));
    };
    if !path.is_absolute() || !context_path.is_absolute() {
        return Err(sentinel_assertion(
            "reviewed headless server-log paths were not absolute",
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
    let mut context_options = std::fs::OpenOptions::new();
    context_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        context_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let context_file = context_options
        .open(&context_path)
        .map_err(|_| sentinel_assertion("reviewed headless server-log context was unreadable"))?;
    let context_metadata = context_file
        .metadata()
        .map_err(|_| sentinel_assertion("reviewed headless server-log context was unreadable"))?;
    if !context_metadata.file_type().is_file() {
        return Err(sentinel_assertion(
            "reviewed headless server-log context was not a regular file",
        ));
    }
    if context_metadata.len() > 4096 {
        return Err(sentinel_assertion(
            "reviewed headless server-log context was oversized",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        // SAFETY: `geteuid` has no preconditions and does not dereference
        // pointers or mutate process state.
        let effective_uid = unsafe { libc::geteuid() };
        if context_metadata.permissions().mode() & 0o777 != 0o600
            || context_metadata.uid() != effective_uid
        {
            return Err(sentinel_assertion(
                "reviewed headless server-log context ownership or permissions were unsafe",
            ));
        }
    }
    use std::io::Read as _;
    let mut context = String::new();
    context_file
        .take(4097)
        .read_to_string(&mut context)
        .map_err(|_| sentinel_assertion("reviewed headless server-log context was unreadable"))?;
    if context.len() > 4096 {
        return Err(sentinel_assertion(
            "reviewed headless server-log context was oversized",
        ));
    }
    let context = parse_reviewed_evidence_context(&context)?;
    if context.run_marker != marker {
        return Err(sentinel_assertion(
            "reviewed headless server-log marker did not match its context",
        ));
    }
    let mut source_options = std::fs::OpenOptions::new();
    source_options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        source_options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = source_options
        .open(&path)
        .map_err(|_| sentinel_assertion("reviewed headless server log was unreadable"))?;
    let metadata = file
        .metadata()
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
        if metadata.dev() != context.start_device
            || metadata.ino() != context.start_inode
            || metadata.len() < context.start_bytes
        {
            return Err(sentinel_assertion(
                "reviewed headless server log identity or size changed",
            ));
        }
    }
    #[cfg(not(unix))]
    return Err(sentinel_assertion(
        "reviewed headless server log identity requires Unix metadata",
    ));
    #[cfg(unix)]
    {
        let mut file = file;
        let log = {
            use std::io::{Read, Seek, SeekFrom};

            if context.anchor_length > 4096
                || context.anchor_start.saturating_add(context.anchor_length) != context.start_bytes
            {
                return Err(sentinel_assertion(
                    "reviewed headless server-log anchor bounds were invalid",
                ));
            }
            file.seek(SeekFrom::Start(context.anchor_start))
                .map_err(|_| {
                    sentinel_assertion("reviewed headless server-log anchor was unreadable")
                })?;
            let anchor_length = usize::try_from(context.anchor_length).map_err(|_| {
                sentinel_assertion("reviewed headless server-log anchor was too large")
            })?;
            let mut anchor = vec![0; anchor_length];
            file.read_exact(&mut anchor).map_err(|_| {
                sentinel_assertion("reviewed headless server-log anchor was unreadable")
            })?;
            if file_sha256(&anchor) != context.anchor_hash {
                return Err(sentinel_assertion(
                    "reviewed headless server-log pre-start anchor changed",
                ));
            }
            file.seek(SeekFrom::Start(context.start_bytes))
                .map_err(|_| {
                    sentinel_assertion("reviewed headless server-log window was unreadable")
                })?;
            let mut log = Vec::new();
            file.take(524_289).read_to_end(&mut log).map_err(|_| {
                sentinel_assertion("reviewed headless server-log window was unreadable")
            })?;
            log
        };
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
        let mut event_count = 0usize;
        for line in std::str::from_utf8(&log)
            .map_err(|_| sentinel_assertion("reviewed headless server log was not UTF-8"))?
            .lines()
        {
            if line.is_empty() {
                continue;
            } else if reviewed_server_event_line(line) {
                event_count = event_count.saturating_add(1);
            } else {
                return Err(sentinel_assertion(
                    "reviewed headless server log contained a non-allowlisted line",
                ));
            }
        }
        if event_count == 0 {
            return Err(sentinel_assertion(
                "reviewed headless server log lacked current-run provenance or events",
            ));
        }
        Ok(())
    }
}

struct ReviewedEvidenceContext<'a> {
    run_marker: &'a str,
    start_device: u64,
    start_inode: u64,
    start_bytes: u64,
    anchor_start: u64,
    anchor_length: u64,
    anchor_hash: &'a str,
}

fn parse_reviewed_evidence_context(contents: &str) -> TestResult<ReviewedEvidenceContext<'_>> {
    let mut run_marker = None;
    let mut start_device = None;
    let mut start_inode = None;
    let mut start_bytes = None;
    let mut anchor_start = None;
    let mut anchor_length = None;
    let mut anchor_hash = None;
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return Err(sentinel_assertion(
                "reviewed headless server-log context was invalid",
            ));
        };
        let destination = match key {
            "run_marker" => &mut run_marker,
            "anchor_hash" => &mut anchor_hash,
            "start_device" => {
                if start_device
                    .replace(parse_reviewed_context_number(value)?)
                    .is_some()
                {
                    return Err(sentinel_assertion(
                        "reviewed headless server-log context was invalid",
                    ));
                }
                continue;
            }
            "start_inode" => {
                if start_inode
                    .replace(parse_reviewed_context_number(value)?)
                    .is_some()
                {
                    return Err(sentinel_assertion(
                        "reviewed headless server-log context was invalid",
                    ));
                }
                continue;
            }
            "start_bytes" => {
                if start_bytes
                    .replace(parse_reviewed_context_number(value)?)
                    .is_some()
                {
                    return Err(sentinel_assertion(
                        "reviewed headless server-log context was invalid",
                    ));
                }
                continue;
            }
            "anchor_start" => {
                if anchor_start
                    .replace(parse_reviewed_context_number(value)?)
                    .is_some()
                {
                    return Err(sentinel_assertion(
                        "reviewed headless server-log context was invalid",
                    ));
                }
                continue;
            }
            "anchor_length" => {
                if anchor_length
                    .replace(parse_reviewed_context_number(value)?)
                    .is_some()
                {
                    return Err(sentinel_assertion(
                        "reviewed headless server-log context was invalid",
                    ));
                }
                continue;
            }
            _ => {
                return Err(sentinel_assertion(
                    "reviewed headless server-log context was invalid",
                ));
            }
        };
        if destination.replace(value).is_some() {
            return Err(sentinel_assertion(
                "reviewed headless server-log context was invalid",
            ));
        }
    }
    let context = ReviewedEvidenceContext {
        run_marker: run_marker.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        start_device: start_device.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        start_inode: start_inode.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        start_bytes: start_bytes.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        anchor_start: anchor_start.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        anchor_length: anchor_length.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
        anchor_hash: anchor_hash.ok_or_else(|| {
            sentinel_assertion("reviewed headless server-log context was incomplete")
        })?,
    };
    if context.run_marker.len() != 64
        || context.anchor_hash.len() != 64
        || !context
            .run_marker
            .bytes()
            .chain(context.anchor_hash.bytes())
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(sentinel_assertion(
            "reviewed headless server-log context digests were invalid",
        ));
    }
    Ok(context)
}

fn parse_reviewed_context_number(value: &str) -> TestResult<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(sentinel_assertion(
            "reviewed headless server-log context number was invalid",
        ));
    }
    value
        .parse()
        .map_err(|_| sentinel_assertion("reviewed headless server-log context number was invalid"))
}

struct ReviewedServerEvent {
    values: std::collections::BTreeMap<String, String>,
}

impl<'de> serde::Deserialize<'de> for ReviewedServerEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct EventVisitor;

        impl<'de> serde::de::Visitor<'de> for EventVisitor {
            type Value = ReviewedServerEvent;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a reviewed server event with unique string fields")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                use serde::de::Error as _;

                let mut values = std::collections::BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if values.insert(key, value).is_some() {
                        return Err(A::Error::custom("duplicate reviewed server-event key"));
                    }
                }
                Ok(ReviewedServerEvent { values })
            }
        }

        deserializer.deserialize_map(EventVisitor)
    }
}

fn reviewed_server_event_line(line: &str) -> bool {
    const KEYS: &[&str] = &[
        "timestamp",
        "severity",
        "component",
        "category",
        "fixture_id",
    ];
    let Ok(ReviewedServerEvent { values: event }) =
        serde_json::from_str::<ReviewedServerEvent>(line)
    else {
        return false;
    };
    event.len() >= 2
        && event.keys().all(|key| KEYS.contains(&key.as_str()))
        && event
            .get("severity")
            .is_some_and(|value| !value.is_empty() && value.len() <= 32)
        && ["component", "category"].iter().any(|key| {
            event
                .get(*key)
                .is_some_and(|value| !value.is_empty() && value.len() <= 128)
        })
        && event
            .values()
            .all(|value| !value.is_empty() && value.len() <= 256)
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

#[cfg(feature = "acceptance-harness")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ArtifactChildProcessEvidence {
    cleanup_events: u64,
    cleanup_records: u64,
    reconciliation_events: u64,
    reconciled_records: u64,
    cancelled_operations: u64,
    stdout_bytes: u64,
    stderr_bytes: u64,
}

#[cfg(feature = "acceptance-harness")]
fn artifact_child_process_evidence(
    output: &ProcessOutput,
    forbidden_response_id: Option<u64>,
) -> Result<ArtifactChildProcessEvidence, String> {
    if output.stdout != output.consumed_stdout {
        return Err("artifact child emitted unconsumed protocol output".to_owned());
    }
    for line in output.stdout.split_inclusive(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if line.last() != Some(&b'\n') {
            return Err("artifact child stdout ended with an unterminated frame".to_owned());
        }
        let frame: Value = serde_json::from_slice(&line[..line.len().saturating_sub(1)])
            .map_err(|_| "artifact child stdout contained a non-JSON frame".to_owned())?;
        let object = frame
            .as_object()
            .ok_or_else(|| "artifact child stdout frame was not an object".to_owned())?;
        if frame["jsonrpc"] != Value::String("2.0".to_owned())
            || !frame.get("id").is_some_and(Value::is_number)
            || frame.get("result").is_some() == frame.get("error").is_some()
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "jsonrpc" | "id" | "result" | "error"))
            || contains_forbidden_diagnostic_field(&frame)
        {
            return Err("artifact child stdout violated the exact JSON-RPC contract".to_owned());
        }
        if forbidden_response_id.is_some_and(|forbidden| frame["id"].as_u64() == Some(forbidden)) {
            return Err("cancelled artifact request emitted a response frame".to_owned());
        }
    }

    let stderr = std::str::from_utf8(&output.stderr)
        .map_err(|_| "artifact child stderr was not UTF-8".to_owned())?;
    let mut evidence = ArtifactChildProcessEvidence {
        stdout_bytes: u64::try_from(output.stdout.len())
            .map_err(|_| "artifact child stdout exceeds the addressable range".to_owned())?,
        stderr_bytes: u64::try_from(output.stderr.len())
            .map_err(|_| "artifact child stderr exceeds the addressable range".to_owned())?,
        ..ArtifactChildProcessEvidence::default()
    };
    for line in stderr.lines() {
        if line.is_empty() {
            return Err("artifact child stderr contained a blank diagnostic line".to_owned());
        }
        let diagnostic = parse_artifact_diagnostic(line)?;
        match diagnostic {
            ArtifactDiagnostic::RuntimeReady => {}
            ArtifactDiagnostic::Operation { operation, outcome } => {
                if operation == "file_import" && outcome == "cancelled" {
                    evidence.cancelled_operations = evidence.cancelled_operations.saturating_add(1);
                }
            }
            ArtifactDiagnostic::Cleanup { count } => {
                evidence.cleanup_events = evidence.cleanup_events.saturating_add(1);
                evidence.cleanup_records = evidence.cleanup_records.saturating_add(count);
            }
            ArtifactDiagnostic::Reconciliation { count } => {
                evidence.reconciliation_events = evidence.reconciliation_events.saturating_add(1);
                evidence.reconciled_records = evidence.reconciled_records.saturating_add(count);
            }
        }
    }
    Ok(evidence)
}

#[cfg(feature = "acceptance-harness")]
fn crash06_mid_frame_evidence(output: &ProcessOutput) -> Result<AdversarialExecution, String> {
    if output.exit_category != "signal"
        || output.stdout.len() > MAX_STDOUT_BYTES
        || output.stderr.len() > support::process::MAX_STDERR_BYTES
    {
        return Err("CRASH-06 process capture was not bounded termination evidence".to_owned());
    }
    if output.consumed_stdout.is_empty()
        || !output.consumed_stdout.ends_with(b"\n")
        || !output.stdout.starts_with(&output.consumed_stdout)
    {
        return Err("CRASH-06 lost the complete pre-crash frame prefix".to_owned());
    }
    let fragment = &output.stdout[output.consumed_stdout.len()..];
    if fragment.is_empty()
        || fragment.contains(&b'\n')
        || fragment.first() != Some(&b'{')
        || serde_json::from_slice::<Value>(fragment).is_ok()
    {
        return Err("CRASH-06 did not capture one truncated final JSON frame".to_owned());
    }
    for line in output
        .consumed_stdout
        .split_inclusive(|byte| *byte == b'\n')
    {
        let frame: Value = serde_json::from_slice(&line[..line.len().saturating_sub(1)])
            .map_err(|_| "CRASH-06 complete stdout prefix was not JSON".to_owned())?;
        if frame["jsonrpc"] != "2.0" || frame.get("id").is_none() {
            return Err("CRASH-06 complete stdout prefix was not JSON-RPC".to_owned());
        }
    }
    for diagnostic in output.stderr.split(|byte| *byte == b'\n') {
        if !diagnostic.is_empty()
            && output
                .stdout
                .windows(diagnostic.len())
                .any(|window| window == diagnostic)
        {
            return Err("CRASH-06 copied a diagnostic line to stdout".to_owned());
        }
    }
    let mut execution = AdversarialExecution::default();
    execution.record_executed(AdversarialCaseId::Crash06)?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn crash06_evidence_accepts_only_one_truncated_final_frame() {
    let complete = br#"{"jsonrpc":"2.0","id":1,"result":{}}
"#;
    let fragment = br#"{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"file_"#;
    let output = ProcessOutput {
        stdout: [complete.as_slice(), fragment.as_slice()].concat(),
        consumed_stdout: complete.to_vec(),
        stderr: b"2026-08-08T00:00:00Z INFO authenticated Anytype runtime ready\n".to_vec(),
        exit_category: "signal",
    };
    let execution = crash06_mid_frame_evidence(&output).expect("bounded truncated frame evidence");
    execution
        .assert_exact(&[AdversarialCaseId::Crash06])
        .expect("CRASH-06 is the only recorded row");

    let mut complete_tail = output;
    complete_tail.stdout.push(b'\n');
    assert!(
        crash06_mid_frame_evidence(&complete_tail).is_err(),
        "the interrupted frame must remain the sole final fragment"
    );
}

#[cfg(feature = "acceptance-harness")]
fn contains_forbidden_diagnostic_field(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(name, value)| {
            let normalized = name.to_ascii_lowercase();
            (normalized.contains("authorization")
                || normalized.contains("bearer")
                || normalized.contains("credential")
                || normalized.contains("session_token")
                || normalized.contains("access_token")
                || normalized.contains("refresh_token")
                || normalized.contains("api_key"))
                || contains_forbidden_diagnostic_field(value)
        }),
        Value::Array(values) => values.iter().any(contains_forbidden_diagnostic_field),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

#[cfg(feature = "acceptance-harness")]
fn run_spawned_read_only_cleanup_cases(
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
) -> Result<AdversarialExecution, String> {
    const READ_ONLY_MESSAGE: &str =
        "This Anytype server is read-only. Mutating workflows are disabled.";
    let staging_before = policy.staging_snapshot()?;
    let export_before = policy.export_snapshot()?;
    let mut execution = AdversarialExecution::default();
    let mut guard = lock_driver(child);
    let driver = guard
        .as_mut()
        .ok_or_else(|| "registered read-only artifact child disappeared".to_owned())?;
    // The read-only catalog removes every artifact mutation tool, and a call
    // to a removed name is answered by the fixed bounded read-only refusal
    // before argument decoding, so a stale client catalog cannot reach a
    // handler, a root, or Anytype. The arguments below are deliberately not
    // schema-valid for any artifact tool.
    for name in ARTIFACT_TOOL_NAMES
        .into_iter()
        .filter(|name| *name != "artifact_status")
    {
        let evidence = driver.call_tool_error_sync(name, json!({"secret-unparsed": true}))?;
        if evidence.code() != "validation"
            || evidence
                .normalized_result()
                .pointer("/structuredContent/message")
                .and_then(Value::as_str)
                != Some(READ_ONLY_MESSAGE)
        {
            return Err("CLEAN-07 did not return the fixed read-only refusal".to_owned());
        }
    }
    execution.record_executed(AdversarialCaseId::Clean07)?;

    // `artifact_status` deliberately accepts no arguments, so a supplied
    // handle is refused by strict schema decoding before any handler,
    // staging, or Anytype access - there is no handle parameter to probe.
    let handle = format!("clean08-{}", unique_suffix());
    execution.record_forbidden_log_needle(handle.as_bytes())?;
    let response = driver.request(
        "tools/call",
        json!({"name": "artifact_status", "arguments": {"handle": handle}}),
    );
    let error = response
        .get("error")
        .and_then(Value::as_object)
        .ok_or_else(|| "CLEAN-08 supplied-handle call was routed to a handler".to_owned())?;
    if error.get("code") != Some(&json!(-32602))
        || error.get("message") != Some(&json!("Tool arguments do not match the declared schema."))
        || response.pointer("/error/data/code").and_then(Value::as_str) != Some("validation")
    {
        return Err("CLEAN-08 did not return the strict schema refusal".to_owned());
    }
    drop(guard);
    if policy.staging_snapshot()? != staging_before || policy.export_snapshot()? != export_before {
        return Err("spawned read-only cleanup cases changed private artifact state".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Clean08)?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
enum ArtifactDiagnostic<'a> {
    RuntimeReady,
    Operation {
        operation: &'a str,
        outcome: &'a str,
    },
    Cleanup {
        count: u64,
    },
    Reconciliation {
        count: u64,
    },
}

#[cfg(feature = "acceptance-harness")]
fn parse_artifact_diagnostic(line: &str) -> Result<ArtifactDiagnostic<'_>, String> {
    const RUNTIME_READY: &str = "authenticated Anytype runtime ready";
    const OPERATION: &str = "Anytype operation completed";
    const CLEANUP: &str = "Artifact staging cleanup completed";
    const RECONCILIATION: &str = "Artifact staging reconciliation completed";

    let (message, required_fields): (&str, &[&str]) = if line.contains(RUNTIME_READY) {
        (RUNTIME_READY, &["http_available", "grpc_available"])
    } else if line.contains(OPERATION) {
        (
            OPERATION,
            &[
                "operation",
                "correlation_id",
                "duration_ms",
                "outcome",
                "upstream_status",
                "upstream_http_status",
                "upstream_http_status_present",
            ],
        )
    } else if line.contains(CLEANUP) {
        (CLEANUP, &["operation", "outcome", "cleanup_count"])
    } else if line.contains(RECONCILIATION) {
        (RECONCILIATION, &["operation", "outcome", "cleanup_count"])
    } else {
        return Err("artifact child stderr contained a non-allowlisted line".to_owned());
    };
    let (prefix, suffix) = line
        .split_once(message)
        .ok_or_else(|| "artifact child diagnostic omitted its fixed message".to_owned())?;
    let prefix = prefix.split_whitespace().collect::<Vec<_>>();
    if prefix.len() != 2
        || prefix.get(1).copied() != Some("INFO")
        || prefix.first().is_none_or(|timestamp| {
            timestamp.len() > 64
                || !timestamp.bytes().all(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'-' | b':' | b'.' | b'+' | b'T' | b'Z')
                })
        })
    {
        return Err("artifact child diagnostic prefix was not fixed-format INFO".to_owned());
    }
    let mut fields = Vec::new();
    for field in suffix.split_whitespace() {
        let (name, value) = field
            .split_once('=')
            .ok_or_else(|| "artifact child diagnostic field was malformed".to_owned())?;
        if fields.iter().any(|(existing, _)| *existing == name) {
            return Err("artifact child diagnostic repeated a field".to_owned());
        }
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .unwrap_or(value);
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err("artifact child diagnostic value was not bounded".to_owned());
        }
        fields.push((name, value));
    }
    if fields.len() != required_fields.len()
        || required_fields
            .iter()
            .any(|required| !fields.iter().any(|(name, _)| name == required))
    {
        return Err("artifact child diagnostic fields were not exact".to_owned());
    }
    let field = |name: &str| {
        fields
            .iter()
            .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
            .ok_or_else(|| "artifact child diagnostic omitted a field".to_owned())
    };
    match message {
        RUNTIME_READY => {
            if !matches!(field("http_available")?, "true" | "false")
                || !matches!(field("grpc_available")?, "true" | "false")
            {
                return Err("artifact runtime diagnostic booleans were malformed".to_owned());
            }
            Ok(ArtifactDiagnostic::RuntimeReady)
        }
        OPERATION => {
            let operation = field("operation")?;
            let outcome = field("outcome")?;
            if !ARTIFACT_TOOL_NAMES.contains(&operation)
                || field("duration_ms")?.parse::<u64>().is_err()
                || field("upstream_http_status")?.parse::<u16>().is_err()
                || !matches!(field("upstream_http_status_present")?, "true" | "false")
            {
                return Err("artifact operation diagnostic values were malformed".to_owned());
            }
            Ok(ArtifactDiagnostic::Operation { operation, outcome })
        }
        CLEANUP => {
            if field("operation")? != "artifact_staging_cleanup"
                || field("outcome")? != "expired_reaped"
            {
                return Err("artifact cleanup diagnostic values were malformed".to_owned());
            }
            Ok(ArtifactDiagnostic::Cleanup {
                count: field("cleanup_count")?
                    .parse()
                    .map_err(|_| "artifact cleanup count was malformed".to_owned())?,
            })
        }
        RECONCILIATION => {
            if field("operation")? != "artifact_staging_reconciliation"
                || field("outcome")? != "startup_complete"
            {
                return Err("artifact reconciliation diagnostic values were malformed".to_owned());
            }
            Ok(ArtifactDiagnostic::Reconciliation {
                count: field("cleanup_count")?
                    .parse()
                    .map_err(|_| "artifact reconciliation count was malformed".to_owned())?,
            })
        }
        _ => Err("artifact child diagnostic message was not fixed".to_owned()),
    }
}

#[cfg(feature = "acceptance-harness")]
fn finish_registered_artifact_child(
    child: &Arc<Mutex<Option<StdioDriver>>>,
    forbidden_response_id: Option<u64>,
) -> Result<ArtifactChildProcessEvidence, String> {
    let driver = lock_driver(child)
        .take()
        .ok_or_else(|| "registered artifact child disappeared".to_owned())?;
    let (_, output) = driver.try_finish()?;
    artifact_child_process_evidence(&output, forbidden_response_id)
}

#[cfg(feature = "acceptance-harness")]
fn terminate_registered_artifact_child(
    child: &Arc<Mutex<Option<StdioDriver>>>,
) -> Result<ArtifactChildProcessEvidence, String> {
    let driver = lock_driver(child)
        .take()
        .ok_or_else(|| "registered artifact child disappeared".to_owned())?;
    let (_, output) = driver.terminate()?;
    artifact_child_process_evidence(&output, None)
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
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
fn spawn_disposable_artifact_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
    options: DriverOptions,
) -> TestResult<Arc<Mutex<Option<StdioDriver>>>> {
    spawn_disposable_artifact_driver_configured(ctx, cleanup_record, policy, options, None)
}

#[cfg(feature = "acceptance-harness")]
fn spawn_disposable_mid_frame_crash_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
) -> TestResult<(Arc<Mutex<Option<StdioDriver>>>, MidFramePause)> {
    let child_environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| sentinel_assertion("disposable callback omitted its child environment"))?
        .clone();
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    child_environment.configure(&mut command)?;
    configure_stdio_command(&mut command, DriverOptions::STANDARD, None);
    command.env("ANY_MCP_CONFIG", policy.config_path());
    ctx.spawn_owned_child(move || {
        let mut retained_policy = Some(policy);
        let (driver, pause) =
            StdioDriver::spawn_paused_in_second_frame(command, DriverOptions::STANDARD);
        let driver = Arc::new(Mutex::new(Some(driver)));
        let stopped = Arc::clone(&driver);
        ((driver, pause), move || {
            *cleanup_record.lock().expect("child cleanup record lock") =
                ChildCleanupRecord::Attempted;
            let result = lock_driver(&stopped)
                .take()
                .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
            drop(retained_policy.take());
            match result {
                Ok(()) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Stopped;
                    Ok(())
                }
                Err(_) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Failed;
                    Err(sentinel_assertion(
                        "registered crash-frame child did not stop cleanly",
                    ))
                }
            }
        })
    })
}

#[cfg(feature = "acceptance-harness")]
fn spawn_disposable_paused_artifact_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
    options: DriverOptions,
) -> TestResult<(Arc<Mutex<Option<StdioDriver>>>, ChildArtifactGate)> {
    let key = format!("artifact-cancel-import-{}", unique_suffix());
    spawn_disposable_gated_artifact_driver(
        ctx,
        cleanup_record,
        policy,
        options,
        "import-first-upload-chunk",
        key,
    )
}

#[cfg(feature = "acceptance-harness")]
fn spawn_disposable_gated_artifact_driver(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
    options: DriverOptions,
    point: &'static str,
    key: String,
) -> TestResult<(Arc<Mutex<Option<StdioDriver>>>, ChildArtifactGate)> {
    let gate = ChildArtifactGate::create(policy.acceptance_gate_base(), point, &key)?;
    let child = spawn_disposable_artifact_driver_configured(
        ctx,
        cleanup_record,
        policy,
        options,
        Some((&gate, point, &key)),
    )?;
    Ok((child, gate))
}

#[cfg(feature = "acceptance-harness")]
fn spawn_disposable_artifact_driver_configured(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
    options: DriverOptions,
    gate: Option<(&ChildArtifactGate, &str, &str)>,
) -> TestResult<Arc<Mutex<Option<StdioDriver>>>> {
    let child_environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| sentinel_assertion("disposable callback omitted its child environment"))?
        .clone();
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    child_environment.configure(&mut command)?;
    configure_stdio_command(&mut command, options, Some("artifacts"));
    command.env("ANY_MCP_CONFIG", policy.config_path());
    if let Some((gate, point, key)) = gate {
        gate.configure(&mut command, point, key);
    }
    ctx.spawn_owned_child(move || {
        // The fixture tree must outlive the child so no export or staged byte
        // is removed while the production process still holds its roots.
        let mut retained_policy = Some(policy);
        let driver = Arc::new(Mutex::new(Some(StdioDriver::spawn(command, options, None))));
        let stopped = Arc::clone(&driver);
        (driver, move || {
            *cleanup_record.lock().expect("child cleanup record lock") =
                ChildCleanupRecord::Attempted;
            let result = lock_driver(&stopped)
                .take()
                .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
            drop(retained_policy.take());
            match result {
                Ok(()) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Stopped;
                    Ok(())
                }
                Err(_) => {
                    *cleanup_record.lock().expect("child cleanup record lock") =
                        ChildCleanupRecord::Failed;
                    Err(sentinel_assertion(
                        "registered artifact stdio child did not stop cleanly",
                    ))
                }
            }
        })
    })
}

/// Drives artifact tools through exact JSON-RPC frames.
///
/// Every response envelope is validated before its structured content is
/// returned, so the scripted control plane proves the wire contract instead of
/// a decoded convenience value.
#[cfg(feature = "acceptance-harness")]
struct ScriptedArtifactDriver {
    driver: Arc<Mutex<Option<StdioDriver>>>,
}

#[cfg(feature = "acceptance-harness")]
impl ScriptedArtifactDriver {
    fn with_driver<T>(&self, operation: impl FnOnce(&mut StdioDriver) -> T) -> T {
        let mut driver = lock_driver(&self.driver);
        operation(
            driver
                .as_mut()
                .expect("registered scripted artifact child remains owned"),
        )
    }
}

#[cfg(feature = "acceptance-harness")]
impl McpDriver for ScriptedArtifactDriver {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        let result = self.with_driver(|driver| driver.scripted_tool_frame(name, arguments));
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

    fn list_tool_descriptors<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, String>> + 'a>> {
        let result = self.with_driver(StdioDriver::list_tool_descriptors_sync);
        Box::pin(std::future::ready(result))
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "scripted artifact scenario does not use resources/list".to_owned(),
        )))
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "scripted artifact scenario does not use resources/templates/list".to_owned(),
        )))
    }

    fn read_resource<'a>(
        &'a mut self,
        _uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(std::future::ready(Err(
            "scripted artifact scenario does not use resources/read".to_owned(),
        )))
    }
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
            Ok(Err(error)) => panic!(
                "disposable stdio lifecycle failed: {}; setup={:?}; readiness={:?}; callback={:?}",
                error.category(),
                error.setup_failure(),
                error.readiness_failure(),
                error.callback_failure(),
            ),
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
    let record = Arc::new(Mutex::new(CaseRecord::default()));
    let captured = Arc::clone(&record);
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let child_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let callback_cleanup = Arc::clone(&child_cleanup);
    let cleanup = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-preview-baseline",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let driver =
                    spawn_disposable_driver(ctx.as_ref(), callback_cleanup, options, None)?;
                lock_driver(&driver)
                    .as_mut()
                    .ok_or_else(|| sentinel_assertion("registered baseline child disappeared"))?
                    .initialize();
                let mut owned = OwnedStdioDriver {
                    driver: Arc::clone(&driver),
                };
                if options.preview {
                    let tools = owned
                        .list_tools()
                        .await
                        .map_err(|_| sentinel_assertion("preview compact catalog failed"))?;
                    validate_preview_compact_catalog(&tools)
                        .map_err(|_| sentinel_assertion("preview compact catalog mismatch"))?;
                } else if options.profile == "standard" && !options.read_only {
                    let tools = owned
                        .list_tools()
                        .await
                        .map_err(|_| sentinel_assertion("standard catalog failed"))?;
                    let borrowed = tools.iter().map(String::as_str).collect::<Vec<_>>();
                    validate_live_ownership(
                        &borrowed,
                        &[
                            "resources/list",
                            "resources/read",
                            "resources/templates/list",
                        ],
                    )
                    .map_err(|_| sentinel_assertion("spawned catalog ownership mismatch"))?;
                }
                let mut evidence = ScenarioEvidence::new(scenario);
                let result = AssertUnwindSafe(run_scenario(
                    scenario,
                    &mut owned,
                    ctx.as_ref(),
                    &mut evidence,
                ))
                .catch_unwind()
                .await;
                drop(owned);
                let driver = lock_driver(&driver)
                    .take()
                    .ok_or_else(|| sentinel_assertion("registered baseline child disappeared"))?;
                *captured.lock().expect("case record lock") =
                    complete_case(driver, evidence, result, options);
                Ok(())
            })
        },
    ))
    .await;
    let cleanup_status = if cleanup.is_ok() { "success" } else { "failed" };
    {
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
    }
    match cleanup.expect("cleanup-safe spawned baseline scenario") {
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *child_cleanup.lock().expect("baseline child cleanup record"),
                ChildCleanupRecord::Stopped
            );
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            panic!("required disposable spawned baseline was skipped: {reason:?}");
        }
    }
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

async fn run_members_real_workflow() -> OptionalRealWorkflowRun {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let cleanup_record = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let callback_cleanup = Arc::clone(&cleanup_record);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-members",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let credential_needles = disposable_child_credential_needles(ctx.as_ref())?;
                let child = spawn_disposable_driver(
                    ctx.as_ref(),
                    callback_cleanup,
                    DriverOptions::STANDARD,
                    Some("members"),
                )?;
                lock_driver(&child)
                    .as_mut()
                    .ok_or_else(|| sentinel_assertion("registered members child disappeared"))?
                    .initialize();
                let mut driver = OwnedStdioDriver {
                    driver: Arc::clone(&child),
                };
                let tools = driver
                    .list_tools()
                    .await
                    .map_err(|_| sentinel_assertion("members tools/list failed"))?;
                assert!(tools.iter().any(|name| name == "member_list"));
                assert!(tools.iter().any(|name| name == "member_get"));
                assert!(tools.iter().any(|name| name == "optional_toolset_status"));

                let status = driver
                    .call_tool("optional_toolset_status", json!({}))
                    .await
                    .expect("members status");
                assert_eq!(status["configured_toolsets"], json!(["members"]));
                assert_eq!(status["active_toolsets"], json!(["members"]));
                let page = driver
                    .call_tool("member_list", json!({"space": ctx.space_id, "limit": 100}))
                    .await
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
                        .call_tool(
                            "member_get",
                            json!({
                                "space": ctx.space_id,
                                "member_id": item["id"]
                            }),
                        )
                        .await
                        .expect("spawned member_get");
                    assert_eq!(exact["member"], *item);
                }
                let registered = lock_driver(&child)
                    .take()
                    .ok_or_else(|| sentinel_assertion("registered members child disappeared"))?;
                let (transcript, output) = registered
                    .try_finish()
                    .map_err(|_| sentinel_assertion("members child did not stop cleanly"))?;
                require_spawned_diagnostics(
                    &transcript,
                    &output,
                    &[b"network-secret"],
                    &credential_needles,
                )?;
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
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *cleanup_record.lock().expect("members cleanup record"),
                ChildCleanupRecord::Stopped
            );
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned members suite skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_members_minimizes_personal_data() {
    let _ = run_members_real_workflow().await;
}

const FILE_RESOURCE_TEMPLATE: &str =
    "anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}";

#[derive(Debug)]
struct SpawnedFilesEvidence {
    normalized: Value,
}

fn disposable_child_credential_needles(ctx: &TestContext) -> TestResult<Vec<Vec<u8>>> {
    const CREDENTIAL_NAMES: &[&str] = &[
        "ANYTYPE_KEY_HTTP_TOKEN",
        "ANYTYPE_KEY_ACCOUNT_ID",
        "ANYTYPE_KEY_ACCOUNT_KEY",
        "ANYTYPE_KEY_SESSION_TOKEN",
    ];
    let environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| sentinel_assertion("workflow omitted disposable child credentials"))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    environment.configure(&mut command)?;
    let needles = command
        .get_envs()
        .filter_map(|(name, value)| {
            let name = name.to_str()?;
            CREDENTIAL_NAMES.contains(&name).then_some(value?)
        })
        .map(|value| value.to_string_lossy().into_owned().into_bytes())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if needles.len() < 2 {
        return Err(sentinel_assertion(
            "workflow child credentials were incomplete",
        ));
    }
    Ok(needles)
}

fn require_spawned_diagnostics(
    transcript: &str,
    output: &ProcessOutput,
    secrets: &[&[u8]],
    credential_needles: &[Vec<u8>],
) -> TestResult<()> {
    let metrics = stderr_metrics(&output.stderr);
    let categorized =
        metrics.runtime_ready + metrics.operation_success + metrics.operation_non_success;
    let transcript = transcript.as_bytes();
    let leaks_secret = secrets.iter().any(|secret| {
        !secret.is_empty()
            && (contains_bytes(transcript, secret) || contains_bytes(&output.stderr, secret))
    });
    let leaks_credential = credential_needles.iter().any(|credential| {
        contains_bytes(transcript, credential) || contains_bytes(&output.stderr, credential)
    });
    if output.stderr.len() > 524_288
        || leaks_secret
        || leaks_credential
        || metrics.invalid_utf8
        || metrics.runtime_ready != 1
        || metrics.operation_success == 0
        || metrics.stack_overflow != 0
        || metrics.panic != 0
        || metrics.fatal != 0
        || metrics.other != 0
        || metrics.lines != categorized
    {
        return Err(sentinel_assertion(
            "child diagnostics violated fixed-category/redaction bounds",
        ));
    }
    Ok(())
}

fn file_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Sha256::digest(bytes)
        .iter()
        .flat_map(|byte| {
            [
                HEX[usize::from(*byte >> 4)] as char,
                HEX[usize::from(*byte & 0x0f)] as char,
            ]
        })
        .collect()
}

fn verify_file_resource(
    resource: &Value,
    expected_uri: &str,
    expected_bytes: &[u8],
) -> TestResult<()> {
    let contents = resource["contents"]
        .as_array()
        .ok_or_else(|| sentinel_assertion("files resource omitted contents"))?;
    if contents.len() != 1 {
        return Err(sentinel_assertion(
            "files resource returned the wrong content count",
        ));
    }
    let content = &contents[0];
    if content["uri"] != expected_uri || content["mimeType"] != "application/octet-stream" {
        return Err(sentinel_assertion(
            "files resource identity or media type diverged",
        ));
    }
    let encoded = content["blob"]
        .as_str()
        .ok_or_else(|| sentinel_assertion("files resource omitted blob bytes"))?;
    let decoded = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| sentinel_assertion("files resource returned invalid base64"))?;
    if decoded != expected_bytes || file_sha256(&decoded) != file_sha256(expected_bytes) {
        return Err(sentinel_assertion(
            "files resource returned incorrect bounded bytes",
        ));
    }
    Ok(())
}

async fn files_tool_value(
    driver: &mut OwnedStdioDriver,
    name: &'static str,
    arguments: Value,
) -> TestResult<Value> {
    driver.call_tool(name, arguments).await.map_err(|error| {
        eprintln!("spawned files debug: tool={name} category={error}");
        sentinel_assertion("spawned files tool call failed")
    })
}

async fn exercise_spawned_files_workflow(
    driver: &mut OwnedStdioDriver,
    ctx: &TestContext,
    label: &str,
    file_name: &str,
    payload: &[u8],
) -> TestResult<SpawnedFilesEvidence> {
    let mut tools = driver
        .list_tools()
        .await
        .map_err(|_| sentinel_assertion("spawned files tools/list failed"))?;
    tools.sort();
    let file_tools = tools
        .iter()
        .filter(|name| {
            matches!(
                name.as_str(),
                "file_metadata" | "file_read" | "file_upload" | "optional_toolset_status"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if file_tools
        != [
            "file_metadata",
            "file_read",
            "file_upload",
            "optional_toolset_status",
        ]
    {
        return Err(sentinel_assertion(
            "spawned files catalog did not expose the exact registry",
        ));
    }
    let status = files_tool_value(driver, "optional_toolset_status", json!({})).await?;
    if status
        != json!({
            "configured_toolsets":["files"],
            "active_toolsets":["files"]
        })
    {
        return Err(sentinel_assertion(
            "spawned files status did not identify the exact registry",
        ));
    }
    let templates = driver
        .list_resource_templates()
        .await
        .map_err(|_| sentinel_assertion("spawned files resource templates failed"))?;
    let file_templates = templates["resourceTemplates"]
        .as_array()
        .ok_or_else(|| sentinel_assertion("spawned files templates omitted inventory"))?
        .iter()
        .filter(|template| template["uriTemplate"] == FILE_RESOURCE_TEMPLATE)
        .count();
    if file_templates != 1 {
        return Err(sentinel_assertion(
            "spawned files registry omitted its exact resource template",
        ));
    }
    let content_sha256 = file_sha256(payload);
    let encoded_payload = BASE64_STANDARD.encode(payload);
    let upload = files_tool_value(
        driver,
        "file_upload",
        json!({
            "space":ctx.space_id,
            "name":file_name,
            "content_base64":encoded_payload,
            "media_type":"application/octet-stream",
            "idempotency_key":format!("mcp-files-{label}-{}", unique_suffix())
        }),
    )
    .await?;
    let file_id = upload["file_id"]
        .as_str()
        .ok_or_else(|| sentinel_assertion("spawned files upload omitted its object ID"))?
        .to_owned();
    ctx.register_file(&file_id);

    if upload["space_id"] != ctx.space_id
        || upload["requested_name"] != file_name
        || upload["media_type"] != "application/octet-stream"
        || upload["size_bytes"] != payload.len()
        || upload["content_sha256"] != content_sha256
        || upload["reused"] != false
    {
        return Err(sentinel_assertion(
            "spawned files upload verification diverged",
        ));
    }

    let metadata = files_tool_value(
        driver,
        "file_metadata",
        json!({"space":ctx.space_id,"file_id":file_id}),
    )
    .await?;
    if metadata["file_id"] != file_id
        || metadata["space_id"] != ctx.space_id
        || metadata["media_type"] != "application/octet-stream"
        || metadata["size_bytes"] != payload.len()
        || metadata["accepts_byte_ranges"] != true
    {
        return Err(sentinel_assertion(
            "spawned files metadata verification diverged",
        ));
    }

    let split = payload.len() / 2;
    let mut ranges = Vec::new();
    for (offset, length) in [(0, split), (split, payload.len() - split)] {
        let expected_bytes = &payload[offset..offset + length];
        let range_sha256 = file_sha256(expected_bytes);
        let read = files_tool_value(
            driver,
            "file_read",
            json!({
                "space":ctx.space_id,
                "file_id":file_id,
                "offset":offset,
                "length":length
            }),
        )
        .await?;
        let expected_uri = format!(
            "anytype-file://bytes/{}/{}/{}/{}/{}",
            ctx.space_id, file_id, offset, length, range_sha256
        );
        if read["file_id"] != file_id
            || read["space_id"] != ctx.space_id
            || read["media_type"] != "application/octet-stream"
            || read["offset"] != offset
            || read["requested_bytes"] != length
            || read["returned_bytes"] != length
            || read["total_bytes"] != payload.len()
            || read["complete"] != (offset + length == payload.len())
            || read["content_sha256"] != range_sha256
            || read["content_kind"] != "blob_resource"
            || read["resource_uri"] != expected_uri
        {
            return Err(sentinel_assertion(
                "spawned files bounded read verification diverged",
            ));
        }
        let resource = driver
            .read_resource(&expected_uri)
            .await
            .map_err(|_| sentinel_assertion("spawned files resources/read failed"))?;
        verify_file_resource(&resource, &expected_uri, expected_bytes)?;
        ranges.push(json!({
            "offset":offset,
            "requested_bytes":length,
            "returned_bytes":length,
            "total_bytes":payload.len(),
            "complete":offset + length == payload.len(),
            "content_sha256":range_sha256,
            "content_kind":"blob_resource",
            "resource_media_type":"application/octet-stream"
        }));
    }

    let independent = ctx
        .client
        .files()
        .download_request(&ctx.space_id, &file_id)
        .response_limit_bytes(payload.len() as u64 + 1)
        .error_limit_bytes(16_384)
        .header_evidence_limit_bytes(4_096)
        .max_attempts(3)
        .download()
        .await?;
    if independent.status.as_u16() != 200
        || independent.bytes.as_ref() != payload
        || file_sha256(&independent.bytes) != content_sha256
    {
        return Err(sentinel_assertion(
            "independent Anytype API file download diverged",
        ));
    }

    Ok(SpawnedFilesEvidence {
        normalized: json!({
            "tools":file_tools,
            "status":status,
            "resource_template":FILE_RESOURCE_TEMPLATE,
            "upload":{
                "media_type":upload["media_type"],
                "size_bytes":upload["size_bytes"],
                "content_sha256":upload["content_sha256"],
                "reused":upload["reused"]
            },
            "metadata":{
                "media_type":metadata["media_type"],
                "size_bytes":metadata["size_bytes"],
                "accepts_byte_ranges":metadata["accepts_byte_ranges"],
                "strong_etag_present":metadata.get("strong_etag").is_some(),
                "last_modified_present":metadata.get("last_modified").is_some()
            },
            "ranges":ranges,
            "independent_download":{
                "status":independent.status.as_u16(),
                "size_bytes":independent.bytes.len(),
                "content_sha256":content_sha256
            }
        }),
    })
}

async fn run_spawned_files_transport(
    ctx: &TestContext,
    label: &str,
    options: DriverOptions,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    payload: &[u8],
    credential_needles: &[Vec<u8>],
) -> TestResult<SpawnedFilesEvidence> {
    let child = spawn_disposable_driver(ctx, cleanup_record, options, Some("files"))?;
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(&child),
    };
    driver.with_driver(StdioDriver::initialize);
    let file_name = format!("private-mcp-files-{label}-{}.bin", unique_suffix());
    let encoded_payload = BASE64_STANDARD.encode(payload);
    let result =
        exercise_spawned_files_workflow(&mut driver, ctx, label, &file_name, payload).await;
    drop(driver);
    let child = lock_driver(&child)
        .take()
        .ok_or_else(|| sentinel_assertion("registered files child disappeared"))?;
    let (transcript, output) = child
        .try_finish()
        .map_err(|_| sentinel_assertion("registered files child did not stop cleanly"))?;
    require_spawned_diagnostics(
        &transcript,
        &output,
        &[file_name.as_bytes(), encoded_payload.as_bytes()],
        credential_needles,
    )?;
    result
}

async fn run_files_real_workflow() -> OptionalRealWorkflowRun {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_callback_cleanup = Arc::clone(&stable_cleanup);
    let preview_callback_cleanup = Arc::clone(&preview_cleanup);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-files-registry",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let credential_needles = disposable_child_credential_needles(ctx.as_ref())?;
                let mut state = 0x0A11_F17E_u32;
                let mut payload = Vec::with_capacity(8_192);
                for _ in 0..8_192 {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    payload.push((state & 0xff) as u8);
                }

                let stable = run_spawned_files_transport(
                    ctx.as_ref(),
                    "stable",
                    DriverOptions::STANDARD,
                    stable_callback_cleanup,
                    &payload,
                    &credential_needles,
                )
                .await?;
                let preview = run_spawned_files_transport(
                    ctx.as_ref(),
                    "preview",
                    DriverOptions::PREVIEW_STANDARD,
                    preview_callback_cleanup,
                    &payload,
                    &credential_needles,
                )
                .await?;
                if stable.normalized != preview.normalized {
                    return Err(sentinel_assertion(
                        "stable and preview spawned files evidence diverged",
                    ));
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned files registry suite");
    match outcome {
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *stable_cleanup.lock().expect("stable files cleanup record"),
                ChildCleanupRecord::Stopped
            );
            assert_eq!(
                *preview_cleanup
                    .lock()
                    .expect("preview files cleanup record"),
                ChildCleanupRecord::Stopped
            );
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned files registry skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_files_registry_runs_stable_and_preview_workflows() {
    let _ = run_files_real_workflow().await;
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
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    credential_needles: &[Vec<u8>],
    label: &str,
    options: DriverOptions,
    fixture: ChatsRegistryFixture<'_>,
) -> Result<ChatsRegistryEvidence, TestError> {
    let search_query = fixture.search_query;
    let add_text = fixture.add_text;
    let child = spawn_disposable_driver(ctx, cleanup_record, options, Some("chats"))?;
    if std::panic::catch_unwind(AssertUnwindSafe(|| {
        lock_driver(&child)
            .as_mut()
            .expect("registered chats child remains owned")
            .initialize();
    }))
    .is_err()
    {
        let driver = lock_driver(&child)
            .take()
            .ok_or_else(|| sentinel_assertion("registered chats child disappeared"))?;
        return Err(chats_process_failure(label, "initialize", driver));
    }
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(&child),
    };
    let result = AssertUnwindSafe(run_chats_registry_scenario(&mut driver, fixture))
        .catch_unwind()
        .await;
    match result {
        Ok(Ok(evidence)) => {
            let driver = lock_driver(&child)
                .take()
                .ok_or_else(|| sentinel_assertion("registered chats child disappeared"))?;
            let (transcript, output) = driver
                .try_finish()
                .map_err(|_| sentinel_assertion("registered chats child did not stop cleanly"))?;
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
            require_spawned_diagnostics(
                &transcript,
                &output,
                &[
                    search_query.as_bytes(),
                    add_text.as_bytes(),
                    b"private-content-sentinel",
                ],
                credential_needles,
            )?;
            Ok(evidence)
        }
        Ok(Err(message)) => {
            if let Some(driver) = lock_driver(&child).take() {
                let _ = driver.try_finish();
            }
            eprintln!("spawned chats registry scenario failed: transport={label} stage={message}");
            Err(TestError::Assertion { message })
        }
        Err(_) => {
            let driver = lock_driver(&child)
                .take()
                .ok_or_else(|| sentinel_assertion("registered chats child disappeared"))?;
            Err(chats_process_failure(label, "scenario", driver))
        }
    }
}

async fn run_chats_real_workflow() -> OptionalRealWorkflowRun {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_callback_cleanup = Arc::clone(&stable_cleanup);
    let preview_callback_cleanup = Arc::clone(&preview_cleanup);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-chats-registry",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let credential_needles = disposable_child_credential_needles(ctx.as_ref())?;
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

                for (label, options, cleanup_record) in [
                    ("stable", DriverOptions::COMPACT, stable_callback_cleanup),
                    ("preview", DriverOptions::PREVIEW, preview_callback_cleanup),
                ] {
                    let add_text = format!("{label} chats registry {suffix}");
                    let idempotency_key = format!("{label}-chats-registry-{suffix}");
                    let evidence = Box::pin(run_spawned_chats_registry_transport(
                        ctx.as_ref(),
                        cleanup_record,
                        &credential_needles,
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
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *stable_cleanup.lock().expect("stable chats cleanup record"),
                ChildCleanupRecord::Stopped
            );
            assert_eq!(
                *preview_cleanup
                    .lock()
                    .expect("preview chats cleanup record"),
                ChildCleanupRecord::Stopped
            );
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned chats registry suite skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_chats_registry_runs_stable_and_preview_workflows() {
    let _ = run_chats_real_workflow().await;
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
async fn run_views_write_real_workflow() -> OptionalRealWorkflowRun {
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
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned views-write acceptance skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn shared_direct_stable_preview_views_write_acceptance_is_exact() {
    let _ = run_views_write_real_workflow().await;
}

async fn run_schema_real_workflow() -> OptionalRealWorkflowRun {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let cleanup_record = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let callback_cleanup = Arc::clone(&cleanup_record);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-schema",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let credential_needles = disposable_child_credential_needles(ctx.as_ref())?;
                let child = spawn_disposable_driver(
                    ctx.as_ref(),
                    callback_cleanup,
                    DriverOptions::STANDARD,
                    Some("schema"),
                )?;
                lock_driver(&child)
                    .as_mut()
                    .ok_or_else(|| sentinel_assertion("registered schema child disappeared"))?
                    .initialize();
                let driver = OwnedStdioDriver {
                    driver: Arc::clone(&child),
                };
                let call_tool = |name: &'static str, arguments: Value| {
                    driver.with_driver(|driver| driver.call_tool_sync(name, arguments))
                };
                let tools = driver
                    .with_driver(StdioDriver::list_tools_sync)
                    .expect("schema tools/list");
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
                let status = call_tool("optional_toolset_status", json!({}))
                    .expect("schema status");
                assert_eq!(status["configured_toolsets"], json!(["schema"]));
                assert_eq!(status["active_toolsets"], json!(["schema"]));

                let created_space_name = format!("MCP schema registry space {}", unique_suffix());
                let created_space_claim =
                    Arc::new(ctx.prepare_space_fixture_claim(&created_space_name).await?);
                let created_space = match std::panic::catch_unwind(AssertUnwindSafe(|| {
                    call_tool(
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
                        let registered = lock_driver(&child)
                            .take()
                            .expect("registered schema child remains owned");
                        let (_, output, process_category) = registered.finish_after_panic();
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
                let updated_space = call_tool(
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
                let created_type = call_tool(
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
                let fetched_type = call_tool(
                    "type_get",
                    json!({"space":ctx.space_id,"type":type_id}),
                )
                    .expect("spawned type_get");
                assert_eq!(
                    fetched_type.pointer("/type/id").and_then(Value::as_str),
                    Some(type_id.as_str())
                );
                let updated_type_name = format!("MCP schema updated type {}", unique_suffix());
                let updated_type = call_tool(
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
                let created_property = call_tool(
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
                let updated_property = call_tool(
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
                let created_tag = call_tool(
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
                let updated_tag = call_tool(
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

                let registered = lock_driver(&child)
                    .take()
                    .ok_or_else(|| sentinel_assertion("registered schema child disappeared"))?;
                let (transcript, output) = registered
                    .try_finish()
                    .map_err(|_| sentinel_assertion("schema child did not stop cleanly"))?;
                require_spawned_diagnostics(
                    &transcript,
                    &output,
                    &[
                        created_space_name.as_bytes(),
                        type_name.as_bytes(),
                        updated_type_name.as_bytes(),
                        property_name.as_bytes(),
                        updated_property_name.as_bytes(),
                        tag_name.as_bytes(),
                        updated_tag_name.as_bytes(),
                    ],
                    &credential_needles,
                )?;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned schema registry suite");
    match outcome {
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *cleanup_record.lock().expect("schema cleanup record"),
                ChildCleanupRecord::Stopped
            );
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("spawned schema registry suite skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_schema_registry_runs_all_nine_workflows() {
    let _ = run_schema_real_workflow().await;
}

const ALL_OPTIONAL_TOOLSETS: &str = "body-blocks,chats,files,members,schema,views-write";
const ALL_OPTIONAL_TOOLSETS_REVERSED: &str = "views-write,schema,members,files,chats,body-blocks";
const ALL_OPTIONAL_TOOLSET_NAMES: [&str; 6] = [
    "body-blocks",
    "chats",
    "files",
    "members",
    "schema",
    "views-write",
];
const STANDARD_CORE_TOOLS: [&str; 14] = [
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
const READ_ONLY_CORE_TOOLS: [&str; 10] = [
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
const ALL_OPTIONAL_READ_WRITE_TOOLS: [&str; 31] = [
    "body_block_create",
    "body_block_delete",
    "body_block_list",
    "body_block_move",
    "body_block_update",
    "chat_list",
    "chat_message_add",
    "chat_message_delete",
    "chat_message_get",
    "chat_message_list",
    "chat_message_search",
    "collection_member_add",
    "collection_member_list",
    "collection_member_remove",
    "file_metadata",
    "file_read",
    "file_upload",
    "member_get",
    "member_list",
    "optional_toolset_status",
    "property_create",
    "property_update",
    "rich_page_create",
    "rich_page_resume",
    "space_create",
    "space_update",
    "tag_create",
    "tag_update",
    "type_create",
    "type_get",
    "type_update",
];
const ALL_OPTIONAL_READ_ONLY_TOOLS: [&str; 12] = [
    "body_block_list",
    "chat_list",
    "chat_message_get",
    "chat_message_list",
    "chat_message_search",
    "collection_member_list",
    "file_metadata",
    "file_read",
    "member_get",
    "member_list",
    "optional_toolset_status",
    "type_get",
];

fn aggregate_sentinel_error(message: impl Into<String>) -> TestError {
    TestError::Assertion {
        message: message.into(),
    }
}

async fn require_aggregate_contract(
    driver: &mut OwnedStdioDriver,
    core_tools: &[&str],
    optional_tools: &[&str],
) -> TestResult<()> {
    let mut actual = driver
        .list_tools()
        .await
        .map_err(|_| aggregate_sentinel_error("aggregate tools/list failed"))?;
    actual.sort();
    let mut expected = core_tools
        .iter()
        .chain(optional_tools)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    expected.sort();
    if actual != expected {
        return Err(aggregate_sentinel_error(format!(
            "aggregate catalog mismatch: expected {} core + {} optional tools, observed {} total",
            core_tools.len(),
            optional_tools.len(),
            actual.len()
        )));
    }

    let status = driver
        .call_tool("optional_toolset_status", json!({}))
        .await
        .map_err(|_| aggregate_sentinel_error("aggregate optional status failed"))?;
    if status["configured_toolsets"] != json!(ALL_OPTIONAL_TOOLSET_NAMES)
        || status["active_toolsets"] != json!(ALL_OPTIONAL_TOOLSET_NAMES)
    {
        return Err(aggregate_sentinel_error(
            "aggregate optional status was not canonical and fully active",
        ));
    }

    let templates = driver
        .list_resource_templates()
        .await
        .map_err(|_| aggregate_sentinel_error("aggregate resource templates failed"))?;
    let file_template_count = templates["resourceTemplates"]
        .as_array()
        .ok_or_else(|| aggregate_sentinel_error("aggregate templates omitted their inventory"))?
        .iter()
        .filter(|template| template["uriTemplate"] == FILE_RESOURCE_TEMPLATE)
        .count();
    if file_template_count != 1 {
        return Err(aggregate_sentinel_error(
            "aggregate files registry did not contribute exactly one resource template",
        ));
    }
    Ok(())
}

async fn aggregate_tool_value(
    driver: &mut OwnedStdioDriver,
    name: &'static str,
    arguments: Value,
) -> TestResult<Value> {
    driver.call_tool(name, arguments).await.map_err(|error| {
        eprintln!("aggregate optional sentinel tool={name} category={error}");
        aggregate_sentinel_error(format!("aggregate {name} call failed"))
    })
}

async fn require_aggregate_representative_reads(
    driver: &mut OwnedStdioDriver,
    ctx: &TestContext,
    page_id: &str,
    chat_id: &str,
    file_id: &str,
    type_id: &str,
    collection_id: &str,
) -> TestResult<()> {
    let body = aggregate_tool_value(
        driver,
        "body_block_list",
        json!({"space":ctx.space_id,"object_id":page_id,"limit":8}),
    )
    .await?;
    if body["object_id"] != page_id || body["space_id"] != ctx.space_id {
        return Err(aggregate_sentinel_error(
            "aggregate body-blocks read returned another fixture",
        ));
    }

    let chats = aggregate_tool_value(
        driver,
        "chat_list",
        json!({"space":ctx.space_id,"limit":20}),
    )
    .await?;
    if !chats["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["id"].as_str() == Some(chat_id))
    }) {
        return Err(aggregate_sentinel_error(
            "aggregate chats read omitted its fixture",
        ));
    }

    let file = aggregate_tool_value(
        driver,
        "file_metadata",
        json!({"space":ctx.space_id,"file_id":file_id}),
    )
    .await?;
    if file["file_id"] != file_id || file["space_id"] != ctx.space_id {
        return Err(aggregate_sentinel_error(
            "aggregate files read returned another fixture",
        ));
    }

    let members = aggregate_tool_value(
        driver,
        "member_list",
        json!({"space":ctx.space_id,"limit":100}),
    )
    .await?;
    if members["items"]
        .as_array()
        .is_none_or(|items| items.is_empty())
    {
        return Err(aggregate_sentinel_error(
            "aggregate members read omitted the disposable-space owner",
        ));
    }

    let schema = aggregate_tool_value(
        driver,
        "type_get",
        json!({"space":ctx.space_id,"type":type_id}),
    )
    .await?;
    if schema.pointer("/type/id").and_then(Value::as_str) != Some(type_id) {
        return Err(aggregate_sentinel_error(
            "aggregate schema read returned another type",
        ));
    }

    let membership = aggregate_tool_value(
        driver,
        "collection_member_list",
        json!({"space":ctx.space_id,"collection_id":collection_id,"limit":20}),
    )
    .await?;
    let contains_page = membership["items"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["object_id"].as_str() == Some(page_id))
    });
    if !contains_page {
        return Err(aggregate_sentinel_error(
            "aggregate views-write read omitted its fixture member",
        ));
    }
    Ok(())
}

fn finish_aggregate_child(
    child: &Arc<Mutex<Option<StdioDriver>>>,
    secrets: &[&[u8]],
    credential_needles: &[Vec<u8>],
) -> TestResult<()> {
    let child = lock_driver(child)
        .take()
        .ok_or_else(|| aggregate_sentinel_error("aggregate stdio child disappeared"))?;
    let (transcript, output) = child
        .try_finish()
        .map_err(|_| aggregate_sentinel_error("aggregate stdio child did not stop cleanly"))?;
    require_spawned_diagnostics(&transcript, &output, secrets, credential_needles)
        .map_err(|_| aggregate_sentinel_error("aggregate child diagnostics were not redacted"))
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_all_optional_toolsets_compose_in_rw_and_preview_ro_children() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let stable_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let preview_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let stable_callback_cleanup = Arc::clone(&stable_cleanup);
    let preview_callback_cleanup = Arc::clone(&preview_cleanup);

    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-all-optional-toolsets",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let suffix = unique_suffix();
                let page_name = format!("private aggregate page {suffix}");
                let page = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(&page_name)
                    .create()
                    .await?;
                ctx.register_object(&page.id);

                let chat_name = format!("private aggregate chat {suffix}");
                let chat = ctx
                    .client
                    .chats()
                    .in_space(&ctx.space_id)
                    .create(
                        &chat_name,
                        Icon::Emoji {
                            emoji: "🧩".to_owned(),
                        },
                    )
                    .create()
                    .await?;
                ctx.register_object(&chat.id);

                let type_name = format!("private aggregate collection type {suffix}");
                let collection_type = ctx.create_collection_type_fixture(&type_name).await?;
                let collection_name = format!("private aggregate collection {suffix}");
                let collection = ctx
                    .create_collection_fixture(&collection_type, &collection_name)
                    .await?;
                ctx.client
                    .view_add_objects(&ctx.space_id, &collection.id, [&page.id])
                    .await?;
                let membership_before = ctx
                    .client
                    .collection_membership_page(&ctx.space_id, &collection.id, 20, None)
                    .await?;
                if membership_before.continuation.is_some()
                    || membership_before.object_ids != [page.id.clone()]
                {
                    return Err(aggregate_sentinel_error(
                        "aggregate collection fixture was not exact",
                    ));
                }
                let credential_needles = disposable_child_credential_needles(ctx.as_ref())?;
                let mut state = 0xA66E_6A7E_u32;
                let mut payload = Vec::with_capacity(8_192);
                for _ in 0..8_192 {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    payload.push((state & 0xff) as u8);
                }
                let encoded_payload = BASE64_STANDARD.encode(&payload);
                let file_name = format!("private-aggregate-{suffix}.bin");

                let stable_child = spawn_disposable_driver(
                    ctx.as_ref(),
                    stable_callback_cleanup,
                    DriverOptions::STANDARD,
                    Some(ALL_OPTIONAL_TOOLSETS_REVERSED),
                )?;
                let mut stable = OwnedStdioDriver {
                    driver: Arc::clone(&stable_child),
                };
                stable.with_driver(StdioDriver::initialize);
                require_aggregate_contract(
                    &mut stable,
                    &STANDARD_CORE_TOOLS,
                    &ALL_OPTIONAL_READ_WRITE_TOOLS,
                )
                .await?;
                let upload = aggregate_tool_value(
                    &mut stable,
                    "file_upload",
                    json!({
                        "space":ctx.space_id,
                        "name":file_name,
                        "content_base64":encoded_payload,
                        "media_type":"application/octet-stream",
                        "idempotency_key":format!("aggregate-file-{suffix}")
                    }),
                )
                .await?;
                let file_id = upload["file_id"]
                    .as_str()
                    .ok_or_else(|| aggregate_sentinel_error("aggregate upload omitted file ID"))?
                    .to_owned();
                ctx.register_file(&file_id);
                require_aggregate_representative_reads(
                    &mut stable,
                    ctx.as_ref(),
                    &page.id,
                    &chat.id,
                    &file_id,
                    &collection_type.id,
                    &collection.id,
                )
                .await?;
                drop(stable);
                finish_aggregate_child(
                    &stable_child,
                    &[
                        page_name.as_bytes(),
                        chat_name.as_bytes(),
                        collection_name.as_bytes(),
                        file_name.as_bytes(),
                        encoded_payload.as_bytes(),
                    ],
                    &credential_needles,
                )?;

                let preview_child = spawn_disposable_driver(
                    ctx.as_ref(),
                    preview_callback_cleanup,
                    DriverOptions::PREVIEW_READ_ONLY,
                    Some(ALL_OPTIONAL_TOOLSETS),
                )?;
                let mut preview = OwnedStdioDriver {
                    driver: Arc::clone(&preview_child),
                };
                preview.with_driver(StdioDriver::initialize);
                require_aggregate_contract(
                    &mut preview,
                    &READ_ONLY_CORE_TOOLS,
                    &ALL_OPTIONAL_READ_ONLY_TOOLS,
                )
                .await?;
                require_aggregate_representative_reads(
                    &mut preview,
                    ctx.as_ref(),
                    &page.id,
                    &chat.id,
                    &file_id,
                    &collection_type.id,
                    &collection.id,
                )
                .await?;

                let rejection = preview.with_driver(|driver| {
                    driver.request(
                        "tools/call",
                        json!({
                            "name":"collection_member_remove",
                            "arguments":{
                                "space":ctx.space_id,
                                "collection_id":collection.id,
                                "object_id":page.id
                            }
                        }),
                    )
                });
                if rejection.pointer("/result/isError") != Some(&json!(true))
                    || rejection
                        .pointer("/result/structuredContent/code")
                        .and_then(Value::as_str)
                        != Some("validation")
                {
                    return Err(aggregate_sentinel_error(
                        "aggregate preview read-only child did not reject a stale mutation",
                    ));
                }

                let membership_after = ctx
                    .client
                    .collection_membership_page(&ctx.space_id, &collection.id, 20, None)
                    .await?;
                if membership_after.object_ids != membership_before.object_ids
                    || membership_after.continuation != membership_before.continuation
                {
                    return Err(aggregate_sentinel_error(
                        "aggregate read-only rejection changed canonical membership",
                    ));
                }
                let page_after = ctx.client.object(&ctx.space_id, &page.id).get().await?;
                if page_after.id != page.id || page_after.name != page.name {
                    return Err(aggregate_sentinel_error(
                        "aggregate read-only rejection changed its fixture object",
                    ));
                }

                drop(preview);
                finish_aggregate_child(
                    &preview_child,
                    &[
                        page_name.as_bytes(),
                        chat_name.as_bytes(),
                        collection_name.as_bytes(),
                        file_name.as_bytes(),
                        encoded_payload.as_bytes(),
                    ],
                    &credential_needles,
                )?;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe aggregate optional-toolset sentinels");

    match outcome {
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *stable_cleanup
                    .lock()
                    .expect("aggregate stable cleanup record"),
                ChildCleanupRecord::Stopped
            );
            assert_eq!(
                *preview_cleanup
                    .lock()
                    .expect("aggregate preview cleanup record"),
                ChildCleanupRecord::Stopped
            );
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            panic!("required aggregate optional-toolset sentinels were skipped: {reason:?}");
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
    let record = Arc::new(Mutex::new(CaseRecord::default()));
    let captured = Arc::clone(&record);
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let child_cleanup = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let callback_cleanup = Arc::clone(&child_cleanup);
    let cleanup = Box::pin(with_disposable_space_context(
        "any-mcp-stdio-read-sentinel",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let name = format!("MCP profile sentinel {}", unique_suffix());
                let object = ctx
                    .client
                    .new_object(&ctx.space_id, "page")
                    .name(&name)
                    .ensure_available()
                    .create()
                    .await?;
                ctx.register_object(&object.id);
                let mut evidence = ScenarioEvidence::new(ScenarioId::Documents);
                evidence.sensitive(&name);
                evidence.fixture(&object.id);
                let driver =
                    spawn_disposable_driver(ctx.as_ref(), callback_cleanup, options, None)?;
                lock_driver(&driver)
                    .as_mut()
                    .ok_or_else(|| sentinel_assertion("registered read child disappeared"))?
                    .initialize();
                let mut owned = OwnedStdioDriver {
                    driver: Arc::clone(&driver),
                };
                let result = AssertUnwindSafe(async {
                    let tools = owned.list_tools().await?;
                    if options.profile == "compact"
                        && !tools.iter().any(|name| name == "object_get")
                    {
                        return Err("compact catalog omitted object_get".to_owned());
                    }
                    if options.read_only && tools.iter().any(|name| name == "object_edit") {
                        return Err("read-only catalog retained object_edit".to_owned());
                    }
                    owned
                        .call_tool(
                            "object_get",
                            json!({"space": ctx.space_id, "object_id": object.id}),
                        )
                        .await?;
                    Ok::<(), String>(())
                })
                .catch_unwind()
                .await;
                drop(owned);
                let driver = lock_driver(&driver)
                    .take()
                    .ok_or_else(|| sentinel_assertion("registered read child disappeared"))?;
                let mut completed = complete_case(driver, evidence, result, options);
                completed.scenario = format!("{}_read_sentinel", options.profile);
                *captured.lock().expect("sentinel case record lock") = completed;
                Ok(())
            })
        },
    ))
    .await;
    let cleanup_status = if cleanup.is_ok() { "success" } else { "failed" };
    {
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
    }
    match cleanup.expect("cleanup-safe spawned read sentinel") {
        DisposableRun::Completed(()) => {
            assert!(callback_ran.load(Ordering::SeqCst));
            assert_eq!(
                *child_cleanup.lock().expect("read child cleanup record"),
                ChildCleanupRecord::Stopped
            );
        }
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            panic!("required disposable spawned read sentinel was skipped: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_stdio_compact_sentinel() {
    run_spawned_read_sentinel(DriverOptions::COMPACT).await;
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
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

/// Spawned control planes covered by the artifact acceptance matrix.
#[cfg(feature = "acceptance-harness")]
const SPAWNED_ARTIFACT_CONTROLS: [ArtifactControlPlane; 3] = [
    ArtifactControlPlane::ScriptedProtocol,
    ArtifactControlPlane::SpawnedStableStdio,
    ArtifactControlPlane::SpawnedPreviewStdio,
];

/// Production stdio controls that own the adversarial artifact matrix.
#[cfg(feature = "acceptance-harness")]
const ADVERSARIAL_STDIO_CONTROLS: [ArtifactControlPlane; 2] = [
    ArtifactControlPlane::SpawnedStableStdio,
    ArtifactControlPlane::SpawnedPreviewStdio,
];

/// Aggregate diagnostic limits for one complete adversarial production child.
#[cfg(feature = "acceptance-harness")]
const ADVERSARIAL_CHILD_OUTPUT_BYTES: u64 = 1_048_576;

/// Maximum retained sensitive value passed only to the bounded log audit.
#[cfg(feature = "acceptance-harness")]
const ARTIFACT_LOG_NEEDLE_BYTES: usize = 4 * 1024;

/// Required reviewed Anytype server log inspected for new failure classes.
#[cfg(feature = "acceptance-harness")]
const ARTIFACT_SERVER_LOG_ENV: &str = "ANY_MCP_HEADLESS_REDACTED_LOG_FILE";

/// Test-side capability directory for one private artifact child gate.
///
/// The production child treats this directory and its nonce as a capability;
/// all marker names and payloads stay derived from the nonce.
#[cfg(feature = "acceptance-harness")]
#[allow(dead_code)]
#[derive(Clone)]
struct ChildArtifactGate {
    directory: PathBuf,
    nonce: String,
    key: String,
    owner: Arc<()>,
}

#[cfg(feature = "acceptance-harness")]
#[allow(dead_code)]
impl ChildArtifactGate {
    fn create(base: &Path, point: &str, key: &str) -> TestResult<Self> {
        let digest = Sha256::digest(format!("{}:{}:{}", point, key, unique_suffix()).as_bytes());
        // Production requires exactly 64 lowercase hex characters; the full
        // 32-byte digest encodes to exactly that.
        let nonce = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let directory = base.join(format!("artifact-gate-{nonce}"));
        std::fs::create_dir(&directory)
            .map_err(|_| sentinel_assertion("create artifact gate directory"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| sentinel_assertion("secure artifact gate directory"))?;
        }
        Ok(Self {
            directory,
            nonce,
            key: key.to_owned(),
            owner: Arc::new(()),
        })
    }

    fn configure(&self, command: &mut Command, point: &str, key: &str) {
        command
            .env("ANY_MCP_ACCEPTANCE_GATE_DIR", &self.directory)
            .env(
                "ANY_MCP_ACCEPTANCE_GATE",
                format!("v1|{point}|{key}|{}", self.nonce),
            );
    }

    fn marker(&self, kind: &str) -> PathBuf {
        self.directory.join(format!("{kind}-{}", self.nonce))
    }

    fn key(&self) -> &str {
        &self.key
    }

    fn wait_ready(&self) -> TestResult<()> {
        self.wait_marker("ready")
    }
    fn wait_done(&self) -> TestResult<()> {
        self.wait_marker("done")
    }

    fn wait_marker(&self, kind: &str) -> TestResult<()> {
        let path = self.marker(kind);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !path.exists() {
            if std::time::Instant::now() >= deadline {
                return Err(sentinel_assertion("artifact gate marker timeout"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| sentinel_assertion("inspect artifact gate marker"))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || std::fs::read(&path).ok().as_deref() != Some(format!("{}\n", self.nonce).as_bytes())
        {
            return Err(sentinel_assertion("artifact gate marker was invalid"));
        }
        Ok(())
    }

    fn release(&self) -> TestResult<()> {
        use std::io::Write as _;
        let path = self.marker("release");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|_| sentinel_assertion("create artifact gate release marker"))?;
        file.write_all(format!("{}\n", self.nonce).as_bytes())
            .map_err(|_| sentinel_assertion("write artifact gate release marker"))?;
        file.sync_all()
            .map_err(|_| sentinel_assertion("sync artifact gate release marker"))
    }
}

#[cfg(feature = "acceptance-harness")]
impl Drop for ChildArtifactGate {
    fn drop(&mut self) {
        if Arc::strong_count(&self.owner) != 1 {
            return;
        }
        let _ = self.release();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(feature = "acceptance-harness")]
struct ChildArtifactGateHooks(ChildArtifactGate);

#[cfg(feature = "acceptance-harness")]
struct ChildArtifactGateLease(ChildArtifactGate);

#[cfg(feature = "acceptance-harness")]
impl ArtifactGateLease for ChildArtifactGateLease {
    fn wait<'a>(
        &'a mut self,
        _timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        // `block_in_place` panics on a current-thread test runtime; a
        // dedicated blocking task waits on the child gate on every flavor.
        let gate = self.0.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || gate.wait_ready().is_ok())
                .await
                .unwrap_or(false)
        })
    }

    fn release(&self) {
        let _ = self.0.release();
    }
}

#[cfg(feature = "acceptance-harness")]
impl ArtifactGateHooks for ChildArtifactGateHooks {
    fn arm_import<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>> {
        Box::pin(async move {
            if key != self.0.key() {
                return Err("stable child import gate key did not match".to_owned());
            }
            Ok(Box::new(ChildArtifactGateLease(self.0.clone())) as Box<dyn ArtifactGateLease>)
        })
    }

    fn arm_export<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>> {
        Box::pin(async move {
            if key != self.0.key() {
                return Err("stable child export gate key did not match".to_owned());
            }
            Ok(Box::new(ChildArtifactGateLease(self.0.clone())) as Box<dyn ArtifactGateLease>)
        })
    }

    fn arm_document<'a>(
        &'a self,
        _key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn ArtifactGateLease>, String>> + Send + 'a>> {
        Box::pin(std::future::ready(Err(
            "stable child does not arm a document gate in this owner".to_owned(),
        )))
    }
}

#[cfg(feature = "acceptance-harness")]
fn artifact_driver_options(control: ArtifactControlPlane) -> DriverOptions {
    // Every control plane uses the standard profile so the advertised artifact
    // catalog is comparable across the complete matrix.
    if control == ArtifactControlPlane::SpawnedPreviewStdio {
        DriverOptions::PREVIEW_STANDARD
    } else {
        DriverOptions::STANDARD
    }
}

/// Records the captured server-log descriptor before the acceptance matrix runs.
#[cfg(feature = "acceptance-harness")]
fn artifact_server_log_baseline() -> ArtifactServerLogBaseline {
    let path = PathBuf::from(
        std::env::var_os(ARTIFACT_SERVER_LOG_ENV)
            .unwrap_or_else(|| panic!("reviewed artifact server-log evidence was not configured")),
    );
    server_log_baseline(&path)
        .unwrap_or_else(|error| panic!("captured artifact server log baseline: {error}"))
}

/// Retains sensitive values solely for the descriptor-bounded server-log audit.
#[cfg(feature = "acceptance-harness")]
fn record_artifact_log_needle(needles: &Arc<Mutex<Vec<Vec<u8>>>>, value: &[u8]) -> TestResult<()> {
    if value.is_empty() || value.len() > ARTIFACT_LOG_NEEDLE_BYTES {
        return Err(sentinel_assertion(
            "artifact log audit received an invalid forbidden value",
        ));
    }
    needles
        .lock()
        .map_err(|_| sentinel_assertion("artifact log-audit needles lock poisoned"))?
        .push(value.to_vec());
    Ok(())
}

/// Retains a staging bearer for the descriptor-bounded audit without reporting it.
#[cfg(feature = "acceptance-harness")]
fn record_artifact_stage_log_needle(
    needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    bearer: &[u8],
) -> Result<(), String> {
    if bearer.is_empty() || bearer.len() > ARTIFACT_LOG_NEEDLE_BYTES {
        return Err("artifact log audit received an invalid staging bearer".to_owned());
    }
    needles
        .lock()
        .map_err(|_| "artifact log-audit needles lock poisoned".to_owned())?
        .push(bearer.to_vec());
    Ok(())
}

/// Adds the private fixture base path to the server-log redaction audit.
#[cfg(feature = "acceptance-harness")]
fn artifact_fixture_log_needle(policy: &ArtifactPolicyFixture) -> TestResult<Vec<u8>> {
    let needle = policy.forbidden_log_needle();
    if needle.is_empty() || needle.len() > ARTIFACT_LOG_NEEDLE_BYTES {
        return Err(sentinel_assertion(
            "artifact fixture produced an invalid log-audit value",
        ));
    }
    Ok(needle.to_vec())
}

/// Adds the private fixture base path to the server-log redaction audit.
#[cfg(feature = "acceptance-harness")]
fn record_artifact_fixture_log_needle(
    policy: &ArtifactPolicyFixture,
    needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<()> {
    let needle = artifact_fixture_log_needle(policy)?;
    record_artifact_log_needle(needles, &needle)
}

/// Adds disposable child credentials to the server-log redaction audit.
#[cfg(feature = "acceptance-harness")]
fn record_artifact_credential_log_needles(
    ctx: &TestContext,
    needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<()> {
    for credential in disposable_child_credential_needles(ctx)? {
        record_artifact_log_needle(needles, &credential)?;
    }
    Ok(())
}

/// Audits the retained descriptor without exposing paths, credentials, or log bytes.
#[cfg(feature = "acceptance-harness")]
fn assert_artifact_server_log_clean(
    baseline: &ArtifactServerLogBaseline,
    needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    workflow: &'static str,
) -> ArtifactServerLogAudit {
    let needles = needles.lock().expect("artifact log-audit needles lock");
    let borrowed = needles.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let audit = audit_server_log(baseline, &borrowed).expect("audit captured artifact server log");
    eprintln!(
        "artifact {workflow} server log inspected={} panic_or_fatal={} unclassified={} forbidden={}",
        audit.inspected_lines,
        audit.panic_or_fatal_lines,
        audit.unclassified_error_lines,
        audit.forbidden_needle_matches,
    );
    assert!(
        audit.is_clean() && audit.forbidden_needle_matches == 0,
        "captured server log violated the fixed-category or redaction contract"
    );
    audit
}

/// Runs the complete spawned artifact acceptance matrix in one disposable space.
///
/// Each spawned control plane owns a private strict policy, exports through
/// both data planes, and contributes one content-free evidence record. The
/// records are compared for exact parity after every child has stopped.
#[cfg(feature = "acceptance-harness")]
async fn run_artifacts_real_workflow() -> OptionalRealWorkflowRun {
    let cleanup: [Arc<Mutex<ChildCleanupRecord>>; SPAWNED_ARTIFACT_CONTROLS.len()] =
        std::array::from_fn(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)));
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                let mut evidence = Vec::with_capacity(ArtifactTransport::SPAWNED_MATRIX.len());
                for (index, control) in SPAWNED_ARTIFACT_CONTROLS.into_iter().enumerate() {
                    let options = artifact_driver_options(control);
                    let policy =
                        Arc::new(ArtifactPolicyFixture::create(&ctx.space_id).map_err(|_| {
                            sentinel_assertion("create artifact acceptance policy")
                        })?);
                    record_artifact_fixture_log_needle(&policy, &callback_audit_needles)?;
                    let record = callback_cleanup
                        .get(index)
                        .ok_or_else(|| sentinel_assertion("artifact cleanup record missing"))?;
                    let child = spawn_disposable_artifact_driver(
                        ctx.as_ref(),
                        Arc::clone(record),
                        Arc::clone(&policy),
                        options,
                    )?;
                    lock_driver(&child)
                        .as_mut()
                        .ok_or_else(|| sentinel_assertion("registered artifact child disappeared"))?
                        .initialize();

                    for data in ArtifactDataPlane::ALL {
                        let transport = ArtifactTransport::new(control, data);
                        eprintln!("artifact acceptance transport={}", transport.id());
                        let fixture = ArtifactSmokeFixture {
                            transport,
                            policy: policy.as_ref(),
                            ctx: ctx.as_ref(),
                        };
                        let observed = if control == ArtifactControlPlane::ScriptedProtocol {
                            let mut driver = ScriptedArtifactDriver {
                                driver: Arc::clone(&child),
                            };
                            Box::pin(run_artifact_smoke_scenario(&mut driver, &fixture)).await
                        } else {
                            let mut driver = OwnedStdioDriver {
                                driver: Arc::clone(&child),
                            };
                            Box::pin(run_artifact_smoke_scenario(&mut driver, &fixture)).await
                        };
                        evidence.push(observed.map_err(|_| {
                            eprintln!(
                                "artifact acceptance transport={} outcome=failed",
                                transport.id()
                            );
                            sentinel_assertion("artifact acceptance transport failed")
                        })?);
                    }
                }

                assert_artifact_parity(&evidence, &ArtifactTransport::SPAWNED_MATRIX).map_err(
                    |_| {
                        eprintln!("artifact acceptance parity outcome=diverged");
                        sentinel_assertion("spawned artifact transports diverged")
                    },
                )?;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned artifact acceptance matrix");

    match outcome {
        DisposableRun::Completed(()) => {
            for record in &cleanup {
                assert_eq!(
                    *record.lock().expect("artifact cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            assert_artifact_server_log_clean(&log_baseline, &audit_needles, "acceptance");
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(_) => {
            eprintln!("artifact acceptance outcome=admission_skipped");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_spawned_transport_matrix_scenario() {
    require_optional_workflow_executed(run_artifacts_real_workflow().await)
        .expect("spawned artifact acceptance matrix");
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_crash06_mid_frame_scenario() {
    let cleanup_record = Arc::new(Mutex::new(ChildCleanupRecord::NotRun));
    let callback_cleanup = Arc::clone(&cleanup_record);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-crash06",
        move |ctx| {
            Box::pin(async move {
                let policy = Arc::new(
                    ArtifactPolicyFixture::create(&ctx.space_id)
                        .map_err(|_| sentinel_assertion("create CRASH-06 artifact fixture"))?,
                );
                let (child, pause) = spawn_disposable_mid_frame_crash_driver(
                    ctx.as_ref(),
                    callback_cleanup,
                    policy,
                )?;
                {
                    let mut guard = lock_driver(&child);
                    let driver = guard.as_mut().ok_or_else(|| {
                        sentinel_assertion("registered CRASH-06 child disappeared")
                    })?;
                    driver.initialize();
                    let request_id = driver.next_id;
                    driver.next_id = driver.next_id.saturating_add(1);
                    driver.process.send(json!({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "tools/list",
                        "params": {}
                    }));
                }
                pause
                    .wait_ready(Duration::from_secs(30))
                    .map_err(|_| sentinel_assertion("CRASH-06 never reached a stdout frame"))?;
                let driver = lock_driver(&child)
                    .take()
                    .ok_or_else(|| sentinel_assertion("registered CRASH-06 child disappeared"))?;
                let (_, output) = driver
                    .terminate()
                    .map_err(|_| sentinel_assertion("terminate CRASH-06 child"))?;
                let execution = crash06_mid_frame_evidence(&output)
                    .map_err(|_| sentinel_assertion("validate CRASH-06 stdout capture"))?;
                execution
                    .assert_exact(&[AdversarialCaseId::Crash06])
                    .map_err(|_| sentinel_assertion("CRASH-06 owner inventory diverged"))?;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe CRASH-06 acceptance");
    require_completed(outcome, "CRASH-06 acceptance")
        .expect("prefix-authorized disposable admission");
    assert_eq!(
        *cleanup_record.lock().expect("CRASH-06 cleanup record"),
        ChildCleanupRecord::Stopped
    );
}

#[cfg(feature = "acceptance-harness")]
async fn run_spawned_validator_flood_cases(
    ctx: &TestContext,
    cleanup: [Arc<Mutex<ChildCleanupRecord>>; 2],
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<AdversarialExecution> {
    // Production pins validator executables under a 128 MiB hash ceiling
    // with a non-writable mode, so the flood fixtures pin the dedicated
    // small validator binary rather than the debug process-test binary.
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_any-mcp-validator-fixture"));
    let optional = Arc::new(
        ArtifactPolicyFixture::create_with_validator_executable(
            &ctx.space_id,
            ArtifactPolicyOptions {
                validators: FixtureValidatorPolicy::Optional,
                ..ArtifactPolicyOptions::default()
            },
            &executable,
        )
        .map_err(|_| sentinel_assertion("create optional validator-flood fixture"))?,
    );
    record_artifact_fixture_log_needle(&optional, audit_needles)?;
    let optional_child = spawn_disposable_artifact_driver(
        ctx,
        Arc::clone(&cleanup[0]),
        Arc::clone(&optional),
        DriverOptions::STANDARD,
    )?;
    lock_driver(&optional_child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("optional validator-flood child disappeared"))?
        .initialize();
    let staging_before = optional
        .staging_snapshot()
        .map_err(|_| sentinel_assertion("capture validator-flood staging state"))?;
    let export_before = optional
        .export_snapshot()
        .map_err(|_| sentinel_assertion("capture validator-flood export state"))?;
    let mut execution = AdversarialExecution::default();
    for (case, label) in [
        (AdversarialCaseId::Flood01, "FLOOD-01"),
        (AdversarialCaseId::Flood03, "FLOOD-03"),
    ] {
        let source = format!("{}-{}.txt", label.to_ascii_lowercase(), unique_suffix());
        optional
            .seed_import(&source, label.as_bytes())
            .map_err(|_| sentinel_assertion("seed validator-flood source"))?;
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&optional_child),
        };
        let imported = driver
            .call_tool(
                "file_import",
                json!({
                    "space": ctx.space_id,
                    "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": source}},
                    "name": format!("{label}.txt"),
                    "media_type": "text/plain",
                    "idempotency_key": format!("{label}-{}", unique_suffix()),
                }),
            )
            .await
            .map_err(|_| sentinel_assertion("optional validator flood import failed"))?;
        let file_id = imported
            .get("file_id")
            .and_then(Value::as_str)
            .ok_or_else(|| sentinel_assertion("validator-flood import omitted file id"))?;
        ctx.register_file(file_id);
        let expected = [json!({"id": "mime", "status": "failed"})];
        if imported
            .pointer("/receipt/validators")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            != Some(expected.as_slice())
            || serde_json::to_vec(&imported)
                .map_err(|_| sentinel_assertion("serialize validator-flood result"))?
                .len()
                > ARTIFACT_FRAME_CEILING_BYTES as usize
        {
            // Bounded fixed-category findings only (id/status pairs).
            eprintln!(
                "validator-flood {label} findings diverged: {:?}",
                imported.pointer("/receipt/validators")
            );
            return Err(sentinel_assertion(
                "validator flood result was not one bounded finding",
            ));
        }
        execution
            .record_executed(case)
            .map_err(|_| sentinel_assertion("record validator-flood case"))?;
    }
    if optional.staging_snapshot().ok() != Some(staging_before)
        || optional.export_snapshot().ok() != Some(export_before)
    {
        return Err(sentinel_assertion(
            "validator flood changed artifact private state",
        ));
    }
    finish_registered_artifact_child(&optional_child, None)
        .map_err(|_| sentinel_assertion("stop optional validator-flood child"))?;

    let required = Arc::new(
        ArtifactPolicyFixture::create_with_validator_executable(
            &ctx.space_id,
            ArtifactPolicyOptions {
                validators: FixtureValidatorPolicy::Required,
                ..ArtifactPolicyOptions::default()
            },
            &executable,
        )
        .map_err(|_| sentinel_assertion("create required validator-flood fixture"))?,
    );
    record_artifact_fixture_log_needle(&required, audit_needles)?;
    required
        .seed_import("flood02.txt", b"FLOOD-02")
        .map_err(|_| sentinel_assertion("seed validator-timeout source"))?;
    let required_child = spawn_disposable_artifact_driver(
        ctx,
        Arc::clone(&cleanup[1]),
        Arc::clone(&required),
        DriverOptions::STANDARD,
    )?;
    lock_driver(&required_child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("required validator-flood child disappeared"))?
        .initialize();
    let started = std::time::Instant::now();
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(&required_child),
    };
    let refusal = driver
        .call_tool_error(
            "file_import",
            json!({
                "space": ctx.space_id,
                "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": "flood02.txt"}},
                "name": "FLOOD-02.txt",
                "media_type": "text/plain",
                "idempotency_key": format!("FLOOD-02-{}", unique_suffix()),
            }),
        )
        .await
        .map_err(|_| sentinel_assertion("required validator timeout omitted tool error"))?;
    if refusal.code() != "validation" || started.elapsed() > Duration::from_secs(25) {
        return Err(sentinel_assertion(
            "FLOOD-02 did not return bounded validator timeout",
        ));
    }
    finish_registered_artifact_child(&required_child, None)
        .map_err(|_| sentinel_assertion("stop required validator-flood child"))?;
    execution
        .record_executed(AdversarialCaseId::Flood02)
        .map_err(|_| sentinel_assertion("record validator timeout case"))?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_validator_flood_spawned_scenarios() {
    let cleanup = std::array::from_fn(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)));
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-validator-flood",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                let execution = run_spawned_validator_flood_cases(
                    ctx.as_ref(),
                    callback_cleanup,
                    &callback_audit_needles,
                )
                .await
                .inspect_err(|error| {
                    // Fixed harness category only; the disposable wrapper
                    // withholds callback messages.
                    eprintln!("validator-flood inner failure: {error:?}");
                })?;
                execution
                    .assert_exact(&[
                        AdversarialCaseId::Flood01,
                        AdversarialCaseId::Flood02,
                        AdversarialCaseId::Flood03,
                    ])
                    .map_err(|_| sentinel_assertion("validator-flood owner inventory diverged"))
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned validator-flood acceptance");
    require_completed(outcome, "spawned validator-flood acceptance")
        .expect("prefix-authorized disposable admission");
    for record in &cleanup {
        assert_eq!(
            *record.lock().expect("validator-flood cleanup record"),
            ChildCleanupRecord::Stopped
        );
    }
    assert_artifact_server_log_clean(&log_baseline, &audit_needles, "validator-flood");
}

#[cfg(feature = "acceptance-harness")]
async fn run_spawned_artifact_adversarial_default(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    control: ArtifactControlPlane,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<AdversarialExecution> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create(&ctx.space_id)
            .map_err(|_| sentinel_assertion("create adversarial artifact fixture"))?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)?;
    let child = spawn_disposable_artifact_driver(
        ctx,
        cleanup_record,
        Arc::clone(&policy),
        artifact_driver_options(control),
    )?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("registered adversarial artifact child disappeared"))?
        .initialize();

    let run = ArtifactAdversarialRun {
        control,
        policy: policy.as_ref(),
        ctx,
        root_access_attempts: None,
        successful_import_opens: None,
        gate_hooks: None,
    };
    let observed = {
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        Box::pin(async {
            let mut execution = run_artifact_adversarial_stdio_sentinels(&mut driver, &run).await?;
            if control == ArtifactControlPlane::SpawnedStableStdio {
                execution.merge(
                    run_artifact_dynamic_filesystem_stdio_sentinels(&mut driver, &run).await?,
                )?;
                run_artifact_diagnostic_flood_burst(&mut driver).await?;
            }
            Ok::<_, String>(execution)
        })
        .await
    };
    let process = finish_registered_artifact_child(&child, None).map_err(|_| {
        eprintln!(
            "artifact adversarial control={} child_outcome=failed",
            control.as_str()
        );
        sentinel_assertion("spawned adversarial artifact child did not stop cleanly")
    })?;
    if process.stdout_bytes > ADVERSARIAL_CHILD_OUTPUT_BYTES
        || process.stderr_bytes > ADVERSARIAL_CHILD_OUTPUT_BYTES
    {
        return Err(sentinel_assertion(
            "spawned adversarial artifact child exceeded its output bound",
        ));
    }
    let mut execution = observed.map_err(|_| {
        eprintln!(
            "artifact adversarial control={} outcome=failed",
            control.as_str()
        );
        sentinel_assertion("spawned adversarial artifact scenarios failed")
    })?;
    if control == ArtifactControlPlane::SpawnedStableStdio {
        execution
            .record_executed(AdversarialCaseId::Flood07)
            .map_err(|_| sentinel_assertion("record stable FLOOD-07 evidence"))?;
        execution.record_quota_not_applicable();
        execution
            .merge(
                run_spawned_artifact_gated_race(
                    ctx,
                    Arc::new(Mutex::new(ChildCleanupRecord::NotRun)),
                    audit_needles,
                    "import-first-upload-chunk",
                    AdversarialCaseId::Race01,
                )
                .await?,
            )
            .map_err(|_| sentinel_assertion("merge stable RACE-01 evidence"))?;
        execution
            .merge(
                run_spawned_artifact_gated_race(
                    ctx,
                    Arc::new(Mutex::new(ChildCleanupRecord::NotRun)),
                    audit_needles,
                    "export-prepublication",
                    AdversarialCaseId::Race04,
                )
                .await?,
            )
            .map_err(|_| sentinel_assertion("merge stable RACE-04 evidence"))?;
    }
    let mut expected = ADVERSARIAL_STDIO_SENTINEL_IDS.to_vec();
    if control == ArtifactControlPlane::SpawnedStableStdio {
        expected.extend(ADVERSARIAL_DYNAMIC_STDIO_IMPLEMENTED_IDS);
        expected.push(AdversarialCaseId::Flood07);
    }
    execution
        .assert_exact(&expected)
        .map_err(|_| sentinel_assertion("spawned adversarial case inventory diverged"))?;
    for needle in execution.forbidden_log_needles() {
        record_artifact_log_needle(audit_needles, needle)?;
    }
    Ok(execution)
}

/// FLOOD-07: a rapid burst of failing calls must produce byte-uniform bounded
/// refusals that never echo the offered handle. The caller separately asserts
/// the aggregate child diagnostic ceiling and the redaction audit after
/// shutdown, so the burst plus those checks form the complete case evidence.
#[cfg(feature = "acceptance-harness")]
async fn run_artifact_diagnostic_flood_burst(driver: &mut OwnedStdioDriver) -> Result<(), String> {
    let mut expected: Option<Value> = None;
    for index in 0..48_u32 {
        let error = driver
            .call_tool_error(
                "artifact_release",
                json!({"handle": format!("flood07-burst-{index}-{}", unique_suffix())}),
            )
            .await?;
        if error.code() != "not_found" {
            return Err("FLOOD-07 burst produced a non-uniform error class".to_owned());
        }
        match &expected {
            None => expected = Some(error.normalized_result().clone()),
            Some(first) if first == error.normalized_result() => {}
            Some(_) => return Err("FLOOD-07 burst responses diverged".to_owned()),
        }
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
async fn run_spawned_artifact_gated_race(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    point: &'static str,
    case: AdversarialCaseId,
) -> TestResult<AdversarialExecution> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create(&ctx.space_id)
            .map_err(|_| sentinel_assertion("create gated artifact fixture"))?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)?;
    let key = format!(
        "{}-stable-{}",
        case.as_str().to_ascii_lowercase(),
        unique_suffix()
    );
    let (child, gate) = spawn_disposable_gated_artifact_driver(
        ctx,
        cleanup_record,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
        point,
        key.clone(),
    )?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("registered gated child disappeared"))?
        .initialize();
    let hooks = ChildArtifactGateHooks(gate);
    let run = ArtifactAdversarialRun {
        control: ArtifactControlPlane::SpawnedStableStdio,
        policy: policy.as_ref(),
        ctx,
        root_access_attempts: None,
        successful_import_opens: None,
        gate_hooks: Some(&hooks),
    };
    let observed = {
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        let attempt = AssertUnwindSafe(async {
            match case {
                AdversarialCaseId::Race01 => run_artifact_race01(&mut driver, &run, key).await,
                AdversarialCaseId::Race04 => run_artifact_race04(&mut driver, &run, key).await,
                _ => Err("unsupported stable gated race case".to_owned()),
            }
        })
        .catch_unwind()
        .await;
        match attempt {
            Ok(result) => result,
            Err(_) => Err("gated race request hit its transport deadline".to_owned()),
        }
    };
    let mut owned = lock_driver(&child)
        .take()
        .ok_or_else(|| sentinel_assertion("registered gated child disappeared"))?;
    if observed.is_err()
        && let Some(failure) = owned.process.take_failure()
    {
        // Child stderr carries only fixed diagnostic categories (the log
        // audit proves needle absence); print it for gate diagnosis.
        eprintln!(
            "gated race child failure category={} stderr:\n{}",
            failure.category,
            String::from_utf8_lossy(&failure.output.stderr)
        );
    }
    let finished = if observed.is_err() {
        owned.terminate()
    } else {
        owned.try_finish()
    }
    .map_err(|_| sentinel_assertion("gated artifact child did not stop cleanly"))?;
    if observed.is_err() {
        eprintln!(
            "gated race child stderr:\n{}",
            String::from_utf8_lossy(&finished.1.stderr)
        );
    }
    artifact_child_process_evidence(&finished.1, None)
        .map_err(|_| sentinel_assertion("gated artifact child evidence"))?;
    observed
        .inspect_err(|error| {
            // Fixed harness category only.
            eprintln!("gated artifact race case={case:?} inner failure: {error}");
        })
        .map_err(|_| sentinel_assertion("gated artifact race failed"))?;
    let mut execution = AdversarialExecution::default();
    execution
        .record_executed(case)
        .map_err(|_| sentinel_assertion("record gated artifact race"))?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
fn alias07_policy_contents(policy: &ArtifactPolicyFixture) -> TestResult<String> {
    let configured = policy
        .policy_contents()
        .map_err(|_| sentinel_assertion("read adversarial startup policy"))?;
    let export = format!(
        "[[roots.export]]\nid = \"{}\"",
        ArtifactPolicyFixture::EXPORT_ROOT
    );
    let replacement = "[[roots.export]]\nid = \"INBOX\"";
    if !configured.contains(&export) {
        return Err(sentinel_assertion(
            "adversarial startup policy omitted its export root",
        ));
    }
    Ok(configured.replacen(&export, replacement, 1))
}

#[cfg(feature = "acceptance-harness")]
fn run_alias07_startup_rejection(
    ctx: &TestContext,
    control: ArtifactControlPlane,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<AdversarialExecution> {
    let policy = ArtifactPolicyFixture::create(&ctx.space_id)
        .map_err(|_| sentinel_assertion("create adversarial startup fixture"))?;
    let fixture_needle = artifact_fixture_log_needle(&policy)?;
    record_artifact_log_needle(audit_needles, &fixture_needle)?;
    std::fs::write(policy.config_path(), alias07_policy_contents(&policy)?)
        .map_err(|_| sentinel_assertion("write adversarial startup policy"))?;

    let credential_needles = disposable_child_credential_needles(ctx)?;
    let environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| sentinel_assertion("adversarial startup omitted child environment"))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    environment.configure(&mut command)?;
    configure_stdio_command(
        &mut command,
        artifact_driver_options(control),
        Some("artifacts"),
    );
    command.env("ANY_MCP_CONFIG", policy.config_path());

    let mut process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(5));
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| process.read_frame()))
        .expect_err("ALIAS-07 startup rejection closes stdout without a frame");
    let panic_text = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    if panic_text != "bounded protocol process failed: child_eof" {
        return Err(sentinel_assertion(
            "ALIAS-07 startup rejection did not produce the bounded EOF category",
        ));
    }
    let failure = process
        .take_failure()
        .ok_or_else(|| sentinel_assertion("ALIAS-07 omitted bounded process evidence"))?;
    if failure.category != "child_eof"
        || failure.output.exit_category != "exit_code"
        || !failure.output.stdout.is_empty()
        || failure.output.stderr.len() > support::process::MAX_STDERR_BYTES
    {
        return Err(sentinel_assertion(
            "ALIAS-07 startup rejection violated the production output contract",
        ));
    }
    let stderr = std::str::from_utf8(&failure.output.stderr)
        .map_err(|_| sentinel_assertion("ALIAS-07 stderr was not UTF-8"))?;
    let lines = stderr
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let category = match lines.as_slice() {
        [line]
            if line
                .strip_suffix(
                    "any-mcp startup or service failure reason=invalid any-mcp artifact root",
                )
                .is_some_and(|prefix| {
                    prefix
                        .split_ascii_whitespace()
                        .any(|field| field == "ERROR")
                }) =>
        {
            "invalid any-mcp artifact root"
        }
        _ => "unexpected startup category",
    };
    ExpectedOutcome::StartupRejected {
        category: "invalid any-mcp artifact root",
    }
    .assert_matches(ObservedOutcome::StartupRejected { category })
    .map_err(|_| sentinel_assertion("ALIAS-07 startup category diverged"))?;
    if contains_bytes(&failure.output.stderr, &fixture_needle)
        || credential_needles
            .iter()
            .any(|needle| contains_bytes(&failure.output.stderr, needle))
    {
        return Err(sentinel_assertion(
            "ALIAS-07 startup diagnostics exposed disposable credentials",
        ));
    }
    let mut execution = AdversarialExecution::default();
    execution
        .record_executed(AdversarialCaseId::Alias07)
        .map_err(|_| sentinel_assertion("record ALIAS-07 startup rejection"))?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
fn run_dynamic_symlink_startup_rejection(
    ctx: &TestContext,
    target: ArtifactSymlinkStartupTarget,
    category: &'static str,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<ArtifactStartupCaseOutcome> {
    let policy = ArtifactPolicyFixture::create(&ctx.space_id)
        .map_err(|_| sentinel_assertion("create dynamic symlink startup fixture"))?;
    let fixture_needle = artifact_fixture_log_needle(&policy)?;
    record_artifact_log_needle(audit_needles, &fixture_needle)?;
    if !prepare_artifact_symlink_startup_case(&policy, target)
        .map_err(|_| sentinel_assertion("prepare dynamic symlink startup fixture"))?
    {
        return Ok(ArtifactStartupCaseOutcome::Unsupported);
    }

    let credential_needles = disposable_child_credential_needles(ctx)?;
    let environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| sentinel_assertion("dynamic symlink startup omitted child environment"))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    environment.configure(&mut command)?;
    configure_stdio_command(&mut command, DriverOptions::STANDARD, Some("artifacts"));
    command.env("ANY_MCP_CONFIG", policy.config_path());

    let mut process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(5));
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| process.read_frame()))
        .expect_err("dynamic symlink startup rejection closes stdout without a frame");
    let panic_text = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    if panic_text != "bounded protocol process failed: child_eof" {
        return Err(sentinel_assertion(
            "dynamic symlink startup rejection did not produce the bounded EOF category",
        ));
    }
    let failure = process.take_failure().ok_or_else(|| {
        sentinel_assertion("dynamic symlink startup omitted bounded process evidence")
    })?;
    if failure.category != "child_eof"
        || failure.output.exit_category != "exit_code"
        || !failure.output.stdout.is_empty()
        || failure.output.stderr.len() > support::process::MAX_STDERR_BYTES
    {
        return Err(sentinel_assertion(
            "dynamic symlink startup violated the production output contract",
        ));
    }
    let stderr = std::str::from_utf8(&failure.output.stderr)
        .map_err(|_| sentinel_assertion("dynamic symlink startup stderr was not UTF-8"))?;
    let expected_suffix = format!("any-mcp startup or service failure reason={category}");
    let lines = stderr
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !matches!(
        lines.as_slice(),
        [line]
            if line.strip_suffix(&expected_suffix).is_some_and(|prefix| {
                prefix
                    .split_ascii_whitespace()
                    .any(|field| field == "ERROR")
            })
    ) {
        return Err(sentinel_assertion(
            "dynamic symlink startup category diverged",
        ));
    }
    if contains_bytes(&failure.output.stderr, &fixture_needle)
        || credential_needles
            .iter()
            .any(|needle| contains_bytes(&failure.output.stderr, needle))
    {
        return Err(sentinel_assertion(
            "dynamic symlink startup diagnostics exposed forbidden values",
        ));
    }
    Ok(ArtifactStartupCaseOutcome::Rejected(category))
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_adversarial_spawned_stdio_scenarios() {
    let cleanup: [Arc<Mutex<ChildCleanupRecord>>; ADVERSARIAL_STDIO_CONTROLS.len()] =
        std::array::from_fn(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)));
    let callback_cleanup = cleanup.clone();
    let owner_evidence = Arc::new(Mutex::new(Vec::new()));
    let callback_evidence = Arc::clone(&owner_evidence);
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-adversarial",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                for (index, control) in ADVERSARIAL_STDIO_CONTROLS.into_iter().enumerate() {
                    let record = callback_cleanup
                        .get(index)
                        .ok_or_else(|| sentinel_assertion("adversarial cleanup record missing"))?;
                    let mut execution = run_spawned_artifact_adversarial_default(
                        ctx.as_ref(),
                        Arc::clone(record),
                        control,
                        &callback_audit_needles,
                    )
                    .await
                    .inspect_err(|error| {
                        // Fixed harness category only; the disposable wrapper
                        // withholds callback messages.
                        eprintln!(
                            "spawned adversarial control={control:?} inner failure: {error:?}"
                        );
                    })?;
                    let startup = run_alias07_startup_rejection(
                        ctx.as_ref(),
                        control,
                        &callback_audit_needles,
                    )?;
                    startup
                        .assert_exact(&[AdversarialCaseId::Alias07])
                        .map_err(|_| sentinel_assertion("ALIAS-07 owner inventory diverged"))?;
                    execution
                        .merge(startup)
                        .map_err(|_| sentinel_assertion("merge spawned adversarial evidence"))?;
                    if control == ArtifactControlPlane::SpawnedStableStdio {
                        let sym11 = run_dynamic_symlink_startup_rejection(
                            ctx.as_ref(),
                            ArtifactSymlinkStartupTarget::ImportRoot,
                            "invalid any-mcp artifact root",
                            &callback_audit_needles,
                        )?;
                        let sym12 = run_dynamic_symlink_startup_rejection(
                            ctx.as_ref(),
                            ArtifactSymlinkStartupTarget::StagingRoot,
                            "invalid any-mcp staging policy",
                            &callback_audit_needles,
                        )?;
                        let startup =
                            record_artifact_dynamic_filesystem_startup_cases(sym11, sym12)
                                .map_err(|_| {
                                    sentinel_assertion("record dynamic symlink startup outcomes")
                                })?;
                        startup
                            .assert_exact(&[AdversarialCaseId::Sym11, AdversarialCaseId::Sym12])
                            .map_err(|_| {
                                sentinel_assertion(
                                    "dynamic symlink startup owner inventory diverged",
                                )
                            })?;
                        execution.merge(startup).map_err(|_| {
                            sentinel_assertion("merge dynamic symlink startup evidence")
                        })?;
                    }
                    let mut expected = ADVERSARIAL_STDIO_SENTINEL_IDS.to_vec();
                    if control == ArtifactControlPlane::SpawnedStableStdio {
                        expected.extend(ADVERSARIAL_DYNAMIC_STDIO_IMPLEMENTED_IDS);
                        expected.extend([
                            AdversarialCaseId::Sym11,
                            AdversarialCaseId::Sym12,
                            AdversarialCaseId::Flood07,
                        ]);
                    }
                    expected.push(AdversarialCaseId::Alias07);
                    execution
                        .assert_exact(&expected)
                        .map_err(|_| sentinel_assertion("spawned owner inventory diverged"))?;
                    callback_evidence
                        .lock()
                        .map_err(|_| sentinel_assertion("retain spawned adversarial evidence"))?
                        .push((control, execution));
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned artifact adversarial scenarios");

    match outcome {
        DisposableRun::Completed(()) => {
            for record in &cleanup {
                assert_eq!(
                    *record.lock().expect("adversarial artifact cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            let audit =
                assert_artifact_server_log_clean(&log_baseline, &audit_needles, "adversarial");
            let evidence = owner_evidence
                .lock()
                .expect("spawned adversarial evidence lock");
            assert_eq!(evidence.len(), ADVERSARIAL_STDIO_CONTROLS.len());
            for (control, execution) in evidence.iter() {
                execution
                    .emit_owner_evidence(*control, &audit)
                    .expect("bounded spawned adversarial owner evidence");
            }
        }
        DisposableRun::Skipped(_) => {
            panic!("artifact adversarial scenarios require disposable admission");
        }
    }
}

/// Selects the spawned child profile for one policy scenario.
///
/// Read-only is a server mode rather than a policy field, so it is selected
/// through the production read-only switch while the strict TOML policy stays
/// writable.
#[cfg(feature = "acceptance-harness")]
fn artifact_policy_driver_options(control: ArtifactControlPlane, read_only: bool) -> DriverOptions {
    match (
        control == ArtifactControlPlane::SpawnedPreviewStdio,
        read_only,
    ) {
        (true, true) => DriverOptions::PREVIEW_READ_ONLY,
        (true, false) => DriverOptions::PREVIEW_STANDARD,
        (false, true) => DriverOptions::READ_ONLY,
        (false, false) => DriverOptions::STANDARD,
    }
}

/// Runs one spawned control plane through one artifact policy scenario.
///
/// The child is stopped before the next scenario starts, so at most one policy
/// server per control plane is alive at a time and the fixture tree is removed
/// with the disposable context.
#[cfg(feature = "acceptance-harness")]
async fn run_spawned_artifact_policy_scenario(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    scenario: ArtifactPolicyScenario,
    control: ArtifactControlPlane,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<ArtifactPolicyEvidence> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create_with(&ctx.space_id, scenario.policy_options())
            .map_err(|_| sentinel_assertion("create artifact policy fixture"))?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)?;
    let options = artifact_policy_driver_options(control, scenario.is_read_only());
    let child =
        spawn_disposable_artifact_driver(ctx, cleanup_record, Arc::clone(&policy), options)?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("registered artifact policy child disappeared"))?
        .initialize();

    let run = ArtifactPolicyRun {
        scenario,
        control,
        policy: policy.as_ref(),
        ctx,
    };
    let observed = if control == ArtifactControlPlane::ScriptedProtocol {
        let mut driver = ScriptedArtifactDriver {
            driver: Arc::clone(&child),
        };
        Box::pin(run_artifact_policy_scenario(&mut driver, &run)).await
    } else {
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        Box::pin(run_artifact_policy_scenario(&mut driver, &run)).await
    };

    if scenario == ArtifactPolicyScenario::ReadOnly {
        run_spawned_read_only_cleanup_cases(&child, &policy)
            .and_then(|execution| {
                execution.assert_exact(&[AdversarialCaseId::Clean07, AdversarialCaseId::Clean08])
            })
            .map_err(|error| {
                eprintln!("read-only cleanup fixed-category error: {error}");
                sentinel_assertion("spawned read-only cleanup cases failed")
            })?;
    }

    // Stop this scenario's child before reporting, so a failure never leaves a
    // production process holding the fixture policy.
    let stopped = lock_driver(&child)
        .take()
        .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
    let evidence = observed.map_err(|_| {
        eprintln!(
            "artifact policy scenario={} control={} outcome=failed",
            scenario.as_str(),
            control.as_str()
        );
        sentinel_assertion("spawned artifact policy scenario failed")
    })?;
    if stopped.is_err() {
        return Err(sentinel_assertion(
            "spawned artifact policy child did not stop cleanly",
        ));
    }
    Ok(evidence)
}

/// Runs every artifact policy scenario across the spawned control planes.
#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_policy_spawned_scenarios() {
    let cleanup: Vec<Arc<Mutex<ChildCleanupRecord>>> = (0..ArtifactPolicyScenario::ALL.len()
        * SPAWNED_ARTIFACT_CONTROLS.len())
        .map(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)))
        .collect();
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-policy",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                for (scenario_index, scenario) in
                    ArtifactPolicyScenario::ALL.into_iter().enumerate()
                {
                    eprintln!("artifact policy scenario={}", scenario.as_str());
                    let mut evidence = Vec::with_capacity(SPAWNED_ARTIFACT_CONTROLS.len());
                    for (control_index, control) in
                        SPAWNED_ARTIFACT_CONTROLS.into_iter().enumerate()
                    {
                        let record = callback_cleanup
                            .get(scenario_index * SPAWNED_ARTIFACT_CONTROLS.len() + control_index)
                            .ok_or_else(|| {
                                sentinel_assertion("artifact policy cleanup record missing")
                            })?;
                        evidence.push(
                            Box::pin(run_spawned_artifact_policy_scenario(
                                ctx.as_ref(),
                                Arc::clone(record),
                                scenario,
                                control,
                                &callback_audit_needles,
                            ))
                            .await?,
                        );
                    }
                    assert_artifact_policy_parity(&evidence, &SPAWNED_ARTIFACT_CONTROLS).map_err(
                        |_| {
                            eprintln!(
                                "artifact policy scenario={} parity_outcome=diverged",
                                scenario.as_str()
                            );
                            sentinel_assertion("spawned artifact policy planes diverged")
                        },
                    )?;
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned artifact policy scenarios");

    match outcome {
        DisposableRun::Completed(()) => {
            for record in &cleanup {
                assert_eq!(
                    *record.lock().expect("artifact policy cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            assert_artifact_server_log_clean(&log_baseline, &audit_needles, "policy");
        }
        DisposableRun::Skipped(_) => {
            panic!("artifact policy scenarios require disposable admission");
        }
    }
}

/// Runs one spawned control plane through one artifact content scenario.
///
/// Each scenario owns a private policy fixture and a private production child,
/// and the child is stopped before the next scenario starts, so a failure
/// never leaves a server holding the fixture tree.
#[cfg(feature = "acceptance-harness")]
async fn run_spawned_artifact_content_scenario(
    ctx: &TestContext,
    cleanup_record: Arc<Mutex<ChildCleanupRecord>>,
    scenario: ArtifactContentScenario,
    control: ArtifactControlPlane,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<ArtifactContentEvidence> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create_with(&ctx.space_id, scenario.policy_options()).map_err(
            |_| {
                eprintln!(
                    "artifact content scenario={} fixture_outcome=failed",
                    scenario.as_str()
                );
                sentinel_assertion("create artifact content fixture")
            },
        )?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)?;
    let child = spawn_disposable_artifact_driver(
        ctx,
        cleanup_record,
        Arc::clone(&policy),
        artifact_driver_options(control),
    )?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| sentinel_assertion("registered artifact content child disappeared"))?
        .initialize();

    let run = ArtifactContentRun {
        scenario,
        control,
        policy: policy.as_ref(),
        ctx,
    };
    let observed = if control == ArtifactControlPlane::ScriptedProtocol {
        let mut driver = ScriptedArtifactDriver {
            driver: Arc::clone(&child),
        };
        Box::pin(run_artifact_content_scenario(&mut driver, &run)).await
    } else {
        let mut driver = OwnedStdioDriver {
            driver: Arc::clone(&child),
        };
        Box::pin(run_artifact_content_scenario(&mut driver, &run)).await
    };

    let stopped = lock_driver(&child)
        .take()
        .map_or(Ok(()), |driver| driver.try_finish().map(|_| ()));
    let evidence = observed.map_err(|_| {
        eprintln!(
            "artifact content scenario={} control={} outcome=failed",
            scenario.as_str(),
            control.as_str()
        );
        sentinel_assertion("spawned artifact content scenario failed")
    })?;
    if stopped.is_err() {
        return Err(sentinel_assertion(
            "spawned artifact content child did not stop cleanly",
        ));
    }
    Ok(evidence)
}

/// Runs every artifact content scenario across the spawned control planes.
///
/// The scenarios cover representative MIME import/export on both data planes,
/// Markdown and plain-text create/update/export including explicit no-op and
/// lossy canonicalization evidence, and optional and required validators.
#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_content_spawned_scenarios() {
    let cleanup: Vec<Arc<Mutex<ChildCleanupRecord>>> = (0..ArtifactContentScenario::ALL.len()
        * SPAWNED_ARTIFACT_CONTROLS.len())
        .map(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)))
        .collect();
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-content",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                for (scenario_index, scenario) in
                    ArtifactContentScenario::ALL.into_iter().enumerate()
                {
                    eprintln!("artifact content scenario={}", scenario.as_str());
                    let mut evidence = Vec::with_capacity(SPAWNED_ARTIFACT_CONTROLS.len());
                    for (control_index, control) in
                        SPAWNED_ARTIFACT_CONTROLS.into_iter().enumerate()
                    {
                        let record = callback_cleanup
                            .get(scenario_index * SPAWNED_ARTIFACT_CONTROLS.len() + control_index)
                            .ok_or_else(|| {
                                sentinel_assertion("artifact content cleanup record missing")
                            })?;
                        evidence.push(
                            Box::pin(run_spawned_artifact_content_scenario(
                                ctx.as_ref(),
                                Arc::clone(record),
                                scenario,
                                control,
                                &callback_audit_needles,
                            ))
                            .await?,
                        );
                    }
                    assert_artifact_content_parity(&evidence, &SPAWNED_ARTIFACT_CONTROLS).map_err(
                        |_| {
                            eprintln!(
                                "artifact content scenario={} parity_outcome=diverged",
                                scenario.as_str()
                            );
                            sentinel_assertion("spawned artifact content planes diverged")
                        },
                    )?;
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe spawned artifact content scenarios");

    match outcome {
        DisposableRun::Completed(()) => {
            for record in &cleanup {
                assert_eq!(
                    *record.lock().expect("artifact content cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            assert_artifact_server_log_clean(&log_baseline, &audit_needles, "content");
        }
        DisposableRun::Skipped(_) => {
            panic!("artifact content scenarios require disposable admission");
        }
    }
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_quota_acceptance(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<AdversarialExecution, String> {
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let catalog = artifact_catalog_snapshot(&mut driver).await?;
    let mut execution = AdversarialExecution::default();
    let first = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        400 * 1024,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, first.handle().as_bytes())?;
    let second = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        400 * 1024,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, second.handle().as_bytes())?;
    let reserved = policy.staging_snapshot()?;
    if reserved.temporary_files != 2 || reserved.unexpected_entries != 0 {
        return Err("quota reservations did not produce the exact staging snapshot".to_owned());
    }
    let maximum_status = lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered quota child disappeared".to_owned())?
        .measured_tool_frame("artifact_status", json!({}))?;
    if maximum_status.frame_bytes > ARTIFACT_FRAME_CEILING_BYTES
        || maximum_status
            .structured_content
            .get("staging_available_entries")
            .and_then(Value::as_u64)
            != Some(0)
        || maximum_status
            .structured_content
            .get("staging_available_bytes")
            .and_then(Value::as_u64)
            != Some(224 * 1024)
    {
        return Err("FLOOD-04 maximum-record status was not a bounded aggregate".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Flood04)?;
    let entry_refusal = driver
        .call_tool_error(
            "artifact_stage_upload",
            json!({
                "space": ctx.space_id,
                "size_bytes": 1,
                "media_type": ARTIFACT_FILE_MEDIA_TYPE
            }),
        )
        .await?;
    if entry_refusal.code() != "bounded_result" {
        return Err("staging entry quota did not return bounded_result".to_owned());
    }

    release_stage_upload(&mut driver, &first).await?;
    let released_again = driver
        .call_tool_error("artifact_release", json!({"handle": first.handle()}))
        .await?;
    if released_again.code() != "not_found" || policy.staging_snapshot()?.temporary_files != 1 {
        return Err("CLEAN-03 did not remove and invalidate the released record".to_owned());
    }
    execution.record_executed(AdversarialCaseId::Clean03)?;
    let third = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        300 * 1024,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, third.handle().as_bytes())?;
    release_stage_upload(&mut driver, &third).await?;
    let byte_refusal = driver
        .call_tool_error(
            "artifact_stage_upload",
            json!({
                "space": ctx.space_id,
                "size_bytes": 700 * 1024,
                "media_type": ARTIFACT_FILE_MEDIA_TYPE
            }),
        )
        .await?;
    if byte_refusal.code() != "bounded_result" {
        return Err("staging byte quota did not return bounded_result".to_owned());
    }
    release_stage_upload(&mut driver, &second).await?;
    if !policy.staging_snapshot()?.is_reaped() {
        return Err("quota scenario did not release its exact staging state".to_owned());
    }
    catalog.compare(&artifact_catalog_snapshot(&mut driver).await?)?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
fn well_formed_unknown_stage_handle() -> String {
    let record = [0x55_u8; 16];
    let secret = [0x77_u8; 32];
    let mut checksum = Sha256::new();
    checksum.update(b"any-mcp/artifact-handle/v1");
    checksum.update(record);
    checksum.update(secret);
    let mut bytes = Vec::with_capacity(57);
    bytes.push(1);
    bytes.extend_from_slice(&record);
    bytes.extend_from_slice(&secret);
    bytes.extend(checksum.finalize().iter().copied().take(8));
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_ttl_acceptance(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<AdversarialExecution, String> {
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let catalog = artifact_catalog_snapshot(&mut driver).await?;
    let quota_before = driver.call_tool("artifact_status", json!({})).await?;
    let mut execution = AdversarialExecution::default();
    let allocation = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        1,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, allocation.handle().as_bytes())?;
    let allocated = policy.staging_snapshot()?;
    if allocated.temporary_files != 1 || allocated.unexpected_entries != 0 {
        return Err("TTL allocation did not produce the exact staging snapshot".to_owned());
    }
    let cleanup_deadline = Duration::from_secs(
        policy
            .options()
            .limits
            .staging_ttl_secs()
            .saturating_add(75),
    );
    let reaped = wait_for_stage_reaped(policy, &allocation, cleanup_deadline).await?;
    if !reaped.is_reaped() {
        return Err("TTL cleanup did not produce the exact reaped snapshot".to_owned());
    }
    let expired = driver
        .call_tool_error("artifact_release", json!({"handle": allocation.handle()}))
        .await?;
    if expired.code() != "not_found"
        || driver.call_tool("artifact_status", json!({})).await? != quota_before
    {
        return Err("CLEAN-04 did not invalidate the expired handle and restore quota".to_owned());
    }
    let unknown_handle = well_formed_unknown_stage_handle();
    record_artifact_stage_log_needle(audit_needles, unknown_handle.as_bytes())?;
    let unknown = driver
        .call_tool_error("artifact_release", json!({"handle": unknown_handle}))
        .await?;
    let uniform = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        1,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, uniform.handle().as_bytes())?;
    // HAND-07 requires a resolvable, policy-admitted space B: a valid-format
    // foreign space ID passes resolution without I/O (this scenario's policy
    // omits `allowed`), so the refusal is the staging space binding itself.
    let wrong_space = driver
        .call_tool_error(
            "file_import",
            json!({
                "space": "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7",
                "source": {"staged_handle": uniform.handle()},
                "name": "hand07.bin",
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("HAND-07-{}", unique_suffix()),
            }),
        )
        .await?;
    if unknown.normalized_result() != expired.normalized_result()
        || wrong_space.normalized_result() != expired.normalized_result()
    {
        eprintln!(
            "HAND-16 divergence: expired={:?} unknown={:?} wrong_space={:?}",
            expired.normalized_result(),
            unknown.normalized_result(),
            wrong_space.normalized_result(),
        );
        return Err("HAND-16 MCP not-found payloads were distinguishable".to_owned());
    }
    let client = reqwest::Client::new();
    let expired_http = client
        .head(allocation.url())
        .bearer_auth(allocation.handle())
        .send()
        .await
        .map_err(|_| "HAND-16 expired HTTP request failed".to_owned())?;
    let wrong_route = uniform
        .url()
        .replace(uniform.record(), "00000000000000000000000000000000");
    let wrong_route_http = client
        .head(wrong_route)
        .bearer_auth(uniform.handle())
        .send()
        .await
        .map_err(|_| "HAND-16 wrong-route HTTP request failed".to_owned())?;
    if expired_http.status() != reqwest::StatusCode::NOT_FOUND
        || wrong_route_http.status() != expired_http.status()
        || wrong_route_http
            .bytes()
            .await
            .map_err(|_| "read HAND-16 wrong-route body".to_owned())?
            != expired_http
                .bytes()
                .await
                .map_err(|_| "read HAND-16 expired body".to_owned())?
    {
        return Err("HAND-16 HTTP not-found payloads were distinguishable".to_owned());
    }
    release_stage_upload(&mut driver, &uniform).await?;
    catalog.compare(&artifact_catalog_snapshot(&mut driver).await?)?;
    execution.record_executed(AdversarialCaseId::Hand03)?;
    execution.record_executed(AdversarialCaseId::Hand16)?;
    execution.record_executed(AdversarialCaseId::Clean04)?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
async fn import_collision_fixture(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
) -> Result<String, String> {
    let suffix = unique_suffix();
    let imported = lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered collision child disappeared".to_owned())?
        .call_tool_sync(
            "file_import",
            json!({
                "space": ctx.space_id,
                "source": {"local": {
                    "root": ArtifactPolicyFixture::IMPORT_ROOT,
                    "path": ArtifactPolicyFixture::FILE_SOURCE
                }},
                "name": format!("artifact-collision-{suffix}.bin"),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("artifact-collision-import-{suffix}")
            }),
        )?;
    let file_id = imported["file_id"]
        .as_str()
        .ok_or_else(|| "collision fixture import omitted file_id".to_owned())?
        .to_owned();
    ctx.register_file(&file_id);
    Ok(file_id)
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_collision_acceptance(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
) -> Result<(), String> {
    let mut catalog_driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let catalog = artifact_catalog_snapshot(&mut catalog_driver).await?;
    let file_id = import_collision_fixture(ctx, child).await?;
    let suffix = unique_suffix();
    let destination = format!("collision-{suffix}.bin");
    let arguments = |key: &str| {
        json!({
            "space": ctx.space_id,
            "file_id": file_id,
            "destination": {"local": {
                "root": ArtifactPolicyFixture::EXPORT_ROOT,
                "path": destination
            }},
            "idempotency_key": key
        })
    };
    let (ids, frames) = lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered collision child disappeared".to_owned())?
        .collision_tool_frames(
            "file_export",
            arguments(&format!("artifact-collision-first-{suffix}")),
            arguments(&format!("artifact-collision-second-{suffix}")),
        );
    let winner = classify_collision_frames("file_export", ids, &frames, false)?;
    if winner.pointer("/receipt/sha256").and_then(Value::as_str)
        != Some(artifact_sha256(ARTIFACT_FILE_PAYLOAD).as_str())
    {
        return Err("collision winner did not publish the exact source bytes".to_owned());
    }
    if policy.read_export(&destination)? != ARTIFACT_FILE_PAYLOAD {
        return Err("collision destination bytes diverged from the winner".to_owned());
    }
    let exported = policy.export_snapshot()?;
    if exported.ordinary_files != 1
        || exported.total_file_bytes != ARTIFACT_FILE_PAYLOAD.len() as u64
        || exported.unexpected_entries != 0
    {
        return Err("collision scenario did not produce one exact destination".to_owned());
    }
    catalog.compare(&artifact_catalog_snapshot(&mut catalog_driver).await?)
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_cancellation_acceptance(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    gate: &ChildArtifactGate,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<u64, String> {
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let catalog = artifact_catalog_snapshot(&mut driver).await?;
    let objects_before = ctx
        .client
        .objects(&ctx.space_id)
        .limit(200)
        .list()
        .await
        .map_err(|_| "capture cancellation object inventory".to_owned())?
        .collect_all()
        .await
        .map_err(|_| "capture cancellation object inventory".to_owned())?
        .into_iter()
        .map(|object| object.id)
        .collect::<std::collections::BTreeSet<_>>();
    let payload = vec![0x5a; 256 * 1024];
    let expected = artifact_sha256(&payload);
    let allocation = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        payload.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&expected),
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, allocation.handle().as_bytes())?;
    upload_stage_bytes(&allocation, &payload, ARTIFACT_FILE_MEDIA_TYPE).await?;
    let suffix = unique_suffix();
    let arguments = json!({
        "space": ctx.space_id,
        "source": {"staged_handle": allocation.handle()},
        "name": format!("artifact-cancel-{suffix}.bin"),
        "media_type": ARTIFACT_FILE_MEDIA_TYPE,
        "idempotency_key": gate.key()
    });
    let cancelled_id = lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered cancellation child disappeared".to_owned())?
        .cancel_tool_call("file_import", arguments.clone(), gate)?;
    let objects_after_cancel = ctx
        .client
        .objects(&ctx.space_id)
        .limit(200)
        .list()
        .await
        .map_err(|_| "capture cancellation object inventory".to_owned())?
        .collect_all()
        .await
        .map_err(|_| "capture cancellation object inventory".to_owned())?
        .into_iter()
        .map(|object| object.id)
        .collect::<std::collections::BTreeSet<_>>();
    if objects_after_cancel != objects_before {
        return Err("cancelled artifact import created an object".to_owned());
    }
    let imported = driver.call_tool("file_import", arguments).await?;
    let file_id = imported["file_id"]
        .as_str()
        .ok_or_else(|| "post-cancellation retry omitted file_id".to_owned())?
        .to_owned();
    ctx.register_file(&file_id);
    if imported.pointer("/receipt/sha256").and_then(Value::as_str) != Some(expected.as_str()) {
        return Err("post-cancellation retry did not preserve exact staged bytes".to_owned());
    }
    release_stage_upload(&mut driver, &allocation).await?;
    if !policy.staging_snapshot()?.is_reaped() {
        return Err("cancelled artifact operation left private staging state".to_owned());
    }
    catalog.compare(&artifact_catalog_snapshot(&mut driver).await?)?;
    Ok(cancelled_id)
}

#[cfg(feature = "acceptance-harness")]
async fn cancellation_object_ids(
    ctx: &TestContext,
) -> Result<std::collections::BTreeSet<String>, String> {
    ctx.client
        .objects(&ctx.space_id)
        .limit(200)
        .list()
        .await
        .map_err(|_| "capture exact cancellation object inventory".to_owned())?
        .collect_all()
        .await
        .map_err(|_| "capture exact cancellation object inventory".to_owned())
        .map(|objects| objects.into_iter().map(|object| object.id).collect())
}

#[cfg(feature = "acceptance-harness")]
async fn seed_cancellation_file(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    label: &str,
) -> Result<String, String> {
    let source = format!("{label}-source.bin");
    policy.seed_import(&source, ARTIFACT_FILE_PAYLOAD)?;
    let imported = OwnedStdioDriver {
        driver: Arc::clone(child),
    }
    .call_tool(
        "file_import",
        json!({
            "space": ctx.space_id,
            "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": source}},
            "name": format!("{label}.bin"),
            "media_type": ARTIFACT_FILE_MEDIA_TYPE,
            "idempotency_key": format!("{label}-seed-{}", unique_suffix()),
        }),
    )
    .await?;
    let file_id = imported
        .get("file_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "cancellation file seed omitted file_id".to_owned())?
        .to_owned();
    ctx.register_file(&file_id);
    Ok(file_id)
}

#[cfg(feature = "acceptance-harness")]
async fn run_file_export_cancellation_case(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    gate: &ChildArtifactGate,
    case: AdversarialCaseId,
) -> Result<(), String> {
    let label = case.as_str().to_ascii_lowercase();
    let file_id = seed_cancellation_file(ctx, child, policy, &label).await?;
    let destination = format!("{label}-destination.bin");
    let before = policy.export_snapshot()?;
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let quota_before = driver.call_tool("artifact_status", json!({})).await?;
    let arguments = json!({
        "space": ctx.space_id,
        "file_id": file_id,
        "destination": {"local": {"root": ArtifactPolicyFixture::EXPORT_ROOT, "path": destination}},
        "idempotency_key": gate.key(),
    });
    lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered export-cancellation child disappeared".to_owned())?
        .cancel_tool_call_exact("file_export", arguments, gate)?;
    let path = policy.export_root().join(&destination);
    let after = policy.export_snapshot()?;
    if driver.call_tool("artifact_status", json!({})).await? != quota_before {
        return Err("cancelled file export changed staging quota".to_owned());
    }
    match case {
        AdversarialCaseId::Part08 => {
            if path.exists() || after != before {
                return Err("PART-08 published or retained a cancelled export".to_owned());
            }
        }
        AdversarialCaseId::Part09 => {
            if path.exists()
                && std::fs::read(&path)
                    .map(|bytes| bytes != ARTIFACT_FILE_PAYLOAD)
                    .unwrap_or(true)
            {
                return Err("PART-09 published a partial or changed destination".to_owned());
            }
            let expected_ordinary = before.ordinary_files + u64::from(path.exists());
            if after.ordinary_files != expected_ordinary
                || after.temporary_files != before.temporary_files
                || after.unexpected_entries != before.unexpected_entries
            {
                return Err("PART-09 left an invalid export-root inventory".to_owned());
            }
        }
        _ => return Err("file-export cancellation received a non-export case".to_owned()),
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
async fn run_file_import_post_dispatch_cancellation_case(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    gate: &ChildArtifactGate,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<(), String> {
    let before = cancellation_object_ids(ctx).await?;
    let expected = artifact_sha256(ARTIFACT_FILE_PAYLOAD);
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let allocation = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        ARTIFACT_FILE_PAYLOAD.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&expected),
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, allocation.handle().as_bytes())?;
    upload_stage_bytes(&allocation, ARTIFACT_FILE_PAYLOAD, ARTIFACT_FILE_MEDIA_TYPE).await?;
    let arguments = json!({
        "space": ctx.space_id,
        "source": {"staged_handle": allocation.handle()},
        "name": "part10.bin",
        "media_type": ARTIFACT_FILE_MEDIA_TYPE,
        "idempotency_key": gate.key(),
    });
    lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered import-cancellation child disappeared".to_owned())?
        .cancel_tool_call_exact("file_import", arguments.clone(), gate)?;
    let after_cancel = cancellation_object_ids(ctx).await?;
    if after_cancel.len() > before.len().saturating_add(1) || !before.is_subset(&after_cancel) {
        return Err("PART-10 cancellation created more than one object".to_owned());
    }
    let imported = driver.call_tool("file_import", arguments).await?;
    let file_id = imported
        .get("file_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "PART-10 replay omitted file_id".to_owned())?;
    ctx.register_file(file_id);
    // The idempotency-ledger proof that no second upload was dispatched is
    // the replay flag: a fresh dispatch would report `reused: false`. The
    // space listing cannot carry this evidence because freshly imported file
    // objects are not returned by the object list.
    if imported.get("reused").and_then(Value::as_bool) != Some(true)
        || imported.pointer("/receipt/sha256").and_then(Value::as_str) != Some(expected.as_str())
    {
        return Err("PART-10 retry dispatched a duplicate or lost its candidate".to_owned());
    }
    let after_retry = cancellation_object_ids(ctx).await?;
    if after_retry.len() > before.len().saturating_add(1) || !before.is_subset(&after_retry) {
        return Err("PART-10 retry changed unrelated space inventory".to_owned());
    }
    release_stage_upload(&mut driver, &allocation).await?;
    if !policy.staging_snapshot()?.is_reaped() {
        return Err("PART-10 left staged private state".to_owned());
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
async fn run_document_post_dispatch_cancellation_case(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    gate: &ChildArtifactGate,
) -> Result<(), String> {
    let object = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(format!("PART-12-{}", unique_suffix()))
        .body("# PART-12 old\n")
        .create()
        .await
        .map_err(|_| "create PART-12 document".to_owned())?;
    ctx.register_object(&object.id);
    let old = ctx
        .client
        .object(&ctx.space_id, &object.id)
        .get()
        .await
        .map_err(|_| "read PART-12 old document".to_owned())?
        .markdown
        .ok_or_else(|| "PART-12 old document omitted markdown".to_owned())?;
    let old_sha256 = artifact_sha256(old.as_bytes());
    let source = format!("part12-{}.md", unique_suffix());
    policy.seed_import(&source, b"# PART-12 new\n")?;
    let arguments = json!({
        "space": ctx.space_id,
        "object_id": object.id,
        "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": source}},
        "source_format": "markdown",
        "expected_body_sha256": old_sha256,
        "idempotency_key": gate.key(),
    });
    lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered document-cancellation child disappeared".to_owned())?
        .cancel_tool_call_exact("document_import_update", arguments.clone(), gate)?;
    let after_cancel = ctx
        .client
        .object(&ctx.space_id, &object.id)
        .get()
        .await
        .map_err(|_| "read PART-12 cancelled document".to_owned())?
        .markdown
        .ok_or_else(|| "PART-12 cancelled document omitted markdown".to_owned())?;
    if after_cancel.contains("PART-12 old") == after_cancel.contains("PART-12 new") {
        return Err("PART-12 produced a spliced or unrecognized body".to_owned());
    }
    // The pause point is after the body dispatch, so the update is applied
    // deterministically. A replay with the original expected hash must then
    // receive the definitive conflict from the body precondition - never a
    // second dispatch and never a spliced body.
    if !after_cancel.contains("PART-12 new") {
        return Err("PART-12 post-dispatch cancellation lost the applied update".to_owned());
    }
    let replay = OwnedStdioDriver {
        driver: Arc::clone(child),
    }
    .call_tool_error("document_import_update", arguments)
    .await?;
    if replay.code() != "conflict" {
        return Err("PART-12 replay did not classify the applied update".to_owned());
    }
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
async fn run_exact_cancellation_cases(
    ctx: &TestContext,
    cleanup: [Arc<Mutex<ChildCleanupRecord>>; 4],
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<AdversarialExecution> {
    let specs = [
        (AdversarialCaseId::Part08, "export-prepublication"),
        (AdversarialCaseId::Part09, "export-atomic-publication"),
        (AdversarialCaseId::Part10, "import-post-dispatch"),
        (AdversarialCaseId::Part12, "document-post-dispatch"),
    ];
    let mut execution = AdversarialExecution::default();
    for ((case, point), cleanup_record) in specs.into_iter().zip(cleanup) {
        let policy = Arc::new(
            ArtifactPolicyFixture::create_with(
                &ctx.space_id,
                ArtifactPolicyOptions {
                    limits: support::live_scenario::ArtifactLimitProfile::PayloadCeiling,
                    ..ArtifactPolicyOptions::default()
                },
            )
            .map_err(|_| sentinel_assertion("create exact cancellation fixture"))?,
        );
        record_artifact_fixture_log_needle(&policy, audit_needles)?;
        let key = format!("{}-{}", case.as_str(), unique_suffix());
        let (child, gate) = spawn_disposable_gated_artifact_driver(
            ctx,
            cleanup_record,
            Arc::clone(&policy),
            DriverOptions::STANDARD,
            point,
            key,
        )?;
        lock_driver(&child)
            .as_mut()
            .ok_or_else(|| sentinel_assertion("exact cancellation child disappeared"))?
            .initialize();
        let result = match case {
            AdversarialCaseId::Part08 | AdversarialCaseId::Part09 => {
                run_file_export_cancellation_case(ctx, &child, &policy, &gate, case).await
            }
            AdversarialCaseId::Part10 => {
                run_file_import_post_dispatch_cancellation_case(
                    ctx,
                    &child,
                    &policy,
                    &gate,
                    audit_needles,
                )
                .await
            }
            AdversarialCaseId::Part12 => {
                run_document_post_dispatch_cancellation_case(ctx, &child, &policy, &gate).await
            }
            _ => Err("exact cancellation inventory contained an unrelated case".to_owned()),
        };
        result.map_err(|error| {
            eprintln!(
                "exact cancellation case={} fixed-category error: {error}",
                case.as_str()
            );
            sentinel_assertion("exact cancellation case failed")
        })?;
        finish_registered_artifact_child(&child, None)
            .map_err(|_| sentinel_assertion("stop exact cancellation child"))?;
        execution
            .record_executed(case)
            .map_err(|_| sentinel_assertion("record exact cancellation case"))?;
    }
    execution.record_quota_not_applicable();
    Ok(execution)
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_exact_cancellation_spawned_scenarios() {
    let cleanup = std::array::from_fn(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)));
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-exact-cancellation",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                let execution = run_exact_cancellation_cases(
                    ctx.as_ref(),
                    callback_cleanup,
                    &callback_audit_needles,
                )
                .await?;
                execution
                    .assert_exact(&[
                        AdversarialCaseId::Part08,
                        AdversarialCaseId::Part09,
                        AdversarialCaseId::Part10,
                        AdversarialCaseId::Part12,
                    ])
                    .map_err(|_| sentinel_assertion("exact cancellation inventory diverged"))
            })
        },
    ))
    .await
    .expect("cleanup-safe exact cancellation acceptance");
    require_completed(outcome, "exact cancellation acceptance")
        .expect("prefix-authorized disposable admission");
    for record in &cleanup {
        assert_eq!(
            *record.lock().expect("exact cancellation cleanup record"),
            ChildCleanupRecord::Stopped
        );
    }
    assert_artifact_server_log_clean(&log_baseline, &audit_needles, "exact-cancellation");
}

/// Kills production children mid-operation and proves post-crash recovery:
/// HAND-04 and CRASH-01/02/03/05/07 from the failure-robustness matrix.
#[cfg(feature = "acceptance-harness")]
async fn run_artifact_crash_restart_cases(
    ctx: &TestContext,
    cleanup: [Arc<Mutex<ChildCleanupRecord>>; 6],
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> TestResult<AdversarialExecution> {
    let [
        first,
        restarted,
        import_gated,
        import_restarted,
        export_gated,
        export_restarted,
    ] = cleanup;
    let mut execution = AdversarialExecution::default();
    run_crash_generation_cases(ctx, first, restarted, audit_needles, &mut execution)
        .await
        .map_err(|_| sentinel_assertion("crash generation cases failed"))?;
    run_crash_import_dispatch_case(
        ctx,
        import_gated,
        import_restarted,
        audit_needles,
        &mut execution,
    )
    .await
    .map_err(|_| sentinel_assertion("crash import-dispatch case failed"))?;
    run_crash_export_commit_case(
        ctx,
        export_gated,
        export_restarted,
        audit_needles,
        &mut execution,
    )
    .await
    .map_err(|_| sentinel_assertion("crash export-commit case failed"))?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

/// CRASH-01 (kill mid-upload, restart, reuse every pre-kill handle), HAND-04
/// (previous-generation handle payload uniformity), CRASH-05 (second process
/// on the same staging root), and CRASH-07 (full happy-path import after
/// recovery) against one shared policy fixture.
#[cfg(feature = "acceptance-harness")]
async fn run_crash_generation_cases(
    ctx: &TestContext,
    first_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    restarted_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create(&ctx.space_id)
            .map_err(|_| "create crash generation fixture".to_owned())?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)
        .map_err(|_| "record crash fixture needle".to_owned())?;
    let first_child = spawn_disposable_artifact_driver(
        ctx,
        first_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
    )
    .map_err(|_| "spawn crash generation child".to_owned())?;
    lock_driver(&first_child)
        .as_mut()
        .ok_or_else(|| "crash generation child disappeared".to_owned())?
        .initialize();
    let mut first_driver = OwnedStdioDriver {
        driver: Arc::clone(&first_child),
    };

    // CRASH-01 setup: one record paused mid-upload plus one untouched
    // reservation, so the kill invalidates handles in different states.
    let payload = vec![0x6d; 2 * ACCEPTANCE_TRANSFER_CHUNK_BYTES];
    let mid_upload = allocate_stage_upload(
        &mut first_driver,
        &ctx.space_id,
        payload.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, mid_upload.handle().as_bytes())
        .map_err(|_| "record crash mid-upload needle".to_owned())?;
    let first_chunk = &payload[..ACCEPTANCE_TRANSFER_CHUNK_BYTES];
    let partial = reqwest::Client::new()
        .put(mid_upload.url())
        .bearer_auth(mid_upload.handle())
        .header("content-type", ARTIFACT_FILE_MEDIA_TYPE)
        .header(
            "content-range",
            format!(
                "bytes 0-{}/{}",
                ACCEPTANCE_TRANSFER_CHUNK_BYTES - 1,
                payload.len()
            ),
        )
        .body(first_chunk.to_vec())
        .send()
        .await
        .map_err(|_| "send crash mid-upload chunk".to_owned())?;
    if partial.status() != reqwest::StatusCode::NO_CONTENT {
        return Err("crash mid-upload chunk was not committed".to_owned());
    }
    let untouched = allocate_stage_upload(
        &mut first_driver,
        &ctx.space_id,
        1,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, untouched.handle().as_bytes())
        .map_err(|_| "record crash untouched needle".to_owned())?;

    // CRASH-05: a second production process on the same private staging root
    // must be rejected at startup while the first keeps serving. The durable
    // layout reports the fixed `invalid staging policy` category.
    run_staging_startup_rejection(ctx, &policy, audit_needles, "invalid staging policy").await?;
    if stage_head_status(&mid_upload).await? != reqwest::StatusCode::OK {
        return Err("first owner stopped serving after the rejected second owner".to_owned());
    }
    execution
        .record_executed(AdversarialCaseId::Crash05)
        .map_err(|_| "record CRASH-05".to_owned())?;

    // CRASH-01: kill without cleanup and restart on the same staging root.
    let _terminated = terminate_registered_artifact_child(&first_child)?;

    // CRASH-04: with the killed generation's durable records still on disk,
    // corrupt one persisted record and require the exact reconciliation
    // startup rejection, with no ambiguous file deleted. Restoring the
    // original bytes afterwards lets the CRASH-01 restart proceed.
    let record_paths = policy
        .staged_record_paths()
        .map_err(|_| "inventory durable records for CRASH-04".to_owned())?;
    let corrupted_path = record_paths
        .first()
        .ok_or_else(|| "CRASH-04 requires one persisted durable record".to_owned())?;
    let original_bytes =
        std::fs::read(corrupted_path).map_err(|_| "capture CRASH-04 record bytes".to_owned())?;
    let corrupt_bytes = b"{\"format_version\":1,\"crash04\":true}".to_vec();
    std::fs::write(corrupted_path, &corrupt_bytes)
        .map_err(|_| "corrupt CRASH-04 record".to_owned())?;
    let before_rejection = policy
        .staging_snapshot()
        .map_err(|_| "snapshot CRASH-04 staging state".to_owned())?;
    run_staging_startup_rejection(
        ctx,
        &policy,
        audit_needles,
        "artifact state reconciliation failed",
    )
    .await
    .map_err(|_| "CRASH-04 startup rejection diverged".to_owned())?;
    let retained_bytes =
        std::fs::read(corrupted_path).map_err(|_| "reread CRASH-04 record".to_owned())?;
    if retained_bytes != corrupt_bytes {
        return Err("CRASH-04 rejection modified the corrupt record".to_owned());
    }
    let after_rejection = policy
        .staging_snapshot()
        .map_err(|_| "resnapshot CRASH-04 staging state".to_owned())?;
    if after_rejection != before_rejection {
        return Err("CRASH-04 rejection deleted or altered staging entries".to_owned());
    }
    std::fs::write(corrupted_path, &original_bytes)
        .map_err(|_| "restore CRASH-04 record".to_owned())?;
    execution
        .record_executed(AdversarialCaseId::Crash04)
        .map_err(|_| "record CRASH-04".to_owned())?;

    let second_child = spawn_disposable_artifact_driver(
        ctx,
        restarted_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
    )
    .map_err(|_| "spawn restarted crash child".to_owned())?;
    lock_driver(&second_child)
        .as_mut()
        .ok_or_else(|| "restarted crash child disappeared".to_owned())?
        .initialize();
    let mut second_driver = OwnedStdioDriver {
        driver: Arc::clone(&second_child),
    };
    let unknown = second_driver
        .call_tool_error(
            "artifact_release",
            json!({"handle": format!("hand04-{}", unique_suffix())}),
        )
        .await?;
    if unknown.code() != "not_found" {
        return Err("fresh unknown handle did not return not_found".to_owned());
    }
    for stale in [&mid_upload, &untouched] {
        if stage_head_status(stale).await? != reqwest::StatusCode::NOT_FOUND {
            return Err("pre-kill staging handle survived the restart".to_owned());
        }
        let released = second_driver
            .call_tool_error("artifact_release", json!({"handle": stale.handle()}))
            .await?;
        // HAND-04: a previous-generation handle is byte-uniform with an
        // unknown handle, so restart leaks no generation oracle.
        if released.code() != "not_found"
            || released.normalized_result() != unknown.normalized_result()
        {
            return Err("previous-generation handle was distinguishable".to_owned());
        }
    }
    if !policy
        .staging_snapshot()
        .map_err(|_| "inspect crash staging root".to_owned())?
        .is_reaped()
    {
        return Err("restart did not reap the killed generation's staging state".to_owned());
    }
    execution
        .record_executed(AdversarialCaseId::Crash01)
        .map_err(|_| "record CRASH-01".to_owned())?;
    execution
        .record_executed(AdversarialCaseId::Hand04)
        .map_err(|_| "record HAND-04".to_owned())?;

    // CRASH-07: recovery is complete, not degraded - a full happy-path
    // import through the restarted child succeeds.
    let source = format!("crash07-{}.bin", unique_suffix());
    policy
        .seed_import(&source, ARTIFACT_FILE_PAYLOAD)
        .map_err(|_| "seed CRASH-07 import".to_owned())?;
    let imported = second_driver
        .call_tool(
            "file_import",
            json!({
                "space": ctx.space_id,
                "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": source}},
                "name": format!("crash07-{}.bin", unique_suffix()),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("crash07-{}", unique_suffix()),
            }),
        )
        .await?;
    let file_id = imported
        .get("file_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "CRASH-07 import omitted file_id".to_owned())?;
    ctx.register_file(file_id);
    execution
        .record_executed(AdversarialCaseId::Crash07)
        .map_err(|_| "record CRASH-07".to_owned())?;
    finish_registered_artifact_child(&second_child, None)?;
    Ok(())
}

/// CRASH-04/CRASH-05 helper: spawns a production child against a staging
/// root that must be refused, and requires the exact bounded startup
/// rejection with the supplied fixed reason.
#[cfg(feature = "acceptance-harness")]
async fn run_staging_startup_rejection(
    ctx: &TestContext,
    policy: &Arc<ArtifactPolicyFixture>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    expected_reason: &'static str,
) -> Result<(), String> {
    let fixture_needle = artifact_fixture_log_needle(policy)
        .map_err(|_| "derive crash second-owner needle".to_owned())?;
    record_artifact_log_needle(audit_needles, &fixture_needle)
        .map_err(|_| "record crash second-owner needle".to_owned())?;
    let credential_needles = disposable_child_credential_needles(ctx)
        .map_err(|_| "derive crash second-owner credentials".to_owned())?;
    let environment = ctx
        .disposable_child_environment()
        .ok_or_else(|| "crash second owner omitted child environment".to_owned())?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
    environment
        .configure(&mut command)
        .map_err(|_| "configure crash second owner".to_owned())?;
    configure_stdio_command(&mut command, DriverOptions::STANDARD, Some("artifacts"));
    command.env("ANY_MCP_CONFIG", policy.config_path());
    let mut process = ProtocolProcess::spawn_with_deadline(command, Duration::from_secs(10));
    let panic = std::panic::catch_unwind(AssertUnwindSafe(|| process.read_frame()))
        .err()
        .ok_or_else(|| "second staging owner served a frame".to_owned())?;
    let panic_text = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("non-string panic");
    if panic_text != "bounded protocol process failed: child_eof" {
        return Err("second staging owner did not fail with bounded EOF".to_owned());
    }
    let failure = process
        .take_failure()
        .ok_or_else(|| "second staging owner omitted process evidence".to_owned())?;
    if failure.category != "child_eof"
        || failure.output.exit_category != "exit_code"
        || !failure.output.stdout.is_empty()
    {
        return Err("second staging owner violated the startup output contract".to_owned());
    }
    let stderr = std::str::from_utf8(&failure.output.stderr)
        .map_err(|_| "second staging owner stderr was not UTF-8".to_owned())?;
    let lines = stderr
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let expected_suffix = format!("any-mcp startup or service failure reason={expected_reason}");
    let category = match lines.as_slice() {
        [line]
            if line
                .strip_suffix(expected_suffix.as_str())
                .is_some_and(|prefix| {
                    prefix
                        .split_ascii_whitespace()
                        .any(|field| field == "ERROR")
                }) =>
        {
            expected_reason
        }
        _ => "unexpected startup category",
    };
    ExpectedOutcome::StartupRejected {
        category: expected_reason,
    }
    .assert_matches(ObservedOutcome::StartupRejected { category })
    .map_err(|_| "second staging owner startup category diverged".to_owned())?;
    if contains_bytes(&failure.output.stderr, &fixture_needle)
        || credential_needles
            .iter()
            .any(|needle| contains_bytes(&failure.output.stderr, needle))
    {
        return Err("second staging owner diagnostics exposed private state".to_owned());
    }
    Ok(())
}

/// CRASH-02: kill during the Anytype import dispatch, restart, and prove the
/// space holds at most one candidate object.
#[cfg(feature = "acceptance-harness")]
async fn run_crash_import_dispatch_case(
    ctx: &TestContext,
    gated_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    restarted_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create(&ctx.space_id)
            .map_err(|_| "create crash import fixture".to_owned())?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)
        .map_err(|_| "record crash import needle".to_owned())?;
    let before = cancellation_object_ids(ctx).await?;
    let key = format!("crash02-{}", unique_suffix());
    let (child, gate) = spawn_disposable_gated_artifact_driver(
        ctx,
        gated_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
        "import-post-dispatch",
        key.clone(),
    )
    .map_err(|_| "spawn crash import child".to_owned())?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| "crash import child disappeared".to_owned())?
        .initialize();
    let source = format!("crash02-{}.bin", unique_suffix());
    policy
        .seed_import(&source, ARTIFACT_FILE_PAYLOAD)
        .map_err(|_| "seed CRASH-02 import".to_owned())?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| "crash import child disappeared".to_owned())?
        .send_tool_call_only(
            "file_import",
            json!({
                "space": ctx.space_id,
                "source": {"local": {"root": ArtifactPolicyFixture::IMPORT_ROOT, "path": source}},
                "name": format!("crash02-{}.bin", unique_suffix()),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": key,
            }),
        );
    gate.wait_ready()
        .map_err(|_| "CRASH-02 dispatch never reached its gate".to_owned())?;
    let _terminated = terminate_registered_artifact_child(&child)?;
    let restarted = spawn_disposable_artifact_driver(
        ctx,
        restarted_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
    )
    .map_err(|_| "spawn restarted import child".to_owned())?;
    lock_driver(&restarted)
        .as_mut()
        .ok_or_else(|| "restarted import child disappeared".to_owned())?
        .initialize();
    let after = cancellation_object_ids(ctx).await?;
    if !before.is_subset(&after) || after.len() > before.len().saturating_add(1) {
        return Err("CRASH-02 dispatched more than one candidate object".to_owned());
    }
    for file_id in after.difference(&before) {
        ctx.register_file(file_id);
    }
    if !policy
        .staging_snapshot()
        .map_err(|_| "inspect crash import staging root".to_owned())?
        .is_reaped()
    {
        return Err("CRASH-02 restart left private staging state".to_owned());
    }
    execution
        .record_executed(AdversarialCaseId::Crash02)
        .map_err(|_| "record CRASH-02".to_owned())?;
    finish_registered_artifact_child(&restarted, None)?;
    Ok(())
}

/// CRASH-03: kill during the atomic export commit, restart, and prove the
/// destination is absent or complete and hash-correct - never partial.
#[cfg(feature = "acceptance-harness")]
async fn run_crash_export_commit_case(
    ctx: &TestContext,
    gated_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    restarted_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
    execution: &mut AdversarialExecution,
) -> Result<(), String> {
    let policy = Arc::new(
        ArtifactPolicyFixture::create(&ctx.space_id)
            .map_err(|_| "create crash export fixture".to_owned())?,
    );
    record_artifact_fixture_log_needle(&policy, audit_needles)
        .map_err(|_| "record crash export needle".to_owned())?;
    let key = format!("crash03-{}", unique_suffix());
    let (child, gate) = spawn_disposable_gated_artifact_driver(
        ctx,
        gated_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
        "export-atomic-publication",
        key.clone(),
    )
    .map_err(|_| "spawn crash export child".to_owned())?;
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| "crash export child disappeared".to_owned())?
        .initialize();
    let file_id = seed_cancellation_file(ctx, &child, &policy, "crash03").await?;
    let destination = format!("crash03-{}.bin", unique_suffix());
    lock_driver(&child)
        .as_mut()
        .ok_or_else(|| "crash export child disappeared".to_owned())?
        .send_tool_call_only(
            "file_export",
            json!({
                "space": ctx.space_id,
                "file_id": file_id,
                "destination": {
                    "local": {"root": ArtifactPolicyFixture::EXPORT_ROOT, "path": destination}
                },
                "idempotency_key": key,
            }),
        );
    gate.wait_ready()
        .map_err(|_| "CRASH-03 commit never reached its gate".to_owned())?;
    let _terminated = terminate_registered_artifact_child(&child)?;
    let restarted = spawn_disposable_artifact_driver(
        ctx,
        restarted_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
    )
    .map_err(|_| "spawn restarted export child".to_owned())?;
    lock_driver(&restarted)
        .as_mut()
        .ok_or_else(|| "restarted export child disappeared".to_owned())?
        .initialize();
    let path = policy.export_root().join(&destination);
    if path.exists() {
        let published = std::fs::read(&path).map_err(|_| "read CRASH-03 destination".to_owned())?;
        if published != ARTIFACT_FILE_PAYLOAD {
            return Err("CRASH-03 destination was partial or changed".to_owned());
        }
    }
    if !policy
        .staging_snapshot()
        .map_err(|_| "inspect crash export staging root".to_owned())?
        .is_reaped()
    {
        return Err("CRASH-03 restart left private staging state".to_owned());
    }
    execution
        .record_executed(AdversarialCaseId::Crash03)
        .map_err(|_| "record CRASH-03".to_owned())?;
    finish_registered_artifact_child(&restarted, None)?;
    Ok(())
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_crash_restart_scenarios() {
    let cleanup = std::array::from_fn(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)));
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-crash-restart",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                let execution = run_artifact_crash_restart_cases(
                    ctx.as_ref(),
                    callback_cleanup,
                    &callback_audit_needles,
                )
                .await?;
                execution
                    .assert_exact(&[
                        AdversarialCaseId::Hand04,
                        AdversarialCaseId::Crash01,
                        AdversarialCaseId::Crash02,
                        AdversarialCaseId::Crash03,
                        AdversarialCaseId::Crash04,
                        AdversarialCaseId::Crash05,
                        AdversarialCaseId::Crash07,
                    ])
                    .map_err(|_| sentinel_assertion("crash-restart inventory diverged"))
            })
        },
    ))
    .await
    .expect("cleanup-safe crash-restart acceptance");
    require_completed(outcome, "crash-restart acceptance")
        .expect("prefix-authorized disposable admission");
    for record in &cleanup {
        assert_eq!(
            *record.lock().expect("crash-restart cleanup record"),
            ChildCleanupRecord::Stopped
        );
    }
    assert_artifact_server_log_clean(&log_baseline, &audit_needles, "crash-restart");
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_restart_acceptance(
    ctx: &TestContext,
    first_child: &Arc<Mutex<Option<StdioDriver>>>,
    first_cleanup: &Arc<Mutex<ChildCleanupRecord>>,
    second_cleanup: Arc<Mutex<ChildCleanupRecord>>,
    policy: Arc<ArtifactPolicyFixture>,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<ArtifactChildProcessEvidence, String> {
    let mut first_driver = OwnedStdioDriver {
        driver: Arc::clone(first_child),
    };
    let catalog = artifact_catalog_snapshot(&mut first_driver).await?;
    let payload = vec![0x33; 64 * 1024];
    let expected = artifact_sha256(&payload);
    let stale = allocate_stage_upload(
        &mut first_driver,
        &ctx.space_id,
        payload.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&expected),
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, stale.handle().as_bytes())?;
    upload_stage_bytes(&stale, &payload, ARTIFACT_FILE_MEDIA_TYPE).await?;
    if policy.staging_snapshot()?.record_files != 1 {
        return Err("restart fixture did not retain one complete staged record".to_owned());
    }
    let _terminated = terminate_registered_artifact_child(first_child)?;
    if *first_cleanup
        .lock()
        .map_err(|_| "restart cleanup lock poisoned".to_owned())?
        != ChildCleanupRecord::NotRun
    {
        return Err("restart child cleanup ran before disposable teardown".to_owned());
    }

    let second_child = spawn_disposable_artifact_driver(
        ctx,
        second_cleanup,
        Arc::clone(&policy),
        DriverOptions::STANDARD,
    )
    .map_err(|_| "spawn restarted artifact child".to_owned())?;
    lock_driver(&second_child)
        .as_mut()
        .ok_or_else(|| "restarted artifact child disappeared".to_owned())?
        .initialize();
    let mut second_driver = OwnedStdioDriver {
        driver: Arc::clone(&second_child),
    };
    catalog.compare(&artifact_catalog_snapshot(&mut second_driver).await?)?;
    if stage_head_status(&stale).await? != reqwest::StatusCode::NOT_FOUND {
        return Err("pre-restart staging handle remained HTTP-accessible".to_owned());
    }
    let stale_release = second_driver
        .call_tool_error("artifact_release", json!({"handle": stale.handle()}))
        .await?;
    if stale_release.code() != "not_found" {
        return Err("pre-restart handle did not return fixed stale not_found".to_owned());
    }
    if !policy.staging_snapshot()?.is_reaped() {
        return Err("restart reconciliation did not reap private staging state".to_owned());
    }
    let fresh = allocate_stage_upload(
        &mut second_driver,
        &ctx.space_id,
        1,
        ARTIFACT_FILE_MEDIA_TYPE,
        None,
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, fresh.handle().as_bytes())?;
    release_stage_upload(&mut second_driver, &fresh).await?;
    let evidence = finish_registered_artifact_child(&second_child, None)?;
    if evidence.reconciliation_events == 0 || evidence.reconciled_records == 0 {
        return Err("restarted child omitted bounded reconciliation log evidence".to_owned());
    }
    Ok(evidence)
}

#[cfg(feature = "acceptance-harness")]
async fn measured_payload_import(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    payload: &[u8],
    label: &str,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<(ArtifactFrameMeasurement, ArtifactStageAllocation), String> {
    let expected = artifact_sha256(payload);
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let allocation = allocate_stage_upload(
        &mut driver,
        &ctx.space_id,
        payload.len() as u64,
        ARTIFACT_FILE_MEDIA_TYPE,
        Some(&expected),
    )
    .await?;
    record_artifact_stage_log_needle(audit_needles, allocation.handle().as_bytes())?;
    if payload.len() > ACCEPTANCE_TRANSFER_CHUNK_BYTES {
        reject_oversized_stage_chunk(&allocation, payload, ARTIFACT_FILE_MEDIA_TYPE).await?;
    }
    upload_stage_bytes(&allocation, payload, ARTIFACT_FILE_MEDIA_TYPE).await?;
    let suffix = unique_suffix();
    let measured = lock_driver(child)
        .as_mut()
        .ok_or_else(|| "registered payload child disappeared".to_owned())?
        .measured_tool_frame(
            "file_import",
            json!({
                "space": ctx.space_id,
                "source": {"staged_handle": allocation.handle()},
                "name": format!("artifact-payload-{label}-{suffix}.bin"),
                "media_type": ARTIFACT_FILE_MEDIA_TYPE,
                "idempotency_key": format!("artifact-payload-{label}-{suffix}")
            }),
        )?;
    let file_id = measured
        .structured_content
        .get("file_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "measured payload import omitted file_id".to_owned())?
        .to_owned();
    ctx.register_file(&file_id);
    if measured
        .structured_content
        .pointer("/receipt/sha256")
        .and_then(Value::as_str)
        != Some(expected.as_str())
        || measured
            .structured_content
            .pointer("/receipt/size_bytes")
            .and_then(Value::as_u64)
            != Some(payload.len() as u64)
    {
        return Err("measured payload import did not verify exact bytes".to_owned());
    }
    Ok((measured, allocation))
}

#[cfg(feature = "acceptance-harness")]
async fn run_artifact_payload_acceptance(
    ctx: &TestContext,
    child: &Arc<Mutex<Option<StdioDriver>>>,
    policy: &ArtifactPolicyFixture,
    audit_needles: &Arc<Mutex<Vec<Vec<u8>>>>,
) -> Result<AdversarialExecution, String> {
    let mut driver = OwnedStdioDriver {
        driver: Arc::clone(child),
    };
    let catalog = artifact_catalog_snapshot(&mut driver).await?;
    let large_markdown = (0..65)
        .map(|index| format!("## FLOOD-05-{index}\n\n{}\n\n", "x".repeat(16 * 1024)))
        .collect::<String>();
    let large_document = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(format!("FLOOD-05-{}", unique_suffix()))
        .body(&large_markdown)
        .create()
        .await
        .map_err(|_| "create FLOOD-05 large document".to_owned())?;
    ctx.register_object(&large_document.id);
    let export_before = policy.export_snapshot()?;
    let flood = driver
        .call_tool_error(
            "document_export",
            json!({
                "space": ctx.space_id,
                "object_id": large_document.id,
                "destination": {"local": {"root": ArtifactPolicyFixture::EXPORT_ROOT, "path": "flood05.md"}},
                "idempotency_key": format!("FLOOD-05-{}", unique_suffix()),
            }),
        )
        .await?;
    if flood.code() != "bounded_result"
        || serde_json::to_vec(flood.normalized_result())
            .map_err(|_| "serialize FLOOD-05 refusal".to_owned())?
            .len()
            > ARTIFACT_FRAME_CEILING_BYTES as usize
        || policy.export_snapshot()? != export_before
    {
        return Err("FLOOD-05 did not return a bounded refusal without publication".to_owned());
    }
    let (small, small_stage) =
        measured_payload_import(ctx, child, b"payload-small", "small", audit_needles).await?;
    let large_payload = (0..1024 * 1024)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let (large, large_stage) =
        measured_payload_import(ctx, child, &large_payload, "ceiling", audit_needles).await?;
    assert_payload_frame_independence(&small, &large)?;
    let over_limit = driver
        .call_tool_error(
            "artifact_stage_upload",
            json!({
                "space": ctx.space_id,
                "size_bytes": 1024 * 1024 + 1,
                "media_type": ARTIFACT_FILE_MEDIA_TYPE
            }),
        )
        .await?;
    if over_limit.code() != "bounded_result" {
        return Err("over-ceiling payload did not return bounded_result".to_owned());
    }
    release_stage_upload(&mut driver, &small_stage).await?;
    release_stage_upload(&mut driver, &large_stage).await?;
    if !policy.staging_snapshot()?.is_reaped() {
        return Err("payload scenario left private staging state".to_owned());
    }
    // FLOOD-07 runs on this owner (its registered lifecycle witness): a
    // failing-call burst must stay byte-uniform, and the caller's child
    // evidence plus post-run log audit bound and redact the diagnostics.
    run_artifact_diagnostic_flood_burst(&mut driver).await?;
    catalog.compare(&artifact_catalog_snapshot(&mut driver).await?)?;
    let mut execution = AdversarialExecution::default();
    execution.record_executed(AdversarialCaseId::Flood05)?;
    execution.record_executed(AdversarialCaseId::Flood07)?;
    execution.record_quota_not_applicable();
    Ok(execution)
}

/// Runs quota, TTL, collision, cancellation, restart, stale-generation, and
/// measured payload-ceiling scenarios against spawned production handlers.
#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_artifact_lifecycle_and_payload_scenarios() {
    let cleanup = (0..ArtifactLifecycleScenario::ALL.len() + 1)
        .map(|_| Arc::new(Mutex::new(ChildCleanupRecord::NotRun)))
        .collect::<Vec<_>>();
    let callback_cleanup = cleanup.clone();
    let log_baseline = artifact_server_log_baseline();
    let audit_needles = Arc::new(Mutex::new(Vec::new()));
    let callback_audit_needles = Arc::clone(&audit_needles);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-artifact-lifecycle",
        move |ctx| {
            Box::pin(async move {
                record_artifact_credential_log_needles(ctx.as_ref(), &callback_audit_needles)?;
                let mut cleanup_index = 0_usize;
                for scenario in ArtifactLifecycleScenario::ALL {
                    eprintln!("artifact lifecycle scenario={}", scenario.as_str());
                    let policy = Arc::new(
                        ArtifactPolicyFixture::create_with(
                            &ctx.space_id,
                            scenario.policy_options(),
                        )
                        .map_err(|_| sentinel_assertion("create artifact lifecycle fixture"))?,
                    );
                    record_artifact_fixture_log_needle(&policy, &callback_audit_needles)?;
                    let record = callback_cleanup
                        .get(cleanup_index)
                        .ok_or_else(|| sentinel_assertion("artifact lifecycle cleanup record missing"))?;
                    cleanup_index = cleanup_index.saturating_add(1);
                    let (child, gate) = if scenario == ArtifactLifecycleScenario::Cancellation {
                        let (child, gate) = spawn_disposable_paused_artifact_driver(
                            ctx.as_ref(),
                            Arc::clone(record),
                            Arc::clone(&policy),
                            DriverOptions::STANDARD,
                        )?;
                        (child, Some(gate))
                    } else {
                        (
                        spawn_disposable_artifact_driver(
                            ctx.as_ref(),
                            Arc::clone(record),
                            Arc::clone(&policy),
                            DriverOptions::STANDARD,
                        )?,
                        None,
                        )
                    };
                    lock_driver(&child)
                        .as_mut()
                        .ok_or_else(|| sentinel_assertion("artifact lifecycle child disappeared"))?
                        .initialize();

                    let result = match scenario {
                        ArtifactLifecycleScenario::Quota => {
                            run_artifact_quota_acceptance(
                                ctx.as_ref(),
                                &child,
                                &policy,
                                &callback_audit_needles,
                            )
                            .await
                            .and_then(|execution| {
                                execution.assert_exact(&[
                                    AdversarialCaseId::Flood04,
                                    AdversarialCaseId::Clean03,
                                ])
                            })
                        }
                        ArtifactLifecycleScenario::TtlCleanup => {
                            run_artifact_ttl_acceptance(
                                ctx.as_ref(),
                                &child,
                                &policy,
                                &callback_audit_needles,
                            )
                            .await
                            .and_then(|execution| {
                                execution.assert_exact(&[
                                    AdversarialCaseId::Hand03,
                                    AdversarialCaseId::Hand16,
                                    AdversarialCaseId::Clean04,
                                ])
                            })
                        }
                        ArtifactLifecycleScenario::Collision => {
                            run_artifact_collision_acceptance(ctx.as_ref(), &child, &policy).await
                        }
                        ArtifactLifecycleScenario::Cancellation => {
                            match run_artifact_cancellation_acceptance(
                                ctx.as_ref(),
                                &child,
                                &policy,
                                gate.as_ref().ok_or_else(|| sentinel_assertion("cancellation child omitted its gate"))?,
                                &callback_audit_needles,
                            )
                            .await
                            {
                                Err(error) => Err(error),
                                Ok(cancelled) => {
                                    match finish_registered_artifact_child(&child, Some(cancelled)) {
                                        Err(error) => Err(error),
                                        Ok(evidence) if evidence.cancelled_operations == 0 => Err(
                                            "cancelled artifact operation omitted fixed log evidence"
                                                .to_owned(),
                                        ),
                                        Ok(_) => Ok(()),
                                    }
                                }
                            }
                        }
                        ArtifactLifecycleScenario::RestartStaleGeneration => {
                            let second = callback_cleanup.get(cleanup_index).ok_or_else(|| {
                                sentinel_assertion("artifact restart cleanup record missing")
                            })?;
                            cleanup_index = cleanup_index.saturating_add(1);
                            run_artifact_restart_acceptance(
                                ctx.as_ref(),
                                &child,
                                record,
                                Arc::clone(second),
                                Arc::clone(&policy),
                                &callback_audit_needles,
                            )
                            .await
                            .map(|_| ())
                        }
                        ArtifactLifecycleScenario::PayloadCeiling => {
                            run_artifact_payload_acceptance(
                                ctx.as_ref(),
                                &child,
                                &policy,
                                &callback_audit_needles,
                            )
                            .await
                            .and_then(|execution| {
                                execution.assert_exact(&[
                                    AdversarialCaseId::Flood05,
                                    AdversarialCaseId::Flood07,
                                ])
                            })
                        }
                    };
                    result.map_err(|error| {
                        // The inner message is a fixed harness category;
                        // surfacing it keeps live failures diagnosable.
                        eprintln!(
                            "artifact lifecycle scenario={} outcome=failed error={error:?}",
                            scenario.as_str()
                        );
                        sentinel_assertion("artifact lifecycle scenario failed")
                    })?;
                    if !matches!(
                        scenario,
                        ArtifactLifecycleScenario::Cancellation
                            | ArtifactLifecycleScenario::RestartStaleGeneration
                    ) {
                        let evidence = finish_registered_artifact_child(&child, None)
                            .map_err(|_| sentinel_assertion("stop artifact lifecycle child"))?;
                        if scenario == ArtifactLifecycleScenario::TtlCleanup
                            && (evidence.cleanup_events == 0 || evidence.cleanup_records == 0)
                        {
                            return Err(sentinel_assertion(
                                "TTL cleanup omitted bounded reap log evidence",
                            ));
                        }
                    }
                    if !policy.tree_exists() {
                        return Err(sentinel_assertion(
                            "artifact fixture tree disappeared before child teardown",
                        ));
                    }
                }
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe artifact lifecycle scenarios");

    match outcome {
        DisposableRun::Completed(()) => {
            for record in &cleanup {
                assert_eq!(
                    *record.lock().expect("artifact lifecycle cleanup record"),
                    ChildCleanupRecord::Stopped
                );
            }
            assert_artifact_server_log_clean(&log_baseline, &audit_needles, "lifecycle");
        }
        DisposableRun::Skipped(_) => {
            panic!("artifact lifecycle scenarios require disposable admission");
        }
    }
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
                        "rich_page_resume",
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
fn body_callback_boundary(stage: DisposableCallbackStage, error: TestError) -> TestError {
    match error {
        error @ TestError::DisposableCallback { .. } => error,
        error => disposable_callback_error(stage, error),
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
        require_body_diagnostics(&output.stderr, b"SECRET_UNPARSED_BODY_VALUE", true)?;
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

        let (direct_evidence, direct_descriptors) = run_direct_body_phase(&ctx)
            .await
            .map_err(|error| body_callback_boundary(DisposableCallbackStage::BodyDirect, error))?;
        let stable = run_spawned_body_phase(
            &ctx,
            stable_cleanup,
            DriverOptions::STANDARD,
            "stable",
            &parity_page.id,
        )
        .await
        .map_err(|error| body_callback_boundary(DisposableCallbackStage::BodyStdioStable, error))?;
        let preview = run_spawned_body_phase(
            &ctx,
            preview_cleanup,
            DriverOptions::PREVIEW_STANDARD,
            "preview",
            &parity_page.id,
        )
        .await
        .map_err(|error| {
            body_callback_boundary(DisposableCallbackStage::BodyStdioPreview, error)
        })?;
        let stable_read_only = run_spawned_read_only_body_phase(
            &ctx,
            stable_read_only_cleanup,
            DriverOptions::READ_ONLY,
            &ctx.space_id,
            &parity_page.id,
        )
        .await
        .map_err(|error| {
            body_callback_boundary(DisposableCallbackStage::BodyReadOnlyStable, error)
        })?;
        let preview_read_only = run_spawned_read_only_body_phase(
            &ctx,
            preview_read_only_cleanup,
            DriverOptions::PREVIEW_READ_ONLY,
            &ctx.space_id,
            &parity_page.id,
        )
        .await
        .map_err(|error| {
            body_callback_boundary(DisposableCallbackStage::BodyReadOnlyPreview, error)
        })?;

        inspect_reviewed_body_server_log(&[
            BODY_DIAGNOSTIC_SECRET.as_bytes(),
            b"SECRET_UNPARSED_BODY_VALUE",
        ])
        .map_err(|error| body_callback_boundary(DisposableCallbackStage::BodyReviewedLog, error))?;
        if stable.descriptors != preview.descriptors || direct_descriptors != stable.descriptors {
            return Err(disposable_callback_error(
                DisposableCallbackStage::BodyParity,
                sentinel_assertion(
                    "direct/stable/preview body descriptors, schemas, or annotations diverged",
                ),
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
            return Err(disposable_callback_error(
                DisposableCallbackStage::BodyParity,
                sentinel_assertion(
                    "stable/preview raw body success or error JSON-RPC frames diverged",
                ),
            ));
        }
        if stable.scenario != preview.scenario {
            return Err(disposable_callback_error(
                DisposableCallbackStage::BodyParity,
                sentinel_assertion("stable and preview normalized body result shapes diverged"),
            ));
        }
        if direct_evidence != stable.scenario {
            return Err(disposable_callback_error(
                DisposableCallbackStage::BodyParity,
                sentinel_assertion("direct and stdio normalized body result shapes diverged"),
            ));
        }
        if stable_read_only != preview_read_only {
            return Err(disposable_callback_error(
                DisposableCallbackStage::BodyParity,
                sentinel_assertion("stable and preview read-only body evidence diverged"),
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
async fn run_body_blocks_real_workflow() -> OptionalRealWorkflowRun {
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
    .unwrap_or_else(|error| {
        panic!(
            "cleanup-safe shared stable/preview body scenario failed: {}; setup={:?}; readiness={:?}; callback={:?}",
            error.category(),
            error.setup_failure(),
            error.readiness_failure(),
            error.callback_failure(),
        )
    });
    match outcome {
        DisposableRun::Completed(space_id) => {
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
            OptionalRealWorkflowRun::Executed
        }
        DisposableRun::Skipped(reason) => {
            eprintln!("body-block workflow skipped before callback: {reason:?}");
            OptionalRealWorkflowRun::Skipped
        }
    }
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_body_blocks_shared_direct_stable_preview_scenarios() {
    let _ = run_body_blocks_real_workflow().await;
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn optional_real_workflow_registration_is_exact() {
    let registered = OPTIONAL_REAL_WORKFLOWS.map(|registration| registration.workflow);
    assert_eq!(registered, OptionalRealWorkflow::ALL);
    assert_eq!(
        registered.map(OptionalRealWorkflow::carrier_registry),
        OptionalRegistry::ALL
    );
    assert_eq!(
        OptionalOperation::ALL
            .into_iter()
            .map(OptionalOperation::fast_workflow)
            .collect::<std::collections::BTreeSet<_>>(),
        OptionalFastWorkflow::ALL.into_iter().collect()
    );
    for owner in OPTIONAL_LIVE_OWNERSHIP
        .iter()
        .filter(|owner| owner.scenario.tier() == OptionalEvidenceTier::RealHeadless)
    {
        let workflow = owner.scenario.workflow();
        assert_eq!(workflow.tier(), OptionalEvidenceTier::RealHeadless);
        let OptionalExecutableWorkflow::RealHeadless(workflow) = workflow else {
            panic!("real ownership resolved to a fast executable workflow");
        };
        assert!(
            registered.contains(&workflow),
            "real operation owner lacks a registered executable workflow"
        );
        assert!(owner.scenario.covers(owner.operation));
        assert_eq!(
            owner.scenario.workflow().carrier_registry(),
            workflow.carrier_registry()
        );
        assert_eq!(owner.scenario.registry(), owner.operation.registry());
    }
    for workflow in OptionalRealWorkflow::ALL {
        assert!(
            OPTIONAL_LIVE_OWNERSHIP.iter().any(|owner| {
                owner.scenario.workflow() == OptionalExecutableWorkflow::RealHeadless(workflow)
                    && owner.scenario.covers(owner.operation)
            }),
            "registered workflow owns no real operation evidence"
        );
    }
}

#[cfg(feature = "acceptance-harness")]
#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials, an authenticated headless Anytype server, and descriptor-bound reviewed server-log context"]
async fn headless_stdio_all_registered_optional_real_workflows() {
    for registration in OPTIONAL_REAL_WORKFLOWS {
        require_optional_workflow_executed(registration.run().await).unwrap_or_else(|message| {
            panic!("{message}: {:?}", registration.workflow);
        });
    }
}

#[cfg(feature = "acceptance-harness")]
#[test]
fn terminal_optional_real_workflow_gate_rejects_missing_disposable_environment() {
    assert_eq!(
        require_optional_workflow_executed(OptionalRealWorkflowRun::Skipped),
        Err("required real-headless workflow was skipped")
    );
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
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

    #[cfg(unix)]
    fn write_evidence_context(path: &Path, marker: &str) -> PathBuf {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let metadata = path.metadata().expect("read reviewed log metadata");
        let start_bytes = metadata.len();
        let anchor_length = start_bytes.min(4096);
        let anchor_start = start_bytes - anchor_length;
        let contents = std::fs::read(path).expect("read reviewed log anchor");
        let anchor = &contents[usize::try_from(anchor_start).expect("small fixture")..];
        let context = temporary_path("reviewed-context");
        std::fs::write(
            &context,
            format!(
                "run_marker={marker}\nstart_device={}\nstart_inode={}\nstart_bytes={start_bytes}\nanchor_start={anchor_start}\nanchor_length={anchor_length}\nanchor_hash={}\n",
                metadata.dev(),
                metadata.ino(),
                file_sha256(anchor),
            ),
        )
        .expect("write reviewed evidence context");
        std::fs::set_permissions(&context, std::fs::Permissions::from_mode(0o600))
            .expect("set private context permissions");
        context
    }

    #[cfg(unix)]
    fn inspect_log(
        path: &Path,
        context: &Path,
        marker: Option<&str>,
        credentials_absent: bool,
    ) -> TestResult<()> {
        inspect_reviewed_body_server_log_at(
            Some(path.as_os_str().to_owned()),
            Some(context.as_os_str().to_owned()),
            marker,
            &[],
            |_| credentials_absent,
        )
    }

    #[test]
    fn body_server_log_inspection_fails_closed_when_path_is_missing() {
        assert!(
            inspect_reviewed_body_server_log_at(None, None, Some(RUN_MARKER), &[], |_| true)
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn body_server_log_requires_private_current_allowlisted_evidence() {
        use std::io::Write;

        let valid = write_reviewed_log("reviewed-valid.log", b"stale pre-start bytes\n");
        let context = write_evidence_context(&valid, RUN_MARKER);
        let crlf_context = std::fs::read_to_string(&context)
            .expect("read LF context")
            .replace('\n', "\r\n");
        assert!(
            parse_reviewed_evidence_context(&crlf_context).is_ok(),
            "context parsing intentionally normalizes CRLF line endings"
        );
        std::fs::OpenOptions::new()
            .append(true)
            .open(&valid)
            .expect("open reviewed log fixture")
            .write_all(format!("{REVIEWED_EVENT}\n").as_bytes())
            .expect("append fresh reviewed event");
        assert!(inspect_log(&valid, &context, Some(RUN_MARKER), true).is_ok());
        assert!(
            inspect_reviewed_body_server_log_at(
                Some(valid.as_os_str().to_owned()),
                Some(context.as_os_str().to_owned()),
                Some(RUN_MARKER),
                &[b"".as_slice()],
                |_| true,
            )
            .is_ok()
        );
        assert!(
            inspect_reviewed_body_server_log_at(
                Some(valid.as_os_str().to_owned()),
                Some(context.as_os_str().to_owned()),
                Some(RUN_MARKER),
                &[b"body_acceptance".as_slice()],
                |_| true,
            )
            .is_err()
        );
        assert!(inspect_log(&valid, &context, None, true).is_err());
        assert!(inspect_log(&valid, &context, Some(&"a".repeat(63)), true).is_err());
        assert!(inspect_log(&valid, &context, Some(RUN_MARKER), false).is_err());

        for (name, contents) in [
            ("reviewed-empty.log", "".to_owned()),
            ("reviewed-arbitrary.log", "arbitrary\n".to_owned()),
            (
                "reviewed-unknown-field.log",
                "{\"severity\":\"info\",\"component\":\"anytype\",\"body\":\"forbidden\"}\n"
                    .to_owned(),
            ),
            (
                "reviewed-duplicate-key.log",
                "{\"severity\":\"info\",\"severity\":\"error\",\"component\":\"anytype\"}\n"
                    .to_owned(),
            ),
        ] {
            let path = write_reviewed_log(name, b"stale\n");
            let invalid_context = write_evidence_context(&path, RUN_MARKER);
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("open invalid reviewed log fixture")
                .write_all(contents.as_bytes())
                .expect("append invalid fresh evidence");
            assert!(
                inspect_log(&path, &invalid_context, Some(RUN_MARKER), true).is_err(),
                "accepted {name}"
            );
            let _ = std::fs::remove_file(path);
            let _ = std::fs::remove_file(invalid_context);
        }

        let stale_only = write_reviewed_log(
            "reviewed-stale-only.log",
            format!("{REVIEWED_EVENT}\n").as_bytes(),
        );
        let stale_context = write_evidence_context(&stale_only, RUN_MARKER);
        assert!(
            inspect_log(&stale_only, &stale_context, Some(RUN_MARKER), true).is_err(),
            "a stale allowlisted event before the start offset must not pass"
        );
        let _ = std::fs::remove_file(stale_only);
        let _ = std::fs::remove_file(stale_context);

        let duplicate_context = write_reviewed_log(
            "reviewed-duplicate-context",
            format!(
                "{}start_bytes=0\n",
                std::fs::read_to_string(&context).expect("read valid context")
            )
            .as_bytes(),
        );
        assert!(
            inspect_log(&valid, &duplicate_context, Some(RUN_MARKER), true).is_err(),
            "duplicate numeric context keys must fail"
        );
        let _ = std::fs::remove_file(duplicate_context);

        let oversized_context = write_reviewed_log("reviewed-oversized-context", &[b'x'; 4097]);
        assert!(
            inspect_log(&valid, &oversized_context, Some(RUN_MARKER), true).is_err(),
            "oversized context must fail before parsing"
        );
        let _ = std::fs::remove_file(oversized_context);

        let source_link = temporary_path("reviewed-source-link");
        std::os::unix::fs::symlink(&valid, &source_link).expect("create source symlink");
        assert!(
            inspect_log(&source_link, &context, Some(RUN_MARKER), true).is_err(),
            "no-follow source open must reject symlinks"
        );
        let _ = std::fs::remove_file(source_link);

        let context_link = temporary_path("reviewed-context-link");
        std::os::unix::fs::symlink(&context, &context_link).expect("create context symlink");
        assert!(
            inspect_log(&valid, &context_link, Some(RUN_MARKER), true).is_err(),
            "no-follow context open must reject symlinks"
        );
        let _ = std::fs::remove_file(context_link);

        let replaced = write_reviewed_log("reviewed-replaced.log", b"pre-start\n");
        let replaced_context = write_evidence_context(&replaced, RUN_MARKER);
        let retired = temporary_path("reviewed-retired.log");
        std::fs::rename(&replaced, &retired).expect("retire reviewed source identity");
        std::fs::write(&replaced, format!("pre-start\n{REVIEWED_EVENT}\n"))
            .expect("write replacement source");
        std::fs::set_permissions(&replaced, std::fs::Permissions::from_mode(0o600))
            .expect("set replacement source permissions");
        assert!(
            inspect_log(&replaced, &replaced_context, Some(RUN_MARKER), true).is_err(),
            "replacement between anchor and audit must fail identity binding"
        );
        let _ = std::fs::remove_file(replaced);
        let _ = std::fs::remove_file(retired);
        let _ = std::fs::remove_file(replaced_context);

        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&valid, std::fs::Permissions::from_mode(0o640))
            .expect("set unsafe reviewed log permissions");
        assert!(inspect_log(&valid, &context, Some(RUN_MARKER), true).is_err());
        let _ = std::fs::remove_file(valid);
        let _ = std::fs::remove_file(context);

        let directory = temporary_path("reviewed-directory");
        std::fs::create_dir(&directory).expect("create non-file fixture");
        let context = write_reviewed_log("reviewed-directory-context", b"invalid");
        assert!(inspect_log(&directory, &context, Some(RUN_MARKER), true).is_err());
        let _ = std::fs::remove_dir(directory);
        let _ = std::fs::remove_file(context);
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
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
        let mut command = Command::new(env!("CARGO_BIN_EXE_any-mcp-process-test"));
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
