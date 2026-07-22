// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Test-only complete registries proving the common optional composition seam.

use std::{
    collections::BTreeMap,
    fs,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
use rmcp::{
    model::{
        CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData, ListToolsResult,
        ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
        ResourceTemplate, TaskMetadata,
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
        OptionalToolsetSelection,
    },
    protocol::{ToolProfile, workflow_tool},
    runtime::StartupStatus,
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
        OptionalToolsetMetadata::new("alpha", false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read(workflow_tool::<EmptyInput, CompleteOutput>(
                ALPHA_READ,
                "Read the bounded test-only alpha state.",
                ToolProfile::Read,
            )?),
            OptionalRegistryTool::mutation(workflow_tool::<EmptyInput, CompleteOutput>(
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
        OptionalToolsetMetadata::new("beta", true)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![OptionalRegistryTool::read(workflow_tool::<
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
        OptionalToolsetMetadata::new("gamma", false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![OptionalRegistryTool::mutation(workflow_tool::<
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

fn server(value: &str, profile: ApplicationProfile, read_only: bool) -> AnyMcpServer {
    AnyMcpServer::new_with_optional_registries(runtime(value, profile, read_only, true), &LINKED)
        .expect("composed test-only catalog")
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

fn tools_list_tokens(tokenizer: &CoreBPE, value: &str, read_only: bool) -> usize {
    let server = server(value, ApplicationProfile::Compact, read_only);
    token_count(
        tokenizer,
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec()))
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
            serde_json::to_value(ListToolsResult::with_all_items(vec![status]))
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

    let result = selected
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ),
            &CancellationToken::new(),
        )
        .await
        .expect("selected alpha dispatch");
    assert_eq!(result.structured_content, Some(json!({"complete": true})));
    assert_eq!(ALPHA_CALLS.load(Ordering::SeqCst), 1);

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
    let response = crate::stdio::dispatch_modern(
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

    let task_error = disabled
        .dispatch_tool(
            CallToolRequestParams::new(ALPHA_READ).with_task(TaskMetadata::new()),
            &CancellationToken::new(),
        )
        .await
        .expect_err("disabled task-augmented name is method-not-found");
    assert_eq!(
        task_error,
        ErrorData::method_not_found::<CallToolRequestMethod>()
    );

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
    let preview = crate::stdio::dispatch_modern(
        &server,
        json!(1),
        "tools/list",
        Map::new(),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(preview["result"]["tools"], stable["tools"]);

    let stable_resources = serde_json::to_value(server.list_resources_wire(None).unwrap()).unwrap();
    let preview_resources = crate::stdio::dispatch_modern(
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
    let preview_templates = crate::stdio::dispatch_modern(
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
}

#[test]
fn optional_transport_requirements_union_with_phase_one() {
    assert!(
        AnyMcpServer::new_with_optional_registries(
            runtime("beta", ApplicationProfile::Compact, true, false),
            &LINKED,
        )
        .is_err()
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
        OptionalToolsetMetadata::new(self.name, false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        self.tool
            .map(|name| {
                workflow_tool::<EmptyInput, CompleteOutput>(
                    name,
                    "Test-only collision contract.",
                    ToolProfile::Read,
                )
                .map(|tool| vec![OptionalRegistryTool::read(tool)])
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
}
