// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Complete production descriptor for the default-off `schema` toolset.
//!
//! The descriptor composes the independently reviewed space, type, property,
//! and tag workflow slices. It is immutable after startup, contributes exactly
//! nine tools in read-write mode and only `type_get` in read-only mode, and
//! uses the process runtime's existing `anytype-api` client for both HTTP and
//! bounded gRPC-backed type classification.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Weak},
};

use rmcp::model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::CursorStore,
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    runtime::RuntimeContext,
    schema::SchemaContractError,
    schema_property_toolset::{
        PROPERTY_CREATE, PROPERTY_UPDATE, SchemaPropertyHandlers, schema_property_tools,
    },
    schema_space_toolset::{SPACE_CREATE, SPACE_UPDATE, SchemaSpaceHandlers, schema_space_tools},
    schema_tag_toolset::{SchemaTagHandlers, TAG_CREATE, TAG_UPDATE, schema_tag_tools},
    schema_type_toolset::{
        SchemaTypeHandlers, TYPE_CREATE, TYPE_GET, TYPE_UPDATE, schema_type_tools,
    },
};

/// Exact production selector for the complete schema workflow surface.
pub const SCHEMA_TOOLSET_NAME: &str = "schema";
/// Reviewed incremental catalog-token ceiling for all nine schema tools.
pub const SCHEMA_CATALOG_TOKEN_CEILING: usize = 9_500;
/// Reviewed selected contribution ceiling including common optional status.
pub const SCHEMA_SELECTED_TOKEN_CEILING: usize = 10_000;

const SCRIPTED_SCENARIOS: &[&str] = &[
    "schema_space_direct",
    "schema_space_stdio",
    "schema_type_direct",
    "schema_type_stdio",
    "schema_property_direct",
    "schema_property_stdio",
    "schema_tag_direct",
    "schema_tag_stdio",
    "schema_registry_direct_contract",
    "schema_registry_stdio_contract",
];
const HEADLESS_SCENARIOS: &[&str] = &[
    "schema_space_headless",
    "schema_type_headless",
    "schema_property_headless",
    "schema_tag_headless",
    "schema_registry_real_headless",
];

#[derive(Debug)]
struct SchemaRegistry;

static SCHEMA_REGISTRY_IMPL: SchemaRegistry = SchemaRegistry;

/// Complete production descriptor for the `schema` registry.
pub static SCHEMA_REGISTRY: &dyn OptionalToolsetRegistry = &SCHEMA_REGISTRY_IMPL;

#[derive(Clone, Debug)]
struct SchemaHandlers {
    spaces: SchemaSpaceHandlers,
    types: SchemaTypeHandlers,
    properties: SchemaPropertyHandlers,
    tags: SchemaTagHandlers,
}

impl SchemaHandlers {
    fn new() -> Result<Self, SchemaContractError> {
        Ok(Self {
            spaces: SchemaSpaceHandlers::new()?,
            types: SchemaTypeHandlers::new()?,
            properties: SchemaPropertyHandlers::new()?,
            tags: SchemaTagHandlers::new()?,
        })
    }
}

type RuntimeSchemaHandlers = HashMap<usize, (Weak<()>, Arc<SchemaHandlers>)>;

static RUNTIME_HANDLERS: LazyLock<std::sync::Mutex<RuntimeSchemaHandlers>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn runtime_handlers(runtime: &RuntimeContext) -> Result<Arc<SchemaHandlers>, ErrorData> {
    let identity = runtime.identity();
    let key = Arc::as_ptr(identity) as usize;
    let mut handlers = match RUNTIME_HANDLERS.lock() {
        Ok(handlers) => handlers,
        Err(poisoned) => poisoned.into_inner(),
    };
    handlers.retain(|_, (owner, _)| owner.strong_count() != 0);
    if let Some((owner, existing)) = handlers.get(&key)
        && owner
            .upgrade()
            .is_some_and(|owner| Arc::ptr_eq(&owner, identity))
    {
        return Ok(existing.clone());
    }
    let created = Arc::new(
        SchemaHandlers::new()
            .map_err(|_| ErrorData::internal_error("Schema contracts unavailable.", None))?,
    );
    handlers.insert(key, (Arc::downgrade(identity), created.clone()));
    Ok(created)
}

impl OptionalToolsetRegistry for SchemaRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new(SCHEMA_TOOLSET_NAME)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        let mut tools = schema_space_tools()?;
        tools.extend(schema_type_tools()?);
        tools.extend(schema_property_tools()?);
        tools.extend(schema_tag_tools()?);
        Ok(tools)
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        SCRIPTED_SCENARIOS
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        HEADLESS_SCENARIOS
    }

    fn catalog_token_ceiling(&self) -> usize {
        SCHEMA_CATALOG_TOKEN_CEILING
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        _cursors: &'a CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            let handlers = runtime_handlers(runtime)?;
            match request.name.as_ref() {
                SPACE_CREATE | SPACE_UPDATE => {
                    Box::pin(handlers.spaces.call_tool(request, runtime, cancellation)).await
                }
                TYPE_GET | TYPE_CREATE | TYPE_UPDATE => {
                    Box::pin(handlers.types.call_tool(request, runtime, cancellation)).await
                }
                PROPERTY_CREATE | PROPERTY_UPDATE => {
                    Box::pin(
                        handlers
                            .properties
                            .call_tool(request, runtime, cancellation),
                    )
                    .await
                }
                TAG_CREATE | TAG_UPDATE => {
                    Box::pin(handlers.tags.call_tool(request, runtime, cancellation)).await
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, future::Future, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig};
    use rmcp::model::CallToolRequestParams;
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};

    use super::*;
    use crate::{
        config::ApplicationProfile,
        file_content::FILE_CONTENT_REGISTRY,
        member_toolset::MEMBERS_REGISTRY,
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        runtime::StartupStatus,
        server::AnyMcpServer,
    };

    const SCHEMA_NAMES: [&str; 9] = [
        PROPERTY_CREATE,
        PROPERTY_UPDATE,
        SPACE_CREATE,
        SPACE_UPDATE,
        TAG_CREATE,
        TAG_UPDATE,
        TYPE_CREATE,
        TYPE_GET,
        TYPE_UPDATE,
    ];
    const MUTATION_NAMES: [&str; 8] = [
        PROPERTY_CREATE,
        PROPERTY_UPDATE,
        SPACE_CREATE,
        SPACE_UPDATE,
        TAG_CREATE,
        TAG_UPDATE,
        TYPE_CREATE,
        TYPE_UPDATE,
    ];
    const TOKEN_BUDGET_SNAPSHOT: &str = include_str!("../tests/snapshots/schema-token-budget.json");

    fn client() -> AnytypeClient {
        AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("schema-registry-no-io".to_owned()),
            app_name: "schema-registry-no-io".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("schema registry client")
    }

    fn runtime(
        selected: Option<&str>,
        profile: ApplicationProfile,
        read_only: bool,
        grpc_available: bool,
    ) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            selected.map(str::to_owned),
            &production_optional_metadata(),
        )
        .expect("production optional selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client(),
            4,
            Duration::from_secs(2),
            StartupStatus {
                http_available: true,
                grpc_available,
            },
            profile,
            read_only,
            selection,
        )
    }

    fn server(
        selected: Option<&str>,
        profile: ApplicationProfile,
        read_only: bool,
    ) -> AnyMcpServer {
        AnyMcpServer::new(runtime(selected, profile, read_only, true))
            .expect("schema registry server")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    fn schema_names(server: &AnyMcpServer) -> Vec<String> {
        server
            .tools()
            .iter()
            .map(|tool| tool.name.to_string())
            .filter(|name| SCHEMA_NAMES.contains(&name.as_str()))
            .collect()
    }

    fn run_large_future<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("schema-registry-router".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("schema registry test runtime")
                    .block_on(test());
            })
            .expect("spawn schema registry test")
            .join()
            .expect("schema registry test thread");
    }

    #[test]
    fn production_inventory_access_projection_and_transport_are_exact() {
        assert_eq!(
            SCHEMA_REGISTRY.metadata(),
            OptionalToolsetMetadata::new(SCHEMA_TOOLSET_NAME)
        );
        assert_eq!(
            SCHEMA_REGISTRY.catalog_token_ceiling(),
            SCHEMA_CATALOG_TOKEN_CEILING
        );
        assert!(
            production_optional_metadata()
                .iter()
                .any(|metadata| metadata == &OptionalToolsetMetadata::new("schema"))
        );

        let read_write = server(Some("schema"), ApplicationProfile::Compact, false);
        assert_eq!(schema_names(&read_write), SCHEMA_NAMES);
        assert_eq!(
            read_write
                .tools()
                .iter()
                .filter(|tool| tool.name == "optional_toolset_status")
                .count(),
            1
        );
        assert!(
            read_write
                .tools()
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name)
        );

        let read_only = server(Some("schema"), ApplicationProfile::Compact, true);
        assert_eq!(schema_names(&read_only), [TYPE_GET]);
        assert_eq!(
            read_only
                .tools()
                .iter()
                .filter(|tool| tool.name == "optional_toolset_status")
                .count(),
            1
        );

        assert!(
            AnyMcpServer::new(runtime(
                Some("schema"),
                ApplicationProfile::Compact,
                false,
                false,
            ))
            .is_ok()
        );
    }

    #[tokio::test]
    async fn absent_and_read_only_calls_are_rejected_before_argument_decoding() {
        let absent = server(None, ApplicationProfile::Compact, false);
        for name in SCHEMA_NAMES {
            let error = absent
                .dispatch_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args(json!({"secret-unparsed": true}))),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("absent schema call is method-not-found");
            assert_eq!(error.code, rmcp::model::ErrorCode::METHOD_NOT_FOUND);
        }

        let read_only = server(Some("schema"), ApplicationProfile::Compact, true);
        for name in MUTATION_NAMES {
            let result = read_only
                .dispatch_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args(json!({"secret-unparsed": true}))),
                    &CancellationToken::new(),
                )
                .await
                .expect("stale mutation is a bounded tool error");
            assert_eq!(
                result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str),
                Some("validation")
            );
        }
    }

    #[test]
    fn production_router_dispatches_every_schema_name_and_preserves_contract_identity() {
        run_large_future(|| async {
            let compact = server(Some("schema"), ApplicationProfile::Compact, false);
            let standard = server(Some("schema"), ApplicationProfile::Standard, false);
            for name in SCHEMA_NAMES {
                let compact_tool = compact
                    .tools()
                    .iter()
                    .find(|tool| tool.name == name)
                    .expect("compact schema contract");
                let standard_tool = standard
                    .tools()
                    .iter()
                    .find(|tool| tool.name == name)
                    .expect("standard schema contract");
                assert_eq!(compact_tool, standard_tool);

                for protocol in [
                    rmcp::model::ProtocolVersion::V_2025_11_25,
                    rmcp::model::ProtocolVersion::V_2026_07_28,
                ] {
                    let error = Box::pin(compact.dispatch_tool_for_protocol(
                        CallToolRequestParams::new(name).with_arguments(args(json!({}))),
                        &protocol,
                        &CancellationToken::new(),
                    ))
                    .await
                    .expect_err("selected name reaches its strict decoder");
                    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
                }
            }
        });
    }

    #[tokio::test]
    async fn valid_schema_dispatch_fits_the_default_runtime_stack() {
        let router = server(Some("schema"), ApplicationProfile::Compact, false);
        let cases = [
            (
                SPACE_CREATE,
                json!({"name":"created","idempotency_key":"default-stack-create"}),
            ),
            (SPACE_UPDATE, json!({"space":"space","name":"updated"})),
            (TYPE_GET, json!({"space":"space","type":"type"})),
            (TYPE_CREATE, json!({"space":"space","name":"created"})),
            (
                TYPE_UPDATE,
                json!({"space":"space","type":"type","name":"updated"}),
            ),
            (
                PROPERTY_CREATE,
                json!({"space":"space","name":"created","format":"text"}),
            ),
            (
                PROPERTY_UPDATE,
                json!({"space":"space","property":"property","name":"updated"}),
            ),
            (
                TAG_CREATE,
                json!({"space":"space","property":"property","name":"created"}),
            ),
            (
                TAG_UPDATE,
                json!({
                    "space":"space",
                    "property":"property",
                    "tag_id":"tag",
                    "name":"updated"
                }),
            ),
        ];
        for (name, arguments) in cases {
            let result = Box::pin(router.dispatch_tool(
                CallToolRequestParams::new(name).with_arguments(args(arguments)),
                &CancellationToken::new(),
            ))
            .await
            .expect("valid schema dispatch returns one bounded tool result");
            assert_eq!(result.is_error, Some(true));
        }
    }

    #[tokio::test]
    async fn no_selection_is_byte_identical_with_or_without_the_linked_descriptor() {
        static WITHOUT_SCHEMA: [&dyn OptionalToolsetRegistry; 2] =
            [MEMBERS_REGISTRY, &FILE_CONTENT_REGISTRY];
        let production_runtime = runtime(None, ApplicationProfile::Compact, false, true);
        let without_runtime = production_runtime.clone();
        let production = AnyMcpServer::new(production_runtime).expect("production no-selection");
        let without = AnyMcpServer::new_with_optional_registries(without_runtime, &WITHOUT_SCHEMA)
            .expect("pre-link no-selection");
        assert_eq!(
            serde_json::to_vec(&crate::server::stable_list_tools_result(
                production.tools().to_vec()
            ))
            .expect("production catalog bytes"),
            serde_json::to_vec(&crate::server::stable_list_tools_result(
                without.tools().to_vec()
            ))
            .expect("pre-link catalog bytes")
        );
        let production_status = production
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("production server status");
        let without_status = without
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .expect("pre-link server status");
        assert_eq!(
            serde_json::to_vec(&production_status).expect("production status bytes"),
            serde_json::to_vec(&without_status).expect("pre-link status bytes")
        );
    }

    fn canonical_json(value: Value) -> Value {
        match value {
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
            scalar => scalar,
        }
    }

    fn canonical_compact(value: Value) -> String {
        serde_json::to_string(&canonical_json(value)).expect("canonical JSON")
    }

    fn token_count(tokenizer: &CoreBPE, value: Value) -> usize {
        tokenizer
            .encode_with_special_tokens(&canonical_compact(value))
            .len()
    }

    fn tools_value(server: &AnyMcpServer) -> Value {
        serde_json::to_value(crate::server::stable_list_tools_result(
            server.tools().to_vec(),
        ))
        .expect("tools/list value")
    }

    fn adversarial_text(seed: usize, length: usize) -> String {
        const ALPHABET: &[char] = &[
            '\0', '\u{001f}', '"', '\\', '\n', '\r', '\t', '界', '🚀', '𐍈', 'Ω', 'א', 'ق', 'क',
            'あ', '가', '\u{2028}', '\u{2029}',
        ];
        (0..length)
            .map(|position| ALPHABET[(seed + position) % ALPHABET.len()])
            .collect()
    }

    fn dense_safe_id(prefix: &str, seed: usize) -> String {
        const ALPHABET: &[u8] =
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~-";
        prefix
            .chars()
            .chain((prefix.chars().count()..256).map(|position| {
                char::from(ALPHABET[(seed.saturating_mul(17) + position) % ALPHABET.len()])
            }))
            .collect()
    }

    fn maximum_outputs() -> BTreeMap<&'static str, Value> {
        let id = "a".repeat(256);
        let type_name = "界".repeat(512);
        let tag = json!({
            "id":id,
            "name":"🦀".repeat(512),
            "key":"k".repeat(256),
            "color":"purple"
        });
        let property = json!({
            "id":dense_safe_id("property", 91),
            "name":adversarial_text(47, 512),
            "key":adversarial_text(83, 256),
            "format":"multi_select"
        });
        let space = json!({
            "space":{
                "id":id,
                "name":adversarial_text(109, 512),
                "description":adversarial_text(127, 4096)
            }
        });
        let type_output = json!({
            "type":{
                "id":id,
                "key":"a".repeat(256),
                "name":type_name,
                "plural_name":"語".repeat(512),
                "layout":"collection",
                "archived":true
            }
        });
        let tags = (0..20)
            .map(|index| {
                json!({
                    "id":dense_safe_id(&format!("t{index:02}"), index),
                    "name":adversarial_text(index, 512),
                    "key":adversarial_text(index + 31, 256),
                    "color":"lime"
                })
            })
            .collect::<Vec<_>>();
        BTreeMap::from([
            (SPACE_CREATE, space.clone()),
            (SPACE_UPDATE, space),
            (TYPE_GET, type_output.clone()),
            (TYPE_CREATE, type_output.clone()),
            (TYPE_UPDATE, type_output),
            (PROPERTY_CREATE, json!({"property":property,"tags":tags})),
            (PROPERTY_UPDATE, json!({"property":property})),
            (TAG_CREATE, json!({"tag":tag})),
            (TAG_UPDATE, json!({"tag":tag})),
        ])
    }

    fn result_frame(output: Value) -> Value {
        let text = serde_json::to_string(&output).expect("compact result text");
        json!({
            "content":[{"type":"text","text":text}],
            "structuredContent":output,
            "isError":false
        })
    }

    fn schema_token_budget() -> Value {
        let tokenizer = o200k_base().expect("o200k tokenizer");
        let base = server(None, ApplicationProfile::Compact, false);
        let compact = server(Some("schema"), ApplicationProfile::Compact, false);
        let compact_read_only = server(Some("schema"), ApplicationProfile::Compact, true);
        let standard = server(Some("schema"), ApplicationProfile::Standard, false);
        let standard_read_only = server(Some("schema"), ApplicationProfile::Standard, true);
        let mixed = server(
            Some("files,members,schema"),
            ApplicationProfile::Compact,
            false,
        );
        let base_value = tools_value(&base);
        let base_json = canonical_compact(base_value.clone());
        let base_tokens = token_count(&tokenizer, base_value);
        let per_tool = compact
            .tools()
            .iter()
            .filter(|tool| SCHEMA_NAMES.contains(&tool.name.as_ref()))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(
                        &tokenizer,
                        serde_json::to_value(tool).expect("schema tool value"),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let domain_tokens = per_tool.values().sum::<usize>();
        let compact_tokens = token_count(&tokenizer, tools_value(&compact));
        let representative_results = maximum_outputs()
            .into_iter()
            .map(|(name, output)| {
                let frame = result_frame(output);
                let encoded = canonical_compact(frame.clone());
                let sha256 = Sha256::digest(encoded.as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                (
                    name,
                    json!({
                        "bytes":encoded.len(),
                        "tokens":token_count(&tokenizer, frame),
                        "sha256":sha256
                    }),
                )
            })
            .collect::<BTreeMap<_, _>>();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog_sha256":Sha256::digest(base_json.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "base_catalog_tokens":base_tokens,
            "selected":["schema"],
            "schema_domain_ceiling_tokens":SCHEMA_CATALOG_TOKEN_CEILING,
            "schema_domain_tokens":domain_tokens,
            "schema_selected_ceiling_tokens":SCHEMA_SELECTED_TOKEN_CEILING,
            "schema_selected_contribution_tokens":compact_tokens.saturating_sub(base_tokens),
            "per_tool_tokens":per_tool,
            "compact_composed_total_tokens":compact_tokens,
            "compact_read_only_total_tokens":token_count(&tokenizer, tools_value(&compact_read_only)),
            "standard_composed_total_tokens":token_count(&tokenizer, tools_value(&standard)),
            "standard_read_only_total_tokens":token_count(&tokenizer, tools_value(&standard_read_only)),
            "files_members_schema_compact_total_tokens":token_count(&tokenizer, tools_value(&mixed)),
            "representative_max_results":representative_results
        })
    }

    #[test]
    fn production_schema_token_budget_matches_reviewed_snapshot() {
        let actual = schema_token_budget();
        let reviewed: Value =
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("schema token snapshot JSON");
        assert_eq!(actual, reviewed);
        assert!(
            actual["schema_domain_tokens"]
                .as_u64()
                .expect("domain tokens")
                <= SCHEMA_CATALOG_TOKEN_CEILING as u64
        );
        assert!(
            actual["schema_selected_contribution_tokens"]
                .as_u64()
                .expect("selected tokens")
                <= SCHEMA_SELECTED_TOKEN_CEILING as u64
        );
    }

    #[test]
    #[ignore = "prints the reviewed snapshot for explicit diff review"]
    fn print_production_schema_token_budget_snapshot() {
        println!(
            "{}",
            serde_json::to_string_pretty(&schema_token_budget()).expect("schema token budget JSON")
        );
    }
}
