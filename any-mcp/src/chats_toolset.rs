// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Complete production descriptor for the default-off `chats` toolset.
//!
//! The immutable descriptor composes four bounded reads with verified add and
//! delete mutations. It uses only the process runtime's authenticated HTTP
//! client, contributes no resources or templates, and retains only reads in a
//! read-only process.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Weak},
};

use rmcp::model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData};
use tokio_util::sync::CancellationToken;

use crate::{
    chat_add_toolset::{CHAT_MESSAGE_ADD, ChatMessageAddHandlers, chat_add_tools},
    chat_delete_toolset::{CHAT_MESSAGE_DELETE, ChatMessageDeleteHandlers, chat_delete_tools},
    chat_read_toolset::{
        CHAT_LIST, CHAT_MESSAGE_GET, CHAT_MESSAGE_LIST, CHAT_MESSAGE_SEARCH, chat_read_registry,
    },
    cursor::CursorStore,
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    runtime::RuntimeContext,
    schema::SchemaContractError,
};

/// Exact production selector for the complete chat workflow surface.
pub const CHATS_TOOLSET_NAME: &str = "chats";
/// Reviewed read-write catalog contribution ceiling for all six chat tools.
pub const CHATS_READ_WRITE_TOKEN_CEILING: usize = 8_500;
/// Reviewed read-only catalog contribution ceiling for the four chat reads.
pub const CHATS_READ_ONLY_TOKEN_CEILING: usize = 6_500;
/// Reviewed ceiling for every individual chat tool contract.
pub const CHATS_PER_TOOL_TOKEN_CEILING: usize = 2_000;

const SCRIPTED_SCENARIOS: &[&str] = &[
    "chats_read_direct",
    "chats_read_stdio",
    "chat_add_direct",
    "chat_add_stdio",
    "chat_delete_direct",
    "chat_delete_stdio",
    "chats_registry_direct_contract",
    "chats_registry_stable_stdio_contract",
    "chats_registry_preview_stdio_contract",
];
const HEADLESS_SCENARIOS: &[&str] = &[
    "chats_read_headless",
    "chat_add_headless",
    "chat_delete_headless",
    "chats_registry_real_direct",
    "chats_registry_real_stable_stdio",
    "chats_registry_real_preview_stdio",
];

#[derive(Debug)]
struct ChatsRegistry;

static CHATS_REGISTRY_IMPL: ChatsRegistry = ChatsRegistry;

/// Complete production descriptor for the `chats` registry.
pub static CHATS_REGISTRY: &dyn OptionalToolsetRegistry = &CHATS_REGISTRY_IMPL;

#[derive(Debug)]
struct ChatMutationHandlers {
    add: ChatMessageAddHandlers,
    delete: ChatMessageDeleteHandlers,
}

impl ChatMutationHandlers {
    fn new() -> Result<Self, SchemaContractError> {
        Ok(Self {
            add: ChatMessageAddHandlers::new()?,
            delete: ChatMessageDeleteHandlers::new()?,
        })
    }
}

type RuntimeHandlers = HashMap<usize, (Weak<()>, Arc<ChatMutationHandlers>)>;

static RUNTIME_HANDLERS: LazyLock<std::sync::Mutex<RuntimeHandlers>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

fn runtime_handlers(runtime: &RuntimeContext) -> Result<Arc<ChatMutationHandlers>, ErrorData> {
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
        ChatMutationHandlers::new()
            .map_err(|_| ErrorData::internal_error("Chat contracts unavailable.", None))?,
    );
    handlers.insert(key, (Arc::downgrade(identity), created.clone()));
    Ok(created)
}

impl OptionalToolsetRegistry for ChatsRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new(CHATS_TOOLSET_NAME, false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        let mut tools = chat_read_registry().tools()?;
        tools.extend(chat_add_tools()?);
        tools.extend(chat_delete_tools()?);
        Ok(tools)
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        SCRIPTED_SCENARIOS
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        HEADLESS_SCENARIOS
    }

    fn catalog_token_ceiling(&self) -> usize {
        CHATS_READ_WRITE_TOKEN_CEILING
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        cursors: &'a CursorStore,
        protocol_version: &'a rmcp::model::ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            match request.name.as_ref() {
                CHAT_LIST | CHAT_MESSAGE_LIST | CHAT_MESSAGE_GET | CHAT_MESSAGE_SEARCH => {
                    chat_read_registry()
                        .call_tool(request, runtime, cursors, protocol_version, cancellation)
                        .await
                }
                CHAT_MESSAGE_ADD => {
                    Box::pin(runtime_handlers(runtime)?.add.call_tool(
                        request,
                        runtime,
                        cancellation,
                    ))
                    .await
                }
                CHAT_MESSAGE_DELETE => {
                    Box::pin(runtime_handlers(runtime)?.delete.call_tool(
                        request,
                        runtime,
                        cancellation,
                    ))
                    .await
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, time::Duration};

    use anytype::prelude::{AnytypeClient, ClientConfig};
    use rmcp::model::{CallToolRequestParams, ListToolsResult};
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
        schema_toolset::SCHEMA_REGISTRY,
        server::AnyMcpServer,
    };

    const CHAT_NAMES: [&str; 6] = [
        CHAT_LIST,
        CHAT_MESSAGE_ADD,
        CHAT_MESSAGE_DELETE,
        CHAT_MESSAGE_GET,
        CHAT_MESSAGE_LIST,
        CHAT_MESSAGE_SEARCH,
    ];
    const READ_NAMES: [&str; 4] = [
        CHAT_LIST,
        CHAT_MESSAGE_GET,
        CHAT_MESSAGE_LIST,
        CHAT_MESSAGE_SEARCH,
    ];
    const MUTATION_NAMES: [&str; 2] = [CHAT_MESSAGE_ADD, CHAT_MESSAGE_DELETE];
    const DEFERRED_NAMES: [&str; 12] = [
        "chat_create",
        "chat_delete",
        "chat_message_edit",
        "chat_message_stream",
        "chat_reaction_add",
        "chat_reaction_remove",
        "chat_attachment_upload",
        "chat_attachment_download",
        "chat_pin",
        "chat_unpin",
        "chat_read_state_update",
        "chat_subscribe",
    ];
    const TOKEN_BUDGET_SNAPSHOT: &str = include_str!("../tests/snapshots/chats-token-budget.json");
    const READ_SNAPSHOT: &str = include_str!("../tests/snapshots/chats-read-token-budget.json");
    const ADD_SNAPSHOT: &str = include_str!("../tests/snapshots/chats-add-token-budget.json");
    const DELETE_SNAPSHOT: &str = include_str!("../tests/snapshots/chats-delete-token-budget.json");

    fn client() -> AnytypeClient {
        AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("chats-registry-no-io".to_owned()),
            app_name: "chats-registry-no-io".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("chats registry client")
    }

    fn runtime(
        selected: Option<&str>,
        profile: ApplicationProfile,
        read_only: bool,
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
                grpc_available: profile.requires_grpc(read_only),
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
        AnyMcpServer::new(runtime(selected, profile, read_only)).expect("chats registry server")
    }

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().cloned().expect("object arguments")
    }

    fn chat_names(server: &AnyMcpServer) -> Vec<String> {
        server
            .tools()
            .iter()
            .map(|tool| tool.name.to_string())
            .filter(|name| CHAT_NAMES.contains(&name.as_str()))
            .collect()
    }

    #[test]
    fn production_inventory_access_transport_and_ownership_are_exact() {
        assert_eq!(
            CHATS_REGISTRY.metadata(),
            OptionalToolsetMetadata::new(CHATS_TOOLSET_NAME, false)
        );
        assert_eq!(
            CHATS_REGISTRY.catalog_token_ceiling(),
            CHATS_READ_WRITE_TOKEN_CEILING
        );
        assert!(production_optional_metadata().contains(&CHATS_REGISTRY.metadata()));

        let read_write = server(Some("chats"), ApplicationProfile::Compact, false);
        assert_eq!(chat_names(&read_write), CHAT_NAMES);
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
        assert!(
            DEFERRED_NAMES
                .iter()
                .all(|name| read_write.tools().iter().all(|tool| tool.name != *name))
        );

        let read_only = server(Some("chats"), ApplicationProfile::Compact, true);
        assert_eq!(chat_names(&read_only), READ_NAMES);
        assert!(CHATS_REGISTRY.resources().is_empty());
        assert!(CHATS_REGISTRY.resource_templates().is_empty());
        assert!(!CHATS_REGISTRY.metadata().requires_grpc);
    }

    #[tokio::test]
    async fn absent_and_read_only_mutations_fail_before_decode_or_http() {
        let absent = server(None, ApplicationProfile::Compact, false);
        let before = absent.runtime().client().http_metrics();
        for name in CHAT_NAMES {
            let error = absent
                .dispatch_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args(json!({"secret-unparsed":true}))),
                    &CancellationToken::new(),
                )
                .await
                .expect_err("absent chat call is method-not-found");
            assert_eq!(error.code, rmcp::model::ErrorCode::METHOD_NOT_FOUND);
        }
        assert_eq!(absent.runtime().client().http_metrics(), before);

        let read_only = server(Some("chats"), ApplicationProfile::Compact, true);
        let before = read_only.runtime().client().http_metrics();
        for name in MUTATION_NAMES {
            let result = read_only
                .dispatch_tool(
                    CallToolRequestParams::new(name)
                        .with_arguments(args(json!({"secret-unparsed":true}))),
                    &CancellationToken::new(),
                )
                .await
                .expect("stale read-only mutation is bounded");
            assert_eq!(
                result
                    .structured_content
                    .as_ref()
                    .and_then(|value| value["code"].as_str()),
                Some("validation")
            );
        }
        assert_eq!(read_only.runtime().client().http_metrics(), before);
    }

    #[tokio::test]
    async fn selected_router_preserves_stable_preview_contract_and_strict_decoding() {
        let compact = server(Some("chats"), ApplicationProfile::Compact, false);
        let standard = server(Some("chats"), ApplicationProfile::Standard, false);
        for name in CHAT_NAMES {
            let compact_tool = compact
                .tools()
                .iter()
                .find(|tool| tool.name == name)
                .unwrap();
            let standard_tool = standard
                .tools()
                .iter()
                .find(|tool| tool.name == name)
                .unwrap();
            assert_eq!(compact_tool, standard_tool);
            for protocol in [
                rmcp::model::ProtocolVersion::V_2025_11_25,
                rmcp::model::ProtocolVersion::V_2026_07_28,
            ] {
                let error = compact
                    .dispatch_tool_for_protocol(
                        CallToolRequestParams::new(name)
                            .with_arguments(args(json!({"unknown":true}))),
                        &protocol,
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("strict chat decoder rejects unknown input");
                assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            }
        }
    }

    #[tokio::test]
    async fn valid_chat_dispatch_fits_the_default_test_stack() {
        let router = server(Some("chats"), ApplicationProfile::Compact, false);
        let before = router.runtime().client().http_metrics();
        let cases = [
            (CHAT_LIST, json!({"space":"space"})),
            (CHAT_MESSAGE_LIST, json!({"space":"space","chat_id":"chat"})),
            (
                CHAT_MESSAGE_GET,
                json!({"space":"space","chat_id":"chat","message_id":"message"}),
            ),
            (
                CHAT_MESSAGE_SEARCH,
                json!({"space":"space","chat_id":"chat","query":"query"}),
            ),
            (
                CHAT_MESSAGE_ADD,
                json!({
                    "space":"space",
                    "chat_id":"chat",
                    "text":"text",
                    "idempotency_key":"default-stack-add"
                }),
            ),
            (
                CHAT_MESSAGE_DELETE,
                json!({
                    "space":"space",
                    "chat_id":"chat",
                    "message_id":"message",
                    "expected_modified_at":"2026-07-22T12:00:00.001Z",
                    "confirm_delete":"delete_message"
                }),
            ),
        ];
        for (name, arguments) in cases {
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let result = router
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(args(arguments)),
                    &cancellation,
                )
                .await
                .expect("valid pre-cancelled chat dispatch is a bounded tool result");
            assert_eq!(result.is_error, Some(true));
        }
        assert_eq!(router.runtime().client().http_metrics(), before);
    }

    #[tokio::test]
    async fn linking_is_inert_without_a_selector() {
        static WITHOUT_CHATS: [&dyn OptionalToolsetRegistry; 3] =
            [MEMBERS_REGISTRY, &FILE_CONTENT_REGISTRY, SCHEMA_REGISTRY];
        let production_runtime = runtime(None, ApplicationProfile::Compact, false);
        let production = AnyMcpServer::new(production_runtime.clone()).unwrap();
        let before_link =
            AnyMcpServer::new_with_optional_registries(production_runtime, &WITHOUT_CHATS).unwrap();
        assert_eq!(
            serde_json::to_vec(&ListToolsResult::with_all_items(
                production.tools().to_vec()
            ))
            .unwrap(),
            serde_json::to_vec(&ListToolsResult::with_all_items(
                before_link.tools().to_vec()
            ))
            .unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&production.list_resources_wire(None).unwrap()).unwrap(),
            serde_json::to_vec(&before_link.list_resources_wire(None).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_vec(&production.list_resource_templates_wire(None).unwrap()).unwrap(),
            serde_json::to_vec(&before_link.list_resource_templates_wire(None).unwrap()).unwrap()
        );
        let production_status = production
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let before_status = before_link
            .dispatch_tool(
                CallToolRequestParams::new("server_status"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_vec(&production_status).unwrap(),
            serde_json::to_vec(&before_status).unwrap()
        );
    }

    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(values) => Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            scalar => scalar,
        }
    }

    fn compact(value: Value) -> String {
        serde_json::to_string(&canonical(value)).expect("canonical JSON")
    }

    fn tokens(tokenizer: &CoreBPE, value: Value) -> usize {
        tokenizer.encode_with_special_tokens(&compact(value)).len()
    }

    fn tools_value(server: &AnyMcpServer) -> Value {
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec())).unwrap()
    }

    fn hash(value: &Value) -> String {
        Sha256::digest(compact(value.clone()).as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn token_snapshot() -> Value {
        let tokenizer = o200k_base().expect("o200k tokenizer");
        let base = server(None, ApplicationProfile::Compact, false);
        let read_write = server(Some("chats"), ApplicationProfile::Compact, false);
        let read_only = server(Some("chats"), ApplicationProfile::Compact, true);
        let standard = server(Some("chats"), ApplicationProfile::Standard, false);
        let standard_read_only = server(Some("chats"), ApplicationProfile::Standard, true);
        let mixed = server(
            Some("chats,files,members"),
            ApplicationProfile::Compact,
            false,
        );
        let base_value = tools_value(&base);
        let read_write_value = tools_value(&read_write);
        let read_only_value = tools_value(&read_only);
        let per_tool = read_write
            .tools()
            .iter()
            .filter(|tool| CHAT_NAMES.contains(&tool.name.as_ref()))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    tokens(&tokenizer, serde_json::to_value(tool).unwrap()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let read_component: Value = serde_json::from_str(READ_SNAPSHOT).unwrap();
        let add_component: Value = serde_json::from_str(ADD_SNAPSHOT).unwrap();
        let delete_component: Value = serde_json::from_str(DELETE_SNAPSHOT).unwrap();
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "selected":["chats"],
            "base_catalog_sha256":hash(&base_value),
            "selected_catalog_sha256":hash(&read_write_value),
            "read_only_catalog_sha256":hash(&read_only_value),
            "per_tool_tokens":per_tool,
            "read_write_domain_tokens":per_tool.values().sum::<usize>(),
            "read_only_domain_tokens":READ_NAMES.iter().map(|name| per_tool[*name]).sum::<usize>(),
            "read_write_domain_ceiling_tokens":CHATS_READ_WRITE_TOKEN_CEILING,
            "read_only_domain_ceiling_tokens":CHATS_READ_ONLY_TOKEN_CEILING,
            "per_tool_ceiling_tokens":CHATS_PER_TOOL_TOKEN_CEILING,
            "compact_composed_total_tokens":tokens(&tokenizer, read_write_value),
            "compact_read_only_total_tokens":tokens(&tokenizer, read_only_value),
            "standard_composed_total_tokens":tokens(&tokenizer, tools_value(&standard)),
            "standard_read_only_total_tokens":tokens(&tokenizer, tools_value(&standard_read_only)),
            "chats_files_members_compact_total_tokens":tokens(&tokenizer, tools_value(&mixed)),
            "reviewed_adversarial_results":{
                "reads":read_component["maximum_results"].clone(),
                "read_boundaries":read_component["typed_adversarial_boundaries"].clone(),
                "add":add_component["maximum_result"].clone(),
                "delete":delete_component["maximum_result"].clone(),
            }
        })
    }

    #[test]
    fn production_chats_token_budget_matches_reviewed_snapshot() {
        let actual = token_snapshot();
        let reviewed: Value = serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).unwrap();
        assert_eq!(actual, reviewed);
        assert!(
            actual["read_write_domain_tokens"].as_u64().unwrap()
                <= CHATS_READ_WRITE_TOKEN_CEILING as u64
        );
        assert!(
            actual["read_only_domain_tokens"].as_u64().unwrap()
                <= CHATS_READ_ONLY_TOKEN_CEILING as u64
        );
        assert!(
            actual["per_tool_tokens"]
                .as_object()
                .unwrap()
                .values()
                .all(|value| value.as_u64().unwrap() <= CHATS_PER_TOOL_TOKEN_CEILING as u64)
        );
    }

    #[test]
    #[ignore = "prints the reviewed snapshot for explicit diff review"]
    fn print_production_chats_token_budget_snapshot() {
        println!(
            "{}",
            serde_json::to_string_pretty(&token_snapshot()).unwrap()
        );
    }
}
