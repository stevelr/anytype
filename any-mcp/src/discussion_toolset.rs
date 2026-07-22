// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Default-off, read-only discovery of discussions attached to exact objects.

use std::time::{Duration, Instant};

use anytype::attached_discussions::{
    AttachedDiscussion, MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT,
};
use rmcp::{
    model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData},
    schemars::{JsonSchema, Schema, SchemaGenerator, json_schema},
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::CursorStore,
    discovery::DiscoveryReference,
    domain::{DomainValueError, EntityId},
    error::ToolError,
    handler_support::{HandlerError, HandlerOperationError, execute_prepared_handler_until},
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    server::decode_arguments,
};

const OBJECT_DISCUSSION_GET: &str = "object_discussion_get";
const DISCUSSIONS_CATALOG_TOKEN_CEILING: usize = 1_500;
const RESULT_BYTE_CEILING: usize = 2 * 1024;

/// Input for exact attached-discussion discovery.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObjectDiscussionGetInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Exact parent page identifier; names are never resolved here.
    pub object_id: EntityId,
}

/// Exact closed state of a parent's attached discussion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectDiscussionGetOutput {
    /// The verified parent has no attached discussion.
    Absent {
        /// Stable resolved space identifier.
        space_id: EntityId,
        /// Stable parent object identifier.
        object_id: EntityId,
    },
    /// The verified parent has one identity-bound derived discussion.
    Attached {
        /// Stable resolved space identifier.
        space_id: EntityId,
        /// Stable parent object identifier.
        object_id: EntityId,
        /// Stable discussion identifier accepted unchanged by chat-message tools.
        discussion_id: EntityId,
    },
}

impl JsonSchema for ObjectDiscussionGetOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ObjectDiscussionGetOutput".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        let entity_id = json_schema!({
            "type": "string",
            "description": "A validated identifier for an Anytype entity.",
            "minLength": 1,
            "maxLength": crate::domain::MAX_ENTITY_ID_CHARS,
            "pattern": "^(?!\\.{1,2}$)[A-Za-z0-9._~-]+$"
        });
        json_schema!({
            "type": "object",
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "state": {"const": "absent"},
                        "space_id": entity_id.clone(),
                        "object_id": entity_id.clone()
                    },
                    "required": ["state", "space_id", "object_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "state": {"const": "attached"},
                        "space_id": entity_id.clone(),
                        "object_id": entity_id.clone(),
                        "discussion_id": entity_id
                    },
                    "required": ["state", "space_id", "object_id", "discussion_id"]
                }
            ]
        })
    }
}

/// Constructs the exact read-only `object_discussion_get` contract.
pub fn object_discussion_get_tool()
-> Result<WorkflowTool<ObjectDiscussionGetOutput>, SchemaContractError> {
    workflow_tool::<ObjectDiscussionGetInput, ObjectDiscussionGetOutput>(
        OBJECT_DISCUSSION_GET,
        "Resolve the discussion attached to one exact Anytype page. Returns normal absent state or a stable discussion ID; it does not read comments.",
        ToolProfile::Read,
    )
}

#[derive(Debug)]
struct DiscussionsRegistry;

static DISCUSSIONS_REGISTRY_IMPL: DiscussionsRegistry = DiscussionsRegistry;

/// Complete production-candidate descriptor for the default-off `discussions` registry.
pub static DISCUSSIONS_REGISTRY: &dyn OptionalToolsetRegistry = &DISCUSSIONS_REGISTRY_IMPL;

/// Returns the complete production-unlinked `discussions` candidate registry.
#[must_use]
pub fn discussions_registry() -> &'static dyn OptionalToolsetRegistry {
    DISCUSSIONS_REGISTRY
}

impl OptionalToolsetRegistry for DiscussionsRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("discussions", true)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![OptionalRegistryTool::read(
            object_discussion_get_tool()?,
        )])
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["discussions_direct", "discussions_stdio"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["discussions_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        DISCUSSIONS_CATALOG_TOKEN_CEILING
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
            match request.name.as_ref() {
                OBJECT_DISCUSSION_GET => {
                    let input = decode_arguments::<ObjectDiscussionGetInput>(request.arguments)?;
                    Ok(object_discussion_get(runtime, input, cancellation).await)
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }
}

/// Runs the reviewed discussions slice in a test-only stdio child.
///
/// This entrypoint exists only behind the non-default `acceptance-harness`
/// feature. It does not link `discussions` into the shipped registry inventory.
#[cfg(feature = "acceptance-harness")]
pub async fn serve_acceptance_stdio_from_env() -> Result<(), Box<dyn std::error::Error>> {
    use crate::config::{ProtocolMode, RuntimeConfig};

    let mut arguments = std::env::args_os().skip(1);
    let metrics_path = arguments
        .next()
        .map(std::path::PathBuf::from)
        .ok_or("acceptance harness requires a metrics path")?;
    let mode = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or("acceptance harness requires one exact mode")?;
    if arguments.next().is_some() {
        return Err("acceptance harness rejects extra arguments".into());
    }
    let protocol = match mode.as_str() {
        "stable" => ProtocolMode::Stable,
        "preview" => ProtocolMode::Experimental20260728,
        _ => return Err("acceptance harness mode is invalid".into()),
    };
    let chats = crate::chat_read_toolset::chat_read_registry();
    let metadata = [DISCUSSIONS_REGISTRY.metadata(), chats.metadata()];
    let mut config = RuntimeConfig::from_env_with_optional_metadata(&metadata)?;
    if !config.optional_toolsets.is_empty() {
        return Err("acceptance harness does not accept a registry selector".into());
    }
    config.optional_toolsets = crate::optional_toolsets::OptionalToolsetSelection::parse(
        Some("chats,discussions".to_owned()),
        &metadata,
    )?;
    config.read_only = true;
    config.protocol_mode = protocol;
    let runtime = RuntimeContext::start(&config).await?;
    let client = runtime.client().clone();
    let http_before = client.http_metrics();
    let discussions_before = client.attached_discussion_metrics();
    let registries: &'static [&'static dyn OptionalToolsetRegistry] =
        Box::leak(vec![DISCUSSIONS_REGISTRY, chats].into_boxed_slice());
    let server = crate::server::AnyMcpServer::new_with_optional_registries(runtime, registries)?;
    let served = crate::stdio::serve_stdio(server, protocol).await;
    let http = client.http_metrics();
    let discussions = client.attached_discussion_metrics();
    let snapshot = serde_json::json!({
        "http_logical_operations": http.logical_operations.saturating_sub(http_before.logical_operations),
        "http_physical_attempts": http.physical_attempts.saturating_sub(http_before.physical_attempts),
        "parent_get_attempts": discussions.parent_get_attempts.saturating_sub(discussions_before.parent_get_attempts),
        "show_attempts": discussions.show_attempts.saturating_sub(discussions_before.show_attempts),
        "accepted_shows": discussions.accepted_shows.saturating_sub(discussions_before.accepted_shows),
        "close_attempts": discussions.close_attempts.saturating_sub(discussions_before.close_attempts),
        "close_successes": discussions.close_successes.saturating_sub(discussions_before.close_successes),
        "write_dispatches": discussions.write_dispatches.saturating_sub(discussions_before.write_dispatches),
    });
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(metrics_path)?;
    serde_json::to_writer(file, &snapshot)?;
    served?;
    Ok(())
}

async fn object_discussion_get(
    runtime: &RuntimeContext,
    input: ObjectDiscussionGetInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
    let Ok(contract) = object_discussion_get_tool() else {
        return tool_error(&ToolError::upstream());
    };
    let deadline = runtime.request_deadline();
    let client = runtime.client().clone();
    execute_prepared_handler_until(
        runtime,
        deadline,
        &contract,
        OperationContext::new(OBJECT_DISCUSSION_GET),
        cancellation,
        async move {
            let space_id = client.resolve_space_id(input.space.as_str()).await?;
            let remaining = bounded_remaining(deadline)?;
            client
                .attached_discussion(space_id, input.object_id.as_str())
                .operation_timeout(remaining)
                .get()
                .await
                .map_err(HandlerOperationError::from)
        },
        |state| async move { project_state(state) },
    )
    .await
}

fn bounded_remaining(deadline: Instant) -> Result<Duration, HandlerOperationError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(HandlerError::new(ToolError::upstream()).into());
    }
    Ok(remaining.min(MAX_ATTACHED_DISCUSSION_OPERATION_TIMEOUT))
}

fn project_state(state: AttachedDiscussion) -> Result<ObjectDiscussionGetOutput, HandlerError> {
    let projected = match state {
        AttachedDiscussion::Absent {
            space_id,
            parent_id,
        } => ObjectDiscussionGetOutput::Absent {
            space_id: entity_id(space_id)?,
            object_id: entity_id(parent_id)?,
        },
        AttachedDiscussion::Attached {
            space_id,
            parent_id,
            discussion_id,
        } => ObjectDiscussionGetOutput::Attached {
            space_id: entity_id(space_id)?,
            object_id: entity_id(parent_id)?,
            discussion_id: entity_id(discussion_id)?,
        },
    };
    let bytes =
        serde_json::to_vec(&projected).map_err(|_| HandlerError::new(ToolError::upstream()))?;
    if bytes.len() > RESULT_BYTE_CEILING {
        Err(HandlerError::new(ToolError::bounded_result()))
    } else {
        Ok(projected)
    }
}

fn entity_id(value: String) -> Result<EntityId, HandlerError> {
    EntityId::new(value).map_err(|error| match error {
        DomainValueError::TooLong { .. } => HandlerError::new(ToolError::bounded_result()),
        DomainValueError::Empty | DomainValueError::InvalidIdentifierCharacter => {
            HandlerError::new(ToolError::bounded_result())
        }
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, sync::Arc, time::Duration};

    use anytype::{
        attached_discussions::AttachedDiscussionErrorKind,
        error::AnytypeError,
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
    };
    use rmcp::model::{CallToolRequestParams, ListToolsResult, ToolAnnotations};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex, split},
        sync::Notify,
    };
    use url::Url;

    use super::*;
    use crate::{
        config::ApplicationProfile,
        error::{AnytypeErrorMapping, ToolErrorCode},
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        runtime::{OperationContext, StartupStatus},
        schema::{input_schema, output_schema},
        server::AnyMcpServer,
    };

    const SPACE: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const PARENT: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a";
    const DISCUSSION: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4b";
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/discussions-token-budget.json");
    static TEST_DISCUSSION_REGISTRIES: [&dyn OptionalToolsetRegistry; 1] = [DISCUSSIONS_REGISTRY];

    fn runtime(selected: bool, read_only: bool) -> RuntimeContext {
        runtime_with_options(selected, read_only, Duration::from_secs(2), true)
    }

    fn runtime_with_options(
        selected: bool,
        read_only: bool,
        request_timeout: Duration,
        authenticated: bool,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_config(ClientConfig {
            base_url: Some("http://127.0.0.1:1".to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("discussion-toolset-test".to_owned()),
            app_name: "discussion-toolset-test".to_owned(),
            disable_cache: true,
            ..ClientConfig::default()
        })
        .expect("offline client");
        if authenticated {
            client.set_api_key(HttpCredentials::new("fixture-token"));
        }
        let selection = OptionalToolsetSelection::parse(
            selected.then(|| "discussions".to_owned()),
            &[DISCUSSIONS_REGISTRY.metadata()],
        )
        .expect("selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            request_timeout,
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            read_only,
            selection,
        )
    }

    fn server(selected: bool, read_only: bool) -> AnyMcpServer {
        server_with_options(selected, read_only, Duration::from_secs(2), true)
    }

    fn server_with_options(
        selected: bool,
        read_only: bool,
        request_timeout: Duration,
        authenticated: bool,
    ) -> AnyMcpServer {
        AnyMcpServer::new_with_optional_registries(
            runtime_with_options(selected, read_only, request_timeout, authenticated),
            &TEST_DISCUSSION_REGISTRIES,
        )
        .expect("server")
    }

    #[test]
    fn exact_contract_is_closed_read_only_and_branch_local() {
        let contract = object_discussion_get_tool().expect("contract");
        let wire = serde_json::to_value(contract.as_tool()).expect("tool JSON");
        assert_eq!(
            wire["annotations"],
            serde_json::to_value(
                ToolAnnotations::new()
                    .read_only(true)
                    .destructive(false)
                    .open_world(false)
            )
            .expect("annotations")
        );
        let input = input_schema::<ObjectDiscussionGetInput>().expect("input schema");
        assert_eq!(input["additionalProperties"], false);
        assert_eq!(input["required"], json!(["space", "object_id"]));
        let output = output_schema::<ObjectDiscussionGetOutput>().expect("output schema");
        assert_eq!(output["oneOf"].as_array().map(Vec::len), Some(2));
        let serialized = serde_json::to_string(output.as_ref()).expect("output schema JSON");
        for forbidden in ["title", "name", "comment", "message", "author", "timestamp"] {
            assert!(
                !serialized.contains(forbidden),
                "forbidden output field {forbidden}"
            );
        }
    }

    #[test]
    fn viewer_markdown_parser_accepts_only_canonical_same_space_object_links() {
        let first = "bafyreiaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "bafyreibbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let markdown = format!(
            "bafyreiccccccccccccccccccccccccccccccccccccccccccccccccccc [one](https://object.any.coop/{first}?spaceId={SPACE}) [two](https://object.any.coop/{second}?spaceId={SPACE}) [foreign](https://object.any.coop/bafyreiddddddddddddddddddddddddddddddddddddddddddddddddddd?spaceId=other) [web](https://example.com/{first})"
        );
        let targets = markdown_object_link_targets(&markdown, SPACE);
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(first));
        assert!(targets.contains(second));
    }

    #[test]
    fn projection_is_exact_and_rejects_malformed_identity() {
        let absent = project_state(AttachedDiscussion::Absent {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
        })
        .expect("absent");
        assert_eq!(
            serde_json::to_value(absent).expect("absent JSON"),
            json!({"state":"absent","space_id":SPACE,"object_id":PARENT})
        );
        let attached = project_state(AttachedDiscussion::Attached {
            space_id: SPACE.to_owned(),
            parent_id: PARENT.to_owned(),
            discussion_id: DISCUSSION.to_owned(),
        })
        .expect("attached");
        assert_eq!(
            serde_json::to_value(attached).expect("attached JSON"),
            json!({
                "state":"attached",
                "space_id":SPACE,
                "object_id":PARENT,
                "discussion_id":DISCUSSION
            })
        );
        assert!(
            project_state(AttachedDiscussion::Attached {
                space_id: SPACE.to_owned(),
                parent_id: PARENT.to_owned(),
                discussion_id: "bad/id".to_owned(),
            })
            .is_err()
        );
    }

    #[test]
    fn typed_api_failures_map_to_closed_design_categories() {
        let cases = [
            (
                AttachedDiscussionErrorKind::UnsupportedParentLayout,
                ToolErrorCode::Validation,
            ),
            (
                AttachedDiscussionErrorKind::MalformedEvidence,
                ToolErrorCode::BoundedResult,
            ),
            (
                AttachedDiscussionErrorKind::RpcDeadline,
                ToolErrorCode::Upstream,
            ),
            (
                AttachedDiscussionErrorKind::OperationDeadline,
                ToolErrorCode::Upstream,
            ),
            (
                AttachedDiscussionErrorKind::CleanupFailed,
                ToolErrorCode::Upstream,
            ),
            (
                AttachedDiscussionErrorKind::Upstream,
                ToolErrorCode::Upstream,
            ),
            (
                AttachedDiscussionErrorKind::OwnedTaskFailed,
                ToolErrorCode::Upstream,
            ),
        ];
        for (kind, expected) in cases {
            let mapped = ToolError::from_anytype(&AnytypeError::AttachedDiscussion { kind });
            let AnytypeErrorMapping::Ready(mapped) = mapped else {
                panic!("closed attached-discussion error never needs candidates");
            };
            assert_eq!(mapped.code(), expected);
            assert!(!mapped.message().contains(SPACE));
        }
        for source in [AnytypeError::Unauthorized, AnytypeError::Forbidden] {
            let AnytypeErrorMapping::Ready(mapped) = ToolError::from_anytype(&source) else {
                panic!("typed authentication never needs ambiguity candidates");
            };
            assert_eq!(mapped.code(), ToolErrorCode::Authentication);
            assert!(!mapped.message().contains(SPACE));
        }
    }

    #[tokio::test]
    async fn default_off_read_only_parity_and_strict_dispatch_hold_without_io() {
        assert!(
            OptionalToolsetSelection::parse(
                Some("discussions".to_owned()),
                &production_optional_metadata(),
            )
            .is_err(),
            "the shipped selector stays unsupported until acceptance unblocks"
        );
        let shipped = AnyMcpServer::new(runtime(false, false)).expect("shipped server");
        let shipped_error = shipped
            .dispatch_tool(
                CallToolRequestParams::new(OBJECT_DISCUSSION_GET),
                &CancellationToken::new(),
            )
            .await
            .expect_err("production-unlinked method");
        assert_eq!(shipped_error.code.0, -32601);

        let absent = server(false, false);
        assert!(
            absent
                .tools()
                .iter()
                .all(|tool| tool.name != OBJECT_DISCUSSION_GET)
        );
        let error = absent
            .dispatch_tool(
                CallToolRequestParams::new(OBJECT_DISCUSSION_GET),
                &CancellationToken::new(),
            )
            .await
            .expect_err("unselected tool");
        assert_eq!(error.code.0, -32601);

        for read_only in [false, true] {
            let selected = server(true, read_only);
            assert!(
                selected
                    .tools()
                    .iter()
                    .any(|tool| tool.name == OBJECT_DISCUSSION_GET)
            );
            for arguments in [
                json!({"space":SPACE,"object_id":PARENT,"extra":true}),
                json!({"space":SPACE,"object_id":null}),
                json!({"space":SPACE,"object_id":"bad/id"}),
            ] {
                let error = selected
                    .dispatch_tool(
                        CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                            arguments.as_object().cloned().expect("arguments object"),
                        ),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect_err("invalid params");
                assert_eq!(error.code.0, -32602);
            }
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let cancelled = selected
                .dispatch_tool(
                    CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                        json!({"space":SPACE,"object_id":PARENT})
                            .as_object()
                            .cloned()
                            .expect("arguments"),
                    ),
                    &cancellation,
                )
                .await
                .expect("pre-cancelled result");
            assert_eq!(cancelled.is_error, Some(true));
            assert_eq!(
                cancelled
                    .structured_content
                    .as_ref()
                    .and_then(|value| value["code"].as_str()),
                Some("upstream")
            );
            assert_eq!(
                selected
                    .runtime()
                    .client()
                    .http_metrics()
                    .logical_operations,
                0
            );
            assert_eq!(
                selected
                    .runtime()
                    .client()
                    .attached_discussion_metrics()
                    .write_dispatches,
                0
            );
        }
    }

    async fn read_json_line(
        reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    ) -> Value {
        let mut line = String::new();
        reader.read_line(&mut line).await.expect("read stdio frame");
        serde_json::from_str(&line).expect("stdio JSON")
    }

    #[tokio::test]
    async fn stable_and_preview_stdio_advertise_the_same_read_only_descriptor() {
        let (stable_client, stable_server) = duplex(64 * 1024);
        let (stable_server_reader, stable_server_writer) = split(stable_server);
        let stable_task = tokio::spawn(crate::stdio::serve_stable(
            server(true, true),
            BufReader::new(stable_server_reader),
            stable_server_writer,
        ));
        let (stable_client_reader, mut stable_client_writer) = split(stable_client);
        let mut stable_client_reader = BufReader::new(stable_client_reader);
        let initialize = json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{
                "protocolVersion":"2025-11-25",
                "capabilities":{},
                "clientInfo":{"name":"discussion-test","version":"1"}
            }
        });
        stable_client_writer
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .expect("write initialize");
        let initialized = read_json_line(&mut stable_client_reader).await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        stable_client_writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .expect("write initialized notification");
        stable_client_writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")
            .await
            .expect("write tools/list");
        let stable_tools = read_json_line(&mut stable_client_reader).await;
        assert!(
            stable_tools["result"]["tools"]
                .as_array()
                .is_some_and(|tools| {
                    tools
                        .iter()
                        .any(|tool| tool["name"] == OBJECT_DISCUSSION_GET)
                })
        );
        stable_client_writer
            .shutdown()
            .await
            .expect("close stable input");
        drop(stable_client_writer);
        drop(stable_client_reader);
        stable_task
            .await
            .expect("stable task")
            .expect("stable transport");

        let (preview_client, preview_server) = duplex(64 * 1024);
        let (preview_server_reader, preview_server_writer) = split(preview_server);
        let preview_task = tokio::spawn(crate::stdio::serve_preview(
            server(true, true),
            BufReader::new(preview_server_reader),
            preview_server_writer,
        ));
        let (preview_client_reader, mut preview_client_writer) = split(preview_client);
        let mut preview_client_reader = BufReader::new(preview_client_reader);
        let tools_list = json!({
            "jsonrpc":"2.0",
            "id":3,
            "method":"tools/list",
            "params":{
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{"name":"discussion-test","version":"1"},
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        preview_client_writer
            .write_all(format!("{tools_list}\n").as_bytes())
            .await
            .expect("write preview tools/list");
        let preview_tools = read_json_line(&mut preview_client_reader).await;
        assert!(
            preview_tools["result"]["tools"]
                .as_array()
                .is_some_and(|tools| {
                    tools
                        .iter()
                        .any(|tool| tool["name"] == OBJECT_DISCUSSION_GET)
                })
        );
        assert_eq!(
            stable_tools["result"]["tools"], preview_tools["result"]["tools"],
            "stable and preview catalogs are identical"
        );
        preview_client_writer
            .shutdown()
            .await
            .expect("close preview input");
        drop(preview_client_writer);
        drop(preview_client_reader);
        preview_task
            .await
            .expect("preview task")
            .expect("preview transport");
    }

    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            Value::Array(items) => Value::Array(items.into_iter().map(canonical).collect()),
            scalar => scalar,
        }
    }

    fn canonical_bytes(value: &Value) -> Vec<u8> {
        serde_json::to_vec(&canonical(value.clone())).expect("canonical JSON")
    }

    fn record(tokenizer: &CoreBPE, value: Value) -> Value {
        let bytes = canonical_bytes(&value);
        json!({
            "sha256": Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "bytes": bytes.len(),
            "tokens": tokenizer.encode_with_special_tokens(
                std::str::from_utf8(&bytes).expect("JSON UTF-8")
            ).len()
        })
    }

    fn token_snapshot() -> Value {
        let tokenizer = o200k_base().expect("tokenizer");
        let tool = serde_json::to_value(object_discussion_get_tool().expect("tool").as_tool())
            .expect("tool JSON");
        let selected_server = server(true, false);
        let selected = serde_json::to_value(ListToolsResult::with_all_items(
            selected_server.tools().to_vec(),
        ))
        .expect("selected tools JSON");
        let selected_read_only_server = server(true, true);
        let selected_read_only = serde_json::to_value(ListToolsResult::with_all_items(
            selected_read_only_server.tools().to_vec(),
        ))
        .expect("read-only tools JSON");
        let base_server = server(false, false);
        let base = serde_json::to_value(ListToolsResult::with_all_items(
            base_server.tools().to_vec(),
        ))
        .expect("base tools JSON");
        let base_read_only_server = server(false, true);
        let base_read_only = serde_json::to_value(ListToolsResult::with_all_items(
            base_read_only_server.tools().to_vec(),
        ))
        .expect("read-only base tools JSON");
        let attached = object_discussion_get_tool()
            .expect("tool")
            .success(&ObjectDiscussionGetOutput::Attached {
                space_id: EntityId::new("s".repeat(256)).expect("space"),
                object_id: EntityId::new("o".repeat(256)).expect("object"),
                discussion_id: EntityId::new("d".repeat(256)).expect("discussion"),
            })
            .expect("result");
        let result = serde_json::to_value(attached).expect("result JSON");
        let base_record = record(&tokenizer, base);
        let base_read_only_record = record(&tokenizer, base_read_only);
        let selected_record = record(&tokenizer, selected);
        let selected_read_only_record = record(&tokenizer, selected_read_only);
        let contribution = selected_record["tokens"]
            .as_u64()
            .expect("selected tokens")
            .saturating_sub(base_record["tokens"].as_u64().expect("base tokens"));
        let read_only_contribution = selected_read_only_record["tokens"]
            .as_u64()
            .expect("selected read-only tokens")
            .saturating_sub(
                base_read_only_record["tokens"]
                    .as_u64()
                    .expect("base read-only tokens"),
            );
        json!({
            "tokenizer":"tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog":base_record,
            "base_read_only_catalog":base_read_only_record,
            "tool":record(&tokenizer, tool),
            "selected_catalog":selected_record,
            "selected_read_only_catalog":selected_read_only_record,
            "selected_contribution_tokens":contribution,
            "selected_read_only_contribution_tokens":read_only_contribution,
            "maximum_result":record(&tokenizer, result),
            "catalog_ceiling_tokens":DISCUSSIONS_CATALOG_TOKEN_CEILING,
            "selection_ceiling_tokens":2000,
            "result_ceiling_bytes":RESULT_BYTE_CEILING,
            "result_ceiling_tokens":600
        })
    }

    #[test]
    fn production_unlink_and_reviewed_token_snapshot_are_exact() {
        let metadata = production_optional_metadata();
        assert!(
            metadata.iter().all(|entry| entry.name != "discussions"),
            "discussions stays unsupported until its mandatory viewer fixture passes"
        );
        assert_eq!(DISCUSSIONS_REGISTRY.tools().expect("tools").len(), 1);
        assert!(DISCUSSIONS_REGISTRY.resources().is_empty());
        assert!(DISCUSSIONS_REGISTRY.resource_templates().is_empty());
        let actual = canonical(token_snapshot());
        let reviewed = canonical(
            serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).expect("reviewed token snapshot"),
        );
        assert_eq!(actual, reviewed, "discussion token snapshot drifted");
        assert!(actual["tool"]["tokens"].as_u64().expect("tokens") <= 1_500);
        assert!(actual["maximum_result"]["bytes"].as_u64().expect("bytes") <= 2_048);
        assert!(actual["maximum_result"]["tokens"].as_u64().expect("tokens") <= 600);
        assert!(
            actual["selected_contribution_tokens"]
                .as_u64()
                .expect("selected contribution")
                <= 2_000
        );
        assert!(
            actual["selected_read_only_contribution_tokens"]
                .as_u64()
                .expect("read-only selected contribution")
                <= 2_000
        );
        let read_write_tool = server(true, false)
            .tools()
            .iter()
            .find(|tool| tool.name == OBJECT_DISCUSSION_GET)
            .expect("read-write discussion tool")
            .clone();
        let read_only_tool = server(true, true)
            .tools()
            .iter()
            .find(|tool| tool.name == OBJECT_DISCUSSION_GET)
            .expect("read-only discussion tool")
            .clone();
        assert_eq!(
            serde_json::to_value(read_write_tool).expect("read-write tool JSON"),
            serde_json::to_value(read_only_tool).expect("read-only tool JSON")
        );
    }

    #[tokio::test]
    #[ignore = "requires configured read-only test12 fixture"]
    async fn read_only_fixture_exposes_page_discussion_without_retaining_content() {
        let Ok(space_id) = std::env::var("ANYTYPE_TEST_READ_ONLY_SPACE_ID") else {
            return;
        };
        let client = anytype::test_util::test_client_named("any-mcp-discussions-viewer")
            .expect("configured read-only fixture client");
        client
            .ping_http()
            .await
            .unwrap_or_else(|_| panic!("viewer HTTP authentication failed"));
        client
            .ping_grpc()
            .await
            .unwrap_or_else(|_| panic!("viewer gRPC authentication failed"));

        let pages = client
            .objects(&space_id)
            .limit(1000)
            .list()
            .await
            .unwrap_or_else(|_| panic!("viewer object listing failed"))
            .collect_all()
            .await
            .unwrap_or_else(|_| panic!("viewer object collection failed"));
        let page = pages
            .into_iter()
            .find(|object| object.name.as_deref() == Some("Page One"))
            .expect("Page One exists exactly in the configured fixture");
        let state = match client.attached_discussion(&space_id, &page.id).get().await {
            Ok(state) => state,
            Err(AnytypeError::AttachedDiscussion { kind }) => {
                let metrics = client.attached_discussion_metrics();
                panic!(
                    "viewer attached-discussion read failed with safe kind {kind:?}; parent_get={} show={} accepted={} close={} closed={}",
                    metrics.parent_get_attempts,
                    metrics.show_attempts,
                    metrics.accepted_shows,
                    metrics.close_attempts,
                    metrics.close_successes
                )
            }
            Err(_) => panic!("viewer attached-discussion read failed outside its closed error"),
        };
        let discussion_id = state
            .discussion_id()
            .expect("Page One has an attached discussion")
            .to_owned();
        let messages = client
            .chats()
            .in_space(&space_id)
            .older_messages(&discussion_id)
            .limit(12)
            .get()
            .await
            .unwrap_or_else(|_| panic!("viewer discussion message read failed"));
        assert_eq!(messages.messages.len(), 2);
        assert!(messages.next_before.is_none());

        let markdown = page.markdown.unwrap_or_default();
        let mut linked_ids = markdown_object_link_targets(&markdown, &space_id);
        assert_eq!(
            linked_ids.len(),
            2,
            "Page One has two distinct canonical Anytype object links"
        );
        for linked_id in &linked_ids {
            assert!(linked_id != &discussion_id);
            let chat = client
                .chats()
                .get_chat(&space_id, linked_id)
                .get()
                .await
                .unwrap_or_else(|_| panic!("viewer ordinary-chat link verification failed"));
            assert!(chat.id == *linked_id);
            assert!(chat.space_id == space_id);
            assert!(chat.layout == anytype::objects::ObjectLayout::Chat);
        }
        linked_ids.clear();
        drop(markdown);
        drop(messages);

        let server = candidate_server_from_client(client, true);
        let result = server
            .dispatch_tool(
                CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                    json!({"space":space_id,"object_id":page.id})
                        .as_object()
                        .cloned()
                        .expect("arguments"),
                ),
                &CancellationToken::new(),
            )
            .await
            .expect("viewer direct dispatch");
        assert_eq!(result.is_error, Some(false));
        let output = result.structured_content.expect("viewer result");
        assert_eq!(output["state"], "attached");
        assert!(output["discussion_id"].as_str() == Some(discussion_id.as_str()));
        assert_eq!(
            server
                .runtime()
                .client()
                .attached_discussion_metrics()
                .write_dispatches,
            0
        );
    }

    fn markdown_object_link_targets(
        markdown: &str,
        expected_space_id: &str,
    ) -> std::collections::BTreeSet<String> {
        let mut targets = std::collections::BTreeSet::new();
        let mut remainder = markdown;
        while let Some(start) = remainder.find("](") {
            remainder = &remainder[start.saturating_add(2)..];
            let Some(end) = remainder.find(')') else {
                break;
            };
            let destination = &remainder[..end];
            remainder = &remainder[end.saturating_add(1)..];
            let Ok(url) = Url::parse(destination) else {
                continue;
            };
            if url.scheme() != "https" || url.host_str() != Some("object.any.coop") {
                continue;
            }
            let Some(object_id) = url.path_segments().and_then(|mut segments| {
                let object_id = segments.next()?;
                segments.next().is_none().then_some(object_id)
            }) else {
                continue;
            };
            if !url
                .query_pairs()
                .any(|(key, value)| key == "spaceId" && value == expected_space_id)
            {
                continue;
            }
            if let Ok(object_id) = EntityId::new(object_id) {
                targets.insert(object_id.as_str().to_owned());
            }
        }
        targets
    }

    fn runtime_from_client(client: AnytypeClient, read_only: bool) -> RuntimeContext {
        let selection = OptionalToolsetSelection::parse(
            Some("discussions".to_owned()),
            &[DISCUSSIONS_REGISTRY.metadata()],
        )
        .expect("discussion selection");
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            2,
            Duration::from_secs(30),
            StartupStatus {
                http_available: true,
                grpc_available: true,
            },
            ApplicationProfile::Compact,
            read_only,
            selection,
        )
    }

    fn candidate_server_from_client(client: AnytypeClient, read_only: bool) -> AnyMcpServer {
        AnyMcpServer::new_with_optional_registries(
            runtime_from_client(client, read_only),
            &TEST_DISCUSSION_REGISTRIES,
        )
        .expect("discussion candidate server")
    }

    async fn preview_tool_call(server: AnyMcpServer, arguments: Value) -> Value {
        preview_named_tool_call(server, OBJECT_DISCUSSION_GET, arguments).await
    }

    async fn preview_named_tool_call(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Value {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_preview(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client_io);
        let mut client_reader = BufReader::new(client_reader);
        let frame = json!({
            "jsonrpc":"2.0",
            "id":7,
            "method":"tools/call",
            "params":{
                "name":name,
                "arguments":arguments,
                "_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientInfo":{"name":"discussion-live-test","version":"1"},
                    "io.modelcontextprotocol/clientCapabilities":{}
                }
            }
        });
        client_writer
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .expect("write preview call");
        let response = read_json_line(&mut client_reader).await;
        client_writer.shutdown().await.expect("close preview input");
        drop(client_writer);
        drop(client_reader);
        task.await
            .expect("preview task")
            .expect("preview transport");
        response
    }

    async fn stable_tool_call(server: AnyMcpServer, arguments: Value) -> Value {
        stable_named_tool_call(server, OBJECT_DISCUSSION_GET, arguments).await
    }

    async fn stable_named_tool_call(
        server: AnyMcpServer,
        name: &'static str,
        arguments: Value,
    ) -> Value {
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_stable(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = split(client_io);
        let mut client_reader = BufReader::new(client_reader);
        let initialize = json!({
            "jsonrpc":"2.0","id":1,"method":"initialize",
            "params":{
                "protocolVersion":"2025-11-25",
                "capabilities":{},
                "clientInfo":{"name":"discussion-live-test","version":"1"}
            }
        });
        client_writer
            .write_all(format!("{initialize}\n").as_bytes())
            .await
            .expect("write stable initialize");
        let initialized = read_json_line(&mut client_reader).await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        client_writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .expect("write stable initialized");
        let call = json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":name,"arguments":arguments}
        });
        client_writer
            .write_all(format!("{call}\n").as_bytes())
            .await
            .expect("write stable call");
        let response = read_json_line(&mut client_reader).await;
        client_writer.shutdown().await.expect("close stable input");
        drop(client_writer);
        drop(client_reader);
        task.await.expect("stable task").expect("stable transport");
        response
    }

    fn direct_outcome(result: Result<CallToolResult, ErrorData>) -> Value {
        match result {
            Ok(result) => json!({"result":result}),
            Err(error) => json!({"error":error}),
        }
    }

    fn protocol_outcome(response: Value) -> Value {
        let mut outcome = match (response.get("result"), response.get("error")) {
            (Some(result), None) => json!({"result":result}),
            (None, Some(error)) => json!({"error":error}),
            _ => panic!("protocol response must contain one outcome"),
        };
        if let Some(result) = outcome.get_mut("result").and_then(Value::as_object_mut) {
            result.remove("resultType");
        }
        outcome
    }

    fn work(client: &AnytypeClient) -> (u64, u64, u64, u64, u64, u64, u64, u64) {
        let http = client.http_metrics();
        let discussions = client.attached_discussion_metrics();
        (
            http.logical_operations,
            discussions.parent_get_attempts,
            discussions.show_attempts,
            discussions.accepted_shows,
            discussions.close_attempts,
            discussions.close_successes,
            discussions.write_dispatches,
            http.physical_attempts,
        )
    }

    async fn occupy_runtime_permit(
        server: &AnyMcpServer,
    ) -> (Arc<Notify>, tokio::task::JoinHandle<()>) {
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let task_started = Arc::clone(&started);
        let task_release = Arc::clone(&release);
        let runtime = server.runtime().clone();
        let task = tokio::spawn(async move {
            let cancellation = CancellationToken::new();
            runtime
                .execute(
                    OperationContext::new("discussion_cancellation_gate"),
                    &cancellation,
                    async move {
                        task_started.notify_one();
                        task_release.notified().await;
                        Ok(())
                    },
                )
                .await
                .expect("release cancellation gate");
        });
        tokio::time::timeout(Duration::from_secs(1), started.notified())
            .await
            .expect("cancellation gate admission");
        (release, task)
    }

    async fn read_protocol_id(
        reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        expected: u64,
        cancelled: u64,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let response = read_json_line(reader).await;
                assert_ne!(response["id"], json!(cancelled));
                if response["id"] == json!(expected) {
                    return response;
                }
            }
        })
        .await
        .expect("bounded protocol response")
    }

    async fn stable_stdio_precancel_is_zero_io() {
        let server = server(true, false);
        let client = server.runtime().client().clone();
        let (release, blocker) = occupy_runtime_permit(&server).await;
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_stable(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut writer) = split(client_io);
        let mut reader = BufReader::new(client_reader);
        writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"discussion-cancel\",\"version\":\"1\"}}}\n",
            )
            .await
            .expect("write stable initialize");
        assert_eq!(read_protocol_id(&mut reader, 1, 71).await["id"], 1);
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .expect("write initialized notification");
        writer
            .write_all(
                format!(
                    "{}\n",
                    json!({
                        "jsonrpc":"2.0","id":71,"method":"tools/call",
                        "params":{"name":OBJECT_DISCUSSION_GET,"arguments":{"space":SPACE,"object_id":PARENT}}
                    })
                )
                .as_bytes(),
            )
            .await
            .expect("write stable cancellable call");
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":72,\"method\":\"ping\"}\n")
            .await
            .expect("write stable admission ping");
        assert_eq!(read_protocol_id(&mut reader, 72, 71).await["id"], 72);
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":71,\"reason\":\"caller cancelled\"}}\n")
            .await
            .expect("write stable cancellation");
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":73,\"method\":\"ping\"}\n")
            .await
            .expect("write stable cancellation barrier");
        assert_eq!(read_protocol_id(&mut reader, 73, 71).await["id"], 73);
        release.notify_one();
        blocker.await.expect("stable blocker join");
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":74,\"method\":\"ping\"}\n")
            .await
            .expect("write stable completion ping");
        assert_eq!(read_protocol_id(&mut reader, 74, 71).await["id"], 74);
        writer.shutdown().await.expect("close stable input");
        drop(writer);
        drop(reader);
        task.await.expect("stable task").expect("stable transport");
        assert_eq!(work(&client), (0, 0, 0, 0, 0, 0, 0, 0));
    }

    fn preview_request(id: u64, method: &str, params: Value) -> Value {
        let mut params = params.as_object().cloned().expect("preview params object");
        params.insert(
            "_meta".to_owned(),
            json!({
                "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                "io.modelcontextprotocol/clientInfo":{"name":"discussion-cancel","version":"1"},
                "io.modelcontextprotocol/clientCapabilities":{}
            }),
        );
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
    }

    async fn preview_stdio_precancel_is_zero_io() {
        let server = server(true, false);
        let client = server.runtime().client().clone();
        let (release, blocker) = occupy_runtime_permit(&server).await;
        let (client_io, server_io) = duplex(64 * 1024);
        let (server_reader, server_writer) = split(server_io);
        let task = tokio::spawn(crate::stdio::serve_preview(
            server,
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut writer) = split(client_io);
        let mut reader = BufReader::new(client_reader);
        let call = preview_request(
            71,
            "tools/call",
            json!({
                "name":OBJECT_DISCUSSION_GET,
                "arguments":{"space":SPACE,"object_id":PARENT}
            }),
        );
        writer
            .write_all(format!("{call}\n").as_bytes())
            .await
            .expect("write preview cancellable call");
        let ping = preview_request(72, "ping", json!({}));
        writer
            .write_all(format!("{ping}\n").as_bytes())
            .await
            .expect("write preview admission ping");
        assert_eq!(read_protocol_id(&mut reader, 72, 71).await["id"], 72);
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":71,\"reason\":\"caller cancelled\"}}\n")
            .await
            .expect("write preview cancellation");
        let barrier = preview_request(73, "ping", json!({}));
        writer
            .write_all(format!("{barrier}\n").as_bytes())
            .await
            .expect("write preview cancellation barrier");
        assert_eq!(read_protocol_id(&mut reader, 73, 71).await["id"], 73);
        release.notify_one();
        blocker.await.expect("preview blocker join");
        let completion = preview_request(74, "ping", json!({}));
        writer
            .write_all(format!("{completion}\n").as_bytes())
            .await
            .expect("write preview completion ping");
        assert_eq!(read_protocol_id(&mut reader, 74, 71).await["id"], 74);
        writer.shutdown().await.expect("close preview input");
        drop(writer);
        drop(reader);
        task.await
            .expect("preview task")
            .expect("preview transport");
        assert_eq!(work(&client), (0, 0, 0, 0, 0, 0, 0, 0));
    }

    async fn assert_direct_stable_preview_error_result_redaction_and_work_matrix() {
        for (name, arguments, expected_code) in [
            (
                "object_discussion_unknown",
                json!({"space":SPACE,"object_id":PARENT}),
                -32601,
            ),
            (
                OBJECT_DISCUSSION_GET,
                json!({"space":SPACE,"object_id":null}),
                -32602,
            ),
            (
                OBJECT_DISCUSSION_GET,
                json!({"space":SPACE,"object_id":PARENT,"unknown":true}),
                -32602,
            ),
            (
                OBJECT_DISCUSSION_GET,
                json!({"space":SPACE,"object_id":"bad/id"}),
                -32602,
            ),
        ] {
            let direct_server = server(true, false);
            let direct_client = direct_server.runtime().client().clone();
            let direct = direct_outcome(
                direct_server
                    .dispatch_tool(
                        CallToolRequestParams::new(name).with_arguments(
                            arguments.as_object().cloned().expect("matrix arguments"),
                        ),
                        &CancellationToken::new(),
                    )
                    .await,
            );
            let stable_server = server(true, false);
            let stable_client = stable_server.runtime().client().clone();
            let stable = protocol_outcome(
                stable_named_tool_call(stable_server, name, arguments.clone()).await,
            );
            let preview_server = server(true, false);
            let preview_client = preview_server.runtime().client().clone();
            let preview =
                protocol_outcome(preview_named_tool_call(preview_server, name, arguments).await);
            assert!(direct == stable && stable == preview);
            assert_eq!(direct["error"]["code"], expected_code);
            assert_eq!(work(&direct_client), (0, 0, 0, 0, 0, 0, 0, 0));
            assert_eq!(work(&stable_client), (0, 0, 0, 0, 0, 0, 0, 0));
            assert_eq!(work(&preview_client), (0, 0, 0, 0, 0, 0, 0, 0));
        }

        for read_only in [false, true] {
            let arguments = json!({"space":SPACE,"object_id":PARENT});
            let direct_server = server_with_options(true, read_only, Duration::from_secs(2), false);
            let direct_client = direct_server.runtime().client().clone();
            let direct = direct_outcome(
                direct_server
                    .dispatch_tool(
                        CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                            arguments.as_object().cloned().expect("auth arguments"),
                        ),
                        &CancellationToken::new(),
                    )
                    .await,
            );
            let stable_server = server_with_options(true, read_only, Duration::from_secs(2), false);
            let stable_client = stable_server.runtime().client().clone();
            let stable = protocol_outcome(stable_tool_call(stable_server, arguments.clone()).await);
            let preview_server =
                server_with_options(true, read_only, Duration::from_secs(2), false);
            let preview_client = preview_server.runtime().client().clone();
            let preview = protocol_outcome(preview_tool_call(preview_server, arguments).await);
            assert!(direct == stable && stable == preview);
            assert_eq!(
                direct["result"]["structuredContent"]["code"],
                "authentication"
            );
            let encoded = serde_json::to_string(&direct).expect("auth outcome JSON");
            assert!(!encoded.contains(SPACE));
            assert!(!encoded.contains(PARENT));
            assert!(work(&direct_client) == work(&stable_client));
            assert!(work(&stable_client) == work(&preview_client));
            assert_eq!(work(&direct_client), (0, 1, 0, 0, 0, 0, 0, 0));
        }

        let arguments = json!({"space":SPACE,"object_id":PARENT});
        let direct_server = server_with_options(true, false, Duration::from_nanos(1), true);
        let direct_client = direct_server.runtime().client().clone();
        tokio::task::yield_now().await;
        let direct = direct_outcome(
            direct_server
                .dispatch_tool(
                    CallToolRequestParams::new(OBJECT_DISCUSSION_GET)
                        .with_arguments(arguments.as_object().cloned().expect("timeout arguments")),
                    &CancellationToken::new(),
                )
                .await,
        );
        let stable_server = server_with_options(true, false, Duration::from_nanos(1), true);
        let stable_client = stable_server.runtime().client().clone();
        let stable = protocol_outcome(stable_tool_call(stable_server, arguments.clone()).await);
        let preview_server = server_with_options(true, false, Duration::from_nanos(1), true);
        let preview_client = preview_server.runtime().client().clone();
        let preview = protocol_outcome(preview_tool_call(preview_server, arguments).await);
        assert!(direct == stable && stable == preview);
        assert_eq!(direct["result"]["structuredContent"]["code"], "upstream");
        assert_eq!(work(&direct_client), (0, 0, 0, 0, 0, 0, 0, 0));
        assert_eq!(work(&stable_client), (0, 0, 0, 0, 0, 0, 0, 0));
        assert_eq!(work(&preview_client), (0, 0, 0, 0, 0, 0, 0, 0));

        let direct_server = server(true, false);
        let direct_client = direct_server.runtime().client().clone();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let direct = direct_outcome(
            direct_server
                .dispatch_tool(
                    CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                        json!({"space":SPACE,"object_id":PARENT})
                            .as_object()
                            .cloned()
                            .expect("cancel arguments"),
                    ),
                    &cancelled,
                )
                .await,
        );
        assert_eq!(direct["result"]["structuredContent"]["code"], "upstream");
        assert_eq!(work(&direct_client), (0, 0, 0, 0, 0, 0, 0, 0));
        stable_stdio_precancel_is_zero_io().await;
        preview_stdio_precancel_is_zero_io().await;
    }

    #[test]
    fn direct_stable_preview_error_result_redaction_and_work_matrix_is_exact() {
        std::thread::Builder::new()
            .name("discussion-matrix".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("discussion matrix runtime")
                    .block_on(Box::pin(
                        assert_direct_stable_preview_error_result_redaction_and_work_matrix(),
                    ));
            })
            .expect("spawn discussion matrix thread")
            .join()
            .expect("discussion matrix thread");
    }

    #[test]
    #[ignore = "requires configured disposable real Anytype server"]
    #[serial_test::serial(disposable_anytype_api)]
    fn live_disposable_absent_attached_repeat_and_protocol_parity() {
        use anytype::{
            chats::MessageContent,
            test_util::{DisposableRun, unique_suffix, with_disposable_space_context},
        };

        std::thread::Builder::new()
            .name("discussion-toolset-live".to_owned())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("live discussions runtime")
                    .block_on(async {
                        let outcome = Box::pin(with_disposable_space_context(
                "any-mcp-discussions",
                |ctx| {
                    Box::pin(async move {
                        let suffix = unique_suffix();
                        let page = ctx
                            .client
                            .new_object(&ctx.space_id, "page")
                            .name(format!("mcp-discussion-page-{suffix}"))
                            .create()
                            .await?;
                        ctx.register_object(&page.id);
                        let note = ctx
                            .client
                            .new_object(&ctx.space_id, "note")
                            .name(format!("mcp-discussion-note-{suffix}"))
                            .create()
                            .await?;
                        ctx.register_object(&note.id);
                        let action = ctx
                            .client
                            .new_object(&ctx.space_id, "task")
                            .name(format!("mcp-discussion-action-{suffix}"))
                            .create()
                            .await?;
                        ctx.register_object(&action.id);
                        let other_space = ctx
                            .create_space_fixture(format!(
                                "any-mcp-discussions-other-{suffix}"
                            ))
                            .await?;

                        let server = candidate_server_from_client(ctx.client.clone(), false);
                        for object_id in [&page.id, &note.id] {
                            let http_before = ctx.client.http_metrics();
                            let work_before = ctx.client.attached_discussion_metrics();
                            let result = server
                                .dispatch_tool(
                                    CallToolRequestParams::new(OBJECT_DISCUSSION_GET)
                                        .with_arguments(
                                            json!({"space":ctx.space_id,"object_id":object_id})
                                                .as_object()
                                                .cloned()
                                                .expect("arguments"),
                                        ),
                                    &CancellationToken::new(),
                                )
                                .await
                                .expect("direct absent dispatch");
                            assert_eq!(result.is_error, Some(false));
                            assert_eq!(
                                result.structured_content.expect("absent output")["state"],
                                "absent"
                            );
                            let http_after = ctx.client.http_metrics();
                            let work_after = ctx.client.attached_discussion_metrics();
                            assert_eq!(
                                http_after.logical_operations - http_before.logical_operations,
                                1
                            );
                            assert!(
                                (1..=6).contains(
                                    &(http_after.physical_attempts
                                        - http_before.physical_attempts),
                                )
                            );
                            assert_eq!(
                                work_after.parent_get_attempts - work_before.parent_get_attempts,
                                1
                            );
                            assert_eq!(work_after.show_attempts - work_before.show_attempts, 1);
                            assert_eq!(work_after.accepted_shows - work_before.accepted_shows, 1);
                            assert_eq!(work_after.close_attempts - work_before.close_attempts, 1);
                            assert_eq!(work_after.close_successes - work_before.close_successes, 1);
                            assert_eq!(
                                work_after.write_dispatches - work_before.write_dispatches,
                                0
                            );
                        }

                        let action_before = work(&ctx.client);
                        let action_result = server
                            .dispatch_tool(
                                CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                                    json!({"space":ctx.space_id,"object_id":action.id})
                                        .as_object()
                                        .cloned()
                                        .expect("action arguments"),
                                ),
                                &CancellationToken::new(),
                            )
                            .await
                            .expect("direct action dispatch");
                        assert_eq!(action_result.is_error, Some(true));
                        assert_eq!(
                            action_result
                                .structured_content
                                .as_ref()
                                .and_then(|value| value["code"].as_str()),
                            Some("validation")
                        );
                        let action_after = work(&ctx.client);
                        assert_eq!(action_after.0 - action_before.0, 1);
                        assert_eq!(action_after.1 - action_before.1, 1);
                        assert_eq!(action_after.2 - action_before.2, 0);
                        assert!((1..=6).contains(&(action_after.7 - action_before.7)));

                        let scope_before = work(&ctx.client);
                        let scope_result = server
                            .dispatch_tool(
                                CallToolRequestParams::new(OBJECT_DISCUSSION_GET).with_arguments(
                                    json!({"space":other_space.id,"object_id":page.id})
                                        .as_object()
                                        .cloned()
                                        .expect("scope arguments"),
                                ),
                                &CancellationToken::new(),
                            )
                            .await
                            .expect("direct wrong-scope dispatch");
                        assert_eq!(scope_result.is_error, Some(true));
                        let scope_after = work(&ctx.client);
                        assert_eq!(scope_after.0 - scope_before.0, 1);
                        assert_eq!(scope_after.1 - scope_before.1, 1);
                        assert_eq!(scope_after.2 - scope_before.2, 0);
                        assert!((1..=6).contains(&(scope_after.7 - scope_before.7)));

                        let attached = ctx
                            .client
                            .attached_discussion(&ctx.space_id, &page.id)
                            .ensure()
                            .await?;
                        let discussion_id = attached
                            .discussion_id()
                            .expect("ensure returns attached discussion")
                            .to_owned();
                        ctx.register_object(&discussion_id);
                        for index in 0..2 {
                            let message_id = ctx
                                .client
                                .chats()
                                .in_space(&ctx.space_id)
                                .add_message(
                                    &discussion_id,
                                    MessageContent::new()
                                        .text(format!("mcp-discussion-{suffix}-{index}")),
                                )
                                .send()
                                .await?;
                            ctx.register_chat_message(&discussion_id, &message_id)?;
                        }

                        let chats = crate::chat_read_toolset::chat_read_registry();
                        let combined_selection = OptionalToolsetSelection::parse(
                            Some("chats,discussions".to_owned()),
                            &[DISCUSSIONS_REGISTRY.metadata(), chats.metadata()],
                        )
                        .expect("combined read selection");
                        let combined_runtime =
                            RuntimeContext::from_parts_with_profile_and_optional_toolsets(
                                ctx.client.clone(),
                                2,
                                Duration::from_secs(30),
                                StartupStatus {
                                    http_available: true,
                                    grpc_available: true,
                                },
                                ApplicationProfile::Compact,
                                false,
                                combined_selection,
                            );
                        let combined_registries: &'static [&'static dyn OptionalToolsetRegistry] =
                            Box::leak(vec![DISCUSSIONS_REGISTRY, chats].into_boxed_slice());
                        let combined_server = AnyMcpServer::new_with_optional_registries(
                            combined_runtime,
                            combined_registries,
                        )
                        .expect("combined chats/discussions server");
                        let handoff = combined_server
                            .dispatch_tool(
                                CallToolRequestParams::new("chat_message_list").with_arguments(
                                    json!({
                                        "space":ctx.space_id,
                                        "chat_id":discussion_id,
                                        "limit":8
                                    })
                                    .as_object()
                                    .cloned()
                                    .expect("chat arguments"),
                                ),
                                &CancellationToken::new(),
                            )
                            .await
                            .expect("discussion ID handoff to unchanged chat tool");
                        assert_eq!(handoff.is_error, Some(false));
                        assert_eq!(
                            handoff.structured_content.expect("message list output")["items"]
                                .as_array()
                                .map(Vec::len),
                            Some(2)
                        );

                        let arguments = json!({"space":ctx.space_id,"object_id":page.id});
                        for _ in 0..2 {
                            let output = server
                                .dispatch_tool(
                                    CallToolRequestParams::new(OBJECT_DISCUSSION_GET)
                                        .with_arguments(
                                            arguments.as_object().cloned().expect("arguments"),
                                        ),
                                    &CancellationToken::new(),
                                )
                                .await
                                .expect("direct attached dispatch")
                                .structured_content
                                .expect("attached output");
                            assert_eq!(output["state"], "attached");
                            assert!(
                                output["discussion_id"].as_str() == Some(discussion_id.as_str())
                            );
                        }
                        let stable_before = work(&ctx.client);
                        let stable = stable_tool_call(
                            candidate_server_from_client(ctx.client.clone(), false),
                            arguments.clone(),
                        )
                        .await;
                        let stable_after = work(&ctx.client);
                        assert_eq!(stable_after.0 - stable_before.0, 1);
                        assert_eq!(stable_after.1 - stable_before.1, 1);
                        assert_eq!(stable_after.2 - stable_before.2, 2);
                        assert_eq!(stable_after.3 - stable_before.3, 2);
                        assert_eq!(stable_after.4 - stable_before.4, 2);
                        assert_eq!(stable_after.5 - stable_before.5, 2);
                        assert_eq!(stable_after.6 - stable_before.6, 0);
                        assert!((1..=6).contains(&(stable_after.7 - stable_before.7)));
                        let preview_before = work(&ctx.client);
                        let preview = preview_tool_call(
                            candidate_server_from_client(ctx.client.clone(), false),
                            arguments,
                        )
                        .await;
                        let preview_after = work(&ctx.client);
                        assert_eq!(preview_after.0 - preview_before.0, 1);
                        assert_eq!(preview_after.1 - preview_before.1, 1);
                        assert_eq!(preview_after.2 - preview_before.2, 2);
                        assert_eq!(preview_after.3 - preview_before.3, 2);
                        assert_eq!(preview_after.4 - preview_before.4, 2);
                        assert_eq!(preview_after.5 - preview_before.5, 2);
                        assert_eq!(preview_after.6 - preview_before.6, 0);
                        assert!((1..=6).contains(&(preview_after.7 - preview_before.7)));
                        assert!(
                            stable["result"]["structuredContent"]
                                == preview["result"]["structuredContent"]
                        );
                        assert!(
                            stable["result"]["structuredContent"]["discussion_id"].as_str()
                                == Some(discussion_id.as_str())
                        );
                        assert_eq!(ctx.client.attached_discussion_metrics().write_dispatches, 1);
                        Ok(())
                    })
                },
            ))
            .await
            .expect("disposable discussion lifecycle");
                        assert!(matches!(outcome, DisposableRun::Completed(())));
                    });
            })
            .expect("spawn live discussions thread")
            .join()
            .expect("live discussions thread");
    }
}
