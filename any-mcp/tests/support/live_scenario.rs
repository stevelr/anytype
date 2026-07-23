// any-mcp - bounded, workflow-oriented MCP server for Anytype
//
// SPDX-FileCopyrightText: 2026 Steve Schoettler
// SPDX-License-Identifier: Apache-2.0

//! Transport-neutral scenarios and live-coverage ownership declarations.
#![cfg_attr(not(feature = "acceptance-harness"), allow(dead_code))]

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    future::Future,
    pin::Pin,
    time::Duration,
};

use anytype::{
    body::{
        BlockContent, BodyBlock, BodySnapshot, CalloutIcon, DividerStyle, EmbedProcessor,
        HorizontalAlign, LayoutStyle, LinkCardStyle, LinkDescriptionMode, LinkIconSize, MarkKind,
        TextStyle, VerticalAlign,
    },
    prelude::{BodyOp, Color, InsertPosition, NewBlock, ObjectLayout, PropertyFormat},
    test_util::{TestContext, unique_suffix},
};

/// Seeded value that must never cross into spawned diagnostics.
pub const BODY_DIAGNOSTIC_SECRET: &str = "SECRET_BODY_DIAGNOSTIC_SENTINEL";
/// Exact root-inclusive DFS item count in the live pagination fixture.
pub const BODY_PAGINATION_ITEM_COUNT: usize = 20;

fn body_fixture_marker(event: &'static str) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=fixture event={event}");
    }
}

fn body_scenario_marker(event: &'static str) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=scenario event={event}");
    }
}

fn body_scenario_count(event: &'static str, count: usize) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=scenario event={event} count={count}");
    }
}

fn body_scenario_check(event: &'static str, ok: bool) -> bool {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=scenario event={event} ok={ok}");
    }
    ok
}

fn body_scenario_update(index: usize, result: &'static str) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=scenario event=update index={index} result={result}");
    }
}

fn body_fixture_count(event: &'static str, count: usize) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!("body_semantic_phase=fixture event={event} count={count}");
    }
}

fn body_fixture_plan_diagnostic(initial: usize, append: usize, planned: usize) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!(
            "body_semantic_phase=fixture event=plan initial={initial} \
             append={append} planned={planned}"
        );
    }
}

fn body_fixture_outcome_diagnostic(
    category: &'static str,
    applied: usize,
    failed: usize,
    not_attempted: usize,
) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!(
            "body_semantic_phase=fixture event=apply_all category={category} \
             applied={applied} failed={failed} not_attempted={not_attempted}"
        );
    }
}

fn body_fixture_receipt_diagnostic(
    index: usize,
    affected: usize,
    address_ok: bool,
    block_present: bool,
    content_ok: bool,
    root_last_ok: bool,
) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_some() {
        eprintln!(
            "body_semantic_phase=fixture event=receipt index={index} affected={affected} \
             address_ok={address_ok} block_present={block_present} \
             content_ok={content_ok} root_last_ok={root_last_ok}"
        );
    }
}

fn body_fixture_shape_diagnostic(
    snapshot: &BodySnapshot,
    initial_blocks: &[BodyBlock],
    created_ids: &[String],
    expected_suffix: &[(TextStyle, String)],
) {
    if std::env::var_os("ANY_MCP_BODY_SEMANTIC_DIAGNOSTICS").is_none() {
        return;
    }
    let blocks = snapshot.iter().collect::<Vec<_>>();
    let suffix = blocks.get(initial_blocks.len()..);
    let prefix_ok = body_initial_full_prefix_unchanged(&blocks, initial_blocks);
    let root_children_prefix_ok = body_initial_root_children_preserved(snapshot, initial_blocks);
    let suffix_count_ok = suffix.is_some_and(|items| {
        items.len() == created_ids.len() && items.len() == expected_suffix.len()
    });
    let suffix_ids_ok = suffix.is_some_and(|items| {
        items
            .iter()
            .map(|block| block.id.as_str())
            .eq(created_ids.iter().map(String::as_str))
    });
    let suffix_content_ok = suffix.is_some_and(|items| {
        items
            .iter()
            .zip(expected_suffix)
            .all(|(block, (expected_style, expected_text))| {
                matches!(
                    &block.content,
                    BlockContent::Text(content)
                        if content.style == *expected_style
                            && content.text == *expected_text
                )
            })
    });
    let direct_root_ok = snapshot.root().children.len() >= created_ids.len()
        && snapshot
            .root()
            .children
            .iter()
            .rev()
            .take(created_ids.len())
            .map(|id| id.as_str())
            .eq(created_ids.iter().rev().map(String::as_str));
    eprintln!(
        "body_semantic_phase=fixture event=shape total={} expected={} prefix_ok={prefix_ok} \
         root_children_prefix_ok={root_children_prefix_ok} suffix_count_ok={suffix_count_ok} \
         suffix_ids_ok={suffix_ids_ok} suffix_content_ok={suffix_content_ok} \
         direct_root_ok={direct_root_ok}",
        blocks.len(),
        BODY_PAGINATION_ITEM_COUNT
    );
}

fn body_block_state_except_children_matches(actual: &BodyBlock, expected: &BodyBlock) -> bool {
    actual.id == expected.id
        && actual.content == expected.content
        && actual.align == expected.align
        && actual.vertical_align == expected.vertical_align
        && actual.background_color == expected.background_color
        && actual.restrictions == expected.restrictions
}

fn body_initial_full_prefix_unchanged(blocks: &[&BodyBlock], initial_blocks: &[BodyBlock]) -> bool {
    blocks.len() >= initial_blocks.len()
        && blocks
            .iter()
            .zip(initial_blocks)
            .enumerate()
            .all(|(index, (actual, expected))| {
                if index == 0 {
                    body_block_state_except_children_matches(actual, expected)
                } else {
                    *actual == expected
                }
            })
}

fn body_initial_root_children_preserved(
    snapshot: &BodySnapshot,
    initial_blocks: &[BodyBlock],
) -> bool {
    initial_blocks.first().is_some_and(|initial_root| {
        snapshot.root().children.get(..initial_root.children.len())
            == Some(initial_root.children.as_slice())
    })
}

fn body_pagination_suffix_spec(append_count: usize) -> Vec<(TextStyle, String)> {
    (0..append_count)
        .map(|index| {
            if index == 0 {
                (TextStyle::Header1, "Existing heading".to_owned())
            } else {
                (TextStyle::Paragraph, format!("Paragraph {}", index - 1))
            }
        })
        .collect()
}

fn body_pagination_append_operations(append_count: usize) -> Result<Vec<BodyOp>, String> {
    if append_count > BODY_PAGINATION_ITEM_COUNT {
        return Err("body fixture append count exceeds the exact page size".to_owned());
    }
    let mut operations = Vec::with_capacity(append_count);
    for (style, text) in body_pagination_suffix_spec(append_count) {
        let block = match style {
            TextStyle::Header1 => NewBlock::heading(1, text)
                .map_err(|_| "body fixture heading constructor failed".to_owned())?,
            TextStyle::Paragraph => NewBlock::paragraph(text)
                .map_err(|_| "body fixture paragraph constructor failed".to_owned())?,
            _ => return Err("body fixture plan contained an unsupported style".to_owned()),
        };
        operations.push(BodyOp::Append { block });
    }
    Ok(operations)
}

fn is_exact_body_pagination_fixture(
    snapshot: &BodySnapshot,
    initial_blocks: &[BodyBlock],
    created_ids: &[String],
    expected_suffix: &[(TextStyle, String)],
) -> bool {
    let blocks = snapshot.iter().collect::<Vec<_>>();
    let Some(suffix) = blocks.get(initial_blocks.len()..) else {
        return false;
    };
    let prefix_unchanged = body_initial_full_prefix_unchanged(&blocks, initial_blocks);
    let root_children_prefix_unchanged =
        body_initial_root_children_preserved(snapshot, initial_blocks);
    let suffix_ids_match = suffix
        .iter()
        .map(|block| block.id.as_str())
        .eq(created_ids.iter().map(String::as_str));
    let suffix_content_matches =
        suffix
            .iter()
            .zip(expected_suffix)
            .all(|(block, (expected_style, expected_text))| {
                matches!(
                    &block.content,
                    BlockContent::Text(content)
                        if content.style == *expected_style && content.text == *expected_text
                )
            });
    let direct_root_suffix = snapshot.root().children.len() >= created_ids.len()
        && snapshot
            .root()
            .children
            .iter()
            .rev()
            .take(created_ids.len())
            .map(|id| id.as_str())
            .eq(created_ids.iter().rev().map(String::as_str));
    blocks.len() == BODY_PAGINATION_ITEM_COUNT
        && prefix_unchanged
        && root_children_prefix_unchanged
        && suffix.len() == created_ids.len()
        && suffix.len() == expected_suffix.len()
        && suffix_ids_match
        && suffix_content_matches
        && direct_root_suffix
}

#[test]
fn body_pagination_fixture_plan_fills_twenty_including_preserved_root_prefix() {
    let expected = body_pagination_suffix_spec(16);
    assert_eq!(expected.len(), 16);
    assert_eq!(
        expected.first(),
        Some(&(TextStyle::Header1, "Existing heading".to_owned()))
    );
    assert_eq!(
        expected.last(),
        Some(&(TextStyle::Paragraph, "Paragraph 14".to_owned()))
    );
    for initial_count in [1, 4, 19] {
        let append_count = BODY_PAGINATION_ITEM_COUNT - initial_count;
        let operations =
            body_pagination_append_operations(append_count).expect("valid fixture constructors");
        assert_eq!(initial_count + operations.len(), BODY_PAGINATION_ITEM_COUNT);
        assert!(
            operations
                .iter()
                .all(|operation| matches!(operation, BodyOp::Append { .. }))
        );
        assert_eq!(
            body_pagination_suffix_spec(append_count)
                .first()
                .map(|(style, _)| *style),
            Some(TextStyle::Header1)
        );
    }
}

/// Content-free evidence from one transport-neutral rich-body workflow.
#[derive(Debug, PartialEq, Eq)]
pub struct BodyScenarioEvidence {
    pub normalized_results: Vec<Value>,
    pub listed_block_count: usize,
}

/// Heap-owned future for the fixture-heavy rich-body acceptance workflow.
pub type BodyScenarioFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BodyScenarioEvidence, String>> + 'a>>;

#[test]
fn body_scenario_futures_keep_only_a_heap_handle_inline() {
    assert!(std::mem::size_of::<BodyScenarioFuture<'static>>() <= 2 * std::mem::size_of::<usize>());
    assert!(
        std::mem::size_of::<BodyReadOnlyScenarioFuture<'static>>()
            <= 2 * std::mem::size_of::<usize>()
    );
}

/// Content-free evidence from one read-only body catalog check.
// This shared module is also compiled into direct-router unit tests, whose
// body slice intentionally exercises only the read-write scenario.
#[allow(dead_code)]
#[derive(Debug, PartialEq, Eq)]
pub struct BodyReadOnlyEvidence {
    pub body_tools: Vec<String>,
    pub mutation_error_categories: Vec<String>,
}

/// Heap-owned future for the read-only rich-body acceptance workflow.
pub type BodyReadOnlyScenarioFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BodyReadOnlyEvidence, String>> + 'a>>;

/// Payload-free production lifecycle counters optionally exposed by a direct
/// acceptance driver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyDriverMetrics {
    pub page_create_polls: usize,
    pub show_attempts: usize,
    pub foreground_close_attempts: usize,
    pub foreground_close_confirmed: usize,
    pub fallback_close_attempts: usize,
    pub fallback_close_confirmed: usize,
    pub write_polls: usize,
    pub show_limit_rejections: usize,
    pub non_show_limit_rejections: usize,
    pub close_limit_rejections: usize,
    pub mutation_limit_rejections: usize,
}

fn body_metrics_delta(
    before: BodyDriverMetrics,
    after: BodyDriverMetrics,
) -> Result<BodyDriverMetrics, String> {
    macro_rules! delta {
        ($field:ident) => {
            after.$field.checked_sub(before.$field).ok_or_else(|| {
                format!("body acceptance counter decreased: {}", stringify!($field))
            })?
        };
    }
    Ok(BodyDriverMetrics {
        page_create_polls: delta!(page_create_polls),
        show_attempts: delta!(show_attempts),
        foreground_close_attempts: delta!(foreground_close_attempts),
        foreground_close_confirmed: delta!(foreground_close_confirmed),
        fallback_close_attempts: delta!(fallback_close_attempts),
        fallback_close_confirmed: delta!(fallback_close_confirmed),
        write_polls: delta!(write_polls),
        show_limit_rejections: delta!(show_limit_rejections),
        non_show_limit_rejections: delta!(non_show_limit_rejections),
        close_limit_rejections: delta!(close_limit_rejections),
        mutation_limit_rejections: delta!(mutation_limit_rejections),
    })
}

fn expected_rich_metrics(page_create_polls: usize, blocks: usize) -> BodyDriverMetrics {
    let shows = blocks.saturating_add(1);
    BodyDriverMetrics {
        page_create_polls,
        show_attempts: shows,
        foreground_close_attempts: shows,
        foreground_close_confirmed: shows,
        write_polls: blocks,
        ..BodyDriverMetrics::default()
    }
}

fn expected_primitive_metrics() -> BodyDriverMetrics {
    BodyDriverMetrics {
        show_attempts: 2,
        foreground_close_attempts: 2,
        foreground_close_confirmed: 2,
        write_polls: 1,
        ..BodyDriverMetrics::default()
    }
}

fn expected_create_replay_metrics() -> BodyDriverMetrics {
    BodyDriverMetrics {
        show_attempts: 1,
        foreground_close_attempts: 1,
        foreground_close_confirmed: 1,
        ..BodyDriverMetrics::default()
    }
}

async fn call_body_tool_with_metrics(
    driver: &mut impl McpDriver,
    name: &'static str,
    arguments: Value,
    expected: BodyDriverMetrics,
    label: &str,
) -> Result<Value, String> {
    let before = driver.body_acceptance_metrics();
    let result = driver.call_tool(name, arguments).await?;
    if let (Some(before), Some(after)) = (before, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if observed != expected {
            return Err(format!("{label} production metrics diverged: {observed:?}"));
        }
    }
    Ok(result)
}

#[derive(Clone, Copy)]
enum BodyUpdateExpectation {
    Text,
    TextStyle,
    Checked(bool),
    TextColor(Option<&'static str>),
    CalloutIcon(bool),
    DividerStyle,
    BackgroundColor(Option<&'static str>),
    HorizontalAlign,
    VerticalAlign,
    EmbedSource,
    LinkAppearance,
}

fn update_target_changed_exactly(
    before: &BodyBlock,
    after: &BodyBlock,
    expectation: BodyUpdateExpectation,
) -> bool {
    let mut restored = after.clone();
    let exact_value = match expectation {
        BodyUpdateExpectation::Text => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.text == "matrix text"
                && after.marks.len() == 1
                && after.marks[0].range.start == 0
                && after.marks[0].range.end == 6
                && matches!(after.marks[0].kind, MarkKind::Bold);
            after.text.clone_from(&before.text);
            after.marks.clone_from(&before.marks);
            exact
        }
        BodyUpdateExpectation::TextStyle => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.style == TextStyle::Header2;
            after.style = before.style;
            exact
        }
        BodyUpdateExpectation::Checked(expected) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.checked == expected;
            after.checked = before.checked;
            exact
        }
        BodyUpdateExpectation::TextColor(expected) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.color.as_ref().map(|color| color.as_str()) == expected;
            after.color.clone_from(&before.color);
            exact
        }
        BodyUpdateExpectation::CalloutIcon(present) => {
            let (BlockContent::Text(before), BlockContent::Text(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = if present {
                matches!(after.icon, Some(CalloutIcon::Emoji(ref emoji)) if emoji == "📌")
            } else {
                after.icon.is_none()
            };
            after.icon.clone_from(&before.icon);
            exact
        }
        BodyUpdateExpectation::DividerStyle => {
            let (BlockContent::Divider(before), BlockContent::Divider(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = *after == DividerStyle::Line;
            *after = *before;
            exact
        }
        BodyUpdateExpectation::BackgroundColor(expected) => {
            let exact = restored
                .background_color
                .as_ref()
                .map(|color| color.as_str())
                == expected;
            restored
                .background_color
                .clone_from(&before.background_color);
            exact
        }
        BodyUpdateExpectation::HorizontalAlign => {
            let exact = restored.align == HorizontalAlign::Left;
            restored.align = before.align;
            exact
        }
        BodyUpdateExpectation::VerticalAlign => {
            let exact = restored.vertical_align == VerticalAlign::Top;
            restored.vertical_align = before.vertical_align;
            exact
        }
        BodyUpdateExpectation::EmbedSource => {
            let (BlockContent::Embed(before), BlockContent::Embed(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.processor == EmbedProcessor::Mermaid
                && after.text == "graph LR; Updated-->Verified";
            after.text.clone_from(&before.text);
            exact
        }
        BodyUpdateExpectation::LinkAppearance => {
            let (BlockContent::Link(before), BlockContent::Link(after)) =
                (&before.content, &mut restored.content)
            else {
                return false;
            };
            let exact = after.card_style == LinkCardStyle::Inline
                && after.icon_size == LinkIconSize::Medium
                && after.description == LinkDescriptionMode::Added
                && after.relations.is_empty();
            after.card_style = before.card_style;
            after.icon_size = before.icon_size;
            after.description = before.description;
            after.relations.clone_from(&before.relations);
            exact
        }
    };
    exact_value && restored == *before
}

fn exact_update_snapshot_transition(
    before: &BodySnapshot,
    after: &BodySnapshot,
    block_id: &str,
    expectation: BodyUpdateExpectation,
) -> bool {
    if before.space_id != after.space_id
        || before.object_id != after.object_id
        || before.root_id != after.root_id
        || before.len() != after.len()
    {
        return false;
    }
    let before_ids = before.iter().map(|block| &block.id).collect::<Vec<_>>();
    let after_ids = after.iter().map(|block| &block.id).collect::<Vec<_>>();
    if before_ids != after_ids {
        return false;
    }
    before.iter().all(|prior| {
        let Some(fresh) = after.get(&prior.id) else {
            return false;
        };
        if prior.id.as_str() == block_id {
            update_target_changed_exactly(prior, fresh, expectation)
        } else {
            fresh == prior
        }
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_body_update_arm(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    object_id: &str,
    snapshot_hash: &str,
    block_id: &str,
    change: Value,
    expectation: BodyUpdateExpectation,
    label: &str,
) -> Result<(Value, String), String> {
    let before = ctx
        .client
        .blocks()
        .body(&ctx.space_id, object_id)
        .fetch()
        .await
        .map_err(|_| format!("{label} independent before read failed"))?;
    let before_metrics = driver.body_acceptance_metrics();
    let result = driver
        .call_tool(
            "body_block_update",
            json!({
                "space":ctx.space_id,
                "object_id":object_id,
                "expected_snapshot_hash":snapshot_hash,
                "block_id":block_id,
                "change":change
            }),
        )
        .await?;
    if let (Some(before), Some(after)) = (before_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if observed != expected_primitive_metrics() {
            return Err(format!("{label} production metrics diverged: {observed:?}"));
        }
    }
    let after = ctx
        .client
        .blocks()
        .body(&ctx.space_id, object_id)
        .fetch()
        .await
        .map_err(|_| format!("{label} independent after read failed"))?;
    if !exact_update_snapshot_transition(&before, &after, block_id, expectation) {
        return Err(format!(
            "{label} changed more or less than its one exact typed field"
        ));
    }
    let next_hash = body_string(&result, "/snapshot_hash", "update snapshot hash")?.to_owned();
    Ok((normalize_body_result(&result), next_hash))
}

/// Proves that read-only mode advertises only body reads and rejects every
/// direct mutation name before decoding caller arguments.
#[allow(dead_code)]
pub fn run_body_read_only_scenario<'a>(
    driver: &'a mut impl McpDriver,
) -> BodyReadOnlyScenarioFuture<'a> {
    Box::pin(run_body_read_only_scenario_inner(driver))
}

async fn run_body_read_only_scenario_inner(
    driver: &mut impl McpDriver,
) -> Result<BodyReadOnlyEvidence, String> {
    const BODY_TOOLS: [&str; 6] = [
        "body_block_create",
        "body_block_delete",
        "body_block_list",
        "body_block_move",
        "body_block_update",
        "rich_page_create",
    ];
    const MUTATIONS: [&str; 5] = [
        "body_block_create",
        "body_block_update",
        "body_block_delete",
        "body_block_move",
        "rich_page_create",
    ];
    let body_tools = driver
        .list_tools()
        .await?
        .into_iter()
        .filter(|name| BODY_TOOLS.contains(&name.as_str()))
        .collect::<Vec<_>>();
    if body_tools != ["body_block_list"] {
        return Err("read-only body catalog was not the exact one-tool inventory".to_owned());
    }
    let mut mutation_error_categories = Vec::new();
    for name in MUTATIONS {
        let error = driver
            .call_tool_error(name, json!({"SECRET_UNPARSED_BODY_VALUE":true}))
            .await?;
        if error.contains("SECRET_UNPARSED_BODY_VALUE") {
            return Err("read-only mutation error exposed caller input".to_owned());
        }
        if !error.contains("validation") {
            return Err("read-only mutation did not fail before argument decoding".to_owned());
        }
        mutation_error_categories.push("validation".to_owned());
    }
    Ok(BodyReadOnlyEvidence {
        body_tools,
        mutation_error_categories,
    })
}

fn body_string<'a>(value: &'a Value, pointer: &str, field: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("body scenario omitted {field}"))
}

fn normalize_body_result(value: &Value) -> Value {
    fn normalized(value: &Value, field: Option<&str>) -> Value {
        match value {
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(key, value)| (key.clone(), normalized(value, Some(key))))
                    .collect(),
            ),
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| normalized(value, field))
                    .collect(),
            ),
            Value::String(_) => match field {
                Some(
                    "id" | "space_id" | "object_id" | "root_id" | "parent_id" | "block_id"
                    | "target_object_id",
                ) => {
                    json!("<id>")
                }
                Some("snapshot_hash" | "final_snapshot_hash") => json!("<snapshot-hash>"),
                Some("next_cursor") => json!("<cursor>"),
                _ => value.clone(),
            },
            _ => value.clone(),
        }
    }
    normalized(value, None)
}

fn rich_applied_ids(result: &Value, local_keys: &[&str]) -> Result<Vec<String>, String> {
    let applied = result["applied"]
        .as_array()
        .ok_or_else(|| "rich result omitted applied receipts".to_owned())?;
    if applied.len() != local_keys.len() {
        return Err("rich result returned the wrong applied-receipt count".to_owned());
    }
    applied
        .iter()
        .zip(local_keys)
        .enumerate()
        .map(|(index, (receipt, expected_key))| {
            if receipt["index"].as_u64() != Some(index as u64)
                || receipt["local_key"].as_str() != Some(expected_key)
            {
                return Err("rich result reordered applied receipts".to_owned());
            }
            body_string(receipt, "/block_id", "rich block ID").map(str::to_owned)
        })
        .collect()
}

fn rich_root_contains_exact_suffix(snapshot: &BodySnapshot, ids: &[String]) -> bool {
    let children = snapshot
        .children(&snapshot.root_id)
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>();
    let expected = ids.iter().map(String::as_str).collect::<Vec<_>>();
    children.ends_with(&expected)
}

fn verify_table_shape(
    snapshot: &BodySnapshot,
    table_id: &str,
    rows: usize,
    columns: usize,
    header_row: bool,
) -> bool {
    let Some(table) = snapshot.iter().find(|block| block.id.as_str() == table_id) else {
        return false;
    };
    if !matches!(table.content, BlockContent::Table) || table.children.len() != 2 {
        return false;
    }
    let Some(column_region) = snapshot.get(&table.children[0]) else {
        return false;
    };
    let Some(row_region) = snapshot.get(&table.children[1]) else {
        return false;
    };
    matches!(
        column_region.content,
        BlockContent::Layout(LayoutStyle::TableColumns)
    ) && column_region.children.len() == columns
        && column_region.children.iter().all(|id| {
            snapshot
                .get(id)
                .is_some_and(|block| matches!(block.content, BlockContent::TableColumn))
        })
        && matches!(
            row_region.content,
            BlockContent::Layout(LayoutStyle::TableRows)
        )
        && row_region.children.len() == rows
        && row_region.children.iter().enumerate().all(|(index, id)| {
            snapshot.get(id).is_some_and(|block| {
                matches!(
                    block.content,
                    BlockContent::TableRow { is_header }
                        if is_header == (header_row && index == 0)
                )
            })
        })
}

fn verify_primary_rich_snapshot(snapshot: &BodySnapshot, ids: &[String], target: &str) -> bool {
    let expected_text = [
        (TextStyle::Paragraph, "Paragraph", false),
        (TextStyle::Header1, "Heading 1", false),
        (TextStyle::Header2, "Heading 2", false),
        (TextStyle::Header3, "Heading 3", false),
        (TextStyle::Quote, "Quote", false),
        (TextStyle::Code, "let answer = 42;", false),
        (TextStyle::Bulleted, "Bulleted", false),
        (TextStyle::Numbered, "Numbered", false),
        (TextStyle::Checkbox, "Checked", true),
        (TextStyle::Toggle, "Toggle", false),
        (TextStyle::Callout, "Callout", false),
    ];
    if ids.len() != 16 || !rich_root_contains_exact_suffix(snapshot, ids) {
        return false;
    }
    for (index, (style, text, checked)) in expected_text.into_iter().enumerate() {
        let Some(block) = snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
        else {
            return false;
        };
        let BlockContent::Text(content) = &block.content else {
            return false;
        };
        if content.style != style || content.text != text || content.checked != checked {
            return false;
        }
        if index == 0
            && (content.color.as_ref().map(|color| color.as_str()) != Some("blue")
                || block.align != HorizontalAlign::Center
                || block.vertical_align != VerticalAlign::Middle
                || block.background_color.as_ref().map(|color| color.as_str()) != Some("grey"))
        {
            return false;
        }
        if index == 10
            && !matches!(content.icon, Some(CalloutIcon::Emoji(ref emoji)) if emoji == "💡")
        {
            return false;
        }
    }
    let by_id = |index: usize| {
        snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
            .map(|block| &block.content)
    };
    matches!(by_id(11), Some(BlockContent::Divider(DividerStyle::Dots)))
        && matches!(by_id(12), Some(BlockContent::Relation(relation)) if relation.key == "tag")
        && snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[12])
            .is_some_and(|block| block.align == HorizontalAlign::Justify)
        && matches!(
            by_id(13),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Card
                    && link.icon_size == LinkIconSize::Small
                    && link.description == LinkDescriptionMode::Content
                    && link.relations == ["tag"]
        )
        && matches!(
            by_id(14),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Mermaid
                    && embed.text == "graph TD; A-->B"
        )
        && snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[13])
            .is_some_and(|block| {
                block.align == HorizontalAlign::Right
                    && block.vertical_align == VerticalAlign::Bottom
                    && block.background_color.as_ref().map(|color| color.as_str()) == Some("yellow")
            })
        && verify_table_shape(snapshot, &ids[15], 2, 3, true)
}

fn verify_supplemental_rich_snapshot(
    snapshot: &BodySnapshot,
    ids: &[String],
    target: &str,
) -> bool {
    if ids.len() != 7 || !rich_root_contains_exact_suffix(snapshot, ids) {
        return false;
    }
    let by_id = |index: usize| {
        snapshot
            .iter()
            .find(|block| block.id.as_str() == ids[index])
            .map(|block| &block.content)
    };
    matches!(by_id(0), Some(BlockContent::Divider(DividerStyle::Line)))
        && matches!(
            by_id(1),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Latex && embed.text == "x^2 + y^2"
        )
        && matches!(
            by_id(2),
            Some(BlockContent::Embed(embed))
                if embed.processor == EmbedProcessor::Youtube
                    && embed.text == "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        )
        && matches!(by_id(3), Some(BlockContent::TableOfContents))
        && matches!(
            by_id(4),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Text
                    && link.icon_size == LinkIconSize::None
                    && link.description == LinkDescriptionMode::None
                    && link.relations.is_empty()
        )
        && matches!(
            by_id(5),
            Some(BlockContent::Link(link))
                if link.target_object_id == target
                    && link.card_style == LinkCardStyle::Inline
                    && link.icon_size == LinkIconSize::Medium
                    && link.description == LinkDescriptionMode::Added
                    && link.relations == ["tag"]
        )
        && verify_table_shape(snapshot, &ids[6], 1, 1, false)
}

/// Runs ordinary body behavior through any direct or protocol driver while an
/// independent `anytype-api` client owns and verifies every live fixture.
///
/// The erased, heap-owned return type keeps the complete workflow state out of
/// the caller's async frame so the acceptance suite fits the default test
/// thread stack.
pub fn run_body_scenario<'a>(
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
    transport: &'a str,
) -> BodyScenarioFuture<'a> {
    Box::pin(run_body_scenario_inner(driver, ctx, transport))
}

async fn run_body_scenario_inner(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    transport: &str,
) -> Result<BodyScenarioEvidence, String> {
    let mut normalized_results = Vec::new();
    let suffix = unique_suffix();
    let page = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(format!(
            "Body {transport} {suffix} {BODY_DIAGNOSTIC_SECRET}"
        ))
        .create()
        .await
        .map_err(|_| {
            body_fixture_marker("page_create_failed");
            "body fixture page create failed".to_owned()
        })?;
    ctx.register_object(&page.id);
    body_fixture_marker("page_created");
    let initial = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| {
            body_fixture_marker("initial_fetch_failed");
            "body initial fixture read failed".to_owned()
        })?;
    let initial_blocks = initial.iter().cloned().collect::<Vec<_>>();
    let append_count = BODY_PAGINATION_ITEM_COUNT
        .checked_sub(initial_blocks.len())
        .filter(|count| *count > 0)
        .ok_or_else(|| {
            body_fixture_marker("initial_count_invalid");
            "body initial fixture already contains twenty or more blocks".to_owned()
        })?;
    let expected_suffix = body_pagination_suffix_spec(append_count);
    let fixture_operations = body_pagination_append_operations(append_count).inspect_err(|_| {
        body_fixture_marker("append_plan_failed");
    })?;
    let operation_count = fixture_operations.len();
    body_fixture_plan_diagnostic(initial_blocks.len(), append_count, operation_count);
    let fixture_outcome = initial
        .edit(&ctx.client)
        .apply_all(fixture_operations)
        .await
        .map_err(|_| {
            body_fixture_marker("apply_all_error");
            "body deterministic fixture batch failed".to_owned()
        })?;
    let outcome_category = if fixture_outcome.failed.is_some() {
        "failed"
    } else if fixture_outcome.not_attempted.is_empty()
        && fixture_outcome.applied.len() == operation_count
    {
        "complete"
    } else {
        "incomplete"
    };
    body_fixture_outcome_diagnostic(
        outcome_category,
        fixture_outcome.applied.len(),
        usize::from(fixture_outcome.failed.is_some()),
        fixture_outcome.not_attempted.len(),
    );
    if fixture_outcome.failed.is_some()
        || !fixture_outcome.not_attempted.is_empty()
        || fixture_outcome.applied.len() != operation_count
    {
        body_fixture_marker("apply_all_incomplete");
        return Err("body deterministic fixture batch did not complete".to_owned());
    }
    let mut created_ids = Vec::with_capacity(operation_count);
    for (index, (receipt, (expected_style, expected_text))) in fixture_outcome
        .applied
        .iter()
        .zip(&expected_suffix)
        .enumerate()
    {
        let Some(affected) = receipt.affected.first() else {
            body_fixture_receipt_diagnostic(index, 0, false, false, false, false);
            return Err("body fixture append receipt omitted the created block".to_owned());
        };
        let address_ok = affected.space_id == ctx.space_id && affected.object_id == page.id;
        let receipt_block = receipt.snapshot.get(&affected.block_id);
        let block_present = receipt_block.is_some();
        let content_ok = receipt_block.is_some_and(|block| {
            matches!(
                &block.content,
                BlockContent::Text(content)
                    if content.style == *expected_style && content.text == *expected_text
            )
        });
        let root_last_ok = receipt.snapshot.root().children.last() == Some(&affected.block_id);
        body_fixture_receipt_diagnostic(
            index,
            receipt.affected.len(),
            address_ok,
            block_present,
            content_ok,
            root_last_ok,
        );
        let receipt_is_exact = receipt.affected.len() == 1
            && address_ok
            && block_present
            && content_ok
            && root_last_ok;
        if !receipt_is_exact {
            body_fixture_marker("receipt_invalid");
            return Err("body fixture append receipt did not prove the exact suffix".to_owned());
        }
        created_ids.push(affected.block_id.as_str().to_owned());
    }
    if created_ids.len() != expected_suffix.len() {
        body_fixture_marker("receipt_coverage_invalid");
        return Err("body fixture append receipts did not cover the exact suffix".to_owned());
    }
    body_fixture_count("receipts_valid", created_ids.len());
    let heading_id = created_ids.first().cloned().ok_or_else(|| {
        body_fixture_marker("heading_receipt_missing");
        "body fixture omitted its created heading receipt".to_owned()
    })?;
    let fixture = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| {
            body_fixture_marker("refetch_failed");
            "body deterministic fixture read failed".to_owned()
        })?;
    body_fixture_shape_diagnostic(&fixture, &initial_blocks, &created_ids, &expected_suffix);
    if !is_exact_body_pagination_fixture(&fixture, &initial_blocks, &created_ids, &expected_suffix)
    {
        body_fixture_marker("shape_invalid");
        return Err(
            "body deterministic fixture did not contain the exact ordered blocks".to_owned(),
        );
    }
    body_fixture_marker("complete");

    body_scenario_marker("catalog_start");
    let tools = driver.list_tools().await?;
    body_scenario_count("catalog_received", tools.len());
    for name in [
        "body_block_list",
        "body_block_create",
        "body_block_update",
        "body_block_delete",
        "body_block_move",
        "rich_page_create",
    ] {
        if !tools.iter().any(|candidate| candidate == name) {
            body_scenario_marker("catalog_missing_required_tool");
            return Err(format!("{transport} catalog omitted {name}"));
        }
    }
    body_scenario_marker("catalog_complete");

    body_scenario_marker("pagination_first_start");
    let first = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    body_scenario_marker("pagination_first_received");
    normalized_results.push(normalize_body_result(&first));
    let root_id = body_string(&first, "/root_id", "root ID")?.to_owned();
    body_string(&first, "/snapshot_hash", "snapshot hash")?;
    let cursor = body_string(&first, "/next_cursor", "continuation cursor")?.to_owned();
    let mut listed_block_ids = first["items"]
        .as_array()
        .ok_or_else(|| "body first page omitted items".to_owned())?
        .iter()
        .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    body_scenario_count("pagination_first_items", listed_block_ids.len());
    if !body_scenario_check("pagination_first_exact", listed_block_ids.len() == 8) {
        return Err("body first page did not contain the exact limit of eight".to_owned());
    }
    body_scenario_marker("pagination_second_start");
    let second = driver
        .call_tool(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":cursor
            }),
        )
        .await?;
    body_scenario_marker("pagination_second_received");
    normalized_results.push(normalize_body_result(&second));
    if !body_scenario_check(
        "pagination_second_hash_matches",
        second["snapshot_hash"] == first["snapshot_hash"],
    ) {
        return Err("body pages mixed snapshot hashes".to_owned());
    }
    let second_cursor =
        body_string(&second, "/next_cursor", "second continuation cursor")?.to_owned();
    listed_block_ids.extend(
        second["items"]
            .as_array()
            .ok_or_else(|| "body second page omitted items".to_owned())?
            .iter()
            .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?,
    );
    body_scenario_count("pagination_second_cumulative_items", listed_block_ids.len());
    if !body_scenario_check("pagination_second_exact", listed_block_ids.len() == 16) {
        return Err("body second page did not consume the next eight blocks".to_owned());
    }
    body_scenario_marker("pagination_third_start");
    let third = driver
        .call_tool(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":second_cursor
            }),
        )
        .await?;
    body_scenario_marker("pagination_third_received");
    normalized_results.push(normalize_body_result(&third));
    if !body_scenario_check(
        "pagination_third_hash_matches",
        third["snapshot_hash"] == first["snapshot_hash"],
    ) {
        return Err("body pages mixed snapshot hashes".to_owned());
    }
    let third_ids = third["items"]
        .as_array()
        .ok_or_else(|| "body third page omitted items".to_owned())?
        .iter()
        .map(|item| body_string(item, "/id", "listed block ID").map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    body_scenario_count("pagination_third_items", third_ids.len());
    if !body_scenario_check(
        "pagination_third_exact",
        third_ids.len() == BODY_PAGINATION_ITEM_COUNT - 16,
    ) {
        return Err("body third page did not contain the final four blocks".to_owned());
    }
    listed_block_ids.extend(third_ids);
    body_scenario_count("pagination_total_items", listed_block_ids.len());
    if !body_scenario_check(
        "pagination_total_exact",
        listed_block_ids.len() == BODY_PAGINATION_ITEM_COUNT,
    ) {
        return Err("body pagination did not contain exactly twenty blocks".to_owned());
    }
    if !body_scenario_check("pagination_terminated", third.get("next_cursor").is_none()) {
        return Err("body three-page fixture unexpectedly returned a fourth cursor".to_owned());
    }
    body_scenario_marker("pagination_independent_start");
    let independent = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent body read failed".to_owned())?;
    let independent_ids = independent
        .iter()
        .map(|block| block.id.as_str().to_owned())
        .collect::<Vec<_>>();
    body_scenario_count("pagination_independent_items", independent_ids.len());
    if !body_scenario_check(
        "pagination_independent_equal",
        listed_block_ids == independent_ids,
    ) {
        return Err("body pages did not preserve exact DFS order".to_owned());
    }
    body_scenario_marker("pagination_complete");

    body_scenario_marker("stale_cursor_start");
    let stale_first = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    let stale_cursor = body_string(&stale_first, "/next_cursor", "stale cursor")?.to_owned();
    independent
        .edit(&ctx.client)
        .create(
            NewBlock::paragraph("independent revision")
                .map_err(|_| "independent revision constructor failed".to_owned())?,
            &independent.root_id,
            InsertPosition::LastChild,
        )
        .await
        .map_err(|_| "independent revision write failed".to_owned())?;
    body_scenario_marker("stale_cursor_revision_written");
    let stale_error = driver
        .call_tool_error(
            "body_block_list",
            json!({
                "space":ctx.space_id,"object_id":page.id,"limit":8,"cursor":stale_cursor
            }),
        )
        .await?;
    if !body_scenario_check("stale_cursor_conflict", stale_error.contains("conflict")) {
        return Err("body continuation did not reject revision drift".to_owned());
    }
    normalized_results.push(json!({"error_category":"conflict"}));
    body_scenario_marker("stale_cursor_complete");

    body_scenario_marker("primitive_start");
    let fresh = driver
        .call_tool(
            "body_block_list",
            json!({"space":ctx.space_id,"object_id":page.id,"limit":8}),
        )
        .await?;
    body_scenario_marker("primitive_fresh_list_received");
    normalized_results.push(normalize_body_result(&fresh));
    let mut snapshot_hash = body_string(&fresh, "/snapshot_hash", "fresh hash")?.to_owned();
    let create_input = json!({
        "space":ctx.space_id,"object_id":page.id,
        "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
        "position":"last_child",
        "block":{"kind":"text","style":"paragraph","text":"created block","marks":[]},
        "idempotency_key":format!("body-{transport}-{suffix}")
    });
    let created = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        create_input.clone(),
        expected_primitive_metrics(),
        "primitive create",
    )
    .await?;
    body_scenario_marker("primitive_create_complete");
    normalized_results.push(normalize_body_result(&created));
    let created_block_id = body_string(&created, "/block/id", "created block ID")?.to_owned();
    let replay = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        create_input,
        expected_create_replay_metrics(),
        "primitive create replay",
    )
    .await?;
    body_scenario_marker("primitive_replay_received");
    normalized_results.push(normalize_body_result(&replay));
    let replay_id_matches = replay["block"]["id"] == created["block"]["id"];
    let replay_key_reused = replay["idempotency"]["key_reused"] == true;
    let replay_id_ok = body_scenario_check("primitive_replay_id_matches", replay_id_matches);
    let replay_key_ok = body_scenario_check("primitive_replay_key_reused", replay_key_reused);
    if !replay_id_ok || !replay_key_ok {
        return Err("body create replay did not retain one assigned ID".to_owned());
    }
    body_scenario_marker("primitive_replay_complete");
    snapshot_hash = body_string(&replay, "/snapshot_hash", "replay hash")?.to_owned();

    let child = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":heading_id,
            "position":"last_child",
            "block":{"kind":"text","style":"paragraph","text":"targeted child","marks":[]},
            "idempotency_key":format!("body-child-{transport}-{suffix}")
        }),
        expected_primitive_metrics(),
        "heading append",
    )
    .await?;
    body_scenario_marker("primitive_heading_append_received");
    normalized_results.push(normalize_body_result(&child));
    let child_id = body_string(&child, "/block/id", "child ID")?.to_owned();
    snapshot_hash = body_string(&child, "/snapshot_hash", "child hash")?.to_owned();
    let heading_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent heading append read failed".to_owned())?;
    let appended_under_heading = heading_snapshot
        .iter()
        .find(|block| block.id.as_str() == heading_id)
        .is_some_and(|heading| {
            heading
                .children
                .iter()
                .any(|child| child.as_str() == child_id)
        });
    if !body_scenario_check("primitive_heading_append_verified", appended_under_heading) {
        return Err("targeted append did not land beneath the existing heading".to_owned());
    }
    body_scenario_marker("primitive_heading_append_complete");
    let moved = call_body_tool_with_metrics(
        driver,
        "body_block_move",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":child_id,
            "target_block_id":created_block_id,"position":"after"
        }),
        expected_primitive_metrics(),
        "primitive move",
    )
    .await?;
    body_scenario_marker("primitive_move_complete");
    normalized_results.push(normalize_body_result(&moved));
    snapshot_hash = body_string(&moved, "/snapshot_hash", "move hash")?.to_owned();
    let deleted = call_body_tool_with_metrics(
        driver,
        "body_block_delete",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":child_id,
            "expected_subtree_blocks":1,"confirm_delete":"delete_subtree"
        }),
        expected_primitive_metrics(),
        "primitive delete",
    )
    .await?;
    body_scenario_marker("primitive_delete_complete");
    normalized_results.push(normalize_body_result(&deleted));
    snapshot_hash = body_string(&deleted, "/snapshot_hash", "delete hash")?.to_owned();
    body_scenario_marker("primitive_complete");

    body_scenario_marker("relation_start");
    let relation = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
            "position":"last_child","block":{"kind":"relation","key":"tag"},
            "idempotency_key":format!("body-relation-{transport}-{suffix}")
        }),
        expected_primitive_metrics(),
        "relation create",
    )
    .await?;
    body_scenario_marker("relation_create_complete");
    normalized_results.push(normalize_body_result(&relation));
    let relation_id = body_string(&relation, "/block/id", "relation block ID")?.to_owned();
    snapshot_hash = body_string(&relation, "/snapshot_hash", "relation hash")?.to_owned();
    let relation_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent relation detection read failed".to_owned())?;
    let relation_detected = relation_snapshot.iter().any(|block| {
        block.id.as_str() == relation_id
            && matches!(
                block.content,
                BlockContent::Relation(ref relation) if relation.key == "tag"
            )
    });
    if !body_scenario_check("relation_create_verified", relation_detected) {
        return Err("created relation block was not independently detected".to_owned());
    }
    let relation_deleted = call_body_tool_with_metrics(
        driver,
        "body_block_delete",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":relation_id,
            "expected_subtree_blocks":1,"confirm_delete":"delete_subtree"
        }),
        expected_primitive_metrics(),
        "relation delete",
    )
    .await?;
    body_scenario_marker("relation_delete_complete");
    normalized_results.push(normalize_body_result(&relation_deleted));
    snapshot_hash =
        body_string(&relation_deleted, "/snapshot_hash", "relation delete hash")?.to_owned();
    let recreated_relation = call_body_tool_with_metrics(
        driver,
        "body_block_create",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"target_block_id":root_id,
            "position":"last_child","block":{"kind":"relation","key":"tag"},
            "idempotency_key":format!("body-relation-recreate-{transport}-{suffix}")
        }),
        expected_primitive_metrics(),
        "relation recreate",
    )
    .await?;
    body_scenario_marker("relation_recreate_complete");
    normalized_results.push(normalize_body_result(&recreated_relation));
    let recreated_relation_id =
        body_string(&recreated_relation, "/block/id", "recreated relation ID")?.to_owned();
    snapshot_hash = body_string(
        &recreated_relation,
        "/snapshot_hash",
        "recreated relation hash",
    )?
    .to_owned();
    let relation_moved = call_body_tool_with_metrics(
        driver,
        "body_block_move",
        json!({
            "space":ctx.space_id,"object_id":page.id,
            "expected_snapshot_hash":snapshot_hash,"block_id":recreated_relation_id,
            "target_block_id":heading_id,"position":"before"
        }),
        expected_primitive_metrics(),
        "relation move",
    )
    .await?;
    body_scenario_marker("relation_move_received");
    normalized_results.push(normalize_body_result(&relation_moved));
    let moved_relation_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &page.id)
        .fetch()
        .await
        .map_err(|_| "independent moved relation read failed".to_owned())?;
    let root_children = moved_relation_snapshot.children(&moved_relation_snapshot.root_id);
    let adjacent = root_children
        .windows(2)
        .any(|pair| pair[0].as_str() == recreated_relation_id && pair[1].as_str() == heading_id);
    let recreated_relation_detected = moved_relation_snapshot.iter().any(|block| {
        block.id.as_str() == recreated_relation_id
            && matches!(
                    block.content,
                    BlockContent::Relation(ref relation) if relation.key == "tag"
            )
    });
    let relation_adjacent_ok = body_scenario_check("relation_move_adjacent", adjacent);
    let relation_content_ok = body_scenario_check(
        "relation_move_content_verified",
        recreated_relation_detected,
    );
    if !relation_adjacent_ok || !relation_content_ok {
        return Err("relation recreation/move was not independently verified".to_owned());
    }
    body_scenario_marker("relation_complete");

    body_scenario_marker("rich_primary_start");
    let rich_input = json!({
        "space":ctx.space_id,"name":format!("Rich {transport} {suffix}"),
        "idempotency_key":format!("rich-{transport}-{suffix}"),
        "blocks":[
            {"local_key":"paragraph","block":{"kind":"text","style":"paragraph","text":"Paragraph","marks":[],"text_color":"blue","horizontal_align":"center","vertical_align":"middle","background_color":"grey"}},
            {"local_key":"heading1","block":{"kind":"text","style":"heading_1","text":"Heading 1","marks":[]}},
            {"local_key":"heading2","block":{"kind":"text","style":"heading_2","text":"Heading 2","marks":[]}},
            {"local_key":"heading3","block":{"kind":"text","style":"heading_3","text":"Heading 3","marks":[]}},
            {"local_key":"quote","block":{"kind":"text","style":"quote","text":"Quote","marks":[]}},
            {"local_key":"code","block":{"kind":"text","style":"code","text":"let answer = 42;","marks":[]}},
            {"local_key":"bulleted","block":{"kind":"text","style":"bulleted","text":"Bulleted","marks":[]}},
            {"local_key":"numbered","block":{"kind":"text","style":"numbered","text":"Numbered","marks":[]}},
            {"local_key":"checkbox","block":{"kind":"text","style":"checkbox","text":"Checked","checked":true,"marks":[]}},
            {"local_key":"toggle","block":{"kind":"text","style":"toggle","text":"Toggle","marks":[]}},
            {"local_key":"callout","block":{"kind":"text","style":"callout","text":"Callout","icon":{"kind":"emoji","emoji":"💡"},"marks":[]}},
            {"local_key":"divider","block":{"kind":"divider","style":"dots"}},
            {"local_key":"relation","block":{"kind":"relation","key":"tag","horizontal_align":"justify"}},
            {"local_key":"link","block":{"kind":"link","target_object_id":page.id,"card_style":"card","icon_size":"small","description":"content","relations":["tag"],"horizontal_align":"right","vertical_align":"bottom","background_color":"yellow"}},
            {"local_key":"mermaid","block":{"kind":"embed","processor":"mermaid","source":"graph TD; A-->B"}},
            {"local_key":"table","block":{"kind":"table","rows":2,"columns":3,"header_row":true}}
        ]
    });
    let primary_keys = [
        "paragraph",
        "heading1",
        "heading2",
        "heading3",
        "quote",
        "code",
        "bulleted",
        "numbered",
        "checkbox",
        "toggle",
        "callout",
        "divider",
        "relation",
        "link",
        "mermaid",
        "table",
    ];
    let before_rich_metrics = driver.body_acceptance_metrics();
    let rich = driver
        .call_tool("rich_page_create", rich_input.clone())
        .await?;
    body_scenario_marker("rich_primary_received");
    if let (Some(before), Some(after)) = (before_rich_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if !body_scenario_check(
            "rich_primary_metrics_match",
            observed == expected_rich_metrics(1, primary_keys.len()),
        ) {
            return Err(format!(
                "primary rich production metrics diverged: {observed:?}"
            ));
        }
    }
    let rich_page_id = body_string(&rich, "/object_id", "rich page ID")?.to_owned();
    ctx.register_object(&rich_page_id);
    normalized_results.push(normalize_body_result(&rich));
    if !body_scenario_check("rich_primary_status_complete", rich["status"] == "complete") {
        return Err("rich page workflow did not complete".to_owned());
    }
    let primary_ids = rich_applied_ids(&rich, &primary_keys)?;
    body_scenario_count("rich_primary_applied_ids", primary_ids.len());
    let rich_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &rich_page_id)
        .fetch()
        .await
        .map_err(|_| "independent rich body read failed".to_owned())?;
    if !body_scenario_check(
        "rich_primary_snapshot_verified",
        verify_primary_rich_snapshot(&rich_snapshot, &primary_ids, &page.id),
    ) {
        return Err("independent primary rich ObjectShow verification failed".to_owned());
    }
    body_scenario_marker("rich_primary_complete");
    body_scenario_marker("rich_replay_start");
    let before_replay_metrics = driver.body_acceptance_metrics();
    let rich_replay = driver.call_tool("rich_page_create", rich_input).await?;
    body_scenario_marker("rich_replay_received");
    if let (Some(before), Some(after)) = (before_replay_metrics, driver.body_acceptance_metrics()) {
        let observed = body_metrics_delta(before, after)?;
        if !body_scenario_check(
            "rich_replay_metrics_match",
            observed == expected_rich_metrics(0, 0),
        ) {
            return Err(format!(
                "rich replay production metrics diverged: {observed:?}"
            ));
        }
    }
    let rich_replay_id_matches = rich_replay["object_id"] == rich["object_id"];
    let rich_replay_key_reused = rich_replay["idempotency"]["key_reused"] == true;
    let rich_replay_id_ok = body_scenario_check("rich_replay_id_matches", rich_replay_id_matches);
    let rich_replay_key_ok = body_scenario_check("rich_replay_key_reused", rich_replay_key_reused);
    if !rich_replay_id_ok || !rich_replay_key_ok {
        return Err("rich page replay did not retain one exact page".to_owned());
    }
    normalized_results.push(normalize_body_result(&rich_replay));
    body_scenario_marker("rich_replay_complete");

    let mut rich_snapshot_hash = body_string(
        &rich_replay,
        "/final_snapshot_hash",
        "rich replay snapshot hash",
    )?
    .to_owned();
    let update_arms = [
        (
            "set_text",
            primary_ids[0].as_str(),
            json!({
                "kind":"set_text","text":"matrix text",
                "marks":[{"kind":"bold","start":0,"end":6}]
            }),
            BodyUpdateExpectation::Text,
        ),
        (
            "set_text_style",
            primary_ids[1].as_str(),
            json!({"kind":"set_text_style","style":"heading_2"}),
            BodyUpdateExpectation::TextStyle,
        ),
        (
            "set_checked",
            primary_ids[8].as_str(),
            json!({"kind":"set_checked","checked":false}),
            BodyUpdateExpectation::Checked(false),
        ),
        (
            "set_text_color",
            primary_ids[0].as_str(),
            json!({"kind":"set_text_color","color":"red"}),
            BodyUpdateExpectation::TextColor(Some("red")),
        ),
        (
            "clear_text_color",
            primary_ids[0].as_str(),
            json!({"kind":"clear_text_color"}),
            BodyUpdateExpectation::TextColor(None),
        ),
        (
            "set_callout_icon",
            primary_ids[10].as_str(),
            json!({"kind":"set_callout_icon","icon":{"kind":"emoji","emoji":"📌"}}),
            BodyUpdateExpectation::CalloutIcon(true),
        ),
        (
            "clear_callout_icon",
            primary_ids[10].as_str(),
            json!({"kind":"clear_callout_icon"}),
            BodyUpdateExpectation::CalloutIcon(false),
        ),
        (
            "set_divider_style",
            primary_ids[11].as_str(),
            json!({"kind":"set_divider_style","style":"line"}),
            BodyUpdateExpectation::DividerStyle,
        ),
        (
            "set_background_color",
            primary_ids[0].as_str(),
            json!({"kind":"set_background_color","color":"green"}),
            BodyUpdateExpectation::BackgroundColor(Some("green")),
        ),
        (
            "clear_background_color",
            primary_ids[0].as_str(),
            json!({"kind":"clear_background_color"}),
            BodyUpdateExpectation::BackgroundColor(None),
        ),
        (
            "set_horizontal_align",
            primary_ids[0].as_str(),
            json!({"kind":"set_horizontal_align","align":"left"}),
            BodyUpdateExpectation::HorizontalAlign,
        ),
        (
            "set_vertical_align",
            primary_ids[0].as_str(),
            json!({"kind":"set_vertical_align","align":"top"}),
            BodyUpdateExpectation::VerticalAlign,
        ),
        (
            "set_embed_source",
            primary_ids[14].as_str(),
            json!({"kind":"set_embed_source","source":"graph LR; Updated-->Verified"}),
            BodyUpdateExpectation::EmbedSource,
        ),
        (
            "set_link_appearance",
            primary_ids[13].as_str(),
            json!({
                "kind":"set_link_appearance","card_style":"inline","icon_size":"medium",
                "description":"added","relations":[]
            }),
            BodyUpdateExpectation::LinkAppearance,
        ),
    ];
    body_scenario_count("update_cases", update_arms.len());
    if !body_scenario_check("update_case_count_exact", update_arms.len() == 14) {
        return Err("body update matrix did not own exactly fourteen arms".to_owned());
    }
    for (index, (label, block_id, change, expectation)) in update_arms.into_iter().enumerate() {
        body_scenario_update(index, "start");
        let (evidence, next_hash) = run_body_update_arm(
            driver,
            ctx,
            &rich_page_id,
            &rich_snapshot_hash,
            block_id,
            change,
            expectation,
            label,
        )
        .await
        .inspect_err(|_| {
            body_scenario_update(index, "failed");
        })?;
        normalized_results.push(evidence);
        rich_snapshot_hash = next_hash;
        body_scenario_update(index, "complete");
    }
    body_scenario_marker("updates_complete");

    body_scenario_marker("rich_supplemental_start");
    let supplemental_input = json!({
        "space":ctx.space_id,"name":format!("Rich variants {transport} {suffix}"),
        "idempotency_key":format!("rich-variants-{transport}-{suffix}"),
        "blocks":[
            {"local_key":"line","block":{"kind":"divider","style":"line"}},
            {"local_key":"latex","block":{"kind":"embed","processor":"latex","source":"x^2 + y^2"}},
            {"local_key":"youtube","block":{"kind":"embed","processor":"youtube","source":"dQw4w9WgXcQ"}},
            {"local_key":"toc","block":{"kind":"table_of_contents"}},
            {"local_key":"link_text","block":{"kind":"link","target_object_id":page.id,"card_style":"text","icon_size":"none","description":"none","relations":[]}},
            {"local_key":"link_inline","block":{"kind":"link","target_object_id":page.id,"card_style":"inline","icon_size":"medium","description":"added","relations":["tag"]}},
            {"local_key":"table_plain","block":{"kind":"table","rows":1,"columns":1,"header_row":false}}
        ]
    });
    let supplemental_keys = [
        "line",
        "latex",
        "youtube",
        "toc",
        "link_text",
        "link_inline",
        "table_plain",
    ];
    let before_supplemental_metrics = driver.body_acceptance_metrics();
    let supplemental = driver
        .call_tool("rich_page_create", supplemental_input)
        .await?;
    body_scenario_marker("rich_supplemental_received");
    if let (Some(before), Some(after)) = (
        before_supplemental_metrics,
        driver.body_acceptance_metrics(),
    ) {
        let observed = body_metrics_delta(before, after)?;
        if !body_scenario_check(
            "rich_supplemental_metrics_match",
            observed == expected_rich_metrics(1, supplemental_keys.len()),
        ) {
            return Err(format!(
                "supplemental rich production metrics diverged: {observed:?}"
            ));
        }
    }
    let supplemental_page_id =
        body_string(&supplemental, "/object_id", "supplemental rich page ID")?.to_owned();
    ctx.register_object(&supplemental_page_id);
    normalized_results.push(normalize_body_result(&supplemental));
    if !body_scenario_check(
        "rich_supplemental_status_complete",
        supplemental["status"] == "complete",
    ) {
        return Err("supplemental rich workflow did not complete".to_owned());
    }
    let supplemental_ids = rich_applied_ids(&supplemental, &supplemental_keys)?;
    body_scenario_count("rich_supplemental_applied_ids", supplemental_ids.len());
    let supplemental_snapshot = ctx
        .client
        .blocks()
        .body(&ctx.space_id, &supplemental_page_id)
        .fetch()
        .await
        .map_err(|_| "independent supplemental rich body read failed".to_owned())?;
    if !body_scenario_check(
        "rich_supplemental_snapshot_verified",
        verify_supplemental_rich_snapshot(&supplemental_snapshot, &supplemental_ids, &page.id),
    ) {
        return Err("independent supplemental rich ObjectShow verification failed".to_owned());
    }
    body_scenario_marker("rich_supplemental_complete");
    body_scenario_count("final_normalized_results", normalized_results.len());
    body_scenario_count("final_listed_blocks", listed_block_ids.len());
    body_scenario_marker("final_evidence_complete");

    Ok(BodyScenarioEvidence {
        normalized_results,
        listed_block_count: listed_block_ids.len(),
    })
}
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Test-only transport seam used by both direct-router and stdio drivers.
pub trait McpDriver {
    fn body_acceptance_metrics(&self) -> Option<BodyDriverMetrics> {
        None
    }

    fn call_tool<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn call_tool_error<'a>(
        &'a mut self,
        name: &'static str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>>;

    fn list_tools<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>>;

    fn list_resources<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn list_resource_templates<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;

    fn read_resource<'a>(
        &'a mut self,
        uri: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>>;
}

/// Runs a fixture-heavy live scenario on an isolated test runtime.
pub fn run_live_scenario_on_large_stack<F, Fut>(thread_name: &str, scenario: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + 'static,
{
    std::thread::Builder::new()
        .name(thread_name.to_owned())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build fixture-heavy live scenario runtime")
                .block_on(scenario());
        })
        .expect("spawn fixture-heavy live scenario thread")
        .join()
        .expect("fixture-heavy live scenario thread");
}

/// Content-free result of the representative-layout scenario.
#[derive(Debug, PartialEq, Eq)]
pub struct LayoutScenarioEvidence {
    pub collection_id: String,
    pub grid_view_id: String,
    pub kanban_view_id: String,
    pub moved_item_id: String,
    pub member_ids: Vec<String>,
}

struct PageWalk {
    items: Vec<Value>,
    pages: usize,
}

async fn collect_id_pages(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    id_pointer: &'static str,
    max_pages: usize,
) -> Result<PageWalk, String> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut items = Vec::new();
    for page_number in 0..max_pages {
        let mut input = base
            .as_object()
            .cloned()
            .ok_or_else(|| format!("{tool} page input must be an object"))?;
        input.insert("limit".to_owned(), json!(1));
        if let Some(cursor) = cursor.take() {
            input.insert("cursor".to_owned(), Value::String(cursor));
        }
        let page = driver.call_tool(tool, Value::Object(input)).await?;
        let page_items = page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} items must be an array"))?;
        require(page_items.len() <= 1, &format!("{tool} honors limit one"))?;
        for item in page_items {
            let id = item
                .pointer(id_pointer)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{tool} item omitted its identity"))?;
            require(
                seen_ids.insert(id.to_owned()),
                &format!("{tool} item progress"),
            )?;
            items.push(item.clone());
        }
        let Some(next) = page.get("next_cursor").and_then(Value::as_str) else {
            return Ok(PageWalk {
                items,
                pages: page_number + 1,
            });
        };
        require(
            seen_cursors.insert(next.to_owned()),
            &format!("{tool} cursor progress"),
        )?;
        cursor = Some(next.to_owned());
    }
    Err(format!("{tool} did not terminate within {max_pages} pages"))
}

async fn canonical_members(ctx: &TestContext, collection_id: &str) -> Result<Vec<String>, String> {
    let page = ctx
        .client
        .collection_membership_page(&ctx.space_id, collection_id, 61, None)
        .await
        .map_err(|_| "read independent canonical membership".to_owned())?;
    require(
        page.continuation.is_none(),
        "independent canonical membership terminates",
    )?;
    Ok(page.object_ids)
}

async fn mcp_members(
    driver: &mut impl McpDriver,
    space_id: &str,
    collection_id: &str,
) -> Result<(Vec<String>, usize), String> {
    let walk = collect_id_pages(
        driver,
        "collection_member_list",
        json!({"space":space_id,"collection_id":collection_id}),
        "/object_id",
        16,
    )
    .await?;
    let members = walk
        .items
        .into_iter()
        .map(|item| {
            item["object_id"]
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| "collection_member_list omitted object_id".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((members, walk.pages))
}

/// Exercises ordinary MCP workflows across basic, collection, grid, filtered,
/// and Kanban layouts without introducing a layout-specific protocol surface.
pub fn run_representative_layout_scenario<'a>(
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
) -> Pin<Box<dyn Future<Output = Result<LayoutScenarioEvidence, String>> + 'a>> {
    Box::pin(async move {
        const REQUIRED: [&str; 8] = [
            "collection_member_add",
            "collection_member_list",
            "collection_member_remove",
            "object_get",
            "object_update",
            "type_list",
            "view_list",
            "view_object_list",
        ];
        const FORBIDDEN: [&str; 4] = [
            "kanban_column_move",
            "kanban_get",
            "layout_get",
            "view_filter_set",
        ];
        let tools = driver.list_tools().await?;
        for required in REQUIRED {
            require(
                tools.iter().any(|name| name == required),
                &format!("representative layout scenario requires {required}"),
            )?;
        }
        for forbidden in FORBIDDEN {
            require(
                !tools.iter().any(|name| name == forbidden),
                &format!("layout-specific tool must remain absent: {forbidden}"),
            )?;
        }

        let suffix = unique_suffix();
        let fixture_name = format!("MCP representative layout {suffix}");
        let mut fixture = Box::pin(ctx.create_kanban_fixture(&fixture_name))
            .await
            .map_err(|_| "create cleanup-owned representative Kanban fixture".to_owned())?;
        let first_item = fixture
            .items
            .first()
            .ok_or_else(|| "Kanban fixture omitted its first item".to_owned())?;
        let first_item_id = first_item.object.id.clone();
        let first_item_name = first_item
            .object
            .name
            .clone()
            .ok_or_else(|| "Kanban fixture item omitted its name".to_owned())?;
        let removed_item_id = fixture
            .items
            .get(2)
            .map(|item| item.object.id.clone())
            .ok_or_else(|| "Kanban fixture omitted its third item".to_owned())?;
        let destination_id = fixture
            .columns
            .get(1)
            .map(|column| column.id.clone())
            .ok_or_else(|| "Kanban fixture omitted its destination column".to_owned())?;

        let expected_type_layouts = BTreeMap::from([
            (fixture.item_type.id.clone(), "basic".to_owned()),
            (fixture.collection_type.id.clone(), "collection".to_owned()),
        ]);
        let mut observed_type_layouts = BTreeMap::new();
        let mut observed_type_pages = 0;
        for _ in 0..10 {
            let types = collect_id_pages(
                driver,
                "type_list",
                json!({
                    "space":ctx.space_id,
                    "filters":{
                        "operator":"and",
                        "conditions":[{
                            "format":"text",
                            "property_key":"name",
                            "condition":"contains",
                            "value":fixture_name
                        }]
                    }
                }),
                "/id",
                8,
            )
            .await?;
            observed_type_pages = types.pages;
            observed_type_layouts.clear();
            for item in &types.items {
                if let (Some(id), Some(layout)) = (item["id"].as_str(), item["layout"].as_str())
                    && expected_type_layouts.contains_key(id)
                {
                    observed_type_layouts.insert(id.to_owned(), layout.to_owned());
                }
            }
            if observed_type_layouts == expected_type_layouts && types.pages == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        require(
            observed_type_layouts == expected_type_layouts && observed_type_pages == 2,
            "type_list preserves representative basic and collection layouts",
        )?;

        for (object_id, expected_type_key) in [
            (first_item_id.as_str(), fixture.item_type.key.as_str()),
            (
                fixture.collection.id.as_str(),
                fixture.collection_type.key.as_str(),
            ),
        ] {
            let object = driver
                .call_tool(
                    "object_get",
                    json!({"space":ctx.space_id,"object_id":object_id}),
                )
                .await?;
            require(
                object.pointer("/object/summary/id").and_then(Value::as_str) == Some(object_id)
                    && object
                        .pointer("/object/summary/type_key")
                        .and_then(Value::as_str)
                        == Some(expected_type_key),
                "object_get preserves representative object type identity",
            )?;
        }

        let views = collect_id_pages(
            driver,
            "view_list",
            json!({"space":ctx.space_id,"list_id":fixture.collection.id}),
            "/id",
            16,
        )
        .await?;
        require(
            views.pages >= 2,
            "view_list follows a limit-one continuation",
        )?;
        let view_layouts = views
            .items
            .iter()
            .filter_map(|item| Some((item["id"].as_str()?, item["layout"].as_str()?)))
            .collect::<BTreeMap<_, _>>();
        require(
            view_layouts.get(fixture.view.id.as_str()) == Some(&"kanban"),
            "view_list preserves the Kanban layout",
        )?;
        let grid_view_id = view_layouts
            .iter()
            .find_map(|(id, layout)| (*layout == "grid").then(|| (*id).to_owned()))
            .ok_or_else(|| "view_list omitted the representative grid layout".to_owned())?;

        let canonical_before = canonical_members(ctx, &fixture.collection.id).await?;
        require(
            canonical_before.len() == fixture.items.len()
                && fixture
                    .items
                    .iter()
                    .all(|item| canonical_before.contains(&item.object.id)),
            "independent canonical membership contains every Kanban card",
        )?;
        let (mcp_before, before_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            mcp_before == canonical_before && before_pages == canonical_before.len(),
            "collection_member_list matches independent canonical order",
        )?;

        let updated = driver
            .call_tool(
                "object_update",
                json!({
                    "space":ctx.space_id,
                    "object_id":first_item_id,
                    "properties":[{
                        "format":"select",
                        "key":fixture.status_property.key,
                        "select":destination_id
                    }]
                }),
            )
            .await?;
        require(
            updated.pointer("/object/id").and_then(Value::as_str) == Some(first_item_id.as_str()),
            "ordinary object_update preserves moved card identity",
        )?;
        let moved = ctx
            .client
            .object(&ctx.space_id, &first_item_id)
            .get()
            .await
            .map_err(|_| "independently read moved Kanban card".to_owned())?;
        require(
            moved
                .get_property_select(&fixture.status_property.key)
                .map(|tag| tag.id.as_str())
                == Some(destination_id.as_str()),
            "ordinary Select-property update moves the Kanban card",
        )?;
        if let Some(item) = fixture
            .items
            .iter_mut()
            .find(|item| item.object.id == first_item_id)
        {
            item.object = moved;
            item.column_id = Some(destination_id.clone());
        }

        let destination_items = collect_id_pages(
            driver,
            "view_object_list",
            json!({
                "space":ctx.space_id,
                "list_id":fixture.collection.id,
                "view":fixture.view.id,
                "property_keys":[fixture.status_property.key],
                "filters":{
                    "operator":"and",
                    "conditions":[{
                        "format":"select",
                        "property_key":fixture.status_property.key,
                        "condition":"in",
                        "values":[destination_id]
                    }]
                }
            }),
            "/summary/id",
            8,
        )
        .await?;
        let expected_destination_ids = fixture
            .items
            .iter()
            .filter(|item| item.column_id.as_deref() == Some(destination_id.as_str()))
            .map(|item| item.object.id.as_str())
            .collect::<BTreeSet<_>>();
        require(
            destination_items.pages == 2,
            "filtered Kanban view follows its limit-one continuation",
        )?;
        let actual_destination_ids = destination_items
            .items
            .iter()
            .filter_map(|item| item.pointer("/summary/id").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        require(
            expected_destination_ids.len() == 2
                && actual_destination_ids == expected_destination_ids,
            "filtered Kanban pagination returns the exact destination column",
        )?;

        let removed = driver
            .call_tool(
                "collection_member_remove",
                json!({
                    "space":ctx.space_id,
                    "collection_id":fixture.collection.id,
                    "object_id":removed_item_id
                }),
            )
            .await?;
        require(
            removed["collection_id"] == fixture.collection.id
                && removed["object_id"] == removed_item_id
                && removed["membership"] == "absent",
            "collection_member_remove returns exact absence evidence",
        )?;
        let after_remove = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_after_remove, remove_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            !after_remove.contains(&removed_item_id)
                && mcp_after_remove == after_remove
                && remove_pages == after_remove.len(),
            "removed card is absent from canonical membership",
        )?;
        let survived = ctx
            .client
            .object(&ctx.space_id, &removed_item_id)
            .get()
            .await
            .map_err(|_| "removed collection member object must survive".to_owned())?;
        require(
            survived.id == removed_item_id,
            "collection removal does not delete the card",
        )?;

        let added = driver
            .call_tool(
                "collection_member_add",
                json!({
                    "space":ctx.space_id,
                    "collection_id":fixture.collection.id,
                    "object_id":removed_item_id
                }),
            )
            .await?;
        require(
            added["collection_id"] == fixture.collection.id
                && added["object_id"] == removed_item_id
                && added["membership"] == "present",
            "collection_member_add returns exact presence evidence",
        )?;
        let canonical_after = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_after_add, add_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            canonical_after.contains(&removed_item_id)
                && mcp_after_add == canonical_after
                && add_pages == canonical_after.len(),
            "re-added card returns to canonical paginated membership",
        )?;

        Box::pin(ctx.add_collection_name_filter_fixture(
            &fixture.collection.id,
            &fixture.view.id,
            &first_item_name,
        ))
        .await
        .map_err(|_| "configure cleanup-owned filtered Kanban view".to_owned())?;
        let canonical_filtered = canonical_members(ctx, &fixture.collection.id).await?;
        let (mcp_filtered, filtered_member_pages) =
            mcp_members(driver, &ctx.space_id, &fixture.collection.id).await?;
        require(
            canonical_filtered == canonical_after
                && mcp_filtered == canonical_after
                && filtered_member_pages == canonical_after.len(),
            "saved-view filtering preserves exact canonical membership",
        )?;
        let filtered_after = collect_id_pages(
            driver,
            "view_object_list",
            json!({
                "space":ctx.space_id,
                "list_id":fixture.collection.id,
                "view":fixture.view.id
            }),
            "/summary/id",
            8,
        )
        .await?;
        require(
            filtered_after.pages == 1
                && filtered_after.items.len() == 1
                && filtered_after.items[0]
                    .pointer("/summary/id")
                    .and_then(Value::as_str)
                    == Some(first_item_id.as_str()),
            "membership and column mutations preserve saved-view filtering",
        )?;
        require(
            canonical_filtered.len() > filtered_after.items.len()
                && canonical_filtered.iter().any(|id| id == &first_item_id),
            "canonical membership remains independent of filter visibility",
        )?;

        Ok(LayoutScenarioEvidence {
            collection_id: fixture.collection.id,
            grid_view_id,
            kanban_view_id: fixture.view.id,
            moved_item_id: first_item_id,
            member_ids: canonical_filtered,
        })
    })
}

/// Cleanup-owned identities and unique text for the complete chats scenario.
pub struct ChatsRegistryFixture<'a> {
    pub space_id: &'a str,
    pub chat_id: &'a str,
    pub seed_message_id: &'a str,
    pub search_query: &'a str,
    pub add_text: &'a str,
    pub idempotency_key: &'a str,
}

/// Content-minimized evidence returned by the complete chats scenario.
#[derive(Debug, PartialEq, Eq)]
pub struct ChatsRegistryEvidence {
    pub chat_id: String,
    pub seed_message_id: String,
    pub added_message_id: String,
    pub deleted: bool,
}

fn page_contains_id(page: &Value, id: &str) -> bool {
    page["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["id"] == id))
}

fn search_contains_id(page: &Value, id: &str) -> bool {
    page["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["message"]["id"] == id))
}

/// Runs all six production chat tools through one transport-neutral driver.
pub async fn run_chats_registry_scenario(
    driver: &mut impl McpDriver,
    fixture: ChatsRegistryFixture<'_>,
) -> Result<ChatsRegistryEvidence, String> {
    const CHAT_NAMES: [&str; 6] = [
        "chat_list",
        "chat_message_add",
        "chat_message_delete",
        "chat_message_get",
        "chat_message_list",
        "chat_message_search",
    ];
    let tools = driver.list_tools().await?;
    let actual = tools
        .iter()
        .filter(|name| name.starts_with("chat_"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if actual != CHAT_NAMES {
        return Err("chats registry inventory differs from the reviewed six tools".to_owned());
    }

    let chats = driver
        .call_tool("chat_list", json!({"space":fixture.space_id,"limit":20}))
        .await?;
    if !page_contains_id(&chats, fixture.chat_id) {
        return Err("chat_list omitted the cleanup-owned chat".to_owned());
    }
    let history = driver
        .call_tool(
            "chat_message_list",
            json!({"space":fixture.space_id,"chat_id":fixture.chat_id,"limit":12}),
        )
        .await?;
    if !page_contains_id(&history, fixture.seed_message_id) {
        return Err("chat_message_list omitted the cleanup-owned seed".to_owned());
    }
    let mut search_observed = false;
    for attempt in 0..20 {
        let search = driver
            .call_tool(
                "chat_message_search",
                json!({
                    "space":fixture.space_id,
                    "chat_id":fixture.chat_id,
                    "query":fixture.search_query,
                    "limit":12
                }),
            )
            .await?;
        if search_contains_id(&search, fixture.seed_message_id) {
            search_observed = true;
            break;
        }
        if attempt != 19 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    if !search_observed {
        return Err("chat_message_search omitted the cleanup-owned seed".to_owned());
    }

    let add_input = json!({
        "space":fixture.space_id,
        "chat_id":fixture.chat_id,
        "text":fixture.add_text,
        "idempotency_key":fixture.idempotency_key,
    });
    let added = driver
        .call_tool("chat_message_add", add_input.clone())
        .await?;
    let message_id = added["message"]["id"]
        .as_str()
        .ok_or_else(|| "chat_message_add omitted the assigned ID".to_owned())?
        .to_owned();
    let modified_at = added["message"]["modified_at"]
        .as_str()
        .ok_or_else(|| "chat_message_add omitted the canonical timestamp".to_owned())?
        .to_owned();
    if added["message"]["text"] != fixture.add_text || added["idempotency"]["key_reused"] != false {
        return Err("chat_message_add returned incorrect first-call evidence".to_owned());
    }

    let replay = driver
        .call_tool("chat_message_add", add_input.clone())
        .await?;
    if replay["message"]["id"] != message_id || replay["idempotency"]["key_reused"] != true {
        return Err("chat_message_add replay changed identity or missed reuse".to_owned());
    }
    let detail = driver
        .call_tool(
            "chat_message_get",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
            }),
        )
        .await?;
    if detail["message"]["id"] != message_id || detail["message"]["text"] != fixture.add_text {
        return Err("chat_message_get disagreed with verified add".to_owned());
    }

    let conflict = driver
        .call_tool_error(
            "chat_message_add",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "text":format!("{} conflict", fixture.add_text),
                "idempotency_key":fixture.idempotency_key,
            }),
        )
        .await?;
    if conflict != "conflict" {
        return Err("changed chat add replay was not a conflict".to_owned());
    }

    let deleted = driver
        .call_tool(
            "chat_message_delete",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
                "expected_modified_at":modified_at,
                "confirm_delete":"delete_message",
            }),
        )
        .await?;
    if deleted["message_id"] != message_id || deleted["deleted"] != true {
        return Err("chat_message_delete omitted verified absence".to_owned());
    }
    let absence = driver
        .call_tool_error(
            "chat_message_get",
            json!({
                "space":fixture.space_id,
                "chat_id":fixture.chat_id,
                "message_id":message_id,
            }),
        )
        .await?;
    if absence != "not_found" {
        return Err("deleted chat message remained readable".to_owned());
    }

    Ok(ChatsRegistryEvidence {
        chat_id: fixture.chat_id.to_owned(),
        seed_message_id: fixture.seed_message_id.to_owned(),
        added_message_id: message_id,
        deleted: true,
    })
}

/// Stable identifiers for every executable live scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScenarioId {
    Discovery,
    Documents,
    Views,
    Mutations,
    MarkdownNoop,
    Archive,
    #[cfg(test)]
    SyntheticNonExecutable,
}

impl ScenarioId {
    pub const EXECUTABLE: [Self; 6] = [
        Self::Discovery,
        Self::Documents,
        Self::Views,
        Self::Mutations,
        Self::MarkdownNoop,
        Self::Archive,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "standard_discovery",
            Self::Documents => "standard_documents",
            Self::Views => "standard_views",
            Self::Mutations => "standard_mutations",
            Self::MarkdownNoop => "standard_markdown_noop",
            Self::Archive => "standard_archive",
            #[cfg(test)]
            Self::SyntheticNonExecutable => "synthetic_non_executable",
        }
    }

    pub const fn is_executable(self) -> bool {
        match self {
            Self::Discovery
            | Self::Documents
            | Self::Views
            | Self::Mutations
            | Self::MarkdownNoop
            | Self::Archive => true,
            #[cfg(test)]
            Self::SyntheticNonExecutable => false,
        }
    }
}

/// Bounded non-content evidence accumulated while a scenario builds fixtures.
#[derive(Debug)]
pub struct ScenarioEvidence {
    pub scenario: ScenarioId,
    pub fixture_ids: Vec<String>,
    redactions: Vec<String>,
}

impl ScenarioEvidence {
    pub fn new(scenario: ScenarioId) -> Self {
        Self {
            scenario,
            fixture_ids: Vec::new(),
            redactions: Vec::new(),
        }
    }

    pub fn fixture(&mut self, id: &str) {
        self.fixture_ids.push(id.to_owned());
    }

    pub fn sensitive(&mut self, value: &str) {
        if !value.is_empty() {
            self.redactions.push(value.to_owned());
        }
    }

    pub fn sanitize(&self, value: &str) -> String {
        let mut sanitized = value.to_owned();
        for secret in &self.redactions {
            sanitized = sanitized.replace(secret, "<redacted-content>");
        }
        const MAX_EVIDENCE_CHARS: usize = 16_384;
        sanitized.chars().take(MAX_EVIDENCE_CHARS).collect()
    }
}

/// Inputs owned by the fixture rather than by a transport driver.
pub struct DocumentFixture<'a> {
    pub space_id: &'a str,
    pub object_id: &'a str,
    pub name: &'a str,
    pub initial_body: &'a str,
    pub old_text: &'a str,
    pub new_text: &'a str,
}

/// Observable result used for an independent backend readback assertion.
pub struct DocumentScenarioEvidence {
    pub expected_body: String,
    pub edited_sha256: String,
}

/// Transport-neutral inputs for an exact exported-Markdown replacement.
pub struct MarkdownNoopFixture<'a> {
    pub space_id: &'a str,
    pub object_id: &'a str,
    pub exported_body: &'a str,
}

/// Content-free protocol evidence for an exact Markdown replacement.
#[derive(Debug, PartialEq, Eq)]
pub struct MarkdownNoopProtocolEvidence {
    pub body_sha256: String,
    pub before_bytes: usize,
    pub after_bytes: usize,
}

/// Runs the MCP-only portion of an exported-Markdown no-op workflow.
pub async fn run_markdown_noop_protocol(
    driver: &mut impl McpDriver,
    fixture: MarkdownNoopFixture<'_>,
) -> Result<MarkdownNoopProtocolEvidence, String> {
    let before = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        before.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op pre-read object identity",
    )?;
    require(
        before
            .pointer("/object/summary/space_id")
            .and_then(Value::as_str)
            == Some(fixture.space_id),
        "Markdown no-op pre-read space identity",
    )?;
    let before_body = required_string(&before, "/body/text")?;
    require(
        before_body == fixture.exported_body,
        "Markdown no-op pre-read complete export",
    )?;
    let body_sha256 = required_string(&before, "/body/sha256")?;
    let independent_hash = Sha256::digest(fixture.exported_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        body_sha256 == independent_hash,
        "Markdown no-op pre-read hash",
    )?;

    let updated = driver
        .call_tool(
            "object_update",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body_markdown": fixture.exported_body,
                "expected_body_sha256": body_sha256
            }),
        )
        .await?;
    require(
        updated.pointer("/object/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op update object identity",
    )?;
    require(
        updated.pointer("/object/space_id").and_then(Value::as_str) == Some(fixture.space_id),
        "Markdown no-op update space identity",
    )?;
    require(
        updated.get("body_sha256").and_then(Value::as_str) == Some(body_sha256.as_str()),
        "Markdown no-op update hash",
    )?;

    let after = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        after.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "Markdown no-op repeated export object identity",
    )?;
    let after_body = required_string(&after, "/body/text")?;
    require(
        after_body == fixture.exported_body,
        "Markdown no-op repeated export byte identity",
    )?;
    require(
        after.pointer("/body/sha256").and_then(Value::as_str) == Some(body_sha256.as_str()),
        "Markdown no-op repeated export hash",
    )?;
    Ok(MarkdownNoopProtocolEvidence {
        body_sha256,
        before_bytes: before_body.len(),
        after_bytes: after_body.len(),
    })
}

/// Runs the compact document workflow through an arbitrary MCP transport.
pub async fn run_document_scenario(
    driver: &mut impl McpDriver,
    fixture: DocumentFixture<'_>,
) -> Result<DocumentScenarioEvidence, String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let mut found = false;
    for _ in 0..10 {
        let search = driver
            .call_tool(
                "object_search",
                json!({
                    "space": fixture.space_id,
                    "text": fixture.name,
                    "limit": 100
                }),
            )
            .await?;
        found = search["items"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item.pointer("/summary/id").and_then(Value::as_str) == Some(fixture.object_id)
            })
        });
        if found {
            break;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    require(found, "object_search observes the fixture object")?;

    let object = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        object.pointer("/object/summary/id").and_then(Value::as_str) == Some(fixture.object_id),
        "object_get identity",
    )?;
    require(
        object.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get complete body",
    )?;
    let uri = required_string(&object, "/object/summary/resource_uri")?;

    let resource = driver.read_resource(&uri).await?;
    require(
        resource.pointer("/contents/0/uri").and_then(Value::as_str) == Some(uri.as_str()),
        "resources/read canonical URI",
    )?;
    require(
        resource.pointer("/contents/0/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "resources/read complete body",
    )?;

    // Refresh the optimistic-concurrency token immediately before the edit.
    // A newly created Anytype document can still be converging while the
    // independent resource observation above completes.
    let current = driver
        .call_tool(
            "object_get",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "body": {"max_chars": 100_000}
            }),
        )
        .await?;
    require(
        current.pointer("/body/text").and_then(Value::as_str) == Some(fixture.initial_body),
        "object_get body remains stable before edit",
    )?;
    let body_sha256 = required_string(&current, "/body/sha256")?;
    let independently_hashed = Sha256::digest(fixture.initial_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        body_sha256 == independently_hashed,
        "object_get hash matches the complete observed body",
    )?;
    require(
        fixture.initial_body.match_indices(fixture.old_text).count() == 1,
        "fixture body contains exactly one edit match",
    )?;

    let edited = driver
        .call_tool(
            "object_edit",
            json!({
                "space": fixture.space_id,
                "object_id": fixture.object_id,
                "edits": [{
                    "old_text": fixture.old_text,
                    "new_text": fixture.new_text,
                    "expected_matches": 1
                }],
                "expected_body_sha256": body_sha256
            }),
        )
        .await?;
    let edited_sha256 = required_string(&edited, "/body_sha256")?;
    let expected_body = fixture
        .initial_body
        .replacen(fixture.old_text, fixture.new_text, 1);
    require(
        expected_body != fixture.initial_body,
        "fixture edit changes exactly one fragment",
    )?;
    Ok(DocumentScenarioEvidence {
        expected_body,
        edited_sha256,
    })
}

/// Heap-owned dispatch future for a complete live scenario.
pub type ScenarioFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + 'a>>;

/// Executes one complete standard-baseline scenario through the selected driver.
///
/// The erased, heap-owned return type is intentional: these fixture-heavy
/// debug futures exceed Tokio's worker-stack budget when their state is kept
/// inline by an ordinary `async fn` dispatcher.
pub fn run_scenario<'a>(
    scenario: ScenarioId,
    driver: &'a mut impl McpDriver,
    ctx: &'a TestContext,
    evidence: &'a mut ScenarioEvidence,
) -> ScenarioFuture<'a> {
    Box::pin(async move {
        match scenario {
            ScenarioId::Discovery => Box::pin(run_discovery(driver, ctx, evidence)).await,
            ScenarioId::Documents => Box::pin(run_documents(driver, ctx, evidence)).await,
            ScenarioId::Views => Box::pin(run_views(driver, ctx, evidence)).await,
            ScenarioId::Mutations => Box::pin(run_mutations(driver, ctx, evidence)).await,
            ScenarioId::MarkdownNoop => Box::pin(run_markdown_noop(driver, ctx, evidence)).await,
            ScenarioId::Archive => Box::pin(run_archive(driver, ctx, evidence)).await,
            #[cfg(test)]
            ScenarioId::SyntheticNonExecutable => {
                Err("scenario is intentionally non-executable".to_owned())
            }
        }
    })
}

async fn run_discovery(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let status = driver.call_tool("server_status", json!({})).await?;
    require(
        status["http_available"] == true,
        "server_status HTTP availability",
    )?;

    let first_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create first disposable space fixture".to_owned())?;
    evidence.fixture(&first_space.id);
    let second_space = ctx
        .create_space_fixture(format!("MCP shared space {}", unique_suffix()))
        .await
        .map_err(|_| "create second disposable space fixture".to_owned())?;
    evidence.fixture(&second_space.id);

    let duplicate_name = format!("MCP shared ambiguous {}", unique_suffix());
    evidence.sensitive(&duplicate_name);
    let first_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_a_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create first type fixture".to_owned())?;
    ctx.register_type(&first_type.id);
    evidence.fixture(&first_type.id);
    let second_type = ctx
        .client
        .new_type(&ctx.space_id, &duplicate_name)
        .key(format!("mcp_shared_b_{}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create second type fixture".to_owned())?;
    ctx.register_type(&second_type.id);
    evidence.fixture(&second_type.id);

    let property = ctx
        .client
        .new_property(
            &ctx.space_id,
            format!("MCP shared select {}", unique_suffix()),
            PropertyFormat::Select,
        )
        .create()
        .await
        .map_err(|_| "create select property fixture".to_owned())?;
    ctx.register_property(&property.id);
    evidence.fixture(&property.id);
    let mut tag_ids = Vec::new();
    for (name, color) in [("First", Color::Blue), ("Second", Color::Red)] {
        let tag = ctx
            .client
            .new_tag(&ctx.space_id, &property.id)
            .name(format!("{name} {}", unique_suffix()))
            .color(color)
            .create()
            .await
            .map_err(|_| "create tag fixture".to_owned())?;
        evidence.fixture(&tag.id);
        tag_ids.push(tag.id);
    }

    let templates = ctx
        .create_template_fixtures(
            format!("MCP shared template type {}", unique_suffix()),
            [
                format!("MCP shared template A {}", unique_suffix()),
                format!("MCP shared template B {}", unique_suffix()),
            ],
        )
        .await
        .map_err(|_| "create template fixtures".to_owned())?;
    evidence.fixture(&templates.type_.id);
    let template_ids = templates
        .templates
        .iter()
        .map(|template| {
            evidence.fixture(&template.id);
            template.id.clone()
        })
        .collect::<Vec<_>>();

    let search_term = format!("McpSharedSearch{}", unique_suffix());
    evidence.sensitive(&search_term);
    let mut object_ids = Vec::new();
    for ordinal in ["first", "second"] {
        let object = create_object(ctx, &format!("{search_term} {ordinal}"), "").await?;
        evidence.fixture(&object.id);
        object_ids.push(object.id);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    walk_pages(
        driver,
        "space_list",
        json!({}),
        &[first_space.id, second_space.id],
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "type_list",
        json!({"space": ctx.space_id}),
        &[first_type.id.clone(), second_type.id.clone()],
        1_000,
    )
    .await?;
    assert_filtered_cursor_contract(
        driver,
        "type_list",
        json!({"space": ctx.space_id}),
        &duplicate_name,
        &[first_type.id.clone(), second_type.id.clone()],
    )
    .await?;
    walk_pages(
        driver,
        "property_list",
        json!({"space": ctx.space_id}),
        std::slice::from_ref(&property.id),
        1_000,
    )
    .await?;
    walk_pages(
        driver,
        "tag_list",
        json!({"space": ctx.space_id, "property": property.id}),
        &tag_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "template_list",
        json!({"space": ctx.space_id, "type": templates.type_.id}),
        &template_ids,
        32,
    )
    .await?;
    walk_pages(
        driver,
        "object_search",
        json!({"space": ctx.space_id, "text": search_term}),
        &object_ids,
        32,
    )
    .await?;
    let ambiguity = driver
        .call_tool_error(
            "property_list",
            json!({"space": ctx.space_id, "type": duplicate_name, "limit": 1}),
        )
        .await?;
    require(ambiguity == "ambiguous", "ambiguous type resolution")
}

async fn run_documents(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let name = format!("MCP shared document {}", unique_suffix());
    let body = "gamma concurrent body";
    evidence.sensitive(&name);
    evidence.sensitive(body);
    evidence.sensitive("gamma");
    evidence.sensitive("delta");
    let object = create_object(ctx, &name, "").await?;
    evidence.fixture(&object.id);
    ctx.client
        .update_object(&ctx.space_id, &object.id)
        .body(body)
        .ensure_available()
        .update()
        .await
        .map_err(|_| "set document scenario body".to_owned())?;
    let initial = read_body(ctx, &object.id).await?;

    let resources = driver.list_resources().await?;
    require(
        resources["resources"] == json!([]),
        "resources/list is empty",
    )?;
    let templates = driver.list_resource_templates().await?;
    require(
        templates["resourceTemplates"][0]["uriTemplate"]
            == "anytype://spaces/{space_id}/objects/{object_id}",
        "resource template identity",
    )?;
    let result = run_document_scenario(
        driver,
        DocumentFixture {
            space_id: &ctx.space_id,
            object_id: &object.id,
            name: &name,
            initial_body: &initial,
            old_text: "gamma",
            new_text: "delta",
        },
    )
    .await?;
    let stored_body = read_body(ctx, &object.id).await?;
    require(
        stored_body == result.expected_body,
        "independent document edit readback",
    )?;
    let stored_sha256 = Sha256::digest(stored_body.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    require(
        stored_sha256 == result.edited_sha256,
        "edit result hash matches independent backend readback",
    )
}

async fn run_views(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let collection_type = ctx
        .create_collection_type_fixture(format!("MCP shared collection type {}", unique_suffix()))
        .await
        .map_err(|_| "create collection type fixture".to_owned())?;
    require(
        collection_type.layout == ObjectLayout::Collection,
        "collection fixture layout",
    )?;
    evidence.fixture(&collection_type.id);
    let collection = ctx
        .create_collection_fixture(
            &collection_type,
            format!("MCP shared collection {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create collection fixture".to_owned())?;
    evidence.fixture(&collection.id);
    let second_view = ctx
        .create_collection_view_fixture(
            &collection.id,
            &format!("MCP shared second view {}", unique_suffix()),
        )
        .await
        .map_err(|_| "create second view fixture".to_owned())?;
    evidence.fixture(&second_view.id);
    let first = create_object(ctx, &format!("MCP view A {}", unique_suffix()), "").await?;
    let second = create_object(ctx, &format!("MCP view B {}", unique_suffix()), "").await?;
    evidence.fixture(&first.id);
    evidence.fixture(&second.id);
    ctx.client
        .view_add_objects(
            &ctx.space_id,
            &collection.id,
            vec![first.id.clone(), second.id.clone()],
        )
        .await
        .map_err(|_| "add collection members".to_owned())?;
    let views = ctx
        .client
        .list_views(&ctx.space_id, &collection.id)
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read collection views".to_owned())?
        .into_response();
    let view_ids = views
        .items
        .into_iter()
        .map(|view| view.id)
        .collect::<Vec<_>>();
    walk_pages(
        driver,
        "view_list",
        json!({"space": ctx.space_id, "list_id": collection.id}),
        &view_ids,
        16,
    )
    .await?;
    for _ in 0..10 {
        match walk_pages(
            driver,
            "view_object_list",
            json!({
                "space": ctx.space_id,
                "list_id": collection.id,
                "view": second_view.id
            }),
            &[first.id.clone(), second.id.clone()],
            16,
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    Err("view_object_list did not converge".to_owned())
}

async fn run_mutations(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let suffix = unique_suffix();
    let name = format!("MCP shared mutation {suffix}");
    evidence.sensitive(&name);
    let create_input = json!({
        "space": ctx.space_id,
        "type": "page",
        "name": name,
        "idempotency_key": format!("mcp-shared-{suffix}")
    });
    let created = driver
        .call_tool("object_create", create_input.clone())
        .await?;
    let object_id = required_string(&created, "/object/id")?;
    ctx.register_object(&object_id);
    evidence.fixture(&object_id);
    let replay = driver.call_tool("object_create", create_input).await?;
    require(
        replay["object"]["id"] == object_id,
        "idempotent create replay identity",
    )?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read created object".to_owned())?;
    require(
        visible.name.as_deref() == Some(name.as_str()),
        "create readback",
    )?;

    let current = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let updated_name = format!("MCP shared updated {suffix}");
    evidence.sensitive(&updated_name);
    driver
        .call_tool(
            "object_update",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "name": updated_name,
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    let visible = ctx
        .client
        .object(&ctx.space_id, &object_id)
        .get()
        .await
        .map_err(|_| "read updated object".to_owned())?;
    require(
        visible.name.as_deref() == Some(updated_name.as_str()),
        "update readback",
    )?;

    ctx.client
        .update_object(&ctx.space_id, &object_id)
        .body("gamma concurrent body")
        .ensure_available()
        .update()
        .await
        .map_err(|_| "create concurrent body state".to_owned())?;
    let stale = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "stale"}],
                "expected_body_sha256": current["body"]["sha256"]
            }),
        )
        .await?;
    require(stale == "conflict", "stale edit conflict")?;
    let fresh = driver
        .call_tool(
            "object_get",
            json!({"space": ctx.space_id, "object_id": object_id, "body": {"max_chars": 100}}),
        )
        .await?;
    let count = driver
        .call_tool_error(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "absent", "new_text": "never", "expected_matches": 1}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(count == "conflict", "match-count conflict")?;
    driver
        .call_tool(
            "object_edit",
            json!({
                "space": ctx.space_id,
                "object_id": object_id,
                "edits": [{"old_text": "gamma", "new_text": "delta"}],
                "expected_body_sha256": fresh["body"]["sha256"]
            }),
        )
        .await?;
    require(
        read_body(ctx, &object_id)
            .await?
            .contains("delta concurrent body"),
        "edit readback",
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MarkdownBlockEvidence {
    identity: Vec<(String, String, Vec<String>)>,
    semantics: Vec<(String, usize)>,
    has_link: bool,
}

fn markdown_block_kind(content: &BlockContent) -> String {
    match content {
        BlockContent::Text(text) => format!("text:{:?}", text.style),
        BlockContent::Layout(_) => "layout".to_owned(),
        BlockContent::Divider(_) => "divider".to_owned(),
        BlockContent::Bookmark(_) => "bookmark".to_owned(),
        BlockContent::Link(_) => "link".to_owned(),
        BlockContent::Relation(_) => "relation".to_owned(),
        BlockContent::FeaturedRelations => "featured_relations".to_owned(),
        BlockContent::Embed(_) => "embed".to_owned(),
        BlockContent::TableOfContents => "table_of_contents".to_owned(),
        BlockContent::Table => "table".to_owned(),
        BlockContent::TableRow { .. } => "table_row".to_owned(),
        BlockContent::TableColumn => "table_column".to_owned(),
        BlockContent::File(_) => "file".to_owned(),
        BlockContent::Unsupported(_) => "unsupported".to_owned(),
        _ => "future".to_owned(),
    }
}

fn markdown_block_evidence(snapshot: &BodySnapshot) -> Result<MarkdownBlockEvidence, String> {
    let mut identity = Vec::with_capacity(snapshot.len());
    let mut semantics = Vec::with_capacity(snapshot.len());
    let mut has_link = false;
    for block in snapshot.iter() {
        let kind = markdown_block_kind(&block.content);
        if let BlockContent::Text(text) = &block.content {
            has_link |= text
                .marks
                .iter()
                .any(|mark| matches!(mark.kind, MarkKind::Link { .. }));
        }
        let content = serde_json::to_string(&block.content)
            .map_err(|_| "serialize Markdown block evidence".to_owned())?;
        identity.push((
            block.id.as_str().to_owned(),
            kind,
            block
                .children
                .iter()
                .map(|child| child.as_str().to_owned())
                .collect(),
        ));
        semantics.push((content, block.children.len()));
    }
    Ok(MarkdownBlockEvidence {
        identity,
        semantics,
        has_link,
    })
}

async fn stable_markdown_export(ctx: &TestContext, object_id: &str) -> Result<String, String> {
    let mut previous = None;
    for _ in 0..12 {
        let markdown = ctx
            .client
            .object(&ctx.space_id, object_id)
            .get()
            .await
            .map_err(|_| "read independent Markdown export".to_owned())?
            .markdown
            .ok_or_else(|| "independent readback omitted Markdown".to_owned())?;
        if previous.as_ref() == Some(&markdown) {
            return Ok(markdown);
        }
        previous = Some(markdown);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("independent Markdown export did not stabilize".to_owned())
}

async fn stable_markdown_blocks(
    ctx: &TestContext,
    object_id: &str,
) -> Result<MarkdownBlockEvidence, String> {
    let mut previous = None;
    for _ in 0..12 {
        let snapshot = ctx
            .client
            .blocks()
            .body(&ctx.space_id, object_id)
            .fetch()
            .await
            .map_err(|_| "read independent Markdown ObjectShow evidence".to_owned())?;
        let evidence = markdown_block_evidence(&snapshot)?;
        if previous.as_ref() == Some(&evidence) {
            return Ok(evidence);
        }
        previous = Some(evidence);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err("independent Markdown ObjectShow evidence did not stabilize".to_owned())
}

async fn run_markdown_noop(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let name = format!("MCP Markdown no-op {}", unique_suffix());
    let requested = concat!(
        "# Document heading\n\n",
        "## Stable heading\n\n",
        "- bullet one\n",
        "- bullet two\n\n",
        "1. numbered one\n",
        "2. numbered two\n\n",
        "- [ ] unchecked\n",
        "- [x] checked\n\n",
        "> one-line quote\n\n",
        "A [bounded link](https://example.com/path?q=one) with Unicode こんにちは 👋.\n\n",
        "First paragraph spans\n",
        "multiple source lines.\n\n",
        "Final paragraph."
    );
    evidence.sensitive(&name);
    evidence.sensitive(requested);
    let object = create_object(ctx, &name, requested).await?;
    evidence.fixture(&object.id);

    let before_export = stable_markdown_export(ctx, &object.id).await?;
    evidence.sensitive(&before_export);
    let before_blocks = stable_markdown_blocks(ctx, &object.id).await?;
    let before_kinds = before_blocks
        .identity
        .iter()
        .map(|(_, kind, _)| kind.as_str())
        .collect::<Vec<_>>();
    for expected in [
        "text:Header2",
        "text:Bulleted",
        "text:Numbered",
        "text:Checkbox",
        "text:Quote",
        "text:Paragraph",
    ] {
        if !before_kinds.contains(&expected) {
            return Err(format!(
                "Markdown no-op fixture expected block kind={expected} observed={before_kinds:?}"
            ));
        }
    }
    require(before_blocks.has_link, "Markdown no-op fixture link mark")?;

    let protocol = run_markdown_noop_protocol(
        driver,
        MarkdownNoopFixture {
            space_id: &ctx.space_id,
            object_id: &object.id,
            exported_body: &before_export,
        },
    )
    .await?;
    let after_export = stable_markdown_export(ctx, &object.id).await?;
    let after_blocks = stable_markdown_blocks(ctx, &object.id).await?;
    require(
        after_export == before_export,
        "independent Markdown no-op byte identity",
    )?;
    require(
        Sha256::digest(after_export.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            == protocol.body_sha256,
        "independent Markdown no-op hash",
    )?;
    require(
        before_blocks.semantics == after_blocks.semantics,
        "independent Markdown no-op typed semantics and order",
    )?;
    require(
        before_blocks.has_link == after_blocks.has_link,
        "independent Markdown no-op link semantics",
    )?;
    eprintln!(
        "MCP Markdown no-op evidence: transport_scenario={} bytes_before={} bytes_after={} blocks_before={} blocks_after={} block_identity={}",
        ScenarioId::MarkdownNoop.as_str(),
        protocol.before_bytes,
        protocol.after_bytes,
        before_blocks.identity.len(),
        after_blocks.identity.len(),
        before_blocks.identity == after_blocks.identity,
    );
    Ok(())
}

async fn run_archive(
    driver: &mut impl McpDriver,
    ctx: &TestContext,
    evidence: &mut ScenarioEvidence,
) -> Result<(), String> {
    let type_key = format!("mcp_shared_archive_{}", unique_suffix());
    let archive_type = ctx
        .client
        .new_type(
            &ctx.space_id,
            format!("MCP shared archive type {}", unique_suffix()),
        )
        .key(&type_key)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive type fixture".to_owned())?;
    ctx.register_type(&archive_type.id);
    evidence.fixture(&archive_type.id);
    let object = ctx
        .client
        .new_object(&ctx.space_id, &type_key)
        .name(format!("MCP shared archive {}", unique_suffix()))
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create archive object fixture".to_owned())?;
    ctx.register_object(&object.id);
    evidence.fixture(&object.id);
    let type_id = object
        .r#type
        .as_ref()
        .map(|value| value.id.clone())
        .ok_or_else(|| "archive fixture type".to_owned())?;
    let result = driver
        .call_tool(
            "object_archive",
            json!({"space": ctx.space_id, "object_id": object.id}),
        )
        .await?;
    require(result["archived"] == true, "archive result")?;
    for _ in 0..10 {
        let active = active_contains(ctx, &object.id, &type_id).await?;
        let archived = archived_contains(ctx, &object.id, &type_id).await?;
        if !active && archived {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err("archive evidence did not converge".to_owned())
}

async fn active_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .objects(&ctx.space_id)
        .filter(anytype::prelude::Filter::type_in([type_id.to_owned()]))
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read active archive evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds active evidence page",
    )?;
    Ok(page
        .items
        .iter()
        .any(|object| object.id == object_id && !object.archived))
}

async fn archived_contains(
    ctx: &TestContext,
    object_id: &str,
    type_id: &str,
) -> Result<bool, String> {
    let page = ctx
        .client
        .list_archived(&ctx.space_id)
        .types([type_id])
        .limit(100)
        .offset(0)
        .list()
        .await
        .map_err(|_| "read archived evidence".to_owned())?;
    require(
        !page.pagination.has_more,
        "unique archive type unexpectedly exceeds archived evidence page",
    )?;
    Ok(page.items.iter().any(|object| object.id == object_id))
}

async fn create_object(
    ctx: &TestContext,
    name: &str,
    body: &str,
) -> Result<anytype::prelude::Object, String> {
    let object = ctx
        .client
        .new_object(&ctx.space_id, "page")
        .name(name)
        .body(body)
        .ensure_available()
        .create()
        .await
        .map_err(|_| "create live object fixture".to_owned())?;
    ctx.register_object(&object.id);
    Ok(object)
}

async fn read_body(ctx: &TestContext, object_id: &str) -> Result<String, String> {
    ctx.client
        .object(&ctx.space_id, object_id)
        .get()
        .await
        .map_err(|_| "read live object fixture".to_owned())
        .map(|object| object.markdown.unwrap_or_default())
}

async fn walk_pages(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    expected_ids: &[String],
    max_pages: usize,
) -> Result<(), String> {
    let mut cursor = None;
    let mut seen_cursors = HashSet::new();
    let mut seen_ids = HashSet::new();
    let mut binding_checked = false;
    for _ in 0..max_pages {
        let mut input = base
            .as_object()
            .cloned()
            .ok_or_else(|| "page input must be an object".to_owned())?;
        input.insert("limit".to_owned(), json!(1));
        if let Some(cursor) = &cursor {
            input.insert("cursor".to_owned(), json!(cursor));
        }
        let page = driver.call_tool(tool, Value::Object(input.clone())).await?;
        for item in page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} items array"))?
        {
            if let Some(id) = item_id(item) {
                require(
                    seen_ids.insert(id.to_owned()),
                    &format!("{tool} item progress"),
                )?;
            }
        }
        let Some(next) = page.get("next_cursor").and_then(Value::as_str) else {
            for id in expected_ids {
                require(
                    seen_ids.contains(id),
                    &format!("{tool} observes fixture {id}"),
                )?;
            }
            return Ok(());
        };
        require(
            seen_cursors.insert(next.to_owned()),
            &format!("{tool} cursor progress"),
        )?;
        if !binding_checked {
            let mut mismatch = input;
            mismatch.insert("limit".to_owned(), json!(2));
            mismatch.insert("cursor".to_owned(), json!(next));
            let code = driver
                .call_tool_error(tool, Value::Object(mismatch))
                .await?;
            require(code == "validation", &format!("{tool} cursor binding"))?;
            binding_checked = true;
        }
        cursor = Some(next.to_owned());
    }
    Err(format!("{tool} did not terminate within {max_pages} pages"))
}

async fn assert_filtered_cursor_contract(
    driver: &mut impl McpDriver,
    tool: &'static str,
    base: Value,
    filter_value: &str,
    expected_ids: &[String],
) -> Result<(), String> {
    const MAX_PAGES: usize = 8;

    let filter = |value: &str| {
        json!({
            "operator": "and",
            "conditions": [{
                "format": "text",
                "property_key": "name",
                "condition": "contains",
                "value": value
            }]
        })
    };
    let mut request = base
        .as_object()
        .cloned()
        .ok_or_else(|| "filtered page input must be an object".to_owned())?;
    request.insert("filters".to_owned(), filter(filter_value));
    request.insert("limit".to_owned(), json!(1));

    let first = driver
        .call_tool(tool, Value::Object(request.clone()))
        .await?;
    let first_items = first["items"]
        .as_array()
        .ok_or_else(|| format!("{tool} filtered items array"))?;
    require(
        first_items.len() == 1,
        &format!("{tool} filtered first page"),
    )?;
    let first_id =
        item_id(&first_items[0]).ok_or_else(|| format!("{tool} filtered item identity"))?;
    let mut seen_ids = HashSet::from([first_id.to_owned()]);
    let first_cursor = first
        .get("next_cursor")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{tool} filtered continuation"))?
        .to_owned();

    let mut mismatch = request.clone();
    mismatch.insert(
        "filters".to_owned(),
        filter(&format!("{filter_value}-mismatch")),
    );
    mismatch.insert("cursor".to_owned(), json!(first_cursor));
    let code = driver
        .call_tool_error(tool, Value::Object(mismatch))
        .await?;
    require(
        code == "validation",
        &format!("{tool} filter cursor binding"),
    )?;

    let mut cursor = Some(first_cursor);
    for _ in 1..MAX_PAGES {
        let Some(next) = cursor.take() else {
            break;
        };
        let mut continuation = request.clone();
        continuation.insert("cursor".to_owned(), json!(next));
        let page = driver.call_tool(tool, Value::Object(continuation)).await?;
        for item in page["items"]
            .as_array()
            .ok_or_else(|| format!("{tool} filtered continuation items"))?
        {
            let id =
                item_id(item).ok_or_else(|| format!("{tool} filtered continuation identity"))?;
            require(
                seen_ids.insert(id.to_owned()),
                &format!("{tool} filtered identity progress"),
            )?;
        }
        cursor = page
            .get("next_cursor")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    require(
        cursor.is_none(),
        &format!("{tool} filtered pagination terminates"),
    )?;
    require(
        seen_ids == expected_ids.iter().cloned().collect(),
        &format!("{tool} filtered exact identities"),
    )
}

fn item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.pointer("/summary/id"))
        .or_else(|| item.pointer("/object/id"))
        .and_then(Value::as_str)
}

/// Closed inventory of standard tool and resource operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LiveOperation {
    ObjectArchive,
    ObjectCreate,
    ObjectEdit,
    ObjectGet,
    ObjectSearch,
    ObjectUpdate,
    PropertyList,
    ServerStatus,
    SpaceList,
    TagList,
    TemplateList,
    TypeList,
    ViewList,
    ViewObjectList,
    ResourcesList,
    ResourcesRead,
    ResourcesTemplatesList,
}

/// Typed binding from one advertised operation to one executable scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ownership {
    pub operation: LiveOperation,
    pub scenario: ScenarioId,
}

pub const LIVE_OWNERSHIP: &[Ownership] = &[
    own(LiveOperation::ObjectArchive, ScenarioId::Archive),
    own(LiveOperation::ObjectCreate, ScenarioId::Mutations),
    own(LiveOperation::ObjectEdit, ScenarioId::Documents),
    own(LiveOperation::ObjectGet, ScenarioId::Documents),
    own(LiveOperation::ObjectSearch, ScenarioId::Documents),
    own(LiveOperation::ObjectUpdate, ScenarioId::Mutations),
    own(LiveOperation::PropertyList, ScenarioId::Discovery),
    own(LiveOperation::ServerStatus, ScenarioId::Discovery),
    own(LiveOperation::SpaceList, ScenarioId::Discovery),
    own(LiveOperation::TagList, ScenarioId::Discovery),
    own(LiveOperation::TemplateList, ScenarioId::Discovery),
    own(LiveOperation::TypeList, ScenarioId::Discovery),
    own(LiveOperation::ViewList, ScenarioId::Views),
    own(LiveOperation::ViewObjectList, ScenarioId::Views),
    own(LiveOperation::ResourcesList, ScenarioId::Documents),
    own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    own(LiveOperation::ResourcesTemplatesList, ScenarioId::Documents),
];

const fn own(operation: LiveOperation, scenario: ScenarioId) -> Ownership {
    Ownership {
        operation,
        scenario,
    }
}

fn parse_tool(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "object_archive" => LiveOperation::ObjectArchive,
        "object_create" => LiveOperation::ObjectCreate,
        "object_edit" => LiveOperation::ObjectEdit,
        "object_get" => LiveOperation::ObjectGet,
        "object_search" => LiveOperation::ObjectSearch,
        "object_update" => LiveOperation::ObjectUpdate,
        "property_list" => LiveOperation::PropertyList,
        "server_status" => LiveOperation::ServerStatus,
        "space_list" => LiveOperation::SpaceList,
        "tag_list" => LiveOperation::TagList,
        "template_list" => LiveOperation::TemplateList,
        "type_list" => LiveOperation::TypeList,
        "view_list" => LiveOperation::ViewList,
        "view_object_list" => LiveOperation::ViewObjectList,
        _ => return None,
    })
}

fn parse_resource(name: &str) -> Option<LiveOperation> {
    Some(match name {
        "resources/list" => LiveOperation::ResourcesList,
        "resources/read" => LiveOperation::ResourcesRead,
        "resources/templates/list" => LiveOperation::ResourcesTemplatesList,
        _ => return None,
    })
}

/// Validates exact, unique, executable live ownership for the production catalog.
pub fn validate_live_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
) -> Result<(), String> {
    validate_ownership(expected_tools, expected_resources, LIVE_OWNERSHIP)
}

fn validate_ownership(
    expected_tools: &[&str],
    expected_resources: &[&str],
    owners: &[Ownership],
) -> Result<(), String> {
    let mut expected = HashSet::new();
    for name in expected_tools {
        let operation =
            parse_tool(name).ok_or_else(|| format!("unknown advertised tool operation: {name}"))?;
        expected.insert(operation);
    }
    for name in expected_resources {
        let operation = parse_resource(name)
            .ok_or_else(|| format!("unknown advertised resource operation: {name}"))?;
        expected.insert(operation);
    }
    let mut seen = HashSet::new();
    for owner in owners {
        if !expected.contains(&owner.operation) {
            return Err(format!(
                "unknown live operation owner: {:?}",
                owner.operation
            ));
        }
        if !seen.insert(owner.operation) {
            return Err(format!(
                "duplicate live operation owner: {:?}",
                owner.operation
            ));
        }
        if !owner.scenario.is_executable() || !ScenarioId::EXECUTABLE.contains(&owner.scenario) {
            return Err(format!(
                "non-executable live scenario owner: {}",
                owner.scenario.as_str()
            ));
        }
    }
    let mut missing = expected.difference(&seen).copied().collect::<Vec<_>>();
    missing.sort_unstable();
    if let Some(operation) = missing.first() {
        return Err(format!("missing live operation owner: {operation:?}"));
    }
    Ok(())
}

fn required_string(value: &Value, pointer: &str) -> Result<String, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn require(condition: bool, message: &str) -> Result<(), String> {
    condition.then_some(()).ok_or_else(|| message.to_owned())
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedNoopDriver {
        responses: VecDeque<Value>,
        calls: Vec<(&'static str, Value)>,
    }

    impl ScriptedNoopDriver {
        fn new(exported: &str, repeated: &str) -> Self {
            let hash = Sha256::digest(exported.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let object_get = |body: &str| {
                json!({
                    "object": {"summary": {"id": "object-id", "space_id": "space-id"}},
                    "body": {"text": body, "sha256": hash}
                })
            };
            Self {
                responses: VecDeque::from([
                    object_get(exported),
                    json!({
                        "object": {"id": "object-id", "space_id": "space-id"},
                        "body_sha256": hash
                    }),
                    object_get(repeated),
                ]),
                calls: Vec::new(),
            }
        }
    }

    impl McpDriver for ScriptedNoopDriver {
        fn call_tool<'a>(
            &'a mut self,
            name: &'static str,
            arguments: Value,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            self.calls.push((name, arguments));
            Box::pin(std::future::ready(self.responses.pop_front().ok_or_else(
                || "scripted Markdown response exhausted".to_owned(),
            )))
        }

        fn call_tool_error<'a>(
            &'a mut self,
            _name: &'static str,
            _arguments: Value,
        ) -> Pin<Box<dyn Future<Output = Result<String, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted error call".to_owned()
            )))
        }

        fn list_tools<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_tools".to_owned()
            )))
        }

        fn list_resources<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_resources".to_owned(),
            )))
        }

        fn list_resource_templates<'a>(
            &'a mut self,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted list_resource_templates".to_owned(),
            )))
        }

        fn read_resource<'a>(
            &'a mut self,
            _uri: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + 'a>> {
            Box::pin(std::future::ready(Err(
                "unexpected scripted read_resource".to_owned()
            )))
        }
    }

    // This compile-time assignment ensures callers cannot accidentally regain
    // the large inline dispatcher future that overflowed the live-test worker.
    #[allow(dead_code)]
    fn assert_heap_owned_dispatch<'a, D: McpDriver>(
        driver: &'a mut D,
        ctx: &'a TestContext,
        evidence: &'a mut ScenarioEvidence,
    ) {
        let future: ScenarioFuture<'a> = run_scenario(ScenarioId::Discovery, driver, ctx, evidence);
        std::mem::drop(future);
    }

    const TOOLS: &[&str] = &["server_status", "object_get"];
    const RESOURCES: &[&str] = &["resources/read"];
    const COMPLETE: &[Ownership] = &[
        own(LiveOperation::ServerStatus, ScenarioId::Discovery),
        own(LiveOperation::ObjectGet, ScenarioId::Documents),
        own(LiveOperation::ResourcesRead, ScenarioId::Documents),
    ];

    #[test]
    fn synthetic_missing_operation_fails_deterministically() {
        let error = validate_ownership(TOOLS, RESOURCES, &COMPLETE[..2]).unwrap_err();
        assert_eq!(error, "missing live operation owner: ResourcesRead");
    }

    #[test]
    fn scenario_dispatch_storage_is_only_a_fat_pointer() {
        assert_eq!(
            std::mem::size_of::<ScenarioFuture<'static>>(),
            2 * std::mem::size_of::<usize>()
        );
    }

    #[tokio::test]
    async fn markdown_noop_protocol_forwards_exact_export_hash_and_repeats_read() {
        let body = "## Stable\n\nUnicode こんにちは and [link](https://example.com).";
        let mut driver = ScriptedNoopDriver::new(body, body);
        let result = run_markdown_noop_protocol(
            &mut driver,
            MarkdownNoopFixture {
                space_id: "space-id",
                object_id: "object-id",
                exported_body: body,
            },
        )
        .await
        .unwrap();
        assert_eq!(result.before_bytes, body.len());
        assert_eq!(result.after_bytes, body.len());
        assert_eq!(driver.calls.len(), 3);
        assert_eq!(
            driver
                .calls
                .iter()
                .map(|(name, _)| *name)
                .collect::<Vec<_>>(),
            ["object_get", "object_update", "object_get"]
        );
        assert_eq!(driver.calls[1].1["body_markdown"], body);
        assert_eq!(
            driver.calls[1].1["expected_body_sha256"],
            result.body_sha256
        );
    }

    #[tokio::test]
    async fn markdown_noop_protocol_rejects_lossy_repeated_export() {
        let mut driver = ScriptedNoopDriver::new("before", "after");
        let error = run_markdown_noop_protocol(
            &mut driver,
            MarkdownNoopFixture {
                space_id: "space-id",
                object_id: "object-id",
                exported_body: "before",
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Markdown no-op repeated export byte identity");
    }

    #[test]
    fn duplicate_unknown_and_non_executable_owners_fail() {
        let duplicate = [COMPLETE[0], COMPLETE[0], COMPLETE[1], COMPLETE[2]];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &duplicate)
                .unwrap_err()
                .starts_with("duplicate live operation owner")
        );
        let unknown = [
            COMPLETE[0],
            COMPLETE[1],
            COMPLETE[2],
            own(LiveOperation::ObjectCreate, ScenarioId::Discovery),
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &unknown)
                .unwrap_err()
                .starts_with("unknown live operation owner")
        );
        let non_executable = [
            Ownership {
                operation: LiveOperation::ServerStatus,
                scenario: ScenarioId::SyntheticNonExecutable,
            },
            COMPLETE[1],
            COMPLETE[2],
        ];
        assert!(
            validate_ownership(TOOLS, RESOURCES, &non_executable)
                .unwrap_err()
                .starts_with("non-executable live scenario owner")
        );
    }
}
