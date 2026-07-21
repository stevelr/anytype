// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Cleanup-safe tests of the production router against a headless Anytype server.

use std::{
    collections::{BTreeMap, HashSet},
    time::Duration,
};

use anytype::{
    prelude::{Color, Object, ObjectLayout, PropertyFormat},
    test_util::{TestContext, unique_suffix, with_test_context},
};
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject, ReadResourceRequestParams};
use serde_json::{Value, json};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::runtime::{RuntimeContext, StartupStatus};

fn arguments(value: Value) -> JsonObject {
    value
        .as_object()
        .expect("live test tool arguments must be an object")
        .clone()
}

async fn live_server(ctx: &TestContext) -> AnyMcpServer {
    ctx.client
        .ping_http()
        .await
        .expect("live suite requires authenticated HTTP");
    ctx.client
        .ping_grpc()
        .await
        .expect("live suite requires authenticated gRPC");
    let runtime = RuntimeContext::from_parts(
        ctx.client.clone(),
        1,
        Duration::from_secs(30),
        StartupStatus {
            http_available: true,
            grpc_available: true,
        },
    );
    AnyMcpServer::new(runtime).expect("production MCP catalog")
}

async fn call(server: &AnyMcpServer, name: &'static str, value: Value) -> CallToolResult {
    Box::pin(server.dispatch_tool(
        CallToolRequestParams::new(name).with_arguments(arguments(value)),
        &CancellationToken::new(),
    ))
    .await
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

fn item_id(item: &Value) -> Option<&Value> {
    item.get("id")
        .or_else(|| item.get("summary").and_then(|summary| summary.get("id")))
        .or_else(|| item.get("object").and_then(|object| object.get("id")))
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

async fn assert_fixture_space_continuation(
    ctx: &TestContext,
    server: &AnyMcpServer,
    fixture_ids: &[&str],
) {
    const MAX_SPACE_LIST_PAGES: usize = 1_000;

    let response = ctx
        .client
        .spaces()
        .limit(1_000)
        .offset(0)
        .list()
        .await
        .expect("list registered space fixtures")
        .into_response();
    assert_eq!(response.pagination.offset, 0);
    assert!(!response.pagination.has_more);
    assert_eq!(response.pagination.total, response.items.len());
    let expected_ids: HashSet<String> = response
        .items
        .iter()
        .map(|space| space.id.clone())
        .collect();
    assert_eq!(expected_ids.len(), response.items.len());
    for fixture_id in fixture_ids {
        assert!(
            response.items.iter().any(|space| space.id == *fixture_id),
            "registered fixture must be present in complete space listing"
        );
    }
    assert!(
        response.items.len() >= 2,
        "two registered fixtures must force limit=1 continuation"
    );
    assert!(response.items.len() <= MAX_SPACE_LIST_PAGES);

    let mut next_cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut binding_checked = false;
    let mut reached_terminal = false;
    for _ in 0..MAX_SPACE_LIST_PAGES {
        let mut request = arguments(json!({"limit": 1}));
        if let Some(cursor) = &next_cursor {
            request.insert("cursor".to_owned(), json!(cursor));
        }
        let page = success(server, SPACE_LIST, Value::Object(request)).await;
        let items = page["items"]
            .as_array()
            .expect("space_list items must be an array");
        assert_eq!(items.len(), 1, "each bounded space page must progress");
        let id = item_id(&items[0])
            .and_then(Value::as_str)
            .expect("space_list item id");
        assert!(
            seen_ids.insert(id.to_owned()),
            "space_list must not repeat an item while advancing"
        );

        let Some(cursor) = page.get("next_cursor") else {
            reached_terminal = true;
            break;
        };
        let cursor = cursor
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .expect("space_list next_cursor must be a nonempty string")
            .to_owned();
        assert!(
            seen_cursors.insert(cursor.clone()),
            "space_list cursor chain must not loop"
        );

        if !binding_checked {
            let mismatch = failure(
                server,
                SPACE_LIST,
                json!({"limit": 2, "cursor": cursor.as_str()}),
            )
            .await;
            assert_eq!(mismatch["code"], "validation", "space_list cursor binding");
            binding_checked = true;
        }
        next_cursor = Some(cursor);
    }

    assert!(
        binding_checked,
        "fixture-backed listing must expose a cursor"
    );
    assert!(
        reached_terminal,
        "space_list must terminate within its hard bound"
    );
    assert_eq!(seen_ids, expected_ids);
    for fixture_id in fixture_ids {
        assert!(
            seen_ids.contains(*fixture_id),
            "cursor walk must observe each registered fixture id"
        );
    }
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

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_default_discovery_routes_paginate_and_report_ambiguity() {
    Box::pin(with_test_context(|ctx| {
        Box::pin(async move {
            let server = live_server(ctx.as_ref()).await;
            let status = success(&server, SERVER_STATUS, json!({})).await;
            assert_eq!(status["http_available"], true);
            assert_eq!(status["grpc_available"], true);

            let first_space = ctx
                .create_space_fixture(format!("MCP pagination space {}", unique_suffix()))
                .await
                .expect("create first disposable space");
            let second_space = ctx
                .create_space_fixture(format!("MCP pagination space {}", unique_suffix()))
                .await
                .expect("create second disposable space");

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

            let template_fixtures = ctx
                .create_template_fixtures(
                    format!("MCP template type {}", unique_suffix()),
                    [
                        format!("MCP template first {}", unique_suffix()),
                        format!("MCP template second {}", unique_suffix()),
                    ],
                )
                .await
                .expect("create cleanup-owned template fixtures");

            let property = ctx
                .client
                .new_property(
                    &ctx.space_id,
                    format!("MCP pagination select {}", unique_suffix()),
                    PropertyFormat::Select,
                )
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
            for (name, color) in [("First", Color::Blue), ("Second", Color::Red)] {
                ctx.client
                    .new_tag(&ctx.space_id, &property.id)
                    .name(format!("{name} {}", unique_suffix()))
                    .color(color)
                    .create()
                    .await
                    .expect("create tag fixture");
            }

            let search_term = format!("McpPagination{}", unique_suffix());
            let first_object =
                create_object(ctx.as_ref(), "page", &format!("{search_term} first"), "").await;
            let second_object =
                create_object(ctx.as_ref(), "page", &format!("{search_term} second"), "").await;
            sleep(Duration::from_millis(300)).await;

            assert_fixture_space_continuation(
                ctx.as_ref(),
                &server,
                &[first_space.id.as_str(), second_space.id.as_str()],
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
    }))
    .await
    .expect("cleanup-safe live discovery suite");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_view_body_and_resource_routes_are_complete_and_bound() {
    Box::pin(with_test_context(|ctx| {
        Box::pin(async move {
            let server = live_server(ctx.as_ref()).await;
            let collection_type = ctx
                .create_collection_type_fixture(format!("MCP collection type {}", unique_suffix()))
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
            let first = create_object(
                ctx.as_ref(),
                "page",
                &format!("MCP collection first {}", unique_suffix()),
                "first resource body",
            )
            .await;
            let second = create_object(
                ctx.as_ref(),
                "page",
                &format!("MCP collection second {}", unique_suffix()),
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

            let mut listed = None;
            for _ in 0..10 {
                let result = call(
                    &server,
                    VIEW_OBJECT_LIST,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "list_id": collection.id.as_str(),
                        "view": view_id.as_str(),
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
            assert!(
                observed.contains(first.id.as_str()),
                "first collection member missing from pages: {listed} / {continued}"
            );
            assert!(
                observed.contains(second.id.as_str()),
                "second collection member missing from pages: {listed} / {continued}"
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
    }))
    .await
    .expect("cleanup-safe live view and resource suite");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_mutations_are_visible_idempotent_and_conflict_safe() {
    Box::pin(with_test_context(|ctx| {
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
    }))
    .await
    .expect("cleanup-safe live mutation suite");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_archive_applies_and_returns_verified_success() {
    Box::pin(with_test_context(|ctx| {
        Box::pin(async move {
            let server = live_server(ctx.as_ref()).await;
            let object = create_object(
                ctx.as_ref(),
                "page",
                &format!("MCP archive diagnostic {}", unique_suffix()),
                "",
            )
            .await;
            let type_id = object
                .r#type
                .as_ref()
                .expect("archive diagnostic type")
                .id
                .clone();

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
    }))
    .await
    .expect("cleanup-safe live archive workflow");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_create_body_canonicalization_is_verified_once() {
    Box::pin(with_test_context(|ctx| {
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
    }))
    .await
    .expect("cleanup-safe any-uvg representative");
}

#[tokio::test]
#[ignore = "requires source .test-env and an authenticated headless Anytype server"]
async fn headless_exact_edit_accepts_a_converged_arbitrary_body() {
    Box::pin(with_test_context(|ctx| {
        Box::pin(async move {
            let server = live_server(ctx.as_ref()).await;
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
}
