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
        cancellation: &'a CancellationToken,
    ) -> OptionalRegistryFuture<'a, Result<CallToolResult, ErrorData>> {
        Box::pin(async move {
            match request.name.as_ref() {
                MEMBER_LIST => {
                    let input = decode_arguments::<MemberListInput>(request.arguments)?;
                    Ok(member_list(runtime, cursors, input, cancellation).await)
                }
                MEMBER_GET => {
                    let input = decode_arguments::<MemberGetInput>(request.arguments)?;
                    Ok(member_get(runtime, input, cancellation).await)
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

async fn member_list(
    runtime: &RuntimeContext,
    cursors: &CursorStore,
    input: MemberListInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
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
        async move {
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
        },
        |(page, request): (PagedResult<Member>, _)| async move {
            let upstream = UpstreamPagination::try_from(&page.pagination)?;
            let items = page
                .items
                .iter()
                .map(convert_member)
                .collect::<Result<Vec<_>, _>>()?;
            finish_page(cursors, request, upstream, items)
        },
    )
    .await
}

async fn member_get(
    runtime: &RuntimeContext,
    input: MemberGetInput,
    cancellation: &CancellationToken,
) -> CallToolResult {
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
        async move {
            let space_id = client.resolve_space_id(input.space.as_str()).await?;
            client
                .member(space_id, input.member_id.as_str())
                .get()
                .await
        },
        move |member| async move {
            if member.id != expected_id {
                return Err(HandlerError::new(ToolError::upstream()));
            }
            Ok(MemberGetOutput {
                member: convert_member(&member)?,
            })
        },
    )
    .await
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
    use std::{collections::BTreeMap, future::Future, sync::Arc, time::Duration};

    use super::*;
    use crate::{
        config::ApplicationProfile,
        optional_toolsets::{OptionalToolsetSelection, production_optional_metadata},
        schema::{input_schema, output_schema},
        server::AnyMcpServer,
    };
    use anytype::prelude::{AnytypeClient, ClientConfig, HttpCredentials};
    use rmcp::model::ListToolsResult;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tiktoken_rs::{CoreBPE, o200k_base};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::timeout,
    };

    use crate::runtime::StartupStatus;

    const SPACE_ID: &str =
        "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4y.2tq5w93cr6oe7";
    const MEMBER_1: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4a";
    const MEMBER_2: &str = "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4b";
    const TOKEN_BUDGET_SNAPSHOT: &str =
        include_str!("../tests/snapshots/members-token-budget.json");

    fn run_production_router_test<F, Fut>(test: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        std::thread::Builder::new()
            .name("members-production-router".to_owned())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("members production-router test runtime")
                    .block_on(test());
            })
            .expect("spawn members production-router test")
            .join()
            .expect("members production-router test thread");
    }

    enum FixtureReply {
        Json {
            status: &'static str,
            headers: &'static str,
            body: serde_json::Value,
        },
        Raw {
            status: &'static str,
            headers: &'static str,
            body: String,
        },
        Hang(Duration),
    }

    struct ExpectedRequest {
        path: String,
        query: BTreeMap<String, String>,
        reply: FixtureReply,
    }

    impl ExpectedRequest {
        fn new(path: impl Into<String>, query: &[(&str, &str)], body: serde_json::Value) -> Self {
            Self {
                path: path.into(),
                query: query
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
                reply: FixtureReply::Json {
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
            body: serde_json::Value,
        ) -> Self {
            let mut request = Self::new(path, query, body);
            let FixtureReply::Json { status: value, .. } = &mut request.reply else {
                unreachable!("new fixture request has a JSON reply")
            };
            *value = status;
            request
        }

        fn status_with_headers(
            path: impl Into<String>,
            query: &[(&str, &str)],
            status: &'static str,
            headers: &'static str,
            body: serde_json::Value,
        ) -> Self {
            let mut request = Self::status(path, query, status, body);
            let FixtureReply::Json { headers: value, .. } = &mut request.reply else {
                unreachable!("status fixture request has a JSON reply")
            };
            *value = headers;
            request
        }

        fn raw(path: impl Into<String>, query: &[(&str, &str)], body: impl Into<String>) -> Self {
            let mut request = Self::new(path, query, serde_json::Value::Null);
            request.reply = FixtureReply::Raw {
                status: "200 OK",
                headers: "",
                body: body.into(),
            };
            request
        }

        fn hang(path: impl Into<String>, query: &[(&str, &str)], duration: Duration) -> Self {
            let mut request = Self::new(path, query, serde_json::Value::Null);
            request.reply = FixtureReply::Hang(duration);
            request
        }
    }

    struct HttpFixture {
        endpoint: String,
        task: JoinHandle<()>,
    }

    impl HttpFixture {
        async fn start(expected: Vec<ExpectedRequest>) -> Self {
            Self::start_inner(expected, None).await
        }

        async fn start_and_reject_extra(
            expected: Vec<ExpectedRequest>,
            no_extra_window: Duration,
        ) -> Self {
            Self::start_inner(expected, Some(no_extra_window)).await
        }

        async fn start_inner(
            expected: Vec<ExpectedRequest>,
            no_extra_window: Option<Duration>,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                for expected in expected {
                    let (mut socket, _) = timeout(Duration::from_secs(15), listener.accept())
                        .await
                        .expect("member fixture request timeout")
                        .expect("member fixture accept");
                    let mut request = Vec::new();
                    loop {
                        let mut chunk = [0_u8; 1024];
                        let read = socket.read(&mut chunk).await.expect("member fixture read");
                        assert!(read > 0, "member fixture closed before headers");
                        request.extend_from_slice(&chunk[..read]);
                        assert!(
                            request.len() <= 64 * 1024,
                            "member fixture request too large"
                        );
                        if request.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                    let request = std::str::from_utf8(&request).expect("ASCII member request");
                    let target = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_ascii_whitespace().nth(1))
                        .expect("member request target");
                    let (path, raw_query) = target
                        .split_once('?')
                        .map_or((target, ""), |(path, query)| (path, query));
                    assert_eq!(path, expected.path);
                    let query = url::form_urlencoded::parse(raw_query.as_bytes())
                        .map(|(key, value)| (key.into_owned(), value.into_owned()))
                        .collect::<BTreeMap<_, _>>();
                    assert_eq!(query, expected.query, "query for {path}");
                    match expected.reply {
                        FixtureReply::Json {
                            status,
                            headers,
                            body,
                        } => {
                            let body = body.to_string();
                            write_response(&mut socket, status, headers, &body).await;
                        }
                        FixtureReply::Raw {
                            status,
                            headers,
                            body,
                        } => write_response(&mut socket, status, headers, &body).await,
                        FixtureReply::Hang(duration) => {
                            std::mem::drop(tokio::spawn(async move {
                                tokio::time::sleep(duration).await;
                                drop(socket);
                            }));
                        }
                    }
                }
                if let Some(window) = no_extra_window {
                    assert!(
                        timeout(window, listener.accept()).await.is_err(),
                        "fixture unexpectedly received an extra physical attempt"
                    );
                }
            });
            Self { endpoint, task }
        }

        async fn finish(self) {
            self.task.await.expect("member fixture task");
        }
    }

    async fn write_response(
        socket: &mut tokio::net::TcpStream,
        status: &str,
        headers: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("member fixture response write");
        socket
            .shutdown()
            .await
            .expect("member fixture response shutdown");
    }

    fn runtime(endpoint: &str) -> RuntimeContext {
        runtime_with_members(endpoint, false, false)
    }

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

    fn runtime_config_with_transport_timeout(
        endpoint: &str,
        request_timeout: Duration,
        transport_timeout: Duration,
    ) -> RuntimeContext {
        let client = AnytypeClient::with_client(
            reqwest::Client::builder()
                .no_proxy()
                .timeout(transport_timeout),
            member_client_config(endpoint, 5),
        )
        .unwrap();
        runtime_from_client(client, true, false, request_timeout)
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
        serde_json::to_value(ListToolsResult::with_all_items(server.tools().to_vec())).unwrap()
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

    fn page(
        items: Vec<serde_json::Value>,
        offset: usize,
        limit: usize,
        total: usize,
    ) -> serde_json::Value {
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

    fn push_six_attempt_success(
        requests: &mut Vec<ExpectedRequest>,
        path: impl Into<String>,
        query: &[(&str, &str)],
        success: serde_json::Value,
    ) {
        let path = path.into();
        for attempt in 0..5 {
            requests.push(if attempt != 1 {
                ExpectedRequest::status_with_headers(
                    &path,
                    query,
                    "429 Too Many Requests",
                    "RateLimit-Reset: 0\r\n",
                    json!({"class": "rate-limit"}),
                )
            } else {
                ExpectedRequest::status(
                    &path,
                    query,
                    "504 Gateway Timeout",
                    json!({"class": "retryable-status"}),
                )
            });
        }
        requests.push(ExpectedRequest::new(path, query, success));
    }

    fn space_page() -> serde_json::Value {
        page(
            vec![json!({"id": SPACE_ID, "name": "Workspace", "object": "space"})],
            0,
            99,
            1,
        )
    }

    fn member_value(id: &str, name: Option<&str>, role: &str, status: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": name,
            "global_name": "must-not-project",
            "identity": "must-not-project",
            "icon": {"url": "must-not-project"},
            "role": role,
            "status": status
        })
    }

    fn member(role: MemberRole, status: MemberStatus) -> Member {
        Member {
            global_name: Some("global-secret".to_owned()),
            icon: Some(json!({"url": "private-icon"})),
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

    #[tokio::test]
    async fn list_cursor_and_get_use_exact_scoped_requests_and_minimal_outputs() {
        let first_member = member_value(MEMBER_1, Some("One"), "owner", "active");
        let second_member = member_value(MEMBER_2, None, "editor", "joining");
        let members_path = format!("/v1/spaces/{SPACE_ID}/members");
        let fixture = HttpFixture::start(vec![
            ExpectedRequest::new("/v1/spaces", &[("limit", "99")], space_page()),
            ExpectedRequest::new(
                &members_path,
                &[("limit", "1")],
                page(vec![first_member.clone()], 0, 1, 2),
            ),
            ExpectedRequest::new("/v1/spaces", &[("limit", "99")], space_page()),
            ExpectedRequest::new(
                &members_path,
                &[("limit", "1"), ("offset", "1")],
                page(vec![second_member], 1, 1, 2),
            ),
            ExpectedRequest::new("/v1/spaces", &[("limit", "99")], space_page()),
            ExpectedRequest::new(
                format!("{members_path}/{MEMBER_1}"),
                &[],
                json!({"member": first_member}),
            ),
        ])
        .await;
        let runtime = runtime(&fixture.endpoint);
        let cursors = Arc::new(CursorStore::new().unwrap());
        let first = member_list(
            &runtime,
            &cursors,
            serde_json::from_value(json!({"space": "Workspace", "limit": 1})).unwrap(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(first.is_error, Some(false), "first member page: {first:?}");
        let first_output = first.structured_content.unwrap();
        assert_eq!(
            first_output["items"],
            json!([{"id":MEMBER_1,"name":"One","role":"owner","status":"active"}])
        );
        let cursor = first_output["next_cursor"].as_str().unwrap().to_owned();
        let second = member_list(
            &runtime,
            &cursors,
            serde_json::from_value(json!({
                "space": "Workspace",
                "limit": 1,
                "cursor": cursor
            }))
            .unwrap(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(second.is_error, Some(false));
        assert_eq!(
            second.structured_content.unwrap(),
            json!({"items":[{"id":MEMBER_2,"role":"editor","status":"joining"}]})
        );
        let exact = member_get(
            &runtime,
            serde_json::from_value(json!({
                "space": "Workspace",
                "member_id": MEMBER_1
            }))
            .unwrap(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(exact.is_error, Some(false), "exact member: {exact:?}");
        assert_eq!(
            exact.structured_content.unwrap(),
            json!({"member":{"id":MEMBER_1,"name":"One","role":"owner","status":"active"}})
        );
        fixture.finish().await;
    }

    fn selected_server(endpoint: &str) -> AnyMcpServer {
        AnyMcpServer::new(runtime_with_members(endpoint, true, false))
            .expect("selected members fixture server")
    }

    async fn dispatch(
        server: &AnyMcpServer,
        name: &'static str,
        arguments: serde_json::Value,
        cancellation: &CancellationToken,
    ) -> Result<CallToolResult, ErrorData> {
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
    async fn direct_router_rejects_cursor_reuse_across_bound_space_and_limit() {
        Box::pin(async {
        let members_path = format!("/v1/spaces/{SPACE_ID}/members");
        let fixture = HttpFixture::start(vec![ExpectedRequest::new(
            &members_path,
            &[("limit", "1")],
            page(
                vec![member_value(MEMBER_1, Some("One"), "owner", "active")],
                0,
                1,
                2,
            ),
        )])
        .await;
        let server = selected_server(&fixture.endpoint);
        let first = dispatch(
            &server,
            MEMBER_LIST,
            json!({"space": SPACE_ID, "limit": 1}),
            &CancellationToken::new(),
        )
        .await
        .expect("first cursor page");
        let cursor = first
            .structured_content
            .as_ref()
            .and_then(|value| value["next_cursor"].as_str())
            .expect("continuation cursor")
            .to_owned();
        for arguments in [
            json!({"space": SPACE_ID, "limit": 2, "cursor": cursor.clone()}),
            json!({"space": "bafyreid5fvqlnsobih2keakcxjrrlpmly6kf37klzjzen4ibfdgalcdp4z.2tq5w93cr6oe7", "limit": 1, "cursor": cursor}),
        ] {
            let rejected = dispatch(&server, MEMBER_LIST, arguments, &CancellationToken::new())
                .await
                .expect("cursor mismatch is a tool error");
            assert_tool_error(&rejected, "validation");
        }
        let metrics = server.runtime().client().http_metrics();
        assert_eq!(metrics.logical_operations, 1);
        assert_eq!(metrics.physical_attempts, 1);
        fixture.finish().await;
        })
        .await;
    }

    #[test]
    fn direct_router_preserves_ambiguity_and_maps_authorization_failures() {
        run_production_router_test(|| async {
            let ambiguous = HttpFixture::start(vec![ExpectedRequest::new(
                "/v1/spaces",
                &[("limit", "99")],
                page(
                    vec![
                        json!({"id": "space-alpha", "name": "Shared", "object": "space"}),
                        json!({"id": "space-beta", "name": "shared", "object": "space"}),
                    ],
                    0,
                    99,
                    2,
                ),
            )])
            .await;
            let server = selected_server(&ambiguous.endpoint);
            let result = dispatch(
                &server,
                MEMBER_LIST,
                json!({"space": "Shared"}),
                &CancellationToken::new(),
            )
            .await
            .expect("ambiguity is a tool error");
            assert_tool_error(&result, "ambiguous");
            assert_eq!(
                result.structured_content.as_ref().unwrap()["candidates"],
                json!([
                    {"id": "space-alpha", "name": "Shared"},
                    {"id": "space-beta", "name": "shared"}
                ])
            );
            ambiguous.finish().await;

            for status in ["401 Unauthorized", "403 Forbidden"] {
                let fixture = HttpFixture::start(vec![ExpectedRequest::status(
                    format!("/v1/spaces/{SPACE_ID}/members"),
                    &[("limit", "20")],
                    status,
                    json!({"credential": "DO-NOT-RETURN", "member": MEMBER_1}),
                )])
                .await;
                let server = selected_server(&fixture.endpoint);
                let result = dispatch(
                    &server,
                    MEMBER_LIST,
                    json!({"space": SPACE_ID}),
                    &CancellationToken::new(),
                )
                .await
                .expect("authorization is a tool error");
                assert_tool_error(&result, "authentication");
                let wire = serde_json::to_string(&result).expect("authorization wire");
                assert!(!wire.contains("DO-NOT-RETURN"));
                assert!(!wire.contains(MEMBER_1));
                fixture.finish().await;
            }
        });
    }

    #[test]
    fn direct_router_rejects_exact_member_response_identity_mismatch() {
        run_production_router_test(|| async {
            let path = format!("/v1/spaces/{SPACE_ID}/members/{MEMBER_1}");
            let fixture = HttpFixture::start(vec![ExpectedRequest::new(
                &path,
                &[],
                json!({
                    "member": member_value(MEMBER_2, Some("Wrong"), "owner", "active")
                }),
            )])
            .await;
            let server = selected_server(&fixture.endpoint);
            let result = dispatch(
                &server,
                MEMBER_GET,
                json!({"space": SPACE_ID, "member_id": MEMBER_1}),
                &CancellationToken::new(),
            )
            .await
            .expect("mismatched identity tool result");
            assert_tool_error(&result, "upstream");
            fixture.finish().await;
        });
    }

    #[test]
    fn direct_router_accepts_exact_256_character_member_id() {
        run_production_router_test(|| async {
            let member_id = format!("m{}", "A".repeat(255));
            let fixture = HttpFixture::start(vec![ExpectedRequest::new(
                format!("/v1/spaces/{SPACE_ID}/members/{member_id}"),
                &[],
                json!({
                    "member": member_value(&member_id, Some("Boundary"), "viewer", "active")
                }),
            )])
            .await;
            let server = selected_server(&fixture.endpoint);
            let result = dispatch(
                &server,
                MEMBER_GET,
                json!({"space": SPACE_ID, "member_id": member_id}),
                &CancellationToken::new(),
            )
            .await
            .expect("256-character member request");
            assert_eq!(result.is_error, Some(false), "boundary result: {result:?}");
            assert_eq!(
                result.structured_content.as_ref().unwrap()["member"]["id"],
                member_id
            );
            fixture.finish().await;
        });
    }

    #[test]
    fn direct_router_rejects_malformed_success_and_unknown_closed_values() {
        run_production_router_test(|| async {
            let path = format!("/v1/spaces/{SPACE_ID}/members/{MEMBER_1}");
            let fixture = HttpFixture::start(vec![
                ExpectedRequest::raw(&path, &[], "{not-json"),
                ExpectedRequest::new(
                    &path,
                    &[],
                    json!({"member": member_value(MEMBER_1, Some("One"), "superuser", "active")}),
                ),
                ExpectedRequest::new(
                    &path,
                    &[],
                    json!({"member": member_value(MEMBER_1, Some("One"), "owner", "unknown")}),
                ),
            ])
            .await;
            let server = selected_server(&fixture.endpoint);
            for _case in 0..3 {
                let result = dispatch(
                    &server,
                    MEMBER_GET,
                    json!({"space": SPACE_ID, "member_id": MEMBER_1}),
                    &CancellationToken::new(),
                )
                .await
                .expect("malformed success tool result");
                assert_tool_error(&result, "upstream");
            }
            fixture.finish().await;
        });
    }

    #[test]
    fn direct_router_bounds_cancellation_timeout_5xx_and_redaction() {
        run_production_router_test(|| async {
            let cancelled_server = selected_server("http://127.0.0.1:1");
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            let cancelled = dispatch(
                &cancelled_server,
                MEMBER_LIST,
                json!({"space": SPACE_ID}),
                &cancellation,
            )
            .await
            .expect("cancelled tool result");
            assert_tool_error(&cancelled, "upstream");
            assert_eq!(
                cancelled_server
                    .runtime()
                    .client()
                    .http_metrics()
                    .physical_attempts,
                0
            );

            let path = format!("/v1/spaces/{SPACE_ID}/members");
            let timeout_fixture = HttpFixture::start(vec![ExpectedRequest::hang(
                &path,
                &[("limit", "20")],
                Duration::from_millis(100),
            )])
            .await;
            let timeout_server = AnyMcpServer::new(runtime_config(
                &timeout_fixture.endpoint,
                true,
                false,
                Duration::from_millis(20),
                5,
            ))
            .unwrap();
            let timed_out = dispatch(
                &timeout_server,
                MEMBER_LIST,
                json!({"space": SPACE_ID}),
                &CancellationToken::new(),
            )
            .await
            .expect("timeout tool result");
            assert_tool_error(&timed_out, "upstream");
            timeout_fixture.finish().await;

            let secret = "UPSTREAM-BODY-SECRET-7f961c";
            let fixture = HttpFixture::start(vec![ExpectedRequest::status(
                &path,
                &[("limit", "20")],
                "503 Service Unavailable",
                json!({"secret": secret}),
            )])
            .await;
            let server = Box::new(
                AnyMcpServer::new(runtime_config(
                    &fixture.endpoint,
                    true,
                    false,
                    Duration::from_secs(20),
                    5,
                ))
                .expect("5xx members fixture server"),
            );
            let failed = dispatch(
                &server,
                MEMBER_LIST,
                json!({"space": SPACE_ID}),
                &CancellationToken::new(),
            )
            .await
            .expect("5xx tool result");
            assert_tool_error(&failed, "upstream");
            let wire = serde_json::to_string(&failed).expect("fixed 5xx error wire");
            assert!(!wire.contains(secret));
            assert!(!wire.contains(&fixture.endpoint));
            fixture.finish().await;
        });
    }

    #[test]
    fn direct_router_asserts_member_logical_and_physical_work_independently() {
        run_production_router_test(|| async {
            let mut replies = Vec::new();
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
                    let last = spaces.last_mut().expect("terminal resolver row");
                    *last = json!({"id": SPACE_ID, "name": "Target", "object": "space"});
                }
                let offset_string = offset.to_string();
                let mut query = vec![("limit", "99")];
                if page_index != 0 {
                    query.push(("offset", offset_string.as_str()));
                }
                replies.push(ExpectedRequest::new(
                    "/v1/spaces",
                    &query,
                    page(spaces, offset, 99, 1000),
                ));
            }
            replies.push(ExpectedRequest::new(
                format!("/v1/spaces/{SPACE_ID}/members"),
                &[("limit", "20")],
                page(vec![], 0, 20, 0),
            ));
            let fixture = HttpFixture::start(replies).await;
            let server = selected_server(&fixture.endpoint);
            let result = dispatch(
                &server,
                MEMBER_LIST,
                json!({"space": "Target"}),
                &CancellationToken::new(),
            )
            .await
            .expect("worst-case resolver result");
            assert_eq!(result.is_error, Some(false), "resolver result: {result:?}");
            let metrics = server.runtime().client().http_metrics();
            assert_eq!(metrics.logical_operations, 12);
            assert_eq!(metrics.physical_attempts, 12);
            fixture.finish().await;

            let get_fixture = HttpFixture::start(vec![ExpectedRequest::new(
                format!("/v1/spaces/{SPACE_ID}/members/{MEMBER_1}"),
                &[],
                json!({"member": member_value(MEMBER_1, None, "viewer", "active")}),
            )])
            .await;
            let get_server = selected_server(&get_fixture.endpoint);
            let get = dispatch(
                &get_server,
                MEMBER_GET,
                json!({"space": SPACE_ID, "member_id": MEMBER_1}),
                &CancellationToken::new(),
            )
            .await
            .expect("exact member result");
            assert_eq!(get.is_error, Some(false));
            let metrics = get_server.runtime().client().http_metrics();
            assert_eq!(metrics.logical_operations, 1);
            assert_eq!(metrics.physical_attempts, 1);
            get_fixture.finish().await;
        });
    }

    #[test]
    fn direct_router_proves_full_member_physical_work_ceilings() {
        run_production_router_test(|| async {
            for exact_get in [false, true] {
                let mut requests = Vec::new();
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
                        *spaces.last_mut().expect("terminal retry resolver row") =
                            json!({"id": SPACE_ID, "name": "Target", "object": "space"});
                    }
                    let offset_string = offset.to_string();
                    let mut query = vec![("limit", "99")];
                    if page_index != 0 {
                        query.push(("offset", offset_string.as_str()));
                    }
                    push_six_attempt_success(
                        &mut requests,
                        "/v1/spaces",
                        &query,
                        page(spaces, offset, 99, 1000),
                    );
                }

                let (tool, arguments, final_path, final_query, final_success) = if exact_get {
                    (
                        MEMBER_GET,
                        json!({"space": "Target", "member_id": MEMBER_1}),
                        format!("/v1/spaces/{SPACE_ID}/members/{MEMBER_1}"),
                        Vec::new(),
                        json!({
                            "member": member_value(MEMBER_1, None, "viewer", "active")
                        }),
                    )
                } else {
                    (
                        MEMBER_LIST,
                        json!({"space": "Target"}),
                        format!("/v1/spaces/{SPACE_ID}/members"),
                        vec![("limit", "20")],
                        page(Vec::new(), 0, 20, 0),
                    )
                };
                push_six_attempt_success(&mut requests, final_path, &final_query, final_success);
                assert_eq!(requests.len(), 72, "twelve six-attempt operations");

                let fixture =
                    HttpFixture::start_and_reject_extra(requests, Duration::from_millis(500)).await;
                let server = AnyMcpServer::new(runtime_config(
                    &fixture.endpoint,
                    true,
                    false,
                    Duration::from_secs(120),
                    5,
                ))
                .expect("full physical-budget member server");
                let result = dispatch(&server, tool, arguments, &CancellationToken::new())
                    .await
                    .expect("full physical-budget member result");
                let metrics = server.runtime().client().http_metrics();
                assert_eq!(
                    result.is_error,
                    Some(false),
                    "budget result: {result:?}; metrics: {metrics:?}"
                );
                assert_eq!(metrics.logical_operations, 12);
                assert_eq!(metrics.physical_attempts, 72);
                assert_eq!(metrics.total_requests, 72);
                assert_eq!(metrics.retries, 60);
                fixture.finish().await;
            }
        });
    }

    #[test]
    fn direct_router_mixed_retry_classes_never_send_a_seventh_attempt() {
        run_production_router_test(|| async {
            let path = format!("/v1/spaces/{SPACE_ID}/members");
            let query = [("limit", "20")];
            let replies = vec![
                ExpectedRequest::status_with_headers(
                    &path,
                    &query,
                    "429 Too Many Requests",
                    "RateLimit-Reset: 0\r\n",
                    json!({"class": "rate-limit"}),
                ),
                ExpectedRequest::status(
                    &path,
                    &query,
                    "504 Gateway Timeout",
                    json!({"class": "status"}),
                ),
                ExpectedRequest::hang(&path, &query, Duration::from_millis(100)),
                ExpectedRequest::status_with_headers(
                    &path,
                    &query,
                    "429 Too Many Requests",
                    "RateLimit-Reset: 0\r\n",
                    json!({"class": "rate-limit"}),
                ),
                ExpectedRequest::status(
                    &path,
                    &query,
                    "504 Gateway Timeout",
                    json!({"class": "status"}),
                ),
                ExpectedRequest::hang(&path, &query, Duration::from_millis(100)),
            ];
            let fixture =
                HttpFixture::start_and_reject_extra(replies, Duration::from_millis(500)).await;
            let server = AnyMcpServer::new(runtime_config_with_transport_timeout(
                &fixture.endpoint,
                Duration::from_secs(20),
                Duration::from_millis(20),
            ))
            .unwrap();
            let result = dispatch(
                &server,
                MEMBER_LIST,
                json!({"space": SPACE_ID}),
                &CancellationToken::new(),
            )
            .await
            .expect("mixed retry terminal tool result");
            assert_tool_error(&result, "upstream");
            let metrics = server.runtime().client().http_metrics();
            assert_eq!(metrics.logical_operations, 1);
            assert_eq!(metrics.physical_attempts, 6);
            assert_eq!(metrics.retries, 5);
            fixture.finish().await;
        });
    }
}
