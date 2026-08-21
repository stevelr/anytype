// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Test-only complete registries proving the common optional composition seam.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
use rmcp::{
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData,
        ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
        ResourceTemplate,
    },
    schemars::JsonSchema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tiktoken_rs::{CoreBPE, o200k_base};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetSelection, production_optional_metadata, production_optional_registries,
    },
    protocol::{ToolProfile, workflow_tool},
    runtime::StartupStatus,
};

use super::headless_integration::live_scenario::{
    ARTIFACT_TOOL_NAMES, ArtifactCatalogSnapshot, OptionalFastWorkflow, OptionalOperation,
    OptionalRealWorkflow,
};

const ALPHA_READ: &str = "alpha_read";
const ALPHA_WRITE: &str = "alpha_write";
const BETA_READ: &str = "beta_read";
const GAMMA_WRITE: &str = "gamma_write";
const ALPHA_URI: &str = "anytype://optional/alpha";
const ALPHA_ITEM_PREFIX: &str = "anytype://optional/alpha/items/";
const ALPHA_ITEM_URI: &str = "anytype://optional/alpha/items/example";
const ALPHA_TEMPLATE: &str = "anytype://optional/alpha/items/{item_id}";
const OPTIONAL_SNAPSHOT: &str = include_str!("../../tests/snapshots/optional-toolsets.snap");
const OPTIONAL_TOKEN_BUDGET: &str =
    include_str!("../../tests/snapshots/optional-toolsets-token-budget.json");
const PRODUCTION_TOKEN_BUDGET: &str =
    include_str!("../../tests/snapshots/production-optional-token-budget.json");
const ARTIFACT_CATALOG_SNAPSHOT: &str = include_str!("../../tests/snapshots/artifact-catalog.snap");
const COMPACT_CATALOG_SNAPSHOT: &str = include_str!("../../tests/snapshots/catalog-compact.snap");
const COMPACT_READ_ONLY_CATALOG_SNAPSHOT: &str =
    include_str!("../../tests/snapshots/catalog-compact-read-only.snap");
const STANDARD_CATALOG_SNAPSHOT: &str = include_str!("../../tests/snapshots/catalog-normal.snap");
const STANDARD_READ_ONLY_CATALOG_SNAPSHOT: &str =
    include_str!("../../tests/snapshots/catalog-read-only.snap");

const PRODUCTION_SELECTOR: &str = "artifacts,body-blocks,chats,files,members,schema,views-write";
const REVERSE_PRODUCTION_SELECTOR: &str =
    "views-write,schema,members,files,chats,body-blocks,artifacts";
const PRODUCTION_TOOLSET_NAMES: [&str; 7] = [
    "artifacts",
    "body-blocks",
    "chats",
    "files",
    "members",
    "schema",
    "views-write",
];
const PRODUCTION_READ_WRITE_TOOLS: [&str; 39] = [
    "artifact_release",
    "artifact_stage_upload",
    "artifact_status",
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
    "document_export",
    "document_import_create",
    "document_import_update",
    "file_export",
    "file_import",
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
const PRODUCTION_READ_ONLY_TOOLS: [&str; 13] = [
    "artifact_status",
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
const PRODUCTION_MUTATION_TOOLS: [&str; 26] = [
    "artifact_release",
    "artifact_stage_upload",
    "body_block_create",
    "body_block_delete",
    "body_block_move",
    "body_block_update",
    "chat_message_add",
    "chat_message_delete",
    "collection_member_add",
    "collection_member_remove",
    "document_export",
    "document_import_create",
    "document_import_update",
    "file_export",
    "file_import",
    "file_upload",
    "property_create",
    "property_update",
    "rich_page_create",
    "rich_page_resume",
    "space_create",
    "space_update",
    "tag_create",
    "tag_update",
    "type_create",
    "type_update",
];
const FILE_RESOURCE_PROBE: &str = "anytype-file://bytes/space-id/file-id/0/1/0000000000000000000000000000000000000000000000000000000000000000";

type OptionalFastWorkflowFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;
type OptionalFastWorkflowRunner = fn() -> OptionalFastWorkflowFuture;

#[derive(Clone, Copy)]
struct OptionalFastWorkflowRegistration {
    workflow: OptionalFastWorkflow,
    runner: OptionalFastWorkflowRunner,
}

const fn fast_registration(
    workflow: OptionalFastWorkflow,
    runner: OptionalFastWorkflowRunner,
) -> OptionalFastWorkflowRegistration {
    OptionalFastWorkflowRegistration { workflow, runner }
}

fn optional_status_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(
        OptionalFastWorkflow::OptionalStatus,
    ))
}

fn artifacts_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::Artifacts))
}

fn body_blocks_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::BodyBlocks))
}

fn chats_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::Chats))
}

fn files_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::Files))
}

fn members_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::Members))
}

fn schema_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::Schema))
}

fn views_write_fast_runner() -> OptionalFastWorkflowFuture {
    Box::pin(run_optional_fast_workflow(OptionalFastWorkflow::ViewsWrite))
}

const OPTIONAL_FAST_WORKFLOWS: [OptionalFastWorkflowRegistration; 8] = [
    fast_registration(
        OptionalFastWorkflow::OptionalStatus,
        optional_status_fast_runner,
    ),
    fast_registration(OptionalFastWorkflow::Artifacts, artifacts_fast_runner),
    fast_registration(OptionalFastWorkflow::BodyBlocks, body_blocks_fast_runner),
    fast_registration(OptionalFastWorkflow::Chats, chats_fast_runner),
    fast_registration(OptionalFastWorkflow::Files, files_fast_runner),
    fast_registration(OptionalFastWorkflow::Members, members_fast_runner),
    fast_registration(OptionalFastWorkflow::Schema, schema_fast_runner),
    fast_registration(OptionalFastWorkflow::ViewsWrite, views_write_fast_runner),
];

static ALPHA_CALLS: AtomicUsize = AtomicUsize::new(0);
static BETA_CALLS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompleteOutput {
    /// Whether the test-only operation completed.
    complete: bool,
}

#[derive(Debug)]
struct AlphaRegistry;

impl OptionalToolsetRegistry for AlphaRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("alpha")
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read_http(workflow_tool::<EmptyInput, CompleteOutput>(
                ALPHA_READ,
                "Read the bounded test-only alpha state.",
                ToolProfile::Read,
            )?),
            OptionalRegistryTool::mutation_http(workflow_tool::<EmptyInput, CompleteOutput>(
                ALPHA_WRITE,
                "Update the bounded test-only alpha state.",
                ToolProfile::Update,
            )?),
        ])
    }

    fn resources(&self) -> Vec<Resource> {
        vec![
            Resource::new(ALPHA_URI, "alpha_resource")
                .with_description("Bounded test-only alpha resource"),
        ]
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new(ALPHA_TEMPLATE, "alpha_item")
                .with_description("Bounded test-only alpha item"),
        ]
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["alpha_scripted"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["alpha_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        750
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        _runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            if request.name != ALPHA_READ && request.name != ALPHA_WRITE {
                return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            decode_empty(&request)?;
            ALPHA_CALLS.fetch_add(1, Ordering::SeqCst);
            success_result()
        })
    }

    fn owns_resource_uri(&self, uri: &str) -> bool {
        uri == ALPHA_URI
            || uri.strip_prefix(ALPHA_ITEM_PREFIX).is_some_and(|item_id| {
                !item_id.is_empty()
                    && item_id.len() <= 32
                    && item_id.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
    }

    fn owns_resource_template(&self, uri_template: &str) -> bool {
        uri_template == ALPHA_TEMPLATE
    }

    fn read_resource<'a>(
        &'a self,
        request: ReadResourceRequestParams,
        _runtime: &'a RuntimeContext,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<ReadResourceResult, ErrorData>> {
        Box::pin(async move {
            if !self.owns_resource_uri(&request.uri) {
                return Err(ErrorData::method_not_found::<
                    rmcp::model::ReadResourceRequestMethod,
                >());
            }
            let uri = request.uri;
            Ok(ReadResourceResult::new(vec![ResourceContents::text(
                "alpha", uri,
            )]))
        })
    }
}

#[derive(Debug)]
struct BetaRegistry;

impl OptionalToolsetRegistry for BetaRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("beta")
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![OptionalRegistryTool::read_http(workflow_tool::<
            EmptyInput,
            CompleteOutput,
        >(
            BETA_READ,
            "Read the bounded test-only beta state.",
            ToolProfile::Read,
        )?)])
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["beta_scripted"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["beta_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        500
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        _runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            if request.name != BETA_READ {
                return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            decode_empty(&request)?;
            BETA_CALLS.fetch_add(1, Ordering::SeqCst);
            success_result()
        })
    }
}

static ALPHA: AlphaRegistry = AlphaRegistry;
static BETA: BetaRegistry = BetaRegistry;

#[derive(Debug)]
struct GammaRegistry;

impl OptionalToolsetRegistry for GammaRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("gamma")
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![OptionalRegistryTool::mutation_http(workflow_tool::<
            EmptyInput,
            CompleteOutput,
        >(
            GAMMA_WRITE,
            "Update the bounded test-only gamma state.",
            ToolProfile::Update,
        )?)])
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["gamma_scripted"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["gamma_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        500
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        _runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            if request.name != GAMMA_WRITE {
                return Err(ErrorData::method_not_found::<CallToolRequestMethod>());
            }
            decode_empty(&request)?;
            success_result()
        })
    }
}

static GAMMA: GammaRegistry = GammaRegistry;
static LINKED: [&dyn OptionalToolsetRegistry; 3] = [&ALPHA, &BETA, &GAMMA];

fn decode_empty(request: &CallToolRequestParams) -> Result<(), ErrorData> {
    serde_json::from_value::<EmptyInput>(Value::Object(
        request.arguments.clone().unwrap_or_default(),
    ))
    .map(|_| ())
    .map_err(|_| {
        ErrorData::invalid_params(
            "Tool arguments do not match the declared schema.",
            Some(json!({"code": "validation"})),
        )
    })
}

fn success_result() -> Result<CallToolResult, ErrorData> {
    workflow_tool::<EmptyInput, CompleteOutput>(
        "test_result",
        "Encode one test-only result.",
        ToolProfile::Read,
    )
    .map_err(|_| ErrorData::internal_error("Test contract unavailable.", None))?
    .success(&CompleteOutput { complete: true })
    .map_err(|_| ErrorData::internal_error("Test result encoding failed.", None))
}

fn selection(value: &str) -> OptionalToolsetSelection {
    let metadata = LINKED
        .iter()
        .map(|registry| registry.metadata())
        .collect::<Vec<_>>();
    OptionalToolsetSelection::parse(Some(value.to_owned()), &metadata)
        .expect("valid test-only selection")
}

fn runtime(
    value: &str,
    profile: ApplicationProfile,
    read_only: bool,
    grpc_available: bool,
) -> RuntimeContext {
    runtime_with_selection(selection(value), profile, read_only, grpc_available)
}

fn runtime_with_selection(
    optional_toolsets: OptionalToolsetSelection,
    profile: ApplicationProfile,
    read_only: bool,
    grpc_available: bool,
) -> RuntimeContext {
    let client = AnytypeClient::with_config(ClientConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        keystore: Some("env".to_owned()),
        keystore_service: Some("any-mcp-optional-registry-test".to_owned()),
        app_name: "any-mcp-optional-registry-test".to_owned(),
        ..ClientConfig::default()
    })
    .expect("in-memory test client");
    client.set_api_key(HttpCredentials::new("fixture-token"));
    RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        client,
        1,
        Duration::from_secs(1),
        StartupStatus {
            http_available: true,
            grpc_available,
        },
        profile,
        read_only,
        optional_toolsets,
    )
}

fn production_selection(value: Option<&str>) -> OptionalToolsetSelection {
    OptionalToolsetSelection::parse(
        value.map(str::to_owned),
        production_optional_metadata().as_slice(),
    )
    .expect("valid production selection")
}

fn production_server(
    value: Option<&str>,
    profile: ApplicationProfile,
    read_only: bool,
    grpc_available: bool,
) -> Result<AnyMcpServer, ServerBuildError> {
    AnyMcpServer::new(runtime_with_selection(
        production_selection(value),
        profile,
        read_only,
        grpc_available,
    ))
}

const fn fast_workflow_selector(workflow: OptionalFastWorkflow) -> &'static str {
    match workflow {
        OptionalFastWorkflow::OptionalStatus | OptionalFastWorkflow::Members => "members",
        OptionalFastWorkflow::Artifacts => "artifacts",
        OptionalFastWorkflow::BodyBlocks => "body-blocks",
        OptionalFastWorkflow::Chats => "chats",
        OptionalFastWorkflow::Files => "files",
        OptionalFastWorkflow::Schema => "schema",
        OptionalFastWorkflow::ViewsWrite => "views-write",
    }
}

async fn run_optional_fast_workflow(workflow: OptionalFastWorkflow) {
    let server = production_server(
        Some(fast_workflow_selector(workflow)),
        ApplicationProfile::Compact,
        false,
        true,
    )
    .expect("selected production fast-workflow server");
    let before = server.runtime().client().http_metrics();
    assert_eq!(server.phase1_dispatch_polls(), 0);

    let operations = OptionalOperation::ALL
        .into_iter()
        .filter(|operation| operation.fast_workflow() == workflow)
        .collect::<Vec<_>>();
    assert!(!operations.is_empty(), "{workflow:?}");

    for operation in operations {
        if operation == OptionalOperation::FileByteResource {
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let error = server
                .read_resource_wire(
                    ReadResourceRequestParams::new(FILE_RESOURCE_PROBE),
                    &cancellation,
                )
                .await
                .expect_err("selected file resource reaches its production handler");
            assert_ne!(
                error,
                ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>(),
                "{operation:?}"
            );
        } else {
            let name = operation.tool_name().expect("tool operation has a name");
            assert!(
                server.tools().iter().any(|tool| tool.name == name),
                "{operation:?} is absent from its selected production catalog"
            );
            let error = server
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(
                        json!({"fast_workflow_unknown": true})
                            .as_object()
                            .cloned()
                            .expect("strict malformed arguments"),
                    ),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("selected optional tool reaches its strict production decoder");
            assert_eq!(
                error.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "{operation:?}"
            );
        }
        assert_eq!(server.phase1_dispatch_polls(), 0, "{operation:?}");
        assert_eq!(
            server.runtime().client().http_metrics(),
            before,
            "{operation:?} performed unexpected HTTP work"
        );
    }
}

fn server(value: &str, profile: ApplicationProfile, read_only: bool) -> AnyMcpServer {
    AnyMcpServer::new_with_optional_registries(runtime(value, profile, read_only, true), &LINKED)
        .expect("composed test-only catalog")
}

fn tool_names_owned(server: &AnyMcpServer) -> Vec<String> {
    server
        .tools()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect()
}

fn production_optional_tool_names(selected: &AnyMcpServer, base: &AnyMcpServer) -> Vec<String> {
    let base_names = base
        .tools()
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<std::collections::HashSet<_>>();
    selected
        .tools()
        .iter()
        .filter(|tool| !base_names.contains(tool.name.as_ref()))
        .map(|tool| tool.name.to_string())
        .collect()
}

fn optional_tool_names(server: &AnyMcpServer) -> Vec<&str> {
    server
        .tools()
        .iter()
        .map(|tool| tool.name.as_ref())
        .filter(|name| {
            name.starts_with("alpha_") || *name == BETA_READ || *name == "optional_toolset_status"
        })
        .collect()
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

fn optional_snapshot() -> String {
    let catalogs = [false, true]
        .into_iter()
        .map(|read_only| {
            let server = server("beta,alpha", ApplicationProfile::Compact, read_only);
            let tools = server
                .tools()
                .iter()
                .filter(|tool| {
                    let name = tool.name.as_ref();
                    name.starts_with("alpha_")
                        || name == BETA_READ
                        || name == "optional_toolset_status"
                })
                .collect::<Vec<_>>();
            let resources = server
                .list_resources_wire(None)
                .expect("resource inventory")
                .resources
                .into_iter()
                .filter(|resource| resource.uri.starts_with("anytype://optional/"))
                .collect::<Vec<_>>();
            let templates = server
                .list_resource_templates_wire(None)
                .expect("template inventory")
                .resource_templates
                .into_iter()
                .filter(|template| template.uri_template.starts_with("anytype://optional/"))
                .collect::<Vec<_>>();
            json!({
                "read_only": read_only,
                "tools": tools,
                "resources": resources,
                "resource_templates": templates,
            })
        })
        .collect::<Vec<_>>();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&canonical_json(json!({"catalogs": catalogs})))
            .expect("serialize optional snapshot")
    )
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct OptionalTokenBudget {
    tokenizer: String,
    base_catalog_sha256: String,
    base_catalog_tokens: usize,
    common_status_ceiling_tokens: usize,
    common_status_tokens: usize,
    alpha_selected: Vec<String>,
    alpha_ceiling_tokens: usize,
    alpha_tool_tokens: BTreeMap<String, usize>,
    alpha_composed_total_tokens: usize,
    alpha_read_only_composed_total_tokens: usize,
    alpha_representative_max_result_tokens: usize,
    beta_selected: Vec<String>,
    beta_ceiling_tokens: usize,
    beta_tool_tokens: BTreeMap<String, usize>,
    beta_composed_total_tokens: usize,
    beta_representative_max_result_tokens: usize,
    gamma_selected: Vec<String>,
    gamma_ceiling_tokens: usize,
    gamma_tool_tokens: BTreeMap<String, usize>,
    gamma_composed_total_tokens: usize,
    gamma_read_only_composed_total_tokens: usize,
    gamma_representative_max_result_tokens: usize,
    all_selected: Vec<String>,
    all_composed_total_tokens: usize,
}

fn compact_canonical_json(value: Value) -> String {
    serde_json::to_string(&canonical_json(value)).expect("canonical compact JSON")
}

fn token_count(tokenizer: &CoreBPE, value: Value) -> usize {
    tokenizer
        .encode_with_special_tokens(&compact_canonical_json(value))
        .len()
}

fn canonical_sha256(value: Value) -> String {
    let encoded = compact_canonical_json(value);
    Sha256::digest(encoded.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn tools_list_value(server: &AnyMcpServer) -> Value {
    serde_json::to_value(crate::server::stable_list_tools_result(
        server.tools().to_vec(),
    ))
    .expect("complete tools/list result")
}

fn production_catalog_snapshot(server: &AnyMcpServer, read_only: bool) -> String {
    format!(
        "{}\n",
        serde_json::to_string_pretty(&canonical_json(json!({
            "read_only": read_only,
            "tools": server.tools(),
        })))
        .expect("production catalog snapshot")
    )
}

fn production_composition_budget(
    tokenizer: &CoreBPE,
    profile: ApplicationProfile,
    read_only: bool,
) -> Value {
    let base = production_server(None, profile, read_only, true).expect("base production server");
    let selected = production_server(Some(PRODUCTION_SELECTOR), profile, read_only, true)
        .expect("all-selected production server");
    let base_value = tools_list_value(&base);
    let selected_value = tools_list_value(&selected);
    let base_tokens = token_count(tokenizer, base_value.clone());
    let selected_tokens = token_count(tokenizer, selected_value.clone());
    json!({
        "base_sha256":canonical_sha256(base_value),
        "base_tokens":base_tokens,
        "selected_sha256":canonical_sha256(selected_value),
        "selected_tokens":selected_tokens,
        "selected_contribution_tokens":selected_tokens.saturating_sub(base_tokens),
    })
}

fn production_token_budget() -> Value {
    let tokenizer = o200k_base().expect("pinned o200k_base tokenizer");
    let base = production_server(None, ApplicationProfile::Compact, false, true)
        .expect("base compact production server");
    let mut registry_budgets = BTreeMap::new();
    let mut registry_ceiling_tokens = 0_usize;

    for registry in production_optional_registries() {
        let metadata = registry.metadata();
        let selected = production_server(
            Some(metadata.name),
            ApplicationProfile::Compact,
            false,
            true,
        )
        .expect("single production registry");
        let base_names = base
            .tools()
            .iter()
            .map(|tool| tool.name.as_ref())
            .collect::<std::collections::HashSet<_>>();
        let domain_tools = selected
            .tools()
            .iter()
            .filter(|tool| {
                tool.name != "optional_toolset_status" && !base_names.contains(tool.name.as_ref())
            })
            .collect::<Vec<_>>();
        let domain_tokens = domain_tools
            .iter()
            .map(|tool| {
                token_count(
                    &tokenizer,
                    serde_json::to_value(tool).expect("production tool value"),
                )
            })
            .sum::<usize>();
        let base_tokens = token_count(&tokenizer, tools_list_value(&base));
        let selected_tokens = token_count(&tokenizer, tools_list_value(&selected));
        let ceiling = registry.catalog_token_ceiling();
        registry_ceiling_tokens = registry_ceiling_tokens.saturating_add(ceiling);
        registry_budgets.insert(
            metadata.name,
            json!({
                "catalog_ceiling_tokens":ceiling,
                "domain_tokens":domain_tokens,
                "selected_contribution_tokens":selected_tokens.saturating_sub(base_tokens),
                "tool_count":domain_tools.len(),
            }),
        );
    }

    let status = production_server(Some("members"), ApplicationProfile::Compact, false, true)
        .expect("status production server")
        .tools()
        .iter()
        .find(|tool| tool.name == "optional_toolset_status")
        .cloned()
        .expect("optional status tool");
    let status_tokens = token_count(
        &tokenizer,
        serde_json::to_value(crate::server::stable_list_tools_result(vec![status]))
            .expect("status tools/list value"),
    );

    canonical_json(json!({
        "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
        "selected":PRODUCTION_TOOLSET_NAMES,
        "common_status_ceiling_tokens":500,
        "common_status_tokens":status_tokens,
        "registry_ceiling_tokens":registry_ceiling_tokens,
        "registry_budgets":registry_budgets,
        "compositions":{
            "compact_read_write":production_composition_budget(
                &tokenizer,
                ApplicationProfile::Compact,
                false,
            ),
            "compact_read_only":production_composition_budget(
                &tokenizer,
                ApplicationProfile::Compact,
                true,
            ),
            "standard_read_write":production_composition_budget(
                &tokenizer,
                ApplicationProfile::Standard,
                false,
            ),
            "standard_read_only":production_composition_budget(
                &tokenizer,
                ApplicationProfile::Standard,
                true,
            ),
        }
    }))
}

fn production_budget_json() -> String {
    let pretty = serde_json::to_string_pretty(&production_token_budget())
        .expect("serialize production token budget")
        .replace(
            "[\n    \"artifacts\",\n    \"body-blocks\",\n    \"chats\",\n    \"files\",\n    \"members\",\n    \"schema\",\n    \"views-write\"\n  ]",
            "[\"artifacts\", \"body-blocks\", \"chats\", \"files\", \"members\", \"schema\", \"views-write\"]",
        );
    format!("{pretty}\n")
}

fn tools_list_tokens(tokenizer: &CoreBPE, value: &str, read_only: bool) -> usize {
    let server = server(value, ApplicationProfile::Compact, read_only);
    token_count(
        tokenizer,
        serde_json::to_value(crate::server::stable_list_tools_result(
            server.tools().to_vec(),
        ))
        .expect("complete tools/list result"),
    )
}

fn canonical_selected(value: &str) -> Vec<String> {
    selection(value).names().map(str::to_owned).collect()
}

fn optional_tool_tokens(tokenizer: &CoreBPE, selected: &str) -> BTreeMap<String, usize> {
    server(selected, ApplicationProfile::Compact, false)
        .tools()
        .iter()
        .filter(|tool| tool.name != "optional_toolset_status" && tool.name.starts_with(selected))
        .map(|tool| {
            (
                tool.name.to_string(),
                token_count(
                    tokenizer,
                    serde_json::to_value(tool).expect("serialize optional tool"),
                ),
            )
        })
        .collect()
}

fn actual_token_budget() -> OptionalTokenBudget {
    let tokenizer = o200k_base().expect("pinned o200k_base tokenizer");
    let status = optional_toolset_status_tool()
        .expect("optional status contract")
        .into_tool();
    let base_value = serde_json::to_value(
        AnyMcpServer::new(runtime_with_selection(
            OptionalToolsetSelection::default(),
            ApplicationProfile::Compact,
            false,
            false,
        ))
        .expect("base compact catalog")
        .list_tools_wire(None)
        .expect("base tools/list"),
    )
    .expect("serialize base tools/list");
    let base_json = compact_canonical_json(base_value.clone());
    let base_catalog_sha256 = Sha256::digest(base_json.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let representative_result = serde_json::to_value(success_result().expect("test result"))
        .expect("serialize representative result");
    let representative_result_tokens = token_count(&tokenizer, representative_result);
    OptionalTokenBudget {
        tokenizer: "o200k_base".to_owned(),
        base_catalog_sha256,
        base_catalog_tokens: token_count(&tokenizer, base_value),
        common_status_ceiling_tokens: 500,
        common_status_tokens: token_count(
            &tokenizer,
            serde_json::to_value(crate::server::stable_list_tools_result(vec![status]))
                .expect("status tools/list result"),
        ),
        alpha_selected: canonical_selected("alpha"),
        alpha_ceiling_tokens: ALPHA.catalog_token_ceiling(),
        alpha_tool_tokens: optional_tool_tokens(&tokenizer, "alpha"),
        alpha_composed_total_tokens: tools_list_tokens(&tokenizer, "alpha", false),
        alpha_read_only_composed_total_tokens: tools_list_tokens(&tokenizer, "alpha", true),
        alpha_representative_max_result_tokens: representative_result_tokens,
        beta_selected: canonical_selected("beta"),
        beta_ceiling_tokens: BETA.catalog_token_ceiling(),
        beta_tool_tokens: optional_tool_tokens(&tokenizer, "beta"),
        beta_composed_total_tokens: tools_list_tokens(&tokenizer, "beta", false),
        beta_representative_max_result_tokens: representative_result_tokens,
        gamma_selected: canonical_selected("gamma"),
        gamma_ceiling_tokens: GAMMA.catalog_token_ceiling(),
        gamma_tool_tokens: optional_tool_tokens(&tokenizer, "gamma"),
        gamma_composed_total_tokens: tools_list_tokens(&tokenizer, "gamma", false),
        gamma_read_only_composed_total_tokens: tools_list_tokens(&tokenizer, "gamma", true),
        gamma_representative_max_result_tokens: representative_result_tokens,
        all_selected: canonical_selected("beta,gamma,alpha"),
        all_composed_total_tokens: tools_list_tokens(&tokenizer, "beta,gamma,alpha", false),
    }
}

fn budget_json(budget: &OptionalTokenBudget) -> String {
    let pretty = serde_json::to_string_pretty(budget)
        .expect("serialize optional token budget")
        .replace("[\n    \"alpha\"\n  ]", "[\"alpha\"]")
        .replace("[\n    \"beta\"\n  ]", "[\"beta\"]")
        .replace("[\n    \"gamma\"\n  ]", "[\"gamma\"]")
        .replace(
            "[\n    \"alpha\",\n    \"beta\"\n  ]",
            "[\"alpha\", \"beta\"]",
        )
        .replace(
            "[\n    \"alpha\",\n    \"beta\",\n    \"gamma\"\n  ]",
            "[\"alpha\", \"beta\", \"gamma\"]",
        );
    format!("{pretty}\n")
}

#[test]
fn selected_registries_compose_deterministically_and_orthogonally() {
    let first = server("beta,alpha", ApplicationProfile::Compact, false);
    let second = server("alpha,beta", ApplicationProfile::Standard, false);
    assert_eq!(
        optional_tool_names(&first),
        [
            ALPHA_READ,
            ALPHA_WRITE,
            BETA_READ,
            "optional_toolset_status"
        ]
    );
    for name in optional_tool_names(&first) {
        let left = first.tools().iter().find(|tool| tool.name == name).unwrap();
        let right = second
            .tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap();
        assert_eq!(left, right, "{name} contract must be profile-independent");
    }

    let read_only = server("alpha,beta", ApplicationProfile::Compact, true);
    assert_eq!(
        optional_tool_names(&read_only),
        [ALPHA_READ, BETA_READ, "optional_toolset_status"]
    );
    assert_eq!(
        first
            .list_resources_wire(None)
            .unwrap()
            .resources
            .iter()
            .map(|resource| resource.uri.as_str())
            .collect::<Vec<_>>(),
        [ALPHA_URI]
    );
    assert!(
        first
            .list_resource_templates_wire(None)
            .unwrap()
            .resource_templates
            .iter()
            .any(|template| template.uri_template == ALPHA_TEMPLATE)
    );
    for name in [ALPHA_READ, BETA_READ, "optional_toolset_status"] {
        let read_write_tool = first.tools().iter().find(|tool| tool.name == name).unwrap();
        let read_only_tool = read_only
            .tools()
            .iter()
            .find(|tool| tool.name == name)
            .unwrap();
        assert_eq!(
            read_write_tool, read_only_tool,
            "{name} contract must be access-mode-independent"
        );
    }
    assert_eq!(optional_snapshot(), OPTIONAL_SNAPSHOT);
}

#[tokio::test]
#[serial_test::serial(optional_registry_calls)]
async fn optional_toolset_status_direct_contract() {
    ALPHA_CALLS.store(0, Ordering::SeqCst);
    let selected = server("beta,alpha", ApplicationProfile::Compact, false);
    let status = selected
        .dispatch_tool(
            CallToolRequestParams::new("optional_toolset_status"),
            &CancellationToken::new(),
        )
        .await
        .expect("selected status result");
    assert_eq!(
        status.structured_content,
        Some(json!({
            "configured_toolsets": ["alpha", "beta"],
            "active_toolsets": ["alpha", "beta"]
        }))
    );
    assert_eq!(selected.phase1_dispatch_polls(), 0);
    let status_error = selected
        .dispatch_tool(
            CallToolRequestParams::new("optional_toolset_status")
                .with_arguments(json!({"unexpected": true}).as_object().cloned().unwrap()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("selected malformed status arguments");
    assert_eq!(status_error, invalid_arguments());
    assert_eq!(selected.phase1_dispatch_polls(), 0);

    let result = selected
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ),
            &CancellationToken::new(),
        )
        .await
        .expect("selected alpha dispatch");
    assert_eq!(result.structured_content, Some(json!({"complete": true})));
    assert_eq!(ALPHA_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(selected.phase1_dispatch_polls(), 0);

    let resource = selected
        .read_resource_wire(
            ReadResourceRequestParams::new(ALPHA_URI),
            &CancellationToken::new(),
        )
        .await
        .expect("selected alpha resource");
    assert_eq!(resource.contents.len(), 1);
    let template_resource = selected
        .read_resource_wire(
            ReadResourceRequestParams::new(ALPHA_ITEM_URI),
            &CancellationToken::new(),
        )
        .await
        .expect("selected alpha template instance");
    assert_eq!(template_resource.contents.len(), 1);
}

#[tokio::test]
async fn optional_toolset_status_stdio_contract() {
    let server = server("beta,alpha", ApplicationProfile::Compact, false);
    let response = crate::preview::dispatch_modern(
        &server,
        json!(1),
        "tools/call",
        json!({"name": "optional_toolset_status", "arguments": {}})
            .as_object()
            .cloned()
            .unwrap(),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        response["result"]["structuredContent"],
        json!({
            "configured_toolsets": ["alpha", "beta"],
            "active_toolsets": ["alpha", "beta"]
        })
    );
    assert_eq!(response["result"]["isError"], false);
}

#[tokio::test]
async fn mutation_only_registry_is_configured_but_inactive_in_read_only_mode() {
    let read_only = server("gamma", ApplicationProfile::Compact, true);
    assert_eq!(optional_tool_names(&read_only), ["optional_toolset_status"]);
    let status = read_only
        .dispatch_tool(
            CallToolRequestParams::new("optional_toolset_status"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        status.structured_content,
        Some(json!({
            "configured_toolsets": ["gamma"],
            "active_toolsets": []
        }))
    );
}

#[tokio::test]
#[serial_test::serial(optional_registry_calls)]
async fn disabled_and_read_only_calls_reject_before_decode_or_handler_work() {
    ALPHA_CALLS.store(0, Ordering::SeqCst);
    let disabled = server("beta", ApplicationProfile::Compact, false);
    let secret_arguments = json!({"token": "do-not-decode"})
        .as_object()
        .cloned()
        .unwrap();
    let error = disabled
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ).with_arguments(secret_arguments.clone()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled name is method-not-found");
    assert_eq!(
        error,
        ErrorData::method_not_found::<CallToolRequestMethod>()
    );
    assert_eq!(ALPHA_CALLS.load(Ordering::SeqCst), 0);

    let continuation_error = disabled
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ).with_request_state("opaque-state"),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled continuation name is method-not-found");
    assert_eq!(
        continuation_error,
        ErrorData::method_not_found::<CallToolRequestMethod>()
    );
    assert_eq!(disabled.phase1_dispatch_polls(), 0);

    let status_error = AnyMcpServer::new(runtime_with_selection(
        OptionalToolsetSelection::default(),
        ApplicationProfile::Compact,
        false,
        false,
    ))
    .unwrap()
    .dispatch_tool(
        CallToolRequestParams::new("optional_toolset_status")
            .with_arguments(secret_arguments.clone()),
        &CancellationToken::new(),
    )
    .await
    .expect_err("unselected status is method-not-found");
    assert_eq!(
        status_error,
        ErrorData::method_not_found::<CallToolRequestMethod>()
    );

    let read_only = server("alpha", ApplicationProfile::Compact, true);
    let result = read_only
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_WRITE).with_arguments(secret_arguments),
            &CancellationToken::new(),
        )
        .await
        .expect("read-only rejection is a tool error");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(ALPHA_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(read_only.phase1_dispatch_polls(), 0);

    let selected = server("alpha", ApplicationProfile::Compact, false);
    let selected_continuation_error = selected
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ).with_input_responses(BTreeMap::new()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("selected optional continuation metadata is rejected");
    assert_eq!(selected_continuation_error, invalid_arguments());
    assert_eq!(ALPHA_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(selected.phase1_dispatch_polls(), 0);

    let resource_error = disabled
        .read_resource_wire(
            ReadResourceRequestParams::new(ALPHA_URI),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled resource is method-not-found");
    assert_eq!(
        resource_error,
        ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>()
    );
    let template_error = disabled
        .read_resource_wire(
            ReadResourceRequestParams::new(ALPHA_ITEM_URI),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled template instance is method-not-found");
    assert_eq!(
        template_error,
        ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>()
    );
}

#[tokio::test]
async fn stable_and_preview_expose_identical_selected_contracts() {
    let server = server("alpha,beta", ApplicationProfile::Compact, false);
    let stable = serde_json::to_value(server.list_tools_wire(None).unwrap()).unwrap();
    let preview = crate::preview::dispatch_modern(
        &server,
        json!(1),
        "tools/list",
        Map::new(),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(preview["result"]["tools"], stable["tools"]);

    let stable_resources = serde_json::to_value(server.list_resources_wire(None).unwrap()).unwrap();
    let preview_resources = crate::preview::dispatch_modern(
        &server,
        json!(2),
        "resources/list",
        Map::new(),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        preview_resources["result"]["resources"],
        stable_resources["resources"]
    );

    let stable_templates =
        serde_json::to_value(server.list_resource_templates_wire(None).unwrap()).unwrap();
    let preview_templates = crate::preview::dispatch_modern(
        &server,
        json!(3),
        "resources/templates/list",
        Map::new(),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        preview_templates["result"]["resourceTemplates"],
        stable_templates["resourceTemplates"]
    );
}

#[tokio::test]
async fn phase_one_server_status_is_unchanged_by_optional_selection() {
    let base = AnyMcpServer::new(runtime_with_selection(
        OptionalToolsetSelection::default(),
        ApplicationProfile::Compact,
        false,
        true,
    ))
    .unwrap();
    let selected = server("alpha", ApplicationProfile::Compact, false);
    let cancellation = CancellationToken::new();
    let base_result = base
        .dispatch_tool(CallToolRequestParams::new("server_status"), &cancellation)
        .await
        .unwrap();
    let selected_result = selected
        .dispatch_tool(CallToolRequestParams::new("server_status"), &cancellation)
        .await
        .unwrap();
    assert_eq!(selected_result, base_result);
    assert_eq!(base.phase1_dispatch_polls(), 1);
    assert_eq!(selected.phase1_dispatch_polls(), 1);
}

async fn production_status(server: &AnyMcpServer, protocol: &ProtocolVersion) -> Value {
    server
        .dispatch_tool_for_protocol(
            CallToolRequestParams::new("optional_toolset_status"),
            protocol,
            &CancellationToken::new(),
        )
        .await
        .expect("production optional status")
        .structured_content
        .expect("structured optional status")
}

#[tokio::test]
async fn production_optional_fast_workflow_registration_is_exact_and_executable() {
    assert_eq!(
        OPTIONAL_FAST_WORKFLOWS.map(|registration| registration.workflow),
        OptionalFastWorkflow::ALL
    );

    let expected_counts = [
        (OptionalFastWorkflow::OptionalStatus, 1_usize),
        (OptionalFastWorkflow::Artifacts, 8),
        (OptionalFastWorkflow::BodyBlocks, 7),
        (OptionalFastWorkflow::Chats, 6),
        (OptionalFastWorkflow::Files, 4),
        (OptionalFastWorkflow::Members, 2),
        (OptionalFastWorkflow::Schema, 9),
        (OptionalFastWorkflow::ViewsWrite, 3),
    ];
    let mut partition = BTreeMap::new();
    let mut tool_names = BTreeSet::new();
    for operation in OptionalOperation::ALL {
        *partition
            .entry(operation.fast_workflow())
            .or_insert(0_usize) += 1;
        if let Some(name) = operation.tool_name() {
            assert!(tool_names.insert(name), "duplicate optional tool {name}");
        }
    }
    assert_eq!(OptionalOperation::ALL.len(), 40);
    assert_eq!(
        partition,
        expected_counts.into_iter().collect::<BTreeMap<_, _>>()
    );
    assert_eq!(
        tool_names,
        PRODUCTION_READ_WRITE_TOOLS
            .into_iter()
            .collect::<BTreeSet<_>>()
    );
    assert_eq!(
        OptionalOperation::FileByteResource.resource_family_name(),
        Some("anytype-file://bytes/{space_id}/{file_id}/{offset}/{length}/{sha256}")
    );
    assert_eq!(
        OptionalOperation::ALL
            .into_iter()
            .map(OptionalOperation::real_workflow)
            .collect::<BTreeSet<_>>(),
        OptionalRealWorkflow::ALL.into_iter().collect()
    );

    for registration in OPTIONAL_FAST_WORKFLOWS {
        (registration.runner)().await;
    }
}

#[tokio::test]
async fn production_all_selected_inventories_status_and_reverse_order_are_exact() {
    let expected_status = json!({
        "configured_toolsets":PRODUCTION_TOOLSET_NAMES,
        "active_toolsets":PRODUCTION_TOOLSET_NAMES,
    });
    for profile in [ApplicationProfile::Compact, ApplicationProfile::Standard] {
        for read_only in [false, true] {
            let base =
                production_server(None, profile, read_only, true).expect("base production server");
            let selected = production_server(Some(PRODUCTION_SELECTOR), profile, read_only, true)
                .expect("all-selected production server");
            let reversed =
                production_server(Some(REVERSE_PRODUCTION_SELECTOR), profile, read_only, true)
                    .expect("reverse-selected production server");
            let expected = if read_only {
                PRODUCTION_READ_ONLY_TOOLS.as_slice()
            } else {
                PRODUCTION_READ_WRITE_TOOLS.as_slice()
            };
            assert_eq!(
                production_optional_tool_names(&selected, &base),
                expected,
                "profile={profile:?} read_only={read_only}"
            );
            assert_eq!(tool_names_owned(&reversed), tool_names_owned(&selected));
            assert_eq!(
                production_status(&selected, &ProtocolVersion::V_2025_11_25).await,
                expected_status
            );
            assert_eq!(
                production_status(&reversed, &ProtocolVersion::V_2026_07_28).await,
                expected_status
            );
        }
    }
}

#[tokio::test]
async fn production_all_selected_stable_and_preview_catalogs_are_identical() {
    for profile in [ApplicationProfile::Compact, ApplicationProfile::Standard] {
        for read_only in [false, true] {
            let server = production_server(Some(PRODUCTION_SELECTOR), profile, read_only, true)
                .expect("all-selected production server");
            let stable_tools =
                serde_json::to_value(server.list_tools_wire(None).expect("stable tools"))
                    .expect("stable tools value");
            let preview_tools = crate::preview::dispatch_modern(
                &server,
                json!(1),
                "tools/list",
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(preview_tools["result"]["tools"], stable_tools["tools"]);

            let stable_resources =
                serde_json::to_value(server.list_resources_wire(None).expect("stable resources"))
                    .expect("stable resources value");
            let preview_resources = crate::preview::dispatch_modern(
                &server,
                json!(2),
                "resources/list",
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(
                preview_resources["result"]["resources"],
                stable_resources["resources"]
            );

            let stable_templates = serde_json::to_value(
                server
                    .list_resource_templates_wire(None)
                    .expect("stable templates"),
            )
            .expect("stable templates value");
            let preview_templates = crate::preview::dispatch_modern(
                &server,
                json!(3),
                "resources/templates/list",
                Map::new(),
                &CancellationToken::new(),
            )
            .await;
            assert_eq!(
                preview_templates["result"]["resourceTemplates"],
                stable_templates["resourceTemplates"]
            );

            let stable_status = production_status(&server, &ProtocolVersion::V_2025_11_25).await;
            let preview_status = production_status(&server, &ProtocolVersion::V_2026_07_28).await;
            assert_eq!(preview_status, stable_status);
        }
    }
}

#[tokio::test]
async fn production_disabled_and_read_only_sweeps_stop_before_decode_or_io() {
    let disabled = production_server(None, ApplicationProfile::Compact, false, false)
        .expect("disabled production server");
    let before_disabled = disabled.runtime().client().http_metrics();
    let secret_arguments = json!({"credential-like": "must-not-decode"})
        .as_object()
        .cloned()
        .expect("secret arguments");
    for name in PRODUCTION_READ_WRITE_TOOLS
        .iter()
        .copied()
        .filter(|name| *name != "optional_toolset_status")
    {
        let error = disabled
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(secret_arguments.clone()),
                &CancellationToken::new(),
            )
            .await
            .expect_err("disabled production tool");
        assert_eq!(
            error,
            ErrorData::method_not_found::<CallToolRequestMethod>(),
            "{name}"
        );
    }
    let status_error = disabled
        .dispatch_tool(
            CallToolRequestParams::new("optional_toolset_status")
                .with_arguments(secret_arguments.clone()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled optional status");
    assert_eq!(
        status_error,
        ErrorData::method_not_found::<CallToolRequestMethod>()
    );
    let resource_error = disabled
        .read_resource_wire(
            ReadResourceRequestParams::new(FILE_RESOURCE_PROBE),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled file resource");
    assert_eq!(
        resource_error,
        ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>()
    );
    assert_eq!(disabled.phase1_dispatch_polls(), 0);
    assert_eq!(disabled.runtime().client().http_metrics(), before_disabled);

    let read_only = production_server(
        Some(PRODUCTION_SELECTOR),
        ApplicationProfile::Compact,
        true,
        true,
    )
    .expect("read-only all-selected production server");
    let read_write = production_server(
        Some(PRODUCTION_SELECTOR),
        ApplicationProfile::Compact,
        false,
        true,
    )
    .expect("read-write all-selected production server");
    let read_only_names = read_only
        .tools()
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<std::collections::HashSet<_>>();
    let omitted = production_optional_tool_names(&read_write, &disabled)
        .into_iter()
        .filter(|name| {
            name != "optional_toolset_status" && !read_only_names.contains(name.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(omitted, PRODUCTION_MUTATION_TOOLS);

    let before_read_only = read_only.runtime().client().http_metrics();
    let mut expected_rejection = None;
    for name in PRODUCTION_MUTATION_TOOLS {
        let result = read_only
            .dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(secret_arguments.clone()),
                &CancellationToken::new(),
            )
            .await
            .expect("read-only stale mutation rejection");
        assert_eq!(result.is_error, Some(true), "{name}");
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value.get("code"))
                .and_then(Value::as_str),
            Some("validation"),
            "{name}"
        );
        if let Some(expected) = expected_rejection.as_ref() {
            assert_eq!(&result, expected, "{name}");
        } else {
            expected_rejection = Some(result);
        }
    }
    assert_eq!(read_only.phase1_dispatch_polls(), 0);
    assert_eq!(
        read_only.runtime().client().http_metrics(),
        before_read_only
    );
}

#[tokio::test]
async fn production_leave_one_out_isolates_each_registry_before_decode_or_io() {
    let secret_arguments = json!({"credential-like": "must-not-decode"})
        .as_object()
        .cloned()
        .expect("secret arguments");
    let mut dynamic_resource_owner_cells = 0_usize;

    for omitted_registry in production_optional_registries() {
        let omitted_name = omitted_registry.metadata().name;
        let selected_names = PRODUCTION_TOOLSET_NAMES
            .iter()
            .copied()
            .filter(|name| *name != omitted_name)
            .collect::<Vec<_>>();
        assert_eq!(selected_names.len(), PRODUCTION_TOOLSET_NAMES.len() - 1);
        let selector = selected_names.join(",");
        let expected_status = json!({
            "configured_toolsets": selected_names,
            "active_toolsets": selected_names,
        });

        let read_write_base = production_server(None, ApplicationProfile::Compact, false, true)
            .expect("read-write base production server");
        let read_write_all = production_server(
            Some(PRODUCTION_SELECTOR),
            ApplicationProfile::Compact,
            false,
            true,
        )
        .expect("read-write all-selected production server");
        let read_write_single =
            production_server(Some(omitted_name), ApplicationProfile::Compact, false, true)
                .expect("read-write single-registry production server");

        let base_tool_names = tool_names_owned(&read_write_base)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let owned_tool_names = tool_names_owned(&read_write_single)
            .into_iter()
            .filter(|name| {
                name != "optional_toolset_status" && !base_tool_names.contains(name.as_str())
            })
            .collect::<BTreeSet<_>>();
        assert!(
            !owned_tool_names.is_empty(),
            "{omitted_name} must own at least one production tool"
        );

        let base_resource_uris = read_write_base
            .list_resources_wire(None)
            .expect("base resource inventory")
            .resources
            .into_iter()
            .map(|resource| resource.uri.to_string())
            .collect::<BTreeSet<_>>();
        let all_resource_uris = read_write_all
            .list_resources_wire(None)
            .expect("all-selected resource inventory")
            .resources
            .into_iter()
            .map(|resource| resource.uri.to_string())
            .collect::<BTreeSet<_>>();
        let owned_resource_uris = read_write_single
            .list_resources_wire(None)
            .expect("single-registry resource inventory")
            .resources
            .into_iter()
            .map(|resource| resource.uri.to_string())
            .filter(|uri| !base_resource_uris.contains(uri))
            .collect::<BTreeSet<_>>();
        assert!(
            owned_resource_uris
                .iter()
                .all(|uri| omitted_registry.owns_resource_uri(uri)),
            "{omitted_name} resource inventory must match registry ownership"
        );

        let base_template_uris = read_write_base
            .list_resource_templates_wire(None)
            .expect("base resource-template inventory")
            .resource_templates
            .into_iter()
            .map(|template| template.uri_template.to_string())
            .collect::<BTreeSet<_>>();
        let all_template_uris = read_write_all
            .list_resource_templates_wire(None)
            .expect("all-selected resource-template inventory")
            .resource_templates
            .into_iter()
            .map(|template| template.uri_template.to_string())
            .collect::<BTreeSet<_>>();
        let owned_template_uris = read_write_single
            .list_resource_templates_wire(None)
            .expect("single-registry resource-template inventory")
            .resource_templates
            .into_iter()
            .map(|template| template.uri_template.to_string())
            .filter(|uri| !base_template_uris.contains(uri))
            .collect::<BTreeSet<_>>();
        assert!(
            owned_template_uris
                .iter()
                .all(|uri| omitted_registry.owns_resource_template(uri)),
            "{omitted_name} resource-template inventory must match registry ownership"
        );

        for read_only in [false, true] {
            let base = production_server(None, ApplicationProfile::Compact, read_only, true)
                .expect("base production server");
            let all = production_server(
                Some(PRODUCTION_SELECTOR),
                ApplicationProfile::Compact,
                read_only,
                true,
            )
            .expect("all-selected production server");
            let leave_one = production_server(
                Some(selector.as_str()),
                ApplicationProfile::Compact,
                read_only,
                true,
            )
            .expect("leave-one-out production server");
            let single = production_server(
                Some(omitted_name),
                ApplicationProfile::Compact,
                read_only,
                true,
            )
            .expect("single-registry production server");

            let base_names = tool_names_owned(&base).into_iter().collect::<BTreeSet<_>>();
            let all_names = tool_names_owned(&all).into_iter().collect::<BTreeSet<_>>();
            let single_names = tool_names_owned(&single)
                .into_iter()
                .filter(|name| {
                    name != "optional_toolset_status" && !base_names.contains(name.as_str())
                })
                .collect::<BTreeSet<_>>();
            let expected_names = all_names
                .difference(&single_names)
                .cloned()
                .collect::<BTreeSet<_>>();
            assert_eq!(
                tool_names_owned(&leave_one)
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                expected_names,
                "tool inventory omitted={omitted_name} read_only={read_only}"
            );
            assert!(
                owned_tool_names.iter().all(|name| !leave_one
                    .tools()
                    .iter()
                    .any(|tool| tool.name == name.as_str())),
                "all read-write-owned tools must stay absent when omitted={omitted_name} \
                 read_only={read_only}"
            );

            assert_eq!(
                production_status(&leave_one, &ProtocolVersion::V_2025_11_25).await,
                expected_status,
                "status omitted={omitted_name} read_only={read_only}"
            );

            let leave_resource_uris = leave_one
                .list_resources_wire(None)
                .expect("leave-one-out resource inventory")
                .resources
                .into_iter()
                .map(|resource| resource.uri.to_string())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                leave_resource_uris,
                all_resource_uris
                    .difference(&owned_resource_uris)
                    .cloned()
                    .collect(),
                "resource inventory omitted={omitted_name} read_only={read_only}"
            );

            let leave_template_uris = leave_one
                .list_resource_templates_wire(None)
                .expect("leave-one-out resource-template inventory")
                .resource_templates
                .into_iter()
                .map(|template| template.uri_template.to_string())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                leave_template_uris,
                all_template_uris
                    .difference(&owned_template_uris)
                    .cloned()
                    .collect(),
                "resource-template inventory omitted={omitted_name} read_only={read_only}"
            );

            let before = leave_one.runtime().client().http_metrics();
            for name in &owned_tool_names {
                let error = leave_one
                    .dispatch_tool(
                        CallToolRequestParams::new(name.clone())
                            .with_arguments(secret_arguments.clone()),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("omitted production tool");
                assert_eq!(
                    error,
                    ErrorData::method_not_found::<CallToolRequestMethod>(),
                    "stale tool omitted={omitted_name} read_only={read_only} name={name}"
                );
            }
            for uri in &owned_resource_uris {
                let error = leave_one
                    .read_resource_wire(
                        ReadResourceRequestParams::new(uri.clone()),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("omitted static production resource");
                assert_eq!(
                    error,
                    ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>(),
                    "stale resource omitted={omitted_name} read_only={read_only} uri={uri}"
                );
            }
            if omitted_registry.owns_resource_uri(FILE_RESOURCE_PROBE) {
                dynamic_resource_owner_cells = dynamic_resource_owner_cells.saturating_add(1);
                let error = leave_one
                    .read_resource_wire(
                        ReadResourceRequestParams::new(FILE_RESOURCE_PROBE),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("omitted dynamic production resource");
                assert_eq!(
                    error,
                    ErrorData::method_not_found::<rmcp::model::ReadResourceRequestMethod>(),
                    "stale dynamic resource omitted={omitted_name} read_only={read_only}"
                );
            }
            assert_eq!(
                leave_one.phase1_dispatch_polls(),
                0,
                "phase-one dispatch omitted={omitted_name} read_only={read_only}"
            );
            assert_eq!(
                leave_one.runtime().client().http_metrics(),
                before,
                "I/O omitted={omitted_name} read_only={read_only}"
            );
        }
    }

    assert_eq!(
        dynamic_resource_owner_cells, 2,
        "the files registry must own the dynamic resource probe in both access modes"
    );
}

#[tokio::test]
async fn production_optional_selection_preserves_phase_one_snapshots_and_status() {
    let snapshots = [
        (ApplicationProfile::Compact, false, COMPACT_CATALOG_SNAPSHOT),
        (
            ApplicationProfile::Compact,
            true,
            COMPACT_READ_ONLY_CATALOG_SNAPSHOT,
        ),
        (
            ApplicationProfile::Standard,
            false,
            STANDARD_CATALOG_SNAPSHOT,
        ),
        (
            ApplicationProfile::Standard,
            true,
            STANDARD_READ_ONLY_CATALOG_SNAPSHOT,
        ),
    ];
    for (profile, read_only, snapshot) in snapshots {
        let base =
            production_server(None, profile, read_only, true).expect("base production server");
        let selected = production_server(Some(PRODUCTION_SELECTOR), profile, read_only, true)
            .expect("all-selected production server");
        assert_eq!(production_catalog_snapshot(&base, read_only), snapshot);
        let base_status = base
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("base server status");
        let selected_status = selected
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("selected server status");
        assert_eq!(selected_status, base_status);
    }
}

#[test]
fn production_optional_registry_names_are_exact() {
    let metadata = production_optional_metadata();
    assert_eq!(
        metadata.iter().map(|entry| entry.name).collect::<Vec<_>>(),
        [
            "artifacts",
            "body-blocks",
            "chats",
            "members",
            "files",
            "schema",
            "views-write",
        ]
    );
    assert!(
        production_server(
            Some(PRODUCTION_SELECTOR),
            ApplicationProfile::Compact,
            true,
            false,
        )
        .is_ok()
    );
    assert!(
        production_server(
            Some("chats,files,members"),
            ApplicationProfile::Compact,
            false,
            false,
        )
        .is_ok()
    );
    for name in ["body-blocks", "schema", "views-write"] {
        assert!(
            production_server(Some(name), ApplicationProfile::Compact, true, false,).is_ok(),
            "{name}"
        );
    }
}

#[test]
fn production_catalogs_match_reviewed_aggregate_token_budget() {
    let actual = production_token_budget();
    let reviewed: Value =
        serde_json::from_str(PRODUCTION_TOKEN_BUDGET).expect("production token snapshot");
    assert_eq!(actual, reviewed);
    assert_eq!(production_budget_json(), PRODUCTION_TOKEN_BUDGET);
    assert!(
        actual["common_status_tokens"]
            .as_u64()
            .expect("common status tokens")
            <= actual["common_status_ceiling_tokens"]
                .as_u64()
                .expect("common status ceiling")
    );
    for budget in actual["registry_budgets"]
        .as_object()
        .expect("registry budgets")
        .values()
    {
        assert!(
            budget["domain_tokens"].as_u64().expect("domain tokens")
                <= budget["catalog_ceiling_tokens"]
                    .as_u64()
                    .expect("catalog ceiling")
        );
    }
    let aggregate_ceiling = actual["registry_ceiling_tokens"]
        .as_u64()
        .expect("aggregate registry ceiling")
        + actual["common_status_ceiling_tokens"]
            .as_u64()
            .expect("status ceiling");
    for composition in actual["compositions"]
        .as_object()
        .expect("production compositions")
        .values()
    {
        assert!(
            composition["selected_contribution_tokens"]
                .as_u64()
                .expect("selected contribution")
                <= aggregate_ceiling
        );
    }
}

#[test]
fn optional_catalog_construction_does_not_probe_backends() {
    assert!(
        AnyMcpServer::new_with_optional_registries(
            runtime("beta", ApplicationProfile::Compact, true, false),
            &LINKED,
        )
        .is_ok()
    );
    assert!(
        AnyMcpServer::new_with_optional_registries(
            runtime("alpha", ApplicationProfile::Compact, true, false),
            &LINKED,
        )
        .is_ok()
    );
}

#[derive(Debug)]
struct CollisionRegistry {
    name: &'static str,
    tool: Option<&'static str>,
    resource: Option<&'static str>,
    template: Option<&'static str>,
    scripted_scenarios: &'static [&'static str],
    headless_scenarios: &'static [&'static str],
}

impl OptionalToolsetRegistry for CollisionRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new(self.name)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        self.tool
            .map(|name| {
                workflow_tool::<EmptyInput, CompleteOutput>(
                    name,
                    "Test-only collision contract.",
                    ToolProfile::Read,
                )
                .map(|tool| vec![OptionalRegistryTool::read_http(tool)])
            })
            .unwrap_or_else(|| Ok(Vec::new()))
    }

    fn resources(&self) -> Vec<Resource> {
        self.resource
            .map(|uri| vec![Resource::new(uri, self.name)])
            .unwrap_or_default()
    }

    fn resource_templates(&self) -> Vec<ResourceTemplate> {
        self.template
            .map(|uri| vec![ResourceTemplate::new(uri, self.name)])
            .unwrap_or_default()
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        self.scripted_scenarios
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        self.headless_scenarios
    }

    fn catalog_token_ceiling(&self) -> usize {
        1
    }

    fn call_tool<'a>(
        &'a self,
        _request: CallToolRequestParams,
        _runtime: &'a RuntimeContext,
        _cursors: &'a crate::cursor::CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        _cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async { Err(ErrorData::method_not_found::<CallToolRequestMethod>()) })
    }

    fn owns_resource_uri(&self, uri: &str) -> bool {
        self.resource == Some(uri)
    }

    fn owns_resource_template(&self, uri_template: &str) -> bool {
        self.template == Some(uri_template)
    }
}

static PHASE_ONE_COLLISION: CollisionRegistry = CollisionRegistry {
    name: "phase-one-collision",
    tool: Some("object_get"),
    resource: None,
    template: None,
    scripted_scenarios: &["phase_one_collision_scripted"],
    headless_scenarios: &["phase_one_collision_headless"],
};
static ALPHA_COLLISION: CollisionRegistry = CollisionRegistry {
    name: "alpha-collision",
    tool: Some(ALPHA_READ),
    resource: Some(ALPHA_URI),
    template: Some(ALPHA_TEMPLATE),
    scripted_scenarios: &["alpha_collision_scripted"],
    headless_scenarios: &["alpha_collision_headless"],
};
static SCENARIO_COLLISION: CollisionRegistry = CollisionRegistry {
    name: "scenario-collision",
    tool: Some("scenario_collision_read"),
    resource: None,
    template: None,
    scripted_scenarios: &["optional_toolset_status_direct_contract"],
    headless_scenarios: &["scenario_collision_headless"],
};
static EMPTY_REGISTRY: CollisionRegistry = CollisionRegistry {
    name: "empty-registry",
    tool: None,
    resource: None,
    template: None,
    scripted_scenarios: &["empty_registry_scripted"],
    headless_scenarios: &["empty_registry_headless"],
};
static SCENARIO_LINKED: [&dyn OptionalToolsetRegistry; 1] = [&SCENARIO_COLLISION];
static EMPTY_LINKED: [&dyn OptionalToolsetRegistry; 1] = [&EMPTY_REGISTRY];

#[test]
fn composition_rejects_tool_resource_template_and_scenario_collisions() {
    static PHASE_LINKED: [&dyn OptionalToolsetRegistry; 1] = [&PHASE_ONE_COLLISION];
    let phase_selection = OptionalToolsetSelection::parse(
        Some("phase-one-collision".to_owned()),
        &[PHASE_ONE_COLLISION.metadata()],
    )
    .unwrap();
    assert!(
        compose_optional_catalog(
            &phase_selection,
            &PHASE_LINKED,
            false,
            &["object_get"],
            &[],
            &[],
        )
        .is_err()
    );

    static COLLIDING: [&dyn OptionalToolsetRegistry; 2] = [&ALPHA, &ALPHA_COLLISION];
    let both = OptionalToolsetSelection::parse(
        Some("alpha,alpha-collision".to_owned()),
        &[ALPHA.metadata(), ALPHA_COLLISION.metadata()],
    )
    .unwrap();
    assert!(
        compose_optional_catalog(&both, &COLLIDING, false, &[], &[], &[]).is_err(),
        "the first duplicate ownership category must fail closed"
    );

    for (resources, templates) in [
        (&[ALPHA_URI][..], &[][..]),
        (&[][..], &[ALPHA_TEMPLATE][..]),
    ] {
        let alpha =
            OptionalToolsetSelection::parse(Some("alpha".to_owned()), &[ALPHA.metadata()]).unwrap();
        static ALPHA_ONLY: [&dyn OptionalToolsetRegistry; 1] = [&ALPHA];
        assert!(
            compose_optional_catalog(&alpha, &ALPHA_ONLY, false, &[], resources, templates)
                .is_err()
        );
    }

    for (registry, linked) in [
        (
            &SCENARIO_COLLISION as &'static dyn OptionalToolsetRegistry,
            &SCENARIO_LINKED[..],
        ),
        (
            &EMPTY_REGISTRY as &'static dyn OptionalToolsetRegistry,
            &EMPTY_LINKED[..],
        ),
    ] {
        let selected = OptionalToolsetSelection::parse(
            Some(registry.metadata().name.to_owned()),
            &[registry.metadata()],
        )
        .unwrap();
        assert!(
            compose_optional_catalog(&selected, linked, false, &[], &[], &[]).is_err(),
            "scenario collisions and contribution-free descriptors fail closed"
        );
    }
}

#[test]
fn composed_catalogs_match_reviewed_token_measurements_and_ceilings() {
    let reviewed: OptionalTokenBudget =
        serde_json::from_str(OPTIONAL_TOKEN_BUDGET).expect("reviewed optional token budget");
    let actual = actual_token_budget();
    assert_eq!(reviewed.tokenizer, "o200k_base");
    assert_eq!(actual, reviewed);
    assert_eq!(budget_json(&actual), OPTIONAL_TOKEN_BUDGET);
    assert!(actual.common_status_tokens <= actual.common_status_ceiling_tokens);

    let tokenizer = o200k_base().unwrap();
    let base = token_count(
        &tokenizer,
        serde_json::to_value(
            AnyMcpServer::new(runtime_with_selection(
                OptionalToolsetSelection::default(),
                ApplicationProfile::Compact,
                false,
                false,
            ))
            .unwrap()
            .list_tools_wire(None)
            .unwrap(),
        )
        .unwrap(),
    );
    assert!(
        actual.alpha_composed_total_tokens
            <= base + actual.common_status_ceiling_tokens + actual.alpha_ceiling_tokens
    );
    assert!(
        actual.beta_composed_total_tokens
            <= base + actual.common_status_ceiling_tokens + actual.beta_ceiling_tokens
    );
    assert!(
        actual.gamma_composed_total_tokens
            <= base + actual.common_status_ceiling_tokens + actual.gamma_ceiling_tokens
    );
    assert!(
        actual.all_composed_total_tokens
            <= base
                + actual.common_status_ceiling_tokens
                + actual.alpha_ceiling_tokens
                + actual.beta_ceiling_tokens
                + actual.gamma_ceiling_tokens
    );
}

/// Serializes the exact advertised artifact catalog, including every schema.
fn artifact_catalog_snapshot() -> String {
    let server = production_server(Some("artifacts"), ApplicationProfile::Standard, false, true)
        .expect("linked artifacts production catalog");
    let tools = server
        .tools()
        .iter()
        .filter(|tool| ARTIFACT_TOOL_NAMES.contains(&tool.name.as_ref()))
        .cloned()
        .collect::<Vec<_>>();
    format!(
        "{}\n",
        serde_json::to_string_pretty(&canonical_json(json!({"tools": tools})))
            .expect("artifact catalog snapshot")
    )
}

#[test]
fn artifact_catalog_matches_its_reviewed_snapshot() {
    let snapshot = artifact_catalog_snapshot();
    assert_eq!(
        snapshot, ARTIFACT_CATALOG_SNAPSHOT,
        "regenerate tests/snapshots/artifact-catalog.snap and review the complete diff"
    );
    let value: Value = serde_json::from_str(&snapshot).expect("artifact catalog snapshot value");
    let descriptors = value["tools"].as_array().expect("artifact descriptors");
    let observed =
        ArtifactCatalogSnapshot::from_descriptors(descriptors).expect("exact artifact inventory");
    ArtifactCatalogSnapshot::reviewed()
        .expect("reviewed artifact catalog")
        .compare(&observed)
        .expect("reviewed artifact catalog matches the linked production contract");
}

#[test]
#[ignore = "manual updater; review every optional contract and token delta"]
fn write_optional_snapshots() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    fs::write(
        directory.join("optional-toolsets.snap"),
        optional_snapshot(),
    )
    .expect("write reviewed optional catalog snapshot");
    fs::write(
        directory.join("optional-toolsets-token-budget.json"),
        budget_json(&actual_token_budget()),
    )
    .expect("write reviewed optional token budget");
    fs::write(
        directory.join("production-optional-token-budget.json"),
        production_budget_json(),
    )
    .expect("write reviewed production optional token budget");
    fs::write(
        directory.join("artifact-catalog.snap"),
        artifact_catalog_snapshot(),
    )
    .expect("write reviewed artifact catalog snapshot");
}
