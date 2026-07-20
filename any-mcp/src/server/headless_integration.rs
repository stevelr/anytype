// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Cleanup-safe tests of the production router against a headless Anytype server.

use std::{collections::HashSet, time::Duration};

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

async fn assert_terminal_page(
    server: &AnyMcpServer,
    name: &'static str,
    mut base: JsonObject,
    limit: u16,
) -> Value {
    base.insert("limit".to_owned(), json!(limit));
    let page = success(server, name, Value::Object(base)).await;
    assert!(page["items"].is_array());
    assert!(page.get("next_cursor").is_none(), "{name} must be terminal");
    page
}

async fn assert_ambient_space_page(server: &AnyMcpServer, current_space_id: &str) -> Value {
    let base = arguments(json!({"limit": 100}));
    let page = success(server, SPACE_LIST, Value::Object(base.clone())).await;
    let items = page["items"]
        .as_array()
        .expect("space_list items must be an array");
    assert!(!items.is_empty(), "space_list must expose the test space");
    assert!(
        items
            .iter()
            .any(|item| item_id(item).and_then(Value::as_str) == Some(current_space_id)),
        "space_list first page must contain the current test space"
    );

    if let Some(cursor) = page.get("next_cursor") {
        let cursor = cursor
            .as_str()
            .filter(|cursor| !cursor.is_empty())
            .expect("space_list next_cursor must be a nonempty string");
        let mut mismatched = base;
        mismatched.insert("limit".to_owned(), json!(99));
        mismatched.insert("cursor".to_owned(), json!(cursor));
        let mismatch = failure(server, SPACE_LIST, Value::Object(mismatched)).await;
        assert_eq!(mismatch["code"], "validation", "space_list cursor binding");
    }

    page
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

            assert_ambient_space_page(&server, &ctx.space_id).await;
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

            let templates = assert_terminal_page(
                &server,
                TEMPLATE_LIST,
                arguments(json!({
                    "space": ctx.space_id.as_str(),
                    "type": first_type.id.as_str()
                })),
                1,
            )
            .await;
            assert!(templates["items"].as_array().unwrap().is_empty());
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
                .client
                .resolve_type(&ctx.space_id, "collection")
                .await
                .expect("discover and validate collection-layout type");
            assert_eq!(collection_type.layout, ObjectLayout::Collection);
            let collection = create_object(
                ctx.as_ref(),
                &collection_type.key,
                &format!("MCP collection {}", unique_suffix()),
                "",
            )
            .await;
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

            let mut view_page = None;
            for _ in 0..10 {
                let result = call(
                    &server,
                    VIEW_LIST,
                    json!({
                        "space": ctx.space_id.as_str(),
                        "list_id": collection.id.as_str(),
                        "limit": 1
                    }),
                )
                .await;
                if result.is_error == Some(false)
                    && result
                        .structured_content
                        .as_ref()
                        .and_then(|value| value["items"].as_array())
                        .is_some_and(|items| !items.is_empty())
                {
                    view_page = result.structured_content;
                    break;
                }
                sleep(Duration::from_millis(500)).await;
            }
            let view_page = view_page.expect("new collection exposes a readable view");
            let view_id = view_page["items"][0]["id"]
                .as_str()
                .expect("safe discovered view id")
                .to_owned();

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
            let object_id = output["object"]["id"]
                .as_str()
                .expect("created object id")
                .to_owned();
            ctx.register_object(&object_id);
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
