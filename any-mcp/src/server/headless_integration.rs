// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Cleanup-safe tests of the production router against a headless Anytype server.

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anytype::{
    chats::MessageContent,
    objects::Icon,
    prelude::{
        AnytypeClient, ClientConfig, Color, Filter, FilterExpression, HttpCredentials,
        HttpMetricsSnapshot, Object, ObjectLayout, PropertyFormat, SetProperty,
    },
    test_util::{
        DisposableCallbackStage, DisposableRun, TestContext, TestError, unique_suffix,
        with_disposable_space_context,
    },
};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, ReadResourceRequestParams};
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::optional_toolsets::{
    OptionalToolsetSelection, production_optional_metadata, production_optional_registries,
};
use crate::runtime::{RuntimeContext, StartupStatus};

#[path = "../../tests/support/live_scenario.rs"]
pub(super) mod live_scenario;

use live_scenario::{
    BODY_PAGINATION_ITEM_COUNT, ChatsRegistryFixture, McpDriver, OptionalFastWorkflow,
    OptionalOperation, OptionalRealWorkflow, OptionalRegistry, OptionalScenarioDeclaration,
    ScenarioEvidence, ScenarioId, ToolErrorEvidence, run_body_scenario,
    run_chats_registry_scenario, run_live_scenario_on_large_stack,
    run_representative_layout_scenario, run_scenario,
};

fn arguments(value: Value) -> JsonObject {
    value
        .as_object()
        .expect("live test tool arguments must be an object")
        .clone()
}

async fn live_server(ctx: &TestContext) -> AnyMcpServer {
    live_server_with(ctx, ApplicationProfile::Standard, false).await
}

async fn live_server_with(
    ctx: &TestContext,
    profile: ApplicationProfile,
    read_only: bool,
) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("live suite requires authenticated HTTP");
    ctx.client
        .ping_grpc()
        .await
        .expect("live suite requires authenticated gRPC");
    let runtime = RuntimeContext::from_parts_with_profile(
        ctx.client.clone(),
        1,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: true,
        },
        profile,
        read_only,
    );
    AnyMcpServer::new(runtime).expect("production MCP catalog")
}

async fn live_members_server(ctx: &TestContext, read_only: bool) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("members suite requires authenticated HTTP");
    let selected = OptionalToolsetSelection::parse(
        Some("members".to_owned()),
        &production_optional_metadata(),
    )
    .expect("complete members registry");
    let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        ctx.client.clone(),
        1,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: false,
        },
        ApplicationProfile::Compact,
        read_only,
        selected,
    );
    AnyMcpServer::new(runtime).expect("production members MCP catalog")
}

async fn live_chats_server(ctx: &TestContext) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("chats suite requires authenticated HTTP");
    let selected =
        OptionalToolsetSelection::parse(Some("chats".to_owned()), &production_optional_metadata())
            .expect("complete chats registry");
    let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        ctx.client.clone(),
        2,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: false,
        },
        ApplicationProfile::Compact,
        false,
        selected,
    );
    AnyMcpServer::new(runtime).expect("production chats MCP catalog")
}

async fn live_views_write_server(ctx: &TestContext) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("layout suite requires authenticated HTTP");
    ctx.client
        .ping_grpc()
        .await
        .expect("layout suite requires authenticated gRPC");
    let selected = OptionalToolsetSelection::parse(
        Some("views-write".to_owned()),
        &production_optional_metadata(),
    )
    .expect("complete views-write registry");
    let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        ctx.client.clone(),
        2,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: true,
        },
        ApplicationProfile::Standard,
        false,
        selected,
    );
    AnyMcpServer::new(runtime).expect("production views-write MCP catalog")
}

async fn live_body_server(ctx: &TestContext) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("body suite requires authenticated HTTP");
    ctx.client
        .ping_grpc()
        .await
        .expect("body suite requires authenticated gRPC");
    let selected = OptionalToolsetSelection::parse(
        Some("body-blocks".to_owned()),
        &production_optional_metadata(),
    )
    .expect("complete body-blocks registry");
    let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        ctx.client.clone(),
        2,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: true,
        },
        ApplicationProfile::Standard,
        false,
        selected,
    );
    AnyMcpServer::new(runtime).expect("production body-blocks MCP catalog")
}

async fn call(server: &AnyMcpServer, name: &'static str, value: Value) -> CallToolResult {
    // Poll the selected route from its own runtime task so the fixture-heavy
    // caller stack unwinds before the handler runs.
    let server = server.clone();
    tokio::spawn(async move {
        let cancellation = CancellationToken::new();
        Box::pin(server.dispatch_tool(
            CallToolRequestParams::new(name).with_arguments(arguments(value)),
            &cancellation,
        ))
        .await
    })
    .await
    .expect("live production router task")
    .expect("well-formed production router call")
}

async fn success(server: &AnyMcpServer, name: &'static str, value: Value) -> Value {
    let result = call(server, name, value).await;
    assert_eq!(result.is_error, Some(false), "{name} failed: {result:?}");
    result
        .structured_content
        .expect("successful tool result has structured content")
}

async fn failure(server: &AnyMcpServer, name: &'static str, value: Value) -> Value {
    let result = call(server, name, value).await;
    assert_eq!(result.is_error, Some(true), "{name} unexpectedly succeeded");
    result
        .structured_content
        .expect("failed tool result has structured content")
}

struct DirectRouterDriver<'a> {
    server: &'a AnyMcpServer,
}

impl McpDriver for DirectRouterDriver<'_> {
    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            let result = call(self.server, name, arguments).await;
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
            let result = call(self.server, name, arguments).await;
            let value = serde_json::to_value(result)
                .map_err(|_| format!("{name} error result was not serializable"))?;
            ToolErrorEvidence::from_result(&value, false)
        })
    }

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
        Box::pin(async move {
            Ok(self
                .server
                .tools()
                .iter()
                .map(|tool| tool.name.to_string())
                .collect())
        })
    }

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            self.server
                .list_resources_wire(None)
                .map_err(|_| "resources/list protocol error".to_owned())
                .and_then(|result| {
                    serde_json::to_value(result)
                        .map_err(|error| format!("serialize resources/list result: {error}"))
                })
        })
    }

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            self.server
                .list_resource_templates_wire(None)
                .map_err(|_| "resources/templates/list protocol error".to_owned())
                .and_then(|result| {
                    serde_json::to_value(result).map_err(|error| {
                        format!("serialize resources/templates/list result: {error}")
                    })
                })
        })
    }

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
        Box::pin(async move {
            self.server
                .read_resource_wire(
                    ReadResourceRequestParams::new(uri),
                    &CancellationToken::new(),
                )
                .await
                .map_err(|_| "resources/read protocol error".to_owned())
                .and_then(|result| {
                    serde_json::to_value(result)
                        .map_err(|error| format!("serialize resources/read result: {error}"))
                })
        })
    }
}

fn item_id(item: &Value) -> Option<&Value> {
    item.get("id")
        .or_else(|| item.get("summary").and_then(|summary| summary.get("id")))
        .or_else(|| item.get("object").and_then(|object| object.get("id")))
}

fn exact_item_ids(page: &Value) -> HashSet<String> {
    let items = page["items"].as_array().expect("filtered items array");
    let ids = items
        .iter()
        .map(|item| {
            item_id(item)
                .and_then(Value::as_str)
                .expect("filtered item exact id")
                .to_owned()
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        ids.len(),
        items.len(),
        "filtered page contains duplicate ids"
    );
    ids
}

async fn assert_filtered_ids(
    server: &AnyMcpServer,
    name: &'static str,
    mut input: JsonObject,
    property_key: &str,
    value: &str,
    expected: &HashSet<String>,
) {
    input.insert(
        "filters".to_owned(),
        json!({
            "operator": "and",
            "conditions": [{
                "format": "text",
                "property_key": property_key,
                "condition": "contains",
                "value": value
            }]
        }),
    );
    input.insert("limit".to_owned(), json!(100));
    let page = success(server, name, Value::Object(input)).await;
    assert_eq!(exact_item_ids(&page), *expected, "{name} filter identities");
    assert!(page.get("next_cursor").is_none());
}

async fn assert_cursor_continuation(
    server: &AnyMcpServer,
    name: &'static str,
    mut base: JsonObject,
) -> (Value, Value) {
    base.insert("limit".to_owned(), json!(1));
    let first = success(server, name, Value::Object(base.clone())).await;
    assert_eq!(first["items"].as_array().map(Vec::len), Some(1));
    let cursor = first["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("{name} must expose a live continuation"))
        .to_owned();

    let mut mismatched = base.clone();
    mismatched.insert("limit".to_owned(), json!(2));
    mismatched.insert("cursor".to_owned(), json!(cursor.clone()));
    let mismatch = failure(server, name, Value::Object(mismatched)).await;
    assert_eq!(mismatch["code"], "validation", "{name} cursor binding");

    let mut next = base;
    next.insert("cursor".to_owned(), json!(cursor));
    let second = success(server, name, Value::Object(next)).await;
    assert_eq!(second["items"].as_array().map(Vec::len), Some(1));
    if let (Some(first_id), Some(second_id)) =
        (item_id(&first["items"][0]), item_id(&second["items"][0]))
    {
        assert_ne!(first_id, second_id);
    }
    (first, second)
}

async fn assert_collection_view_continuation(
    ctx: &TestContext,
    server: &AnyMcpServer,
    collection_id: &str,
    changed_list_id: &str,
    added_view_id: &str,
    added_view_name: &str,
) {
    const MAX_VIEW_LIST_PAGES: usize = 8;

    let response = ctx
        .client
        .list_views(&ctx.space_id, collection_id)
        .limit(1_000)
        .offset(0)
        .list()
        .await
        .expect("list complete collection view fixture")
        .into_response();
    assert_eq!(response.pagination.offset, 0);
    assert!(!response.pagination.has_more);
    assert_eq!(response.pagination.total, response.items.len());
    assert_eq!(
        response.items.len(),
        2,
        "fixture has default plus added view"
    );
    let expected = response
        .items
        .iter()
        .map(|view| (view.id.clone(), view.name.clone().unwrap_or_default()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(expected.len(), response.items.len());
    assert_eq!(
        expected.get(added_view_id).map(String::as_str),
        Some(added_view_name)
    );
    assert!(
        expected.keys().any(|view_id| view_id != added_view_id),
        "fixture must retain its distinct server-created default view"
    );

    let mut next_cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut observed = BTreeMap::new();
    let mut binding_checked = false;
    let mut reached_terminal = false;
    for _ in 0..MAX_VIEW_LIST_PAGES {
        let mut request = arguments(json!({
            "space": ctx.space_id.as_str(),
            "list_id": collection_id,
            "limit": 1
        }));
        if let Some(cursor) = &next_cursor {
            request.insert("cursor".to_owned(), json!(cursor));
        }
        let page = success(server, VIEW_LIST, Value::Object(request)).await;
        let items = page["items"]
            .as_array()
            .expect("view_list items must be an array");
        assert_eq!(items.len(), 1, "each bounded view page must progress");
        let id = items[0]["id"].as_str().expect("view_list item id");
        let name = items[0]["name"].as_str().expect("view_list item name");
        assert!(
            observed.insert(id.to_owned(), name.to_owned()).is_none(),
            "view_list must not repeat an item while advancing"
        );

        let Some(cursor) = page.get("next_cursor") else {
            reached_terminal = true;
            break;
        };
        let cursor = cursor
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .expect("view_list next_cursor must be a nonempty string")
            .to_owned();
        assert!(
            seen_cursors.insert(cursor.clone()),
            "view_list cursor chain must not loop"
        );

        if !binding_checked {
            let changed_limit = failure(
                server,
                VIEW_LIST,
                json!({
                    "space": ctx.space_id.as_str(),
                    "list_id": collection_id,
                    "limit": 2,
                    "cursor": cursor.as_str()
                }),
            )
            .await;
            assert_eq!(changed_limit["code"], "validation");
            let changed_query = failure(
                server,
                VIEW_LIST,
                json!({
                    "space": ctx.space_id.as_str(),
                    "list_id": changed_list_id,
                    "limit": 1,
                    "cursor": cursor.as_str()
                }),
            )
            .await;
            assert_eq!(changed_query["code"], "validation");
            binding_checked = true;
        }
        next_cursor = Some(cursor);
    }

    assert!(
        binding_checked,
        "fixture-backed view list must expose a cursor"
    );
    assert!(
        reached_terminal,
        "view_list must terminate within its hard bound"
    );
    assert_eq!(observed, expected, "MCP and ordinary API view lists differ");
}

async fn assert_fixture_template_continuation(
    server: &AnyMcpServer,
    space_id: &str,
    type_id: &str,
    fixture_ids: &HashSet<&str>,
) {
    const PAGE_HARD_BOUND: usize = 32;
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut terminal = false;

    for page_index in 0..PAGE_HARD_BOUND {
        let mut request = arguments(json!({
            "space": space_id,
            "type": type_id,
            "limit": 1
        }));
        if let Some(cursor) = cursor.as_ref() {
            request.insert("cursor".to_owned(), json!(cursor));
        }
        let page = success(server, TEMPLATE_LIST, Value::Object(request.clone())).await;
        let items = page["items"]
            .as_array()
            .expect("template_list items must be an array");
        assert_eq!(items.len(), 1, "each fixture page has exactly one item");
        let id = item_id(&items[0])
            .and_then(Value::as_str)
            .expect("template summary has an exact id");
        assert!(
            seen_ids.insert(id.to_owned()),
            "template page repeated an id"
        );

        let Some(next_cursor) = page.get("next_cursor") else {
            terminal = true;
            break;
        };
        let next_cursor = next_cursor
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .expect("template continuation cursor is nonempty")
            .to_owned();
        assert!(
            seen_cursors.insert(next_cursor.clone()),
            "template continuation cursor loop"
        );

        if page_index == 0 {
            let mut changed_limit = request.clone();
            changed_limit.insert("limit".to_owned(), json!(2));
            changed_limit.insert("cursor".to_owned(), json!(next_cursor.clone()));
            let mismatch = failure(server, TEMPLATE_LIST, Value::Object(changed_limit)).await;
            assert_eq!(mismatch["code"], "validation", "template limit binding");

            let mut changed_type = request;
            changed_type.insert("type".to_owned(), json!("page"));
            changed_type.insert("cursor".to_owned(), json!(next_cursor.clone()));
            let mismatch = failure(server, TEMPLATE_LIST, Value::Object(changed_type)).await;
            assert_eq!(mismatch["code"], "validation", "template query binding");
        }
        cursor = Some(next_cursor);
    }

    assert!(
        terminal,
        "template pagination must reach a bounded terminal page"
    );
    assert_eq!(
        seen_ids,
        fixture_ids
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<HashSet<_>>(),
        "template wire pages contain exactly the cleanup-owned fixtures"
    );
}

async fn create_object(ctx: &TestContext, type_key: &str, name: &str, body: &str) -> Object {
    let object = ctx
        .client
        .new_object(&ctx.space_id, type_key)
        .name(name)
        .body(body)
        .ensure_available()
        .create()
        .await
        .expect("create live fixture object");
    ctx.register_object(&object.id);
    object
}

async fn read_body(ctx: &TestContext, object_id: &str) -> String {
    ctx.client
        .object(&ctx.space_id, object_id)
        .get()
        .await
        .expect("read live fixture object")
        .markdown
        .unwrap_or_default()
}

async fn active_contains(ctx: &TestContext, object_id: &str) -> bool {
    for offset in (0..1_000).step_by(100) {
        let page = ctx
            .client
            .objects(&ctx.space_id)
            .limit(100)
            .offset(offset)
            .list()
            .await
            .expect("query active object surface");
        if page
            .items
            .iter()
            .any(|object| object.id == object_id && !object.archived)
        {
            return true;
        }
        if !page.pagination.has_more {
            return false;
        }
    }
    panic!("active evidence exceeds the diagnostic 1,000-object bound")
}

async fn archived_contains(ctx: &TestContext, object_id: &str, type_id: &str) -> bool {
    for offset in (0..10_000).step_by(1_000) {
        let page = ctx
            .client
            .list_archived(&ctx.space_id)
            .types([type_id])
            .limit(1_000)
            .offset(offset)
            .list()
            .await
            .expect("query archived object surface");
        if page.items.iter().any(|object| object.id == object_id) {
            return true;
        }
        if page.items.is_empty() || !page.pagination.has_more {
            return false;
        }
    }
    panic!("archived evidence exceeds the diagnostic 10,000-object bound")
}

async fn assert_archive_evidence(ctx: &TestContext, object_id: &str, type_id: &str) {
    let mut last = (true, false);
    for _ in 0..10 {
        last = (
            active_contains(ctx, object_id).await,
            archived_contains(ctx, object_id, type_id).await,
        );
        if !last.0 && last.1 {
            return;
        }
        sleep(Duration::from_millis(250)).await;
    }
    panic!(
        "archive evidence did not converge: active={}, archived={}",
        last.0, last.1
    )
}

async fn execute_fixture_search(
    ctx: &TestContext,
    type_key: &str,
    filter: Filter,
    offset: u32,
    limit: u32,
) -> anytype::paged::PagedResult<Object> {
    tokio::time::timeout(
        Duration::from_secs(10),
        ctx.client
            .search_in(&ctx.space_id)
            .types([type_key])
            .filters(FilterExpression::from(vec![filter]))
            .sort_asc("name")
            .offset(offset)
            .limit(limit)
            .execute(),
    )
    .await
    .expect("live filter search must finish within ten seconds")
    .expect("supported live filter search must succeed")
}

fn assert_one_http_request(
    before: HttpMetricsSnapshot,
    after: HttpMetricsSnapshot,
    operation: &str,
) {
    assert_eq!(
        after.total_requests - before.total_requests,
        1,
        "{operation} must issue exactly one upstream request"
    );
    assert_eq!(
        after.retries - before.retries,
        0,
        "{operation} must not retry with rewritten semantics"
    );
}

async fn assert_live_filter_result(
    ctx: &TestContext,
    server: &AnyMcpServer,
    type_key: &str,
    filter: Filter,
    wire_filter: Value,
    expected_ids: &[String],
    label: &str,
) {
    let api_before = ctx.client.http_metrics();
    let api_page = tokio::time::timeout(
        Duration::from_secs(10),
        ctx.client
            .search_in(&ctx.space_id)
            .types([type_key])
            .filters(FilterExpression::from(vec![filter]))
            .sort_asc("name")
            .limit(100)
            .offset(0)
            .execute(),
    )
    .await
    .expect("live filter search must finish within ten seconds")
    .expect("checked server must accept the numeric/checkbox representation")
    .into_response();
    let api_after = ctx.client.http_metrics();
    assert_one_http_request(api_before, api_after, label);
    assert_eq!(api_page.pagination.offset, 0);
    assert_eq!(api_page.pagination.limit, 100);
    assert_eq!(api_page.pagination.total, expected_ids.len());
    assert!(!api_page.pagination.has_more);
    assert_eq!(
        api_page
            .items
            .iter()
            .map(|object| object.id.as_str())
            .collect::<Vec<_>>(),
        expected_ids.iter().map(String::as_str).collect::<Vec<_>>()
    );

    let mcp_before = ctx.client.http_metrics();
    let result = success(
        server,
        OBJECT_SEARCH,
        json!({
            "space": ctx.space_id.as_str(),
            "types": [format!("@{type_key}")],
            "filters": {
                "operator": "and",
                "conditions": [wire_filter]
            },
            "sort": {"property_key": "name", "direction": "asc"},
            "limit": 100
        }),
    )
    .await;
    let mcp_after = ctx.client.http_metrics();
    assert_one_http_request(mcp_before, mcp_after, label);
    assert_eq!(
        result["items"]
            .as_array()
            .expect("MCP live-filter items")
            .iter()
            .filter_map(item_id)
            .filter_map(Value::as_str)
            .collect::<Vec<_>>(),
        expected_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        "MCP and independently checked API filter identities differ"
    );
    assert!(result.get("next_cursor").is_none());
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_shared_filters_conform_and_preserve_server_pagination() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-shared-filters",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                const FIXTURE_COUNT: usize = 3;
                const MAX_INDEX_ATTEMPTS: usize = 20;

                let server = live_server(ctx.as_ref()).await;
                let suffix = unique_suffix();
                let type_key = format!("mcp_filter_conformance_{suffix}");
                let number_key = format!("mcp_filter_number_{suffix}");
                let checkbox_key = format!("mcp_filter_checkbox_{suffix}");

                let type_ = ctx
                    .client
                    .new_type(&ctx.space_id, format!("MCP filter conformance {suffix}"))
                    .key(&type_key)
                    .ensure_available()
                    .create()
                    .await
                    .expect("create cleanup-owned filter type");
                ctx.register_type(&type_.id);

                let number_property = ctx
                    .client
                    .new_property(
                        &ctx.space_id,
                        format!("MCP filter number {suffix}"),
                        PropertyFormat::Number,
                    )
                    .key(&number_key)
                    .ensure_available()
                    .create()
                    .await
                    .expect("create cleanup-owned number property");
                ctx.register_property(&number_property.id);
                let checkbox_property = ctx
                    .client
                    .new_property(
                        &ctx.space_id,
                        format!("MCP filter checkbox {suffix}"),
                        PropertyFormat::Checkbox,
                    )
                    .key(&checkbox_key)
                    .ensure_available()
                    .create()
                    .await
                    .expect("create cleanup-owned checkbox property");
                ctx.register_property(&checkbox_property.id);

                let mut expected_ids = Vec::with_capacity(FIXTURE_COUNT);
                for index in 0..FIXTURE_COUNT {
                    let object = ctx
                        .client
                        .new_object(&ctx.space_id, &type_key)
                        .name(format!("MCP filter {index:02} {suffix}"))
                        .set_number(
                            &number_key,
                            i64::try_from(index).expect("small fixture index"),
                        )
                        .set_checkbox(&checkbox_key, index % 2 == 0)
                        .ensure_available()
                        .create()
                        .await
                        .expect("create cleanup-owned filter object");
                    ctx.register_object(&object.id);
                    expected_ids.push(object.id.clone());
                }

                for (index, object_id) in expected_ids.iter().enumerate() {
                    let object = ctx
                        .client
                        .object(&ctx.space_id, object_id)
                        .get()
                        .await
                        .expect("independently read filter fixture");
                    assert_eq!(object.id, *object_id);
                    assert_eq!(
                        object.get_property_i64(&number_key),
                        Some(i64::try_from(index).expect("small fixture index"))
                    );
                    assert_eq!(
                        object.get_property_bool(&checkbox_key),
                        Some(index % 2 == 0)
                    );
                }

                ctx.client
                    .resolve_space_id(&ctx.space_id)
                    .await
                    .expect("warm exact space resolution");
                assert_eq!(
                    ctx.client
                        .resolve_type_key(&ctx.space_id, &format!("@{type_key}"))
                        .await
                        .expect("warm exact type resolution"),
                    type_key
                );

                assert_live_filter_result(
                    ctx.as_ref(),
                    &server,
                    &type_key,
                    Filter::number_greater(&number_key, -1),
                    json!({
                        "format": "number",
                        "property_key": number_key.as_str(),
                        "condition": "gt",
                        "value": -1
                    }),
                    &expected_ids,
                    "numeric filter",
                )
                .await;
                let checked_ids = expected_ids.iter().step_by(2).cloned().collect::<Vec<_>>();
                assert_live_filter_result(
                    ctx.as_ref(),
                    &server,
                    &type_key,
                    Filter::checkbox_true(&checkbox_key),
                    json!({
                        "format": "checkbox",
                        "property_key": checkbox_key.as_str(),
                        "condition": "eq",
                        "value": true
                    }),
                    &checked_ids,
                    "checkbox filter",
                )
                .await;

                let expected_ids = tokio::time::timeout(Duration::from_secs(20), async {
                    for _ in 0..MAX_INDEX_ATTEMPTS {
                        let page = execute_fixture_search(
                            ctx.as_ref(),
                            &type_key,
                            Filter::number_greater(&number_key, -1),
                            0,
                            100,
                        )
                        .await
                        .into_response();
                        let observed = page
                            .items
                            .iter()
                            .map(|object| object.id.clone())
                            .collect::<Vec<_>>();
                        if observed.len() == FIXTURE_COUNT {
                            return observed;
                        }
                        sleep(Duration::from_millis(250)).await;
                    }
                    panic!("filter fixtures did not become searchable within the attempt bound")
                })
                .await
                .expect("filter fixtures must become searchable within twenty seconds");

                let mut cursor: Option<String> = None;
                let mut observed_ids = Vec::with_capacity(FIXTURE_COUNT);
                for (offset, expected_id) in expected_ids.iter().enumerate() {
                    let api_page = execute_fixture_search(
                        ctx.as_ref(),
                        &type_key,
                        Filter::number_greater(&number_key, -1),
                        u32::try_from(offset).expect("small fixture offset"),
                        1,
                    )
                    .await
                    .into_response();
                    assert_eq!(
                        api_page.pagination.offset,
                        u32::try_from(offset).expect("small fixture offset")
                    );
                    assert_eq!(api_page.pagination.limit, 1);
                    assert_eq!(api_page.pagination.total, FIXTURE_COUNT);
                    assert_eq!(api_page.pagination.has_more, offset + 1 < FIXTURE_COUNT);
                    assert_eq!(api_page.items.len(), 1);
                    assert_eq!(api_page.items[0].id, *expected_id);

                    let mut input = arguments(json!({
                        "space": ctx.space_id.as_str(),
                        "types": [format!("@{type_key}")],
                        "filters": {
                            "operator": "and",
                            "conditions": [{
                                "format": "number",
                                "property_key": number_key.as_str(),
                                "condition": "gt",
                                "value": -1
                            }]
                        },
                        "sort": {"property_key": "name", "direction": "asc"},
                        "limit": 1
                    }));
                    if let Some(cursor) = &cursor {
                        input.insert("cursor".to_owned(), json!(cursor));
                    }
                    let before = ctx.client.http_metrics();
                    let mcp_page = success(&server, OBJECT_SEARCH, Value::Object(input)).await;
                    let after = ctx.client.http_metrics();
                    assert_one_http_request(before, after, "paginated MCP filter search");
                    let items = mcp_page["items"].as_array().expect("MCP filter page items");
                    assert_eq!(items.len(), 1);
                    assert_eq!(
                        item_id(&items[0]).and_then(Value::as_str),
                        Some(expected_id.as_str())
                    );
                    observed_ids.push(expected_id.clone());

                    cursor = mcp_page.get("next_cursor").map(|value| {
                        value
                            .as_str()
                            .filter(|cursor| !cursor.is_empty())
                            .expect("MCP continuation cursor must be nonempty")
                            .to_owned()
                    });
                    assert_eq!(cursor.is_some(), api_page.pagination.has_more);
                }
                assert_eq!(observed_ids, expected_ids);
                assert!(
                    cursor.is_none(),
                    "terminal checked server page has no cursor"
                );
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe live shared-filter conformance");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable shared-filter suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_default_discovery_routes_paginate_and_report_ambiguity() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-discovery",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let status = success(&server, SERVER_STATUS, json!({})).await;
                assert_eq!(status["http_available"], true);
                assert_eq!(status["grpc_available"], true);

                let all_spaces = ctx
                    .client
                    .spaces()
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("list spaces containing the cleanup-owned fixture")
                    .into_response();
                let space_filter_term = all_spaces
                    .items
                    .iter()
                    .find(|space| space.id == ctx.space_id)
                    .map(|space| space.name.clone())
                    .expect("cleanup-owned disposable space is independently visible");
                assert!(!space_filter_term.is_empty());

                let duplicate_name = format!("MCP ambiguous {}", unique_suffix());
                let first_type = ctx
                    .client
                    .new_type(&ctx.space_id, &duplicate_name)
                    .key(format!("mcp_ambiguous_a_{}", unique_suffix()))
                    .ensure_available()
                    .create()
                    .await
                    .expect("create first pagination type");
                ctx.register_type(&first_type.id);
                let second_type = ctx
                    .client
                    .new_type(&ctx.space_id, &duplicate_name)
                    .key(format!("mcp_ambiguous_b_{}", unique_suffix()))
                    .ensure_available()
                    .create()
                    .await
                    .expect("create second pagination type");
                ctx.register_type(&second_type.id);

                let template_filter_term = format!("MCP filter templates {}", unique_suffix());
                let template_fixtures = ctx
                    .create_template_fixtures(
                        format!("MCP template type {}", unique_suffix()),
                        [
                            format!("{template_filter_term} first"),
                            format!("{template_filter_term} second"),
                        ],
                    )
                    .await
                    .expect("create cleanup-owned template fixtures");

                let property_filter_term = format!("MCP filter property {}", unique_suffix());
                let property = ctx
                    .client
                    .new_property(&ctx.space_id, &property_filter_term, PropertyFormat::Select)
                    .create()
                    .await
                    .expect("create select pagination property");
                ctx.register_property(&property.id);
                let text_property = ctx
                    .client
                    .new_property(
                        &ctx.space_id,
                        format!("MCP pagination text {}", unique_suffix()),
                        PropertyFormat::Text,
                    )
                    .create()
                    .await
                    .expect("create text pagination property");
                ctx.register_property(&text_property.id);
                let tag_filter_term = format!("MCP filter tags {}", unique_suffix());
                let mut tag_ids = HashSet::new();
                for (name, color) in [("First", Color::Blue), ("Second", Color::Red)] {
                    let tag = ctx
                        .client
                        .new_tag(&ctx.space_id, &property.id)
                        .name(format!("{tag_filter_term} {name}"))
                        .color(color)
                        .create()
                        .await
                        .expect("create tag fixture");
                    assert!(tag_ids.insert(tag.id));
                }

                let search_term = format!("McpPagination{}", unique_suffix());
                let first_object =
                    create_object(ctx.as_ref(), "page", &format!("{search_term} first"), "").await;
                let second_object =
                    create_object(ctx.as_ref(), "page", &format!("{search_term} second"), "").await;
                sleep(Duration::from_millis(300)).await;

                let expected_space_ids = HashSet::from([ctx.space_id.clone()]);
                let api_spaces = ctx
                    .client
                    .spaces()
                    .filter(Filter::text_contains("name", &space_filter_term))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered space list")
                    .into_response();
                assert_eq!(api_spaces.pagination.offset, 0);
                assert_eq!(api_spaces.pagination.limit, 100);
                assert_eq!(api_spaces.pagination.total, expected_space_ids.len());
                assert!(!api_spaces.pagination.has_more);
                assert_eq!(
                    api_spaces
                        .items
                        .iter()
                        .map(|space| space.id.clone())
                        .collect::<HashSet<_>>(),
                    expected_space_ids
                );
                assert_filtered_ids(
                    &server,
                    SPACE_LIST,
                    arguments(json!({})),
                    "name",
                    &space_filter_term,
                    &expected_space_ids,
                )
                .await;

                let expected_type_ids = [first_type.id.clone(), second_type.id.clone()]
                    .into_iter()
                    .collect::<HashSet<_>>();
                let api_types = ctx
                    .client
                    .types(&ctx.space_id)
                    .filter(Filter::text_contains("name", &duplicate_name))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered type list")
                    .into_response();
                assert_eq!(api_types.pagination.offset, 0);
                assert_eq!(api_types.pagination.limit, 100);
                assert_eq!(api_types.pagination.total, expected_type_ids.len());
                assert!(!api_types.pagination.has_more);
                assert_eq!(
                    api_types
                        .items
                        .iter()
                        .map(|type_| type_.id.clone())
                        .collect::<HashSet<_>>(),
                    expected_type_ids
                );
                assert_filtered_ids(
                    &server,
                    TYPE_LIST,
                    arguments(json!({"space": ctx.space_id.as_str()})),
                    "name",
                    &duplicate_name,
                    &expected_type_ids,
                )
                .await;

                let expected_property_ids = HashSet::from([property.id.clone()]);
                let api_properties = ctx
                    .client
                    .properties(&ctx.space_id)
                    .filter(Filter::text_contains("name", &property_filter_term))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered property list")
                    .into_response();
                assert_eq!(api_properties.pagination.offset, 0);
                assert_eq!(api_properties.pagination.limit, 100);
                assert_eq!(api_properties.pagination.total, 1);
                assert!(!api_properties.pagination.has_more);
                assert_eq!(
                    api_properties
                        .items
                        .iter()
                        .map(|property| property.id.clone())
                        .collect::<HashSet<_>>(),
                    expected_property_ids
                );
                assert_filtered_ids(
                    &server,
                    PROPERTY_LIST,
                    arguments(json!({"space": ctx.space_id.as_str()})),
                    "name",
                    &property_filter_term,
                    &expected_property_ids,
                )
                .await;

                let api_tags = ctx
                    .client
                    .tags(&ctx.space_id, &property.id)
                    .filter(Filter::text_contains("name", &tag_filter_term))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered tag list")
                    .into_response();
                assert_eq!(api_tags.pagination.offset, 0);
                assert_eq!(api_tags.pagination.limit, 100);
                assert_eq!(api_tags.pagination.total, tag_ids.len());
                assert!(!api_tags.pagination.has_more);
                assert_eq!(
                    api_tags
                        .items
                        .iter()
                        .map(|tag| tag.id.clone())
                        .collect::<HashSet<_>>(),
                    tag_ids
                );
                assert_filtered_ids(
                    &server,
                    TAG_LIST,
                    arguments(json!({
                        "space": ctx.space_id.as_str(),
                        "property": property.id.as_str()
                    })),
                    "name",
                    &tag_filter_term,
                    &tag_ids,
                )
                .await;

                let expected_template_ids = template_fixtures
                    .templates
                    .iter()
                    .map(|template| template.id.clone())
                    .collect::<HashSet<_>>();
                let api_templates = ctx
                    .client
                    .templates(&ctx.space_id, &template_fixtures.type_.id)
                    .filter(Filter::text_contains("name", &template_filter_term))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered template list")
                    .into_response();
                assert_eq!(api_templates.pagination.offset, 0);
                assert_eq!(api_templates.pagination.limit, 100);
                assert_eq!(api_templates.pagination.total, expected_template_ids.len());
                assert!(!api_templates.pagination.has_more);
                assert_eq!(
                    api_templates
                        .items
                        .iter()
                        .map(|template| template.id.clone())
                        .collect::<HashSet<_>>(),
                    expected_template_ids
                );
                assert_filtered_ids(
                    &server,
                    TEMPLATE_LIST,
                    arguments(json!({
                        "space": ctx.space_id.as_str(),
                        "type": template_fixtures.type_.id.as_str()
                    })),
                    "name",
                    &template_filter_term,
                    &expected_template_ids,
                )
                .await;

                assert_cursor_continuation(
                    &server,
                    TYPE_LIST,
                    arguments(json!({"space": ctx.space_id.as_str()})),
                )
                .await;
                assert_cursor_continuation(
                    &server,
                    PROPERTY_LIST,
                    arguments(json!({"space": ctx.space_id.as_str()})),
                )
                .await;
                assert_cursor_continuation(
                    &server,
                    TAG_LIST,
                    arguments(json!({
                        "space": ctx.space_id.as_str(),
                        "property": property.id.as_str()
                    })),
                )
                .await;

                assert_fixture_template_continuation(
                    &server,
                    ctx.space_id.as_str(),
                    template_fixtures.type_.id.as_str(),
                    &template_fixtures
                        .templates
                        .iter()
                        .map(|template| template.id.as_str())
                        .collect(),
                )
                .await;
                let (search_first, search_second) = assert_cursor_continuation(
                    &server,
                    OBJECT_SEARCH,
                    arguments(json!({
                        "space": ctx.space_id.as_str(),
                        "text": search_term
                    })),
                )
                .await;
                let searched_ids = [
                    item_id(&search_first["items"][0]).and_then(Value::as_str),
                    item_id(&search_second["items"][0]).and_then(Value::as_str),
                ]
                .into_iter()
                .flatten()
                .collect::<HashSet<_>>();
                assert!(searched_ids.contains(first_object.id.as_str()));
                assert!(searched_ids.contains(second_object.id.as_str()));

                let ambiguous = failure(
                    &server,
                    PROPERTY_LIST,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "type": duplicate_name,
                        "limit": 1
                    }),
                )
                .await;
                assert_eq!(ambiguous["code"], "ambiguous");
                let ids = ambiguous["candidates"]
                    .as_array()
                    .expect("ambiguity candidates")
                    .iter()
                    .filter_map(|candidate| candidate["id"].as_str())
                    .collect::<HashSet<_>>();
                assert!(ids.contains(first_type.id.as_str()));
                assert!(ids.contains(second_type.id.as_str()));
                Ok(())
            })
        },
    ))
    .await
    .expect("prefix-authorized live discovery harness");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable discovery suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_view_body_and_resource_routes_are_complete_and_bound() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-view-resources",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let listed_resources = serde_json::to_value(
                    server
                        .list_resources_wire(None)
                        .expect("production resources/list"),
                )
                .expect("serialize resources/list");
                assert_eq!(listed_resources["resources"], json!([]));
                let listed_templates = serde_json::to_value(
                    server
                        .list_resource_templates_wire(None)
                        .expect("production resources/templates/list"),
                )
                .expect("serialize resources/templates/list");
                assert_eq!(
                    listed_templates["resourceTemplates"][0]["uriTemplate"],
                    "anytype://spaces/{space_id}/objects/{object_id}"
                );
                let collection_type = ctx
                    .create_collection_type_fixture(format!(
                        "MCP collection type {}",
                        unique_suffix()
                    ))
                    .await
                    .expect("create and register collection-layout type fixture");
                assert_eq!(collection_type.layout, ObjectLayout::Collection);
                let collection = ctx
                    .create_collection_fixture(
                        &collection_type,
                        format!("MCP collection {}", unique_suffix()),
                    )
                    .await
                    .expect("create privately owned collection fixture");
                let second_view_name = format!("MCP second view {}", unique_suffix());
                let second_view = ctx
                    .create_collection_view_fixture(&collection.id, &second_view_name)
                    .await
                    .expect("create cleanup-owned second collection view");
                let view_filter_term = format!("MCP filtered members {}", unique_suffix());
                let first = create_object(
                    ctx.as_ref(),
                    "page",
                    &format!("{view_filter_term} first"),
                    "first resource body",
                )
                .await;
                let second = create_object(
                    ctx.as_ref(),
                    "page",
                    &format!("{view_filter_term} second"),
                    "second resource body",
                )
                .await;
                ctx.client
                    .view_add_objects(
                        &ctx.space_id,
                        &collection.id,
                        vec![first.id.clone(), second.id.clone()],
                    )
                    .await
                    .expect("add live collection members");
                assert_collection_view_continuation(
                    ctx.as_ref(),
                    &server,
                    &collection.id,
                    &first.id,
                    &second_view.id,
                    &second_view_name,
                )
                .await;
                let view_id = second_view.id;
                let expected_member_ids = HashSet::from([first.id.clone(), second.id.clone()]);
                let independent_members = ctx
                    .client
                    .view_list_objects(&ctx.space_id, &collection.id)
                    .view(&view_id)
                    .filter(Filter::text_contains("name", &view_filter_term))
                    .limit(100)
                    .offset(0)
                    .list()
                    .await
                    .expect("independent filtered view-object list")
                    .into_response();
                assert_eq!(independent_members.pagination.offset, 0);
                assert_eq!(independent_members.pagination.limit, 100);
                assert_eq!(independent_members.pagination.total, 2);
                assert!(!independent_members.pagination.has_more);
                assert_eq!(
                    independent_members
                        .items
                        .iter()
                        .map(|object| object.id.clone())
                        .collect::<HashSet<_>>(),
                    expected_member_ids
                );

                let mut listed = None;
                for _ in 0..10 {
                    let result = call(
                        &server,
                        VIEW_OBJECT_LIST,
                        json!({
                            "space": ctx.space_id.as_str(),
                            "list_id": collection.id.as_str(),
                            "view": view_id.as_str(),
                            "filters": {
                                "operator": "and",
                                "conditions": [{
                                    "format": "text",
                                    "property_key": "name",
                                    "condition": "contains",
                                    "value": view_filter_term.as_str()
                                }]
                            },
                            "limit": 1
                        }),
                    )
                    .await;
                    if result.is_error == Some(false)
                        && result
                            .structured_content
                            .as_ref()
                            .and_then(|value| value["next_cursor"].as_str())
                            .is_some()
                    {
                        listed = result.structured_content;
                        break;
                    }
                    sleep(Duration::from_millis(500)).await;
                }
                let listed = listed.expect("explicit selected view exposes a continuation");
                let cursor = listed["next_cursor"].as_str().unwrap().to_owned();
                let mismatch = failure(
                    &server,
                    VIEW_OBJECT_LIST,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "list_id": collection.id.as_str(),
                        "view": view_id.as_str(),
                        "filters": {
                            "operator": "and",
                            "conditions": [{
                                "format": "text",
                                "property_key": "name",
                                "condition": "contains",
                                "value": view_filter_term.as_str()
                            }]
                        },
                        "limit": 2,
                        "cursor": cursor.as_str()
                    }),
                )
                .await;
                assert_eq!(mismatch["code"], "validation");
                let continued = success(
                    &server,
                    VIEW_OBJECT_LIST,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "list_id": collection.id.as_str(),
                        "view": view_id.as_str(),
                        "filters": {
                            "operator": "and",
                            "conditions": [{
                                "format": "text",
                                "property_key": "name",
                                "condition": "contains",
                                "value": view_filter_term.as_str()
                            }]
                        },
                        "limit": 1,
                        "cursor": cursor
                    }),
                )
                .await;
                let observed = [
                    item_id(&listed["items"][0]).and_then(Value::as_str),
                    item_id(&continued["items"][0]).and_then(Value::as_str),
                ]
                .into_iter()
                .flatten()
                .collect::<HashSet<_>>();
                assert_eq!(
                    observed,
                    expected_member_ids
                        .iter()
                        .map(String::as_str)
                        .collect::<HashSet<_>>(),
                    "filtered MCP and independent API view identities differ"
                );

                let first_chunk = success(
                    &server,
                    OBJECT_GET,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": first.id.as_str(),
                        "body": {"offset": 0, "max_chars": 5}
                    }),
                )
                .await;
                let complete_body = read_body(ctx.as_ref(), &first.id).await;
                assert_eq!(first_chunk["body"]["text"], "first");
                let next_offset = first_chunk["body"]["next_offset"]
                    .as_u64()
                    .expect("body continuation offset");
                let second_chunk = success(
                    &server,
                    OBJECT_GET,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": first.id.as_str(),
                        "body": {"offset": next_offset, "max_chars": 100}
                    }),
                )
                .await;
                let reconstructed = format!(
                    "{}{}",
                    first_chunk["body"]["text"].as_str().unwrap(),
                    second_chunk["body"]["text"].as_str().unwrap()
                );
                assert_eq!(reconstructed, complete_body);
                assert_eq!(
                    first_chunk["body"]["sha256"],
                    second_chunk["body"]["sha256"]
                );
                assert!(second_chunk["body"].get("next_offset").is_none());

                let uri = first_chunk["object"]["summary"]["resource_uri"]
                    .as_str()
                    .expect("canonical object resource URI");
                let resource = server
                    .state
                    .resources
                    .read_resource(
                        ReadResourceRequestParams::new(uri),
                        &CancellationToken::new(),
                    )
                    .await
                    .expect("production document resource read");
                let resource = serde_json::to_value(resource).expect("serialize resource result");
                assert_eq!(resource["contents"][0]["text"], complete_body);
                assert_eq!(resource["contents"][0]["uri"], uri);
                Ok(())
            })
        },
    ))
    .await
    .expect("prefix-authorized live view and resource harness");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable view/resource suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_mutations_are_visible_idempotent_and_conflict_safe() {
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-routed-mutations",
        |ctx| {
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let suffix = unique_suffix();
                let created_name = format!("MCP routed mutation {suffix}");
                let create_arguments = json!({
                    "space": ctx.space_id.as_str(),
                    "type": "page",
                    "name": created_name,
                    "idempotency_key": format!("mcp-live-{suffix}")
                });
                let created_a = success(&server, OBJECT_CREATE, create_arguments.clone()).await;
                let object_id = created_a["object"]["id"]
                    .as_str()
                    .expect("created object id")
                    .to_owned();
                ctx.register_object(&object_id);
                let replay = success(&server, OBJECT_CREATE, create_arguments).await;
                assert_eq!(replay["object"]["id"], object_id);
                let visible = ctx
                    .client
                    .object(&ctx.space_id, &object_id)
                    .get()
                    .await
                    .expect("read routed create");
                assert_eq!(visible.name.as_deref(), Some(created_name.as_str()));

                let initial = success(
                    &server,
                    OBJECT_GET,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "body": {"max_chars": 100}
                    }),
                )
                .await;
                let initial_hash = initial["body"]["sha256"].as_str().unwrap().to_owned();
                let updated_name = format!("MCP routed updated {suffix}");
                let updated = success(
                    &server,
                    OBJECT_UPDATE,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "name": updated_name,
                        "expected_body_sha256": initial_hash
                    }),
                )
                .await;
                let beta_hash = updated["body_sha256"].as_str().unwrap().to_owned();
                let visible = ctx
                    .client
                    .object(&ctx.space_id, &object_id)
                    .get()
                    .await
                    .expect("read routed update");
                assert_eq!(visible.name.as_deref(), Some(updated_name.as_str()));

                ctx.client
                    .update_object(&ctx.space_id, &object_id)
                    .body("gamma concurrent body")
                    .ensure_available()
                    .update()
                    .await
                    .expect("intervening concurrent writer");
                let stale = failure(
                    &server,
                    OBJECT_EDIT,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "edits": [{"old_text": "gamma", "new_text": "stale-write"}],
                        "expected_body_sha256": beta_hash
                    }),
                )
                .await;
                assert_eq!(stale["code"], "conflict");
                let concurrent_body = read_body(ctx.as_ref(), &object_id).await;
                assert!(concurrent_body.contains("gamma concurrent body"));

                let current = success(
                    &server,
                    OBJECT_GET,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "body": {"max_chars": 100}
                    }),
                )
                .await;
                let gamma_hash = current["body"]["sha256"].as_str().unwrap().to_owned();
                let count_conflict = failure(
                    &server,
                    OBJECT_EDIT,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "edits": [{
                            "old_text": "absent fragment",
                            "new_text": "must not write",
                            "expected_matches": 1
                        }],
                        "expected_body_sha256": gamma_hash
                    }),
                )
                .await;
                assert_eq!(count_conflict["code"], "conflict");
                assert_eq!(read_body(ctx.as_ref(), &object_id).await, concurrent_body);

                let edited = success(
                    &server,
                    OBJECT_EDIT,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object_id.as_str(),
                        "edits": [{"old_text": "gamma", "new_text": "delta"}],
                        "expected_body_sha256": current["body"]["sha256"]
                    }),
                )
                .await;
                assert!(edited["body_sha256"].is_string());
                assert!(
                    read_body(ctx.as_ref(), &object_id)
                        .await
                        .contains("delta concurrent body")
                );
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe live mutation suite");
    match outcome {
        DisposableRun::Completed(()) => {}
        DisposableRun::Skipped(reason) => {
            panic!("disposable routed-mutation suite skipped before callback: {reason:?}")
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_archive_applies_and_returns_verified_success() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-archive",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let suffix = unique_suffix();
                let type_key = format!("mcp_archive_diagnostic_{suffix}");
                let type_ = ctx
                    .client
                    .new_type(&ctx.space_id, format!("MCP archive diagnostic {suffix}"))
                    .key(&type_key)
                    .ensure_available()
                    .create()
                    .await
                    .expect("create cleanup-owned archive type");
                ctx.register_type(&type_.id);
                let object = create_object(
                    ctx.as_ref(),
                    &type_key,
                    &format!("MCP archive diagnostic object {suffix}"),
                    "",
                )
                .await;
                let type_id = object
                    .r#type
                    .as_ref()
                    .expect("archive diagnostic type")
                    .id
                    .clone();
                assert_eq!(type_id, type_.id);

                let retries_before = ctx.client.http_metrics().retries;
                let archived = success(
                    &server,
                    OBJECT_ARCHIVE,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object.id.as_str()
                    }),
                )
                .await;
                assert_eq!(archived["id"], object.id);
                assert_eq!(archived["archived"], true);
                assert_eq!(
                    archived["resource_uri"],
                    format!("anytype://spaces/{}/objects/{}", ctx.space_id, object.id)
                );
                assert_eq!(
                    ctx.client.http_metrics().retries,
                    retries_before,
                    "object_archive must not replay DELETE in HTTP middleware"
                );

                assert_archive_evidence(ctx.as_ref(), &object.id, &type_id).await;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe live archive workflow");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable archive suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_create_body_canonicalization_is_verified_once() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = callback_ran.clone();
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-create-body",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let suffix = unique_suffix();
                let name = format!("MCP create-body canonical {suffix}");
                let requested_body = "alpha stable body";
                let key = format!("mcp-body-canonical-{suffix}");
                let before = ctx.client.http_metrics();
                let result = call(
                    &server,
                    OBJECT_CREATE,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "type": "page",
                        "name": name,
                        "body_markdown": requested_body,
                        "idempotency_key": key
                    }),
                )
                .await;
                assert_eq!(result.is_error, Some(false), "create failed: {result:?}");
                let object_id = result
                    .structured_content
                    .as_ref()
                    .and_then(|output| output["object"]["id"].as_str())
                    .expect("created object id")
                    .to_owned();
                ctx.register_object(&object_id);
                let after_first = ctx.client.http_metrics();
                assert_eq!(after_first.total_requests - before.total_requests, 3);
                assert_eq!(after_first.retries - before.retries, 0);

                let cached = call(
                    &server,
                    OBJECT_CREATE,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "type": "page",
                        "name": name,
                        "body_markdown": "alpha stable body   \n",
                        "idempotency_key": key
                    }),
                )
                .await;
                assert_eq!(cached, result);
                assert_eq!(ctx.client.http_metrics(), after_first);

                let output = result.structured_content.expect("create-body success body");
                assert_eq!(output["object"]["id"].as_str(), Some(object_id.as_str()));
                let created = ctx
                    .client
                    .object(&ctx.space_id, &object_id)
                    .get()
                    .await
                    .expect("read canonical created object");
                assert_eq!(created.name.as_deref(), Some(name.as_str()));
                let type_id = created
                    .r#type
                    .as_ref()
                    .expect("create canonical type")
                    .id
                    .clone();
                let stored_body = created.markdown.expect("canonical created body");
                assert_eq!(stored_body, "alpha stable body   \n");
                assert_ne!(stored_body, requested_body);

                ctx.client
                    .object(&ctx.space_id, &object_id)
                    .delete()
                    .await
                    .expect("archive canonical create fixture");
                assert_archive_evidence(ctx.as_ref(), &object_id, &type_id).await;
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe any-uvg representative");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable create-body suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_exact_edit_accepts_a_converged_arbitrary_body() {
    let outcome = Box::pin(with_disposable_space_context("any-mcp-exact-edit", |ctx| {
        Box::pin(async move {
            let suffix = unique_suffix();
            let object = create_object(
                ctx.as_ref(),
                "page",
                &format!("MCP arbitrary exact edit {suffix}"),
                "",
            )
            .await;
            let requested = format!("alpha arbitrary body {suffix}");
            ctx.client
                .update_object(&ctx.space_id, &object.id)
                .body(&requested)
                .update()
                .await
                .expect("set arbitrary exact-edit fixture body");
            let server = live_server(ctx.as_ref()).await;

            let mut observed = success(
                &server,
                OBJECT_GET,
                json!({
                    "space": ctx.space_id.as_str(),
                    "object_id": object.id.as_str(),
                    "body": {"max_chars": 100_000}
                }),
            )
            .await;
            let mut converged = None;
            for _ in 0..12 {
                sleep(Duration::from_millis(100)).await;
                let next = success(
                    &server,
                    OBJECT_GET,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "object_id": object.id.as_str(),
                        "body": {"max_chars": 100_000}
                    }),
                )
                .await;
                if next["body"]["text"] == observed["body"]["text"]
                    && next["body"]["sha256"] == observed["body"]["sha256"]
                {
                    converged = Some(next);
                    break;
                }
                observed = next;
            }
            let observed = converged.expect("two independent exact body reads must converge");
            let observed_body = observed["body"]["text"]
                .as_str()
                .expect("complete observed body");
            assert_eq!(observed_body.matches("arbitrary").count(), 1);

            let edited = success(
                &server,
                OBJECT_EDIT,
                json!({
                    "space": ctx.space_id.as_str(),
                    "object_id": object.id.as_str(),
                    "edits": [{
                        "old_text": "arbitrary",
                        "new_text": "verified",
                        "expected_matches": 1
                    }],
                    "expected_body_sha256": observed["body"]["sha256"]
                }),
            )
            .await;
            assert!(edited["body_sha256"].is_string());
            let stored = read_body(ctx.as_ref(), &object.id).await;
            assert_eq!(stored.matches("verified").count(), 1);
            assert!(!stored.contains("arbitrary"));
            Ok(())
        })
    }))
    .await
    .expect("cleanup-safe arbitrary exact-edit sentinel");
    match outcome {
        DisposableRun::Completed(()) => {}
        DisposableRun::Skipped(reason) => {
            panic!("disposable exact-edit suite skipped before callback: {reason:?}")
        }
    }
}

#[test]
fn advertised_catalog_has_exact_live_scenario_ownership() {
    live_scenario::validate_live_ownership(
        ALL_TOOL_NAMES.as_slice(),
        &[
            "resources/list",
            "resources/read",
            "resources/templates/list",
        ],
    )
    .expect("complete typed executable live ownership");
}

#[test]
fn advertised_optional_catalog_has_exact_typed_scenario_ownership() {
    let metadata = production_optional_metadata();
    let selector = metadata
        .iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>()
        .join(",");
    let selection = OptionalToolsetSelection::parse(Some(selector), &metadata)
        .expect("all linked production optional toolsets");
    let client = AnytypeClient::with_config(ClientConfig {
        base_url: Some("http://127.0.0.1:1".to_owned()),
        keystore: Some("env".to_owned()),
        keystore_service: Some("any-mcp-optional-ownership-test".to_owned()),
        app_name: "any-mcp-optional-ownership-test".to_owned(),
        ..ClientConfig::default()
    })
    .expect("non-I/O optional ownership client");
    client.set_api_key(HttpCredentials::new("fixture-token"));
    let runtime = RuntimeContext::from_parts_with_profile_and_optional_toolsets(
        client,
        1,
        Duration::from_secs(1),
        StartupStatus {
            http_available: true,
            grpc_available: true,
        },
        ApplicationProfile::Standard,
        false,
        selection,
    );
    let server = AnyMcpServer::new(runtime).expect("all-selected production optional catalog");

    let phase_one = ALL_TOOL_NAMES.iter().copied().collect::<HashSet<_>>();
    let optional_tools = server
        .tools()
        .iter()
        .map(|tool| tool.name.to_string())
        .filter(|name| !phase_one.contains(name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(optional_tools.len(), 30);
    let optional_tool_refs = optional_tools
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();

    let templates = serde_json::to_value(
        server
            .list_resource_templates_wire(None)
            .expect("all-selected production resource templates"),
    )
    .expect("serialize production resource templates");
    let optional_resource_families = templates["resourceTemplates"]
        .as_array()
        .expect("resource template array")
        .iter()
        .filter_map(|template| template["uriTemplate"].as_str())
        .filter(|uri| uri.starts_with("anytype-file://"))
        .collect::<Vec<_>>();
    assert_eq!(optional_resource_families.len(), 1);

    let mut scenario_declarations = vec![
        OptionalScenarioDeclaration::fast(
            OptionalRegistry::CommonFoundation,
            "optional_toolset_status_direct_contract",
        ),
        OptionalScenarioDeclaration::fast(
            OptionalRegistry::CommonFoundation,
            "optional_toolset_status_stdio_contract",
        ),
        OptionalScenarioDeclaration::real_headless(
            OptionalRegistry::CommonFoundation,
            "common_optional_status_headless",
        ),
    ];
    for registry in production_optional_registries() {
        let registry_id = OptionalRegistry::from_name(registry.metadata().name)
            .expect("known production optional registry identity");
        scenario_declarations.extend(
            registry
                .scripted_scenario_ids()
                .iter()
                .copied()
                .map(|scenario| OptionalScenarioDeclaration::fast(registry_id, scenario)),
        );
        scenario_declarations.extend(
            registry
                .headless_scenario_ids()
                .iter()
                .copied()
                .map(|scenario| OptionalScenarioDeclaration::real_headless(registry_id, scenario)),
        );
    }
    assert_eq!(scenario_declarations.len(), 66);
    live_scenario::validate_optional_live_ownership(
        &optional_tool_refs,
        &optional_resource_families,
        &scenario_declarations,
    )
    .expect("complete typed fast and real-headless optional ownership");
    assert_eq!(
        OptionalOperation::ALL
            .into_iter()
            .map(OptionalOperation::fast_workflow)
            .collect::<HashSet<_>>(),
        OptionalFastWorkflow::ALL.into_iter().collect()
    );
    assert_eq!(
        OptionalOperation::ALL
            .into_iter()
            .map(OptionalOperation::real_workflow)
            .collect::<HashSet<_>>(),
        OptionalRealWorkflow::ALL.into_iter().collect()
    );
}

async fn run_direct_baseline(scenario: ScenarioId) {
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None));
    let captured = failure.clone();
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let cleanup = Box::pin(with_disposable_space_context(
        "any-mcp-direct-standard",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_server(ctx.as_ref()).await;
                let before = ctx.client.http_metrics();
                let mut driver = DirectRouterDriver { server: &server };
                let mut evidence = ScenarioEvidence::new(scenario);
                if let Err(error) = run_scenario(
                    scenario,
                    &mut driver,
                    ctx.as_ref(),
                    &mut evidence,
                )
                .await
                {
                    let after = ctx.client.http_metrics();
                    *captured.lock().expect("failure evidence lock") = Some(format!(
                        "scenario={} fixtures={:?} error={} http_before={before:?} http_after={after:?}",
                        evidence.scenario.as_str(),
                        evidence.fixture_ids,
                        evidence.sanitize(&error)
                    ));
                }
                Ok(())
            })
        },
    ))
    .await;
    if let Some(failure) = failure.lock().expect("failure evidence lock").take() {
        panic!(
            "{failure} cleanup={}",
            if cleanup.is_ok() { "success" } else { "failed" }
        );
    }
    match cleanup.expect("cleanup-safe disposable direct baseline scenario") {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("disposable direct baseline skipped before callback: {reason:?}");
        }
    }
}

macro_rules! direct_baseline_test {
    ($name:ident, $scenario:expr) => {
        #[tokio::test]
        #[serial_test::serial]
        #[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
        async fn $name() {
            run_direct_baseline($scenario).await;
        }
    };
}

direct_baseline_test!(headless_direct_standard_discovery, ScenarioId::Discovery);
direct_baseline_test!(headless_direct_standard_documents, ScenarioId::Documents);
direct_baseline_test!(headless_direct_standard_views, ScenarioId::Views);
direct_baseline_test!(headless_direct_standard_mutations, ScenarioId::Mutations);
direct_baseline_test!(
    headless_direct_standard_markdown_noop,
    ScenarioId::MarkdownNoop
);
direct_baseline_test!(headless_direct_standard_archive, ScenarioId::Archive);

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_direct_members_minimizes_personal_data() {
    let callback_ran = Arc::new(AtomicBool::new(false));
    let callback_flag = Arc::clone(&callback_ran);
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-direct-members",
        move |ctx| {
            callback_flag.store(true, Ordering::SeqCst);
            Box::pin(async move {
                let server = live_members_server(ctx.as_ref(), false).await;
                let tools = server
                    .tools()
                    .iter()
                    .map(|tool| tool.name.as_ref())
                    .collect::<Vec<_>>();
                assert!(tools.contains(&"member_list"));
                assert!(tools.contains(&"member_get"));
                assert!(tools.contains(&"optional_toolset_status"));
                let status = success(&server, "optional_toolset_status", json!({})).await;
                assert_eq!(status["configured_toolsets"], json!(["members"]));
                assert_eq!(status["active_toolsets"], json!(["members"]));

                let page = success(
                    &server,
                    "member_list",
                    json!({"space": ctx.space_id, "limit": 100}),
                )
                .await;
                let items = page["items"].as_array().expect("members page items");
                assert!(!items.is_empty(), "disposable space has an owner member");
                assert!(page.get("next_cursor").is_none());
                for item in items {
                    let wire = item.to_string();
                    for forbidden in ["identity", "global_name", "globalName", "icon"] {
                        assert!(!wire.contains(forbidden));
                    }
                    let id = item["id"].as_str().expect("exact member id");
                    let exact = success(
                        &server,
                        "member_get",
                        json!({"space": ctx.space_id, "member_id": id}),
                    )
                    .await;
                    assert_eq!(exact["member"], *item);
                }

                let read_only = live_members_server(ctx.as_ref(), true).await;
                let read_only_tools = read_only
                    .tools()
                    .iter()
                    .map(|tool| tool.name.as_ref())
                    .collect::<Vec<_>>();
                assert!(read_only_tools.contains(&"member_list"));
                assert!(read_only_tools.contains(&"member_get"));
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe direct members suite");
    match outcome {
        DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
        DisposableRun::Skipped(reason) => {
            assert!(!callback_ran.load(Ordering::SeqCst));
            eprintln!("direct members suite skipped before callback: {reason:?}");
        }
    }
}

#[tokio::test]
#[serial_test::serial(disposable_anytype_api)]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_direct_chats_registry_runs_all_six_workflows() {
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-direct-chats-registry",
        |ctx| {
            Box::pin(async move {
                let suffix = unique_suffix();
                let query = format!("mcpchats{suffix}");
                let chat = ctx
                    .client
                    .chats()
                    .in_space(&ctx.space_id)
                    .create(
                        format!("MCP chats registry {suffix}"),
                        Icon::Emoji {
                            emoji: "💬".to_owned(),
                        },
                    )
                    .create()
                    .await
                    .map_err(|_| {
                        eprintln!("direct chats registry chat fixture creation failed");
                        TestError::Assertion {
                            message: "create direct chats registry fixture".to_owned(),
                        }
                    })?;
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
                    .await
                    .map_err(|_| {
                        eprintln!("direct chats registry seed creation failed");
                        TestError::Assertion {
                            message: "create direct chats registry seed".to_owned(),
                        }
                    })?;
                ctx.register_chat_message(&chat.id, &seed_id)?;

                let server = live_chats_server(ctx.as_ref()).await;
                let mut driver = DirectRouterDriver { server: &server };
                let evidence = Box::pin(run_chats_registry_scenario(
                    &mut driver,
                    ChatsRegistryFixture {
                        space_id: &ctx.space_id,
                        chat_id: &chat.id,
                        seed_message_id: &seed_id,
                        search_query: &query,
                        add_text: &format!("direct chats registry {suffix}"),
                        idempotency_key: &format!("direct-chats-registry-{suffix}"),
                    },
                ))
                .await
                .map_err(|message| {
                    eprintln!("direct chats registry protocol scenario failed: {message}");
                    TestError::Assertion { message }
                })?;
                assert_eq!(evidence.chat_id, chat.id);
                assert_eq!(evidence.seed_message_id, seed_id);
                assert!(evidence.deleted);
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe direct chats registry acceptance");
    if let DisposableRun::Skipped(reason) = outcome {
        eprintln!("direct chats registry acceptance skipped before callback: {reason:?}");
    }
}

#[test]
#[serial_test::serial(disposable_anytype_api)]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
fn headless_direct_body_blocks_runs_shared_scenario() {
    run_live_scenario_on_large_stack("direct-body-blocks", || async {
        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&callback_ran);
        let outcome = Box::pin(with_disposable_space_context(
            "any-mcp-direct-body-blocks",
            move |ctx| {
                callback_flag.store(true, Ordering::SeqCst);
                Box::pin(async move {
                    let server = live_body_server(ctx.as_ref()).await;
                    let mut driver = DirectRouterDriver { server: &server };
                    let evidence = run_body_scenario(&mut driver, ctx.as_ref(), "direct")
                        .await
                        .map_err(|failure| TestError::DisposableCallback {
                            stage: DisposableCallbackStage::BodyDirect,
                            category: failure.category(),
                        })?;
                    if evidence.normalized_results.is_empty()
                        || evidence.listed_block_count != BODY_PAGINATION_ITEM_COUNT
                    {
                        return Err(TestError::Assertion {
                            message: "direct shared body evidence was incomplete".to_owned(),
                        });
                    }
                    Ok(())
                })
            },
        ))
        .await
        .expect("cleanup-safe direct shared body scenario");
        match outcome {
            DisposableRun::Completed(()) => assert!(callback_ran.load(Ordering::SeqCst)),
            DisposableRun::Skipped(reason) => {
                assert!(!callback_ran.load(Ordering::SeqCst));
                eprintln!("direct shared body scenario skipped before callback: {reason:?}");
            }
        }
    });
}

#[test]
#[serial_test::serial(disposable_anytype_api)]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
fn headless_direct_ordinary_tools_cover_representative_layouts() {
    run_live_scenario_on_large_stack("direct-representative-layouts", || async {
        let outcome = Box::pin(with_disposable_space_context(
            "any-mcp-direct-layouts",
            |ctx| {
                Box::pin(async move {
                    let server = live_views_write_server(ctx.as_ref()).await;
                    let mut driver = DirectRouterDriver { server: &server };
                    let evidence = Box::pin(run_representative_layout_scenario(
                        &mut driver,
                        ctx.as_ref(),
                    ))
                    .await
                    .map_err(|message| {
                        eprintln!("direct representative-layout scenario failed: stage={message}");
                        TestError::Assertion { message }
                    })?;
                    assert_eq!(evidence.member_ids.len(), 3);
                    assert_ne!(evidence.kanban_view_id, evidence.grid_view_id);
                    Ok(())
                })
            },
        ))
        .await
        .expect("cleanup-safe direct representative-layout acceptance");
        if let DisposableRun::Skipped(reason) = outcome {
            eprintln!(
                "direct representative-layout acceptance skipped before callback: {reason:?}"
            );
        }
    });
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_direct_compact_read_sentinel() {
    let outcome = Box::pin(with_disposable_space_context(
        "any-mcp-compact-read",
        |ctx| {
            Box::pin(async move {
                let object =
                    create_object(ctx.as_ref(), "page", "MCP compact sentinel", "sentinel").await;
                let server =
                    live_server_with(ctx.as_ref(), ApplicationProfile::Compact, false).await;
                let mut driver = DirectRouterDriver { server: &server };
                assert_eq!(
                    driver.list_tools().await.expect("compact catalog"),
                    [
                        "object_edit",
                        "object_get",
                        "object_search",
                        "server_status"
                    ]
                );
                driver
                    .call_tool(
                        "object_get",
                        json!({"space": ctx.space_id, "object_id": object.id}),
                    )
                    .await
                    .expect("compact real-headless read");
                Ok(())
            })
        },
    ))
    .await
    .expect("cleanup-safe compact sentinel");
    match outcome {
        DisposableRun::Completed(()) => {}
        DisposableRun::Skipped(reason) => {
            panic!("disposable compact-read suite skipped before callback: {reason:?}")
        }
    }
}

#[tokio::test]
#[serial_test::serial]
#[ignore = "requires env-only disposable credentials and an authenticated headless Anytype server"]
async fn headless_direct_read_only_sentinel() {
    let outcome = Box::pin(with_disposable_space_context("any-mcp-read-only", |ctx| {
        Box::pin(async move {
            let object =
                create_object(ctx.as_ref(), "page", "MCP read-only sentinel", "sentinel").await;
            let server = live_server_with(ctx.as_ref(), ApplicationProfile::Standard, true).await;
            let mut driver = DirectRouterDriver { server: &server };
            let tools = driver.list_tools().await.expect("read-only catalog");
            assert!(!tools.iter().any(|name| matches!(
                name.as_str(),
                OBJECT_CREATE | OBJECT_UPDATE | OBJECT_EDIT | OBJECT_ARCHIVE
            )));
            driver
                .call_tool(
                    "object_get",
                    json!({"space": ctx.space_id, "object_id": object.id}),
                )
                .await
                .expect("read-only real-headless read");
            let rejected = failure(&server, OBJECT_EDIT, json!({})).await;
            assert_eq!(rejected["code"], "validation");
            assert!(
                rejected["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("read-only")),
                "direct mutation dispatch reaches the read-only guard"
            );
            Ok(())
        })
    }))
    .await
    .expect("cleanup-safe read-only sentinel");
    match outcome {
        DisposableRun::Completed(()) => {}
        DisposableRun::Skipped(reason) => {
            panic!("disposable read-only suite skipped before callback: {reason:?}")
        }
    }
}
