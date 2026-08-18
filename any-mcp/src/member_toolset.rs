// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Optional, read-only member discovery with minimized personal data.

use anytype::{
    members::{Member, MemberRole, MemberStatus},
    paged::PagedResult,
};
use rmcp::{
    model::{CallToolRequestMethod, CallToolRequestParams, CallToolResult, ErrorData},
    schemars::{JsonSchema, Schema, SchemaGenerator},
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    cursor::{CursorStore, CursorToken},
    discovery::DiscoveryReference,
    domain::{DisplayName, DomainValueError, EntityId},
    error::ToolError,
    handler_support::{
        HandlerError, HandlerOperationError, UpstreamPagination, begin_page, execute_handler,
        execute_prepared_handler, finish_page, validate_page_binding_size,
    },
    optional_toolsets::{
        OptionalRegistryFuture, OptionalRegistryTool, OptionalToolsetMetadata,
        OptionalToolsetRegistry,
    },
    pagination::{Page, PageLimit},
    protocol::{ToolProfile, WorkflowTool, workflow_tool},
    result::tool_error,
    runtime::{OperationContext, RuntimeContext},
    schema::SchemaContractError,
    server::decode_arguments,
    validation::{Omittable, optional_non_null_schema},
};

const MEMBER_LIST: &str = "member_list";
const MEMBER_GET: &str = "member_get";
const MEMBER_CATALOG_TOKEN_CEILING: usize = 1_500;

/// Input for one bounded member page.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberListInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Requested item limit, defaulting to 20.
    #[serde(default)]
    pub limit: PageLimit,
    /// Opaque continuation cursor for the same resolved space and limit.
    #[serde(default)]
    #[schemars(schema_with = "optional_cursor_schema")]
    pub cursor: Omittable<CursorToken>,
}

fn optional_cursor_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<CursorToken>(generator)
}

/// Input for one exact member read.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberGetInput {
    /// Unique space name or identifier.
    pub space: DiscoveryReference,
    /// Exact member profile identifier.
    pub member_id: EntityId,
}

/// Personal-data-minimized member metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberSummary {
    /// Stable member profile identifier.
    id: EntityId,
    /// Explicit space-local name, when the server supplied one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(default, schema_with = "optional_name_schema")]
    name: Option<DisplayName>,
    /// Closed permission role.
    role: MemberRoleSummary,
    /// Closed membership lifecycle state.
    status: MemberStatusSummary,
}

fn optional_name_schema(generator: &mut SchemaGenerator) -> Schema {
    optional_non_null_schema::<DisplayName>(generator)
}

/// Closed member roles exposed to MCP callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberRoleSummary {
    /// Read-only access.
    Viewer,
    /// Content editing access.
    Editor,
    /// Space administration access.
    Admin,
    /// Space ownership.
    Owner,
    /// No current permission.
    NoPermission,
}

/// Closed member lifecycle states exposed to MCP callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberStatusSummary {
    /// Invitation or join is pending.
    Joining,
    /// Membership is active.
    Active,
    /// Membership was removed.
    Removed,
    /// Invitation was declined.
    Declined,
    /// Removal is in progress.
    Removing,
    /// Invitation was canceled.
    Canceled,
}

/// Exact output for one member read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MemberGetOutput {
    /// Exact minimized member.
    member: MemberSummary,
}

/// Constructs the bounded `member_list` contract.
pub fn member_list_tool() -> Result<WorkflowTool<Page<MemberSummary>>, SchemaContractError> {
    workflow_tool::<MemberListInput, Page<MemberSummary>>(
        MEMBER_LIST,
        "List one bounded page of space members with space-local names, roles, and statuses. Network identities, global names, and icons are omitted.",
        ToolProfile::Read,
    )
}

/// Constructs the exact `member_get` contract.
pub fn member_get_tool() -> Result<WorkflowTool<MemberGetOutput>, SchemaContractError> {
    workflow_tool::<MemberGetInput, MemberGetOutput>(
        MEMBER_GET,
        "Get one exact space member with a space-local name, role, and status. Network identity, global name, and icon are omitted.",
        ToolProfile::Read,
    )
}

#[derive(Debug)]
struct MembersRegistry;

static MEMBERS_REGISTRY_IMPL: MembersRegistry = MembersRegistry;

/// Complete production descriptor for the `members` registry.
pub static MEMBERS_REGISTRY: &dyn OptionalToolsetRegistry = &MEMBERS_REGISTRY_IMPL;

/// Returns the complete production `members` registry.
#[must_use]
pub fn members_registry() -> &'static dyn OptionalToolsetRegistry {
    MEMBERS_REGISTRY
}

impl OptionalToolsetRegistry for MembersRegistry {
    fn metadata(&self) -> OptionalToolsetMetadata {
        OptionalToolsetMetadata::new("members", false)
    }

    fn tools(&self) -> Result<Vec<OptionalRegistryTool>, SchemaContractError> {
        Ok(vec![
            OptionalRegistryTool::read(member_get_tool()?),
            OptionalRegistryTool::read(member_list_tool()?),
        ])
    }

    fn scripted_scenario_ids(&self) -> &'static [&'static str] {
        &["members_direct"]
    }

    fn headless_scenario_ids(&self) -> &'static [&'static str] {
        &["members_headless"]
    }

    fn catalog_token_ceiling(&self) -> usize {
        MEMBER_CATALOG_TOKEN_CEILING
    }

    fn call_tool<'a>(
        &'a self,
        request: CallToolRequestParams,
        runtime: &'a RuntimeContext,
        cursors: &'a CursorStore,
        _protocol_version: &'a rmcp::model::ProtocolVersion,
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            match request.name.as_ref() {
                MEMBER_LIST => {
                    let input = decode_arguments::<MemberListInput>(request.arguments)?;
                    Ok(Box::pin(member_list(runtime, cursors, input, cancellation)).await)
                }
                MEMBER_GET => {
                    let input = decode_arguments::<MemberGetInput>(request.arguments)?;
                    Ok(Box::pin(member_get(runtime, input, cancellation)).await)
                }
                _ => Err(ErrorData::method_not_found::<CallToolRequestMethod>()),
            }
        })
    }
}

#[derive(Serialize)]
struct RawMemberPageParams<'a> {
    space: &'a str,
}

#[derive(Serialize)]
struct ResolvedMemberPageParams<'a> {
    space_id: &'a str,
}

fn member_list<'a>(
    runtime: &'a RuntimeContext,
    cursors: &'a CursorStore,
    input: MemberListInput,
    cancellation: &'a CancellationToken,
) -> OptionalRegistryFuture<'a, CallToolResult> {
    Box::pin(async move {
        let Ok(contract) = member_list_tool() else {
            return tool_error(&ToolError::upstream());
        };
        if let Err(error) = validate_page_binding_size(
            MEMBER_LIST,
            input.limit,
            &RawMemberPageParams {
                space: input.space.as_str(),
            },
        ) {
            return tool_error(error.tool_error());
        }
        let client = runtime.client().clone();
        execute_prepared_handler(
            runtime,
            &contract,
            OperationContext::new(MEMBER_LIST),
            cancellation,
            Box::pin(async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                let request = begin_page(
                    cursors,
                    input.cursor.as_ref(),
                    MEMBER_LIST,
                    input.limit,
                    &ResolvedMemberPageParams {
                        space_id: &space_id,
                    },
                )?;
                let page = client
                    .members(space_id)
                    .limit(u32::from(input.limit.get()))
                    .offset(request.offset().get())
                    .list()
                    .await?;
                Ok::<_, HandlerOperationError>((page, request))
            }),
            |(page, request): (PagedResult<Member>, _)| {
                Box::pin(async move {
                    let upstream = UpstreamPagination::try_from(&page.pagination)?;
                    let items = page
                        .items
                        .iter()
                        .map(convert_member)
                        .collect::<Result<Vec<_>, _>>()?;
                    finish_page(cursors, request, upstream, items)
                })
            },
        )
        .await
    })
}

fn member_get<'a>(
    runtime: &'a RuntimeContext,
    input: MemberGetInput,
    cancellation: &'a CancellationToken,
) -> OptionalRegistryFuture<'a, CallToolResult> {
    Box::pin(async move {
        let Ok(contract) = member_get_tool() else {
            return tool_error(&ToolError::upstream());
        };
        let client = runtime.client().clone();
        let expected_id = input.member_id.as_str().to_owned();
        execute_handler(
            runtime,
            &contract,
            OperationContext::new(MEMBER_GET),
            cancellation,
            Box::pin(async move {
                let space_id = client.resolve_space_id(input.space.as_str()).await?;
                client
                    .member(space_id, input.member_id.as_str())
                    .get()
                    .await
            }),
            move |member| {
                Box::pin(async move {
                    if member.id != expected_id {
                        return Err(HandlerError::new(ToolError::upstream()));
                    }
                    Ok(MemberGetOutput {
                        member: convert_member(&member)?,
                    })
                })
            },
        )
        .await
    })
}

fn convert_member(member: &Member) -> Result<MemberSummary, HandlerError> {
    Ok(MemberSummary {
        id: EntityId::new(member.id.clone()).map_err(domain_error)?,
        name: member
            .name
            .clone()
            .map(DisplayName::new)
            .transpose()
            .map_err(domain_error)?,
        role: match member.role {
            MemberRole::Viewer => MemberRoleSummary::Viewer,
            MemberRole::Editor => MemberRoleSummary::Editor,
            MemberRole::Admin => MemberRoleSummary::Admin,
            MemberRole::Owner => MemberRoleSummary::Owner,
            MemberRole::NoPermission => MemberRoleSummary::NoPermission,
        },
        status: match member.status {
            MemberStatus::Joining => MemberStatusSummary::Joining,
            MemberStatus::Active => MemberStatusSummary::Active,
            MemberStatus::Removed => MemberStatusSummary::Removed,
            MemberStatus::Declined => MemberStatusSummary::Declined,
            MemberStatus::Removing => MemberStatusSummary::Removing,
            MemberStatus::Canceled => MemberStatusSummary::Canceled,
        },
    })
}

fn domain_error(_: DomainValueError) -> HandlerError {
    HandlerError::new(ToolError::upstream())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, future::Future, time::Duration};

    use super::*;
    use crate::runtime::StartupStatus;
    use crate::{
        config::ApplicationProfile,
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        schema::{input_schema, output_schema},
        server::AnyMcpServer,
    };
    use anytype::{
        objects::{DataModel, Icon},
        prelude::{AnytypeClient, ClientConfig, HttpCredentials},
    };
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const MEMBER_1: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a";
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/members-token-budget.json");

    fn runtime_with_members(endpoint: &str, selected: bool, read_only: bool) -> RuntimeContext {
        runtime_config(endpoint, selected, read_only, Duration::from_secs(2), 5)
    }

    fn runtime_config(
        endpoint: &str,
        selected: bool,
        read_only: bool,
        request_timeout: Duration,
        rate_limit_max_retries: u32,
    ) -> RuntimeContext {
        let client =
            AnytypeClient::with_config(member_client_config(endpoint, rate_limit_max_retries))
                .unwrap();
        runtime_from_client(client, selected, read_only, request_timeout)
    }

    fn member_client_config(endpoint: &str, rate_limit_max_retries: u32) -> ClientConfig {
        ClientConfig {
            base_url: Some(endpoint.to_owned()),
            keystore: Some("env".to_owned()),
            keystore_service: Some("member-toolset-test".to_owned()),
            app_name: "member-toolset-test".to_owned(),
            disable_cache: true,
            rate_limit_max_retries,
            ..ClientConfig::default()
        }
    }

    fn runtime_from_client(
        client: AnytypeClient,
        selected: bool,
        read_only: bool,
        request_timeout: Duration,
    ) -> RuntimeContext {
        client.set_api_key(HttpCredentials::new("fixture-token"));
        let toolsets = OptionalToolsetSelection::parse(
            selected.then(|| "members".to_owned()),
            &production_optional_metadata(),
        )
        .unwrap();
        RuntimeContext::from_parts_with_profile_and_optional_toolsets(
            client,
            1,
            request_timeout,
            StartupStatus {
                http_available: true,
                grpc_available: false,
            },
            ApplicationProfile::Compact,
            read_only,
            toolsets,
        )
    }

    fn canonical_json(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
            }
            scalar => scalar,
        }
    }

    fn canonical_compact(value: serde_json::Value) -> String {
        serde_json::to_string(&canonical_json(value)).unwrap()
    }

    fn token_count(tokenizer: &CoreBPE, value: serde_json::Value) -> usize {
        tokenizer
            .encode_with_special_tokens(&canonical_compact(value))
            .len()
    }

    fn tools_list_value(server: &AnyMcpServer) -> serde_json::Value {
        serde_json::to_value(crate::server::stable_list_tools_result(
            server.tools().to_vec(),
        ))
        .unwrap()
    }

    fn member_token_budget() -> serde_json::Value {
        let tokenizer = o200k_base().unwrap();
        let base =
            AnyMcpServer::new(runtime_with_members("http://127.0.0.1:1", false, false)).unwrap();
        let selected =
            AnyMcpServer::new(runtime_with_members("http://127.0.0.1:1", true, false)).unwrap();
        let selected_read_only =
            AnyMcpServer::new(runtime_with_members("http://127.0.0.1:1", true, true)).unwrap();
        let base_value = tools_list_value(&base);
        let base_json = canonical_compact(base_value.clone());
        let base_hash = Sha256::digest(base_json.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let per_tool = selected
            .tools()
            .iter()
            .filter(|tool| matches!(tool.name.as_ref(), MEMBER_GET | MEMBER_LIST))
            .map(|tool| {
                (
                    tool.name.to_string(),
                    token_count(&tokenizer, serde_json::to_value(tool).unwrap()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let maximum_page =
            Page::new((0..100).map(maximum_member).collect::<Vec<_>>(), None).unwrap();
        let representative = member_list_tool().unwrap().success(&maximum_page).unwrap();
        json!({
            "tokenizer": "tiktoken o200k_base (tiktoken-rs 0.12.0)",
            "base_catalog_sha256": base_hash,
            "base_catalog_tokens": token_count(&tokenizer, base_value),
            "selected": ["members"],
            "member_catalog_ceiling_tokens": MEMBER_CATALOG_TOKEN_CEILING,
            "per_tool_tokens": per_tool,
            "composed_total_tokens": token_count(&tokenizer, tools_list_value(&selected)),
            "read_only_composed_total_tokens": token_count(
                &tokenizer,
                tools_list_value(&selected_read_only),
            ),
            "representative_max_result_tokens": token_count(
                &tokenizer,
                serde_json::to_value(representative).unwrap(),
            ),
        })
    }

    fn maximum_member(index: usize) -> MemberSummary {
        const ID_ALPHABET: &[u8] =
            b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghijklmnopqrstuvwxyz~-";
        const NAME_ALPHABET: &[char] = &[
            '漢', '字', '界', '語', '🚀', '🧭', 'Ω', 'Ж', 'א', 'ق', 'क', 'ก', 'あ', 'カ', '가', 'ñ',
        ];
        let id_prefix = format!("m{index:03}");
        let id = id_prefix
            .chars()
            .chain((id_prefix.len()..256).map(|position| {
                let offset = index
                    .checked_mul(17)
                    .and_then(|value| value.checked_add(position))
                    .expect("small deterministic fixture offset");
                char::from(ID_ALPHABET[offset % ID_ALPHABET.len()])
            }))
            .collect::<String>();
        let name = (0..512)
            .map(|position| NAME_ALPHABET[(index + position) % NAME_ALPHABET.len()])
            .collect::<String>();
        MemberSummary {
            id: EntityId::new(id).expect("valid 256-character token-dense member id"),
            name: Some(
                DisplayName::new(name).expect("valid 512-scalar token-dense local member name"),
            ),
            role: MemberRoleSummary::Owner,
            status: MemberStatusSummary::Removing,
        }
    }

    fn member(role: MemberRole, status: MemberStatus) -> Member {
        Member {
            object: DataModel::Member,
            global_name: Some("global-secret".to_owned()),
            icon: Some(Icon::File {
                file: "private-icon".to_owned(),
            }),
            id: "member-1".to_owned(),
            identity: Some("network-secret".to_owned()),
            name: Some("Local name".to_owned()),
            role,
            status,
        }
    }

    #[test]
    fn production_registry_is_http_only_and_complete() {
        let metadata = production_optional_metadata();
        assert!(
            metadata
                .iter()
                .any(|entry| entry.name == "members" && !entry.requires_grpc)
        );
        let tools = MEMBERS_REGISTRY.tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(MEMBERS_REGISTRY.scripted_scenario_ids(), ["members_direct"]);
        assert_eq!(
            MEMBERS_REGISTRY.headless_scenario_ids(),
            ["members_headless"]
        );
        assert_eq!(MEMBERS_REGISTRY.catalog_token_ceiling(), 1_500);
    }

    #[tokio::test]
    async fn members_are_absent_by_default_and_read_only_when_selected() {
        let absent =
            AnyMcpServer::new(runtime_with_members("http://127.0.0.1:1", false, false)).unwrap();
        assert!(
            absent
                .tools()
                .iter()
                .all(|tool| !matches!(tool.name.as_ref(), MEMBER_GET | MEMBER_LIST))
        );
        let error = absent
            .dispatch_tool(
                CallToolRequestParams::new(MEMBER_GET),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code.0, -32601);

        for read_only in [false, true] {
            let selected =
                AnyMcpServer::new(runtime_with_members("http://127.0.0.1:1", true, read_only))
                    .unwrap();
            let names = selected
                .tools()
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>();
            assert!(names.contains(&MEMBER_GET));
            assert!(names.contains(&MEMBER_LIST));
            assert!(names.contains(&"optional_toolset_status"));
            let status = selected
                .dispatch_tool(
                    CallToolRequestParams::new("optional_toolset_status")
                        .with_arguments(serde_json::Map::new()),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                status.structured_content.unwrap(),
                json!({
                    "configured_toolsets": ["members"],
                    "active_toolsets": ["members"]
                })
            );
        }
    }

    #[test]
    fn member_catalog_and_maximum_result_match_reviewed_token_budget() {
        let actual = canonical_json(member_token_budget());
        let reviewed = canonical_json(serde_json::from_str(TOKEN_BUDGET_SNAPSHOT).unwrap());
        assert_eq!(actual, reviewed, "members token budget drifted");
        let domain_tokens = actual["per_tool_tokens"]
            .as_object()
            .unwrap()
            .values()
            .map(|value| value.as_u64().unwrap() as usize)
            .sum::<usize>();
        assert!(domain_tokens <= MEMBER_CATALOG_TOKEN_CEILING);
    }

    #[test]
    fn member_wire_contracts_are_strict_and_bounded() {
        let list_input = input_schema::<MemberListInput>().unwrap();
        assert_eq!(list_input["$defs"]["PageLimit"]["minimum"], 1);
        assert_eq!(list_input["$defs"]["PageLimit"]["maximum"], 100);
        assert_eq!(list_input["additionalProperties"], false);
        let get_input = input_schema::<MemberGetInput>().unwrap();
        assert_eq!(
            get_input["$defs"]["EntityId"]["pattern"],
            "^(?!\\.{1,2}$)[A-Za-z0-9._~-]+$"
        );
        let page = output_schema::<Page<MemberSummary>>().unwrap();
        assert_eq!(page["properties"]["items"]["maxItems"], 100);
        let exact = output_schema::<MemberGetOutput>().unwrap();
        let serialized = serde_json::to_string(exact.as_ref()).unwrap();
        for forbidden in ["identity", "global_name", "globalName", "icon"] {
            assert!(
                !serialized.contains(forbidden),
                "forbidden field {forbidden}"
            );
        }
    }

    #[test]
    fn projection_uses_only_explicit_local_name_and_closed_values() {
        let projected = convert_member(&member(MemberRole::Admin, MemberStatus::Removing)).unwrap();
        assert_eq!(
            serde_json::to_value(projected).unwrap(),
            json!({
                "id": "member-1",
                "name": "Local name",
                "role": "admin",
                "status": "removing"
            })
        );

        let mut without_local_name = member(MemberRole::NoPermission, MemberStatus::Canceled);
        without_local_name.name = None;
        assert_eq!(
            serde_json::to_value(convert_member(&without_local_name).unwrap()).unwrap(),
            json!({
                "id": "member-1",
                "role": "no_permission",
                "status": "canceled"
            })
        );
    }

    #[test]
    fn every_upstream_role_and_status_maps_without_fallback() {
        let roles = [
            (MemberRole::Viewer, "viewer"),
            (MemberRole::Editor, "editor"),
            (MemberRole::Admin, "admin"),
            (MemberRole::Owner, "owner"),
            (MemberRole::NoPermission, "no_permission"),
        ];
        for (role, expected) in roles {
            let value =
                serde_json::to_value(convert_member(&member(role, MemberStatus::Active)).unwrap())
                    .unwrap();
            assert_eq!(value["role"], expected);
        }
        let statuses = [
            (MemberStatus::Joining, "joining"),
            (MemberStatus::Active, "active"),
            (MemberStatus::Removed, "removed"),
            (MemberStatus::Declined, "declined"),
            (MemberStatus::Removing, "removing"),
            (MemberStatus::Canceled, "canceled"),
        ];
        for (status, expected) in statuses {
            let value =
                serde_json::to_value(convert_member(&member(MemberRole::Editor, status)).unwrap())
                    .unwrap();
            assert_eq!(value["status"], expected);
        }
    }

    #[test]
    fn malformed_upstream_identity_and_name_fail_closed() {
        let mut malformed = member(MemberRole::Owner, MemberStatus::Active);
        malformed.id = "bad/member".to_owned();
        assert!(convert_member(&malformed).is_err());
        malformed.id = "member-1".to_owned();
        malformed.name = Some("x".repeat(513));
        assert!(convert_member(&malformed).is_err());
    }

    fn selected_server(endpoint: &str) -> AnyMcpServer {
        AnyMcpServer::new(runtime_with_members(endpoint, true, false))
            .expect("selected members registry server")
    }

    fn dispatch<'a>(
        server: &'a AnyMcpServer,
        name: &'static str,
        arguments: serde_json::Value,
        cancellation: &'a CancellationToken,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<CallToolResult, ErrorData>> + Send + 'a>>
    {
        Box::pin(async move {
            server
                .dispatch_tool(
                    CallToolRequestParams::new(name).with_arguments(
                        arguments
                            .as_object()
                            .cloned()
                            .expect("fixture arguments object"),
                    ),
                    cancellation,
                )
                .await
        })
    }

    fn assert_tool_error(result: &CallToolResult, expected_code: &str) {
        assert_eq!(result.is_error, Some(true), "tool error result: {result:?}");
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .and_then(|value| value["code"].as_str()),
            Some(expected_code),
            "tool error category: {result:?}"
        );
        let structured = result
            .structured_content
            .as_ref()
            .expect("tool error structured content");
        let compact = structured.to_string();
        assert_eq!(
            result.content[0]
                .as_text()
                .map(|content| content.text.as_str()),
            Some(compact.as_str()),
            "text and structured errors remain identical"
        );
    }

    #[tokio::test]
    async fn direct_router_enforces_strict_runtime_inputs_without_io() {
        let server = selected_server("http://127.0.0.1:1");
        for (tool, arguments) in [
            (
                MEMBER_LIST,
                json!({"space": SPACE_ID, "limit": 1, "filter": "forbidden"}),
            ),
            (MEMBER_LIST, json!({"space": SPACE_ID, "limit": 101})),
            (MEMBER_LIST, json!({"space": SPACE_ID, "cursor": null})),
            (
                MEMBER_GET,
                json!({"space": SPACE_ID, "member_id": MEMBER_1, "extra": true}),
            ),
            (
                MEMBER_GET,
                json!({"space": SPACE_ID, "member_id": "bad/member"}),
            ),
        ] {
            let error = dispatch(&server, tool, arguments, &CancellationToken::new())
                .await
                .expect_err("schema-invalid runtime input");
            assert_eq!(error.code.0, -32602, "invalid params for {tool}");
        }
        assert_eq!(
            server.runtime().client().http_metrics().logical_operations,
            0
        );
        assert_eq!(
            server.runtime().client().http_metrics().physical_attempts,
            0
        );
    }

    #[tokio::test]
    async fn direct_router_precancellation_is_zero_io() {
        let server = selected_server("http://127.0.0.1:1");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let cancelled = dispatch(
            &server,
            MEMBER_LIST,
            json!({"space": SPACE_ID}),
            &cancellation,
        )
        .await
        .expect("cancelled tool result");
        assert_tool_error(&cancelled, "upstream");
        let metrics = server.runtime().client().http_metrics();
        assert_eq!(metrics.logical_operations, 0);
        assert_eq!(metrics.physical_attempts, 0);
    }
}
